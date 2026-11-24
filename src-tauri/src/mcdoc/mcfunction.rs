#![allow(dead_code)]

use std::cmp::Ordering;
use std::collections::HashSet;

use super::checker::parse_java_uuid;
use super::json_parser::{Diagnostic, Severity, Span};
use super::version::compare_version;

/// Extracts `scoreboard objectives add <name>`, `team add <name>`, and `tag <target> add <name>`
/// definitions from a `.mcfunction` file.
pub fn extract_mcfunction_symbols(
    src: &str,
    objectives: &mut Vec<String>,
    teams: &mut Vec<String>,
    entity_tags: &mut Vec<String>,
) {
    for raw_line in src.lines() {
        let mut line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('$') {
            line = rest.trim_start();
        }
        if let Some(rest) = line.strip_prefix('/') {
            line = rest.trim_start();
        }
        // Unwrap `execute ... run <command>` chains.
        while line.starts_with("execute ") {
            if let Some(idx) = line.find(" run ") {
                line = line[idx + 5..].trim_start();
                if let Some(rest) = line.strip_prefix('/') {
                    line = rest.trim_start();
                }
            } else {
                break;
            }
        }
        if let Some(rest) = line.strip_prefix("scoreboard ") {
            let mut parts = rest.split_ascii_whitespace();
            if parts.next() == Some("objectives")
                && parts.next() == Some("add")
                && let Some(name) = parts.next()
                && is_plain_symbol(name)
            {
                objectives.push(name.to_string());
            }
        } else if let Some(rest) = line.strip_prefix("team ") {
            let mut parts = rest.split_ascii_whitespace();
            if parts.next() == Some("add")
                && let Some(name) = parts.next()
                && is_plain_symbol(name)
            {
                teams.push(name.to_string());
            }
        } else if let Some(rest) = line.strip_prefix("tag ") {
            // Skip target selector (which may contain spaces inside `[...]`).
            let after_target = skip_selector_token(rest.trim_start());
            let mut parts = after_target.split_ascii_whitespace();
            if parts.next() == Some("add")
                && let Some(name) = parts.next()
                && is_plain_symbol(name)
            {
                entity_tags.push(name.to_string());
            }
        }
    }
}

fn is_plain_symbol(s: &str) -> bool {
    !s.is_empty() && !s.contains("$(")
}

fn skip_selector_token(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut bracket_depth = 0usize;
    let mut in_quote: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = in_quote {
            if b == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if b == q {
                in_quote = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' | b'\'' => in_quote = Some(b),
            b'[' | b'{' => bracket_depth += 1,
            b']' | b'}' => bracket_depth = bracket_depth.saturating_sub(1),
            b' ' | b'\t' if bracket_depth == 0 => break,
            _ => {}
        }
        i += 1;
    }
    &s[i..]
}

/// Validates an entity selector, player name, or UUID for `#[entity(amount=..., type=...)]`.
pub fn validate_entity_selector(src: &str, amount: &str, kind: &str, minecraft: Option<&str>) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    if src.is_empty() {
        diags.push(Diagnostic { span: Span::new(0, 0), severity: Severity::Error, message: "Expected an entity selector or player name.".into() });
        return diags;
    }
    if !src.starts_with('@') {
        if src.bytes().any(|b| b.is_ascii_whitespace()) {
            diags.push(Diagnostic {
                span: Span::new(0, src.len()),
                severity: Severity::Error,
                message: format!("\"{src}\" is not a valid player name or UUID."),
            });
        } else if parse_java_uuid(src).is_none() && (src.len() > 16 || !src.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')) {
            diags.push(Diagnostic {
                span: Span::new(0, src.len()),
                severity: Severity::Warning,
                message: format!("\"{src}\" is not a standard player name or UUID."),
            });
        }
        return diags;
    }

    let bytes = src.as_bytes();
    if bytes.len() < 2 {
        diags.push(Diagnostic { span: Span::new(0, src.len()), severity: Severity::Error, message: "Expected selector variable after '@'.".into() });
        return diags;
    }
    let var = bytes[1];
    let has_at_n = minecraft.is_none_or(|mc| compare_version(mc, "1.21") != Ordering::Less);
    let valid_var = matches!(var, b'p' | b'a' | b'e' | b'r' | b's') || (var == b'n' && has_at_n);
    if !valid_var {
        diags.push(Diagnostic {
            span: Span::new(0, 2),
            severity: Severity::Error,
            message: format!("Unknown entity selector \"@{}\".", var as char),
        });
        return diags;
    }

    let mut limit_one = matches!(var, b'p' | b'r' | b's' | b'n');
    let mut players_only = matches!(var, b'p' | b'a' | b'r');
    let mut pos = 2;
    if bytes.get(pos) == Some(&b'[') {
        pos += 1;
        let mut closed = false;
        let mut seen_non_negatable = HashSet::new();
        while pos < bytes.len() {
            while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
                pos += 1;
            }
            if bytes.get(pos) == Some(&b']') {
                pos += 1;
                closed = true;
                break;
            }
            let key_start = pos;
            while pos < bytes.len() && !matches!(bytes[pos], b'=' | b',' | b']' | b' ' | b'\t') {
                pos += 1;
            }
            let key = &src[key_start..pos];
            if key.is_empty() {
                diags.push(Diagnostic {
                    span: Span::new(pos, (pos + 1).min(bytes.len())),
                    severity: Severity::Error,
                    message: "Expected a selector argument name.".into(),
                });
                break;
            }
            while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
                pos += 1;
            }
            if bytes.get(pos) != Some(&b'=') {
                diags.push(Diagnostic { span: Span::new(pos, pos), severity: Severity::Error, message: "Expected '=' after selector argument.".into() });
                break;
            }
            pos += 1;
            while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
                pos += 1;
            }
            let val_start = pos;
            let mut depth = 0usize;
            let mut quote: Option<u8> = None;
            while pos < bytes.len() {
                let b = bytes[pos];
                if let Some(q) = quote {
                    if b == b'\\' && pos + 1 < bytes.len() {
                        pos += 2;
                        continue;
                    }
                    if b == q {
                        quote = None;
                    }
                    pos += 1;
                    continue;
                }
                match b {
                    b'"' | b'\'' => quote = Some(b),
                    b'{' | b'[' => depth += 1,
                    b'}' => depth = depth.saturating_sub(1),
                    b']' if depth == 0 => break,
                    b']' => depth -= 1,
                    b',' if depth == 0 => break,
                    _ => {}
                }
                pos += 1;
            }
            let val = src[val_start..pos].trim();
            if val.is_empty() {
                diags.push(Diagnostic { span: Span::new(val_start, pos), severity: Severity::Error, message: "Expected a selector argument value.".into() });
            }
            if key == "limit" && val == "1" {
                limit_one = true;
            }
            if key == "type" && matches!(val, "player" | "minecraft:player") {
                players_only = true;
            }
            if matches!(key, "limit" | "sort" | "x" | "y" | "z" | "dx" | "dy" | "dz" | "distance" | "level") && !seen_non_negatable.insert(key) {
                diags.push(Diagnostic {
                    span: Span::new(key_start, key_start + key.len()),
                    severity: Severity::Error,
                    message: format!("Duplicate selector argument \"{key}\"."),
                });
            }
            if bytes.get(pos) == Some(&b',') {
                pos += 1;
            } else if bytes.get(pos) == Some(&b']') {
                pos += 1;
                closed = true;
                break;
            }
        }
        if !closed {
            diags.push(Diagnostic { span: Span::new(pos, pos), severity: Severity::Error, message: "Expected closing ']' in entity selector.".into() });
        }
    }
    if pos < bytes.len() {
        diags.push(Diagnostic {
            span: Span::new(pos, bytes.len()),
            severity: Severity::Error,
            message: "Unexpected trailing content in entity selector.".into(),
        });
    }
    if amount == "single" && !limit_one {
        diags.push(Diagnostic {
            span: Span::new(0, src.len()),
            severity: Severity::Error,
            message: "Only a single entity is allowed here (add limit=1 or use @p/@r/@s/@n).".into(),
        });
    }
    if kind == "players" && !players_only && var != b's' {
        diags.push(Diagnostic {
            span: Span::new(0, src.len()),
            severity: Severity::Error,
            message: "Only players are allowed here (add type=minecraft:player or use @p/@a/@r/@s).".into(),
        });
    }
    diags
}

/// Validates a `#[score_holder]` value (`*`, entity selector, or fake player name without whitespace).
pub fn validate_score_holder(src: &str, amount: &str, minecraft: Option<&str>) -> Vec<Diagnostic> {
    if src == "*" {
        if amount == "single" {
            return vec![Diagnostic {
                span: Span::new(0, 1),
                severity: Severity::Error,
                message: "Wildcard '*' matches multiple score holders; only a single score holder is allowed here.".into(),
            }];
        }
        return Vec::new();
    }
    if src.starts_with('@') {
        return validate_entity_selector(src, amount, "entities", minecraft);
    }
    if src.is_empty() || src.bytes().any(|b| b.is_ascii_whitespace()) {
        return vec![Diagnostic {
            span: Span::new(0, src.len()),
            severity: Severity::Error,
            message: format!("\"{src}\" is not a valid score holder."),
        }];
    }
    Vec::new()
}

/// Validates a `#[command(...)]` string (slash rules, max length, empty/macro handling, and balanced delimiters).
pub fn validate_command(
    src: &str,
    slash: Option<&str>,
    macro_implicit: bool,
    empty_allowed: bool,
    incomplete: bool,
    max_length: Option<usize>,
) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    if let Some(limit) = max_length
        && src.chars().count() > limit
    {
        diags.push(Diagnostic {
            span: Span::new(0, src.len()),
            severity: Severity::Error,
            message: format!("Command exceeds maximum length of {limit} characters."),
        });
    }
    if src.is_empty() {
        if !empty_allowed {
            diags.push(Diagnostic { span: Span::new(0, 0), severity: Severity::Error, message: "Expected a command.".into() });
        }
        return diags;
    }
    if macro_implicit {
        return diags;
    }
    if slash == Some("chat") && !src.starts_with('/') {
        return diags;
    }
    if slash == Some("required") && !src.starts_with('/') {
        diags.push(Diagnostic { span: Span::new(0, src.len().min(1)), severity: Severity::Error, message: "Command must start with '/'.".into() });
        return diags;
    }
    if slash.is_none() && src.starts_with('/') {
        diags.push(Diagnostic { span: Span::new(0, 1), severity: Severity::Error, message: "Leading '/' is not allowed in this command.".into() });
    }
    let body_start = usize::from(src.starts_with('/'));
    let body = &src[body_start..];
    if body.is_empty() {
        if !incomplete {
            diags.push(Diagnostic { span: Span::new(body_start, src.len()), severity: Severity::Error, message: "Expected a command name.".into() });
        }
        return diags;
    }
    let verb = body.split_ascii_whitespace().next().unwrap_or("");
    if verb.is_empty() || !verb.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b':')) {
        diags.push(Diagnostic {
            span: Span::new(body_start, body_start + verb.len().max(1)),
            severity: Severity::Error,
            message: format!("Invalid command name \"{verb}\"."),
        });
    }
    if !incomplete {
        // Check balanced quotes, braces, and brackets in the command arguments.
        let bytes = src.as_bytes();
        let mut stack: Vec<(u8, usize)> = Vec::new();
        let mut in_quote: Option<(u8, usize)> = None;
        let mut i = body_start;
        while i < bytes.len() {
            let b = bytes[i];
            if let Some((q, _)) = in_quote {
                if b == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                    continue;
                }
                if b == q {
                    in_quote = None;
                }
                i += 1;
                continue;
            }
            match b {
                b'"' | b'\'' => in_quote = Some((b, i)),
                b'{' | b'[' => stack.push((b, i)),
                b'}' => match stack.last().copied() {
                    Some((b'{', _)) => {
                        stack.pop();
                    }
                    _ => diags.push(Diagnostic { span: Span::new(i, i + 1), severity: Severity::Error, message: "Unmatched '}' in command.".into() }),
                },
                b']' => match stack.last().copied() {
                    Some((b'[', _)) => {
                        stack.pop();
                    }
                    _ => diags.push(Diagnostic { span: Span::new(i, i + 1), severity: Severity::Error, message: "Unmatched ']' in command.".into() }),
                },
                _ => {}
            }
            i += 1;
        }
        if let Some((q, start)) = in_quote {
            diags.push(Diagnostic {
                span: Span::new(start, src.len()),
                severity: Severity::Error,
                message: format!("Unclosed {} quote in command.", q as char),
            });
        }
        if let Some((open, start)) = stack.last().copied() {
            let expected = if open == b'{' { '}' } else { ']' };
            diags.push(Diagnostic {
                span: Span::new(start, src.len()),
                severity: Severity::Error,
                message: format!("Expected closing '{expected}' in command."),
            });
        }
    }
    diags
}

/// Validates an `http://` or `https://` URL (Minecraft's `open_url` click event only allows these schemes).
pub fn validate_url(src: &str) -> Option<String> {
    let rest = src.strip_prefix("https://").or_else(|| src.strip_prefix("http://"));
    match rest {
        Some(host) if !host.is_empty() && !host.starts_with('/') && !src.bytes().any(|b| b.is_ascii_whitespace()) => None,
        _ => Some("Expected a valid http:// or https:// URL.".into()),
    }
}

/// Validates a Java `DateTimeFormatter` pattern (used by `assets::item_definition::LocalTime.pattern`).
pub fn validate_time_pattern(src: &str) -> Option<String> {
    const VALID_LETTERS: &[u8] = b"GyYuQqMLwWdDFgEecahHKkHmsSAnNVzOXxZvp";
    let bytes = src.as_bytes();
    let mut i = 0;
    let mut in_quote = false;
    let mut optional_depth = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if in_quote {
            if b == b'\'' {
                if bytes.get(i + 1) == Some(&b'\'') {
                    i += 2;
                    continue;
                }
                in_quote = false;
            }
            i += 1;
            continue;
        }
        match b {
            b'\'' => in_quote = true,
            b'[' => optional_depth += 1,
            b']' => {
                if optional_depth == 0 {
                    return Some("Unmatched ']' in DateTimeFormatter pattern.".into());
                }
                optional_depth -= 1;
            }
            b'A'..=b'Z' | b'a'..=b'z' if !VALID_LETTERS.contains(&b) => {
                return Some(format!("Unknown DateTimeFormatter pattern letter '{}'.", b as char));
            }
            _ => {}
        }
        i += 1;
    }
    if in_quote {
        return Some("Unclosed single quote in DateTimeFormatter pattern.".into());
    }
    if optional_depth > 0 {
        return Some("Unclosed '[' optional section in DateTimeFormatter pattern.".into());
    }
    None
}

/// Validates a space-separated coordinate vector (`#[vector(dimension=3, integer=true)]`).
pub fn validate_vector(src: &str, dimension: usize, integer: bool) -> Option<String> {
    let parts: Vec<&str> = src.split_ascii_whitespace().collect();
    if parts.len() != dimension {
        return Some(format!("Expected {dimension} coordinates, found {}.", parts.len()));
    }
    let has_local = parts.iter().any(|p| p.starts_with('^'));
    let has_non_local = parts.iter().any(|p| !p.starts_with('^'));
    if has_local && has_non_local {
        return Some("Cannot mix local ('^') and world/relative coordinates.".into());
    }
    for part in parts {
        let num = part.strip_prefix('^').or_else(|| part.strip_prefix('~')).unwrap_or(part);
        if num.is_empty() && (part.starts_with('^') || part.starts_with('~')) {
            continue;
        }
        let ok = if integer && !part.starts_with('^') { num.parse::<i32>().is_ok() } else { num.parse::<f64>().is_ok_and(|f| f.is_finite()) };
        if !ok {
            return Some(format!("Invalid coordinate \"{part}\"."));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_objectives_teams_and_tags_from_mcfunction() {
        let src = r#"
# Setup
scoreboard objectives add kills minecraft.custom:minecraft.mob_kills
/team add red "Red Team"
execute as @a[tag=ready, scores={kills=1..}] run tag @s add winner
$scoreboard objectives add $(dynamic) dummy
"#;
        let (mut objs, mut teams, mut tags) = (Vec::new(), Vec::new(), Vec::new());
        extract_mcfunction_symbols(src, &mut objs, &mut teams, &mut tags);
        assert_eq!(objs, ["kills"]);
        assert_eq!(teams, ["red"]);
        assert_eq!(tags, ["winner"]);
    }

    #[test]
    fn validates_entity_selectors_score_holders_and_commands() {
        assert!(validate_entity_selector("@e[type=minecraft:zombie,limit=1]", "single", "entities", Some("1.21.1")).is_empty());
        assert!(validate_entity_selector("@n", "single", "entities", Some("1.21.1")).is_empty());
        assert!(!validate_entity_selector("@n", "single", "entities", Some("1.20.1")).is_empty());
        assert!(!validate_entity_selector("@a", "single", "players", Some("1.21.1")).is_empty());
        assert!(!validate_entity_selector("@e", "multiple", "players", Some("1.21.1")).is_empty());
        assert!(validate_entity_selector("@e[type=player]", "multiple", "players", Some("1.21.1")).is_empty());

        assert!(validate_score_holder("*", "multiple", Some("1.21.1")).is_empty());
        assert!(!validate_score_holder("*", "single", Some("1.21.1")).is_empty());
        assert!(validate_score_holder("#total", "single", Some("1.21.1")).is_empty());
        assert!(!validate_score_holder("bad name", "single", Some("1.21.1")).is_empty());

        assert!(validate_command("/say hello", Some("required"), false, false, false, None).is_empty());
        assert!(!validate_command("say hello", Some("required"), false, false, false, None).is_empty());
        assert!(validate_command("plain chat", Some("chat"), false, false, false, None).is_empty());
        assert!(!validate_command("/give @p stone{a: 1", Some("allowed"), false, false, false, None).is_empty());
        assert!(validate_command("/give @p stone{a: 1", Some("allowed"), false, false, true, None).is_empty());
    }

    #[test]
    fn validates_urls_time_patterns_and_vectors() {
        assert!(validate_url("https://example.com/path").is_none());
        assert!(validate_url("ftp://example.com").is_some());
        assert!(validate_time_pattern("HH:mm:ss 'UTC' [XXX]").is_none());
        assert!(validate_time_pattern("HH:mm 'unclosed").is_some());
        assert!(validate_time_pattern("JJ:mm").is_some());
        assert!(validate_vector("~ ~1 ~", 3, true).is_none());
        assert!(validate_vector("^ ^ ^2.5", 3, true).is_none());
        assert!(validate_vector("^ ~ 0", 3, true).is_some());
        assert!(validate_vector("1 2", 3, true).is_some());
    }
}
