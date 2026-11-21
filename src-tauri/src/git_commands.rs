use serde::Serialize;
use tauri::{AppHandle, State};

use crate::commands::AppState;
use crate::gitops::{self, Commit, Diff, IgnoreOption, IgnorePreview, LineAction, PullPreview, Status, UntrackPreview, Version};
use crate::modsync::Prefer;

type CmdResult<T> = Result<T, String>;

fn err(error: anyhow::Error) -> String {
    format!("{error:#}")
}

#[derive(Serialize)]
pub struct GitOverview {
    status: Status,
    commits: Vec<Commit>,
}

// These run git, which can take a while on a big instance, so we keep them off the main thread.
#[tauri::command(async)]
pub fn git_overview(state: State<'_, AppState>) -> CmdResult<GitOverview> {
    let dir = state.settings().instance_dir;
    let (status, commits) = std::thread::scope(|scope| {
        let commits = scope.spawn(|| gitops::recent_commits(&dir, 30));
        let status = gitops::status(&dir);
        (status, commits.join().unwrap_or_default())
    });
    Ok(GitOverview { status: status.map_err(err)?, commits })
}

#[tauri::command(async)]
pub fn git_status(state: State<'_, AppState>) -> CmdResult<Status> {
    gitops::status(&state.settings().instance_dir).map_err(err)
}

#[tauri::command(async)]
pub fn git_diff(state: State<'_, AppState>, path: String, staged: bool, untracked: bool) -> CmdResult<Diff> {
    gitops::diff(&state.settings().instance_dir, &path, staged, untracked).map_err(err)
}

#[tauri::command(async)]
pub fn git_blob(state: State<'_, AppState>, path: String, version: Version) -> CmdResult<tauri::ipc::Response> {
    gitops::blob(&state.settings().instance_dir, &path, version).map(tauri::ipc::Response::new).map_err(err)
}

#[tauri::command(async)]
pub fn git_apply_lines(state: State<'_, AppState>, path: String, diff: String, selected: Vec<usize>, action: LineAction) -> CmdResult<()> {
    gitops::apply_lines(&state.settings().instance_dir, &path, &diff, &selected, action).map_err(err)
}

#[tauri::command(async)]
pub fn git_stage(state: State<'_, AppState>, paths: Vec<String>) -> CmdResult<()> {
    gitops::stage(&state.settings().instance_dir, &paths).map_err(err)
}

#[tauri::command(async)]
pub fn git_unstage(state: State<'_, AppState>, paths: Vec<String>) -> CmdResult<()> {
    gitops::unstage(&state.settings().instance_dir, &paths).map_err(err)
}

#[tauri::command(async)]
pub fn git_discard(state: State<'_, AppState>, tracked: Vec<String>, untracked: Vec<String>) -> CmdResult<()> {
    gitops::discard(&state.settings().instance_dir, &tracked, &untracked).map_err(err)
}

#[tauri::command(async)]
pub fn git_ignore_options(state: State<'_, AppState>, paths: Vec<String>) -> CmdResult<Vec<IgnoreOption>> {
    gitops::ignore_options(&state.settings().instance_dir, &paths).map_err(err)
}

#[tauri::command(async)]
pub fn git_ignore_preview(state: State<'_, AppState>, patterns: Vec<String>) -> CmdResult<IgnorePreview> {
    gitops::ignore_preview(&state.settings().instance_dir, &patterns).map_err(err)
}

#[tauri::command(async)]
pub fn git_ignore(state: State<'_, AppState>, patterns: Vec<String>) -> CmdResult<Vec<String>> {
    gitops::ignore(&state.settings().instance_dir, &patterns).map_err(err)
}

#[tauri::command(async)]
pub fn git_untrack_preview(state: State<'_, AppState>, paths: Vec<String>) -> CmdResult<UntrackPreview> {
    gitops::untrack_preview(&state.settings().instance_dir, &paths).map_err(err)
}

#[tauri::command(async)]
pub fn git_keep_tracking(state: State<'_, AppState>, paths: Vec<String>) -> CmdResult<Vec<String>> {
    gitops::keep_tracking(&state.settings().instance_dir, &paths).map_err(err)
}

#[tauri::command(async)]
pub fn git_untrack(state: State<'_, AppState>, paths: Vec<String>) -> CmdResult<()> {
    gitops::untrack(&state.settings().instance_dir, &paths).map_err(err)
}

#[tauri::command]
pub fn git_commit(app: AppHandle, state: State<'_, AppState>, message: String) -> CmdResult<()> {
    let settings = state.settings();
    let (dir, identity) = (settings.instance_dir.clone(), settings.identity());
    state.jobs.start(&app, "git", move |log| async move { gitops::commit(&dir, &message, &identity, &log).await })
}

#[tauri::command]
pub async fn git_pull_preview(state: State<'_, AppState>) -> CmdResult<PullPreview> {
    gitops::pull_preview(&state.settings().instance_dir).await.map_err(err)
}

#[tauri::command]
pub fn git_sync(app: AppHandle, state: State<'_, AppState>, action: String, prefer: Option<Prefer>) -> CmdResult<()> {
    let dir = state.settings().instance_dir;
    state.jobs.start(&app, "git", move |log| async move {
        match action.as_str() {
            // Pull restores mods, whose future the compiler can't prove Send, so we drive it on a
            // blocking thread as revert_mods does.
            "pull" => tauri::async_runtime::spawn_blocking(move || tauri::async_runtime::block_on(gitops::pull(&dir, prefer, &log))).await?,
            "push" => gitops::push(&dir, &log).await,
            _ => gitops::fetch(&dir, &log).await,
        }
    })
}
