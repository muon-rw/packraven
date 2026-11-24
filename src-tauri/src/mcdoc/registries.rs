#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use walkdir::WalkDir;
use zip::ZipArchive;

use super::json_parser;
use super::overlays::{self, OverlayEnv};
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

    pub fn contains(&self, registry: &str) -> bool {
        self.0.contains(&normalize_category(registry))
    }

    /// Matches a known multi-segment modded registry prefix in `tags/<ns>/<reg_path...>/<id>`
    /// (e.g. `tags/mod/worldgen/foo/bar.json` -> `("mod:worldgen/foo", "bar")`), preferring the longest match.
    fn match_modded_tag_folder<'a>(&self, tag_rest: &'a str) -> Option<(String, &'a str)> {
        let (ns, rest) = tag_rest.split_once('/')?;
        let mut best: Option<(String, &'a str)> = None;
        for (idx, b) in rest.bytes().enumerate() {
            if b == b'/' {
                let reg_path = &rest[..idx];
                let candidate = format!("{ns}:{reg_path}");
                if self.0.contains(&candidate) {
                    best = Some((candidate, &rest[idx + 1..]));
                }
            }
        }
        best
    }

    /// Returns whether `tags/<first>/<second>/` holds a modded registry's tags. Without any names
    /// (offline before the first fetch) we read every folder as a vanilla registry.
    fn names_modded_tag_folder(&self, first: &str, second: &str) -> bool {
        if self.0.contains(&format!("{first}:{second}")) {
            return true;
        }
        let singular = singularize_tag_folder(first);
        // Never misclassify known vanilla categories/registries (even if absent from an older mcmeta summary).
        let vanilla = matches!(first, "function" | "functions")
            || self.0.contains(singular)
            || is_data_driven_category(singular)
            || matches!(
                singular,
                "block"
                    | "entity_type"
                    | "fluid"
                    | "game_event"
                    | "item"
                    | "mob_effect"
                    | "potion"
                    | "sound_event"
                    | "villager_profession"
                    | "point_of_interest_type"
            );
        !self.0.is_empty() && !vanilla
    }
}

#[derive(Debug, Clone, Default)]
pub struct ModelTextureInfo {
    pub parent: Option<String>,
    pub slots: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ScanExtras {
    pub translation_keys: Vec<String>,
    pub deprecated_translation_keys: HashSet<String>,
    pub lang_codes: Vec<String>,
    pub advancement_criteria: HashMap<String, Vec<String>>,
    pub model_textures: HashMap<String, ModelTextureInfo>,
    pub objectives: Vec<String>,
    pub teams: Vec<String>,
    pub entity_tags: Vec<String>,
}

impl ScanExtras {
    pub fn merge(&mut self, other: ScanExtras) {
        self.translation_keys.extend(other.translation_keys);
        self.deprecated_translation_keys.extend(other.deprecated_translation_keys);
        self.lang_codes.extend(other.lang_codes);
        for (adv, crits) in other.advancement_criteria {
            self.advancement_criteria.entry(adv).or_default().extend(crits);
        }
        for (model, info) in other.model_textures {
            self.model_textures.insert(model, info);
        }
        self.objectives.extend(other.objectives);
        self.teams.extend(other.teams);
        self.entity_tags.extend(other.entity_tags);
    }
}

#[derive(Debug, Clone, Default)]
pub struct RegistryIndex {
    /// Category (e.g. `item`, `block`, `tag/item`, `recipe`, `loot_table`, `worldgen/biome`) -> sorted full IDs (`ns:path`).
    pub entries: HashMap<String, Vec<String>>,
    /// Block ID (`ns:path`) -> property name -> allowed values.
    pub block_states: HashMap<String, BTreeMap<String, Vec<String>>>,
    /// Installed mod IDs, from a ProbeRaven dump or the IDs in a mod jar's metadata. Data namespaces explicitly don't
    /// count! Plenty of mods ship `data/create/` compat files without Create installed, for example.
    pub mod_ids: HashSet<String>,
    pub code_source: CodeRegistrySource,
    pub known: KnownRegistries,
    /// Newest-first `(data_pack_version, release_id)` pairs from `mcmeta` for `#[pack_format]` completion.
    pub data_pack_formats: Vec<(u32, String)>,
    /// Vanilla boolean and integer game rules from `mcmeta` `commands/data.min.json` for `#[game_rule]`.
    pub game_rules: super::fetch::GameRules,
    /// Sorted translation keys: vanilla's `en_us.json` from `mcmeta`, plus keys indexed from `assets/*/lang/*.json`.
    pub translation_keys: Vec<String>,
    /// Whether `translation_keys` includes vanilla's. Without them, every vanilla key would look unknown.
    pub has_vanilla_translation_keys: bool,
    /// Asset kinds (`texture`, `shader/vertex`, `sound`, ...) we have vanilla's complete list of, from the
    /// launcher's client jar and asset index.
    pub vanilla_asset_kinds: HashSet<String>,
    /// Translation keys marked `removed` or `renamed` in `assets/*/lang/deprecated.json`.
    pub deprecated_translation_keys: HashSet<String>,
    /// Sorted language codes (e.g. `en_us`) found under `assets/*/lang/`.
    pub lang_codes: Vec<String>,
    /// Advancement ID (`ns:path`) -> sorted criterion names, for `PlayerAdvancementCriteria`.
    pub advancement_criteria: HashMap<String, Vec<String>>,
    /// Model ID (`ns:path`) -> parent model + defined texture slots, for `#[texture_slot]` chains.
    pub model_textures: HashMap<String, ModelTextureInfo>,
    /// Scoreboard objectives found via `scoreboard objectives add` in `.mcfunction` files.
    pub objectives: Vec<String>,
    /// Teams found via `team add` in `.mcfunction` files.
    pub teams: Vec<String>,
    /// Entity tags found via `tag <target> add` in `.mcfunction` files.
    pub entity_tags: Vec<String>,
    pub(crate) has_uppercase: bool,
}

impl RegistryIndex {
    pub fn insert_entries(&mut self, category: &str, ids: impl IntoIterator<Item = String>) {
        let cat = normalize_category(category);
        let list = self.entries.entry(cat).or_default();
        for id in ids {
            let full = lengthen_id(&id);
            if full.bytes().any(|b| b.is_ascii_uppercase()) {
                self.has_uppercase = true;
            }
            list.push(full);
        }
    }

    pub fn merge_extras(&mut self, extras: ScanExtras) {
        self.translation_keys.extend(extras.translation_keys);
        self.deprecated_translation_keys.extend(extras.deprecated_translation_keys);
        self.lang_codes.extend(extras.lang_codes);
        for (adv, crits) in extras.advancement_criteria {
            self.advancement_criteria.entry(adv).or_default().extend(crits);
        }
        for (model, info) in extras.model_textures {
            self.model_textures.insert(model, info);
        }
        self.objectives.extend(extras.objectives);
        self.teams.extend(extras.teams);
        self.entity_tags.extend(extras.entity_tags);
    }

    pub fn finalize(&mut self) {
        let mut has_upper = false;
        for list in self.entries.values_mut() {
            list.sort_unstable();
            list.dedup();
            if !has_upper && list.iter().any(|s| s.bytes().any(|b| b.is_ascii_uppercase())) {
                has_upper = true;
            }
        }
        self.has_uppercase = has_upper;
        self.translation_keys.sort_unstable();
        self.translation_keys.dedup();
        self.lang_codes.sort_unstable();
        self.lang_codes.dedup();
        self.objectives.sort_unstable();
        self.objectives.dedup();
        self.teams.sort_unstable();
        self.teams.dedup();
        self.entity_tags.sort_unstable();
        self.entity_tags.dedup();
        for crits in self.advancement_criteria.values_mut() {
            crits.sort_unstable();
            crits.dedup();
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

        // We only know a namespace's assets when we indexed that kind for it: vanilla's from the launcher's
        // client jar and asset index, or from mcmeta (which lacks some kinds, like shaders, and more on older
        // versions), and a mod's from its jar. Mods also ship `minecraft` assets, so those don't count for
        // vanilla. Mods like Moonlight generate resource packs at runtime, and so do atlas
        // `paletted_permutations` sources (vanilla's armor trims).
        if !is_tag && is_asset_category(&cat) {
            let indexed_namespace = if full.starts_with("minecraft:") {
                self.vanilla_asset_kinds.contains(&cat) || self.known.contains(&cat)
            } else {
                self.entries.get(&cat).is_some_and(|list| {
                    let namespace = &full[..=full.find(':').unwrap_or(0)];
                    list.get(list.partition_point(|id| id.as_str() < namespace)).is_some_and(|id| id.starts_with(namespace))
                })
            };
            let atlas_generated = cat == "texture" && full.starts_with("minecraft:trims/");
            return if indexed_namespace && !atlas_generated { IdLookup::UnknownAuthoritative } else { IdLookup::Known };
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
        self.complete_filtered(category, allow_tag, require_tag, None, &[], typed, limit)
    }

    /// Returns ranked prefix/substring completions for `category`, honoring `#[id(path=..., exclude=...)]`.
    pub fn complete_filtered(
        &self,
        category: &str,
        allow_tag: bool,
        require_tag: bool,
        implicit_path: Option<&str>,
        exclude: &[String],
        typed: &str,
        limit: usize,
    ) -> Vec<String> {
        let cat = normalize_category(category);
        let typed_lower = typed.to_lowercase();
        let typed_is_tag = typed_lower.starts_with('#');
        let excluded: HashSet<String> = exclude.iter().map(|e| lengthen_id(e)).collect();
        let mut starts = Vec::new();
        let mut contains = Vec::new();

        if implicit_path.is_none() && excluded.is_empty() {
            if require_tag || (allow_tag && typed_is_tag) {
                let tag_cat = format!("tag/{cat}");
                let needle = typed_lower.strip_prefix('#').unwrap_or(&typed_lower);
                self.scan_completions(&tag_cat, "#", needle, limit, &mut starts, &mut contains);
            } else {
                self.scan_completions(&cat, "", &typed_lower, limit, &mut starts, &mut contains);
                if allow_tag && starts.len() + contains.len() < limit {
                    let tag_cat = format!("tag/{cat}");
                    self.scan_completions(&tag_cat, "#", &typed_lower, limit, &mut starts, &mut contains);
                }
            }
        } else if require_tag || (allow_tag && typed_is_tag) {
            let tag_cat = format!("tag/{cat}");
            let needle = typed_lower.strip_prefix('#').unwrap_or(&typed_lower);
            self.scan_completions_filtered(&tag_cat, "#", implicit_path, &excluded, needle, limit, &mut starts, &mut contains);
        } else {
            self.scan_completions_filtered(&cat, "", implicit_path, &excluded, &typed_lower, limit, &mut starts, &mut contains);
            if allow_tag && starts.len() + contains.len() < limit {
                let tag_cat = format!("tag/{cat}");
                self.scan_completions_filtered(&tag_cat, "#", implicit_path, &excluded, &typed_lower, limit, &mut starts, &mut contains);
            }
        }

        starts.extend(contains);
        starts.truncate(limit);
        starts
    }

    #[allow(clippy::too_many_arguments)]
    fn scan_completions_filtered(
        &self,
        lookup_cat: &str,
        prefix: &str,
        implicit_path: Option<&str>,
        excluded: &HashSet<String>,
        needle: &str,
        limit: usize,
        starts: &mut Vec<String>,
        contains: &mut Vec<String>,
    ) {
        let Some(list) = self.entries.get(lookup_cat) else { return };
        for id in list {
            if excluded.contains(id) {
                continue;
            }
            let display = match implicit_path {
                Some(path_prefix) => {
                    let Some((ns, p)) = id.split_once(':') else { continue };
                    let Some(stripped) = p.strip_prefix(path_prefix) else { continue };
                    format!("{ns}:{stripped}")
                }
                None => id.clone(),
            };
            let formatted = if prefix.is_empty() { display.clone() } else { format!("{prefix}{display}") };
            if needle.is_empty() {
                if starts.len() < limit {
                    starts.push(formatted);
                }
                if starts.len() >= limit {
                    break;
                }
                continue;
            }
            let short = strip_ascii_prefix(&display, "minecraft:");
            let is_start = starts_with_ascii_lower(&display, needle) || short.is_some_and(|s| starts_with_ascii_lower(s, needle));
            if is_start {
                if starts.len() < limit {
                    starts.push(formatted);
                }
            } else if contains.len() < limit && contains_ascii_lower(&display, needle) {
                contains.push(formatted);
            }
        }
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
        let format_id = |id: &String| if prefix.is_empty() { id.clone() } else { format!("{prefix}{id}") };

        if needle.is_empty() {
            for id in list.iter().take(limit.saturating_sub(starts.len())) {
                starts.push(format_id(id));
            }
            return;
        }

        if !self.has_uppercase {
            let direct_start = list.partition_point(|id| id.as_str() < needle);
            let direct_end = direct_start + list[direct_start..].iter().take_while(|id| id.starts_with(needle)).count();
            let direct = &list[direct_start..direct_end];

            // When the user types a short path like "diamond", also match "minecraft:diamond…" in sorted order.
            let mc_slice = if !needle.contains(':') && !"minecraft:".starts_with(needle) {
                let mc_needle = format!("minecraft:{needle}");
                let mc_start = list.partition_point(|id| id.as_str() < mc_needle.as_str());
                let mc_end = mc_start + list[mc_start..].iter().take_while(|id| id.starts_with(mc_needle.as_str())).count();
                &list[mc_start..mc_end]
            } else {
                &[]
            };

            let (mut i, mut j) = (0usize, 0usize);
            while starts.len() < limit && (i < direct.len() || j < mc_slice.len()) {
                let pick = match (direct.get(i), mc_slice.get(j)) {
                    (Some(a), Some(b)) => {
                        if a <= b {
                            i += 1;
                            a
                        } else {
                            j += 1;
                            b
                        }
                    }
                    (Some(a), None) => {
                        i += 1;
                        a
                    }
                    (None, Some(b)) => {
                        j += 1;
                        b
                    }
                    (None, None) => break,
                };
                starts.push(format_id(pick));
            }

            let need_contains = limit.saturating_sub(starts.len());
            if need_contains == 0 || contains.len() >= need_contains {
                return;
            }
            for id in list {
                if id.starts_with(needle) || id.strip_prefix("minecraft:").is_some_and(|s| s.starts_with(needle)) {
                    continue;
                }
                if id.contains(needle) {
                    contains.push(format_id(id));
                    if contains.len() >= need_contains {
                        break;
                    }
                }
            }
            return;
        }

        for id in list {
            let short = strip_ascii_prefix(id, "minecraft:");
            let is_start = starts_with_ascii_lower(id, needle) || short.is_some_and(|s| starts_with_ascii_lower(s, needle));
            if is_start {
                if starts.len() < limit {
                    starts.push(format_id(id));
                }
            } else if contains.len() < limit && contains_ascii_lower(id, needle) {
                contains.push(format_id(id));
            }
        }
    }
}

fn starts_with_ascii_lower(haystack: &str, needle_lower: &str) -> bool {
    let hb = haystack.as_bytes();
    let nb = needle_lower.as_bytes();
    hb.len() >= nb.len() && hb[..nb.len()].iter().zip(nb).all(|(a, b)| a.to_ascii_lowercase() == *b)
}

fn strip_ascii_prefix<'a>(s: &'a str, prefix_lower: &str) -> Option<&'a str> {
    starts_with_ascii_lower(s, prefix_lower).then(|| &s[prefix_lower.len()..])
}

fn contains_ascii_lower(haystack: &str, needle_lower: &str) -> bool {
    let hb = haystack.as_bytes();
    let nb = needle_lower.as_bytes();
    if nb.is_empty() {
        return true;
    }
    if hb.len() < nb.len() {
        return false;
    }
    let first = nb[0];
    let last_start = hb.len() - nb.len();
    for i in 0..=last_start {
        if hb[i].to_ascii_lowercase() == first && hb[i + 1..i + nb.len()].iter().zip(&nb[1..]).all(|(a, b)| a.to_ascii_lowercase() == *b) {
            return true;
        }
    }
    false
}

/// Scans `mods/*.jar` in parallel for `data/<ns>/<category>/...json` entries, active overlays,
/// and optionally the mod IDs the jars declare. Also returns any jars declaring conditional overlays
/// so callers without a complete mod list yet can re-scan those few jars once `mod_ids` is final.
pub fn scan_mod_jars_data(
    root: &Path,
    known: &KnownRegistries,
    need_mod_ids: bool,
    overlay_env: Option<&OverlayEnv<'_>>,
) -> (HashMap<String, Vec<String>>, HashSet<String>, Vec<PathBuf>) {
    let (entries, mods, cond, _) = scan_mod_jars_data_with_extras(root, known, need_mod_ids, overlay_env);
    (entries, mods, cond)
}

pub fn scan_mod_jars_data_with_extras(
    root: &Path,
    known: &KnownRegistries,
    need_mod_ids: bool,
    overlay_env: Option<&OverlayEnv<'_>>,
) -> (HashMap<String, Vec<String>>, HashSet<String>, Vec<PathBuf>, ScanExtras) {
    let jars: Vec<PathBuf> = std::fs::read_dir(root.join("mods"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("jar")))
        .collect();
    if jars.is_empty() {
        return (HashMap::new(), HashSet::new(), Vec::new(), ScanExtras::default());
    }

    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(8).min(jars.len());
    let chunk_size = jars.len().div_ceil(threads);
    let partials: Vec<(HashMap<String, Vec<String>>, HashSet<String>, Vec<PathBuf>, ScanExtras)> = std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(threads);
        for slice in jars.chunks(chunk_size) {
            handles.push(scope.spawn(move || {
                let mut local: HashMap<String, Vec<String>> = HashMap::new();
                let mut mod_ids: HashSet<String> = HashSet::new();
                let mut conditional_jars: Vec<PathBuf> = Vec::new();
                let mut extras = ScanExtras::default();
                for jar_path in slice {
                    if scan_single_jar(jar_path, known, overlay_env, &mut local, &mut extras, need_mod_ids.then_some(&mut mod_ids), true) {
                        conditional_jars.push(jar_path.clone());
                    }
                }
                (local, mod_ids, conditional_jars, extras)
            }));
        }
        handles.into_iter().map(|h| h.join().unwrap_or_default()).collect()
    });

    let mut merged: HashMap<String, Vec<String>> = HashMap::new();
    let mut mod_ids: HashSet<String> = HashSet::new();
    let mut conditional_jars: Vec<PathBuf> = Vec::new();
    let mut merged_extras = ScanExtras::default();
    for (partial, ns, cond, extras) in partials {
        mod_ids.extend(ns);
        conditional_jars.extend(cond);
        merged_extras.merge(extras);
        for (cat, ids) in partial {
            merged.entry(cat).or_default().extend(ids);
        }
    }
    for ids in merged.values_mut() {
        ids.sort_unstable();
        ids.dedup();
    }
    (merged, mod_ids, conditional_jars, merged_extras)
}

/// Re-scans the subset of jars that declared conditional overlays once the full mod list is known.
pub fn scan_conditional_overlay_jars(
    jars: &[PathBuf],
    known: &KnownRegistries,
    overlay_env: &OverlayEnv<'_>,
) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    let mut extras = ScanExtras::default();
    for jar in jars {
        scan_single_jar(jar, known, Some(overlay_env), &mut out, &mut extras, None, true);
    }
    for ids in out.values_mut() {
        ids.sort_unstable();
        ids.dedup();
    }
    out
}

/// Scans NeoForge/Forge universal jars in `install_dir/libraries` for built-in `c:` and `forge:` tags.
pub fn scan_loader_jars_data(
    install_dir: &Path,
    lock: Option<&Lockfile>,
    known: &KnownRegistries,
    overlay_env: Option<&OverlayEnv<'_>>,
) -> HashMap<String, Vec<String>> {
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
    let mut extras = ScanExtras::default();
    for jar in candidates {
        if jar.is_file() {
            scan_single_jar(&jar, known, overlay_env, &mut out, &mut extras, None, true);
        }
    }
    for ids in out.values_mut() {
        ids.sort_unstable();
        ids.dedup();
    }
    out
}

pub fn client_jar_path(install_dir: &Path, minecraft: &str) -> PathBuf {
    install_dir.join("versions").join(minecraft).join(format!("{minecraft}.jar"))
}

#[derive(Debug, Default)]
pub struct VanillaClientAssets {
    pub entries: HashMap<String, Vec<String>>,
    pub translation_keys: Vec<String>,
}

/// Lists vanilla's assets from the launcher's install: the client jar (`versions/<mc>/<mc>.jar`) and the
/// asset index it names, which holds the sounds and every language but `en_us`. We only read the jar's
/// directory, plus `en_us.json` for translation keys.
pub fn scan_vanilla_client_assets(install_dir: &Path, minecraft: &str) -> VanillaClientAssets {
    let mut assets = VanillaClientAssets::default();
    let version_dir = install_dir.join("versions").join(minecraft);
    if let Ok(file) = File::open(client_jar_path(install_dir, minecraft))
        && let Ok(mut zip) = ZipArchive::new(file)
    {
        let mut en_us = None;
        for i in 0..zip.len() {
            let Ok(entry) = zip.by_index_raw(i) else { continue };
            let Ok(name) = entry.name() else { continue };
            if !name.starts_with("assets/") {
                continue;
            }
            if name == "assets/minecraft/lang/en_us.json" {
                en_us = Some(i);
            }
            if let Some((cat, id)) = parse_asset_file_path(&name).or_else(|| parse_non_json_asset_path(&name)) {
                assets.entries.entry(cat).or_default().push(id);
            }
        }
        if let Some(i) = en_us
            && let Ok(entry) = zip.by_index(i)
            && let Ok(lang) = serde_json::from_reader::<_, serde_json::Map<String, serde_json::Value>>(entry)
        {
            assets.translation_keys = lang.into_iter().map(|(k, _)| k).collect();
        }
    }
    let index_id = std::fs::read(version_dir.join(format!("{minecraft}.json")))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|v| Some(v.get("assetIndex")?.get("id")?.as_str()?.to_string()));
    if let Some(id) = index_id
        && let Ok(bytes) = std::fs::read(install_dir.join("assets").join("indexes").join(format!("{id}.json")))
        && let Ok(index) = serde_json::from_slice::<serde_json::Value>(&bytes)
        && let Some(objects) = index.get("objects").and_then(serde_json::Value::as_object)
    {
        for path in objects.keys() {
            let path = format!("assets/{path}");
            if let Some((cat, id)) = parse_asset_file_path(&path).or_else(|| parse_non_json_asset_path(&path)) {
                assets.entries.entry(cat).or_default().push(id);
            }
        }
    }
    assets
}

fn scan_zip_entries<R: std::io::Read + std::io::Seek>(
    zip: &mut ZipArchive<R>,
    known: &KnownRegistries,
    overlay_env: Option<&OverlayEnv<'_>>,
    out: &mut HashMap<String, Vec<String>>,
    extras: &mut ScanExtras,
    collect_bundled: bool,
    is_mod_jar: bool,
) -> (Vec<usize>, bool) {
    let mut active_overlays: HashSet<String> = HashSet::new();
    let mut has_conditional = false;
    if let Some(env) = overlay_env
        && let Ok(mut mcmeta) = zip.by_name("pack.mcmeta")
        && mcmeta.size() <= 1024 * 1024
    {
        let mut text = String::new();
        if mcmeta.read_to_string(&mut text).is_ok()
            && let Some(meta) = json_parser::parse(&text, true).root
        {
            has_conditional = overlays::has_conditional_overlays(&meta);
            active_overlays.extend(overlays::active_overlays(&meta, env));
        }
    }

    let mut bundled = Vec::new();
    let mut inline_reads: Vec<(usize, String, String)> = Vec::new();
    let mut mcfunctions: Vec<usize> = Vec::new();
    for i in 0..zip.len() {
        let Ok(entry) = zip.by_index_raw(i) else { continue };
        let Ok(name) = entry.name() else { continue };
        // Fabric bundles mods in META-INF/jars/ and NeoForge in META-INF/jarjar/, and both load them
        // as mods of their own (Fabric API's modules, for one).
        if collect_bundled && (name.starts_with("META-INF/jars/") || name.starts_with("META-INF/jarjar/")) && name.ends_with(".jar") {
            bundled.push(i);
            continue;
        }
        let effective: &str = if !active_overlays.is_empty()
            && let Some((first, rest)) = name.split_once('/')
            && active_overlays.contains(first)
        {
            rest
        } else {
            &name
        };
        if effective.starts_with("assets/") {
            if let Some((cat, id)) = parse_asset_file_path(effective).or_else(|| parse_non_json_asset_path(effective)) {
                let read_content = match cat.as_str() {
                    "sounds" | "lang/deprecated" | "atlas" => true,
                    "lang" => !is_mod_jar || id.ends_with(":en_us"),
                    "model" => !is_mod_jar,
                    _ => false,
                };
                if read_content {
                    inline_reads.push((i, cat.clone(), id.clone()));
                }
                out.entry(cat).or_default().push(id);
            }
            continue;
        }
        if !effective.starts_with("data/") {
            continue;
        }
        if let Some((cat, id)) = parse_data_file_path(effective, known).or_else(|| parse_non_json_data_path(effective)) {
            if cat == "worldgen/world_preset" || cat == "advancement" {
                inline_reads.push((i, cat.clone(), id.clone()));
            } else if cat == "function" && !is_mod_jar {
                mcfunctions.push(i);
            }
            out.entry(cat).or_default().push(id);
        }
    }
    for (idx, cat, id) in inline_reads {
        if let Ok(mut entry) = zip.by_index(idx)
            && entry.size() <= 1024 * 1024
        {
            let mut text = String::new();
            if entry.read_to_string(&mut text).is_ok() {
                index_inline_definitions(&cat, &id, &text, out, extras);
            }
        }
    }
    for idx in mcfunctions {
        if let Ok(mut entry) = zip.by_index(idx)
            && entry.size() <= 512 * 1024
        {
            let mut text = String::new();
            if entry.read_to_string(&mut text).is_ok() {
                super::mcfunction::extract_mcfunction_symbols(&text, &mut extras.objectives, &mut extras.teams, &mut extras.entity_tags);
            }
        }
    }
    (bundled, has_conditional)
}

fn scan_single_jar(
    path: &Path,
    known: &KnownRegistries,
    overlay_env: Option<&OverlayEnv<'_>>,
    out: &mut HashMap<String, Vec<String>>,
    extras: &mut ScanExtras,
    mut mod_ids: Option<&mut HashSet<String>>,
    is_mod_jar: bool,
) -> bool {
    let Ok(file) = File::open(path) else { return false };
    let Ok(mut zip) = ZipArchive::new(file) else { return false };
    if let Some(ids) = mod_ids.as_deref_mut() {
        ids.extend(crate::mod_manifest::declared_ids(&mut zip));
    }
    let (bundled, has_conditional) = scan_zip_entries(&mut zip, known, overlay_env, out, extras, mod_ids.is_some(), is_mod_jar);
    if let Some(ids) = mod_ids {
        for index in bundled {
            collect_bundled_jar(&mut zip, index, 1, known, overlay_env, out, extras, ids);
        }
    }
    has_conditional
}

// Fabric and NeoForge both load jars nested inside nested jars, so we follow them a few levels down.
#[allow(clippy::too_many_arguments)]
fn collect_bundled_jar<R: std::io::Read + std::io::Seek>(
    zip: &mut ZipArchive<R>,
    index: usize,
    depth: u8,
    known: &KnownRegistries,
    overlay_env: Option<&OverlayEnv<'_>>,
    out: &mut HashMap<String, Vec<String>>,
    extras: &mut ScanExtras,
    ids: &mut HashSet<String>,
) {
    const MAX_BUNDLED_JAR_BYTES: u64 = 64 * 1024 * 1024;
    const MAX_DEPTH: u8 = 4;
    let Ok(mut entry) = zip.by_index(index) else { return };
    if entry.size() > MAX_BUNDLED_JAR_BYTES {
        return;
    }
    let mut bytes = Vec::with_capacity(entry.size() as usize);
    if std::io::Read::read_to_end(&mut entry, &mut bytes).is_err() {
        return;
    }
    drop(entry);
    let Ok(mut inner) = ZipArchive::new(std::io::Cursor::new(bytes)) else { return };
    ids.extend(crate::mod_manifest::declared_ids(&mut inner));
    let (nested, _) = scan_zip_entries(&mut inner, known, overlay_env, out, extras, depth < MAX_DEPTH, true);
    for i in nested {
        collect_bundled_jar(&mut inner, i, depth + 1, known, overlay_env, out, extras, ids);
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

/// Scans live datapack and resource pack files in the instance (`datapacks/`, `resourcepacks/`,
/// `kubejs/data/`, `kubejs/assets/`, and global pack loaders), including active overlays listed in
/// each pack's `pack.mcmeta`.
pub fn scan_pack_data(
    root: &Path,
    lock: Option<&Lockfile>,
    known: &KnownRegistries,
    overlay_env: Option<&OverlayEnv<'_>>,
) -> HashMap<String, Vec<String>> {
    scan_pack_data_with_extras(root, lock, known, overlay_env).0
}

pub fn scan_pack_data_with_extras(
    root: &Path,
    lock: Option<&Lockfile>,
    known: &KnownRegistries,
    overlay_env: Option<&OverlayEnv<'_>>,
) -> (HashMap<String, Vec<String>>, ScanExtras) {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    let mut extras = ScanExtras::default();
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut zips: Vec<PathBuf> = Vec::new();

    if root.join("data").is_dir() || root.join("assets").is_dir() || root.join("pack.mcmeta").is_file() {
        roots.push(root.to_path_buf());
    }
    if root.join("kubejs/data").is_dir() || root.join("kubejs/assets").is_dir() {
        roots.push(root.join("kubejs"));
    }
    for kind in [PackKind::Data, PackKind::Resource] {
        for loader in global_packs::loaders(root, lock, kind) {
            for folder in loader.folders {
                for pack in folder.packs {
                    let p = root.join(&pack.path);
                    if p.is_dir() {
                        roots.push(p);
                    } else if pack.zipped && p.is_file() {
                        zips.push(p);
                    }
                }
            }
        }
    }
    for folder in ["datapacks", "resourcepacks"] {
        let dir = root.join(folder);
        if dir.is_dir() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let p = entry.path();
                if p.is_dir() {
                    roots.push(p);
                } else if p.is_file() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")) {
                    zips.push(p);
                }
            }
        }
    }

    zips.sort_unstable();
    zips.dedup();
    if !zips.is_empty() {
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(8).min(zips.len());
        let chunk_size = zips.len().div_ceil(threads);
        let partials: Vec<(HashMap<String, Vec<String>>, ScanExtras)> = std::thread::scope(|scope| {
            let mut handles = Vec::with_capacity(threads);
            for slice in zips.chunks(chunk_size) {
                handles.push(scope.spawn(move || {
                    let mut local: HashMap<String, Vec<String>> = HashMap::new();
                    let mut local_extras = ScanExtras::default();
                    for zip_path in slice {
                        scan_single_jar(zip_path, known, overlay_env, &mut local, &mut local_extras, None, false);
                    }
                    (local, local_extras)
                }));
            }
            handles.into_iter().map(|h| h.join().unwrap_or_default()).collect()
        });
        for (partial, partial_extras) in partials {
            extras.merge(partial_extras);
            for (cat, ids) in partial {
                out.entry(cat).or_default().extend(ids);
            }
        }
    }

    let mut seen_roots = HashSet::new();
    for pack_root in roots {
        if !seen_roots.insert(pack_root.clone()) {
            continue;
        }
        let mut scan_roots = vec![pack_root.clone()];
        if let Some(env) = overlay_env
            && let Ok(text) = std::fs::read_to_string(pack_root.join("pack.mcmeta"))
            && let Some(meta) = json_parser::parse(&text, true).root
        {
            for dir in overlays::active_overlays(&meta, env) {
                scan_roots.push(pack_root.join(dir));
            }
        }
        for base in scan_roots {
            let data_dir = base.join("data");
            if data_dir.is_dir() {
                for entry in WalkDir::new(&data_dir).into_iter().flatten() {
                    if !entry.file_type().is_file() {
                        continue;
                    }
                    let Ok(rel) = entry.path().strip_prefix(&base) else { continue };
                    let rel_str = rel.to_string_lossy().replace('\\', "/");
                    if let Some((cat, id)) = parse_data_file_path(&rel_str, known).or_else(|| parse_non_json_data_path(&rel_str)) {
                        if matches!(cat.as_str(), "worldgen/world_preset" | "loot_table" | "trade_set" | "advancement")
                            && let Ok(text) = std::fs::read_to_string(entry.path())
                        {
                            index_inline_definitions(&cat, &id, &text, &mut out, &mut extras);
                        } else if cat == "function"
                            && let Ok(text) = std::fs::read_to_string(entry.path())
                        {
                            super::mcfunction::extract_mcfunction_symbols(&text, &mut extras.objectives, &mut extras.teams, &mut extras.entity_tags);
                        }
                        out.entry(cat).or_default().push(id);
                    }
                }
            }
            let assets_dir = base.join("assets");
            if assets_dir.is_dir() {
                for entry in WalkDir::new(&assets_dir).into_iter().flatten() {
                    if !entry.file_type().is_file() {
                        continue;
                    }
                    let Ok(rel) = entry.path().strip_prefix(&base) else { continue };
                    let rel_str = rel.to_string_lossy().replace('\\', "/");
                    if let Some((cat, id)) = parse_asset_file_path(&rel_str).or_else(|| parse_non_json_asset_path(&rel_str)) {
                        if matches!(cat.as_str(), "lang" | "lang/deprecated" | "sounds" | "atlas" | "model")
                            && let Ok(text) = std::fs::read_to_string(entry.path())
                        {
                            index_inline_definitions(&cat, &id, &text, &mut out, &mut extras);
                        }
                        out.entry(cat).or_default().push(id);
                    }
                }
            }
        }
    }

    for ids in out.values_mut() {
        ids.sort_unstable();
        ids.dedup();
    }
    (out, extras)
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

/// Indexes inline symbols inside data and asset JSON files (`WorldPreset.dimensions`,
/// `LootTable`/`TradeSet` `random_sequence`, `Advancement.criteria`, `lang/*.json`,
/// `lang/deprecated.json`, `sounds.json`, `atlases/*.json`, and `models/**/*.json`).
fn index_inline_definitions(
    category: &str,
    id: &str,
    text: &str,
    out: &mut HashMap<String, Vec<String>>,
    extras: &mut ScanExtras,
) {
    let Some(root) = json_parser::parse(text, true).root else { return };
    match category {
        "worldgen/world_preset" => {
            if let Some(dims) = root.get("dimensions").and_then(|n| n.as_object()) {
                for pair in dims {
                    if !pair.key.value.is_empty() {
                        out.entry("dimension".into()).or_default().push(lengthen_id(&pair.key.value));
                    }
                }
            }
        }
        "loot_table" | "trade_set" => {
            if let Some(seq) = root.get("random_sequence").and_then(|n| n.as_str())
                && !seq.is_empty()
            {
                out.entry("random_sequence".into()).or_default().push(lengthen_id(seq));
            }
        }
        "advancement" => {
            if let Some(criteria) = root.get("criteria").and_then(|n| n.as_object()) {
                let entry = extras.advancement_criteria.entry(lengthen_id(id)).or_default();
                for pair in criteria {
                    if !pair.key.value.is_empty() {
                        entry.push(pair.key.value.clone());
                    }
                }
            }
        }
        "lang" => {
            if let Some((_, code)) = id.split_once(':')
                && !code.is_empty()
            {
                extras.lang_codes.push(code.to_string());
            }
            if let Some(pairs) = root.as_object() {
                for pair in pairs {
                    if !pair.key.value.is_empty() {
                        extras.translation_keys.push(pair.key.value.clone());
                    }
                }
            }
        }
        "lang/deprecated" => {
            if let Some(json_parser::JsonKind::Array(items)) = root.get("removed").map(|n| &n.kind) {
                for item in items {
                    if let Some(k) = item.as_str() {
                        extras.deprecated_translation_keys.insert(k.to_string());
                    }
                }
            }
            if let Some(renamed) = root.get("renamed").and_then(|n| n.as_object()) {
                for pair in renamed {
                    extras.deprecated_translation_keys.insert(pair.key.value.clone());
                    if let Some(new_k) = pair.value.as_ref().and_then(|v| v.as_str()) {
                        extras.translation_keys.push(new_k.to_string());
                    }
                }
            }
        }
        "sounds" => {
            if let Some((ns, _)) = id.split_once(':')
                && let Some(pairs) = root.as_object()
            {
                let sounds = out.entry("sound_event".into()).or_default();
                for pair in pairs {
                    if !pair.key.value.is_empty() {
                        sounds.push(format!("{ns}:{}", pair.key.value));
                    }
                }
            }
        }
        "atlas" => {
            if let Some(json_parser::JsonKind::Array(sources)) = root.get("sources").map(|n| &n.kind) {
                for src in sources {
                    let Some(ty) = src.get("type").and_then(|n| n.as_str()) else { continue };
                    if !matches!(ty, "paletted_permutations" | "minecraft:paletted_permutations") {
                        continue;
                    }
                    let perms: Vec<String> = src
                        .get("permutations")
                        .and_then(|n| n.as_object())
                        .map(|pairs| pairs.iter().map(|p| p.key.value.clone()).collect())
                        .unwrap_or_default();
                    let sep = src.get("separator").and_then(|n| n.as_str()).unwrap_or("_");
                    if let Some(json_parser::JsonKind::Array(textures)) = src.get("textures").map(|n| &n.kind) {
                        let tex_out = out.entry("texture".into()).or_default();
                        for tex in textures {
                            if let Some(base) = tex.as_str() {
                                for perm in &perms {
                                    tex_out.push(lengthen_id(&format!("{base}{sep}{perm}")));
                                }
                            }
                        }
                    }
                }
            }
        }
        "model" => {
            let parent = root.get("parent").and_then(|n| n.as_str()).map(lengthen_id);
            let slots: Vec<String> = root
                .get("textures")
                .and_then(|n| n.as_object())
                .map(|pairs| pairs.iter().map(|p| p.key.value.clone()).collect())
                .unwrap_or_default();
            if parent.is_some() || !slots.is_empty() {
                extras.model_textures.insert(lengthen_id(id), ModelTextureInfo { parent, slots });
            }
        }
        _ => {}
    }
}

/// Maps a resource-pack JSON/mcmeta path like `assets/minecraft/models/item/apple.json` or
/// `assets/mymod/sounds.json` to `(category, "namespace:id")`.
pub fn parse_asset_file_path(path: &str) -> Option<(String, String)> {
    let norm = path.replace('\\', "/");
    let after_assets = norm
        .strip_prefix("assets/")
        .or_else(|| norm.find("/assets/").map(|i| &norm[i + 8..]))?;
    let (namespace, rest) = after_assets.split_once('/')?;
    if namespace.is_empty() {
        return None;
    }
    if let Some(tex_rest) = rest.strip_prefix("textures/")
        && let Some(id) = tex_rest.strip_suffix(".png.mcmeta")
        && !id.is_empty()
    {
        return Some(("texture_meta".into(), format!("{namespace}:{id}")));
    }
    let stripped = rest.strip_suffix(".json")?;
    match stripped {
        "sounds" => return Some(("sounds".into(), format!("{namespace}:sounds"))),
        "regional_compliancies" => return Some(("regional_compliancies".into(), format!("{namespace}:regional_compliancies"))),
        "gpu_warnlist" => return Some(("gpu_warnlist".into(), format!("{namespace}:gpu_warnlist"))),
        "lang/deprecated" => return Some(("lang/deprecated".into(), format!("{namespace}:deprecated"))),
        _ => {}
    }
    if let Some(id) = stripped.strip_prefix("models/equipment/")
        && !id.is_empty()
    {
        return Some(("equipment".into(), format!("{namespace}:{id}")));
    }
    if let Some(id) = stripped.strip_prefix("shaders/post/")
        && !id.is_empty()
    {
        return Some(("post_effect".into(), format!("{namespace}:{id}")));
    }
    let (folder, id) = stripped.split_once('/')?;
    if id.is_empty() {
        return None;
    }
    let category = match folder {
        "atlases" => "atlas",
        "blockstates" => "block_definition",
        "equipment" => "equipment",
        "font" => "font",
        "items" => "item_definition",
        "lang" => "lang",
        "models" => "model",
        "particles" => "particle",
        "post_effect" => "post_effect",
        "shaders" => "shader",
        "waypoint_style" => "waypoint_style",
        _ => return None,
    };
    Some((category.to_string(), format!("{namespace}:{id}")))
}

/// Maps non-JSON asset files (textures, sounds, fonts, shader stages) to `(category, "namespace:id")`.
fn parse_non_json_asset_path(path: &str) -> Option<(String, String)> {
    let after_assets = path.strip_prefix("assets/").or_else(|| path.find("/assets/").map(|i| &path[i + 8..]))?;
    let (namespace, rest) = after_assets.split_once('/')?;
    let (folder, file) = rest.split_once('/')?;
    let (category, id) = match folder {
        "textures" => ("texture", file.strip_suffix(".png")?),
        "sounds" => ("sound", file.strip_suffix(".ogg")?),
        "font" => {
            if let Some(id) = file.strip_suffix(".ttf") {
                ("font/ttf", id)
            } else if let Some(id) = file.strip_suffix(".otf") {
                ("font/otf", id)
            } else if let Some(id) = file.strip_suffix(".zip") {
                ("font/unihex", id)
            } else {
                return None;
            }
        }
        "shaders" => {
            if let Some(id) = file.strip_suffix(".fsh") {
                ("shader/fragment", id)
            } else if let Some(id) = file.strip_suffix(".vsh") {
                ("shader/vertex", id)
            } else {
                return None;
            }
        }
        _ => return None,
    };
    (!namespace.is_empty() && !id.is_empty()).then(|| (category.to_string(), format!("{namespace}:{id}")))
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

    // Tags: `tags/<registry>/<id>` (including nested registries like `tags/worldgen/biome/<id>` and
    // multi-segment modded registries like `tags/<mod_ns>/worldgen/<reg>/<id>`).
    if let Some(tag_rest) = rest.strip_prefix("tags/") {
        if let Some((modded_reg, modded_id)) = known.match_modded_tag_folder(tag_rest) {
            return Some((format!("tag/{modded_reg}"), format!("{namespace}:{modded_id}")));
        }
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

    // Placebo registries (1.21 flat `data/<ns>/<folder>/` and 26.1 `data/<ns>/placebo/<folder>/`).
    if let Some(id) = rest.strip_prefix("placebo/gear_sets/").or_else(|| rest.strip_prefix("gear_sets/")) {
        return Some(("placebo:gear_sets".into(), format!("{namespace}:{id}")));
    }
    if let Some(id) = rest.strip_prefix("placebo/wanderer_trades/").or_else(|| rest.strip_prefix("wanderer_trades/")) {
        return Some(("placebo:wanderer_trades".into(), format!("{namespace}:{id}")));
    }
    if let Some(id) = rest.strip_prefix("placebo/brewing_mixes/").or_else(|| rest.strip_prefix("brewing_mixes/")) {
        return Some(("placebo:brewing_mixes".into(), format!("{namespace}:{id}")));
    }
    if let Some(id) = rest
        .strip_prefix("apothic_enchanting/enchanting_stats/")
        .or_else(|| rest.strip_prefix("enchanting_stats/"))
    {
        return Some(("apothic_enchanting:enchanting_stats".into(), format!("{namespace}:{id}")));
    }

    // Global loot modifier index (`data/<loader>/loot_modifiers/global_loot_modifiers.json`).
    if rest == "loot_modifiers/global_loot_modifiers" && matches!(namespace, "neoforge" | "forge" | "porting_lib") {
        return Some(("neoforge:global_loot_modifiers".into(), format!("{namespace}:{rest}")));
    }

    // Iron's Spellbooks & Iron's Jewelry datapack registries.
    if let Some(id) = rest.strip_prefix("irons_spellbooks/upgrade_orb_type/") {
        return Some(("irons_spellbooks:upgrade_orb_type".into(), format!("{namespace}:{id}")));
    }
    if let Some(id) = rest.strip_prefix("irons_jewelry/material/") {
        return Some(("irons_jewelry:material".into(), format!("{namespace}:{id}")));
    }
    if let Some(id) = rest.strip_prefix("irons_jewelry/part/") {
        return Some(("irons_jewelry:part".into(), format!("{namespace}:{id}")));
    }
    if let Some(id) = rest.strip_prefix("irons_jewelry/pattern/") {
        return Some(("irons_jewelry:pattern".into(), format!("{namespace}:{id}")));
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
    let category = category.strip_prefix("minecraft:").unwrap_or(category);
    // Vanilla resolves `sounds.json` event references against the merged `sounds.json` events, which
    // are the `sound_event` registry plus every resource pack's `sounds.json` keys (we index both there).
    if category == "weighed_sound_event" {
        return "sound_event".into();
    }
    category.to_string()
}

pub fn lengthen_id(id: &str) -> String {
    if let Some(rest) = id.strip_prefix(':') {
        format!("minecraft:{rest}")
    } else if id.contains(':') {
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
            | "function"
            | "structure"
            | "dimension"
            | "dimension_type"
            | "damage_type"
            | "enchantment"
            | "enchantment_provider"
            | "banner_pattern"
            | "block_sound_set"
            | "block_transformer"
            | "chat_type"
            | "context_float_provider"
            | "context_int_provider"
            | "decorated_pot_pattern"
            | "dispense_behavior"
            | "instrument"
            | "jukebox_song"
            | "painting_variant"
            | "slot_source"
            | "sulfur_cube_archetype"
            | "test_environment"
            | "test_instance"
            | "timeline"
            | "trade_set"
            | "trim_material"
            | "trim_pattern"
            | "villager_trade"
            | "wolf_variant"
            | "wolf_sound_variant"
            | "cat_variant"
            | "cat_sound_variant"
            | "chicken_variant"
            | "chicken_sound_variant"
            | "cow_variant"
            | "cow_sound_variant"
            | "frog_variant"
            | "pig_variant"
            | "pig_sound_variant"
            | "zombie_nautilus_variant"
            | "world_clock"
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
            | "placebo:wanderer_trades"
            | "placebo:brewing_mixes"
            | "apothic_enchanting:enchanting_stats"
            | "weapon_attributes"
            | "loot_modifiers"
            | "irons_spellbooks:upgrade_orb_type"
            | "irons_jewelry:material"
            | "irons_jewelry:part"
            | "irons_jewelry:pattern"
    ) || cat.starts_with("worldgen/")
        || cat.starts_with("tag/")
}

pub(crate) fn is_asset_category(cat: &str) -> bool {
    matches!(
        cat,
        "atlas"
            | "block_definition"
            | "equipment"
            | "font"
            | "font/ttf"
            | "font/otf"
            | "font/unihex"
            | "item_definition"
            | "lang"
            | "model"
            | "particle"
            | "post_effect"
            | "shader"
            | "shader/fragment"
            | "shader/vertex"
            | "sound"
            | "texture"
            | "waypoint_style"
    )
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

        let jar = |id: &str, nested: Option<&[u8]>| {
            let mut jar = ZipWriter::new(std::io::Cursor::new(Vec::new()));
            jar.start_file("fabric.mod.json", SimpleFileOptions::default()).unwrap();
            std::io::Write::write_all(&mut jar, format!(r#"{{"id": "{id}"}}"#).as_bytes()).unwrap();
            if let Some(nested) = nested {
                jar.start_file("META-INF/jars/inner.jar", SimpleFileOptions::default()).unwrap();
                std::io::Write::write_all(&mut jar, nested).unwrap();
            }
            jar.finish().unwrap().into_inner()
        };
        let bundled = jar("bundled_lib", Some(&jar("deeper_lib", None)));

        let jar_path = root.join("mods/example.jar");
        let mut zip = ZipWriter::new(File::create(&jar_path).unwrap());
        for (name, body) in [
            ("fabric.mod.json", br#"{"id": "example"}"#.as_slice()),
            ("data/create/recipe/brass_ingot.json", b"{}"),
            ("data/c/tags/item/ingots/brass.json", b"{}"),
            ("assets/create/models/item/brass_ingot.json", b"{}"),
            ("META-INF/jars/bundled_lib.jar", &bundled),
        ] {
            zip.start_file(name, SimpleFileOptions::default()).unwrap();
            std::io::Write::write_all(&mut zip, body).unwrap();
        }
        zip.finish().unwrap();

        let mut index = RegistryIndex::default();
        let (jar_entries, jar_mods, _) = scan_mod_jars_data(&root, &KnownRegistries::default(), true, None);
        assert!(["example", "bundled_lib", "deeper_lib"].iter().all(|id| jar_mods.contains(*id)), "{jar_mods:?}");
        assert!(!jar_mods.contains("create"), "a jar's compat data for Create doesn't install Create");
        let (no_id_entries, no_id_mods, _) = scan_mod_jars_data(&root, &KnownRegistries::default(), false, None);
        assert!(no_id_mods.is_empty(), "skipping mod IDs must skip jar manifests and nested jars");
        assert_eq!(no_id_entries, jar_entries, "data entries stay identical when skipping mod IDs");
        for (cat, ids) in jar_entries {
            index.insert_entries(&cat, ids);
        }
        index.insert_entries(
            "item",
            [
                "minecraft:diamond".into(),
                "minecraft:diamond_sword".into(),
                "create:brass_ingot".into(),
                "diamond_mod:ruby".into(),
                "other:blue_diamond".into(),
            ],
        );
        index.finalize();

        assert_eq!(index.lookup("recipe", "create:brass_ingot", false, false), IdLookup::Known);
        assert_eq!(index.lookup("recipe", "create:missing", false, false), IdLookup::UnknownAuthoritative);
        assert_eq!(index.lookup("item", "#c:ingots/brass", true, false), IdLookup::Known);
        assert_eq!(index.lookup("item", "create:unknown_item", false, false), IdLookup::UnknownUnverified);

        assert_eq!(
            index.complete("item", true, false, "diam", 10),
            vec!["diamond_mod:ruby", "minecraft:diamond", "minecraft:diamond_sword", "other:blue_diamond"]
        );
        assert_eq!(index.complete("item", true, false, "DIAM", 2), vec!["diamond_mod:ruby", "minecraft:diamond"]);
        assert_eq!(index.complete("item", true, false, "mine", 10), vec!["minecraft:diamond", "minecraft:diamond_sword"]);
        assert_eq!(index.complete("item", true, false, "#c:ing", 10), vec!["#c:ingots/brass"]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scans_active_overlays_in_packs_and_jars_and_filters_id_completions() {
        let root = std::env::temp_dir().join(format!("packraven-mcdoc-overlay-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dp = root.join("datapacks/over");
        std::fs::create_dir_all(dp.join("data/demo/recipe")).unwrap();
        std::fs::create_dir_all(dp.join("v48/data/demo/recipe")).unwrap();
        std::fs::create_dir_all(dp.join("v99/data/demo/recipe")).unwrap();
        std::fs::write(
            dp.join("pack.mcmeta"),
            r#"{"pack":{"pack_format":15},"overlays":{"entries":[{"formats":[48,60],"directory":"v48"},{"formats":[99,120],"directory":"v99"}]}}"#,
        )
        .unwrap();
        std::fs::write(dp.join("data/demo/recipe/base.json"), "{}").unwrap();
        std::fs::write(dp.join("v48/data/demo/recipe/active_only.json"), "{}").unwrap();
        std::fs::write(dp.join("v99/data/demo/recipe/inactive_only.json"), "{}").unwrap();

        let mods = HashSet::new();
        let env = overlays::OverlayEnv {
            minecraft: "1.21.1",
            format: overlays::PackFormat { major: 48, minor: 0 },
            neoforge: false,
            fabric_api: false,
            mods: &mods,
        };
        let found = scan_pack_data(&root, None, &KnownRegistries::default(), Some(&env));
        let recipes = found.get("recipe").expect("recipes scanned");
        assert!(recipes.contains(&"demo:base".to_string()));
        assert!(recipes.contains(&"demo:active_only".to_string()));
        assert!(!recipes.contains(&"demo:inactive_only".to_string()));

        assert_eq!(lengthen_id(":stone"), "minecraft:stone");
        assert!(is_data_driven_category("timeline"));
        assert!(is_data_driven_category("test_instance"));
        assert!(is_data_driven_category("function"));

        let mut index = RegistryIndex::default();
        index.insert_entries(
            "texture",
            ["minecraft:painting/kebab".into(), "minecraft:painting/aztec".into(), "minecraft:item/apple".into()],
        );
        index.finalize();
        assert_eq!(
            index.complete_filtered("texture", false, false, Some("painting/"), &["minecraft:painting/aztec".into()], "", 10),
            vec!["minecraft:kebab"]
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scans_vanilla_assets_from_the_launcher_install() {
        let install = std::env::temp_dir().join(format!("packraven-mcdoc-client-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&install);
        let version_dir = install.join("versions/1.21.1");
        std::fs::create_dir_all(&version_dir).unwrap();
        std::fs::create_dir_all(install.join("assets/indexes")).unwrap();
        let mut zip = ZipWriter::new(File::create(version_dir.join("1.21.1.jar")).unwrap());
        for (name, body) in [
            ("assets/minecraft/shaders/core/screenquad.vsh", b"".as_slice()),
            ("assets/minecraft/textures/block/stone.png", b""),
            ("assets/minecraft/lang/en_us.json", br#"{"block.minecraft.stone": "Stone"}"#),
            ("data/minecraft/recipe/stick.json", b"{}"),
        ] {
            zip.start_file(name, SimpleFileOptions::default()).unwrap();
            std::io::Write::write_all(&mut zip, body).unwrap();
        }
        zip.finish().unwrap();
        std::fs::write(version_dir.join("1.21.1.json"), r#"{"assetIndex": {"id": "17"}}"#).unwrap();
        std::fs::write(
            install.join("assets/indexes/17.json"),
            r#"{"objects": {"minecraft/sounds/ambient/cave/cave1.ogg": {}, "minecraft/lang/de_de.json": {}}}"#,
        )
        .unwrap();

        let assets = scan_vanilla_client_assets(&install, "1.21.1");
        assert_eq!(assets.entries["shader/vertex"], vec!["minecraft:core/screenquad".to_string()]);
        assert_eq!(assets.entries["texture"], vec!["minecraft:block/stone".to_string()]);
        assert_eq!(assets.entries["sound"], vec!["minecraft:ambient/cave/cave1".to_string()]);
        assert!(assets.entries["lang"].contains(&"minecraft:de_de".to_string()));
        assert!(!assets.entries.contains_key("recipe"));
        assert_eq!(assets.translation_keys, vec!["block.minecraft.stone".to_string()]);
        assert!(scan_vanilla_client_assets(&install, "1.20.1").entries.is_empty());

        let mut index = RegistryIndex::default();
        for (cat, ids) in assets.entries {
            index.vanilla_asset_kinds.insert(cat.clone());
            index.insert_entries(&cat, ids);
        }
        index.finalize();
        assert_eq!(index.lookup("shader/vertex", "minecraft:core/screenquad", false, false), IdLookup::Known);
        assert_eq!(index.lookup("shader/vertex", "minecraft:core/screenquadd", false, false), IdLookup::UnknownAuthoritative);
        assert_eq!(index.lookup("sound", "minecraft:ambient/cave/cave2", false, false), IdLookup::UnknownAuthoritative);
        let _ = std::fs::remove_dir_all(&install);
    }

    #[test]
    fn reports_unknown_assets_only_in_indexed_namespaces() {
        let mut index = RegistryIndex::default();
        index.apply_vanilla_registries(HashMap::from([("texture".into(), vec!["painting/kebab".into()])]));
        index.insert_entries("texture", ["demo:painting/sunset".into()]);
        index.insert_entries("model", ["demo:block/lamp".into()]);
        // A mod's own `minecraft` shaders don't make vanilla's (which mcmeta doesn't list) checkable.
        index.insert_entries("shader/vertex", ["minecraft:core/fancy_quad".into()]);
        index.finalize();
        assert_eq!(index.lookup("shader/vertex", "minecraft:core/screenquad", false, false), IdLookup::Known);
        assert_eq!(index.lookup("texture", "demo:painting/sunset", false, false), IdLookup::Known);
        assert_eq!(index.lookup("texture", "demo:painting/sunsett", false, false), IdLookup::UnknownAuthoritative);
        assert_eq!(index.lookup("texture", "minecraft:painting/kebabb", false, false), IdLookup::UnknownAuthoritative);
        // Namespaces we have no textures for, atlas-generated trims, and kinds mcmeta doesn't list stay unverifiable.
        assert_eq!(index.lookup("texture", "dynamic:block/oak_lamp", false, false), IdLookup::Known);
        assert_eq!(index.lookup("texture", "minecraft:trims/items/helmet_trim_ruby", false, false), IdLookup::Known);
        assert_eq!(index.lookup("model", "minecraft:block/stone", false, false), IdLookup::Known);
        assert_eq!(index.lookup("sound", "demo:zap", false, false), IdLookup::Known);
    }

    #[test]
    fn handles_multi_segment_modded_tags_substring_tags_and_inline_definitions() {
        let mut known = KnownRegistries::default();
        known.insert("mod:worldgen/foo");
        assert_eq!(
            parse_data_file_path("data/mymod/tags/mod/worldgen/foo/bar.json", &known),
            Some(("tag/mod:worldgen/foo".into(), "mymod:bar".into()))
        );
        // Unknown vanilla tag folders must not be misclassified as modded namespaces.
        assert_eq!(
            parse_data_file_path("data/minecraft/tags/item/sub/planks.json", &known),
            Some(("tag/item".into(), "minecraft:sub/planks".into()))
        );
        assert_eq!(
            parse_data_file_path("data/minecraft/tags/dialog/sub/greet.json", &known),
            Some(("tag/dialog".into(), "minecraft:sub/greet".into()))
        );

        // Substring `#tag` completion matches even when the user didn't type `#`.
        let mut index = RegistryIndex::default();
        index.insert_entries("item", ["minecraft:stone".into()]);
        index.insert_entries("tag/item", ["minecraft:planks".into()]);
        index.finalize();
        assert_eq!(index.complete("item", true, false, "plan", 10), vec!["#minecraft:planks"]);

        // `sounds.json` event references resolve against `sound_event`, which holds vanilla's events
        // and every indexed `sounds.json` key.
        let mut sounds = RegistryIndex { code_source: CodeRegistrySource::ProbeRaven, ..Default::default() };
        sounds.insert_entries("sound_event", ["minecraft:block.chest.close".into(), "demo:zap".into()]);
        sounds.finalize();
        assert_eq!(sounds.lookup("weighed_sound_event", "block.chest.close", false, false), IdLookup::Known);
        assert_eq!(sounds.lookup("minecraft:weighed_sound_event", "demo:zap", false, false), IdLookup::Known);
        assert_eq!(sounds.lookup("weighed_sound_event", "demo:missing", false, false), IdLookup::UnknownAuthoritative);

        // Inline `WorldPreset.dimensions` and `LootTable.random_sequence` definitions are indexed.
        let root = std::env::temp_dir().join(format!("packraven-mcdoc-inline-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dp = root.join("datapacks/inline_pack");
        std::fs::create_dir_all(dp.join("data/demo/worldgen/world_preset")).unwrap();
        std::fs::create_dir_all(dp.join("data/demo/loot_table")).unwrap();
        std::fs::write(
            dp.join("data/demo/worldgen/world_preset/custom.json"),
            r#"{"dimensions": {"demo:sky": {}, "minecraft:overworld": {}}}"#,
        )
        .unwrap();
        std::fs::write(
            dp.join("data/demo/loot_table/chest.json"),
            r#"{"random_sequence": "demo:custom_seq"}"#,
        )
        .unwrap();

        let found = scan_pack_data(&root, None, &known, None);
        let dims = found.get("dimension").expect("inline dimensions");
        assert!(dims.contains(&"demo:sky".to_string()), "{dims:?}");
        assert!(dims.contains(&"minecraft:overworld".to_string()), "{dims:?}");
        let seqs = found.get("random_sequence").expect("inline random sequences");
        assert!(seqs.contains(&"demo:custom_seq".to_string()), "{seqs:?}");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn parses_asset_paths_and_scans_assets_advancements_and_mcfunctions() {
        assert_eq!(parse_asset_file_path("assets/minecraft/models/item/apple.json"), Some(("model".into(), "minecraft:item/apple".into())));
        assert_eq!(parse_asset_file_path("assets/demo/blockstates/lamp.json"), Some(("block_definition".into(), "demo:lamp".into())));
        assert_eq!(parse_asset_file_path("assets/demo/sounds.json"), Some(("sounds".into(), "demo:sounds".into())));
        assert_eq!(parse_asset_file_path("assets/demo/lang/en_us.json"), Some(("lang".into(), "demo:en_us".into())));
        assert_eq!(parse_asset_file_path("assets/demo/lang/deprecated.json"), Some(("lang/deprecated".into(), "demo:deprecated".into())));
        assert_eq!(parse_asset_file_path("assets/demo/textures/block/fire.png.mcmeta"), Some(("texture_meta".into(), "demo:block/fire".into())));
        assert_eq!(parse_asset_file_path("assets/demo/items/sword.json"), Some(("item_definition".into(), "demo:sword".into())));
        assert_eq!(parse_asset_file_path("assets/demo/atlases/blocks.json"), Some(("atlas".into(), "demo:blocks".into())));

        let root = std::env::temp_dir().join(format!("packraven-mcdoc-phase4-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("assets/demo/lang")).unwrap();
        std::fs::create_dir_all(root.join("assets/demo/models/block")).unwrap();
        std::fs::create_dir_all(root.join("assets/demo/atlases")).unwrap();
        std::fs::create_dir_all(root.join("data/demo/advancement")).unwrap();
        std::fs::create_dir_all(root.join("data/demo/function")).unwrap();

        std::fs::write(root.join("assets/demo/lang/en_us.json"), r#"{"demo.hi": "Hello"}"#).unwrap();
        std::fs::write(root.join("assets/demo/lang/deprecated.json"), r#"{"removed": ["demo.gone"], "renamed": {"demo.old": "demo.new"}}"#).unwrap();
        std::fs::write(root.join("assets/demo/sounds.json"), r#"{"zap": {"sounds": []}}"#).unwrap();
        std::fs::write(root.join("assets/demo/models/block/base.json"), r#"{"textures": {"all": "demo:block/x"}}"#).unwrap();
        std::fs::write(
            root.join("assets/demo/atlases/armor_trims.json"),
            r#"{"sources": [{"type": "minecraft:paletted_permutations", "textures": ["demo:trims/helmet"], "permutations": {"gold": "demo:palettes/gold"}}]}"#,
        )
        .unwrap();
        std::fs::write(root.join("data/demo/advancement/root.json"), r#"{"criteria": {"first": {}, "second": {}}}"#).unwrap();
        std::fs::write(
            root.join("data/demo/function/init.mcfunction"),
            "scoreboard objectives add score dummy\nteam add blue\ntag @a add ready\n",
        )
        .unwrap();

        let (entries, extras) = scan_pack_data_with_extras(&root, None, &KnownRegistries::default(), None);
        assert!(entries["sound_event"].contains(&"demo:zap".to_string()));
        assert!(entries["texture"].contains(&"demo:trims/helmet_gold".to_string()));
        assert!(extras.translation_keys.contains(&"demo.hi".to_string()));
        assert!(extras.translation_keys.contains(&"demo.new".to_string()));
        assert!(extras.deprecated_translation_keys.contains("demo.gone"));
        assert!(extras.deprecated_translation_keys.contains("demo.old"));
        assert_eq!(extras.lang_codes, vec!["en_us"]);
        assert!(extras.model_textures["demo:block/base"].slots.contains(&"all".to_string()));
        let mut crits = extras.advancement_criteria["demo:root"].clone();
        crits.sort();
        assert_eq!(crits, vec!["first", "second"]);
        assert_eq!(extras.objectives, vec!["score"]);
        assert_eq!(extras.teams, vec!["blue"]);
        assert_eq!(extras.entity_tags, vec!["ready"]);

        let _ = std::fs::remove_dir_all(&root);
    }
}
