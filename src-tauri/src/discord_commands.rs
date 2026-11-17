use serde_json::json;
use tauri::{AppHandle, Manager, State};

use crate::changelog;
use crate::commands::AppState;
use crate::discord;
use crate::sides::MetadataCache;
use crate::pack::Pack;
use crate::publish::{self, Release};

type CmdResult<T> = Result<T, String>;

fn err(error: anyhow::Error) -> String {
    format!("{error:#}")
}

#[tauri::command]
pub fn get_webhook(state: State<'_, AppState>) -> Option<String> {
    let settings = state.settings();
    settings.webhook(&settings.instance_dir)
}

#[tauri::command]
pub fn set_webhook(state: State<'_, AppState>, url: String) -> CmdResult<()> {
    if !url.trim().is_empty() {
        discord::validate_webhook(url.trim()).map_err(err)?;
    }
    state
        .update_settings(|s| {
            let pack = s.instance_dir.clone();
            s.set_webhook(&pack, &url);
        })
        .map_err(err)
}

#[tauri::command]
pub async fn discord_test(state: State<'_, AppState>) -> CmdResult<()> {
    let settings = state.settings();
    let webhook = settings.webhook(&settings.instance_dir).ok_or("Add a webhook URL first.")?;
    let pack = Pack::open(&settings.instance_dir).map_err(err)?;
    let message = json!({
        "username": pack.name(),
        "content": format!("packraven is connected. We'll post release announcements for {} here.", pack.name()),
        "allowed_mentions": { "parse": [] },
    });
    discord::send(&webhook, &message, &[]).await.map(|_| ()).map_err(err)
}

#[tauri::command]
pub async fn announce_release(state: State<'_, AppState>, version: String) -> CmdResult<()> {
    let settings = state.settings();
    let pack_data = state.pack_data();
    let webhook = settings.webhook(&settings.instance_dir).ok_or("Add a Discord webhook in Pack settings first.")?;
    discord::announce(&settings.instance_dir, &pack_data, &pack_data.join("releases.json"), &version, &webhook).await.map_err(err)
}

fn published_release(state: &AppState, version: &str) -> CmdResult<Release> {
    publish::load_releases(&state.pack_data().join("releases.json"))
        .into_iter()
        .find(|r| r.version == version)
        .ok_or_else(|| format!("You haven't published release {version} from this machine."))
}

/// Returns the announcement for the release you're about to publish, which has no file to link to yet.
async fn draft_announcement(state: &AppState, pack: &Pack, version: String) -> CmdResult<discord::Announcement> {
    let mut cache = MetadataCache::load(&state.cache_path());
    let diff = changelog::mod_diff(pack, &pack.lock, &mut cache, &state.pack_data().join(changelog::BASELINE_FILE)).await.map_err(err)?;
    let project = discord::pack_project(pack).await;
    Ok(discord::Announcement {
        name: pack.name(),
        version,
        changelog: changelog::render(&changelog::read_notes(pack), &diff),
        file_url: project.url.clone(),
        project,
        game: discord::game(pack),
        timestamp: chrono::Utc::now().to_rfc3339(),
    })
}

// Discohook's share links can't carry files, so the message we open there leaves the images out.
#[tauri::command]
pub async fn discohook_link(state: State<'_, AppState>, version: String) -> CmdResult<String> {
    let pack = Pack::open(&state.settings().instance_dir).map_err(err)?;
    let release = published_release(&state, &version)?;
    let composed = discord::compose(&discord::announcement(&pack, &release).await, &pack.config.discord, &[]);
    discord::discohook_link(&composed.payload).await.map_err(err)
}

#[tauri::command]
pub async fn discohook_preview(state: State<'_, AppState>, version: String) -> CmdResult<String> {
    let pack = Pack::open(&state.settings().instance_dir).map_err(err)?;
    let announcement = draft_announcement(&state, &pack, version).await?;
    discord::discohook_link(&discord::compose(&announcement, &pack.config.discord, &[]).payload).await.map_err(err)
}

/// Returns exactly what announcing the published release would send now.
#[tauri::command]
pub async fn compose_announcement(state: State<'_, AppState>, version: String) -> CmdResult<discord::Composed> {
    let pack = Pack::open(&state.settings().instance_dir).map_err(err)?;
    let release = published_release(&state, &version)?;
    Ok(discord::compose(&discord::announcement(&pack, &release).await, &pack.config.discord, &release.announcement_images))
}

/// Returns what the release you're about to publish would announce, with the images you've chosen so far.
#[tauri::command]
pub async fn compose_draft_announcement(state: State<'_, AppState>, version: String, images: Vec<String>) -> CmdResult<discord::Composed> {
    let pack = Pack::open(&state.settings().instance_dir).map_err(err)?;
    let announcement = draft_announcement(&state, &pack, version).await?;
    Ok(discord::compose(&announcement, &pack.config.discord, &images))
}

fn allow_images(app: &AppHandle, images: &[String]) -> CmdResult<()> {
    for image in images.iter().filter(|image| discord::is_image(image)) {
        app.asset_protocol_scope().allow_file(image).map_err(|e| e.to_string())?;
    }
    Ok(())
}

// The webview can only show files the asset protocol's scope allows, so we allow the images before
// we check them. That way the thumbnail of a rejected image still loads.
#[tauri::command]
pub fn check_announcement_images(app: AppHandle, images: Vec<String>) -> CmdResult<()> {
    allow_images(&app, &images)?;
    discord::check_images(&images).map_err(err)
}

#[tauri::command]
pub fn get_pending_announcement_images(app: AppHandle, state: State<'_, AppState>) -> CmdResult<Vec<String>> {
    let images = discord::load_pending_images(&state.pack_data());
    allow_images(&app, &images)?;
    Ok(images)
}

#[tauri::command]
pub fn set_pending_announcement_images(app: AppHandle, state: State<'_, AppState>, images: Vec<String>) -> CmdResult<Vec<String>> {
    let pack_data = state.pack_data();
    let existing = discord::load_pending_images(&pack_data);
    let only_removes = images.iter().all(|image| existing.contains(image));
    if !only_removes {
        discord::check_images(&images).map_err(err)?;
    }
    let cached = discord::save_pending_images(&pack_data, &images).map_err(err)?;
    allow_images(&app, &cached)?;
    Ok(cached)
}

#[tauri::command]
pub fn set_announcement_images(app: AppHandle, state: State<'_, AppState>, version: String, images: Vec<String>) -> CmdResult<Vec<String>> {
    let pack_data = state.pack_data();
    let releases_path = pack_data.join("releases.json");
    let mut releases = publish::load_releases(&releases_path);
    let release = releases.iter_mut().find(|r| r.version == version).ok_or_else(|| format!("You haven't published release {version} from this machine."))?;
    // Removing an image has to work even while another one is missing or too large.
    let only_removes = images.iter().all(|image| release.announcement_images.contains(image));
    if !only_removes {
        discord::check_images(&images).map_err(err)?;
    }
    let cached = discord::cache_images(&discord::release_images_dir(&pack_data, &version), &images).map_err(err)?;
    allow_images(&app, &cached)?;
    release.announcement_images = cached.clone();
    publish::save_releases(&releases_path, &releases).map_err(err)?;
    Ok(cached)
}
