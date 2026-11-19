use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_opener::OpenerExt;
use tokio::sync::oneshot;

use crate::commands::AppState;
use crate::launch::Player;
use crate::minecraft_auth::{self, Account, Sessions, Slot};

type CmdResult<T> = Result<T, String>;

#[derive(Serialize)]
pub struct Accounts {
    first: Option<Account>,
    second: Option<Account>,
}

#[tauri::command]
pub fn minecraft_accounts() -> Accounts {
    Accounts { first: minecraft_auth::account(Slot::First), second: minecraft_auth::account(Slot::Second) }
}

#[tauri::command]
pub async fn minecraft_sign_in(app: AppHandle, state: State<'_, AppState>, slot: Slot) -> CmdResult<Option<Account>> {
    let (cancel_tx, cancel_rx) = oneshot::channel();
    if let Some(earlier) = state.minecraft_sign_in.lock().unwrap().replace(cancel_tx) {
        let _ = earlier.send(());
    }
    let open = |url: &str| app.opener().open_url(url, None::<&str>).map_err(|e| anyhow::anyhow!("couldn't open your browser: {e}"));
    let Some(session) = minecraft_auth::sign_in(slot, open, cancel_rx).await.map_err(|e| format!("{e:#}"))? else { return Ok(None) };
    let account = session.account.clone();
    minecraft_auth::remember(&state.minecraft, slot, session).await;
    Ok(Some(account))
}

#[tauri::command]
pub fn minecraft_sign_in_cancel(state: State<'_, AppState>) {
    if let Some(cancel) = state.minecraft_sign_in.lock().unwrap().take() {
        let _ = cancel.send(());
    }
}

#[tauri::command]
pub async fn minecraft_sign_out(state: State<'_, AppState>, slot: Slot) -> CmdResult<()> {
    minecraft_auth::sign_out(&state.minecraft, slot).await.map_err(|e| format!("{e:#}"))
}

// Mojang only allows launching the game for an account whose ownership we've verified, so there's no offline fallback.
pub async fn signed_in_player(sessions: &Sessions, slot: Slot) -> anyhow::Result<Player> {
    if minecraft_auth::account(slot).is_none() {
        anyhow::bail!("{}", slot.sign_in_first());
    }
    Ok(minecraft_auth::session(sessions, slot).await?.player())
}
