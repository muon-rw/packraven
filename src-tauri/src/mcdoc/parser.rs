#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};

use super::types::{
    AccessorKey, Attribute, AttributeTree, AttributeValue, DynamicKeyword, EnumField, EnumKind, EnumValue, Index, LiteralValue, LongRange, McdocType,
    NumericRange, RangeKind, StructField, StructKey, SymbolTables, TreeTag, TypeParam,
};

/// Validates `.mcdoc` syntax without binding into a symbol table.
pub fn validate_syntax(src: &str) -> Result<(), String> {
    let mut parser = Parser::new(src);
    parser.parse_module().map(|_| ())
}

/// Parses a single `.mcdoc` source file under `module_path` (e.g. `"::packraven"`) and merges
/// its lowered `mcdoc` types and `mcdoc/dispatcher` entries into `tables`.
pub fn parse_and_bind(src: &str, module_path: &str, tables: &mut SymbolTables) -> Result<(), String> {
    let mut parser = Parser::new(src);
    let items = parser.parse_module()?;
    let module_prefix = normalize_module_prefix(module_path);
    let mut aliases: HashMap<String, String> = HashMap::new();

    // Pass 1: collect `use` aliases and hoist top-level named declarations in this file.
    for item in &items {
        match item {
            TopItem::Use { path, alias } => {
                let resolved = resolve_path(path, &module_prefix, &HashMap::new());
                let local = alias
                    .clone()
                    .or_else(|| path.segments.last().cloned())
                    .unwrap_or_default();
                if !local.is_empty() {
                    aliases.insert(local, resolved);
                }
            }
            TopItem::Struct { name: Some(name), .. }
            | TopItem::Enum { name: Some(name), .. }
            | TopItem::TypeAlias { name, .. } => {
                aliases.insert(name.clone(), format!("{module_prefix}::{name}"));
            }
            TopItem::Dispatch { target, .. } => {
                if let AstTypeKind::Struct { name: Some(name), .. } | AstTypeKind::Enum { name: Some(name), .. } = &target.kind {
                    aliases.insert(name.clone(), format!("{module_prefix}::{name}"));
                }
            }
            _ => {}
        }
    }

    // Pass 2: lower every top-level declaration and dispatch statement into `SymbolTables`.
    let mut binder = Binder { module_prefix: &module_prefix, aliases: &aliases, tables };
    for item in items {
        binder.bind_top(item);
    }
    Ok(())
}

fn normalize_module_prefix(module_path: &str) -> String {
    let trimmed = module_path.trim().trim_end_matches("::");
    if trimmed.is_empty() || trimmed == "::" {
        String::new()
    } else if trimmed.starts_with("::") {
        trimmed.to_string()
    } else {
        format!("::{trimmed}")
    }
}

#[derive(Debug, Clone)]
struct AstPath {
    absolute: bool,
    segments: Vec<String>,
}

#[derive(Debug, Clone)]
enum TopItem {
    Use {
        path: AstPath,
        alias: Option<String>,
    },
    Struct {
        attrs: Vec<AstAttr>,
        name: Option<String>,
        fields: Vec<AstStructField>,
    },
    Enum {
        attrs: Vec<AstAttr>,
        kind: EnumKind,
        name: Option<String>,
        fields: Vec<AstEnumField>,
    },
    TypeAlias {
        attrs: Vec<AstAttr>,
        name: String,
        type_params: Vec<String>,
        target: AstType,
    },
    Dispatch {
        attrs: Vec<AstAttr>,
        registry: String,
        indices: Vec<AstIndex>,
        type_params: Vec<String>,
        target: AstType,
    },
}

#[derive(Debug, Clone)]
struct AstAttr {
    name: String,
    value: Option<AstAttrValue>,
}

#[derive(Debug, Clone)]
enum AstAttrValue {
    Tree(Vec<(Option<String>, AstAttrValue)>),
    Type(AstType),
}

#[derive(Debug, Clone)]
enum AstStructField {
    Pair {
        desc: Option<String>,
        attrs: Vec<AstAttr>,
        key: AstStructKey,
        optional: bool,
        ty: AstType,
    },
    Spread {
        attrs: Vec<AstAttr>,
        ty: AstType,
    },
}

#[derive(Debug, Clone)]
enum AstStructKey {
    Literal(String),
    Dynamic(AstType),
}

#[derive(Debug, Clone)]
struct AstEnumField {
    desc: Option<String>,
    attrs: Vec<AstAttr>,
    ident: String,
    value: EnumValue,
}

#[derive(Debug, Clone)]
enum AstIndex {
    Static(String),
    Dynamic(Vec<AccessorKey>),
}

#[derive(Debug, Clone)]
struct AstType {
    attrs: Vec<AstAttr>,
    kind: AstTypeKind,
    suffixes: Vec<AstTypeSuffix>,
}

#[derive(Debug, Clone)]
enum AstTypeSuffix {
    TypeArgs(Vec<AstType>),
    Index(Vec<AstIndex>),
}

#[derive(Debug, Clone)]
enum AstTypeKind {
    Any,
    Boolean,
    String(Option<NumericRange>),
    Byte(Option<NumericRange>),
    Short(Option<NumericRange>),
    Int(Option<NumericRange>),
    Long(Option<LongRange>),
    Float(Option<NumericRange>),
    Double(Option<NumericRange>),
    ByteArray { value: Option<NumericRange>, length: Option<NumericRange> },
    IntArray { value: Option<NumericRange>, length: Option<NumericRange> },
    LongArray { value: Option<LongRange>, length: Option<NumericRange> },
    Literal(LiteralValue),
    List { item: Box<AstType>, length: Option<NumericRange> },
    Tuple(Vec<AstType>),
    Union(Vec<AstType>),
    Struct { name: Option<String>, fields: Vec<AstStructField> },
    Enum { name: Option<String>, kind: EnumKind, fields: Vec<AstEnumField> },
    Dispatcher { registry: String, indices: Vec<AstIndex> },
    Reference(AstPath),
}

struct Binder<'a> {
    module_prefix: &'a str,
    aliases: &'a HashMap<String, String>,
    tables: &'a mut SymbolTables,
}

impl<'a> Binder<'a> {
    fn bind_top(&mut self, item: TopItem) {
        match item {
            TopItem::Use { .. } => {}
            TopItem::Struct { attrs, name: Some(name), fields } => {
                let path = format!("{}::{name}", self.module_prefix);
                let ty = self.lower_struct(attrs, fields, &HashMap::new());
                self.tables.mcdoc.insert(path, ty);
            }
            TopItem::Enum { attrs, kind, name: Some(name), fields } => {
                let path = format!("{}::{name}", self.module_prefix);
                let ty = self.lower_enum(attrs, kind, fields, &HashMap::new());
                self.tables.mcdoc.insert(path, ty);
            }
            TopItem::TypeAlias { attrs, name, type_params, target } => {
                let path = format!("{}::{name}", self.module_prefix);
                let ty = self.lower_with_params(&path, attrs, type_params, target);
                self.tables.mcdoc.insert(path, ty);
            }
            TopItem::Dispatch { attrs, registry, indices, type_params, target } => {
                let inline_name = match &target.kind {
                    AstTypeKind::Struct { name: Some(name), .. } | AstTypeKind::Enum { name: Some(name), .. } => Some(name.clone()),
                    _ => None,
                };
                let full_registry = lengthen_res_loc(&registry);
                let base_path = inline_name
                    .as_ref()
                    .map(|n| format!("{}::{n}", self.module_prefix))
                    .unwrap_or_else(|| format!("{}::<dispatch:{full_registry}>", self.module_prefix));
                let ty = self.lower_with_params(&base_path, attrs, type_params, target);
                if let Some(name) = inline_name {
                    self.tables.mcdoc.insert(format!("{}::{name}", self.module_prefix), ty.clone());
                }
                let map = self.tables.dispatchers.entry(full_registry).or_default();
                for idx in indices {
                    if let AstIndex::Static(key) = idx {
                        let key = key.strip_prefix("minecraft:").unwrap_or(&key).to_string();
                        map.insert(key, ty.clone());
                    }
                }
            }
            _ => {}
        }
    }

    fn lower_with_params(&mut self, owner_path: &str, attrs: Vec<AstAttr>, type_params: Vec<String>, target: AstType) -> McdocType {
        let mut param_map = HashMap::new();
        let params: Vec<TypeParam> = type_params
            .into_iter()
            .map(|p| {
                let full = format!("{owner_path}::{p}");
                param_map.insert(p, full.clone());
                TypeParam { path: full }
            })
            .collect();
        let mut lowered = self.lower_type(target, &param_map);
        let extra_attrs = self.lower_attrs(attrs, &param_map);
        if !extra_attrs.is_empty() {
            let mut combined = extra_attrs;
            combined.extend(lowered.attributes().iter().cloned());
            *lowered.attributes_mut() = combined;
        }
        if params.is_empty() {
            lowered
        } else {
            McdocType::Template {
                child: Box::new(lowered),
                type_params: params,
                attributes: Vec::new(),
            }
        }
    }

    fn lower_attrs(&mut self, attrs: Vec<AstAttr>, params: &HashMap<String, String>) -> Vec<Attribute> {
        attrs
            .into_iter()
            .map(|a| Attribute {
                name: a.name,
                value: a.value.map(|v| self.lower_attr_val(v, params)),
            })
            .collect()
    }

    fn lower_attr_val(&mut self, val: AstAttrValue, params: &HashMap<String, String>) -> AttributeValue {
        match val {
            AstAttrValue::Type(t) => AttributeValue::Type(self.lower_type(t, params)),
            AstAttrValue::Tree(entries) => {
                let mut values = BTreeMap::new();
                let mut pos = 0usize;
                for (name, v) in entries {
                    let key = name.unwrap_or_else(|| {
                        let k = pos.to_string();
                        pos += 1;
                        k
                    });
                    values.insert(key, self.lower_attr_val(v, params));
                }
                AttributeValue::Tree(AttributeTree { kind: TreeTag, values })
            }
        }
    }

    fn lower_struct(&mut self, attrs: Vec<AstAttr>, fields: Vec<AstStructField>, params: &HashMap<String, String>) -> McdocType {
        let attributes = self.lower_attrs(attrs, params);
        let lowered_fields = fields
            .into_iter()
            .map(|f| match f {
                AstStructField::Pair { desc, attrs, key, optional, ty } => StructField::Pair {
                    key: match key {
                        AstStructKey::Literal(s) => StructKey::Literal(s),
                        AstStructKey::Dynamic(t) => StructKey::Dynamic(self.lower_type(t, params)),
                    },
                    field_type: self.lower_type(ty, params),
                    optional,
                    deprecated: false,
                    desc,
                    attributes: self.lower_attrs(attrs, params),
                },
                AstStructField::Spread { attrs, ty } => StructField::Spread {
                    field_type: self.lower_type(ty, params),
                    attributes: self.lower_attrs(attrs, params),
                },
            })
            .collect();
        McdocType::Struct { fields: lowered_fields, attributes }
    }

    fn lower_enum(&mut self, attrs: Vec<AstAttr>, kind: EnumKind, fields: Vec<AstEnumField>, params: &HashMap<String, String>) -> McdocType {
        let attributes = self.lower_attrs(attrs, params);
        let values = fields
            .into_iter()
            .map(|f| EnumField {
                identifier: f.ident,
                value: f.value,
                desc: f.desc,
                attributes: self.lower_attrs(f.attrs, params),
            })
            .collect();
        McdocType::Enum { enum_kind: Some(kind), values, attributes }
    }

    fn lower_type(&mut self, ty: AstType, params: &HashMap<String, String>) -> McdocType {
        let attributes = self.lower_attrs(ty.attrs, params);
        let mut base = match ty.kind {
            AstTypeKind::Any => McdocType::Any { attributes },
            AstTypeKind::Boolean => McdocType::Boolean { attributes },
            AstTypeKind::String(length_range) => McdocType::String { length_range, attributes },
            AstTypeKind::Byte(value_range) => McdocType::Byte { value_range, attributes },
            AstTypeKind::Short(value_range) => McdocType::Short { value_range, attributes },
            AstTypeKind::Int(value_range) => McdocType::Int { value_range, attributes },
            AstTypeKind::Long(value_range) => McdocType::Long { value_range, attributes },
            AstTypeKind::Float(value_range) => McdocType::Float { value_range, attributes },
            AstTypeKind::Double(value_range) => McdocType::Double { value_range, attributes },
            AstTypeKind::ByteArray { value, length } => McdocType::ByteArray { value_range: value, length_range: length, attributes },
            AstTypeKind::IntArray { value, length } => McdocType::IntArray { value_range: value, length_range: length, attributes },
            AstTypeKind::LongArray { value, length } => McdocType::LongArray { value_range: value, length_range: length, attributes },
            AstTypeKind::Literal(value) => McdocType::Literal { value, attributes },
            AstTypeKind::List { item, length } => McdocType::List {
                item: Box::new(self.lower_type(*item, params)),
                length_range: length,
                attributes,
            },
            AstTypeKind::Tuple(items) => McdocType::Tuple {
                items: items.into_iter().map(|t| self.lower_type(t, params)).collect(),
                attributes,
            },
            AstTypeKind::Union(members) => McdocType::Union {
                members: members.into_iter().map(|t| self.lower_type(t, params)).collect(),
                attributes,
            },
            AstTypeKind::Struct { fields, .. } => {
                let mut s = self.lower_struct(Vec::new(), fields, params);
                *s.attributes_mut() = attributes;
                s
            }
            AstTypeKind::Enum { kind, fields, .. } => {
                let mut e = self.lower_enum(Vec::new(), kind, fields, params);
                *e.attributes_mut() = attributes;
                e
            }
            AstTypeKind::Dispatcher { registry, indices } => McdocType::Dispatcher {
                registry: lengthen_res_loc(&registry),
                parallel_indices: lower_indices(indices),
                attributes,
            },
            AstTypeKind::Reference(path) => {
                let resolved = if !path.absolute && path.segments.len() == 1 {
                    params
                        .get(&path.segments[0])
                        .cloned()
                        .unwrap_or_else(|| resolve_path(&path, self.module_prefix, self.aliases))
                } else {
                    resolve_path(&path, self.module_prefix, self.aliases)
                };
                McdocType::Reference { path: Some(resolved), attributes }
            }
        };

        for suffix in ty.suffixes {
            base = match suffix {
                AstTypeSuffix::TypeArgs(args) => McdocType::Concrete {
                    child: Box::new(base),
                    type_args: args.into_iter().map(|a| self.lower_type(a, params)).collect(),
                    attributes: Vec::new(),
                },
                AstTypeSuffix::Index(indices) => McdocType::Indexed {
                    parallel_indices: lower_indices(indices),
                    child: Box::new(base),
                    attributes: Vec::new(),
                },
            };
        }
        base
    }
}

fn lower_indices(indices: Vec<AstIndex>) -> Vec<Index> {
    indices
        .into_iter()
        .map(|i| match i {
            AstIndex::Static(value) => Index::Static { value },
            AstIndex::Dynamic(accessor) => Index::Dynamic { accessor },
        })
        .collect()
}

fn lengthen_res_loc(s: &str) -> String {
    if s.contains(':') {
        s.to_string()
    } else {
        format!("minecraft:{s}")
    }
}

fn resolve_path(path: &AstPath, module_prefix: &str, aliases: &HashMap<String, String>) -> String {
    if path.absolute {
        return format!("::{}", path.segments.join("::"));
    }
    if let Some(first) = path.segments.first() {
        if first == "super" {
            let mut base_parts: Vec<&str> = module_prefix.split("::").filter(|s| !s.is_empty()).collect();
            let mut idx = 0;
            while idx < path.segments.len() && path.segments[idx] == "super" {
                base_parts.pop();
                idx += 1;
            }
            for seg in &path.segments[idx..] {
                base_parts.push(seg);
            }
            return format!("::{}", base_parts.join("::"));
        }
        if let Some(target) = aliases.get(first) {
            if path.segments.len() == 1 {
                return target.clone();
            }
            return format!("{target}::{}", path.segments[1..].join("::"));
        }
    }
    if module_prefix.is_empty() {
        format!("::{}", path.segments.join("::"))
    } else {
        format!("{module_prefix}::{}", path.segments.join("::"))
    }
}

struct Parser<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Self {
        Self { src, bytes: src.as_bytes(), pos: 0 }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn starts_with(&self, s: &str) -> bool {
        self.src[self.pos..].starts_with(s)
    }

    fn skip_ws_and_comments(&mut self) {
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            if b.is_ascii_whitespace() {
                self.pos += 1;
                continue;
            }
            if self.starts_with("//") && !self.starts_with("///") {
                while self.pos < self.bytes.len() && self.bytes[self.pos] != b'\n' {
                    self.pos += 1;
                }
                continue;
            }
            break;
        }
    }

    fn skip_all_ws_and_comments(&mut self) {
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            if b.is_ascii_whitespace() {
                self.pos += 1;
                continue;
            }
            if self.starts_with("//") {
                while self.pos < self.bytes.len() && self.bytes[self.pos] != b'\n' {
                    self.pos += 1;
                }
                continue;
            }
            break;
        }
    }

    fn parse_doc_comments(&mut self) -> Option<String> {
        let mut lines = Vec::new();
        loop {
            self.skip_ws_and_comments();
            if !self.starts_with("///") {
                break;
            }
            self.pos += 3;
            let start = self.pos;
            while self.pos < self.bytes.len() && self.bytes[self.pos] != b'\n' {
                self.pos += 1;
            }
            let line = self.src[start..self.pos].trim_end_matches('\r');
            lines.push(line.strip_prefix(' ').unwrap_or(line));
            if self.peek() == Some(b'\n') {
                self.pos += 1;
            }
        }
        if lines.is_empty() { None } else { Some(lines.join("\n")) }
    }

    fn eat(&mut self, s: &str) -> bool {
        self.skip_all_ws_and_comments();
        if self.starts_with(s) {
            self.pos += s.len();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, s: &str) -> Result<(), String> {
        if self.eat(s) {
            Ok(())
        } else {
            Err(format!("expected `{s}` at byte {}", self.pos))
        }
    }

    fn eat_keyword(&mut self, kw: &str) -> bool {
        self.skip_all_ws_and_comments();
        if self.starts_with(kw) {
            let after = self.pos + kw.len();
            let boundary = after >= self.bytes.len() || !is_ident_cont(self.src[after..].chars().next().unwrap());
            if boundary {
                self.pos = after;
                return true;
            }
        }
        false
    }

    fn parse_ident(&mut self) -> Result<String, String> {
        self.skip_all_ws_and_comments();
        let start = self.pos;
        if let Some(ch) = self.src[self.pos..].chars().next()
            && is_ident_start(ch)
        {
            self.pos += ch.len_utf8();
            while let Some(c) = self.src[self.pos..].chars().next() {
                if is_ident_cont(c) {
                    self.pos += c.len_utf8();
                } else {
                    break;
                }
            }
            Ok(self.src[start..self.pos].to_string())
        } else {
            Err(format!("expected identifier at byte {}", self.pos))
        }
    }

    fn parse_string(&mut self) -> Result<String, String> {
        self.skip_all_ws_and_comments();
        if self.peek() != Some(b'"') {
            return Err(format!("expected string at byte {}", self.pos));
        }
        self.pos += 1;
        let mut out = String::new();
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            self.pos += 1;
            match b {
                b'"' => return Ok(out),
                b'\\' => {
                    if self.pos >= self.bytes.len() {
                        break;
                    }
                    let esc = self.bytes[self.pos];
                    self.pos += 1;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'b' => out.push('\x08'),
                        b'f' => out.push('\x0C'),
                        other => out.push(other as char),
                    }
                }
                _ => {
                    let ch = self.src[self.pos - 1..].chars().next().unwrap();
                    self.pos += ch.len_utf8() - 1;
                    out.push(ch);
                }
            }
        }
        Err(format!("unclosed string at byte {}", self.pos))
    }

    fn parse_res_loc(&mut self) -> Result<String, String> {
        self.skip_all_ws_and_comments();
        let start = self.pos;
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'/' | b':') {
                // Stop before `::` (mcdoc path separator)
                if b == b':' && self.bytes.get(self.pos + 1) == Some(&b':') {
                    break;
                }
                self.pos += 1;
            } else {
                break;
            }
        }
        let s = &self.src[start..self.pos];
        if s.contains(':') {
            Ok(s.to_string())
        } else {
            self.pos = start;
            Err(format!("expected resource location at byte {}", self.pos))
        }
    }

    fn parse_path(&mut self) -> Result<AstPath, String> {
        self.skip_all_ws_and_comments();
        let absolute = if self.starts_with("::") {
            self.pos += 2;
            true
        } else {
            false
        };
        let mut segments = vec![self.parse_ident()?];
        while self.starts_with("::") {
            self.pos += 2;
            segments.push(self.parse_ident()?);
        }
        Ok(AstPath { absolute, segments })
    }

    fn parse_attributes(&mut self) -> Result<Vec<AstAttr>, String> {
        let mut attrs = Vec::new();
        loop {
            self.skip_all_ws_and_comments();
            if !self.starts_with("#[") {
                break;
            }
            self.pos += 2;
            let name = self.parse_ident()?;
            self.skip_all_ws_and_comments();
            let value = if self.eat("=") {
                Some(self.parse_attr_value()?)
            } else if matches!(self.peek(), Some(b'(' | b'[' | b'{')) {
                Some(self.parse_attr_tree()?)
            } else {
                None
            };
            self.expect("]")?;
            attrs.push(AstAttr { name, value });
        }
        Ok(attrs)
    }

    fn parse_attr_value(&mut self) -> Result<AstAttrValue, String> {
        self.skip_all_ws_and_comments();
        if matches!(self.peek(), Some(b'(' | b'[' | b'{')) {
            self.parse_attr_tree()
        } else {
            Ok(AstAttrValue::Type(self.parse_type()?))
        }
    }

    fn parse_attr_tree(&mut self) -> Result<AstAttrValue, String> {
        self.skip_all_ws_and_comments();
        let close = match self.peek() {
            Some(b'(') => ")",
            Some(b'[') => "]",
            Some(b'{') => "}",
            _ => return Err(format!("expected attribute tree delimiter at byte {}", self.pos)),
        };
        self.pos += 1;
        let mut entries = Vec::new();
        loop {
            self.skip_all_ws_and_comments();
            if self.eat(close) {
                break;
            }
            // Check for named entry: `(ident | string) (= value | tree)`
            let save = self.pos;
            let named = if self.peek() == Some(b'"') {
                self.parse_string().ok()
            } else {
                self.parse_ident().ok()
            };
            self.skip_all_ws_and_comments();
            if let Some(key) = named
                && (self.starts_with("=") || matches!(self.peek(), Some(b'(' | b'[' | b'{')))
            {
                let val = if self.eat("=") {
                    self.parse_attr_value()?
                } else {
                    self.parse_attr_tree()?
                };
                entries.push((Some(key), val));
            } else {
                self.pos = save;
                let val = self.parse_attr_value()?;
                entries.push((None, val));
            }
            if !self.eat(",") {
                self.expect(close)?;
                break;
            }
        }
        Ok(AstAttrValue::Tree(entries))
    }

    fn parse_module(&mut self) -> Result<Vec<TopItem>, String> {
        let mut items = Vec::new();
        loop {
            let _doc = self.parse_doc_comments();
            self.skip_all_ws_and_comments();
            if self.pos >= self.bytes.len() {
                break;
            }
            let attrs = self.parse_attributes()?;
            self.skip_all_ws_and_comments();
            if self.pos >= self.bytes.len() {
                break;
            }
            if self.eat_keyword("use") {
                let path = self.parse_path()?;
                let alias = if self.eat_keyword("as") { Some(self.parse_ident()?) } else { None };
                items.push(TopItem::Use { path, alias });
            } else if self.eat_keyword("dispatch") {
                let registry = self.parse_res_loc()?;
                let indices = self.parse_index_body()?;
                let type_params = self.parse_optional_type_params()?;
                self.expect("to")?;
                let target = self.parse_type()?;
                items.push(TopItem::Dispatch { attrs, registry, indices, type_params, target });
            } else if self.eat_keyword("type") {
                let name = self.parse_ident()?;
                let type_params = self.parse_optional_type_params()?;
                self.expect("=")?;
                let target = self.parse_type()?;
                items.push(TopItem::TypeAlias { attrs, name, type_params, target });
            } else if self.eat_keyword("struct") {
                let name = if self.peek_non_ws() != Some(b'{') { Some(self.parse_ident()?) } else { None };
                let fields = self.parse_struct_block()?;
                items.push(TopItem::Struct { attrs, name, fields });
            } else if self.eat_keyword("enum") {
                let (kind, name, fields) = self.parse_enum_after_keyword()?;
                items.push(TopItem::Enum { attrs, kind, name, fields });
            } else if self.eat_keyword("inject") {
                // Spyglass itself leaves `inject` unimplemented; skip the balanced block safely.
                self.skip_inject()?;
            } else {
                return Err(format!("unexpected token at byte {}: {:?}", self.pos, &self.src[self.pos..(self.pos + 24).min(self.src.len())]));
            }
        }
        Ok(items)
    }

    fn skip_inject(&mut self) -> Result<(), String> {
        while self.pos < self.bytes.len() && self.bytes[self.pos] != b'{' {
            self.pos += 1;
        }
        if self.peek() == Some(b'{') {
            self.pos += 1;
            let mut depth = 1usize;
            while self.pos < self.bytes.len() && depth > 0 {
                match self.bytes[self.pos] {
                    b'"' => {
                        let _ = self.parse_string();
                    }
                    b'{' => {
                        depth += 1;
                        self.pos += 1;
                    }
                    b'}' => {
                        depth -= 1;
                        self.pos += 1;
                    }
                    _ => self.pos += 1,
                }
            }
        }
        Ok(())
    }

    fn peek_non_ws(&mut self) -> Option<u8> {
        self.skip_all_ws_and_comments();
        self.peek()
    }

    fn parse_optional_type_params(&mut self) -> Result<Vec<String>, String> {
        if !self.eat("<") {
            return Ok(Vec::new());
        }
        let mut params = Vec::new();
        loop {
            if self.eat(">") {
                break;
            }
            params.push(self.parse_ident()?);
            if !self.eat(",") {
                self.expect(">")?;
                break;
            }
        }
        Ok(params)
    }

    fn parse_index_body(&mut self) -> Result<Vec<AstIndex>, String> {
        self.expect("[")?;
        let mut indices = Vec::new();
        loop {
            self.skip_all_ws_and_comments();
            if self.eat("]") {
                break;
            }
            let idx = if self.peek() == Some(b'[') {
                self.pos += 1;
                let mut acc = Vec::new();
                loop {
                    acc.push(self.parse_accessor_key()?);
                    if !self.eat(".") {
                        self.expect("]")?;
                        break;
                    }
                }
                AstIndex::Dynamic(acc)
            } else if self.peek() == Some(b'"') {
                AstIndex::Static(self.parse_string()?)
            } else if self.peek() == Some(b'%') {
                let start = self.pos;
                self.pos += 1;
                while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_alphabetic() {
                    self.pos += 1;
                }
                AstIndex::Static(self.src[start..self.pos].to_string())
            } else if let Ok(rl) = self.parse_res_loc() {
                AstIndex::Static(rl)
            } else {
                AstIndex::Static(self.parse_ident()?)
            };
            indices.push(idx);
            if !self.eat(",") {
                self.expect("]")?;
                break;
            }
        }
        Ok(indices)
    }

    fn parse_accessor_key(&mut self) -> Result<AccessorKey, String> {
        self.skip_all_ws_and_comments();
        if self.starts_with("%key") {
            self.pos += 4;
            Ok(AccessorKey::Keyword { keyword: DynamicKeyword::Key })
        } else if self.starts_with("%parent") {
            self.pos += 7;
            Ok(AccessorKey::Keyword { keyword: DynamicKeyword::Parent })
        } else if self.peek() == Some(b'"') {
            Ok(AccessorKey::Field(self.parse_string()?))
        } else {
            Ok(AccessorKey::Field(self.parse_ident()?))
        }
    }

    fn parse_struct_block(&mut self) -> Result<Vec<AstStructField>, String> {
        self.expect("{")?;
        let mut fields = Vec::new();
        loop {
            let desc = self.parse_doc_comments();
            let attrs = self.parse_attributes()?;
            self.skip_all_ws_and_comments();
            if self.eat("}") {
                break;
            }
            if self.eat("...") {
                let ty = self.parse_type()?;
                fields.push(AstStructField::Spread { attrs, ty });
            } else {
                let key = if self.peek() == Some(b'"') {
                    AstStructKey::Literal(self.parse_string()?)
                } else if self.eat("[") {
                    let key_ty = self.parse_type()?;
                    self.expect("]")?;
                    AstStructKey::Dynamic(key_ty)
                } else {
                    AstStructKey::Literal(self.parse_ident()?)
                };
                let optional = self.eat("?");
                self.expect(":")?;
                let ty = self.parse_type()?;
                fields.push(AstStructField::Pair { desc, attrs, key, optional, ty });
            }
            if !self.eat(",") {
                self.expect("}")?;
                break;
            }
        }
        Ok(fields)
    }

    fn parse_enum_after_keyword(&mut self) -> Result<(EnumKind, Option<String>, Vec<AstEnumField>), String> {
        self.expect("(")?;
        let kind_str = self.parse_ident()?;
        let kind = match kind_str.as_str() {
            "byte" => EnumKind::Byte,
            "short" => EnumKind::Short,
            "int" => EnumKind::Int,
            "long" => EnumKind::Long,
            "float" => EnumKind::Float,
            "double" => EnumKind::Double,
            "string" => EnumKind::String,
            other => return Err(format!("unknown enum kind `{other}`")),
        };
        self.expect(")")?;
        let name = if self.peek_non_ws() != Some(b'{') { Some(self.parse_ident()?) } else { None };
        self.expect("{")?;
        let mut fields = Vec::new();
        loop {
            let desc = self.parse_doc_comments();
            let attrs = self.parse_attributes()?;
            self.skip_all_ws_and_comments();
            if self.eat("}") {
                break;
            }
            let ident = self.parse_ident()?;
            self.expect("=")?;
            self.skip_all_ws_and_comments();
            let value = if self.peek() == Some(b'"') {
                EnumValue::String(self.parse_string()?)
            } else {
                match self.parse_typed_number()? {
                    LiteralValue::Long { value } => EnumValue::Int(value),
                    LiteralValue::Byte { value } | LiteralValue::Short { value } | LiteralValue::Int { value } => EnumValue::Int(value as i64),
                    LiteralValue::Float { value } | LiteralValue::Double { value } => EnumValue::Float(value),
                    _ => unreachable!(),
                }
            };
            fields.push(AstEnumField { desc, attrs, ident, value });
            if !self.eat(",") {
                self.expect("}")?;
                break;
            }
        }
        Ok((kind, name, fields))
    }

    fn parse_type(&mut self) -> Result<AstType, String> {
        let attrs = self.parse_attributes()?;
        self.skip_all_ws_and_comments();

        let kind = if self.eat("(") {
            let mut members = Vec::new();
            loop {
                self.skip_all_ws_and_comments();
                if self.eat(")") {
                    break;
                }
                members.push(self.parse_type()?);
                if !self.eat("|") {
                    self.expect(")")?;
                    break;
                }
            }
            AstTypeKind::Union(members)
        } else if self.eat("[") {
            let first = self.parse_type()?;
            if self.eat(",") {
                let mut items = vec![first];
                loop {
                    self.skip_all_ws_and_comments();
                    if self.eat("]") {
                        break;
                    }
                    items.push(self.parse_type()?);
                    if !self.eat(",") {
                        self.expect("]")?;
                        break;
                    }
                }
                AstTypeKind::Tuple(items)
            } else {
                self.expect("]")?;
                let length = self.parse_optional_numeric_range()?;
                AstTypeKind::List { item: Box::new(first), length }
            }
        } else if self.peek() == Some(b'"') {
            AstTypeKind::Literal(LiteralValue::String { value: self.parse_string()? })
        } else if matches!(self.peek(), Some(b'-' | b'+' | b'0'..=b'9')) {
            AstTypeKind::Literal(self.parse_typed_number()?)
        } else if self.eat_keyword("any") {
            AstTypeKind::Any
        } else if self.eat_keyword("boolean") {
            AstTypeKind::Boolean
        } else if self.eat_keyword("true") {
            AstTypeKind::Literal(LiteralValue::Boolean { value: true })
        } else if self.eat_keyword("false") {
            AstTypeKind::Literal(LiteralValue::Boolean { value: false })
        } else if self.eat_keyword("string") {
            AstTypeKind::String(self.parse_optional_numeric_range()?)
        } else if self.eat_keyword("byte") {
            let r = self.parse_optional_numeric_range()?;
            if self.eat("[]") {
                AstTypeKind::ByteArray { value: r, length: self.parse_optional_numeric_range()? }
            } else {
                AstTypeKind::Byte(r)
            }
        } else if self.eat_keyword("short") {
            AstTypeKind::Short(self.parse_optional_numeric_range()?)
        } else if self.eat_keyword("int") {
            let r = self.parse_optional_numeric_range()?;
            if self.eat("[]") {
                AstTypeKind::IntArray { value: r, length: self.parse_optional_numeric_range()? }
            } else {
                AstTypeKind::Int(r)
            }
        } else if self.eat_keyword("long") {
            let r = self.parse_optional_long_range()?;
            if self.eat("[]") {
                AstTypeKind::LongArray { value: r, length: self.parse_optional_numeric_range()? }
            } else {
                AstTypeKind::Long(r)
            }
        } else if self.eat_keyword("float") {
            AstTypeKind::Float(self.parse_optional_numeric_range()?)
        } else if self.eat_keyword("double") {
            AstTypeKind::Double(self.parse_optional_numeric_range()?)
        } else if self.eat_keyword("struct") {
            let name = if self.peek_non_ws() != Some(b'{') {
                Some(self.parse_ident()?)
            } else {
                None
            };
            AstTypeKind::Struct { name, fields: self.parse_struct_block()? }
        } else if self.eat_keyword("enum") {
            let (kind, name, fields) = self.parse_enum_after_keyword()?;
            AstTypeKind::Enum { name, kind, fields }
        } else {
            let save = self.pos;
            if let Ok(registry) = self.parse_res_loc()
                && self.peek_non_ws() == Some(b'[')
            {
                let indices = self.parse_index_body()?;
                AstTypeKind::Dispatcher { registry, indices }
            } else {
                self.pos = save;
                AstTypeKind::Reference(self.parse_path()?)
            }
        };

        let mut suffixes = Vec::new();
        loop {
            self.skip_all_ws_and_comments();
            if self.eat("<") {
                let mut args = Vec::new();
                loop {
                    self.skip_all_ws_and_comments();
                    if self.eat(">") {
                        break;
                    }
                    args.push(self.parse_type()?);
                    if !self.eat(",") {
                        self.expect(">")?;
                        break;
                    }
                }
                suffixes.push(AstTypeSuffix::TypeArgs(args));
            } else if self.peek() == Some(b'[') {
                suffixes.push(AstTypeSuffix::Index(self.parse_index_body()?));
            } else {
                break;
            }
        }

        Ok(AstType { attrs, kind, suffixes })
    }

    fn parse_typed_number(&mut self) -> Result<LiteralValue, String> {
        self.skip_all_ws_and_comments();
        let start = self.pos;
        if matches!(self.peek(), Some(b'+' | b'-')) {
            self.pos += 1;
        }
        let mut is_float = false;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        if self.peek() == Some(b'.') && self.bytes.get(self.pos + 1) != Some(&b'.') {
            is_float = true;
            self.pos += 1;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            is_float = true;
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        let num_str = &self.src[start..self.pos];
        if num_str.is_empty() || num_str == "+" || num_str == "-" {
            return Err(format!("expected number at byte {start}"));
        }
        let suffix = self.peek().filter(|b| matches!(b, b'b' | b'B' | b's' | b'S' | b'l' | b'L' | b'f' | b'F' | b'd' | b'D'));
        if suffix.is_some() {
            self.pos += 1;
        }
        match suffix.map(|b| b.to_ascii_lowercase()) {
            Some(b'b') => Ok(LiteralValue::Byte { value: num_str.parse::<f64>().map_err(|e| e.to_string())? }),
            Some(b's') => Ok(LiteralValue::Short { value: num_str.parse::<f64>().map_err(|e| e.to_string())? }),
            Some(b'l') => Ok(LiteralValue::Long { value: num_str.parse::<i64>().map_err(|e| e.to_string())? }),
            Some(b'f') => Ok(LiteralValue::Float { value: num_str.parse::<f64>().map_err(|e| e.to_string())? }),
            Some(b'd') => Ok(LiteralValue::Double { value: num_str.parse::<f64>().map_err(|e| e.to_string())? }),
            _ if is_float => Ok(LiteralValue::Double { value: num_str.parse::<f64>().map_err(|e| e.to_string())? }),
            _ => Ok(LiteralValue::Int { value: num_str.parse::<f64>().map_err(|e| e.to_string())? }),
        }
    }

    fn parse_optional_numeric_range(&mut self) -> Result<Option<NumericRange>, String> {
        if !self.eat("@") {
            return Ok(None);
        }
        self.skip_all_ws_and_comments();
        let (kind, min, max) = self.parse_raw_range()?;
        Ok(Some(NumericRange {
            kind,
            min: min.map(|s| s.parse::<f64>()).transpose().map_err(|e| e.to_string())?,
            max: max.map(|s| s.parse::<f64>()).transpose().map_err(|e| e.to_string())?,
        }))
    }

    fn parse_optional_long_range(&mut self) -> Result<Option<LongRange>, String> {
        if !self.eat("@") {
            return Ok(None);
        }
        self.skip_all_ws_and_comments();
        let (kind, min, max) = self.parse_raw_range()?;
        Ok(Some(LongRange {
            kind,
            min: min.map(|s| s.parse::<i64>()).transpose().map_err(|e| e.to_string())?,
            max: max.map(|s| s.parse::<i64>()).transpose().map_err(|e| e.to_string())?,
        }))
    }

    fn parse_raw_range(&mut self) -> Result<(RangeKind, Option<String>, Option<String>), String> {
        self.skip_all_ws_and_comments();
        if let Some(kind) = self.eat_range_delim() {
            let max = self.read_number_token()?;
            return Ok((kind, None, Some(max)));
        }
        let min = self.read_number_token()?;
        if let Some(kind) = self.eat_range_delim() {
            let max = if matches!(self.peek(), Some(b'-' | b'+' | b'0'..=b'9')) {
                Some(self.read_number_token()?)
            } else {
                None
            };
            Ok((kind, Some(min), max))
        } else {
            Ok((RangeKind::INCLUSIVE, Some(min.clone()), Some(min)))
        }
    }

    fn eat_range_delim(&mut self) -> Option<RangeKind> {
        if self.starts_with("<..<") {
            self.pos += 4;
            Some(RangeKind::EXCLUSIVE)
        } else if self.starts_with("<..") {
            self.pos += 3;
            Some(RangeKind::LEFT_EXCLUSIVE)
        } else if self.starts_with("..<") {
            self.pos += 3;
            Some(RangeKind::RIGHT_EXCLUSIVE)
        } else if self.starts_with("..") {
            self.pos += 2;
            Some(RangeKind::INCLUSIVE)
        } else {
            None
        }
    }

    fn read_number_token(&mut self) -> Result<String, String> {
        let start = self.pos;
        if matches!(self.peek(), Some(b'+' | b'-')) {
            self.pos += 1;
        }
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        if self.peek() == Some(b'.') && self.bytes.get(self.pos + 1) != Some(&b'.') {
            self.pos += 1;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        if self.pos == start {
            return Err(format!("expected number in range at byte {start}"));
        }
        Ok(self.src[start..self.pos].to_string())
    }
}

fn is_ident_start(ch: char) -> bool {
    ch.is_alphabetic() || ch == '_'
}

fn is_ident_cont(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcdoc::fetch::PACKRAVEN_MCDOC;

    #[test]
    fn parses_bundled_packraven_mcdoc() {
        let mut tables = SymbolTables::default();
        parse_and_bind(PACKRAVEN_MCDOC, "::packraven", &mut tables).unwrap();
        let conds = &tables.dispatchers["neoforge:condition_codec"];
        assert!(conds.contains_key("neoforge:true"));
        assert!(conds.contains_key("neoforge:always"));
        assert!(conds.contains_key("neoforge:mod_loaded"));
        assert!(conds.contains_key("neoforge:tag_empty"));
        assert!(conds.contains_key("neoforge:feature_flags_enabled"));
    }

    #[test]
    fn parses_modded_mcdoc_with_uses_templates_and_dispatchers() {
        let src = r#"
use ::java::data::recipe::Ingredient as VanillaIngredient

/// Custom cutting recipe.
dispatch minecraft:recipe_serializer["create:cutting"] to struct CuttingRecipe {
	/// Input ingredients.
	ingredients: [VanillaIngredient] @ 1..2,
	results: [Weighted<string>],
	#[since="1.21"]
	processing_time?: int @ 1..1000,
	...minecraft:extra[[%parent.type]],
}

type Weighted<T> = struct {
	item: T,
	chance?: float @ 0.0..1.0,
}

enum(string) HeatMode {
	None = "none",
	Heated = "heated",
}
"#;
        let mut tables = SymbolTables::default();
        parse_and_bind(src, "::create::recipe", &mut tables).unwrap();
        assert!(tables.dispatchers["minecraft:recipe_serializer"].contains_key("create:cutting"));
        assert!(tables.mcdoc.contains_key("::create::recipe::Weighted"));
        assert!(tables.mcdoc.contains_key("::create::recipe::HeatMode"));
    }
}
