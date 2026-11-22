#![allow(dead_code)]

use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    pub fn contains(self, offset: usize) -> bool {
        offset >= self.start && offset <= self.end
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub span: Span,
    pub severity: Severity,
    pub message: String,
}

/// How Minecraft reads a data file. Each version and loader picks a Gson mode per kind of file, so we
/// warn about non-standard JSON that it accepts and report what it rejects as errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonDialect {
    /// `JsonParser.parseReader`: comments, unquoted keys, single quotes and raw control characters all load.
    Lenient,
    /// Gson's default, non-lenient reader before 2.11: standard JSON, except raw control characters in strings load.
    LegacyStrict,
    /// `StrictJsonParser` (Gson 2.11 `Strictness.STRICT`): standard JSON only.
    Strict,
}

/// Non-standard JSON that some of Minecraft's parsers accept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Nonstandard {
    Comment,
    ControlCharacter,
    TrailingCommaInArray,
    TrailingCommaInObject,
    UnquotedKey,
    SingleQuotes,
}

impl Nonstandard {
    fn describe(self) -> &'static str {
        match self {
            Self::Comment => "comments",
            Self::ControlCharacter => "line breaks or other control characters inside strings",
            Self::TrailingCommaInArray | Self::TrailingCommaInObject => "trailing commas",
            Self::UnquotedKey => "unquoted keys",
            Self::SingleQuotes => "single-quoted strings",
        }
    }

    fn accepted_by(self, dialect: JsonDialect) -> bool {
        match (self, dialect) {
            // Gson rejects a trailing comma in an object even when lenient.
            (Self::TrailingCommaInObject, _) => false,
            (_, JsonDialect::Lenient) => true,
            (Self::ControlCharacter, JsonDialect::LegacyStrict) => true,
            _ => false,
        }
    }
}

/// Maps byte offsets in an unescaped JSON string back to byte offsets in the raw JSON source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StringMap {
    /// Source offset right after the opening `"`.
    pub content_start: usize,
    /// `(unescaped_byte_offset, source_byte_offset)` pairs for escape sequences that shifted offsets.
    entries: Vec<(usize, usize)>,
}

impl StringMap {
    pub fn new(content_start: usize) -> Self {
        Self { content_start, entries: Vec::new() }
    }

    /// Translates an offset inside the unescaped string to its byte offset in the raw JSON text.
    pub fn to_source(&self, unescaped_offset: usize) -> usize {
        let mut base_unesc = 0usize;
        let mut base_src = self.content_start;
        for &(u, s) in &self.entries {
            if u > unescaped_offset {
                break;
            }
            base_unesc = u;
            base_src = s;
        }
        base_src + (unescaped_offset - base_unesc)
    }

    pub fn span_to_source(&self, start: usize, end: usize) -> Span {
        Span::new(self.to_source(start), self.to_source(end))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct JsonString {
    pub span: Span,
    pub value: String,
    pub closed: bool,
    pub map: StringMap,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JsonPair {
    pub span: Span,
    pub key: JsonString,
    pub colon: Option<usize>,
    pub value: Option<JsonNode>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum JsonKind {
    Object(Vec<JsonPair>),
    Array(Vec<JsonNode>),
    String(JsonString),
    Number { raw: String, value: f64, is_int: bool, int_value: Option<i64> },
    Boolean(bool),
    Null,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JsonNode {
    pub span: Span,
    pub kind: JsonKind,
}

impl JsonNode {
    pub fn as_object(&self) -> Option<&[JsonPair]> {
        match &self.kind {
            JsonKind::Object(pairs) => Some(pairs),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match &self.kind {
            JsonKind::String(s) => Some(&s.value),
            _ => None,
        }
    }

    pub fn get(&self, key: &str) -> Option<&JsonNode> {
        self.as_object()?
            .iter()
            .find(|pair| pair.key.value == key)
            .and_then(|pair| pair.value.as_ref())
    }
}

#[derive(Debug, Clone)]
pub struct ParsedJson {
    pub root: Option<JsonNode>,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn parse(src: &str, allow_extensions: bool) -> ParsedJson {
    parse_as(src, allow_extensions, None)
}

/// Parses `src` the way Minecraft reads it in `dialect`, or as plain JSON when `dialect` is `None`.
pub fn parse_as(src: &str, allow_extensions: bool, dialect: Option<JsonDialect>) -> ParsedJson {
    let start_pos = if src.starts_with('\u{FEFF}') { '\u{FEFF}'.len_utf8() } else { 0 };
    let mut parser = Parser {
        src,
        bytes: src.as_bytes(),
        pos: start_pos,
        allow_extensions,
        dialect,
        diagnostics: Vec::new(),
    };
    parser.skip_ws_and_comments();
    let root = if parser.pos < parser.bytes.len() {
        parser.parse_value()
    } else {
        None
    };
    parser.skip_ws_and_comments();
    if parser.pos < parser.bytes.len() {
        parser.error(Span::new(parser.pos, parser.bytes.len()), "Unexpected content after the end of the document");
    }
    ParsedJson { root, diagnostics: parser.diagnostics }
}

struct Parser<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    allow_extensions: bool,
    dialect: Option<JsonDialect>,
    diagnostics: Vec<Diagnostic>,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn error(&mut self, span: Span, message: impl Into<String>) {
        self.diagnostics.push(Diagnostic { span, severity: Severity::Error, message: message.into() });
    }

    fn warn(&mut self, span: Span, message: impl Into<String>) {
        self.diagnostics.push(Diagnostic { span, severity: Severity::Warning, message: message.into() });
    }

    /// Reports non-standard JSON: an error if Minecraft won't load it, otherwise a warning.
    fn nonstandard(&mut self, span: Span, kind: Nonstandard) {
        if self.allow_extensions && matches!(kind, Nonstandard::Comment | Nonstandard::TrailingCommaInArray | Nonstandard::TrailingCommaInObject) {
            return;
        }
        let what = kind.describe();
        match self.dialect {
            Some(dialect) if !kind.accepted_by(dialect) => self.error(span, format!("Minecraft won't load this file, because its parser doesn't allow {what} here.")),
            Some(JsonDialect::Lenient) if kind == Nonstandard::TrailingCommaInArray => {
                self.warn(span, "Minecraft reads this trailing comma as an extra null entry, which usually fails to load.")
            }
            Some(_) => self.warn(span, format!("Standard JSON doesn't allow {what}, so other tools may reject this file, but Minecraft loads it.")),
            None if matches!(kind, Nonstandard::Comment | Nonstandard::TrailingCommaInArray | Nonstandard::TrailingCommaInObject) => {
                self.warn(span, format!("Standard JSON doesn't allow {what}, so strict parsers will reject this file."))
            }
            None => self.error(span, format!("Standard JSON doesn't allow {what}.")),
        }
    }

    fn skip_ws_and_comments(&mut self) {
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            if matches!(b, b' ' | b'\t' | b'\r' | b'\n') {
                self.pos += 1;
                continue;
            }
            if b == b'/' && self.pos + 1 < self.bytes.len() {
                let next = self.bytes[self.pos + 1];
                if next == b'/' {
                    let start = self.pos;
                    self.pos += 2;
                    while self.pos < self.bytes.len() && self.bytes[self.pos] != b'\n' {
                        self.pos += 1;
                    }
                    self.nonstandard(Span::new(start, self.pos), Nonstandard::Comment);
                    continue;
                }
                if next == b'*' {
                    let start = self.pos;
                    self.pos += 2;
                    let mut closed = false;
                    while self.pos + 1 < self.bytes.len() {
                        if self.bytes[self.pos] == b'*' && self.bytes[self.pos + 1] == b'/' {
                            self.pos += 2;
                            closed = true;
                            break;
                        }
                        self.pos += 1;
                    }
                    if !closed {
                        self.pos = self.bytes.len();
                        self.error(Span::new(start, self.pos), "Unclosed comment");
                    } else {
                        self.nonstandard(Span::new(start, self.pos), Nonstandard::Comment);
                    }
                    continue;
                }
            }
            if b == b'#' {
                let start = self.pos;
                while self.pos < self.bytes.len() && self.bytes[self.pos] != b'\n' {
                    self.pos += 1;
                }
                self.nonstandard(Span::new(start, self.pos), Nonstandard::Comment);
                continue;
            }
            break;
        }
    }

    fn parse_value(&mut self) -> Option<JsonNode> {
        self.skip_ws_and_comments();
        let b = self.peek()?;
        match b {
            b'{' => Some(self.parse_object()),
            b'[' => Some(self.parse_array()),
            b'"' | b'\'' => {
                let s = self.parse_string();
                Some(JsonNode { span: s.span, kind: JsonKind::String(s) })
            }
            b'-' | b'0'..=b'9' => Some(self.parse_number()),
            b't' => self.parse_keyword("true", JsonKind::Boolean(true)),
            b'f' => self.parse_keyword("false", JsonKind::Boolean(false)),
            b'n' => self.parse_keyword("null", JsonKind::Null),
            _ => {
                let start = self.pos;
                self.advance_char();
                self.error(Span::new(start, self.pos), "Expected a value");
                None
            }
        }
    }

    fn advance_char(&mut self) {
        if self.pos < self.bytes.len() {
            let ch = self.src[self.pos..].chars().next().unwrap();
            self.pos += ch.len_utf8();
        }
    }

    fn parse_keyword(&mut self, expected: &str, kind: JsonKind) -> Option<JsonNode> {
        let start = self.pos;
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_alphanumeric() {
            self.pos += 1;
        }
        let word = &self.src[start..self.pos];
        if word == expected {
            Some(JsonNode { span: Span::new(start, self.pos), kind })
        } else {
            self.error(Span::new(start, self.pos), "Unexpected character");
            None
        }
    }

    fn parse_object(&mut self) -> JsonNode {
        let start = self.pos;
        self.pos += 1; // '{'
        let mut pairs = Vec::new();
        let mut seen = HashSet::new();

        loop {
            self.skip_ws_and_comments();
            match self.peek() {
                None => {
                    self.error(Span::new(self.pos, self.pos), "Expected a closing brace");
                    break;
                }
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                Some(b'"') | Some(b'\'') => {
                    let pair_start = self.pos;
                    let key = self.parse_string();
                    if !seen.insert(key.value.clone()) {
                        self.warn(
                            key.span,
                            format!("Duplicate key \"{}\". Depending on the parser, this either fails or silently keeps only one of the values.", key.value),
                        );
                    }
                    self.skip_ws_and_comments();
                    let (colon, value) = if self.peek() == Some(b':') {
                        let colon_pos = self.pos;
                        self.pos += 1;
                        self.skip_ws_and_comments();
                        let val = if matches!(self.peek(), None | Some(b'}') | Some(b',')) {
                            self.error(Span::new(self.pos, self.pos), "Expected a value");
                            None
                        } else {
                            self.parse_value()
                        };
                        (Some(colon_pos), val)
                    } else {
                        self.error(Span::new(self.pos, self.pos), "Expected a colon");
                        (None, None)
                    };
                    let pair_end = value.as_ref().map(|v| v.span.end).or(colon.map(|c| c + 1)).unwrap_or(key.span.end);
                    pairs.push(JsonPair { span: Span::new(pair_start, pair_end), key, colon, value });

                    self.skip_ws_and_comments();
                    match self.peek() {
                        Some(b',') => {
                            let comma = self.pos;
                            self.pos += 1;
                            self.skip_ws_and_comments();
                            if self.peek() == Some(b'}') {
                                self.nonstandard(Span::new(comma, comma + 1), Nonstandard::TrailingCommaInObject);
                            }
                        }
                        Some(b'}') | None => {}
                        _ => {
                            self.error(Span::new(self.pos, self.pos), "Expected a comma");
                        }
                    }
                }
                Some(b',') => {
                    let comma = self.pos;
                    self.pos += 1;
                    self.error(Span::new(comma, comma + 1), "Expected a quoted key");
                }
                Some(_) if self.unquoted_key_end().is_some() => {
                    let pair_start = self.pos;
                    let end = self.unquoted_key_end().unwrap_or(self.pos);
                    let key = JsonString {
                        span: Span::new(self.pos, end),
                        value: self.src[self.pos..end].to_string(),
                        closed: true,
                        map: StringMap::new(self.pos),
                    };
                    self.nonstandard(key.span, Nonstandard::UnquotedKey);
                    self.pos = end;
                    self.skip_ws_and_comments();
                    let colon = self.pos;
                    self.pos += 1;
                    self.skip_ws_and_comments();
                    let value = if matches!(self.peek(), None | Some(b'}') | Some(b',')) {
                        self.error(Span::new(self.pos, self.pos), "Expected a value");
                        None
                    } else {
                        self.parse_value()
                    };
                    let pair_end = value.as_ref().map_or(colon + 1, |v| v.span.end);
                    seen.insert(key.value.clone());
                    pairs.push(JsonPair { span: Span::new(pair_start, pair_end), key, colon: Some(colon), value });
                    self.skip_ws_and_comments();
                    match self.peek() {
                        Some(b',') => self.pos += 1,
                        Some(b'}') | None => {}
                        _ => self.error(Span::new(self.pos, self.pos), "Expected a comma"),
                    }
                }
                Some(_) => {
                    let err_start = self.pos;
                    while let Some(b) = self.peek() {
                        if matches!(b, b'}' | b',' | b'"' | b'\n') {
                            break;
                        }
                        self.advance_char();
                    }
                    if self.pos == err_start {
                        self.advance_char();
                    }
                    self.error(Span::new(err_start, self.pos), "Expected a quoted key");
                }
            }
        }

        JsonNode { span: Span::new(start, self.pos), kind: JsonKind::Object(pairs) }
    }

    fn parse_array(&mut self) -> JsonNode {
        let start = self.pos;
        self.pos += 1; // '['
        let mut items = Vec::new();

        loop {
            self.skip_ws_and_comments();
            match self.peek() {
                None => {
                    self.error(Span::new(self.pos, self.pos), "Expected a closing bracket");
                    break;
                }
                Some(b']') => {
                    self.pos += 1;
                    break;
                }
                Some(b',') => {
                    let comma = self.pos;
                    self.pos += 1;
                    self.error(Span::new(comma, comma + 1), "Expected a value");
                }
                Some(_) => {
                    if let Some(val) = self.parse_value() {
                        items.push(val);
                    }
                    self.skip_ws_and_comments();
                    match self.peek() {
                        Some(b',') => {
                            let comma = self.pos;
                            self.pos += 1;
                            self.skip_ws_and_comments();
                            if self.peek() == Some(b']') {
                                self.nonstandard(Span::new(comma, comma + 1), Nonstandard::TrailingCommaInArray);
                            }
                        }
                        Some(b']') | None => {}
                        _ => {
                            self.error(Span::new(self.pos, self.pos), "Expected a comma");
                        }
                    }
                }
            }
        }

        JsonNode { span: Span::new(start, self.pos), kind: JsonKind::Array(items) }
    }

    /// Returns where an unquoted key at the cursor ends, if a colon follows it.
    fn unquoted_key_end(&self) -> Option<usize> {
        let is_literal = |b: u8| !matches!(b, b'/' | b'\\' | b';' | b'#' | b'=' | b'{' | b'}' | b'[' | b']' | b':' | b',' | b' ' | b'\t' | b'\r' | b'\n' | b'"' | b'\'');
        let mut end = self.pos;
        while end < self.bytes.len() && is_literal(self.bytes[end]) {
            end += 1;
        }
        if end == self.pos {
            return None;
        }
        let mut after = end;
        while after < self.bytes.len() && matches!(self.bytes[after], b' ' | b'\t') {
            after += 1;
        }
        (self.bytes.get(after) == Some(&b':')).then_some(end)
    }

    /// Returns whether a line break at `pos` inside a string is part of it, meaning the string closes
    /// later and JSON continues after it. Otherwise we treat it as an unclosed string you're still typing.
    fn string_continues_past_line_break(&self, pos: usize, quote: u8) -> bool {
        let mut i = pos;
        while i < self.bytes.len() {
            match self.bytes[i] {
                b'\\' => i += 2,
                b if b == quote => {
                    let mut after = i + 1;
                    while after < self.bytes.len() && matches!(self.bytes[after], b' ' | b'\t' | b'\r' | b'\n') {
                        after += 1;
                    }
                    return matches!(self.bytes.get(after), None | Some(b',' | b'}' | b']' | b':'));
                }
                _ => i += 1,
            }
        }
        false
    }

    fn parse_string(&mut self) -> JsonString {
        let start = self.pos;
        let quote = self.bytes[self.pos];
        if quote == b'\'' {
            self.nonstandard(Span::new(start, start + 1), Nonstandard::SingleQuotes);
        }
        self.pos += 1;
        let content_start = self.pos;
        let mut out = String::new();
        let mut map = StringMap::new(content_start);
        let mut closed = false;
        let mut reported_control_character = false;

        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            match b {
                b if b == quote => {
                    self.pos += 1;
                    closed = true;
                    break;
                }
                b'\n' | b'\r' if !self.string_continues_past_line_break(self.pos, quote) => {
                    break;
                }
                b'\\' => {
                    let esc_start = self.pos;
                    self.pos += 1;
                    if self.pos >= self.bytes.len() {
                        self.error(Span::new(esc_start, self.pos), "Invalid escape sequence");
                        break;
                    }
                    let esc = self.bytes[self.pos];
                    self.pos += 1;
                    match esc {
                        b'"' => out.push('"'),
                        b'\'' => out.push('\''),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\x08'),
                        b'f' => out.push('\x0C'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            if let Some(code) = self.read_hex4() {
                                if (0xD800..=0xDBFF).contains(&code)
                                    && self.pos + 6 <= self.bytes.len()
                                    && &self.bytes[self.pos..self.pos + 2] == b"\\u"
                                {
                                    let save = self.pos;
                                    self.pos += 2;
                                    if let Some(low) = self.read_hex4()
                                        && (0xDC00..=0xDFFF).contains(&low)
                                    {
                                        let combined = 0x10000 + (((code as u32 - 0xD800) << 10) | (low as u32 - 0xDC00));
                                        if let Some(ch) = char::from_u32(combined) {
                                            out.push(ch);
                                        }
                                    } else {
                                        self.pos = save;
                                        out.push(char::REPLACEMENT_CHARACTER);
                                    }
                                } else if let Some(ch) = char::from_u32(code as u32) {
                                    out.push(ch);
                                } else {
                                    out.push(char::REPLACEMENT_CHARACTER);
                                }
                            } else {
                                self.error(Span::new(esc_start, self.pos.min(self.bytes.len())), "Invalid unicode escape");
                            }
                        }
                        _ => {
                            self.error(Span::new(esc_start, self.pos), "Invalid escape sequence");
                        }
                    }
                    map.entries.push((out.len(), self.pos));
                }
                0x00..=0x1F => {
                    let bad = self.pos;
                    self.pos += 1;
                    if !reported_control_character {
                        reported_control_character = true;
                        self.nonstandard(Span::new(bad, self.pos), Nonstandard::ControlCharacter);
                    }
                    out.push(b as char);
                }
                _ => {
                    let ch = self.src[self.pos..].chars().next().unwrap();
                    self.pos += ch.len_utf8();
                    out.push(ch);
                }
            }
        }

        if !closed {
            self.error(Span::new(start, self.pos), "Unclosed string");
        }

        JsonString { span: Span::new(start, self.pos), value: out, closed, map }
    }

    fn read_hex4(&mut self) -> Option<u16> {
        if self.pos + 4 > self.bytes.len() {
            return None;
        }
        let hex_str = std::str::from_utf8(&self.bytes[self.pos..self.pos + 4]).ok()?;
        let val = u16::from_str_radix(hex_str, 16).ok()?;
        self.pos += 4;
        Some(val)
    }

    fn parse_number(&mut self) -> JsonNode {
        let start = self.pos;
        let mut is_int = true;
        let mut valid = true;

        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => {
                self.pos += 1;
                if matches!(self.peek(), Some(b'0'..=b'9')) {
                    valid = false;
                    while matches!(self.peek(), Some(b'0'..=b'9')) {
                        self.pos += 1;
                    }
                }
            }
            Some(b'1'..=b'9') => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.pos += 1;
                }
            }
            _ => valid = false,
        }

        if self.peek() == Some(b'.') {
            is_int = false;
            self.pos += 1;
            if matches!(self.peek(), Some(b'0'..=b'9')) {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.pos += 1;
                }
            } else {
                valid = false;
            }
        }

        if matches!(self.peek(), Some(b'e' | b'E')) {
            is_int = false;
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if matches!(self.peek(), Some(b'0'..=b'9')) {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.pos += 1;
                }
            } else {
                valid = false;
            }
        }

        let raw = self.src[start..self.pos].to_string();
        if !valid {
            self.error(Span::new(start, self.pos), "Invalid number");
        }
        let int_value = if is_int && valid { raw.parse::<i64>().ok() } else { None };
        let value = raw.parse::<f64>().unwrap_or(0.0);
        JsonNode {
            span: Span::new(start, self.pos),
            kind: JsonKind::Number { raw, value, is_int, int_value },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_json_and_tracks_string_escapes() {
        let src = r#"{"a": "hi\nthere", "n": 42, "f": 1.5, "b": true, "x": [null]}"#;
        let parsed = parse(src, false);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let root = parsed.root.unwrap();
        let JsonKind::String(s) = &root.get("a").unwrap().kind else { panic!("expected string") };
        assert_eq!(s.value, "hi\nthere");
        assert!(s.closed);
        // "there" starts at index 3 in unescaped "hi\nthere", which is after `\n` in raw source.
        let src_pos = s.map.to_source(3);
        assert_eq!(&src[src_pos..src_pos + 5], "there");
    }

    #[test]
    fn recovers_on_half_typed_json_for_autocomplete() {
        let src = "{\n  \"type\": \"minecraft:crafting_shaped\",\n  \"result\": {\n    \"id\": \"create:b\n  }\n}";
        let parsed = parse(src, false);
        let root = parsed.root.expect("should still return root object");
        let result = root.get("result").expect("should parse nested object");
        let JsonKind::String(id) = &result.get("id").expect("should keep unclosed string").kind else { panic!() };
        assert_eq!(id.value, "create:b");
        assert!(!id.closed);
        assert!(parsed.diagnostics.iter().any(|d| d.severity == Severity::Error && d.message == "Unclosed string"));
    }

    #[test]
    fn flags_duplicate_keys_comments_and_trailing_commas() {
        let src = "{\n  // comment\n  \"a\": 1,\n  \"a\": 2,\n}";
        let parsed = parse(src, false);
        assert_eq!(parsed.diagnostics.iter().filter(|d| d.severity == Severity::Warning).count(), 3);
        let jsonc = parse(src, true);
        assert_eq!(jsonc.diagnostics.iter().filter(|d| d.severity == Severity::Warning).count(), 1);
    }
}
