use std::collections::{HashMap, HashSet};

use super::modules::strip_module_prefixes;
use super::parser::{format_params, split_top_level, strip_generics, AliasDef, Member, MemberKind, Param, TypeDef, TypeKind};
use super::{CompletionItem, CompletionResult, HoverInfo, ProbeIndex};

const MAX_COMPLETIONS: usize = 120;
const MAX_STRING_COMPLETIONS: usize = 100;

#[derive(Debug, Clone)]
struct TypeRef {
    name: String,
    static_side: bool,
    type_arg: Option<String>,
}

#[derive(Debug, Clone)]
struct LocalBinding {
    name: String,
    ty: Option<TypeRef>,
    kind: &'static str,
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

pub fn complete(index: &ProbeIndex, text: &str, utf16_offset: usize) -> CompletionResult {
    let pos = utf16_to_byte(text, utf16_offset);
    let locals = collect_locals(text, pos, index);

    if let Some(str_ctx) = find_string_context(text, pos) {
        let items = complete_in_string(index, text, &str_ctx, &locals);
        return CompletionResult {
            from: byte_to_utf16(text, str_ctx.content_start),
            to: byte_to_utf16(text, pos),
            items,
            in_string: true,
        };
    }

    let word_start = scan_ident_start_backward(text, pos);
    let prefix = &text[word_start..pos];

    let before_word = text[..word_start].trim_end();
    if let Some(dot_stripped) = before_word.strip_suffix('.')
        && !dot_stripped.ends_with('.')
    {
        let receiver_expr = extract_expr_backward(dot_stripped);
        let items = if let Some(ty) = eval_expr_type(index, receiver_expr, &locals) {
            complete_members(index, &ty, prefix)
        } else {
            Vec::new()
        };
        return CompletionResult {
            from: byte_to_utf16(text, word_start),
            to: byte_to_utf16(text, pos),
            items,
            in_string: false,
        };
    }

    let items = complete_identifiers(index, &locals, prefix);
    CompletionResult {
        from: byte_to_utf16(text, word_start),
        to: byte_to_utf16(text, pos),
        items,
        in_string: false,
    }
}

pub fn hover(index: &ProbeIndex, text: &str, utf16_offset: usize) -> Option<HoverInfo> {
    let pos = utf16_to_byte(text, utf16_offset);
    let (start, end) = word_bounds_at(text, pos)?;
    let word = &text[start..end];
    if word.is_empty() {
        return None;
    }
    let locals = collect_locals(text, start, index);

    let before_word = text[..start].trim_end();
    if let Some(dot_stripped) = before_word.strip_suffix('.')
        && !dot_stripped.ends_with('.')
    {
        let receiver_expr = extract_expr_backward(dot_stripped);
        let ty = eval_expr_type(index, receiver_expr, &locals)?;
        let members = find_members_named(index, &ty, word);
        if members.is_empty() {
            return None;
        }
        let (signature, docs) = format_member_hover(&ty.name, &members);
        return Some(HoverInfo {
            from: byte_to_utf16(text, start),
            to: byte_to_utf16(text, end),
            signature,
            docs,
        });
    }

    if let Some(local) = locals.get(word) {
        let signature = match &local.ty {
            Some(ty) => format!("({}) {word}: {}", local.kind, clean_display_type(&ty.name)),
            None => format!("({}) {word}", local.kind),
        };
        return Some(HoverInfo {
            from: byte_to_utf16(text, start),
            to: byte_to_utf16(text, end),
            signature,
            docs: String::new(),
        });
    }

    if let Some(global) = index.globals.get(word) {
        let signature = global.signature.clone().unwrap_or_else(|| {
            if global.is_class_value {
                format!("class {}", global.name)
            } else if global.kind == "namespace" {
                format!("namespace {}", global.name)
            } else if global.ty.starts_with("__obj_") {
                format!("const {}: {{ ... }}", global.name)
            } else {
                format!("const {}: {}", global.name, clean_display_type(&global.ty))
            }
        });
        return Some(HoverInfo {
            from: byte_to_utf16(text, start),
            to: byte_to_utf16(text, end),
            signature,
            docs: global.docs.clone().unwrap_or_default(),
        });
    }

    if let Some(def) = resolve_type_def(index, word) {
        let kw = match def.kind {
            TypeKind::Class => "class",
            TypeKind::Interface => "interface",
            TypeKind::Namespace => "namespace",
            TypeKind::Object => "type",
        };
        let signature = format!("{kw} {word}");
        return Some(HoverInfo {
            from: byte_to_utf16(text, start),
            to: byte_to_utf16(text, end),
            signature,
            docs: def.docs.clone().unwrap_or_default(),
        });
    }

    None
}

fn complete_members(index: &ProbeIndex, receiver: &TypeRef, prefix: &str) -> Vec<CompletionItem> {
    let mut by_name: HashMap<String, Vec<&Member>> = HashMap::new();
    collect_all_members(index, &receiver.name, receiver.static_side, &mut HashSet::new(), &mut by_name);

    // KubeJS globals like `Item` and `Fluid` are declared as classes with static methods,
    // or as `declare const Item: Item`, so when one side has no matches we check both.
    if by_name.is_empty() {
        collect_all_members(index, &receiver.name, !receiver.static_side, &mut HashSet::new(), &mut by_name);
    }

    let needle = prefix.to_lowercase();
    let mut starts: Vec<(u8, String, CompletionItem)> = Vec::new();
    let mut contains: Vec<(u8, String, CompletionItem)> = Vec::new();

    for (name, overloads) in by_name {
        let lower = name.to_lowercase();
        let rank = if matches!(name.as_str(), "toString" | "equals" | "hashCode") { 2 } else { 0 };
        if !needle.is_empty() && !lower.contains(&needle) {
            continue;
        }
        let item = member_to_completion(&name, &overloads);
        if needle.is_empty() || lower.starts_with(&needle) {
            starts.push((rank, lower, item));
        } else {
            contains.push((rank, lower, item));
        }
    }

    starts.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    contains.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

    starts
        .into_iter()
        .chain(contains)
        .take(MAX_COMPLETIONS)
        .map(|(_, _, item)| item)
        .collect()
}

fn complete_identifiers(index: &ProbeIndex, locals: &HashMap<String, LocalBinding>, prefix: &str) -> Vec<CompletionItem> {
    let needle = prefix.to_lowercase();
    let mut starts: Vec<(u8, String, CompletionItem)> = Vec::new();
    let mut contains: Vec<(u8, String, CompletionItem)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for local in locals.values() {
        let lower = local.name.to_lowercase();
        if !needle.is_empty() && !lower.contains(&needle) {
            continue;
        }
        seen.insert(local.name.clone());
        let detail = local.ty.as_ref().map(|t| clean_display_type(&t.name));
        let item = CompletionItem {
            label: local.name.clone(),
            kind: local.kind,
            detail,
            signature: None,
            docs: None,
            insert_text: None,
        };
        if needle.is_empty() || lower.starts_with(&needle) {
            starts.push((0, lower, item));
        } else {
            contains.push((0, lower, item));
        }
    }

    for global in index.globals.values() {
        if seen.contains(&global.name) {
            continue;
        }
        let lower = global.name.to_lowercase();
        if !needle.is_empty() && !lower.contains(&needle) {
            continue;
        }
        let rank = if is_common_kubejs_global(&global.name) { 1 } else { 2 };
        let detail = if global.ty.starts_with("__obj_") {
            None
        } else {
            Some(clean_display_type(&global.ty))
        };
        let item = CompletionItem {
            label: global.name.clone(),
            kind: global.kind,
            detail,
            signature: global.signature.clone(),
            docs: global.docs.clone(),
            insert_text: None,
        };
        if needle.is_empty() || lower.starts_with(&needle) {
            starts.push((rank, lower, item));
        } else {
            contains.push((rank, lower, item));
        }
    }

    starts.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    contains.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

    starts
        .into_iter()
        .chain(contains)
        .take(MAX_COMPLETIONS)
        .map(|(_, _, item)| item)
        .collect()
}

fn is_common_kubejs_global(name: &str) -> bool {
    matches!(
        name,
        "ServerEvents"
            | "ClientEvents"
            | "StartupEvents"
            | "JEIEvents"
            | "REIEvents"
            | "EMIEvents"
            | "RecipeViewerEvents"
            | "NativeEvents"
            | "NetworkEvents"
            | "ItemEvents"
            | "BlockEvents"
            | "PlayerEvents"
            | "EntityEvents"
            | "LevelEvents"
            | "Item"
            | "Fluid"
            | "Ingredient"
            | "Java"
            | "Utils"
            | "Text"
            | "Component"
            | "BlockPos"
            | "Vec3d"
            | "Color"
            | "NBT"
            | "JsonIO"
            | "Platform"
            | "global"
            | "console"
    )
}

fn member_to_completion(name: &str, overloads: &[&Member]) -> CompletionItem {
    let first = overloads[0];
    let kind = match first.kind {
        MemberKind::Method => "method",
        MemberKind::Property | MemberKind::Getter => "property",
        MemberKind::Const => "constant",
    };
    let detail = if first.kind == MemberKind::Method {
        if overloads.len() > 1 {
            Some(format!("(+{} overloads)", overloads.len() - 1))
        } else {
            let ret = clean_display_type(&first.return_type);
            (ret != "void" && !ret.starts_with("__obj_")).then_some(ret)
        }
    } else {
        let ret = clean_display_type(&first.return_type);
        (!ret.starts_with("__obj_")).then_some(ret)
    };
    let (signature, docs) = format_member_hover("", overloads);
    CompletionItem {
        label: name.to_string(),
        kind,
        detail,
        signature: (!signature.is_empty()).then_some(signature),
        docs: (!docs.is_empty()).then_some(docs),
        insert_text: None,
    }
}

fn format_member_hover(owner: &str, overloads: &[&Member]) -> (String, String) {
    let mut sigs = Vec::new();
    let mut doc = String::new();
    let prefix = if owner.is_empty() || owner.starts_with("__obj_") {
        String::new()
    } else {
        format!("{}.", clean_display_type(owner))
    };
    for m in overloads.iter().take(6) {
        let sig = match m.kind {
            MemberKind::Method => format!("{prefix}{}({}): {}", m.name, format_params(&m.params), clean_display_type(&m.return_type)),
            MemberKind::Property | MemberKind::Getter | MemberKind::Const => format!("{prefix}{}: {}", m.name, clean_display_type(&m.return_type)),
        };
        if !sigs.contains(&sig) {
            sigs.push(sig);
        }
        if doc.is_empty()
            && let Some(d) = &m.docs
        {
            doc = d.clone();
        }
    }
    if overloads.len() > 6 {
        sigs.push(format!("// ... and {} more overloads", overloads.len() - 6));
    }
    (sigs.join("\n"), doc)
}

struct StringContext<'a> {
    quote_pos: usize,
    content_start: usize,
    prefix: &'a str,
}

fn find_string_context<'a>(text: &'a str, pos: usize) -> Option<StringContext<'a>> {
    let line_start = text[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let before = &text[line_start..pos];
    let bytes = before.as_bytes();
    let mut quote: Option<(u8, usize)> = None;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some((q, _)) = quote {
            if b == b'\\' {
                i += 2;
                continue;
            }
            if b == q {
                quote = None;
            }
            i += 1;
        } else {
            if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                return None;
            }
            if b == b'"' || b == b'\'' || b == b'`' {
                quote = Some((b, line_start + i));
            }
            i += 1;
        }
    }
    let (_, quote_pos) = quote?;
    let content_start = quote_pos + 1;
    Some(StringContext {
        quote_pos,
        content_start,
        prefix: &text[content_start..pos],
    })
}

struct CallContext<'a> {
    callee: &'a str,
    arg_index: usize,
    first_string_arg: Option<String>,
    object_key: Option<String>,
}

fn find_enclosing_call<'a>(text: &'a str, pos: usize) -> Option<CallContext<'a>> {
    let bytes = text.as_bytes();
    let mut i = pos;
    let mut paren_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut brace_depth = 0usize;
    let mut arg_index = 0usize;
    let mut object_key: Option<String> = None;

    let before_str = text[..pos].trim_end();
    if let Some(before_colon) = before_str.strip_suffix(':') {
        let key_end = before_colon.trim_end().len();
        let trimmed = &before_colon[..key_end];
        if let Some(stripped) = trimmed.strip_suffix('"').or_else(|| trimmed.strip_suffix('\'')) {
            if let Some(open) = stripped.rfind(['"', '\'']) {
                object_key = Some(stripped[open + 1..].to_string());
            }
        } else {
            let key_start = scan_ident_start_backward(trimmed, key_end);
            if key_start < key_end {
                object_key = Some(trimmed[key_start..key_end].to_string());
            }
        }
    }

    while i > 0 {
        i -= 1;
        let b = bytes[i];
        if b == b'"' || b == b'\'' || b == b'`' {
            let q = b;
            while i > 0 {
                i -= 1;
                if bytes[i] == q && !is_escaped(bytes, i) {
                    break;
                }
            }
            continue;
        }
        match b {
            b')' => paren_depth += 1,
            b'(' => {
                if paren_depth == 0 {
                    let callee = extract_expr_backward(text[..i].trim_end());
                    if callee.is_empty() {
                        return None;
                    }
                    let first_string_arg = extract_first_string_arg(&text[i + 1..pos]);
                    return Some(CallContext {
                        callee,
                        arg_index,
                        first_string_arg,
                        object_key,
                    });
                }
                paren_depth -= 1;
            }
            b']' => bracket_depth += 1,
            b'[' => bracket_depth = bracket_depth.saturating_sub(1),
            b'}' => brace_depth += 1,
            b'{' => brace_depth = brace_depth.saturating_sub(1),
            b',' if paren_depth == 0 && bracket_depth == 0 && brace_depth == 0 => {
                arg_index += 1;
            }
            b';' if paren_depth == 0 && brace_depth == 0 => return None,
            _ => {}
        }
    }
    None
}

#[derive(Default)]
struct StringSources {
    literals: Vec<String>,
    /// `(prefix, union_alias_name)` e.g. `("", "Special.Item")`, `("#", "Special.ItemTag")`, `("@", "Special.Mod")`.
    unions: Vec<(String, String)>,
}

fn complete_in_string(index: &ProbeIndex, text: &str, ctx: &StringContext<'_>, locals: &HashMap<String, LocalBinding>) -> Vec<CompletionItem> {
    let mut sources = StringSources::default();

    if let Some(call) = find_enclosing_call(text, ctx.quote_pos) {
        if let Some((receiver, members)) = resolve_callee(index, call.callee, locals) {
            let type_arg = receiver.type_arg.as_deref();
            let mut param_types = Vec::new();
            for m in &members {
                if let Some(first_arg) = &call.first_string_arg
                    && call.arg_index > 0
                    && let Some(p0) = m.params.first()
                    && let Some(lit) = &p0.literal
                    && lit != first_arg
                {
                    continue;
                }
                if let Some(param) = param_at(&m.params, call.arg_index) {
                    if let Some(lit) = &param.literal {
                        if !sources.literals.contains(lit) {
                            sources.literals.push(lit.clone());
                        }
                    } else {
                        param_types.push(param.ty.as_str());
                    }
                } else if m.kind != MemberKind::Method && m.return_type.contains("RecipeTypeFunction") {
                    // KubeJS shorthand recipe builders (event.shapeless, event.smelting, etc.) are
                    // typed as Internal.RecipeTypeFunction properties and take item/tag/mod inputs.
                    push_available_unions(index, &mut sources, &[("", "Special.Item"), ("", "RegistryTypes.Item"), ("#", "Special.ItemTag"), ("#", "RegistryTypes.ItemTag"), ("@", "Special.Mod"), ("@", "SpecialTypes.ModId")]);
                }
            }

            if let Some(obj_key) = &call.object_key {
                collect_object_key_sources(index, &param_types, obj_key, type_arg, &mut sources);
            } else {
                let mut visited = HashSet::new();
                for ty in param_types {
                    expand_string_sources(index, ty, "", type_arg, &mut visited, &mut sources, 0);
                }
            }
        } else if let Some(obj_key) = &call.object_key {
            fallback_object_key_sources(index, obj_key, &mut sources);
        }

        if call.callee.ends_with("Java.loadClass") || call.callee.ends_with("loadClass") {
            push_available_unions(index, &mut sources, &[("", "Special.JavaClass"), ("", "SpecialTypes.ClassPath")]);
        }
    }

    // When you type a `#`, `@` or `%` prefix and no call narrows it, we offer the matching global
    // ProbeJS registry union.
    if sources.literals.is_empty() && sources.unions.is_empty() {
        if ctx.prefix.starts_with('#') {
            push_available_unions(
                index,
                &mut sources,
                &[
                    ("#", "Special.ItemTag"),
                    ("#", "RegistryTypes.ItemTag"),
                    ("#", "Special.BlockTag"),
                    ("#", "RegistryTypes.BlockTag"),
                    ("#", "Special.FluidTag"),
                    ("#", "RegistryTypes.FluidTag"),
                ],
            );
        } else if ctx.prefix.starts_with('@') {
            push_available_unions(index, &mut sources, &[("@", "Special.Mod"), ("@", "SpecialTypes.ModId")]);
        } else if ctx.prefix.starts_with('%') {
            push_available_unions(index, &mut sources, &[("%", "Special.CreativeModeTab"), ("%", "RegistryTypes.CreativeModeTab")]);
        }
    }

    rank_string_completions(index, &sources, ctx.prefix)
}

fn push_available_unions(index: &ProbeIndex, out: &mut StringSources, candidates: &[(&str, &str)]) {
    for (pref, alias) in candidates {
        if index.aliases.contains_key(*alias) {
            push_union_source(out, pref, alias);
        }
    }
}

fn collect_object_key_sources(index: &ProbeIndex, param_types: &[&str], key: &str, type_arg: Option<&str>, out: &mut StringSources) {
    let mut visited = HashSet::new();
    for ty in param_types {
        if let Some(member_ty) = lookup_object_property_in_type(index, ty, key, &mut HashSet::new(), 0) {
            expand_string_sources(index, &member_ty, "", type_arg, &mut visited, out, 0);
        }
    }
    if out.literals.is_empty() && out.unions.is_empty() {
        fallback_object_key_sources(index, key, out);
    }
}

fn fallback_object_key_sources(index: &ProbeIndex, key: &str, out: &mut StringSources) {
    let candidates: &[(&str, &str)] = match key {
        "item" | "output" | "input" | "result" => &[
            ("", "Special.Item"),
            ("", "RegistryTypes.Item"),
            ("#", "Special.ItemTag"),
            ("#", "RegistryTypes.ItemTag"),
            ("@", "Special.Mod"),
            ("@", "SpecialTypes.ModId"),
        ],
        "fluid" | "fluidInput" | "fluidOutput" => &[("", "Special.Fluid"), ("", "RegistryTypes.Fluid"), ("#", "Special.FluidTag"), ("#", "RegistryTypes.FluidTag")],
        "block" => &[("", "Special.Block"), ("", "RegistryTypes.Block"), ("#", "Special.BlockTag"), ("#", "RegistryTypes.BlockTag")],
        "tag" => &[
            ("", "Special.ItemTag"),
            ("", "RegistryTypes.ItemTag"),
            ("", "Special.BlockTag"),
            ("", "RegistryTypes.BlockTag"),
            ("", "Special.FluidTag"),
            ("", "RegistryTypes.FluidTag"),
        ],
        "mod" => &[("", "Special.Mod"), ("", "SpecialTypes.ModId")],
        "id" => &[("", "Special.RecipeId"), ("", "SpecialTypes.RecipeId")],
        "type" => &[("", "Special.RecipeSerializer"), ("", "RegistryTypes.RecipeSerializer"), ("", "Special.RecipeType"), ("", "RegistryTypes.RecipeType")],
        "gas" => &[("", "Special.Gas"), ("", "RegistryTypes.Gas")],
        "biome" => &[("", "Special.Biome"), ("", "RegistryTypes.Biome")],
        "entity" | "entityType" => &[("", "Special.EntityType"), ("", "RegistryTypes.EntityType")],
        _ => &[],
    };
    push_available_unions(index, out, candidates);
}

fn lookup_object_property_in_type(index: &ProbeIndex, ty: &str, key: &str, visited: &mut HashSet<String>, depth: u8) -> Option<String> {
    if depth > 6 {
        return None;
    }
    let clean = strip_generics(ty.trim().trim_end_matches("[]"));
    if !visited.insert(clean.to_string()) {
        return None;
    }
    if let Some(def) = resolve_type_def(index, clean) {
        for m in &def.members {
            if m.name == key {
                return Some(m.return_type.clone());
            }
        }
    }
    if let Some(AliasDef::Refs { types, object_type, .. }) = resolve_alias(index, clean) {
        if let Some(obj) = object_type
            && let Some(def) = index.types.get(obj)
        {
            for m in &def.members {
                if m.name == key {
                    return Some(m.return_type.clone());
                }
            }
        }
        for sub in types {
            if let Some(found) = lookup_object_property_in_type(index, sub, key, visited, depth + 1) {
                return Some(found);
            }
        }
    }
    None
}

fn expand_string_sources(index: &ProbeIndex, ty: &str, prefix: &str, type_arg: Option<&str>, visited: &mut HashSet<(String, String)>, out: &mut StringSources, depth: u8) {
    if depth > 8 {
        return;
    }
    for branch in split_top_level(ty, b'|') {
        let p = branch.trim();
        if p.is_empty() {
            continue;
        }
        if let Some(inner) = p.strip_prefix('"').and_then(|s| s.strip_suffix('"')).or_else(|| p.strip_prefix('\'').and_then(|s| s.strip_suffix('\''))) {
            let full = format!("{prefix}{inner}");
            if !out.literals.contains(&full) {
                out.literals.push(full);
            }
            continue;
        }
        if let Some(inner) = p.strip_prefix('`').and_then(|s| s.strip_suffix('`'))
            && let Some(dollar) = inner.find("${")
            && inner.ends_with('}')
        {
            let tpl_pref = format!("{prefix}{}", &inner[..dollar]);
            let target = inner[dollar + 2..inner.len() - 1].trim();
            expand_string_sources(index, target, &tpl_pref, type_arg, visited, out, depth + 1);
            continue;
        }

        let unwrapped = p.trim_start_matches('(').trim_end_matches(')').trim_end_matches("[]").trim();
        let (clean, inner_arg) = split_generic_arg(unwrapped);

        // ProbeJS 7 generic events (TagEvent<T>, $RemoveEntriesKubeEvent<F>, $AddEntriesKubeEvent<E>, etc.).
        if let Some(arg) = type_arg {
            if clean.len() == 1 && clean.as_bytes()[0].is_ascii_uppercase() {
                expand_string_sources(index, arg, prefix, None, visited, out, depth + 1);
                continue;
            }
            let base = arg.trim_start_matches('$').trim_end_matches('_');
            if clean.ends_with("ResolveTag") {
                push_available_unions(index, out, &[(prefix, &format!("RegistryTypes.{base}Tag")), (prefix, &format!("Special.{base}Tag"))]);
                continue;
            }
            if clean.ends_with("ResolveObject") {
                push_available_unions(index, out, &[(prefix, &format!("RegistryTypes.{base}")), (prefix, &format!("Special.{base}"))]);
                continue;
            }
        }

        // Unwrap ProbeJS 7 collection containers like `$List_<$Ingredient_>` or `$Collection<T>`.
        if let Some(inner) = inner_arg
            && matches!(clean.rsplit('.').next().unwrap_or(clean), "$List_" | "$List" | "List_" | "List" | "Array" | "ReadonlyArray" | "$Collection_" | "$Collection" | "$Set_" | "$Set")
        {
            expand_string_sources(index, inner, prefix, type_arg, visited, out, depth + 1);
            continue;
        }

        if clean.is_empty() || matches!(clean, "string" | "number" | "boolean" | "any" | "void" | "object" | "null" | "undefined" | "RegExp") {
            continue;
        }
        let key = (prefix.to_string(), clean.to_string());
        if !visited.insert(key) {
            continue;
        }
        if let Some((resolved_name, alias)) = resolve_alias_with_name(index, clean) {
            match alias {
                AliasDef::StringUnion(_) => push_union_source(out, prefix, resolved_name),
                AliasDef::Refs { types, templates, literals, .. } => {
                    for lit in literals {
                        let full = format!("{prefix}{lit}");
                        if !out.literals.contains(&full) {
                            out.literals.push(full);
                        }
                    }
                    for (tpl_pref, target) in templates {
                        let combined = format!("{prefix}{tpl_pref}");
                        expand_string_sources(index, target, &combined, type_arg, visited, out, depth + 1);
                    }
                    for sub in types {
                        expand_string_sources(index, sub, prefix, type_arg, visited, out, depth + 1);
                    }
                }
            }
        }
    }
}

fn push_union_source(out: &mut StringSources, prefix: &str, name: &str) {
    if !out.unions.iter().any(|(p, n)| p == prefix && n == name) {
        out.unions.push((prefix.to_string(), name.to_string()));
    }
}

fn rank_string_completions(index: &ProbeIndex, sources: &StringSources, typed: &str) -> Vec<CompletionItem> {
    let needle = typed.to_lowercase();
    let mut starts: Vec<(String, &'static str)> = Vec::new();
    let mut contains: Vec<(String, &'static str)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for lit in &sources.literals {
        let lower = lit.to_lowercase();
        if !needle.is_empty() && !lower.contains(&needle) {
            continue;
        }
        if seen.insert(lit.clone()) {
            if needle.is_empty() || lower.starts_with(&needle) {
                starts.push((lit.clone(), "literal"));
            } else {
                contains.push((lit.clone(), "literal"));
            }
        }
    }

    let typed_sigil = typed.chars().next().filter(|c| matches!(c, '#' | '@' | '%'));

    for (pref, alias_name) in &sources.unions {
        if let Some(sigil) = typed_sigil {
            if !pref.starts_with(sigil) {
                continue;
            }
        } else if !pref.is_empty() && !needle.is_empty() {
            // `#tag` and `@mod` unions can't match while you're typing `minecraft:...`.
            continue;
        }

        let Some(AliasDef::StringUnion(list)) = index.aliases.get(alias_name) else {
            continue;
        };
        let label_detail = short_special_name(alias_name);
        let sub_needle = if !pref.is_empty() && needle.starts_with(&pref.to_lowercase()) {
            &needle[pref.len()..]
        } else {
            needle.as_str()
        };

        for entry in list {
            if starts.len() >= MAX_STRING_COMPLETIONS * 2 && !sub_needle.is_empty() {
                break;
            }
            if sub_needle.is_empty() && starts.len() >= MAX_STRING_COMPLETIONS {
                break;
            }
            let entry_lower = entry.to_lowercase();
            let is_start = sub_needle.is_empty() || entry_lower.starts_with(sub_needle);
            let is_match = is_start || entry_lower.contains(sub_needle);
            if !is_match {
                continue;
            }
            let full = if pref.is_empty() {
                entry.to_string()
            } else {
                format!("{pref}{entry}")
            };
            if !seen.insert(full.clone()) {
                continue;
            }
            if is_start {
                starts.push((full, label_detail));
            } else if contains.len() < MAX_STRING_COMPLETIONS {
                contains.push((full, label_detail));
            }
        }
    }

    starts.sort_by(|a, b| a.0.len().cmp(&b.0.len()).then_with(|| a.0.cmp(&b.0)));
    contains.sort_by(|a, b| a.0.len().cmp(&b.0.len()).then_with(|| a.0.cmp(&b.0)));

    starts
        .into_iter()
        .chain(contains)
        .take(MAX_STRING_COMPLETIONS)
        .map(|(label, detail)| CompletionItem {
            label,
            kind: "text",
            detail: (detail != "literal").then(|| detail.to_string()),
            signature: None,
            docs: None,
            insert_text: None,
        })
        .collect()
}

fn short_special_name(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or(name) {
        "Item" => "item",
        "ItemTag" => "item tag",
        "Block" => "block",
        "BlockTag" => "block tag",
        "Fluid" => "fluid",
        "FluidTag" => "fluid tag",
        "Mod" | "ModId" => "mod",
        "RecipeId" => "recipe",
        "RecipeType" => "recipe type",
        "RecipeSerializer" => "recipe serializer",
        "EntityType" => "entity",
        "EntityTypeTag" => "entity tag",
        "Biome" => "biome",
        "JavaClass" | "ClassPath" => "java class",
        "CreativeModeTab" => "creative tab",
        "SoundEvent" => "sound",
        "Gas" => "gas",
        "LangKey" | "TranslationKey" => "lang key",
        "Texture" => "texture",
        "LootTable" => "loot table",
        _ => "id",
    }
}

fn split_generic_arg(ty: &str) -> (&str, Option<&str>) {
    let trimmed = ty.trim();
    if let Some(open) = trimmed.find('<')
        && trimmed.ends_with('>')
    {
        let base = trimmed[..open].trim();
        let inner = trimmed[open + 1..trimmed.len() - 1].trim();
        return (base, (!inner.is_empty()).then_some(inner));
    }
    (strip_generics(trimmed), None)
}

fn collect_locals(text: &str, pos: usize, index: &ProbeIndex) -> HashMap<String, LocalBinding> {
    let mut locals: HashMap<String, LocalBinding> = HashMap::new();
    let prefix = &text[..pos];

    for (open_brace, callback_param, call_open_paren) in find_open_callbacks(prefix) {
        let _ = open_brace;
        let callee = extract_expr_backward(prefix[..call_open_paren].trim_end());
        if callee.is_empty() {
            continue;
        }
        let inside_call = &prefix[call_open_paren + 1..];
        let first_str = extract_first_string_arg(inside_call);
        let arg_index = count_args_before_callback(inside_call);
        let mut inferred = None;
        if let Some((_, members)) = resolve_callee(index, callee, &locals) {
            inferred = infer_callback_param_type(&members, arg_index, first_str.as_deref());
        }
        locals.insert(
            callback_param.clone(),
            LocalBinding {
                name: callback_param,
                ty: inferred.map(|(name, type_arg)| TypeRef { name, static_side: false, type_arg }),
                kind: "variable",
            },
        );
    }

    let mut pending_jsdoc_type: Option<String> = None;
    for line in prefix.lines() {
        let trimmed = line.trim();
        if let Some(jt) = extract_jsdoc_type(trimmed) {
            pending_jsdoc_type = Some(jt);
            continue;
        }
        if let Some(rest) = trimmed
            .strip_prefix("const ")
            .or_else(|| trimmed.strip_prefix("let "))
            .or_else(|| trimmed.strip_prefix("var "))
        {
            let rest = rest.trim_start();
            if let Some(eq) = rest.find('=') {
                let lhs = rest[..eq].trim();
                let rhs = rest[eq + 1..].trim().trim_end_matches(';').trim();
                if is_simple_ident(lhs) {
                    let ty = if let Some(jt) = pending_jsdoc_type.take() {
                        let (base, arg) = split_generic_arg(&jt);
                        Some(TypeRef {
                            name: base.to_string(),
                            static_side: false,
                            type_arg: arg.map(str::to_string),
                        })
                    } else if let Some(loaded) = extract_java_load_class(rhs) {
                        Some(resolve_java_class_ref(index, &loaded))
                    } else if rhs.contains("=>") || rhs.starts_with("function") {
                        None
                    } else {
                        eval_expr_type(index, rhs, &locals)
                    };
                    let kind = if rhs.contains("=>") || rhs.starts_with("function") {
                        "function"
                    } else {
                        "variable"
                    };
                    locals.insert(lhs.to_string(), LocalBinding { name: lhs.to_string(), ty, kind });
                } else if lhs.starts_with('[') && lhs.ends_with(']') {
                    for part in lhs[1..lhs.len() - 1].split(',') {
                        let name = part.trim();
                        if is_simple_ident(name) {
                            locals.insert(name.to_string(), LocalBinding { name: name.to_string(), ty: None, kind: "variable" });
                        }
                    }
                    pending_jsdoc_type = None;
                }
            }
        } else if let Some(rest) = trimmed.strip_prefix("function ") {
            let name = rest.split(|c: char| !is_ident_char(c)).next().unwrap_or("");
            if !name.is_empty() {
                locals.insert(name.to_string(), LocalBinding { name: name.to_string(), ty: None, kind: "function" });
            }
            pending_jsdoc_type = None;
        } else if !trimmed.is_empty() && !trimmed.starts_with("//") && !trimmed.starts_with('*') {
            pending_jsdoc_type = None;
        }
    }

    locals
}

fn find_open_callbacks(prefix: &str) -> Vec<(usize, String, usize)> {
    let bytes = prefix.as_bytes();
    let mut brace_stack: Vec<usize> = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'"' || b == b'\'' || b == b'`' {
            let q = b;
            i += 1;
            let mut esc = false;
            while i < bytes.len() {
                let c = bytes[i];
                i += 1;
                if esc {
                    esc = false;
                } else if c == b'\\' {
                    esc = true;
                } else if c == q {
                    break;
                }
            }
            continue;
        }
        if b == b'/' && i + 1 < bytes.len() {
            if bytes[i + 1] == b'/' {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            if bytes[i + 1] == b'*' {
                i += 2;
                while i + 1 < bytes.len() {
                    if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
                continue;
            }
        }
        if b == b'{' {
            brace_stack.push(i);
        } else if b == b'}' {
            brace_stack.pop();
        }
        i += 1;
    }

    let mut out = Vec::new();
    for brace_pos in brace_stack {
        let before = prefix[..brace_pos].trim_end();
        let param_and_start = if let Some(before_arrow) = before.strip_suffix("=>") {
            let trimmed = before_arrow.trim_end();
            if let Some(inner) = trimmed.strip_suffix(')') {
                inner.rfind('(').and_then(|open| {
                    let param = inner[open + 1..].trim().split(',').next()?.trim();
                    is_simple_ident(param).then(|| (param.to_string(), open))
                })
            } else {
                let start = scan_ident_start_backward(trimmed, trimmed.len());
                let param = &trimmed[start..];
                is_simple_ident(param).then(|| (param.to_string(), start))
            }
        } else if let Some(inner) = before.strip_suffix(')') {
            inner.rfind('(').and_then(|open| {
                let pre = inner[..open].trim_end();
                pre.ends_with("function").then(|| {
                    let param = inner[open + 1..].trim().split(',').next().unwrap_or("").trim();
                    is_simple_ident(param).then(|| (param.to_string(), pre.len() - "function".len()))
                })?
            })
        } else {
            None
        };

        let Some((param, fn_start)) = param_and_start else { continue };
        if let Some(call_open) = find_open_paren_before(prefix, fn_start) {
            out.push((brace_pos, param, call_open));
        }
    }
    out
}

fn find_open_paren_before(text: &str, pos: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = pos;
    let mut depth = 0usize;
    while i > 0 {
        i -= 1;
        let b = bytes[i];
        if b == b'"' || b == b'\'' || b == b'`' {
            let q = b;
            while i > 0 {
                i -= 1;
                if bytes[i] == q && !is_escaped(bytes, i) {
                    break;
                }
            }
            continue;
        }
        match b {
            b')' => depth += 1,
            b'(' => {
                if depth == 0 {
                    return Some(i);
                }
                depth -= 1;
            }
            b';' | b'{' if depth == 0 => return None,
            _ => {}
        }
    }
    None
}

fn count_args_before_callback(after_open_paren: &str) -> usize {
    let bytes = after_open_paren.as_bytes();
    let mut depth = 0usize;
    let mut commas = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'"' || b == b'\'' || b == b'`' {
            let q = b;
            i += 1;
            let mut esc = false;
            while i < bytes.len() {
                let c = bytes[i];
                i += 1;
                if esc {
                    esc = false;
                } else if c == b'\\' {
                    esc = true;
                } else if c == q {
                    break;
                }
            }
            continue;
        }
        if b == b'=' && i + 1 < bytes.len() && bytes[i + 1] == b'>' && depth == 0 {
            break;
        }
        match b {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => commas += 1,
            _ => {}
        }
        i += 1;
    }
    commas
}

fn infer_callback_param_type(members: &[&Member], arg_index: usize, first_str: Option<&str>) -> Option<(String, Option<String>)> {
    if let Some(first) = first_str {
        for m in members {
            let p0_matches = m
                .params
                .first()
                .and_then(|p| p.literal.as_deref())
                .is_some_and(|lit| lit == first || lit == format!("minecraft:{first}"));
            if p0_matches
                && let Some(cb) = param_at(&m.params, arg_index)
                && let Some(ty) = extract_fn_first_param_type(&cb.ty)
            {
                return Some(ty);
            }
        }
    }
    for m in members {
        if let Some(cb) = param_at(&m.params, arg_index)
            && let Some(ty) = extract_fn_first_param_type(&cb.ty)
        {
            return Some(ty);
        }
    }
    for m in members {
        for p in &m.params {
            if let Some(ty) = extract_fn_first_param_type(&p.ty) {
                return Some(ty);
            }
        }
    }
    None
}

fn extract_fn_first_param_type(fn_ty: &str) -> Option<(String, Option<String>)> {
    let mut s = fn_ty.trim();
    // ProbeJS 7 wraps callback signatures in extra parens: `((event: $RecipesKubeEvent) => void)`.
    while s.starts_with('(') && s.ends_with(')') && s[1..s.len() - 1].trim_start().starts_with('(') {
        s = s[1..s.len() - 1].trim();
    }
    let open = s.find('(')?;
    let close = s.find(')')?;
    if close <= open + 1 {
        return None;
    }
    let first_param = split_top_level(&s[open + 1..close], b',').into_iter().next()?.trim();
    let colon = first_param.find(':')?;
    let raw_ty = first_param[colon + 1..].trim();
    let (base, arg) = split_generic_arg(raw_ty);
    (!base.is_empty()).then(|| (base.to_string(), arg.map(str::to_string)))
}

fn eval_expr_type(index: &ProbeIndex, expr: &str, locals: &HashMap<String, LocalBinding>) -> Option<TypeRef> {
    let expr = expr.trim();
    if expr.is_empty() {
        return None;
    }
    let segments = split_chain(expr);
    let first = segments.first()?;
    let (head_name, head_call) = split_call_segment(first);

    let mut current = if let Some(local) = locals.get(head_name) {
        local.ty.clone()?
    } else if let Some(global) = index.globals.get(head_name) {
        let (_, arg) = split_generic_arg(&global.ty);
        if head_call.is_some() && global.kind == "function" {
            TypeRef {
                name: normalize_type_name(index, &global.ty),
                static_side: false,
                type_arg: arg.map(str::to_string),
            }
        } else {
            TypeRef {
                name: normalize_type_name(index, &global.ty),
                static_side: global.is_class_value,
                type_arg: arg.map(str::to_string),
            }
        }
    } else if index.types.contains_key(head_name) {
        TypeRef {
            name: head_name.to_string(),
            static_side: false,
            type_arg: None,
        }
    } else {
        return None;
    };

    for seg in &segments[1..] {
        let (name, args) = split_call_segment(seg);
        if name.is_empty() {
            return None;
        }
        let members = find_members_named(index, &current, name);
        if members.is_empty() {
            return None;
        }
        let chosen = if let Some(arg_str) = args {
            let first_lit = extract_first_string_arg(arg_str);
            pick_overload(&members, first_lit.as_deref())
        } else {
            members[0]
        };
        let ret = chosen.return_type.trim();
        if ret == "this" {
            current.static_side = false;
            continue;
        }
        if ret == "void" || ret == "any" || ret.is_empty() {
            return None;
        }
        let (_, ret_arg) = split_generic_arg(ret);
        let propagated = match ret_arg {
            Some("T") => current.type_arg.clone(),
            Some(other) => Some(other.to_string()),
            None => None,
        };
        current = TypeRef {
            name: normalize_type_name(index, ret),
            static_side: false,
            type_arg: propagated,
        };
    }

    Some(current)
}

fn resolve_callee<'a>(index: &'a ProbeIndex, callee: &str, locals: &HashMap<String, LocalBinding>) -> Option<(TypeRef, Vec<&'a Member>)> {
    let segments = split_chain(callee.trim());
    if segments.len() < 2 {
        return None;
    }
    let receiver_expr = segments[..segments.len() - 1].join(".");
    let (method_name, _) = split_call_segment(segments.last()?);
    let receiver = eval_expr_type(index, &receiver_expr, locals)?;
    let members = find_members_named(index, &receiver, method_name);
    (!members.is_empty()).then_some((receiver, members))
}

fn pick_overload<'a>(members: &[&'a Member], first_str: Option<&str>) -> &'a Member {
    if let Some(first) = first_str {
        for m in members {
            if m.params.first().and_then(|p| p.literal.as_deref()) == Some(first) {
                return m;
            }
        }
    }
    members.iter().copied().find(|m| m.return_type != "any" && m.return_type != "void").unwrap_or(members[0])
}

fn resolve_type_def<'a>(index: &'a ProbeIndex, name: &str) -> Option<&'a TypeDef> {
    let clean = strip_generics(name.trim().trim_end_matches("[]"));
    if let Some(def) = index.types.get(clean) {
        return Some(def);
    }
    if !clean.contains('.') {
        let internal = format!("Internal.{clean}");
        if let Some(def) = index.types.get(&internal) {
            return Some(def);
        }
    }
    // ProbeJS often uses `Foo_` alias for `Foo | ...`
    if let Some(stripped) = clean.strip_suffix('_') {
        if let Some(def) = index.types.get(stripped) {
            return Some(def);
        }
        if !stripped.contains('.') {
            let internal = format!("Internal.{stripped}");
            if let Some(def) = index.types.get(&internal) {
                return Some(def);
            }
        }
    }
    None
}

fn resolve_alias<'a>(index: &'a ProbeIndex, name: &str) -> Option<&'a AliasDef> {
    resolve_alias_with_name(index, name).map(|(_, a)| a)
}

fn resolve_alias_with_name<'a>(index: &'a ProbeIndex, name: &str) -> Option<(&'a str, &'a AliasDef)> {
    let clean = strip_generics(name.trim().trim_end_matches("[]"));
    if let Some((k, v)) = index.aliases.get_key_value(clean) {
        return Some((k.as_str(), v));
    }
    if !clean.contains('.') {
        let internal = format!("Internal.{clean}");
        if let Some((k, v)) = index.aliases.get_key_value(&internal) {
            return Some((k.as_str(), v));
        }
        let special = format!("Special.{clean}");
        if let Some((k, v)) = index.aliases.get_key_value(&special) {
            return Some((k.as_str(), v));
        }
    }
    None
}

fn normalize_type_name(index: &ProbeIndex, raw: &str) -> String {
    let first = split_top_level(raw, b'|').into_iter().next().unwrap_or(raw).trim();
    let clean = strip_generics(first.trim_start_matches('(').trim_end_matches(')').trim_end_matches("[]").trim());
    if index.types.contains_key(clean) {
        return clean.to_string();
    }
    if !clean.contains('.') {
        let internal = format!("Internal.{clean}");
        if index.types.contains_key(&internal) {
            return internal;
        }
    }
    if let Some(stripped) = clean.strip_suffix('_') {
        if index.types.contains_key(stripped) {
            return stripped.to_string();
        }
        if !stripped.contains('.') {
            let internal = format!("Internal.{stripped}");
            if index.types.contains_key(&internal) {
                return internal;
            }
        }
    }
    clean.to_string()
}

fn collect_all_members<'a>(index: &'a ProbeIndex, type_name: &str, static_side: bool, visited: &mut HashSet<String>, out: &mut HashMap<String, Vec<&'a Member>>) {
    let clean = strip_generics(type_name.trim());
    if !visited.insert(clean.to_string()) {
        return;
    }
    let Some(def) = resolve_type_def(index, clean) else {
        return;
    };
    for m in &def.members {
        if m.is_static == static_side {
            out.entry(m.name.clone()).or_default().push(m);
        }
    }
    for base in &def.extends {
        collect_all_members(index, base, static_side, visited, out);
    }
}

fn find_members_named<'a>(index: &'a ProbeIndex, receiver: &TypeRef, name: &str) -> Vec<&'a Member> {
    let mut found = Vec::new();
    find_members_named_inner(index, &receiver.name, receiver.static_side, name, &mut HashSet::new(), &mut found);
    if found.is_empty() {
        find_members_named_inner(index, &receiver.name, !receiver.static_side, name, &mut HashSet::new(), &mut found);
    }
    found
}

fn find_members_named_inner<'a>(
    index: &'a ProbeIndex,
    type_name: &str,
    static_side: bool,
    name: &str,
    visited: &mut HashSet<String>,
    out: &mut Vec<&'a Member>,
) {
    let clean = strip_generics(type_name.trim());
    if !visited.insert(clean.to_string()) {
        return;
    }
    let Some(def) = resolve_type_def(index, clean) else {
        return;
    };
    for m in &def.members {
        if m.name == name && m.is_static == static_side {
            out.push(m);
        }
    }
    for base in &def.extends {
        find_members_named_inner(index, base, static_side, name, visited, out);
    }
}

fn param_at(params: &[Param], arg_index: usize) -> Option<&Param> {
    if let Some(p) = params.get(arg_index) {
        return Some(p);
    }
    params.last().filter(|p| p.rest)
}

fn clean_display_type(ty: &str) -> String {
    if ty.starts_with("__obj_") {
        "{ ... }".to_string()
    } else {
        strip_module_prefixes(ty).into_owned()
    }
}

fn extract_expr_backward(src: &str) -> &str {
    let bytes = src.as_bytes();
    let mut i = bytes.len();
    while i > 0 {
        let b = bytes[i - 1];
        if is_ident_byte(b) || b == b'.' {
            i -= 1;
        } else if b == b')' || b == b']' {
            let close = b;
            let open = if b == b')' { b'(' } else { b'[' };
            let mut depth = 1usize;
            i -= 1;
            while i > 0 && depth > 0 {
                i -= 1;
                let c = bytes[i];
                if c == b'"' || c == b'\'' || c == b'`' {
                    let q = c;
                    while i > 0 {
                        i -= 1;
                        if bytes[i] == q && !is_escaped(bytes, i) {
                            break;
                        }
                    }
                    continue;
                }
                if c == close {
                    depth += 1;
                } else if c == open {
                    depth -= 1;
                }
            }
        } else if b.is_ascii_whitespace() {
            // We allow whitespace around `.` for multiline fluent chains (`foo()\n  .bar()`), but stop at
            // blank lines and at `//` comments that happen to end in a period.
            let mut j = i;
            let mut newlines = 0usize;
            while j > 0 && bytes[j - 1].is_ascii_whitespace() {
                if bytes[j - 1] == b'\n' {
                    newlines += 1;
                }
                j -= 1;
            }
            let prev_is_dot = j > 0 && bytes[j - 1] == b'.' && !in_line_comment(src, j - 1);
            let next_is_dot = bytes.get(i) == Some(&b'.');
            if newlines <= 1 && (next_is_dot || prev_is_dot) && !in_line_comment(src, j.saturating_sub(1)) {
                i = j;
            } else {
                break;
            }
        } else {
            break;
        }
    }
    src[i..].trim()
}

fn split_chain(expr: &str) -> Vec<&str> {
    let bytes = expr.as_bytes();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut depth = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'"' || b == b'\'' || b == b'`' {
            let q = b;
            i += 1;
            let mut esc = false;
            while i < bytes.len() {
                let c = bytes[i];
                i += 1;
                if esc {
                    esc = false;
                } else if c == b'\\' {
                    esc = true;
                } else if c == q {
                    break;
                }
            }
            continue;
        }
        match b {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b'.' if depth == 0 => {
                let seg = expr[start..i].trim();
                if !seg.is_empty() {
                    out.push(seg);
                }
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    let tail = expr[start..].trim();
    if !tail.is_empty() {
        out.push(tail);
    }
    out
}

fn split_call_segment(seg: &str) -> (&str, Option<&str>) {
    if let Some(open) = seg.find('(')
        && seg.ends_with(')')
    {
        (seg[..open].trim(), Some(&seg[open + 1..seg.len() - 1]))
    } else {
        (seg.trim(), None)
    }
}

fn extract_first_string_arg(args: &str) -> Option<String> {
    let s = args.trim_start();
    let quote = s.as_bytes().first().copied()?;
    if quote != b'"' && quote != b'\'' {
        return None;
    }
    let bytes = s.as_bytes();
    let mut i = 1usize;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == quote {
            return Some(s[1..i].to_string());
        }
        i += 1;
    }
    None
}

fn extract_jsdoc_type(line: &str) -> Option<String> {
    let idx = line.find("@type")?;
    let rest = &line[idx + 5..];
    let open = rest.find('{')?;
    let close = rest[open + 1..].find('}')?;
    let ty = rest[open + 1..open + 1 + close].trim();
    (!ty.is_empty()).then(|| ty.to_string())
}

fn extract_java_load_class(rhs: &str) -> Option<String> {
    let idx = rhs.find("Java.loadClass")?;
    let after = &rhs[idx + "Java.loadClass".len()..];
    let open = after.find('(')?;
    extract_first_string_arg(&after[open + 1..])
}

fn resolve_java_class_ref(index: &ProbeIndex, fqn: &str) -> TypeRef {
    if index.types.contains_key(fqn) {
        return TypeRef { name: fqn.to_string(), static_side: true, type_arg: None };
    }
    if let Some(key) = index.modules.java_class_key(fqn).filter(|key| index.types.contains_key(key)) {
        return TypeRef { name: key, static_side: true, type_arg: None };
    }
    let short = fqn.rsplit('.').next().unwrap_or(fqn);
    let dollar = format!("${short}");
    if index.types.contains_key(&dollar) {
        return TypeRef { name: dollar, static_side: true, type_arg: None };
    }
    let internal = format!("Internal.{short}");
    if index.types.contains_key(&internal) {
        return TypeRef { name: internal, static_side: true, type_arg: None };
    }
    TypeRef { name: short.to_string(), static_side: true, type_arg: None }
}

fn scan_ident_start_backward(text: &str, pos: usize) -> usize {
    let bytes = text.as_bytes();
    let mut i = pos.min(bytes.len());
    while i > 0 && is_ident_byte(bytes[i - 1]) {
        i -= 1;
    }
    i
}

fn word_bounds_at(text: &str, pos: usize) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut p = pos.min(bytes.len());
    // A cursor just past the end of a word still hovers that word.
    if p > 0 && is_ident_byte(bytes[p - 1]) && (p == bytes.len() || !is_ident_byte(bytes[p])) {
        p -= 1;
    }
    if p >= bytes.len() || !is_ident_byte(bytes[p]) {
        return None;
    }
    let mut start = p;
    while start > 0 && is_ident_byte(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = p;
    while end < bytes.len() && is_ident_byte(bytes[end]) {
        end += 1;
    }
    Some((start, end))
}

fn is_escaped(bytes: &[u8], pos: usize) -> bool {
    let mut backslashes = 0usize;
    let mut i = pos;
    while i > 0 && bytes[i - 1] == b'\\' {
        backslashes += 1;
        i -= 1;
    }
    backslashes % 2 == 1
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

fn is_simple_ident(s: &str) -> bool {
    !s.is_empty() && !s.as_bytes()[0].is_ascii_digit() && s.bytes().all(is_ident_byte)
}

fn in_line_comment(src: &str, pos: usize) -> bool {
    let line_start = src[..pos.min(src.len())].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line = &src[line_start..pos.min(src.len())];
    let bytes = line.as_bytes();
    let mut quote: Option<u8> = None;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = quote {
            if b == b'\\' {
                i += 2;
                continue;
            }
            if b == q {
                quote = None;
            }
        } else if b == b'"' || b == b'\'' || b == b'`' {
            quote = Some(b);
        } else if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            return true;
        }
        i += 1;
    }
    false
}
