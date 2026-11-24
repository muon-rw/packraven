#![allow(dead_code)]

use std::cmp::Ordering;

use super::types::{Attribute, AttributeValue, LiteralValue, McdocType};

#[derive(Debug, Clone, Default)]
pub struct VersionContext {
    pub minecraft: Option<String>,
    pub neoforge: Option<String>,
}

impl VersionContext {
    /// Returns whether an element with `attributes` applies to the active Minecraft and NeoForge versions.
    pub fn keeps(&self, attributes: &[Attribute]) -> bool {
        for attr in attributes {
            match attr.name.as_str() {
                "since" => {
                    if let (Some(current), Some(target)) = (self.minecraft.as_deref(), attr_string(attr))
                        && compare_version(current, target) == Ordering::Less
                    {
                        return false;
                    }
                }
                "until" => {
                    if let (Some(current), Some(target)) = (self.minecraft.as_deref(), attr_string(attr))
                        && compare_version(current, target) != Ordering::Less
                    {
                        return false;
                    }
                }
                "neoforge_since" => {
                    if let (Some(current), Some(target)) = (self.neoforge.as_deref(), attr_string(attr))
                        && compare_version(current, target) == Ordering::Less
                    {
                        return false;
                    }
                }
                "neoforge_until" => {
                    if let (Some(current), Some(target)) = (self.neoforge.as_deref(), attr_string(attr))
                        && compare_version(current, target) != Ordering::Less
                    {
                        return false;
                    }
                }
                _ => {}
            }
        }
        true
    }

    pub fn is_deprecated(&self, attributes: &[Attribute]) -> bool {
        for attr in attributes {
            if attr.name == "deprecated" {
                return match (self.minecraft.as_deref(), attr_string(attr)) {
                    (Some(current), Some(since)) => compare_version(current, since) != Ordering::Less,
                    _ => true,
                };
            }
        }
        false
    }
}

pub fn attr_string(attr: &Attribute) -> Option<&str> {
    val_as_string(attr.value.as_ref()?)
}

pub fn attr_number(attr: &Attribute) -> Option<f64> {
    match attr.value.as_ref()? {
        AttributeValue::Type(McdocType::Literal { value, .. }) => match value {
            LiteralValue::Byte { value }
            | LiteralValue::Short { value }
            | LiteralValue::Int { value }
            | LiteralValue::Float { value }
            | LiteralValue::Double { value } => Some(*value),
            LiteralValue::Long { value } => Some(*value as f64),
            _ => None,
        },
        _ => None,
    }
}

/// Compares dotted numeric versions (`1.21.1` vs `1.21.11`, `1.21.11` vs `26.1`, `21.4.20-beta` vs `21.4.20`).
pub fn compare_version(a: &str, b: &str) -> Ordering {
    let pa = parse_version_parts(a);
    let pb = parse_version_parts(b);
    let len = pa.len().max(pb.len());
    for i in 0..len {
        let va = pa.get(i).copied().unwrap_or(0);
        let vb = pb.get(i).copied().unwrap_or(0);
        match va.cmp(&vb) {
            Ordering::Equal => continue,
            ord => return ord,
        }
    }
    Ordering::Equal
}

fn parse_version_parts(v: &str) -> Vec<u32> {
    // Strip pre-release/snapshot suffix (`-beta`, `-pre1`, `+...`) for numeric segment comparison.
    let core = v.split(['-', '+']).next().unwrap_or(v);
    core.split('.')
        .filter_map(|part| part.parse::<u32>().ok())
        .collect()
}

#[derive(Debug, Clone, Default)]
pub struct IdConfig {
    pub registry: Option<String>,
    pub tags: Option<TagMode>,
    pub definition: bool,
    pub prefix: Option<String>,
    pub path: Option<String>,
    pub empty_allowed: bool,
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagMode {
    Allowed,
    Implicit,
    Required,
}

pub fn parse_id_attribute(attr: &Attribute) -> Option<IdConfig> {
    if attr.name != "id" {
        return None;
    }
    let Some(val) = &attr.value else {
        return Some(IdConfig::default());
    };
    match val {
        AttributeValue::Type(ty) => match ty_as_string(ty) {
            Some(s) => Some(IdConfig { registry: Some(s.to_string()), ..Default::default() }),
            None => Some(IdConfig::default()),
        },
        AttributeValue::Tree(tree) => {
            let mut cfg = IdConfig::default();
            for (k, v) in &tree.values {
                match k.as_str() {
                    "registry" => cfg.registry = val_as_string(v).map(String::from),
                    "tags" => {
                        cfg.tags = match val_as_string(v) {
                            Some("allowed") => Some(TagMode::Allowed),
                            Some("implicit") => Some(TagMode::Implicit),
                            Some("required") => Some(TagMode::Required),
                            _ => None,
                        };
                    }
                    "definition" => cfg.definition = val_as_bool(v).unwrap_or(false),
                    "prefix" => cfg.prefix = val_as_string(v).map(String::from),
                    "path" => cfg.path = val_as_string(v).map(String::from),
                    "empty" => cfg.empty_allowed = val_as_string(v) == Some("allowed"),
                    "exclude" => match v {
                        AttributeValue::Tree(list) => {
                            for item in list.values.values() {
                                if let Some(s) = val_as_string(item) {
                                    cfg.exclude.push(s.to_string());
                                }
                            }
                        }
                        other => {
                            if let Some(s) = val_as_string(other) {
                                cfg.exclude.push(s.to_string());
                            }
                        }
                    },
                    _ => {}
                }
            }
            Some(cfg)
        }
    }
}

fn ty_as_string(ty: &McdocType) -> Option<&str> {
    match ty {
        McdocType::Literal { value: LiteralValue::String { value }, .. } => Some(value.as_str()),
        // Spyglass `validator.string` also accepts bare identifier references (like `#[id(tags=allowed)]`),
        // stripping any bound module prefix.
        McdocType::Reference { path: Some(path), .. } => Some(path.rsplit("::").next().unwrap_or(path.as_str())),
        _ => None,
    }
}

fn val_as_string(val: &AttributeValue) -> Option<&str> {
    match val {
        AttributeValue::Type(ty) => ty_as_string(ty),
        _ => None,
    }
}

fn val_as_bool(val: &AttributeValue) -> Option<bool> {
    match val {
        AttributeValue::Type(McdocType::Literal { value: LiteralValue::Boolean { value }, .. }) => Some(*value),
        _ => None,
    }
}

fn val_as_i64(val: &AttributeValue) -> Option<i64> {
    match val {
        AttributeValue::Type(McdocType::Literal { value, .. }) => match value {
            LiteralValue::Long { value } => Some(*value),
            LiteralValue::Byte { value }
            | LiteralValue::Short { value }
            | LiteralValue::Int { value }
            | LiteralValue::Float { value }
            | LiteralValue::Double { value } => Some(*value as i64),
            _ => None,
        },
        _ => None,
    }
}

#[derive(Debug, Clone, Default)]
pub struct IntegerConfig {
    pub min: Option<i64>,
    pub max: Option<i64>,
}

pub fn parse_integer_attribute(attr: &Attribute) -> Option<IntegerConfig> {
    if attr.name != "integer" {
        return None;
    }
    let Some(val) = &attr.value else {
        return Some(IntegerConfig::default());
    };
    let mut cfg = IntegerConfig::default();
    if let AttributeValue::Tree(tree) = val {
        for (k, v) in &tree.values {
            match k.as_str() {
                "min" => cfg.min = val_as_i64(v),
                "max" => cfg.max = val_as_i64(v),
                _ => {}
            }
        }
    }
    Some(cfg)
}

pub fn is_weekly_snapshot(v: &str) -> bool {
    let bytes = v.as_bytes();
    let Some(w) = bytes.iter().position(|&b| b == b'w') else { return false };
    w >= 2
        && bytes[..w].iter().all(u8::is_ascii_digit)
        && bytes.len() >= w + 3
        && bytes[w + 1..bytes.len() - 1].iter().all(u8::is_ascii_digit)
        && bytes[bytes.len() - 1].is_ascii_lowercase()
}

pub fn parse_game_rule_attribute(attr: &Attribute) -> Option<&str> {
    if attr.name != "game_rule" {
        return None;
    }
    let AttributeValue::Tree(tree) = attr.value.as_ref()? else { return None };
    val_as_string(tree.values.get("type")?)
}

/// Vanilla's `SlotRanges` names, which every `#[item_slots]` field reads (they all exist from 1.20.5 on).
/// `(name, count, since, until)`, where a count expands to `name.0` through `name.<count - 1>` plus `name.*`.
/// TODO: Something like this should reside in vanilla mcdoc. Spyglass is currently hardcoding similar data.
const ITEM_SLOTS: &[(&str, Option<u8>, Option<&str>, Option<&str>)] = &[
    ("contents", None, None, None),
    ("container", Some(54), None, None),
    ("hotbar", Some(9), None, None),
    ("inventory", Some(27), None, None),
    ("enderchest", Some(27), None, None),
    ("villager", Some(8), None, Some("26.1")),
    ("mob.inventory", Some(8), Some("26.1"), None),
    ("horse", Some(15), None, None),
    ("weapon", None, None, None),
    ("weapon.mainhand", None, None, None),
    ("weapon.offhand", None, None, None),
    ("weapon.*", None, None, None),
    ("armor.head", None, None, None),
    ("armor.chest", None, None, None),
    ("armor.legs", None, None, None),
    ("armor.feet", None, None, None),
    ("armor.body", None, None, None),
    ("armor.*", None, None, None),
    ("horse.saddle", None, None, Some("1.21.5")),
    ("saddle", None, Some("1.21.5"), None),
    ("horse.chest", None, None, None),
    ("player.cursor", None, None, None),
    ("player.crafting", Some(4), None, None),
];

/// Returns the valid `#[item_slots]` values for `minecraft`, or the newest version's without one.
pub fn item_slots_for_version(minecraft: Option<&str>) -> Vec<String> {
    let at_least = |target: &str| minecraft.is_none_or(|mc| compare_version(mc, target) != Ordering::Less);
    let mut out = Vec::with_capacity(180);
    for &(name, count, since, until) in ITEM_SLOTS {
        if since.is_some_and(|v| !at_least(v)) || until.is_some_and(at_least) {
            continue;
        }
        match count {
            Some(count) => {
                out.extend((0..count).map(|n| format!("{name}.{n}")));
                out.push(format!("{name}.*"));
            }
            None => out.push(name.to_string()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_item_slots_like_vanilla() {
        let slots = |mc| item_slots_for_version(Some(mc));
        let mid = slots("1.21.1");
        assert!(mid.contains(&"container.*".into()) && mid.contains(&"horse.saddle".into()) && mid.contains(&"villager.7".into()));
        assert!(!mid.contains(&"saddle".into()) && !mid.contains(&"mob.inventory.0".into()));
        let new = slots("26.1.2");
        assert!(new.contains(&"mob.inventory.*".into()) && new.contains(&"saddle".into()) && new.contains(&"armor.*".into()));
        assert!(!new.iter().any(|s| s.starts_with("villager.") || s == "horse.saddle"));
        assert_eq!(new.len(), 1 + 55 + 10 + 28 + 28 + 9 + 16 + 4 + 6 + 1 + 1 + 1 + 5);
    }

    #[test]
    fn compares_minecraft_and_neoforge_versions() {
        assert_eq!(compare_version("1.21.1", "1.21.11"), Ordering::Less);
        assert_eq!(compare_version("1.21.11", "26.1"), Ordering::Less);
        assert_eq!(compare_version("26.2", "26.2.0"), Ordering::Equal);
        assert_eq!(compare_version("21.1.265", "21.4.20"), Ordering::Less);
        assert_eq!(compare_version("21.4.20-beta", "21.4.20"), Ordering::Equal);
    }

    #[test]
    fn filters_since_until_and_neoforge_gates() {
        let ctx = VersionContext {
            minecraft: Some("1.21.1".into()),
            neoforge: Some("21.1.265".into()),
        };
        let attr = |name: &str, val: &str| Attribute {
            name: name.into(),
            value: Some(AttributeValue::Type(McdocType::Literal {
                value: LiteralValue::String { value: val.into() },
                attributes: Vec::new(),
            })),
        };
        assert!(ctx.keeps(&[attr("since", "1.20.5"), attr("until", "1.21.4")]));
        assert!(!ctx.keeps(&[attr("since", "1.21.4")]));
        assert!(ctx.keeps(&[attr("neoforge_until", "21.4.20")]));
        assert!(!ctx.keeps(&[attr("neoforge_since", "21.4.20")]));
    }

    #[test]
    fn parses_bare_identifier_attribute_values() {
        let mut tables = crate::mcdoc::types::SymbolTables::default();
        crate::mcdoc::parser::parse_and_bind(
            r#"
            struct Sample {
                a: #[id=item] string,
                b: #[id(registry=item, tags=allowed, empty=allowed, exclude=[air])] string,
                c: #[color=hex_rgb] string,
            }
            "#,
            "::test",
            &mut tables,
        )
        .unwrap();
        let McdocType::Struct { fields, .. } = &tables.mcdoc["::test::Sample"] else { panic!() };
        let field_attr = |idx: usize| match &fields[idx] {
            crate::mcdoc::types::StructField::Pair { field_type, .. } => &field_type.attributes()[0],
            _ => panic!(),
        };
        let a = parse_id_attribute(field_attr(0)).unwrap();
        assert_eq!(a.registry.as_deref(), Some("item"));
        let b = parse_id_attribute(field_attr(1)).unwrap();
        assert_eq!(b.registry.as_deref(), Some("item"));
        assert_eq!(b.tags, Some(TagMode::Allowed));
        assert!(b.empty_allowed);
        assert_eq!(b.exclude, vec!["air"]);
        assert_eq!(attr_string(field_attr(2)), Some("hex_rgb"));
    }
}
