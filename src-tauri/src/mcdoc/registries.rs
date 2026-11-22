#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use walkdir::WalkDir;
use zip::ZipArchive;

use crate::global_packs::{self, PackKind};
use crate::pack::Lockfile;
use crate::probejs::ProbeIndex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CodeRegistrySource {
    #[default]
    VanillaOnly,
    ProbeJs,
    ProbeRaven,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdLookup {
    Known,
    /// ID not found in a data-driven registry (where jar + pack scanning is authoritative).
    UnknownAuthoritative,
    /// ID not found in a code-registered registry and we have no full mod dump for this pack.
    UnknownUnverified,
}

/// JSON format written by ProbeRaven (`.proberaven/dump.json`).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeRavenDump {
    #[serde(default)]
    pub format_version: u32,
    #[serde(default)]
    pub minecraft: String,
    #[serde(default)]
    pub loader: String,
    #[serde(default)]
    pub mods: Vec<String>,
    /// Registry ID without `minecraft:` prefix (e.g. `item`, `block`, `worldgen/biome`) -> sorted IDs.
    #[serde(default)]
    pub registries: HashMap<String, Vec<String>>,
    /// Tag registry without `minecraft:` prefix (e.g. `item`, `block`) -> sorted tag IDs (no leading `#`).
    #[serde(default)]
    pub tags: HashMap<String, Vec<String>>,
    /// Block ID -> property name -> allowed values.
    #[serde(default)]
    pub block_states: HashMap<String, BTreeMap<String, Vec<String>>>,
}

/// The registry names a pack has: vanilla's for its Minecraft version (from misode's mcmeta
/// summary) plus modded ones from a ProbeRaven dump. Minecraft writes a vanilla registry's tags to
/// `tags/<path>/` and a modded registry's to `tags/<namespace>/<path>/`, so reading a tag path back
/// needs these names to tell `tags/item/foo/` (tag `foo/…` in `item`) from `tags/irons_spellbooks/spells/`.
#[derive(Default, Clone, Debug)]
pub struct KnownRegistries(HashSet<String>);

impl KnownRegistries {
    pub fn insert(&mut self, registry: &str) {
        self.0.insert(normalize_category(registry));
    }

    /// Returns whether `tags/<first>/<second>/` holds a modded registry's tags. Without any names
    /// (offline before the first fetch) we read every folder as a vanilla registry.
    fn names_modded_tag_folder(&self, first: &str, second: &str) -> bool {
        if self.0.contains(&format!("{first}:{second}")) {
            return true;
        }
        // Function tags are the one tag folder that isn't a registry.
        let vanilla = matches!(first, "function" | "functions") || self.0.contains(singularize_tag_folder(first));
        !self.0.is_empty() && !vanilla
    }
}

#[derive(Debug, Clone, Default)]
pub struct RegistryIndex {
    /// Category (e.g. `item`, `block`, `tag/item`, `recipe`, `loot_table`, `worldgen/biome`) -> sorted full IDs (`ns:path`).
    pub entries: HashMap<String, Vec<String>>,
    /// Block ID (`ns:path`) -> property name -> allowed values.
    pub block_states: HashMap<String, BTreeMap<String, Vec<String>>>,
    /// Installed mod IDs (from ProbeRaven dump or jar namespaces).
    pub mod_ids: HashSet<String>,
    pub code_source: CodeRegistrySource,
    pub known: KnownRegistries,
}

impl RegistryIndex {
    pub fn insert_entries(&mut self, category: &str, ids: impl IntoIterator<Item = String>) {
        let cat = normalize_category(category);
        let list = self.entries.entry(cat).or_default();
        for id in ids {
            list.push(lengthen_id(&id));
        }
    }

    pub fn finalize(&mut self) {
        for list in self.entries.values_mut() {
            list.sort_unstable();
            list.dedup();
        }
    }

    pub fn apply_vanilla_registries(&mut self, registries: HashMap<String, Vec<String>>) {
        for (reg, ids) in registries {
            self.known.insert(&reg);
            self.insert_entries(&reg, ids);
        }
    }

    pub fn apply_vanilla_block_states(&mut self, states: HashMap<String, BTreeMap<String, Vec<String>>>) {
        for (block, props) in states {
            self.block_states.entry(lengthen_id(&block)).or_insert(props);
        }
    }

    pub fn apply_probejs(&mut self, probe: &ProbeIndex) {
        let regs = probe.registries();
        if regs.is_empty() {
            return;
        }
        for (category, ids) in regs {
            self.insert_entries(&category, ids);
        }
        if self.code_source == CodeRegistrySource::VanillaOnly {
            self.code_source = CodeRegistrySource::ProbeJs;
        }
    }

    pub fn apply_proberaven(&mut self, dump: ProbeRavenDump) {
        for mod_id in dump.mods {
            self.mod_ids.insert(mod_id);
        }
        for (reg, ids) in dump.registries {
            self.known.insert(&reg);
            self.insert_entries(&reg, ids);
        }
        for (tag_reg, ids) in dump.tags {
            let cat = format!("tag/{}", normalize_category(&tag_reg));
            self.insert_entries(&cat, ids);
        }
        for (block, props) in dump.block_states {
            self.block_states.insert(lengthen_id(&block), props);
        }
        self.code_source = CodeRegistrySource::ProbeRaven;
    }

    /// Looks up `raw` (optionally prefixed with `#` for tags) in `category`.
    pub fn lookup(&self, category: &str, raw: &str, allow_tag: bool, require_tag: bool) -> IdLookup {
        let (is_tag, body) = match raw.strip_prefix('#') {
            Some(rest) => (true, rest),
            None => (false, raw),
        };
        if is_tag && !allow_tag && !require_tag {
            return IdLookup::UnknownAuthoritative;
        }
        if require_tag && !is_tag {
            return IdLookup::UnknownAuthoritative;
        }
        let cat = normalize_category(category);
        let lookup_cat = if is_tag { format!("tag/{cat}") } else { cat.clone() };
        let full = lengthen_id(body);

        if let Some(list) = self.entries.get(&lookup_cat)
            && list.binary_search(&full).is_ok()
        {
            return IdLookup::Known;
        }

        if is_tag || is_data_driven_category(&cat) {
            IdLookup::UnknownAuthoritative
        } else {
            match self.code_source {
                CodeRegistrySource::ProbeRaven => IdLookup::UnknownAuthoritative,
                CodeRegistrySource::VanillaOnly | CodeRegistrySource::ProbeJs => {
                    // ProbeJS dumps most registries, but modded custom registries can still be absent.
                    if self.code_source == CodeRegistrySource::ProbeJs && self.entries.contains_key(&lookup_cat) && full.starts_with("minecraft:") {
                        IdLookup::UnknownAuthoritative
                    } else {
                        IdLookup::UnknownUnverified
                    }
                }
            }
        }
    }

    /// Returns ranked prefix/substring completions for `category`, mirroring `probejs/query.rs`.
    pub fn complete(&self, category: &str, allow_tag: bool, require_tag: bool, typed: &str, limit: usize) -> Vec<String> {
        let cat = normalize_category(category);
        let typed_lower = typed.to_lowercase();
        let typed_is_tag = typed_lower.starts_with('#');
        let mut starts = Vec::new();
        let mut contains = Vec::new();

        if require_tag || (allow_tag && typed_is_tag) {
            let tag_cat = format!("tag/{cat}");
            let needle = typed_lower.strip_prefix('#').unwrap_or(&typed_lower);
            self.scan_completions(&tag_cat, "#", needle, limit, &mut starts, &mut contains);
        } else {
            self.scan_completions(&cat, "", &typed_lower, limit, &mut starts, &mut contains);
            if allow_tag && typed_lower.is_empty() && starts.len() < limit {
                let tag_cat = format!("tag/{cat}");
                self.scan_completions(&tag_cat, "#", "", limit, &mut starts, &mut contains);
            }
        }

        starts.extend(contains);
        starts.truncate(limit);
        starts
    }

    fn scan_completions(
        &self,
        lookup_cat: &str,
        prefix: &str,
        needle: &str,
        limit: usize,
        starts: &mut Vec<String>,
        contains: &mut Vec<String>,
    ) {
        let Some(list) = self.entries.get(lookup_cat) else { return };
        for id in list {
            let lower = id.to_lowercase();
            let short = lower.strip_prefix("minecraft:");
            let is_start = needle.is_empty()
                || lower.starts_with(needle)
                || short.is_some_and(|s| s.starts_with(needle));
            let is_match = is_start || lower.contains(needle);
            if !is_match {
                continue;
            }
            let formatted = if prefix.is_empty() { id.clone() } else { format!("{prefix}{id}") };
            if is_start {
                starts.push(formatted);
            } else if contains.len() < limit {
                contains.push(formatted);
            }
            if needle.is_empty() && starts.len() >= limit {
                break;
            }
        }
    }
}

/// Scans `mods/*.jar` in parallel for `data/<ns>/<category>/...json` entries and discovered mod namespaces.
pub fn scan_mod_jars_data(root: &Path, known: &KnownRegistries) -> (HashMap<String, Vec<String>>, HashSet<String>) {
    let jars: Vec<PathBuf> = std::fs::read_dir(root.join("mods"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("jar")))
        .collect();
    if jars.is_empty() {
        return (HashMap::new(), HashSet::new());
    }

    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(8).min(jars.len());
    let chunk_size = jars.len().div_ceil(threads);
    let partials: Vec<(HashMap<String, Vec<String>>, HashSet<String>)> = std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(threads);
        for slice in jars.chunks(chunk_size) {
            handles.push(scope.spawn(move || {
                let mut local: HashMap<String, Vec<String>> = HashMap::new();
                let mut namespaces: HashSet<String> = HashSet::new();
                for jar_path in slice {
                    scan_single_jar(jar_path, known, &mut local, Some(&mut namespaces));
                }
                (local, namespaces)
            }));
        }
        handles.into_iter().map(|h| h.join().unwrap_or_default()).collect()
    });

    let mut merged: HashMap<String, Vec<String>> = HashMap::new();
    let mut mod_ids: HashSet<String> = HashSet::new();
    for (partial, ns) in partials {
        mod_ids.extend(ns);
        for (cat, ids) in partial {
            merged.entry(cat).or_default().extend(ids);
        }
    }
    for ids in merged.values_mut() {
        ids.sort_unstable();
        ids.dedup();
    }
    (merged, mod_ids)
}

/// Scans NeoForge/Forge universal jars in `install_dir/libraries` for built-in `c:` and `forge:` tags.
pub fn scan_loader_jars_data(install_dir: &Path, lock: Option<&Lockfile>, known: &KnownRegistries) -> HashMap<String, Vec<String>> {
    let Some(lock) = lock else { return HashMap::new() };
    let Ok(loader) = lock.mod_loader() else { return HashMap::new() };
    let libraries = install_dir.join("libraries");
    let mut candidates = Vec::new();
    match loader {
        crate::pack::ModLoader::NeoForge(ver) => {
            candidates.push(libraries.join("net/neoforged/neoforge").join(ver).join(format!("neoforge-{ver}-universal.jar")));
        }
        crate::pack::ModLoader::Forge(ver) => {
            if let Some(mc) = lock.minecraft.as_deref() {
                let full = format!("{mc}-{ver}");
                candidates.push(libraries.join("net/minecraftforge/forge").join(&full).join(format!("forge-{full}-universal.jar")));
            }
        }
        crate::pack::ModLoader::Fabric(_) => {}
    }

    let mut out = HashMap::new();
    for jar in candidates {
        if jar.is_file() {
            scan_single_jar(&jar, known, &mut out, None);
        }
    }
    for ids in out.values_mut() {
        ids.sort_unstable();
        ids.dedup();
    }
    out
}

fn scan_single_jar(path: &Path, known: &KnownRegistries, out: &mut HashMap<String, Vec<String>>, mut namespaces: Option<&mut HashSet<String>>) {
    let Ok(file) = File::open(path) else { return };
    let Ok(mut zip) = ZipArchive::new(file) else { return };
    if let Some(ns_set) = namespaces.as_deref_mut() {
        ns_set.extend(crate::mod_manifest::declared_ids(&mut zip));
    }
    for i in 0..zip.len() {
        let Ok(entry) = zip.by_index_raw(i) else { continue };
        let Ok(name) = entry.name() else { continue };
        if let Some(ns_set) = namespaces.as_deref_mut()
            && let Some(rest) = name.strip_prefix("data/").or_else(|| name.strip_prefix("assets/"))
            && let Some((ns, _)) = rest.split_once('/')
            && !ns.is_empty()
            && ns != "minecraft"
            && ns != "c"
            && ns != "forge"
        {
            ns_set.insert(ns.to_string());
        }
        // Data files like painting variants name textures that ship in the same jar's assets.
        if let Some(texture) = name.strip_prefix("assets/").and_then(|rest| {
            let (namespace, path) = rest.split_once("/textures/")?;
            Some(format!("{namespace}:{}", path.strip_suffix(".png")?))
        }) {
            out.entry("texture".into()).or_default().push(texture);
            continue;
        }
        if !name.starts_with("data/") {
            continue;
        }
        if let Some((cat, id)) = parse_data_file_path(&name, known).or_else(|| parse_non_json_data_path(&name)) {
            out.entry(cat).or_default().push(id);
        }
    }
}

/// Maps the data files that aren't JSON but still define IDs: structure templates (`.nbt`) and functions (`.mcfunction`).
fn parse_non_json_data_path(path: &str) -> Option<(String, String)> {
    let after_data = path.strip_prefix("data/").or_else(|| path.find("/data/").map(|i| &path[i + 6..]))?;
    let (namespace, rest) = after_data.split_once('/')?;
    let (folder, file) = rest.split_once('/')?;
    let (category, id) = match folder {
        "structure" | "structures" => ("structure", file.strip_suffix(".nbt")?),
        "function" | "functions" => ("function", file.strip_suffix(".mcfunction")?),
        _ => return None,
    };
    (!namespace.is_empty() && !id.is_empty()).then(|| (category.to_string(), format!("{namespace}:{id}")))
}

/// Scans live datapack files in the instance (`datapacks/`, `kubejs/data/`, and global pack loaders).
pub fn scan_pack_data(root: &Path, lock: Option<&Lockfile>, known: &KnownRegistries) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    let mut roots: Vec<PathBuf> = Vec::new();

    if root.join("data").is_dir() {
        roots.push(root.to_path_buf());
    }
    if root.join("kubejs/data").is_dir() {
        roots.push(root.join("kubejs"));
    }
    for loader in global_packs::loaders(root, lock, PackKind::Data) {
        for folder in loader.folders {
            for pack in folder.packs {
                let p = root.join(&pack.path);
                if p.is_dir() {
                    roots.push(p);
                } else if pack.zipped && p.is_file() {
                    scan_single_jar(&p, known, &mut out, None);
                }
            }
        }
    }
    let datapacks = root.join("datapacks");
    if datapacks.is_dir() {
        for entry in std::fs::read_dir(&datapacks).into_iter().flatten().flatten() {
            let p = entry.path();
            if p.is_dir() {
                roots.push(p);
            } else if p.is_file() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")) {
                scan_single_jar(&p, known, &mut out, None);
            }
        }
    }

    let mut seen_roots = HashSet::new();
    for pack_root in roots {
        if !seen_roots.insert(pack_root.clone()) {
            continue;
        }
        let data_dir = pack_root.join("data");
        if !data_dir.is_dir() {
            continue;
        }
        for entry in WalkDir::new(&data_dir).into_iter().flatten() {
            if !entry.file_type().is_file() {
                continue;
            }
            let Ok(rel) = entry.path().strip_prefix(&pack_root) else { continue };
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            if let Some((cat, id)) = parse_data_file_path(&rel_str, known).or_else(|| parse_non_json_data_path(&rel_str)) {
                out.entry(cat).or_default().push(id);
            }
        }
    }

    for ids in out.values_mut() {
        ids.sort_unstable();
        ids.dedup();
    }
    out
}

/// Loads `.proberaven/dump.json` from the pack instance directory if present.
pub fn read_proberaven_dump(root: &Path) -> Option<ProbeRavenDump> {
    for rel in [".proberaven/dump.json", "proberaven/dump.json"] {
        let path = root.join(rel);
        if path.is_file()
            && let Ok(mut file) = File::open(&path)
        {
            let mut buf = String::new();
            if file.read_to_string(&mut buf).is_ok()
                && let Ok(dump) = serde_json::from_str::<ProbeRavenDump>(&buf)
            {
                return Some(dump);
            }
        }
    }
    None
}

/// Maps a path like `data/create/recipe/cutting/wood.json` or `data/c/tags/item/ingots/iron.json`
/// to `(category, "namespace:id")`. Normalizes pre-1.21 plural folders (`recipes`, `tags/items`, etc.).
pub fn parse_data_file_path(path: &str, known: &KnownRegistries) -> Option<(String, String)> {
    let norm = path.replace('\\', "/");
    let after_data = norm
        .strip_prefix("data/")
        .or_else(|| norm.find("/data/").map(|i| &norm[i + 6..]))?;
    let stripped = after_data.strip_suffix(".json")?;
    let (namespace, rest) = stripped.split_once('/')?;
    if namespace.is_empty() || rest.is_empty() {
        return None;
    }

    // Tags: `tags/<registry>/<id>` (including nested registries like `tags/worldgen/biome/<id>`).
    if let Some(tag_rest) = rest.strip_prefix("tags/") {
        if let Some(wg_rest) = tag_rest.strip_prefix("worldgen/") {
            let (wg_sub, id) = wg_rest.split_once('/')?;
            return Some((format!("tag/worldgen/{wg_sub}"), format!("{namespace}:{id}")));
        }
        let (reg, id) = tag_rest.split_once('/')?;
        if let Some((reg_path, modded_id)) = id.split_once('/')
            && known.names_modded_tag_folder(reg, reg_path)
        {
            return Some((format!("tag/{reg}:{reg_path}"), format!("{namespace}:{modded_id}")));
        }
        let reg = singularize_tag_folder(reg);
        return Some((format!("tag/{reg}"), format!("{namespace}:{id}")));
    }

    // Worldgen: `worldgen/<subcat>/<id>`.
    if let Some(wg_rest) = rest.strip_prefix("worldgen/") {
        let (subcat, id) = wg_rest.split_once('/')?;
        return Some((format!("worldgen/{subcat}"), format!("{namespace}:{id}")));
    }

    // NeoForge datapack categories: biome/structure modifiers and built-in data maps.
    if namespace == "neoforge"
        && let Some(dm_rest) = rest.strip_prefix("data_maps/")
        && let Some(cat) = neoforge_data_map_category(dm_rest)
    {
        return Some((cat.into(), format!("{namespace}:{dm_rest}")));
    }
    if let Some(nf_rest) = rest.strip_prefix("neoforge/") {
        if let Some(id) = nf_rest.strip_prefix("biome_modifier/") {
            return Some(("neoforge:biome_modifier".into(), format!("{namespace}:{id}")));
        }
        if let Some(id) = nf_rest.strip_prefix("structure_modifier/") {
            return Some(("neoforge:structure_modifier".into(), format!("{namespace}:{id}")));
        }
        if let Some(dm_rest) = nf_rest.strip_prefix("data_maps/") {
            if let Some(cat) = neoforge_data_map_category(dm_rest) {
                return Some((cat.into(), format!("{namespace}:{dm_rest}")));
            }
        }
    }

    // Apotheosis data maps (`data/apotheosis/data_maps/...`).
    if namespace == "apotheosis"
        && let Some(dm_rest) = rest.strip_prefix("data_maps/")
        && let Some(cat) = apotheosis_data_map_category(dm_rest)
    {
        return Some((cat.into(), format!("{namespace}:{dm_rest}")));
    }

    // Apotheosis datapack categories (both 1.21 flat `data/<ns>/<folder>/` and 26.1 namespaced `data/<ns>/apotheosis/<folder>/`).
    if let Some(apoth_rest) = rest.strip_prefix("apotheosis/")
        && let Some((folder, id)) = apoth_rest.split_once('/')
        && let Some(cat) = apotheosis_data_category(folder)
    {
        return Some((cat.into(), format!("{namespace}:{id}")));
    }
    if let Some((folder, id)) = rest.split_once('/')
        && let Some(cat) = apotheosis_data_category(folder)
    {
        return Some((cat.into(), format!("{namespace}:{id}")));
    }

    // Placebo gear sets (1.21 `data/<ns>/gear_sets/` and 26.1 `data/<ns>/placebo/gear_sets/`).
    if let Some(id) = rest.strip_prefix("placebo/gear_sets/").or_else(|| rest.strip_prefix("gear_sets/")) {
        return Some(("placebo:gear_sets".into(), format!("{namespace}:{id}")));
    }

    // Modded partner top-level categories.
    if let Some(id) = rest.strip_prefix("planets/") {
        return Some(("ad_astra:planets".into(), format!("{namespace}:{id}")));
    }
    if let Some(id) = rest.strip_prefix("block_growths/") {
        return Some(("immersive_weathering:block_growth".into(), format!("{namespace}:{id}")));
    }

    // Standard single-segment datapack category: `<category>/<id>`.
    let (folder, id) = rest.split_once('/')?;
    let category = singularize_data_folder(folder);
    Some((category.to_string(), format!("{namespace}:{id}")))
}

fn neoforge_data_map_category(dm_path: &str) -> Option<&'static str> {
    match dm_path {
        "entity_type/acceptable_villager_distances" => Some("neoforge:data_map_acceptable_villager_distances"),
        "item/compostables" => Some("neoforge:data_map_compostables"),
        "item/furnace_fuels" => Some("neoforge:data_map_furnace_fuels"),
        "entity_type/monster_room_mobs" => Some("neoforge:data_map_monster_room_mobs"),
        "block/oxidizables" => Some("neoforge:data_map_oxidizables"),
        "entity_type/parrot_imitations" => Some("neoforge:data_map_parrot_imitations"),
        "villager_profession/raid_hero_gifts" => Some("neoforge:data_map_raid_hero_gifts"),
        "block/strippables" => Some("neoforge:data_map_strippables"),
        "game_event/vibration_frequencies" => Some("neoforge:data_map_vibration_frequencies"),
        "worldgen/biome/villager_types" => Some("neoforge:data_map_villager_types"),
        "block/waxables" => Some("neoforge:data_map_waxables"),
        _ => None,
    }
}

fn apotheosis_data_map_category(dm_path: &str) -> Option<&'static str> {
    match dm_path {
        "dimension_type/invader_spawn_rules" => Some("apotheosis:data_map_invader_spawn_rules"),
        "item/loot_category_overrides" => Some("apotheosis:data_map_loot_category_overrides"),
        _ => None,
    }
}

fn apotheosis_data_category(folder: &str) -> Option<&'static str> {
    match folder {
        "affixes" => Some("apotheosis:affixes"),
        "gems" => Some("apotheosis:gems"),
        "extra_gem_bonuses" => Some("apotheosis:extra_gem_bonuses"),
        "purity_weights" => Some("apotheosis:purity_weights"),
        "affix_loot_entries" => Some("apotheosis:affix_loot_entries"),
        "rarities" => Some("apotheosis:rarities"),
        "rarity_override" => Some("apotheosis:rarity_override"),
        "apothic_invaders" => Some("apotheosis:apothic_invaders"),
        "apothic_elites" => Some("apotheosis:apothic_elites"),
        "apothic_augments" => Some("apotheosis:apothic_augments"),
        "rogue_spawners" => Some("apotheosis:rogue_spawners"),
        "tier_augments" => Some("apotheosis:tier_augments"),
        _ => None,
    }
}

fn singularize_data_folder(folder: &str) -> &str {
    match folder {
        "advancements" => "advancement",
        "recipes" => "recipe",
        "loot_tables" => "loot_table",
        "predicates" => "predicate",
        "item_modifiers" => "item_modifier",
        "functions" => "function",
        "structures" => "structure",
        _ => folder,
    }
}

fn singularize_tag_folder(folder: &str) -> &str {
    match folder {
        "blocks" => "block",
        "items" => "item",
        "fluids" => "fluid",
        "entity_types" => "entity_type",
        "game_events" => "game_event",
        "functions" => "function",
        _ => folder,
    }
}

pub fn normalize_category(category: &str) -> String {
    category.strip_prefix("minecraft:").unwrap_or(category).to_string()
}

pub fn lengthen_id(id: &str) -> String {
    if id.contains(':') {
        id.to_string()
    } else {
        format!("minecraft:{id}")
    }
}

fn is_data_driven_category(cat: &str) -> bool {
    matches!(
        cat,
        "advancement"
            | "recipe"
            | "loot_table"
            | "predicate"
            | "item_modifier"
            | "dimension"
            | "dimension_type"
            | "damage_type"
            | "enchantment"
            | "enchantment_provider"
            | "banner_pattern"
            | "chat_type"
            | "instrument"
            | "jukebox_song"
            | "painting_variant"
            | "trim_material"
            | "trim_pattern"
            | "wolf_variant"
            | "wolf_sound_variant"
            | "cat_variant"
            | "chicken_variant"
            | "cow_variant"
            | "frog_variant"
            | "pig_variant"
            | "dialog"
            | "trial_spawner"
            | "apotheosis:affixes"
            | "apotheosis:gems"
            | "apotheosis:extra_gem_bonuses"
            | "apotheosis:purity_weights"
            | "apotheosis:affix_loot_entries"
            | "apotheosis:rarities"
            | "apotheosis:rarity_override"
            | "apotheosis:apothic_invaders"
            | "apotheosis:apothic_elites"
            | "apotheosis:apothic_augments"
            | "apotheosis:rogue_spawners"
            | "apotheosis:tier_augments"
            | "placebo:gear_sets"
    ) || cat.starts_with("worldgen/")
        || cat.starts_with("tag/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    fn test_known() -> KnownRegistries {
        let mut known = KnownRegistries::default();
        for name in ["item", "block", "entity_type", "worldgen/biome", "recipe"] {
            known.insert(name);
        }
        known
    }

    #[test]
    fn reads_tag_folders_from_known_registry_names() {
        let path = "data/x/tags/irons_spellbooks/spells/fire.json";
        // A dump names the modded registry outright.
        let mut dumped = test_known();
        dumped.insert("irons_spellbooks:spells");
        assert_eq!(parse_data_file_path(path, &dumped), Some(("tag/irons_spellbooks:spells".into(), "x:fire".into())));
        // Vanilla names alone still show the folder isn't a vanilla registry.
        assert_eq!(parse_data_file_path(path, &test_known()), Some(("tag/irons_spellbooks:spells".into(), "x:fire".into())));
        // Function tags aren't a registry but are vanilla.
        assert_eq!(parse_data_file_path("data/x/tags/function/load.json", &test_known()), Some(("tag/function".into(), "x:load".into())));
        // Offline with no names, we fall back to reading every folder as a vanilla registry.
        assert_eq!(parse_data_file_path(path, &KnownRegistries::default()), Some(("tag/irons_spellbooks".into(), "x:spells/fire".into())));
    }

    #[test]
    fn maps_modded_tag_folders_templates_and_functions() {
        // A modded registry's tags sit under `tags/<registry namespace>/<registry path>/`.
        assert_eq!(
            parse_data_file_path("data/aces/tags/irons_spellbooks/spells/slash_like_spell.json", &test_known()),
            Some(("tag/irons_spellbooks:spells".into(), "aces:slash_like_spell".into()))
        );
        assert_eq!(
            parse_data_file_path("data/c/tags/item/armors/boots.json", &test_known()),
            Some(("tag/item".into(), "c:armors/boots".into()))
        );
        assert_eq!(
            parse_non_json_data_path("data/aether_villages/structure/olympic_citadel/villager_0.nbt"),
            Some(("structure".into(), "aether_villages:olympic_citadel/villager_0".into()))
        );
        assert_eq!(
            parse_non_json_data_path("data/true_ending/function/load.mcfunction"),
            Some(("function".into(), "true_ending:load".into()))
        );
        assert_eq!(parse_non_json_data_path("data/x/recipe/readme.txt"), None);
    }

    #[test]
    fn parses_datapack_file_paths_across_versions() {
        assert_eq!(
            parse_data_file_path("data/create/recipe/cutting/oak.json", &test_known()),
            Some(("recipe".into(), "create:cutting/oak".into()))
        );
        assert_eq!(
            parse_data_file_path("data/minecraft/recipes/stick.json", &test_known()),
            Some(("recipe".into(), "minecraft:stick".into()))
        );
        assert_eq!(
            parse_data_file_path("data/c/tags/item/ingots/iron.json", &test_known()),
            Some(("tag/item".into(), "c:ingots/iron".into()))
        );
        assert_eq!(
            parse_data_file_path("data/forge/tags/items/ingots/iron.json", &test_known()),
            Some(("tag/item".into(), "forge:ingots/iron".into()))
        );
        assert_eq!(
            parse_data_file_path("kubejs/data/pack/worldgen/biome/ash.json", &test_known()),
            Some(("worldgen/biome".into(), "pack:ash".into()))
        );
        assert_eq!(
            parse_data_file_path("datapacks/mine/data/c/tags/worldgen/biome/is_hot.json", &test_known()),
            Some(("tag/worldgen/biome".into(), "c:is_hot".into()))
        );
    }

    #[test]
    fn scans_mod_jars_and_ranks_completions() {
        let root = std::env::temp_dir().join(format!("packraven-mcdoc-reg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("mods")).unwrap();

        let jar_path = root.join("mods/example.jar");
        let mut zip = ZipWriter::new(File::create(&jar_path).unwrap());
        for name in [
            "data/create/recipe/brass_ingot.json",
            "data/c/tags/item/ingots/brass.json",
            "assets/create/models/item/brass_ingot.json",
        ] {
            zip.start_file(name, SimpleFileOptions::default()).unwrap();
            std::io::Write::write_all(&mut zip, b"{}").unwrap();
        }
        zip.finish().unwrap();

        let mut index = RegistryIndex::default();
        let (jar_entries, jar_mods) = scan_mod_jars_data(&root, &KnownRegistries::default());
        assert!(jar_mods.contains("create"));
        for (cat, ids) in jar_entries {
            index.insert_entries(&cat, ids);
        }
        index.insert_entries("item", ["minecraft:diamond".into(), "create:brass_ingot".into()]);
        index.finalize();

        assert_eq!(index.lookup("recipe", "create:brass_ingot", false, false), IdLookup::Known);
        assert_eq!(index.lookup("recipe", "create:missing", false, false), IdLookup::UnknownAuthoritative);
        assert_eq!(index.lookup("item", "#c:ingots/brass", true, false), IdLookup::Known);
        assert_eq!(index.lookup("item", "create:unknown_item", false, false), IdLookup::UnknownUnverified);

        assert_eq!(index.complete("item", true, false, "diam", 10), vec!["minecraft:diamond"]);
        assert_eq!(index.complete("item", true, false, "#c:ing", 10), vec!["#c:ingots/brass"]);

        let _ = std::fs::remove_dir_all(&root);
    }
}
