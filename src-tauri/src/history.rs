use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Serialize;
use trash::TrashItem;

const MAX_ENTRIES: usize = 50;

/// A file operation we can apply to the disk. Each history entry holds the action that
/// reverses what you did, and applying one yields the action that reverses it in turn.
pub enum Action {
    Move(Vec<(PathBuf, PathBuf)>),
    Trash(Vec<PathBuf>),
    Restore(Vec<TrashItem>),
    Rewrite(Vec<Rewrite>),
}

/// Puts back a file's earlier contents, as long as it still holds what we last wrote to it.
pub struct Rewrite {
    pub path: PathBuf,
    pub expected: Vec<u8>,
    pub restore: Vec<u8>,
}

struct Entry {
    label: String,
    action: Action,
}

#[derive(Default)]
pub struct History {
    root: PathBuf,
    undo: Vec<Entry>,
    redo: Vec<Entry>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryView {
    undo: Option<String>,
    redo: Option<String>,
}

/// What an undo or redo changed inside the pack, as pack-relative paths.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    label: String,
    moved: Vec<(String, String)>,
    removed: Vec<String>,
    added: Vec<String>,
    changed: Vec<String>,
}

impl Outcome {
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.moved
            .iter()
            .flat_map(|(from, to)| [from.as_str(), to.as_str()])
            .chain(self.removed.iter().map(String::as_str))
            .chain(self.added.iter().map(String::as_str))
            .chain(self.changed.iter().map(String::as_str))
    }
}

#[derive(Default)]
struct Changes {
    moved: Vec<(PathBuf, PathBuf)>,
    removed: Vec<PathBuf>,
    added: Vec<PathBuf>,
    changed: Vec<PathBuf>,
}

impl History {
    // Each pack has its own history, so switching packs starts a fresh one.
    fn use_root(&mut self, root: &Path) {
        if self.root != root {
            *self = History { root: root.to_path_buf(), ..Default::default() };
        }
    }

    /// Records an operation you just did. An operation we can't reverse still clears redo,
    /// since redoing older steps on top of it would act on files that moved on.
    pub fn record(&mut self, root: &Path, label: String, undo: Option<Action>) {
        self.use_root(root);
        self.redo.clear();
        if let Some(action) = undo {
            self.undo.push(Entry { label, action });
            if self.undo.len() > MAX_ENTRIES {
                self.undo.remove(0);
            }
        }
    }

    /// Undoes or redoes the latest step. A step that fails leaves the history, as in Explorer,
    /// because the files it refers to have usually changed since.
    pub fn step(&mut self, root: &Path, redo: bool) -> Result<Option<Outcome>> {
        self.use_root(root);
        let (from, to) = if redo { (&mut self.redo, &mut self.undo) } else { (&mut self.undo, &mut self.redo) };
        let Some(entry) = from.pop() else { return Ok(None) };
        let (inverse, changes) = apply(entry.action)?;
        if let Some(action) = inverse {
            to.push(Entry { label: entry.label.clone(), action });
        }
        Ok(Some(Outcome::new(root, entry.label, changes)))
    }

    pub fn view(&mut self, root: &Path) -> HistoryView {
        self.use_root(root);
        HistoryView { undo: self.undo.last().map(|e| e.label.clone()), redo: self.redo.last().map(|e| e.label.clone()) }
    }
}

impl Outcome {
    fn new(root: &Path, label: String, changes: Changes) -> Self {
        let relative = |path: &Path| path.strip_prefix(root).ok().map(|p| p.to_string_lossy().replace('\\', "/")).filter(|p| !p.is_empty());
        let mut outcome = Outcome {
            label,
            moved: Vec::new(),
            removed: changes.removed.iter().filter_map(|p| relative(p)).collect(),
            added: changes.added.iter().filter_map(|p| relative(p)).collect(),
            changed: changes.changed.iter().filter_map(|p| relative(p)).collect(),
        };
        for (from, to) in &changes.moved {
            match (relative(from), relative(to)) {
                (Some(from), Some(to)) => outcome.moved.push((from, to)),
                (None, Some(to)) => outcome.added.push(to),
                (Some(from), None) => outcome.removed.push(from),
                (None, None) => {}
            }
        }
        outcome
    }
}

fn apply(action: Action) -> Result<(Option<Action>, Changes)> {
    match action {
        Action::Move(pairs) => {
            crate::files::move_all(&pairs)?;
            let inverse = pairs.iter().map(|(from, to)| (to.clone(), from.clone())).collect();
            Ok((Some(Action::Move(inverse)), Changes { moved: pairs, ..Default::default() }))
        }
        Action::Trash(paths) => {
            if let Some(missing) = paths.iter().find(|p| !p.exists()) {
                bail!("{} is no longer there", missing.display());
            }
            let trashed = trash(&paths)?;
            Ok((trashed.map(Action::Restore), Changes { removed: paths, ..Default::default() }))
        }
        Action::Restore(items) => {
            let paths: Vec<PathBuf> = items.iter().map(|i| i.original_parent.join(&i.name)).collect();
            if let Some(taken) = paths.iter().find(|p| p.exists()) {
                bail!("{} already exists, so we can't restore it from the trash", taken.display());
            }
            restore(items)?;
            Ok((Some(Action::Trash(paths.clone())), Changes { added: paths, ..Default::default() }))
        }
        Action::Rewrite(rewrites) => {
            // An edit since the replace would be lost, so we check every file before writing any.
            for rewrite in &rewrites {
                if std::fs::read(&rewrite.path).ok().as_ref() != Some(&rewrite.expected) {
                    bail!("{} changed since, so we can't undo this without losing that change", rewrite.path.display());
                }
            }
            for rewrite in &rewrites {
                crate::files::replace_contents(&rewrite.path, &rewrite.restore)?;
            }
            let changed = rewrites.iter().map(|r| r.path.clone()).collect();
            let inverse = rewrites.into_iter().map(|r| Rewrite { path: r.path, expected: r.restore, restore: r.expected }).collect();
            Ok((Some(Action::Rewrite(inverse)), Changes { changed, ..Default::default() }))
        }
    }
}

/// Moves the paths to the OS trash and returns their trash entries, or None when the OS
/// doesn't let us restore from the trash.
pub fn trash(paths: &[PathBuf]) -> Result<Option<Vec<TrashItem>>> {
    let originals: Vec<String> = paths.iter().map(|p| comparable(p)).collect();
    trash::delete_all(paths).context("moving files to the trash")?;
    Ok(find_trashed(&originals))
}

// The trash records where each item came from, so we find ours by original path and take
// the newest match in case you trashed something with the same path before.
#[cfg(any(windows, all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android"))))]
fn find_trashed(originals: &[String]) -> Option<Vec<TrashItem>> {
    let items = trash::os_limited::list().ok()?;
    originals
        .iter()
        .map(|original| {
            items.iter().filter(|i| comparable(&i.original_parent.join(&i.name)) == *original).max_by_key(|i| i.time_deleted).cloned()
        })
        .collect()
}

#[cfg(any(windows, all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android"))))]
fn restore(items: Vec<TrashItem>) -> Result<()> {
    trash::os_limited::restore_all(items).context("restoring from the trash")
}

#[cfg(not(any(windows, all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))))]
fn find_trashed(_: &[String]) -> Option<Vec<TrashItem>> {
    None
}

#[cfg(not(any(windows, all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))))]
fn restore(_: Vec<TrashItem>) -> Result<()> {
    bail!("packraven can't restore from the Trash here. Use Put Back in Finder instead.")
}

// Windows paths compare case-insensitively and may use either separator or the \\?\ prefix.
fn comparable(path: &Path) -> String {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf()).to_string_lossy().into_owned();
    if cfg!(windows) {
        let path = path.replace('/', "\\");
        path.strip_prefix(r"\\?\").unwrap_or(&path).to_lowercase()
    } else {
        path
    }
}

pub fn describe(paths: &[impl AsRef<Path>]) -> String {
    match paths {
        [one] => one.as_ref().file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        many => format!("{} items", many.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_and_redo_a_move() {
        let dir = std::env::temp_dir().join("packraven-history-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::write(dir.join("a.json"), "").unwrap();
        crate::files::move_into(&dir, &["a.json".into()], "config").unwrap();
        let mut history = History::default();
        history.record(&dir, "Move a.json".into(), Some(Action::Move(vec![(dir.join("config/a.json"), dir.join("a.json"))])));

        let undone = history.step(&dir, false).unwrap().unwrap();
        assert_eq!(undone.moved, [("config/a.json".to_string(), "a.json".to_string())]);
        assert!(dir.join("a.json").exists());
        assert_eq!(history.view(&dir).redo.as_deref(), Some("Move a.json"));

        let redone = history.step(&dir, true).unwrap().unwrap();
        assert_eq!(redone.moved, [("a.json".to_string(), "config/a.json".to_string())]);
        assert!(dir.join("config/a.json").exists());
        assert!(history.view(&dir).redo.is_none());
    }

    // Touches the real OS trash, so you run it on purpose: cargo test --lib -- --ignored trash_round_trip
    #[test]
    #[ignore]
    fn trash_round_trip() {
        let dir = std::env::temp_dir().join("packraven-trash-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("undo me.json");
        std::fs::write(&file, "{}").unwrap();
        // The forward-slash form matches how settings can store the pack folder.
        let stored = PathBuf::from(file.to_string_lossy().replace('\\', "/"));
        let (inverse, _) = apply(Action::Trash(vec![stored.clone()])).unwrap();
        assert!(!file.exists());
        let Some(Action::Restore(items)) = inverse else { panic!("we should find the item in the trash") };
        assert_eq!(items.len(), 1);
        let (inverse, changes) = apply(Action::Restore(items)).unwrap();
        assert!(file.exists());
        assert_eq!(changes.added.len(), 1);
        assert!(matches!(inverse, Some(Action::Trash(_))));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn undoing_a_rewrite_refuses_to_lose_a_later_edit() {
        let dir = std::env::temp_dir().join("packraven-rewrite-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.toml");
        std::fs::write(&file, "new").unwrap();
        let undo = || Action::Rewrite(vec![Rewrite { path: file.clone(), expected: b"new".to_vec(), restore: b"old".to_vec() }]);

        let (inverse, changes) = apply(undo()).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"old");
        assert_eq!(changes.changed, [file.clone()]);
        assert!(matches!(inverse, Some(Action::Rewrite(ref r)) if r[0].expected == b"old"));

        std::fs::write(&file, "edited").unwrap();
        assert!(apply(undo()).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"edited");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_new_operation_clears_redo_and_a_new_pack_clears_everything() {
        let mut history = History::default();
        let pack = Path::new("C:/pack");
        history.record(pack, "Rename a".into(), Some(Action::Move(Vec::new())));
        history.step(pack, false).unwrap();
        history.record(pack, "Delete b".into(), None);
        assert!(history.view(pack).redo.is_none());
        history.record(pack, "Rename c".into(), Some(Action::Move(Vec::new())));
        assert!(history.view(Path::new("C:/other")).undo.is_none());
    }
}
