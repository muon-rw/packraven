use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::links::Skew;
use crate::mod_manifest::{self, ModManifest};
use crate::pack::{LockEntry, ModKey, Pack};
use crate::{curseforge, modrinth};

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MetadataCache {
    sides: HashMap<String, SideMetadata>,
    names: HashMap<ModKey, ProjectName>,
    #[serde(default)]
    skews: HashMap<String, CheckedSkew>,
    #[serde(default)]
    manifests: HashMap<String, ReadManifest>,
    // Reading every jar's metadata is the slow part of classifying, so callers that otherwise only read the
    // cache save it when we've read new jars.
    #[serde(skip)]
    read_new_manifests: bool,
}

// Bump this when mod_manifest reads something an older read didn't, so we read every jar again.
const MANIFEST_FORMAT: u32 = 2;

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ReadManifest {
    format: u32,
    manifest: ModManifest,
}

// Authors upload new versions all the time, so we look again after a day.
const SKEW_RECHECK_SECS: u64 = 24 * 60 * 60;
// Bump this when Skew gains something an older lookup didn't fill in, so we look again right away.
const SKEW_FORMAT: u32 = 1;

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct CheckedSkew {
    checked_at: u64,
    #[serde(default)]
    format: u32,
    skew: Option<Skew>,
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct SideMetadata {
    modrinth_client: Option<String>,
    modrinth_server: Option<String>,
    curseforge_tags: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ProjectName {
    pub name: String,
    pub url: Option<String>,
    // Caches written before we kept authors have none, so None means "fetch again".
    #[serde(default)]
    pub authors: Option<Vec<String>>,
    #[serde(default)]
    pub logo: Option<String>,
    // Like authors, None means a cache from before we kept licenses. A project without one has Some("").
    #[serde(default)]
    pub license: Option<String>,
}

impl ProjectName {
    pub fn of_file(path: &str) -> Self {
        Self { name: path.rsplit('/').next().unwrap_or(path).to_string(), url: None, authors: None, logo: None, license: None }
    }
}

#[derive(Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Both,
    Client,
    Server,
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum Override {
    Auto,
    Client,
    Keep,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ModSide {
    pub path: String,
    pub key: Option<String>,
    pub name: String,
    pub url: Option<String>,
    pub side: Side,
    pub reason: String,
    pub r#override: Override,
    // The server mods that require this one, so the view can name them all and flag the ones you excluded.
    pub required_by: Vec<String>,
}

impl ModSide {
    /// Returns whether you excluded a mod that server mods require, which keeps the server from starting.
    pub fn breaks_server(&self) -> bool {
        self.side == Side::Client && !self.required_by.is_empty()
    }
}

impl MetadataCache {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    /// Returns whether we read jars the cache didn't know yet, so it's worth saving.
    pub fn read_new_manifests(&self) -> bool {
        self.read_new_manifests
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string(self)?)?;
        Ok(())
    }

    pub async fn ensure_names(&mut self, keys: &[ModKey]) -> Result<()> {
        let missing: Vec<&ModKey> = keys.iter().filter(|key| self.names.get(key).is_none_or(|n| n.authors.is_none() || n.license.is_none())).collect();
        let from_curseforge: Vec<u64> = missing.iter().filter_map(|key| key.curseforge_id()).collect();
        let from_modrinth: Vec<String> = missing
            .iter()
            .filter_map(|key| match key {
                ModKey::Modrinth(id) => Some(id.clone()),
                ModKey::Curseforge(_) => None,
            })
            .collect();
        if !from_curseforge.is_empty() {
            for (id, m) in curseforge::mods(&from_curseforge).await? {
                let authors = Some(m.authors.into_iter().map(|a| a.name).collect());
                let license = Some(m.license_type.unwrap_or_default());
                self.names.insert(ModKey::Curseforge(id), ProjectName { name: m.name, url: m.links.website_url, authors, logo: m.logo.map(|l| l.thumbnail_url), license });
            }
        }
        if !from_modrinth.is_empty() {
            let projects = modrinth::projects(&from_modrinth).await?;
            let mut authors = modrinth::authors(&projects.values().collect::<Vec<_>>()).await?;
            for (id, project) in projects {
                let license = Some(project.license.as_ref().map(modrinth::ProjectLicense::display).unwrap_or_default());
                let name = ProjectName { url: Some(project.url()), authors: Some(authors.remove(&id).unwrap_or_default()), logo: project.icon_url, license, name: project.title };
                self.names.insert(ModKey::Modrinth(id), name);
            }
        }
        Ok(())
    }

    /// Returns what Modrinth has of the mod behind a file it doesn't host, when we looked within the last day.
    pub fn skew(&self, key: &str) -> Option<Option<&Skew>> {
        self.skews
            .get(key)
            .filter(|checked| checked.format == SKEW_FORMAT && unix_now().saturating_sub(checked.checked_at) < SKEW_RECHECK_SECS)
            .map(|checked| checked.skew.as_ref())
    }

    pub fn remember_skew(&mut self, key: String, skew: Option<Skew>) {
        self.skews.insert(key, CheckedSkew { checked_at: unix_now(), format: SKEW_FORMAT, skew });
    }

    #[cfg(test)]
    pub fn remember(&mut self, key: ModKey, name: ProjectName) {
        self.names.insert(key, name);
    }

    pub fn known_name(&self, key: &ModKey) -> Option<&ProjectName> {
        self.names.get(key)
    }

    pub fn name(&self, key: &ModKey) -> ProjectName {
        let id = match key {
            ModKey::Curseforge(id) => id.to_string(),
            ModKey::Modrinth(id) => id.clone(),
        };
        self.names.get(key).cloned().unwrap_or(ProjectName { name: format!("Project {id}"), url: None, authors: None, logo: None, license: None })
    }

    // A jar we can't read counts as requiring nothing, so it keeps whatever its tags say. We don't
    // remember that, since the jar may just not be downloaded yet.
    fn manifest(&mut self, pack_dir: &Path, entry: &LockEntry) -> ModManifest {
        if let Some(read) = self.manifests.get(&entry.sha1).filter(|read| read.format == MANIFEST_FORMAT) {
            return read.manifest.clone();
        }
        let Ok(manifest) = mod_manifest::read(&pack_dir.join(&entry.path)) else { return ModManifest::default() };
        self.manifests.insert(entry.sha1.clone(), ReadManifest { format: MANIFEST_FORMAT, manifest: manifest.clone() });
        self.read_new_manifests = true;
        manifest
    }

    async fn ensure_sides(&mut self, entries: &[&LockEntry]) -> Result<()> {
        let missing: Vec<&LockEntry> = entries.iter().copied().filter(|e| !self.sides.contains_key(&e.sha1)).collect();
        if missing.is_empty() {
            return Ok(());
        }
        // Entries that record Modrinth's copy already name its project, so we only look the rest up by hash.
        let hashes: Vec<String> = missing.iter().filter(|e| e.modrinth.is_none()).map(|e| e.sha1.clone()).collect();
        let by_hash = if hashes.is_empty() { HashMap::new() } else { modrinth::files_by_sha1(&hashes).await? };
        let project_of = |e: &LockEntry| e.modrinth.as_ref().or_else(|| by_hash.get(&e.sha1)).map(|f| f.project_id.clone());
        let mut ids: Vec<String> = missing.iter().filter_map(|e| project_of(e)).collect();
        ids.sort();
        ids.dedup();
        let from_modrinth = modrinth::projects(&ids).await?;
        let file_ids: Vec<u64> = missing.iter().filter_map(|e| e.curseforge.as_ref()).map(|f| f.file_id).collect();
        let from_curseforge = curseforge::files(&file_ids).await?;
        for entry in missing {
            let project = project_of(entry).and_then(|id| from_modrinth.get(&id));
            self.sides.insert(
                entry.sha1.clone(),
                SideMetadata {
                    modrinth_client: project.map(|p| p.client_side.clone()),
                    modrinth_server: project.map(|p| p.server_side.clone()),
                    curseforge_tags: entry.curseforge.as_ref().and_then(|f| from_curseforge.get(&f.file_id)).map(|f| f.game_versions.clone()).unwrap_or_default(),
                },
            );
        }
        Ok(())
    }
}

pub fn is_server_candidate(entry: &LockEntry) -> bool {
    entry.path.starts_with("mods/") && entry.path.ends_with(".jar")
}

pub async fn classify(pack: &Pack, cache: &mut MetadataCache) -> Result<Vec<ModSide>> {
    let entries: Vec<&LockEntry> = pack.lock.files.iter().filter(|e| is_server_candidate(e)).collect();
    cache.ensure_sides(&entries).await?;
    cache.ensure_names(&entries.iter().filter_map(|e| e.key()).collect::<Vec<_>>()).await?;
    Ok(classify_known(pack, cache))
}

/// Classifies with only what the cache already knows, treating a mod with no cached tags as running on both sides.
pub fn classify_known(pack: &Pack, cache: &mut MetadataCache) -> Vec<ModSide> {
    let entries: Vec<&LockEntry> = pack.lock.files.iter().filter(|e| is_server_candidate(e)).collect();
    let manifests: Vec<ModManifest> = entries.iter().map(|entry| cache.manifest(&pack.dir, entry)).collect();
    let config = &pack.config.server;
    let mut result: Vec<ModSide> = entries
        .iter()
        .zip(&manifests)
        .map(|(&entry, manifest)| {
            let metadata = cache.sides.get(&entry.sha1).cloned().unwrap_or_default();
            let (detected, reason) = match detect(&metadata) {
                (Side::Client, reason) => (Side::Client, reason),
                _ if manifest.client_only => (Side::Client, "fabric.mod.json: client only".to_string()),
                detected => detected,
            };
            let (side, reason, r#override) = if config.client_only.iter().any(|key| entry.has_key(key)) {
                (Side::Client, "Marked client-only".to_string(), Override::Client)
            } else if config.keep.iter().any(|key| entry.has_key(key)) {
                (Side::Both, "Kept on the server".to_string(), Override::Keep)
            } else {
                (detected, reason, Override::Auto)
            };
            let key = entry.key();
            let name = key.as_ref().map_or_else(|| ProjectName::of_file(&entry.path), |key| cache.name(key));
            ModSide { path: entry.path.clone(), key: key.map(|k| k.to_string()), name: name.name, url: name.url, side, reason, r#override, required_by: Vec::new() }
        })
        .collect();
    keep_required_dependencies(&mut result, &manifests);
    result.sort_by_key(|m| m.name.to_lowercase());
    result
}

// Platform tags often call a library client-only when server mods need it, and Fabric won't start
// a server that's missing a required dependency, even one whose own jar says it's client-only
// (Fabric then just doesn't load that one). So we ship whatever a server mod requires.
fn keep_required_dependencies(mods: &mut [ModSide], manifests: &[ModManifest]) {
    let mut provider: HashMap<&str, usize> = HashMap::new();
    for (index, manifest) in manifests.iter().enumerate() {
        for id in &manifest.ids {
            provider.entry(id).or_insert(index);
        }
    }
    let excluded_by_you = |m: &ModSide| matches!(m.r#override, Override::Client);
    let mut pulled = vec![false; mods.len()];
    loop {
        let kept: Vec<usize> = (0..mods.len()).filter(|&i| mods[i].side != Side::Client).collect();
        let on_server: std::collections::HashSet<&str> = kept.iter().flat_map(|&i| manifests[i].ids.iter().chain(&manifests[i].bundled_ids)).map(String::as_str).collect();
        let missing: Vec<usize> = kept
            .iter()
            .flat_map(|&i| &manifests[i].requires)
            .filter(|id| !on_server.contains(id.as_str()))
            .filter_map(|id| provider.get(id.as_str()).copied())
            .filter(|&j| !excluded_by_you(&mods[j]))
            .collect();
        if missing.is_empty() {
            break;
        }
        for j in missing {
            mods[j].side = Side::Both;
            pulled[j] = true;
        }
    }
    let mut required_by: Vec<Vec<usize>> = vec![Vec::new(); mods.len()];
    for i in (0..mods.len()).filter(|&i| mods[i].side != Side::Client) {
        for j in manifests[i].requires.iter().filter_map(|id| provider.get(id.as_str()).copied()) {
            if j != i && (pulled[j] || excluded_by_you(&mods[j])) && !required_by[j].contains(&i) {
                required_by[j].push(i);
            }
        }
    }
    for (j, requirers) in required_by.into_iter().enumerate().filter(|(_, r)| !r.is_empty()) {
        let mut names: Vec<String> = requirers.iter().map(|&i| mods[i].name.clone()).collect();
        names.sort_by_key(|name| name.to_lowercase());
        let verb = if names.len() == 1 { "requires" } else { "require" };
        // Fabric and (Neo)Forge both refuse to start without a required dependency.
        mods[j].reason = if pulled[j] {
            format!("{} ({})", required_by_text(&names), mods[j].reason)
        } else {
            format!("Marked client-only, but {} {verb} it, so the server won't start", list(&names))
        };
        mods[j].required_by = names;
    }
}

fn required_by_text(names: &[String]) -> String {
    match names.len() {
        1 => format!("Required by {}, which is needed on the server", names[0]),
        2 | 3 => format!("Required by {}, which are needed on the server", list(names)),
        _ => format!("Required by {}", list(names)),
    }
}

pub fn list(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [a, b] => format!("{a} and {b}"),
        [a, b, c] => format!("{a}, {b} and {c}"),
        [a, b, rest @ ..] => format!("{a}, {b} and {} other server mods", rest.len()),
    }
}

fn detect(metadata: &SideMetadata) -> (Side, String) {
    match (metadata.modrinth_client.as_deref(), metadata.modrinth_server.as_deref()) {
        (_, Some("unsupported")) => return (Side::Client, "Modrinth: server unsupported".into()),
        (Some("unsupported"), _) => return (Side::Server, "Modrinth: client unsupported".into()),
        (Some(_), Some(_)) => return (Side::Both, "Modrinth: runs on both".into()),
        _ => {}
    }
    let client = metadata.curseforge_tags.iter().any(|t| t == "Client");
    let server = metadata.curseforge_tags.iter().any(|t| t == "Server");
    match (client, server) {
        (true, false) => (Side::Client, "CurseForge: tagged Client only".into()),
        (false, true) => (Side::Server, "CurseForge: tagged Server only".into()),
        (true, true) => (Side::Both, "CurseForge: tagged Client and Server".into()),
        (false, false) => (Side::Both, "No side metadata".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn side(name: &str, side: Side, r#override: Override) -> ModSide {
        ModSide { path: format!("mods/{name}.jar"), key: None, name: name.to_string(), url: None, side, reason: "tags".into(), r#override, required_by: Vec::new() }
    }

    fn manifest(ids: &[&str], bundled: &[&str], requires: &[&str]) -> ModManifest {
        let strings = |list: &[&str]| list.iter().map(|s| s.to_string()).collect();
        ModManifest { ids: strings(ids), bundled_ids: strings(bundled), requires: strings(requires), client_only: false }
    }

    #[test]
    fn server_mods_keep_their_client_tagged_dependencies_and_theirs() {
        let mut mods = [side("Chipped", Side::Both, Override::Auto), side("Athena", Side::Client, Override::Auto), side("Mod Menu", Side::Client, Override::Auto), side("Zoomify", Side::Client, Override::Auto)];
        let manifests = [manifest(&["chipped"], &[], &["athena", "minecraft"]), manifest(&["athena"], &[], &["modmenu"]), manifest(&["modmenu"], &[], &[]), manifest(&["zoomify"], &[], &["modmenu"])];
        keep_required_dependencies(&mut mods, &manifests);
        assert!(mods[..3].iter().all(|m| m.side == Side::Both));
        assert_eq!(mods[1].reason, "Required by Chipped, which is needed on the server (tags)");
        assert_eq!(mods[1].required_by, ["Chipped"]);
        assert_eq!(mods[2].reason, "Required by Athena, which is needed on the server (tags)");
        assert!(mods[3].side == Side::Client && mods[3].reason == "tags");
    }

    #[test]
    fn bundled_libraries_satisfy_server_mods() {
        let mut mods = [side("Enderscape", Side::Both, Override::Auto), side("MixinExtras", Side::Client, Override::Auto)];
        keep_required_dependencies(&mut mods, &[manifest(&["enderscape"], &["mixinextras"], &["mixinextras"]), manifest(&["mixinextras"], &[], &[])]);
        assert!(mods[1].side == Side::Client);
    }

    #[test]
    fn your_exclusion_wins_but_warns() {
        let mut mods = [side("Simply Swords", Side::Both, Override::Auto), side("Simply Bows", Side::Both, Override::Keep), side("s_lib", Side::Client, Override::Client)];
        keep_required_dependencies(&mut mods, &[manifest(&["simplyswords"], &[], &["s_lib"]), manifest(&["simplybows"], &[], &["s_lib"]), manifest(&["s_lib"], &[], &[])]);
        assert!(mods[2].side == Side::Client);
        assert_eq!(mods[2].reason, "Marked client-only, but Simply Bows and Simply Swords require it, so the server won't start");
        assert!(mods[2].breaks_server());
    }

    #[test]
    fn many_requirers_get_counted() {
        let names = ["A", "B", "C", "D", "E"].map(String::from);
        assert_eq!(required_by_text(&names[..3]), "Required by A, B and C, which are needed on the server");
        assert_eq!(required_by_text(&names), "Required by A, B and 3 other server mods");
    }
}
