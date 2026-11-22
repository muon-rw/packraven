use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use tauri::path::BaseDirectory;
use tauri::{AppHandle, Manager};
use tokio::process::Command;

use crate::export::{self, server_args};
use crate::jobs::Log;
use crate::mcdoc::registries::ProbeRavenDump;
use crate::mcdoc::DatapackCache;
use crate::mirror::Planned;
use crate::pack::{ModLoader, Pack};
use crate::process::spawn_watched;
use crate::settings::RunArgs;
use crate::sides::ModSide;
use crate::testrun;

pub const PROBERAVEN_VERSION: &str = "0.1.0";
pub const DUMP_REL_PATH: &str = ".proberaven/dump.json";

/// Returns the bundled ProbeRaven jar filename for a pack's Minecraft version and loader, or `None`
/// if that target isn't shipped yet.
pub fn jar_name_for(minecraft: &str, loader: &ModLoader<'_>) -> Option<String> {
    let (loader_slug, mc_target) = match loader {
        ModLoader::Fabric(_) => ("fabric", match_target(minecraft, &["1.20.1", "1.21.1", "1.21.11", "26.1.2", "26.2", "26.3"])?),
        ModLoader::Forge(_) => ("forge", match_target(minecraft, &["1.20.1"])?),
        ModLoader::NeoForge(_) => ("neoforge", match_target(minecraft, &["1.21.1", "1.21.11", "26.1.2", "26.2", "26.3"])?),
    };
    Some(format!("proberaven-{loader_slug}-{PROBERAVEN_VERSION}+{mc_target}.jar"))
}

fn match_target<'a>(minecraft: &str, supported: &[&'a str]) -> Option<&'a str> {
    let mc = minecraft.trim();
    for &target in supported {
        if mc == target {
            return Some(target);
        }
    }
    // 1.21 shares ABI with 1.21.1, 26.1/26.1.x maps to 26.1.2, and 26.2.x / 26.3.x match their base target.
    if mc == "1.21" && supported.contains(&"1.21.1") {
        return Some("1.21.1");
    }
    if (mc == "26.1" || mc.starts_with("26.1.")) && supported.contains(&"26.1.2") {
        return Some("26.1.2");
    }
    for &target in supported {
        if (target == "26.2" || target == "26.3") && mc.starts_with(&format!("{target}.")) {
            return Some(target);
        }
    }
    None
}

/// Resolves the bundled ProbeRaven jar path for `pack`, returning `None` when unsupported or missing.
pub fn resolve_bundled_jar(app: &AppHandle, pack: &Pack) -> Option<(String, PathBuf)> {
    let mc = pack.lock.minecraft.as_deref()?;
    let loader = pack.lock.mod_loader().ok()?;
    let name = jar_name_for(mc, &loader)?;
    let resolved = app.path().resolve(format!("proberaven/{name}"), BaseDirectory::Resource).ok()?;
    if resolved.is_file() {
        return Some((name, resolved));
    }
    // Dev builds sometimes run with cwd at the repo root before Tauri copies bundled resources.
    let dev_fallback = PathBuf::from("src-tauri/resources/proberaven").join(&name);
    if dev_fallback.is_file() {
        return Some((name, dev_fallback));
    }
    None
}

pub fn dump_output_path(instance_dir: &Path) -> PathBuf {
    instance_dir.join(DUMP_REL_PATH)
}

/// Checks via `git check-ignore` whether `.proberaven/dump.json` is already ignored in `pack_dir`;
/// if `pack_dir` is a git repository and the dump isn't ignored yet, appends `.proberaven/` to
/// `pack_dir/.gitignore`. Returns `true` if `.gitignore` was updated.
pub fn ensure_gitignored(pack_dir: &Path) -> bool {
    let status = crate::git::command()
        .current_dir(pack_dir)
        .args(["check-ignore", "-q", "--no-index", "--", DUMP_REL_PATH])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    // Exit 0 = already ignored, 1 = not ignored, 128/Err = not a git repo or git unavailable.
    let Ok(status) = status else { return false };
    if status.code() != Some(1) {
        return false;
    }

    let gitignore = pack_dir.join(".gitignore");
    let existing = std::fs::read_to_string(&gitignore).unwrap_or_default();
    if existing.lines().any(|line| matches!(line.trim(), ".proberaven/" | ".proberaven" | "/.proberaven/" | "/.proberaven")) {
        return false;
    }
    let eol = if existing.contains("\r\n") { "\r\n" } else { "\n" };
    let mut updated = existing;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push_str(eol);
    }
    updated.push_str(".proberaven/");
    updated.push_str(eol);
    std::fs::write(&gitignore, updated).is_ok()
}

/// Stages a throwaway server directory with ProbeRaven injected, runs a headless dump that halts
/// before world load, validates the output JSON, and invalidates `datapack_cache`.
#[allow(clippy::too_many_arguments)]
pub async fn run_headless_dump(
    app: AppHandle,
    pack: Pack,
    sides: Vec<ModSide>,
    java: PathBuf,
    data_dir: PathBuf,
    pack_data: PathBuf,
    run_args: RunArgs,
    datapack_cache: Arc<DatapackCache>,
    log: Log,
) -> Result<String> {
    let mc = pack.lock.minecraft()?.to_string();
    let loader_label = pack.lock.loader()?.to_string();
    let (jar_name, jar_path) = resolve_bundled_jar(&app, &pack).with_context(|| {
        format!("Packraven doesn't ship a ProbeRaven jar for Minecraft {mc} ({loader_label}) yet.")
    })?;

    let version = pack.version().unwrap_or_else(|| "dev".into());
    log.info(format!("Preparing headless registry dump for Minecraft {mc} ({loader_label})"));
    let mut plan = export::test_server_plan(&pack, &sides, &version, &java, &data_dir, &log).await?;
    plan.insert(format!("mods/{jar_name}"), Planned::Copy(jar_path));

    let probe_dir = pack_data.join("proberaven-probe");
    let report = testrun::sync_server_dir(plan, &pack, &probe_dir, false, &log)?;
    log.info(report.to_string());

    if ensure_gitignored(&pack.dir) {
        log.info("Added .proberaven/ to .gitignore");
    }
    let output = dump_output_path(&pack.dir);
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let staged_output = output.with_extension("json.next");
    let _ = std::fs::remove_file(&staged_output);

    let extra_jvm = run_args.server_jvm();
    let args = server_args(&pack, &probe_dir, &extra_jvm, &[])?;
    let mut command = Command::new(&java);
    command
        .current_dir(&probe_dir)
        .args(args)
        .env("PROBERAVEN_EXIT_AFTER_DUMP", "true")
        .env("PROBERAVEN_OUTPUT", &staged_output);

    log.info("Launching headless server to dump registries, tags, and block states");
    let startup_failed = Arc::new(tokio::sync::Notify::new());
    let fail_signal = Arc::clone(&startup_failed);
    let hook: crate::process::LineHook = Arc::new(move |line: &str| {
        if line.contains("Failed to start the minecraft server") {
            fail_signal.notify_one();
        }
    });
    let mut child = spawn_watched(command, &log, false, Some(hook))?;
    let status = tokio::select! {
        res = child.wait() => res.context("waiting for headless ProbeRaven process")?,
        _ = startup_failed.notified() => {
            // Give the server a moment to flush the crash stack trace and exit on its own;
            // mods with non-daemon worker threads otherwise keep the JVM alive indefinitely.
            match tokio::time::timeout(std::time::Duration::from_secs(3), child.wait()).await {
                Ok(res) => res.context("waiting for headless ProbeRaven process")?,
                Err(_) => {
                    let _ = child.kill().await;
                    let _ = std::fs::remove_file(&staged_output);
                    bail!("Headless registry dump failed: the dedicated server crashed during startup (see log above).");
                }
            }
        }
    };
    if !status.success() {
        let _ = std::fs::remove_file(&staged_output);
        bail!("Headless registry dump exited with {status}");
    }

    let raw = std::fs::read_to_string(&staged_output)
        .with_context(|| format!("ProbeRaven exited cleanly but did not write {}", staged_output.display()))?;
    let dump: ProbeRavenDump = serde_json::from_str(&raw).context("parsing ProbeRaven dump.json")?;
    std::fs::rename(&staged_output, &output)
        .with_context(|| format!("saving {}", output.display()))?;
    let reg_count = dump.registries.len();
    let entry_count: usize = dump.registries.values().map(Vec::len).sum();
    let tag_count: usize = dump.tags.values().map(Vec::len).sum();
    let block_count = dump.block_states.len();

    datapack_cache.invalidate(&pack.dir);
    Ok(format!(
        "Dumped {reg_count} registries ({entry_count} entries), {tag_count} tags, and {block_count} block states."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_expected_proberaven_jar_names() {
        assert_eq!(
            jar_name_for("1.20.1", &ModLoader::Forge("47.4.0")),
            Some("proberaven-forge-0.1.0+1.20.1.jar".into())
        );
        assert_eq!(
            jar_name_for("1.20.1", &ModLoader::Fabric("0.20.1")),
            Some("proberaven-fabric-0.1.0+1.20.1.jar".into())
        );
        assert_eq!(
            jar_name_for("1.21.1", &ModLoader::NeoForge("21.1.265")),
            Some("proberaven-neoforge-0.1.0+1.21.1.jar".into())
        );
        assert_eq!(
            jar_name_for("1.21", &ModLoader::NeoForge("21.0.10")),
            Some("proberaven-neoforge-0.1.0+1.21.1.jar".into())
        );
        assert_eq!(
            jar_name_for("26.1.2", &ModLoader::NeoForge("26.1.2.129")),
            Some("proberaven-neoforge-0.1.0+26.1.2.jar".into())
        );
        assert_eq!(
            jar_name_for("26.1", &ModLoader::Fabric("0.20.1")),
            Some("proberaven-fabric-0.1.0+26.1.2.jar".into())
        );
        assert_eq!(
            jar_name_for("26.3.1", &ModLoader::Fabric("0.20.1")),
            Some("proberaven-fabric-0.1.0+26.3.jar".into())
        );
        assert_eq!(jar_name_for("1.19.2", &ModLoader::Forge("43.3.0")), None);
        assert_eq!(jar_name_for("1.21.1", &ModLoader::Forge("52.0.0")), None);
    }

    #[test]
    fn ensure_gitignored_appends_only_when_unignored_in_git_repo() {
        let base = std::env::temp_dir().join(format!("packraven-proberaven-ignore-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);

        // Non-git directory: must not create .gitignore.
        let non_git = base.join("plain");
        std::fs::create_dir_all(&non_git).unwrap();
        assert!(!ensure_gitignored(&non_git));
        assert!(!non_git.join(".gitignore").exists());

        // Git repo without rule: appends `.proberaven/` once, then becomes a no-op.
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        assert!(crate::git::command().current_dir(&repo).arg("init").output().unwrap().status.success());
        std::fs::write(repo.join(".gitignore"), "logs/\n").unwrap();
        assert!(ensure_gitignored(&repo));
        assert_eq!(std::fs::read_to_string(repo.join(".gitignore")).unwrap(), "logs/\n.proberaven/\n");
        assert!(!ensure_gitignored(&repo));

        // Git repo already ignoring via wildcard rule: leaves .gitignore untouched.
        let wildcard = base.join("wildcard");
        std::fs::create_dir_all(&wildcard).unwrap();
        assert!(crate::git::command().current_dir(&wildcard).arg("init").output().unwrap().status.success());
        std::fs::write(wildcard.join(".gitignore"), ".*\n").unwrap();
        assert!(!ensure_gitignored(&wildcard));
        assert_eq!(std::fs::read_to_string(wildcard.join(".gitignore")).unwrap(), ".*\n");

        let _ = std::fs::remove_dir_all(&base);
    }
}
