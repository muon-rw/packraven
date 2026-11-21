// ProbeJS 7 declares each Java package as its own `declare module "@package/..."`, so two packages
// can both export a `$Level` or a `$Component`. We give a name declared by more than one module a
// key of its own per module, like `__m12.$Level`, and point each file's references at the module it
// imported from. Names only one module declares keep their plain key, so ProbeJS 6 dumps and the
// lookups by name in query.rs work as before.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use super::parser::{AliasDef, ParsedChunk};

const PREFIX: &str = "__m";

#[derive(Default)]
pub struct ModuleTable {
    ids: HashMap<String, u32>,
    /// The modules that declare each name more than one module declares, in the order we prefer
    /// them for a reference that doesn't say which one it means.
    owners: HashMap<String, Vec<String>>,
}

impl ModuleTable {
    pub fn build(chunks: &[ParsedChunk]) -> Self {
        let mut declared_by: HashMap<&str, HashSet<&str>> = HashMap::new();
        let mut modules: HashSet<&str> = HashSet::new();
        for chunk in chunks {
            for (module, name) in &chunk.module_decls {
                modules.insert(module);
                declared_by.entry(name).or_default().insert(module);
            }
        }
        let mut sorted: Vec<&str> = modules.into_iter().collect();
        sorted.sort_unstable();
        let ids = sorted.iter().enumerate().map(|(i, module)| (module.to_string(), i as u32)).collect();
        let owners = declared_by
            .into_iter()
            .filter(|(_, modules)| modules.len() > 1)
            .map(|(name, modules)| {
                let mut modules: Vec<String> = modules.into_iter().map(String::from).collect();
                modules.sort_unstable_by(|a, b| preference(a).cmp(&preference(b)).then_with(|| a.cmp(b)));
                (name.to_string(), modules)
            })
            .collect();
        Self { ids, owners }
    }

    pub fn collision_count(&self) -> usize {
        self.owners.len()
    }

    // ProbeJS sometimes imports a class from a module it never wrote it into, so a module only gets
    // its own key for a name it declares.
    fn key(&self, module: &str, name: &str) -> Option<String> {
        if !self.owners.get(name)?.iter().any(|owner| owner == module) {
            return None;
        }
        self.ids.get(module).map(|id| format!("{PREFIX}{id}.{name}"))
    }

    pub fn requalified_names(&self, chunk: &ParsedChunk) -> HashMap<String, String> {
        let mut scope = HashMap::new();
        for (module, name) in &chunk.module_decls {
            if let Some(key) = self.key(module, name) {
                scope.entry(name.clone()).or_insert(key);
            }
        }
        for import in &chunk.imports {
            let target = self.key(&import.module, &import.name).unwrap_or_else(|| import.name.clone());
            if target != import.local {
                scope.insert(import.local.clone(), target);
            }
        }
        scope
    }

    // Java.loadClass takes a class path, which names its package, so we can find the exact module.
    pub fn java_class_key(&self, class_path: &str) -> Option<String> {
        let (package, class) = class_path.rsplit_once('.')?;
        self.key(&format!("@package/{}", package.replace('.', "/")), &format!("${class}"))
    }

    // A reference that doesn't import its target, like one from your own JSDoc, gets the module we
    // prefer, with vanilla first, then KubeJS, then the JDK.
    pub fn add_fallbacks(&self, merged: &mut ParsedChunk) {
        let preferred: HashMap<String, &str> = self
            .owners
            .iter()
            .filter_map(|(name, modules)| Some((format!("{PREFIX}{}.{name}", self.ids.get(modules.first()?)?), name.as_str())))
            .collect();
        let plain = |key: &str| -> Option<String> {
            let (id, rest) = key.strip_prefix(PREFIX)?.split_once('.')?;
            let name = rest.split('.').next()?;
            preferred.get(&format!("{PREFIX}{id}.{name}")).map(|_| rest.to_string())
        };
        let types: Vec<(String, String)> = merged.types.keys().filter_map(|key| plain(key).map(|p| (key.clone(), p))).collect();
        for (key, plain) in types {
            let def = merged.types[&key].clone();
            merged.types.entry(plain).or_insert(def);
        }
        let aliases: Vec<(String, String)> = merged.aliases.keys().filter_map(|key| plain(key).map(|p| (key.clone(), p))).collect();
        for (key, plain) in aliases {
            let alias = merged.aliases[&key].clone();
            merged.aliases.entry(plain).or_insert(alias);
        }
    }
}

fn preference(module: &str) -> u8 {
    if module.starts_with("@package/net/minecraft/") {
        0
    } else if module.starts_with("@package/dev/latvian/") {
        1
    } else if module.starts_with("@package/java/") {
        2
    } else {
        3
    }
}

pub fn requalify(chunk: &mut ParsedChunk, scope: &HashMap<String, String>) {
    if scope.is_empty() {
        return;
    }
    let rewrite = |text: &mut String| {
        if let Some(rewritten) = rewrite_type_text(text, scope) {
            *text = rewritten;
        }
    };
    chunk.types = std::mem::take(&mut chunk.types)
        .into_iter()
        .map(|(key, mut def)| {
            def.extends.iter_mut().for_each(rewrite);
            for member in &mut def.members {
                rewrite(&mut member.return_type);
                member.params.iter_mut().for_each(|param| rewrite(&mut param.ty));
            }
            (rekey(key, scope), def)
        })
        .collect();
    chunk.aliases = std::mem::take(&mut chunk.aliases)
        .into_iter()
        .map(|(key, mut alias)| {
            if let AliasDef::Refs { types, templates, .. } = &mut alias {
                types.iter_mut().for_each(rewrite);
                templates.iter_mut().for_each(|(_, target)| rewrite(target));
            }
            (rekey(key, scope), alias)
        })
        .collect();
    for global in chunk.globals.values_mut() {
        rewrite(&mut global.ty);
    }
}

fn rekey(key: String, scope: &HashMap<String, String>) -> String {
    let (first, rest) = key.split_once('.').map_or((key.as_str(), ""), |(first, rest)| (first, rest));
    match scope.get(first) {
        Some(target) if rest.is_empty() => target.clone(),
        Some(target) => format!("{target}.{rest}"),
        None => key,
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Context {
    Brace,
    Template,
    TemplateExpr,
}

/// Returns `text` with each name in `scope` replaced, or `None` when nothing changed. We leave
/// string literals alone, but rewrite inside a template literal's `${...}`, since ProbeJS writes
/// tag types like `` `#${RegistryTypes.ItemTag}` ``.
pub fn rewrite_type_text(text: &str, scope: &HashMap<String, String>) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out: Option<String> = None;
    let mut copied = 0;
    let mut stack: Vec<Context> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if stack.last() == Some(&Context::Template) {
            if b == b'`' {
                stack.pop();
            } else if b == b'\\' {
                i += 1;
            } else if b == b'$' && bytes.get(i + 1) == Some(&b'{') {
                stack.push(Context::TemplateExpr);
                i += 1;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' | b'\'' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            }
            b'`' => {
                stack.push(Context::Template);
                i += 1;
            }
            b'{' => {
                stack.push(Context::Brace);
                i += 1;
            }
            b'}' => {
                stack.pop();
                i += 1;
            }
            b if is_ident_start(b) => {
                let start = i;
                while i < bytes.len() && is_ident_cont(bytes[i]) {
                    i += 1;
                }
                let member = start > 0 && bytes[start - 1] == b'.';
                let key = bytes.get(i) == Some(&b':') || (bytes.get(i) == Some(&b'?') && bytes.get(i + 1) == Some(&b':'));
                if member || key {
                    continue;
                }
                if let Some(target) = scope.get(&text[start..i]) {
                    let out = out.get_or_insert_with(|| String::with_capacity(text.len() + 16));
                    out.push_str(&text[copied..start]);
                    out.push_str(target);
                    copied = i;
                }
            }
            _ => i += 1,
        }
    }
    let mut out = out?;
    out.push_str(&text[copied..]);
    Some(out)
}

pub fn strip_module_prefixes(text: &str) -> Cow<'_, str> {
    if !text.contains(PREFIX) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(PREFIX) {
        let after = &rest[at + PREFIX.len()..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        let starts_word = at == 0 || !is_ident_cont(rest.as_bytes()[at - 1]);
        if starts_word && digits > 0 && after.as_bytes().get(digits) == Some(&b'.') {
            out.push_str(&rest[..at]);
            rest = &after[digits + 1..];
        } else {
            out.push_str(&rest[..at + PREFIX.len()]);
            rest = after;
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b'$'
}

fn is_ident_cont(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    #[test]
    fn rewrites_type_names_but_not_members_keys_or_strings() {
        let scope = scope(&[("$Level", "__m3.$Level"), ("$Function$1", "$Function")]);
        assert_eq!(rewrite_type_text("$List_<$Level>", &scope).as_deref(), Some("$List_<__m3.$Level>"));
        assert_eq!(rewrite_type_text("((level: $Level) => $Function$1)", &scope).as_deref(), Some("((level: __m3.$Level) => $Function)"));
        assert_eq!(rewrite_type_text("Foo.$Level | \"$Level\"", &scope), None);
        assert_eq!(rewrite_type_text("{ $Level: string, other?: $Level }", &scope).as_deref(), Some("{ $Level: string, other?: __m3.$Level }"));
        assert_eq!(rewrite_type_text("`#${$Level}` | `$Level`", &scope).as_deref(), Some("`#${__m3.$Level}` | `$Level`"));
    }

    #[test]
    fn strips_module_keys_for_display() {
        assert_eq!(strip_module_prefixes("$List<__m12.$Level> | __m3.$Component_"), "$List<$Level> | $Component_");
        assert_eq!(strip_module_prefixes("my__m1.x"), "my__m1.x");
        assert_eq!(strip_module_prefixes("$Plain"), "$Plain");
    }
}
