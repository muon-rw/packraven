#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(from = "RawSymbolTables")]
pub struct SymbolTables {
    #[serde(rename = "ref", default)]
    pub git_ref: Option<String>,
    #[serde(default)]
    pub mcdoc: HashMap<String, McdocType>,
    #[serde(rename = "mcdoc/dispatcher", default)]
    pub dispatchers: HashMap<String, HashMap<String, McdocType>>,
}

#[derive(Deserialize)]
struct RawSymbolTables {
    #[serde(rename = "ref", default)]
    git_ref: Option<String>,
    #[serde(default)]
    mcdoc: HashMap<String, McdocType>,
    #[serde(rename = "mcdoc/dispatcher", default)]
    dispatchers: HashMap<String, HashMap<String, McdocType>>,
}

impl From<RawSymbolTables> for SymbolTables {
    fn from(mut raw: RawSymbolTables) -> Self {
        for ty in raw.mcdoc.values_mut() {
            ty.normalize_long_enums();
        }
        for map in raw.dispatchers.values_mut() {
            for ty in map.values_mut() {
                ty.normalize_long_enums();
            }
        }
        Self { git_ref: raw.git_ref, mcdoc: raw.mcdoc, dispatchers: raw.dispatchers }
    }
}

impl SymbolTables {
    pub fn merge_from(&mut self, other: SymbolTables) {
        self.mcdoc.extend(other.mcdoc);
        for (registry, entries) in other.dispatchers {
            self.dispatchers.entry(registry).or_default().extend(entries);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Attribute {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<AttributeValue>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum AttributeValue {
    Tree(AttributeTree),
    Type(McdocType),
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct AttributeTree {
    pub kind: TreeTag,
    pub values: BTreeMap<String, AttributeValue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TreeTag;

impl Serialize for TreeTag {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str("tree")
    }
}

impl<'de> Deserialize<'de> for TreeTag {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        if s == "tree" {
            Ok(TreeTag)
        } else {
            Err(de::Error::custom(format!("expected tree tag, got {s}")))
        }
    }
}

impl<'de> Deserialize<'de> for AttributeValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        if value.get("kind").and_then(|k| k.as_str()) == Some("tree") {
            let tree: AttributeTree = serde_json::from_value(value).map_err(de::Error::custom)?;
            Ok(AttributeValue::Tree(tree))
        } else {
            let ty: McdocType = serde_json::from_value(value).map_err(de::Error::custom)?;
            Ok(AttributeValue::Type(ty))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(transparent)]
pub struct RangeKind(pub u8);

impl RangeKind {
    pub const INCLUSIVE: Self = Self(0b00);
    pub const RIGHT_EXCLUSIVE: Self = Self(0b01);
    pub const LEFT_EXCLUSIVE: Self = Self(0b10);
    pub const EXCLUSIVE: Self = Self(0b11);

    pub fn is_left_exclusive(self) -> bool {
        (self.0 & 0b10) != 0
    }

    pub fn is_right_exclusive(self) -> bool {
        (self.0 & 0b01) != 0
    }

    pub fn delimiter(self) -> &'static str {
        match self.0 & 0b11 {
            0b00 => "..",
            0b01 => "..<",
            0b10 => "<..",
            _ => "<..<",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize)]
pub struct NumericRange {
    pub kind: RangeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
}

impl NumericRange {
    pub fn contains(&self, val: f64) -> bool {
        if let Some(min) = self.min {
            if self.kind.is_left_exclusive() {
                if val <= min {
                    return false;
                }
            } else if val < min {
                return false;
            }
        }
        if let Some(max) = self.max {
            if self.kind.is_right_exclusive() {
                if val >= max {
                    return false;
                }
            } else if val > max {
                return false;
            }
        }
        true
    }

    pub fn intersect(&self, other: &Self) -> Self {
        let min = match (self.min, other.min) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        let max = match (self.max, other.max) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let mut kind = 0u8;
        if (min == self.min && self.kind.is_left_exclusive()) || (min == other.min && other.kind.is_left_exclusive()) {
            kind |= 0b10;
        }
        if (max == self.max && self.kind.is_right_exclusive()) || (max == other.max && other.kind.is_right_exclusive()) {
            kind |= 0b01;
        }
        Self { kind: RangeKind(kind), min, max }
    }
}

impl fmt::Display for NumericRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.min == self.max && self.kind == RangeKind::INCLUSIVE {
            if let Some(v) = self.min {
                return write!(f, "{v}");
            }
            return f.write_str(self.kind.delimiter());
        }
        if let Some(min) = self.min {
            write!(f, "{min}")?;
        }
        f.write_str(self.kind.delimiter())?;
        if let Some(max) = self.max {
            write!(f, "{max}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct LongRange {
    pub kind: RangeKind,
    #[serde(default, deserialize_with = "de_opt_i64", skip_serializing_if = "Option::is_none")]
    pub min: Option<i64>,
    #[serde(default, deserialize_with = "de_opt_i64", skip_serializing_if = "Option::is_none")]
    pub max: Option<i64>,
}

impl LongRange {
    pub fn contains(&self, val: i64) -> bool {
        if let Some(min) = self.min {
            if self.kind.is_left_exclusive() {
                if val <= min {
                    return false;
                }
            } else if val < min {
                return false;
            }
        }
        if let Some(max) = self.max {
            if self.kind.is_right_exclusive() {
                if val >= max {
                    return false;
                }
            } else if val > max {
                return false;
            }
        }
        true
    }

    pub fn intersect(&self, other: &Self) -> Self {
        let min = match (self.min, other.min) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        let max = match (self.max, other.max) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let mut kind = 0u8;
        if (min == self.min && self.kind.is_left_exclusive()) || (min == other.min && other.kind.is_left_exclusive()) {
            kind |= 0b10;
        }
        if (max == self.max && self.kind.is_right_exclusive()) || (max == other.max && other.kind.is_right_exclusive()) {
            kind |= 0b01;
        }
        Self { kind: RangeKind(kind), min, max }
    }
}

impl fmt::Display for LongRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.min == self.max && self.kind == RangeKind::INCLUSIVE {
            if let Some(v) = self.min {
                return write!(f, "{v}");
            }
            return f.write_str(self.kind.delimiter());
        }
        if let Some(min) = self.min {
            write!(f, "{min}")?;
        }
        f.write_str(self.kind.delimiter())?;
        if let Some(max) = self.max {
            write!(f, "{max}")?;
        }
        Ok(())
    }
}

fn de_i64<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
    struct Visitor;
    impl<'de> de::Visitor<'de> for Visitor {
        type Value = i64;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("an integer or stringified 64-bit integer")
        }
        fn visit_i64<E: de::Error>(self, v: i64) -> Result<i64, E> {
            Ok(v)
        }
        fn visit_u64<E: de::Error>(self, v: u64) -> Result<i64, E> {
            i64::try_from(v).map_err(|_| E::custom(format!("i64 out of range: {v}")))
        }
        fn visit_f64<E: de::Error>(self, v: f64) -> Result<i64, E> {
            if v.fract() == 0.0 && v >= i64::MIN as f64 && v <= i64::MAX as f64 {
                Ok(v as i64)
            } else {
                Err(E::custom(format!("invalid i64: {v}")))
            }
        }
        fn visit_str<E: de::Error>(self, v: &str) -> Result<i64, E> {
            v.parse::<i64>().map_err(E::custom)
        }
    }
    deserializer.deserialize_any(Visitor)
}

fn de_opt_i64<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<i64>, D::Error> {
    let opt = Option::<serde_json::Value>::deserialize(deserializer)?;
    match opt {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(v) => de_i64(v).map(Some).map_err(de::Error::custom),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Index {
    Static { value: String },
    Dynamic { accessor: Vec<AccessorKey> },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum AccessorKey {
    Field(String),
    Keyword { keyword: DynamicKeyword },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DynamicKeyword {
    Key,
    Parent,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StructField {
    Pair {
        key: StructKey,
        #[serde(rename = "type")]
        field_type: McdocType,
        #[serde(default)]
        optional: bool,
        #[serde(default)]
        deprecated: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        desc: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attributes: Vec<Attribute>,
    },
    Spread {
        #[serde(rename = "type")]
        field_type: McdocType,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attributes: Vec<Attribute>,
    },
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum StructKey {
    Literal(String),
    Dynamic(McdocType),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnumKind {
    Byte,
    Short,
    Int,
    Long,
    Float,
    Double,
    String,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct EnumField {
    pub identifier: String,
    pub value: EnumValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desc: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attributes: Vec<Attribute>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum EnumValue {
    String(String),
    Int(i64),
    Float(f64),
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LiteralValue {
    Boolean { value: bool },
    String { value: String },
    Byte { value: f64 },
    Short { value: f64 },
    Int { value: f64 },
    Long {
        #[serde(deserialize_with = "de_i64")]
        value: i64,
    },
    Float { value: f64 },
    Double { value: f64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct TypeParam {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum McdocType {
    Any {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attributes: Vec<Attribute>,
    },
    Boolean {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attributes: Vec<Attribute>,
    },
    Unsafe {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attributes: Vec<Attribute>,
    },
    String {
        #[serde(rename = "lengthRange", default, skip_serializing_if = "Option::is_none")]
        length_range: Option<NumericRange>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attributes: Vec<Attribute>,
    },
    Literal {
        value: LiteralValue,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attributes: Vec<Attribute>,
    },
    Byte {
        #[serde(rename = "valueRange", default, skip_serializing_if = "Option::is_none")]
        value_range: Option<NumericRange>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attributes: Vec<Attribute>,
    },
    Short {
        #[serde(rename = "valueRange", default, skip_serializing_if = "Option::is_none")]
        value_range: Option<NumericRange>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attributes: Vec<Attribute>,
    },
    Int {
        #[serde(rename = "valueRange", default, skip_serializing_if = "Option::is_none")]
        value_range: Option<NumericRange>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attributes: Vec<Attribute>,
    },
    Long {
        #[serde(rename = "valueRange", default, skip_serializing_if = "Option::is_none")]
        value_range: Option<LongRange>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attributes: Vec<Attribute>,
    },
    Float {
        #[serde(rename = "valueRange", default, skip_serializing_if = "Option::is_none")]
        value_range: Option<NumericRange>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attributes: Vec<Attribute>,
    },
    Double {
        #[serde(rename = "valueRange", default, skip_serializing_if = "Option::is_none")]
        value_range: Option<NumericRange>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attributes: Vec<Attribute>,
    },
    ByteArray {
        #[serde(rename = "valueRange", default, skip_serializing_if = "Option::is_none")]
        value_range: Option<NumericRange>,
        #[serde(rename = "lengthRange", default, skip_serializing_if = "Option::is_none")]
        length_range: Option<NumericRange>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attributes: Vec<Attribute>,
    },
    IntArray {
        #[serde(rename = "valueRange", default, skip_serializing_if = "Option::is_none")]
        value_range: Option<NumericRange>,
        #[serde(rename = "lengthRange", default, skip_serializing_if = "Option::is_none")]
        length_range: Option<NumericRange>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attributes: Vec<Attribute>,
    },
    LongArray {
        #[serde(rename = "valueRange", default, skip_serializing_if = "Option::is_none")]
        value_range: Option<LongRange>,
        #[serde(rename = "lengthRange", default, skip_serializing_if = "Option::is_none")]
        length_range: Option<NumericRange>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attributes: Vec<Attribute>,
    },
    List {
        item: Box<McdocType>,
        #[serde(rename = "lengthRange", default, skip_serializing_if = "Option::is_none")]
        length_range: Option<NumericRange>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attributes: Vec<Attribute>,
    },
    Tuple {
        items: Vec<McdocType>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attributes: Vec<Attribute>,
    },
    Struct {
        fields: Vec<StructField>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attributes: Vec<Attribute>,
    },
    Enum {
        #[serde(rename = "enumKind", default)]
        enum_kind: Option<EnumKind>,
        values: Vec<EnumField>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attributes: Vec<Attribute>,
    },
    Union {
        members: Vec<McdocType>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attributes: Vec<Attribute>,
    },
    Reference {
        #[serde(default)]
        path: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attributes: Vec<Attribute>,
    },
    Dispatcher {
        registry: String,
        #[serde(rename = "parallelIndices")]
        parallel_indices: Vec<Index>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attributes: Vec<Attribute>,
    },
    Indexed {
        #[serde(rename = "parallelIndices")]
        parallel_indices: Vec<Index>,
        child: Box<McdocType>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attributes: Vec<Attribute>,
    },
    Template {
        child: Box<McdocType>,
        #[serde(rename = "typeParams")]
        type_params: Vec<TypeParam>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attributes: Vec<Attribute>,
    },
    Concrete {
        child: Box<McdocType>,
        #[serde(rename = "typeArgs")]
        type_args: Vec<McdocType>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attributes: Vec<Attribute>,
    },
    Mapped {
        child: Box<McdocType>,
        mapping: BTreeMap<String, McdocType>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attributes: Vec<Attribute>,
    },
}

impl McdocType {
    pub fn empty_union() -> Self {
        Self::Union { members: Vec::new(), attributes: Vec::new() }
    }

    /// Converts stringified 64-bit integer values inside `long` enums (emitted when JS BigInts are
    /// serialized to JSON) from `EnumValue::String` to `EnumValue::Int`.
    pub fn normalize_long_enums(&mut self) {
        match self {
            Self::Enum { enum_kind: Some(EnumKind::Long), values, .. } => {
                for field in values {
                    if let EnumValue::String(s) = &field.value
                        && let Ok(parsed) = s.parse::<i64>()
                    {
                        field.value = EnumValue::Int(parsed);
                    }
                }
            }
            Self::List { item, .. } => item.normalize_long_enums(),
            Self::Tuple { items, .. } => {
                for item in items {
                    item.normalize_long_enums();
                }
            }
            Self::Struct { fields, .. } => {
                for field in fields {
                    match field {
                        StructField::Pair { key, field_type, .. } => {
                            if let StructKey::Dynamic(kty) = key {
                                kty.normalize_long_enums();
                            }
                            field_type.normalize_long_enums();
                        }
                        StructField::Spread { field_type, .. } => field_type.normalize_long_enums(),
                    }
                }
            }
            Self::Union { members, .. } => {
                for member in members {
                    member.normalize_long_enums();
                }
            }
            Self::Indexed { child, .. } | Self::Template { child, .. } => child.normalize_long_enums(),
            Self::Concrete { child, type_args, .. } => {
                child.normalize_long_enums();
                for arg in type_args {
                    arg.normalize_long_enums();
                }
            }
            Self::Mapped { child, mapping, .. } => {
                child.normalize_long_enums();
                for val in mapping.values_mut() {
                    val.normalize_long_enums();
                }
            }
            _ => {}
        }
    }

    pub fn attributes(&self) -> &[Attribute] {
        match self {
            Self::Any { attributes }
            | Self::Boolean { attributes }
            | Self::Unsafe { attributes }
            | Self::String { attributes, .. }
            | Self::Literal { attributes, .. }
            | Self::Byte { attributes, .. }
            | Self::Short { attributes, .. }
            | Self::Int { attributes, .. }
            | Self::Long { attributes, .. }
            | Self::Float { attributes, .. }
            | Self::Double { attributes, .. }
            | Self::ByteArray { attributes, .. }
            | Self::IntArray { attributes, .. }
            | Self::LongArray { attributes, .. }
            | Self::List { attributes, .. }
            | Self::Tuple { attributes, .. }
            | Self::Struct { attributes, .. }
            | Self::Enum { attributes, .. }
            | Self::Union { attributes, .. }
            | Self::Reference { attributes, .. }
            | Self::Dispatcher { attributes, .. }
            | Self::Indexed { attributes, .. }
            | Self::Template { attributes, .. }
            | Self::Concrete { attributes, .. }
            | Self::Mapped { attributes, .. } => attributes,
        }
    }

    pub fn attributes_mut(&mut self) -> &mut Vec<Attribute> {
        match self {
            Self::Any { attributes }
            | Self::Boolean { attributes }
            | Self::Unsafe { attributes }
            | Self::String { attributes, .. }
            | Self::Literal { attributes, .. }
            | Self::Byte { attributes, .. }
            | Self::Short { attributes, .. }
            | Self::Int { attributes, .. }
            | Self::Long { attributes, .. }
            | Self::Float { attributes, .. }
            | Self::Double { attributes, .. }
            | Self::ByteArray { attributes, .. }
            | Self::IntArray { attributes, .. }
            | Self::LongArray { attributes, .. }
            | Self::List { attributes, .. }
            | Self::Tuple { attributes, .. }
            | Self::Struct { attributes, .. }
            | Self::Enum { attributes, .. }
            | Self::Union { attributes, .. }
            | Self::Reference { attributes, .. }
            | Self::Dispatcher { attributes, .. }
            | Self::Indexed { attributes, .. }
            | Self::Template { attributes, .. }
            | Self::Concrete { attributes, .. }
            | Self::Mapped { attributes, .. } => attributes,
        }
    }

    pub fn with_attributes(mut self, attrs: Vec<Attribute>) -> Self {
        *self.attributes_mut() = attrs;
        self
    }

    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Any { .. } => "any",
            Self::Boolean { .. } => "boolean",
            Self::Unsafe { .. } => "unsafe",
            Self::String { .. } => "string",
            Self::Literal { .. } => "literal",
            Self::Byte { .. } => "byte",
            Self::Short { .. } => "short",
            Self::Int { .. } => "int",
            Self::Long { .. } => "long",
            Self::Float { .. } => "float",
            Self::Double { .. } => "double",
            Self::ByteArray { .. } => "byte_array",
            Self::IntArray { .. } => "int_array",
            Self::LongArray { .. } => "long_array",
            Self::List { .. } => "list",
            Self::Tuple { .. } => "tuple",
            Self::Struct { .. } => "struct",
            Self::Enum { .. } => "enum",
            Self::Union { .. } => "union",
            Self::Reference { .. } => "reference",
            Self::Dispatcher { .. } => "dispatcher",
            Self::Indexed { .. } => "indexed",
            Self::Template { .. } => "template",
            Self::Concrete { .. } => "concrete",
            Self::Mapped { .. } => "mapped",
        }
    }
}

fn fmt_indices(f: &mut fmt::Formatter<'_>, indices: &[Index]) -> fmt::Result {
    f.write_str("[")?;
    for (i, idx) in indices.iter().enumerate() {
        if i > 0 {
            f.write_str(", ")?;
        }
        match idx {
            Index::Static { value } => write!(f, "[{value}]")?,
            Index::Dynamic { accessor } => {
                f.write_str("[[")?;
                for (j, part) in accessor.iter().enumerate() {
                    if j > 0 {
                        f.write_str(".")?;
                    }
                    match part {
                        AccessorKey::Field(name) => f.write_str(name)?,
                        AccessorKey::Keyword { keyword: DynamicKeyword::Key } => f.write_str("key")?,
                        AccessorKey::Keyword { keyword: DynamicKeyword::Parent } => f.write_str("parent")?,
                    }
                }
                f.write_str("]]")?;
            }
        }
    }
    f.write_str("]")
}

impl fmt::Display for McdocType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for attr in self.attributes() {
            if attr.value.is_some() {
                write!(f, "#[{}=<value ...>] ", attr.name)?;
            } else {
                write!(f, "#[{}] ", attr.name)?;
            }
        }
        match self {
            Self::Any { .. } => f.write_str("any"),
            Self::Boolean { .. } => f.write_str("boolean"),
            Self::Unsafe { .. } => f.write_str("unsafe"),
            Self::String { length_range, .. } => {
                f.write_str("string")?;
                if let Some(r) = length_range {
                    write!(f, " @ {r}")?;
                }
                Ok(())
            }
            Self::Literal { value, .. } => match value {
                LiteralValue::Boolean { value } => write!(f, "{value}"),
                LiteralValue::String { value } => write!(f, "\"{value}\""),
                LiteralValue::Byte { value }
                | LiteralValue::Short { value }
                | LiteralValue::Int { value }
                | LiteralValue::Float { value }
                | LiteralValue::Double { value } => write!(f, "{value}"),
                LiteralValue::Long { value } => write!(f, "{value}"),
            },
            Self::Byte { value_range, .. }
            | Self::Short { value_range, .. }
            | Self::Int { value_range, .. }
            | Self::Float { value_range, .. }
            | Self::Double { value_range, .. } => {
                f.write_str(self.kind_name())?;
                if let Some(r) = value_range {
                    write!(f, " @ {r}")?;
                }
                Ok(())
            }
            Self::Long { value_range, .. } => {
                f.write_str("long")?;
                if let Some(r) = value_range {
                    write!(f, " @ {r}")?;
                }
                Ok(())
            }
            Self::ByteArray { value_range, length_range, .. } | Self::IntArray { value_range, length_range, .. } => {
                let elem = if matches!(self, Self::ByteArray { .. }) { "byte" } else { "int" };
                f.write_str(elem)?;
                if let Some(r) = value_range {
                    write!(f, " @ {r}")?;
                }
                f.write_str("[]")?;
                if let Some(r) = length_range {
                    write!(f, " @ {r}")?;
                }
                Ok(())
            }
            Self::LongArray { value_range, length_range, .. } => {
                f.write_str("long")?;
                if let Some(r) = value_range {
                    write!(f, " @ {r}")?;
                }
                f.write_str("[]")?;
                if let Some(r) = length_range {
                    write!(f, " @ {r}")?;
                }
                Ok(())
            }
            Self::List { item, length_range, .. } => {
                write!(f, "[{item}]")?;
                if let Some(r) = length_range {
                    write!(f, " @ {r}")?;
                }
                Ok(())
            }
            Self::Tuple { items, .. } => {
                f.write_str("[")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{item}")?;
                }
                if items.len() == 1 {
                    f.write_str(",")?;
                }
                f.write_str("]")
            }
            Self::Struct { .. } => f.write_str("<struct ...>"),
            Self::Enum { .. } => f.write_str("<enum ...>"),
            Self::Union { members, .. } => {
                f.write_str("(")?;
                for (i, m) in members.iter().enumerate() {
                    if i > 0 {
                        f.write_str(" | ")?;
                    }
                    write!(f, "{m}")?;
                }
                f.write_str(")")
            }
            Self::Reference { path, .. } => f.write_str(path.as_deref().unwrap_or("<unknown_reference>")),
            Self::Dispatcher { registry, parallel_indices, .. } => {
                f.write_str(registry)?;
                fmt_indices(f, parallel_indices)
            }
            Self::Indexed { parallel_indices, child, .. } => {
                write!(f, "{child}")?;
                fmt_indices(f, parallel_indices)
            }
            Self::Template { child, type_params, .. } => {
                write!(f, "{child}")?;
                if !type_params.is_empty() {
                    f.write_str("<")?;
                    for (i, p) in type_params.iter().enumerate() {
                        if i > 0 {
                            f.write_str(", ")?;
                        }
                        write!(f, "?{}", p.path)?;
                    }
                    f.write_str(">")?;
                }
                Ok(())
            }
            Self::Concrete { child, type_args, .. } => {
                write!(f, "{child}")?;
                if !type_args.is_empty() {
                    f.write_str("<")?;
                    for (i, a) in type_args.iter().enumerate() {
                        if i > 0 {
                            f.write_str(", ")?;
                        }
                        write!(f, "{a}")?;
                    }
                    f.write_str(">")?;
                }
                Ok(())
            }
            Self::Mapped { child, .. } => write!(f, "{child}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_symbol_table_subset() {
        let raw = r#"{
            "ref": "abc123",
            "mcdoc": {
                "::java::example::Test": {
                    "kind": "struct",
                    "fields": [
                        {
                            "kind": "pair",
                            "key": "item",
                            "type": {
                                "kind": "string",
                                "attributes": [
                                    {
                                        "name": "id",
                                        "value": {
                                            "kind": "tree",
                                            "values": {
                                                "registry": { "kind": "literal", "value": { "kind": "string", "value": "item" } },
                                                "tags": { "kind": "literal", "value": { "kind": "string", "value": "allowed" } }
                                            }
                                        }
                                    }
                                ]
                            },
                            "optional": true,
                            "desc": "The item ID."
                        },
                        {
                            "kind": "spread",
                            "type": {
                                "kind": "dispatcher",
                                "registry": "minecraft:recipe_serializer",
                                "parallelIndices": [
                                    { "kind": "dynamic", "accessor": [{ "keyword": "parent" }, "type"] }
                                ]
                            }
                        }
                    ]
                }
            },
            "mcdoc/dispatcher": {
                "minecraft:resource": {
                    "recipe": { "kind": "reference", "path": "::java::example::Test" }
                }
            }
        }"#;
        let tables: SymbolTables = serde_json::from_str(raw).unwrap();
        assert_eq!(tables.git_ref.as_deref(), Some("abc123"));
        assert_eq!(tables.mcdoc.len(), 1);
        assert_eq!(tables.dispatchers["minecraft:resource"].len(), 1);
    }

    #[test]
    fn numeric_and_long_ranges_check_bounds() {
        let r = NumericRange { kind: RangeKind::RIGHT_EXCLUSIVE, min: Some(0.0), max: Some(10.0) };
        assert!(r.contains(0.0));
        assert!(r.contains(9.5));
        assert!(!r.contains(10.0));
        assert!(!r.contains(-0.1));
        assert_eq!(r.to_string(), "0..<10");

        let lr: LongRange = serde_json::from_str(r#"{"kind":0,"min":-2,"max":"9223372036854775807"}"#).unwrap();
        assert!(lr.contains(-2));
        assert!(lr.contains(i64::MAX));
        assert!(!lr.contains(-3));
    }

    #[test]
    fn normalizes_stringified_long_enum_values() {
        let raw = r#"{
            "mcdoc": {
                "::test::BigEnum": {
                    "kind": "enum",
                    "enumKind": "long",
                    "values": [
                        { "identifier": "Max", "value": "9223372036854775807" }
                    ]
                },
                "::test::StrEnum": {
                    "kind": "enum",
                    "enumKind": "string",
                    "values": [
                        { "identifier": "NumStr", "value": "42" }
                    ]
                }
            }
        }"#;
        let tables: SymbolTables = serde_json::from_str(raw).unwrap();
        let McdocType::Enum { values, .. } = &tables.mcdoc["::test::BigEnum"] else { panic!() };
        assert_eq!(values[0].value, EnumValue::Int(i64::MAX));
        let McdocType::Enum { values, .. } = &tables.mcdoc["::test::StrEnum"] else { panic!() };
        assert_eq!(values[0].value, EnumValue::String("42".into()));
    }
}
