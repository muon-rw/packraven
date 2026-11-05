use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::{AppHandle, State};

use crate::commands::AppState;
use crate::export::{ServerMods, ServerReadme};
use crate::join;
use crate::pack::{DEFAULT_SERVER_README, SERVER_ICON, SERVER_README};
use crate::server_icon;
use crate::settings::RunArgs;
use crate::setup::{self, InstanceInfo, SetupOptions};

type CmdResult<T> = Result<T, String>;

fn err(error: anyhow::Error) -> String {
    format!("{error:#}")
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackList {
    active: Option<PathBuf>,
    packs: Vec<InstanceInfo>,
    instances_root: PathBuf,
}

#[tauri::command]
pub fn list_packs(state: State<'_, AppState>) -> PackList {
    let settings = state.settings();
    let packs = settings.packs.iter().filter_map(|dir| setup::inspect(dir, &settings.packs).ok()).collect();
    let active = settings.packs.iter().find(|p| setup::same_dir(p, &settings.instance_dir)).cloned();
    PackList { active, packs, instances_root: settings.instances_root() }
}

#[tauri::command]
pub fn discover_instances(state: State<'_, AppState>, root: Option<PathBuf>) -> Vec<InstanceInfo> {
    let settings = state.settings();
    setup::discover(&root.unwrap_or_else(|| settings.instances_root()), &settings.packs)
}

#[tauri::command]
pub fn inspect_instance(state: State<'_, AppState>, dir: PathBuf) -> CmdResult<InstanceInfo> {
    setup::inspect(&dir, &state.settings().packs).map_err(err)
}

fn activate(app: &AppHandle, dir: &Path) -> anyhow::Result<()> {
    use tauri::Manager;
    let state = app.state::<AppState>();
    state.update_settings(|s| s.activate(dir))?;
    state.rewatch(app);
    Ok(())
}

#[tauri::command]
pub fn setup_instance(app: AppHandle, state: State<'_, AppState>, dir: PathBuf, options: SetupOptions) -> CmdResult<()> {
    let finished = app.clone();
    let identity = state.settings().identity();
    state.jobs.start(&app, "setup", move |log| async move {
        // Setup can download missing mods, whose future the compiler can't prove Send, so we drive it on a
        // blocking thread as pull does.
        let setup_dir = dir.clone();
        let message = tauri::async_runtime::spawn_blocking(move || tauri::async_runtime::block_on(setup::setup(setup_dir, options, &identity, log))).await??;
        activate(&finished, &dir)?;
        Ok(message)
    })
}

// We run joins as the setup job, since both happen on the Add pack page and share its log.
#[tauri::command]
pub fn join_pack(app: AppHandle, state: State<'_, AppState>, dir: PathBuf, remote: String) -> CmdResult<()> {
    let finished = app.clone();
    state.jobs.start(&app, "setup", move |log| async move {
        let join_dir = dir.clone();
        let message = tauri::async_runtime::spawn_blocking(move || tauri::async_runtime::block_on(join::join(&join_dir, &remote, &log))).await??;
        activate(&finished, &dir)?;
        Ok(message)
    })
}

#[tauri::command]
pub async fn switch_pack(app: AppHandle, state: State<'_, AppState>, dir: PathBuf, kill_tests: bool) -> CmdResult<()> {
    if kill_tests {
        state.test.kill_all().await;
    }
    if state.test.running() {
        return Err("Stop the test server and client before switching packs.".into());
    }
    state.update_settings(|s| s.activate(&dir)).map_err(err)?;
    state.rewatch(&app);
    Ok(())
}

#[tauri::command]
pub fn remove_pack(app: AppHandle, state: State<'_, AppState>, dir: PathBuf) -> CmdResult<()> {
    state.update_settings(|s| s.remove(&dir)).map_err(err)?;
    state.rewatch(&app);
    Ok(())
}

#[tauri::command]
pub fn update_hooks(state: State<'_, AppState>) -> CmdResult<()> {
    setup::install_hooks(&state.settings().instance_dir).map_err(err)
}

#[tauri::command]
pub fn get_pack_java(state: State<'_, AppState>) -> Option<PathBuf> {
    let settings = state.settings();
    settings.pack_java(&settings.instance_dir).cloned()
}

#[tauri::command]
pub fn set_pack_java(state: State<'_, AppState>, java: Option<PathBuf>) -> CmdResult<()> {
    if let Some(java) = java.as_ref().filter(|java| !java.is_file()) {
        return Err(format!("{} isn't a file", java.display()));
    }
    state
        .update_settings(|s| {
            let pack = s.instance_dir.clone();
            s.set_pack_java(&pack, java);
        })
        .map_err(err)
}

#[tauri::command]
pub fn get_run_args(state: State<'_, AppState>) -> RunArgs {
    let settings = state.settings();
    settings.pack_run_args(&settings.instance_dir)
}

#[tauri::command]
pub fn set_run_args(state: State<'_, AppState>, args: RunArgs) -> CmdResult<()> {
    state
        .update_settings(|s| {
            let pack = s.instance_dir.clone();
            s.set_pack_run_args(&pack, args);
        })
        .map_err(err)
}

#[tauri::command]
pub fn get_server_icon(state: State<'_, AppState>) -> CmdResult<Option<String>> {
    Ok(server_icon::data_url(&state.pack().map_err(err)?))
}

#[tauri::command]
pub fn set_server_icon(state: State<'_, AppState>, image: PathBuf) -> CmdResult<Option<String>> {
    let pack = state.pack().map_err(err)?;
    let bytes = std::fs::read(&image).map_err(|e| format!("Couldn't read {}: {e}", image.display()))?;
    server_icon::save(&pack, &bytes).map_err(err)?;
    Ok(server_icon::data_url(&pack))
}

#[tauri::command]
pub async fn use_project_logo_as_server_icon(state: State<'_, AppState>) -> CmdResult<Option<String>> {
    let pack = state.pack().map_err(err)?;
    let logo = server_icon::project_logo(pack.config.curseforge.project_id).await.map_err(err)?;
    server_icon::save(&pack, &logo).map_err(err)?;
    Ok(server_icon::data_url(&pack))
}

#[tauri::command]
pub fn remove_server_icon(state: State<'_, AppState>) -> CmdResult<()> {
    match std::fs::remove_file(state.pack().map_err(err)?.dir.join(SERVER_ICON)) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(format!("Couldn't remove server-icon.png: {error}")),
        _ => Ok(()),
    }
}

// We start your copy from the default template, so you edit the README you'd otherwise ship.
#[tauri::command]
pub fn customize_server_readme(state: State<'_, AppState>) -> CmdResult<()> {
    let path = state.pack().map_err(err)?.dir.join(SERVER_README);
    if path.exists() {
        return Ok(());
    }
    std::fs::write(&path, DEFAULT_SERVER_README).map_err(|e| format!("Couldn't create {SERVER_README}: {e}"))
}

// We preview the README a published server pack would get, since that's the one hosts download.
#[tauri::command]
pub fn server_readme(state: State<'_, AppState>) -> CmdResult<ServerReadme> {
    let pack = state.pack().map_err(err)?;
    let version = pack.version().unwrap_or_else(|| "{version}".into());
    crate::export::server_readme(&pack, &version, ServerMods::DownloadedOnStart).map_err(err)
}

#[derive(Serialize)]
pub struct ServerLaunch {
    windows: String,
    unix: String,
}

#[tauri::command]
pub fn server_launch(state: State<'_, AppState>) -> CmdResult<ServerLaunch> {
    let pack = state.pack().map_err(err)?;
    let launch = |windows| crate::export::loader_launch(&pack, windows).map_err(err);
    Ok(ServerLaunch { windows: launch(true)?, unix: launch(false)? })
}
