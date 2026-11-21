use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use tokio::process::Command;

use crate::git::{self, Identity};
use crate::jobs::Log;
use crate::modsync::{self, ModConflict, ModCounts, Prefer};
use crate::pack::LOCKFILE;
use crate::remote;

const MAX_DIFF_BYTES: usize = 400_000;

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FileChange {
    pub path: String,
    pub orig_path: Option<String>,
    pub status: char,
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub has_commits: bool,
    pub has_remote: bool,
    pub staged: Vec<FileChange>,
    pub unstaged: Vec<FileChange>,
    pub conflicted: Vec<FileChange>,
    pub tracked_ignored: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Commit {
    pub hash: String,
    pub short: String,
    pub author: String,
    pub when: String,
    pub subject: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diff {
    pub text: String,
    pub binary: bool,
    pub truncated: bool,
}

pub(crate) type IndexStamp = (u64, Option<SystemTime>, Vec<u8>);

struct TrackedIgnoredCache {
    index_stamp: IndexStamp,
    tracked_dirs: Vec<String>,
    rules_hash: [u8; 20],
    files: Vec<String>,
}

struct ExcludesFiles {
    config_hash: [u8; 20],
    paths: Vec<PathBuf>,
}

static TRACKED_IGNORED_CACHE: OnceLock<Mutex<HashMap<PathBuf, TrackedIgnoredCache>>> = OnceLock::new();
static EXCLUDES_FILES: OnceLock<Mutex<HashMap<PathBuf, ExcludesFiles>>> = OnceLock::new();

fn tracked_ignored_cache() -> &'static Mutex<HashMap<PathBuf, TrackedIgnoredCache>> {
    TRACKED_IGNORED_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

// Git rewrites .git/index through a lock file and ends it with a checksum (all zeros under
// index.skipHash), so its length, mtime and last 32 bytes together change on every write.
pub(crate) fn index_stamp(git_dir: &Path) -> Option<IndexStamp> {
    let mut file = std::fs::File::open(git_dir.join("index")).ok()?;
    let meta = file.metadata().ok()?;
    let len = meta.len();
    let tail_len = len.min(32) as usize;
    file.seek(SeekFrom::End(-(tail_len as i64))).ok()?;
    let mut tail = vec![0u8; tail_len];
    file.read_exact(&mut tail).ok()?;
    Some((len, meta.modified().ok(), tail))
}

fn xdg_git_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| dirs::home_dir().map(|home| home.join(".config"))).map(|config| config.join("git"))
}

// We only ask git for core.excludesFile again when a config file that could set it changes.
fn excludes_files(dir: &Path) -> Vec<PathBuf> {
    let mut hasher = Sha1::new();
    let mut configs = vec![dir.join(".git/config")];
    configs.extend(dirs::home_dir().map(|home| home.join(".gitconfig")));
    configs.extend(xdg_git_dir().map(|git| git.join("config")));
    for path in &configs {
        hash_ignore_file(&mut hasher, &path.to_string_lossy(), path);
    }
    let config_hash: [u8; 20] = hasher.finalize().into();
    let cache = EXCLUDES_FILES.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(entry) = cache.lock().unwrap().get(dir)
        && entry.config_hash == config_hash
    {
        return entry.paths.clone();
    }
    let mut paths = Vec::new();
    if let Ok(out) = git::run(dir, &["config", "--path", "--get", "core.excludesfile"]) {
        let trimmed = out.trim();
        if !trimmed.is_empty() {
            paths.push(PathBuf::from(trimmed));
        }
    }
    paths.extend(xdg_git_dir().map(|git| git.join("ignore")));
    cache.lock().unwrap().insert(dir.to_path_buf(), ExcludesFiles { config_hash, paths: paths.clone() });
    paths
}

pub(crate) fn hash_ignore_file(hasher: &mut Sha1, label: &str, path: &Path) {
    hasher.update(label.as_bytes());
    hasher.update([0]);
    match std::fs::read(path) {
        Ok(bytes) => {
            hasher.update([1]);
            hasher.update((bytes.len() as u64).to_le_bytes());
            hasher.update(&bytes);
        }
        Err(_) => hasher.update([0]),
    }
}

/// Hashes the ignore rules that apply across the whole repository: info/exclude, the repository
/// config, and the global excludes file.
pub(crate) fn hash_repo_ignore_rules(hasher: &mut Sha1, dir: &Path) {
    let git_dir = dir.join(".git");
    hash_ignore_file(hasher, "info/exclude", &git_dir.join("info/exclude"));
    hash_ignore_file(hasher, "config", &git_dir.join("config"));
    for path in excludes_files(dir) {
        hash_ignore_file(hasher, &path.to_string_lossy(), &path);
    }
}

fn ignore_rules_hash(dir: &Path, tracked_dirs: &[String]) -> [u8; 20] {
    let mut hasher = Sha1::new();
    hash_repo_ignore_rules(&mut hasher, dir);
    for parent in tracked_dirs {
        let rel = if parent.is_empty() { ".gitignore".to_string() } else { format!("{parent}/.gitignore") };
        hash_ignore_file(&mut hasher, &rel, &dir.join(&rel));
    }
    hasher.finalize().into()
}

// A tracked file can only become ignored through a .gitignore in one of its folders, whether git
// tracks, ignores or doesn't know that .gitignore, so we watch every folder holding a tracked file.
fn tracked_dirs(dir: &Path) -> Result<Vec<String>> {
    let mut dirs = BTreeSet::from([String::new()]);
    for file in listed(dir, &["ls-files", "-z"])? {
        let mut end = file.len();
        while let Some(slash) = file[..end].rfind('/') {
            if !dirs.insert(file[..slash].to_string()) {
                break;
            }
            end = slash;
        }
    }
    Ok(dirs.into_iter().collect())
}

fn compute_tracked_ignored(dir: &Path) -> Result<Vec<String>> {
    Ok(tracked_ignored(dir, &[])?.into_iter().filter(|p| !modsync::is_binary(p)).collect())
}

fn cached_tracked_ignored(dir: &Path, stamp: &IndexStamp) -> Option<Vec<String>> {
    let tracked_dirs = {
        let guard = tracked_ignored_cache().lock().unwrap();
        guard.get(dir).filter(|entry| &entry.index_stamp == stamp)?.tracked_dirs.clone()
    };
    let hash = ignore_rules_hash(dir, &tracked_dirs);
    let guard = tracked_ignored_cache().lock().unwrap();
    guard.get(dir).filter(|entry| &entry.index_stamp == stamp && entry.rules_hash == hash).map(|entry| entry.files.clone())
}

fn refresh_tracked_ignored(dir: &Path, stamp: Option<IndexStamp>) -> Result<Vec<String>> {
    let Some(stamp) = stamp else { return compute_tracked_ignored(dir) };
    let (files, tracked_dirs) = std::thread::scope(|scope| {
        let tracked_dirs = scope.spawn(|| tracked_dirs(dir));
        (compute_tracked_ignored(dir), tracked_dirs.join().unwrap())
    });
    let files = files?;
    // If the index changed while git listed files, the listing may already be stale.
    if let Ok(tracked_dirs) = tracked_dirs
        && index_stamp(&dir.join(".git")).as_ref() == Some(&stamp)
    {
        let rules_hash = ignore_rules_hash(dir, &tracked_dirs);
        tracked_ignored_cache().lock().unwrap().insert(dir.to_path_buf(), TrackedIgnoredCache { index_stamp: stamp, tracked_dirs, rules_hash, files: files.clone() });
    }
    Ok(files)
}

pub fn status(dir: &Path) -> Result<Status> {
    let git_dir = dir.join(".git");
    let stamp = if git_dir.is_dir() { index_stamp(&git_dir) } else { None };

    let (out, remote, tracked_ignored) = std::thread::scope(|scope| {
        let status_run = scope.spawn(|| git::run(dir, &["status", "--porcelain=v2", "--branch", "-z", "--untracked-files=all"]));
        let remote_run = scope.spawn(|| git::run(dir, &["remote"]));
        let tracked_ignored = match stamp.as_ref().and_then(|stamp| cached_tracked_ignored(dir, stamp)) {
            Some(files) => Ok(files),
            None => refresh_tracked_ignored(dir, stamp.clone()),
        };
        (status_run.join().unwrap(), remote_run.join().unwrap(), tracked_ignored)
    });

    let out = out?;
    let mut status = Status { has_remote: !remote?.trim().is_empty(), ..Default::default() };
    let mut records = out.split('\0').filter(|r| !r.is_empty());
    while let Some(record) = records.next() {
        if let Some(header) = record.strip_prefix("# ") {
            let (key, value) = header.split_once(' ').unwrap_or((header, ""));
            match key {
                "branch.oid" => status.has_commits = value != "(initial)",
                "branch.head" => status.branch = Some(value.to_string()).filter(|v| v != "(detached)"),
                "branch.upstream" => status.upstream = Some(value.to_string()),
                "branch.ab" => {
                    for part in value.split_whitespace() {
                        if let Some(n) = part.strip_prefix('+') {
                            status.ahead = n.parse().unwrap_or(0);
                        } else if let Some(n) = part.strip_prefix('-') {
                            status.behind = n.parse().unwrap_or(0);
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        let kind = record.chars().next().unwrap_or(' ');
        match kind {
            '?' => status.unstaged.push(FileChange { path: record[2..].to_string(), orig_path: None, status: '?' }),
            '1' | '2' => {
                let fields: Vec<&str> = record.splitn(if kind == '1' { 9 } else { 10 }, ' ').collect();
                let xy: Vec<char> = fields[1].chars().collect();
                let path = fields.last().copied().unwrap_or_default().to_string();
                let orig_path = if kind == '2' { records.next().map(String::from) } else { None };
                if xy[0] != '.' {
                    status.staged.push(FileChange { path: path.clone(), orig_path: orig_path.clone(), status: xy[0] });
                }
                if xy[1] != '.' {
                    status.unstaged.push(FileChange { path, orig_path, status: xy[1] });
                }
            }
            'u' => {
                let path = record.splitn(11, ' ').last().unwrap_or_default().to_string();
                status.conflicted.push(FileChange { path, orig_path: None, status: 'U' });
            }
            _ => {}
        }
    }

    status.tracked_ignored = tracked_ignored?;
    Ok(status)
}

pub fn diff(dir: &Path, path: &str, staged: bool, untracked: bool) -> Result<Diff> {
    let text = if untracked {
        let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
        git::run_accepting(dir, &["diff", "--no-color", "--no-index", "--", null, path], &[0, 1])?
    } else if staged {
        git::run(dir, &["diff", "--no-color", "--cached", "--", path])?
    } else {
        git::run(dir, &["diff", "--no-color", "--", path])?
    };
    let binary = text.lines().any(|l| l.starts_with("Binary files "));
    let truncated = text.len() > MAX_DIFF_BYTES;
    let text = if truncated { text[..text.floor_char_boundary(MAX_DIFF_BYTES)].to_string() } else { text };
    Ok(Diff { text, binary, truncated })
}

const MAX_BLOB_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum Version {
    Head,
    Index,
}

/// Returns a file's bytes as HEAD or the index has them.
pub fn blob(dir: &Path, path: &str, version: Version) -> Result<Vec<u8>> {
    let object = match version {
        Version::Head => format!("HEAD:{path}"),
        Version::Index => format!(":{path}"),
    };
    let size: u64 = git::run(dir, &["cat-file", "-s", &object])?.trim().parse()?;
    if size > MAX_BLOB_BYTES {
        bail!("{path} is too large to show here");
    }
    git::run_bytes(dir, &["cat-file", "blob", &object])
}

#[derive(Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum LineAction {
    Stage,
    Unstage,
    Discard,
}

/// Stages, unstages or discards the chosen lines of `diff`, which indexes `selected` by line.
/// We apply a patch built from the diff you saw, so if the file changed since, git refuses it
/// instead of applying lines you didn't pick.
pub fn apply_lines(dir: &Path, path: &str, diff: &str, selected: &[usize], action: LineAction) -> Result<()> {
    let Some(patch) = partial_patch(path, diff, &selected.iter().copied().collect(), action != LineAction::Stage) else {
        bail!("Select some added or removed lines first.");
    };
    let args: &[&str] = match action {
        LineAction::Stage => &["apply", "--cached", "--whitespace=nowarn", "-"],
        LineAction::Unstage => &["apply", "--cached", "--reverse", "--whitespace=nowarn", "-"],
        LineAction::Discard => &["apply", "--reverse", "--whitespace=nowarn", "-"],
    };
    git::run_with_input(dir, args, &patch).map(|_| ()).context("The file changed since packraven showed this diff, so it couldn't apply those lines. Pick them again.")
}

struct Hunk {
    old_start: usize,
    new_start: usize,
    body: Vec<String>,
    old_count: usize,
    new_count: usize,
}

fn hunk_starts(header: &str) -> Option<(usize, usize)> {
    let mut ranges = header.strip_prefix("@@ -")?.split_whitespace();
    let start = |range: &str| range.split(',').next()?.parse().ok();
    Some((start(ranges.next()?)?, start(ranges.next()?.strip_prefix('+')?)?))
}

// A count of zero puts a hunk's start on the line before it, so we convert to and from the first
// line the hunk touches when we move a start across sides.
fn first_line(start: usize, count: usize) -> usize {
    if count == 0 { start + 1 } else { start }
}

fn start_for(first: usize, count: usize) -> usize {
    if count == 0 { first.saturating_sub(1) } else { first }
}

fn quote_path(path: &str) -> String {
    let mut quoted = String::from("\"");
    for c in path.chars() {
        match c {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\t' => quoted.push_str("\\t"),
            '\n' => quoted.push_str("\\n"),
            _ => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

/// Builds a patch holding only the selected change lines. Forward patches apply to the diff's old
/// side, so unselected removals stay as context and unselected additions drop out. Reverse patches
/// apply backwards to the new side, so it's the other way around.
fn partial_patch(path: &str, diff: &str, selected: &HashSet<usize>, reverse: bool) -> Option<String> {
    let lines: Vec<&str> = diff.split('\n').collect();
    let mode_of = |prefix: &str| lines.iter().take_while(|l| !l.starts_with("@@")).find_map(|l| l.strip_prefix(prefix)).map(str::to_string);
    let (new_mode, deleted_mode) = (mode_of("new file mode "), mode_of("deleted file mode "));

    let mut hunks = Vec::new();
    let mut current: Option<(Hunk, bool)> = None;
    let mut kept_last = false;
    for (index, line) in lines.iter().enumerate() {
        if line.starts_with("@@") {
            hunks.extend(current.take().filter(|(_, picked)| *picked).map(|(hunk, _)| hunk));
            let (old_start, new_start) = hunk_starts(line)?;
            current = Some((Hunk { old_start, new_start, body: Vec::new(), old_count: 0, new_count: 0 }, false));
            continue;
        }
        let Some((hunk, picked)) = current.as_mut() else { continue };
        let chosen = selected.contains(&index);
        let kept = match line.chars().next() {
            Some('\\') => {
                if kept_last {
                    hunk.body.push(line.to_string());
                }
                continue;
            }
            Some('+') if chosen => Some(('+', true)),
            Some('-') if chosen => Some(('-', true)),
            Some('+') => reverse.then_some((' ', false)),
            Some('-') => (!reverse).then_some((' ', false)),
            Some(' ') => Some((' ', false)),
            _ => None,
        };
        kept_last = kept.is_some();
        if let Some((sign, change)) = kept {
            *picked |= change;
            hunk.body.push(format!("{sign}{}", &line[1..]));
            match sign {
                '+' => hunk.new_count += 1,
                '-' => hunk.old_count += 1,
                _ => {
                    hunk.old_count += 1;
                    hunk.new_count += 1;
                }
            }
        }
    }
    hunks.extend(current.take().filter(|(_, picked)| *picked).map(|(hunk, _)| hunk));
    if hunks.is_empty() {
        return None;
    }

    // The side the patch applies to keeps its line numbers, and we shift the other side by what
    // the hunks before it add or remove.
    let mut shift: isize = 0;
    for hunk in &mut hunks {
        if reverse {
            let first = first_line(hunk.new_start, hunk.new_count) as isize - shift;
            hunk.old_start = start_for(first.max(1) as usize, hunk.old_count);
        } else {
            let first = first_line(hunk.old_start, hunk.old_count) as isize + shift;
            hunk.new_start = start_for(first.max(1) as usize, hunk.new_count);
        }
        shift += hunk.new_count as isize - hunk.old_count as isize;
    }

    let old_empty = hunks.iter().all(|h| h.old_count == 0);
    let new_empty = hunks.iter().all(|h| h.new_count == 0);
    let (a, b) = (quote_path(&format!("a/{path}")), quote_path(&format!("b/{path}")));
    let mut patch = format!("diff --git {a} {b}\n");
    match (&new_mode, &deleted_mode) {
        (Some(mode), _) if old_empty => patch.push_str(&format!("new file mode {mode}\n--- /dev/null\n+++ {b}\n")),
        (_, Some(mode)) if new_empty => patch.push_str(&format!("deleted file mode {mode}\n--- {a}\n+++ /dev/null\n")),
        _ => patch.push_str(&format!("--- {a}\n+++ {b}\n")),
    }
    for hunk in hunks {
        patch.push_str(&format!("@@ -{},{} +{},{} @@\n", hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count));
        for line in hunk.body {
            patch.push_str(&line);
            patch.push('\n');
        }
    }
    Some(patch)
}

pub fn stage(dir: &Path, paths: &[String]) -> Result<()> {
    for chunk in paths.chunks(200) {
        let mut args = vec!["add", "-A", "--"];
        args.extend(chunk.iter().map(String::as_str));
        git::run(dir, &args)?;
    }
    Ok(())
}

pub fn unstage(dir: &Path, paths: &[String]) -> Result<()> {
    let has_head = git::run(dir, &["rev-parse", "-q", "--verify", "HEAD"]).is_ok();
    for chunk in paths.chunks(200) {
        let mut args = if has_head { vec!["restore", "--staged", "--"] } else { vec!["rm", "--cached", "-r", "--quiet", "--"] };
        args.extend(chunk.iter().map(String::as_str));
        git::run(dir, &args)?;
    }
    Ok(())
}

pub fn discard(dir: &Path, tracked: &[String], untracked: &[String]) -> Result<()> {
    for chunk in tracked.chunks(200) {
        let mut args = vec!["restore", "--worktree", "--"];
        args.extend(chunk.iter().map(String::as_str));
        git::run(dir, &args)?;
    }
    for chunk in untracked.chunks(200) {
        let mut args = vec!["clean", "-f", "-q", "--"];
        args.extend(chunk.iter().map(String::as_str));
        git::run(dir, &args)?;
    }
    Ok(())
}

fn escape_glob(text: &str) -> String {
    let mut escaped = String::new();
    for c in text.chars() {
        if matches!(c, '*' | '?' | '[' | '\\' | '!' | '#') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

// Git strips unescaped trailing spaces from .gitignore lines.
fn keep_trailing_space(mut line: String) -> String {
    if line.ends_with(' ') {
        line.insert(line.len() - 1, '\\');
    }
    line
}

fn ignore_pattern(path: &str, is_dir: bool) -> String {
    let pattern = keep_trailing_space(format!("/{}", escape_glob(path)));
    if is_dir { format!("{pattern}/") } else { pattern }
}

#[derive(Serialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct IgnoreOption {
    pub label: String,
    pub patterns: Vec<String>,
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct IgnorePreview {
    pub untracked: Vec<String>,
    pub tracked: Vec<String>,
}

pub fn ignore_options(dir: &Path, paths: &[String]) -> Result<Vec<IgnoreOption>> {
    let dirs = paths.iter().map(|p| Ok(crate::files::resolve(dir, p)?.is_dir())).collect::<Result<Vec<_>>>()?;
    Ok(ignore_options_for(paths, &dirs))
}

// We list the exact match for your selection first, then wider patterns, from one folder's files
// of a type up to that type everywhere.
fn ignore_options_for(paths: &[String], dirs: &[bool]) -> Vec<IgnoreOption> {
    if paths.is_empty() {
        return Vec::new();
    }
    let noun = match (paths.len(), dirs.iter().all(|d| *d), dirs.iter().any(|d| *d)) {
        (1, true, _) => "this folder".to_string(),
        (1, ..) => "this file".to_string(),
        (n, true, _) => format!("these {n} folders"),
        (n, _, true) => format!("these {n} items"),
        (n, ..) => format!("these {n} files"),
    };
    let mut options = vec![IgnoreOption { label: format!("Only {noun}"), patterns: paths.iter().zip(dirs).map(|(p, d)| ignore_pattern(p, *d)).collect() }];
    let parent = |path: &str| path.rsplit_once('/').map_or("", |(dir, _)| dir).to_string();
    let extension = |path: &str| path.rsplit('/').next().and_then(|name| name.rsplit_once('.')).map(|(stem, ext)| (stem, ext.to_string())).filter(|(stem, ext)| !stem.is_empty() && !ext.is_empty()).map(|(_, ext)| ext);
    let files_only = !dirs.iter().any(|d| *d);
    let common_ext = files_only.then(|| extension(&paths[0])).flatten().filter(|ext| paths.iter().all(|p| extension(p).as_deref() == Some(ext)));
    let parents: Vec<String> = paths.iter().map(|p| parent(p)).collect();
    let same_parent = parents.iter().all(|p| *p == parents[0]).then(|| parents[0].clone()).filter(|p| !p.is_empty());
    let ancestor = parents.iter().skip(1).fold(parents[0].clone(), |common, p| common_dir(&common, p));
    let mut push = |label: String, pattern: String| {
        if !options.iter().any(|o| o.patterns == [pattern.as_str()]) {
            options.push(IgnoreOption { label, patterns: vec![pattern] });
        }
    };
    if let Some(ext) = &common_ext {
        match &same_parent {
            Some(dir) => push(format!("All .{ext} files in {dir}/"), keep_trailing_space(format!("/{}/*.{}", escape_glob(dir), escape_glob(ext)))),
            None if !ancestor.is_empty() => push(format!("All .{ext} files under {ancestor}/"), keep_trailing_space(format!("/{}/**/*.{}", escape_glob(&ancestor), escape_glob(ext)))),
            None => {}
        }
    }
    if !ancestor.is_empty() {
        push(format!("Everything in {ancestor}/"), ignore_pattern(&ancestor, true));
    }
    if let Some(ext) = &common_ext {
        push(format!("All .{ext} files anywhere"), keep_trailing_space(format!("*.{}", escape_glob(ext))));
    }
    options
}

fn common_dir(a: &str, b: &str) -> String {
    a.split('/').zip(b.split('/')).take_while(|(x, y)| x == y).map(|(x, _)| x).collect::<Vec<_>>().join("/")
}

fn exclude_args(patterns: &[String]) -> Vec<String> {
    patterns.iter().map(|p| p.trim_start()).filter(|p| !p.trim().is_empty() && !p.starts_with('#')).map(|p| format!("--exclude={p}")).collect()
}

fn listed(dir: &Path, args: &[&str]) -> Result<Vec<String>> {
    Ok(git::run(dir, args)?.split('\0').filter(|p| !p.is_empty()).map(String::from).collect())
}

fn tracked_matching(dir: &Path, excludes: &[String]) -> Result<Vec<String>> {
    let mut args = vec!["ls-files", "-z", "-c", "-i"];
    args.extend(excludes.iter().map(String::as_str));
    listed(dir, &args)
}

// We list the untracked files git shows before and after adding the patterns, so we can leave
// out files your .gitignore already covers.
pub fn ignore_preview(dir: &Path, patterns: &[String]) -> Result<IgnorePreview> {
    let excludes = exclude_args(patterns);
    if excludes.is_empty() {
        return Ok(IgnorePreview::default());
    }
    let before = listed(dir, &["ls-files", "-z", "-o", "--exclude-standard"])?;
    let mut args = vec!["ls-files", "-z", "-o", "--exclude-standard"];
    args.extend(excludes.iter().map(String::as_str));
    let after: HashSet<String> = listed(dir, &args)?.into_iter().collect();
    Ok(IgnorePreview { untracked: before.into_iter().filter(|p| !after.contains(p)).collect(), tracked: tracked_matching(dir, &excludes)? })
}

/// Returns the tracked files the new patterns match, since .gitignore doesn't affect those.
pub fn ignore(dir: &Path, patterns: &[String]) -> Result<Vec<String>> {
    let excludes = exclude_args(patterns);
    if excludes.is_empty() {
        bail!("Enter a pattern to add.");
    }
    let lines: Vec<&str> = patterns.iter().map(|p| p.trim_start()).filter(|p| !p.trim().is_empty()).collect();
    crate::setup::ensure_lines(&dir.join(".gitignore"), &lines)?;
    tracked_matching(dir, &excludes)
}

// Git keeps tracking a file after a .gitignore pattern starts to match it. We leave out the jars
// you commit directly, since mods/*.jar matches those on purpose.
fn tracked_ignored(dir: &Path, paths: &[String]) -> Result<Vec<String>> {
    let mut args = vec!["--literal-pathspecs", "ls-files", "-z", "-c", "-i", "--exclude-standard", "--"];
    args.extend(paths.iter().map(String::as_str));
    listed(dir, &args)
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UntrackPreview {
    pub ignored: Vec<String>,
    pub uncovered: Vec<String>,
}

/// Splits the tracked files under `paths` by whether .gitignore covers them, since an uncovered
/// file would show up again as a new file once you stop tracking it.
pub fn untrack_preview(dir: &Path, paths: &[String]) -> Result<UntrackPreview> {
    if paths.is_empty() {
        return Ok(UntrackPreview::default());
    }
    let mut args = vec!["--literal-pathspecs", "ls-files", "-z", "-c", "--"];
    args.extend(paths.iter().map(String::as_str));
    let ignored: HashSet<String> = tracked_ignored(dir, paths)?.into_iter().collect();
    let (ignored, uncovered) = listed(dir, &args)?.into_iter().partition(|p| ignored.contains(p));
    Ok(UntrackPreview { ignored, uncovered })
}

/// Returns the paths .gitignore still leaves out, since git can't re-include a file whose folder
/// a pattern excludes.
pub fn keep_tracking(dir: &Path, paths: &[String]) -> Result<Vec<String>> {
    let lines: Vec<String> = paths.iter().map(|p| format!("!{}", ignore_pattern(p, false))).collect();
    crate::setup::ensure_lines(&dir.join(".gitignore"), &lines.iter().map(String::as_str).collect::<Vec<_>>())?;
    tracked_ignored(dir, paths)
}

pub fn untrack(dir: &Path, paths: &[String]) -> Result<()> {
    for chunk in paths.chunks(200) {
        let mut args = vec!["rm", "-r", "--cached", "--quiet", "--"];
        args.extend(chunk.iter().map(String::as_str));
        git::run(dir, &args)?;
    }
    Ok(())
}

pub fn recent_commits(dir: &Path, count: usize) -> Vec<Commit> {
    let format = "--format=%H%x1f%h%x1f%an%x1f%ar%x1f%s%x1e";
    let Ok(out) = git::run(dir, &["log", &format!("-n{count}"), format]) else { return Vec::new() };
    out.split('\x1e')
        .filter_map(|record| {
            let mut fields = record.trim_start_matches('\n').split('\x1f');
            Some(Commit {
                hash: fields.next().filter(|h| !h.is_empty())?.to_string(),
                short: fields.next()?.to_string(),
                author: fields.next()?.to_string(),
                when: fields.next()?.to_string(),
                subject: fields.next()?.to_string(),
            })
        })
        .collect()
}

async fn git_logged(dir: &Path, args: &[&str], log: &Log) -> Result<()> {
    log.info(format!("git {}", args.join(" ")));
    let mut command = Command::new(crate::git::program());
    command.current_dir(dir).args(args).env("GIT_TERMINAL_PROMPT", "0");
    crate::process::run_logged(command, log).await
}

pub async fn commit(dir: &Path, message: &str, identity: &Identity, log: &Log) -> Result<String> {
    let message = message.trim();
    if message.is_empty() {
        bail!("Write a commit message first.");
    }
    let mut args = identity.config_args();
    args.extend(["commit", "-m", message].map(String::from));
    git_logged(dir, &args.iter().map(String::as_str).collect::<Vec<_>>(), log).await?;
    Ok(format!("Committed \"{}\"", message.lines().next().unwrap_or(message)))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PullPreview {
    pub incoming: ModCounts,
    pub conflicts: Vec<ModConflict>,
}

// git status reports a file whose size changed as modified without comparing content, so a lockfile
// checked out with CRLF and rewritten with LF would count as changed. git diff compares content.
fn has_lock_changes(dir: &Path) -> bool {
    git::run(dir, &["diff", "--quiet", "HEAD", "--", LOCKFILE]).is_err()
}

pub async fn pull_preview(dir: &Path) -> Result<PullPreview> {
    let fetch_dir = dir.to_path_buf();
    tauri::async_runtime::spawn_blocking(move || remote::run(&fetch_dir, &["fetch"])).await??;
    let base = modsync::lock_at(dir, "HEAD")?;
    let theirs = modsync::lock_at(dir, "@{upstream}")?;
    let mine = modsync::folder_lock(dir).await?;
    let conflicts = if modsync::mod_counts(&base, &mine).is_empty() { Vec::new() } else { modsync::merge_locks(&base, &mine, &theirs, None).err().unwrap_or_default() };
    Ok(PullPreview { incoming: modsync::mod_counts(&base, &theirs), conflicts })
}

fn conflict_message(conflicts: &[ModConflict]) -> String {
    let names: Vec<&str> = conflicts.iter().map(|c| c.name.as_str()).collect();
    format!("You and the remote both changed {}. Choose whose versions to keep, then pull again.", names.join(", "))
}

// git's own merge would treat mods.lock.json as text and stop on any nearby edit, so we merge it by
// mod: we set your mod list aside, pull, then replay your mod changes onto the one you pulled.
pub async fn pull(dir: &Path, prefer: Option<Prefer>, log: &Log) -> Result<String> {
    let _guard = modsync::MOD_LIST_GUARD.lock().await;
    modsync::update_mod_list(dir).await.context("packraven can't pull until the mod list records every mod")?;
    remote::run_logged(dir, &["fetch"], log).await?;
    // Reading the pulled mod list here means a format we can't read stops the pull, not the restore after it.
    modsync::lock_at(dir, "@{upstream}")?;

    let lock_path = dir.join(LOCKFILE);
    let mine_text = has_lock_changes(dir).then(|| std::fs::read_to_string(&lock_path)).transpose()?;
    let merged = match &mine_text {
        Some(text) => {
            let mine = crate::pack::Lockfile::parse(text)?;
            let merged = modsync::merge_locks(&modsync::lock_at(dir, "HEAD")?, &mine, &modsync::lock_at(dir, "@{upstream}")?, prefer)
                .map_err(|conflicts| anyhow!(conflict_message(&conflicts)))?;
            git::run(dir, &["checkout", "HEAD", "--", LOCKFILE])?;
            Some(merged)
        }
        None => None,
    };

    // We restore the mods ourselves once the merged list is in place, so the post-merge hook must not
    // restore the pulled list first and set your new mods aside.
    let no_hooks = PathBuf::from(git::run(dir, &["rev-parse", "--absolute-git-dir"])?.trim()).join("modsync").join("no-hooks");
    std::fs::create_dir_all(&no_hooks)?;
    let hooks_path = format!("core.hooksPath={}", no_hooks.to_string_lossy());
    if let Err(error) = remote::run_logged(dir, &["-c", &hooks_path, "pull", "--ff-only"], log).await {
        if let Some(text) = &mine_text {
            std::fs::write(&lock_path, text)?;
        }
        if remote::is_explained(&error) {
            return Err(error);
        }
        return Err(error.context("git stopped the pull because the branches have diverged or you have conflicting local changes. Commit or discard your changes, then pull again."));
    }
    if let Some(mut merged) = merged {
        modsync::write_lock(dir, &mut merged)?;
        log.info("Replayed your mod changes onto the pulled mod list");
    }
    modsync::restore(dir, log).await?;
    Ok(if mine_text.is_some() { "Pulled and kept your mod changes".into() } else { "Pulled".into() })
}

pub async fn push(dir: &Path, log: &Log) -> Result<String> {
    let has_upstream = git::run(dir, &["rev-parse", "--abbrev-ref", "@{upstream}"]).is_ok();
    if has_upstream {
        remote::run_logged(dir, &["push", "--follow-tags"], log).await?;
    } else {
        remote::run_logged(dir, &["push", "--follow-tags", "-u", "origin", "HEAD"], log).await?;
    }
    Ok("Pushed".into())
}

pub async fn fetch(dir: &Path, log: &Log) -> Result<String> {
    remote::run_logged(dir, &["fetch", "--prune"], log).await?;
    Ok("Fetched".into())
}

#[cfg(test)]
mod tests {
    use super::ignore_pattern;
    use super::*;
    use sha1::{Digest, Sha1};

    const ADDONS: [(&str, u64, u64); 5] = [("base", 1, 10), ("shared-1", 2, 20), ("shared-2", 2, 21), ("theirs", 3, 30), ("mine", 4, 40)];

    fn sha1(content: &str) -> String {
        hex::encode(Sha1::digest(content.as_bytes()))
    }

    fn git(dir: &Path, args: &[&str]) {
        let mut all = vec!["-c", "user.name=Test", "-c", "user.email=test@example.com"];
        all.extend(args);
        git::run(dir, &all).unwrap();
    }

    fn put_mod(dir: &Path, name: &str) {
        std::fs::write(dir.join("mods").join(format!("{name}.jar")), name).unwrap();
    }

    fn cache_mod(dir: &Path, name: &str) {
        let cache = dir.join(".git/modsync/cache");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join(sha1(name)), name).unwrap();
    }

    fn mod_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir.join("mods")).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    fn write_instance(dir: &Path) {
        let addons: Vec<_> = ADDONS
            .iter()
            .map(|(name, project, file)| serde_json::json!({ "addonID": project, "installedFile": { "id": file, "fileName": format!("{name}.jar"), "downloadUrl": "http://127.0.0.1:9/unreachable", "hashes": [{ "type": 1, "value": sha1(name) }] } }))
            .collect();
        let instance = serde_json::json!({ "gameVersion": "1.21.1", "baseModLoader": { "name": "neoforge-21.1.1" }, "installedAddons": addons });
        std::fs::write(dir.join("minecraftinstance.json"), instance.to_string()).unwrap();
    }

    fn commit_mods(dir: &Path, runtime: &tokio::runtime::Runtime, message: &str) {
        runtime.block_on(modsync::update_mod_list(dir)).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-m", message]);
        git(dir, &["push", "-u", "origin", "HEAD"]);
    }

    // Two clones of one pack: "they" update a mod and add one, while "you" add a mod without committing.
    fn diverged_clones(name: &str) -> (PathBuf, PathBuf, tokio::runtime::Runtime) {
        let root = std::env::temp_dir().join(format!("packraven-pull-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        git(&root, &["init", "-q", "--bare", "origin.git"]);
        git(&root, &["clone", "-q", "origin.git", "theirs"]);
        let theirs = root.join("theirs");
        std::fs::create_dir_all(theirs.join("mods")).unwrap();
        std::fs::write(theirs.join(".gitignore"), "mods/*.jar\nminecraftinstance.json\n").unwrap();
        write_instance(&theirs);
        put_mod(&theirs, "base");
        put_mod(&theirs, "shared-1");
        commit_mods(&theirs, &runtime, "Initial");

        git(&root, &["clone", "-q", "origin.git", "mine"]);
        let mine = root.join("mine");
        std::fs::create_dir_all(mine.join("mods")).unwrap();
        write_instance(&mine);
        put_mod(&mine, "base");
        put_mod(&mine, "shared-1");

        std::fs::remove_file(theirs.join("mods/shared-1.jar")).unwrap();
        put_mod(&theirs, "shared-2");
        put_mod(&theirs, "theirs");
        commit_mods(&theirs, &runtime, "Update shared, add theirs");

        // packraven can't download in a test, so we place what the pull needs in the cache.
        cache_mod(&mine, "shared-2");
        cache_mod(&mine, "theirs");
        (theirs, mine, runtime)
    }

    #[test]
    fn pull_keeps_your_uncommitted_mods() {
        let (_theirs, mine, runtime) = diverged_clones("keeps");
        put_mod(&mine, "mine");
        runtime.block_on(modsync::update_mod_list(&mine)).unwrap();

        let message = runtime.block_on(pull(&mine, None, &Log::console("pull"))).unwrap();
        assert_eq!(message, "Pulled and kept your mod changes");
        assert_eq!(mod_names(&mine), ["base.jar", "mine.jar", "shared-2.jar", "theirs.jar"]);
        let lock = crate::pack::Lockfile::parse(&std::fs::read_to_string(mine.join(LOCKFILE)).unwrap()).unwrap();
        let mut paths: Vec<&str> = lock.files.iter().map(|e| e.path.as_str()).collect();
        paths.sort();
        assert_eq!(paths, ["mods/base.jar", "mods/mine.jar", "mods/shared-2.jar", "mods/theirs.jar"]);
        assert_eq!(git::ahead_behind(&mine), Some((0, 0)));
    }

    #[test]
    fn pull_without_local_mod_changes_restores_theirs() {
        let (_theirs, mine, runtime) = diverged_clones("plain");
        let message = runtime.block_on(pull(&mine, None, &Log::console("pull"))).unwrap();
        assert_eq!(message, "Pulled");
        assert_eq!(mod_names(&mine), ["base.jar", "shared-2.jar", "theirs.jar"]);
        assert!(git::changed_files(&mine).is_empty());
    }

    #[test]
    fn pull_stops_on_a_mod_both_sides_changed() {
        let (_theirs, mine, runtime) = diverged_clones("conflict");
        std::fs::remove_file(mine.join("mods/shared-1.jar")).unwrap();
        runtime.block_on(modsync::update_mod_list(&mine)).unwrap();
        let before = std::fs::read_to_string(mine.join(LOCKFILE)).unwrap();

        let error = runtime.block_on(pull(&mine, None, &Log::console("pull"))).unwrap_err();
        assert!(format!("{error:#}").contains("You and the remote both changed shared-1.jar"), "{error:#}");
        assert_eq!(std::fs::read_to_string(mine.join(LOCKFILE)).unwrap(), before);
        assert_eq!(git::ahead_behind(&mine), Some((0, 1)));

        runtime.block_on(pull(&mine, Some(Prefer::Theirs), &Log::console("pull"))).unwrap();
        assert_eq!(mod_names(&mine), ["base.jar", "shared-2.jar", "theirs.jar"]);
    }

    #[test]
    fn pull_refuses_a_mod_list_from_a_newer_packraven() {
        let (theirs, mine, runtime) = diverged_clones("newer");
        std::fs::write(theirs.join(LOCKFILE), format!(r#"{{"formatVersion": {}, "entries": []}}"#, crate::pack::LOCK_FORMAT + 1)).unwrap();
        git(&theirs, &["commit", "-qam", "Newer format"]);
        git(&theirs, &["push", "-q"]);

        let error = runtime.block_on(pull(&mine, None, &Log::console("pull"))).unwrap_err();
        assert!(format!("{error:#}").contains("A newer version of packraven"), "{error:#}");
        assert_eq!(git::ahead_behind(&mine), Some((0, 2)));
        assert_eq!(mod_names(&mine), ["base.jar", "shared-1.jar"]);
    }

    fn option_patterns(paths: &[&str], dirs: &[bool]) -> Vec<(String, Vec<String>)> {
        let paths: Vec<String> = paths.iter().map(|p| p.to_string()).collect();
        ignore_options_for(&paths, dirs).into_iter().map(|o| (o.label, o.patterns)).collect()
    }

    #[test]
    fn suggests_wider_ignore_patterns() {
        assert_eq!(
            option_patterns(&["config/sub/a.json"], &[false]),
            [
                ("Only this file".to_string(), vec!["/config/sub/a.json".to_string()]),
                ("All .json files in config/sub/".to_string(), vec!["/config/sub/*.json".to_string()]),
                ("Everything in config/sub/".to_string(), vec!["/config/sub/".to_string()]),
                ("All .json files anywhere".to_string(), vec!["*.json".to_string()]),
            ]
        );
        let spread = option_patterns(&["config/a/x.toml", "config/b/y.toml"], &[false, false]);
        assert_eq!(spread[0].1, ["/config/a/x.toml", "/config/b/y.toml"]);
        assert_eq!(spread[1], ("All .toml files under config/".to_string(), vec!["/config/**/*.toml".to_string()]));
        assert_eq!(spread[2].1, ["/config/"]);
        let mixed = option_patterns(&["saves", "options.txt"], &[true, false]);
        assert_eq!(mixed, [("Only these 2 items".to_string(), vec!["/saves/".to_string(), "/options.txt".to_string()])]);
        assert_eq!(option_patterns(&[".env"], &[false]).len(), 1);
    }

    #[test]
    fn previews_only_files_the_patterns_newly_ignore() {
        let dir = std::env::temp_dir().join(format!("packraven-ignore-preview-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for file in ["config/a.json", "config/b.json", "logs/x.log", "top.json"] {
            let path = dir.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "x").unwrap();
        }
        std::fs::write(dir.join(".gitignore"), "/logs/\n").unwrap();
        git::run(&dir, &["init", "-q"]).unwrap();
        git::run(&dir, &["add", "config/a.json"]).unwrap();

        let preview = ignore_preview(&dir, &["*.json".into(), "/logs/".into()]).unwrap();
        assert_eq!(preview.untracked, ["config/b.json", "top.json"]);
        assert_eq!(preview.tracked, ["config/a.json"]);
        assert!(ignore_preview(&dir, &["  ".into(), "# note".into()]).unwrap().untracked.is_empty());

        assert_eq!(ignore(&dir, &["/config/".into()]).unwrap(), ["config/a.json"]);
        untrack(&dir, &["config/a.json".into()]).unwrap();
        assert!(git::run(&dir, &["ls-files"]).unwrap().is_empty());
        assert!(dir.join("config/a.json").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn finds_tracked_files_gitignore_covers() {
        let dir = std::env::temp_dir().join(format!("packraven-tracked-ignored-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for file in ["config/jei/a.ini.bak.1", "config/jei/a.ini", "config/[x].toml", "mods/local.jar"] {
            let path = dir.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "x").unwrap();
        }
        git::run(&dir, &["init", "-q"]).unwrap();
        git::run(&dir, &["add", "-A"]).unwrap();
        std::fs::write(dir.join(".gitignore"), "*.bak.*
mods/*.jar
/config/[[]x].toml
").unwrap();

        assert_eq!(status(&dir).unwrap().tracked_ignored, ["config/[x].toml", "config/jei/a.ini.bak.1"]);
        let preview = untrack_preview(&dir, &["config/jei".into()]).unwrap();
        assert_eq!((preview.ignored, preview.uncovered), (vec!["config/jei/a.ini.bak.1".to_string()], vec!["config/jei/a.ini".to_string()]));
        let literal = untrack_preview(&dir, &["config/[x].toml".into()]).unwrap();
        assert_eq!(literal.ignored, ["config/[x].toml"]);

        assert!(keep_tracking(&dir, &["config/[x].toml".into(), "config/jei/a.ini.bak.1".into()]).unwrap().is_empty());
        assert!(status(&dir).unwrap().tracked_ignored.is_empty());
        std::fs::write(dir.join(".gitignore"), "/config/jei/\n").unwrap();
        assert_eq!(keep_tracking(&dir, &["config/jei/a.ini".into()]).unwrap(), ["config/jei/a.ini"]);

        // An untracked nested .gitignore that covers an already-tracked file must invalidate the cache.
        std::fs::write(dir.join(".gitignore"), "").unwrap();
        assert!(status(&dir).unwrap().tracked_ignored.is_empty());
        std::fs::write(dir.join("config/jei/.gitignore"), "*.ini\n").unwrap();
        assert_eq!(status(&dir).unwrap().tracked_ignored, ["config/jei/a.ini"]);
        std::fs::write(dir.join("config/jei/.gitignore"), "").unwrap();
        assert!(status(&dir).unwrap().tracked_ignored.is_empty());

        // Git still reads a nested .gitignore that is itself ignored, though status never lists it.
        std::fs::write(dir.join(".gitignore"), ".gitignore\n!/.gitignore\n").unwrap();
        assert!(status(&dir).unwrap().tracked_ignored.is_empty());
        std::fs::write(dir.join("config/jei/.gitignore"), "*.ini\n").unwrap();
        assert_eq!(status(&dir).unwrap().tracked_ignored, ["config/jei/a.ini"]);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn line_repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("packraven-lines-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        git(&dir, &["config", "core.autocrlf", "false"]);
        let text: String = (1..=20).map(|n| format!("line {n}\n")).collect();
        std::fs::write(dir.join("a b.txt"), text).unwrap();
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-q", "-m", "Initial"]);
        dir
    }

    fn picked(diff: &str, wanted: &[&str]) -> Vec<usize> {
        diff.split('\n').enumerate().filter(|(_, l)| wanted.contains(l)).map(|(i, _)| i).collect()
    }

    fn worktree_diff(dir: &Path, path: &str, staged: bool) -> String {
        diff(dir, path, staged, false).unwrap().text
    }

    #[test]
    fn stages_unstages_and_discards_single_lines() {
        let dir = line_repo("modify");
        let edited: String = (1..=20).map(|n| if n == 2 || n == 15 { format!("changed {n}\n") } else { format!("line {n}\n") }).collect();
        std::fs::write(dir.join("a b.txt"), format!("{edited}added\n")).unwrap();

        let unstaged = worktree_diff(&dir, "a b.txt", false);
        apply_lines(&dir, "a b.txt", &unstaged, &picked(&unstaged, &["-line 15", "+changed 15"]), LineAction::Stage).unwrap();
        let staged = worktree_diff(&dir, "a b.txt", true);
        assert!(staged.contains("+changed 15") && !staged.contains("changed 2") && !staged.contains("+added"), "{staged}");

        apply_lines(&dir, "a b.txt", &staged, &picked(&staged, &["+changed 15"]), LineAction::Unstage).unwrap();
        let index = String::from_utf8(blob(&dir, "a b.txt", Version::Index).unwrap()).unwrap();
        assert!(!index.contains(" 15\n") && index.contains("line 14\nline 16\n"), "{index}");

        let unstaged = worktree_diff(&dir, "a b.txt", false);
        apply_lines(&dir, "a b.txt", &unstaged, &picked(&unstaged, &["-line 2", "+changed 2", "+added"]), LineAction::Discard).unwrap();
        let worktree = std::fs::read_to_string(dir.join("a b.txt")).unwrap();
        assert!(worktree.contains("line 2\n") && worktree.contains("changed 15\n") && !worktree.contains("added"), "{worktree}");

        let stale = apply_lines(&dir, "a b.txt", &unstaged, &picked(&unstaged, &["+added"]), LineAction::Discard);
        assert!(stale.is_err());
        assert!(apply_lines(&dir, "a b.txt", &unstaged, &[0], LineAction::Stage).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stages_part_of_new_and_deleted_files() {
        let dir = line_repo("whole");
        std::fs::write(dir.join("new.txt"), "one\ntwo\nthree\n").unwrap();
        let untracked = diff(&dir, "new.txt", false, true).unwrap().text;
        apply_lines(&dir, "new.txt", &untracked, &picked(&untracked, &["+one", "+three"]), LineAction::Stage).unwrap();
        assert_eq!(String::from_utf8(blob(&dir, "new.txt", Version::Index).unwrap()).unwrap(), "one\nthree\n");

        let staged = worktree_diff(&dir, "new.txt", true);
        apply_lines(&dir, "new.txt", &staged, &picked(&staged, &["+one", "+three"]), LineAction::Unstage).unwrap();
        assert!(!git::run(&dir, &["ls-files", "--", "new.txt"]).unwrap().contains("new.txt"));

        std::fs::remove_file(dir.join("a b.txt")).unwrap();
        let deleted = worktree_diff(&dir, "a b.txt", false);
        apply_lines(&dir, "a b.txt", &deleted, &picked(&deleted, &["-line 1"]), LineAction::Stage).unwrap();
        let index = String::from_utf8(blob(&dir, "a b.txt", Version::Index).unwrap()).unwrap();
        assert!(index.starts_with("line 2\n") && index.ends_with("line 20\n"), "{index}");
        assert_eq!(blob(&dir, "a b.txt", Version::Head).unwrap().len(), (1..=20).map(|n| format!("line {n}\n").len()).sum::<usize>());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn anchors_and_escapes_ignore_patterns() {
        assert_eq!(ignore_pattern("config/a.json", false), "/config/a.json");
        assert_eq!(ignore_pattern("saves", true), "/saves/");
        assert_eq!(ignore_pattern("#notes [old]!.txt", false), r"/\#notes \[old]\!.txt");
        assert_eq!(ignore_pattern("trailing ", false), r"/trailing\ ");
    }
}
