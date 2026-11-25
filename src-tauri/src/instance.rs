use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::modsync;
use crate::pack::{CurseforgeFile, Loader};

const CURSEFORGE_INSTANCE: &str = "minecraftinstance.json";

#[derive(Clone)]
pub struct Instance {
    pub name: Option<String>,
    pub minecraft: Option<String>,
    pub loader: Option<Loader>,
    /// The launcher's name for the loader's version profile, which launching reads.
    pub version_id: Option<String>,
    pub thumbnail_url: Option<String>,
    pub addon_count: usize,
    /// Names of the files the launcher installed, including their disabled forms.
    pub installed_names: HashSet<String>,
    pub sources_by_sha1: HashMap<String, CurseforgeFile>,
}

// The launcher rewrites these as you add and remove content, so a change to one can change the mod list.
pub fn is_metadata_file(relative: &str) -> bool {
    relative == CURSEFORGE_INSTANCE
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawInstance {
    name: Option<String>,
    game_version: Option<String>,
    base_mod_loader: Option<RawModLoader>,
    installed_modpack: Option<RawInstalledModpack>,
    #[serde(default)]
    installed_addons: Vec<RawAddon>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawInstalledModpack {
    thumbnail_url: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawModLoader {
    name: Option<String>,
    version_json: Option<String>,
}

#[derive(Deserialize)]
struct RawProfile {
    id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawAddon {
    #[serde(rename = "addonID")]
    addon_id: Option<u64>,
    file_name_on_disk: Option<String>,
    installed_file: Option<RawInstalledFile>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawInstalledFile {
    id: Option<u64>,
    file_name_on_disk: Option<String>,
    file_name: Option<String>,
    download_url: Option<String>,
    #[serde(default)]
    hashes: Vec<RawHash>,
}

#[derive(Deserialize)]
struct RawHash {
    #[serde(rename = "type")]
    kind: Option<u64>,
    value: Option<String>,
}

type FileStamp = (u64, Option<SystemTime>);

struct CachedInstance {
    stamp: FileStamp,
    instance: Instance,
}

static INSTANCE_CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedInstance>>> = OnceLock::new();

fn file_stamp(path: &Path) -> Option<FileStamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()))
}

impl Instance {
    pub fn read(dir: &Path) -> Result<Self> {
        let path = dir.join(CURSEFORGE_INSTANCE);
        let stamp = file_stamp(&path).with_context(|| format!("{} is not a CurseForge instance (no {CURSEFORGE_INSTANCE})", dir.display()))?;
        let cache = INSTANCE_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        if let Some(entry) = cache.lock().unwrap().get(&path)
            && entry.stamp == stamp
        {
            return Ok(entry.instance.clone());
        }
        let raw: RawInstance = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(t.trim_start_matches('\u{feff}')).ok())
            .with_context(|| format!("{} is not a CurseForge instance (no {CURSEFORGE_INSTANCE})", dir.display()))?;
        let mut installed_names = HashSet::new();
        let mut sources_by_sha1 = HashMap::new();
        for addon in &raw.installed_addons {
            let file = addon.installed_file.as_ref();
            for name in [
                addon.file_name_on_disk.as_deref(),
                file.and_then(|f| f.file_name_on_disk.as_deref()),
                file.and_then(|f| f.file_name.as_deref()),
            ]
            .into_iter()
            .flatten()
            {
                installed_names.insert(name.to_string());
                installed_names.insert(format!("{name}.disabled"));
            }
            let Some(file) = file else { continue };
            let sha1 = file.hashes.iter().find(|h| h.kind == Some(1)).and_then(|h| h.value.as_deref());
            let (Some(sha1), Some(project_id), Some(file_id)) = (sha1, addon.addon_id, file.id) else { continue };
            let url = match file.download_url.as_deref().filter(|u| !u.is_empty()) {
                Some(url) => url.to_string(),
                None => modsync::cdn_url(file_id, file.file_name.as_deref().unwrap_or_default()),
            };
            sources_by_sha1.insert(sha1.to_string(), CurseforgeFile { project_id, file_id, url });
        }
        let loader_name = raw.base_mod_loader.as_ref().and_then(|l| l.name.as_deref());
        // CurseForge names Fabric loaders fabric-<version>-<minecraft> but installs their profile as
        // fabric-loader-<version>-<minecraft>, so we take the id from the profile CurseForge installed.
        let profile_id = raw
            .base_mod_loader
            .as_ref()
            .and_then(|l| l.version_json.as_deref())
            .and_then(|json| serde_json::from_str::<RawProfile>(json).ok())
            .and_then(|profile| profile.id);
        let thumbnail_url = raw
            .installed_modpack
            .and_then(|modpack| modpack.thumbnail_url)
            .map(|u| u.trim().to_string())
            .filter(|u| u.starts_with("https://") || u.starts_with("http://"));
        let parsed = Self {
            name: raw.name,
            minecraft: raw.game_version,
            loader: loader_name.and_then(Loader::from_curseforge),
            version_id: profile_id.or(loader_name.map(String::from)),
            thumbnail_url,
            addon_count: raw.installed_addons.len(),
            installed_names,
            sources_by_sha1,
        };
        cache.lock().unwrap().insert(path, CachedInstance { stamp, instance: parsed.clone() });
        Ok(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_installed_modpack_thumbnail_url() {
        let dir = std::env::temp_dir().join(format!("packraven-instance-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let raw = serde_json::json!({
            "name": "Demo Pack",
            "gameVersion": "1.21.1",
            "installedModpack": {
                "thumbnailUrl": "https://media.forgecdn.net/avatars/thumbnails/1/2/256/256/pack.png"
            },
            "installedAddons": []
        });
        std::fs::write(dir.join(CURSEFORGE_INSTANCE), raw.to_string()).unwrap();
        let instance = Instance::read(&dir).unwrap();
        assert_eq!(instance.thumbnail_url.as_deref(), Some("https://media.forgecdn.net/avatars/thumbnails/1/2/256/256/pack.png"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
