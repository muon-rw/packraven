use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::git;
use crate::instance::Instance;
use crate::jobs::Log;
use crate::modsync;
use crate::pack::{PackConfig, CONFIG_FILE, LOCKFILE};

pub const MODSYNC_COMMAND: &str = "modsync";
pub const IRIS_CLEAN_COMMAND: &str = "iris-clean";
const PACKRAVEN_PATH_KEY: &str = "packraven.path";
const HOOKS: [(&str, &str); 5] = [
    ("run.sh", include_str!("../resources/githooks/run.sh")),
    ("pre-commit", include_str!("../resources/githooks/pre-commit")),
    ("post-merge", include_str!("../resources/githooks/post-merge")),
    ("post-checkout", include_str!("../resources/githooks/post-checkout")),
    ("post-rewrite", include_str!("../resources/githooks/post-rewrite")),
];
const REMOVED_HOOKS: [&str; 2] = ["modsync.py", "iris_clean.py"];
const GITIGNORE_TEMPLATE: &str = include_str!("../resources/pack.gitignore");
const GITATTRIBUTES: [&str; 3] = [".githooks/* text eol=lf", "mods.lock.json text eol=lf", "config/iris.properties filter=iris"];
const BINARY_PATTERNS: [&str; 9] = [
    "mods/*.jar",
    "mods/*.disabled",
    "resourcepacks/*.zip",
    "resourcepacks/*.disabled",
    "shaderpacks/*.zip",
    "shaderpacks/*.disabled",
    "datapacks/*.zip",
    "datapacks/*.disabled",
    "config/spark",
];

#[derive(Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HooksState {
    Missing,
    Outdated,
    Current,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackSummary {
    pub dir: PathBuf,
    pub name: String,
    pub minecraft: Option<String>,
    pub loader: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceInfo {
    pub dir: PathBuf,
    pub name: String,
    pub minecraft: Option<String>,
    pub loader: Option<String>,
    pub addon_count: usize,
    pub has_git: bool,
    pub has_lock: bool,
    pub missing_mods: usize,
    pub hooks: HooksState,
    pub tracked_binaries: Vec<TrackedBinary>,
    pub remote: Option<String>,
    pub added: bool,
}

#[derive(Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BinaryKind {
    Curseforge,
    Local,
    Generated,
}

#[derive(Serialize)]
pub struct TrackedBinary {
    pub path: String,
    pub kind: BinaryKind,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupOptions {
    pub remote: Option<String>,
    pub author: String,
    pub project_id: u64,
    pub untrack_binaries: bool,
    pub commit: Option<FirstCommit>,
}

#[derive(Deserialize)]
pub struct FirstCommit {
    pub message: String,
    pub author: Option<String>,
}

pub fn hooks_state(dir: &Path) -> HooksState {
    let hooks = dir.join(".githooks");
    let mut any = false;
    let mut all_current = true;
    for (name, content) in HOOKS {
        match std::fs::read_to_string(hooks.join(name)) {
            Ok(existing) => {
                any = true;
                all_current &= existing.replace("\r\n", "\n") == content.replace("\r\n", "\n");
            }
            Err(_) => all_current = false,
        }
    }
    all_current &= !REMOVED_HOOKS.iter().any(|name| hooks.join(name).exists());
    match (any, all_current) {
        (false, _) => HooksState::Missing,
        (true, false) => HooksState::Outdated,
        (true, true) => HooksState::Current,
    }
}

fn tracked_binaries(dir: &Path, instance: &Instance) -> Vec<TrackedBinary> {
    let Ok(out) = git::run(dir, &[&["ls-files", "--"][..], &BINARY_PATTERNS].concat()) else { return Vec::new() };
    out.lines()
        .map(|path| {
            let name = path.rsplit('/').next().unwrap_or(path);
            let kind = if path.starts_with("config/spark") {
                BinaryKind::Generated
            } else if instance.installed_names.contains(name) {
                BinaryKind::Curseforge
            } else {
                BinaryKind::Local
            };
            TrackedBinary { path: path.to_string(), kind }
        })
        .collect()
}

pub fn summarize(dir: &Path) -> Result<PackSummary> {
    let instance = Instance::read_shared(dir)?;
    Ok(PackSummary {
        dir: dir.to_path_buf(),
        name: instance.name.clone().unwrap_or_else(|| dir.file_name().unwrap_or_default().to_string_lossy().into_owned()),
        minecraft: instance.minecraft.clone(),
        loader: instance.loader.as_ref().map(ToString::to_string),
    })
}

pub fn inspect(dir: &Path, added: &[PathBuf]) -> Result<InstanceInfo> {
    let instance = Instance::read_shared(dir)?;
    let has_git = dir.join(".git").exists();
    Ok(InstanceInfo {
        dir: dir.to_path_buf(),
        name: instance.name.clone().unwrap_or_else(|| dir.file_name().unwrap_or_default().to_string_lossy().into_owned()),
        minecraft: instance.minecraft.clone(),
        loader: instance.loader.as_ref().map(ToString::to_string),
        addon_count: instance.addon_count,
        has_git,
        has_lock: dir.join(LOCKFILE).exists(),
        missing_mods: modsync::missing_count(dir),
        hooks: hooks_state(dir),
        tracked_binaries: if has_git { tracked_binaries(dir, &instance) } else { Vec::new() },
        remote: if has_git { git::run(dir, &["remote", "get-url", "origin"]).ok().map(|s| s.trim().to_string()) } else { None },
        added: added.iter().any(|p| same_dir(p, dir)),
    })
}

pub fn same_dir(a: &Path, b: &Path) -> bool {
    let normalize = |p: &Path| p.to_string_lossy().trim_end_matches(['\\', '/']).replace('\\', "/").to_lowercase();
    normalize(a) == normalize(b)
}

pub fn discover(root: &Path, added: &[PathBuf]) -> Vec<InstanceInfo> {
    let Ok(entries) = std::fs::read_dir(root) else { return Vec::new() };
    let mut found: Vec<InstanceInfo> = entries.flatten().filter(|e| e.path().is_dir()).filter_map(|e| inspect(&e.path(), added).ok()).collect();
    found.sort_by_key(|i| i.name.to_lowercase());
    found
}

pub(crate) fn ensure_lines(path: &Path, lines: &[&str]) -> Result<bool> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let present: Vec<&str> = existing.lines().map(str::trim).collect();
    let missing: Vec<&&str> = lines.iter().filter(|l| !present.contains(l)).collect();
    if missing.is_empty() {
        return Ok(false);
    }
    let mut text = existing;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    for line in missing {
        text.push_str(line);
        text.push('\n');
    }
    std::fs::write(path, text)?;
    Ok(true)
}

pub fn install_hooks(dir: &Path) -> Result<()> {
    let hooks = dir.join(".githooks");
    std::fs::create_dir_all(&hooks)?;
    for (name, content) in HOOKS {
        std::fs::write(hooks.join(name), content.replace("\r\n", "\n"))?;
    }
    for name in REMOVED_HOOKS {
        let _ = std::fs::remove_file(hooks.join(name));
    }
    link_hooks(dir)
}

/// Points this clone's hooks and Iris filter at the running packraven, whose path differs per machine and install.
pub fn link_hooks(dir: &Path) -> Result<()> {
    let exe = std::env::current_exe()?.to_string_lossy().replace('\\', "/");
    let iris_filter = format!("\"{exe}\" {IRIS_CLEAN_COMMAND}");
    for (key, value) in [(PACKRAVEN_PATH_KEY, exe.as_str()), ("filter.iris.clean", iris_filter.as_str())] {
        if git::run(dir, &["config", "--get", key]).ok().as_deref().map(str::trim) != Some(value) {
            git::run(dir, &["config", key, value])?;
        }
    }
    Ok(())
}

pub fn has_hooks(dir: &Path) -> bool {
    dir.join(".git").exists() && dir.join(".githooks").join("run.sh").exists()
}

pub async fn setup(dir: PathBuf, options: SetupOptions, identity: &crate::git::Identity, log: Log) -> Result<String> {
    if !dir.join(".git").exists() {
        git::run(&dir, &["init", "-b", "main"])?;
        log.info("Created a git repository");
    }

    install_hooks(&dir)?;
    log.info("Installed the sync hooks in .githooks");
    let gitignore = dir.join(".gitignore");
    if gitignore.exists() {
        let binary_block: Vec<&str> = GITIGNORE_TEMPLATE.lines().skip_while(|l| !l.starts_with("# Binaries")).filter(|l| !l.starts_with('#')).collect();
        if ensure_lines(&gitignore, &binary_block)? {
            log.info("Added the binary folders to the existing .gitignore");
        }
    } else {
        std::fs::write(&gitignore, GITIGNORE_TEMPLATE)?;
        log.info("Wrote a .gitignore for CurseForge instances");
    }
    ensure_lines(&dir.join(".gitattributes"), &GITATTRIBUTES)?;

    if options.untrack_binaries {
        let removable: Vec<String> =
            tracked_binaries(&dir, &Instance::read(&dir)?).into_iter().filter(|b| b.kind != BinaryKind::Local).map(|b| b.path).collect();
        for chunk in removable.chunks(100) {
            let mut args = vec!["rm", "--cached", "--quiet", "--"];
            args.extend(chunk.iter().map(String::as_str));
            git::run(&dir, &args)?;
        }
        for path in &removable {
            log.info(format!("Stopped tracking {path}"));
        }
    }

    if !dir.join(CONFIG_FILE).exists() {
        let mut config = PackConfig { author: options.author.clone(), ..Default::default() };
        config.curseforge.project_id = options.project_id;
        std::fs::write(dir.join(CONFIG_FILE), toml::to_string_pretty(&config)?)?;
        log.info("Wrote packraven.toml");
    }

    git::run(&dir, &["config", "core.hooksPath", ".githooks"])?;
    // In a fresh clone the mods folder is still empty, and rebuilding the list from it would drop every mod.
    if modsync::missing_count(&dir) > 0 {
        modsync::download_missing(&dir, &log).await.context("downloading the mods in mods.lock.json failed")?;
    }
    let committed_directly = modsync::adopt(&dir, &log).await.context("building mods.lock.json failed")?;

    if let Some(remote) = options.remote.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
        if git::run(&dir, &["remote", "get-url", "origin"]).is_ok() {
            git::run(&dir, &["remote", "set-url", "origin", remote])?;
        } else {
            git::run(&dir, &["remote", "add", "origin", remote])?;
        }
        log.info(format!("Set origin to {remote}"));
    }

    let new_repo = git::run(&dir, &["rev-parse", "-q", "--verify", "HEAD"]).is_err();
    if new_repo {
        git::run(&dir, &["add", "-A"])?;
    } else {
        git::run(&dir, &["add", "--", ".githooks", ".gitignore", ".gitattributes", CONFIG_FILE, LOCKFILE])?;
    }
    for (name, _) in HOOKS {
        git::run(&dir, &["update-index", "--chmod=+x", &format!(".githooks/{name}")])?;
    }
    match options.commit.filter(|_| new_repo) {
        Some(commit) => {
            let message = Some(commit.message.trim()).filter(|m| !m.is_empty()).unwrap_or("Initial");
            let identity_args = identity.config_args();
            let mut args: Vec<&str> = identity_args.iter().map(String::as_str).collect();
            args.extend(["commit", "-m", message]);
            let author = commit.author.as_deref().map(str::trim).filter(|a| !a.is_empty()).map(String::from);
            if let Some(author) = &author {
                args.push("--author");
                args.push(author);
            }
            git::run(&dir, &args)?;
            log.info(format!("Committed \"{message}\""));
            Ok(format!("Ready. packraven made the first commit.{}", committed_directly_note(&committed_directly)))
        }
        None => Ok(format!("Ready. packraven staged the changes for your next commit.{}", committed_directly_note(&committed_directly))),
    }
}

fn committed_directly_note(paths: &[String]) -> String {
    match paths.len() {
        0 => String::new(),
        1 => format!(" 1 file isn't on CurseForge, so git tracks it directly: {}", paths[0]),
        n => format!(" {n} files aren't on CurseForge, so git tracks them directly. The setup log lists them."),
    }
}
