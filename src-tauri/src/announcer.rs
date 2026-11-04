use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Notify;

use crate::commands::{pack_data_dir, AppState};
use crate::pack::Pack;
use crate::discord::Listing;
use crate::{discord, publish, update_checker};

const CHECK_EVERY: Duration = Duration::from_secs(300);
const CHECK_PENDING_EVERY: Duration = Duration::from_secs(30);

static CHECK_REQUESTED: Notify = Notify::const_new();

pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            let delay = if check(&app).await { CHECK_PENDING_EVERY } else { CHECK_EVERY };
            tokio::select! {
                () = tokio::time::sleep(delay) => {}
                () = CHECK_REQUESTED.notified() => {}
            }
        }
    });
}

/// Checks releases again now rather than at the next interval, as when a publish just uploaded one.
pub fn check_soon() {
    CHECK_REQUESTED.notify_one();
}

// Both the Discord announcement and the update checker entry tell players to update, so both wait until
// every platform the release went to lets them download it. We return whether any release is still in
// CurseForge's automated pipeline, so the loop checks again sooner.
async fn check(app: &AppHandle) -> bool {
    let mut any_pending = false;
    let state = app.state::<AppState>();
    let settings = state.settings();
    for pack in &settings.packs {
        let webhook = settings.webhook(pack);
        let pack_data = pack_data_dir(&state.data_dir(), pack);
        let releases_path = pack_data.join("releases.json");
        let opened = Pack::open(pack).ok();
        let announces = |release: &publish::Release| webhook.is_some() && release.announce && release.announced_at.is_none();
        for mut release in publish::load_releases(&releases_path).into_iter().filter(|r| announces(r) || update_checker::awaits(r)) {
            // A release still waiting for its Modrinth upload or its server pack isn't out everywhere yet.
            let awaiting_modrinth = opened.as_ref().is_some_and(|opened| publish::awaiting_modrinth(opened, &pack_data, &release).is_some());
            if awaiting_modrinth {
                continue;
            }
            let listing = discord::listing(&release).await;
            any_pending |= listing == Listing::Pending;
            if listing != Listing::Public || !publish::link_server_pack(&releases_path, &mut release).await {
                continue;
            }
            if let Some(webhook) = webhook.as_deref().filter(|_| announces(&release)) {
                match discord::announce(pack, &releases_path, &release.version, webhook).await {
                    Ok(()) => {
                        let _ = app.emit("release-announced", &release.version);
                    }
                    Err(error) => eprintln!("packraven: announcing {} failed: {error:#}", release.version),
                }
            }
            // After a failure we wait for you to try again from Releases, so a bad token doesn't hit GitHub every few minutes.
            let failed_before = release.update_checker.as_ref().is_some_and(|e| e.error.is_some());
            if let Some(opened) = opened.as_ref().filter(|_| update_checker::awaits(&release) && !failed_before) {
                if let Err(error) = update_checker::add_release(opened, &releases_path, &release.version).await {
                    eprintln!("packraven: adding {} to the update checker failed: {error:#}", release.version);
                }
                let _ = app.emit("release-update-checker", &release.version);
            }
        }
    }
    any_pending
}
