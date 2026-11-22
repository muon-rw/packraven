#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use regex::Regex;

use super::json_parser::{Diagnostic, JsonKind, JsonNode, JsonString, Severity, Span};
use super::registries::{lengthen_id, normalize_category, IdLookup, RegistryIndex};
use super::types::{
    AccessorKey, Attribute, DynamicKeyword, EnumField, EnumKind, EnumValue, Index, LiteralValue, LongRange, McdocType, NumericRange, RangeKind,
    StructField, StructKey, SymbolTables,
};
use super::version::{attr_number, attr_string, parse_id_attribute, parse_integer_attribute, TagMode, VersionContext};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct JsonId(pub usize);

#[derive(Debug, Clone)]
pub enum ArenaKind {
    Object(Vec<(JsonId, Option<JsonId>)>),
    Array(Vec<JsonId>),
    String(JsonString),
    Number { raw: String, value: f64, is_int: bool, int_value: Option<i64> },
    Boolean(bool),
    Null,
}

#[derive(Debug, Clone)]
pub struct ArenaNode {
    pub span: Span,
    pub parent: Option<JsonId>,
    /// When this node is the value of an object pair, points to the pair's key node.
    pub pair_key: Option<JsonId>,
    pub kind: ArenaKind,
}

#[derive(Debug, Clone, Default)]
pub struct JsonArena {
    pub nodes: Vec<ArenaNode>,
    pub root: Option<JsonId>,
}

impl JsonArena {
    pub fn from_root(root: &JsonNode) -> Self {
        let mut arena = Self::default();
        let id = arena.push_node(root, None, None);
        arena.root = Some(id);
        arena
    }

    fn push_node(&mut self, node: &JsonNode, parent: Option<JsonId>, pair_key: Option<JsonId>) -> JsonId {
        let id = JsonId(self.nodes.len());
        self.nodes.push(ArenaNode { span: node.span, parent, pair_key, kind: ArenaKind::Null });
        let kind = match &node.kind {
            JsonKind::Object(pairs) => {
                let mut out = Vec::with_capacity(pairs.len());
                for pair in pairs {
                    let key_id = JsonId(self.nodes.len());
                    self.nodes.push(ArenaNode {
                        span: pair.key.span,
                        parent: Some(id),
                        pair_key: None,
                        kind: ArenaKind::String(pair.key.clone()),
                    });
                    let val_id = pair.value.as_ref().map(|v| self.push_node(v, Some(id), Some(key_id)));
                    out.push((key_id, val_id));
                }
                ArenaKind::Object(out)
            }
            JsonKind::Array(items) => {
                let ids = items.iter().map(|it| self.push_node(it, Some(id), None)).collect();
                ArenaKind::Array(ids)
            }
            JsonKind::String(s) => ArenaKind::String(s.clone()),
            JsonKind::Number { raw, value, is_int, int_value } => ArenaKind::Number {
                raw: raw.clone(),
                value: *value,
                is_int: *is_int,
                int_value: *int_value,
            },
            JsonKind::Boolean(b) => ArenaKind::Boolean(*b),
            JsonKind::Null => ArenaKind::Null,
        };
        self.nodes[id.0].kind = kind;
        id
    }

    pub fn get(&self, id: JsonId) -> &ArenaNode {
        &self.nodes[id.0]
    }
}

#[derive(Debug, Clone)]
pub struct AttachedInfo {
    pub type_def: McdocType,
    pub desc: Option<String>,
    pub deprecated: bool,
}

#[derive(Debug, Clone)]
pub struct CheckOutput {
    pub arena: JsonArena,
    pub diagnostics: Vec<Diagnostic>,
    pub attached: HashMap<JsonId, AttachedInfo>,
}

pub struct CheckEnv<'a> {
    pub tables: &'a SymbolTables,
    pub registries: &'a RegistryIndex,
    pub version: &'a VersionContext,
    pub require_canonical: bool,
    pub allow_missing_keys: bool,
    /// 1.20.1 reads most data files with Gson helpers that turn any JSON number or boolean into a
    /// string where one is expected. Codecs (tags, world data, and everything from 1.20.5) don't.
    pub primitives_read_as_strings: bool,
    /// Loaders whose keys the pack's loader skips, like `neoforge:conditions` in a Fabric pack. We
    /// accept those keys without checking their values.
    pub ignored_loaders: &'a [&'a str],
}

pub fn check_json_with_type(root: &JsonNode, root_type: &McdocType, env: &CheckEnv<'_>) -> CheckOutput {
    let arena = JsonArena::from_root(root);
    let root_id = arena.root.unwrap();
    // Modpacks commonly disable a mod's datapack file by overriding it with only an unconditionally-false
    // loader condition (`neoforge:false`, `neoforge:never`, `fabric:false`, or `forge:false`). Validate the
    // conditions array itself without requiring the skipped body's fields.
    let disabled_stub_type = if is_unconditionally_disabled_stub(root) {
        let mut fields = Vec::new();
        add_loader_condition_fields(&mut fields);
        Some(McdocType::Struct { fields, attributes: Vec::new() })
    } else {
        None
    };
    let effective_root = disabled_stub_type.as_ref().unwrap_or(root_type);
    let mut engine = Engine {
        arena: &arena,
        env,
        groups: Vec::new(),
        defs: Vec::new(),
        node_groups: vec![Vec::new(); arena.nodes.len()],
        diagnostics: Vec::new(),
        attached: HashMap::new(),
    };
    engine.run(root_id, effective_root);
    let diagnostics = engine.diagnostics;
    let attached = engine.attached;
    CheckOutput { arena, diagnostics, attached }
}

fn is_unconditionally_disabled_stub(root: &JsonNode) -> bool {
    let JsonKind::Object(pairs) = &root.kind else { return false };
    if pairs.len() != 1 {
        return false;
    }
    let key = pairs[0].key.value.as_str();
    let Some(JsonNode { kind: JsonKind::Array(items), .. }) = pairs[0].value.as_ref() else {
        return false;
    };
    let (field_name, false_ids): (&str, &[&str]) = match key {
        "neoforge:conditions" => ("type", &["neoforge:false", "neoforge:never"]),
        "fabric:load_conditions" => ("condition", &["fabric:false"]),
        "conditions" => ("type", &["forge:false"]),
        _ => return false,
    };
    items.iter().any(|item| {
        let JsonKind::Object(cond_pairs) = &item.kind else { return false };
        cond_pairs.iter().any(|p| {
            p.key.value == field_name
                && matches!(
                    p.value.as_ref().map(|v| &v.kind),
                    Some(JsonKind::String(s)) if false_ids.contains(&s.value.as_str())
                )
        })
    })
}

/// Builds the root `McdocType` for a datapack file category, wrapping vanilla's schema with
/// NeoForge/Fabric load conditions and NeoForge tag `remove`.
pub fn resource_root_type(category: &str, tables: &SymbolTables) -> Option<McdocType> {
    if category == "pack_mcmeta" {
        return Some(McdocType::Reference { path: Some("::java::pack::Pack".into()), attributes: Vec::new() });
    }
    if let Some(tag_reg) = category.strip_prefix("tag/") {
        let mut fields = vec![
            StructField::Spread {
                field_type: McdocType::Concrete {
                    child: Box::new(McdocType::Reference {
                        path: Some("::java::data::tag::Tag".into()),
                        attributes: Vec::new(),
                    }),
                    type_args: vec![McdocType::String {
                        length_range: None,
                        attributes: vec![Attribute {
                            name: "id".into(),
                            value: Some(super::types::AttributeValue::Tree(super::types::AttributeTree {
                                kind: super::types::TreeTag,
                                values: BTreeMap::from([
                                    (
                                        "registry".into(),
                                        super::types::AttributeValue::Type(McdocType::Literal {
                                            value: LiteralValue::String { value: tag_reg.to_string() },
                                            attributes: Vec::new(),
                                        }),
                                    ),
                                    (
                                        "tags".into(),
                                        super::types::AttributeValue::Type(McdocType::Literal {
                                            value: LiteralValue::String { value: "allowed".into() },
                                            attributes: Vec::new(),
                                        }),
                                    ),
                                ]),
                            })),
                        }],
                    }],
                    attributes: Vec::new(),
                },
                attributes: Vec::new(),
            },
            // NeoForge extends tag files with `remove`.
            StructField::Pair {
                key: StructKey::Literal("remove".into()),
                field_type: McdocType::List {
                    item: Box::new(McdocType::Any { attributes: Vec::new() }),
                    length_range: None,
                    attributes: Vec::new(),
                },
                optional: true,
                deprecated: false,
                desc: Some("Entries to remove from this tag (NeoForge).".into()),
                attributes: Vec::new(),
            },
        ];
        add_loader_condition_fields(&mut fields);
        return Some(McdocType::Struct { fields, attributes: Vec::new() });
    }

    let resources = tables.dispatchers.get("minecraft:resource")?;
    let cat_key = normalize_category(category);
    let root = resources.get(&cat_key)?;
    Some(wrap_root_with_conditions(root, tables, 0))
}

/// Injects optional loader condition fields (`neoforge:conditions`, `fabric:load_conditions`, `conditions`)
/// into each top-level struct branch of a resource schema without wrapping non-struct union members.
fn wrap_root_with_conditions(ty: &McdocType, tables: &SymbolTables, depth: u8) -> McdocType {
    if depth > 8 {
        return ty.clone();
    }
    match ty {
        McdocType::Reference { path: Some(path), attributes } => {
            if let Some(target) = tables.mcdoc.get(path) {
                let mut wrapped = wrap_root_with_conditions(target, tables, depth + 1);
                prepend_attrs(&mut wrapped, attributes);
                wrapped
            } else {
                ty.clone()
            }
        }
        McdocType::Union { members, attributes } => McdocType::Union {
            members: members.iter().map(|m| wrap_root_with_conditions(m, tables, depth + 1)).collect(),
            attributes: attributes.clone(),
        },
        McdocType::Struct { .. } => {
            // Put loader condition fields BEFORE the schema spread so schemas that define their own
            // `conditions` field (like loot tables, advancements, and Apotheosis augments) take precedence.
            let mut fields = Vec::new();
            add_loader_condition_fields(&mut fields);
            fields.push(StructField::Spread { field_type: ty.clone(), attributes: Vec::new() });
            McdocType::Struct { fields, attributes: ty.attributes().to_vec() }
        }
        other => other.clone(),
    }
}

fn add_loader_condition_fields(fields: &mut Vec<StructField>) {
    let fabric_condition = McdocType::Dispatcher {
        registry: "minecraft:resource".into(),
        parallel_indices: vec![Index::Static { value: "packraven:fabric_condition".into() }],
        attributes: Vec::new(),
    };
    fields.push(StructField::Pair {
        key: StructKey::Literal("neoforge:conditions".into()),
        field_type: McdocType::List {
            item: Box::new(McdocType::Dispatcher {
                registry: "minecraft:resource".into(),
                parallel_indices: vec![Index::Static { value: "packraven:neoforge_condition".into() }],
                attributes: Vec::new(),
            }),
            length_range: None,
            attributes: Vec::new(),
        },
        optional: true,
        deprecated: false,
        desc: Some("NeoForge load conditions for this data file.".into()),
        attributes: vec![loader_attr("neoforge")],
    });
    fields.push(StructField::Pair {
        key: StructKey::Literal("fabric:load_conditions".into()),
        // Fabric API reads only a list until its 1.20.5 codec rewrite, which also accepts a single
        // condition (`ResourceCondition.CONDITION_CODEC`).
        field_type: McdocType::Union {
            members: vec![
                McdocType::List { item: Box::new(fabric_condition.clone()), length_range: None, attributes: Vec::new() },
                McdocType::Dispatcher {
                    registry: "minecraft:resource".into(),
                    parallel_indices: vec![Index::Static { value: "packraven:fabric_condition".into() }],
                    attributes: vec![Attribute {
                        name: "since".into(),
                        value: Some(super::types::AttributeValue::Type(McdocType::Literal {
                            value: LiteralValue::String { value: "1.20.5".into() },
                            attributes: Vec::new(),
                        })),
                    }],
                },
            ],
            attributes: Vec::new(),
        },
        optional: true,
        deprecated: false,
        desc: Some("Fabric Resource Conditions for this data file: one condition, or a list of them.".into()),
        attributes: vec![loader_attr("fabric")],
    });
    fields.push(StructField::Pair {
        key: StructKey::Literal("conditions".into()),
        field_type: McdocType::List {
            item: Box::new(McdocType::Dispatcher {
                registry: "minecraft:resource".into(),
                parallel_indices: vec![Index::Static { value: "packraven:forge_condition".into() }],
                attributes: Vec::new(),
            }),
            length_range: None,
            attributes: Vec::new(),
        },
        optional: true,
        deprecated: false,
        desc: Some("Forge 1.20.1 load conditions for this data file.".into()),
        attributes: vec![loader_attr("forge"), Attribute {
            name: "until".into(),
            value: Some(super::types::AttributeValue::Type(McdocType::Literal {
                value: LiteralValue::String { value: "1.20.5".into() },
                attributes: Vec::new(),
            })),
        }],
    });
}

// ============================================================================
// Arena-based 4-tier DAG Checker + Bottom-up Error Condensing
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct GroupId(usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct DefId(usize);

struct GroupNode {
    parents: Vec<DefId>,
    runtime_node: JsonId,
    key_def: Option<McdocType>,
    original_type: McdocType,
    desc: Option<String>,
    deprecated: bool,
    valid_defs: Vec<DefId>,
    condensed_errors: Vec<Vec<RuntimeError>>,
}

struct DefNode {
    group: GroupId,
    type_def: McdocType,
    children: Vec<GroupId>,
}

#[derive(Debug, Clone)]
enum RuntimeErrorKind {
    TypeMismatch { expected: Vec<McdocType> },
    UnknownKey,
    MissingKey { keys: Vec<String> },
    UnknownTupleElement,
    InvalidCollectionLength { ranges: Vec<NumericRange> },
    InvalidStringLength { ranges: Vec<NumericRange> },
    NumberOutOfRange { ranges: Vec<AnyRange> },
    Internal,
    IntegerReadAs { read_as: i64 },
    StateNeverMatches,
}

#[derive(Debug, Clone, PartialEq)]
enum AnyRange {
    Float(NumericRange),
    Long(LongRange),
}

impl AnyRange {
    fn format(&self) -> String {
        match self {
            AnyRange::Float(r) => r.to_string(),
            AnyRange::Long(r) => r.to_string(),
        }
    }
}

#[derive(Debug, Clone)]
struct RuntimeError {
    node: JsonId,
    kind: RuntimeErrorKind,
    conflicting_nodes: Option<Vec<JsonId>>,
}

struct ChildDefSpec {
    ty: McdocType,
    key_type: Option<McdocType>,
    desc: Option<String>,
    deprecated: bool,
}

struct Engine<'a> {
    arena: &'a JsonArena,
    env: &'a CheckEnv<'a>,
    groups: Vec<GroupNode>,
    defs: Vec<DefIdNode>,
    node_groups: Vec<Vec<GroupId>>,
    diagnostics: Vec<Diagnostic>,
    attached: HashMap<JsonId, AttachedInfo>,
}

type DefIdNode = DefNode;

impl<'a> Engine<'a> {
    fn run(&mut self, root_id: JsonId, root_type: &McdocType) {
        let simplified = self.simplify(root_type, root_id, false, &[], None, 0);
        let mut root_members = flatten_union(simplified);
        if root_members.is_empty() && !matches!(root_type, McdocType::Union { members, .. } if members.is_empty()) {
            root_members.push(McdocType::Any { attributes: Vec::new() });
        }

        let root_group = self.push_group(Vec::new(), root_id, None, root_type.clone(), None, false, root_members);
        self.node_groups[root_id.0].push(root_group);

        let mut queue = VecDeque::new();
        queue.push_back(root_id);

        while let Some(node_id) = queue.pop_front() {
            let group_ids = self.node_groups[node_id.0].clone();
            let children_ids = self.child_value_nodes(node_id);

            for group_id in group_ids {
                let valid_defs = self.groups[group_id.0].valid_defs.clone();
                if valid_defs.is_empty() {
                    self.groups[group_id.0].condensed_errors.push(vec![RuntimeError {
                        node: node_id,
                        kind: RuntimeErrorKind::TypeMismatch { expected: Vec::new() },
                        conflicting_nodes: None,
                    }]);
                    self.propagate_to_parents(group_id, 0);
                    continue;
                }

                let mut def_errors: Vec<(DefId, Vec<RuntimeError>)> = Vec::with_capacity(valid_defs.len());
                for def_id in valid_defs {
                    let def_type = self.defs[def_id.0].type_def.clone();
                    let (errors, child_specs) = self.check_shallowly(node_id, &def_type);
                    def_errors.push((def_id, errors));

                    for (child_node_id, spec) in child_specs {
                        let existing = self.node_groups[child_node_id.0].iter().copied().find(|&gid| {
                            let g = &self.groups[gid.0];
                            g.key_def == spec.key_type && g.original_type == spec.ty
                        });
                        if let Some(existing_gid) = existing {
                            if !self.groups[existing_gid.0].parents.contains(&def_id) {
                                self.groups[existing_gid.0].parents.push(def_id);
                            }
                            self.defs[def_id.0].children.push(existing_gid);
                        } else {
                            let simplified_child = self.simplify(&spec.ty, child_node_id, false, &[], None, 0);
                            let members = flatten_union(simplified_child);
                            let child_gid = self.push_group(
                                vec![def_id],
                                child_node_id,
                                spec.key_type,
                                spec.ty,
                                spec.desc,
                                spec.deprecated,
                                members,
                            );
                            self.node_groups[child_node_id.0].push(child_gid);
                            self.defs[def_id.0].children.push(child_gid);
                        }
                    }
                }

                self.condense_and_propagate(group_id, def_errors);
            }

            for child_id in children_ids {
                queue.push_back(child_id);
            }
        }

        // Attach resolved type info and run leaf attribute/registry checks.
        self.attach_and_check_attributes(root_id);

        // Emit condensed diagnostics from the root group(s).
        let root_errors: Vec<RuntimeError> = self.node_groups[root_id.0]
            .iter()
            .flat_map(|&gid| self.groups[gid.0].condensed_errors.iter().flatten().cloned())
            .collect();
        for err in root_errors {
            self.report_runtime_error(&err);
        }
    }

    fn push_group(
        &mut self,
        parents: Vec<DefId>,
        runtime_node: JsonId,
        key_def: Option<McdocType>,
        original_type: McdocType,
        desc: Option<String>,
        deprecated: bool,
        members: Vec<McdocType>,
    ) -> GroupId {
        let gid = GroupId(self.groups.len());
        let mut valid_defs = Vec::with_capacity(members.len());
        for m in members {
            let did = DefId(self.defs.len());
            self.defs.push(DefNode { group: gid, type_def: m, children: Vec::new() });
            valid_defs.push(did);
        }
        self.groups.push(GroupNode {
            parents,
            runtime_node,
            key_def,
            original_type,
            desc,
            deprecated,
            valid_defs,
            condensed_errors: Vec::new(),
        });
        gid
    }

    fn child_value_nodes(&self, node_id: JsonId) -> Vec<JsonId> {
        match &self.arena.get(node_id).kind {
            ArenaKind::Object(pairs) => pairs.iter().filter_map(|(_, v)| *v).collect(),
            ArenaKind::Array(items) => items.clone(),
            _ => Vec::new(),
        }
    }

    // ------------------------------------------------------------------------
    // Shallow check
    // ------------------------------------------------------------------------

    fn check_shallowly(&self, node_id: JsonId, type_def: &McdocType) -> (Vec<RuntimeError>, Vec<(JsonId, ChildDefSpec)>) {
        let mut child_specs = Vec::new();
        if matches!(type_def, McdocType::Any { .. } | McdocType::Unsafe { .. }) {
            return (Vec::new(), child_specs);
        }

        let node = self.arena.get(node_id);
        let mut inferred = infer_json_type(node);

        let expected_kind = value_kind_of(type_def);
        let mut lossy_integer = None;
        if value_kind_of(&inferred) != expected_kind {
            if let Some(converted) = self.read_primitive_as_string(node, expected_kind) {
                inferred = converted;
            } else if let Some(converted) = try_convert_json(node, expected_kind) {
                inferred = converted;
            } else if let Some((converted, read_as)) = java_integer_conversion(node, expected_kind) {
                inferred = converted;
                lossy_integer = Some(read_as);
            } else {
                return (
                    vec![RuntimeError {
                        node: node_id,
                        kind: RuntimeErrorKind::TypeMismatch { expected: vec![type_def.clone()] },
                        conflicting_nodes: None,
                    }],
                    child_specs,
                );
            }
        }

        let mut errors = Vec::new();
        if let Some(read_as) = lossy_integer {
            errors.push(RuntimeError { node: node_id, kind: RuntimeErrorKind::IntegerReadAs { read_as }, conflicting_nodes: None });
        }
        if !check_inferred_attributes(type_def.attributes(), &mut inferred) {
            errors.push(RuntimeError { node: node_id, kind: RuntimeErrorKind::Internal, conflicting_nodes: None });
        }

        // Literal / Enum exact match check.
        match type_def {
            McdocType::Literal { value: expected_val, .. } => {
                if !matches!(&inferred, McdocType::Literal { value, .. } if literal_eq(value, expected_val)) {
                    return (
                        vec![RuntimeError {
                            node: node_id,
                            kind: RuntimeErrorKind::TypeMismatch { expected: vec![type_def.clone()] },
                            conflicting_nodes: None,
                        }],
                        child_specs,
                    );
                }
            }
            McdocType::Enum { values, attributes, .. } => {
                let matched = match &inferred {
                    McdocType::Literal { value, .. } => {
                        values.iter().any(|ef| enum_value_matches(&ef.value, value))
                            || matches!(value, LiteralValue::String { value } if self.accepts_unlisted_enum_value(values, attributes, value))
                    }
                    _ => false,
                };
                if !matched && has_attr(attributes, STATE_PREDICATE_ATTR) && matches!(&inferred, McdocType::Literal { value: LiteralValue::String { .. }, .. }) {
                    errors.push(RuntimeError { node: node_id, kind: RuntimeErrorKind::StateNeverMatches, conflicting_nodes: None });
                } else if !matched {
                    return (
                        vec![RuntimeError {
                            node: node_id,
                            kind: RuntimeErrorKind::TypeMismatch { expected: vec![type_def.clone()] },
                            conflicting_nodes: None,
                        }],
                        child_specs,
                    );
                }
            }
            _ => {}
        }

        match type_def {
            McdocType::Byte { value_range, .. }
            | McdocType::Short { value_range, .. }
            | McdocType::Int { value_range, .. }
            | McdocType::Float { value_range, .. }
            | McdocType::Double { value_range, .. } => {
                if let Some(range) = value_range
                    && let Some(num) = inferred_number(&inferred)
                    && !range.contains(num)
                {
                    errors.push(RuntimeError {
                        node: node_id,
                        kind: RuntimeErrorKind::NumberOutOfRange { ranges: vec![AnyRange::Float(*range)] },
                        conflicting_nodes: None,
                    });
                }
            }
            McdocType::Long { value_range, .. } => {
                if let Some(range) = value_range
                    && let McdocType::Literal { value: LiteralValue::Long { value }, .. } = &inferred
                    && !range.contains(*value)
                {
                    errors.push(RuntimeError {
                        node: node_id,
                        kind: RuntimeErrorKind::NumberOutOfRange { ranges: vec![AnyRange::Long(*range)] },
                        conflicting_nodes: None,
                    });
                }
            }
            McdocType::String { length_range, .. } => {
                if let Some(range) = length_range
                    && let McdocType::Literal { value: LiteralValue::String { value }, .. } = &inferred
                    && !range.contains(value.chars().count() as f64)
                {
                    errors.push(RuntimeError {
                        node: node_id,
                        kind: RuntimeErrorKind::InvalidStringLength { ranges: vec![*range] },
                        conflicting_nodes: None,
                    });
                }
            }
            McdocType::Struct { fields, .. } => {
                let ArenaKind::Object(pairs) = &node.kind else { return (errors, child_specs) };
                let mut pair_matched: Vec<Option<ChildDefSpec>> = (0..pairs.len()).map(|_| None).collect();
                let mut missing_keys = Vec::new();
                // Last literal field definition wins for effective optionality.
                let mut literal_optional: HashMap<&str, bool> = HashMap::new();
                for field in fields {
                    if let StructField::Pair { key: StructKey::Literal(k), optional, .. } = field {
                        literal_optional.insert(k.as_str(), *optional);
                    }
                }

                for field in fields {
                    let StructField::Pair { key, field_type, optional, deprecated, desc, .. } = field else {
                        continue;
                    };
                    let mut found_match = false;
                    if let StructKey::Literal(lit_key) = key {
                        for (idx, (kid, _)) in pairs.iter().enumerate() {
                            if let ArenaKind::String(ks) = &self.arena.get(*kid).kind
                                && ks.value == *lit_key
                            {
                                found_match = true;
                                pair_matched[idx] = Some(ChildDefSpec {
                                    ty: field_type.clone(),
                                    key_type: Some(McdocType::Literal {
                                        value: LiteralValue::String { value: lit_key.clone() },
                                        attributes: Vec::new(),
                                    }),
                                    desc: desc.clone(),
                                    deprecated: *deprecated,
                                });
                            }
                        }
                        let eff_optional = literal_optional.get(lit_key.as_str()).copied().unwrap_or(*optional);
                        if !found_match && !eff_optional && !self.env.allow_missing_keys && !missing_keys.contains(lit_key) {
                            missing_keys.push(lit_key.clone());
                        }
                    } else if let StructKey::Dynamic(key_ty) = key {
                        for (idx, (kid, _)) in pairs.iter().enumerate() {
                            // Specific literal field matches take priority over dynamic map keys.
                            if matches!(&pair_matched[idx], Some(spec) if matches!(spec.key_type, Some(McdocType::Literal { .. }))) {
                                continue;
                            }
                            if let ArenaKind::String(ks) = &self.arena.get(*kid).kind
                                && self.is_string_key_assignable(&ks.value, key_ty)
                            {
                                found_match = true;
                                pair_matched[idx] = Some(ChildDefSpec {
                                    ty: field_type.clone(),
                                    key_type: Some(key_ty.clone()),
                                    desc: desc.clone(),
                                    deprecated: *deprecated,
                                });
                            }
                        }
                        if !found_match
                            && !*optional
                            && !self.env.allow_missing_keys
                            && let McdocType::Literal { value: LiteralValue::String { value }, .. } = key_ty
                            && !missing_keys.contains(value)
                        {
                            missing_keys.push(value.clone());
                        }
                    }
                }

                for missing in missing_keys {
                    errors.push(RuntimeError {
                        node: node_id,
                        kind: RuntimeErrorKind::MissingKey { keys: vec![missing] },
                        conflicting_nodes: None,
                    });
                }

                for (idx, (kid, vid)) in pairs.iter().enumerate() {
                    if let Some(spec) = pair_matched[idx].take() {
                        if let Some(val_id) = vid {
                            child_specs.push((*val_id, spec));
                        }
                    } else {
                        errors.push(RuntimeError { node: *kid, kind: RuntimeErrorKind::UnknownKey, conflicting_nodes: None });
                    }
                }
            }
            McdocType::List { item, length_range, .. } => {
                let ArenaKind::Array(items) = &node.kind else { return (errors, child_specs) };
                for &it in items {
                    child_specs.push((it, ChildDefSpec { ty: (**item).clone(), key_type: None, desc: None, deprecated: false }));
                }
                if let Some(range) = length_range
                    && !range.contains(items.len() as f64)
                {
                    errors.push(RuntimeError {
                        node: node_id,
                        kind: RuntimeErrorKind::InvalidCollectionLength { ranges: vec![*range] },
                        conflicting_nodes: None,
                    });
                }
            }
            McdocType::ByteArray { value_range, length_range, .. }
            | McdocType::IntArray { value_range, length_range, .. } => {
                let ArenaKind::Array(items) = &node.kind else { return (errors, child_specs) };
                let elem = if matches!(type_def, McdocType::ByteArray { .. }) {
                    McdocType::Byte { value_range: *value_range, attributes: Vec::new() }
                } else {
                    McdocType::Int { value_range: *value_range, attributes: Vec::new() }
                };
                for &it in items {
                    child_specs.push((it, ChildDefSpec { ty: elem.clone(), key_type: None, desc: None, deprecated: false }));
                }
                if let Some(range) = length_range
                    && !range.contains(items.len() as f64)
                {
                    errors.push(RuntimeError {
                        node: node_id,
                        kind: RuntimeErrorKind::InvalidCollectionLength { ranges: vec![*range] },
                        conflicting_nodes: None,
                    });
                }
            }
            McdocType::LongArray { value_range, length_range, .. } => {
                let ArenaKind::Array(items) = &node.kind else { return (errors, child_specs) };
                let elem = McdocType::Long { value_range: *value_range, attributes: Vec::new() };
                for &it in items {
                    child_specs.push((it, ChildDefSpec { ty: elem.clone(), key_type: None, desc: None, deprecated: false }));
                }
                if let Some(range) = length_range
                    && !range.contains(items.len() as f64)
                {
                    errors.push(RuntimeError {
                        node: node_id,
                        kind: RuntimeErrorKind::InvalidCollectionLength { ranges: vec![*range] },
                        conflicting_nodes: None,
                    });
                }
            }
            McdocType::Tuple { items: expected_items, .. } => {
                let ArenaKind::Array(items) = &node.kind else { return (errors, child_specs) };
                for (i, &it) in items.iter().enumerate() {
                    if let Some(expected_ty) = expected_items.get(i) {
                        child_specs.push((it, ChildDefSpec { ty: expected_ty.clone(), key_type: None, desc: None, deprecated: false }));
                    } else {
                        errors.push(RuntimeError {
                            node: it,
                            kind: RuntimeErrorKind::UnknownTupleElement,
                            conflicting_nodes: None,
                        });
                    }
                }
                if expected_items.len() > items.len() {
                    let exact = expected_items.len() as f64;
                    errors.push(RuntimeError {
                        node: node_id,
                        kind: RuntimeErrorKind::InvalidCollectionLength {
                            ranges: vec![NumericRange { kind: RangeKind::INCLUSIVE, min: Some(exact), max: Some(exact) }],
                        },
                        conflicting_nodes: None,
                    });
                }
            }
            _ => {}
        }

        (errors, child_specs)
    }

    fn read_primitive_as_string(&self, node: &ArenaNode, expected_kind: &str) -> Option<McdocType> {
        if !self.env.primitives_read_as_strings || expected_kind != value_kind_of(&McdocType::String { length_range: None, attributes: Vec::new() }) {
            return None;
        }
        let value = match &node.kind {
            ArenaKind::Number { raw, .. } => raw.clone(),
            ArenaKind::Boolean(b) => b.to_string(),
            _ => return None,
        };
        Some(McdocType::Literal { value: LiteralValue::String { value }, attributes: Vec::new() })
    }

    fn is_string_key_assignable(&self, key: &str, expected: &McdocType) -> bool {
        let mut inferred = McdocType::Literal {
            value: LiteralValue::String { value: key.to_string() },
            attributes: Vec::new(),
        };
        if !check_inferred_attributes(expected.attributes(), &mut inferred) {
            return false;
        }
        let McdocType::Literal { value: LiteralValue::String { value: normalized }, .. } = inferred else {
            return false;
        };
        match expected {
            McdocType::Any { .. } | McdocType::Unsafe { .. } => true,
            McdocType::String { length_range, .. } => length_range.is_none_or(|r| r.contains(normalized.chars().count() as f64)),
            McdocType::Literal { value: LiteralValue::String { value }, .. } => normalized == *value,
            McdocType::Enum { values, attributes, .. } => {
                values.iter().any(|v| matches!(&v.value, EnumValue::String(s) if *s == normalized))
                    || self.accepts_unlisted_enum_value(values, attributes, &normalized)
            }
            McdocType::Union { members, .. } => members.iter().any(|m| self.is_string_key_assignable(key, m)),
            _ => false,
        }
    }

    /// Dispatcher names (`minecraft:feature_config`) don't name the registry their keys come from
    /// (`worldgen/feature`), so we look in every registry. Our vanilla lists match the pack's version,
    /// so a vanilla-namespace ID only shows up there when a mod registers it. Vanilla-mcdoc keys its
    /// dispatchers without the `minecraft:` namespace.
    fn registered_at_runtime(&self, key: &str) -> bool {
        let id = super::registries::lengthen_id(key);
        self.env.registries.entries.iter().any(|(category, ids)| !category.starts_with("tag/") && ids.binary_search(&id).is_ok())
    }

    /// Vanilla-mcdoc lists only vanilla values for some sets that mods extend. An enum marked with a
    /// bare `#[id]` (dimension effects, loot context types, multiface blocks) is really a resource
    /// location, and mods register more, sometimes even in the `minecraft` namespace. NeoForge's enum
    /// extension adds values to plain enums like `MobCategory` too, and requires them to be namespaced.
    fn accepts_unlisted_enum_value(&self, values: &[super::types::EnumField], attributes: &[Attribute], value: &str) -> bool {
        if !is_valid_resource_location(value) {
            return false;
        }
        if attributes.iter().any(|a| a.name == "id" && a.value.is_none()) {
            return true;
        }
        self.env.version.neoforge.is_some()
            && value.contains(':')
            && values.iter().all(|v| matches!(&v.value, EnumValue::String(s) if !s.contains(':')))
    }

    // ------------------------------------------------------------------------
    // Bottom-up error condensing and union sibling pruning
    // ------------------------------------------------------------------------

    fn condense_and_propagate(&mut self, start_group: GroupId, initial_errors: Vec<(DefId, Vec<RuntimeError>)>) {
        let mut queue: VecDeque<(GroupId, Vec<(DefId, Vec<RuntimeError>)>, usize)> = VecDeque::new();
        queue.push_back((start_group, initial_errors, 0));
        self.drain_propagation_queue(&mut queue);
    }

    fn propagate_to_parents(&mut self, group_id: GroupId, depth: usize) {
        let mut queue = VecDeque::new();
        self.enqueue_ready_parents(group_id, depth, &mut queue);
        self.drain_propagation_queue(&mut queue);
    }

    fn drain_propagation_queue(&mut self, queue: &mut VecDeque<(GroupId, Vec<(DefId, Vec<RuntimeError>)>, usize)>) {
        while let Some((gid, errors_on_layer, depth)) = queue.pop_front() {
            let (still_valid, condensed) = condense_errors_and_filter_siblings(&errors_on_layer);
            self.groups[gid.0].condensed_errors.push(condensed);

            let old_valid = self.groups[gid.0].valid_defs.clone();
            if old_valid.len() != still_valid.len() {
                let removed: Vec<DefId> = old_valid.into_iter().filter(|d| !still_valid.contains(d)).collect();
                let runtime_node = self.groups[gid.0].runtime_node;
                self.filter_child_definitions(&removed, runtime_node);
                self.groups[gid.0].valid_defs = still_valid;
            }

            self.enqueue_ready_parents(gid, depth, queue);
        }
    }

    fn enqueue_ready_parents(
        &self,
        gid: GroupId,
        depth: usize,
        queue: &mut VecDeque<(GroupId, Vec<(DefId, Vec<RuntimeError>)>, usize)>,
    ) {
        let parents = self.groups[gid.0].parents.clone();
        for parent_def in parents {
            let parent_group = self.defs[parent_def.0].group;
            let parent_valid = &self.groups[parent_group.0].valid_defs;

            // Wait for the last valid definition in parent_group that references `gid`.
            let last_def_with_gid = parent_valid.iter().rev().copied().find(|&d| self.defs[d.0].children.contains(&gid));
            if last_def_with_gid != Some(parent_def) {
                continue;
            }

            // Wait for the last child group in parent_group that has (or will have) errors at `depth`.
            let last_child = parent_valid
                .iter()
                .flat_map(|&d| self.defs[d.0].children.iter().copied())
                .rev()
                .find(|&child_gid| self.group_has_depth(child_gid, depth));
            if last_child != Some(gid) {
                continue;
            }

            let next_layer: Vec<(DefId, Vec<RuntimeError>)> = self.groups[parent_group.0]
                .valid_defs
                .iter()
                .copied()
                .map(|d| {
                    let errs = self.defs[d.0]
                        .children
                        .iter()
                        .flat_map(|&c| self.groups[c.0].condensed_errors.get(depth).cloned().unwrap_or_default())
                        .collect();
                    (d, errs)
                })
                .collect();
            queue.push_back((parent_group, next_layer, depth + 1));
        }
    }

    fn group_has_depth(&self, gid: GroupId, depth: usize) -> bool {
        if self.groups[gid.0].condensed_errors.len() > depth {
            return true;
        }
        let mut frontier = vec![gid];
        for _ in 0..depth {
            let mut next = Vec::new();
            for g in frontier {
                for &d in &self.groups[g.0].valid_defs {
                    next.extend(self.defs[d.0].children.iter().copied());
                }
            }
            frontier = next;
            if frontier.is_empty() {
                return false;
            }
        }
        !frontier.is_empty()
    }

    fn filter_child_definitions(&mut self, removed_defs: &[DefId], parent_node: JsonId) {
        if removed_defs.is_empty() {
            return;
        }
        let children = self.child_value_nodes(parent_node);
        for child_node in children {
            let mut removed_child_defs = Vec::new();
            let mut kept_groups = Vec::new();
            let gids = std::mem::take(&mut self.node_groups[child_node.0]);
            for gid in gids {
                self.groups[gid.0].parents.retain(|p| !removed_defs.contains(p));
                if self.groups[gid.0].parents.is_empty() {
                    removed_child_defs.extend(self.groups[gid.0].valid_defs.iter().copied());
                } else {
                    kept_groups.push(gid);
                }
            }
            self.node_groups[child_node.0] = kept_groups;
            if !removed_child_defs.is_empty() {
                self.filter_child_definitions(&removed_child_defs, child_node);
            }
        }
    }

    // ------------------------------------------------------------------------
    // Simplify + Dynamic Index Resolution
    // ------------------------------------------------------------------------

    fn simplify(
        &self,
        ty: &McdocType,
        node: JsonId,
        is_member: bool,
        type_args: &[McdocType],
        type_mapping: Option<&BTreeMap<String, McdocType>>,
        depth: u8,
    ) -> McdocType {
        if depth > 24 {
            return McdocType::Any { attributes: Vec::new() };
        }
        let mut result = match ty {
            McdocType::Reference { path, attributes } => {
                let Some(path) = path else {
                    return McdocType::empty_union();
                };
                if let Some(mapped) = type_mapping.and_then(|m| m.get(path)) {
                    let mut out = mapped.clone();
                    prepend_attrs(&mut out, attributes);
                    out
                } else if let Some(target) = self.env.tables.mcdoc.get(path) {
                    let mut out = self.simplify(target, node, is_member, type_args, type_mapping, depth + 1);
                    prepend_attrs(&mut out, attributes);
                    out
                } else {
                    McdocType::empty_union()
                }
            }
            McdocType::Dispatcher { registry, parallel_indices, .. } => {
                // Vanilla-mcdoc names these `mcdoc:*`. We answer them from the pack's block states.
                match registry.as_str() {
                    "minecraft:block_states" | "mcdoc:block_states" => return self.simplify_block_states(parallel_indices, node, is_member, false),
                    STATE_PREDICATE_DISPATCHER => return self.simplify_block_states(parallel_indices, node, is_member, true),
                    "mcdoc:block_state_keys" => return self.simplify_block_state_keys(parallel_indices, node, is_member),
                    _ => {}
                }
                let Some(entries) = self.env.tables.dispatchers.get(registry) else {
                    return McdocType::Any { attributes: Vec::new() };
                };
                self.resolve_indices(true, parallel_indices, entries, node, is_member, type_args, type_mapping, depth + 1)
            }
            McdocType::Indexed { parallel_indices, child, .. } => {
                let simplified_child = self.simplify(child, node, is_member, &[], type_mapping, depth + 1);
                let McdocType::Struct { fields, .. } = simplified_child else {
                    // Indexing into something we know nothing about (like an unknown block's states) tells us nothing either.
                    return if matches!(simplified_child, McdocType::Any { .. }) { simplified_child } else { McdocType::empty_union() };
                };
                let mut map = HashMap::new();
                for f in fields {
                    if let StructField::Pair { key, field_type, .. } = f {
                        let k = match key {
                            StructKey::Literal(s) => Some(s),
                            StructKey::Dynamic(McdocType::Literal { value: LiteralValue::String { value }, .. }) => Some(value),
                            _ => None,
                        };
                        if let Some(k) = k {
                            map.insert(k, field_type);
                        }
                    }
                }
                self.resolve_indices(false, parallel_indices, &map, node, is_member, type_args, type_mapping, depth + 1)
            }
            McdocType::Union { members, attributes } => {
                let mut kept: Vec<&McdocType> = members.iter().filter(|m| self.env.version.keeps(m.attributes())).collect();
                if self.env.require_canonical && kept.iter().any(|m| has_attr(m.attributes(), "canonical")) {
                    kept.retain(|m| has_attr(m.attributes(), "canonical"));
                }
                let mut flat = Vec::new();
                for m in kept {
                    let sm = self.simplify(m, node, is_member, type_args, type_mapping, depth + 1);
                    flat.extend(flatten_union(sm));
                }
                if is_holder_set(&flat) {
                    flat.push(loader_holder_set());
                }
                if flat.len() == 1 {
                    let mut single = flat.pop().unwrap();
                    prepend_attrs(&mut single, attributes);
                    single
                } else {
                    McdocType::Union { members: flat, attributes: attributes.clone() }
                }
            }
            McdocType::Struct { fields, attributes } => {
                let mut out_fields = Vec::new();
                let mut opened_by_unknown_type = false;
                let mut union_spreads: Vec<Vec<McdocType>> = Vec::new();
                for field in fields {
                    match field {
                        StructField::Pair { key, field_type, optional, deprecated, desc, attributes: fattrs } => {
                            if !self.env.version.keeps(fattrs) {
                                continue;
                            }
                            let is_dep = *deprecated || self.env.version.is_deprecated(fattrs);
                            let mapped_val = match type_mapping {
                                _ if loader_ignored(fattrs, self.env.ignored_loaders) => McdocType::Any { attributes: Vec::new() },
                                Some(m) if !m.is_empty() => McdocType::Mapped {
                                    child: Box::new(field_type.clone()),
                                    mapping: m.clone(),
                                    attributes: Vec::new(),
                                },
                                _ => field_type.clone(),
                            };
                            match key {
                                StructKey::Literal(s) => out_fields.push(StructField::Pair {
                                    key: StructKey::Literal(s.clone()),
                                    field_type: mapped_val,
                                    optional: *optional,
                                    deprecated: is_dep,
                                    desc: desc.clone(),
                                    attributes: fattrs.clone(),
                                }),
                                StructKey::Dynamic(kty) => {
                                    let skey = self.simplify(kty, node, true, &[], type_mapping, depth + 1);
                                    if let McdocType::Union { members, .. } = skey {
                                        for m in members {
                                            out_fields.push(StructField::Pair {
                                                key: struct_key_from_simplified(m),
                                                field_type: mapped_val.clone(),
                                                optional: true,
                                                deprecated: is_dep,
                                                desc: desc.clone(),
                                                attributes: fattrs.clone(),
                                            });
                                        }
                                    } else {
                                        out_fields.push(StructField::Pair {
                                            key: struct_key_from_simplified(skey),
                                            field_type: mapped_val,
                                            optional: *optional,
                                            deprecated: is_dep,
                                            desc: desc.clone(),
                                            attributes: fattrs.clone(),
                                        });
                                    }
                                }
                            }
                        }
                        StructField::Spread { field_type, attributes: fattrs } => {
                            if !self.env.version.keeps(fattrs) {
                                continue;
                            }
                            let spread = self.simplify(field_type, node, true, &[], type_mapping, depth + 1);
                            match spread {
                                McdocType::Any { attributes: spread_attrs } => {
                                    opened_by_unknown_type |= has_attr(&spread_attrs, MODDED_TYPE_ATTR);
                                    out_fields.push(StructField::Pair {
                                        key: StructKey::Dynamic(McdocType::Any { attributes: Vec::new() }),
                                        field_type: McdocType::Any { attributes: Vec::new() },
                                        optional: true,
                                        deprecated: false,
                                        desc: None,
                                        attributes: Vec::new(),
                                    });
                                }
                                McdocType::Struct { fields: spread_fields, .. } => {
                                    out_fields.extend(spread_fields);
                                }
                                McdocType::Union { members, .. } => union_spreads.push(members),
                                _ => {}
                            }
                        }
                    }
                }
                // `{ a, ...(B | C) }` means `{ a, ...B } | { a, ...C }`. This matters for dispatch targets
                // that changed shape between versions, like 1.20.1's `{"type": "constant", "value": 1}`.
                // We only expand one union spread per struct, so a struct can't multiply into dozens of variants.
                let distributed = match union_spreads.as_slice() {
                    [members] => members
                        .iter()
                        .filter_map(|member| match member {
                            McdocType::Struct { fields: member_fields, .. } => Some(member_fields),
                            _ => None,
                        })
                        .map(|member_fields| {
                            // Spyglass treats a union spread as adding nothing, so we don't require its fields either.
                            let optional_fields = member_fields.iter().cloned().map(|field| match field {
                                StructField::Pair { key, field_type, deprecated, desc, attributes, .. } => {
                                    StructField::Pair { key, field_type, optional: true, deprecated, desc, attributes }
                                }
                                spread => spread,
                            });
                            out_fields.iter().cloned().chain(optional_fields).collect::<Vec<_>>()
                        })
                        .collect::<Vec<_>>(),
                    _ => Vec::new(),
                };
                // A spread we couldn't resolve is a modded type with no schema (like `lithostitched:delegating`
                // in place of a vanilla structure). Its codec may not use the fields vanilla shares between
                // its own types, so we stop requiring them.
                if opened_by_unknown_type {
                    for field in &mut out_fields {
                        if let StructField::Pair { optional, .. } = field {
                            *optional = true;
                        }
                    }
                }
                if distributed.is_empty() {
                    McdocType::Struct { fields: out_fields, attributes: attributes.clone() }
                } else {
                    McdocType::Union {
                        members: distributed.into_iter().map(|fields| McdocType::Struct { fields, attributes: attributes.clone() }).collect(),
                        attributes: Vec::new(),
                    }
                }
            }
            McdocType::List { item, length_range, attributes } => {
                let mapped_item = match type_mapping {
                    Some(m) if !m.is_empty() => Box::new(McdocType::Mapped {
                        child: item.clone(),
                        mapping: m.clone(),
                        attributes: Vec::new(),
                    }),
                    _ => item.clone(),
                };
                McdocType::List { item: mapped_item, length_range: *length_range, attributes: attributes.clone() }
            }
            McdocType::Tuple { items, attributes } => {
                let mapped_items = match type_mapping {
                    Some(m) if !m.is_empty() => items
                        .iter()
                        .map(|it| McdocType::Mapped {
                            child: Box::new(it.clone()),
                            mapping: m.clone(),
                            attributes: Vec::new(),
                        })
                        .collect(),
                    _ => items.clone(),
                };
                McdocType::Tuple { items: mapped_items, attributes: attributes.clone() }
            }
            McdocType::Enum { enum_kind, values, attributes } => {
                let filtered: Vec<EnumField> = values.iter().filter(|v| self.env.version.keeps(&v.attributes)).cloned().collect();
                McdocType::Enum { enum_kind: *enum_kind, values: filtered, attributes: attributes.clone() }
            }
            McdocType::Concrete { child, type_args: raw_args, .. } => {
                let simplified_args: Vec<McdocType> = raw_args
                    .iter()
                    .map(|a| self.simplify(a, node, is_member, &[], type_mapping, depth + 1))
                    .collect();
                self.simplify(child, node, is_member, &simplified_args, type_mapping, depth + 1)
            }
            McdocType::Template { child, type_params, .. } => {
                let mut mapping = BTreeMap::new();
                for (i, param) in type_params.iter().enumerate() {
                    let arg = type_args.get(i).cloned().unwrap_or_else(McdocType::empty_union);
                    mapping.insert(param.path.clone(), arg);
                }
                self.simplify(child, node, is_member, &[], Some(&mapping), depth + 1)
            }
            McdocType::Mapped { child, mapping, .. } => {
                let mut simplified_map = BTreeMap::new();
                for (k, v) in mapping {
                    simplified_map.insert(k.clone(), self.simplify(v, node, is_member, &[], type_mapping, depth + 1));
                }
                self.simplify(child, node, is_member, &[], Some(&simplified_map), depth + 1)
            }
            other => other.clone(),
        };

        // Apply `#[id]` mapType (lengthens literal/enum strings with `minecraft:`).
        if has_attr(ty.attributes(), "id") || has_attr(result.attributes(), "id") {
            result = apply_id_map_type(result);
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_indices(
        &self,
        dispatches_registry_ids: bool,
        parallel_indices: &[Index],
        entries: &HashMap<String, McdocType>,
        node: JsonId,
        is_member: bool,
        type_args: &[McdocType],
        type_mapping: Option<&BTreeMap<String, McdocType>>,
        depth: u8,
    ) -> McdocType {
        let mut values = Vec::new();
        let unknown = entries.get("%unknown");

        for index in parallel_indices {
            let mut lookups = Vec::new();
            match index {
                Index::Static { value } => {
                    if value == "%fallback" {
                        values.clear();
                        for target in entries.values() {
                            if self.env.version.keeps(target.attributes()) {
                                let s = self.simplify(target, node, is_member, type_args, type_mapping, depth + 1);
                                values.extend(flatten_union(s));
                            }
                        }
                        break;
                    }
                    lookups.push(normalize_category(value));
                }
                Index::Dynamic { accessor } => {
                    for val in self.resolve_dynamic_accessor(accessor, node, is_member) {
                        lookups.push(val);
                    }
                }
            }
            if lookups.is_empty() {
                lookups.push("%none".to_string());
            }

            let mut resolved_targets = Vec::new();
            for key in &lookups {
                if let Some(target) = entries.get(key) {
                    if dispatches_registry_ids && !self.env.version.keeps(target.attributes()) && self.registered_at_runtime(key) {
                        // A mod backports this vanilla type (like `minecraft:fallen_tree` on 1.21.1), and
                        // the newer schema may not match the backport, so we treat it as an unknown type.
                        return McdocType::Any { attributes: vec![Attribute { name: MODDED_TYPE_ATTR.into(), value: None }] };
                    }
                    resolved_targets.push(target);
                } else if key.contains(':') && !key.starts_with("minecraft:") {
                    // Schemaless modded codec (e.g. `apotheosis:affixed_item` in `minecraft:item_sub_predicate`):
                    // treat as opaque `any` instead of vanilla's closed `%unknown` empty union.
                    return McdocType::Any { attributes: vec![Attribute { name: MODDED_TYPE_ATTR.into(), value: None }] };
                } else if let Some(target) = unknown {
                    resolved_targets.push(target);
                } else {
                    // Schemaless type: treat as opaque `any` so we don't flag every key.
                    return McdocType::Any { attributes: Vec::new() };
                }
            }
            for target in resolved_targets {
                if self.env.version.keeps(target.attributes()) {
                    let s = self.simplify(target, node, is_member, type_args, type_mapping, depth + 1);
                    values.extend(flatten_union(s));
                }
            }
        }

        if values.len() == 1 {
            values.pop().unwrap()
        } else {
            McdocType::Union { members: values, attributes: Vec::new() }
        }
    }

    fn simplify_block_states(&self, parallel_indices: &[Index], node: JsonId, is_member: bool, predicate: bool) -> McdocType {
        let mut lookups = Vec::new();
        for idx in parallel_indices {
            match idx {
                Index::Static { value } => lookups.push(lengthen_id(value)),
                Index::Dynamic { accessor } => {
                    for v in self.resolve_dynamic_accessor(accessor, node, is_member) {
                        if !v.starts_with('%') {
                            lookups.push(lengthen_id(&v));
                        }
                    }
                }
            }
        }
        let mut fields = Vec::new();
        let mut matched_any = false;
        for block_id in lookups {
            if let Some(props) = self.env.registries.block_states.get(&block_id) {
                matched_any = true;
                for (prop, vals) in props {
                    let enum_vals: Vec<EnumField> = vals
                        .iter()
                        .map(|v| EnumField {
                            identifier: v.clone(),
                            value: EnumValue::String(v.clone()),
                            desc: None,
                            attributes: Vec::new(),
                        })
                        .collect();
                    fields.push(StructField::Pair {
                        key: StructKey::Literal(prop.clone()),
                        field_type: McdocType::Enum {
                            enum_kind: Some(EnumKind::String),
                            values: enum_vals,
                            attributes: if predicate { vec![Attribute { name: STATE_PREDICATE_ATTR.into(), value: None }] } else { Vec::new() },
                        },
                        optional: true,
                        deprecated: false,
                        desc: None,
                        attributes: Vec::new(),
                    });
                }
            }
        }
        if matched_any {
            McdocType::Struct { fields, attributes: Vec::new() }
        } else {
            McdocType::Any { attributes: Vec::new() }
        }
    }

    fn simplify_block_state_keys(&self, parallel_indices: &[Index], node: JsonId, is_member: bool) -> McdocType {
        match self.simplify_block_states(parallel_indices, node, is_member, false) {
            McdocType::Struct { fields, .. } => McdocType::Union {
                members: fields
                    .into_iter()
                    .filter_map(|f| match f {
                        StructField::Pair { key: StructKey::Literal(name), .. } => {
                            Some(McdocType::Literal { value: LiteralValue::String { value: name }, attributes: Vec::new() })
                        }
                        _ => None,
                    })
                    .collect(),
                attributes: Vec::new(),
            },
            other => other,
        }
    }

    fn resolve_dynamic_accessor(&self, accessor: &[AccessorKey], node: JsonId, is_member: bool) -> Vec<String> {
        let start = if is_member { Some(node) } else { self.arena.get(node).parent };
        let mut curr: Vec<(Option<JsonId>, Option<JsonId>)> = vec![(start, self.arena.get(node).pair_key)];

        for step in accessor {
            match step {
                AccessorKey::Keyword { keyword: DynamicKeyword::Parent } => {
                    curr = curr
                        .into_iter()
                        .map(|(n, _)| match n {
                            Some(id) => (self.arena.get(id).parent, self.arena.get(id).pair_key),
                            None => (None, None),
                        })
                        .collect();
                }
                AccessorKey::Keyword { keyword: DynamicKeyword::Key } => {
                    curr = curr.into_iter().map(|(_, k)| (k, None)).collect();
                    break;
                }
                AccessorKey::Field(field_name) => {
                    let mut next = Vec::new();
                    for (n, _) in curr {
                        if let Some(id) = n
                            && let ArenaKind::Object(pairs) = &self.arena.get(id).kind
                        {
                            for (kid, vid) in pairs {
                                if let ArenaKind::String(ks) = &self.arena.get(*kid).kind
                                    && ks.value == *field_name
                                {
                                    next.push((*vid, Some(*kid)));
                                }
                            }
                        }
                    }
                    curr = next;
                }
            }
        }

        let mut out = Vec::new();
        for (n, _) in curr {
            if let Some(id) = n
                && let ArenaKind::String(s) = &self.arena.get(id).kind
            {
                out.push(normalize_category(&s.value));
            } else {
                out.push("%none".to_string());
            }
        }
        out
    }

    // ------------------------------------------------------------------------
    // Attach types + leaf attribute & registry checks
    // ------------------------------------------------------------------------

    fn attach_and_check_attributes(&mut self, node_id: JsonId) {
        let gids = self.node_groups[node_id.0].clone();
        let mut active_defs: Vec<(GroupId, DefId)> = Vec::new();
        for gid in &gids {
            for &did in &self.groups[gid.0].valid_defs {
                active_defs.push((*gid, did));
            }
        }

        if active_defs.len() == 1 {
            let (gid, did) = active_defs[0];
            let def_type = self.defs[did.0].type_def.clone();
            let desc = self.groups[gid.0].desc.clone();
            let deprecated = self.groups[gid.0].deprecated;
            let key_def = self.groups[gid.0].key_def.clone();

            self.attached.insert(node_id, AttachedInfo { type_def: def_type.clone(), desc: desc.clone(), deprecated });
            self.check_leaf_attributes(node_id, &def_type);

            if let Some(key_id) = self.arena.get(node_id).pair_key
                && let Some(kd) = key_def
            {
                if deprecated {
                    self.diagnostics.push(Diagnostic {
                        span: self.arena.get(key_id).span,
                        severity: Severity::Warning,
                        message: "This key is deprecated in this Minecraft version.".into(),
                    });
                }
                self.attached.insert(key_id, AttachedInfo { type_def: kd.clone(), desc, deprecated });
                self.check_leaf_attributes(key_id, &kd);
            }
        } else if active_defs.len() > 1 {
            let members: Vec<McdocType> = active_defs.iter().map(|(_, did)| self.defs[did.0].type_def.clone()).collect();
            self.attached.insert(
                node_id,
                AttachedInfo { type_def: McdocType::Union { members, attributes: Vec::new() }, desc: None, deprecated: false },
            );
            if let Some(key_id) = self.arena.get(node_id).pair_key {
                let key_members: Vec<McdocType> = gids.iter().filter_map(|gid| self.groups[gid.0].key_def.clone()).collect();
                if !key_members.is_empty() {
                    self.attached.insert(
                        key_id,
                        AttachedInfo { type_def: McdocType::Union { members: key_members, attributes: Vec::new() }, desc: None, deprecated: false },
                    );
                }
            }
        }

        for child in self.child_value_nodes(node_id) {
            self.attach_and_check_attributes(child);
        }
    }

    fn check_leaf_attributes(&mut self, node_id: JsonId, type_def: &McdocType) {
        let node = self.arena.get(node_id);
        for attr in type_def.attributes() {
            match attr.name.as_str() {
                "id" => {
                    let ArenaKind::String(s) = &node.kind else { continue };
                    let Some(cfg) = parse_id_attribute(attr) else { continue };
                    if cfg.empty_allowed && s.value.is_empty() {
                        continue;
                    }
                    let mut raw = s.value.as_str();
                    if let Some(pref) = &cfg.prefix {
                        if let Some(stripped) = raw.strip_prefix(pref.as_str()) {
                            raw = stripped;
                        } else {
                            self.diagnostics.push(Diagnostic {
                                span: s.span,
                                severity: Severity::Error,
                                message: format!("Expected \"{pref}\" prefix."),
                            });
                            continue;
                        }
                    }
                    let body = raw.strip_prefix('#').unwrap_or(raw);
                    if !is_valid_resource_location(body) {
                        self.diagnostics.push(Diagnostic {
                            span: s.span,
                            severity: Severity::Error,
                            message: format!("\"{raw}\" is not a valid resource location."),
                        });
                        continue;
                    }
                    let full = if let Some(implicit_path) = &cfg.path {
                        let base = lengthen_id(body);
                        if let Some((ns, p)) = base.split_once(':') {
                            format!("{ns}:{implicit_path}{p}")
                        } else {
                            base
                        }
                    } else {
                        lengthen_id(body)
                    };
                    for excl in &cfg.exclude {
                        if full == lengthen_id(excl) {
                            self.diagnostics.push(Diagnostic {
                                span: s.span,
                                severity: Severity::Warning,
                                message: format!("\"{full}\" is not allowed here."),
                            });
                        }
                    }
                    if let Some(reg) = &cfg.registry
                        && !cfg.definition
                    {
                        let (lookup_reg, allow_tag, require_tag) = match cfg.tags {
                            Some(TagMode::Implicit) => (format!("tag/{}", normalize_category(reg)), false, false),
                            Some(TagMode::Allowed) => (normalize_category(reg), true, false),
                            Some(TagMode::Required) => (normalize_category(reg), true, true),
                            None => (normalize_category(reg), false, false),
                        };
                        let lookup_target = if cfg.path.is_some() {
                            if raw.starts_with('#') { format!("#{full}") } else { full.clone() }
                        } else {
                            raw.to_string()
                        };
                        match self.env.registries.lookup(&lookup_reg, &lookup_target, allow_tag, require_tag) {
                            IdLookup::Known => {}
                            IdLookup::UnknownAuthoritative => {
                                self.diagnostics.push(Diagnostic {
                                    span: s.span,
                                    severity: Severity::Warning,
                                    message: format!("Unknown {lookup_reg} \"{raw}\"."),
                                });
                            }
                            IdLookup::UnknownUnverified => {
                                self.diagnostics.push(Diagnostic {
                                    span: s.span,
                                    severity: Severity::Warning,
                                    message: format!("Packraven doesn't know this {lookup_reg} ID (\"{raw}\"), but it might exist. Run a registry dump to verify modded IDs."),
                                });
                            }
                        }
                    }
                }
                "divisible_by" => {
                    if let Some(div) = attr_number(attr)
                        && div != 0.0
                        && let ArenaKind::Number { value, .. } = &node.kind
                        && (*value % div).abs() > 1e-9
                    {
                        self.diagnostics.push(Diagnostic {
                            span: node.span,
                            severity: Severity::Warning,
                            message: format!("{value} is not divisible by {div}."),
                        });
                    }
                }
                "regex_pattern" => {
                    if let ArenaKind::String(s) = &node.kind
                        && let Err(e) = Regex::new(&s.value)
                    {
                        self.diagnostics.push(Diagnostic {
                            span: s.span,
                            severity: Severity::Warning,
                            message: format!("Invalid regular expression: {e}"),
                        });
                    }
                }
                "match_regex" => {
                    if let ArenaKind::String(s) = &node.kind {
                        let (pat, msg) = match &attr.value {
                            Some(super::types::AttributeValue::Type(McdocType::Literal { value: LiteralValue::String { value }, .. })) => {
                                (Some(value.as_str()), None)
                            }
                            Some(super::types::AttributeValue::Tree(t)) => {
                                let p = t.values.get("pattern").and_then(|v| match v {
                                    super::types::AttributeValue::Type(McdocType::Literal { value: LiteralValue::String { value }, .. }) => {
                                        Some(value.as_str())
                                    }
                                    _ => None,
                                });
                                let m = t.values.get("message").and_then(|v| match v {
                                    super::types::AttributeValue::Type(McdocType::Literal { value: LiteralValue::String { value }, .. }) => {
                                        Some(value.as_str())
                                    }
                                    _ => None,
                                });
                                (p, m)
                            }
                            _ => (None, None),
                        };
                        if let Some(pattern) = pat
                            && let Ok(re) = Regex::new(pattern)
                            && !re.is_match(&s.value)
                        {
                            self.diagnostics.push(Diagnostic {
                                span: s.span,
                                severity: Severity::Warning,
                                message: msg.map(String::from).unwrap_or_else(|| format!("String does not match pattern /{pattern}/.")),
                            });
                        }
                    }
                }
                "color" => {
                    if let ArenaKind::String(s) = &node.kind
                        && let Some(kind) = attr_string(attr)
                        && matches!(kind, "hex_rgb" | "hex_argb" | "hex_rgba")
                    {
                        let expected_len = if kind == "hex_rgb" { 7 } else { 9 };
                        let valid = s.value.starts_with('#')
                            && s.value.len() == expected_len
                            && s.value[1..].bytes().all(|b| b.is_ascii_hexdigit());
                        if !valid {
                            self.diagnostics.push(Diagnostic {
                                span: s.span,
                                severity: Severity::Warning,
                                message: format!("Expected a {kind} color string."),
                            });
                        }
                    }
                }
                "uuid" => {
                    if let ArenaKind::String(s) = &node.kind
                        && !is_valid_uuid(&s.value)
                    {
                        self.diagnostics.push(Diagnostic {
                            span: s.span,
                            severity: Severity::Warning,
                            message: format!("\"{}\" is not a valid UUID.", s.value),
                        });
                    }
                }
                "integer" => {
                    if let ArenaKind::String(s) = &node.kind {
                        match s.value.parse::<i64>() {
                            Err(_) => {
                                self.diagnostics.push(Diagnostic {
                                    span: s.span,
                                    severity: Severity::Error,
                                    message: format!("Expected an integer string, got \"{}\".", s.value),
                                });
                            }
                            Ok(val) => {
                                if let Some(cfg) = parse_integer_attribute(attr) {
                                    if cfg.min.is_some_and(|m| val < m) || cfg.max.is_some_and(|m| val > m) {
                                        let min_s = cfg.min.map(|m| m.to_string()).unwrap_or_default();
                                        let max_s = cfg.max.map(|m| m.to_string()).unwrap_or_default();
                                        self.diagnostics.push(Diagnostic {
                                            span: s.span,
                                            severity: Severity::Error,
                                            message: format!("Expected an integer in range {min_s}..{max_s}."),
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
                "text_component" => {
                    if let ArenaKind::String(s) = &node.kind {
                        let trimmed = s.value.trim_start();
                        // Minecraft parses stringified text components with Gson lenient mode, which allows single-quoted JSON.
                        if (trimmed.starts_with('{') || trimmed.starts_with('['))
                            && !trimmed.starts_with("{'")
                            && !trimmed.starts_with("[{'")
                            && !trimmed.starts_with("['")
                        {
                            let parsed = super::json_parser::parse(&s.value, false);
                            for diag in parsed.diagnostics {
                                self.diagnostics.push(Diagnostic {
                                    span: s.map.span_to_source(diag.span.start, diag.span.end),
                                    severity: diag.severity,
                                    message: diag.message,
                                });
                            }
                            if let Some(root) = &parsed.root
                                && self.env.tables.mcdoc.contains_key("::java::util::text::Text")
                            {
                                let text_ty = McdocType::Reference {
                                    path: Some("::java::util::text::Text".into()),
                                    attributes: Vec::new(),
                                };
                                let nested = check_json_with_type(root, &text_ty, self.env);
                                for diag in nested.diagnostics {
                                    self.diagnostics.push(Diagnostic {
                                        span: s.map.span_to_source(diag.span.start, diag.span.end),
                                        severity: diag.severity,
                                        message: diag.message,
                                    });
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // ------------------------------------------------------------------------
    // Diagnostic formatting
    // ------------------------------------------------------------------------

    fn report_runtime_error(&mut self, err: &RuntimeError) {
        let node = self.arena.get(err.node);
        let span = match err.kind {
            RuntimeErrorKind::MissingKey { .. } | RuntimeErrorKind::InvalidCollectionLength { .. } => {
                Span::new(node.span.start, (node.span.start + 1).min(node.span.end.max(node.span.start)))
            }
            _ => node.span,
        };
        match &err.kind {
            RuntimeErrorKind::Internal => {}
            RuntimeErrorKind::UnknownTupleElement => {
                self.diagnostics.push(Diagnostic { span, severity: Severity::Error, message: "Unexpected extra tuple element.".into() });
            }
            RuntimeErrorKind::UnknownKey => {
                if let Some(conflicts) = &err.conflicting_nodes {
                    let names: Vec<String> = conflicts
                        .iter()
                        .filter_map(|&id| match &self.arena.get(id).kind {
                            ArenaKind::String(s) => Some(format!("\"{}\"", s.value)),
                            _ => None,
                        })
                        .collect();
                    self.diagnostics.push(Diagnostic {
                        span,
                        severity: Severity::Error,
                        message: format!("Invalid key combination: {}.", names.join(" and ")),
                    });
                } else {
                    let name = match &node.kind {
                        ArenaKind::String(s) => format!("\"{}\"", s.value),
                        _ => "<key>".into(),
                    };
                    self.diagnostics.push(Diagnostic {
                        span,
                        severity: Severity::Warning,
                        message: format!("Unknown key {name}."),
                    });
                }
            }
            RuntimeErrorKind::MissingKey { keys } => {
                let message = if keys.len() == 1 {
                    format!("Missing required key \"{}\".", keys[0])
                } else {
                    let list: Vec<String> = keys.iter().map(|k| format!("\"{k}\"")).collect();
                    format!("Missing one of required keys: {}.", list.join(" or "))
                };
                self.diagnostics.push(Diagnostic { span, severity: Severity::Error, message });
            }
            RuntimeErrorKind::InvalidCollectionLength { ranges } => {
                let list: Vec<String> = ranges.iter().map(|r| r.to_string()).collect();
                self.diagnostics.push(Diagnostic {
                    span,
                    severity: Severity::Error,
                    message: format!("Expected collection length in range {}.", list.join(" or ")),
                });
            }
            RuntimeErrorKind::InvalidStringLength { ranges } => {
                let list: Vec<String> = ranges.iter().map(|r| r.to_string()).collect();
                self.diagnostics.push(Diagnostic {
                    span,
                    severity: Severity::Error,
                    message: format!("Expected string length in range {}.", list.join(" or ")),
                });
            }
            RuntimeErrorKind::StateNeverMatches => {
                let value = match &node.kind {
                    ArenaKind::String(s) => s.value.clone(),
                    _ => String::new(),
                };
                self.diagnostics.push(Diagnostic {
                    span,
                    severity: Severity::Warning,
                    message: format!("This block has no \"{value}\" state for this property, so this never matches."),
                });
            }
            RuntimeErrorKind::IntegerReadAs { read_as } => {
                let raw = match &node.kind {
                    ArenaKind::Number { raw, .. } => raw.as_str(),
                    _ => "this number",
                };
                self.diagnostics.push(Diagnostic {
                    span,
                    severity: Severity::Warning,
                    message: format!("Expected an integer. Minecraft reads {raw} as {read_as}."),
                });
            }
            RuntimeErrorKind::NumberOutOfRange { ranges } => {
                let list: Vec<String> = ranges.iter().map(|r| r.format()).collect();
                self.diagnostics.push(Diagnostic {
                    span,
                    severity: Severity::Error,
                    message: format!("Expected a number in range {}.", list.join(" or ")),
                });
            }
            RuntimeErrorKind::TypeMismatch { expected } => {
                let mut labels = Vec::new();
                for ty in expected {
                    let label = format_expected_type(ty);
                    if !labels.contains(&label) {
                        labels.push(label);
                    }
                }
                let message = if labels.is_empty() {
                    "Unexpected value.".into()
                } else {
                    let expects_specific = expected.iter().any(|t| matches!(t, McdocType::Literal { .. } | McdocType::Enum { .. }));
                    let actual = format_actual_node(&node.kind, expects_specific);
                    format!("Expected {}, found {actual}.", labels.join(" or "))
                };
                self.diagnostics.push(Diagnostic { span, severity: Severity::Error, message });
            }
        }
    }
}

// ============================================================================
// Pure error-condensing helpers (matching Spyglass's error.ts priority order)
// ============================================================================

fn condense_errors_and_filter_siblings(defs: &[(DefId, Vec<RuntimeError>)]) -> (Vec<DefId>, Vec<RuntimeError>) {
    if defs.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let mut valid: Vec<(DefId, Vec<RuntimeError>)> = defs.to_vec();
    let mut errors = Vec::new();

    // 1. TypeMismatch
    let (next, errs) = condense_layer(
        &valid,
        |e| matches!(e.kind, RuntimeErrorKind::TypeMismatch { .. }),
        |a, b| match (&a.kind, &b.kind) {
            (RuntimeErrorKind::TypeMismatch { expected: ea }, RuntimeErrorKind::TypeMismatch { expected: eb }) => ea == eb,
            _ => false,
        },
        Some(|group: &[RuntimeError]| {
            let mut merged = Vec::new();
            for e in group {
                if let RuntimeErrorKind::TypeMismatch { expected } = &e.kind {
                    for ty in expected {
                        if !merged.contains(ty) {
                            merged.push(ty.clone());
                        }
                    }
                }
            }
            RuntimeError { node: group[0].node, kind: RuntimeErrorKind::TypeMismatch { expected: merged }, conflicting_nodes: None }
        }),
    );
    valid = next;
    errors.extend(errs);

    // 2. UnknownKey / UnknownTupleElement. DFU's map codecs ignore keys they don't read, so when every
    // member has some, a member whose only problems are extra keys still decodes (Nature's Spirit's
    // `{"type": "minecraft:constant", "absolute": 80}` reads as an inline `absolute` anchor).
    let is_extra = |e: &RuntimeError| matches!(e.kind, RuntimeErrorKind::UnknownKey | RuntimeErrorKind::UnknownTupleElement);
    if !valid.iter().any(|(_, errs)| !errs.iter().any(is_extra)) {
        let decodes: Vec<(DefId, Vec<RuntimeError>)> = valid.iter().filter(|(_, errs)| errs.iter().all(is_extra)).cloned().collect();
        if !decodes.is_empty() {
            valid = decodes;
        }
    }
    for check in [
        (|e: &RuntimeError| matches!(e.kind, RuntimeErrorKind::UnknownKey)) as fn(&RuntimeError) -> bool,
        |e| matches!(e.kind, RuntimeErrorKind::UnknownTupleElement),
    ] {
        let (next, errs) = condense_layer(&valid, check, |_, _| true, None::<fn(&[RuntimeError]) -> RuntimeError>);
        valid = next;
        errors.extend(errs);
    }

    // 3. MissingKey
    let (next, errs) = condense_layer(
        &valid,
        |e| matches!(e.kind, RuntimeErrorKind::MissingKey { .. }),
        |a, b| match (&a.kind, &b.kind) {
            (RuntimeErrorKind::MissingKey { keys: ka }, RuntimeErrorKind::MissingKey { keys: kb }) => {
                ka.iter().all(|k| kb.contains(k)) && kb.iter().all(|k| ka.contains(k))
            }
            _ => false,
        },
        Some(|group: &[RuntimeError]| {
            let mut keys = Vec::new();
            for e in group {
                if let RuntimeErrorKind::MissingKey { keys: ks } = &e.kind {
                    for k in ks {
                        if !keys.contains(k) {
                            keys.push(k.clone());
                        }
                    }
                }
            }
            RuntimeError { node: group[0].node, kind: RuntimeErrorKind::MissingKey { keys }, conflicting_nodes: None }
        }),
    );
    valid = next;
    errors.extend(errs);

    // 4. Ranges (collection length, string length, numeric range)
    let (next, errs) = condense_layer(
        &valid,
        |e| matches!(e.kind, RuntimeErrorKind::InvalidCollectionLength { .. }),
        |a, b| match (&a.kind, &b.kind) {
            (RuntimeErrorKind::InvalidCollectionLength { ranges: ra }, RuntimeErrorKind::InvalidCollectionLength { ranges: rb }) => ra == rb,
            _ => false,
        },
        Some(|group: &[RuntimeError]| {
            let mut ranges = Vec::new();
            for e in group {
                if let RuntimeErrorKind::InvalidCollectionLength { ranges: rs } = &e.kind {
                    for r in rs {
                        if !ranges.contains(r) {
                            ranges.push(*r);
                        }
                    }
                }
            }
            RuntimeError { node: group[0].node, kind: RuntimeErrorKind::InvalidCollectionLength { ranges }, conflicting_nodes: None }
        }),
    );
    valid = next;
    errors.extend(errs);

    let (next, errs) = condense_layer(
        &valid,
        |e| matches!(e.kind, RuntimeErrorKind::InvalidStringLength { .. }),
        |a, b| match (&a.kind, &b.kind) {
            (RuntimeErrorKind::InvalidStringLength { ranges: ra }, RuntimeErrorKind::InvalidStringLength { ranges: rb }) => ra == rb,
            _ => false,
        },
        Some(|group: &[RuntimeError]| {
            let mut ranges = Vec::new();
            for e in group {
                if let RuntimeErrorKind::InvalidStringLength { ranges: rs } = &e.kind {
                    for r in rs {
                        if !ranges.contains(r) {
                            ranges.push(*r);
                        }
                    }
                }
            }
            RuntimeError { node: group[0].node, kind: RuntimeErrorKind::InvalidStringLength { ranges }, conflicting_nodes: None }
        }),
    );
    valid = next;
    errors.extend(errs);

    let (next, errs) = condense_layer(
        &valid,
        |e| matches!(e.kind, RuntimeErrorKind::NumberOutOfRange { .. }),
        |a, b| match (&a.kind, &b.kind) {
            (RuntimeErrorKind::NumberOutOfRange { ranges: ra }, RuntimeErrorKind::NumberOutOfRange { ranges: rb }) => ra == rb,
            _ => false,
        },
        Some(|group: &[RuntimeError]| {
            let mut ranges = Vec::new();
            for e in group {
                if let RuntimeErrorKind::NumberOutOfRange { ranges: rs } = &e.kind {
                    for r in rs {
                        if !ranges.contains(r) {
                            ranges.push(r.clone());
                        }
                    }
                }
            }
            RuntimeError { node: group[0].node, kind: RuntimeErrorKind::NumberOutOfRange { ranges }, conflicting_nodes: None }
        }),
    );
    valid = next;
    errors.extend(errs);

    // 5. Internal
    let (next, errs) = condense_layer(
        &valid,
        |e| matches!(e.kind, RuntimeErrorKind::Internal),
        |_, _| false,
        None::<fn(&[RuntimeError]) -> RuntimeError>,
    );
    valid = next;
    errors.extend(errs);

    // 6. State predicate values the block doesn't have. These load but never match.
    let (next, errs) = condense_layer(
        &valid,
        |e| matches!(e.kind, RuntimeErrorKind::StateNeverMatches),
        |_, _| true,
        None::<fn(&[RuntimeError]) -> RuntimeError>,
    );
    valid = next;
    errors.extend(errs);

    // 7. Integers vanilla truncates or wraps. These still load, so a member that reads the number
    // exactly wins.
    let (next, errs) = condense_layer(
        &valid,
        |e| matches!(e.kind, RuntimeErrorKind::IntegerReadAs { .. }),
        |a, b| matches!((&a.kind, &b.kind), (RuntimeErrorKind::IntegerReadAs { read_as: x }, RuntimeErrorKind::IntegerReadAs { read_as: y }) if x == y),
        Some(|group: &[RuntimeError]| group[0].clone()),
    );
    valid = next;
    errors.extend(errs);

    (valid.into_iter().map(|(d, _)| d).collect(), errors)
}

fn condense_layer<F, Eq, Comb>(
    valid: &[(DefId, Vec<RuntimeError>)],
    matches_kind: F,
    eq: Eq,
    combine: Option<Comb>,
) -> (Vec<(DefId, Vec<RuntimeError>)>, Vec<RuntimeError>)
where
    F: Fn(&RuntimeError) -> bool,
    Eq: Fn(&RuntimeError, &RuntimeError) -> bool,
    Comb: Fn(&[RuntimeError]) -> RuntimeError,
{
    let by_def: Vec<(usize, Vec<&RuntimeError>)> = valid
        .iter()
        .enumerate()
        .map(|(idx, (_, errs))| (idx, errs.iter().filter(|e| matches_kind(e)).collect()))
        .collect();

    let clean: Vec<(DefId, Vec<RuntimeError>)> = by_def
        .iter()
        .filter(|(_, errs)| errs.is_empty())
        .map(|(idx, _)| valid[*idx].clone())
        .collect();
    if !clean.is_empty() {
        return (clean, Vec::new());
    }

    // Group distinct errors per runtime node.
    struct DistinctErr {
        error: RuntimeError,
        defs: Vec<usize>,
    }
    struct PerNode {
        node: JsonId,
        entries: Vec<DistinctErr>,
    }
    let mut per_node: Vec<PerNode> = Vec::new();
    for (def_idx, errs) in &by_def {
        for err in errs {
            let bucket = match per_node.iter_mut().find(|p| p.node == err.node) {
                Some(b) => b,
                None => {
                    per_node.push(PerNode { node: err.node, entries: Vec::new() });
                    per_node.last_mut().unwrap()
                }
            };
            if let Some(existing) = bucket.entries.iter_mut().find(|d| eq(&d.error, err)) {
                if !existing.defs.contains(def_idx) {
                    existing.defs.push(*def_idx);
                }
            } else {
                bucket.entries.push(DistinctErr { error: (*err).clone(), defs: vec![*def_idx] });
            }
        }
    }

    let total_defs = valid.len();
    let mut common_errors = Vec::new();
    let mut defs_with_uncommon: HashSet<usize> = HashSet::new();

    for bucket in &per_node {
        for entry in &bucket.entries {
            if entry.defs.len() == total_defs {
                common_errors.push(entry.error.clone());
            } else {
                for &d in &entry.defs {
                    defs_with_uncommon.insert(d);
                }
            }
        }
    }

    let only_common: Vec<(DefId, Vec<RuntimeError>)> = (0..total_defs)
        .filter(|i| !defs_with_uncommon.contains(i))
        .map(|i| valid[i].clone())
        .collect();
    if !only_common.is_empty() {
        return (only_common, common_errors);
    }

    let mut combined: Vec<(Vec<usize>, RuntimeError)> = Vec::new();
    if let Some(combine_fn) = combine {
        for bucket in per_node {
            let uncommon: Vec<DistinctErr> = bucket.entries.into_iter().filter(|e| e.defs.len() < total_defs).collect();
            if uncommon.is_empty() {
                continue;
            }
            let mut def_set = Vec::new();
            for e in &uncommon {
                for &d in &e.defs {
                    if !def_set.contains(&d) {
                        def_set.push(d);
                    }
                }
            }
            let errs: Vec<RuntimeError> = uncommon.into_iter().map(|e| e.error).collect();
            combined.push((def_set, combine_fn(&errs)));
        }
    } else {
        for bucket in per_node {
            for entry in bucket.entries {
                if entry.defs.len() < total_defs {
                    combined.push((entry.defs, entry.error));
                }
            }
        }
    }

    let conflicting_nodes: Vec<JsonId> = combined
        .iter()
        .filter(|(defs, _)| defs.len() < total_defs)
        .map(|(_, e)| e.node)
        .fold(Vec::new(), |mut acc, n| {
            if !acc.contains(&n) {
                acc.push(n);
            }
            acc
        });

    let mut out_errors = common_errors;
    for (defs, mut err) in combined {
        if defs.len() < total_defs && conflicting_nodes.len() > 1 {
            err.conflicting_nodes = Some(conflicting_nodes.clone());
        }
        out_errors.push(err);
    }

    (valid.to_vec(), out_errors)
}

// ============================================================================
// Type inference, conversion, and small utilities
// ============================================================================

/// Marks a field only some loaders read, listing them comma-separated, so completion can skip
/// fields the pack's loader ignores while validation still accepts them.
pub const LOADER_ATTR: &str = "packraven_loader";

pub fn loader_attr(loaders: &str) -> Attribute {
    Attribute {
        name: LOADER_ATTR.into(),
        value: Some(super::types::AttributeValue::Type(McdocType::Literal {
            value: LiteralValue::String { value: loaders.into() },
            attributes: Vec::new(),
        })),
    }
}

fn loader_ignored(attributes: &[Attribute], ignored: &[&str]) -> bool {
    attributes.iter().filter(|a| a.name == LOADER_ATTR).any(|a| match &a.value {
        Some(super::types::AttributeValue::Type(McdocType::Literal { value: LiteralValue::String { value }, .. })) => {
            value.split(',').all(|loader| ignored.contains(&loader))
        }
        _ => false,
    })
}

/// Vanilla-mcdoc types state predicates (loot `block_state_property`, block predicates, and trigger
/// `state` conditions) with the same `mcdoc:block_states` dispatcher as real block states. Vanilla reads
/// predicate values as plain strings and a value the block lacks only fails to match, so we route
/// those symbols through this dispatcher to warn instead of erroring.
pub const STATE_PREDICATE_DISPATCHER: &str = "packraven:block_state_predicate";
const STATE_PREDICATE_ATTR: &str = "packraven_state_predicate";

/// Vanilla-mcdoc requires loot entry `weight` to be at least 1, but vanilla has never checked it
/// (`GsonHelper.getAsInt` on 1.20.1, `Codec.INT` since), and an entry with weight 0 just never rolls.
pub fn relax_loot_entry_weight(tables: &mut SymbolTables) {
    let Some(McdocType::Struct { fields, .. }) = tables.mcdoc.get_mut("::java::data::loot::SingletonPoolEntry") else { return };
    for field in fields {
        if let StructField::Pair { key: StructKey::Literal(name), field_type: McdocType::Int { value_range, .. }, .. } = field
            && name == "weight"
        {
            *value_range = None;
        }
    }
}

pub fn mark_block_state_predicates(tables: &mut SymbolTables) {
    for symbol in [
        "::java::data::loot::condition::BlockStateProperty",
        "::java::data::advancement::predicate::BlockPredicateState",
        "::java::data::advancement::trigger::BlockStateConditions",
    ] {
        let Some(ty) = tables.mcdoc.get_mut(symbol) else { continue };
        let Ok(mut json) = serde_json::to_value(&*ty) else { continue };
        retarget_dispatcher(&mut json, "mcdoc:block_states", STATE_PREDICATE_DISPATCHER);
        if let Ok(patched) = serde_json::from_value(json) {
            *ty = patched;
        }
    }
}

fn retarget_dispatcher(json: &mut serde_json::Value, from: &str, to: &str) {
    match json {
        serde_json::Value::Object(map) => {
            if map.get("kind").and_then(|k| k.as_str()) == Some("dispatcher") && map.get("registry").and_then(|r| r.as_str()) == Some(from) {
                map.insert("registry".into(), to.into());
            }
            map.values_mut().for_each(|v| retarget_dispatcher(v, from, to));
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(|v| retarget_dispatcher(v, from, to)),
        _ => {}
    }
}

/// Marks the `any` we substitute for a modded dispatch key that has no schema.
const MODDED_TYPE_ATTR: &str = "packraven_modded_type";

/// Vanilla writes a holder set as an ID-or-tag string or a list of IDs from one registry.
fn is_holder_set(members: &[McdocType]) -> bool {
    let registry_of = |ty: &McdocType| ty.attributes().iter().find_map(parse_id_attribute).and_then(|cfg| cfg.registry);
    let single = members.iter().find_map(|m| matches!(m, McdocType::String { .. }).then(|| registry_of(m)).flatten());
    let listed = members.iter().find_map(|m| match m {
        McdocType::List { item, .. } if matches!(**item, McdocType::String { .. }) => registry_of(item),
        _ => None,
    });
    single.is_some() && single == listed
}

/// Forge and NeoForge also accept holder sets written as objects (`{"type": "neoforge:and", "values": [...]}`),
/// and mods add their own types, so we only require the `type` key.
fn loader_holder_set() -> McdocType {
    McdocType::Struct {
        fields: vec![
            StructField::Pair {
                key: StructKey::Literal("type".into()),
                field_type: McdocType::String { length_range: None, attributes: Vec::new() },
                optional: false,
                deprecated: false,
                desc: Some("Forge or NeoForge holder set type, like `neoforge:and` or `neoforge:any`.".into()),
                attributes: Vec::new(),
            },
            StructField::Pair {
                key: StructKey::Dynamic(McdocType::String { length_range: None, attributes: Vec::new() }),
                field_type: McdocType::Any { attributes: Vec::new() },
                optional: true,
                deprecated: false,
                desc: None,
                attributes: Vec::new(),
            },
        ],
        attributes: Vec::new(),
    }
}

fn flatten_union(ty: McdocType) -> Vec<McdocType> {
    match ty {
        McdocType::Union { members, .. } => members.into_iter().flat_map(flatten_union).collect(),
        other => vec![other],
    }
}

fn prepend_attrs(ty: &mut McdocType, attrs: &[Attribute]) {
    if attrs.is_empty() {
        return;
    }
    let mut merged = attrs.to_vec();
    merged.extend(ty.attributes().iter().cloned());
    *ty.attributes_mut() = merged;
}

fn has_attr(attrs: &[Attribute], name: &str) -> bool {
    attrs.iter().any(|a| a.name == name)
}

fn struct_key_from_simplified(ty: McdocType) -> StructKey {
    match ty {
        McdocType::Literal { value: LiteralValue::String { value }, attributes } if attributes.is_empty() => StructKey::Literal(value),
        other => StructKey::Dynamic(other),
    }
}

fn apply_id_map_type(ty: McdocType) -> McdocType {
    match ty {
        McdocType::Literal { value: LiteralValue::String { value }, attributes } => McdocType::Literal {
            value: LiteralValue::String { value: lengthen_id(&value) },
            attributes,
        },
        McdocType::Enum { enum_kind: Some(EnumKind::String), values, attributes } => {
            let mapped = values
                .into_iter()
                .map(|mut f| {
                    if let EnumValue::String(s) = &mut f.value {
                        *s = lengthen_id(s);
                    }
                    f
                })
                .collect();
            McdocType::Enum { enum_kind: Some(EnumKind::String), values: mapped, attributes }
        }
        McdocType::Union { members, attributes } => McdocType::Union {
            members: members.into_iter().map(apply_id_map_type).collect(),
            attributes,
        },
        other => other,
    }
}

fn check_inferred_attributes(attrs: &[Attribute], inferred: &mut McdocType) -> bool {
    for attr in attrs {
        match attr.name.as_str() {
            "id" => {
                let Some(cfg) = parse_id_attribute(attr) else { continue };
                let McdocType::Literal { value: LiteralValue::String { value }, .. } = inferred else { continue };
                if let Some(pref) = &cfg.prefix {
                    if !value.starts_with(pref.as_str()) {
                        return false;
                    }
                    if !value[pref.len()..].contains(':') {
                        *value = format!("{pref}minecraft:{}", &value[pref.len()..]);
                    }
                } else {
                    if value.starts_with('!') {
                        return false;
                    }
                    if !value.starts_with('#') && !value.contains(':') {
                        *value = format!("minecraft:{value}");
                    }
                }
            }
            "color" => {
                if let Some(kind) = attr_string(attr)
                    && kind.starts_with("hex_")
                    && let McdocType::Literal { value: LiteralValue::String { value }, .. } = inferred
                    && !value.starts_with('#')
                {
                    return false;
                }
            }
            _ => {}
        }
    }
    true
}

fn infer_json_type(node: &ArenaNode) -> McdocType {
    match &node.kind {
        ArenaKind::Boolean(b) => McdocType::Literal { value: LiteralValue::Boolean { value: *b }, attributes: Vec::new() },
        ArenaKind::String(s) => McdocType::Literal { value: LiteralValue::String { value: s.value.clone() }, attributes: Vec::new() },
        ArenaKind::Number { value, is_int, int_value, .. } => {
            let lit = if *is_int && let Some(iv) = int_value {
                LiteralValue::Long { value: *iv }
            } else {
                LiteralValue::Double { value: *value }
            };
            McdocType::Literal { value: lit, attributes: Vec::new() }
        }
        ArenaKind::Array(_) => McdocType::List { item: Box::new(McdocType::Any { attributes: Vec::new() }), length_range: None, attributes: Vec::new() },
        ArenaKind::Object(_) => McdocType::Struct { fields: Vec::new(), attributes: Vec::new() },
        ArenaKind::Null => McdocType::Any { attributes: Vec::new() },
    }
}

fn value_kind_of(ty: &McdocType) -> &'static str {
    match ty {
        McdocType::Literal { value, .. } => match value {
            LiteralValue::Boolean { .. } => "boolean",
            LiteralValue::String { .. } => "string",
            LiteralValue::Byte { .. } => "byte",
            LiteralValue::Short { .. } => "short",
            LiteralValue::Int { .. } => "int",
            LiteralValue::Long { .. } => "long",
            LiteralValue::Float { .. } => "float",
            LiteralValue::Double { .. } => "double",
        },
        McdocType::Enum { enum_kind, .. } => match enum_kind {
            Some(EnumKind::Byte) => "byte",
            Some(EnumKind::Short) => "short",
            Some(EnumKind::Int) => "int",
            Some(EnumKind::Long) => "long",
            Some(EnumKind::Float) => "float",
            Some(EnumKind::Double) => "double",
            Some(EnumKind::String) => "string",
            None => "any",
        },
        other => other.kind_name(),
    }
}

fn try_convert_json(node: &ArenaNode, target_kind: &str) -> Option<McdocType> {
    match &node.kind {
        ArenaKind::Array(_) => match target_kind {
            "list" => Some(McdocType::List {
                item: Box::new(McdocType::Any { attributes: Vec::new() }),
                length_range: None,
                attributes: Vec::new(),
            }),
            "byte_array" => Some(McdocType::ByteArray { value_range: None, length_range: None, attributes: Vec::new() }),
            "int_array" => Some(McdocType::IntArray { value_range: None, length_range: None, attributes: Vec::new() }),
            "long_array" => Some(McdocType::LongArray { value_range: None, length_range: None, attributes: Vec::new() }),
            "tuple" => Some(McdocType::Tuple { items: Vec::new(), attributes: Vec::new() }),
            _ => None,
        },
        ArenaKind::Number { value, is_int, int_value, .. } => {
            // Vanilla reads integers with `Number::intValue` (DFU's `Codec.INT`, Gson's `getAsInt`), so it
            // accepts `1.0`, which vanilla's own datagen writes for loot table `rolls`.
            let int_value = &if *is_int { *int_value } else { whole_number(*value) };
            let allow_int = int_value.is_some();
            let lit = match target_kind {
                "byte" if allow_int && int_value.is_some_and(|v| (-128..=127).contains(&v)) => Some(LiteralValue::Byte { value: *value }),
                "short" if allow_int && int_value.is_some_and(|v| (-32768..=32767).contains(&v)) => Some(LiteralValue::Short { value: *value }),
                "int" if allow_int && int_value.is_some_and(|v| (i32::MIN as i64..=i32::MAX as i64).contains(&v)) => {
                    Some(LiteralValue::Int { value: *value })
                }
                "long" if allow_int => int_value.map(|v| LiteralValue::Long { value: v }),
                "float" => Some(LiteralValue::Float { value: *value }),
                "double" => Some(LiteralValue::Double { value: *value }),
                _ => None,
            }?;
            Some(McdocType::Literal { value: lit, attributes: Vec::new() })
        }
        _ => None,
    }
}

/// Mirrors `Number.intValue()` and friends on Gson's `LazilyParsedNumber`, which vanilla's integer
/// codecs call: Java truncates the fraction and keeps only the low bits that fit the target width.
fn java_integer_conversion(node: &ArenaNode, target_kind: &str) -> Option<(McdocType, i64)> {
    let ArenaKind::Number { value, int_value, .. } = &node.kind else { return None };
    let whole = int_value.map(i128::from).unwrap_or_else(|| value.trunc() as i128);
    let (read_as, value) = match target_kind {
        "byte" => (whole as i8 as i64, LiteralValue::Byte { value: (whole as i8).into() }),
        "short" => (whole as i16 as i64, LiteralValue::Short { value: (whole as i16).into() }),
        "int" => (whole as i32 as i64, LiteralValue::Int { value: (whole as i32).into() }),
        "long" => (whole as i64, LiteralValue::Long { value: whole as i64 }),
        _ => return None,
    };
    Some((McdocType::Literal { value, attributes: Vec::new() }, read_as))
}

fn whole_number(value: f64) -> Option<i64> {
    (value.fract() == 0.0 && value.abs() < i64::MAX as f64).then_some(value as i64)
}

fn inferred_number(ty: &McdocType) -> Option<f64> {
    let McdocType::Literal { value, .. } = ty else { return None };
    match value {
        LiteralValue::Byte { value }
        | LiteralValue::Short { value }
        | LiteralValue::Int { value }
        | LiteralValue::Float { value }
        | LiteralValue::Double { value } => Some(*value),
        LiteralValue::Long { value } => Some(*value as f64),
        _ => None,
    }
}

fn literal_eq(a: &LiteralValue, b: &LiteralValue) -> bool {
    match (a, b) {
        (LiteralValue::Boolean { value: x }, LiteralValue::Boolean { value: y }) => x == y,
        (LiteralValue::String { value: x }, LiteralValue::String { value: y }) => x == y,
        (LiteralValue::Long { value: x }, LiteralValue::Long { value: y }) => x == y,
        (x, y) => match (lit_as_f64(x), lit_as_f64(y)) {
            (Some(xf), Some(yf)) => xf == yf,
            _ => false,
        },
    }
}

fn lit_as_f64(v: &LiteralValue) -> Option<f64> {
    match v {
        LiteralValue::Byte { value }
        | LiteralValue::Short { value }
        | LiteralValue::Int { value }
        | LiteralValue::Float { value }
        | LiteralValue::Double { value } => Some(*value),
        LiteralValue::Long { value } => Some(*value as f64),
        _ => None,
    }
}

fn enum_value_matches(ev: &EnumValue, lit: &LiteralValue) -> bool {
    match (ev, lit) {
        (EnumValue::String(s), LiteralValue::String { value }) => s == value,
        (EnumValue::Int(i), LiteralValue::Long { value }) => i == value,
        (EnumValue::Int(i), other) => lit_as_f64(other).is_some_and(|f| f == *i as f64),
        (EnumValue::Float(f), other) => lit_as_f64(other).is_some_and(|v| v == *f),
        _ => false,
    }
}

fn format_expected_type(ty: &McdocType) -> String {
    match ty {
        McdocType::Enum { values, .. } => {
            let items: Vec<String> = values
                .iter()
                .take(8)
                .map(|v| match &v.value {
                    EnumValue::String(s) => format!("\"{}\"", normalize_category(s)),
                    EnumValue::Int(i) => i.to_string(),
                    EnumValue::Float(f) => f.to_string(),
                })
                .collect();
            if values.len() > 8 {
                format!("{} (or {} more)", items.join(", "), values.len() - 8)
            } else {
                items.join(" or ")
            }
        }
        McdocType::Literal { value, .. } => match value {
            LiteralValue::String { value } => format!("\"{value}\""),
            LiteralValue::Boolean { value } => value.to_string(),
            LiteralValue::Long { value } => value.to_string(),
            other => lit_as_f64(other).map(|f| f.to_string()).unwrap_or_default(),
        },
        McdocType::Struct { .. } => "an object".into(),
        McdocType::List { .. } | McdocType::Tuple { .. } | McdocType::ByteArray { .. } | McdocType::IntArray { .. } | McdocType::LongArray { .. } => {
            "a list".into()
        }
        McdocType::String { .. } => "a string".into(),
        McdocType::Boolean { .. } => "a boolean".into(),
        McdocType::Byte { .. } | McdocType::Short { .. } | McdocType::Int { .. } | McdocType::Long { .. } => "an integer".into(),
        McdocType::Float { .. } | McdocType::Double { .. } => "a number".into(),
        other => other.to_string(),
    }
}

fn format_actual_node(kind: &ArenaKind, show_literal: bool) -> String {
    match kind {
        ArenaKind::Object(_) => "an object".into(),
        ArenaKind::Array(_) => "an array".into(),
        ArenaKind::String(s) => {
            if show_literal && s.value.len() <= 48 {
                format!("\"{}\"", s.value)
            } else {
                "a string".into()
            }
        }
        ArenaKind::Number { raw, is_int, .. } => {
            if *is_int {
                if show_literal { raw.clone() } else { format!("integer {raw}") }
            } else {
                format!("number {raw}")
            }
        }
        ArenaKind::Boolean(b) => {
            if show_literal { b.to_string() } else { "a boolean".into() }
        }
        ArenaKind::Null => "null".into(),
    }
}

fn is_valid_resource_location(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    if let Some((ns, path)) = s.split_once(':') {
        !ns.is_empty()
            && !path.is_empty()
            && ns.bytes().all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.'))
            && path.bytes().all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.' | b'/'))
    } else {
        s.bytes().all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.' | b'/'))
    }
}

fn is_valid_uuid(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 5 {
        return false;
    }
    let expected = [8, 4, 4, 4, 12];
    parts
        .iter()
        .zip(expected)
        .all(|(p, len)| p.len() == len && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcdoc::json_parser;
    use crate::mcdoc::parser::parse_and_bind;

    fn test_tables() -> SymbolTables {
        let mut tables = SymbolTables::default();
        parse_and_bind(
            r#"
dispatch minecraft:resource["recipe"] to struct Recipe {
	type: #[id="recipe_serializer"] string,
	...minecraft:recipe_serializer[[type]],
}

dispatch minecraft:recipe_serializer["minecraft:crafting_shapeless"] to struct Shapeless {
	ingredients: [Ingredient] @ 1..9,
	result: struct {
		id: #[id="item"] string,
		count?: int @ 1..99,
	},
}

type Ingredient = (
	#[id(registry="item", tags="allowed")] string |
	struct { item: #[id="item"] string } |
	struct { tag: #[id(registry="item", tags="implicit")] string }
)
"#,
            "::test",
            &mut tables,
        )
        .unwrap();
        tables
    }

    #[test]
    fn validates_recipe_with_dynamic_dispatcher_and_unions() {
        let tables = test_tables();
        let mut regs = RegistryIndex::default();
        regs.insert_entries("recipe_serializer", ["minecraft:crafting_shapeless".into()]);
        regs.insert_entries("item", ["minecraft:diamond".into(), "minecraft:stick".into()]);
        regs.insert_entries("tag/item", ["c:ingots/iron".into()]);
        regs.finalize();

        let version = VersionContext { minecraft: Some("1.21.1".into()), neoforge: Some("21.1.265".into()) };
        let env = CheckEnv { tables: &tables, registries: &regs, version: &version, require_canonical: false, allow_missing_keys: false, primitives_read_as_strings: false, ignored_loaders: &[] };
        let root_type = resource_root_type("recipe", &tables).unwrap();

        let ok_json = r##"{
            "type": "minecraft:crafting_shapeless",
            "ingredients": ["minecraft:diamond", "#c:ingots/iron", { "tag": "c:ingots/iron" }],
            "result": { "id": "minecraft:stick", "count": 4 }
        }"##;
        let parsed = json_parser::parse(ok_json, false);
        let out = check_json_with_type(parsed.root.as_ref().unwrap(), &root_type, &env);
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);

        // Unknown modded recipe serializer stays opaque (no false unknown-key errors on modded keys).
        let modded_json = r#"{
            "type": "create:cutting",
            "ingredients": [{ "item": "minecraft:diamond" }],
            "processing_time": 50
        }"#;
        let parsed_mod = json_parser::parse(modded_json, false);
        let out_mod = check_json_with_type(parsed_mod.root.as_ref().unwrap(), &root_type, &env);
        assert!(
            !out_mod.diagnostics.iter().any(|d| d.message.contains("Unknown key")),
            "schemaless modded recipe should not flag unknown keys: {:?}",
            out_mod.diagnostics
        );

        // Missing required key + out-of-range count should produce errors.
        let bad_json = r#"{
            "type": "minecraft:crafting_shapeless",
            "ingredients": [],
            "result": { "count": 500 }
        }"#;
        let parsed_bad = json_parser::parse(bad_json, false);
        let out_bad = check_json_with_type(parsed_bad.root.as_ref().unwrap(), &root_type, &env);
        assert!(out_bad.diagnostics.iter().any(|d| d.message.contains("collection length")));
        assert!(out_bad.diagnostics.iter().any(|d| d.message.contains("Missing required key \"id\"")));
        assert!(out_bad.diagnostics.iter().any(|d| d.message.contains("in range 1..99")));
    }

    #[test]
    fn accepts_what_mods_add_to_vanilla_sets() {
        let mut tables = SymbolTables::default();
        parse_and_bind(
            r#"
dispatch minecraft:resource["dimension_type"] to struct {
	effects?: #[id] Effects,
	spawn_overrides?: struct { [MobCategory]: int },
}
enum(string) Effects { Overworld = "overworld", Nether = "the_nether" }
enum(string) MobCategory { Monster = "monster", Creature = "creature" }

dispatch minecraft:resource["worldgen/structure"] to struct {
	type: #[id="worldgen/structure_type"] string,
	biomes: (#[id(registry="worldgen/biome", tags="allowed")] string | [#[id="worldgen/biome"] string]),
	...minecraft:structure_type[[type]],
}
dispatch minecraft:structure_type["minecraft:jigsaw"] to struct { size: int }
"#,
            "::test",
            &mut tables,
        )
        .unwrap();
        let mut regs = RegistryIndex::default();
        regs.insert_entries("worldgen/biome", ["minecraft:plains".into()]);
        regs.insert_entries("worldgen/structure_type", ["minecraft:jigsaw".into(), "lithostitched:delegating".into()]);
        regs.finalize();
        let check = |category: &str, json: &str, neoforge: Option<&str>| {
            let version = VersionContext { minecraft: Some("1.21.1".into()), neoforge: neoforge.map(String::from) };
            let env = CheckEnv { tables: &tables, registries: &regs, version: &version, require_canonical: false, allow_missing_keys: false, primitives_read_as_strings: false, ignored_loaders: &[] };
            let parsed = json_parser::parse(json, false);
            check_json_with_type(parsed.root.as_ref().unwrap(), &resource_root_type(category, &tables).unwrap(), &env).diagnostics
        };

        // An `#[id]` enum is a resource location; mods add values, even in the `minecraft` namespace.
        assert!(check("dimension_type", r#"{"effects": "aether:the_aether"}"#, None).is_empty());
        assert!(check("dimension_type", r#"{"effects": "minecraft:aether_stripping"}"#, None).is_empty());
        assert!(!check("dimension_type", r#"{"effects": "Not An Id"}"#, None).is_empty());

        // NeoForge's enum extension adds namespaced values to plain enums; Fabric has no such thing.
        let extended = r#"{"spawn_overrides": {"aether:aether_aerwhale": 1}}"#;
        assert!(check("dimension_type", extended, Some("21.1.265")).is_empty(), "{:?}", check("dimension_type", extended, Some("21.1.265")));
        assert!(!check("dimension_type", extended, None).is_empty());

        // Forge and NeoForge holder sets can be objects.
        let holder_set = r##"{"type": "minecraft:jigsaw", "size": 1, "biomes": {"type": "neoforge:and", "values": ["#minecraft:is_ocean"]}}"##;
        assert!(check("worldgen/structure", holder_set, Some("21.1.265")).is_empty(), "{:?}", check("worldgen/structure", holder_set, Some("21.1.265")));

        // A modded structure type with no schema doesn't have to carry vanilla's shared fields.
        assert!(check("worldgen/structure", r#"{"type": "lithostitched:delegating", "delegate": {}}"#, None).is_empty());
        // A vanilla type still does.
        let vanilla = check("worldgen/structure", r#"{"type": "minecraft:jigsaw", "size": 1}"#, None);
        assert!(vanilla.iter().any(|d| d.message.contains("Missing required key \"biomes\"")), "{vanilla:?}");
    }

    #[test]
    fn validates_neoforge_conditions_version_gating() {
        let mut tables = test_tables();
        parse_and_bind(crate::mcdoc::fetch::PACKRAVEN_MCDOC, "::packraven", &mut tables).unwrap();
        let mut regs = RegistryIndex::default();
        regs.insert_entries("recipe_serializer", ["minecraft:crafting_shapeless".into()]);
        regs.insert_entries("item", ["minecraft:diamond".into()]);
        regs.finalize();

        let root_type = resource_root_type("recipe", &tables).unwrap();
        let old_cond_json = r#"{
            "neoforge:conditions": [{ "type": "neoforge:item_exists", "item": "minecraft:diamond" }],
            "type": "minecraft:crafting_shapeless",
            "ingredients": ["minecraft:diamond"],
            "result": { "id": "minecraft:diamond" }
        }"#;
        let parsed = json_parser::parse(old_cond_json, false);

        // On NeoForge 21.1, `neoforge:item_exists` is valid.
        let v_old = VersionContext { minecraft: Some("1.21.1".into()), neoforge: Some("21.1.265".into()) };
        let env_old = CheckEnv { tables: &tables, registries: &regs, version: &v_old, require_canonical: false, allow_missing_keys: false, primitives_read_as_strings: false, ignored_loaders: &[] };
        let out_old = check_json_with_type(parsed.root.as_ref().unwrap(), &root_type, &env_old);
        assert!(
            !out_old.diagnostics.iter().any(|d| d.severity == Severity::Error),
            "{:?}",
            out_old.diagnostics
        );

        // Recursive condition combinators (`neoforge:not`, `forge:not`) must resolve nested conditions cleanly.
        let nested_cond = r#"{
            "neoforge:conditions": [{ "type": "neoforge:not", "value": { "type": "neoforge:mod_loaded", "modid": "malum" } }],
            "type": "minecraft:crafting_shapeless",
            "ingredients": ["minecraft:diamond"],
            "result": { "id": "minecraft:diamond" }
        }"#;
        let parsed_nested = json_parser::parse(nested_cond, false);
        let out_nested = check_json_with_type(parsed_nested.root.as_ref().unwrap(), &root_type, &env_old);
        assert!(
            !out_nested.diagnostics.iter().any(|d| d.severity == Severity::Error),
            "nested neoforge:not failed: {:?}",
            out_nested.diagnostics
        );

        // Unconditionally-false disable stubs (`neoforge:false`, `forge:false`) must not require body fields.
        for stub in [
            r#"{ "neoforge:conditions": [{ "type": "neoforge:false" }] }"#,
            r#"{ "conditions": [{ "type": "forge:false" }] }"#,
            r#"{ "fabric:load_conditions": [{ "condition": "fabric:false" }] }"#,
        ] {
            let parsed_stub = json_parser::parse(stub, false);
            let out_stub = check_json_with_type(parsed_stub.root.as_ref().unwrap(), &root_type, &env_old);
            assert!(
                !out_stub.diagnostics.iter().any(|d| d.severity == Severity::Error),
                "disable stub {stub} failed: {:?}",
                out_stub.diagnostics
            );
        }

        // Missing required keys on union-rooted resources (`item_modifier = LootFunction | [LootFunction]`)
        // and wrong-type diagnostics naming both expected and found types.
        let mut im_tables = SymbolTables::default();
        parse_and_bind(crate::mcdoc::fetch::PACKRAVEN_MCDOC, "::packraven", &mut im_tables).unwrap();
        parse_and_bind(
            r#"
dispatch minecraft:resource["item_modifier"] to (LootFunction | [LootFunction])
struct LootFunction {
	function: string,
	...minecraft:loot_function[[function]],
}
dispatch minecraft:loot_function["minecraft:sequence"] to struct {
	functions: [LootFunction],
}
dispatch minecraft:loot_function["minecraft:set_name"] to struct {
	name: (string | struct { text: string }),
}
"#,
            "::test",
            &mut im_tables,
        )
        .unwrap();
        let im_root = resource_root_type("item_modifier", &im_tables).unwrap();
        let im_env = CheckEnv { tables: &im_tables, registries: &regs, version: &v_old, require_canonical: false, allow_missing_keys: false, primitives_read_as_strings: false, ignored_loaders: &[] };

        let missing_fn = json_parser::parse(r#"{ "function": "minecraft:sequence" }"#, false);
        let out_missing = check_json_with_type(missing_fn.root.as_ref().unwrap(), &im_root, &im_env);
        assert!(
            out_missing.diagnostics.iter().any(|d| d.message == "Missing required key \"functions\"."),
            "expected missing functions key error, got: {:?}",
            out_missing.diagnostics
        );

        let wrong_ty = json_parser::parse(r#"{ "function": "minecraft:set_name", "name": [] }"#, false);
        let out_wrong = check_json_with_type(wrong_ty.root.as_ref().unwrap(), &im_root, &im_env);
        assert!(
            out_wrong.diagnostics.iter().any(|d| d.message.contains("Expected") && d.message.contains("found an array")),
            "expected wrong-type diagnostic naming found array, got: {:?}",
            out_wrong.diagnostics
        );
    }

    #[test]
    #[ignore]
    fn validates_against_real_vanilla_mcdoc_symbols() {
        let path = std::env::var("PACKRAVEN_VANILLA_SYMBOLS").expect("set PACKRAVEN_VANILLA_SYMBOLS to symbols.json");
        let raw = std::fs::read(&path).unwrap();
        let t0 = std::time::Instant::now();
        let mut tables: SymbolTables = serde_json::from_slice(&raw).expect("all 2,114 vanilla-mcdoc types should deserialize");
        let parse_ms = t0.elapsed().as_millis();
        parse_and_bind(crate::mcdoc::fetch::PACKRAVEN_MCDOC, "::packraven", &mut tables).unwrap();
        assert!(tables.mcdoc.len() > 2000, "got {} symbols", tables.mcdoc.len());
        assert!(tables.dispatchers.len() > 90, "got {} dispatchers", tables.dispatchers.len());

        let mut regs = RegistryIndex::default();
        regs.insert_entries("recipe_serializer", ["minecraft:crafting_shaped".into()]);
        regs.insert_entries("item", ["minecraft:diamond".into(), "minecraft:stick".into(), "minecraft:diamond_sword".into()]);
        regs.insert_entries("tag/item", ["c:ingots/iron".into()]);
        regs.finalize();

        let version = VersionContext { minecraft: Some("1.21.1".into()), neoforge: Some("21.1.265".into()) };
        let env = CheckEnv { tables: &tables, registries: &regs, version: &version, require_canonical: false, allow_missing_keys: false, primitives_read_as_strings: false, ignored_loaders: &[] };

        let recipe_type = resource_root_type("recipe", &tables).expect("recipe resource");
        let recipe_json = r#"{
            "neoforge:conditions": [{ "type": "neoforge:mod_loaded", "modid": "create" }],
            "type": "minecraft:crafting_shaped",
            "pattern": ["D", "D", "S"],
            "key": {
                "D": { "item": "minecraft:diamond" },
                "S": { "item": "minecraft:stick" }
            },
            "result": { "id": "minecraft:diamond_sword", "count": 1 }
        }"#;
        let parsed = json_parser::parse(recipe_json, false);
        let t1 = std::time::Instant::now();
        let out = check_json_with_type(parsed.root.as_ref().unwrap(), &recipe_type, &env);
        let check_us = t1.elapsed().as_micros();
        eprintln!("vanilla-mcdoc symbols ({} types) deserialized in {parse_ms} ms; real 1.21.1 shaped recipe checked in {check_us} us", tables.mcdoc.len());
        assert!(
            !out.diagnostics.iter().any(|d| d.severity == Severity::Error),
            "unexpected errors on valid 1.21.1 shaped recipe: {:?}",
            out.diagnostics
        );

        let loot_type = resource_root_type("loot_table", &tables).expect("loot_table resource");
        for (label, json) in [
            (
                "control_stand + sequence missing functions",
                r#"{
                  "type": "minecraft:block",
                  "pools": [{
                    "rolls": 1.0,
                    "bonus_rolls": 0.0,
                    "entries": [{
                      "type": "minecraft:item",
                      "name": "aeroworks:control_stand",
                      "functions": [{ "function": "minecraft:sequence" }]
                    }],
                    "conditions": [{ "condition": "minecraft:survives_explosion" }]
                  }]
                }"#,
            ),
            (
                "control_stand + sequence functions wrong type",
                r#"{
                  "type": "minecraft:block",
                  "pools": [{
                    "rolls": 1.0,
                    "entries": [{
                      "type": "minecraft:item",
                      "name": "aeroworks:control_stand",
                      "functions": [{ "function": "minecraft:set_name", "name": [] }]
                    }]
                  }]
                }"#,
            ),
            (
                "control_stand + pool missing entries/rolls",
                r#"{ "type": "minecraft:block", "pools": [{}] }"#,
            ),
        ] {
            let p = json_parser::parse(json, false);
            let o = check_json_with_type(p.root.as_ref().unwrap(), &loot_type, &env);
            eprintln!("=== {label} ===\n{:#?}", o.diagnostics);
        }
    }
}
