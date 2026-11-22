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
    match attr.value.as_ref()? {
        AttributeValue::Type(McdocType::Literal { value: LiteralValue::String { value }, .. }) => Some(value.as_str()),
        _ => None,
    }
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
        AttributeValue::Type(McdocType::Literal { value: LiteralValue::String { value }, .. }) => Some(IdConfig {
            registry: Some(value.clone()),
            ..Default::default()
        }),
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
                        AttributeValue::Type(McdocType::Literal { value: LiteralValue::String { value }, .. }) => {
                            cfg.exclude.push(value.clone());
                        }
                        AttributeValue::Tree(list) => {
                            for item in list.values.values() {
                                if let Some(s) = val_as_string(item) {
                                    cfg.exclude.push(s.to_string());
                                }
                            }
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
            Some(cfg)
        }
        _ => Some(IdConfig::default()),
    }
}

fn val_as_string(val: &AttributeValue) -> Option<&str> {
    match val {
        AttributeValue::Type(McdocType::Literal { value: LiteralValue::String { value }, .. }) => Some(value.as_str()),
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
