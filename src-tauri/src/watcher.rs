use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

const IGNORED_ROOTS: [&str; 14] = [".git", "saves", "logs", "crash-reports", "screenshots", "local", "backups", "simplebackups", "cache", "debug", ".mixin.out", "downloads", ".probe", ".rhino_cache"];
const IGNORED_PREFIXES: [&str; 5] = ["journeymap/data/", "config/spark/", "kubejs/probe/", "kubejs/local/", "kubejs/exported/"];
// Commits, checkouts, fetches and tags from outside packraven only touch .git, and these are the
// files they rewrite. We leave out .git/index, which every staging and status call can rewrite.
const GIT_REF_FILES: [&str; 3] = [".git/HEAD", ".git/packed-refs", ".git/FETCH_HEAD"];
const GIT_REF_PREFIX: &str = ".git/refs/";
const BATCH_WINDOW: Duration = Duration::from_millis(250);
const MAX_BATCH: Duration = Duration::from_secs(1);

pub struct PackWatcher {
    _watcher: RecommendedWatcher,
}

#[derive(Serialize, Clone)]
struct FsChange {
    paths: Vec<String>,
}

#[derive(Serialize, Clone)]
struct GitChange;

enum Change {
    Path(String),
    GitRef,
    // notify lost track of what changed, so we treat everything as changed.
    Rescan,
}

fn relevant(relative: &str) -> bool {
    let first = relative.split('/').next().unwrap_or_default();
    !IGNORED_ROOTS.contains(&first) && !IGNORED_PREFIXES.iter().any(|p| relative.starts_with(p)) && !relative.ends_with(".packraven-tmp")
}

fn git_ref(relative: &str) -> bool {
    GIT_REF_FILES.contains(&relative) || (relative.starts_with(GIT_REF_PREFIX) && !relative.ends_with(".lock"))
}

fn relative(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?.to_string_lossy().replace('\\', "/");
    (!relative.is_empty()).then_some(relative)
}

pub fn watch(app: AppHandle, root: PathBuf) -> Result<PackWatcher> {
    let (sender, receiver) = mpsc::channel::<Change>();
    let event_root = root.clone();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else {
            let _ = sender.send(Change::Rescan);
            return;
        };
        if event.need_rescan() {
            let _ = sender.send(Change::Rescan);
        }
        for path in event.paths {
            let Some(relative) = relative(&event_root, &path) else { continue };
            if git_ref(&relative) {
                let _ = sender.send(Change::GitRef);
            } else if relevant(&relative) {
                let _ = sender.send(Change::Path(relative));
            }
        }
    })?;
    watcher.watch(&root, RecursiveMode::Recursive)?;
    let mod_list = crate::mod_list::spawn_updater(app.clone(), root);
    std::thread::spawn(move || {
        while let Ok(first) = receiver.recv() {
            let started = Instant::now();
            let mut batch = vec![first];
            while started.elapsed() < MAX_BATCH
                && let Ok(change) = receiver.recv_timeout(BATCH_WINDOW)
            {
                batch.push(change);
            }
            let rescan = batch.iter().any(|c| matches!(c, Change::Rescan));
            let git = rescan || batch.iter().any(|c| matches!(c, Change::GitRef));
            let mut paths: BTreeSet<String> = batch.into_iter().filter_map(|c| if let Change::Path(path) = c { Some(path) } else { None }).collect();
            if rescan || paths.iter().any(|path| crate::modsync::affects_mod_list(path)) {
                let _ = mod_list.send(());
            }
            if rescan {
                // The root is the one folder the file tree always has loaded.
                paths.insert(String::new());
            }
            if !paths.is_empty() {
                let _ = app.emit("fs-change", FsChange { paths: paths.into_iter().collect() });
            }
            if git {
                let _ = app.emit("git-change", GitChange);
            }
        }
    });
    Ok(PackWatcher { _watcher: watcher })
}

#[cfg(test)]
mod tests {
    use super::{git_ref, relevant};

    #[test]
    fn skips_noisy_folders() {
        assert!(relevant("config/fml.toml"));
        assert!(relevant("kubejs/server_scripts/main.js"));
        assert!(!relevant(".probe/generated/items.d.ts"));
        assert!(!relevant(".rhino_cache/script.bin"));
        assert!(!relevant("kubejs/probe/generated/items.d.ts"));
        assert!(!relevant("kubejs/local/cache.json"));
        assert!(!relevant("kubejs/exported/recipes.json"));
        assert!(!relevant("saves/New World/level.dat"));
        assert!(!relevant(".git/index"));
        assert!(!relevant("logs/latest.log"));
        assert!(!relevant("journeymap/data/sp/x"));
        assert!(relevant("journeymap/config/6.0/journeymap.core.config"));
        assert!(!relevant("config/a.toml.packraven-tmp"));
    }

    #[test]
    fn reports_ref_changes_from_outside_packraven() {
        assert!(git_ref(".git/HEAD"));
        assert!(git_ref(".git/refs/heads/main"));
        assert!(git_ref(".git/refs/remotes/origin/main"));
        assert!(git_ref(".git/refs/tags/v1.0.0"));
        assert!(git_ref(".git/FETCH_HEAD"));
        assert!(!git_ref(".git/refs/heads/main.lock"));
        assert!(!git_ref(".git/index"));
        assert!(!git_ref(".git/objects/ab/cdef"));
        assert!(!git_ref(".git/modsync/hashes.json"));
    }
}
