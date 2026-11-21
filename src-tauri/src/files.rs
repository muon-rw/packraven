use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};

use anyhow::{bail, Context, Result};
use serde::Serialize;
use sha1::{Digest, Sha1};

use crate::gitops::{self, hash_ignore_file, IndexStamp};

const HIDDEN: [&str; 1] = [".git"];

// git check-ignore leaves tracked files out of its answer, so a cached answer depends on the index
// as well as the ignore rules.
struct CachedIgnored {
    index_stamp: Option<IndexStamp>,
    rules_hash: [u8; 20],
    queries: Vec<String>,
    ignored: HashSet<String>,
}

const MAX_CACHED_DIRS: usize = 512;

static IGNORED_CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedIgnored>>> = OnceLock::new();

fn dir_ignore_rules_hash(root: &Path, relative: &str) -> [u8; 20] {
    let mut hasher = Sha1::new();
    gitops::hash_repo_ignore_rules(&mut hasher, root);
    hash_ignore_file(&mut hasher, ".gitignore", &root.join(".gitignore"));
    let mut prefix = PathBuf::new();
    for part in relative.split('/').filter(|p| !p.is_empty()) {
        prefix.push(part);
        let rel = format!("{}/.gitignore", prefix.to_string_lossy());
        hash_ignore_file(&mut hasher, &rel, &root.join(&prefix).join(".gitignore"));
    }
    hasher.finalize().into()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub ignored: bool,
    /// A jar or zip you can expand like a folder.
    pub archive: bool,
    /// Inside a jar or zip, so read-only.
    pub archived: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileContent {
    pub text: Option<String>,
    pub size: u64,
    pub unreadable: Option<String>,
    pub crlf: bool,
    pub bom: bool,
    pub archived: bool,
    /// Decompressed from a .gz file, so read-only.
    pub compressed: bool,
    /// Over the size you set to ask about, so it opens only in the simplified editor.
    pub large: bool,
}

pub fn resolve(root: &Path, relative: &str) -> Result<PathBuf> {
    let relative = Path::new(relative);
    if relative.components().any(|c| !matches!(c, Component::Normal(_) | Component::CurDir)) {
        bail!("{} is outside the pack folder", relative.display());
    }
    Ok(root.join(relative))
}

fn ignored(root: &Path, paths: &[String]) -> HashSet<String> {
    if paths.is_empty() {
        return HashSet::new();
    }
    let mut command = crate::git::command();
    command.current_dir(root).args(["check-ignore", "-z", "--stdin"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    let Ok(mut child) = command.spawn() else { return HashSet::new() };
    // Git answers each path as it reads it, so we feed stdin from another thread. Writing it
    // all first stalls once git fills the stdout pipe and stops reading.
    let input = format!("{}\0", paths.join("\0"));
    let writer = child.stdin.take().map(|mut stdin| std::thread::spawn(move || stdin.write_all(input.as_bytes())));
    let output = child.wait_with_output();
    if let Some(writer) = writer {
        let _ = writer.join();
    }
    let Ok(output) = output else { return HashSet::new() };
    String::from_utf8_lossy(&output.stdout).split('\0').filter(|p| !p.is_empty()).map(|p| p.trim_end_matches('/').to_string()).collect()
}

pub fn list(root: &Path, relative: &str) -> Result<Vec<Entry>> {
    let dir = resolve(root, relative)?;
    if let Some((file, inner)) = crate::archive::split(root, relative) {
        return crate::archive::list(&file, &inner, relative);
    }
    let prefix = if relative.is_empty() { String::new() } else { format!("{}/", relative.trim_end_matches('/')) };
    // A folder you just deleted or moved can still be expanded in the tree for a moment.
    let listing = match std::fs::read_dir(&dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !relative.is_empty() => return Ok(Vec::new()),
        listing => listing.with_context(|| format!("reading {}", dir.display()))?,
    };
    let mut entries: Vec<Entry> = listing
        .flatten()
        .filter(|e| !HIDDEN.contains(&e.file_name().to_string_lossy().as_ref()))
        .filter_map(|e| {
            let metadata = e.metadata().ok()?;
            let name = e.file_name().to_string_lossy().into_owned();
            let is_dir = metadata.is_dir();
            Some(Entry { path: format!("{prefix}{name}"), archive: !is_dir && crate::archive::is_archive(&name), archived: false, name, is_dir, size: metadata.len(), ignored: false })
        })
        .collect();
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    let queries: Vec<String> = entries.iter().map(|e| if e.is_dir { format!("{}/", e.path) } else { e.path.clone() }).collect();
    let index_stamp = gitops::index_stamp(&root.join(".git"));
    let rules_hash = dir_ignore_rules_hash(root, relative);
    let cache = IGNORED_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let ignored = {
        let guard = cache.lock().unwrap();
        guard.get(&dir).filter(|entry| entry.index_stamp == index_stamp && entry.rules_hash == rules_hash && entry.queries == queries).map(|entry| entry.ignored.clone())
    }
    .unwrap_or_else(|| {
        let computed = ignored(root, &queries);
        let mut guard = cache.lock().unwrap();
        if guard.len() >= MAX_CACHED_DIRS {
            guard.clear();
        }
        guard.insert(dir.clone(), CachedIgnored { index_stamp, rules_hash, queries, ignored: computed.clone() });
        computed
    });
    for entry in &mut entries {
        entry.ignored = ignored.contains(&entry.path);
    }
    Ok(entries)
}

/// The sizes, in bytes, past which we ask before opening a file in the editor, and past which
/// we won't open it there at all.
#[derive(Clone, Copy)]
pub struct Limits {
    pub ask: u64,
    pub max: u64,
}

impl Limits {
    pub const DEFAULT: Limits = Limits { ask: 50_000_000, max: 100_000_000 };
}

fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1e6)
}

// The editor slows down past `ask` and can run the webview out of memory past `max`, so we
// stop at `max` and only go past `ask` when you choose the simplified editor.
fn over_limit(size: u64, limits: Limits, allow_large: bool) -> Option<(String, bool)> {
    if size > limits.max {
        let reason = format!("This file is {}, over the recommended {} limit for the editor. You can raise this limit in App settings.", megabytes(size), megabytes(limits.max));
        Some((reason, false))
    } else if size > limits.ask && !allow_large {
        let reason = format!("This file is {}, over the recommended {} limit for intelligent features. You can open it in a simplified editor.", megabytes(size), megabytes(limits.ask));
        Some((reason, true))
    } else {
        None
    }
}

pub fn read(root: &Path, relative: &str, limits: Limits, allow_large: bool) -> Result<FileContent> {
    let path = resolve(root, relative)?;
    let inside = crate::archive::split(root, relative).filter(|(_, inner)| !inner.is_empty());
    let archived = inside.is_some();
    let size = match &inside {
        Some((file, inner)) => crate::archive::size(file, inner)?,
        None => std::fs::metadata(&path)?.len(),
    };
    let compressed = crate::archive::is_gzip(relative);
    let unreadable = |reason: &str, large: bool| FileContent { text: None, size, unreadable: Some(reason.to_string()), crlf: false, bom: false, archived, compressed, large };
    if let Some((reason, large)) = over_limit(size, limits, allow_large) {
        return Ok(unreadable(&reason, large));
    }
    let bytes = match &inside {
        Some((file, inner)) => crate::archive::read(file, inner, limits.max)?,
        None => std::fs::read(&path)?,
    };
    // Minecraft gzips old logs, so we show them decompressed.
    let bytes = if compressed {
        match crate::archive::gunzip(&bytes, limits.max) {
            Ok(Some(data)) => data,
            Ok(None) => return Ok(unreadable(&format!("This file is over the {} limit for the editor once decompressed.", megabytes(limits.max)), false)),
            Err(_) => return Ok(unreadable("This file ends in .gz but isn't valid gzip, so we can't decompress it.", false)),
        }
    } else {
        bytes
    };
    if let Some((reason, large)) = over_limit(bytes.len() as u64, limits, allow_large) {
        return Ok(unreadable(&reason, large));
    }
    let large = bytes.len() as u64 > limits.ask;
    if bytes.iter().take(8000).any(|b| *b == 0) {
        return Ok(unreadable("This is a binary file, so you can't edit it as text.", false));
    }
    let Ok(text) = String::from_utf8(bytes) else {
        return Ok(unreadable("This file isn't UTF-8 text, so you can't edit it here.", false));
    };
    let bom = text.starts_with('\u{feff}');
    let body = text.trim_start_matches('\u{feff}');
    let crlf = body.contains("\r\n");
    Ok(FileContent { text: Some(body.replace("\r\n", "\n")), size, unreadable: None, crlf, bom, archived, compressed, large })
}

pub fn write(root: &Path, relative: &str, text: &str, crlf: bool, bom: bool) -> Result<()> {
    let path = resolve(root, relative)?;
    // We show these files decompressed, so writing the text back would break them.
    if crate::archive::is_gzip(relative) || crate::archive::split(root, relative).is_some_and(|(_, inner)| !inner.is_empty()) {
        bail!("{relative} is read-only here; extract it to edit a copy");
    }
    let mut body = if crlf { text.replace("\r\n", "\n").replace('\n', "\r\n") } else { text.to_string() };
    if bom {
        body.insert(0, '\u{feff}');
    }
    replace_contents(&path, body.as_bytes())
}

/// Writes to a temporary file first, so a failed write can't leave the file half written.
pub(crate) fn replace_contents(path: &Path, bytes: &[u8]) -> Result<()> {
    let temp = path.with_extension("packraven-tmp");
    std::fs::write(&temp, bytes).with_context(|| format!("writing {}", path.display()))?;
    std::fs::rename(&temp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

pub(crate) fn resolve_entry(root: &Path, relative: &str) -> Result<PathBuf> {
    if relative.trim_matches('/').is_empty() {
        bail!("pick a file or folder inside the pack folder");
    }
    resolve(root, relative)
}

pub fn create_file(root: &Path, relative: &str) -> Result<()> {
    let path = resolve_entry(root, relative)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::OpenOptions::new().write(true).create_new(true).open(&path).with_context(|| format!("creating {relative}"))?;
    Ok(())
}

pub fn create_dir(root: &Path, relative: &str) -> Result<()> {
    let path = resolve_entry(root, relative)?;
    if path.exists() {
        bail!("{relative} already exists");
    }
    std::fs::create_dir_all(&path).with_context(|| format!("creating {relative}"))
}

pub fn trash(root: &Path, relatives: &[String]) -> Result<Option<Vec<trash::TrashItem>>> {
    let paths = relatives.iter().map(|r| resolve_entry(root, r)).collect::<Result<Vec<_>>>()?;
    crate::history::trash(&paths)
}

/// Reads a file's raw bytes for a viewer, from the pack folder or from inside an archive.
pub fn bytes(root: &Path, relative: &str, limit: u64) -> Result<Vec<u8>> {
    if let Some((file, inner)) = crate::archive::split(root, relative).filter(|(_, inner)| !inner.is_empty()) {
        return crate::archive::read(&file, &inner, limit);
    }
    let path = resolve_entry(root, relative)?;
    if std::fs::metadata(&path)?.len() > limit {
        bail!("{relative} is too large to open here");
    }
    Ok(std::fs::read(&path)?)
}

pub fn absolute_paths(root: &Path, relatives: &[String]) -> Result<Vec<String>> {
    relatives.iter().map(|r| Ok(resolve_entry(root, r)?.to_string_lossy().into_owned())).collect()
}

/// The pack's entries as relative paths, with a trailing slash on folders.
#[derive(Serialize)]
pub struct Index {
    pub paths: Vec<String>,
    pub truncated: bool,
}

// A pack with worlds and caches can hold far more files than you'd search by name, so we stop
// at a limit rather than stall the search.
const MAX_INDEXED: usize = 100_000;

pub fn index(root: &Path) -> Index {
    let mut paths = Vec::new();
    let walk = walkdir::WalkDir::new(root).min_depth(1).into_iter().filter_entry(|e| !HIDDEN.contains(&e.file_name().to_string_lossy().as_ref()));
    for entry in walk.flatten() {
        if paths.len() == MAX_INDEXED {
            return Index { paths, truncated: true };
        }
        let Ok(relative) = entry.path().strip_prefix(root) else { continue };
        let relative = relative.to_string_lossy().replace('\\', "/");
        paths.push(if entry.file_type().is_dir() { format!("{relative}/") } else { relative });
    }
    Index { paths, truncated: false }
}

/// Returns the paths the way you'd paste them: absolute in the platform's own form, or
/// pack-relative with forward slashes as configs and scripts expect.
pub fn path_text(root: &Path, relatives: &[String], absolute: bool) -> Result<String> {
    let lines = relatives
        .iter()
        .map(|relative| {
            let path = resolve_entry(root, relative)?;
            Ok(if !absolute {
                relative.trim_matches('/').to_string()
            } else if cfg!(windows) {
                path.to_string_lossy().replace('/', "\\")
            } else {
                path.to_string_lossy().into_owned()
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(lines.join(if cfg!(windows) { "\r\n" } else { "\n" }))
}

// Like Explorer and Finder, we never overwrite on paste. We name a clashing copy
// "name copy.ext", then "name copy 2.ext", and so on.
pub(crate) fn free_name(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(index) if index > 0 => name.split_at(index),
        _ => (name, ""),
    };
    (1..)
        .map(|n| dir.join(if n == 1 { format!("{stem} copy{ext}") } else { format!("{stem} copy {n}{ext}") }))
        .find(|p| !p.exists())
        .expect("an unused name")
}

fn copy_tree(source: &Path, target: &Path) -> Result<()> {
    if !source.is_dir() {
        std::fs::copy(source, target).with_context(|| format!("copying {}", source.display()))?;
        return Ok(());
    }
    for entry in walkdir::WalkDir::new(source) {
        let entry = entry?;
        let dest = target.join(entry.path().strip_prefix(source)?);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&dest).with_context(|| format!("creating {}", dest.display()))?;
        } else {
            std::fs::copy(entry.path(), &dest).with_context(|| format!("copying {}", entry.path().display()))?;
        }
    }
    Ok(())
}

fn same_entry(a: &Path, b: &Path) -> bool {
    matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b)
}

pub fn rename(root: &Path, from: &str, to: &str) -> Result<()> {
    let source = resolve_entry(root, from)?;
    let target = resolve_entry(root, to)?;
    // On Windows and macOS, a case-only rename finds the source itself at the target path.
    if target.exists() && !same_entry(&source, &target) {
        bail!("{to} already exists");
    }
    std::fs::rename(&source, &target).with_context(|| format!("renaming {from} to {to}"))
}

// Windows rejects these characters, and we keep names portable since teams share a pack across
// machines.
const UNPORTABLE: [char; 9] = ['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

fn check_name(name: &str) -> Result<()> {
    if name.trim().is_empty() || name == "." || name == ".." {
        bail!("names can't be empty");
    }
    if let Some(c) = name.chars().find(|c| UNPORTABLE.contains(c) || c.is_control()) {
        bail!("{name} has a {c}, which Windows doesn't allow in names");
    }
    if name.ends_with(['.', ' ']) {
        bail!("{name} ends with a dot or space, which Windows doesn't allow");
    }
    let device = name.split('.').next().unwrap_or_default().to_ascii_uppercase();
    let numbered = |prefix: &str| device.strip_prefix(prefix).is_some_and(|n| matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"));
    if ["CON", "PRN", "AUX", "NUL"].contains(&device.as_str()) || numbered("COM") || numbered("LPT") {
        bail!("Windows reserves the name {device}");
    }
    // macOS counts the limit in UTF-8 bytes, which is never fewer than Windows' UTF-16 units.
    if name.len() > 255 {
        bail!("{name} is over 255 bytes long");
    }
    Ok(())
}

/// Gives each entry a new name in its own folder and returns the (old, new) path of every entry
/// whose name changed.
pub fn rename_all(root: &Path, renames: &[(String, String)]) -> Result<Vec<(String, String)>> {
    let mut changed = Vec::new();
    for (from, name) in renames {
        check_name(name)?;
        let source = resolve_entry(root, from)?;
        let parent = from.rsplit_once('/').map_or("", |(parent, _)| parent);
        let to = if parent.is_empty() { name.clone() } else { format!("{parent}/{name}") };
        if to != *from {
            changed.push((from.clone(), to.clone(), source.clone(), source.with_file_name(name)));
        }
    }
    let pairs: Vec<(PathBuf, PathBuf)> = changed.iter().map(|(_, _, source, target)| (source.clone(), target.clone())).collect();
    move_all(&pairs)?;
    Ok(changed.into_iter().map(|(from, to, ..)| (from, to)).collect())
}

fn folded(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

/// Moves every pair, or none of them. A target may be another pair's source, as when you
/// renumber 1, 2, 3 to 2, 3, 4 or swap two names.
pub(crate) fn move_all(pairs: &[(PathBuf, PathBuf)]) -> Result<()> {
    let mut targets = HashSet::new();
    for (from, to) in pairs {
        if !from.exists() {
            bail!("{} is no longer there", from.display());
        }
        // Windows and macOS compare names case-insensitively, so two targets that differ only
        // in case would land on the same entry.
        if !targets.insert(folded(to)) {
            bail!("two entries would both be named {}", to.file_name().unwrap_or_default().to_string_lossy());
        }
        if to.exists() && !pairs.iter().any(|(source, _)| same_entry(source, to)) {
            bail!("{} already exists", to.display());
        }
    }
    if !pairs.iter().any(|(_, to)| to.exists()) {
        return move_steps(pairs.to_vec());
    }
    // Some targets are still taken by entries we're about to move, so we move everything to a
    // temporary name first. Case-only renames go this way too, since the target is the source.
    let staged: Vec<PathBuf> = pairs.iter().enumerate().map(|(i, (from, _))| from.with_file_name(format!(".packraven-rename-{}-{i}", std::process::id()))).collect();
    let mut steps: Vec<(PathBuf, PathBuf)> = pairs.iter().zip(&staged).map(|((from, _), temp)| (from.clone(), temp.clone())).collect();
    steps.extend(staged.iter().zip(pairs).map(|(temp, (_, to))| (temp.clone(), to.clone())));
    move_steps(steps)
}

fn move_steps(steps: Vec<(PathBuf, PathBuf)>) -> Result<()> {
    for (done, (from, to)) in steps.iter().enumerate() {
        if let Err(error) = move_entry(from, to) {
            for (from, to) in steps[..done].iter().rev() {
                let _ = move_entry(to, from);
            }
            return Err(error);
        }
    }
    Ok(())
}

/// Moves each entry into `relative_dir` and returns the (old, new) path of every entry that moved.
pub fn move_into(root: &Path, relatives: &[String], relative_dir: &str) -> Result<Vec<(String, String)>> {
    let dir = resolve(root, relative_dir)?;
    let prefix = if relative_dir.is_empty() { String::new() } else { format!("{}/", relative_dir.trim_end_matches('/')) };
    let mut moves = Vec::new();
    // We check every entry before moving any, so a clash can't leave the selection half moved.
    for relative in relatives {
        let source = resolve_entry(root, relative)?;
        let name = source.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if dir.starts_with(&source) {
            bail!("you can't move {name} into itself");
        }
        let target = dir.join(&name);
        if source.parent() == Some(dir.as_path()) {
            continue;
        }
        if target.exists() {
            bail!("{} already has a {name}", if relative_dir.is_empty() { "The pack folder" } else { relative_dir });
        }
        moves.push((relative.clone(), format!("{prefix}{name}"), source, target));
    }
    for (from, to, source, target) in &moves {
        std::fs::rename(source, target).with_context(|| format!("moving {from} to {to}"))?;
    }
    Ok(moves.into_iter().map(|(from, to, ..)| (from, to)).collect())
}

#[derive(Serialize, Default, Debug)]
pub struct Pasted {
    pub created: Vec<String>,
    /// The (old, new) paths of moved entries that were already inside the pack folder.
    pub moved: Vec<(String, String)>,
    /// The (old, new) absolute paths of every moved entry, including ones from outside the pack.
    #[serde(skip)]
    pub transfers: Vec<(PathBuf, PathBuf)>,
    #[serde(skip)]
    pub copies: Vec<PathBuf>,
}

pub(crate) fn move_entry(source: &Path, target: &Path) -> Result<()> {
    match std::fs::rename(source, target) {
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
            copy_tree(source, target)?;
            if source.is_dir() { std::fs::remove_dir_all(source) } else { std::fs::remove_file(source) }
                .with_context(|| format!("removing {} after copying it", source.display()))
        }
        result => result.with_context(|| format!("moving {}", source.display())),
    }
}

pub fn paste(root: &Path, relative_dir: &str, sources: &[PathBuf], cut: bool) -> Result<Pasted> {
    let dir = resolve(root, relative_dir)?;
    let canonical_dir = dir.canonicalize().with_context(|| format!("reading {}", dir.display()))?;
    let canonical_root = root.canonicalize().with_context(|| format!("reading {}", root.display()))?;
    let prefix = if relative_dir.is_empty() { String::new() } else { format!("{}/", relative_dir.trim_end_matches('/')) };
    let mut pasted = Pasted::default();
    for source in sources {
        let Some(name) = source.file_name().map(|n| n.to_string_lossy().into_owned()) else { continue };
        let canonical_source = source.canonicalize().with_context(|| format!("reading {}", source.display()))?;
        if canonical_source.is_dir() && canonical_dir.starts_with(&canonical_source) {
            bail!("you can't paste {name} into itself");
        }
        // Moving an entry into the folder it's already in leaves it where it is, as in Explorer.
        if cut && canonical_source.parent() == Some(canonical_dir.as_path()) {
            continue;
        }
        let target = free_name(&dir, &name);
        let to = format!("{prefix}{}", target.file_name().unwrap_or_default().to_string_lossy());
        if cut {
            move_entry(source, &target)?;
            if let Ok(inside) = canonical_source.strip_prefix(&canonical_root) {
                pasted.moved.push((inside.to_string_lossy().replace('\\', "/"), to.clone()));
            }
            pasted.transfers.push((source.clone(), target));
        } else {
            copy_tree(source, &target)?;
            pasted.copies.push(target);
        }
        pasted.created.push(to);
    }
    Ok(pasted)
}

#[cfg(windows)]
fn same_volume(a: &Path, b: &Path) -> bool {
    let volume = |p: &Path| match p.canonicalize().ok()?.components().next()? {
        Component::Prefix(prefix) => Some(prefix.as_os_str().to_ascii_lowercase()),
        _ => None,
    };
    volume(a).is_some_and(|v| Some(v) == volume(b))
}

#[cfg(unix)]
fn same_volume(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    matches!((a.metadata(), b.metadata()), (Ok(a), Ok(b)) if a.dev() == b.dev())
}

/// Brings files you drop on the window into the pack. Like Explorer and Finder, we move files
/// from the same drive and copy them from another, unless `requested_move` says otherwise.
pub fn import(root: &Path, relative_dir: &str, sources: &[PathBuf], requested_move: Option<bool>) -> Result<Pasted> {
    let cut = requested_move.unwrap_or_else(|| sources.iter().all(|s| same_volume(s, root)));
    paste(root, relative_dir, sources, cut)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_never_overwrites() {
        let dir = std::env::temp_dir().join("packraven-paste-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config/nested")).unwrap();
        std::fs::write(dir.join("config/a.json"), "{}").unwrap();
        std::fs::write(dir.join("config/nested/b.toml"), "").unwrap();
        let sources = [dir.join("config/a.json"), dir.join("config/nested")];
        assert_eq!(paste(&dir, "config", &sources, false).unwrap().created, ["config/a copy.json", "config/nested copy"]);
        assert_eq!(paste(&dir, "config", &sources[..1], false).unwrap().created, ["config/a copy 2.json"]);
        assert!(dir.join("config/nested copy/b.toml").exists());
        assert!(paste(&dir, "config/nested", &sources[1..], false).is_err());
    }

    #[test]
    fn cut_paste_moves_from_inside_and_outside_the_pack() {
        let dir = std::env::temp_dir().join("packraven-cut-test");
        let outside = std::env::temp_dir().join("packraven-cut-test-outside");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::create_dir_all(dir.join("kubejs")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(dir.join("config/a.json"), "").unwrap();
        std::fs::write(outside.join("b.toml"), "").unwrap();
        let pasted = paste(&dir, "kubejs", &[dir.join("config/a.json"), outside.join("b.toml")], true).unwrap();
        assert_eq!(pasted.created, ["kubejs/a.json", "kubejs/b.toml"]);
        assert_eq!(pasted.moved, [("config/a.json".to_string(), "kubejs/a.json".to_string())]);
        assert!(!dir.join("config/a.json").exists() && !outside.join("b.toml").exists());
        let unchanged = paste(&dir, "kubejs", &[dir.join("kubejs/a.json")], true).unwrap();
        assert!(unchanged.created.is_empty() && dir.join("kubejs/a.json").exists());
    }

    #[test]
    fn import_moves_on_the_same_drive_unless_you_ask_to_copy() {
        let dir = std::env::temp_dir().join("packraven-import-test");
        let outside = std::env::temp_dir().join("packraven-import-test-outside");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(dir.join("a.json"), "").unwrap();
        std::fs::write(outside.join("b.toml"), "").unwrap();
        assert_eq!(import(&dir, "config", &[outside.join("b.toml")], Some(false)).unwrap().created, ["config/b.toml"]);
        assert!(outside.join("b.toml").exists(), "a requested copy keeps the original");
        assert_eq!(import(&dir, "config", &[dir.join("a.json")], None).unwrap().moved, [("a.json".to_string(), "config/a.json".to_string())]);
        assert!(!dir.join("a.json").exists(), "the same drive moves by default");
    }

    #[test]
    fn moves_and_renames_without_clobbering() {
        let dir = std::env::temp_dir().join("packraven-move-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config/nested")).unwrap();
        std::fs::create_dir_all(dir.join("kubejs")).unwrap();
        std::fs::write(dir.join("config/a.json"), "").unwrap();
        std::fs::write(dir.join("kubejs/a.json"), "").unwrap();
        assert!(move_into(&dir, &["config".into()], "config/nested").is_err());
        assert!(move_into(&dir, &["config/a.json".into(), "config/nested".into()], "kubejs").is_err());
        assert!(dir.join("config/nested").exists(), "a clash moves nothing");
        let moved = move_into(&dir, &["config/nested".into(), "kubejs/a.json".into()], "kubejs").unwrap();
        assert_eq!(moved, [("config/nested".to_string(), "kubejs/nested".to_string())]);
        assert!(rename(&dir, "config/a.json", "kubejs/a.json").is_err());
        rename(&dir, "config/a.json", "config/A.json").unwrap();
        assert!(dir.join("config").read_dir().unwrap().any(|e| e.unwrap().file_name() == "A.json"));
    }

    #[test]
    fn refuses_to_touch_the_pack_root() {
        let root = Path::new("C:/pack");
        assert!(resolve_entry(root, "").is_err());
        assert!(resolve_entry(root, "/").is_err());
        assert!(trash(root, &[String::new()]).is_err());
        assert!(path_text(root, &[String::new()], true).is_err());
    }

    #[test]
    fn lists_a_large_ignored_folder_without_stalling() {
        let root = std::env::temp_dir().join("packraven-ignored-test");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("cache")).unwrap();
        assert!(crate::git::command().current_dir(&root).arg("init").output().unwrap().status.success());
        std::fs::write(root.join(".gitignore"), "cache/\n").unwrap();
        for i in 0..3000 {
            std::fs::write(root.join(format!("cache/entry-with-a-long-name-{i}.json")), "").unwrap();
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let dir = root.clone();
        std::thread::spawn(move || sender.send(list(&dir, "cache").unwrap()));
        let entries = receiver.recv_timeout(std::time::Duration::from_secs(30)).expect("listing stalled");
        assert_eq!(entries.len(), 3000);
        assert!(entries.iter().all(|e| e.ignored));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn tracking_an_ignored_file_clears_its_ignored_flag() {
        let root = std::env::temp_dir().join("packraven-ignored-tracking-test");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let git = |args: &[&str]| assert!(crate::git::command().current_dir(&root).args(args).output().unwrap().status.success());
        git(&["init"]);
        std::fs::write(root.join(".gitignore"), "*.ini\n").unwrap();
        std::fs::write(root.join("a.ini"), "").unwrap();
        let ignored = |root: &Path| list(root, "").unwrap().into_iter().find(|e| e.name == "a.ini").unwrap().ignored;
        assert!(ignored(&root));
        git(&["add", "-f", "a.ini"]);
        assert!(!ignored(&root));
        git(&["rm", "--cached", "-q", "a.ini"]);
        assert!(ignored(&root));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn asks_before_large_files_and_refuses_huge_ones() {
        let root = std::env::temp_dir().join("packraven-limits-test");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("small.json"), "x".repeat(50)).unwrap();
        std::fs::write(root.join("large.json"), "x".repeat(150)).unwrap();
        std::fs::write(root.join("huge.json"), "x".repeat(250)).unwrap();
        let limits = Limits { ask: 100, max: 200 };

        let small = read(&root, "small.json", limits, false).unwrap();
        assert!(small.text.is_some() && !small.large);

        let asked = read(&root, "large.json", limits, false).unwrap();
        assert!(asked.text.is_none() && asked.large);
        let opened = read(&root, "large.json", limits, true).unwrap();
        assert!(opened.text.is_some() && opened.large);

        let huge = read(&root, "huge.json", limits, true).unwrap();
        assert!(huge.text.is_none() && !huge.large);
    }

    #[test]
    fn reads_gzipped_logs_as_read_only_text() {
        let root = std::env::temp_dir().join("packraven-read-gz-test");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("logs")).unwrap();
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, b"line one\r\nline two\r\n").unwrap();
        std::fs::write(root.join("logs/old.log.gz"), encoder.finish().unwrap()).unwrap();
        std::fs::write(root.join("logs/fake.log.gz"), "not gzip").unwrap();

        let content = read(&root, "logs/old.log.gz", Limits::DEFAULT, false).unwrap();
        assert_eq!(content.text.as_deref(), Some("line one\nline two\n"));
        assert!(content.compressed && content.crlf);
        assert!(read(&root, "logs/fake.log.gz", Limits::DEFAULT, false).unwrap().unreadable.is_some());
    }

    #[test]
    fn indexes_everything_but_git_and_copies_paths_in_both_forms() {
        let root = std::env::temp_dir().join("packraven-index-test");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".git/objects")).unwrap();
        std::fs::create_dir_all(root.join("config/sub")).unwrap();
        std::fs::write(root.join("config/sub/a.toml"), "").unwrap();
        let mut index = index(&root);
        index.paths.sort();
        assert_eq!(index.paths, ["config/", "config/sub/", "config/sub/a.toml"]);

        let paths = ["config/sub/a.toml".to_string(), "config".to_string()];
        let relative = path_text(&root, &paths, false).unwrap();
        assert!(relative.starts_with("config/sub/a.toml") && relative.ends_with("config"));
        let absolute = path_text(&root, &paths[..1], true).unwrap();
        assert!(Path::new(&absolute).is_file());
        if cfg!(windows) {
            assert!(!absolute.contains('/'));
        }
    }

    #[test]
    fn rename_all_handles_chains_swaps_and_case() {
        let dir = std::env::temp_dir().join("packraven-rename-all-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("shots")).unwrap();
        for (name, text) in [("1.png", "one"), ("2.png", "two"), ("a.txt", "a"), ("b.txt", "b"), ("Case.txt", "case")] {
            std::fs::write(dir.join("shots").join(name), text).unwrap();
        }
        let renames = |pairs: &[(&str, &str)]| pairs.iter().map(|(from, to)| (format!("shots/{from}"), to.to_string())).collect::<Vec<_>>();
        let renamed = rename_all(&dir, &renames(&[("1.png", "2.png"), ("2.png", "3.png"), ("a.txt", "b.txt"), ("b.txt", "a.txt"), ("Case.txt", "case.txt")])).unwrap();
        assert_eq!(renamed.len(), 5);
        let read = |name: &str| std::fs::read_to_string(dir.join("shots").join(name)).unwrap();
        assert_eq!((read("2.png").as_str(), read("3.png").as_str()), ("one", "two"));
        assert_eq!((read("a.txt").as_str(), read("b.txt").as_str()), ("b", "a"));
        let names: Vec<String> = std::fs::read_dir(dir.join("shots")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert!(names.contains(&"case.txt".to_string()) && !names.iter().any(|n| n.starts_with(".packraven")));
        assert!(rename_all(&dir, &renames(&[("2.png", "a.txt")])).is_err());
        assert!(rename_all(&dir, &renames(&[("2.png", "x.png"), ("3.png", "X.png")])).is_err());
        assert!(rename_all(&dir, &renames(&[("2.png", "what?.png")])).is_err());
        assert!(rename_all(&dir, &renames(&[("2.png", "nul.png")])).is_err());
        assert_eq!(read("2.png"), "one");
        assert!(rename_all(&dir, &renames(&[("2.png", "2.png")])).unwrap().is_empty());
    }

    #[test]
    fn resolve_rejects_escapes() {
        let root = Path::new("C:/pack");
        assert!(resolve(root, "config/a.toml").is_ok());
        assert!(resolve(root, "../outside.txt").is_err());
        assert!(resolve(root, "config/../../x").is_err());
        if cfg!(windows) {
            assert!(resolve(root, "C:/Windows/win.ini").is_err());
        }
        assert!(resolve(root, "/etc/passwd").is_err());
    }

    #[test]
    fn round_trips_line_endings_and_bom() {
        let dir = std::env::temp_dir().join("packraven-files-test");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.toml"), "\u{feff}a = 1\r\nb = 2\r\n").unwrap();
        let content = read(&dir, "a.toml", Limits::DEFAULT, false).unwrap();
        assert!(content.crlf && content.bom);
        assert_eq!(content.text.as_deref(), Some("a = 1\nb = 2\n"));
        write(&dir, "a.toml", "a = 3\nb = 2\n", content.crlf, content.bom).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.toml")).unwrap(), "\u{feff}a = 3\r\nb = 2\r\n");
    }
}
