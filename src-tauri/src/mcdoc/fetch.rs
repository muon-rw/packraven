#![allow(dead_code)]

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

use super::types::SymbolTables;
use super::version::compare_version;
use crate::http::CLIENT;

// Only background refreshes give up early. They always have a cached copy to fall back on, while a
// first download on a slow connection must be allowed to finish.
const BACKGROUND_REFRESH_TIMEOUT: Duration = Duration::from_secs(15);

const VANILLA_MCDOC_SYMBOLS_URL: &str = "https://api.spyglassmc.com/vanilla-mcdoc/symbols";
const MISODE_PARTNER_MCDOC_BASE: &str = "https://raw.githubusercontent.com/misode/misode.github.io/master/public/mcdoc";
const MISODE_CONFIG_URL: &str = "https://raw.githubusercontent.com/misode/misode.github.io/master/src/config.json";
const MUON_MCDOCS_BASE: &str = "https://raw.githubusercontent.com/muon-rw/mcdocs/main";

/// Pairs each load condition dispatcher in [`PACKRAVEN_MCDOC`] with the registry its `type` field names.
/// NeoForge's name matches its real registry, so a ProbeRaven dump fills in modded conditions. Fabric
/// and Forge 1.20.1 keep conditions in plain maps that no dump can read, so their `type` fields only
/// complete our built-in list and accept any other ID.
pub const CONDITION_REGISTRIES: &[(&str, &str)] = &[("neoforge:condition_codec", "neoforge:condition_codecs")];

/// Bundled Packraven mcdoc overlay covering NeoForge, Fabric, and Forge load conditions.
pub const PACKRAVEN_MCDOC: &str = r#"
struct NeoForgeCondition {
	type: #[id="neoforge:condition_codecs"] string,
	...neoforge:condition_codec[[type]],
}

dispatch minecraft:resource["packraven:neoforge_condition"] to NeoForgeCondition

#[neoforge_until="21.4.20"]
dispatch neoforge:condition_codec["neoforge:true", "neoforge:false"] to struct {}

#[neoforge_since="21.4.20"]
dispatch neoforge:condition_codec["neoforge:always", "neoforge:never"] to struct {}

dispatch neoforge:condition_codec["neoforge:not"] to struct {
	value: NeoForgeCondition,
}

dispatch neoforge:condition_codec["neoforge:and", "neoforge:or"] to struct {
	values: [NeoForgeCondition],
}

dispatch neoforge:condition_codec["neoforge:mod_loaded"] to struct {
	modid: string,
}

#[neoforge_until="21.4.20"]
dispatch neoforge:condition_codec["neoforge:item_exists"] to struct {
	item: #[id="item"] string,
}

#[neoforge_since="21.4.20"]
dispatch neoforge:condition_codec["neoforge:registered"] to struct {
	registry?: #[id="root"] string,
	value: string,
}

dispatch neoforge:condition_codec["neoforge:tag_empty"] to struct {
	#[neoforge_since="21.4.20"]
	registry?: #[id="root"] string,
	tag: #[id(registry="item", tags="implicit")] string,
}

#[neoforge_since="21.3"]
dispatch neoforge:condition_codec["neoforge:feature_flags_enabled"] to struct {
	flags: [string],
}

struct FabricCondition {
	condition: #[dispatcher_key="fabric:condition_codec"] string,
	...fabric:condition_codec[[condition]],
}

dispatch minecraft:resource["packraven:fabric_condition"] to FabricCondition

#[since="1.20.5"]
dispatch fabric:condition_codec["fabric:true"] to struct {}

dispatch fabric:condition_codec["fabric:false"] to struct {}

dispatch fabric:condition_codec["fabric:all_mods_loaded"] to struct {
	values: [string],
}

#[until="1.20.5"]
dispatch fabric:condition_codec["fabric:any_mod_loaded"] to struct {
	values: [string],
}

#[since="1.20.5"]
dispatch fabric:condition_codec["fabric:any_mods_loaded"] to struct {
	values: [string],
}

dispatch fabric:condition_codec["fabric:not"] to struct {
	value: FabricCondition,
}

dispatch fabric:condition_codec["fabric:and", "fabric:or"] to struct {
	values: [FabricCondition],
}

dispatch fabric:condition_codec["fabric:tags_populated"] to struct {
	registry?: #[id="root"] string,
	values: [string],
}

#[until="1.20.5"]
dispatch fabric:condition_codec["fabric:block_tags_populated"] to struct {
	values: [#[id(registry="block", tags="implicit")] string],
}

#[until="1.20.5"]
dispatch fabric:condition_codec["fabric:fluid_tags_populated"] to struct {
	values: [#[id(registry="fluid", tags="implicit")] string],
}

#[until="1.20.5"]
dispatch fabric:condition_codec["fabric:item_tags_populated"] to struct {
	values: [#[id(registry="item", tags="implicit")] string],
}

dispatch fabric:condition_codec["fabric:features_enabled"] to struct {
	features: [string],
}

dispatch fabric:condition_codec["fabric:registry_contains"] to struct {
	registry?: #[id="root"] string,
	values: [string],
}

struct ForgeCondition {
	type: #[dispatcher_key="forge:condition_codec"] string,
	...forge:condition_codec[[type]],
}

dispatch minecraft:resource["packraven:forge_condition"] to ForgeCondition

dispatch forge:condition_codec["forge:true", "forge:false"] to struct {}

dispatch forge:condition_codec["forge:not"] to struct {
	value: ForgeCondition,
}

dispatch forge:condition_codec["forge:and", "forge:or"] to struct {
	values: [ForgeCondition],
}

dispatch forge:condition_codec["forge:mod_loaded"] to struct {
	modid: string,
}

dispatch forge:condition_codec["forge:item_exists"] to struct {
	item: #[id="item"] string,
}

dispatch forge:condition_codec["forge:tag_empty"] to struct {
	tag: #[id(registry="item", tags="implicit")] string,
}
"#;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MuonManifest {
    #[serde(default)]
    pub format_version: u32,
    #[serde(default)]
    pub schemas: Vec<MuonSchemaEntry>,
    #[serde(default)]
    pub recipe_dispatcher_bridges: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MuonSchemaEntry {
    pub id: String,
    pub file: String,
    pub module: String,
    #[serde(default)]
    pub loaders: Vec<String>,
    #[serde(default)]
    pub mod_ids: Vec<String>,
    #[serde(default)]
    pub always_load: bool,
    #[serde(default)]
    pub min_minecraft: Option<String>,
    #[serde(default)]
    pub max_minecraft: Option<String>,
    #[serde(default)]
    pub paths: Vec<PathRule>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathRule {
    #[serde(default)]
    pub namespace: Option<String>,
    #[serde(default)]
    pub folder: Option<String>,
    #[serde(default)]
    pub exact: Option<String>,
    pub category: String,
}

impl PathRule {
    /// Matches a path relative to `data/<namespace>/` (without `.json` suffix).
    pub fn matches(&self, namespace: &str, rest: &str) -> bool {
        if let Some(req_ns) = self.namespace.as_deref()
            && !req_ns.eq_ignore_ascii_case(namespace)
        {
            return false;
        }
        if let Some(exact) = self.exact.as_deref() {
            return rest == exact;
        }
        if let Some(folder) = self.folder.as_deref() {
            let trimmed = folder.trim_matches('/');
            return rest.strip_prefix(trimmed).is_some_and(|after| after.starts_with('/'));
        }
        false
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MisodeGenerator {
    pub id: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub dependency: Option<String>,
    #[serde(default)]
    pub min_version: Option<String>,
    #[serde(default)]
    pub max_version: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct MisodeConfig {
    #[serde(default)]
    pub generators: Vec<MisodeGenerator>,
}

/// Fallback partner schema names used only when `misode/misode.github.io/src/config.json` is offline and uncached.
const FALLBACK_PARTNERS: &[&str] = &[
    "ad_astra",
    "ars_nouveau",
    "create",
    "fabric",
    "immersive_weathering",
    "loot-table-modifier",
    "lychee",
    "neoforge",
    "ohthetreesyoullgrow",
    "pixelmon",
    "sky_aesthetics",
    "starexpress",
    "tauridev",
    "thermoo",
    "treeplacer",
];

/// Maps misode partner dependency names to mod namespaces when the two differ.
fn partner_mod_aliases(dep: &str) -> &'static [&'static str] {
    match dep {
        "tauridev" => &["jsg", "jsg_core", "tauridev"],
        "loot-table-modifier" => &["loot-table-modifier", "loot_table_modifier"],
        _ => &[],
    }
}

pub fn schema_matches_version(mc: Option<&str>, min: Option<&str>, max: Option<&str>) -> bool {
    let Some(mc) = mc else { return true };
    if let Some(min_v) = min
        && compare_version(mc, min_v) == Ordering::Less
    {
        return false;
    }
    if let Some(max_v) = max
        && compare_version(mc, max_v) == Ordering::Greater
    {
        return false;
    }
    true
}

/// Returns the list of misode partner `.mcdoc` schema names relevant to a pack, discovered dynamically
/// from `misode/misode.github.io/src/config.json` (with an offline fallback list).
pub fn partner_schemas_for_pack(
    loader_name: Option<&str>,
    mod_ids: &HashSet<String>,
    misode_config: Option<&MisodeConfig>,
) -> Vec<String> {
    let mut deps: Vec<String> = match misode_config {
        Some(cfg) => {
            let mut set = Vec::new();
            for generator in &cfg.generators {
                if let Some(dep) = &generator.dependency
                    && !set.contains(dep)
                {
                    set.push(dep.clone());
                }
            }
            set
        }
        None => FALLBACK_PARTNERS.iter().map(|s| (*s).to_string()).collect(),
    };
    deps.sort();

    let mut out = Vec::new();
    for dep in deps {
        let loader_match = (dep == "neoforge" && loader_name.is_some_and(|l| l.eq_ignore_ascii_case("neoforge")))
            || (dep == "fabric" && loader_name.is_some_and(|l| l.eq_ignore_ascii_case("fabric")));
        let alias_match = partner_mod_aliases(&dep).iter().any(|a| mod_ids.contains(*a));
        if loader_match || mod_ids.contains(&dep) || alias_match {
            out.push(dep);
        }
    }
    out
}

/// Builds dynamic datapack folder-to-category rules from `misode`'s live partner generators.
pub fn misode_partner_path_rules(misode_config: &MisodeConfig, active_partners: &[String], mc: Option<&str>) -> Vec<PathRule> {
    let active: HashSet<&str> = active_partners.iter().map(String::as_str).collect();
    let mut rules = Vec::new();
    for generator in &misode_config.generators {
        let Some(dep) = generator.dependency.as_deref() else { continue };
        if !active.contains(dep) {
            continue;
        }
        if !schema_matches_version(mc, generator.min_version.as_deref(), generator.max_version.as_deref()) {
            continue;
        }
        let Some(path) = generator.path.as_deref().filter(|p| !p.is_empty()) else { continue };
        // NeoForge data maps use real disk paths under `data/neoforge/data_maps/...` (handled in `registries.rs`),
        // and recipe-only generators (`create`, `ars_nouveau`, `lychee`) bridge into `minecraft:recipe_serializer`.
        if generator.id.starts_with("neoforge:data_map_") || matches!(generator.id.as_str(), "create:recipes" | "ars_nouveau:recipes" | "lychee:recipe") {
            continue;
        }
        rules.push(PathRule {
            namespace: None,
            folder: Some(path.to_string()),
            exact: None,
            category: generator.id.clone(),
        });
    }
    rules
}

/// Loads `muon-rw/mcdocs/manifest.json` from persistent local storage, refreshing from GitHub only when absent or `force` is true.
pub async fn load_muon_manifest(force: bool) -> Result<MuonManifest> {
    let path = cache_dir().join("muon").join("manifest.json");
    let url = format!("{MUON_MCDOCS_BASE}/manifest.json");
    load_cached(&path, &url, force, |bytes| {
        serde_json::from_slice(bytes).context("parsing muon-rw/mcdocs manifest.json")
    })
    .await
}

/// Loads a `.mcdoc` file from `muon-rw/mcdocs`, persisted locally and refreshed non-destructively.
pub async fn load_muon_mcdoc(file: &str, force: bool) -> Result<String> {
    let rel = file.trim_start_matches('/');
    let safe_name = sanitize_version(&rel.replace('/', "_"));
    let path = cache_dir().join("muon").join(safe_name);
    let url = format!("{MUON_MCDOCS_BASE}/{rel}");
    load_cached(&path, &url, force, |bytes| parse_mcdoc_bytes(bytes, rel)).await
}

/// Loads `misode/misode.github.io/src/config.json` from persistent local storage, refreshing only when absent or `force` is true.
pub async fn load_misode_config(force: bool) -> Result<MisodeConfig> {
    let path = cache_dir().join("misode-config.json");
    load_cached(&path, MISODE_CONFIG_URL, force, |bytes| {
        serde_json::from_slice(bytes).context("parsing misode config.json")
    })
    .await
}

/// Loads a partner `.mcdoc` file from persistent local storage, refreshing from `misode/misode.github.io` only when absent or `force` is true.
pub async fn load_partner_mcdoc(name: &str, force: bool) -> Result<String> {
    let safe = sanitize_version(name);
    let path = cache_dir().join("partners").join(format!("{safe}.mcdoc"));
    let url = format!("{MISODE_PARTNER_MCDOC_BASE}/{safe}.mcdoc");
    load_cached(&path, &url, force, |bytes| parse_mcdoc_bytes(bytes, name)).await
}

/// Fixes fields that misode's partner schemas get wrong for the loader's own codecs. NeoForge reads
/// `steps` with `optionalFieldOf` (defaulting to every step) on both removal modifiers, and names the
/// carver field `steps`, not `step`.
pub fn patch_partner_mcdoc(name: &str, src: String) -> String {
    match name {
        "neoforge" => src
            .replace("steps: (DecorationStep | [DecorationStep])", "steps?: (DecorationStep | [DecorationStep])")
            .replace("step: (CarveStep | [CarveStep])", "steps?: (CarveStep | [CarveStep])"),
        _ => src,
    }
}

/// Bridges partner recipe dispatchers (e.g. `create:recipes`, `ars_nouveau:recipes`) into
/// `minecraft:recipe_serializer` so vanilla `data/<ns>/recipe/*.json` files validate modded recipes,
/// and widens vanilla/Create ingredient types to accept custom loader ingredients (`forge:nbt`, `neoforge:components`, etc.).
pub fn bridge_partner_dispatchers(tables: &mut SymbolTables, extra_bridges: &[String]) {
    let mut sources: Vec<&str> = vec!["create:recipes", "ars_nouveau:recipes", "lychee:recipe", "lychee:recipes", "jsg_core:recipes"];
    for extra in extra_bridges {
        if !sources.contains(&extra.as_str()) {
            sources.push(extra.as_str());
        }
    }
    for source_reg in sources {
        let Some(entries) = tables.dispatchers.get(source_reg).cloned() else { continue };
        let target = tables.dispatchers.entry("minecraft:recipe_serializer".into()).or_default();
        for (k, v) in entries {
            target.entry(k).or_insert(v);
        }
    }

    annotate_partner_id_fields(tables);

    // Allow Forge/NeoForge and Fabric (`{ "fabric:type": ... }`) custom ingredients inside vanilla and Create recipes.
    let version_attr = |name: &str, version: &str| super::types::Attribute {
        name: name.into(),
        value: Some(super::types::AttributeValue::Type(super::types::McdocType::Literal {
            value: super::types::LiteralValue::String { value: version.into() },
            attributes: Vec::new(),
        })),
    };
    let make_custom_ingredient = |key_name: &str, desc: &str, loaders: &str, versions: Vec<super::types::Attribute>| super::types::McdocType::Struct {
        fields: vec![
            super::types::StructField::Pair {
                key: super::types::StructKey::Literal(key_name.into()),
                field_type: super::types::McdocType::String { length_range: None, attributes: Vec::new() },
                optional: false,
                deprecated: false,
                desc: Some(desc.into()),
                attributes: vec![super::checker::loader_attr(loaders)],
            },
            super::types::StructField::Pair {
                key: super::types::StructKey::Dynamic(super::types::McdocType::String {
                    length_range: None,
                    attributes: Vec::new(),
                }),
                field_type: super::types::McdocType::Any { attributes: Vec::new() },
                optional: true,
                deprecated: false,
                desc: None,
                attributes: Vec::new(),
            },
        ],
        attributes: versions,
    };
    // NeoForge 1.21.1 reads custom ingredients with `dispatchMapOrElse`'s default `type` key, and from
    // 1.21.2 on with `neoforge:ingredient_type`, which Forge never adopted.
    let custom_ingredients = [
        make_custom_ingredient("type", "Custom Forge ingredient serializer ID.", "forge", Vec::new()),
        make_custom_ingredient("type", "Custom NeoForge ingredient serializer ID.", "neoforge", vec![version_attr("until", "1.21.2")]),
        make_custom_ingredient("neoforge:ingredient_type", "Custom NeoForge ingredient serializer ID.", "neoforge", vec![version_attr("since", "1.21.2")]),
        make_custom_ingredient("fabric:type", "Custom Fabric ingredient serializer ID.", "fabric", Vec::new()),
    ];
    for symbol in [
        "::java::data::recipe::Ingredient",
        "::java::data::recipe::IngredientValue",
        "::partner::create::ItemOrTag",
        "::partner::create::ItemOrTagWithCount",
    ] {
        if let Some(existing) = tables.mcdoc.get_mut(symbol) {
            match existing {
                super::types::McdocType::Union { members, .. } => members.extend(custom_ingredients.iter().cloned()),
                other => {
                    let prev = std::mem::replace(other, super::types::McdocType::Any { attributes: Vec::new() });
                    *other = super::types::McdocType::Union {
                        members: std::iter::once(prev).chain(custom_ingredients.iter().cloned()).collect(),
                        attributes: Vec::new(),
                    };
                }
            }
        }
    }

    // Widen NeoForge's closed BiomeModifierType / StructureModifierType enums to also accept modded serializer IDs.
    for (symbol, reg) in [
        ("::partner::neoforge::BiomeModifierType", "neoforge:biome_modifier_serializer"),
        ("::partner::neoforge::StructureModifierType", "neoforge:structure_modifier_serializer"),
    ] {
        if let Some(existing) = tables.mcdoc.get_mut(symbol) {
            let prev = std::mem::replace(existing, super::types::McdocType::Any { attributes: Vec::new() });
            let modded_id = super::types::McdocType::String {
                length_range: None,
                attributes: vec![super::types::Attribute {
                    name: "id".into(),
                    value: Some(super::types::AttributeValue::Type(super::types::McdocType::Literal {
                        value: super::types::LiteralValue::String { value: reg.into() },
                        attributes: Vec::new(),
                    })),
                }],
            };
            *existing = super::types::McdocType::Union {
                members: vec![prev, modded_id],
                attributes: Vec::new(),
            };
        }
    }
}

/// Adds missing `#[id]` attributes to misode partner schemas that declare registry ID fields as
/// bare `string` (notably `create.mcdoc` and one condition in `lychee.mcdoc`). Only touches fields
/// that are still plain strings without an `id` attribute, so upstream fixes pass through cleanly.
/// Pending a PR and merge. //TODO: pr for these
fn annotate_partner_id_fields(tables: &mut SymbolTables) {
    use super::types::{McdocType, StructField, StructKey};

    fn annotate_field(ty: &mut McdocType, field_name: &str, replacement: &dyn Fn() -> McdocType) {
        match ty {
            McdocType::Struct { fields, .. } => {
                for field in fields {
                    if let StructField::Pair { key: StructKey::Literal(k), field_type, .. } = field
                        && k == field_name
                        && matches!(field_type, McdocType::String { attributes, .. } if !attributes.iter().any(|a| a.name == "id"))
                    {
                        *field_type = replacement();
                    }
                }
            }
            McdocType::Union { members, .. } => {
                for member in members {
                    annotate_field(member, field_name, replacement);
                }
            }
            _ => {}
        }
    }

    let item_id = || McdocType::id_string("item", None);
    let item_tag = || McdocType::id_string("item", Some("implicit"));
    let fluid_id = || McdocType::id_string("fluid", None);
    let fluid_tag = || McdocType::id_string("fluid", Some("implicit"));
    let potion_id = || McdocType::id_string("potion", None);
    let item_or_fluid_id = || McdocType::Union {
        members: vec![McdocType::id_string("item", None), McdocType::id_string("fluid", None)],
        attributes: Vec::new(),
    };

    for sym in ["::partner::create::Item", "::partner::create::ItemWithCount", "::partner::create::SimpleItem"] {
        if let Some(ty) = tables.mcdoc.get_mut(sym) {
            annotate_field(ty, "item", &item_id);
            annotate_field(ty, "id", &item_id);
        }
    }
    for sym in ["::partner::create::ItemOrTag", "::partner::create::ItemOrTagWithCount"] {
        if let Some(ty) = tables.mcdoc.get_mut(sym) {
            annotate_field(ty, "item", &item_id);
            annotate_field(ty, "tag", &item_tag);
        }
    }
    if let Some(ty) = tables.mcdoc.get_mut("::partner::create::Fluid") {
        annotate_field(ty, "fluid", &fluid_id);
        annotate_field(ty, "id", &fluid_id);
    }
    if let Some(ty) = tables.mcdoc.get_mut("::partner::create::FluidOrTag") {
        annotate_field(ty, "fluid", &fluid_id);
        annotate_field(ty, "fluidTag", &fluid_tag);
        annotate_field(ty, "fluid_tag", &fluid_tag);
    }
    if let Some(ty) = tables.mcdoc.get_mut("::partner::create::MixingResult") {
        annotate_field(ty, "id", &item_or_fluid_id);
    }
    if let Some(ty) = tables.mcdoc.get_mut("::partner::create::NBT") {
        annotate_field(ty, "Potion", &potion_id);
    }
    if let Some(conditions) = tables.dispatchers.get_mut("lychee:conditions")
        && let Some(ty) = conditions.get_mut("is_off_item_cooldown")
    {
        annotate_field(ty, "item", &item_id);
    }
}

/// Persistent local directory where downloaded mcdoc schemas and vanilla registry snapshots are saved.
pub fn cache_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("dev.muon.packraven")
        .join("mcdoc")
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
}

fn file_stamp(path: &Path) -> Option<FileStamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some(FileStamp { len: meta.len(), modified: meta.modified().ok() })
}

static VANILLA_SYMBOLS_CACHE: OnceLock<Mutex<Option<(FileStamp, SymbolTables)>>> = OnceLock::new();
static MCMETA_REGISTRIES_CACHE: OnceLock<Mutex<HashMap<String, (FileStamp, HashMap<String, Vec<String>>)>>> = OnceLock::new();
static MCMETA_BLOCKS_CACHE: OnceLock<Mutex<HashMap<String, (FileStamp, HashMap<String, BTreeMap<String, Vec<String>>>)>>> = OnceLock::new();
static MCMETA_VERSIONS_CACHE: OnceLock<Mutex<Option<(FileStamp, Vec<McmetaVersion>)>>> = OnceLock::new();
static MCMETA_GAMERULES_CACHE: OnceLock<Mutex<HashMap<String, (FileStamp, GameRules)>>> = OnceLock::new();
static MCMETA_LANG_KEYS_CACHE: OnceLock<Mutex<HashMap<String, (FileStamp, Vec<String>)>>> = OnceLock::new();

const MCMETA_VERSIONS_URL: &str = "https://raw.githubusercontent.com/misode/mcmeta/refs/heads/summary/versions/data.min.json";

#[derive(Debug, Clone, Deserialize)]
pub struct McmetaVersion {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub data_version: Option<u64>,
    #[serde(default)]
    pub data_pack_version: Option<u32>,
    #[serde(default)]
    pub data_pack_version_minor: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct GameRules {
    pub boolean: Vec<String>,
    pub int: Vec<String>,
}

/// Loads the `misode/mcmeta` versions catalog (used for `#[pack_format]` completions and weekly snapshot resolution).
pub async fn load_mcmeta_versions(force: bool) -> Result<Vec<McmetaVersion>> {
    let path = cache_dir().join("mcmeta").join("versions.json");
    let cache = MCMETA_VERSIONS_CACHE.get_or_init(|| Mutex::new(None));
    if !force && let Some(stamp) = file_stamp(&path) {
        if let Some((cached_stamp, versions)) = cache.lock().unwrap().as_ref()
            && *cached_stamp == stamp
        {
            return Ok(versions.clone());
        }
    }
    let versions: Vec<McmetaVersion> = load_cached_parsing_off_thread(&path, MCMETA_VERSIONS_URL, force, |bytes| {
        serde_json::from_slice(bytes).context("parsing mcmeta versions.json")
    })
    .await?;
    if let Some(stamp) = file_stamp(&path) {
        *cache.lock().unwrap() = Some((stamp, versions.clone()));
    }
    Ok(versions)
}

/// Extracts newest-first `(data_pack_version, release_label)` pairs for `#[pack_format]` completion,
/// matching Spyglass's `registerPackFormatAttribute`.
pub fn data_pack_formats_from_versions(versions: &[McmetaVersion]) -> Vec<(u32, String)> {
    let mut sorted: Vec<&McmetaVersion> = versions.iter().collect();
    sorted.sort_by(|a, b| b.data_version.cmp(&a.data_version));
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for v in sorted {
        if v.kind != "release" {
            continue;
        }
        if let Some(fmt) = v.data_pack_version
            && seen.insert(fmt)
        {
            out.push((fmt, v.id.clone()));
        }
    }
    out
}

/// Maps a weekly snapshot like `24w14a` to its target release version (e.g. `1.21`) using the `mcmeta` versions catalog.
pub fn resolve_snapshot_version(mc: &str, versions: &[McmetaVersion]) -> Option<String> {
    if !super::version::is_weekly_snapshot(mc) {
        return None;
    }
    let mut sorted: Vec<&McmetaVersion> = versions.iter().collect();
    sorted.sort_by(|a, b| b.data_version.cmp(&a.data_version));
    let idx = sorted.iter().position(|v| v.id.eq_ignore_ascii_case(mc))?;
    // In descending `data_version` order, the closest release at or before `idx` is the target release
    // that shipped after this snapshot (matching Spyglass `resolveConfiguredVersion`).
    for i in (0..=idx).rev() {
        if sorted[i].kind == "release" {
            return Some(sorted[i].id.clone());
        }
    }
    sorted.iter().find(|v| v.kind == "release").map(|v| v.id.clone())
}

/// Loads vanilla boolean and integer gamerule names from `misode/mcmeta` `commands/data.min.json`.
pub async fn load_mcmeta_game_rules(mc_version: &str, force: bool) -> Result<GameRules> {
    let safe_ver = sanitize_version(mc_version);
    let path = cache_dir().join("mcmeta").join(&safe_ver).join("commands.json");
    let cache = MCMETA_GAMERULES_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if !force && let Some(stamp) = file_stamp(&path) {
        if let Some((cached_stamp, rules)) = cache.lock().unwrap().get(&safe_ver)
            && *cached_stamp == stamp
        {
            return Ok(rules.clone());
        }
    }
    let url = format!("https://raw.githubusercontent.com/misode/mcmeta/{mc_version}-summary/commands/data.min.json");
    let mc = mc_version.to_string();
    let rules = load_cached_parsing_off_thread(&path, &url, force, move |bytes| {
        parse_mcmeta_game_rules(bytes).with_context(|| format!("parsing mcmeta commands for {mc}"))
    })
    .await?;
    if let Some(stamp) = file_stamp(&path) {
        cache.lock().unwrap().insert(safe_ver, (stamp, rules.clone()));
    }
    Ok(rules)
}

pub fn parse_mcmeta_game_rules(bytes: &[u8]) -> Result<GameRules> {
    let root: Value = serde_json::from_slice(bytes)?;
    let mut rules = GameRules::default();
    let Some(children) = root
        .get("children")
        .and_then(|c| c.get("gamerule"))
        .and_then(|g| g.get("children"))
        .and_then(Value::as_object)
    else {
        return Ok(rules);
    };
    for (name, node) in children {
        let Some(val_node) = node.get("children").and_then(|c| c.get("value")) else { continue };
        if val_node.get("type").and_then(Value::as_str) != Some("argument") {
            continue;
        }
        let parser = val_node
            .get("parser")
            .and_then(Value::as_str)
            .or_else(|| val_node.get("argument").and_then(|a| a.get("type")).and_then(Value::as_str));
        match parser {
            Some("brigadier:bool" | "minecraft:bool") => rules.boolean.push(name.clone()),
            Some("brigadier:integer" | "minecraft:integer") => rules.int.push(name.clone()),
            _ => {}
        }
    }
    rules.boolean.sort_unstable();
    rules.int.sort_unstable();
    Ok(rules)
}

/// Loads `vanilla-mcdoc/symbols` from persistent local storage, refreshing from Spyglass's API only when absent or `force` is true.
pub async fn load_vanilla_symbols(force: bool) -> Result<SymbolTables> {
    let path = cache_dir().join("vanilla-mcdoc-symbols.json");
    let cache = VANILLA_SYMBOLS_CACHE.get_or_init(|| Mutex::new(None));
    if !force && let Some(stamp) = file_stamp(&path) {
        if let Some((cached_stamp, tables)) = cache.lock().unwrap().as_ref()
            && *cached_stamp == stamp
        {
            return Ok(tables.clone());
        }
    }
    let tables: SymbolTables = load_cached_parsing_off_thread(&path, VANILLA_MCDOC_SYMBOLS_URL, force, |bytes| {
        serde_json::from_slice(bytes).context("parsing vanilla-mcdoc symbols.json")
    })
    .await?;
    if let Some(stamp) = file_stamp(&path) {
        *cache.lock().unwrap() = Some((stamp, tables.clone()));
    }
    Ok(tables)
}

/// Loads the data pack format (`data_pack_version`, plus `data_pack_version_minor` from 1.21.9) that
/// `mc_version` reports in its `version.json`, from persistent local storage.
pub async fn load_mcmeta_data_pack_format(mc_version: &str, force: bool) -> Result<super::overlays::PackFormat> {
    let safe_ver = sanitize_version(mc_version);
    let path = cache_dir().join("mcmeta").join(&safe_ver).join("version.json");
    let url = format!("https://raw.githubusercontent.com/misode/mcmeta/{mc_version}-summary/version.json");
    load_cached(&path, &url, force, |bytes| {
        let version: serde_json::Value = serde_json::from_slice(bytes).with_context(|| format!("parsing mcmeta version.json for {mc_version}"))?;
        let number = |key: &str| version.get(key).and_then(serde_json::Value::as_u64).and_then(|n| u32::try_from(n).ok());
        let major = number("data_pack_version").with_context(|| format!("mcmeta version.json for {mc_version} has no data_pack_version"))?;
        Ok(super::overlays::PackFormat { major, minor: number("data_pack_version_minor").unwrap_or(0) })
    })
    .await
}

/// Loads vanilla registries (`summary/registries/data.min.json`) for `mc_version` from persistent local storage.
pub async fn load_mcmeta_registries(mc_version: &str, force: bool) -> Result<HashMap<String, Vec<String>>> {
    let safe_ver = sanitize_version(mc_version);
    let path = cache_dir().join("mcmeta").join(&safe_ver).join("registries.json");
    let cache = MCMETA_REGISTRIES_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if !force && let Some(stamp) = file_stamp(&path) {
        if let Some((cached_stamp, regs)) = cache.lock().unwrap().get(&safe_ver)
            && *cached_stamp == stamp
        {
            return Ok(regs.clone());
        }
    }
    let url = format!("https://raw.githubusercontent.com/misode/mcmeta/{mc_version}-summary/registries/data.min.json");
    let mc = mc_version.to_string();
    let regs: HashMap<String, Vec<String>> = load_cached_parsing_off_thread(&path, &url, force, move |bytes| {
        serde_json::from_slice(bytes).with_context(|| format!("parsing mcmeta registries for {mc}"))
    })
    .await?;
    if let Some(stamp) = file_stamp(&path) {
        cache.lock().unwrap().insert(safe_ver, (stamp, regs.clone()));
    }
    Ok(regs)
}

/// Loads the sorted translation keys of vanilla's `en_us.json` for `mc_version` from persistent local storage.
pub async fn load_mcmeta_lang_keys(mc_version: &str, force: bool) -> Result<Vec<String>> {
    let safe_ver = sanitize_version(mc_version);
    let path = cache_dir().join("mcmeta").join(&safe_ver).join("en_us.json");
    let cache = MCMETA_LANG_KEYS_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if !force && let Some(stamp) = file_stamp(&path) {
        if let Some((cached_stamp, keys)) = cache.lock().unwrap().get(&safe_ver)
            && *cached_stamp == stamp
        {
            return Ok(keys.clone());
        }
    }
    let url = format!("https://raw.githubusercontent.com/misode/mcmeta/{mc_version}-assets-json/assets/minecraft/lang/en_us.json");
    let mc = mc_version.to_string();
    let keys = load_cached_parsing_off_thread(&path, &url, force, move |bytes| {
        let lang: serde_json::Map<String, serde_json::Value> =
            serde_json::from_slice(bytes).with_context(|| format!("parsing vanilla en_us.json for {mc}"))?;
        let mut keys: Vec<String> = lang.into_iter().map(|(k, _)| k).collect();
        keys.sort_unstable();
        Ok(keys)
    })
    .await?;
    if let Some(stamp) = file_stamp(&path) {
        cache.lock().unwrap().insert(safe_ver, (stamp, keys.clone()));
    }
    Ok(keys)
}

/// Loads vanilla block state definitions (`summary/blocks/data.min.json`) for `mc_version` from persistent local storage.
pub async fn load_mcmeta_block_states(mc_version: &str, force: bool) -> Result<HashMap<String, BTreeMap<String, Vec<String>>>> {
    let safe_ver = sanitize_version(mc_version);
    let path = cache_dir().join("mcmeta").join(&safe_ver).join("blocks.json");
    let cache = MCMETA_BLOCKS_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if !force && let Some(stamp) = file_stamp(&path) {
        if let Some((cached_stamp, blocks)) = cache.lock().unwrap().get(&safe_ver)
            && *cached_stamp == stamp
        {
            return Ok(blocks.clone());
        }
    }
    let url = format!("https://raw.githubusercontent.com/misode/mcmeta/{mc_version}-summary/blocks/data.min.json");
    let mc = mc_version.to_string();
    let blocks = load_cached_parsing_off_thread(&path, &url, force, move |bytes| {
        parse_mcmeta_blocks(bytes).with_context(|| format!("parsing mcmeta blocks for {mc}"))
    })
    .await?;
    if let Some(stamp) = file_stamp(&path) {
        cache.lock().unwrap().insert(safe_ver, (stamp, blocks.clone()));
    }
    Ok(blocks)
}

fn parse_mcdoc_bytes(bytes: &[u8], label: &str) -> Result<String> {
    let text = std::str::from_utf8(bytes)
        .with_context(|| format!("decoding mcdoc file {label}"))?
        .to_string();
    super::parser::validate_syntax(&text)
        .map_err(|e| anyhow::anyhow!("parsing mcdoc file {label}: {e}"))?;
    Ok(text)
}

/// Checks `vanilla-mcdoc/symbols`, `muon-rw/mcdocs`, and `misode` partner schemas against their
/// remote sources using HTTP `ETag` conditional requests, updating the local cache whenever any file
/// changed. Returns whether any cached schema changed on disk. Offline or failed requests leave the
/// cached files intact and return `false`.
pub async fn refresh_remote_schemas(active_muon_files: &[String], active_partners: &[String]) -> bool {
    let vanilla_path = cache_dir().join("vanilla-mcdoc-symbols.json");
    let manifest_path = cache_dir().join("muon").join("manifest.json");
    let manifest_url = format!("{MUON_MCDOCS_BASE}/manifest.json");
    let misode_path = cache_dir().join("misode-config.json");

    let (vanilla_res, manifest_res, misode_res) = tokio::join!(
        load_cached_with_status(&vanilla_path, VANILLA_MCDOC_SYMBOLS_URL, true, Some(BACKGROUND_REFRESH_TIMEOUT), |bytes| {
            serde_json::from_slice::<SymbolTables>(bytes).context("parsing vanilla-mcdoc symbols.json")
        }),
        load_cached_with_status(&manifest_path, &manifest_url, true, Some(BACKGROUND_REFRESH_TIMEOUT), |bytes| {
            serde_json::from_slice::<MuonManifest>(bytes).context("parsing muon-rw/mcdocs manifest.json")
        }),
        load_cached_with_status(&misode_path, MISODE_CONFIG_URL, true, Some(BACKGROUND_REFRESH_TIMEOUT), |bytes| {
            serde_json::from_slice::<MisodeConfig>(bytes).context("parsing misode config.json")
        }),
    );

    let mut changed = vanilla_res.as_ref().is_ok_and(|(_, c)| *c)
        || manifest_res.as_ref().is_ok_and(|(_, c)| *c)
        || misode_res.as_ref().is_ok_and(|(_, c)| *c);

    let mut muon_files: HashSet<String> = active_muon_files.iter().cloned().collect();
    if let Ok((manifest, _)) = manifest_res {
        for entry in manifest.schemas {
            let rel = entry.file.trim_start_matches('/');
            let cached = cache_dir().join("muon").join(sanitize_version(&rel.replace('/', "_")));
            if cached.is_file() {
                muon_files.insert(entry.file);
            }
        }
    }

    let mut partner_names: HashSet<String> = active_partners.iter().cloned().collect();
    if let Ok(mut entries) = tokio::fs::read_dir(cache_dir().join("partners")).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "mcdoc")
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                partner_names.insert(stem.to_string());
            }
        }
    }

    let mut set = tokio::task::JoinSet::new();
    for file in muon_files {
        set.spawn(async move {
            let rel = file.trim_start_matches('/').to_string();
            let safe_name = sanitize_version(&rel.replace('/', "_"));
            let path = cache_dir().join("muon").join(safe_name);
            let url = format!("{MUON_MCDOCS_BASE}/{rel}");
            load_cached_with_status(&path, &url, true, Some(BACKGROUND_REFRESH_TIMEOUT), |bytes| parse_mcdoc_bytes(bytes, &rel))
                .await
                .is_ok_and(|(_, c)| c)
        });
    }
    for name in partner_names {
        set.spawn(async move {
            let safe = sanitize_version(&name);
            let path = cache_dir().join("partners").join(format!("{safe}.mcdoc"));
            let url = format!("{MISODE_PARTNER_MCDOC_BASE}/{safe}.mcdoc");
            load_cached_with_status(&path, &url, true, Some(BACKGROUND_REFRESH_TIMEOUT), |bytes| parse_mcdoc_bytes(bytes, &name))
                .await
                .is_ok_and(|(_, c)| c)
        });
    }
    while let Some(joined) = set.join_next().await {
        if matches!(joined, Ok(true)) {
            changed = true;
        }
    }

    changed
}

/// Reads a persisted local file when present; on refresh (`force`) or cache miss, fetches from `url`
/// (sending `If-None-Match` when a cached `ETag` is present), validates with `parse`, and atomically
/// replaces the file on disk. A failed or invalid fetch falls back to the existing local file so
/// flaky networks or offline use never block or wipe schemas.
async fn load_cached<T, F>(path: &Path, url: &str, force: bool, parse: F) -> Result<T>
where
    F: Fn(&[u8]) -> Result<T>,
{
    load_cached_with_status(path, url, force, None, parse).await.map(|(value, _)| value)
}

async fn load_cached_parsing_off_thread<T, F>(path: &Path, url: &str, force: bool, parse: F) -> Result<T>
where
    T: Send + 'static,
    F: Fn(&[u8]) -> Result<T> + Send + Sync + 'static,
{
    let parse = std::sync::Arc::new(parse);
    let existing = if path.is_file() {
        if let Ok(bytes) = tokio::fs::read(path).await {
            let p = std::sync::Arc::clone(&parse);
            tokio::task::spawn_blocking(move || p(&bytes).ok()).await.ok().flatten()
        } else {
            None
        }
    } else {
        None
    };
    if !force && let Some(cached) = existing {
        return Ok(cached);
    }
    let etag_file = etag_path(path);
    let cached_etag = if existing.is_some() {
        tokio::fs::read_to_string(&etag_file).await.ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    } else {
        None
    };
    match fetch_conditional(url, cached_etag.as_deref(), None).await {
        Ok(FetchOutcome::NotModified) => match existing {
            Some(cached) => Ok(cached),
            None => anyhow::bail!("received 304 Not Modified without a cached copy for {url}"),
        },
        Ok(FetchOutcome::Body { bytes, etag }) => {
            let raw = bytes.clone();
            let p = std::sync::Arc::clone(&parse);
            let parsed = tokio::task::spawn_blocking(move || p(&raw))
                .await
                .map_err(|e| anyhow::anyhow!("parsing task failed for {url}: {e}"))?;
            match parsed {
                Ok(value) => {
                    let _ = write_atomic(path, &bytes).await;
                    if let Some(tag) = etag {
                        let _ = write_atomic(&etag_file, tag.as_bytes()).await;
                    } else {
                        let _ = tokio::fs::remove_file(&etag_file).await;
                    }
                    Ok(value)
                }
                Err(err) => match existing {
                    Some(cached) => Ok(cached),
                    None => Err(err),
                },
            }
        }
        Err(err) => match existing {
            Some(cached) => Ok(cached),
            None => Err(err),
        },
    }
}

async fn load_cached_with_status<T, F>(path: &Path, url: &str, force: bool, timeout: Option<Duration>, parse: F) -> Result<(T, bool)>
where
    F: Fn(&[u8]) -> Result<T>,
{
    let existing = if path.is_file() {
        tokio::fs::read(path).await.ok().and_then(|bytes| parse(&bytes).ok())
    } else {
        None
    };
    if !force && let Some(cached) = existing {
        return Ok((cached, false));
    }
    let etag_file = etag_path(path);
    let cached_etag = if existing.is_some() {
        tokio::fs::read_to_string(&etag_file).await.ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    } else {
        None
    };
    match fetch_conditional(url, cached_etag.as_deref(), timeout).await {
        Ok(FetchOutcome::NotModified) => match existing {
            Some(cached) => Ok((cached, false)),
            None => anyhow::bail!("received 304 Not Modified without a cached copy for {url}"),
        },
        Ok(FetchOutcome::Body { bytes, etag }) => match parse(&bytes) {
            Ok(parsed) => {
                let _ = write_atomic(path, &bytes).await;
                if let Some(tag) = etag {
                    let _ = write_atomic(&etag_file, tag.as_bytes()).await;
                } else {
                    let _ = tokio::fs::remove_file(&etag_file).await;
                }
                Ok((parsed, true))
            }
            Err(err) => match existing {
                Some(cached) => Ok((cached, false)),
                None => Err(err),
            },
        },
        Err(err) => match existing {
            Some(cached) => Ok((cached, false)),
            None => Err(err),
        },
    }
}

enum FetchOutcome {
    NotModified,
    Body { bytes: Vec<u8>, etag: Option<String> },
}

fn etag_path(path: &Path) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(".etag");
    PathBuf::from(os)
}

async fn fetch_conditional(url: &str, etag: Option<&str>, timeout: Option<Duration>) -> Result<FetchOutcome> {
    let mut req = CLIENT.get(url);
    if let Some(timeout) = timeout {
        req = req.timeout(timeout);
    }
    if let Some(tag) = etag {
        req = req.header(reqwest::header::IF_NONE_MATCH, tag);
    }
    let response = req.send().await?;
    if response.status() == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(FetchOutcome::NotModified);
    }
    let response = response.error_for_status().with_context(|| format!("fetching {url}"))?;
    let next_etag = response
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let bytes = response.bytes().await?.to_vec();
    Ok(FetchOutcome::Body { bytes, etag: next_etag })
}

fn parse_mcmeta_blocks(bytes: &[u8]) -> Result<HashMap<String, BTreeMap<String, Vec<String>>>> {
    // misode's summary/blocks/data.min.json maps block ID -> [properties_object, default_state_object].
    let raw: HashMap<String, Value> = serde_json::from_slice(bytes)?;
    let mut out = HashMap::with_capacity(raw.len());
    for (block, entry) in raw {
        let Some(props_obj) = entry.as_array().and_then(|arr| arr.first()).and_then(Value::as_object) else {
            continue;
        };
        let mut props = BTreeMap::new();
        for (prop, vals) in props_obj {
            if let Some(arr) = vals.as_array() {
                let list: Vec<String> = arr.iter().filter_map(|v| v.as_str().map(String::from)).collect();
                if !list.is_empty() {
                    props.insert(prop.clone(), list);
                }
            }
        }
        if !props.is_empty() {
            out.insert(block, props);
        }
    }
    Ok(out)
}

async fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let tmp = path.with_extension("part");
    tokio::fs::write(&tmp, bytes).await?;
    tokio::fs::rename(&tmp, path).await?;
    Ok(())
}

fn sanitize_version(version: &str) -> String {
    version
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makes_neoforge_removal_steps_optional() {
        // Excerpts from misode's `neoforge.mcdoc`.
        let src = "struct RemoveFeatures {\n\tsteps: (DecorationStep | [DecorationStep]),\n}\nstruct AddCarvers {\n\t#[until=\"1.21.2\"]\n\tstep: CarveStep,\n}\nstruct RemoveCarvers {\n\t#[until=\"1.21.2\"]\n\tstep: (CarveStep | [CarveStep]),\n}\n";
        let patched = patch_partner_mcdoc("neoforge", src.into());
        assert!(patched.contains("\tsteps?: (DecorationStep | [DecorationStep]),"));
        assert!(patched.contains("\tstep: CarveStep,"), "add_carvers keeps its required single step");
        assert!(patched.contains("\t#[until=\"1.21.2\"]\n\tsteps?: (CarveStep | [CarveStep]),"));
        assert_eq!(patch_partner_mcdoc("create", src.into()), src);
    }

    #[test]
    fn annotates_create_and_lychee_partner_id_fields() {
        let mut tables = SymbolTables::default();
        super::super::parser::parse_and_bind(
            r#"
            struct NBT { Bottle?: string, Potion?: string }
            type Item = struct { #[until="1.21.1"] item: string, #[since="1.21.1"] id: string }
            type ItemOrTag = (struct { item: string } | struct { tag: string })
            type Fluid = struct { #[until="1.21.1"] fluid: string, #[since="1.21.1"] id: string, amount: int }
            type FluidOrTag = (struct { fluid: string } | struct { #[until="1.21.1"] fluidTag: string, #[since="1.21.1"] fluid_tag: string })
            type MixingResult = struct { id: string, count?: int, amount?: int }
            "#,
            "::partner::create",
            &mut tables,
        )
        .unwrap();
        super::super::parser::parse_and_bind(
            r#"dispatch lychee:conditions[is_off_item_cooldown] to struct { item: string }"#,
            "::partner::lychee",
            &mut tables,
        )
        .unwrap();

        bridge_partner_dispatchers(&mut tables, &[]);

        let field_type_of = |ty: &super::super::types::McdocType, name: &str| -> Option<super::super::types::McdocType> {
            fn find(ty: &super::super::types::McdocType, name: &str) -> Option<super::super::types::McdocType> {
                match ty {
                    super::super::types::McdocType::Struct { fields, .. } => fields.iter().find_map(|f| match f {
                        super::super::types::StructField::Pair { key: super::super::types::StructKey::Literal(k), field_type, .. } if k == name => {
                            Some(field_type.clone())
                        }
                        _ => None,
                    }),
                    super::super::types::McdocType::Union { members, .. } => members.iter().find_map(|m| find(m, name)),
                    _ => None,
                }
            }
            find(ty, name)
        };

        let check_id = |ty: &super::super::types::McdocType, expected_reg: &str, expected_tags: Option<super::super::version::TagMode>| {
            let attr = ty.attributes().iter().find(|a| a.name == "id").expect("expected #[id]");
            let cfg = super::super::version::parse_id_attribute(attr).unwrap();
            assert_eq!(cfg.registry.as_deref(), Some(expected_reg));
            assert_eq!(cfg.tags, expected_tags);
        };

        let item = &tables.mcdoc["::partner::create::Item"];
        check_id(&field_type_of(item, "item").unwrap(), "item", None);
        check_id(&field_type_of(item, "id").unwrap(), "item", None);

        let iot = &tables.mcdoc["::partner::create::ItemOrTag"];
        check_id(&field_type_of(iot, "item").unwrap(), "item", None);
        check_id(&field_type_of(iot, "tag").unwrap(), "item", Some(super::super::version::TagMode::Implicit));

        let fluid = &tables.mcdoc["::partner::create::Fluid"];
        check_id(&field_type_of(fluid, "fluid").unwrap(), "fluid", None);
        check_id(&field_type_of(fluid, "id").unwrap(), "fluid", None);

        let fot = &tables.mcdoc["::partner::create::FluidOrTag"];
        check_id(&field_type_of(fot, "fluid").unwrap(), "fluid", None);
        check_id(&field_type_of(fot, "fluidTag").unwrap(), "fluid", Some(super::super::version::TagMode::Implicit));
        check_id(&field_type_of(fot, "fluid_tag").unwrap(), "fluid", Some(super::super::version::TagMode::Implicit));

        let nbt = &tables.mcdoc["::partner::create::NBT"];
        check_id(&field_type_of(nbt, "Potion").unwrap(), "potion", None);

        let mixing = &tables.mcdoc["::partner::create::MixingResult"];
        let mixing_id = field_type_of(mixing, "id").unwrap();
        let super::super::types::McdocType::Union { members, .. } = mixing_id else {
            panic!("expected union on MixingResult.id, got {mixing_id:?}");
        };
        assert_eq!(members.len(), 2);
        check_id(&members[0], "item", None);
        check_id(&members[1], "fluid", None);

        let lychee_cond = &tables.dispatchers["lychee:conditions"]["is_off_item_cooldown"];
        check_id(&field_type_of(lychee_cond, "item").unwrap(), "item", None);
    }

    #[test]
    fn parses_mcmeta_block_state_summary() {
        let raw = br#"{"oak_stairs":[{"facing":["north","south"],"half":["top","bottom"]},{"facing":"north","half":"bottom"}]}"#;
        let parsed = parse_mcmeta_blocks(raw).unwrap();
        assert_eq!(parsed["oak_stairs"]["facing"], vec!["north", "south"]);
        assert_eq!(parsed["oak_stairs"]["half"], vec!["top", "bottom"]);
    }

    #[tokio::test]
    async fn load_cached_uses_persisted_copy_and_preserves_it_on_failed_refresh() {
        let dir = std::env::temp_dir().join(format!("packraven-fetch-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("schema.mcdoc");
        let original = "struct KeepMe { id: string }\n";
        std::fs::write(&file, original).unwrap();

        // Non-forced load must return the persisted file without touching the unreachable URL.
        let cached = load_cached(&file, "http://127.0.0.1:1/unreachable", false, |b| parse_mcdoc_bytes(b, "test"))
            .await
            .expect("should load local copy");
        assert_eq!(cached, original);

        // Forced refresh against an unreachable URL must fall back to the local copy without wiping it.
        let preserved = load_cached(&file, "http://127.0.0.1:1/unreachable", true, |b| parse_mcdoc_bytes(b, "test"))
            .await
            .expect("failed refresh must preserve local copy");
        assert_eq!(preserved, original);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn load_cached_uses_etag_for_304_and_updates_on_200() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let dir = std::env::temp_dir().join(format!("packraven-fetch-etag-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("schema.mcdoc");

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let responses = [
                ("v1", "struct A { x: int }\n", None),
                ("v1", "", Some("\"v1\"")),
                ("v2", "struct B { y: string }\n", Some("\"v1\"")),
            ];
            for (etag, body, expect_inm) in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 4096];
                let n = socket.read(&mut buf).await.unwrap();
                let req = String::from_utf8_lossy(&buf[..n]);
                if let Some(expected) = expect_inm {
                    assert!(
                        req.to_ascii_lowercase().contains(&format!("if-none-match: {}", expected.to_ascii_lowercase())),
                        "expected If-None-Match {expected}, got:\n{req}"
                    );
                }
                let resp = if body.is_empty() {
                    "HTTP/1.1 304 Not Modified\r\nconnection: close\r\n\r\n".to_string()
                } else {
                    format!(
                        "HTTP/1.1 200 OK\r\netag: \"{etag}\"\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    )
                };
                socket.write_all(resp.as_bytes()).await.unwrap();
            }
        });

        let url = format!("http://{addr}/schema.mcdoc");
        let (first, changed1) = load_cached_with_status(&file, &url, true, None, |b| parse_mcdoc_bytes(b, "test")).await.unwrap();
        assert!(changed1);
        assert!(first.contains("struct A"));

        let (second, changed2) = load_cached_with_status(&file, &url, true, None, |b| parse_mcdoc_bytes(b, "test")).await.unwrap();
        assert!(!changed2);
        assert_eq!(second, first);

        let (third, changed3) = load_cached_with_status(&file, &url, true, None, |b| parse_mcdoc_bytes(b, "test")).await.unwrap();
        assert!(changed3);
        assert!(third.contains("struct B"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), third);

        server.await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn refresh_replaces_a_stale_copy_cached_before_etags() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let dir = std::env::temp_dir().join(format!("packraven-fetch-stale-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("manifest.mcdoc");
        std::fs::write(&file, "struct Old { x: int }\n").unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = socket.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
            assert!(!req.contains("if-none-match"), "sent a conditional request without a stored ETag:\n{req}");
            let body = "struct New { y: string }\n";
            let resp = format!("HTTP/1.1 200 OK\r\netag: \"n\"\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
            socket.write_all(resp.as_bytes()).await.unwrap();
        });

        let url = format!("http://{addr}/manifest.mcdoc");
        let (fresh, changed) = load_cached_with_status(&file, &url, true, Some(BACKGROUND_REFRESH_TIMEOUT), |b| parse_mcdoc_bytes(b, "test"))
            .await
            .unwrap();
        assert!(changed);
        assert!(fresh.contains("struct New"));
        assert_eq!(std::fs::read_to_string(etag_path(&file)).unwrap(), "\"n\"");

        server.await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_muon_manifest_and_misode_partner_path_rules() {
        let manifest_json = r#"{
            "formatVersion": 1,
            "schemas": [{
                "id": "apotheosis",
                "file": "mcdoc/apotheosis.mcdoc",
                "module": "::apotheosis",
                "modIds": ["apotheosis", "placebo"],
                "minMinecraft": "1.21.1",
                "paths": [
                    { "folder": "apotheosis/affixes", "category": "apotheosis:affixes" },
                    { "namespace": "apotheosis", "exact": "data_maps/item/loot_category_overrides", "category": "apotheosis:data_map_loot_category_overrides" }
                ]
            }],
            "recipeDispatcherBridges": ["create:recipes"]
        }"#;
        let manifest: MuonManifest = serde_json::from_str(manifest_json).unwrap();
        assert_eq!(manifest.schemas.len(), 1);
        assert!(manifest.schemas[0].paths[0].matches("mypack", "apotheosis/affixes/sword"));
        assert!(!manifest.schemas[0].paths[0].matches("mypack", "affixes/sword"));
        assert!(manifest.schemas[0].paths[1].matches("apotheosis", "data_maps/item/loot_category_overrides"));
        assert!(!manifest.schemas[0].paths[1].matches("minecraft", "data_maps/item/loot_category_overrides"));

        let misode_json = r#"{
            "generators": [
                { "id": "thermoo:temperature_effect", "path": "thermoo/temperature_effect", "dependency": "thermoo", "minVersion": "1.20" },
                { "id": "create:recipes", "path": "recipe", "dependency": "create" },
                { "id": "neoforge:data_map_furnace_fuels", "path": "data_maps/item/furnace_fuels", "dependency": "neoforge" }
            ]
        }"#;
        let cfg: MisodeConfig = serde_json::from_str(misode_json).unwrap();
        let active = vec!["thermoo".into(), "create".into(), "neoforge".into()];
        let rules = misode_partner_path_rules(&cfg, &active, Some("1.21.1"));
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].category, "thermoo:temperature_effect");
        assert!(rules[0].matches("mypack", "thermoo/temperature_effect/frost"));
    }

    #[test]
    fn parses_mcmeta_versions_snapshots_and_game_rules() {
        let versions: Vec<McmetaVersion> = serde_json::from_str(
            r#"[
                {"id": "1.20.6", "type": "release", "data_version": 3839, "data_pack_version": 41},
                {"id": "24w14a", "type": "snapshot", "data_version": 3845, "data_pack_version": 38},
                {"id": "1.21", "type": "release", "data_version": 3953, "data_pack_version": 48},
                {"id": "1.21.1", "type": "release", "data_version": 3955, "data_pack_version": 48}
            ]"#,
        )
        .unwrap();
        assert_eq!(data_pack_formats_from_versions(&versions), vec![(48, "1.21.1".into()), (41, "1.20.6".into())]);
        assert_eq!(resolve_snapshot_version("24w14a", &versions).as_deref(), Some("1.21"));
        assert_eq!(resolve_snapshot_version("1.21.1", &versions), None);

        let brigadier = br#"{"children":{"gamerule":{"children":{"keepInventory":{"children":{"value":{"type":"argument","parser":"brigadier:bool"}}},"randomTickSpeed":{"children":{"value":{"type":"argument","parser":"brigadier:integer"}}}}}}}"#;
        let rules = parse_mcmeta_game_rules(brigadier).unwrap();
        assert_eq!(rules.boolean, vec!["keepInventory"]);
        assert_eq!(rules.int, vec!["randomTickSpeed"]);

        let spyglass_fmt = br#"{"children":{"gamerule":{"children":{"doFireTick":{"children":{"value":{"type":"argument","argument":{"type":"minecraft:bool"}}}},"spawnRadius":{"children":{"value":{"type":"argument","argument":{"type":"minecraft:integer"}}}}}}}}"#;
        let rules2 = parse_mcmeta_game_rules(spyglass_fmt).unwrap();
        assert_eq!(rules2.boolean, vec!["doFireTick"]);
        assert_eq!(rules2.int, vec!["spawnRadius"]);
    }
}
