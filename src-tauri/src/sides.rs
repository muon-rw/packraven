use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::links::Skew;
use crate::pack::{LockEntry, ModKey, Pack};
use crate::{curseforge, modrinth};

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MetadataCache {
    sides: HashMap<String, SideMetadata>,
    names: HashMap<ModKey, ProjectName>,
    #[serde(default)]
    skews: HashMap<String, CheckedSkew>,
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
}

impl MetadataCache {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
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
    let config = &pack.config.server;
    let mut result: Vec<ModSide> = entries
        .into_iter()
        .map(|entry| {
            let metadata = cache.sides.get(&entry.sha1).cloned().unwrap_or_default();
            let (detected, reason) = detect(&metadata);
            let (side, reason, r#override) = if config.client_only.iter().any(|key| entry.has_key(key)) {
                (Side::Client, "Marked client-only".to_string(), Override::Client)
            } else if config.keep.iter().any(|key| entry.has_key(key)) {
                (Side::Both, "Kept on the server".to_string(), Override::Keep)
            } else if mistagged_client_only(entry) {
                (Side::Both, "Kept on the server by packraven: server mods need it, despite its client-only tags".to_string(), Override::Auto)
            } else {
                (detected, reason, Override::Auto)
            };
            let key = entry.key();
            let name = key.as_ref().map_or_else(|| ProjectName::of_file(&entry.path), |key| cache.name(key));
            ModSide { path: entry.path.clone(), key: key.map(|k| k.to_string()), name: name.name, url: name.url, side, reason, r#override }
        })
        .collect();
    result.sort_by_key(|m| m.name.to_lowercase());
    Ok(result)
}

// Both platforms tag these client-only, but server-side mods depend on them and they load fine on a
// dedicated server. Enderscape 3 requires Trim Patcher on both sides.
fn mistagged_client_only(entry: &LockEntry) -> bool {
    const CURSEFORGE: [u64; 1] = [1_414_986];
    const MODRINTH: [&str; 1] = ["bNK1MS7o"];
    entry.curseforge_project().is_some_and(|id| CURSEFORGE.contains(&id)) || entry.modrinth.as_ref().is_some_and(|f| MODRINTH.contains(&f.project_id.as_str()))
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
