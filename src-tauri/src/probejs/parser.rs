use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeKind {
    Class,
    Interface,
    Namespace,
    Object,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberKind {
    Method,
    Property,
    Getter,
    Const,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: String,
    pub optional: bool,
    pub rest: bool,
    pub literal: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Member {
    pub name: String,
    pub kind: MemberKind,
    pub is_static: bool,
    pub params: Vec<Param>,
    pub return_type: String,
    pub docs: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TypeDef {
    pub kind: TypeKind,
    pub extends: Vec<String>,
    pub members: Vec<Member>,
    pub docs: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GlobalSymbol {
    pub name: String,
    pub kind: &'static str,
    pub ty: String,
    /// True for `declare class Foo` (member access hits static members unless instantiated).
    pub is_class_value: bool,
    pub signature: Option<String>,
    pub docs: Option<String>,
}

#[derive(Debug, Clone)]
pub enum AliasDef {
    StringUnion(Vec<Box<str>>),
    Refs {
        types: Vec<String>,
        templates: Vec<(String, String)>,
        literals: Vec<String>,
        object_type: Option<String>,
    },
}

#[derive(Default)]
pub struct ParsedChunk {
    pub types: HashMap<String, TypeDef>,
    pub globals: HashMap<String, GlobalSymbol>,
    pub aliases: HashMap<String, AliasDef>,
    pub anon_counter: u32,
    pub imports: Vec<Import>,
    /// `(module, name)` for each declaration at the top level of a `declare module "..."` block.
    pub module_decls: Vec<(String, String)>,
}

pub struct Import {
    pub local: String,
    pub name: String,
    pub module: String,
}

impl ParsedChunk {
    pub fn merge_into(self, target: &mut ParsedChunk) {
        for (name, def) in self.types {
            match target.types.get_mut(&name) {
                Some(existing) => {
                    for base in def.extends {
                        if !existing.extends.contains(&base) {
                            existing.extends.push(base);
                        }
                    }
                    existing.members.extend(def.members);
                    if existing.docs.is_none() {
                        existing.docs = def.docs;
                    }
                }
                None => {
                    target.types.insert(name, def);
                }
            }
        }
        for (name, sym) in self.globals {
            target.globals.entry(name).or_insert(sym);
        }
        for (name, alias) in self.aliases {
            target.aliases.entry(name).or_insert(alias);
        }
    }
}

// Java's Object methods repeat on every class in internals/ and clutter completions.
const SKIP_INTERNAL_MEMBERS: [&str; 4] = ["wait", "notify", "notifyAll", "getClass"];
const MAX_INLINE_OBJECT_MEMBERS: usize = 2500;

pub fn parse_dts(text: &str, file_id: u32, is_internal: bool, keep_docs: bool) -> ParsedChunk {
    let mut parser = Parser {
        src: text,
        bytes: text.as_bytes(),
        pos: 0,
        file_id,
        is_internal,
        keep_docs,
        module: None,
        ns_stack: Vec::new(),
        pending_doc: None,
        chunk: ParsedChunk::default(),
    };
    parser.parse_block(false);
    parser.chunk
}

struct Parser<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    file_id: u32,
    is_internal: bool,
    keep_docs: bool,
    module: Option<String>,
    ns_stack: Vec<String>,
    pending_doc: Option<String>,
    chunk: ParsedChunk,
}

impl<'a> Parser<'a> {
    fn next_anon_name(&mut self, hint: &str) -> String {
        self.chunk.anon_counter += 1;
        format!("__obj_{}_{}_{}", self.file_id, self.chunk.anon_counter, hint)
    }

    fn qualify(&self, name: &str) -> String {
        if self.ns_stack.is_empty() {
            name.to_string()
        } else {
            format!("{}.{name}", self.ns_stack.join("."))
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn starts_with(&self, s: &str) -> bool {
        self.src[self.pos..].starts_with(s)
    }

    fn skip_ws_and_comments(&mut self, capture_doc: bool) {
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            if b.is_ascii_whitespace() {
                self.pos += 1;
                continue;
            }
            if b == b'/' && self.pos + 1 < self.bytes.len() {
                let next = self.bytes[self.pos + 1];
                if next == b'/' {
                    self.pos += 2;
                    while self.pos < self.bytes.len() && self.bytes[self.pos] != b'\n' {
                        self.pos += 1;
                    }
                    continue;
                }
                if next == b'*' {
                    let is_doc = capture_doc && self.keep_docs && self.pos + 2 < self.bytes.len() && self.bytes[self.pos + 2] == b'*' && self.bytes.get(self.pos + 3) != Some(&b'/');
                    let start = self.pos + 2;
                    self.pos += 2;
                    while self.pos + 1 < self.bytes.len() {
                        if self.bytes[self.pos] == b'*' && self.bytes[self.pos + 1] == b'/' {
                            let end = self.pos;
                            self.pos += 2;
                            if is_doc && end > start + 1 {
                                let cleaned = clean_jsdoc(&self.src[start + 1..end]);
                                if !cleaned.is_empty() {
                                    self.pending_doc = Some(cleaned);
                                }
                            }
                            break;
                        }
                        self.pos += 1;
                    }
                    continue;
                }
            }
            break;
        }
    }

    fn eat_ident(&mut self) -> Option<&'a str> {
        let start = self.pos;
        if self.pos < self.bytes.len() && is_ident_start(self.bytes[self.pos]) {
            self.pos += 1;
            while self.pos < self.bytes.len() && is_ident_cont(self.bytes[self.pos]) {
                self.pos += 1;
            }
            Some(&self.src[start..self.pos])
        } else {
            None
        }
    }

    fn eat_qualified_ident(&mut self) -> Option<String> {
        let first = self.eat_ident()?;
        let mut out = first.to_string();
        while self.peek() == Some(b'.') {
            self.pos += 1;
            if let Some(part) = self.eat_ident() {
                out.push('.');
                out.push_str(part);
            } else {
                break;
            }
        }
        Some(out)
    }

    fn eat_keyword(&mut self, kw: &str) -> bool {
        let save = self.pos;
        if let Some(id) = self.eat_ident()
            && id == kw
        {
            return true;
        }
        self.pos = save;
        false
    }

    fn eat_string_literal(&mut self) -> Option<String> {
        let quote = self.peek()?;
        if quote != b'"' && quote != b'\'' {
            return None;
        }
        self.pos += 1;
        let start = self.pos;
        let mut escaped = false;
        let mut has_escape = false;
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            self.pos += 1;
            if escaped {
                escaped = false;
                continue;
            }
            if b == b'\\' {
                escaped = true;
                has_escape = true;
                continue;
            }
            if b == quote {
                let raw = &self.src[start..self.pos - 1];
                return Some(if has_escape { unescape_str(raw) } else { raw.to_string() });
            }
            if b == b'\n' {
                break;
            }
        }
        None
    }

    fn skip_balanced(&mut self, open: u8, close: u8) {
        if self.peek() != Some(open) {
            return;
        }
        self.pos += 1;
        let mut depth = 1usize;
        while self.pos < self.bytes.len() && depth > 0 {
            let b = self.bytes[self.pos];
            if b == b'"' || b == b'\'' || b == b'`' {
                self.skip_quoted(b);
                continue;
            }
            if b == b'/' && self.pos + 1 < self.bytes.len() {
                let next = self.bytes[self.pos + 1];
                if next == b'/' {
                    while self.pos < self.bytes.len() && self.bytes[self.pos] != b'\n' {
                        self.pos += 1;
                    }
                    continue;
                }
                if next == b'*' {
                    self.pos += 2;
                    while self.pos + 1 < self.bytes.len() {
                        if self.bytes[self.pos] == b'*' && self.bytes[self.pos + 1] == b'/' {
                            self.pos += 2;
                            break;
                        }
                        self.pos += 1;
                    }
                    continue;
                }
            }
            if b == open {
                depth += 1;
            } else if b == close {
                depth -= 1;
            }
            self.pos += 1;
        }
    }

    fn skip_quoted(&mut self, quote: u8) {
        self.pos += 1;
        let mut escaped = false;
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            self.pos += 1;
            if escaped {
                escaped = false;
                continue;
            }
            if b == b'\\' {
                escaped = true;
                continue;
            }
            if b == quote {
                return;
            }
            if quote != b'`' && b == b'\n' {
                return;
            }
        }
    }

    fn skip_generics(&mut self) {
        self.skip_ws_and_comments(false);
        if self.peek() == Some(b'<') {
            self.skip_balanced(b'<', b'>');
        }
    }

    fn skip_statement(&mut self) {
        let mut depth = 0usize;
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            if b == b'"' || b == b'\'' || b == b'`' {
                self.skip_quoted(b);
                continue;
            }
            if b == b'/' && self.pos + 1 < self.bytes.len() && (self.bytes[self.pos + 1] == b'/' || self.bytes[self.pos + 1] == b'*') {
                self.skip_ws_and_comments(false);
                continue;
            }
            match b {
                b'{' | b'(' | b'[' => {
                    depth += 1;
                    self.pos += 1;
                }
                b'}' | b')' | b']' => {
                    if depth == 0 {
                        return;
                    }
                    depth -= 1;
                    self.pos += 1;
                    if depth == 0 && b == b'}' {
                        return;
                    }
                }
                b';' if depth == 0 => {
                    self.pos += 1;
                    return;
                }
                _ => self.pos += 1,
            }
        }
    }

    fn parse_block(&mut self, stop_at_brace: bool) {
        while self.pos < self.bytes.len() {
            self.skip_ws_and_comments(true);
            if self.pos >= self.bytes.len() {
                break;
            }
            if stop_at_brace && self.peek() == Some(b'}') {
                self.pos += 1;
                return;
            }
            if self.peek() == Some(b';') {
                self.pos += 1;
                self.pending_doc = None;
                continue;
            }

            let doc = self.pending_doc.take();
            let mut is_declare = false;
            loop {
                self.skip_ws_and_comments(false);
                if self.eat_keyword("export") || self.eat_keyword("default") {
                    continue;
                }
                if self.eat_keyword("declare") {
                    is_declare = true;
                    continue;
                }
                if self.eat_keyword("abstract") {
                    continue;
                }
                break;
            }
            self.skip_ws_and_comments(false);

            if self.eat_keyword("import") {
                self.parse_import();
            } else if self.eat_keyword("global") {
                self.skip_ws_and_comments(false);
                if self.peek() == Some(b'{') {
                    self.pos += 1;
                    let module = self.module.take();
                    self.parse_block(true);
                    self.module = module;
                } else {
                    self.skip_statement();
                }
            } else if self.eat_keyword("namespace") || self.eat_keyword("module") {
                self.parse_namespace(doc);
            } else if self.eat_keyword("class") {
                self.parse_type_decl(TypeKind::Class, doc);
            } else if self.eat_keyword("interface") {
                self.parse_type_decl(TypeKind::Interface, doc);
            } else if self.eat_keyword("type") {
                self.parse_type_alias();
            } else if self.eat_keyword("const") || self.eat_keyword("var") || self.eat_keyword("let") {
                self.parse_var_decl(doc);
            } else if self.eat_keyword("function") {
                self.parse_function_decl(doc);
            } else if self.eat_keyword("enum") {
                self.parse_enum_decl(doc);
            } else if is_declare {
                self.skip_statement();
            } else if self.peek() == Some(b'}') {
                self.pos += 1;
                if stop_at_brace {
                    return;
                }
            } else {
                self.skip_statement();
            }
        }
    }

    // ProbeJS 7 imports every name it uses from another module, renaming the ones that clash, as in
    // `import { $Function as $Function$1 } from "@package/java/util/function"`.
    fn parse_import(&mut self) {
        self.skip_ws_and_comments(false);
        self.eat_keyword("type");
        self.skip_ws_and_comments(false);
        if self.peek() != Some(b'{') {
            self.skip_to_semicolon();
            return;
        }
        self.pos += 1;
        let mut names = Vec::new();
        loop {
            self.skip_ws_and_comments(false);
            match self.peek() {
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                Some(b',') => {
                    self.pos += 1;
                    continue;
                }
                None => return,
                _ => {}
            }
            self.eat_keyword("type");
            self.skip_ws_and_comments(false);
            let Some(name) = self.eat_ident().map(str::to_string) else {
                self.skip_to_semicolon();
                return;
            };
            self.skip_ws_and_comments(false);
            let local = if self.eat_keyword("as") {
                self.skip_ws_and_comments(false);
                self.eat_ident().map_or_else(|| name.clone(), str::to_string)
            } else {
                name.clone()
            };
            names.push((local, name));
        }
        self.skip_ws_and_comments(false);
        if !self.eat_keyword("from") {
            self.skip_to_semicolon();
            return;
        }
        self.skip_ws_and_comments(false);
        let Some(module) = self.eat_string_literal() else {
            self.skip_to_semicolon();
            return;
        };
        if self.peek() == Some(b';') {
            self.pos += 1;
        }
        self.chunk.imports.extend(names.into_iter().map(|(local, name)| Import { local, name, module: module.clone() }));
    }

    // A module's own declarations aren't globals in KubeJS, so we record them for the module table
    // instead of offering them as identifiers.
    fn declares_module_export(&mut self, name: &str) -> bool {
        match &self.module {
            Some(module) if self.ns_stack.is_empty() => {
                self.chunk.module_decls.push((module.clone(), name.to_string()));
                true
            }
            _ => false,
        }
    }

    fn skip_to_semicolon(&mut self) {
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            if b == b'"' || b == b'\'' || b == b'`' {
                self.skip_quoted(b);
                continue;
            }
            self.pos += 1;
            if b == b';' {
                return;
            }
        }
    }

    fn parse_namespace(&mut self, doc: Option<String>) {
        self.skip_ws_and_comments(false);
        let name = if self.peek() == Some(b'"') || self.peek() == Some(b'\'') {
            // ProbeJS 7 wraps per-package and special declarations in `declare module "@package/..." { ... }`.
            let module = self.eat_string_literal();
            self.skip_ws_and_comments(false);
            if self.peek() == Some(b'{') {
                self.pos += 1;
                let outer = std::mem::replace(&mut self.module, module);
                self.parse_block(true);
                self.module = outer;
            }
            return;
        } else {
            let Some(n) = self.eat_qualified_ident() else {
                self.skip_statement();
                return;
            };
            n
        };
        self.skip_ws_and_comments(false);
        if self.peek() != Some(b'{') {
            self.skip_statement();
            return;
        }
        self.pos += 1;

        let qualified = self.qualify(&name);
        let exported = self.declares_module_export(name.split('.').next().unwrap_or(&name));
        if self.ns_stack.is_empty() && !exported {
            let top = name.split('.').next().unwrap_or(&name);
            self.chunk.globals.entry(top.to_string()).or_insert_with(|| GlobalSymbol {
                name: top.to_string(),
                kind: "namespace",
                ty: top.to_string(),
                is_class_value: false,
                signature: None,
                docs: doc.clone(),
            });
        } else if !self.ns_stack.is_empty() {
            let parent = self.ns_stack.join(".");
            let child_name = name.split('.').next().unwrap_or(&name).to_string();
            let target = self.qualify(&child_name);
            let parent_def = self.chunk.types.entry(parent).or_insert_with(|| TypeDef {
                kind: TypeKind::Namespace,
                extends: Vec::new(),
                members: Vec::new(),
                docs: None,
            });
            if !parent_def.members.iter().any(|m| m.name == child_name) {
                parent_def.members.push(Member {
                    name: child_name,
                    kind: MemberKind::Property,
                    is_static: false,
                    params: Vec::new(),
                    return_type: target,
                    docs: doc.clone(),
                });
            }
        }
        self.chunk.types.entry(qualified).or_insert_with(|| TypeDef {
            kind: TypeKind::Namespace,
            extends: Vec::new(),
            members: Vec::new(),
            docs: doc,
        });

        self.ns_stack.push(name);
        self.parse_block(true);
        self.ns_stack.pop();
    }

    fn parse_type_decl(&mut self, kind: TypeKind, doc: Option<String>) {
        self.skip_ws_and_comments(false);
        let Some(raw_name) = self.eat_ident() else {
            self.skip_statement();
            return;
        };
        let raw_name = raw_name.to_string();
        self.skip_generics();

        let mut extends = Vec::new();
        loop {
            self.skip_ws_and_comments(false);
            if self.eat_keyword("extends") || self.eat_keyword("implements") {
                loop {
                    self.skip_ws_and_comments(false);
                    if let Some(base) = self.eat_qualified_ident() {
                        self.skip_generics();
                        let resolved = if !base.contains('.') && !self.ns_stack.is_empty() {
                            self.qualify(&base)
                        } else {
                            base
                        };
                        extends.push(resolved);
                    }
                    self.skip_ws_and_comments(false);
                    if self.peek() == Some(b',') {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
            } else {
                break;
            }
        }

        self.skip_ws_and_comments(false);
        if self.peek() != Some(b'{') {
            self.skip_statement();
            return;
        }
        let members = self.parse_object_body(false, MAX_INLINE_OBJECT_MEMBERS * 20);
        let qualified = self.qualify(&raw_name);
        let exported = self.declares_module_export(&raw_name);

        if self.ns_stack.is_empty() && kind == TypeKind::Class && !exported {
            self.chunk.globals.entry(raw_name.clone()).or_insert_with(|| GlobalSymbol {
                name: raw_name.clone(),
                kind: "class",
                ty: qualified.clone(),
                is_class_value: true,
                signature: None,
                docs: doc.clone(),
            });
        } else if !self.ns_stack.is_empty() && kind == TypeKind::Class {
            let parent = self.ns_stack.join(".");
            let parent_def = self.chunk.types.entry(parent).or_insert_with(|| TypeDef {
                kind: TypeKind::Namespace,
                extends: Vec::new(),
                members: Vec::new(),
                docs: None,
            });
            if !parent_def.members.iter().any(|m| m.name == raw_name) {
                parent_def.members.push(Member {
                    name: raw_name,
                    kind: MemberKind::Property,
                    is_static: false,
                    params: Vec::new(),
                    return_type: qualified.clone(),
                    docs: doc.clone(),
                });
            }
        }

        match self.chunk.types.get_mut(&qualified) {
            Some(existing) => {
                for b in extends {
                    if !existing.extends.contains(&b) {
                        existing.extends.push(b);
                    }
                }
                existing.members.extend(members);
                if existing.docs.is_none() {
                    existing.docs = doc;
                }
            }
            None => {
                self.chunk.types.insert(qualified, TypeDef { kind, extends, members, docs: doc });
            }
        }
    }

    fn parse_enum_decl(&mut self, doc: Option<String>) {
        self.skip_ws_and_comments(false);
        let Some(raw_name) = self.eat_ident() else {
            self.skip_statement();
            return;
        };
        let raw_name = raw_name.to_string();
        self.skip_ws_and_comments(false);
        if self.peek() != Some(b'{') {
            self.skip_statement();
            return;
        }
        self.pos += 1;
        let mut members = Vec::new();
        while self.pos < self.bytes.len() {
            self.skip_ws_and_comments(false);
            if self.peek() == Some(b'}') {
                self.pos += 1;
                break;
            }
            if let Some(member) = self.eat_ident() {
                members.push(Member {
                    name: member.to_string(),
                    kind: MemberKind::Const,
                    is_static: true,
                    params: Vec::new(),
                    return_type: self.qualify(&raw_name),
                    docs: None,
                });
            }
            while self.pos < self.bytes.len() && self.bytes[self.pos] != b',' && self.bytes[self.pos] != b'}' {
                self.pos += 1;
            }
            if self.peek() == Some(b',') {
                self.pos += 1;
            }
        }
        let qualified = self.qualify(&raw_name);
        if self.ns_stack.is_empty() && !self.declares_module_export(&raw_name) {
            self.chunk.globals.entry(raw_name).or_insert_with(|| GlobalSymbol {
                name: qualified.clone(),
                kind: "enum",
                ty: qualified.clone(),
                is_class_value: true,
                signature: None,
                docs: doc.clone(),
            });
        }
        self.chunk.types.insert(qualified, TypeDef { kind: TypeKind::Object, extends: Vec::new(), members, docs: doc });
    }

    fn parse_var_decl(&mut self, doc: Option<String>) {
        self.skip_ws_and_comments(false);
        let Some(raw_name) = self.eat_ident() else {
            self.skip_statement();
            return;
        };
        let raw_name = raw_name.to_string();
        self.skip_ws_and_comments(false);
        let ty = if self.peek() == Some(b':') {
            self.pos += 1;
            self.skip_ws_and_comments(false);
            self.parse_type_expr(&raw_name)
        } else {
            "any".to_string()
        };
        self.skip_ws_and_comments(false);
        if self.peek() == Some(b';') {
            self.pos += 1;
        }

        let (ty, is_class_value) = if let Some(rest) = ty.strip_prefix("typeof ") {
            (rest.trim().to_string(), true)
        } else {
            (ty, false)
        };

        if self.declares_module_export(&raw_name) {
            return;
        }
        if self.ns_stack.is_empty() {
            self.chunk.globals.insert(
                raw_name.clone(),
                GlobalSymbol {
                    name: raw_name,
                    kind: if is_class_value { "class" } else { "constant" },
                    ty,
                    is_class_value,
                    signature: None,
                    docs: doc,
                },
            );
        } else {
            let parent = self.ns_stack.join(".");
            let parent_def = self.chunk.types.entry(parent).or_insert_with(|| TypeDef {
                kind: TypeKind::Namespace,
                extends: Vec::new(),
                members: Vec::new(),
                docs: None,
            });
            parent_def.members.push(Member {
                name: raw_name,
                kind: MemberKind::Const,
                is_static: false,
                params: Vec::new(),
                return_type: ty,
                docs: doc,
            });
        }
    }

    fn parse_function_decl(&mut self, doc: Option<String>) {
        self.skip_ws_and_comments(false);
        let Some(raw_name) = self.eat_ident() else {
            self.skip_statement();
            return;
        };
        let raw_name = raw_name.to_string();
        self.skip_generics();
        self.skip_ws_and_comments(false);
        let params = if self.peek() == Some(b'(') { self.parse_param_list() } else { Vec::new() };
        self.skip_ws_and_comments(false);
        let return_type = if self.peek() == Some(b':') {
            self.pos += 1;
            self.skip_ws_and_comments(false);
            self.parse_type_expr(&raw_name)
        } else {
            "void".to_string()
        };
        if self.peek() == Some(b';') {
            self.pos += 1;
        }
        let sig = format!("function {raw_name}({}): {return_type}", format_params(&params));
        if self.declares_module_export(&raw_name) {
            return;
        }
        if self.ns_stack.is_empty() {
            self.chunk.globals.entry(raw_name.clone()).or_insert_with(|| GlobalSymbol {
                name: raw_name,
                kind: "function",
                ty: return_type,
                is_class_value: false,
                signature: Some(sig),
                docs: doc,
            });
        } else {
            let parent = self.ns_stack.join(".");
            let parent_def = self.chunk.types.entry(parent).or_insert_with(|| TypeDef {
                kind: TypeKind::Namespace,
                extends: Vec::new(),
                members: Vec::new(),
                docs: None,
            });
            parent_def.members.push(Member {
                name: raw_name,
                kind: MemberKind::Method,
                is_static: false,
                params,
                return_type,
                docs: doc,
            });
        }
    }

    fn parse_type_alias(&mut self) {
        self.skip_ws_and_comments(false);
        let Some(raw_name) = self.eat_ident() else {
            self.skip_statement();
            return;
        };
        let qualified = self.qualify(raw_name);
        self.declares_module_export(raw_name);
        self.skip_generics();
        self.skip_ws_and_comments(false);
        if self.peek() != Some(b'=') {
            self.skip_statement();
            return;
        }
        self.pos += 1;
        self.skip_ws_and_comments(false);

        if matches!(self.peek(), Some(b'"' | b'\'')) {
            let save = self.pos;
            if let Some(items) = self.try_parse_pure_string_union() {
                self.chunk.aliases.insert(qualified, AliasDef::StringUnion(items));
                return;
            }
            self.pos = save;
        }

        let rhs = self.read_alias_rhs();
        let ns = self.ns_stack.clone();
        let alias = parse_alias_rhs(&rhs, &ns, |obj_src: &str| {
            let anon = self.next_anon_name("alias");
            let mut sub = Parser {
                src: obj_src,
                bytes: obj_src.as_bytes(),
                pos: 0,
                file_id: self.file_id,
                is_internal: self.is_internal,
                keep_docs: self.keep_docs,
                module: self.module.clone(),
                ns_stack: ns.clone(),
                pending_doc: None,
                chunk: ParsedChunk {
                    anon_counter: self.chunk.anon_counter,
                    ..ParsedChunk::default()
                },
            };
            let members = sub.parse_object_body(true, MAX_INLINE_OBJECT_MEMBERS);
            self.chunk.anon_counter = sub.chunk.anon_counter;
            sub.chunk.merge_into(&mut self.chunk);
            self.chunk.types.insert(
                anon.clone(),
                TypeDef {
                    kind: TypeKind::Object,
                    extends: Vec::new(),
                    members,
                    docs: None,
                },
            );
            anon
        });
        self.chunk.aliases.insert(qualified, alias);
    }

    fn try_parse_pure_string_union(&mut self) -> Option<Vec<Box<str>>> {
        let mut items: Vec<Box<str>> = Vec::new();
        loop {
            while self.pos < self.bytes.len() && matches!(self.bytes[self.pos], b' ' | b'\t' | b'\r') {
                self.pos += 1;
            }
            let s = self.eat_string_literal()?;
            items.push(s.into_boxed_str());
            while self.pos < self.bytes.len() && matches!(self.bytes[self.pos], b' ' | b'\t' | b'\r') {
                self.pos += 1;
            }
            match self.peek() {
                Some(b'|') => {
                    self.pos += 1;
                }
                Some(b';') => {
                    self.pos += 1;
                    return Some(items);
                }
                Some(b'\n') | None => {
                    if self.peek() == Some(b'\n') {
                        self.pos += 1;
                    }
                    return Some(items);
                }
                _ => return None,
            }
        }
    }

    fn read_alias_rhs(&mut self) -> String {
        let start = self.pos;
        let mut depth = 0usize;
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            if b == b'"' || b == b'\'' || b == b'`' {
                self.skip_quoted(b);
                continue;
            }
            match b {
                b'{' | b'(' | b'[' | b'<' => {
                    depth += 1;
                    self.pos += 1;
                }
                b'}' => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                    self.pos += 1;
                }
                b')' | b']' | b'>' => {
                    if b == b'>' && self.pos > 0 && self.bytes[self.pos - 1] == b'=' {
                        self.pos += 1;
                    } else {
                        depth = depth.saturating_sub(1);
                        self.pos += 1;
                    }
                }
                b';' if depth == 0 => {
                    let out = self.src[start..self.pos].trim().to_string();
                    self.pos += 1;
                    return out;
                }
                b'\n' if depth == 0 => {
                    let nl = self.pos;
                    let mut look = nl + 1;
                    while look < self.bytes.len() && matches!(self.bytes[look], b' ' | b'\t' | b'\r') {
                        look += 1;
                    }
                    if look < self.bytes.len() && matches!(self.bytes[look], b'|' | b'&') {
                        self.pos = look;
                        continue;
                    }
                    let out = self.src[start..nl].trim().to_string();
                    self.pos = nl + 1;
                    return out;
                }
                _ => self.pos += 1,
            }
        }
        self.src[start..self.pos].trim().to_string()
    }

    fn parse_object_body(&mut self, is_inline: bool, max_members: usize) -> Vec<Member> {
        let mut members = Vec::new();
        if self.peek() != Some(b'{') {
            return members;
        }
        self.pos += 1;
        let mut quoted_count = 0usize;

        while self.pos < self.bytes.len() {
            self.skip_ws_and_comments(true);
            if self.pos >= self.bytes.len() {
                break;
            }
            if self.peek() == Some(b'}') {
                self.pos += 1;
                break;
            }
            if matches!(self.peek(), Some(b';' | b',')) {
                self.pos += 1;
                self.pending_doc = None;
                continue;
            }
            if members.len() >= max_members {
                let mut depth = 1usize;
                while self.pos < self.bytes.len() && depth > 0 {
                    let b = self.bytes[self.pos];
                    if b == b'"' || b == b'\'' || b == b'`' {
                        self.skip_quoted(b);
                        continue;
                    }
                    if b == b'{' {
                        depth += 1;
                    } else if b == b'}' {
                        depth -= 1;
                    }
                    self.pos += 1;
                }
                break;
            }

            let doc = self.pending_doc.take();
            let mut is_static = false;
            let mut is_getter = false;
            let mut is_setter = false;

            loop {
                self.skip_ws_and_comments(false);
                if self.eat_keyword("static") {
                    is_static = true;
                    continue;
                }
                if self.eat_keyword("readonly") || self.eat_keyword("abstract") || self.eat_keyword("public") || self.eat_keyword("protected") || self.eat_keyword("private") || self.eat_keyword("declare") {
                    continue;
                }
                break;
            }
            self.skip_ws_and_comments(false);

            let save_acc = self.pos;
            if self.eat_keyword("get") {
                self.skip_ws_and_comments(false);
                if self.peek().is_some_and(is_ident_start) {
                    is_getter = true;
                } else {
                    self.pos = save_acc;
                }
            } else if self.eat_keyword("set") {
                self.skip_ws_and_comments(false);
                if self.peek().is_some_and(is_ident_start) {
                    is_setter = true;
                } else {
                    self.pos = save_acc;
                }
            }

            if self.peek() == Some(b'[') || self.peek() == Some(b'(') || self.peek() == Some(b'<') {
                self.skip_member_tail();
                continue;
            }

            let name = if matches!(self.peek(), Some(b'"' | b'\'')) {
                quoted_count += 1;
                let Some(q) = self.eat_string_literal() else {
                    self.skip_member_tail();
                    continue;
                };
                if q.contains('(') || (is_inline && quoted_count > 120) {
                    self.skip_member_tail();
                    continue;
                }
                q
            } else if let Some(id) = self.eat_ident() {
                id.to_string()
            } else {
                self.pos += 1;
                continue;
            };

            if is_setter || name == "constructor" || (self.is_internal && is_noisy_internal_member(&name)) {
                self.skip_member_tail();
                continue;
            }

            self.skip_ws_and_comments(false);
            if self.peek() == Some(b'?') {
                self.pos += 1;
                self.skip_ws_and_comments(false);
            }
            if self.peek() == Some(b'<') {
                self.skip_generics();
                self.skip_ws_and_comments(false);
            }

            if self.peek() == Some(b'(') {
                let params = self.parse_param_list();
                self.skip_ws_and_comments(false);
                let return_type = if self.peek() == Some(b':') {
                    self.pos += 1;
                    self.skip_ws_and_comments(false);
                    self.parse_type_expr(&name)
                } else {
                    "void".to_string()
                };
                self.eat_member_sep();
                if is_getter {
                    members.push(Member {
                        name,
                        kind: MemberKind::Getter,
                        is_static,
                        params: Vec::new(),
                        return_type,
                        docs: doc,
                    });
                } else {
                    members.push(Member {
                        name,
                        kind: MemberKind::Method,
                        is_static,
                        params,
                        return_type,
                        docs: doc,
                    });
                }
            } else if self.peek() == Some(b':') {
                self.pos += 1;
                self.skip_ws_and_comments(false);
                let return_type = self.parse_type_expr(&name);
                self.eat_member_sep();
                members.push(Member {
                    name,
                    kind: MemberKind::Property,
                    is_static,
                    params: Vec::new(),
                    return_type,
                    docs: doc,
                });
            } else {
                self.skip_member_tail();
            }
        }
        members
    }

    fn eat_member_sep(&mut self) {
        self.skip_ws_and_comments(false);
        if matches!(self.peek(), Some(b';' | b',')) {
            self.pos += 1;
        }
    }

    fn skip_member_tail(&mut self) {
        let mut depth = 0usize;
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            if b == b'"' || b == b'\'' || b == b'`' {
                self.skip_quoted(b);
                continue;
            }
            if b == b'/' && self.pos + 1 < self.bytes.len() && (self.bytes[self.pos + 1] == b'/' || self.bytes[self.pos + 1] == b'*') {
                self.skip_ws_and_comments(false);
                continue;
            }
            match b {
                b'{' | b'(' | b'[' => {
                    depth += 1;
                    self.pos += 1;
                }
                b'}' => {
                    if depth == 0 {
                        return;
                    }
                    depth -= 1;
                    self.pos += 1;
                }
                b')' | b']' => {
                    depth = depth.saturating_sub(1);
                    self.pos += 1;
                }
                b';' | b',' | b'\n' if depth == 0 => {
                    if b != b'\n' {
                        self.pos += 1;
                    }
                    return;
                }
                _ => self.pos += 1,
            }
        }
    }

    fn parse_param_list(&mut self) -> Vec<Param> {
        let mut params = Vec::new();
        if self.peek() != Some(b'(') {
            return params;
        }
        self.pos += 1;
        while self.pos < self.bytes.len() {
            self.skip_ws_and_comments(false);
            if self.peek() == Some(b')') {
                self.pos += 1;
                break;
            }
            let rest = if self.starts_with("...") {
                self.pos += 3;
                true
            } else {
                false
            };
            self.skip_ws_and_comments(false);
            let Some(raw_name) = self.eat_ident() else {
                let mut depth = 0usize;
                while self.pos < self.bytes.len() {
                    let b = self.bytes[self.pos];
                    if b == b'(' || b == b'{' || b == b'[' {
                        depth += 1;
                    } else if b == b')' {
                        if depth == 0 {
                            self.pos += 1;
                            return params;
                        }
                        depth -= 1;
                    } else if b == b',' && depth == 0 {
                        self.pos += 1;
                        break;
                    } else if b == b'}' || b == b']' {
                        depth = depth.saturating_sub(1);
                    }
                    self.pos += 1;
                }
                continue;
            };
            let name = raw_name.to_string();
            self.skip_ws_and_comments(false);
            let optional = if self.peek() == Some(b'?') {
                self.pos += 1;
                true
            } else {
                false
            };
            self.skip_ws_and_comments(false);
            let ty = if self.peek() == Some(b':') {
                self.pos += 1;
                self.skip_ws_and_comments(false);
                self.read_param_type()
            } else {
                "any".to_string()
            };
            let literal = extract_single_string_literal(&ty);
            params.push(Param { name, ty, optional, rest, literal });
            self.skip_ws_and_comments(false);
            if self.peek() == Some(b',') {
                self.pos += 1;
            } else if self.peek() == Some(b')') {
                self.pos += 1;
                break;
            }
        }
        params
    }

    fn read_param_type(&mut self) -> String {
        let start = self.pos;
        let mut depth = 0usize;
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            if b == b'"' || b == b'\'' || b == b'`' {
                self.skip_quoted(b);
                continue;
            }
            match b {
                b'(' | b'{' | b'[' | b'<' => {
                    depth += 1;
                    self.pos += 1;
                }
                b')' => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                    self.pos += 1;
                }
                b'}' | b']' | b'>' => {
                    if b == b'>' && self.pos > 0 && self.bytes[self.pos - 1] == b'=' {
                        self.pos += 1;
                    } else {
                        depth = depth.saturating_sub(1);
                        self.pos += 1;
                    }
                }
                b',' if depth == 0 => break,
                _ => self.pos += 1,
            }
        }
        self.src[start..self.pos].trim().to_string()
    }

    fn parse_type_expr(&mut self, hint: &str) -> String {
        self.skip_ws_and_comments(false);
        if self.peek() == Some(b'{') {
            let anon = self.next_anon_name(hint);
            let members = self.parse_object_body(true, MAX_INLINE_OBJECT_MEMBERS);
            self.chunk.types.insert(
                anon.clone(),
                TypeDef {
                    kind: TypeKind::Object,
                    extends: Vec::new(),
                    members,
                    docs: None,
                },
            );
            return anon;
        }
        let start = self.pos;
        let mut depth = 0usize;
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            if b == b'"' || b == b'\'' || b == b'`' {
                self.skip_quoted(b);
                continue;
            }
            if b == b'/' && self.pos + 1 < self.bytes.len() && (self.bytes[self.pos + 1] == b'/' || self.bytes[self.pos + 1] == b'*') {
                break;
            }
            match b {
                b'{' | b'(' | b'[' | b'<' => {
                    depth += 1;
                    self.pos += 1;
                }
                b'}' => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                    self.pos += 1;
                }
                b')' | b']' | b'>' => {
                    if b == b'>' && self.pos > 0 && self.bytes[self.pos - 1] == b'=' {
                        self.pos += 1;
                    } else {
                        depth = depth.saturating_sub(1);
                        self.pos += 1;
                    }
                }
                b';' | b',' | b'\n' if depth == 0 => break,
                _ => self.pos += 1,
            }
        }
        let raw = self.src[start..self.pos].trim();
        if raw.len() > 256 {
            "any".to_string()
        } else {
            raw.to_string()
        }
    }
}

fn parse_alias_rhs(rhs: &str, ns_stack: &[String], mut make_object: impl FnMut(&str) -> String) -> AliasDef {
    let mut types = Vec::new();
    let mut templates = Vec::new();
    let mut literals = Vec::new();
    let mut object_type = None;

    for part in split_top_level(rhs, b'|') {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        if let Some(lit) = extract_single_string_literal(p) {
            literals.push(lit);
            continue;
        }
        if p.starts_with('`') && p.ends_with('`') && p.len() >= 5 {
            let inner = &p[1..p.len() - 1];
            if let Some(dollar) = inner.find("${")
                && inner.ends_with('}')
            {
                let prefix = inner[..dollar].to_string();
                let target = inner[dollar + 2..inner.len() - 1].trim().to_string();
                if target != "string" && !target.is_empty() {
                    templates.push((prefix, target));
                }
                continue;
            }
        }
        if p.starts_with('{') && p.ends_with('}') && object_type.is_none() && p.len() < 4096 {
            object_type = Some(make_object(p));
            continue;
        }
        let cleaned = p.trim_start_matches('(').trim_end_matches(')').trim_end_matches("[]").trim();
        let base = strip_generics(cleaned);
        if is_type_reference(base) {
            if !base.contains('.') && !ns_stack.is_empty() && !matches!(base, "string" | "number" | "boolean" | "any" | "void" | "object" | "null" | "undefined" | "RegExp") {
                types.push(format!("{}.{base}", ns_stack.join(".")));
            }
            types.push(base.to_string());
        }
    }

    AliasDef::Refs { types, templates, literals, object_type }
}

pub fn split_top_level(src: &str, sep: u8) -> Vec<&str> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut depth = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'"' || b == b'\'' || b == b'`' {
            let quote = b;
            i += 1;
            let mut esc = false;
            while i < bytes.len() {
                let c = bytes[i];
                i += 1;
                if esc {
                    esc = false;
                } else if c == b'\\' {
                    esc = true;
                } else if c == quote {
                    break;
                }
            }
            continue;
        }
        match b {
            b'(' | b'{' | b'[' | b'<' => depth += 1,
            b')' | b'}' | b']' | b'>' => {
                if !(b == b'>' && i > 0 && bytes[i - 1] == b'=') {
                    depth = depth.saturating_sub(1);
                }
            }
            c if c == sep && depth == 0 => {
                out.push(&src[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if start <= src.len() {
        out.push(&src[start..]);
    }
    out
}

pub fn strip_generics(ty: &str) -> &str {
    let ty = ty.trim();
    match ty.find('<') {
        Some(i) => ty[..i].trim(),
        None => ty,
    }
}

fn is_type_reference(s: &str) -> bool {
    !s.is_empty()
        && is_ident_start(s.as_bytes()[0])
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b == b'.')
}

fn extract_single_string_literal(s: &str) -> Option<String> {
    let s = s.trim();
    if s.len() >= 2 {
        let b = s.as_bytes();
        if (b[0] == b'"' && b[b.len() - 1] == b'"') || (b[0] == b'\'' && b[b.len() - 1] == b'\'') {
            let inner = &s[1..s.len() - 1];
            if !inner.contains(b[0] as char) {
                return Some(inner.to_string());
            }
        }
    }
    None
}

fn clean_jsdoc(raw: &str) -> String {
    let mut lines = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim().trim_start_matches('*').trim();
        if !trimmed.is_empty() {
            lines.push(trimmed);
        }
    }
    lines.join("\n")
}

fn unescape_str(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '\\'
            && let Some(next) = chars.next()
        {
            match next {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                other => out.push(other),
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub fn format_params(params: &[Param]) -> String {
    params
        .iter()
        .map(|p| {
            let rest = if p.rest { "..." } else { "" };
            let opt = if p.optional { "?" } else { "" };
            format!("{rest}{}{opt}: {}", p.name, super::modules::strip_module_prefixes(&p.ty))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn is_noisy_internal_member(name: &str) -> bool {
    SKIP_INTERNAL_MEMBERS.contains(&name)
        || name.contains("$mixin")
        || name.starts_with("wrapOperation$")
        || name.starts_with("handler$")
        || name.starts_with("redirect$")
        || name.starts_with("modifyArg$")
        || name.starts_with("modifyVariable$")
        || name.starts_with("localvar$")
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b'$'
}

fn is_ident_cont(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}
