
use anyhow::Context;
use serde::Serialize;
use tauri::{AppHandle, State};

use crate::commands::AppState;
use crate::curseforge;
use crate::history::{self, Action};
use crate::jobs::Log;
use crate::mod_updates::{self, AddonKind, Category, InstalledMods, ModCheck, ModDetails, ModFile, ModRequest, RemoteText, SearchPage, SearchRequest};
use crate::modsync;
use crate::pack::{ModKey, Pack, Platform, ReleaseType, BCC_PROJECT_ID};
use crate::sides::MetadataCache;

type CmdResult<T> = Result<T, String>;

const CLIENT_HAS_MODS_OPEN: &str = "packraven can't change mods while the test client has them open. Close the client first.";

fn err(error: anyhow::Error) -> String {
    format!("{error:#}")
}

fn client_running(state: &AppState) -> bool {
    state.test.client.lock().unwrap().is_some()
}

#[tauri::command]
pub async fn installed_mods(state: State<'_, AppState>) -> CmdResult<InstalledMods> {
    let mut pack = state.pack().map_err(err)?;
    sync_locks(&state, &mut pack).map_err(err)?;
    let cache_path = state.cache_path();
    let shared = MetadataCache::load_shared(&cache_path);
    let keys: Vec<_> = pack.lock.files.iter().filter_map(crate::pack::LockEntry::key).collect();
    if shared.has_all_names(&keys) {
        return Ok(mod_updates::installed_from_cache(&pack, &shared));
    }
    let mut cache = (*shared).clone();
    let mods = mod_updates::installed(&pack, &mut cache).await;
    if cache.is_dirty() {
        cache.save(&cache_path).map_err(err)?;
    }
    Ok(mods)
}

#[tauri::command]
pub async fn check_mod_updates(state: State<'_, AppState>) -> CmdResult<Vec<ModCheck>> {
    mod_updates::check(&state.pack().map_err(err)?).await.map_err(err)
}

#[tauri::command]
pub async fn mod_versions(state: State<'_, AppState>, path: String, key: String) -> CmdResult<Vec<ModFile>> {
    mod_updates::versions(&state.pack().map_err(err)?, &path, &key).await.map_err(err)
}

#[tauri::command]
pub fn set_update_channel(state: State<'_, AppState>, channel: ReleaseType) -> CmdResult<()> {
    let mut pack = state.pack().map_err(err)?;
    pack.config.updates.channel = channel;
    pack.save_config().map_err(err)
}

#[tauri::command]
pub async fn search_mods(state: State<'_, AppState>, request: SearchRequest) -> CmdResult<SearchPage> {
    mod_updates::search(&state.pack().map_err(err)?, &request).await.map_err(err)
}

#[tauri::command]
pub async fn mod_categories(platform: Platform, kind: AddonKind) -> CmdResult<Vec<Category>> {
    mod_updates::categories(platform, kind).await.map_err(err)
}

#[derive(Serialize)]
pub struct Counterpart {
    key: String,
    url: String,
}

#[tauri::command]
pub async fn modrinth_counterparts(keys: Vec<String>) -> CmdResult<Vec<Counterpart>> {
    let ids: Vec<u64> = keys.iter().filter_map(|k| k.parse::<ModKey>().ok()?.curseforge_id()).collect();
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let found = crate::links::modrinth_counterparts(&ids).await.map_err(err)?;
    Ok(found.into_iter().map(|(id, url)| Counterpart { key: ModKey::Curseforge(id).to_string(), url }).collect())
}

#[tauri::command]
pub async fn curseforge_counterparts(keys: Vec<String>) -> CmdResult<Vec<Counterpart>> {
    let ids: Vec<String> = keys
        .iter()
        .filter_map(|k| match k.parse::<ModKey>().ok()? {
            ModKey::Modrinth(id) => Some(id),
            ModKey::Curseforge(_) => None,
        })
        .collect();
    if ids.is_empty() || curseforge::core_key().is_none() {
        return Ok(Vec::new());
    }
    let found = crate::links::curseforge_counterparts(&ids).await.map_err(err)?;
    Ok(found.into_iter().map(|(id, url)| Counterpart { key: ModKey::Modrinth(id).to_string(), url }).collect())
}

#[derive(Serialize)]
pub struct ModPreview {
    dependencies: Vec<String>,
    unavailable: Vec<String>,
    projects: Vec<String>,
}

#[tauri::command]
pub async fn preview_mods(state: State<'_, AppState>, requests: Vec<ModRequest>) -> CmdResult<ModPreview> {
    let plan = mod_updates::plan(&state.pack().map_err(err)?, &requests).await.map_err(err)?;
    let projects = plan.placements.iter().filter_map(|p| p.entry.key()).map(|key| key.to_string()).collect();
    Ok(ModPreview { dependencies: plan.dependencies, unavailable: plan.unavailable, projects })
}

fn sync_locks(state: &AppState, pack: &mut Pack) -> anyhow::Result<()> {
    crate::instance_locks::sync_with(pack, &state.curseforge_locks_path(), crate::instance_locks::CurseForgeApp::detect)
}

#[tauri::command(async)]
pub fn set_mod_locked(state: State<'_, AppState>, key: String, locked: bool) -> CmdResult<()> {
    let key: ModKey = key.parse()?;
    let mut pack = state.pack().map_err(err)?;
    let list = &mut pack.config.updates.locked;
    list.retain(|k| *k != key);
    if locked {
        list.push(key);
        list.sort_unstable();
    }
    pack.save_config().map_err(err)?;
    sync_locks(&state, &mut pack).map_err(err)
}

#[tauri::command]
pub async fn mod_description(key: String) -> CmdResult<RemoteText> {
    mod_updates::description(&key).await.map_err(err)
}

#[tauri::command]
pub async fn mod_details(key: String, keys: Vec<String>) -> CmdResult<ModDetails> {
    mod_updates::details(&key, &keys).await.map_err(err)
}

const DISABLED_SUFFIX: &str = ".disabled";

#[tauri::command]
pub async fn mod_changelog(key: String, file: String) -> CmdResult<RemoteText> {
    mod_updates::changelog(&key, &file).await.map_err(err)
}

#[tauri::command]
pub async fn set_mods_enabled(state: State<'_, AppState>, paths: Vec<String>, enabled: bool) -> CmdResult<Vec<String>> {
    // The client never opens .disabled files, so only disabling has to wait for it to close.
    if !enabled && client_running(&state) {
        return Err(CLIENT_HAS_MODS_OPEN.into());
    }
    let dir = state.settings().instance_dir;
    let _guard = modsync::MOD_LIST_GUARD.lock().await;
    let mut renamed_paths = Vec::new();
    for path in paths {
        let Some((folder, _)) = path.split_once('/').filter(|(folder, _)| ["mods", "resourcepacks", "shaderpacks", "datapacks"].contains(folder)) else {
            return Err(format!("{path} isn't in a folder packraven manages"));
        };
        let renamed = match (enabled, path.strip_suffix(DISABLED_SUFFIX)) {
            (true, Some(plain)) => plain.to_string(),
            (false, None) => format!("{path}{DISABLED_SUFFIX}"),
            _ => {
                renamed_paths.push(path);
                continue;
            }
        };
        // Packs set up before we supported disabling resource packs and shaders don't ignore their
        // disabled files yet, and git would offer to commit them.
        if !enabled {
            crate::setup::ensure_lines(&dir.join(".gitignore"), &[&format!("{folder}/*{DISABLED_SUFFIX}")]).map_err(err)?;
        }
        modsync::rename_locked_file(&dir, &path, &renamed).map_err(err)?;
        renamed_paths.push(renamed);
    }
    Ok(renamed_paths)
}

pub struct ModChange {
    requests: Vec<ModRequest>,
    updated: Vec<String>,
}

#[tauri::command]
pub fn change_mods(app: AppHandle, state: State<'_, AppState>, requests: Vec<ModRequest>) -> CmdResult<u64> {
    // The client holds the jars it loaded open, so we can add files while it runs but not replace them.
    if client_running(&state) && requests.iter().any(|r| r.replaces.is_some()) {
        return Err(CLIENT_HAS_MODS_OPEN.into());
    }
    let pack = state.pack().map_err(err)?;
    let cache = MetadataCache::load_shared(&state.cache_path());
    let updated: Vec<String> = requests
        .iter()
        .filter_map(|r| r.replaces.as_deref())
        .map(|path| {
            let key = pack.lock.files.iter().find(|e| e.path == path).and_then(|e| e.key());
            key.and_then(|key| cache.known_name(&key)).map_or_else(|| path.rsplit('/').next().unwrap_or(path).to_string(), |n| n.name.clone())
        })
        .collect();
    let dir = pack.dir;
    state.jobs.enqueue(&app, "mods", &state.mod_changes, ModChange { requests, updated }, move |change, log| {
        let dir = dir.clone();
        async move {
            // An earlier change in the same run may have added files, so we read the pack again.
            let pack = Pack::open(&dir)?;
            apply_mod_requests(&pack, &change.requests, &change.updated, &log).await
        }
    })
}

// Once a change starts, stopping it partway could leave two versions of a mod in the folder, so
// you can only cancel the ones still waiting.
#[tauri::command]
pub fn cancel_mod_change(state: State<'_, AppState>, id: u64) -> bool {
    state.mod_changes.lock().unwrap().cancel(id)
}

async fn apply_mod_requests(pack: &Pack, requests: &[ModRequest], updated: &[String], log: &Log) -> anyhow::Result<String> {
    let plan = mod_updates::plan(pack, requests).await?;
    let mut message = Vec::new();
    // Past a handful of names, a count reads better than a list.
    match updated.len() {
        0 => {}
        1..=4 => message.push(format!("Updated {}.", names(updated))),
        n => message.push(format!("Updated {}, {} and {} more.", updated[0], updated[1], n - 2)),
    }
    if !plan.installs.is_empty() {
        message.push(format!("Installed {}.", names(&plan.installs)));
    }
    if !plan.dependencies.is_empty() {
        message.push(format!("Added {} as required.", names(&plan.dependencies)));
    }
    if !plan.unavailable.is_empty() {
        message.push(format!("There's no file of {} for this pack's version, so you'll need to find one.", names(&plan.unavailable)));
    }
    {
        let _guard = modsync::MOD_LIST_GUARD.lock().await;
        modsync::place(&pack.dir, plan.placements, log).await?;
    }
    Ok(message.join(" "))
}

#[tauri::command]
pub fn set_up_version_checker(app: AppHandle, state: State<'_, AppState>, version: String) -> CmdResult<()> {
    let pack = state.pack().map_err(err)?;
    if pack.has_bcc() {
        return pack.write_bcc_config_if_missing(version.trim()).map_err(err);
    }
    if client_running(&state) {
        return Err(CLIENT_HAS_MODS_OPEN.into());
    }
    state.jobs.start(&app, "mods", move |log| async move {
        let loader = curseforge::loader_type(&pack.lock.mod_loader()?);
        let files = curseforge::project_files(BCC_PROJECT_ID, pack.lock.minecraft()?, Some(loader)).await?;
        let file = files
            .iter()
            .find(|f| f.release_type == 1)
            .or(files.first())
            .context("CurseForge has no Better Compatibility Checker file for this pack's Minecraft version and loader")?;
        let message = apply_mod_requests(&pack, &[ModRequest { file: Some(format!("cf:{}", file.id)), project: None, kind: None, replaces: None }], &[], &log).await?;
        pack.write_bcc_config_if_missing(version.trim())?;
        Ok(message)
    })
}

fn names(list: &[String]) -> String {
    match list {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[tauri::command]
pub async fn remove_mods(state: State<'_, AppState>, paths: Vec<String>) -> CmdResult<()> {
    if client_running(&state) {
        return Err(CLIENT_HAS_MODS_OPEN.into());
    }
    let root = state.settings().instance_dir;
    let _guard = modsync::MOD_LIST_GUARD.lock().await;
    let label = format!("Delete {}", history::describe(&paths));
    let (task_root, task_paths) = (root.clone(), paths.clone());
    let trashed = tauri::async_runtime::spawn_blocking(move || crate::files::trash(&task_root, &task_paths)).await.map_err(|e| e.to_string())?.map_err(err)?;
    state.history.lock().unwrap().record(&root, label, trashed.map(Action::Restore));
    modsync::drop_from_lock(&root, &paths).map_err(err)?;
    forget_removed_projects(&root).map_err(err)
}

// Overrides and locks name projects, so we drop the ones whose last file you just removed.
fn forget_removed_projects(root: &std::path::Path) -> anyhow::Result<()> {
    let mut pack = Pack::open(root)?;
    let lock = &pack.lock;
    let lists = [&mut pack.config.server.client_only, &mut pack.config.server.keep, &mut pack.config.updates.locked];
    let mut changed = false;
    for list in lists {
        let before = list.len();
        list.retain(|key| lock.files.iter().any(|e| e.has_key(key)));
        changed |= list.len() != before;
    }
    if changed {
        pack.save_config()?;
    }
    Ok(())
}
