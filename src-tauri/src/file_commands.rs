use std::collections::HashSet;
use std::path::{Path, PathBuf};

use clipboard_rs::{Clipboard, ClipboardContent, ClipboardContext};
use tauri::path::BaseDirectory;
use tauri::{AppHandle, Manager, State, Window};
use tauri_plugin_opener::OpenerExt;

use crate::commands::AppState;
use crate::files::{self, Entry, FileContent, Pasted};
use crate::global_packs::{self, Copied, Loader, PackFolder, PackKind};
use crate::history::{self, Action, HistoryView, Outcome};
use crate::os_drag;
use crate::text_search;

type CmdResult<T> = Result<T, String>;

fn err(error: anyhow::Error) -> String {
    format!("{error:#}")
}

#[tauri::command(async)]
pub fn list_dir(state: State<'_, AppState>, path: String) -> CmdResult<Vec<Entry>> {
    files::list(&state.settings().instance_dir, &path).map_err(err)
}

#[tauri::command]
pub fn read_file(state: State<'_, AppState>, path: String, allow_large: Option<bool>) -> CmdResult<FileContent> {
    let settings = state.settings();
    files::read(&settings.instance_dir, &path, settings.editor_limits(), allow_large.unwrap_or(false)).map_err(err)
}

#[tauri::command]
pub fn write_file(state: State<'_, AppState>, path: String, text: String, crlf: bool, bom: bool) -> CmdResult<()> {
    let root = state.settings().instance_dir;
    files::write(&root, &path, &text, crlf, bom).map_err(err)?;
    if path.ends_with(".proberaven/dump.json") || path.ends_with("proberaven/dump.json") || path == crate::pack::LOCKFILE {
        state.datapack.invalidate(&root);
    }
    Ok(())
}

#[tauri::command]
pub async fn index_files(state: State<'_, AppState>) -> CmdResult<files::Index> {
    let root = state.settings().instance_dir;
    tauri::async_runtime::spawn_blocking(move || files::index(&root)).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn search_text(state: State<'_, AppState>, query: text_search::Query) -> CmdResult<Option<text_search::Results>> {
    let settings = state.settings();
    let (root, limit) = (settings.instance_dir.clone(), settings.editor_limits().ask);
    tauri::async_runtime::spawn_blocking(move || text_search::search(&root, &query, limit)).await.map_err(|e| e.to_string())?.map_err(err)
}

#[tauri::command]
pub async fn replace_text(state: State<'_, AppState>, query: text_search::Query, targets: Vec<text_search::Target>) -> CmdResult<text_search::Replaced> {
    let settings = state.settings();
    let (root, limit) = (settings.instance_dir.clone(), settings.editor_limits().ask);
    let task_root = root.clone();
    let mut replaced = tauri::async_runtime::spawn_blocking(move || text_search::replace(&task_root, &query, &targets, limit)).await.map_err(|e| e.to_string())?.map_err(err)?;
    if !replaced.files.is_empty() {
        let undo = replaced.undoable.then(|| Action::Rewrite(std::mem::take(&mut replaced.rewrites)));
        state.history.lock().unwrap().record(&root, replaced.label.clone(), undo);
    }
    Ok(replaced)
}

#[tauri::command]
pub async fn copy_paths(state: State<'_, AppState>, paths: Vec<String>, absolute: bool) -> CmdResult<()> {
    let text = files::path_text(&state.settings().instance_dir, &paths, absolute).map_err(err)?;
    tauri::async_runtime::spawn_blocking(move || {
        let clipboard = ClipboardContext::new().map_err(|e| format!("opening the clipboard: {e}"))?;
        clipboard.set_text(text).map_err(|e| format!("copying to the clipboard: {e}"))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn open_with_default_app(app: AppHandle, state: State<'_, AppState>, path: String) -> CmdResult<()> {
    let absolute = files::path_text(&state.settings().instance_dir, &[path], true).map_err(err)?;
    app.opener().open_path(absolute, None::<&str>).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn default_app(extension: String) -> Option<String> {
    tauri::async_runtime::spawn_blocking(move || crate::associations::default_app(&extension)).await.ok().flatten()
}

#[tauri::command]
pub fn absolute_path(state: State<'_, AppState>, path: String) -> CmdResult<String> {
    Ok(files::resolve(&state.settings().instance_dir, &path).map_err(err)?.to_string_lossy().into_owned())
}

#[tauri::command]
pub fn create_file(state: State<'_, AppState>, path: String) -> CmdResult<()> {
    let root = state.settings().instance_dir;
    files::create_file(&root, &path).map_err(err)?;
    invalidate_if_datapack(&state, &root, &[path.as_str()]);
    record_created(&state, &root, "New file", &path);
    Ok(())
}

#[tauri::command]
pub fn create_dir(state: State<'_, AppState>, path: String) -> CmdResult<()> {
    let root = state.settings().instance_dir;
    files::create_dir(&root, &path).map_err(err)?;
    record_created(&state, &root, "New folder", &path);
    Ok(())
}

const MAX_VIEWED_BYTES: u64 = 256 * 1024 * 1024;
const MAX_NBT_BYTES: u64 = 64 * 1024 * 1024;

fn relative_to(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/")
}

#[tauri::command]
pub async fn read_bytes(state: State<'_, AppState>, path: String) -> CmdResult<tauri::ipc::Response> {
    let root = state.settings().instance_dir;
    let bytes = tauri::async_runtime::spawn_blocking(move || files::bytes(&root, &path, MAX_VIEWED_BYTES)).await.map_err(|e| e.to_string())?.map_err(err)?;
    Ok(tauri::ipc::Response::new(bytes))
}

#[tauri::command]
pub async fn read_log(state: State<'_, AppState>, path: String, offset: u64) -> CmdResult<crate::logs::LogChunk> {
    let root = state.settings().instance_dir;
    tauri::async_runtime::spawn_blocking(move || crate::logs::read(&root, &path, offset)).await.map_err(|e| e.to_string())?.map_err(err)
}

#[tauri::command]
pub async fn probejs_status(state: State<'_, AppState>, force: bool) -> CmdResult<crate::probejs::ProbeStatus> {
    let root = state.settings().instance_dir;
    let cache = std::sync::Arc::clone(&state.probejs);
    tauri::async_runtime::spawn_blocking(move || cache.ensure(&root, force).0).await.map_err(|e| e.to_string())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KubejsContext {
    event_sides: std::collections::BTreeMap<String, Vec<String>>,
}

#[tauri::command]
pub async fn kubejs_context(state: State<'_, AppState>) -> CmdResult<KubejsContext> {
    let root = state.settings().instance_dir;
    let cache = std::sync::Arc::clone(&state.probejs);
    let event_sides = tauri::async_runtime::spawn_blocking(move || cache.ensure(&root, false).1.map(|index| index.event_sides().clone()).unwrap_or_default())
        .await
        .map_err(|e| e.to_string())?;
    Ok(KubejsContext { event_sides })
}

#[tauri::command]
pub async fn probejs_complete(state: State<'_, AppState>, text: String, offset: usize) -> CmdResult<crate::probejs::CompletionResult> {
    let root = state.settings().instance_dir;
    let cache = std::sync::Arc::clone(&state.probejs);
    tauri::async_runtime::spawn_blocking(move || {
        match cache.get(&root).or_else(|| cache.ensure(&root, false).1) {
            Some(idx) => idx.complete(&text, offset),
            None => crate::probejs::CompletionResult { from: offset, to: offset, items: Vec::new(), in_string: false },
        }
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn probejs_hover(state: State<'_, AppState>, text: String, offset: usize) -> CmdResult<Option<crate::probejs::HoverInfo>> {
    let root = state.settings().instance_dir;
    let cache = std::sync::Arc::clone(&state.probejs);
    tauri::async_runtime::spawn_blocking(move || {
        let Some(idx) = cache.get(&root) else { return Ok(None) };
        Ok(idx.hover(&text, offset))
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(err)
}

fn touches_datapack(path: &str) -> bool {
    let norm = path.replace('\\', "/").to_lowercase();
    norm.starts_with("data/")
        || norm.contains("/data/")
        || norm.starts_with("datapacks/")
        || norm.starts_with("mods/")
        || norm.contains(".proberaven/")
        || norm == crate::pack::LOCKFILE
}

fn invalidate_if_datapack(state: &AppState, root: &Path, paths: &[&str]) {
    if paths.iter().any(|p| touches_datapack(p)) {
        state.datapack.invalidate(root);
    }
}

#[tauri::command]
pub async fn datapack_status(state: State<'_, AppState>, force: bool) -> CmdResult<crate::mcdoc::DatapackStatus> {
    let settings = state.settings();
    let cache = std::sync::Arc::clone(&state.datapack);
    let probe = std::sync::Arc::clone(&state.probejs);
    Ok(cache.ensure(settings.instance_dir, settings.install_dir, probe, force).await.0)
}

#[tauri::command]
pub async fn datapack_lint(state: State<'_, AppState>, path: String, text: String) -> CmdResult<Vec<crate::mcdoc::LintDiagnostic>> {
    let settings = state.settings();
    let root = settings.instance_dir.clone();
    let cache = std::sync::Arc::clone(&state.datapack);
    let probe = std::sync::Arc::clone(&state.probejs);
    let index = match cache.get(&settings.instance_dir) {
        Some(idx) => Some(idx),
        None => cache.ensure(settings.instance_dir, settings.install_dir, probe, false).await.1,
    };
    let Some(idx) = index else { return Ok(Vec::new()) };
    tauri::async_runtime::spawn_blocking(move || idx.lint_in_pack(&path, &text, &crate::mcdoc::overlays::InstanceFiles(&root)))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn datapack_complete(state: State<'_, AppState>, path: String, text: String, offset: usize) -> CmdResult<crate::mcdoc::completer::CompletionResult> {
    let settings = state.settings();
    let cache = std::sync::Arc::clone(&state.datapack);
    let probe = std::sync::Arc::clone(&state.probejs);
    let index = match cache.get(&settings.instance_dir) {
        Some(idx) => Some(idx),
        None => cache.ensure(settings.instance_dir, settings.install_dir, probe, false).await.1,
    };
    let Some(idx) = index else {
        return Ok(crate::mcdoc::completer::CompletionResult::empty(offset));
    };
    tauri::async_runtime::spawn_blocking(move || idx.complete(&path, &text, offset))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn datapack_hover(state: State<'_, AppState>, path: String, text: String, offset: usize) -> CmdResult<Option<crate::mcdoc::completer::HoverInfo>> {
    let root = state.settings().instance_dir;
    let cache = std::sync::Arc::clone(&state.datapack);
    let Some(idx) = cache.get(&root) else { return Ok(None) };
    tauri::async_runtime::spawn_blocking(move || idx.hover(&path, &text, offset))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn read_nbt(state: State<'_, AppState>, path: String) -> CmdResult<crate::nbt::Document> {
    let root = state.settings().instance_dir;
    tauri::async_runtime::spawn_blocking(move || crate::nbt::parse(&files::bytes(&root, &path, MAX_NBT_BYTES)?)).await.map_err(|e| e.to_string())?.map_err(err)
}

#[tauri::command]
pub async fn decompile_class(app: AppHandle, state: State<'_, AppState>, path: String) -> CmdResult<String> {
    let settings = state.settings();
    let (archive, inner) = crate::archive::split(&settings.instance_dir, &path).filter(|(_, inner)| !inner.is_empty()).ok_or("Only classes inside a jar can be decompiled.")?;
    let vineflower = app.path().resolve("vineflower.jar", BaseDirectory::Resource).map_err(|e| e.to_string())?;
    let java = crate::decompile::java(&settings);
    tauri::async_runtime::spawn_blocking(move || crate::decompile::decompile(&java, &vineflower, &archive, &inner)).await.map_err(|e| e.to_string())?.map_err(err)
}

#[tauri::command]
pub async fn extract_archive(state: State<'_, AppState>, path: String) -> CmdResult<String> {
    let root = state.settings().instance_dir;
    if crate::archive::split(&root, &path).is_some_and(|(_, inner)| !inner.is_empty()) {
        return Err("Extract the archive this one is inside first.".into());
    }
    let source = files::resolve_entry(&root, &path).map_err(err)?;
    let target = tauri::async_runtime::spawn_blocking(move || crate::archive::extract(&source)).await.map_err(|e| e.to_string())?.map_err(err)?;
    let label = format!("Extract {}", history::describe(std::slice::from_ref(&path)));
    state.history.lock().unwrap().record(&root, label, Some(Action::Trash(vec![target.clone()])));
    Ok(relative_to(&root, &target))
}

#[tauri::command]
pub async fn compress_paths(state: State<'_, AppState>, paths: Vec<String>) -> CmdResult<String> {
    let root = state.settings().instance_dir;
    let sources = paths.iter().map(|p| files::resolve_entry(&root, p)).collect::<anyhow::Result<Vec<_>>>().map_err(err)?;
    let dir = sources.first().and_then(|s| s.parent()).ok_or("Pick something to compress.")?.to_path_buf();
    let label = format!("Compress {}", history::describe(&sources));
    let target = tauri::async_runtime::spawn_blocking(move || crate::archive::compress(&sources, &dir)).await.map_err(|e| e.to_string())?.map_err(err)?;
    state.history.lock().unwrap().record(&root, label, Some(Action::Trash(vec![target.clone()])));
    Ok(relative_to(&root, &target))
}

#[tauri::command]
pub fn pack_loaders(state: State<'_, AppState>, kind: PackKind) -> Vec<Loader> {
    let lock = state.pack().ok().map(|pack| pack.lock);
    global_packs::loaders(&state.settings().instance_dir, lock.as_ref(), kind)
}

#[tauri::command]
pub fn pack_folder(state: State<'_, AppState>, path: String) -> CmdResult<PackFolder> {
    global_packs::folder(&state.settings().instance_dir, &path).map_err(err)
}

#[tauri::command]
pub async fn copy_into_pack(state: State<'_, AppState>, sources: Vec<String>, pack: String, create: bool) -> CmdResult<Copied> {
    let settings = state.settings();
    let root = settings.instance_dir.clone();
    let meta = if create && !global_packs::is_kubejs_pack(&pack) {
        let first = sources.first().ok_or("Pick something in a mod's jar to copy into a pack.")?;
        let kind = global_packs::inside_path(first).and_then(PackKind::of).ok_or("Only files under data/ or assets/ in a mod's jar belong in a pack.")?;
        let lock = state.pack().map_err(err)?.lock;
        let name = pack.trim_end_matches('/').rsplit('/').next().unwrap_or(&pack).to_string();
        Some(global_packs::pack_meta(&settings.install_dir, lock.minecraft().map_err(err)?, kind, &name).map_err(err)?)
    } else {
        None
    };
    let task_root = root.clone();
    let task_sources = sources.clone();
    let copied = tauri::async_runtime::spawn_blocking(move || global_packs::copy_into_pack(&task_root, &task_sources, &pack, meta.as_deref()))
        .await
        .map_err(|e| e.to_string())?
        .map_err(err)?;
    if !copied.created.is_empty() {
        let refs: Vec<&str> = copied.created.iter().map(String::as_str).collect();
        invalidate_if_datapack(&state, &root, &refs);
        let label = format!("Override {}", history::describe(&sources));
        let created = copied.created.iter().map(|p| root.join(p)).collect();
        state.history.lock().unwrap().record(&root, label, Some(Action::Trash(created)));
    }
    Ok(copied)
}

// The webview streams video and audio from the asset protocol, which can seek, so we allow
// each file you open there rather than the whole disk.
#[tauri::command]
pub fn media_path(app: AppHandle, state: State<'_, AppState>, path: String) -> CmdResult<String> {
    // Settings can store the pack folder with forward slashes, so we use the platform's own
    // form for both the scope and the URL to keep them matching.
    let absolute = files::path_text(&state.settings().instance_dir, &[path], true).map_err(err)?;
    app.asset_protocol_scope().allow_file(&absolute).map_err(|e| e.to_string())?;
    Ok(absolute)
}

fn record_created(state: &AppState, root: &Path, what: &str, path: &str) {
    let created = root.join(path);
    let label = format!("{what} {}", history::describe(std::slice::from_ref(&created)));
    state.history.lock().unwrap().record(root, label, Some(Action::Trash(vec![created])));
}

fn record_pasted(state: &AppState, root: &Path, verb: &str, pasted: &Pasted) {
    let (label, undo) = if pasted.transfers.is_empty() {
        (format!("{verb} {}", history::describe(&pasted.copies)), Action::Trash(pasted.copies.clone()))
    } else {
        let targets: Vec<&PathBuf> = pasted.transfers.iter().map(|(_, to)| to).collect();
        (format!("{verb} {}", history::describe(&targets)), Action::Move(pasted.transfers.iter().map(|(from, to)| (to.clone(), from.clone())).collect()))
    };
    if !pasted.transfers.is_empty() || !pasted.copies.is_empty() {
        state.history.lock().unwrap().record(root, label, Some(undo));
    }
}

#[tauri::command]
pub async fn trash_paths(state: State<'_, AppState>, paths: Vec<String>) -> CmdResult<()> {
    let root = state.settings().instance_dir;
    let label = format!("Delete {}", history::describe(&paths));
    let datapack_affected = paths.iter().any(|p| touches_datapack(p));
    let task_root = root.clone();
    let trashed = tauri::async_runtime::spawn_blocking(move || files::trash(&task_root, &paths)).await.map_err(|e| e.to_string())?.map_err(err)?;
    if datapack_affected {
        state.datapack.invalidate(&root);
    }
    state.history.lock().unwrap().record(&root, label, trashed.map(Action::Restore));
    Ok(())
}

#[tauri::command]
pub async fn copy_files(state: State<'_, AppState>, paths: Vec<String>, cut: bool) -> CmdResult<()> {
    let absolute = files::absolute_paths(&state.settings().instance_dir, &paths).map_err(err)?;
    tauri::async_runtime::spawn_blocking(move || {
        let clipboard = ClipboardContext::new().map_err(|e| format!("opening the clipboard: {e}"))?;
        let mut contents = vec![ClipboardContent::Files(absolute)];
        // Explorer reads this Windows shell format to decide whether a paste moves or copies.
        // Finder has no equivalent, so on macOS a cut pastes as a copy outside packraven.
        if cfg!(windows) {
            let effect: u32 = if cut { DROPEFFECT_MOVE } else { 1 };
            contents.push(ClipboardContent::Other("Preferred DropEffect".into(), effect.to_le_bytes().to_vec()));
        }
        clipboard.set(contents).map_err(|e| format!("copying to the clipboard: {e}"))
    })
    .await
    .map_err(|e| e.to_string())?
}

const DROPEFFECT_MOVE: u32 = 2;

// Explorer marks a cut by setting this Windows shell format to DROPEFFECT_MOVE and leaves the
// move to whichever app you paste into.
fn explorer_cut(clipboard: &ClipboardContext) -> bool {
    cfg!(windows)
        && clipboard
            .get_buffer("Preferred DropEffect")
            .ok()
            .and_then(|bytes| Some(u32::from_le_bytes(bytes.get(..4)?.try_into().ok()?)))
            .is_some_and(|effect| effect & DROPEFFECT_MOVE != 0)
}

#[tauri::command]
pub async fn paste_files(state: State<'_, AppState>, dir: String, cut: Vec<String>) -> CmdResult<Pasted> {
    let root = state.settings().instance_dir;
    let datapack_affected = touches_datapack(&dir) || cut.iter().any(|p| touches_datapack(p));
    let task_root = root.clone();
    let pasted = tauri::async_runtime::spawn_blocking(move || {
        let root = task_root;
        let clipboard = ClipboardContext::new().map_err(|e| format!("opening the clipboard: {e}"))?;
        // clipboard-rs reports a clipboard without files as an error, which isn't one for us.
        let sources: Vec<PathBuf> = clipboard.get_files().unwrap_or_default().into_iter().map(PathBuf::from).collect();
        if sources.is_empty() {
            return Err("The clipboard has no files to paste.".to_string());
        }
        // You may have copied something else since you cut, so we only treat the paste as a move
        // when the clipboard still holds exactly the files you cut.
        let cut_paths: HashSet<PathBuf> = files::absolute_paths(&root, &cut).map_err(err)?.into_iter().map(PathBuf::from).collect();
        let cut_here = !cut.is_empty() && sources.iter().cloned().collect::<HashSet<_>>() == cut_paths;
        let move_requested = cut_here || explorer_cut(&clipboard);
        let pasted = files::paste(&root, &dir, &sources, move_requested).map_err(err)?;
        // Like Explorer, we empty the clipboard after a cut, since its files are no longer there.
        if move_requested {
            let _ = clipboard.clear();
        }
        Ok(pasted)
    })
    .await
    .map_err(|e| e.to_string())??;
    if datapack_affected {
        state.datapack.invalidate(&root);
    }
    record_pasted(&state, &root, "Paste", &pasted);
    Ok(pasted)
}

#[tauri::command]
pub fn rename_path(state: State<'_, AppState>, from: String, to: String) -> CmdResult<()> {
    let root = state.settings().instance_dir;
    files::rename(&root, &from, &to).map_err(err)?;
    invalidate_if_datapack(&state, &root, &[from.as_str(), to.as_str()]);
    let label = format!("Rename {}", history::describe(&[&from]));
    state.history.lock().unwrap().record(&root, label, Some(Action::Move(vec![(root.join(&to), root.join(&from))])));
    Ok(())
}

#[tauri::command]
pub async fn rename_paths(state: State<'_, AppState>, renames: Vec<(String, String)>) -> CmdResult<Vec<(String, String)>> {
    let root = state.settings().instance_dir;
    let task_root = root.clone();
    let renamed = tauri::async_runtime::spawn_blocking(move || files::rename_all(&task_root, &renames)).await.map_err(|e| e.to_string())?.map_err(err)?;
    if !renamed.is_empty() {
        let refs: Vec<&str> = renamed.iter().flat_map(|(f, t)| [f.as_str(), t.as_str()]).collect();
        invalidate_if_datapack(&state, &root, &refs);
        let sources: Vec<&String> = renamed.iter().map(|(from, _)| from).collect();
        let label = format!("Rename {}", history::describe(&sources));
        let undo = Action::Move(renamed.iter().map(|(from, to)| (root.join(to), root.join(from))).collect());
        state.history.lock().unwrap().record(&root, label, Some(undo));
    }
    Ok(renamed)
}

#[tauri::command]
pub fn move_paths(state: State<'_, AppState>, paths: Vec<String>, dir: String) -> CmdResult<Vec<(String, String)>> {
    let root = state.settings().instance_dir;
    let moved = files::move_into(&root, &paths, &dir).map_err(err)?;
    if !moved.is_empty() {
        let refs: Vec<&str> = moved.iter().flat_map(|(f, t)| [f.as_str(), t.as_str()]).collect();
        invalidate_if_datapack(&state, &root, &refs);
        let targets: Vec<&String> = moved.iter().map(|(_, to)| to).collect();
        let label = format!("Move {}", history::describe(&targets));
        let undo = Action::Move(moved.iter().map(|(from, to)| (root.join(to), root.join(from))).collect());
        state.history.lock().unwrap().record(&root, label, Some(undo));
    }
    Ok(moved)
}

#[tauri::command]
pub async fn import_paths(state: State<'_, AppState>, dir: String, sources: Vec<PathBuf>) -> CmdResult<Pasted> {
    let root = state.settings().instance_dir;
    let datapack_affected = touches_datapack(&dir);
    let requested_move = os_drag::requested_move();
    let task_root = root.clone();
    let pasted = tauri::async_runtime::spawn_blocking(move || files::import(&task_root, &dir, &sources, requested_move)).await.map_err(|e| e.to_string())?.map_err(err)?;
    if datapack_affected {
        state.datapack.invalidate(&root);
    }
    record_pasted(&state, &root, if pasted.transfers.is_empty() { "Copy in" } else { "Move in" }, &pasted);
    Ok(pasted)
}

#[tauri::command]
pub fn file_history(state: State<'_, AppState>) -> HistoryView {
    state.history.lock().unwrap().view(&state.settings().instance_dir)
}

#[tauri::command]
pub async fn file_undo(state: State<'_, AppState>, redo: bool) -> CmdResult<Option<Outcome>> {
    let root = state.settings().instance_dir;
    // Restoring from the trash or moving across drives can take a while, but undo steps must
    // run one at a time, so we hold the history for the whole step.
    let outcome = tokio::task::block_in_place(|| state.history.lock().unwrap().step(&root, redo)).map_err(err)?;
    if outcome.is_some() {
        state.datapack.invalidate(&root);
    }
    Ok(outcome)
}

// The OS drag loop needs the main thread, and on Windows it blocks that thread until you drop,
// so we start it there and wait for it here.
#[tauri::command]
pub async fn drag_out(app: AppHandle, window: Window, state: State<'_, AppState>, paths: Vec<String>) -> CmdResult<()> {
    let files: Vec<PathBuf> = files::absolute_paths(&state.settings().instance_dir, &paths).map_err(err)?.into_iter().map(PathBuf::from).collect();
    let (sender, receiver) = std::sync::mpsc::channel();
    app.run_on_main_thread(move || {
        let _ = sender.send(os_drag::drag_out(&window, &files).map_err(|e| format!("starting the drag: {e}")));
    })
    .map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || receiver.recv().unwrap_or(Ok(()))).await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn open_pack_folder(app: AppHandle, state: State<'_, AppState>) -> CmdResult<()> {
    let root = state.settings().instance_dir;
    app.opener().open_path(root.to_string_lossy(), None::<&str>).map_err(|e| e.to_string())
}
