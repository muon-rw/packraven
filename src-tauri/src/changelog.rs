use std::collections::{BTreeMap, HashMap};
use std::fmt::Write;
use std::io::Read;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::curseforge;
use crate::git;
use crate::pack::{CurseforgeFile, LockEntry, Lockfile, ModKey, Pack, CHANGELOG, LOCKFILE};
use crate::sides::{MetadataCache, ProjectName};

const MAX_CACHED_FILE_NAMES: usize = 10_000;

static FILE_DISPLAY_NAMES: OnceLock<Mutex<HashMap<u64, String>>> = OnceLock::new();

async fn cached_file_names(ids: &[u64]) -> Result<HashMap<u64, String>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let cache = FILE_DISPLAY_NAMES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut found = HashMap::with_capacity(ids.len());
    let mut missing = Vec::new();
    {
        let guard = cache.lock().unwrap();
        for &id in ids {
            if let Some(name) = guard.get(&id) {
                found.insert(id, name.clone());
            } else {
                missing.push(id);
            }
        }
    }
    if !missing.is_empty() {
        missing.sort_unstable();
        missing.dedup();
        let fetched = curseforge::files(&missing).await?;
        let mut guard = cache.lock().unwrap();
        if guard.len() + fetched.len() > MAX_CACHED_FILE_NAMES {
            guard.clear();
        }
        for (id, file) in fetched {
            guard.insert(id, file.display_name.clone());
            found.insert(id, file.display_name);
        }
    }
    Ok(found)
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ModChange {
    pub name: String,
    pub url: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub in_mod_list: bool,
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct ModDiff {
    pub since: Option<String>,
    pub baseline: bool,
    pub added: Vec<ModChange>,
    pub updated: Vec<ModChange>,
    pub removed: Vec<ModChange>,
}

pub fn read_notes(pack: &Pack) -> String {
    std::fs::read_to_string(pack.dir.join(CHANGELOG)).unwrap_or_default()
}

pub fn write_notes(pack: &Pack, text: &str) -> Result<()> {
    std::fs::write(pack.dir.join(CHANGELOG), text)?;
    Ok(())
}

pub const BASELINE_FILE: &str = "compare-baseline.json";

#[derive(Serialize, Deserialize)]
pub struct Baseline {
    label: String,
    lock: Lockfile,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CurseForgeManifest {
    name: Option<String>,
    version: Option<String>,
    files: Vec<ManifestFile>,
}

#[derive(Deserialize)]
struct ManifestFile {
    #[serde(rename = "projectID")]
    project_id: u64,
    #[serde(rename = "fileID")]
    file_id: u64,
}

pub fn baseline_from_zip(zip: &Path) -> Result<Baseline> {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(zip)?).context("not a zip file")?;
    let mut text = String::new();
    archive.by_name("manifest.json").context("this zip has no manifest.json, so it isn't a CurseForge modpack export")?.read_to_string(&mut text)?;
    let manifest: CurseForgeManifest = serde_json::from_str(&text).context("couldn't read manifest.json")?;
    let label = match (manifest.name, manifest.version) {
        (Some(name), Some(version)) => format!("{name} {version}"),
        _ => zip.file_stem().unwrap_or_default().to_string_lossy().into_owned(),
    };
    let files = manifest
        .files
        .into_iter()
        .map(|f| LockEntry::curseforge(String::new(), String::new(), 0, CurseforgeFile { project_id: f.project_id, file_id: f.file_id, url: String::new() }))
        .collect();
    Ok(Baseline { label, lock: Lockfile { files, ..Default::default() } })
}

pub fn save_baseline(path: &Path, baseline: &Baseline) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string(baseline)?)?;
    Ok(())
}

pub fn clear_baseline(path: &Path) -> Result<()> {
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

fn load_baseline(path: &Path) -> Option<Baseline> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn by_project(lock: &Lockfile) -> BTreeMap<ModKey, &LockEntry> {
    lock.files.iter().filter_map(|e| Some((e.key()?, e))).collect()
}

pub struct PreviousLock {
    pub since: String,
    pub baseline: bool,
    pub lock: Lockfile,
}

pub fn previous_lock(dir: &Path, baseline_path: &Path) -> Result<Option<PreviousLock>> {
    if let Some(baseline) = load_baseline(baseline_path) {
        return Ok(Some(PreviousLock { since: baseline.label, baseline: true, lock: baseline.lock }));
    }
    let Some(tag) = git::last_release_tag(dir) else { return Ok(None) };
    let lock = git::show(dir, &tag, LOCKFILE).map(|t| Lockfile::parse(&t)).transpose()?.unwrap_or_default();
    Ok(Some(PreviousLock { since: tag, baseline: false, lock }))
}

async fn diff_with_cache(pack: &Pack, current: &Lockfile, previous: PreviousLock, cache: &MetadataCache) -> Result<ModDiff> {
    let old = by_project(&previous.lock);
    let new = by_project(current);
    let listed = by_project(&pack.lock);
    let changed_files: Vec<u64> = new
        .iter()
        .filter_map(|(key, e)| old.get(key).filter(|o| !o.same_file(e)).map(|o| [o, *e]))
        .flatten()
        .filter_map(|e| e.curseforge.as_ref().map(|f| f.file_id))
        .collect();
    let file_names = cached_file_names(&changed_files).await?;
    let display = |entry: &LockEntry| {
        entry.curseforge.as_ref().and_then(|f| file_names.get(&f.file_id)).cloned().unwrap_or_else(|| ProjectName::of_file(&entry.path).name)
    };
    let change = |key: &ModKey, from: Option<String>, to: Option<String>, in_mod_list: bool| {
        let name = cache.name(key);
        ModChange { name: name.name, url: name.url, from, to, in_mod_list }
    };
    let mut diff = ModDiff { since: Some(previous.since), baseline: previous.baseline, ..Default::default() };
    for (key, entry) in &new {
        let in_mod_list = listed.get(key).is_some_and(|l| l.same_file(entry));
        match old.get(key) {
            None => diff.added.push(change(key, None, None, in_mod_list)),
            Some(previous) if !previous.same_file(entry) => diff.updated.push(change(key, Some(display(previous)), Some(display(entry)), in_mod_list)),
            Some(_) => {}
        }
    }
    for key in old.keys().filter(|key| !new.contains_key(key)) {
        diff.removed.push(change(key, None, None, !listed.contains_key(key)));
    }
    for list in [&mut diff.added, &mut diff.updated, &mut diff.removed] {
        list.sort_by_key(|c| c.name.to_lowercase());
    }
    Ok(diff)
}

pub async fn mod_diff(pack: &Pack, current: &Lockfile, cache: &mut MetadataCache, baseline_path: &Path) -> Result<ModDiff> {
    let Some(previous) = previous_lock(&pack.dir, baseline_path)? else { return Ok(ModDiff::default()) };
    let keys: Vec<ModKey> = by_project(&previous.lock).into_keys().chain(by_project(current).into_keys()).collect();
    cache.ensure_names(&keys).await?;
    diff_with_cache(pack, current, previous, cache).await
}

pub async fn mod_diff_cached(pack: &Pack, current: &Lockfile, previous: PreviousLock, cache_path: &Path) -> Result<ModDiff> {
    let keys: Vec<ModKey> = by_project(&previous.lock).into_keys().chain(by_project(current).into_keys()).collect();
    let shared = MetadataCache::load_shared(cache_path);
    if shared.has_all_names(&keys) {
        return diff_with_cache(pack, current, previous, &shared).await;
    }
    let mut cache = (*shared).clone();
    cache.ensure_names(&keys).await?;
    if cache.is_dirty() {
        cache.save(cache_path)?;
    }
    diff_with_cache(pack, current, previous, &cache).await
}

const PART_START: &[char] = &['-', '_', '+', ' '];
const PART_END: &[char] = &['-', '_', '+', ' ', '.'];

// Two files of one mod usually share a long name, so we keep only the part of each that differs,
// cut at a separator: Apotheosis-1.21.1-8.1.1.jar to Apotheosis-1.21.1-8.1.2.jar reads 8.1.1 to 8.1.2.
// The Mods view trims names the same way (changedParts in src/lib/fileNames.ts).
fn changed_parts(from: &str, to: &str) -> (String, String) {
    let (old, new): (Vec<char>, Vec<char>) = (from.chars().collect(), to.chars().collect());
    let mut start = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    while start > 0 && !PART_START.contains(&new[start - 1]) {
        start -= 1;
    }
    let room = old.len().min(new.len()) - start;
    let mut end = old.iter().rev().zip(new.iter().rev()).take(room).take_while(|(a, b)| a == b).count();
    while end > 0 && !PART_END.contains(&new[new.len() - end]) {
        end -= 1;
    }
    let part = |name: &[char]| name[start..name.len() - end].iter().collect::<String>().trim_matches(PART_END).to_string();
    let (old_part, new_part) = (part(&old), part(&new));
    if old_part.chars().count() >= 2 && new_part.chars().count() >= 2 {
        (old_part, new_part)
    } else {
        (from.to_string(), to.to_string())
    }
}

pub fn render(notes: &str, diff: &ModDiff) -> String {
    let mut out = notes.trim_end().to_string();
    let link = |c: &ModChange| match &c.url {
        Some(url) => format!("[{}]({url})", c.name),
        None => c.name.clone(),
    };
    if diff.added.is_empty() && diff.updated.is_empty() && diff.removed.is_empty() {
        return out;
    }
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str("## Mod changes\n");
    if !diff.added.is_empty() {
        out.push_str("\n### Added\n");
        for c in &diff.added {
            let _ = writeln!(out, "- {}", link(c));
        }
    }
    if !diff.updated.is_empty() {
        out.push_str("\n### Updated\n");
        for c in &diff.updated {
            let (from, to) = changed_parts(c.from.as_deref().unwrap_or("?"), c.to.as_deref().unwrap_or("?"));
            let _ = writeln!(out, "- {}: {from} to {to}", link(c));
        }
    }
    if !diff.removed.is_empty() {
        out.push_str("\n### Removed\n");
        for c in &diff.removed {
            let _ = writeln!(out, "- {}", link(c));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn reads_curseforge_manifest_from_zip() {
        let path = std::env::temp_dir().join("packraven-baseline-test.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        zip.start_file("manifest.json", zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(br#"{"name":"Pack","version":"1.2.0","files":[{"projectID":10,"fileID":20,"required":true}]}"#).unwrap();
        zip.finish().unwrap();
        let baseline = baseline_from_zip(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(baseline.label, "Pack 1.2.0");
        assert_eq!(baseline.lock.files[0].curseforge, Some(CurseforgeFile { project_id: 10, file_id: 20, url: String::new() }));
    }

    #[test]
    fn updated_mods_show_only_the_changed_part_of_each_name() {
        assert_eq!(changed_parts("Apotheosis-1.21.1-8.1.1.jar", "Apotheosis-1.21.1-8.1.2.jar"), ("8.1.1".into(), "8.1.2".into()));
        assert_eq!(changed_parts("sodium-neoforge-0.6.13.jar", "sodium-neoforge-0.6.15.jar"), ("0.6.13".into(), "0.6.15".into()));
        assert_eq!(changed_parts("Mod-1.20.1-2.0.jar", "Mod-1.21.1-2.1.jar"), ("1.20.1-2.0".into(), "1.21.1-2.1".into()));
        assert_eq!(changed_parts("Create 6.0.4", "Create 6.0.6"), ("6.0.4".into(), "6.0.6".into()));
        assert_eq!(changed_parts("mod-1.jar", "mod-2.jar"), ("mod-1.jar".into(), "mod-2.jar".into()));
        assert_eq!(changed_parts("Pokémon-1.0.jar", "Pokémon-1.1.jar"), ("1.0".into(), "1.1".into()));
    }

    #[test]
    fn render_trims_updated_mod_names() {
        let change = ModChange { name: "Apotheosis".into(), url: None, from: Some("Apotheosis-1.21.1-8.1.1.jar".into()), to: Some("Apotheosis-1.21.1-8.1.2.jar".into()), in_mod_list: true };
        let diff = ModDiff { updated: vec![change], ..Default::default() };
        assert_eq!(render("", &diff), "## Mod changes\n\n### Updated\n- Apotheosis: 8.1.1 to 8.1.2\n");
    }
}
