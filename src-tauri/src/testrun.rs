use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use tauri::{AppHandle, Emitter};
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{oneshot, watch};

use crate::export::{local_dev_jars, server_args};
use crate::jobs::Log;
use crate::launch::{self, Player};
use crate::minecraft_auth::Slot;
use crate::mirror::{mirror, FilePlan, MirrorReport, Planned};
use crate::pack::{LockEntry, ModKey, Pack};
use crate::process::{spawn_watched, LineHook};
use crate::settings::{RunArgs, Settings};

// Fabric keeps its remapped mods in .fabric, and rebuilding that cache slows every start.
const KEPT_ON_RESYNC: [&str; 5] = ["world", "logs", "crash-reports", "usercache.json", ".fabric"];
// We join by IP rather than "localhost", which can resolve to IPv6 first and miss a server bound to IPv4.
const TEST_SERVER_IP: &str = "127.0.0.1";
const TESTED_MODS: &str = "tested-mods.json";
// The second client plays from a copy of the instance, so its options, worlds and logs don't collide
// with the first client's. Each launch brings the copy up to date with the instance, apart from what
// the second client writes for itself. We seed its options from the instance's once, so it starts
// with your keybinds and resource packs.
const SECOND_CLIENT_OWNS: [&str; 8] = ["saves", "logs", "crash-reports", "screenshots", ".fabric", "options.txt", "servers.dat", "usercache.json"];
const SEEDED_FROM_INSTANCE: [&str; 2] = ["options.txt", "servers.dat"];

#[derive(Default)]
pub struct Managed {
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    kill: Mutex<Option<oneshot::Sender<()>>>,
}

impl Managed {
    pub async fn send(&self, line: &str) -> Result<()> {
        let mut stdin = self.stdin.lock().await;
        let pipe = stdin.as_mut().context("the server is not running")?;
        pipe.write_all(format!("{line}\n").as_bytes()).await?;
        pipe.flush().await?;
        Ok(())
    }

    pub fn kill(&self) {
        if let Some(kill) = self.kill.lock().unwrap().take() {
            let _ = kill.send(());
        }
    }
}

#[derive(Default)]
pub struct TestRun {
    pub server: Mutex<Option<Arc<Managed>>>,
    pub client: Mutex<Option<Arc<Managed>>>,
    pub second_client: Mutex<Option<Arc<Managed>>>,
}

impl TestRun {
    pub fn client(&self, slot: Slot) -> &Mutex<Option<Arc<Managed>>> {
        match slot {
            Slot::First => &self.client,
            Slot::Second => &self.second_client,
        }
    }

    pub fn client_running(&self, slot: Slot) -> bool {
        self.client(slot).lock().unwrap().is_some()
    }

    pub fn running(&self) -> bool {
        self.server.lock().unwrap().is_some() || self.client_running(Slot::First) || self.client_running(Slot::Second)
    }

    pub async fn kill_all(&self) {
        for slot in [&self.server, &self.client, &self.second_client] {
            if let Some(process) = slot.lock().unwrap().as_ref() {
                process.kill();
            }
        }
        // supervise() clears each slot only after the process exits, so we wait for that.
        for _ in 0..100 {
            if !self.running() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct TestEvent {
    kind: &'static str,
    detail: String,
}

fn emit(app: &AppHandle, kind: &'static str, detail: impl Into<String>) {
    let _ = app.emit("test-event", TestEvent { kind, detail: detail.into() });
}

// The UI shows who you play as only once we've confirmed it, which a sign-in can't promise until a run renews it.
pub fn announce_player(app: &AppHandle, slot: Slot, player: &Player) {
    emit(app, if slot == Slot::First { "player-resolved" } else { "second-player-resolved" }, &player.name);
}

pub fn client_job(slot: Slot) -> &'static str {
    match slot {
        Slot::First => "client",
        Slot::Second => "second-client",
    }
}

pub struct ClientDirs {
    pub game: PathBuf,
    pub natives: PathBuf,
}

// Two clients extracting natives into one folder could each find the other's half-written files.
pub fn client_dirs(slot: Slot, instance: &Path, pack_data: &Path) -> ClientDirs {
    match slot {
        Slot::First => ClientDirs { game: instance.to_path_buf(), natives: pack_data.join("natives") },
        Slot::Second => ClientDirs { game: pack_data.join("second-client"), natives: pack_data.join("natives-second") },
    }
}

pub fn sync_second_client(instance: &Path, dest: &Path, log: &Log) -> Result<()> {
    let mut plan = FilePlan::new();
    let walk = walkdir::WalkDir::new(instance).min_depth(1).into_iter();
    let copied = walk.filter_entry(|entry| entry.depth() > 1 || !(entry.file_name() == ".git" || SECOND_CLIENT_OWNS.iter().any(|owned| entry.file_name() == *owned)));
    for entry in copied {
        let entry = entry?;
        if entry.file_type().is_file() {
            let relative = entry.path().strip_prefix(instance)?.to_string_lossy().replace('\\', "/");
            plan.insert(relative, Planned::Copy(entry.into_path()));
        }
    }
    log.info(format!("Syncing changed files to the second client's copy of the instance at {}", dest.display()));
    let report = mirror(dest, &plan, &SECOND_CLIENT_OWNS, false).context("couldn't update the second client's copy of the instance. Is a second client from an earlier run still open?")?;
    log.info(report.to_string());
    for name in SEEDED_FROM_INSTANCE {
        let (from, to) = (instance.join(name), dest.join(name));
        if from.is_file() && !to.exists() {
            std::fs::copy(&from, &to).with_context(|| format!("copying {name} to the second client"))?;
        }
    }
    Ok(())
}

pub fn announce_server_online(app: &AppHandle, online: bool) {
    emit(app, "server-online", online.to_string());
}

// Vanilla treats a missing online-mode line as online.
pub fn online_mode(test_dir: &Path) -> bool {
    let properties = std::fs::read_to_string(test_dir.join("server.properties")).unwrap_or_default();
    !properties.lines().filter_map(|line| line.split_once('=')).any(|(key, value)| key.trim() == "online-mode" && value.trim() == "false")
}

async fn supervise(mut child: Child, managed: Arc<Managed>, slot: &Mutex<Option<Arc<Managed>>>) -> Result<String> {
    *managed.stdin.lock().await = child.stdin.take();
    let (kill_tx, kill_rx) = oneshot::channel();
    *managed.kill.lock().unwrap() = Some(kill_tx);
    let result = tokio::select! {
        status = child.wait() => status.map(|s| format!("Exited with {s}")).map_err(Into::into),
        _ = kill_rx => {
            child.kill().await?;
            Ok("Killed".to_string())
        }
    };
    slot.lock().unwrap().take();
    result
}

pub fn sync_server_dir(mut plan: FilePlan, pack: &Pack, test_dir: &Path, recopy_all: bool, log: &Log) -> Result<MirrorReport> {
    let dev_jars = local_dev_jars(pack);
    for jar in &dev_jars {
        plan.insert(format!("mods/{jar}"), Planned::Copy(pack.dir.join("mods").join(jar)));
    }
    if !dev_jars.is_empty() {
        log.info(format!("Adding local dev builds: {}", dev_jars.join(", ")));
    }
    plan.insert("eula.txt".into(), Planned::Write("eula=true\n".into()));
    if let Some(Planned::Write(properties)) = plan.get_mut("server.properties") {
        *properties = test_properties(properties, pack.config.test.port);
    }
    mirror(test_dir, &plan, &KEPT_ON_RESYNC, recopy_all).context("couldn't update the test server folder. Is a server from an earlier run still open?")
}

// We leave online-mode to the pack. A test server only ever listens on this PC, since a pack with
// online mode off would let anyone join under any name. The test client joins by that address too.
fn test_properties(text: &str, port: u16) -> String {
    let overridden = ["server-ip=", "server-port="];
    let mut lines: Vec<String> = text.lines().filter(|l| !overridden.iter().any(|key| l.starts_with(key))).map(String::from).collect();
    lines.push(format!("server-ip={TEST_SERVER_IP}"));
    lines.push(format!("server-port={port}"));
    lines.join("\n") + "\n"
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TestedMods {
    files: Vec<TestedFile>,
    client_only: Vec<String>,
    keep: Vec<String>,
    minecraft: Option<String>,
    loader: Option<String>,
}

// We match a file by its project when it has one, so an update counts as one changed mod rather than
// a removal and an addition.
#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct TestedFile {
    id: String,
    sha1: String,
}

#[derive(Serialize, PartialEq, Debug)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Untested {
    Never,
    Changed { mods: usize, minecraft: bool, loader: bool },
}

impl TestedMods {
    // Publish reruns this on every change, so we take each jar's sha1 from the mod list rather than
    // reading hundreds of megabytes. Override jars aren't in the mod list, but there are few of them,
    // and a rebuild can keep the name, so we hash those ourselves.
    pub fn of(pack: &Pack) -> Self {
        let locked: HashMap<&str, &LockEntry> = pack.lock.files.iter().map(|entry| (entry.path.as_str(), entry)).collect();
        let mut files: Vec<TestedFile> = std::fs::read_dir(pack.dir.join("mods"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let path = format!("mods/{}", entry.file_name().to_string_lossy());
                if !crate::modsync::is_binary(&path) || !entry.metadata().is_ok_and(|m| m.is_file()) {
                    return None;
                }
                Some(match locked.get(path.as_str()) {
                    Some(locked) => TestedFile { id: locked.key().map_or(path, |key| key.to_string()), sha1: locked.sha1.clone() },
                    None => TestedFile { sha1: hex::encode(Sha1::digest(std::fs::read(entry.path()).ok()?)), id: path },
                })
            })
            .collect();
        files.sort_unstable_by(|a, b| a.id.cmp(&b.id));
        let keys = |keys: &[ModKey]| keys.iter().map(ToString::to_string).collect();
        Self {
            files,
            client_only: keys(&pack.config.server.client_only),
            keep: keys(&pack.config.server.keep),
            minecraft: pack.lock.minecraft.clone(),
            loader: pack.lock.loader.as_ref().map(ToString::to_string),
        }
    }

    // Moving a mod on or off the server changes what a test runs, so it counts as that mod changing.
    fn changed_since(&self, tested: &TestedMods) -> Option<Untested> {
        let index = |mods: &TestedMods| -> HashMap<String, String> { mods.files.iter().map(|f| (f.id.clone(), f.sha1.clone())).collect() };
        let (before, after) = (index(tested), index(self));
        let mut changed: HashSet<&String> = before.keys().chain(after.keys()).filter(|id| before.get(*id) != after.get(*id)).collect();
        for (was, is) in [(&tested.client_only, &self.client_only), (&tested.keep, &self.keep)] {
            changed.extend(was.iter().filter(|key| !is.contains(key)).chain(is.iter().filter(|key| !was.contains(key))));
        }
        let (minecraft, loader) = (self.minecraft != tested.minecraft, self.loader != tested.loader);
        (!changed.is_empty() || minecraft || loader).then_some(Untested::Changed { mods: changed.len(), minecraft, loader })
    }
}

pub fn untested(pack_data: &Path, pack: &Pack) -> Option<Untested> {
    let tested = std::fs::read_to_string(pack_data.join(TESTED_MODS)).ok().and_then(|text| serde_json::from_str::<TestedMods>(&text).ok());
    match tested {
        Some(tested) => TestedMods::of(pack).changed_since(&tested),
        None => Some(Untested::Never),
    }
}

// A player reaching the world is our best sign the run worked, so we record the mods it ran with then.
pub struct JoinRecord {
    path: PathBuf,
    contents: String,
}

impl JoinRecord {
    pub fn new(pack_data: &Path, pack: &Pack) -> Result<Self> {
        Ok(Self { path: pack_data.join(TESTED_MODS), contents: serde_json::to_string(&TestedMods::of(pack))? })
    }
}

pub async fn run_server(app: AppHandle, run: Arc<TestRun>, pack: &Pack, java: PathBuf, test_dir: PathBuf, run_args: RunArgs, ready: watch::Sender<bool>, tested: JoinRecord, log: Log) -> Result<String> {
    remind_to_update(java.clone(), log.clone());
    let (extra_jvm, extra) = (run_args.server_jvm(), run_args.server());
    log_extra_args(&log, &extra_jvm, &extra);
    let mut command = Command::new(&java);
    command.current_dir(&test_dir).args(server_args(pack, &test_dir, &extra_jvm, &extra)?);
    let hook_app = app.clone();
    let hook: LineHook = Arc::new(move |line: &str| {
        if line.contains("Done (") && line.contains(")! For help") {
            let _ = ready.send(true);
            emit(&hook_app, "server-ready", line);
        } else if let Some(index) = line.find(" joined the game") {
            let name = line[..index].rsplit(' ').next().unwrap_or_default();
            let _ = std::fs::write(&tested.path, &tested.contents);
            emit(&hook_app, "player-joined", name);
        }
    });
    let child = spawn_watched(command, &log, true, Some(hook))?;
    let managed = Arc::new(Managed::default());
    *run.server.lock().unwrap() = Some(managed.clone());
    emit(&app, "server-started", test_dir.to_string_lossy());
    let result = supervise(child, managed, &run.server).await;
    emit(&app, "server-stopped", "");
    result
}

pub async fn run_client(app: AppHandle, run: Arc<TestRun>, settings: &Settings, pack: &Pack, slot: Slot, dirs: ClientDirs, join: bool, player: Player, log: Log) -> Result<String> {
    if run.client_running(slot) {
        bail!(if slot == Slot::First { "the client is already running" } else { "the second client is already running" });
    }
    let address = join.then(|| format!("{TEST_SERVER_IP}:{}", pack.config.test.port));
    std::fs::create_dir_all(&dirs.natives)?;
    let launch = launch::client(settings, pack, &player, address.as_deref(), &dirs.game, &dirs.natives)?;
    let target = address.as_deref().map_or_else(|| "to the title screen".to_string(), |a| format!("joining {a}"));
    log.info(format!("Launching {} as {}, {target}", pack.lock.loader()?, player.name));
    remind_to_update(launch.java.clone(), log.clone());
    let run_args = settings.combined_run_args(&settings.instance_dir);
    log_extra_args(&log, &run_args.client_jvm(), &run_args.client());
    let mut command = Command::new(&launch.java);
    command.current_dir(&dirs.game).args(&launch.args);
    let child = spawn_watched(command, &log, false, None)?;
    let managed = Arc::new(Managed::default());
    *run.client(slot).lock().unwrap() = Some(managed.clone());
    emit(&app, "client-started", client_job(slot));
    let result = supervise(child, managed, run.client(slot)).await;
    emit(&app, "client-stopped", client_job(slot));
    result
}

// The check needs the network, so we log its result when it arrives rather than delay the launch.
fn remind_to_update(java: PathBuf, log: Log) {
    tauri::async_runtime::spawn(async move {
        if let Some(reminder) = crate::java::update_reminder(&java).await {
            log.info(reminder);
        }
    });
}

// Extra arguments are easy to forget once a debugging session ends, so each run lists the ones it adds.
fn log_extra_args(log: &Log, jvm: &[String], args: &[String]) {
    if !jvm.is_empty() {
        log.info(format!("Adding extra JVM flags: {}", jvm.join(" ")));
    }
    if !args.is_empty() {
        log.info(format!("Adding extra arguments: {}", args.join(" ")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_server_listens_on_this_pc_only() {
        let properties = test_properties("motd=Ravens\nonline-mode=false\nserver-ip=0.0.0.0\nserver-port=1\n", 25565);
        assert_eq!(properties, "motd=Ravens\nonline-mode=false\nserver-ip=127.0.0.1\nserver-port=25565\n");
    }

    fn pack_in(name: &str) -> Pack {
        let dir = std::env::temp_dir().join(format!("packraven-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("mods")).unwrap();
        Pack { dir, config: Default::default(), lock: Default::default() }
    }

    fn hosted(path: &str, project_id: u64, sha1: &str) -> LockEntry {
        LockEntry::curseforge(path.into(), sha1.into(), 1, crate::pack::CurseforgeFile { project_id, file_id: 1, url: String::new() })
    }

    #[test]
    fn the_second_client_copies_the_instance_but_keeps_its_own_options_and_worlds() {
        let root = std::env::temp_dir().join(format!("packraven-second-client-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (instance, copy) = (root.join("instance"), root.join("copy"));
        for (path, text) in [("mods/a.jar", "a"), ("config/a.toml", "x=1"), ("options.txt", "lang:en_us"), ("saves/World/level.dat", "first"), (".git/HEAD", "ref")] {
            let file = instance.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
        }
        sync_second_client(&instance, &copy, &Log::console("second-client")).unwrap();
        let seeded = std::fs::read_to_string(copy.join("options.txt")).unwrap();
        std::fs::write(copy.join("options.txt"), "lang:de_de").unwrap();
        std::fs::write(copy.join("config/generated.toml"), "y=2").unwrap();
        std::fs::remove_file(instance.join("mods/a.jar")).unwrap();
        std::fs::write(instance.join("mods/b.jar"), "b").unwrap();
        sync_second_client(&instance, &copy, &Log::console("second-client")).unwrap();
        let kept = std::fs::read_to_string(copy.join("options.txt")).unwrap();
        let exists = |path: &str| copy.join(path).exists();
        let found = [exists("mods/a.jar"), exists("mods/b.jar"), exists("config/a.toml"), exists("config/generated.toml"), exists("saves"), exists(".git")];
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(seeded, "lang:en_us");
        assert_eq!(kept, "lang:de_de");
        assert_eq!(found, [false, true, true, false, false, false]);
    }

    #[test]
    fn rebuilding_an_override_jar_under_the_same_name_needs_a_new_test() {
        let pack = pack_in("rebuilt-override");
        std::fs::write(pack.dir.join("mods/own.jar"), "build 1").unwrap();
        let tested = TestedMods::of(&pack);
        std::fs::write(pack.dir.join("mods/notes.txt"), "not a mod").unwrap();
        let other_files = TestedMods::of(&pack).changed_since(&tested);
        std::fs::write(pack.dir.join("mods/own.jar"), "build 2").unwrap();
        let rebuilt = TestedMods::of(&pack).changed_since(&tested);
        let _ = std::fs::remove_dir_all(&pack.dir);
        assert_eq!(other_files, None);
        assert_eq!(rebuilt, Some(Untested::Changed { mods: 1, minecraft: false, loader: false }));
    }

    #[test]
    fn updating_a_mod_and_moving_it_off_the_server_counts_as_one_change() {
        let mut pack = pack_in("updated-mod");
        std::fs::write(pack.dir.join("mods/a-1.jar"), "a").unwrap();
        pack.lock.files = vec![hosted("mods/a-1.jar", 1, "old")];
        let tested = TestedMods::of(&pack);
        std::fs::remove_file(pack.dir.join("mods/a-1.jar")).unwrap();
        std::fs::write(pack.dir.join("mods/a-2.jar"), "a").unwrap();
        pack.lock.files = vec![hosted("mods/a-2.jar", 1, "new")];
        pack.config.server.client_only.push(ModKey::Curseforge(1));
        let changed = TestedMods::of(&pack).changed_since(&tested);
        let never = untested(&pack.dir.join("no-pack-data"), &pack);
        let _ = std::fs::remove_dir_all(&pack.dir);
        assert_eq!(changed, Some(Untested::Changed { mods: 1, minecraft: false, loader: false }));
        assert_eq!(never, Some(Untested::Never));
    }
}
