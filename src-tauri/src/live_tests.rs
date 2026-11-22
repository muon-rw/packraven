use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::Command;

use crate::jobs::Log;
use crate::pack::Pack;
use crate::settings::Settings;
use crate::sides::{self, MetadataCache, Side};
use crate::{export, launch};

fn scratch() -> PathBuf {
    std::env::var("PACKRAVEN_SCRATCH").map(PathBuf::from).unwrap_or_else(|_| std::env::temp_dir().join("packraven-tests"))
}

fn instance() -> PathBuf {
    std::env::var("PACKRAVEN_INSTANCE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| Settings::default().instances_root().join("Raven's High Fantasy"))
}

fn pack() -> (Settings, Pack) {
    let settings = Settings { instance_dir: instance(), ..Settings::default() };
    let pack = Pack::open(&settings.instance_dir).expect("instance");
    (settings, pack)
}

#[tokio::test]
#[ignore]
async fn setup_fresh_instance() {
    let source = instance();
    let dir = scratch().join("fresh-instance");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("mods")).unwrap();
    std::fs::create_dir_all(dir.join("config")).unwrap();
    std::fs::copy(source.join("minecraftinstance.json"), dir.join("minecraftinstance.json")).unwrap();
    std::fs::copy(source.join("config/fml.toml"), dir.join("config/fml.toml")).unwrap();
    let source_pack = Pack::open(&source).unwrap();
    let jars: Vec<&str> = source_pack.lock.files.iter().filter(|e| e.path.starts_with("mods/") && e.size < 200_000).take(2).map(|e| e.path.as_str()).collect();
    for jar in &jars {
        std::fs::copy(source.join(jar), dir.join(jar)).unwrap();
    }
    std::fs::write(dir.join("mods/not-on-curseforge.jar"), b"local build").unwrap();

    let options = crate::setup::SetupOptions { remote: None, author: "test".into(), project_id: 0, untrack_binaries: true, commit: Some(crate::setup::FirstCommit { message: String::new(), author: None }) };
    let message = crate::setup::setup(dir.clone(), options, &Default::default(), Log::console("setup")).await.unwrap();
    println!("{message}");

    let info = crate::setup::inspect(&dir, &[]).unwrap();
    assert!(info.has_git && info.has_lock && info.hooks == crate::setup::HooksState::Current);
    let pack = Pack::open(&dir).unwrap();
    let locked: Vec<&str> = pack.lock.files.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(locked.len(), jars.len(), "locked {locked:?}");
    let tracked = crate::git::tracked_files(&dir).unwrap();
    assert!(tracked.contains(&"mods/not-on-curseforge.jar".to_string()), "unknown jar should be committed directly");
    assert!(jars.iter().all(|j| !tracked.contains(&j.to_string())), "CurseForge jars must not be tracked");
    assert!(tracked.contains(&"config/fml.toml".to_string()));
    assert_eq!(crate::git::run(&dir, &["status", "--porcelain"]).unwrap().trim(), "", "working tree should be clean");
    assert_eq!(crate::git::run(&dir, &["config", "core.hooksPath"]).unwrap().trim(), ".githooks");
}

#[tokio::test]
#[ignore]
async fn classify_sides() {
    let (_, pack) = pack();
    let mut cache = MetadataCache::load(&scratch().join("metadata-cache.json"));
    let result = sides::classify(&pack, &mut cache).await.unwrap();
    cache.save(&scratch().join("metadata-cache.json")).unwrap();
    for side in [Side::Client, Side::Server] {
        for m in result.iter().filter(|m| m.side == side) {
            println!("{:?} {} ({})", serde_json::to_string(&m.side).unwrap(), m.name, m.reason);
        }
    }
    let both = result.iter().filter(|m| m.side == Side::Both).count();
    println!("total {}, both {both}", result.len());
    // A table you can diff against another tool's exclusion list.
    let table: String = result.iter().map(|m| format!("{}\t{}\t{}\t{}\n", m.path, serde_json::to_string(&m.side).unwrap().trim_matches('"'), m.name, m.reason)).collect();
    std::fs::write(scratch().join("sides.tsv"), table).unwrap();
}

#[test]
#[ignore]
fn client_command() {
    let (settings, pack) = pack();
    let launch = launch::client(&settings, &pack, &launch::Player { name: "Dev".into(), uuid: "0".repeat(32), access_token: "0".into() }, Some("127.0.0.1:25565"), &settings.instance_dir, &scratch().join("natives")).unwrap();
    println!("java {}", launch.java.display());
    for arg in &launch.args {
        if arg.len() > 200 {
            println!("  {}... ({} chars)", &arg[..200], arg.len());
        } else {
            println!("  {arg}");
        }
    }
    let classpath = launch.args.iter().skip_while(|a| *a != "-cp").nth(1).unwrap();
    let separator = if cfg!(windows) { ';' } else { ':' };
    let missing: Vec<&str> = classpath.split(separator).filter(|p| !std::path::Path::new(p).exists()).collect();
    assert!(missing.is_empty(), "missing classpath entries: {missing:?}");
}

async fn boots(java: &Path, pack: &Pack, dir: &Path) -> bool {
    let mut command = Command::new(java);
    command.current_dir(dir).args(export::server_args(pack, dir, &[], &[]).unwrap());
    boots_with(command).await
}

async fn boots_with(mut command: Command) -> bool {
    let mut child = command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let started = tokio::time::timeout(Duration::from_secs(900), async {
        while let Some(line) = crate::process::read_line_lossy(&mut reader).await {
            if line.contains("ERROR") || line.contains("Exception") || line.contains("Done (") || line.contains("Downloading") || line.contains("download") || line.contains("GraalVM") || line.contains("Installing the") || line.contains("Starting the server") {
                println!("{line}");
            }
            if line.contains("Done (") {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    if started {
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(b"stop\n").await.unwrap();
        stdin.flush().await.unwrap();
    }
    let _ = tokio::time::timeout(Duration::from_secs(120), child.wait()).await;
    let _ = child.kill().await;
    started
}

#[tokio::test]
#[ignore]
async fn server_pack_boots() {
    let (settings, pack) = pack();
    let log = Log::console("server-pack");
    let java = launch::java(&settings).unwrap();
    let mut cache = MetadataCache::load(&scratch().join("metadata-cache.json"));
    let sides = sides::classify(&pack, &mut cache).await.unwrap();
    let out = scratch().join("build");
    export::server_pack(&pack, &sides, "test", &out, &scratch(), &log).await.unwrap();
    let zip = export::zip_server_pack(&pack, "test", &out, &log).await.unwrap();
    println!("zip {} ({} MB)", zip.display(), std::fs::metadata(&zip).unwrap().len() / 1_000_000);

    let dir = out.join("server");
    let _ = std::fs::remove_dir_all(dir.join("libraries"));
    std::fs::write(dir.join("eula.txt"), "eula=true\n").unwrap();
    std::fs::write(dir.join("java.txt"), java.to_string_lossy().as_bytes()).unwrap();
    let mut bat = Command::new("cmd");
    bat.current_dir(&dir).args(["/c", ".\\run.bat"]);
    assert!(boots_with(bat).await, "server did not finish starting");
}

async fn modless_pack(source: &Path) -> PathBuf {
    let name = source.file_name().unwrap().to_string_lossy();
    let dir = scratch().join(format!("modless-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("mods")).unwrap();
    std::fs::copy(source.join("minecraftinstance.json"), dir.join("minecraftinstance.json")).unwrap();
    let options = crate::setup::SetupOptions { remote: None, author: "test".into(), project_id: 0, untrack_binaries: true, commit: Some(crate::setup::FirstCommit { message: String::new(), author: None }) };
    crate::setup::setup(dir.clone(), options, &Default::default(), Log::console("setup")).await.unwrap();
    dir
}

/// Boots a modless test server for each instance in PACKRAVEN_LOADER_INSTANCES, a comma-separated
/// list of CurseForge instance names, through the same sync a test run uses.
#[tokio::test]
#[ignore]
async fn loader_test_servers_boot() {
    let names = std::env::var("PACKRAVEN_LOADER_INSTANCES").unwrap_or_else(|_| "MMC4Forge,MMC2Fab".into());
    for name in names.split(',') {
        let source = Settings::default().instances_root().join(name);
        let pack = Pack::open(&modless_pack(&source).await).unwrap();
        // The source instance's CurseForge profile picks the Java version its Minecraft needs.
        let java = launch::java(&Settings { instance_dir: source, ..Settings::default() }).unwrap();
        let log = Log::console(name);
        let test_dir = scratch().join(format!("test-server-{name}"));
        let _ = std::fs::remove_dir_all(&test_dir);
        let plan = export::test_server_plan(&pack, &[], "test", &java, &scratch(), &log).await.unwrap();
        println!("{name}: {}", crate::testrun::sync_server_dir(plan, &pack, &test_dir, false, &log).unwrap());
        assert!(boots(&java, &pack, &test_dir).await, "{name} server did not finish starting");

        let plan = export::test_server_plan(&pack, &[], "test", &java, &scratch(), &log).await.unwrap();
        println!("{name} resync: {}", crate::testrun::sync_server_dir(plan, &pack, &test_dir, false, &log).unwrap());
    }
}

/// Boots a modless server pack through its own run scripts: run.bat here, and run.sh in WSL
/// when it's installed. Each script downloads GraalVM on its first start.
#[tokio::test]
#[ignore]
async fn server_pack_scripts_boot() {
    let name = std::env::var("PACKRAVEN_SCRIPT_INSTANCE").unwrap_or_else(|_| "MMC4Forge".into());
    let source = Settings::default().instances_root().join(&name);
    let mut pack = Pack::open(&modless_pack(&source).await).unwrap();
    pack.config.server.bundle_mods = true;
    let out = scratch().join(format!("scripts-{name}"));
    let dir = export::server_pack(&pack, &[], "test", &out, &scratch(), &Log::console(&name)).await.unwrap();
    std::fs::write(dir.join("eula.txt"), "eula=true\n").unwrap();

    let mut bat = Command::new("cmd");
    bat.current_dir(&dir).args(["/c", ".\\run.bat"]);
    assert!(boots_with(bat).await, "run.bat didn't start the server");

    if std::process::Command::new("wsl").args(["-e", "true"]).status().is_ok_and(|s| s.success()) {
        let _ = std::fs::remove_dir_all(dir.join("world"));
        let mut sh = Command::new("wsl");
        sh.current_dir(&dir).args(["-e", "sh", "./run.sh"]);
        assert!(boots_with(sh).await, "run.sh didn't start the server");
    }
}

#[test]
#[ignore]
fn client_export_manifest() {
    let (_, pack) = pack();
    let zip = export::client_export(&pack, &Default::default(), "test", &scratch().join("build"), &Log::console("client-export")).unwrap();
    println!("zip {} ({} KB)", zip.display(), std::fs::metadata(&zip).unwrap().len() / 1000);
}

#[test]
#[ignore]
fn git_status_matches_porcelain() {
    let dir = instance();
    let status = crate::gitops::status(&dir).unwrap();
    println!("branch {:?} upstream {:?} +{} -{} commits {}", status.branch, status.upstream, status.ahead, status.behind, status.has_commits);
    for change in status.staged.iter().take(5) {
        println!("staged {} {}", change.status, change.path);
    }
    for change in status.unstaged.iter().take(5) {
        println!("unstaged {} {}", change.status, change.path);
    }
    let short = crate::git::run(&dir, &["status", "--short", "--untracked-files=all"]).unwrap();
    let staged = short.lines().filter(|l| !l.starts_with(' ') && !l.starts_with('?')).count();
    let unstaged = short.lines().filter(|l| l.as_bytes().get(1).is_some_and(|c| *c != b' ')).count();
    assert_eq!((status.staged.len(), status.unstaged.len()), (staged, unstaged));
    let diff = crate::gitops::diff(&dir, &status.unstaged.first().unwrap().path, false, false).unwrap();
    println!("diff {} bytes, binary {}", diff.text.len(), diff.binary);
    println!("commits: {:?}", crate::gitops::recent_commits(&dir, 3).iter().map(|c| &c.subject).collect::<Vec<_>>());
}

#[test]
#[ignore]
fn defender_probe() {
    let settings = Settings::default();
    let status = crate::defender::status(&[instance(), settings.install_dir]);
    println!("active {} readable {} missing {:?}", status.active, status.exclusions_readable, status.missing);
}

#[tokio::test]
#[ignore]
async fn git_installer_download_matches_checksum() {
    let installer = crate::git_install::download_installer().await.unwrap();
    println!("{} ({} bytes)", installer.display(), std::fs::metadata(&installer).unwrap().len());
    std::fs::remove_file(installer).unwrap();
}

#[test]
#[ignore]
fn detects_java_on_this_computer() {
    for java in crate::java::detect(&Settings::default().install_dir) {
        println!("{:>2} {:<10} {:<18} {:?} {}", java.major, java.version, java.distribution, java.source, java.path.display());
    }
}

#[tokio::test]
#[ignore]
async fn downloads_graalvm() {
    let dir = scratch().join("java");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = crate::java::DOWNLOAD_DIR.set(dir.clone());
    let java = crate::java::download_graalvm(25, |received, total| println!("{received} of {total:?}")).await.unwrap();
    println!("{java:?}");
    assert_eq!(java.major, 25);
    assert!(java.path.starts_with(&dir));
    let output = std::process::Command::new(&java.path).arg("-version").output().unwrap();
    assert!(output.status.success());

    assert!(!crate::java::outdated_graalvms().await.contains(&25), "we just downloaded Oracle's latest build");
    let home = java.path.parent().unwrap().parent().unwrap();
    std::fs::write(home.join("download.sha256"), "an older build").unwrap();
    assert!(crate::java::outdated_graalvms().await.contains(&25));
    assert!(crate::java::update_reminder(&java.path).await.is_some());
}

// Installs an older Apotheosis into a fresh repo, then updates it, which also pulls in its
// required dependencies.
#[tokio::test]
#[ignore]
async fn update_mod_with_dependencies() {
    use crate::mod_updates::{self, ModRequest};
    use crate::modsync::{self, Placement};
    const APOTHEOSIS: u64 = 313970;
    const APOTHEOSIS_8_8_0: u64 = 8826922;

    let dir = scratch().join("mod-updates");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("mods")).unwrap();
    crate::git::run(&dir, &["init", "-q"]).unwrap();
    std::fs::write(dir.join(crate::pack::LOCKFILE), r#"{"minecraft": "1.21.1", "loader": "neoforge-21.1.209", "files": []}"#).unwrap();
    let log = Log::console("mods");

    let old = crate::curseforge::files(&[APOTHEOSIS_8_8_0]).await.unwrap().remove(&APOTHEOSIS_8_8_0).unwrap();
    let entry = mod_updates::lock_entry(&old, "mods", false).unwrap();
    let old_path = entry.path.clone();
    modsync::place(&dir, vec![Placement { entry, replaces: None }], &log).await.unwrap();
    assert!(dir.join(&old_path).exists());

    let pack = Pack::open(&dir).unwrap();
    let checks = mod_updates::check(&pack).await.unwrap();
    println!("{checks:#?}");
    let newer = serde_json::to_value(&checks[0]).unwrap()["newer"].as_array().unwrap().clone();
    let file = newer.iter().find(|f| f["releaseType"] == "release").expect("a newer release")["file"].as_str().unwrap().to_string();
    let file_id: u64 = file.trim_start_matches("cf:").parse().unwrap();

    let plan = mod_updates::plan(&pack, &[ModRequest { file: Some(file), project: None, kind: None, replaces: Some(old_path.clone()) }]).await.unwrap();
    println!("dependencies {:?}, unavailable {:?}", plan.dependencies, plan.unavailable);
    assert!(!plan.dependencies.is_empty(), "Apotheosis requires Placebo and Apothic Attributes");
    modsync::place(&dir, plan.placements, &log).await.unwrap();

    let lock = Pack::open(&dir).unwrap().lock;
    assert!(!dir.join(&old_path).exists());
    assert!(lock.files.iter().any(|e| e.curseforge.as_ref().is_some_and(|f| f.project_id == APOTHEOSIS && f.file_id == file_id)));
    assert!(lock.files.iter().all(|e| dir.join(&e.path).exists()));
    assert_eq!(lock.files.len(), plan.dependencies.len() + 1);

    let written = std::fs::read_to_string(dir.join(crate::pack::LOCKFILE)).unwrap();
    assert!(modsync::refresh_mod_list(&dir).await.unwrap().unknown.is_empty());
    assert_eq!(std::fs::read_to_string(dir.join(crate::pack::LOCKFILE)).unwrap(), written, "the watcher's refresh should agree with what we wrote");
}

#[tokio::test]
#[ignore]
async fn install_mod_from_search() {
    use crate::mod_updates::{self, ModRequest, SearchRequest};
    use crate::modsync;

    let dir = scratch().join("mod-install");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    crate::git::run(&dir, &["init", "-q"]).unwrap();
    std::fs::write(dir.join(crate::pack::LOCKFILE), r#"{"minecraft": "1.21.1", "loader": "neoforge-21.1.209", "files": []}"#).unwrap();
    let pack = Pack::open(&dir).unwrap();

    let search = |text: &str, kind: &str| serde_json::from_value::<SearchRequest>(serde_json::json!({ "platform": "curseforge", "text": text, "kind": kind, "category": null, "sort": "popularity", "index": 0 })).unwrap();
    let page = serde_json::to_value(mod_updates::search(&pack, &search("apotheosis", "mods")).await.unwrap()).unwrap();
    let apotheosis = page["results"].as_array().unwrap().iter().find(|r| r["key"] == "cf:313970").expect("Apotheosis in the results");
    let file = apotheosis["file"]["file"].as_str().expect("a file for 1.21.1 NeoForge").to_string();
    let request = || ModRequest { file: Some(file.clone()), project: None, kind: None, replaces: None };

    let ranked = serde_json::to_value(mod_updates::search(&pack, &serde_json::from_value(serde_json::json!({ "platform": "curseforge", "text": "storage drawers", "kind": "mods", "category": null, "sort": "relevance", "index": 0 })).unwrap()).await.unwrap()).unwrap();
    assert_eq!(ranked["results"][0]["name"], "Storage Drawers");
    assert!(ranked["results"].as_array().unwrap().iter().all(|r| !r["file"].is_null()));

    let resource_packs = serde_json::to_value(mod_updates::search(&pack, &search("", "resourcepacks")).await.unwrap()).unwrap();
    assert!(!resource_packs["results"].as_array().unwrap().is_empty(), "resource packs have no loader, so they must still match");

    let plan = mod_updates::plan(&pack, &[request()]).await.unwrap();
    assert_eq!(plan.installs, ["Apotheosis"]);
    println!("dependencies {:?}", plan.dependencies);
    assert!(!plan.dependencies.is_empty());
    modsync::place(&dir, plan.placements, &Log::console("mods")).await.unwrap();

    let pack = Pack::open(&dir).unwrap();
    assert_eq!(pack.lock.files.len(), plan.dependencies.len() + 1);
    let again = mod_updates::plan(&pack, &[request()]).await;
    assert!(again.is_err(), "installing a project the pack already has should fail");
}

// Read-only on Modrinth: searches it, installs a mod and what it requires into a scratch pack, then checks for updates.
#[tokio::test]
#[ignore]
async fn install_mod_from_modrinth() {
    use crate::mod_updates::{self, ModRequest, SearchRequest};
    use crate::modsync;

    let dir = scratch().join("modrinth-install");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    crate::git::run(&dir, &["init", "-q"]).unwrap();
    std::fs::write(dir.join(crate::pack::LOCKFILE), r#"{"minecraft": "1.21.1", "loader": "neoforge-21.1.209", "files": []}"#).unwrap();
    let pack = Pack::open(&dir).unwrap();

    let search = |text: &str, kind: &str| serde_json::from_value::<SearchRequest>(serde_json::json!({ "platform": "modrinth", "text": text, "kind": kind, "category": null, "sort": "relevance", "index": 0 })).unwrap();
    let page = serde_json::to_value(mod_updates::search(&pack, &search("sophisticated backpacks", "mods")).await.unwrap()).unwrap();
    let hit = page["results"].as_array().unwrap().iter().find(|r| r["name"] == "Sophisticated Backpacks").expect("Sophisticated Backpacks in the results");
    let key = hit["key"].as_str().unwrap().to_string();
    for kind in ["resourcepacks", "shaderpacks", "datapacks"] {
        let results = serde_json::to_value(mod_updates::search(&pack, &search("", kind)).await.unwrap()).unwrap();
        assert!(!results["results"].as_array().unwrap().is_empty(), "Modrinth should have {kind} for 1.21.1");
    }
    assert!(!mod_updates::categories(crate::pack::Platform::Modrinth, mod_updates::AddonKind::Mods).await.unwrap().is_empty());

    let plan = mod_updates::plan(&pack, &[ModRequest { file: None, project: Some(key.clone()), kind: None, replaces: None }]).await.unwrap();
    println!("installs {:?}, dependencies {:?}, unavailable {:?}", plan.installs, plan.dependencies, plan.unavailable);
    assert_eq!(plan.installs, ["Sophisticated Backpacks"]);
    assert!(plan.dependencies.iter().any(|d| d.contains("Sophisticated Core")), "it requires Sophisticated Core");
    modsync::place(&dir, plan.placements, &Log::console("mods")).await.unwrap();

    let pack = Pack::open(&dir).unwrap();
    assert!(pack.lock.files.iter().all(|e| e.modrinth.is_some() && dir.join(&e.path).exists()));
    let checks = mod_updates::check(&pack).await.unwrap();
    assert_eq!(checks.len(), pack.lock.files.len());
    let versions = mod_updates::versions(&pack, &pack.lock.files[0].path, &format!("mr:{}", pack.lock.files[0].modrinth.as_ref().unwrap().project_id)).await.unwrap();
    assert!(!versions.is_empty());
    let description = mod_updates::description(&key).await.unwrap();
    assert!(description.markdown && !description.body.is_empty());
}

// Set PACKRAVEN_TWIN_SAMPLE to try more of the files Modrinth doesn't host byte for byte.
#[tokio::test]
#[ignore]
async fn finds_modrinth_copies_of_the_pack() {
    let (_, pack) = pack();
    let mut entries: Vec<crate::pack::LockEntry> = pack.lock.files.clone();
    let hashes: Vec<String> = entries.iter().map(|e| e.sha1.clone()).collect();
    let exact = crate::modrinth::files_by_sha1(&hashes).await.unwrap();
    for entry in &mut entries {
        entry.modrinth = exact.get(&entry.sha1).cloned();
    }
    assert!(!exact.is_empty(), "Modrinth should host some of the pack's files byte for byte");
    let exact_paths: Vec<String> = entries.iter().filter(|e| e.modrinth.is_some()).map(|e| e.path.clone()).collect();

    let sample = std::env::var("PACKRAVEN_TWIN_SAMPLE").ok().and_then(|n| n.parse().ok()).unwrap_or(12);
    let target = crate::twins::Target::of(&pack.lock).unwrap();
    let mut wanting: Vec<&mut crate::pack::LockEntry> = entries.iter_mut().filter(|e| e.modrinth.is_none() && e.curseforge.is_some() && e.size < 20_000_000).take(sample).collect();
    crate::twins::find(&pack.dir, &scratch().join("twins"), &target, &mut wanting).await.unwrap();
    for entry in &wanting {
        println!("{}: {}", if entry.modrinth.is_some() { "twin" } else { "miss" }, entry.path);
    }
    let tried: Vec<String> = wanting.iter().map(|e| e.path.clone()).collect();
    for folder in ["mods", "resourcepacks", "shaderpacks", "datapacks"] {
        let in_folder: Vec<&crate::pack::LockEntry> = entries.iter().filter(|e| e.path.starts_with(&format!("{folder}/"))).collect();
        let exact = in_folder.iter().filter(|e| exact_paths.contains(&e.path)).count();
        let tried_here = in_folder.iter().filter(|e| tried.contains(&e.path)).count();
        let twins = in_folder.iter().filter(|e| tried.contains(&e.path) && e.modrinth.is_some()).count();
        println!("{folder}: {} files, {exact} on Modrinth byte for byte, twins for {twins} of the {tried_here} others we tried", in_folder.len());
    }
}

// Read-only: prints what Modrinth has of each mod whose CurseForge file it doesn't host.
#[tokio::test]
#[ignore]
async fn reports_modrinth_version_skew() {
    let (_, pack) = pack();
    let mut cache = MetadataCache::load(&scratch().join("metadata-cache.json"));
    let undecided = export::not_on(&pack, &cache, crate::pack::Platform::Modrinth).unwrap();
    crate::publish::look_up_modrinth_skews(&pack, &mut cache).await.unwrap();
    cache.save(&scratch().join("metadata-cache.json")).unwrap();
    for entry in pack.lock.files.iter().filter(|e| undecided.contains(&e.path) && e.curseforge.is_some()) {
        match cache.skew(&crate::publish::skew_key(&pack, entry)).flatten() {
            None => println!("none   {}", entry.path),
            Some(skew) => println!(
                "linked {} -> {} ({:?}) shared={:?} modrinth_newest={:?} newer={} modrinth_build={:?}",
                entry.path,
                skew.link.title,
                skew.link.evidence,
                skew.shared.as_ref().map(|s| (&s.version, s.newer)),
                skew.modrinth_newest,
                skew.modrinth_newer,
                skew.modrinth_build.as_ref().map(|b| (&b.version, &b.differences))
            ),
        }
    }
}

// Read-only: prints which of the pack's updates Modrinth has too, for a pack on both platforms.
#[tokio::test]
#[ignore]
async fn reports_updates_on_both_platforms() {
    let (_, pack) = pack();
    let checks = serde_json::to_value(crate::mod_updates::check(&pack).await.unwrap()).unwrap();
    for check in checks.as_array().unwrap().iter().filter(|c| !c["newer"].as_array().unwrap().is_empty()) {
        let newer: Vec<String> = check["newer"].as_array().unwrap().iter().map(|f| format!("{} ({}, on Modrinth: {})", f["fileName"], f["releaseType"], f["onModrinth"])).collect();
        println!("{}: {}", check["path"], newer.join("; "));
    }
}

// Read-only: Sodium and JEI are on both platforms, under the same slugs and authors.
#[tokio::test]
#[ignore]
async fn finds_curseforge_copies_of_modrinth_projects() {
    let found = crate::links::curseforge_counterparts(&["AANobbMI".into(), "u6dRKJwZ".into()]).await.unwrap();
    println!("{found:?}");
    assert_eq!(found.len(), 2);
}

#[test]
#[ignore]
fn read_mod_manifest() {
    let jar = std::env::var("PACKRAVEN_JAR").expect("set PACKRAVEN_JAR to a mod jar");
    println!("{:?}", crate::mod_manifest::read(Path::new(&jar)));
}

fn live_settings() -> Settings {
    let path = dirs::config_dir()
        .map(|d| d.join("dev.muon.packraven").join("settings.json"))
        .unwrap_or_default();
    let mut settings = if path.is_file() { Settings::load(&path) } else { Settings::default() };
    settings.instance_dir = instance();
    settings
}

#[tokio::test]
#[ignore]
async fn proberaven_headless_dump() {
    let settings = live_settings();
    let pack = Pack::open(&settings.instance_dir).expect("instance");
    let mc = pack.lock.minecraft().expect("minecraft").to_string();
    let loader = pack.lock.mod_loader().expect("loader");
    let loader_label = loader.to_string_label();
    let jar_name = crate::mcdoc::proberaven::jar_name_for(&mc, &loader)
        .unwrap_or_else(|| panic!("no bundled ProbeRaven jar for {mc} ({loader_label})"));
    let jar_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("resources/proberaven")
        .join(&jar_name);
    assert!(jar_path.is_file(), "missing bundled jar at {}", jar_path.display());

    let java = launch::java(&settings).expect("java");
    println!("Pack: {} | MC {mc} ({loader_label}) | Java: {}", pack.name(), java.display());

    let log = Log::console("proberaven-live");
    let mut cache = MetadataCache::load(&scratch().join("metadata-cache.json"));
    let sides = sides::classify(&pack, &mut cache).await.unwrap();
    cache.save(&scratch().join("metadata-cache.json")).unwrap();

    let safe_name: String = pack
        .dir
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') { c } else { '_' })
        .collect();
    let data_dir = dirs::config_dir()
        .map(|d| d.join("dev.muon.packraven"))
        .unwrap_or_else(|| scratch().join("data"));
    let probe_dir = scratch().join("proberaven-live").join(&safe_name);
    let dump_out = scratch().join("proberaven-live").join(format!("{safe_name}.dump.json"));
    let _ = std::fs::remove_file(&dump_out);

    let t_stage = std::time::Instant::now();
    let version = pack.version().unwrap_or_else(|| "dev".into());
    let mut plan = export::test_server_plan(&pack, &sides, &version, &java, &data_dir, &log).await.unwrap();
    plan.insert(format!("mods/{jar_name}"), crate::mirror::Planned::Copy(jar_path));
    let report = crate::testrun::sync_server_dir(plan, &pack, &probe_dir, false, &log).unwrap();
    crate::mcdoc::proberaven::clear_last_dump_world(&probe_dir).unwrap();
    println!("Staged in {:.1}s: {report}", t_stage.elapsed().as_secs_f64());

    let t_run = std::time::Instant::now();
    let run_args = settings.combined_run_args(&settings.instance_dir);
    let extra_jvm = run_args.server_jvm();
    let args = export::server_args(&pack, &probe_dir, &extra_jvm, &[]).unwrap();
    let mut command = Command::new(&java);
    command
        .current_dir(&probe_dir)
        .args(args)
        .env("PROBERAVEN_EXIT_AFTER_DUMP", "true")
        .env("PROBERAVEN_OUTPUT", &dump_out)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().expect("spawn headless probe");
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let startup_failed = std::sync::Arc::new(tokio::sync::Notify::new());
    let fail_signal = std::sync::Arc::clone(&startup_failed);
    let out_task = tokio::spawn(async move {
        let mut r = BufReader::new(stdout);
        let mut tail = Vec::new();
        while let Some(line) = crate::process::read_line_lossy(&mut r).await {
            if line.contains("Failed to start the minecraft server") {
                fail_signal.notify_one();
            }
            if line.contains("ProbeRaven") || line.contains("ERROR") || line.contains("Exception") || line.contains("FATAL") {
                println!("  [out] {line}");
            }
            if tail.len() >= 25 {
                tail.remove(0);
            }
            tail.push(line);
        }
        tail
    });
    let err_task = tokio::spawn(async move {
        let mut r = BufReader::new(stderr);
        let mut tail = Vec::new();
        while let Some(line) = crate::process::read_line_lossy(&mut r).await {
            if tail.len() >= 25 {
                tail.remove(0);
            }
            tail.push(line);
        }
        tail
    });
    let status = tokio::select! {
        res = tokio::time::timeout(Duration::from_secs(300), child.wait()) => {
            res.expect("headless probe timed out after 300s").unwrap()
        }
        _ = startup_failed.notified() => {
            match tokio::time::timeout(Duration::from_secs(3), child.wait()).await {
                Ok(res) => res.unwrap(),
                Err(_) => {
                    let _ = child.kill().await;
                    child.wait().await.unwrap()
                }
            }
        }
    };
    let out_tail = out_task.await.unwrap_or_default();
    let err_tail = err_task.await.unwrap_or_default();
    let run_secs = t_run.elapsed().as_secs_f64();
    if !status.success() {
        eprintln!("--- STDOUT TAIL ---\n{}", out_tail.join("\n"));
        eprintln!("--- STDERR TAIL ---\n{}", err_tail.join("\n"));
    }
    assert!(status.success(), "headless probe exited with {status} after {run_secs:.1}s");

    let raw = std::fs::read_to_string(&dump_out).expect("dump.json should exist");
    let dump: crate::mcdoc::registries::ProbeRavenDump = serde_json::from_str(&raw).expect("valid ProbeRavenDump JSON");
    let entries: usize = dump.registries.values().map(Vec::len).sum();
    let tags: usize = dump.tags.values().map(Vec::len).sum();
    println!(
        "SUCCESS in {run_secs:.1}s: mc={} loader={} mods={} registries={} ({entries} entries) tag_regs={} ({tags} tags) blockStates={} ({:.1} KB)",
        dump.minecraft,
        dump.loader,
        dump.mods.len(),
        dump.registries.len(),
        dump.tags.len(),
        dump.block_states.len(),
        raw.len() as f64 / 1024.0
    );
    assert!(!dump.registries.is_empty());
    assert!(!dump.block_states.is_empty());
}

#[tokio::test]
#[ignore]
async fn datapack_corpus_lint() {
    let settings = live_settings();
    let pack = Pack::open(&settings.instance_dir).expect("instance");
    let probe_cache = std::sync::Arc::new(crate::probejs::ProbeCache::default());
    let dp_cache = crate::mcdoc::DatapackCache::default();
    let (status, index) = dp_cache
        .ensure(settings.instance_dir.clone(), settings.install_dir.clone(), probe_cache, false)
        .await;
    drop(dp_cache);
    println!("DatapackStatus: {status:?}");
    let mut index = std::sync::Arc::try_unwrap(index.expect("datapack index available"))
        .unwrap_or_else(|arc| panic!("unexpected refcount {}", std::sync::Arc::strong_count(&arc)));

    if let Ok(dump_path) = std::env::var("PACKRAVEN_DUMP") {
        let raw = std::fs::read_to_string(&dump_path).expect("read PACKRAVEN_DUMP");
        let dump: crate::mcdoc::registries::ProbeRavenDump = serde_json::from_str(&raw).expect("parse PACKRAVEN_DUMP");
        index.registries.apply_proberaven(dump);
        index.registries.finalize();
        println!("Applied external ProbeRaven dump from {dump_path}");
    }

    // 1. Collect all loose datapack JSON files (data/, kubejs/data/, datapacks/*/, global_packs/*/).
    let mut pack_loose: Vec<(String, String)> = Vec::new();
    let mut pack_zips: Vec<PathBuf> = Vec::new();
    let mut roots: Vec<PathBuf> = vec![pack.dir.join("data"), pack.dir.join("kubejs/data")];
    for loader in crate::global_packs::loaders(&pack.dir, Some(&pack.lock), crate::global_packs::PackKind::Data) {
        for folder in loader.folders {
            for entry in folder.packs {
                let p = pack.dir.join(&entry.path);
                if p.is_dir() {
                    roots.push(p);
                } else if p.is_file() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")) {
                    pack_zips.push(p);
                }
            }
        }
    }
    let dp_dir = pack.dir.join("datapacks");
    if dp_dir.is_dir() {
        for entry in std::fs::read_dir(&dp_dir).into_iter().flatten().flatten() {
            let p = entry.path();
            if p.is_dir() {
                roots.push(p);
            } else if p.is_file() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")) {
                pack_zips.push(p);
            }
        }
    }
    pack_zips.sort();
    pack_zips.dedup();

    let instance_files = crate::mcdoc::overlays::InstanceFiles(&pack.dir);
    let mut overlay_skips = 0usize;
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        for entry in walkdir::WalkDir::new(&root).into_iter().flatten() {
            if !entry.file_type().is_file() {
                continue;
            }
            let p = entry.path();
            let Ok(rel) = p.strip_prefix(&pack.dir) else { continue };
            let rel = rel.to_string_lossy().replace('\\', "/");
            if !p.extension().is_some_and(|e| e.eq_ignore_ascii_case("json")) || !rel.contains("data/") {
                continue;
            }
            if !matches!(index.file_load(&rel, &instance_files), crate::mcdoc::overlays::FileLoad::Loaded) {
                overlay_skips += 1;
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(p) {
                pack_loose.push((rel, text));
            }
        }
    }

    // 2. Collect all JSON files inside zipped preinstalled datapacks (in-memory, zero temp files on disk).
    let mut pack_zip_files: Vec<(String, String)> = Vec::new();
    for zp in &pack_zips {
        let rel_zip = zp.strip_prefix(&pack.dir).unwrap_or(zp).to_string_lossy().replace('\\', "/");
        let Ok(file) = std::fs::File::open(zp) else { continue };
        let Ok(mut zip) = zip::ZipArchive::new(file) else { continue };
        let files = ArchiveFiles::open(&mut zip);
        for name in files.data_json() {
            if !matches!(index.file_load(&name, &files), crate::mcdoc::overlays::FileLoad::Loaded) {
                overlay_skips += 1;
                continue;
            }
            if let Some(text) = files.text(&mut zip, &name) {
                pack_zip_files.push((format!("{rel_zip}!{name}"), text));
            }
        }
        if let Some(meta) = files.mcmeta.clone() {
            pack_zip_files.push((format!("{rel_zip}!pack.mcmeta"), meta));
        }
    }

    // 3. Collect all bundled `data/**/*.json` across every jar in `mods/*.jar` (in-memory).
    let mod_jars: Vec<PathBuf> = std::fs::read_dir(pack.dir.join("mods"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("jar")))
        .collect();

    let index_ref = &index;
    let mut sample = CorpusSample::default();

    let mut loose = LintTally::default();
    for (rel, text) in &pack_loose {
        loose.lint(index_ref, rel, rel, text);
        sample.offer(index_ref, rel, rel, text);
    }
    loose.report("pack-loose", pack_loose.len(), &index_ref.registries.mod_ids);

    let mut zipped = LintTally::default();
    for (rel, text) in &pack_zip_files {
        let lint_path = rel.split_once('!').map(|(_, inner)| inner).unwrap_or(rel.as_str());
        zipped.lint(index_ref, rel, lint_path, text);
        sample.offer(index_ref, rel, lint_path, text);
    }
    zipped.report(&format!("pack-zips ({} archives)", pack_zips.len()), pack_zip_files.len(), &index_ref.registries.mod_ids);

    // Lint mod jars in parallel across CPU threads (streaming per jar to keep RAM modest).
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(8).min(mod_jars.len().max(1));
    let chunk_size = mod_jars.len().div_ceil(threads).max(1);
    let partials: Vec<(usize, usize, LintTally, CorpusSample)> = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for chunk in mod_jars.chunks(chunk_size) {
            handles.push(scope.spawn(move || {
                let mut checked = 0usize;
                let mut skipped = 0usize;
                let mut tally = LintTally::default();
                let mut local_sample = CorpusSample::default();
                for jp in chunk {
                    let jar_name = jp.file_name().unwrap_or_default().to_string_lossy();
                    let Ok(file) = std::fs::File::open(jp) else { continue };
                    let Ok(mut zip) = zip::ZipArchive::new(file) else { continue };
                    let files = ArchiveFiles::open(&mut zip);
                    for name in files.data_json() {
                        if !matches!(index_ref.file_load(&name, &files), crate::mcdoc::overlays::FileLoad::Loaded) {
                            skipped += 1;
                            continue;
                        }
                        let Some(text) = files.text(&mut zip, &name) else { continue };
                        checked += 1;
                        let rel = format!("{jar_name}!{name}");
                        tally.lint(index_ref, &rel, &name, &text);
                        local_sample.offer(index_ref, &rel, &name, &text);
                    }
                }
                (checked, skipped, tally, local_sample)
            }));
        }
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let mut jar_tally = LintTally::default();
    let mut jar_files = 0usize;
    for (checked, skipped, tally, part) in partials {
        jar_files += checked;
        overlay_skips += skipped;
        jar_tally.merge(tally);
        sample.merge(part);
    }
    jar_tally.report(&format!("mod-jars ({} jars)", mod_jars.len()), jar_files, &index_ref.registries.mod_ids);

    println!("[overlays] skipped {overlay_skips} files that an overlay replaces or that sit in an overlay this version doesn't apply");
    sample.sweep(index_ref);
}

/// The entry names and `pack.mcmeta` of one zipped pack, so we can tell which copy of a file loads.
struct ArchiveFiles {
    names: std::collections::HashSet<String>,
    mcmeta: Option<String>,
}

impl ArchiveFiles {
    fn open<R: std::io::Read + std::io::Seek>(zip: &mut zip::ZipArchive<R>) -> Self {
        let names = zip.file_names().filter_map(|n| n.ok().map(|n| n.into_owned())).collect();
        let mcmeta = zip.by_name("pack.mcmeta").ok().and_then(|mut f| {
            let mut text = String::new();
            std::io::Read::read_to_string(&mut f, &mut text).ok().map(|_| text)
        });
        Self { names, mcmeta }
    }

    /// JSON files under `data/`, or under `<overlay>/data/`.
    fn data_json(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .names
            .iter()
            .filter(|n| n.ends_with(".json") && (n.starts_with("data/") || n.split_once('/').is_some_and(|(_, rest)| rest.starts_with("data/"))))
            .cloned()
            .collect();
        names.sort();
        names
    }

    fn text<R: std::io::Read + std::io::Seek>(&self, zip: &mut zip::ZipArchive<R>, name: &str) -> Option<String> {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut zip.by_name(name).ok()?, &mut text).ok().map(|_| text)
    }
}

impl crate::mcdoc::overlays::PackFiles for ArchiveFiles {
    fn read(&self, relative: &str) -> Option<String> {
        (relative == "pack.mcmeta").then(|| self.mcmeta.clone()).flatten()
    }

    fn exists(&self, relative: &str) -> bool {
        self.names.contains(relative)
    }
}

/// Errors and warnings from linting one source of files, grouped by message.
#[derive(Default)]
struct LintTally {
    schema_files: usize,
    files_with_errors: usize,
    errors: usize,
    warnings: usize,
    timings_us: Vec<u128>,
    error_messages: std::collections::BTreeMap<String, (usize, String)>,
    warning_messages: std::collections::BTreeMap<String, (usize, String)>,
    /// Unknown-ID warnings keyed by `<registry> <namespace>`, to tell registry gaps from references to absent mods.
    unknown_ids: std::collections::BTreeMap<String, (usize, String)>,
}

impl LintTally {
    fn lint(&mut self, index: &crate::mcdoc::DatapackIndex, rel: &str, lint_path: &str, text: &str) {
        if let Some(cat) = index.resolve_category(lint_path)
            && crate::mcdoc::checker::resource_root_type(&cat, &index.tables).is_some()
        {
            self.schema_files += 1;
        }
        let t0 = std::time::Instant::now();
        let diags = index.lint(lint_path, text);
        self.timings_us.push(t0.elapsed().as_micros());
        let mut had_err = false;
        for d in diags {
            let (count, messages) = match d.severity {
                "error" => {
                    had_err = true;
                    (&mut self.errors, &mut self.error_messages)
                }
                "warning" => (&mut self.warnings, &mut self.warning_messages),
                _ => continue,
            };
            *count += 1;
            messages.entry(message_shape(&d.message)).or_insert_with(|| (0, format!("{rel}: {}", d.message))).0 += 1;
            if let Some(rest) = d.message.strip_prefix("Unknown ")
                && let Some((registry, quoted)) = rest.split_once(" \"")
                && registry != "key"
            {
                let id = quoted.split('"').next().unwrap_or_default().trim_start_matches('#');
                let namespace = id.split_once(':').map_or("minecraft", |(ns, _)| ns);
                self.unknown_ids.entry(format!("{registry} {namespace}")).or_insert_with(|| (0, format!("{rel}: {id}"))).0 += 1;
            }
        }
        if had_err {
            self.files_with_errors += 1;
        }
    }

    fn merge(&mut self, other: LintTally) {
        self.schema_files += other.schema_files;
        self.files_with_errors += other.files_with_errors;
        self.errors += other.errors;
        self.warnings += other.warnings;
        self.timings_us.extend(other.timings_us);
        for (mine, theirs) in [
            (&mut self.error_messages, other.error_messages),
            (&mut self.warning_messages, other.warning_messages),
            (&mut self.unknown_ids, other.unknown_ids),
        ] {
            for (shape, (count, sample)) in theirs {
                mine.entry(shape).or_insert((0, sample)).0 += count;
            }
        }
    }

    fn report(mut self, label: &str, files: usize, mod_ids: &std::collections::HashSet<String>) {
        self.timings_us.sort_unstable();
        let at = |q: usize| self.timings_us.get(self.timings_us.len() * q / 100).copied().unwrap_or(0);
        let max = self.timings_us.last().copied().unwrap_or(0);
        println!(
            "[{label}] Checked {files} files ({} schema-backed): {} errors in {} files, {} warnings | p50={}us p95={}us max={max}us",
            self.schema_files,
            self.errors,
            self.files_with_errors,
            self.warnings,
            at(50),
            at(95)
        );
        for (kind, messages) in [("error", self.error_messages), ("warning", self.warning_messages)] {
            let mut top: Vec<(usize, String)> = messages.into_values().collect();
            top.sort_by_key(|entry| std::cmp::Reverse(entry.0));
            for (count, sample) in top.into_iter().take(15) {
                println!("  {kind} [{count}x] {sample}");
            }
        }
        let mut unknown: Vec<(usize, String, String)> = self.unknown_ids.into_iter().map(|(k, (c, sample))| (c, k, sample)).collect();
        unknown.sort_by_key(|entry| std::cmp::Reverse(entry.0));
        for (count, key, sample) in unknown.into_iter().take(15) {
            let namespace = key.rsplit(' ').next().unwrap_or_default();
            let installed = if mod_ids.contains(namespace) { "INSTALLED" } else { "absent" };
            println!("  unknown id [{count}x] {key} ({installed}), e.g. {sample}");
        }
    }
}

/// Groups messages that differ only in the quoted ID, so one unknown registry shows up once.
fn message_shape(message: &str) -> String {
    let mut shape = String::with_capacity(message.len());
    let mut in_quote = false;
    for ch in message.chars() {
        if ch == '"' {
            in_quote = !in_quote;
            shape.push_str(if in_quote { "\"…" } else { "\"" });
        } else if !in_quote {
            shape.push(ch);
        }
    }
    shape
}

// Each completion re-checks the whole file, so we cap the work per file and skip huge ones.
const SWEEP_FILES_PER_CATEGORY: usize = 12;
const SWEEP_MAX_FILE_BYTES: usize = 64 * 1024;
const SWEEP_STRINGS_PER_FILE: usize = 40;

/// A few schema-backed files per category, for the completion and hover sweep.
#[derive(Default)]
struct CorpusSample {
    files: std::collections::BTreeMap<String, Vec<(String, String, String)>>,
}

impl CorpusSample {
    fn offer(&mut self, index: &crate::mcdoc::DatapackIndex, rel: &str, lint_path: &str, text: &str) {
        if text.len() > SWEEP_MAX_FILE_BYTES {
            return;
        }
        let Some(cat) = index.resolve_category(lint_path) else { return };
        if crate::mcdoc::checker::resource_root_type(&cat, &index.tables).is_none() {
            return;
        }
        let bucket = self.files.entry(cat).or_default();
        if bucket.len() < SWEEP_FILES_PER_CATEGORY {
            bucket.push((rel.to_string(), lint_path.to_string(), text.to_string()));
        }
    }

    fn merge(&mut self, other: CorpusSample) {
        for (cat, files) in other.files {
            let bucket = self.files.entry(cat).or_default();
            let room = SWEEP_FILES_PER_CATEGORY.saturating_sub(bucket.len());
            bucket.extend(files.into_iter().take(room));
        }
    }

    /// Completes at the end of every clean key and string value and hovers every key. A key or
    /// value that lints clean should come back as one of its own completions; anything else is a miss.
    fn sweep(&self, index: &crate::mcdoc::DatapackIndex) {
        use crate::mcdoc::completer::byte_to_utf16;
        let (mut files, mut keys, mut values, mut hovers, mut panics) = (0usize, 0usize, 0usize, 0usize, 0usize);
        let mut timings_us: Vec<u128> = Vec::new();
        let mut misses: std::collections::BTreeMap<String, (usize, String)> = std::collections::BTreeMap::new();
        for (cat, bucket) in &self.files {
            for (rel, path, text) in bucket {
                let Some(root) = crate::mcdoc::json_parser::parse(text, false).root else { continue };
                files += 1;
                let flagged: Vec<(usize, usize)> = index.lint(path, text).into_iter().map(|d| (d.from, d.to)).collect();
                let clean = |start: usize, end: usize| {
                    let (s, e) = (byte_to_utf16(text, start), byte_to_utf16(text, end));
                    !flagged.iter().any(|&(f, t)| f < e && t > s)
                };
                let mut strings = Vec::new();
                collect_strings(&root, &mut strings);
                let stride = strings.len().div_ceil(SWEEP_STRINGS_PER_FILE).max(1);
                for (span, content_start, is_key) in strings.into_iter().step_by(stride) {
                    let content = &text[content_start..span.end - 1];
                    if content.contains('\\') || !clean(span.start, span.end) {
                        continue;
                    }
                    let offset = byte_to_utf16(text, span.end - 1);
                    let t0 = std::time::Instant::now();
                    let completed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| index.complete(path, text, offset)));
                    timings_us.push(t0.elapsed().as_micros());
                    let Ok(result) = completed else {
                        panics += 1;
                        misses.entry(format!("PANIC completing in {cat}")).or_insert((0, format!("{rel} @ {offset}"))).0 += 1;
                        continue;
                    };
                    if is_key {
                        keys += 1;
                        let hovered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| index.hover(path, text, offset)));
                        hovers += 1;
                        if hovered.is_err() {
                            panics += 1;
                            misses.entry(format!("PANIC hovering in {cat}")).or_insert((0, format!("{rel} @ {offset}"))).0 += 1;
                        }
                    } else {
                        values += 1;
                    }
                    // Only judge slots where we offered something: free-form strings have no completions.
                    if result.items.is_empty() || result.items.iter().any(|i| i.label == content) {
                        continue;
                    }
                    let kind = if is_key { "key" } else { "value" };
                    let detail = result.items[0].detail.clone().unwrap_or_default();
                    misses
                        .entry(format!("{cat} {kind} ({detail})"))
                        .or_insert((0, format!("{rel}: \"{content}\" not among {} items, first {:?}", result.items.len(), result.items[0].label)))
                        .0 += 1;
                }
            }
        }
        timings_us.sort_unstable();
        let at = |q: usize| timings_us.get(timings_us.len() * q / 100).copied().unwrap_or(0);
        println!(
            "[completion sweep] {files} files in {} categories: {keys} keys, {values} values, {hovers} hovers, {panics} panics | p50={}us p95={}us max={}us",
            self.files.len(),
            at(50),
            at(95),
            timings_us.last().copied().unwrap_or(0)
        );
        let mut top: Vec<(usize, String, String)> = misses.into_iter().map(|(k, (c, s))| (c, k, s)).collect();
        top.sort_by_key(|entry| std::cmp::Reverse(entry.0));
        for (count, shape, sample) in top.into_iter().take(25) {
            println!("  miss [{count}x] {shape}\n      e.g. {sample}");
        }
    }
}

fn collect_strings(node: &crate::mcdoc::json_parser::JsonNode, out: &mut Vec<(crate::mcdoc::json_parser::Span, usize, bool)>) {
    use crate::mcdoc::json_parser::JsonKind;
    match &node.kind {
        JsonKind::Object(pairs) => {
            for pair in pairs {
                if pair.key.closed {
                    out.push((pair.key.span, pair.key.map.content_start, true));
                }
                if let Some(value) = &pair.value {
                    collect_strings(value, out);
                }
            }
        }
        JsonKind::Array(items) => items.iter().for_each(|item| collect_strings(item, out)),
        JsonKind::String(s) if s.closed => out.push((s.span, s.map.content_start, false)),
        _ => {}
    }
}

/// Lints the files named in `PACKRAVEN_LINT_FILES` (`;`-separated, each `mods/<jar>!data/...` or a
/// path inside the instance) and prints every diagnostic with the line it points at.
#[tokio::test]
#[ignore]
async fn datapack_lint_files() {
    let settings = live_settings();
    let (_, index) = crate::mcdoc::DatapackCache::default()
        .ensure(settings.instance_dir.clone(), settings.install_dir.clone(), std::sync::Arc::new(crate::probejs::ProbeCache::default()), false)
        .await;
    let index = index.expect("datapack index available");
    for spec in std::env::var("PACKRAVEN_LINT_FILES").expect("set PACKRAVEN_LINT_FILES").split(';').filter(|s| !s.is_empty()) {
        let (lint_path, text) = match spec.split_once('!') {
            Some((archive, entry)) => {
                let mut zip = zip::ZipArchive::new(std::fs::File::open(settings.instance_dir.join(archive)).expect("archive")).expect("zip");
                let mut text = String::new();
                std::io::Read::read_to_string(&mut zip.by_name(entry).expect("entry"), &mut text).expect("read entry");
                (entry.to_string(), text)
            }
            None => (spec.to_string(), std::fs::read_to_string(settings.instance_dir.join(spec)).expect("file")),
        };
        println!("===== {spec} (category {:?})", index.resolve_category(&lint_path));
        for d in index.lint(&lint_path, &text) {
            let start = text.char_indices().nth(d.from).map_or(text.len(), |(i, _)| i);
            let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
            let line_end = text[start..].find('\n').map_or(text.len(), |i| start + i);
            println!("  {} {}\n      {}", d.severity, d.message, text[line_start..line_end].trim());
        }
    }
}

trait LoaderLabel {
    fn to_string_label(&self) -> String;
}

impl LoaderLabel for crate::pack::ModLoader<'_> {
    fn to_string_label(&self) -> String {
        format!("{} {}", self.name(), self.version())
    }
}
