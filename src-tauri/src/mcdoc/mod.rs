pub mod checker;
pub mod completer;
pub mod fetch;
#[cfg(test)]
mod golden_tests;
pub mod json_parser;
pub mod overlays;
pub mod parser;
pub mod proberaven;
pub mod registries;
pub mod types;
pub mod version;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::Serialize;
use sha1::{Digest, Sha1};

use checker::{check_json_with_type, resource_root_type, CheckEnv};
use completer::{byte_to_utf16, CompleteOptions, CompletionResult, HoverInfo};
use json_parser::Severity;
use registries::{
    parse_data_file_path, read_proberaven_dump, scan_loader_jars_data, scan_mod_jars_data, scan_pack_data, CodeRegistrySource, RegistryIndex,
};
use types::SymbolTables;
use version::VersionContext;

use crate::pack::{Lockfile, ModLoader, LOCKFILE};
use crate::probejs::ProbeCache;

#[derive(Serialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct DatapackStatus {
    pub available: bool,
    pub minecraft: Option<String>,
    pub loader: Option<String>,
    pub symbol_count: usize,
    pub registry_entries: usize,
    pub code_source: CodeRegistrySource,
    pub probe_available: bool,
    pub load_ms: u64,
    pub error: Option<String>,
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LintDiagnostic {
    pub from: usize,
    pub to: usize,
    pub severity: &'static str,
    pub message: String,
}

pub struct DatapackIndex {
    pub tables: SymbolTables,
    pub registries: RegistryIndex,
    pub version: VersionContext,
    pub path_rules: Vec<fetch::PathRule>,
    pub ignored_loaders: Vec<&'static str>,
    /// Loader folders (`data/<ns>/<loader>/`) that the pack's loader never reads, with display names.
    pub foreign_loader_folders: &'static [(&'static str, &'static str)],
    pub data_pack_format: Option<overlays::PackFormat>,
}

/// Returns the loaders whose fields this pack can't use (see `checker::LOADER_ATTR`). We still accept
/// those fields when validating, since mods built for several loaders ship all of them.
fn loaders_the_pack_ignores(loader: Option<&ModLoader<'_>>, mod_ids: &std::collections::HashSet<String>) -> Vec<&'static str> {
    // Forgified Fabric API (which Sinytra Connector requires) ports Fabric's resource conditions and custom ingredients.
    let fabric_api = ["fabric-api", "fabric_api", "fabric-resource-conditions-api-v1", "connector"].iter().any(|m| mod_ids.contains(*m));
    let mut ignored = match loader {
        Some(ModLoader::Fabric(_)) => vec!["forge", "neoforge"],
        Some(ModLoader::NeoForge(_)) => vec!["forge", "fabric"],
        Some(ModLoader::Forge(_)) => vec!["neoforge", "fabric"],
        None => Vec::new(),
    };
    if fabric_api {
        ignored.retain(|l| *l != "fabric");
    }
    ignored
}

impl DatapackIndex {
    pub fn resolve_category(&self, path: &str) -> Option<String> {
        let norm = path.replace('\\', "/");
        let name = norm.rsplit('/').next().unwrap_or(&norm);
        if name.eq_ignore_ascii_case("pack.mcmeta") {
            return Some("pack_mcmeta".into());
        }
        if name.eq_ignore_ascii_case("fabric.mod.json") {
            return Some("fabric:fabric_mod_json".into());
        }
        if let Some(after_data) = norm.strip_prefix("data/").or_else(|| norm.find("/data/").map(|i| &norm[i + 6..]))
            && let Some(stripped) = after_data.strip_suffix(".json")
            && let Some((namespace, rest)) = stripped.split_once('/')
        {
            // Longest matching dynamic folder rule wins over generic single-segment fallback.
            if let Some(rule) = self
                .path_rules
                .iter()
                .filter(|r| r.matches(namespace, rest))
                .max_by_key(|r| r.exact.as_ref().map(String::len).or_else(|| r.folder.as_ref().map(String::len)).unwrap_or(0))
            {
                return Some(rule.category.clone());
            }
        }
        parse_data_file_path(&norm, &self.registries.known).map(|(cat, _)| cat)
    }

    /// Returns how this pack's Minecraft version and loader parse the file at `path`.
    pub fn json_dialect(&self, path: &str) -> Option<json_parser::JsonDialect> {
        let minecraft = self.version.minecraft.as_deref()?;
        let category = self.resolve_category(path);
        Some(json_dialect_for(minecraft, self.version.neoforge.is_some(), category.as_deref()))
    }

    fn reads_primitives_as_strings(&self, path: &str) -> bool {
        let category = self.resolve_category(path);
        self.version.minecraft.as_deref().is_some_and(|mc| version::compare_version(mc, "1.20.5") == std::cmp::Ordering::Less)
            && !category.as_deref().is_some_and(|c| c.starts_with("tag/") || loads_through_registry_data_loader(c))
    }

    /// Lints a file only when Minecraft would load it, so a copy that one of the pack's overlays
    /// replaces, or one in an overlay this version skips, gets a note instead of a check.
    pub fn lint_in_pack(&self, path: &str, text: &str, files: &dyn overlays::PackFiles) -> Vec<LintDiagnostic> {
        let mc = self.version.minecraft.as_deref().unwrap_or("this version");
        let message = match self.file_load(path, files) {
            overlays::FileLoad::Loaded => return self.lint(path, text),
            overlays::FileLoad::Replaced { overlay, .. } => {
                format!("Minecraft {mc} loads the copy in this pack's {overlay} overlay instead, so we don't check this file.")
            }
            overlays::FileLoad::OverlayInactive { overlay } => {
                format!("Minecraft {mc} doesn't apply this pack's {overlay} overlay here, so we don't check this file.")
            }
            overlays::FileLoad::Unlisted { folder } => {
                format!("Minecraft doesn't load {folder}/, since this pack's pack.mcmeta doesn't list it as an overlay, so we don't check this file.")
            }
        };
        vec![LintDiagnostic { from: 0, to: text.chars().next().map_or(0, |_| 1), severity: "info", message }]
    }

    pub fn file_load(&self, path: &str, files: &dyn overlays::PackFiles) -> overlays::FileLoad {
        let (Some(minecraft), Some(format)) = (self.version.minecraft.as_deref(), self.data_pack_format) else {
            return overlays::FileLoad::Loaded;
        };
        let env = overlays::OverlayEnv {
            minecraft,
            format,
            neoforge: self.version.neoforge.is_some(),
            fabric_api: !self.ignored_loaders.contains(&"fabric"),
            mods: &self.registries.mod_ids,
        };
        overlays::file_load(path, files, &env)
    }

    pub fn lint(&self, path: &str, text: &str) -> Vec<LintDiagnostic> {
        let allow_ext = path.to_lowercase().ends_with(".jsonc");
        let parsed = json_parser::parse_as(text, allow_ext, self.json_dialect(path));
        let mut diagnostics = parsed.diagnostics;

        let file_start = json_parser::Span::new(0, text.len().min(1));
        if let Some(message) = self.version.minecraft.as_deref().and_then(|mc| unloaded_folder_message(path, mc)) {
            diagnostics.push(json_parser::Diagnostic { span: file_start, severity: Severity::Warning, message });
        } else if let Some(message) = foreign_loader_folder_message(path, self.foreign_loader_folders) {
            diagnostics.push(json_parser::Diagnostic { span: file_start, severity: Severity::Info, message });
        } else if let Some(root) = &parsed.root
            && let Some(category) = self.resolve_category(path)
            && let Some(root_type) = resource_root_type(&category, &self.tables)
        {
            let env = CheckEnv {
                tables: &self.tables,
                registries: &self.registries,
                version: &self.version,
                require_canonical: false,
                allow_missing_keys: false,
                primitives_read_as_strings: self.reads_primitives_as_strings(path),
                ignored_loaders: &self.ignored_loaders,
            };
            let checked = check_json_with_type(root, &root_type, &env);
            let gated_on_missing_mod = requires_missing_mod(root, &self.registries.mod_ids);
            // Minecraft never loads a file whose load conditions need a missing mod, and that mod's IDs
            // are exactly what such compat files reference, so we don't report them as unknown.
            diagnostics.extend(checked.diagnostics.into_iter().filter(|d| !(gated_on_missing_mod && is_unknown_id_warning(d))));
        }

        diagnostics
            .into_iter()
            .map(|d| LintDiagnostic {
                from: byte_to_utf16(text, d.span.start),
                to: byte_to_utf16(text, d.span.end.max(d.span.start)),
                severity: match d.severity {
                    Severity::Error => "error",
                    Severity::Warning => "warning",
                    Severity::Info => "info",
                },
                message: d.message,
            })
            .collect()
    }

    pub fn complete(&self, path: &str, text: &str, utf16_offset: usize) -> CompletionResult {
        let empty = CompletionResult::empty(utf16_offset);
        let Some(category) = self.resolve_category(path) else { return empty };
        let Some(root_type) = resource_root_type(&category, &self.tables) else { return empty };
        let allow_ext = path.to_lowercase().ends_with(".jsonc");
        let dialect = self.json_dialect(path);
        let parsed = json_parser::parse_as(text, allow_ext, dialect);
        let Some(root) = &parsed.root else { return empty };
        let env = CheckEnv {
            tables: &self.tables,
            registries: &self.registries,
            version: &self.version,
            require_canonical: false,
            allow_missing_keys: false,
            primitives_read_as_strings: self.reads_primitives_as_strings(path),
                ignored_loaders: &self.ignored_loaders,
        };
        let checked = check_json_with_type(root, &root_type, &env);
        let recheck = |edited: &str| json_parser::parse_as(edited, allow_ext, dialect).root.map(|r| check_json_with_type(&r, &root_type, &env));
        let opts = CompleteOptions { recheck: &recheck, ignored_loaders: &self.ignored_loaders };
        completer::complete(text, utf16_offset, &checked, &env, &opts)
    }

    pub fn hover(&self, path: &str, text: &str, utf16_offset: usize) -> Option<HoverInfo> {
        let category = self.resolve_category(path)?;
        let root_type = resource_root_type(&category, &self.tables)?;
        let parsed = json_parser::parse_as(text, path.to_lowercase().ends_with(".jsonc"), self.json_dialect(path));
        let root = parsed.root.as_ref()?;
        let env = CheckEnv {
            tables: &self.tables,
            registries: &self.registries,
            version: &self.version,
            require_canonical: false,
            allow_missing_keys: false,
            primitives_read_as_strings: self.reads_primitives_as_strings(path),
                ignored_loaders: &self.ignored_loaders,
        };
        let checked = check_json_with_type(root, &root_type, &env);
        completer::hover(text, utf16_offset, &checked, &self.tables)
    }
}

struct CachedDatapack {
    root: PathBuf,
    fingerprint: String,
    build_started: Instant,
    status: DatapackStatus,
    index: Option<Arc<DatapackIndex>>,
}

#[derive(Default)]
pub struct DatapackCache {
    inner: Mutex<Option<CachedDatapack>>,
    // Lint, completion and status requests can all arrive while the cache is empty, so we build one index at a time.
    building: tokio::sync::Mutex<()>,
}

impl DatapackCache {
    pub fn get(&self, root: &Path) -> Option<Arc<DatapackIndex>> {
        let guard = self.inner.lock().unwrap();
        guard.as_ref().filter(|c| c.root == root).and_then(|c| c.index.clone())
    }

    pub fn invalidate(&self, root: &Path) {
        let mut guard = self.inner.lock().unwrap();
        if guard.as_ref().is_some_and(|c| c.root == root) {
            *guard = None;
        }
    }

    pub async fn ensure(&self, root: PathBuf, install_dir: PathBuf, probe_cache: Arc<ProbeCache>, force: bool) -> (DatapackStatus, Option<Arc<DatapackIndex>>) {
        let requested = Instant::now();
        let _building = self.building.lock().await;
        let fingerprint = fingerprint_pack(&root);
        // A forced reload that waited on another build is already satisfied if that build started after we asked.
        let force = force
            && !self.inner.lock().unwrap().as_ref().is_some_and(|c| c.root == root && c.build_started >= requested);
        if !force
            && let Some(cached) = self.inner.lock().unwrap().as_ref()
            && cached.root == root
            && cached.fingerprint == fingerprint
        {
            return (cached.status.clone(), cached.index.clone());
        }

        let started = Instant::now();
        let lock = std::fs::read_to_string(root.join(LOCKFILE))
            .ok()
            .and_then(|s| Lockfile::parse(&s).ok());
        let minecraft = lock.as_ref().and_then(|l| l.minecraft.clone());
        let neoforge = lock.as_ref().and_then(|l| match l.mod_loader().ok() {
            Some(ModLoader::NeoForge(v)) => Some(v.to_string()),
            _ => None,
        });
        let loader_kind = lock.as_ref().and_then(|l| l.mod_loader().ok()).map(|m| m.name().to_string());
        let loader_label = lock
            .as_ref()
            .and_then(|l| l.mod_loader().ok())
            .map(|m| format!("{} {}", m.name(), m.version()));
        let probe_available = minecraft
            .as_deref()
            .zip(lock.as_ref().and_then(|l| l.mod_loader().ok()))
            .is_some_and(|(mc, loader)| proberaven::jar_name_for(mc, &loader).is_some());

        let (vanilla_res, muon_manifest_res, misode_cfg_res) = tokio::join!(
            fetch::load_vanilla_symbols(force),
            fetch::load_muon_manifest(force),
            fetch::load_misode_config(force),
        );
        let mut tables = match vanilla_res {
            Ok(t) => t,
            Err(e) => {
                // Keep any already-loaded in-memory index for this pack rather than wiping it on a transient fetch error.
                let mut guard = self.inner.lock().unwrap();
                if let Some(existing) = guard.as_ref()
                    && existing.root == root
                    && existing.index.is_some()
                {
                    return (existing.status.clone(), existing.index.clone());
                }
                let status = DatapackStatus {
                    available: false,
                    minecraft,
                    loader: loader_label,
                    probe_available,
                    error: Some(format!("{e:#}")),
                    load_ms: started.elapsed().as_millis() as u64,
                    ..Default::default()
                };
                *guard = Some(CachedDatapack {
                    root,
                    fingerprint,
                    build_started: started,
                    status: status.clone(),
                    index: None,
                });
                return (status, None);
            }
        };
        let _ = parser::parse_and_bind(fetch::PACKRAVEN_MCDOC, "::packraven", &mut tables);
        let muon_manifest = muon_manifest_res.ok();
        let misode_config = misode_cfg_res.ok();

        let (vanilla_regs, vanilla_blocks, data_pack_format) = if let Some(mc) = minecraft.as_deref() {
            let (r, b, f) = tokio::join!(
                fetch::load_mcmeta_registries(mc, force),
                fetch::load_mcmeta_block_states(mc, force),
                fetch::load_mcmeta_data_pack_format(mc, force),
            );
            (r.ok(), b.ok(), f.ok())
        } else {
            (None, None, None)
        };

        let root_for_scan = root.clone();
        let lock_for_scan = lock.clone();
        let registries = tokio::task::spawn_blocking(move || {
            let mut reg = RegistryIndex::default();
            if let Some(v) = vanilla_regs {
                reg.apply_vanilla_registries(v);
            }
            if let Some(b) = vanilla_blocks {
                reg.apply_vanilla_block_states(b);
            }
            // We read the dump first because its registry names tell us how to read modded tag folders.
            let dump = read_proberaven_dump(&root_for_scan);
            for name in dump.iter().flat_map(|d| d.registries.keys()) {
                reg.known.insert(name);
            }
            let known = reg.known.clone();
            for (cat, ids) in scan_loader_jars_data(&install_dir, lock_for_scan.as_ref(), &known) {
                reg.insert_entries(&cat, ids);
            }
            let (jar_entries, jar_mods) = scan_mod_jars_data(&root_for_scan, &known);
            reg.mod_ids.extend(jar_mods);
            for (cat, ids) in jar_entries {
                reg.insert_entries(&cat, ids);
            }
            for (cat, ids) in scan_pack_data(&root_for_scan, lock_for_scan.as_ref(), &known) {
                reg.insert_entries(&cat, ids);
            }
            if let Some(probe) = probe_cache.get(&root_for_scan).or_else(|| probe_cache.ensure(&root_for_scan, false).1) {
                reg.apply_probejs(&probe);
            }
            if let Some(dump) = dump {
                // The loader's own list covers every nested mod and `provides` alias, so it replaces
                // our jar scan unless you've changed the mods since the dump.
                if dump_postdates_mod_changes(&root_for_scan) {
                    reg.mod_ids.clear();
                }
                reg.apply_proberaven(dump);
            }
            reg.finalize();
            reg
        })
        .await
        .unwrap_or_default();

        let mut path_rules = Vec::new();
        let mut apotheosis_loaded = false;

        // Fetch and bind schemas from `muon-rw/mcdocs` (live, disk-cached).
        if let Some(manifest) = &muon_manifest {
            let mut set = tokio::task::JoinSet::new();
            for entry in &manifest.schemas {
                if !fetch::schema_matches_version(minecraft.as_deref(), entry.min_minecraft.as_deref(), entry.max_minecraft.as_deref()) {
                    continue;
                }
                if !entry.always_load && !entry.mod_ids.iter().any(|m| registries.mod_ids.contains(m)) {
                    continue;
                }
                path_rules.extend(entry.paths.iter().cloned());
                let entry_id = entry.id.clone();
                let file = entry.file.clone();
                let module = entry.module.clone();
                set.spawn(async move { (entry_id, module, fetch::load_muon_mcdoc(&file, force).await) });
            }
            while let Some(joined) = set.join_next().await {
                if let Ok((entry_id, module, Ok(src))) = joined
                    && parser::parse_and_bind(&src, &module, &mut tables).is_ok()
                    && entry_id == "apotheosis"
                {
                    apotheosis_loaded = true;
                }
            }
        }
        // Offline fallback for Apotheosis when `muon-rw/mcdocs` is unreachable and uncached.
        if !apotheosis_loaded
            && fetch::schema_matches_version(minecraft.as_deref(), Some("1.21.1"), None)
        {
            let _ = parser::parse_and_bind(fetch::APOTHEOSIS_MCDOC, "::apotheosis", &mut tables);
        }

        // Fetch and bind misode partner `.mcdoc` schemas dynamically from `misode/misode.github.io`.
        let partner_names = fetch::partner_schemas_for_pack(loader_kind.as_deref(), &registries.mod_ids, misode_config.as_ref());
        if let Some(cfg) = &misode_config {
            path_rules.extend(fetch::misode_partner_path_rules(cfg, &partner_names, minecraft.as_deref()));
        }
        if !partner_names.is_empty() {
            let mut set = tokio::task::JoinSet::new();
            for name in partner_names {
                set.spawn(async move {
                    let res = fetch::load_partner_mcdoc(&name, force).await;
                    (name, res)
                });
            }
            while let Some(joined) = set.join_next().await {
                if let Ok((name, Ok(src))) = joined {
                    let src = fetch::patch_partner_mcdoc(&name, src);
                    let module_path = format!("::partner::{name}");
                    let _ = parser::parse_and_bind(&src, &module_path, &mut tables);
                }
            }
        }
        let extra_bridges = muon_manifest.as_ref().map(|m| m.recipe_dispatcher_bridges.as_slice()).unwrap_or(&[]);
        fetch::bridge_partner_dispatchers(&mut tables, extra_bridges);
        checker::mark_block_state_predicates(&mut tables);
        checker::relax_loot_entry_weight(&mut tables);

        let version = VersionContext { minecraft: minecraft.clone(), neoforge };
        let mut registries = registries;
        seed_condition_registries(&mut registries, &tables, &version);
        let mod_loader = lock.as_ref().and_then(|l| l.mod_loader().ok());
        let ignored_loaders = loaders_the_pack_ignores(mod_loader.as_ref(), &registries.mod_ids);
        let foreign_loader_folders = foreign_loader_folders(mod_loader.as_ref());

        let symbol_count = tables.mcdoc.len();
        let registry_entries: usize = registries.entries.values().map(Vec::len).sum();
        let code_source = registries.code_source;
        let index = Arc::new(DatapackIndex {
            tables,
            registries,
            version,
            path_rules,
            ignored_loaders,
            foreign_loader_folders,
            data_pack_format,
        });
        let status = DatapackStatus {
            available: true,
            minecraft,
            loader: loader_label,
            symbol_count,
            registry_entries,
            code_source,
            probe_available,
            load_ms: started.elapsed().as_millis() as u64,
            error: None,
        };
        *self.inner.lock().unwrap() = Some(CachedDatapack {
            root,
            fingerprint,
            build_started: started,
            status: status.clone(),
            index: Some(index.clone()),
        });
        (status, Some(index))
    }
}

/// Picks the JSON parser Minecraft uses for a data file, as read from each version's loaders:
/// - Through 1.21.1, the shared data loader (`SimpleJsonResourceReloadListener`, which most mods' loaders
///   also extend) reads non-leniently, while tags and `RegistryDataLoader` registries read leniently.
/// - From 1.21.11 (and, we assume, 1.21.2 on) everything uses `StrictJsonParser`, except that NeoForge
///   patches the shared data loader back to lenient through 26.2. NeoForge 26.3 is strict throughout.
fn json_dialect_for(minecraft: &str, neoforge: bool, category: Option<&str>) -> json_parser::JsonDialect {
    use json_parser::JsonDialect;
    use std::cmp::Ordering::Less;
    let tags_or_registry = category.is_some_and(|c| c.starts_with("tag/") || loads_through_registry_data_loader(c));
    if version::compare_version(minecraft, "1.21.2") == Less {
        return if tags_or_registry { JsonDialect::Lenient } else { JsonDialect::LegacyStrict };
    }
    if !tags_or_registry && neoforge && version::compare_version(minecraft, "26.3") == Less && category != Some("pack_mcmeta") {
        return JsonDialect::Lenient;
    }
    JsonDialect::Strict
}

/// Categories `RegistryDataLoader` reads: its `WORLDGEN_REGISTRIES` and dimension registries, and
/// NeoForge's datapack registries.
fn loads_through_registry_data_loader(category: &str) -> bool {
    category.starts_with("worldgen/")
        || category.ends_with("_variant")
        || matches!(
            category,
            "dimension"
                | "dimension_type"
                | "chat_type"
                | "trim_pattern"
                | "trim_material"
                | "trial_spawner"
                | "damage_type"
                | "banner_pattern"
                | "enchantment"
                | "enchantment_provider"
                | "jukebox_song"
                | "instrument"
                | "test_environment"
                | "test_instance"
                | "dialog"
                | "timeline"
                | "neoforge:biome_modifier"
                | "neoforge:structure_modifier"
        )
}

/// Folders Minecraft renamed in 1.21: (before, from 1.21 on).
const RENAMED_DATA_FOLDERS: &[(&str, &str)] = &[
    ("advancements", "advancement"),
    ("functions", "function"),
    ("item_modifiers", "item_modifier"),
    ("loot_tables", "loot_table"),
    ("predicates", "predicate"),
    ("recipes", "recipe"),
    ("structures", "structure"),
];
const RENAMED_TAG_FOLDERS: &[(&str, &str)] =
    &[("blocks", "block"), ("entity_types", "entity_type"), ("fluids", "fluid"), ("functions", "function"), ("game_events", "game_event"), ("items", "item")];

// NeoForge reads only its own `neoforge/` folders from 1.20.2 on, but it began as a Forge fork and
// still reads `forge/` on 1.20.1, so we leave NeoForge packs alone.
fn foreign_loader_folders(loader: Option<&ModLoader<'_>>) -> &'static [(&'static str, &'static str)] {
    match loader {
        Some(ModLoader::Fabric(_)) => &[("forge", "Forge"), ("neoforge", "NeoForge")],
        Some(ModLoader::Forge(_)) => &[("neoforge", "NeoForge")],
        _ => &[],
    }
}

/// Explains why we skip a multi-loader jar's data for a loader the pack doesn't run, like
/// `data/ecologics/neoforge/biome_modifier/` in a Fabric pack.
fn foreign_loader_folder_message(path: &str, folders: &[(&str, &str)]) -> Option<String> {
    let norm = path.replace('\\', "/");
    let after_data = norm.strip_prefix("data/").or_else(|| norm.find("/data/").map(|i| &norm[i + 6..]))?;
    let (_, rest) = after_data.split_once('/')?;
    let (folder, _) = rest.split_once('/')?;
    let (_, label) = folders.iter().find(|(name, _)| *name == folder)?;
    Some(format!("Only {label} loads files in {folder}/, so we don't check this file."))
}

/// Explains why Minecraft won't load a data file that sits in the other version's folder name.
fn unloaded_folder_message(path: &str, minecraft: &str) -> Option<String> {
    let norm = path.replace('\\', "/");
    let after_data = norm.strip_prefix("data/").or_else(|| norm.find("/data/").map(|i| &norm[i + 6..]))?;
    let (_, rest) = after_data.split_once('/')?;
    let (folder, rest) = rest.split_once('/')?;
    let (shown, renames) = match folder {
        "tags" => (format!("tags/{}", rest.split_once('/')?.0), RENAMED_TAG_FOLDERS),
        _ => (folder.to_string(), RENAMED_DATA_FOLDERS),
    };
    let name = shown.rsplit('/').next().unwrap_or(&shown);
    let modern = version::compare_version(minecraft, "1.21") != std::cmp::Ordering::Less;
    let prefix = shown.strip_suffix(name).unwrap_or_default();
    renames.iter().find_map(|&(before, after)| match (modern, name) {
        (true, n) if n == before => Some(format!("Minecraft {minecraft} doesn't load {shown}/, since 1.21 renamed it to {prefix}{after}/.")),
        (false, n) if n == after => Some(format!("Minecraft {minecraft} doesn't load {shown}/, since it uses {prefix}{before}/ until 1.21.")),
        _ => None,
    })
}

/// Returns whether the file's top-level load conditions fail for this pack's mods. We only decide
/// conditions about installed mods (and constants), so `item_exists` and the like count as passing.
fn requires_missing_mod(root: &json_parser::JsonNode, mod_ids: &std::collections::HashSet<String>) -> bool {
    // Fabric conditions can also be a single object rather than a list.
    let conditions = |key: &str| -> Vec<&json_parser::JsonNode> {
        match root.get(key).map(|v| (v, &v.kind)) {
            Some((_, json_parser::JsonKind::Array(items))) => items.iter().collect(),
            Some((single, json_parser::JsonKind::Object(_))) => vec![single],
            _ => Vec::new(),
        }
    };
    let neoforge_fails = |key: &str| conditions(key).into_iter().any(|c| overlays::neoforge_condition(c, mod_ids) == Some(false));
    let fabric_fails = conditions("fabric:load_conditions").into_iter().any(|c| overlays::fabric_condition(c, mod_ids) == Some(false));
    neoforge_fails("neoforge:conditions") || neoforge_fails("conditions") || fabric_fails
}

fn is_unknown_id_warning(diagnostic: &json_parser::Diagnostic) -> bool {
    diagnostic.severity == Severity::Warning
        && ((diagnostic.message.starts_with("Unknown ") && !diagnostic.message.starts_with("Unknown key"))
            || diagnostic.message.starts_with("Packraven doesn't know this"))
}

/// Adds the load condition types we define schemas for to the registries the condition `type` fields
/// name, so built-in conditions validate and complete even without a ProbeRaven dump (which supplies
/// modded ones).
fn seed_condition_registries(registries: &mut RegistryIndex, tables: &SymbolTables, version: &VersionContext) {
    for (dispatcher, registry) in fetch::CONDITION_REGISTRIES {
        let Some(map) = tables.dispatchers.get(*dispatcher) else { continue };
        let ids: Vec<String> = map
            .iter()
            .filter(|(key, ty)| !key.starts_with('%') && version.keeps(ty.attributes()))
            .map(|(key, _)| key.clone())
            .collect();
        registries.insert_entries(registry, ids);
    }
    registries.finalize();
}

fn dump_postdates_mod_changes(root: &Path) -> bool {
    let modified = |path: PathBuf| std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let Some(dumped) = [".proberaven/dump.json", "proberaven/dump.json"].iter().find_map(|rel| modified(root.join(rel))) else {
        return false;
    };
    [root.join(LOCKFILE), root.join("mods")].into_iter().filter_map(modified).all(|changed| changed <= dumped)
}

fn fingerprint_pack(root: &Path) -> String {
    let mut hasher = Sha1::new();
    for rel in [LOCKFILE, ".proberaven/dump.json", "proberaven/dump.json"] {
        let p = root.join(rel);
        hasher.update(rel.as_bytes());
        if let Ok(meta) = std::fs::metadata(&p) {
            hasher.update(meta.len().to_le_bytes());
            if let Ok(mod_t) = meta.modified()
                && let Ok(dur) = mod_t.duration_since(std::time::UNIX_EPOCH)
            {
                hasher.update(dur.as_secs().to_le_bytes());
            }
        }
    }
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn index_with_conditions(registries: RegistryIndex) -> DatapackIndex {
        let mut tables = SymbolTables::default();
        parser::parse_and_bind(fetch::PACKRAVEN_MCDOC, "::packraven", &mut tables).unwrap();
        parser::parse_and_bind(r#"dispatch minecraft:resource["recipe"] to struct { result: string }"#, "::test", &mut tables).unwrap();
        let version = VersionContext { minecraft: Some("1.21.1".into()), neoforge: Some("21.1.265".into()) };
        let mut registries = registries;
        seed_condition_registries(&mut registries, &tables, &version);
        DatapackIndex { tables, registries, version, path_rules: Vec::new(), ignored_loaders: Vec::new(), foreign_loader_folders: &[], data_pack_format: None }
    }

    #[test]
    fn built_in_conditions_validate_and_complete_without_a_dump() {
        let index = index_with_conditions(RegistryIndex::default());
        let path = "data/x/recipe/a.json";
        let neoforge = r#"{"neoforge:conditions": [{"type": "neoforge:mod_loaded", "modid": "create"}], "result": "x"}"#;
        assert!(index.lint(path, neoforge).is_empty(), "{:?}", index.lint(path, neoforge));
        let fabric = r#"{"fabric:load_conditions": [{"condition": "fabric:all_mods_loaded", "values": ["create"]}], "result": "x"}"#;
        assert!(index.lint(path, fabric).is_empty(), "{:?}", index.lint(path, fabric));

        let typing = r#"{"neoforge:conditions": [{"type": "neoforge:mod"}], "result": "x"}"#;
        let offset = typing.find("neoforge:mod\"").unwrap() + "neoforge:mod".len();
        let labels: Vec<String> = index.complete(path, typing, offset).items.into_iter().map(|i| i.label).collect();
        assert_eq!(labels, ["neoforge:mod_loaded"]);
    }

    #[test]
    fn single_fabric_conditions_need_fabric_api_for_1_20_5() {
        let index = |minecraft: &str| {
            let mut tables = SymbolTables::default();
            parser::parse_and_bind(fetch::PACKRAVEN_MCDOC, "::packraven", &mut tables).unwrap();
            parser::parse_and_bind(r#"dispatch minecraft:resource["recipe"] to struct { result: string }"#, "::test", &mut tables).unwrap();
            let version = VersionContext { minecraft: Some(minecraft.into()), neoforge: None };
            DatapackIndex { tables, registries: RegistryIndex::default(), version, path_rules: Vec::new(), ignored_loaders: Vec::new(), foreign_loader_folders: &[], data_pack_format: None }
        };
        let single = r#"{"fabric:load_conditions": {"condition": "fabric:all_mods_loaded", "values": ["create"]}, "result": "x"}"#;
        let list = r#"{"fabric:load_conditions": [{"condition": "fabric:all_mods_loaded", "values": ["create"]}], "result": "x"}"#;
        assert!(!index("1.20.1").lint("data/x/recipes/a.json", single).is_empty());
        assert!(index("1.20.1").lint("data/x/recipes/a.json", list).is_empty());
        assert!(index("1.21.1").lint("data/x/recipe/a.json", single).is_empty());
    }

    #[test]
    fn dumped_modded_conditions_are_known() {
        let mut dumped = RegistryIndex { code_source: CodeRegistrySource::ProbeRaven, ..Default::default() };
        dumped.insert_entries("neoforge:condition_codecs", ["supplementaries:flag".into()]);
        let index = index_with_conditions(dumped);
        let path = "data/x/recipe/a.json";
        let known = r#"{"neoforge:conditions": [{"type": "supplementaries:flag"}], "result": "x"}"#;
        assert!(index.lint(path, known).is_empty(), "{:?}", index.lint(path, known));
        let unknown = r#"{"neoforge:conditions": [{"type": "nobody:nothing"}], "result": "x"}"#;
        assert_eq!(index.lint(path, unknown).len(), 1);
    }

    #[test]
    fn skips_unknown_ids_in_files_gated_on_missing_mods() {
        let mut tables = SymbolTables::default();
        parser::parse_and_bind(fetch::PACKRAVEN_MCDOC, "::packraven", &mut tables).unwrap();
        parser::parse_and_bind(r#"dispatch minecraft:resource["recipe"] to struct { result: #[id="item"] string }"#, "::test", &mut tables).unwrap();
        let version = VersionContext { minecraft: Some("1.21.1".into()), neoforge: Some("21.1.265".into()) };
        let mut registries = RegistryIndex { code_source: CodeRegistrySource::ProbeRaven, ..Default::default() };
        registries.mod_ids.insert("create".into());
        seed_condition_registries(&mut registries, &tables, &version);
        let index = DatapackIndex { tables, registries, version, path_rules: Vec::new(), ignored_loaders: Vec::new(), foreign_loader_folders: &[], data_pack_format: None };
        let path = "data/x/recipe/a.json";

        let absent = r#"{"neoforge:conditions": [{"type": "neoforge:mod_loaded", "modid": "malum"}], "result": "malum:spirit"}"#;
        assert!(index.lint(path, absent).is_empty(), "{:?}", index.lint(path, absent));
        let fabric_absent = r#"{"fabric:load_conditions": [{"condition": "fabric:all_mods_loaded", "values": ["create", "malum"]}], "result": "malum:spirit"}"#;
        assert!(index.lint(path, fabric_absent).is_empty(), "{:?}", index.lint(path, fabric_absent));
        let present = r#"{"neoforge:conditions": [{"type": "neoforge:mod_loaded", "modid": "create"}], "result": "create:nothing"}"#;
        assert_eq!(index.lint(path, present).len(), 1);
    }

    #[test]
    fn trusts_the_dumped_mod_list_only_after_the_last_mod_change() {
        let root = std::env::temp_dir().join(format!("packraven-dump-age-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("mods")).unwrap();
        std::fs::create_dir_all(root.join(".proberaven")).unwrap();
        let now = std::time::SystemTime::now();
        let at = |rel: &str, offset_secs: i64| {
            let shift = std::time::Duration::from_secs(offset_secs.unsigned_abs());
            let time = if offset_secs < 0 { now - shift } else { now + shift };
            let path = root.join(rel);
            if !path.exists() {
                std::fs::write(&path, "{}").unwrap();
            }
            std::fs::File::options().write(true).open(&path).unwrap().set_modified(time).unwrap();
        };
        assert!(!dump_postdates_mod_changes(&root), "no dump yet");
        at(LOCKFILE, -100);
        at(".proberaven/dump.json", -50);
        assert!(!dump_postdates_mod_changes(&root), "the mods folder changed after the dump");
        at(".proberaven/dump.json", 100);
        assert!(dump_postdates_mod_changes(&root));
        at(LOCKFILE, 200);
        assert!(!dump_postdates_mod_changes(&root), "the lockfile changed after the dump");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn notes_files_an_overlay_replaces_instead_of_checking_them() {
        struct Jar;
        impl overlays::PackFiles for Jar {
            fn read(&self, relative: &str) -> Option<String> {
                (relative == "mods/geo.jar/pack.mcmeta")
                    .then(|| r#"{"overlays": {"entries": [{"formats": [41, 512], "min_format": 41, "max_format": 512, "directory": "f41data"}]}}"#.into())
            }
            fn exists(&self, relative: &str) -> bool {
                relative == "mods/geo.jar/f41data/data/x/recipe/a.json"
            }
        }
        let mut tables = SymbolTables::default();
        parser::parse_and_bind(r#"dispatch minecraft:resource["recipe"] to struct { result: string }"#, "::test", &mut tables).unwrap();
        let index = DatapackIndex {
            tables,
            registries: RegistryIndex::default(),
            version: VersionContext { minecraft: Some("1.21.1".into()), neoforge: None },
            path_rules: Vec::new(),
            ignored_loaders: Vec::new(),
            foreign_loader_folders: &[],
            data_pack_format: Some(overlays::PackFormat { major: 48, minor: 0 }),
        };
        let broken = r#"{"result": 1}"#;
        let base = index.lint_in_pack("mods/geo.jar/data/x/recipe/a.json", broken, &Jar);
        assert_eq!(base.len(), 1);
        assert_eq!(base[0].severity, "info");
        assert_eq!(base[0].message, "Minecraft 1.21.1 loads the copy in this pack's f41data overlay instead, so we don't check this file.");
        let overlay = index.lint_in_pack("mods/geo.jar/f41data/data/x/recipe/a.json", broken, &Jar);
        assert_eq!(overlay.len(), 1);
        assert_eq!(overlay[0].severity, "error");
    }

    #[test]
    fn leaves_conditions_for_other_loaders_unchecked() {
        let mut tables = SymbolTables::default();
        parser::parse_and_bind(fetch::PACKRAVEN_MCDOC, "::packraven", &mut tables).unwrap();
        parser::parse_and_bind(r#"dispatch minecraft:resource["recipe"] to struct { result: string }"#, "::test", &mut tables).unwrap();
        let version = VersionContext { minecraft: Some("1.21.1".into()), neoforge: None };
        let mut registries = RegistryIndex::default();
        seed_condition_registries(&mut registries, &tables, &version);
        let index = |ignored_loaders: Vec<&'static str>| DatapackIndex {
            tables: tables.clone(),
            registries: registries.clone(),
            version: version.clone(),
            path_rules: Vec::new(),
            ignored_loaders,
            foreign_loader_folders: &[],
            data_pack_format: None,
        };
        // lne_wizards ships this in its Fabric jar for its NeoForge build.
        let text = r#"{"neoforge:conditions": [{"type": "neoforge:and", "conditions": [{"type": "neoforge:mod_loaded", "modid": "a"}]}], "result": "x"}"#;
        let path = "data/x/recipe/a.json";
        assert!(!index(Vec::new()).lint(path, text).is_empty(), "the test needs a value we'd otherwise flag");
        let fabric = index(vec!["forge", "neoforge"]).lint(path, text);
        assert!(fabric.is_empty(), "{fabric:?}");
    }

    #[test]
    fn reads_primitives_as_strings_only_where_1_20_1_gson_does() {
        let mut tables = SymbolTables::default();
        parser::parse_and_bind(
            r#"
dispatch minecraft:resource["loot_table"] to struct { name: string }
dispatch minecraft:resource["worldgen/noise"] to struct { name: string }
"#,
            "::test",
            &mut tables,
        )
        .unwrap();
        let index = |minecraft: &str| DatapackIndex {
            tables: tables.clone(),
            registries: RegistryIndex::default(),
            version: VersionContext { minecraft: Some(minecraft.into()), neoforge: None },
            path_rules: Vec::new(),
            ignored_loaders: Vec::new(),
            foreign_loader_folders: &[],
            data_pack_format: None,
        };
        let text = r#"{"name": true}"#;
        assert!(index("1.20.1").lint("data/x/loot_tables/a.json", text).is_empty());
        // Codecs read world data, and every data file from 1.20.5.
        assert_eq!(index("1.20.1").lint("data/x/worldgen/noise/a.json", text).len(), 1);
        assert_eq!(index("1.21.1").lint("data/x/loot_table/a.json", text).len(), 1);
    }

    #[test]
    fn picks_the_json_parser_minecraft_uses() {
        use json_parser::JsonDialect::{LegacyStrict, Lenient, Strict};
        assert_eq!(json_dialect_for("1.20.1", false, Some("loot_table")), LegacyStrict);
        assert_eq!(json_dialect_for("1.21.1", true, Some("tag/item")), Lenient);
        assert_eq!(json_dialect_for("1.21.1", true, Some("worldgen/template_pool")), Lenient);
        assert_eq!(json_dialect_for("1.21.11", false, Some("recipe")), Strict);
        assert_eq!(json_dialect_for("1.21.11", true, Some("recipe")), Lenient);
        assert_eq!(json_dialect_for("1.21.11", true, Some("tag/item")), Strict);
        assert_eq!(json_dialect_for("26.2", true, None), Lenient);
        assert_eq!(json_dialect_for("26.3", true, Some("recipe")), Strict);
    }

    #[test]
    fn judges_nonstandard_json_by_dialect() {
        use json_parser::{parse_as, JsonDialect};
        let errors = |src: &str, dialect| parse_as(src, false, Some(dialect)).diagnostics.into_iter().filter(|d| d.severity == Severity::Error).count();
        // A book page with raw line breaks, like Integrated Stronghold's loot tables.
        let multiline = "{\"tag\": \"{pages:[\r\n  \\\"a\\\"\r\n]}\", \"n\": 1}";
        assert_eq!(errors(multiline, JsonDialect::LegacyStrict), 0);
        assert_eq!(errors(multiline, JsonDialect::Strict), 1);
        let parsed = parse_as(multiline, false, Some(JsonDialect::LegacyStrict));
        assert!(parsed.root.as_ref().and_then(|r| r.get("n")).is_some(), "parsing continues after the string");

        let lenient_only = "{ // note\n  type: 'minecraft:stone', \"a\": [1] }";
        assert_eq!(errors(lenient_only, JsonDialect::Lenient), 0);
        assert!(errors(lenient_only, JsonDialect::LegacyStrict) > 0);
        let root = parse_as(lenient_only, false, Some(JsonDialect::Lenient)).root.unwrap();
        assert_eq!(root.get("type").and_then(|v| v.as_str()), Some("minecraft:stone"));

        // Gson rejects a trailing comma in an object in every mode.
        assert_eq!(errors("{\"a\": 1,}", JsonDialect::Lenient), 1);
    }

    #[test]
    fn flags_data_folders_this_version_does_not_load() {
        assert_eq!(
            unloaded_folder_message("data/apotheosis/recipes/broadhead_arrow.json", "1.21.1").as_deref(),
            Some("Minecraft 1.21.1 doesn't load recipes/, since 1.21 renamed it to recipe/.")
        );
        assert_eq!(
            unloaded_folder_message("data/c/tags/item/ingots.json", "1.20.1").as_deref(),
            Some("Minecraft 1.20.1 doesn't load tags/item/, since it uses tags/items/ until 1.21.")
        );
        assert_eq!(unloaded_folder_message("data/x/recipe/a.json", "26.1.2"), None);
        assert_eq!(unloaded_folder_message("data/x/recipes/a.json", "1.20.1"), None);
        assert_eq!(unloaded_folder_message("data/x/tags/worldgen/biome/a.json", "1.21.1"), None);
    }

    #[test]
    fn skips_data_for_loaders_the_pack_doesnt_run() {
        let fabric = foreign_loader_folders(Some(&ModLoader::Fabric("0.16.0")));
        assert_eq!(
            foreign_loader_folder_message("data/ecologics/neoforge/biome_modifier/a.json", fabric).as_deref(),
            Some("Only NeoForge loads files in neoforge/, so we don't check this file.")
        );
        assert_eq!(
            foreign_loader_folder_message("data/x/forge/biome_modifier/a.json", fabric).as_deref(),
            Some("Only Forge loads files in forge/, so we don't check this file.")
        );
        assert_eq!(foreign_loader_folder_message("data/x/recipe/a.json", fabric), None);
        // A namespace named after a loader is ordinary data.
        assert_eq!(foreign_loader_folder_message("data/neoforge/tags/item/a.json", fabric), None);
        let forge = foreign_loader_folders(Some(&ModLoader::Forge("47.4.0")));
        assert!(foreign_loader_folder_message("data/x/neoforge/biome_modifier/a.json", forge).is_some());
        assert_eq!(foreign_loader_folder_message("data/x/forge/biome_modifier/a.json", forge), None);
        let neoforge = foreign_loader_folders(Some(&ModLoader::NeoForge("47.1.106")));
        assert_eq!(foreign_loader_folder_message("data/x/forge/biome_modifier/a.json", neoforge), None);
    }

    #[test]
    fn suggests_fabric_keys_only_where_fabric_api_loads_them() {
        let none = HashSet::new();
        let ffapi: HashSet<String> = ["fabric_api".to_string()].into();
        let neoforge = ModLoader::NeoForge("21.1.265");
        assert_eq!(loaders_the_pack_ignores(Some(&neoforge), &none), ["forge", "fabric"]);
        assert_eq!(loaders_the_pack_ignores(Some(&neoforge), &ffapi), ["forge"]);
        assert_eq!(loaders_the_pack_ignores(Some(&ModLoader::Fabric("0.16.0")), &none), ["forge", "neoforge"]);
        assert_eq!(loaders_the_pack_ignores(Some(&ModLoader::Forge("47.4.10")), &ffapi), ["neoforge"]);
        assert!(loaders_the_pack_ignores(None, &none).is_empty());
    }
}
