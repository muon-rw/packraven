use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};

use crate::commands::{pack_data_dir, AppState};
use crate::pack::Pack;
use crate::{discord, publish};

const CHECK_EVERY: Duration = Duration::from_secs(300);

pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            check(&app).await;
            tokio::time::sleep(CHECK_EVERY).await;
        }
    });
}

async fn check(app: &AppHandle) {
    let state = app.state::<AppState>();
    let settings = state.settings();
    for pack in &settings.packs {
        let Some(webhook) = settings.webhook(pack) else { continue };
        let pack_data = pack_data_dir(&state.data_dir(), pack);
        let releases_path = pack_data.join("releases.json");
        let opened = Pack::open(pack).ok();
        for mut release in publish::load_releases(&releases_path).into_iter().filter(|r| r.announce && r.announced_at.is_none()) {
            // A release still waiting for its Modrinth upload or its server pack isn't out everywhere yet.
            let awaiting_modrinth = opened.as_ref().is_some_and(|opened| publish::awaiting_modrinth(opened, &pack_data, &release).is_some());
            if awaiting_modrinth || !publish::link_server_pack(&releases_path, &mut release).await || !discord::is_approved(&release).await {
                continue;
            }
            match discord::announce(pack, &releases_path, &release.version, &webhook).await {
                Ok(()) => {
                    let _ = app.emit("release-announced", &release.version);
                }
                Err(error) => eprintln!("packraven: announcing {} failed: {error:#}", release.version),
            }
        }
    }
}
