use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::value::RawValue;

use crate::pack::{ModKey, Pack};
use crate::setup::same_dir;

const INSTANCE_JSON: &str = "minecraftinstance.json";

#[derive(Clone)]
pub enum CurseForgeApp {
    Running,
    Closed { database: PathBuf },
}

impl CurseForgeApp {
    pub fn detect() -> Self {
        if agent_running() {
            return Self::Running;
        }
        let database = dirs::config_dir().unwrap_or_default().join("CurseForge").join("agent").join("GameInstances").join("MinecraftGameInstance.json");
        Self::Closed { database }
    }
}

// The CurseForge app runs its agent for as long as the app is open (in the tray too), and the agent
// owns the instance database. When we can't tell, we assume it's running, because writing under a
// running agent is the case that loses locks.
#[cfg(windows)]
fn agent_running() -> bool {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
    let mut system = System::new();
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    system.processes().values().any(|process| process.name().eq_ignore_ascii_case("Curse.Agent.Host.exe"))
}

#[cfg(not(windows))]
fn agent_running() -> bool {
    std::process::Command::new("pgrep").args(["-f", "Curse.Agent.Host"]).output().map_or(true, |output| output.status.code() != Some(1))
}

#[derive(Deserialize)]
struct InstanceLocks {
    #[serde(rename = "installedAddons")]
    installed_addons: Option<Vec<InstalledAddonLock>>,
}

#[derive(Deserialize)]
struct InstalledAddonLock {
    #[serde(rename = "addonID")]
    addon_id: Option<u64>,
    #[serde(rename = "isLocked", default)]
    is_locked: bool,
}

fn locked_addons(instance: &str) -> Option<BTreeSet<u64>> {
    let instance: InstanceLocks = serde_json::from_str(instance.trim_start_matches('\u{feff}')).ok()?;
    Some(instance.installed_addons?.into_iter().filter(|addon| addon.is_locked).filter_map(|addon| addon.addon_id).collect())
}

type FileStamp = (u64, Option<SystemTime>);

struct CachedLocks {
    stamp: FileStamp,
    locks: Option<BTreeSet<u64>>,
}

static INSTANCE_LOCKS_CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedLocks>>> = OnceLock::new();

fn file_stamp(path: &Path) -> Option<FileStamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()))
}

pub fn curseforge_locks(dir: &Path) -> Option<BTreeSet<u64>> {
    let path = dir.join(INSTANCE_JSON);
    let stamp = file_stamp(&path)?;
    let cache = INSTANCE_LOCKS_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(entry) = cache.lock().unwrap().get(&path)
        && entry.stamp == stamp
    {
        return entry.locks.clone();
    }
    let locks = locked_addons(&std::fs::read_to_string(&path).ok()?);
    cache.lock().unwrap().insert(path, CachedLocks { stamp, locks: locks.clone() });
    locks
}

// The CurseForge app owns this file, and parsing and rewriting it would reorder its keys, so we
// change only the one isLocked value in place. Each addon object lists addonID before isLocked,
// and none of the objects between them have an isLocked of their own.
fn set_curseforge_lock(text: &str, project_id: u64, locked: bool) -> Option<String> {
    let addons = text.find("\"installedAddons\"")?;
    let id = format!("\"addonID\":{project_id}");
    let mut from = addons;
    let start = loop {
        let found = from + text[from..].find(&id)?;
        let after = text[found + id.len()..].chars().next()?;
        if after == ',' || after == '}' {
            break found;
        }
        from = found + id.len();
    };
    let key = start + text[start..].find("\"isLocked\":")? + "\"isLocked\":".len();
    let current = if text[key..].starts_with("true") { "true" } else if text[key..].starts_with("false") { "false" } else { return None };
    let wanted = if locked { "true" } else { "false" };
    Some(format!("{}{wanted}{}", &text[..key], &text[key + current.len()..]))
}

fn with_locks(instance: &str, wanted: &BTreeSet<u64>) -> Option<String> {
    let current = locked_addons(instance)?;
    let mut text = instance.to_string();
    for id in current.symmetric_difference(wanted) {
        if let Some(updated) = set_curseforge_lock(&text, *id, wanted.contains(id)) {
            text = updated;
        }
    }
    (text != instance).then_some(text)
}

// CurseForge loads every instance it already knows from this database at startup and regenerates
// minecraftinstance.json from it, so the lock has to change here as well to survive a restart.
fn database_with_locks(database: &str, dir: &Path, wanted: &BTreeSet<u64>) -> Option<String> {
    #[derive(Deserialize)]
    struct Instance {
        #[serde(rename = "installPath")]
        install_path: String,
    }
    let instances: Vec<&RawValue> = serde_json::from_str(database.trim_start_matches('\u{feff}')).ok()?;
    let instance = instances.into_iter().map(RawValue::get).find(|raw| serde_json::from_str::<Instance>(raw).is_ok_and(|i| same_dir(Path::new(&i.install_path), dir)))?;
    let start = instance.as_ptr() as usize - database.as_ptr() as usize;
    let patched = with_locks(instance, wanted)?;
    Some(format!("{}{patched}{}", &database[..start], &database[start + instance.len()..]))
}

#[cfg(test)]
fn sync(pack: &mut Pack, last_seen_path: &Path, app: &CurseForgeApp) -> Result<()> {
    sync_with(pack, last_seen_path, || app.clone())
}

/// Merges lock changes made in the CurseForge app since the last sync into packraven.toml, then
/// writes the pack's locks back to the CurseForge app if it's closed. `last_seen` holds the
/// CurseForge locks from the previous sync on this machine, so an unlock in either app sticks.
pub fn sync_with(pack: &mut Pack, last_seen_path: &Path, detect_app: impl FnOnce() -> CurseForgeApp) -> Result<()> {
    let Some(curseforge) = curseforge_locks(&pack.dir) else { return Ok(()) };
    let last_seen_text = std::fs::read_to_string(last_seen_path).ok();
    let last_seen_existed = last_seen_text.is_some();
    let last_seen: BTreeSet<u64> = last_seen_text.and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let mut ours: BTreeSet<u64> = pack.config.updates.locked.iter().filter_map(ModKey::curseforge_id).collect();
    let before = ours.clone();
    ours.extend(curseforge.difference(&last_seen));
    for id in last_seen.difference(&curseforge) {
        ours.remove(id);
    }
    if ours != before {
        let locked = &mut pack.config.updates.locked;
        locked.retain(|key| key.curseforge_id().is_none());
        locked.extend(ours.iter().copied().map(ModKey::Curseforge));
        locked.sort();
        pack.save_config()?;
    }
    // Only check whether CurseForge is running and write back when our locks actually differ from
    // what minecraftinstance.json already records.
    let mut wrote_instance = false;
    if ours != curseforge
        && let CurseForgeApp::Closed { database } = detect_app()
    {
        wrote_instance = write_curseforge_locks(&pack.dir, &database, &ours)?;
    }
    let seen = if wrote_instance { curseforge_locks(&pack.dir).unwrap_or_default() } else { curseforge };
    if !last_seen_existed || seen != last_seen {
        if let Some(parent) = last_seen_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(last_seen_path, serde_json::to_string(&seen)?)?;
    }
    Ok(())
}

fn write_curseforge_locks(dir: &Path, database: &Path, wanted: &BTreeSet<u64>) -> Result<bool> {
    if let Ok(text) = std::fs::read_to_string(database)
        && let Some(updated) = database_with_locks(&text, dir, wanted)
    {
        let temp = database.with_extension("packraven-tmp");
        std::fs::write(&temp, updated).context("writing CurseForge's instance database")?;
        std::fs::rename(&temp, database).context("replacing CurseForge's instance database")?;
    }
    let path = dir.join(INSTANCE_JSON);
    if let Some(updated) = with_locks(&std::fs::read_to_string(&path)?, wanted) {
        std::fs::write(&path, updated).context("writing minecraftinstance.json")?;
        if let Some(cache) = INSTANCE_LOCKS_CACHE.get() {
            cache.lock().unwrap().remove(&path);
        }
        return Ok(true);
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const INSTANCE: &str = r#"{"installedModpack":{"addonID":7,"isLocked":false},"installedAddons":[{"addonID":12,"installedFile":{"id":1,"dependencies":[{"addonId":7}]},"isEnabled":true,"isLocked":false},{"addonID":7,"isLocked":true}]}"#;

    #[test]
    fn flips_one_lock_and_leaves_the_rest() {
        let locked = set_curseforge_lock(INSTANCE, 12, true).unwrap();
        assert_eq!(locked, INSTANCE.replacen(r#""isEnabled":true,"isLocked":false"#, r#""isEnabled":true,"isLocked":true"#, 1));
        let unlocked = set_curseforge_lock(INSTANCE, 7, false).unwrap();
        assert!(unlocked.starts_with(r#"{"installedModpack":{"addonID":7,"isLocked":false}"#), "the modpack entry isn't an installed addon");
        assert!(unlocked.ends_with(r#"{"addonID":7,"isLocked":false}]}"#));
        assert!(set_curseforge_lock(INSTANCE, 1, true).is_none(), "a missing addon changes nothing");
    }

    #[test]
    fn patches_only_the_matching_database_instance() {
        let other = r#"{"installPath":"C:\\Instances\\Other\\","installedAddons":[{"addonID":12,"isLocked":false}]}"#;
        let ours = format!(r#"{{"installPath":"C:\\Instances\\Pack\\",{}"#, &INSTANCE[1..]);
        let database = format!("\u{feff}[{other},{ours}]");
        let patched = database_with_locks(&database, Path::new("c:/instances/pack"), &BTreeSet::from([12])).unwrap();
        let instances: Vec<Value> = serde_json::from_str(patched.trim_start_matches('\u{feff}')).unwrap();
        assert_eq!(locked_addons(&instances[0].to_string()).unwrap(), BTreeSet::new());
        assert_eq!(locked_addons(&instances[1].to_string()).unwrap(), BTreeSet::from([12]));
        assert!(patched.starts_with(&format!("\u{feff}[{other},")));
        assert!(database_with_locks(&database, Path::new("c:/instances/missing"), &BTreeSet::from([12])).is_none());
    }

    #[test]
    fn sync_keeps_unlocks_from_either_app() {
        let dir = std::env::temp_dir().join(format!("packraven-locks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(crate::pack::LOCKFILE), r#"{"files": []}"#).unwrap();
        std::fs::write(dir.join(INSTANCE_JSON), format!("\u{feff}{INSTANCE}")).unwrap();
        let seen = dir.join("seen.json");
        let closed = CurseForgeApp::Closed { database: dir.join("MinecraftGameInstance.json") };
        let mut pack = Pack::open(&dir).unwrap();

        sync(&mut pack, &seen, &closed).unwrap();
        let keys = |ids: &[u64]| ids.iter().copied().map(ModKey::Curseforge).collect::<Vec<_>>();
        assert_eq!(pack.config.updates.locked, keys(&[7]), "the first sync adopts CurseForge's locks");

        pack.config.updates.locked = keys(&[7, 12]);
        sync(&mut pack, &seen, &CurseForgeApp::Running).unwrap();
        assert_eq!(curseforge_locks(&dir).unwrap(), BTreeSet::from([7]), "we leave a running CurseForge's files alone");
        sync(&mut pack, &seen, &CurseForgeApp::Running).unwrap();
        assert_eq!(pack.config.updates.locked, keys(&[7, 12]), "a pending lock survives syncs while CurseForge runs");

        pack.config.updates.locked = vec![ModKey::Curseforge(12), ModKey::Modrinth("AANobbMI".into())];
        sync(&mut pack, &seen, &closed).unwrap();
        assert_eq!(pack.config.updates.locked, [ModKey::Curseforge(12), ModKey::Modrinth("AANobbMI".into())], "Modrinth locks stay as they are");
        pack.config.updates.locked.pop();
        assert_eq!(curseforge_locks(&dir).unwrap(), BTreeSet::from([12]), "we write packraven's locks back");

        let text = std::fs::read_to_string(dir.join(INSTANCE_JSON)).unwrap();
        std::fs::write(dir.join(INSTANCE_JSON), set_curseforge_lock(&text, 12, false).unwrap()).unwrap();
        sync(&mut pack, &seen, &closed).unwrap();
        assert!(pack.config.updates.locked.is_empty(), "an unlock in the CurseForge app sticks");
        assert!(std::fs::read_to_string(dir.join(INSTANCE_JSON)).unwrap().starts_with('\u{feff}'));

        let mut detected = false;
        sync_with(&mut pack, &seen, || {
            detected = true;
            closed.clone()
        })
        .unwrap();
        assert!(!detected, "a no-op sync skips process detection and database I/O");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
