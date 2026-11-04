use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::Serialize;
use tauri::image::Image;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::commands::AppState;
use crate::credentials::{self, Credential, CredentialStatus};
use crate::defender;
use crate::setup::same_dir;

type CmdResult<T> = Result<T, String>;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DefenderView {
    active: bool,
    exclusions_readable: bool,
    missing: Vec<PathBuf>,
    prompt: bool,
}

fn watched_paths(state: &AppState) -> Vec<PathBuf> {
    let settings = state.settings();
    [settings.instance_dir, settings.install_dir, state.data_dir()].into_iter().filter(|p| !p.as_os_str().is_empty()).collect()
}

#[tauri::command]
pub async fn defender_status(state: State<'_, AppState>) -> CmdResult<DefenderView> {
    let paths = watched_paths(&state);
    let settings = state.settings();
    let status = tauri::async_runtime::spawn_blocking(move || defender::status(&paths)).await.map_err(|e| e.to_string())?;
    let missing: Vec<PathBuf> = if status.exclusions_readable {
        status.missing
    } else {
        status.missing.into_iter().filter(|p| !settings.defender_excluded.iter().any(|e| same_dir(e, p))).collect()
    };
    let prompt = status.active && !missing.is_empty() && !settings.defender_dismissed;
    Ok(DefenderView { active: status.active, exclusions_readable: status.exclusions_readable, missing, prompt })
}

#[tauri::command]
pub async fn defender_exclude(state: State<'_, AppState>, paths: Vec<PathBuf>) -> CmdResult<()> {
    let to_add = paths.clone();
    tauri::async_runtime::spawn_blocking(move || defender::exclude(&to_add)).await.map_err(|e| e.to_string())?.map_err(|e| format!("{e:#}"))?;
    state
        .update_settings(|s| {
            for path in paths {
                if !s.defender_excluded.iter().any(|e| same_dir(e, &path)) {
                    s.defender_excluded.push(path);
                }
            }
        })
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub fn defender_dismiss(state: State<'_, AppState>, dismissed: bool) -> CmdResult<()> {
    state.update_settings(|s| s.defender_dismissed = dismissed).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub fn accept_minecraft_eula(state: State<'_, AppState>) -> CmdResult<()> {
    state.update_settings(|s| s.minecraft_eula_accepted = true).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub fn git_version() -> Option<String> {
    crate::git::version()
}

#[tauri::command]
pub async fn install_git() -> CmdResult<String> {
    crate::git_install::install().await.map_err(|e| format!("{e:#}"))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JavaScan {
    installs: Vec<crate::java::JavaInstall>,
    automatic: BTreeMap<u32, PathBuf>,
    downloads: BTreeMap<u32, u32>,
    needed: BTreeSet<u32>,
}

#[tauri::command]
pub async fn scan_java(state: State<'_, AppState>) -> CmdResult<JavaScan> {
    let settings = state.settings();
    tauri::async_runtime::spawn_blocking(move || {
        let installs = crate::java::detect(&settings.install_dir);
        let automatic = installs.iter().filter_map(|java| Some((java.major, crate::java::automatic(&installs, java.major)?.path.clone()))).collect();
        let needed = settings
            .packs
            .iter()
            .map(|pack| crate::launch::required_java(&crate::settings::Settings { instance_dir: pack.clone(), ..settings.clone() }).1)
            .collect();
        JavaScan { installs, automatic, downloads: crate::java::downloads(), needed }
    })
    .await
    .map_err(|e| e.to_string())
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct JavaDownloadProgress {
    graalvm: u32,
    received: u64,
    total: Option<u64>,
}

#[tauri::command]
pub async fn download_java(app: AppHandle, state: State<'_, AppState>, major: u32) -> CmdResult<crate::java::JavaInstall> {
    let graalvm = crate::java::graalvm_for(major).unwrap_or(major);
    let progress = |received, total| {
        let _ = app.emit("java-download", JavaDownloadProgress { graalvm, received, total });
    };
    let java = crate::java::download_graalvm(major, progress).await.map_err(|e| format!("{e:#}"))?;
    // Automatic picks up a download of the same version. Java 17 gets GraalVM 21, which Automatic
    // won't choose for Java 17 packs, so we choose it for that version under App settings.
    if java.major != major {
        state
            .update_settings(|s| {
                s.java_by_major.insert(major, java.path.clone());
            })
            .map_err(|e| format!("{e:#}"))?;
    }
    Ok(java)
}

#[tauri::command]
pub fn cancel_java_download(graalvm: u32) {
    crate::java::cancel_download(graalvm);
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackJavaDefault {
    major: u32,
    path: Option<PathBuf>,
    error: Option<String>,
}

#[tauri::command]
pub async fn pack_java_default(state: State<'_, AppState>) -> CmdResult<PackJavaDefault> {
    let settings = state.settings();
    tauri::async_runtime::spawn_blocking(move || {
        let (_, major) = crate::launch::required_java(&settings);
        match crate::launch::app_java(&settings) {
            Ok(path) => PackJavaDefault { major, path: Some(path), error: None },
            Err(error) => PackJavaDefault { major, path: None, error: Some(format!("{error:#}")) },
        }
    })
    .await
    .map_err(|e| e.to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialsView {
    built_in_api_key: bool,
    upload: CredentialStatus,
    modrinth: CredentialStatus,
    github: CredentialStatus,
    keychain: &'static str,
}

#[tauri::command]
pub fn credentials_status() -> CredentialsView {
    CredentialsView {
        built_in_api_key: crate::curseforge::core_key().is_some(),
        upload: credentials::status(Credential::Upload),
        modrinth: credentials::status(Credential::Modrinth),
        github: credentials::status(Credential::Github),
        keychain: credentials::keychain_name(),
    }
}

#[tauri::command]
pub fn credential_store(credential: Credential, secret: String) -> CmdResult<()> {
    credentials::store(credential, &secret).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub fn credential_use_env(credential: Credential, name: String) -> CmdResult<()> {
    credentials::use_env(credential, &name).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub fn credential_clear(credential: Credential) -> CmdResult<()> {
    credentials::clear(credential).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub async fn github_identity() -> CmdResult<crate::remote::GithubIdentity> {
    crate::remote::github_identity().await.map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub fn env_var_names() -> Vec<String> {
    credentials::env_names()
}

const LIGHT_WINDOW_ICON: &[u8] = include_bytes!("../icons/light/icon.png");
const LIGHT_TRAY_ICON: &[u8] = include_bytes!("../icons/light/32x32.png");

/// Swaps the window and tray icons to match the app theme. The bundled icon is the dark one.
#[tauri::command]
pub fn set_icon_theme(app: AppHandle, dark: bool) -> CmdResult<()> {
    let bundled = app.default_window_icon().cloned().ok_or("packraven has no bundled icon")?;
    let (window_icon, tray_icon) = if dark {
        (bundled.clone(), bundled)
    } else {
        let light = |bytes| Image::from_bytes(bytes).map_err(|e| e.to_string());
        (light(LIGHT_WINDOW_ICON)?, light(LIGHT_TRAY_ICON)?)
    };
    if let Some(window) = app.get_webview_window("main") {
        window.set_icon(window_icon).map_err(|e| e.to_string())?;
    }
    if let Some(tray) = app.tray_by_id(crate::TRAY_ID) {
        tray.set_icon(Some(tray_icon)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub fn update_status(app: AppHandle) -> crate::self_update::UpdateView {
    crate::self_update::view(&app)
}

#[tauri::command]
pub async fn check_for_update(app: AppHandle) -> CmdResult<crate::self_update::UpdateView> {
    crate::self_update::check(&app).await.map_err(|e| format!("{e:#}"))?;
    Ok(crate::self_update::view(&app))
}

#[tauri::command]
pub async fn install_update(app: AppHandle) -> CmdResult<()> {
    crate::self_update::install(&app).await.map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub async fn update_changelog(app: AppHandle, since: Option<String>, through: Option<String>) -> CmdResult<Vec<crate::self_update::ReleaseNotes>> {
    crate::self_update::changelog(&app, since.as_deref(), through.as_deref()).await.map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub fn dismiss_update_changelog(app: AppHandle) -> CmdResult<()> {
    crate::self_update::dismiss_updated_from(&app).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub fn set_auto_update(app: AppHandle, state: State<'_, AppState>, enabled: bool) -> CmdResult<()> {
    state.update_settings(|s| s.auto_update = enabled).map_err(|e| format!("{e:#}"))?;
    crate::self_update::auto_update_changed(&app, enabled);
    Ok(())
}

#[tauri::command]
pub fn set_discord_shows_pack(state: State<'_, AppState>, enabled: bool) -> CmdResult<()> {
    state.update_settings(|s| s.discord_shows_pack = enabled).map_err(|e| format!("{e:#}"))?;
    state.update_presence();
    Ok(())
}
