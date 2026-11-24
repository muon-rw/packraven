#![allow(dead_code)]

use std::cmp::Ordering;
use std::collections::HashSet;

use super::checker::{check_nbt_with_type, is_valid_resource_location, parse_java_uuid, CheckEnv};
use super::json_parser::{Diagnostic, JsonKind, JsonNode, JsonPair, JsonString, Severity, Span, StringMap};
use super::registries::{lengthen_id, IdLookup};
use super::types::{Index, McdocType};
use super::version::compare_version;

const SNBT_NEW_SYNTAX_SINCE: &str = "1.21.5";
const MAX_SNBT_DEPTH: usize = 64;

#[derive(Debug, Clone)]
pub struct ParsedSnbt {
    pub root: Option<JsonNode>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnbtTag {
    Byte,
    Short,
    Int,
    Long,
    Float,
    Double,
    String,
    List,
    ByteArray,
    IntArray,
    LongArray,
    Compound,
}

impl SnbtTag {
    fn label(self) -> &'static str {
        match self {
            Self::Byte => "a byte",
            Self::Short => "a short",
            Self::Int => "an int",
            Self::Long => "a long",
            Self::Float => "a float",
            Self::Double => "a double",
            Self::String => "a string",
            Self::List => "a list",
            Self::ByteArray => "a byte array",
            Self::IntArray => "an int array",
            Self::LongArray => "a long array",
            Self::Compound => "a compound",
        }
    }

    fn is_number(self) -> bool {
        matches!(self, Self::Byte | Self::Short | Self::Int | Self::Long | Self::Float | Self::Double)
    }
}

pub fn parse_snbt(src: &str, minecraft: Option<&str>) -> ParsedSnbt {
    let old_syntax = is_old_snbt_syntax(minecraft);
    let mut parser = SnbtParser::new(src, old_syntax);
    parser.skip_ws();
    if parser.pos >= parser.bytes.len() {
        parser.error(Span::new(0, src.len()), "Expected an SNBT value.");
        return ParsedSnbt { root: None, diagnostics: parser.diagnostics };
    }
    let root = parser.parse_entry(0).map(|(node, _)| node);
    parser.skip_ws();
    if parser.pos < parser.bytes.len() {
        parser.error(Span::new(parser.pos, parser.bytes.len()), "Unexpected trailing content in SNBT.");
    }
    ParsedSnbt { root, diagnostics: parser.diagnostics }
}

fn is_old_snbt_syntax(minecraft: Option<&str>) -> bool {
    minecraft.is_some_and(|mc| compare_version(mc, SNBT_NEW_SYNTAX_SINCE) == Ordering::Less)
}

struct SnbtParser<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    old_syntax: bool,
    diagnostics: Vec<Diagnostic>,
}

impl<'a> SnbtParser<'a> {
    fn new(src: &'a str, old_syntax: bool) -> Self {
        Self { src, bytes: src.as_bytes(), pos: 0, old_syntax, diagnostics: Vec::new() }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn error(&mut self, span: Span, message: impl Into<String>) {
        self.diagnostics.push(Diagnostic { span, severity: Severity::Error, message: message.into() });
    }

    fn warn(&mut self, span: Span, message: impl Into<String>) {
        self.diagnostics.push(Diagnostic { span, severity: Severity::Warning, message: message.into() });
    }

    fn info(&mut self, span: Span, message: impl Into<String>) {
        self.diagnostics.push(Diagnostic { span, severity: Severity::Info, message: message.into() });
    }

    fn parse_entry(&mut self, depth: usize) -> Option<(JsonNode, SnbtTag)> {
        self.skip_ws();
        if depth > MAX_SNBT_DEPTH {
            self.error(Span::new(self.pos, self.pos), "SNBT nesting is too deep.");
            return None;
        }
        let rest = self.rest();
        if rest.starts_with("[B;") {
            return Some(self.parse_typed_array(SnbtTag::Byte, SnbtTag::ByteArray));
        }
        if rest.starts_with("[I;") {
            return Some(self.parse_typed_array(SnbtTag::Int, SnbtTag::IntArray));
        }
        if rest.starts_with("[L;") {
            return Some(self.parse_typed_array(SnbtTag::Long, SnbtTag::LongArray));
        }
        if rest.starts_with('[') {
            return Some(self.parse_list(depth + 1));
        }
        if rest.starts_with('{') {
            return Some((self.parse_compound(depth + 1), SnbtTag::Compound));
        }
        if rest.starts_with("bool(") {
            return Some(self.parse_bool_fn(depth + 1));
        }
        if rest.starts_with("uuid(") {
            return Some(self.parse_uuid_fn(depth + 1));
        }
        self.parse_primitive()
    }

    fn parse_compound(&mut self, depth: usize) -> JsonNode {
        let start = self.pos;
        self.pos += 1; // '{'
        let mut pairs = Vec::new();
        let mut seen = HashSet::new();

        loop {
            self.skip_ws();
            match self.peek() {
                None => {
                    self.error(Span::new(self.pos, self.pos), "Expected closing '}'.");
                    break;
                }
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                _ => {}
            }
            let pair_start = self.pos;
            let Some(key) = self.parse_string_token(true) else {
                self.error(Span::new(self.pos, (self.pos + 1).min(self.bytes.len())), "Expected a compound key.");
                self.advance_to_compound_recovery();
                if self.peek() == Some(b',') {
                    self.pos += 1;
                }
                continue;
            };
            if !seen.insert(key.value.clone()) {
                self.error(key.span, format!("Duplicate key \"{}\".", key.value));
            }
            self.skip_ws();
            let colon = if self.peek() == Some(b':') {
                let c = self.pos;
                self.pos += 1;
                Some(c)
            } else {
                self.error(Span::new(self.pos, self.pos), "Expected ':'.");
                None
            };
            self.skip_ws();
            let value = if colon.is_some() && !matches!(self.peek(), None | Some(b'}') | Some(b',')) {
                self.parse_entry(depth).map(|(n, _)| n)
            } else {
                if colon.is_some() {
                    self.error(Span::new(self.pos, self.pos), "Expected a value.");
                }
                None
            };
            let pair_end = value.as_ref().map_or(self.pos, |v| v.span.end);
            pairs.push(JsonPair { span: Span::new(pair_start, pair_end), key, colon, value });

            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                None => {
                    self.error(Span::new(self.pos, self.pos), "Expected closing '}'.");
                    break;
                }
                _ => {
                    self.error(Span::new(self.pos, self.pos), "Expected ',' or '}'.");
                    self.advance_to_compound_recovery();
                    if self.peek() == Some(b',') {
                        self.pos += 1;
                    }
                }
            }
        }

        JsonNode { span: Span::new(start, self.pos), kind: JsonKind::Object(pairs) }
    }

    fn advance_to_compound_recovery(&mut self) {
        while let Some(b) = self.peek() {
            if matches!(b, b',' | b'}') {
                break;
            }
            self.pos += 1;
        }
    }

    fn parse_list(&mut self, depth: usize) -> (JsonNode, SnbtTag) {
        let start = self.pos;
        self.pos += 1; // '['
        let mut items = Vec::new();
        let mut first_tag: Option<SnbtTag> = None;

        loop {
            self.skip_ws();
            match self.peek() {
                None => {
                    self.error(Span::new(self.pos, self.pos), "Expected closing ']'.");
                    break;
                }
                Some(b']') => {
                    self.pos += 1;
                    break;
                }
                _ => {}
            }
            if let Some((item, tag)) = self.parse_entry(depth) {
                if self.old_syntax {
                    match first_tag {
                        None => first_tag = Some(tag),
                        Some(expected) if expected != tag => {
                            self.error(
                                item.span,
                                format!("Expected {} in list before Minecraft 1.21.5, found {}.", expected.label(), tag.label()),
                            );
                        }
                        _ => {}
                    }
                }
                items.push(item);
            } else {
                self.error(Span::new(self.pos, (self.pos + 1).min(self.bytes.len())), "Expected a list element.");
                while let Some(b) = self.peek() {
                    if matches!(b, b',' | b']') {
                        break;
                    }
                    self.pos += 1;
                }
            }

            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    break;
                }
                None => {
                    self.error(Span::new(self.pos, self.pos), "Expected closing ']'.");
                    break;
                }
                _ => {
                    self.error(Span::new(self.pos, self.pos), "Expected ',' or ']'.");
                    while let Some(b) = self.peek() {
                        if matches!(b, b',' | b']') {
                            break;
                        }
                        self.pos += 1;
                    }
                }
            }
        }

        (JsonNode { span: Span::new(start, self.pos), kind: JsonKind::Array(items) }, SnbtTag::List)
    }

    fn parse_typed_array(&mut self, elem_tag: SnbtTag, array_tag: SnbtTag) -> (JsonNode, SnbtTag) {
        let start = self.pos;
        self.pos += 3; // `[B;`, `[I;`, `[L;`
        let mut items = Vec::new();

        loop {
            self.skip_ws();
            match self.peek() {
                None => {
                    self.error(Span::new(self.pos, self.pos), "Expected closing ']'.");
                    break;
                }
                Some(b']') => {
                    self.pos += 1;
                    break;
                }
                _ => {}
            }
            if let Some((item, tag)) = self.parse_primitive() {
                if tag != elem_tag {
                    self.error(item.span, format!("Expected {}, found {}.", elem_tag.label(), tag.label()));
                }
                items.push(item);
            } else {
                self.error(Span::new(self.pos, (self.pos + 1).min(self.bytes.len())), format!("Expected {}.", elem_tag.label()));
                while let Some(b) = self.peek() {
                    if matches!(b, b',' | b']') {
                        break;
                    }
                    self.pos += 1;
                }
            }

            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    break;
                }
                None => {
                    self.error(Span::new(self.pos, self.pos), "Expected closing ']'.");
                    break;
                }
                _ => {
                    self.error(Span::new(self.pos, self.pos), "Expected ',' or ']'.");
                    while let Some(b) = self.peek() {
                        if matches!(b, b',' | b']') {
                            break;
                        }
                        self.pos += 1;
                    }
                }
            }
        }

        (JsonNode { span: Span::new(start, self.pos), kind: JsonKind::Array(items) }, array_tag)
    }

    fn parse_bool_fn(&mut self, depth: usize) -> (JsonNode, SnbtTag) {
        let start = self.pos;
        self.pos += 5; // `bool(`
        if self.old_syntax {
            self.error(Span::new(start, self.pos), "SNBT functions require Minecraft 1.21.5 or newer.");
        }
        self.skip_ws();
        let mut bool_val = false;
        if let Some((arg, tag)) = self.parse_entry(depth) {
            if !tag.is_number() {
                self.error(arg.span, "bool() requires a numeric argument.");
            } else {
                bool_val = match &arg.kind {
                    JsonKind::Boolean(b) => *b,
                    JsonKind::Number { value, int_value, .. } => int_value.map_or(*value != 0.0, |i| i != 0),
                    _ => false,
                };
            }
        } else {
            self.error(Span::new(self.pos, self.pos), "Expected a numeric argument for bool().");
        }
        self.skip_ws();
        if self.peek() == Some(b')') {
            self.pos += 1;
        } else {
            self.error(Span::new(self.pos, self.pos), "Expected ')' to close bool().");
        }
        (JsonNode { span: Span::new(start, self.pos), kind: JsonKind::Boolean(bool_val) }, SnbtTag::Byte)
    }

    fn parse_uuid_fn(&mut self, depth: usize) -> (JsonNode, SnbtTag) {
        let start = self.pos;
        self.pos += 5; // `uuid(`
        if self.old_syntax {
            self.error(Span::new(start, self.pos), "SNBT functions require Minecraft 1.21.5 or newer.");
        }
        self.skip_ws();
        let mut ints = [0i32; 4];
        if let Some((arg, tag)) = self.parse_entry(depth) {
            if tag != SnbtTag::String {
                self.error(arg.span, "uuid() requires a string argument.");
            } else if let JsonKind::String(s) = &arg.kind {
                match parse_java_uuid(s.value.trim()) {
                    Some(parsed) => ints = parsed,
                    None => self.error(arg.span, format!("\"{}\" is not a valid UUID.", s.value)),
                }
            }
        } else {
            self.error(Span::new(self.pos, self.pos), "Expected a UUID string argument for uuid().");
        }
        self.skip_ws();
        if self.peek() == Some(b')') {
            self.pos += 1;
        } else {
            self.error(Span::new(self.pos, self.pos), "Expected ')' to close uuid().");
        }
        let span = Span::new(start, self.pos);
        let items = ints
            .into_iter()
            .map(|v| JsonNode {
                span,
                kind: JsonKind::Number { raw: v.to_string(), value: v as f64, is_int: true, int_value: Some(i64::from(v)) },
            })
            .collect();
        (JsonNode { span, kind: JsonKind::Array(items) }, SnbtTag::IntArray)
    }

    fn parse_primitive(&mut self) -> Option<(JsonNode, SnbtTag)> {
        self.skip_ws();
        let b = self.peek()?;
        if b == b'"' || b == b'\'' {
            let s = self.parse_quoted_string();
            return Some((JsonNode { span: s.span, kind: JsonKind::String(s) }, SnbtTag::String));
        }

        let start = self.pos;
        while self.pos < self.bytes.len() && is_brigadier_unquoted(self.bytes[self.pos]) {
            self.pos += 1;
        }
        if self.pos == start {
            return None;
        }
        let span = Span::new(start, self.pos);
        let raw = &self.src[start..self.pos];

        if raw.eq_ignore_ascii_case("true") {
            return Some((JsonNode { span, kind: JsonKind::Boolean(true) }, SnbtTag::Byte));
        }
        if raw.eq_ignore_ascii_case("false") {
            return Some((JsonNode { span, kind: JsonKind::Boolean(false) }, SnbtTag::Byte));
        }

        let diags_before = self.diagnostics.len();
        if let Some(parsed) = self.try_parse_number(raw, span) {
            return Some(parsed);
        }

        // Unquoted string: in 1.21.5+, unquoted strings may not start with `[0-9.+-]` (skip if
        // number parsing already reported an out-of-range diagnostic on this token).
        if !self.old_syntax && self.diagnostics.len() == diags_before {
            if raw.as_bytes().first().is_some_and(|c| c.is_ascii_digit() || matches!(c, b'.' | b'+' | b'-')) {
                self.error(span, "Unquoted SNBT strings cannot start with a digit, '.', '+', or '-' in Minecraft 1.21.5+.");
            }
        }
        let s = JsonString { span, value: raw.to_string(), closed: true, map: StringMap::new(start) };
        Some((JsonNode { span, kind: JsonKind::String(s) }, SnbtTag::String))
    }

    fn try_parse_number(&mut self, raw: &str, span: Span) -> Option<(JsonNode, SnbtTag)> {
        let has_underscore = raw.contains('_');
        let unsigned = raw.strip_prefix(['+', '-']).unwrap_or(raw);
        if unsigned.len() > 2 && unsigned.starts_with('0') && matches!(unsigned.as_bytes()[1], b'x' | b'X' | b'b' | b'B') {
            return self.try_parse_radix_number(raw, span);
        }

        // Decimal integers: `[-+]?(?:0|[1-9](?:_?[0-9])*)[bsil]?`
        if let Some(res) = self.try_parse_decimal_int(raw, span, has_underscore) {
            return Some(res);
        }

        // Decimal floats/doubles
        self.try_parse_decimal_float(raw, span, has_underscore)
    }

    // Mirrors vanilla's `SnbtGrammar`. The digit run is greedy, and hex digits include `b`, `d` and `f`,
    // so hex bytes need a signedness marker (`0x1Fub`). Hex and binary are unsigned unless the suffix
    // starts with `s`, unsuffixed literals are ints, and float suffixes don't apply.
    fn try_parse_radix_number(&mut self, raw: &str, span: Span) -> Option<(JsonNode, SnbtTag)> {
        let (negative, body) = match raw.as_bytes().first().copied()? {
            b'-' => (true, &raw[1..]),
            b'+' => (false, &raw[1..]),
            _ => (false, raw),
        };
        let is_hex = body.as_bytes().get(1)?.eq_ignore_ascii_case(&b'x');
        let after_prefix = &body[2..];
        let run_len = after_prefix
            .bytes()
            .take_while(|&b| if is_hex { b.is_ascii_hexdigit() || b == b'_' } else { matches!(b, b'0' | b'1' | b'_') })
            .count();
        let (digits, suffix) = after_prefix.split_at(run_len);
        if !is_valid_digit_run(digits, is_hex) {
            return None;
        }
        let (signed, type_suffix) = match suffix.to_ascii_lowercase().as_bytes() {
            [] => (false, b'i'),
            [t @ (b'b' | b's' | b'i' | b'l')] => (false, *t),
            [sign @ (b'u' | b's'), t @ (b'b' | b's' | b'i' | b'l')] => (*sign == b's', *t),
            _ => return None,
        };
        if self.old_syntax {
            self.error(span, "Hex and binary literals require Minecraft 1.21.5 or newer.");
        }
        if negative && !signed {
            self.error(span, "Minecraft reads hex and binary numbers as unsigned, so they can't be negative without a signed suffix like `sb` or `si`.");
            return None;
        }
        let clean: String = digits.chars().filter(|&c| c != '_').collect();
        let Ok(magnitude) = i128::from_str_radix(&clean, if is_hex { 16 } else { 2 }) else {
            self.warn(span, format!("Number {raw} is out of range; Minecraft reads it as a string."));
            return None;
        };
        let val = if negative { -magnitude } else { magnitude };
        let (tag, signed_range, unsigned_max) = match type_suffix {
            b'b' => (SnbtTag::Byte, i8::MIN as i128..=i8::MAX as i128, u8::MAX as i128),
            b's' => (SnbtTag::Short, i16::MIN as i128..=i16::MAX as i128, u16::MAX as i128),
            b'i' => (SnbtTag::Int, i32::MIN as i128..=i32::MAX as i128, u32::MAX as i128),
            _ => (SnbtTag::Long, i64::MIN as i128..=i64::MAX as i128, u64::MAX as i128),
        };
        let in_range = if signed { signed_range.contains(&val) } else { (0..=unsigned_max).contains(&val) };
        if !in_range {
            self.warn(span, format!("Number {raw} is out of range for {}; Minecraft reads it as a string.", tag.label()));
            return None;
        }
        // Vanilla stores unsigned values in the signed type, so `0xFFub` is the byte -1.
        let stored = if signed || val <= *signed_range.end() { val } else { val - (unsigned_max + 1) };
        Some((JsonNode { span, kind: JsonKind::Number { raw: raw.to_string(), value: stored as f64, is_int: true, int_value: Some(stored as i64) } }, tag))
    }

    fn try_parse_decimal_int(&mut self, raw: &str, span: Span, has_underscore: bool) -> Option<(JsonNode, SnbtTag)> {
        let (neg, body) = match raw.as_bytes().first().copied()? {
            b'-' => (true, &raw[1..]),
            b'+' => (false, &raw[1..]),
            _ => (false, raw),
        };
        if body.is_empty() {
            return None;
        }
        let (digits, suffix) = match body.as_bytes().last().copied()? {
            c if matches!(c.to_ascii_lowercase(), b'b' | b's' | b'i' | b'l') => (&body[..body.len() - 1], Some(c.to_ascii_lowercase())),
            _ => (body, None),
        };
        if !is_valid_decimal_int_digits(digits) {
            return None;
        }
        let clean: String = digits.chars().filter(|&c| c != '_').collect();
        let Ok(mag) = clean.parse::<i128>() else {
            self.warn(span, format!("Number {raw} is out of range; Minecraft reads it as a string."));
            return None;
        };
        let val = if neg { -mag } else { mag };
        let (tag, in_range) = match suffix {
            Some(b'b') => (SnbtTag::Byte, (i8::MIN as i128..=i8::MAX as i128).contains(&val)),
            Some(b's') => (SnbtTag::Short, (i16::MIN as i128..=i16::MAX as i128).contains(&val)),
            Some(b'i') | None => (SnbtTag::Int, (i32::MIN as i128..=i32::MAX as i128).contains(&val)),
            Some(b'l') => (SnbtTag::Long, (i64::MIN as i128..=i64::MAX as i128).contains(&val)),
            _ => unreachable!(),
        };
        if !in_range {
            self.warn(span, format!("Number {raw} is out of range for {}; Minecraft reads it as a string.", tag.label()));
            return None;
        }
        if self.old_syntax && suffix == Some(b'i') {
            self.error(span, "Explicit integer suffix 'i' requires Minecraft 1.21.5 or newer.");
        }
        if self.old_syntax && has_underscore {
            self.info(span, "Numeric underscore separators require Minecraft 1.21.5 or newer.");
        }
        Some((JsonNode { span, kind: JsonKind::Number { raw: raw.to_string(), value: val as f64, is_int: true, int_value: Some(val as i64) } }, tag))
    }

    fn try_parse_decimal_float(&mut self, raw: &str, span: Span, has_underscore: bool) -> Option<(JsonNode, SnbtTag)> {
        let body = raw.strip_prefix(['+', '-']).unwrap_or(raw);
        if body.is_empty() {
            return None;
        }
        let (num_part, suffix) = match body.as_bytes().last().copied()? {
            b'f' | b'F' => (&body[..body.len() - 1], Some(b'f')),
            b'd' | b'D' => (&body[..body.len() - 1], Some(b'd')),
            _ => (body, None),
        };
        if !is_valid_decimal_float(num_part) {
            return None;
        }
        let clean: String = raw[..raw.len() - usize::from(suffix.is_some())].chars().filter(|&c| c != '_').collect();
        let Ok(val) = clean.parse::<f64>() else {
            return None;
        };
        let tag = if suffix == Some(b'f') { SnbtTag::Float } else { SnbtTag::Double };
        let in_range = if tag == SnbtTag::Float { val.is_finite() && val.abs() <= f32::MAX as f64 } else { val.is_finite() };
        if !in_range {
            self.warn(span, format!("Number {raw} is out of range for {}; Minecraft reads it as a string.", tag.label()));
            return None;
        }
        if self.old_syntax && has_underscore {
            self.info(span, "Numeric underscore separators require Minecraft 1.21.5 or newer.");
        }
        Some((JsonNode { span, kind: JsonKind::Number { raw: raw.to_string(), value: val, is_int: false, int_value: None } }, tag))
    }

    fn parse_string_token(&mut self, allow_unquoted: bool) -> Option<JsonString> {
        self.skip_ws();
        let b = self.peek()?;
        if b == b'"' || b == b'\'' {
            return Some(self.parse_quoted_string());
        }
        if !allow_unquoted {
            return None;
        }
        let start = self.pos;
        while self.pos < self.bytes.len() && is_brigadier_unquoted(self.bytes[self.pos]) {
            self.pos += 1;
        }
        if self.pos == start {
            return None;
        }
        let span = Span::new(start, self.pos);
        Some(JsonString { span, value: self.src[start..self.pos].to_string(), closed: true, map: StringMap::new(start) })
    }

    fn parse_quoted_string(&mut self) -> JsonString {
        let start = self.pos;
        let quote = self.bytes[self.pos];
        self.pos += 1;
        let content_start = self.pos;
        let mut out = String::new();
        let mut map = StringMap::new(content_start);
        let mut closed = false;

        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            if b == quote {
                self.pos += 1;
                closed = true;
                break;
            }
            if b == b'\\' {
                let esc_start = self.pos;
                let u_before = out.len();
                self.pos += 1;
                let Some(esc) = self.peek() else {
                    self.error(Span::new(esc_start, self.pos), "Incomplete escape sequence.");
                    break;
                };
                self.pos += 1;
                match esc {
                    b'\\' => out.push('\\'),
                    b'"' => out.push('"'),
                    b'\'' => out.push('\''),
                    b'b' => out.push('\x08'),
                    b'f' => out.push('\x0C'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b's' => out.push(' '),
                    b't' => out.push('\t'),
                    b'x' => self.parse_hex_escape(esc_start, 2, &mut out),
                    b'u' => self.parse_hex_escape(esc_start, 4, &mut out),
                    b'U' => self.parse_hex_escape(esc_start, 8, &mut out),
                    b'N' => self.parse_named_unicode_escape(esc_start, &mut out),
                    _ => {
                        self.error(Span::new(esc_start, self.pos), format!("Invalid escape sequence '\\{}'.", esc as char));
                    }
                }
                map.push_escape(u_before, esc_start, out.len(), self.pos);
                continue;
            }
            let ch = self.src[self.pos..].chars().next().unwrap();
            self.pos += ch.len_utf8();
            out.push(ch);
        }

        if !closed {
            self.error(Span::new(start, self.pos), "Unclosed SNBT string.");
        }
        JsonString { span: Span::new(start, self.pos), value: out, closed, map }
    }

    fn parse_hex_escape(&mut self, esc_start: usize, digits: usize, out: &mut String) {
        let hex_start = self.pos;
        let hex_end = (hex_start + digits).min(self.bytes.len());
        let slice = &self.src[hex_start..hex_end];
        if slice.len() == digits && slice.bytes().all(|b| b.is_ascii_hexdigit()) {
            self.pos = hex_end;
            if self.old_syntax {
                self.error(Span::new(esc_start, self.pos), "Extended Unicode escapes require Minecraft 1.21.5 or newer.");
            }
            match u32::from_str_radix(slice, 16).ok().and_then(char::from_u32) {
                Some(ch) => out.push(ch),
                None => self.error(Span::new(esc_start, self.pos), "Invalid Unicode code point."),
            }
        } else {
            // Advance past any partial hex digits.
            while self.pos < hex_end && self.bytes[self.pos].is_ascii_hexdigit() {
                self.pos += 1;
            }
            self.error(Span::new(esc_start, self.pos), format!("Expected {digits} hexadecimal digits in Unicode escape."));
        }
    }

    fn parse_named_unicode_escape(&mut self, esc_start: usize, out: &mut String) {
        if self.peek() != Some(b'{') {
            self.error(Span::new(esc_start, self.pos), "Expected '{' after '\\N'.");
            return;
        }
        self.pos += 1;
        let name_start = self.pos;
        let mut closed = false;
        while self.pos < self.bytes.len() {
            if self.bytes[self.pos] == b'}' {
                closed = true;
                break;
            }
            self.pos += 1;
        }
        let name = self.src[name_start..self.pos].trim();
        if closed {
            self.pos += 1;
        } else {
            self.error(Span::new(esc_start, self.pos), "Unclosed '\\N{...}' Unicode escape.");
            return;
        }
        if self.old_syntax {
            self.error(Span::new(esc_start, self.pos), "Extended Unicode escapes require Minecraft 1.21.5 or newer.");
        } else if !is_valid_unicode_escape_name(name) {
            self.error(Span::new(esc_start, self.pos), format!("Invalid Unicode character name \"{name}\"."));
        }
        out.push(char::REPLACEMENT_CHARACTER);
    }
}

fn is_brigadier_unquoted(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'+')
}

fn is_valid_digit_run(s: &str, is_hex: bool) -> bool {
    let bytes = s.as_bytes();
    if bytes.is_empty() || bytes[0] == b'_' || bytes[bytes.len() - 1] == b'_' {
        return false;
    }
    let mut prev_underscore = false;
    for &b in bytes {
        if b == b'_' {
            if prev_underscore {
                return false;
            }
            prev_underscore = true;
        } else {
            prev_underscore = false;
            let ok = if is_hex { b.is_ascii_hexdigit() } else { matches!(b, b'0' | b'1') };
            if !ok {
                return false;
            }
        }
    }
    true
}

fn is_valid_decimal_int_digits(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    if bytes == b"0" {
        return true;
    }
    if !matches!(bytes[0], b'1'..=b'9') || bytes[bytes.len() - 1] == b'_' {
        return false;
    }
    let mut prev_underscore = false;
    for &b in &bytes[1..] {
        if b == b'_' {
            if prev_underscore {
                return false;
            }
            prev_underscore = true;
        } else if b.is_ascii_digit() {
            prev_underscore = false;
        } else {
            return false;
        }
    }
    true
}

fn is_valid_decimal_float(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let (mantissa, exp) = match s.find(['e', 'E']) {
        Some(idx) => (&s[..idx], Some(&s[idx + 1..])),
        None => (s, None),
    };
    if let Some(exp_str) = exp {
        let exp_digits = exp_str.strip_prefix(['+', '-']).unwrap_or(exp_str);
        if !is_valid_decimal_int_digits(exp_digits) {
            return false;
        }
    }
    let mantissa_ok = match mantissa.split_once('.') {
        Some((int_part, frac_part)) => {
            if int_part.is_empty() && frac_part.is_empty() {
                false
            } else {
                let int_ok = int_part.is_empty() || is_valid_decimal_digit_seq(int_part);
                let frac_ok = frac_part.is_empty() || is_valid_decimal_digit_seq(frac_part);
                int_ok && frac_ok
            }
        }
        None => is_valid_decimal_int_digits(mantissa),
    };
    mantissa_ok
}

fn is_valid_decimal_digit_seq(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.is_empty() || bytes[0] == b'_' || bytes[bytes.len() - 1] == b'_' {
        return false;
    }
    let mut prev_underscore = false;
    for &b in bytes {
        if b == b'_' {
            if prev_underscore {
                return false;
            }
            prev_underscore = true;
        } else if b.is_ascii_digit() {
            prev_underscore = false;
        } else {
            return false;
        }
    }
    true
}

fn is_valid_unicode_escape_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    name.split_ascii_whitespace()
        .all(|word| !word.is_empty() && word.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
}

// ============================================================================
// NBT Path validator (`#[nbt_path]`)
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathPart {
    End,
    Filter,
    Index,
    Key,
}

impl PathPart {
    fn label(self) -> &'static str {
        match self {
            Self::End => "end of path",
            Self::Filter => "a compound filter",
            Self::Index => "an index",
            Self::Key => "a key",
        }
    }
}

/// Validates NBT path syntax matching Spyglass `packages/nbt/src/parser/path.ts`.
pub fn validate_nbt_path(src: &str, minecraft: Option<&str>) -> Vec<Diagnostic> {
    let old_syntax = is_old_snbt_syntax(minecraft);
    let mut parser = SnbtParser::new(src, old_syntax);

    let next_part = |p: &SnbtParser<'_>| match p.peek() {
        None | Some(b' ' | b'\n' | b'\r' | b'\t') => PathPart::End,
        Some(b'{') => PathPart::Filter,
        Some(b'[') => PathPart::Index,
        Some(_) => PathPart::Key,
    };

    let mut expected: &[PathPart] = &[PathPart::Filter, PathPart::Key];
    let mut current = next_part(&parser);
    let mut last_pos = usize::MAX;

    while last_pos != parser.pos {
        if !expected.contains(&current) {
            let exp_text: Vec<&str> = expected.iter().map(|p| p.label()).collect();
            parser.error(
                Span::new(parser.pos, (parser.pos + 1).min(parser.bytes.len())),
                format!("Expected {}, found {}.", exp_text.join(" or "), current.label()),
            );
        }
        if current == PathPart::End {
            break;
        }
        last_pos = parser.pos;
        expected = match current {
            PathPart::Filter => {
                let _ = parser.parse_compound(0);
                if parser.peek() == Some(b'.') {
                    parser.pos += 1;
                    &[PathPart::Key]
                } else {
                    &[PathPart::End]
                }
            }
            PathPart::Index => {
                parser.pos += 1; // '['
                match parser.peek() {
                    Some(b'{') => {
                        let _ = parser.parse_compound(0);
                    }
                    Some(b']') => {}
                    _ => {
                        let idx_start = parser.pos;
                        if parser.peek() == Some(b'-') {
                            parser.pos += 1;
                        }
                        let digits_start = parser.pos;
                        while parser.peek().is_some_and(|b| b.is_ascii_digit()) {
                            parser.pos += 1;
                        }
                        if parser.pos == digits_start {
                            while parser.peek().is_some_and(|b| !matches!(b, b']' | b' ' | b'\t' | b'\r' | b'\n')) {
                                parser.pos += 1;
                            }
                            parser.error(Span::new(idx_start, parser.pos), "Expected an integer index or compound filter.");
                        }
                    }
                }
                if parser.peek() == Some(b']') {
                    parser.pos += 1;
                } else {
                    parser.error(Span::new(parser.pos, parser.pos), "Expected closing ']'.");
                }
                if parser.peek() == Some(b'.') {
                    parser.pos += 1;
                    &[PathPart::Index, PathPart::Key]
                } else {
                    &[PathPart::End, PathPart::Index]
                }
            }
            PathPart::Key => {
                if matches!(parser.peek(), Some(b'"' | b'\'')) {
                    let _ = parser.parse_quoted_string();
                } else {
                    let key_start = parser.pos;
                    while let Some(b) = parser.peek() {
                        if matches!(b, b'\n' | b'\r' | b'\t' | b' ' | b'"' | b'[' | b']' | b'.' | b'{' | b'}') {
                            break;
                        }
                        parser.pos += 1;
                    }
                    if parser.pos == key_start {
                        parser.error(Span::new(key_start, (key_start + 1).min(parser.bytes.len())), "Expected an NBT key.");
                        break;
                    }
                }
                if parser.peek() == Some(b'.') {
                    parser.pos += 1;
                    &[PathPart::Index, PathPart::Key]
                } else {
                    &[PathPart::End, PathPart::Filter, PathPart::Index]
                }
            }
            PathPart::End => break,
        };
        current = next_part(&parser);
    }

    if parser.pos < parser.bytes.len() {
        parser.error(Span::new(parser.pos, parser.bytes.len()), "Unexpected trailing content in NBT path.");
    }
    parser.diagnostics
}

// ============================================================================
// Block Predicate validator (`#[block_predicate]`)
// ============================================================================

/// Validates a pre-1.20.5 block predicate string (`<block_or_tag>[state=val]{nbt}`) matching
/// Spyglass `packages/java-edition/src/mcfunction/parser/argument.ts`.
pub fn validate_block_predicate(src: &str, env: &CheckEnv<'_>) -> Vec<Diagnostic> {
    let old_syntax = is_old_snbt_syntax(env.version.minecraft.as_deref());
    let mut parser = SnbtParser::new(src, old_syntax);

    // 1. Block ID or #tag.
    let id_start = parser.pos;
    while let Some(b) = parser.peek() {
        if matches!(b, b'[' | b'{' | b' ' | b'\t' | b'\r' | b'\n') {
            break;
        }
        parser.pos += 1;
    }
    let id_span = Span::new(id_start, parser.pos);
    let raw_id = &src[id_start..parser.pos];
    let is_tag = raw_id.starts_with('#');
    let body = raw_id.strip_prefix('#').unwrap_or(raw_id);
    if raw_id.is_empty() || !is_valid_resource_location(body) {
        parser.error(id_span, format!("\"{raw_id}\" is not a valid block or block tag."));
        return parser.diagnostics;
    }
    let canonical_id = lengthen_id(body);
    match env.registries.lookup("block", raw_id, true, false) {
        IdLookup::Known => {}
        IdLookup::UnknownAuthoritative => {
            parser.warn(id_span, format!("Unknown block \"{raw_id}\"."));
        }
        IdLookup::UnknownUnverified => {
            parser.warn(
                id_span,
                format!("Packraven doesn't know this block ID (\"{raw_id}\"), but it might exist. Run a registry dump to verify modded IDs."),
            );
        }
    }

    // 2. Optional block state predicate `[key=val, ...]`.
    if parser.peek() == Some(b'[') {
        parser.pos += 1;
        let block_props = if !is_tag { env.registries.block_states.get(&canonical_id) } else { None };
        let mut seen_props = HashSet::new();
        loop {
            parser.skip_ws();
            match parser.peek() {
                None => {
                    parser.error(Span::new(parser.pos, parser.pos), "Expected closing ']' in block state predicate.");
                    break;
                }
                Some(b']') => {
                    parser.pos += 1;
                    break;
                }
                _ => {}
            }
            let Some(prop_key) = parser.parse_string_token(true) else {
                parser.error(Span::new(parser.pos, (parser.pos + 1).min(parser.bytes.len())), "Expected a block state property name.");
                break;
            };
            if !seen_props.insert(prop_key.value.clone()) {
                parser.error(prop_key.span, format!("Duplicate block state property \"{}\".", prop_key.value));
            }
            parser.skip_ws();
            if parser.peek() != Some(b'=') {
                parser.error(Span::new(parser.pos, parser.pos), "Expected '=' after block state property.");
                break;
            }
            parser.pos += 1;
            parser.skip_ws();
            let Some(prop_val) = parser.parse_string_token(true) else {
                parser.error(Span::new(parser.pos, (parser.pos + 1).min(parser.bytes.len())), "Expected a block state value.");
                break;
            };
            if let Some(props) = block_props {
                match props.get(&prop_key.value) {
                    None => parser.warn(prop_key.span, format!("Block \"{canonical_id}\" has no property \"{}\".", prop_key.value)),
                    Some(allowed) if !allowed.iter().any(|v| v == &prop_val.value) => {
                        parser.warn(
                            prop_val.span,
                            format!("Invalid value \"{}\" for block state property \"{}\".", prop_val.value, prop_key.value),
                        );
                    }
                    _ => {}
                }
            }
            parser.skip_ws();
            match parser.peek() {
                Some(b',') => parser.pos += 1,
                Some(b']') => {
                    parser.pos += 1;
                    break;
                }
                None => {
                    parser.error(Span::new(parser.pos, parser.pos), "Expected closing ']' in block state predicate.");
                    break;
                }
                _ => {
                    parser.error(Span::new(parser.pos, parser.pos), "Expected ',' or ']' in block state predicate.");
                    break;
                }
            }
        }
    }

    // 3. Optional block-entity SNBT compound `{...}`.
    if parser.peek() == Some(b'{') {
        let compound = parser.parse_compound(0);
        if env.tables.dispatchers.contains_key("minecraft:block") {
            let idx = if is_tag { "%fallback".to_string() } else { canonical_id };
            let target = McdocType::Dispatcher {
                registry: "minecraft:block".into(),
                parallel_indices: vec![Index::Static { value: idx }],
                attributes: Vec::new(),
            };
            let nbt_env = CheckEnv { allow_missing_keys: true, ..*env };
            let checked = check_nbt_with_type(&compound, &target, &nbt_env);
            parser.diagnostics.extend(checked.diagnostics);
        }
    }

    if parser.pos < parser.bytes.len() {
        parser.error(Span::new(parser.pos, parser.bytes.len()), "Unexpected trailing content in block predicate.");
    }
    parser.diagnostics
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcdoc::parser::parse_and_bind;
    use crate::mcdoc::registries::RegistryIndex;
    use crate::mcdoc::types::SymbolTables;
    use crate::mcdoc::version::VersionContext;
    use std::collections::BTreeMap;

    #[test]
    fn parses_snbt_with_1_21_5_version_gating() {
        // Basic compound, arrays, numbers, booleans, and trailing commas
        let ok = parse_snbt(r#"{a: 1b, b: 2s, c: 3, d: 4l, e: 1.5f, f: 2.5d, g: true, h: [B; 1b, 0b], i: [I; 1, 2], j: [L; 3l], k: 'hi',}"#, Some("1.21.1"));
        assert!(ok.diagnostics.is_empty(), "{:?}", ok.diagnostics);
        assert!(ok.root.is_some());

        // 1.21.5+ features: hex/bin, underscores, `i` suffix, bool()/uuid(), extended escapes, heterogeneous lists
        let modern = r#"{hex: 0xFF, bin: 0b0101_0001b, exp_i: 42i, sep: 1_000, b: bool(1), id: uuid("0-0-0-0-1"), mix: [1, "a"], esc: "\x41\u0042\U00000043\N{SNOWMAN}"}"#;
        let modern_ok = parse_snbt(modern, Some("1.21.5"));
        assert!(modern_ok.diagnostics.is_empty(), "{:?}", modern_ok.diagnostics);

        let old = parse_snbt(modern, Some("1.21.1"));
        assert!(old.diagnostics.iter().any(|d| d.message.contains("Hex and binary")), "{:?}", old.diagnostics);
        assert!(old.diagnostics.iter().any(|d| d.message.contains("Explicit integer suffix")), "{:?}", old.diagnostics);
        assert!(old.diagnostics.iter().any(|d| d.message.contains("SNBT functions")), "{:?}", old.diagnostics);
        assert!(old.diagnostics.iter().any(|d| d.message.contains("in list before Minecraft 1.21.5")), "{:?}", old.diagnostics);
        assert!(old.diagnostics.iter().any(|d| d.message.contains("Extended Unicode escapes")), "{:?}", old.diagnostics);
        assert!(old.diagnostics.iter().any(|d| d.severity == Severity::Info && d.message.contains("underscore")), "{:?}", old.diagnostics);

        // 1.21.5+ rejects negative radix and unquoted strings starting with digits
        let neg_hex = parse_snbt("{a: -0xFF, b: 123abc}", Some("1.21.5"));
        assert!(neg_hex.diagnostics.iter().any(|d| d.message.contains("can't be negative")), "{:?}", neg_hex.diagnostics);
        // Hex digits include `b`, so `0x10b` and `0x10_b` are ints. Bytes need `ub`/`sb`, and `s` allows a sign.
        let radix = parse_snbt("{a: 0x10b, b: 0x10_b, c: 0xFFub, d: -0x1Fsb, e: 0b11s, f: 0xFFFFFFFF}", Some("1.21.5"));
        assert!(radix.diagnostics.is_empty(), "{:?}", radix.diagnostics);
        let radix_bad = parse_snbt("{a: 0x100ub, b: 0x80sb, c: 0b1f}", Some("1.21.5"));
        assert_eq!(radix_bad.diagnostics.iter().filter(|d| d.message.contains("out of range")).count(), 2, "{:?}", radix_bad.diagnostics);
        assert!(radix_bad.diagnostics.iter().any(|d| d.message.contains("cannot start with a digit")), "{:?}", radix_bad.diagnostics);
        assert!(neg_hex.diagnostics.iter().any(|d| d.message.contains("cannot start with a digit")), "{:?}", neg_hex.diagnostics);

        // Typed array mismatch and out-of-range byte
        let bad_arr = parse_snbt("{a: [B; 1], b: 200b}", Some("1.21.5"));
        assert!(bad_arr.diagnostics.iter().any(|d| d.severity == Severity::Error && d.message.contains("Expected a byte")), "{:?}", bad_arr.diagnostics);
        assert!(bad_arr.diagnostics.iter().any(|d| d.severity == Severity::Warning && d.message.contains("out of range")), "{:?}", bad_arr.diagnostics);
    }

    #[test]
    fn validates_nbt_paths() {
        for valid in [
            "Items[0].id",
            "Inventory[{Slot: 0b}].tag.display.Name",
            "{OnGround: 1b}.Pos[0]",
            "foo.\"bar.baz\"[0][1]",
            "a.'single quoted'[]",
        ] {
            let diags = validate_nbt_path(valid, Some("1.21.1"));
            assert!(diags.is_empty(), "expected valid path {valid:?}, got {diags:?}");
        }
        for invalid in ["", ".foo", "foo..bar", "foo[abc]", "foo[0", "foo bar", "foo.{a:1}"] {
            let diags = validate_nbt_path(invalid, Some("1.21.1"));
            assert!(!diags.is_empty(), "expected invalid path {invalid:?} to fail");
        }
    }

    #[test]
    fn validates_block_predicates() {
        let mut tables = SymbolTables::default();
        parse_and_bind(r#"dispatch minecraft:block["minecraft:chest"] to struct { Lock?: string }"#, "::test", &mut tables).unwrap();
        let mut registries = RegistryIndex::default();
        registries.insert_entries("block", ["minecraft:chest".into(), "minecraft:stone".into()]);
        registries.insert_entries("tag/block", ["minecraft:logs".into()]);
        let mut chest_props = BTreeMap::new();
        chest_props.insert("facing".into(), vec!["north".into(), "south".into()]);
        registries.block_states.insert("minecraft:chest".into(), chest_props);
        registries.finalize();
        let version = VersionContext { minecraft: Some("1.20.1".into()), neoforge: None };
        let env = CheckEnv {
            tables: &tables,
            registries: &registries,
            version: &version,
            require_canonical: false,
            allow_missing_keys: false,
            primitives_read_as_strings: false,
            ignored_loaders: &[],
        };

        assert!(validate_block_predicate("minecraft:chest[facing=north]{Lock: \"key\"}", &env).is_empty());
        assert!(validate_block_predicate("#minecraft:logs", &env).is_empty());
        assert!(!validate_block_predicate("minecraft:chest[facing=up]", &env).is_empty());
        assert!(!validate_block_predicate("minecraft:chest[nope=true]", &env).is_empty());
        assert!(!validate_block_predicate("minecraft:chest{Lock: 123}", &env).is_empty());
        assert!(!validate_block_predicate("not valid!", &env).is_empty());
    }
}
