#![allow(dead_code)]

use std::collections::HashSet;

use serde::Serialize;

use super::checker::{ArenaKind, CheckEnv, CheckOutput, JsonArena, JsonId};
use super::registries::normalize_category;
use super::types::{EnumValue, LiteralValue, McdocType, NumericRange, RangeKind, StructField, StructKey, SymbolTables};
use super::version::{attr_string, parse_id_attribute, TagMode};

const MAX_COMPLETIONS: usize = 100;

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CompletionItem {
    pub label: String,
    pub kind: &'static str,
    pub detail: Option<String>,
    pub signature: Option<String>,
    pub docs: Option<String>,
    pub insert_text: Option<String>,
    /// UTF-16 offset relative to `CompletionResult::from` where the cursor should land after insertion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor_offset: Option<usize>,
    /// UTF-16 length to select, ending at the cursor, so typing replaces a placeholder like `0.0`.
    #[serde(skip_serializing_if = "is_zero")]
    pub select_len: usize,
    /// When true, consume a closing `"` immediately following `to` so key scaffolding closes the string cleanly.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub consume_closing_quote: bool,
    /// When true, the editor should immediately open completions at the new cursor position.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub retrigger: bool,
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CompletionResult {
    pub from: usize,
    pub to: usize,
    /// End of the existing word or string content after the cursor, which accepting a completion replaces too.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replace_to: Option<usize>,
    pub items: Vec<CompletionItem>,
    pub in_string: bool,
    /// True when the string under the cursor is a key or a value with a fixed set of choices, even if
    /// nothing matches what you've typed. The editor only opens completions on caret moves into such strings.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub slot_has_choices: bool,
}

impl CompletionResult {
    pub fn empty(utf16_offset: usize) -> Self {
        Self { from: utf16_offset, to: utf16_offset, replace_to: None, items: Vec::new(), in_string: false, slot_has_choices: false }
    }

    fn new(text: &str, from: usize, to: usize, replace_to: usize, items: Vec<CompletionItem>, in_string: bool) -> Self {
        let replace_to = (replace_to > to).then(|| byte_to_utf16(text, replace_to));
        Self { from: byte_to_utf16(text, from), to: byte_to_utf16(text, to), replace_to, items, in_string, slot_has_choices: false }
    }
}

pub struct CompleteOptions<'a> {
    /// Re-checks an edited copy of the document. We use it to learn the resolved type of a value slot.
    pub recheck: &'a dyn Fn(&str) -> Option<CheckOutput>,
    /// Loaders whose fields we accept when validating but don't suggest, like Fabric's in a NeoForge pack
    /// without Forgified Fabric API. See [`super::checker::LOADER_ATTR`].
    pub ignored_loaders: &'a [&'a str],
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct HoverInfo {
    pub from: usize,
    pub to: usize,
    pub signature: String,
    pub docs: String,
}

pub fn utf16_to_byte(text: &str, utf16_offset: usize) -> usize {
    let mut u16_pos = 0usize;
    for (byte_idx, ch) in text.char_indices() {
        if u16_pos >= utf16_offset {
            return byte_idx;
        }
        u16_pos += ch.len_utf16();
    }
    text.len()
}

pub fn byte_to_utf16(text: &str, byte_offset: usize) -> usize {
    let clamped = byte_offset.min(text.len());
    let mut end = clamped;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].encode_utf16().count()
}

pub fn complete(text: &str, utf16_offset: usize, check: &CheckOutput, env: &CheckEnv<'_>, opts: &CompleteOptions<'_>) -> CompletionResult {
    let pos = utf16_to_byte(text, utf16_offset).min(text.len());
    let empty = CompletionResult::empty(utf16_offset);
    let Some(root_id) = check.arena.root else { return empty };
    let Some(target_id) = deepest_node_at(&check.arena, root_id, pos) else { return empty };
    let node = check.arena.get(target_id);

    // 1. Inside a string node: could be an object key OR a string value.
    if let ArenaKind::String(s) = &node.kind {
        let in_content = pos >= s.map.content_start && (pos < s.span.end || !s.closed);
        if in_content {
            let typed = &text[s.map.content_start..pos];
            // Keep `to` at the cursor so CodeMirror matches labels against the typed prefix, never against a trailing `"`.
            let content_end = if s.closed { s.span.end.saturating_sub(1).max(pos) } else { pos };

            if let Some(parent_id) = node.parent
                && is_object_key(&check.arena, parent_id, target_id)
            {
                let has_colon = colon_follows_key(text, s.span.end);
                let mode = if has_colon {
                    KeyInsertMode::NameOnly
                } else {
                    let after_key = if s.closed { s.span.end } else { pos };
                    KeyInsertMode::CloseQuoteAndValue {
                        consume_closing_quote: s.closed,
                        trailing_comma: needs_trailing_comma(text, after_key),
                    }
                };
                let without_key = recheck_without_pair(text, check, parent_id, target_id, opts);
                let items = match &without_key {
                    Some((probe_check, probe_obj)) => complete_object_keys(*probe_obj, None, probe_check, env, typed, mode, opts),
                    None => complete_object_keys(parent_id, Some(target_id), check, env, typed, mode, opts),
                };
                let mut result = CompletionResult::new(text, s.map.content_start, pos, content_end, items, true);
                result.slot_has_choices = true;
                return result;
            }

            if let Some(info) = check.attached.get(&target_id) {
                let items = complete_values(&info.type_def, env, typed, true);
                let slot_has_choices = !items.is_empty() || (!typed.is_empty() && !complete_values(&info.type_def, env, "", true).is_empty());
                let mut result = CompletionResult::new(text, s.map.content_start, pos, content_end, items, true);
                result.slot_has_choices = slot_has_choices;
                return result;
            }
        }
    }

    let word_start = bare_word_start(text, pos);
    let word_end = bare_word_end(text, pos);

    // 2. Inside an object (between pairs, typing an unquoted key, or after a colon with no value yet).
    if let ArenaKind::Object(pairs) = &node.kind {
        // If cursor sits after a pair's colon (with no comma after the colon), we are strictly in value position.
        if let Some((key_id, None)) = pairs.iter().rev().find(|(kid, vid)| vid.is_none() && check.arena.get(*kid).span.end <= pos)
            && let ArenaKind::String(ks) = &check.arena.get(*key_id).kind
            && let Some(colon_rel) = text[ks.span.end.min(pos)..pos].find(':')
            && !text[ks.span.end + colon_rel + 1..pos].contains(',')
        {
            return complete_bare_value(text, word_start, pos, word_end, env, opts);
        }

        let typed = &text[word_start..pos];
        let mode = KeyInsertMode::QuotedAndValue { trailing_comma: needs_trailing_comma(text, word_end) };
        let items = complete_object_keys(target_id, None, check, env, typed, mode, opts);
        return CompletionResult::new(text, word_start, pos, word_end, items, false);
    }

    // The cursor sits just outside a string's quotes, where no value can start.
    if matches!(node.kind, ArenaKind::String(_)) {
        return empty;
    }

    // 3. Inside an array between elements, or 4. on a bare primitive token (boolean, number, null).
    if matches!(node.kind, ArenaKind::Array(_)) || check.attached.contains_key(&target_id) {
        return complete_bare_value(text, word_start, pos, word_end, env, opts);
    }

    empty
}

// The key you're still typing is already in the document, and the checker narrows unions by it: a
// partial `"ite"` rules out `{ item }` but not a member with dynamic keys, so we'd only offer that
// member's keys. We re-check without the pair to get the object's type from the other keys alone.
fn recheck_without_pair(text: &str, check: &CheckOutput, obj_id: JsonId, key_id: JsonId, opts: &CompleteOptions<'_>) -> Option<(CheckOutput, JsonId)> {
    let ArenaKind::Object(pairs) = &check.arena.get(obj_id).kind else { return None };
    let &(_, value_id) = pairs.iter().find(|(kid, _)| *kid == key_id)?;
    let key_span = check.arena.get(key_id).span;
    let end = match value_id {
        Some(v) => check.arena.get(v).span.end,
        None if colon_follows_key(text, key_span.end) => key_span.end + text[key_span.end..].find(':')? + 1,
        None => key_span.end,
    };
    let probe = format!("{}{}", &text[..key_span.start], &text[end.min(text.len())..]);
    let probe_check = (opts.recheck)(&probe)?;
    let obj_start = check.arena.get(obj_id).span.start;
    let probe_obj = probe_check
        .arena
        .nodes
        .iter()
        .position(|n| n.span.start == obj_start && matches!(n.kind, ArenaKind::Object(_)))
        .map(JsonId)?;
    Some((probe_check, probe_obj))
}

/// Completes an unquoted value. We quote the typed word, re-check the document, and complete against
/// the type the checker resolves for that string, since field types can still hold unsubstituted
/// generics (like a tag's entry type) until the checker places them at a node.
fn complete_bare_value(text: &str, word_start: usize, pos: usize, word_end: usize, env: &CheckEnv<'_>, opts: &CompleteOptions<'_>) -> CompletionResult {
    let typed = &text[word_start..pos];
    let probe = format!("{}\"{typed}\"{}", &text[..word_start], &text[word_end..]);
    let probe_pos = word_start + 1 + typed.len();
    let items = (opts.recheck)(&probe)
        .and_then(|probe_check| {
            let root = probe_check.arena.root?;
            let id = deepest_node_at(&probe_check.arena, root, probe_pos)?;
            let node = probe_check.arena.get(id);
            if !matches!(node.kind, ArenaKind::String(_)) || node.parent.is_some_and(|p| is_object_key(&probe_check.arena, p, id)) {
                return None;
            }
            let info = probe_check.attached.get(&id)?;
            Some(complete_values(&info.type_def, env, typed, false))
        })
        .unwrap_or_default();
    CompletionResult::new(text, word_start, pos, word_end, items, false)
}

pub fn hover(text: &str, utf16_offset: usize, check: &CheckOutput, tables: &SymbolTables) -> Option<HoverInfo> {
    let pos = utf16_to_byte(text, utf16_offset);
    let root_id = check.arena.root?;
    let target_id = deepest_node_at(&check.arena, root_id, pos)?;
    let node = check.arena.get(target_id);

    // If hovering on an object key, combine key + value type signature when available.
    if let Some(parent_id) = node.parent
        && let ArenaKind::Object(pairs) = &check.arena.get(parent_id).kind
        && let Some((_, val_id)) = pairs.iter().find(|(kid, _)| *kid == target_id)
    {
        let key_info = check.attached.get(&target_id);
        let val_info = val_id.and_then(|vid| check.attached.get(&vid));
        let desc = key_info
            .and_then(|i| i.desc.clone())
            .or_else(|| val_info.and_then(|i| i.desc.clone()))
            .unwrap_or_default();
        let desc = match val_info.filter(|i| super::checker::has_crafting_keys(&i.type_def)) {
            Some(_) => {
                let symbols = super::checker::pattern_symbols(check);
                let note = if symbols.is_empty() {
                    "Keys are single characters from `pattern`.".to_string()
                } else {
                    let list: Vec<String> = symbols.iter().map(|c| format!("`{c}`")).collect();
                    format!("Keys are single characters from `pattern`: {}.", list.join(", "))
                };
                if desc.is_empty() { note } else { format!("{desc}\n\n{note}") }
            }
            None => desc,
        };
        let deprecated = key_info.is_some_and(|i| i.deprecated) || val_info.is_some_and(|i| i.deprecated);
        let key_label = match &node.kind {
            ArenaKind::String(s) => format!("\"{}\"", s.value),
            _ => String::new(),
        };
        let val_sig = val_info.map(|i| type_label(&i.type_def, tables)).or_else(|| key_info.map(|i| type_label(&i.type_def, tables)))?;
        let signature = if deprecated {
            format!("{key_label}: {val_sig} (deprecated)")
        } else {
            format!("{key_label}: {val_sig}")
        };
        return Some(HoverInfo {
            from: byte_to_utf16(text, node.span.start),
            to: byte_to_utf16(text, node.span.end),
            signature,
            docs: desc,
        });
    }

    if matches!(node.kind, ArenaKind::Object(_) | ArenaKind::Array(_)) {
        return None;
    }

    let info = check.attached.get(&target_id)?;
    let signature = if info.deprecated {
        format!("{} (deprecated)", type_label(&info.type_def, tables))
    } else {
        type_label(&info.type_def, tables)
    };
    Some(HoverInfo {
        from: byte_to_utf16(text, node.span.start),
        to: byte_to_utf16(text, node.span.end),
        signature,
        docs: info.desc.clone().unwrap_or_default(),
    })
}

const MAX_LABEL_ENUM_VALUES: usize = 6;

/// Describes a type for hover and completion details. Unlike `Display`, which mirrors mcdoc syntax,
/// this drops version and canonical attributes and spells out what an ID field accepts.
pub fn type_label(ty: &McdocType, tables: &SymbolTables) -> String {
    label_at_depth(ty, tables, 0)
}

const MAX_LABEL_DEPTH: u8 = 6;

fn label_at_depth(ty: &McdocType, tables: &SymbolTables, depth: u8) -> String {
    if depth > MAX_LABEL_DEPTH {
        return "any".into();
    }
    let label = |t: &McdocType| label_at_depth(t, tables, depth + 1);
    let id = ty.attributes().iter().find_map(parse_id_attribute).and_then(|cfg| cfg.registry.map(|reg| (normalize_category(&reg), cfg.tags)));
    if let Some((reg, tags)) = id {
        return match tags {
            Some(TagMode::Allowed) => format!("{reg} ID or #tag"),
            Some(TagMode::Implicit) => format!("{reg} tag"),
            Some(TagMode::Required) => format!("#{reg} tag"),
            None => format!("{reg} ID"),
        };
    }
    match ty {
        McdocType::Struct { .. } => "object".into(),
        McdocType::Enum { values, .. } => {
            let mut labels: Vec<String> = values
                .iter()
                .take(MAX_LABEL_ENUM_VALUES)
                .map(|v| match &v.value {
                    EnumValue::String(s) => format!("\"{s}\""),
                    EnumValue::Int(i) => i.to_string(),
                    EnumValue::Float(f) => f.to_string(),
                })
                .collect();
            if values.len() > MAX_LABEL_ENUM_VALUES {
                labels.push("…".into());
            }
            labels.join(" | ")
        }
        McdocType::Union { members, .. } => {
            let mut labels: Vec<String> = Vec::new();
            for member in members.iter().map(label) {
                if !labels.contains(&member) {
                    labels.push(member);
                }
            }
            labels.join(" | ")
        }
        McdocType::List { item, length_range, .. } => match length_range {
            Some(r) => format!("[{}] @ {r}", label(item)),
            None => format!("[{}]", label(item)),
        },
        McdocType::Concrete { child, .. } | McdocType::Template { child, .. } | McdocType::Mapped { child, .. } => label(child),
        // A path missing from the tables is an unbound template parameter (like `T`), so it could be anything.
        McdocType::Reference { path: Some(path), .. } => tables.mcdoc.get(path).map_or_else(|| "any".into(), label),
        McdocType::Dispatcher { .. } => "object".into(),
        other => {
            let mut bare = other.clone();
            bare.attributes_mut().clear();
            bare.to_string()
        }
    }
}

fn deepest_node_at(arena: &JsonArena, id: JsonId, pos: usize) -> Option<JsonId> {
    let node = arena.get(id);
    if !node.span.contains(pos) {
        return None;
    }
    match &node.kind {
        ArenaKind::Object(pairs) => {
            for &(kid, vid) in pairs {
                if arena.get(kid).span.contains(pos) {
                    return Some(kid);
                }
                if let Some(v) = vid
                    && let Some(found) = deepest_node_at(arena, v, pos)
                {
                    return Some(found);
                }
            }
            Some(id)
        }
        ArenaKind::Array(items) => {
            for &it in items {
                if let Some(found) = deepest_node_at(arena, it, pos) {
                    return Some(found);
                }
            }
            Some(id)
        }
        _ => Some(id),
    }
}

fn is_object_key(arena: &JsonArena, obj_id: JsonId, cand: JsonId) -> bool {
    matches!(&arena.get(obj_id).kind, ArenaKind::Object(pairs) if pairs.iter().any(|(kid, _)| *kid == cand))
}

#[derive(Clone, Copy)]
enum KeyInsertMode {
    /// Renaming an existing key that already has `: <value>` after it.
    NameOnly,
    /// Typing a new key inside quotes (`"fun|"`): close the quote and append `: <value_scaffold>`.
    CloseQuoteAndValue { consume_closing_quote: bool, trailing_comma: bool },
    /// Typing a bare key inside `{ | }` or `{ fun| }`: wrap in quotes and append `: <value_scaffold>`.
    QuotedAndValue { trailing_comma: bool },
}

fn needs_trailing_comma(text: &str, after_pos: usize) -> bool {
    let rest = &text[after_pos.min(text.len())..];
    let next = rest.bytes().find(|b| !matches!(b, b' ' | b'\t' | b'\r' | b'\n'));
    !matches!(next, None | Some(b'}') | Some(b']') | Some(b','))
}

fn is_bare_word_byte(b: u8) -> bool {
    matches!(b, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' | b'.' | b':' | b'/' | b'#')
}

fn bare_word_start(text: &str, pos: usize) -> usize {
    let bytes = text.as_bytes();
    let mut start = pos.min(bytes.len());
    while start > 0 && is_bare_word_byte(bytes[start - 1]) {
        start -= 1;
    }
    start
}

fn bare_word_end(text: &str, pos: usize) -> usize {
    let bytes = text.as_bytes();
    let mut end = pos.min(bytes.len());
    while end < bytes.len() && is_bare_word_byte(bytes[end]) {
        end += 1;
    }
    end
}

fn colon_follows_key(text: &str, key_end: usize) -> bool {
    let rest = &text[key_end.min(text.len())..];
    for b in rest.bytes() {
        match b {
            b' ' | b'\t' => continue,
            b':' => return true,
            _ => return false,
        }
    }
    false
}

#[derive(Default)]
struct ValueKinds {
    string: bool,
    object: bool,
    array: bool,
    boolean: bool,
    number: bool,
    any: bool,
}

fn classify_value_kinds(ty: &McdocType, env: &CheckEnv<'_>, out: &mut ValueKinds, depth: u8) {
    if depth > 8 {
        out.any = true;
        return;
    }
    match ty {
        McdocType::Any { .. } => out.any = true,
        McdocType::String { .. } => out.string = true,
        McdocType::Boolean { .. } => out.boolean = true,
        McdocType::Byte { .. }
        | McdocType::Short { .. }
        | McdocType::Int { .. }
        | McdocType::Long { .. }
        | McdocType::Float { .. }
        | McdocType::Double { .. } => out.number = true,
        McdocType::Struct { .. } => out.object = true,
        McdocType::List { .. }
        | McdocType::Tuple { .. }
        | McdocType::ByteArray { .. }
        | McdocType::IntArray { .. }
        | McdocType::LongArray { .. } => out.array = true,
        McdocType::Literal { value, .. } => match value {
            LiteralValue::String { .. } => out.string = true,
            LiteralValue::Boolean { .. } => out.boolean = true,
            _ => out.number = true,
        },
        McdocType::Enum { values, .. } => {
            for v in values {
                if !env.version.keeps(&v.attributes) {
                    continue;
                }
                match v.value {
                    EnumValue::String(_) => out.string = true,
                    EnumValue::Int(_) | EnumValue::Float(_) => out.number = true,
                }
            }
        }
        McdocType::Union { members, .. } => {
            for m in members {
                if env.version.keeps(m.attributes()) {
                    classify_value_kinds(m, env, out, depth + 1);
                }
            }
        }
        McdocType::Reference { path: Some(path), .. } => {
            if let Some(target) = env.tables.mcdoc.get(path) {
                classify_value_kinds(target, env, out, depth + 1);
            }
        }
        McdocType::Concrete { child, .. } | McdocType::Template { child, .. } | McdocType::Mapped { child, .. } => {
            classify_value_kinds(child, env, out, depth + 1);
        }
        McdocType::Dispatcher { registry, parallel_indices, .. } => {
            if let Some(map) = env.tables.dispatchers.get(registry)
                && parallel_indices.len() == 1
                && let super::types::Index::Static { value } = &parallel_indices[0]
                && let Some(target) = map.get(&normalize_category(value))
            {
                classify_value_kinds(target, env, out, depth + 1);
            } else {
                out.any = true;
            }
        }
        _ => out.any = true,
    }
}

/// What we type after a completed key: the value's skeleton, how far before its end the cursor
/// goes, how much of the value to select, and whether to reopen completions there.
struct ValueScaffold {
    text: String,
    back: usize,
    select: usize,
    retrigger: bool,
}

impl ValueScaffold {
    fn new(text: &str, back: usize, select: usize, retrigger: bool) -> Self {
        Self { text: text.to_string(), back, select, retrigger }
    }
}

fn value_scaffold(ty: &McdocType, env: &CheckEnv<'_>) -> ValueScaffold {
    let mut k = ValueKinds::default();
    classify_value_kinds(ty, env, &mut k, 0);
    let count = usize::from(k.string) + usize::from(k.object) + usize::from(k.array) + usize::from(k.boolean) + usize::from(k.number);
    if k.any || count != 1 {
        return ValueScaffold::new("", 0, 0, false);
    }
    if k.string {
        ValueScaffold::new("\"\"", 1, 0, true)
    } else if k.object {
        ValueScaffold::new("{}", 1, 0, true)
    } else if k.array {
        ValueScaffold::new("[]", 1, 0, false)
    } else if k.boolean {
        ValueScaffold::new("", 0, 0, true)
    } else {
        // We select the number so you can type over it.
        let number = number_placeholder(ty, env, 0).unwrap_or_else(|| "0".into());
        let len = number.len();
        ValueScaffold { text: number, back: 0, select: len, retrigger: false }
    }
}

/// Returns a number that fits the field: 0 (or 0.0 for floats), or the range's nearest end when 0 is outside it.
fn number_placeholder(ty: &McdocType, env: &CheckEnv<'_>, depth: u8) -> Option<String> {
    if depth > 16 {
        return None;
    }
    let pick = |range: &Option<NumericRange>, float: bool| {
        let mut value = 0.0_f64;
        if let Some(r) = range {
            let step = if float { 0.1 } else { 1.0 };
            if let Some(min) = r.min.filter(|min| value < *min || (value == *min && r.kind.0 & RangeKind::LEFT_EXCLUSIVE.0 != 0)) {
                value = if r.kind.0 & RangeKind::LEFT_EXCLUSIVE.0 != 0 { min + step } else { min };
            } else if let Some(max) = r.max.filter(|max| value > *max || (value == *max && r.kind.0 & RangeKind::RIGHT_EXCLUSIVE.0 != 0)) {
                value = if r.kind.0 & RangeKind::RIGHT_EXCLUSIVE.0 != 0 { max - step } else { max };
            }
        }
        if float {
            if value.fract() == 0.0 { format!("{value:.1}") } else { format!("{value}") }
        } else {
            format!("{}", value.round() as i64)
        }
    };
    match ty {
        McdocType::Byte { value_range, .. } | McdocType::Short { value_range, .. } | McdocType::Int { value_range, .. } => Some(pick(value_range, false)),
        McdocType::Long { .. } => Some("0".into()),
        McdocType::Float { value_range, .. } | McdocType::Double { value_range, .. } => Some(pick(value_range, true)),
        McdocType::Union { members, .. } => members
            .iter()
            .filter(|m| env.version.keeps(m.attributes()))
            .find_map(|m| number_placeholder(m, env, depth + 1)),
        McdocType::Reference { path: Some(path), .. } => number_placeholder(env.tables.mcdoc.get(path)?, env, depth + 1),
        McdocType::Concrete { child, .. } | McdocType::Template { child, .. } | McdocType::Mapped { child, .. } => number_placeholder(child, env, depth + 1),
        _ => None,
    }
}

struct KeyInsert {
    insert_text: Option<String>,
    cursor_offset: Option<usize>,
    select_len: usize,
    consume_closing_quote: bool,
    retrigger: bool,
}

fn format_key_insert(name: &str, field_type: &McdocType, env: &CheckEnv<'_>, mode: KeyInsertMode) -> KeyInsert {
    let (prefix, consume_closing_quote, trailing_comma) = match mode {
        KeyInsertMode::NameOnly => {
            return KeyInsert { insert_text: None, cursor_offset: None, select_len: 0, consume_closing_quote: false, retrigger: false };
        }
        KeyInsertMode::CloseQuoteAndValue { consume_closing_quote, trailing_comma } => ("", consume_closing_quote, trailing_comma),
        KeyInsertMode::QuotedAndValue { trailing_comma } => ("\"", false, trailing_comma),
    };
    let value = value_scaffold(field_type, env);
    let comma = if trailing_comma { "," } else { "" };
    let text = format!("{prefix}{name}\": {}{comma}", value.text);
    let cursor = text.encode_utf16().count().saturating_sub(value.back + usize::from(trailing_comma));
    KeyInsert {
        insert_text: Some(text),
        cursor_offset: Some(cursor),
        select_len: value.select,
        consume_closing_quote,
        retrigger: value.retrigger,
    }
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

fn complete_object_keys(
    obj_id: JsonId,
    current_key_id: Option<JsonId>,
    check: &CheckOutput,
    env: &CheckEnv<'_>,
    typed: &str,
    mode: KeyInsertMode,
    opts: &CompleteOptions<'_>,
) -> Vec<CompletionItem> {
    let Some(info) = check.attached.get(&obj_id) else {
        return Vec::new();
    };
    let arena = &check.arena;
    let existing: HashSet<String> = match &arena.get(obj_id).kind {
        ArenaKind::Object(pairs) => pairs
            .iter()
            .filter(|(kid, _)| current_key_id != Some(*kid))
            .filter_map(|(kid, _)| match &arena.get(*kid).kind {
                ArenaKind::String(s) => Some(s.value.clone()),
                _ => None,
            })
            .collect(),
        _ => HashSet::new(),
    };

    let mut raw_fields: Vec<(bool, CompletionItem)> = Vec::new();
    let mut seen = HashSet::new();
    let pattern_symbols = super::checker::pattern_symbols(check);
    let filter = KeyFilter { existing: &existing, ignored_loaders: opts.ignored_loaders, pattern_symbols: &pattern_symbols };
    collect_struct_fields(&info.type_def, env, typed, mode, &filter, &mut seen, &mut raw_fields);

    let needle = typed.to_lowercase();
    raw_fields.retain(|(_, item)| needle.is_empty() || item.label.to_lowercase().contains(&needle));
    raw_fields.sort_by(|(opt_a, a), (opt_b, b)| {
        let start_a = !a.label.to_lowercase().starts_with(&needle);
        let start_b = !b.label.to_lowercase().starts_with(&needle);
        start_a
            .cmp(&start_b)
            .then_with(|| opt_a.cmp(opt_b))
            .then_with(|| a.label.cmp(&b.label))
    });
    raw_fields.into_iter().take(MAX_COMPLETIONS).map(|(_, item)| item).collect()
}

struct KeyFilter<'a> {
    existing: &'a HashSet<String>,
    ignored_loaders: &'a [&'a str],
    pattern_symbols: &'a [char],
}

impl KeyFilter<'_> {
    /// Hides a loader-specific field when the pack's loader can't read it.
    fn hides(&self, attributes: &[super::types::Attribute]) -> bool {
        attributes.iter().filter(|a| a.name == super::checker::LOADER_ATTR).filter_map(attr_string).any(|loaders| {
            loaders.split(',').all(|loader| self.ignored_loaders.contains(&loader))
        })
    }
}

fn collect_struct_fields(
    ty: &McdocType,
    env: &CheckEnv<'_>,
    typed: &str,
    mode: KeyInsertMode,
    filter: &KeyFilter<'_>,
    seen: &mut HashSet<String>,
    out: &mut Vec<(bool, CompletionItem)>,
) {
    match ty {
        McdocType::Union { members, .. } => {
            for m in members {
                collect_struct_fields(m, env, typed, mode, filter, seen, out);
            }
        }
        McdocType::Struct { fields, .. } => {
            for field in fields {
                let StructField::Pair { key, field_type, optional, deprecated, desc, attributes } = field else {
                    continue;
                };
                if filter.hides(attributes) {
                    continue;
                }
                match key {
                    StructKey::Literal(name) => {
                        if filter.existing.contains(name) || !seen.insert(name.clone()) {
                            continue;
                        }
                        let detail = Some(if *deprecated {
                            format!("{} (deprecated)", type_label(field_type, env.tables))
                        } else {
                            type_label(field_type, env.tables)
                        });
                        let key = format_key_insert(name, field_type, env, mode);
                        out.push((
                            *optional,
                            CompletionItem {
                                label: name.clone(),
                                kind: "property",
                                detail,
                                signature: Some(format!("\"{name}\": {}", type_label(field_type, env.tables))),
                                docs: desc.clone(),
                                insert_text: key.insert_text,
                                cursor_offset: key.cursor_offset,
                                select_len: key.select_len,
                                consume_closing_quote: key.consume_closing_quote,
                                retrigger: key.retrigger,
                            },
                        ));
                    }
                    StructKey::Dynamic(kty) if super::checker::crafting_role(kty) == Some(super::checker::CraftingRole::Reference) => {
                        for symbol in filter.pattern_symbols.iter().map(char::to_string) {
                            if filter.existing.contains(&symbol) || !seen.insert(symbol.clone()) {
                                continue;
                            }
                            let key = format_key_insert(&symbol, field_type, env, mode);
                            out.push((
                                false,
                                CompletionItem {
                                    label: symbol.clone(),
                                    kind: "property",
                                    detail: Some(format!("used in pattern, {}", type_label(field_type, env.tables))),
                                    signature: Some(format!("\"{symbol}\": {}", type_label(field_type, env.tables))),
                                    docs: desc.clone(),
                                    insert_text: key.insert_text,
                                    cursor_offset: key.cursor_offset,
                                    select_len: key.select_len,
                                    consume_closing_quote: key.consume_closing_quote,
                                    retrigger: key.retrigger,
                                },
                            ));
                        }
                    }
                    StructKey::Dynamic(kty) => {
                        for item in complete_values(kty, env, typed, true) {
                            if filter.existing.contains(&item.label) || !seen.insert(item.label.clone()) {
                                continue;
                            }
                            let key = format_key_insert(&item.label, field_type, env, mode);
                            out.push((
                                true,
                                CompletionItem {
                                    label: item.label,
                                    kind: "property",
                                    detail: Some(type_label(field_type, env.tables)),
                                    signature: item.signature,
                                    docs: desc.clone().or(item.docs),
                                    insert_text: key.insert_text,
                                    cursor_offset: key.cursor_offset,
                                    select_len: key.select_len,
                                    consume_closing_quote: key.consume_closing_quote,
                                    retrigger: key.retrigger,
                                },
                            ));
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

fn complete_values(ty: &McdocType, env: &CheckEnv<'_>, typed: &str, in_string: bool) -> Vec<CompletionItem> {
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    collect_value_completions(ty, env, typed, in_string, &mut seen, &mut items, 0);
    if !in_string && typed.is_empty() {
        let mut kinds = ValueKinds::default();
        classify_value_kinds(ty, env, &mut kinds, 0);
        if kinds.object && seen.insert("{}".into()) {
            items.push(CompletionItem {
                label: "{}".into(),
                kind: "class",
                detail: Some("object".into()),
                signature: None,
                docs: None,
                insert_text: Some("{}".into()),
                cursor_offset: Some(1),
                select_len: 0,
                consume_closing_quote: false,
                retrigger: true,
            });
        }
        if kinds.array && seen.insert("[]".into()) {
            items.push(CompletionItem {
                label: "[]".into(),
                kind: "variable",
                detail: Some("array".into()),
                signature: None,
                docs: None,
                insert_text: Some("[]".into()),
                cursor_offset: Some(1),
                select_len: 0,
                consume_closing_quote: false,
                retrigger: false,
            });
        }
        if kinds.string && items.is_empty() && seen.insert("\"\"".into()) {
            items.push(CompletionItem {
                label: "\"\"".into(),
                kind: "text",
                detail: Some("string".into()),
                signature: None,
                docs: None,
                insert_text: Some("\"\"".into()),
                cursor_offset: Some(1),
                select_len: 0,
                consume_closing_quote: false,
                retrigger: true,
            });
        }
    }
    items.truncate(MAX_COMPLETIONS);
    items
}

fn collect_value_completions(
    ty: &McdocType,
    env: &CheckEnv<'_>,
    typed: &str,
    in_string: bool,
    seen: &mut HashSet<String>,
    out: &mut Vec<CompletionItem>,
    depth: u8,
) {
    if depth > 8 || out.len() >= MAX_COMPLETIONS {
        return;
    }

    // Attribute-driven string completions (`#[id]`, `#[dispatcher_key]`).
    for attr in ty.attributes() {
        if attr.name == "id"
            && let Some(cfg) = parse_id_attribute(attr)
            && let Some(reg) = cfg.registry
        {
            let (lookup_reg, allow_tag, require_tag) = match cfg.tags {
                Some(TagMode::Implicit) => (format!("tag/{}", normalize_category(&reg)), false, false),
                Some(TagMode::Allowed) => (normalize_category(&reg), true, false),
                Some(TagMode::Required) => (normalize_category(&reg), true, true),
                None => (normalize_category(&reg), false, false),
            };
            let matches = env.registries.complete(&lookup_reg, allow_tag, require_tag, typed, MAX_COMPLETIONS);
            for id in matches {
                if seen.insert(id.clone()) {
                    let insert = if in_string { None } else { Some(format!("\"{id}\"")) };
                    out.push(CompletionItem {
                        label: id,
                        kind: "constant",
                        detail: Some(lookup_reg.clone()),
                        signature: None,
                        docs: None,
                        insert_text: insert,
                        cursor_offset: None,
                        select_len: 0,
                        consume_closing_quote: false,
                        retrigger: false,
                    });
                }
            }
        } else if attr.name == "dispatcher_key"
            && let Some(reg) = attr_string(attr)
            && let Some(map) = env.tables.dispatchers.get(reg)
        {
            let needle = typed.to_lowercase();
            let mut keys: Vec<&String> = map
                .iter()
                .filter(|(k, v)| !k.starts_with('%') && env.version.keeps(v.attributes()))
                .map(|(k, _)| k)
                .collect();
            keys.sort();
            for k in keys {
                if !needle.is_empty() && !k.to_lowercase().contains(&needle) {
                    continue;
                }
                if seen.insert(k.clone()) {
                    let insert = if in_string { None } else { Some(format!("\"{k}\"")) };
                    out.push(CompletionItem {
                        label: k.clone(),
                        kind: "constant",
                        detail: Some(reg.to_string()),
                        signature: None,
                        docs: None,
                        insert_text: insert,
                        cursor_offset: None,
                        select_len: 0,
                        consume_closing_quote: false,
                        retrigger: false,
                    });
                }
            }
        }
    }

    let needle = typed.to_lowercase();
    match ty {
        McdocType::Union { members, .. } => {
            for m in members {
                if env.version.keeps(m.attributes()) {
                    collect_value_completions(m, env, typed, in_string, seen, out, depth + 1);
                }
            }
        }
        McdocType::Reference { path: Some(path), .. } => {
            if let Some(target) = env.tables.mcdoc.get(path) {
                collect_value_completions(target, env, typed, in_string, seen, out, depth + 1);
            }
        }
        McdocType::Literal { value, .. } => {
            let (label, is_str) = match value {
                LiteralValue::String { value } => (value.clone(), true),
                LiteralValue::Boolean { value } => (value.to_string(), false),
                LiteralValue::Long { value } => (value.to_string(), false),
                LiteralValue::Byte { value }
                | LiteralValue::Short { value }
                | LiteralValue::Int { value }
                | LiteralValue::Float { value }
                | LiteralValue::Double { value } => (value.to_string(), false),
            };
            if (needle.is_empty() || label.to_lowercase().contains(&needle)) && seen.insert(label.clone()) {
                let insert = if is_str && !in_string { Some(format!("\"{label}\"")) } else { None };
                out.push(CompletionItem {
                    label,
                    kind: "constant",
                    detail: None,
                    signature: None,
                    docs: None,
                    insert_text: insert,
                    cursor_offset: None,
                    select_len: 0,
                    consume_closing_quote: false,
                    retrigger: false,
                });
            }
        }
        McdocType::Enum { values, .. } => {
            for v in values {
                if !env.version.keeps(&v.attributes) {
                    continue;
                }
                let (label, is_str) = match &v.value {
                    EnumValue::String(s) => (s.clone(), true),
                    EnumValue::Int(i) => (i.to_string(), false),
                    EnumValue::Float(f) => (f.to_string(), false),
                };
                if (needle.is_empty() || label.to_lowercase().contains(&needle)) && seen.insert(label.clone()) {
                    let insert = if is_str && !in_string { Some(format!("\"{label}\"")) } else { None };
                    out.push(CompletionItem {
                        label,
                        kind: "enum",
                        detail: Some(v.identifier.clone()),
                        signature: None,
                        docs: v.desc.clone(),
                        insert_text: insert,
                        cursor_offset: None,
                        select_len: 0,
                        consume_closing_quote: false,
                        retrigger: false,
                    });
                }
            }
        }
        McdocType::Boolean { .. } if !in_string => {
            for b in ["true", "false"] {
                if (needle.is_empty() || b.starts_with(&needle)) && seen.insert(b.to_string()) {
                    out.push(CompletionItem {
                        label: b.to_string(),
                        kind: "keyword",
                        detail: Some("boolean".into()),
                        signature: None,
                        docs: None,
                        insert_text: None,
                        cursor_offset: None,
                        select_len: 0,
                        consume_closing_quote: false,
                        retrigger: false,
                    });
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcdoc::checker::{check_json_with_type, resource_root_type};
    use crate::mcdoc::json_parser;
    use crate::mcdoc::parser::parse_and_bind;
    use crate::mcdoc::registries::RegistryIndex;
    use crate::mcdoc::types::SymbolTables;
    use crate::mcdoc::version::VersionContext;

    #[test]
    fn empty_ingredient_offers_keys_from_every_union_member() {
        let mut tables = SymbolTables::default();
        parse_and_bind(
            r#"
type Ingredient = (IngredientValue | [IngredientValue])
type IngredientValue = (struct { item: #[id="item"] string } | struct { tag: #[id(registry="item",tags="implicit")] string })
"#,
            "::java::data::recipe",
            &mut tables,
        )
        .unwrap();
        parse_and_bind(
            r#"
use ::java::data::recipe::Ingredient
dispatch minecraft:resource["recipe"] to struct { key: struct { [string]: Ingredient } }
"#,
            "::test",
            &mut tables,
        )
        .unwrap();
        crate::mcdoc::fetch::bridge_partner_dispatchers(&mut tables, &[]);
        let regs = RegistryIndex::default();
        let version = VersionContext { minecraft: Some("1.20.1".into()), neoforge: None };
        let ignored: &[&str] = &["forge", "neoforge"];
        let env = CheckEnv { tables: &tables, registries: &regs, version: &version, require_canonical: false, allow_missing_keys: false, primitives_read_as_strings: false, ignored_loaders: ignored };
        let root_type = resource_root_type("recipe", &tables).unwrap();
        let recheck = |edited: &str| json_parser::parse(edited, false).root.map(|r| check_json_with_type(&r, &root_type, &env));
        let opts = CompleteOptions { recheck: &recheck, ignored_loaders: ignored };

        let labels_at = |src: &str, cursor: usize| -> Vec<String> {
            let check = check_json_with_type(json_parser::parse(src, false).root.as_ref().unwrap(), &root_type, &env);
            complete(src, cursor, &check, &env, &opts).items.into_iter().map(|i| i.label).collect()
        };
        let empty = "{\"key\": {\"#\": {\n  \n}}}";
        let labels = labels_at(empty, empty.find("\n  ").unwrap() + 3);
        for key in ["item", "tag", "fabric:type"] {
            assert!(labels.iter().any(|l| l == key), "missing {key} in {labels:?}");
        }
        // The key you're typing is already in the document, and only the custom ingredient's dynamic
        // keys accept it, so it must not narrow the union.
        for src in ["{\"key\": {\"#\": {\"\"}}}", "{\"key\": {\"#\": {\"i\"}}}", "{\"key\": {\"#\": {\"i\": \"\"}}}"] {
            let cursor = src.rfind("{\"").unwrap() + 2 + usize::from(src.contains("\"i\""));
            let labels = labels_at(src, cursor);
            assert!(labels.iter().any(|l| l == "item"), "missing item in {labels:?} for {src}");
        }
    }

    #[test]
    fn shaped_key_completes_and_checks_pattern_symbols() {
        let mut tables = SymbolTables::default();
        parse_and_bind(
            r#"
dispatch minecraft:resource["recipe"] to struct {
	pattern: [#[crafting_ingredient(definition=true)] string @ 1..3] @ 1..3,
	key: struct {
		[#[crafting_ingredient] string]: struct { item: string },
	},
}
"#,
            "::test",
            &mut tables,
        )
        .unwrap();
        let regs = RegistryIndex::default();
        let version = VersionContext { minecraft: Some("1.21.1".into()), neoforge: None };
        let env = CheckEnv { tables: &tables, registries: &regs, version: &version, require_canonical: false, allow_missing_keys: false, primitives_read_as_strings: false, ignored_loaders: &[] };
        let root_type = resource_root_type("recipe", &tables).unwrap();
        let recheck = |edited: &str| json_parser::parse(edited, false).root.map(|r| check_json_with_type(&r, &root_type, &env));
        let opts = CompleteOptions { recheck: &recheck, ignored_loaders: &[] };
        let check_of = |src: &str| check_json_with_type(json_parser::parse(src, false).root.as_ref().unwrap(), &root_type, &env);

        let src = "{\"pattern\": [\"#X#\", \" S \"], \"key\": {\"X\": {\"item\": \"a\"},\n  \n}}";
        let check = check_of(src);
        let comp = complete(src, src.find("\n  ").unwrap() + 3, &check, &env, &opts);
        let labels: Vec<&str> = comp.items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, ["#", "S"], "pattern order, minus the space and the existing X");
        assert_eq!(comp.items[0].insert_text.as_deref(), Some("\"#\": {}"));

        let h = hover(src, src.find("\"key\"").unwrap() + 2, &check, &tables).unwrap();
        assert!(h.docs.contains("`#`, `X`, `S`"), "{h:?}");

        let bad = r##"{"pattern": ["#X"], "key": {"#": {"item": "a"}, "Q": {"item": "b"}, "ab": {"item": "c"}, " ": {"item": "d"}}}"##;
        let messages: Vec<String> = check_of(bad).diagnostics.into_iter().map(|d| d.message).collect();
        for needle in ["doesn't use \"Q\"", "\"ab\" isn't a valid key", "A space marks an empty slot", "doesn't define \"X\""] {
            assert!(messages.iter().any(|m| m.contains(needle)), "missing {needle} in {messages:?}");
        }
        let good = r##"{"pattern": ["# #"], "key": {"#": {"item": "a"}}}"##;
        assert!(check_of(good).diagnostics.is_empty(), "{:?}", check_of(good).diagnostics);
    }

    #[test]
    fn reports_whether_a_string_slot_has_choices() {
        let mut tables = SymbolTables::default();
        parse_and_bind(r#"dispatch minecraft:resource["recipe"] to struct { group: string, result: #[id="item"] string }"#, "::test", &mut tables).unwrap();
        let mut regs = RegistryIndex::default();
        regs.insert_entries("item", ["minecraft:diamond".into()]);
        regs.finalize();
        let version = VersionContext { minecraft: Some("1.21.1".into()), neoforge: None };
        let env = CheckEnv { tables: &tables, registries: &regs, version: &version, require_canonical: false, allow_missing_keys: false, primitives_read_as_strings: false, ignored_loaders: &[] };
        let root_type = resource_root_type("recipe", &tables).unwrap();
        let recheck = |edited: &str| json_parser::parse(edited, false).root.map(|r| check_json_with_type(&r, &root_type, &env));
        let opts = CompleteOptions { recheck: &recheck, ignored_loaders: &[] };

        let src = r#"{"group": "free text", "result": "zzz"}"#;
        let check = check_json_with_type(json_parser::parse(src, false).root.as_ref().unwrap(), &root_type, &env);
        let at = |needle: &str| complete(src, src.find(needle).unwrap() + 2, &check, &env, &opts);

        let plain = at("free text");
        assert!(plain.items.is_empty() && !plain.slot_has_choices, "{plain:?}");
        let unmatched_id = at("zzz");
        assert!(unmatched_id.items.is_empty() && unmatched_id.slot_has_choices, "{unmatched_id:?}");
        assert!(at("group").slot_has_choices);
    }

    #[test]
    fn completes_keys_modded_ids_and_hover_docs() {
        let mut tables = SymbolTables::default();
        parse_and_bind(
            r#"
dispatch minecraft:resource["recipe"] to struct Recipe {
	/// Recipe serializer ID.
	type: #[id="recipe_serializer"] string,
	...minecraft:recipe_serializer[[type]],
}
dispatch minecraft:recipe_serializer["minecraft:crafting_shapeless"] to struct Shapeless {
	/// Output item stack.
	result: struct {
		id: #[id="item"] string,
		count?: int @ 1..99,
	},
	experience?: float,
	show_notification?: boolean,
	ingredients?: [#[id="item"] string],
	#[packraven_loader="fabric"] "fabric:only"?: boolean,
}
"#,
            "::test",
            &mut tables,
        )
        .unwrap();

        let mut regs = RegistryIndex::default();
        regs.insert_entries("recipe_serializer", ["minecraft:crafting_shapeless".into()]);
        regs.insert_entries("item", ["minecraft:diamond".into(), "create:brass_ingot".into()]);
        regs.finalize();
        let version = VersionContext { minecraft: Some("1.21.1".into()), neoforge: None };
        let env = CheckEnv { tables: &tables, registries: &regs, version: &version, require_canonical: false, allow_missing_keys: false, primitives_read_as_strings: false, ignored_loaders: &[] };
        let root_type = resource_root_type("recipe", &tables).unwrap();
        let recheck = |edited: &str| json_parser::parse(edited, false).root.map(|r| check_json_with_type(&r, &root_type, &env));
        let opts = CompleteOptions { recheck: &recheck, ignored_loaders: &[] };

        let src = "{\n  \"type\": \"minecraft:crafting_shapeless\",\n  \"result\": {\n    \"id\": \"create:b\"\n  }\n}";
        let parsed = json_parser::parse(src, false);
        let check = check_json_with_type(parsed.root.as_ref().unwrap(), &root_type, &env);

        let cursor = src.find("create:b").unwrap() + "create:b".len();
        let comp = complete(src, cursor, &check, &env, &opts);
        assert!(comp.in_string);
        assert!(comp.items.iter().any(|i| i.label == "create:brass_ingot"), "{:?}", comp.items);

        let key_off = src.find("\"result\"").unwrap() + 2;
        let h = hover(src, key_off, &check, &tables).expect("expected hover on result key");
        assert!(h.docs.contains("Output item stack"), "{h:?}");

        // Completing a bare key inside `{ }` scaffolds quotes/braces/brackets based on value type.
        let bare = "{\n  \"type\": \"minecraft:crafting_shapeless\",\n  \n}";
        let bare_parsed = json_parser::parse(bare, false);
        let bare_check = check_json_with_type(bare_parsed.root.as_ref().unwrap(), &root_type, &env);
        let bare_pos = bare.rfind("  ").unwrap() + 2;
        let bare_comp = complete(bare, bare_pos, &bare_check, &env, &opts);
        let res_item = bare_comp.items.iter().find(|i| i.label == "result").expect("result completion");
        assert_eq!(res_item.insert_text.as_deref(), Some("\"result\": {}"));
        assert_eq!(res_item.cursor_offset, Some("\"result\": {".len()));
        assert!(res_item.retrigger);

        // A number field gets a placeholder that fits its type and range, selected so typing replaces it.
        let xp = bare_comp.items.iter().find(|i| i.label == "experience").expect("experience completion");
        assert_eq!(xp.insert_text.as_deref(), Some("\"experience\": 0.0"));
        assert_eq!((xp.cursor_offset, xp.select_len), (Some("\"experience\": 0.0".len()), 3));
        let in_result = "{\n  \"type\": \"minecraft:crafting_shapeless\",\n  \"result\": {\n    \n  }\n}";
        let in_result_check = check_json_with_type(json_parser::parse(in_result, false).root.as_ref().unwrap(), &root_type, &env);
        let count_comp = complete(in_result, in_result.find("{\n    ").unwrap() + 6, &in_result_check, &env, &opts);
        let count = count_comp.items.iter().find(|i| i.label == "count").expect("count completion");
        assert_eq!(count.insert_text.as_deref(), Some("\"count\": 1"), "int @ 1..99 starts at its minimum");
        assert_eq!(count.select_len, 1);

        // Typing a new key inside quotes without a following colon keeps `to == pos` (so CodeMirror matches `"res"`,
        // never `"res\""`) and sets `consume_closing_quote` so apply consumes the auto-closed quote cleanly.
        let quoted_new = "{\n  \"type\": \"minecraft:crafting_shapeless\",\n  \"res\"\n}";
        let qn_parsed = json_parser::parse(quoted_new, false);
        let qn_check = check_json_with_type(qn_parsed.root.as_ref().unwrap(), &root_type, &env);
        let qn_pos = quoted_new.find("res").unwrap() + 3;
        let qn_comp = complete(quoted_new, qn_pos, &qn_check, &env, &opts);
        assert_eq!(qn_comp.to, qn_pos);
        let qn_item = qn_comp.items.iter().find(|i| i.label == "result").expect("quoted result completion");
        assert_eq!(qn_item.insert_text.as_deref(), Some("result\": {}"));
        assert_eq!(qn_item.cursor_offset, Some("result\": {".len()));
        assert!(qn_item.consume_closing_quote);

        // Typing an unquoted bare key `{ res| }` sets `from` to the start of `res` so accepting replaces `res`.
        let bare_word = "{\n  \"type\": \"minecraft:crafting_shapeless\",\n  res\n}";
        let bw_parsed = json_parser::parse(bare_word, false);
        let bw_check = check_json_with_type(bw_parsed.root.as_ref().unwrap(), &root_type, &env);
        let bw_start = bare_word.find("res").unwrap();
        let bw_pos = bw_start + 3;
        let bw_comp = complete(bare_word, bw_pos, &bw_check, &env, &opts);
        assert_eq!(bw_comp.from, bw_start);
        assert_eq!(bw_comp.to, bw_pos);
        let bw_item = bw_comp.items.iter().find(|i| i.label == "result").expect("bare word result completion");
        assert_eq!(bw_item.insert_text.as_deref(), Some("\"result\": {}"));

        // Inserting a key BEFORE another sibling property appends a trailing comma and keeps cursor inside delimiters.
        let before_sib = "{\n  \"ty\"\n  \"result\": {}\n}";
        let bs_parsed = json_parser::parse(before_sib, false);
        let bs_check = check_json_with_type(bs_parsed.root.as_ref().unwrap(), &root_type, &env);
        let bs_pos = before_sib.find("ty").unwrap() + 2;
        let bs_comp = complete(before_sib, bs_pos, &bs_check, &env, &opts);
        let bs_item = bs_comp.items.iter().find(|i| i.label == "type").expect("type completion before sibling");
        assert_eq!(bs_item.insert_text.as_deref(), Some("type\": \"\","));
        assert_eq!(bs_item.cursor_offset, Some("type\": \"".len()));
        assert!(bs_item.consume_closing_quote);

        // Typing a bare value after an unknown key's colon (`"f": f|`) must NEVER offer object keys.
        let bad_val = "{\n  \"f\": f\n}";
        let bv_parsed = json_parser::parse(bad_val, false);
        let bv_check = check_json_with_type(bv_parsed.root.as_ref().unwrap(), &root_type, &env);
        let bv_pos = bad_val.rfind('f').unwrap() + 1;
        let bv_comp = complete(bad_val, bv_pos, &bv_check, &env, &opts);
        assert!(bv_comp.items.is_empty(), "must not suggest object keys in value position: {:?}", bv_comp.items);

        // Renaming an existing key that already has `: {}` does NOT duplicate `: {}`.
        let rename_comp = complete(src, key_off, &check, &env, &opts);
        if let Some(existing_item) = rename_comp.items.iter().find(|i| i.label == "result") {
            assert_eq!(existing_item.insert_text, None);
            assert_eq!(existing_item.cursor_offset, None);
            assert!(!existing_item.consume_closing_quote);
        }

        let run = |text: &str, cursor: usize, opts: &CompleteOptions<'_>| {
            let parsed = json_parser::parse(text, false);
            let check = check_json_with_type(parsed.root.as_ref().unwrap(), &root_type, &env);
            complete(text, cursor, &check, &env, opts)
        };
        let head = "{\n  \"type\": \"minecraft:crafting_shapeless\",\n  ";

        // Accepting a completion mid-string replaces the rest of the string content too.
        let mid = format!("{head}\"result\": {{\"id\": \"minecraft:dia_sword\"}}\n}}");
        let mid_pos = mid.find("dia_sword").unwrap() + 3;
        let mid_comp = run(&mid, mid_pos, &opts);
        assert_eq!(mid_comp.to, mid_pos);
        assert_eq!(mid_comp.replace_to, Some(mid.find("_sword").unwrap() + "_sword".len()));

        // A bare word typed inside an array completes quoted IDs and replaces only that word.
        let arr = format!("{head}\"ingredients\": [dia]\n}}");
        let arr_start = arr.find("dia]").unwrap();
        let arr_comp = run(&arr, arr_start + 3, &opts);
        assert_eq!(arr_comp.from, arr_start);
        let dia = arr_comp.items.iter().find(|i| i.label == "minecraft:diamond").expect("diamond in array");
        assert_eq!(dia.insert_text.as_deref(), Some("\"minecraft:diamond\""));

        // An empty array slot offers IDs too, not just `{}` / `[]` scaffolds.
        let empty_arr = format!("{head}\"ingredients\": []\n}}");
        let empty_comp = run(&empty_arr, empty_arr.find("[]").unwrap() + 1, &opts);
        assert!(empty_comp.items.iter().any(|i| i.label == "create:brass_ingot"), "{:?}", empty_comp.items);

        // A boolean slot after a colon offers `true` / `false`, with or without a typed prefix.
        let boolean = format!("{head}\"show_notification\": \n}}");
        let bool_comp = run(&boolean, boolean.find(": \n").unwrap() + 2, &opts);
        assert_eq!(bool_comp.items.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(), ["true", "false"]);
        let typed_bool = format!("{head}\"show_notification\": f\n}}");
        let typed_comp = run(&typed_bool, typed_bool.find(": f").unwrap() + 3, &opts);
        assert_eq!(typed_comp.items.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(), ["false"]);

        // The cursor right after a closing quote is not a value slot.
        let after_quote = format!("{head}\"result\": {{\"id\": \"minecraft:diamond\"}}\n}}");
        let aq_comp = run(&after_quote, after_quote.find("diamond\"").unwrap() + "diamond\"".len(), &opts);
        assert!(aq_comp.items.is_empty(), "{:?}", aq_comp.items);

        // Keys the pack's loader ignores still validate but aren't suggested.
        let keys = format!("{head}\n}}");
        let key_pos = keys.rfind("  ").unwrap() + 2;
        assert!(run(&keys, key_pos, &opts).items.iter().any(|i| i.label == "fabric:only"));
        let hiding = CompleteOptions { recheck: &recheck, ignored_loaders: &["fabric"] };
        assert!(!run(&keys, key_pos, &hiding).items.iter().any(|i| i.label == "fabric:only"));

        // Hover describes ID fields plainly instead of echoing raw mcdoc attributes.
        let id_hover = hover(src, src.find("\"id\"").unwrap() + 1, &check, &tables).expect("hover on id");
        assert_eq!(id_hover.signature, "\"id\": item ID");
    }
}
