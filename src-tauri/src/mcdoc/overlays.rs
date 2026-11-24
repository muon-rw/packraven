//! Works out which copy of a data file Minecraft loads when a pack lists overlays in its
//! `pack.mcmeta`: version-specific folders (`f101-2026-1/data/...`) that vanilla layers over the
//! pack's own `data/` from 1.20.2 on, plus NeoForge's `neoforge:overlays` and Fabric API's
//! `fabric:overlays`, which can depend on load conditions.

use std::cmp::Ordering;
use std::collections::HashSet;

use super::json_parser::{self, JsonKind, JsonNode};
use super::version::compare_version;

/// A data pack format. Minecraft 1.21.9 added minor versions; older versions have only a major.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PackFormat {
    pub major: u32,
    pub minor: u32,
}

/// The last data pack format before minor versions (`PackFormat.lastPreMinorVersion`), after which
/// vanilla reads overlay ranges from `min_format` and `max_format`.
const LAST_PRE_MINOR_DATA_FORMAT: u32 = 81;

/// What the pack's game, loader, and mods mean for overlays.
pub struct OverlayEnv<'a> {
    pub minecraft: &'a str,
    pub format: PackFormat,
    pub neoforge: bool,
    pub fabric_api: bool,
    pub mods: &'a HashSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileLoad {
    Loaded,
    /// An overlay Minecraft applies has its own copy of this file, at the pack-relative path `by`.
    Replaced { overlay: String, by: String },
    /// The file sits in an overlay Minecraft doesn't apply.
    OverlayInactive { overlay: String },
    /// The file sits in a folder of the pack that `pack.mcmeta` doesn't list as an overlay.
    Unlisted { folder: String },
}

/// Reads files relative to a pack's root (a folder, a datapack zip, or a mod jar).
pub trait PackFiles {
    fn read(&self, relative: &str) -> Option<String>;
    fn exists(&self, relative: &str) -> bool;
}

/// Reads pack files under the instance folder, including inside mod jars and datapack zips.
pub struct InstanceFiles<'a>(pub &'a std::path::Path);

impl PackFiles for InstanceFiles<'_> {
    fn read(&self, relative: &str) -> Option<String> {
        let bytes = match crate::archive::split(self.0, relative) {
            Some((archive, inner)) => crate::archive::read(&archive, &inner, MAX_MCMETA_BYTES).ok()?,
            None => std::fs::read(self.0.join(relative)).ok()?,
        };
        String::from_utf8(bytes).ok()
    }

    fn exists(&self, relative: &str) -> bool {
        match crate::archive::split(self.0, relative) {
            Some((archive, inner)) => crate::archive::size(&archive, &inner).is_ok(),
            None => self.0.join(relative).is_file(),
        }
    }
}

const MAX_MCMETA_BYTES: u64 = 1024 * 1024;

/// Decides how Minecraft treats `path` (a file under some pack, like `mods/x.jar/data/...`) by
/// finding the pack's `pack.mcmeta`: either just above `data/`, or one folder higher when the file
/// sits in one of that pack's overlays.
pub fn file_load(path: &str, files: &dyn PackFiles, env: &OverlayEnv<'_>) -> FileLoad {
    if compare_version(env.minecraft, "1.20.2") == Ordering::Less {
        return FileLoad::Loaded;
    }
    let parts: Vec<&str> = path.split('/').collect();
    let join = |range: &[&str]| range.join("/");
    let mcmeta_at = |root: &[&str]| {
        let rel = if root.is_empty() { "pack.mcmeta".to_string() } else { format!("{}/pack.mcmeta", join(root)) };
        files.read(&rel).and_then(|text| json_parser::parse(&text, true).root)
    };
    for (data, part) in parts.iter().enumerate() {
        if *part != "data" && *part != "assets" {
            continue;
        }
        if let Some(meta) = mcmeta_at(&parts[..data]) {
            return resolve(&meta, None, &join(&parts[data..]), &parts[..data], files, env);
        }
        if data >= 1
            && let Some(meta) = mcmeta_at(&parts[..data - 1])
        {
            let folder = parts[data - 1];
            if !listed_overlays(&meta).contains(folder) {
                return FileLoad::Unlisted { folder: folder.to_string() };
            }
            return resolve(&meta, Some(folder), &join(&parts[data..]), &parts[..data - 1], files, env);
        }
    }
    FileLoad::Loaded
}

fn resolve(meta: &JsonNode, overlay: Option<&str>, inner: &str, root: &[&str], files: &dyn PackFiles, env: &OverlayEnv<'_>) -> FileLoad {
    let active = active_overlays(meta, env);
    if let Some(dir) = overlay
        && !active.iter().any(|a| a == dir)
    {
        return FileLoad::OverlayInactive { overlay: dir.to_string() };
    }
    let prefix = if root.is_empty() { String::new() } else { format!("{}/", root.join("/")) };
    // Vanilla stacks the last applied overlay on top, then the earlier ones, then the base pack.
    for dir in active.iter().rev() {
        if Some(dir.as_str()) == overlay {
            break;
        }
        let candidate = format!("{prefix}{dir}/{inner}");
        if files.exists(&candidate) {
            return FileLoad::Replaced { overlay: dir.clone(), by: candidate };
        }
    }
    FileLoad::Loaded
}

fn sections(meta: &JsonNode) -> impl Iterator<Item = (&'static str, &JsonNode)> {
    ["overlays", "neoforge:overlays", "fabric:overlays"].into_iter().flat_map(move |key| {
        let entries = meta.get(key).and_then(|section| section.get("entries"));
        let items: &[JsonNode] = match entries.map(|e| &e.kind) {
            Some(JsonKind::Array(items)) => items,
            _ => &[],
        };
        items.iter().map(move |entry| (key, entry))
    })
}

fn listed_overlays(meta: &JsonNode) -> HashSet<&str> {
    sections(meta).filter_map(|(_, entry)| entry.get("directory")?.as_str()).collect()
}

/// Returns the overlays Minecraft applies, lowest priority first: vanilla's list, then
/// `neoforge:overlays` (NeoForge's `Pack.readPackMetadata`), then `fabric:overlays` (Fabric API's
/// `ResourcePackProfileMixin`).
pub fn active_overlays(meta: &JsonNode, env: &OverlayEnv<'_>) -> Vec<String> {
    sections(meta)
        .filter(|(section, entry)| match *section {
            "overlays" => applies_to_format(entry, env.format) && (!env.neoforge || neoforge_conditions_hold(entry, env.mods)),
            "neoforge:overlays" => env.neoforge && applies_to_format(entry, env.format) && neoforge_conditions_hold(entry, env.mods),
            _ => env.fabric_api && entry.get("condition").is_some_and(|c| fabric_condition(c, env.mods) != Some(false)),
        })
        .filter_map(|(_, entry)| entry.get("directory")?.as_str().map(str::to_string))
        .collect()
}

// From 1.21.9, vanilla reads `min_format`/`max_format` (each a major or `[major, minor]`) when both
// are present, and otherwise the older `formats` range of majors. Before that, only `formats`.
fn applies_to_format(entry: &JsonNode, format: PackFormat) -> bool {
    if format.major > LAST_PRE_MINOR_DATA_FORMAT
        && let (Some(min), Some(max)) = (entry.get("min_format"), entry.get("max_format"))
    {
        return match (pack_format(min, 0), pack_format(max, u32::MAX)) {
            (Some(min), Some(max)) => min <= format && format <= max,
            _ => false,
        };
    }
    match entry.get("formats").and_then(major_range) {
        Some((min, max)) => min <= format.major && format.major <= max,
        None => false,
    }
}

fn pack_format(node: &JsonNode, default_minor: u32) -> Option<PackFormat> {
    match &node.kind {
        JsonKind::Array(items) => Some(PackFormat { major: whole(items.first()?)?, minor: items.get(1).map_or(Some(default_minor), whole)? }),
        _ => Some(PackFormat { major: whole(node)?, minor: default_minor }),
    }
}

/// Reads vanilla's `InclusiveRange` codec: one number, `[min, max]`, or `{min_inclusive, max_inclusive}`.
fn major_range(node: &JsonNode) -> Option<(u32, u32)> {
    match &node.kind {
        JsonKind::Array(items) if items.len() == 2 => Some((whole(&items[0])?, whole(&items[1])?)),
        JsonKind::Object(_) => Some((whole(node.get("min_inclusive")?)?, whole(node.get("max_inclusive")?)?)),
        _ => whole(node).map(|n| (n, n)),
    }
}

fn whole(node: &JsonNode) -> Option<u32> {
    match &node.kind {
        JsonKind::Number { value, .. } if value.fract() == 0.0 && *value >= 0.0 => Some(*value as u32),
        _ => None,
    }
}

/// NeoForge applies an overlay entry only when every condition in its `neoforge:conditions` holds.
fn neoforge_conditions_hold(entry: &JsonNode, mods: &HashSet<String>) -> bool {
    match entry.get("neoforge:conditions").map(|c| &c.kind) {
        Some(JsonKind::Array(conditions)) => conditions.iter().all(|c| neoforge_condition(c, mods) != Some(false)),
        _ => true,
    }
}

pub fn has_conditional_overlays(meta: &JsonNode) -> bool {
    sections(meta).any(|(section, entry)| match section {
        "overlays" => entry.get("neoforge:conditions").is_some(),
        "neoforge:overlays" | "fabric:overlays" => true,
        _ => false,
    })
}

/// Returns whether a pack's `pack.mcmeta` `filter.block` list suppresses `(namespace, path)`.
#[allow(dead_code)]
pub fn pack_filter_blocks(meta: &JsonNode, namespace: &str, path: &str) -> bool {
    let Some(JsonKind::Array(entries)) = meta.get("filter").and_then(|f| f.get("block")).map(|b| &b.kind) else {
        return false;
    };
    entries.iter().any(|entry| {
        let ns_match = match entry.get("namespace").and_then(JsonNode::as_str) {
            Some(pat) => regex::Regex::new(pat).is_ok_and(|re| re.is_match(namespace)),
            None => true,
        };
        let path_match = match entry.get("path").and_then(JsonNode::as_str) {
            Some(pat) => regex::Regex::new(pat).is_ok_and(|re| re.is_match(path)),
            None => true,
        };
        ns_match && path_match
    })
}

/// Evaluates the NeoForge (or Forge) load conditions we can decide from the installed mods.
/// Returns `None` for any other condition, like `neoforge:item_exists`.
pub fn neoforge_condition(node: &JsonNode, mods: &HashSet<String>) -> Option<bool> {
    let kind = node.get("type")?.as_str()?;
    let (_, name) = kind.split_once(':').filter(|(ns, _)| matches!(*ns, "neoforge" | "forge"))?;
    match name {
        "mod_loaded" => Some(mods.contains(node.get("modid")?.as_str()?)),
        "not" => neoforge_condition(node.get("value")?, mods).map(|v| !v),
        "and" => all(list(node, "values")?.iter().map(|c| neoforge_condition(c, mods))),
        "or" => any(list(node, "values")?.iter().map(|c| neoforge_condition(c, mods))),
        "true" | "always" => Some(true),
        "false" | "never" => Some(false),
        _ => None,
    }
}

/// Evaluates the Fabric API resource conditions we can decide from the installed mods.
pub fn fabric_condition(node: &JsonNode, mods: &HashSet<String>) -> Option<bool> {
    let ids = |node: &JsonNode| -> Option<Vec<String>> { Some(list(node, "values")?.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()) };
    match node.get("condition")?.as_str()? {
        "fabric:all_mods_loaded" => Some(ids(node)?.iter().all(|m| mods.contains(m))),
        "fabric:any_mod_loaded" | "fabric:any_mods_loaded" => Some(ids(node)?.iter().any(|m| mods.contains(m))),
        "fabric:not" => fabric_condition(node.get("value")?, mods).map(|v| !v),
        "fabric:and" => all(list(node, "values")?.iter().map(|c| fabric_condition(c, mods))),
        "fabric:or" => any(list(node, "values")?.iter().map(|c| fabric_condition(c, mods))),
        "fabric:true" => Some(true),
        "fabric:false" => Some(false),
        _ => None,
    }
}

fn list<'a>(node: &'a JsonNode, key: &str) -> Option<&'a [JsonNode]> {
    match &node.get(key)?.kind {
        JsonKind::Array(items) => Some(items),
        _ => None,
    }
}

fn all(results: impl Iterator<Item = Option<bool>>) -> Option<bool> {
    let mut known = Some(true);
    for result in results {
        match result {
            Some(false) => return Some(false),
            None => known = None,
            Some(true) => {}
        }
    }
    known
}

fn any(results: impl Iterator<Item = Option<bool>>) -> Option<bool> {
    let mut known = Some(false);
    for result in results {
        match result {
            Some(true) => return Some(true),
            None => known = None,
            Some(false) => {}
        }
    }
    known
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct Files(HashMap<&'static str, &'static str>);

    impl PackFiles for Files {
        fn read(&self, relative: &str) -> Option<String> {
            self.0.get(relative).map(|s| s.to_string())
        }
        fn exists(&self, relative: &str) -> bool {
            self.0.contains_key(relative)
        }
    }

    // Trimmed from Geophilic 3.7's pack.mcmeta.
    const GEOPHILIC: &str = r#"{
        "pack": { "pack_format": 15, "supported_formats": [15, 121], "min_format": 15, "max_format": 121 },
        "overlays": { "entries": [
            { "formats": [41, 512], "min_format": 41, "max_format": 512, "directory": "f41data" },
            { "formats": [101, 512], "min_format": 101, "max_format": 512, "directory": "f101-2026-1" },
            { "formats": [121, 512], "min_format": 121, "max_format": 512, "directory": "f26-3" }
        ] }
    }"#;
    const FEATURE: &str = "data/geophilic/worldgen/configured_feature/coal_and_iron.json";

    fn geophilic() -> Files {
        Files(HashMap::from([
            ("mods/geo.jar/pack.mcmeta", GEOPHILIC),
            ("mods/geo.jar/data/geophilic/worldgen/configured_feature/coal_and_iron.json", "{}"),
            ("mods/geo.jar/f41data/data/geophilic/worldgen/configured_feature/coal_and_iron.json", "{}"),
            ("mods/geo.jar/f101-2026-1/data/geophilic/worldgen/configured_feature/coal_and_iron.json", "{}"),
        ]))
    }

    fn env<'a>(minecraft: &'a str, major: u32, minor: u32, mods: &'a HashSet<String>) -> OverlayEnv<'a> {
        OverlayEnv { minecraft, format: PackFormat { major, minor }, neoforge: true, fabric_api: false, mods }
    }

    #[test]
    fn finds_the_copy_the_highest_applied_overlay_holds() {
        let mods = HashSet::new();
        let files = geophilic();
        let path = format!("mods/geo.jar/{FEATURE}");
        let modern = env("26.1.2", 101, 1, &mods);
        assert_eq!(file_load(&path, &files, &modern), FileLoad::Replaced { overlay: "f101-2026-1".into(), by: format!("mods/geo.jar/f101-2026-1/{FEATURE}") });
        assert_eq!(
            file_load(&format!("mods/geo.jar/f41data/{FEATURE}"), &files, &modern),
            FileLoad::Replaced { overlay: "f101-2026-1".into(), by: format!("mods/geo.jar/f101-2026-1/{FEATURE}") }
        );
        assert_eq!(file_load(&format!("mods/geo.jar/f101-2026-1/{FEATURE}"), &files, &modern), FileLoad::Loaded);
        assert_eq!(
            file_load(&format!("mods/geo.jar/f26-3/{FEATURE}"), &files, &modern),
            FileLoad::OverlayInactive { overlay: "f26-3".into() }
        );
        // 1.21.1 (format 48) applies only f41data.
        let old = env("1.21.1", 48, 0, &mods);
        assert_eq!(file_load(&path, &files, &old), FileLoad::Replaced { overlay: "f41data".into(), by: format!("mods/geo.jar/f41data/{FEATURE}") });
        assert_eq!(file_load(&format!("mods/geo.jar/f41data/{FEATURE}"), &files, &old), FileLoad::Loaded);
        assert_eq!(
            file_load(&format!("mods/geo.jar/biomes_1_20/{FEATURE}"), &files, &modern),
            FileLoad::Unlisted { folder: "biomes_1_20".into() }
        );
        // Overlays arrived in 1.20.2.
        assert_eq!(file_load(&path, &files, &env("1.20.1", 15, 0, &mods)), FileLoad::Loaded);
    }

    #[test]
    fn reads_every_format_range_shape() {
        let entry = |json: &str| json_parser::parse(json, false).root.unwrap();
        let at = |major, minor| PackFormat { major, minor };
        assert!(applies_to_format(&entry(r#"{"formats": 48}"#), at(48, 0)));
        assert!(applies_to_format(&entry(r#"{"formats": {"min_inclusive": 40, "max_inclusive": 50}}"#), at(48, 0)));
        assert!(!applies_to_format(&entry(r#"{"formats": [49, 50]}"#), at(48, 0)));
        // 1.21.1 ignores min_format and max_format.
        assert!(!applies_to_format(&entry(r#"{"min_format": 40, "max_format": 50}"#), at(48, 0)));
        assert!(applies_to_format(&entry(r#"{"min_format": [101, 1], "max_format": 101}"#), at(101, 1)));
        assert!(!applies_to_format(&entry(r#"{"min_format": [101, 2], "max_format": 101}"#), at(101, 1)));
        assert!(applies_to_format(&entry(r#"{"formats": [94, 120]}"#), at(101, 1)), "falls back to formats without min/max");
    }

    #[test]
    fn applies_loader_overlays_only_where_their_conditions_hold() {
        // Trimmed from CliffTree 3.1.4b, which applies its Galosphere compat overlay through both loaders.
        let meta = json_parser::parse(
            r#"{
                "overlays": { "entries": [ { "directory": "deprecated", "formats": [48, 48] } ] },
                "neoforge:overlays": { "entries": [ {
                    "directory": "overlay.galosphere", "formats": [1, 99],
                    "neoforge:conditions": [ { "type": "neoforge:mod_loaded", "modid": "galosphere" } ]
                } ] },
                "fabric:overlays": { "entries": [ {
                    "directory": "overlay.galosphere",
                    "condition": { "condition": "fabric:all_mods_loaded", "values": [ "galosphere" ] }
                } ] }
            }"#,
            false,
        )
        .root
        .unwrap();
        let none = HashSet::new();
        let galosphere: HashSet<String> = ["galosphere".to_string()].into();
        let neo = |mods| OverlayEnv { minecraft: "1.21.1", format: PackFormat { major: 48, minor: 0 }, neoforge: true, fabric_api: false, mods };
        let fabric = |mods| OverlayEnv { minecraft: "1.21.1", format: PackFormat { major: 48, minor: 0 }, neoforge: false, fabric_api: true, mods };
        assert_eq!(active_overlays(&meta, &neo(&none)), ["deprecated"]);
        assert_eq!(active_overlays(&meta, &neo(&galosphere)), ["deprecated", "overlay.galosphere"]);
        assert_eq!(active_overlays(&meta, &fabric(&none)), ["deprecated"]);
        assert_eq!(active_overlays(&meta, &fabric(&galosphere)), ["deprecated", "overlay.galosphere"]);
    }

    #[test]
    fn evaluates_conditions_it_can_decide() {
        let mods: HashSet<String> = ["create".to_string()].into();
        let node = |json: &str| json_parser::parse(json, false).root.unwrap();
        assert_eq!(neoforge_condition(&node(r#"{"type": "neoforge:mod_loaded", "modid": "create"}"#), &mods), Some(true));
        assert_eq!(
            neoforge_condition(&node(r#"{"type": "neoforge:not", "value": {"type": "neoforge:mod_loaded", "modid": "create"}}"#), &mods),
            Some(false)
        );
        assert_eq!(
            neoforge_condition(&node(r#"{"type": "neoforge:and", "values": [{"type": "neoforge:item_exists", "item": "a:b"}, {"type": "neoforge:false"}]}"#), &mods),
            Some(false)
        );
        assert_eq!(neoforge_condition(&node(r#"{"type": "neoforge:item_exists", "item": "a:b"}"#), &mods), None);
        assert_eq!(neoforge_condition(&node(r#"{"type": "neoforge:always"}"#), &mods), Some(true));
        assert_eq!(neoforge_condition(&node(r#"{"type": "neoforge:not", "value": {"type": "neoforge:always"}}"#), &mods), Some(false));
        assert_eq!(fabric_condition(&node(r#"{"condition": "fabric:any_mod_loaded", "values": ["x", "create"]}"#), &mods), Some(true));
        assert_eq!(fabric_condition(&node(r#"{"condition": "fabric:any_mods_loaded", "values": ["x", "create"]}"#), &mods), Some(true));
        assert_eq!(fabric_condition(&node(r#"{"condition": "fabric:all_mods_loaded", "values": ["x", "create"]}"#), &mods), Some(false));
    }

    #[test]
    fn resolves_data_segment_with_pack_mcmeta_above_or_nested_data_folders() {
        let mods = HashSet::new();
        let files = Files(HashMap::from([
            ("data/packs/geo.jar/pack.mcmeta", GEOPHILIC),
            ("data/packs/geo.jar/data/geophilic/loot_table/data/x.json", "{}"),
            ("data/packs/geo.jar/f41data/data/geophilic/loot_table/data/x.json", "{}"),
        ]));
        let env = env("1.21.1", 48, 0, &mods);
        assert_eq!(
            file_load("data/packs/geo.jar/data/geophilic/loot_table/data/x.json", &files, &env),
            FileLoad::Replaced {
                overlay: "f41data".into(),
                by: "data/packs/geo.jar/f41data/data/geophilic/loot_table/data/x.json".into(),
            }
        );

        let filter_meta = json_parser::parse(
            r#"{"filter": {"block": [{"namespace": "^minecraft$", "path": "^recipe/.*"}]}}"#,
            false,
        )
        .root
        .unwrap();
        assert!(pack_filter_blocks(&filter_meta, "minecraft", "recipe/stick.json"));
        assert!(!pack_filter_blocks(&filter_meta, "create", "recipe/stick.json"));
    }
}
