use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use walkdir::WalkDir;

pub enum Planned {
    Copy(PathBuf),
    Write(String),
}

/// Maps each file's path relative to the mirrored folder, with forward slashes, to its contents.
pub type FilePlan = BTreeMap<String, Planned>;

#[derive(Default)]
pub struct MirrorReport {
    pub written: usize,
    pub unchanged: usize,
    pub removed: usize,
}

impl fmt::Display for MirrorReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Updated {} files, removed {}, kept {} unchanged", self.written, self.removed, self.unchanged)
    }
}

/// Makes `dest` match `plan`, copying only files whose size or modified time differ from their source.
/// We leave the top-level entries named in `keep` alone, like a test server's world.
pub fn mirror(dest: &Path, plan: &FilePlan, keep: &[&str], recopy_all: bool) -> Result<MirrorReport> {
    let mut report = MirrorReport::default();
    if dest.exists() {
        report.removed = remove_unplanned(dest, plan, keep, recopy_all)?;
    }
    for (path, planned) in plan {
        let target = dest.join(path);
        if !recopy_all && up_to_date(&target, planned) {
            report.unchanged += 1;
            continue;
        }
        fs::create_dir_all(target.parent().context("planned file has no parent")?)?;
        match planned {
            Planned::Copy(source) => copy(source, &target).with_context(|| format!("copying {} to {}", source.display(), target.display()))?,
            Planned::Write(text) => fs::write(&target, text).with_context(|| format!("writing {}", target.display()))?,
        }
        report.written += 1;
    }
    Ok(report)
}

pub fn out_of_date(dest: &Path, plan: &FilePlan) -> bool {
    let stale = plan.iter().any(|(path, planned)| !up_to_date(&dest.join(path), planned));
    let extra = || {
        WalkDir::new(dest).into_iter().flatten().filter(|entry| entry.file_type().is_file()).any(|entry| {
            let relative = entry.path().strip_prefix(dest).map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_default();
            !plan.contains_key(&relative)
        })
    };
    stale || (dest.exists() && extra())
}

fn remove_unplanned(dest: &Path, plan: &FilePlan, keep: &[&str], remove_all: bool) -> Result<usize> {
    let mut removed = 0;
    for top in fs::read_dir(dest)? {
        let top = top?;
        if keep.contains(&top.file_name().to_string_lossy().as_ref()) {
            continue;
        }
        for entry in WalkDir::new(top.path()).contents_first(true) {
            let entry = entry?;
            if entry.file_type().is_dir() {
                // Fails while the folder still holds planned files, which is what we want.
                let _ = fs::remove_dir(entry.path());
                continue;
            }
            let relative = entry.path().strip_prefix(dest)?.to_string_lossy().replace('\\', "/");
            if remove_all || !plan.contains_key(&relative) {
                fs::remove_file(entry.path()).with_context(|| format!("couldn't remove {}", entry.path().display()))?;
                removed += 1;
            }
        }
    }
    Ok(removed)
}

fn up_to_date(target: &Path, planned: &Planned) -> bool {
    match planned {
        Planned::Copy(source) => {
            let (Ok(have), Ok(want)) = (fs::metadata(target), fs::metadata(source)) else { return false };
            match (have.modified(), want.modified()) {
                (Ok(have_time), Ok(want_time)) => have.len() == want.len() && have_time == want_time,
                _ => false,
            }
        }
        Planned::Write(text) => fs::read(target).is_ok_and(|bytes| bytes == text.as_bytes()),
    }
}

fn copy(source: &Path, target: &Path) -> Result<()> {
    fs::copy(source, target)?;
    // Windows' CopyFileEx keeps the source's modified time, but other platforms stamp the copy
    // with the current time, and up_to_date compares it.
    #[cfg(not(windows))]
    fs::File::open(target)?.set_modified(fs::metadata(source)?.modified()?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("packraven-mirror-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn copies_only_changes_and_removes_unplanned_files() {
        let root = scratch("changes");
        let (source, dest) = (root.join("source"), root.join("dest"));
        fs::create_dir_all(source.join("mods")).unwrap();
        fs::write(source.join("mods/a.jar"), "a").unwrap();
        fs::write(source.join("mods/b.jar"), "b").unwrap();
        let mut plan = FilePlan::new();
        plan.insert("mods/a.jar".into(), Planned::Copy(source.join("mods/a.jar")));
        plan.insert("mods/b.jar".into(), Planned::Copy(source.join("mods/b.jar")));
        plan.insert("server.properties".into(), Planned::Write("motd=hi\n".into()));
        fs::create_dir_all(dest.join("world")).unwrap();
        fs::write(dest.join("world/level.dat"), "world").unwrap();

        let first = mirror(&dest, &plan, &["world"], false).unwrap();
        assert_eq!((first.written, first.unchanged, first.removed), (3, 0, 0));

        plan.remove("mods/b.jar");
        fs::create_dir_all(dest.join("config")).unwrap();
        fs::write(dest.join("config/generated.toml"), "x").unwrap();
        let second = mirror(&dest, &plan, &["world"], false).unwrap();
        assert_eq!((second.written, second.unchanged, second.removed), (0, 2, 2));
        assert!(!dest.join("mods/b.jar").exists());
        assert!(!dest.join("config").exists());
        assert!(dest.join("world/level.dat").exists());

        let forced = mirror(&dest, &plan, &["world"], true).unwrap();
        assert_eq!((forced.written, forced.unchanged, forced.removed), (2, 0, 2));
        assert!(dest.join("world/level.dat").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn recopies_a_file_when_its_source_changes() {
        let root = scratch("source-change");
        let (source, dest) = (root.join("a.txt"), root.join("dest"));
        fs::write(&source, "one").unwrap();
        let plan = FilePlan::from([("a.txt".to_string(), Planned::Copy(source.clone()))]);
        mirror(&dest, &plan, &[], false).unwrap();
        fs::write(&source, "three").unwrap();
        let report = mirror(&dest, &plan, &[], false).unwrap();
        assert_eq!(report.written, 1);
        assert_eq!(fs::read_to_string(dest.join("a.txt")).unwrap(), "three");
        let _ = fs::remove_dir_all(&root);
    }
}
