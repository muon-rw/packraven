use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde::Serialize;
use tauri::{AppHandle, State};
use tokio::sync::watch;

use crate::changelog::{self, ModDiff};
use crate::curseforge;
use crate::export;
use crate::git;
use crate::jobs::{JobQueue, Jobs};
use crate::launch;
use crate::allowlist::Allowlist;
use crate::links::Skew;
use crate::pack::{ModKey, Pack, PackConfig, Platform};
use crate::publish::{self, PublishContext, PublishOptions};
use crate::settings::Settings;
use crate::sides::{self, MetadataCache, ModSide};
use crate::modsync::{self, ModCounts};
use crate::testrun::{self, TestRun};

pub struct AppState {
    pub settings: Mutex<Settings>,
    pub settings_path: PathBuf,
    pub data_dir: PathBuf,
    pub jobs: Jobs,
    pub mod_changes: Arc<Mutex<JobQueue<crate::mod_commands::ModChange>>>,
    pub test: Arc<TestRun>,
    pub watcher: Mutex<Option<crate::watcher::PackWatcher>>,
    pub watch_error: Mutex<Option<String>>,
    pub history: Mutex<crate::history::History>,
    pub mod_list: Mutex<crate::mod_list::ModListState>,
    pub minecraft: Arc<crate::minecraft_auth::SessionCache>,
    pub minecraft_sign_in: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

impl AppState {
    pub(crate) fn settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    pub(crate) fn rewatch(&self, app: &AppHandle) {
        self.update_presence();
        let dir = self.settings().instance_dir;
        if crate::setup::has_hooks(&dir) {
            let _ = crate::setup::link_hooks(&dir);
        }
        let watched = (!dir.as_os_str().is_empty()).then(|| crate::watcher::watch(app.clone(), dir));
        *self.watch_error.lock().unwrap() = watched.as_ref().and_then(|w| w.as_ref().err()).map(|e| format!("{e:#}"));
        *self.watcher.lock().unwrap() = watched.and_then(Result::ok);
    }

    pub(crate) fn update_presence(&self) {
        let settings = self.settings();
        let pack = settings.discord_shows_pack.then(|| Pack::open(&settings.instance_dir).ok()).flatten();
        crate::presence::set_pack(pack.map(|pack| pack.name()));
    }

    pub(crate) fn data_dir(&self) -> PathBuf {
        self.data_dir.clone()
    }

    pub(crate) fn update_settings(&self, change: impl FnOnce(&mut Settings)) -> Result<()> {
        let mut settings = self.settings.lock().unwrap();
        change(&mut settings);
        settings.save(&self.settings_path)
    }

    pub(crate) fn pack(&self) -> Result<Pack> {
        Pack::open(&self.settings().instance_dir)
    }

    pub(crate) fn pack_data(&self) -> PathBuf {
        pack_data_dir(&self.data_dir, &self.settings().instance_dir)
    }

    pub(crate) fn curseforge_locks_path(&self) -> PathBuf {
        self.pack_data().join("curseforge-locks.json")
    }

    pub(crate) fn cache_path(&self) -> PathBuf {
        self.data_dir.join("metadata-cache.json")
    }

    pub(crate) fn allowlist_path(&self) -> PathBuf {
        self.data_dir.join("curseforge-allowlist.json")
    }

    fn version(&self, pack: &Pack) -> String {
        pack.version().unwrap_or_else(|| "dev".into())
    }
}

pub fn pack_data_dir(data_dir: &std::path::Path, instance_dir: &std::path::Path) -> PathBuf {
    let name = instance_dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let slug: String = name.chars().map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    data_dir.join("packs").join(slug)
}

type CmdResult<T> = Result<T, String>;

fn err(error: anyhow::Error) -> String {
    format!("{error:#}")
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Settings {
    state.settings()
}

#[tauri::command]
pub fn save_settings(state: State<'_, AppState>, settings: Settings) -> CmdResult<()> {
    if let Some((major, java)) = settings.java_by_major.iter().find(|(_, java)| !java.is_file()) {
        return Err(format!("The Java {major} path, {}, isn't a file", java.display()));
    }
    settings.save(&state.settings_path).map_err(err)?;
    *state.settings.lock().unwrap() = settings;
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Overview {
    name: String,
    version: Option<String>,
    bcc_installed: bool,
    minecraft: Option<String>,
    loader: Option<String>,
    lock_count: usize,
    addons: AddonCounts,
    /// Files neither platform hosts, which you need to add to git or remove.
    unhosted: Vec<String>,
    not_on_curseforge: Vec<OverrideCandidate>,
    not_on_modrinth: Vec<OverrideCandidate>,
    modrinth_error: Option<String>,
    releases_missing_modrinth: usize,
    releases_missing_server_pack: usize,
    mod_list_error: Option<String>,
    watch_error: Option<String>,
    branch: Option<String>,
    ahead: u32,
    behind: u32,
    incoming: Option<ModCounts>,
    changed: Vec<String>,
    last_tag: Option<String>,
    /// The version the last release tag belongs to, which isn't always the tag without its v.
    last_version: Option<String>,
    dev_jars: Vec<String>,
    project_id: u64,
    modrinth_project_id: String,
    running_jobs: Vec<String>,
    server_running: bool,
    client_running: bool,
    hooks: crate::setup::HooksState,
}

/// A file a platform doesn't host, with the mod it belongs to when we know it, so you can decide whether you may share it there.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverrideCandidate {
    path: String,
    name: Option<String>,
    url: Option<String>,
    authors: Vec<String>,
    license: Option<String>,
    open_license: bool,
    /// CurseForge's list of mods it allows in packs includes this one.
    allowlisted: bool,
    /// We have the project's authors and license, or the file isn't from a project we can ask about.
    known: bool,
    /// What Modrinth has of the mod, for a CurseForge file Modrinth doesn't host.
    skew: Option<Skew>,
    /// We've looked for the mod's other versions, or there's nothing to look for.
    versions_known: bool,
    /// You locked the mod's version in Mods.
    locked: bool,
    /// A pattern for this file's other versions, so one choice covers its updates.
    any_version: Option<String>,
    /// What you decided about another version of this file, which is usually one it replaced.
    earlier: Option<EarlierChoice>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EarlierChoice {
    path: String,
    choice: OverrideChoice,
}

fn earlier_choice(pack: &Pack, platform: Platform, path: &str, any_version: Option<&str>) -> Option<EarlierChoice> {
    let any_version = any_version?;
    let (shipped, kept_off) = pack.config.override_choices(platform);
    let earlier = |list: &[String], choice| list.iter().find(|entry| *entry != path && !entry.contains('*') && export::choice_covers(any_version, entry)).map(|entry| EarlierChoice { path: entry.clone(), choice });
    earlier(shipped, OverrideChoice::Ship).or_else(|| earlier(kept_off, OverrideChoice::LeaveOut))
}

// We build the overview after every file, git and job change, so we only read the caches when
// there's a file to describe.
fn override_candidates(pack: &Pack, state: &AppState, platform: Platform) -> Vec<OverrideCandidate> {
    if !pack.config.targets(platform) {
        return Vec::new();
    }
    let cache = MetadataCache::load(&state.cache_path());
    let paths = export::not_on(pack, &cache, platform).unwrap_or_default();
    let allowlist = if platform == Platform::Curseforge && !paths.is_empty() { Allowlist::load(&state.allowlist_path()) } else { Allowlist::default() };
    paths
        .into_iter()
        .map(|path| {
            let entry = pack.lock.files.iter().find(|e| e.path == path);
            let project = export::project_of(pack, &cache, &path);
            let license = project.and_then(|p| p.license.clone()).filter(|l| !l.is_empty());
            let authors = project.and_then(|p| p.authors.clone()).unwrap_or_default();
            let skew = entry.filter(|e| platform == Platform::Modrinth && e.curseforge.is_some()).map(|e| cache.skew(&publish::skew_key(pack, e)));
            let any_version = export::any_version_pattern(&path);
            OverrideCandidate {
                earlier: earlier_choice(pack, platform, &path, any_version.as_deref()),
                any_version,
                name: project.map(|p| p.name.clone()),
                url: project.and_then(|p| p.url.clone()),
                allowlisted: platform == Platform::Curseforge && project.is_some_and(|p| allowlist.find(&p.name, &authors).is_some()),
                open_license: license.as_deref().is_some_and(export::is_open_license),
                known: entry.and_then(|e| e.key()).is_none() || project.is_some_and(|p| p.authors.is_some() && p.license.is_some()),
                versions_known: skew.is_none_or(|checked| checked.is_some()),
                locked: entry.is_some_and(|e| pack.config.updates.locked.iter().any(|key| e.has_key(key))),
                skew: skew.flatten().flatten().cloned(),
                authors,
                license,
                path,
            }
        })
        .collect()
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AddonCounts {
    mods: usize,
    resource_packs: usize,
    shader_packs: usize,
    data_packs: usize,
    other: usize,
}

fn addon_counts(pack: &Pack) -> AddonCounts {
    let mut counts = AddonCounts::default();
    for entry in &pack.lock.files {
        let slot = match entry.path.split('/').next().unwrap_or_default() {
            "mods" => &mut counts.mods,
            "resourcepacks" => &mut counts.resource_packs,
            "shaderpacks" => &mut counts.shader_packs,
            _ if entry.path.contains("datapacks/") => &mut counts.data_packs,
            _ => &mut counts.other,
        };
        *slot += 1;
    }
    counts
}

fn incoming_mods(dir: &std::path::Path) -> Option<ModCounts> {
    Some(modsync::mod_counts(&modsync::lock_at(dir, "HEAD").ok()?, &modsync::lock_at(dir, "@{upstream}").ok()?))
}

// We call this after file, git and job changes, and it runs git, so we keep it off the main thread.
#[tauri::command(async)]
pub fn overview(state: State<'_, AppState>) -> CmdResult<Overview> {
    let pack = state.pack().map_err(err)?;
    let mod_list = state.mod_list.lock().unwrap().clone();
    let mod_list = if crate::setup::same_dir(&mod_list.dir, &pack.dir) { mod_list } else { Default::default() };
    let (ahead, behind) = git::ahead_behind(&pack.dir).unwrap_or((0, 0));
    let last_tag = git::last_release_tag(&pack.dir);
    Ok(Overview {
        name: pack.name(),
        version: pack.version(),
        bcc_installed: pack.has_bcc(),
        minecraft: pack.lock.minecraft.clone(),
        loader: pack.lock.loader.as_ref().map(ToString::to_string),
        lock_count: pack.lock.files.len(),
        addons: addon_counts(&pack),
        unhosted: mod_list.unhosted,
        not_on_curseforge: override_candidates(&pack, &state, Platform::Curseforge),
        not_on_modrinth: override_candidates(&pack, &state, Platform::Modrinth),
        releases_missing_modrinth: if pack.config.targets_modrinth() {
            publish::load_releases(&state.pack_data().join("releases.json")).iter().filter(|r| publish::awaiting_modrinth(&pack, &state.pack_data(), r).is_some()).count()
        } else {
            0
        },
        releases_missing_server_pack: publish::load_releases(&state.pack_data().join("releases.json")).iter().filter(|r| r.curseforge.as_ref().is_some_and(|c| c.awaits_server_pack())).count(),
        modrinth_error: mod_list.modrinth_error.filter(|_| pack.config.targets_modrinth()),
        mod_list_error: mod_list.error,
        watch_error: state.watch_error.lock().unwrap().clone(),
        branch: git::branch(&pack.dir),
        ahead,
        behind,
        incoming: if behind > 0 { incoming_mods(&pack.dir) } else { None },
        changed: git::changed_files(&pack.dir),
        last_version: last_tag.as_deref().map(|tag| publish::version_of_tag(tag, &publish::load_releases(&state.pack_data().join("releases.json")))),
        last_tag,
        dev_jars: export::local_dev_jars(&pack),
        project_id: pack.config.curseforge.project_id,
        modrinth_project_id: pack.config.modrinth.project_id.trim().to_string(),
        running_jobs: state.jobs.running(),
        server_running: state.test.server.lock().unwrap().is_some(),
        client_running: state.test.client.lock().unwrap().is_some(),
        hooks: crate::setup::hooks_state(&pack.dir),
    })
}

#[tauri::command]
pub fn get_pack_config(state: State<'_, AppState>) -> CmdResult<PackConfig> {
    Ok(state.pack().map_err(err)?.config)
}

#[tauri::command]
pub fn save_pack_config(state: State<'_, AppState>, config: PackConfig) -> CmdResult<()> {
    let mut pack = state.pack().map_err(err)?;
    pack.config = config;
    pack.save_config().map_err(err)
}

// Publish commits whatever is on disk after its uploads, so a version changed mid-publish would
// land in the commit for the old tag.
#[tauri::command]
pub fn set_version(state: State<'_, AppState>, version: String) -> CmdResult<()> {
    if state.jobs.running().iter().any(|job| job == "publish") {
        return Err("packraven is publishing. Change the version once it finishes.".into());
    }
    state.pack().map_err(err)?.set_version(version.trim()).map_err(err)
}

#[tauri::command]
pub fn version_problem(version: String) -> Option<String> {
    crate::pack::version_problem(&version)
}

#[tauri::command]
pub fn get_notes(state: State<'_, AppState>) -> CmdResult<String> {
    Ok(changelog::read_notes(&state.pack().map_err(err)?))
}

#[tauri::command]
pub fn save_notes(state: State<'_, AppState>, text: String) -> CmdResult<()> {
    changelog::write_notes(&state.pack().map_err(err)?, &text).map_err(err)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangelogPreview {
    diff: ModDiff,
    markdown: String,
}

#[tauri::command]
pub async fn changelog_preview(state: State<'_, AppState>) -> CmdResult<ChangelogPreview> {
    let pack = state.pack().map_err(err)?;
    // We diff the mods folder rather than the mod list, so the preview already shows changes you
    // haven't recorded yet. If the folder can't be read as a lock, we fall back to the list.
    let folder = modsync::folder_lock(&pack.dir).await.unwrap_or_else(|_| pack.lock.clone());
    let mut cache = MetadataCache::load(&state.cache_path());
    let diff = changelog::mod_diff(&pack, &folder, &mut cache, &state.pack_data().join(changelog::BASELINE_FILE)).await.map_err(err)?;
    cache.save(&state.cache_path()).map_err(err)?;
    let markdown = changelog::render(&changelog::read_notes(&pack), &diff);
    Ok(ChangelogPreview { diff, markdown })
}

// `fresh` asks CurseForge again about every jar it didn't recognize, which you want when you check
// for updates right after uploading one.
#[tauri::command]
pub async fn resync_mod_list(app: AppHandle, state: State<'_, AppState>, fresh: bool) -> CmdResult<()> {
    let dir = state.settings().instance_dir;
    if !dir.as_os_str().is_empty() {
        crate::mod_list::resync(&app, &dir, if fresh { modsync::Lookup::Fresh } else { modsync::Lookup::Cached }).await;
    }
    Ok(())
}

#[tauri::command]
pub async fn track_directly(app: AppHandle, state: State<'_, AppState>, paths: Vec<String>) -> CmdResult<()> {
    let dir = state.settings().instance_dir;
    modsync::track_directly(&dir, &paths).map_err(err)?;
    crate::mod_list::refresh_now(&app, &dir, modsync::Lookup::Cached).await;
    Ok(())
}

#[tauri::command]
pub fn revert_mods(app: AppHandle, state: State<'_, AppState>) -> CmdResult<()> {
    let dir = state.settings().instance_dir;
    // The restore future borrows lock entries across its download stream, which the compiler can't
    // prove Send, so we drive it on a blocking thread the way the git hook's CLI does.
    state.jobs.start(&app, "restore", move |log| async move {
        tauri::async_runtime::spawn_blocking(move || {
            let _guard = modsync::MOD_LIST_GUARD.blocking_lock();
            git::run(&dir, &["checkout", "HEAD", "--", crate::pack::LOCKFILE])?;
            tauri::async_runtime::block_on(modsync::restore(&dir, &log))
        })
        .await??;
        Ok("Your mods match the last commit again.".into())
    })
}

#[tauri::command]
pub fn set_compare_baseline(state: State<'_, AppState>, zip: String) -> CmdResult<()> {
    let baseline = changelog::baseline_from_zip(std::path::Path::new(&zip)).map_err(err)?;
    changelog::save_baseline(&state.pack_data().join(changelog::BASELINE_FILE), &baseline).map_err(err)
}

#[tauri::command]
pub fn clear_compare_baseline(state: State<'_, AppState>) -> CmdResult<()> {
    changelog::clear_baseline(&state.pack_data().join(changelog::BASELINE_FILE)).map_err(err)
}

#[tauri::command]
pub async fn server_sides(state: State<'_, AppState>) -> CmdResult<Vec<ModSide>> {
    let pack = state.pack().map_err(err)?;
    classify_cached(&state.cache_path(), &pack).await.map_err(err)
}

#[tauri::command]
pub fn set_side_override(state: State<'_, AppState>, key: String, mode: String) -> CmdResult<()> {
    let key: ModKey = key.parse()?;
    let mut pack = state.pack().map_err(err)?;
    let server = &mut pack.config.server;
    server.client_only.retain(|k| *k != key);
    server.keep.retain(|k| *k != key);
    match mode.as_str() {
        "client" => server.client_only.push(key),
        "keep" => server.keep.push(key),
        _ => {}
    }
    server.client_only.sort_unstable();
    server.keep.sort_unstable();
    pack.save_config().map_err(err)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preflight {
    problems: Vec<String>,
    warnings: Vec<String>,
    untested: Option<testrun::Untested>,
    tag_taken: bool,
    client: publish::UploadNames,
    server: publish::UploadNames,
    modrinth_file: String,
    curseforge_shipped: Vec<OverrideFileView>,
    curseforge_left_out: Vec<OverrideFileView>,
    modrinth_shipped: Vec<OverrideFileView>,
    modrinth_left_out: Vec<OverrideFileView>,
}

#[tauri::command(async)]
pub fn publish_preflight(state: State<'_, AppState>, version: String) -> CmdResult<Preflight> {
    let mod_list = state.mod_list.lock().unwrap().clone();
    let pack = state.pack().map_err(err)?;
    let version = version.trim();
    let mut cache = MetadataCache::load(&state.cache_path());
    let warnings = publish::preflight_warnings(&pack, &mut cache);
    if cache.read_new_manifests() {
        let _ = cache.save(&state.cache_path());
    }
    let allowlist = Allowlist::load(&state.allowlist_path());
    let decided = |platform| -> CmdResult<(Vec<OverrideFileView>, Vec<OverrideFileView>)> {
        if !pack.config.targets(platform) {
            return Ok((Vec::new(), Vec::new()));
        }
        let files = export::override_files_for(&pack, &cache, platform).map_err(err)?;
        let view = |path, author| file_view(&pack, &cache, &allowlist, path, author);
        Ok((files.shipped.into_iter().map(|(path, author)| view(path, author)).collect(), files.left_out.into_iter().map(|path| view(path, None)).collect()))
    };
    let (curseforge_shipped, curseforge_left_out) = decided(Platform::Curseforge)?;
    let (modrinth_shipped, modrinth_left_out) = decided(Platform::Modrinth)?;
    Ok(Preflight {
        problems: {
            let mut problems = publish::preflight(&pack, &cache, version, &mod_list.unhosted, mod_list.modrinth_error.as_deref(), &state.settings().identity());
            let pack_data = state.pack_data();
            let finished = |r: &publish::Release| r.version == version && unfinished(Some(&pack), &pack_data, r.clone()).is_none();
            // A finished release almost always has its tag, and preflight already reports that.
            if !publish::tag_exists(&pack, version) && publish::load_releases(&pack_data.join("releases.json")).iter().any(finished) {
                problems.push(format!("You already published {version}. Change the version to publish again."));
            }
            problems
        },
        warnings,
        untested: testrun::untested(&state.pack_data(), &pack),
        tag_taken: !version.is_empty() && publish::tag_exists(&pack, version),
        client: publish::default_client_names(&pack),
        server: publish::default_server_names(&pack),
        modrinth_file: pack.modrinth_pack_name("{version}"),
        curseforge_shipped,
        curseforge_left_out,
        modrinth_shipped,
        modrinth_left_out,
    })
}

#[tauri::command]
pub fn build_client_export(app: AppHandle, state: State<'_, AppState>) -> CmdResult<()> {
    let pack = state.pack().map_err(err)?;
    let out_dir = state.pack_data().join("builds").join(state.version(&pack));
    let cache_path = state.cache_path();
    state.jobs.start(&app, "client-export", move |log| async move {
        let path = export::client_export(&pack, &MetadataCache::load(&cache_path), &pack.version().unwrap_or_else(|| "dev".into()), &out_dir, &log)?;
        Ok(path.to_string_lossy().into_owned())
    })
}

#[tauri::command]
pub fn build_modrinth_export(app: AppHandle, state: State<'_, AppState>) -> CmdResult<()> {
    let pack = state.pack().map_err(err)?;
    let out_dir = state.pack_data().join("builds").join(state.version(&pack));
    let cache_path = state.cache_path();
    state.jobs.start(&app, "modrinth-export", move |log| async move {
        let sides = classify_cached(&cache_path, &pack).await?;
        let path = export::modrinth_export(&pack, &sides, &MetadataCache::load(&cache_path), &pack.version().unwrap_or_else(|| "dev".into()), &out_dir, &log)?;
        Ok(path.to_string_lossy().into_owned())
    })
}

#[tauri::command]
pub fn upload_release_server_pack(app: AppHandle, state: State<'_, AppState>, version: String) -> CmdResult<()> {
    let pack = state.pack().map_err(err)?;
    let paths = release_paths(&state);
    state.jobs.start(&app, "publish", move |log| async move { publish::upload_release_server_pack(&pack, &paths, &version, false, &log).await })
}

#[tauri::command]
pub fn replace_curseforge_file(app: AppHandle, state: State<'_, AppState>, version: String, file: publish::PackFile) -> CmdResult<()> {
    let pack = state.pack().map_err(err)?;
    let paths = release_paths(&state);
    state.jobs.start(&app, "publish", move |log| async move { publish::replace_curseforge_file(&pack, &paths, &version, file, &log).await })
}

#[tauri::command]
pub fn mark_server_pack_uploaded(state: State<'_, AppState>, version: String) -> CmdResult<()> {
    publish::mark_server_pack_uploaded(&state.pack_data(), &version).map_err(err)
}

#[tauri::command]
pub fn finish_release(app: AppHandle, state: State<'_, AppState>, version: String) -> CmdResult<()> {
    let pack = state.pack().map_err(err)?;
    let paths = release_paths(&state);
    let identity = state.settings().identity();
    state.jobs.start(&app, "publish", move |log| async move { publish::finish_release(&pack, &identity, &paths, &version, &log).await })
}

fn release_paths(state: &AppState) -> publish::ReleasePaths {
    publish::ReleasePaths { data_dir: state.data_dir.clone(), pack_data: state.pack_data(), cache_path: state.cache_path() }
}

#[tauri::command]
pub fn upload_release_to_modrinth(app: AppHandle, state: State<'_, AppState>, version: String) -> CmdResult<()> {
    let pack = state.pack().map_err(err)?;
    let pack_data = state.pack_data();
    state.jobs.start(&app, "publish", move |log| async move { publish::upload_release_to_modrinth(&pack, &pack_data, &version, &log).await })
}

#[derive(serde::Deserialize, Serialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum OverrideChoice {
    Ship,
    LeaveOut,
}

// Platforms ask for permission to share each file they don't host, so we record choices by path. The
// one pattern we write is for every version of one file, which you ask for in Publish once you've
// looked at a version. It replaces the choices about the versions it covers. Without a choice, the
// file is undecided again.
#[tauri::command]
pub fn set_override_choice(state: State<'_, AppState>, platform: Platform, paths: Vec<String>, choice: Option<OverrideChoice>) -> CmdResult<()> {
    let mut pack = state.pack().map_err(err)?;
    let (shipped, kept_off) = pack.config.override_choices_mut(platform);
    for (list, kind) in [(shipped, OverrideChoice::Ship), (kept_off, OverrideChoice::LeaveOut)] {
        list.retain(|entry| !paths.iter().any(|path| export::choice_covers(path, entry)));
        if choice == Some(kind) {
            list.extend(paths.iter().cloned());
            list.sort();
        }
    }
    pack.save_config().map_err(err)
}

/// A file a platform doesn't host, with what you'd want to know when you decide about it or review it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverrideFileView {
    path: String,
    name: Option<String>,
    license: Option<String>,
    open_license: bool,
    allowlisted: bool,
    /// The author whose permission ships it, when that's why the pack carries it.
    author: Option<String>,
    /// For a choice about every version of a file, the versions in the pack now.
    matches: Vec<String>,
    /// A pattern for this file's other versions, when the choice is for one file.
    any_version: Option<String>,
}

fn file_view(pack: &Pack, cache: &MetadataCache, allowlist: &Allowlist, path: String, author: Option<String>) -> OverrideFileView {
    let project = export::project_of(pack, cache, &path);
    let license = project.and_then(|p| p.license.clone()).filter(|l| !l.is_empty());
    let allowlisted = project.is_some_and(|p| allowlist.find(&p.name, p.authors.as_deref().unwrap_or_default()).is_some());
    let any_version = (!path.contains('*')).then(|| export::any_version_pattern(&path)).flatten();
    OverrideFileView { name: project.map(|p| p.name.clone()), open_license: license.as_deref().is_some_and(export::is_open_license), allowlisted, license, author, path, matches: Vec::new(), any_version }
}

// We describe a pattern by the first version it matches, since every version is the same project.
fn choice_view(pack: &Pack, cache: &MetadataCache, allowlist: &Allowlist, files: &export::OverrideFiles, choice: &str) -> OverrideFileView {
    if !choice.contains('*') {
        return file_view(pack, cache, allowlist, choice.to_string(), None);
    }
    let all = files.undecided.iter().chain(files.shipped.iter().map(|(path, _)| path)).chain(&files.left_out);
    let mut matches: Vec<String> = all.filter(|path| export::choice_covers(choice, path)).cloned().collect();
    matches.sort();
    let mut view = file_view(pack, cache, allowlist, matches.first().cloned().unwrap_or_default(), None);
    view.path = choice.to_string();
    view.matches = matches;
    view.any_version = None;
    view
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorPermission {
    author: String,
    files: Vec<OverrideFileView>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverrideChoices {
    shipped: Vec<OverrideFileView>,
    by_author: Vec<AuthorPermission>,
    kept_off: Vec<OverrideFileView>,
}

#[tauri::command(async)]
pub fn override_choices(state: State<'_, AppState>, platform: Platform) -> CmdResult<OverrideChoices> {
    let pack = state.pack().map_err(err)?;
    let cache = MetadataCache::load(&state.cache_path());
    let allowlist = if platform == Platform::Curseforge { Allowlist::load(&state.allowlist_path()) } else { Allowlist::default() };
    let files = export::override_files_for(&pack, &cache, platform).map_err(err)?;
    let view = |path: &String, author: Option<String>| file_view(&pack, &cache, &allowlist, path.clone(), author);
    let by_author = pack
        .config
        .allowed_authors(platform)
        .iter()
        .map(|author| AuthorPermission {
            author: author.clone(),
            files: files.shipped.iter().filter(|(_, by)| by.as_ref().is_some_and(|by| by.eq_ignore_ascii_case(author))).map(|(path, by)| view(path, by.clone())).collect(),
        })
        .collect();
    let (shipped, kept_off) = pack.config.override_choices(platform);
    let choice = |entry: &String| choice_view(&pack, &cache, &allowlist, &files, entry);
    Ok(OverrideChoices { shipped: shipped.iter().map(choice).collect(), by_author, kept_off: kept_off.iter().map(choice).collect() })
}

/// Looks up the authors, licenses and other versions of the files a platform doesn't host, and
/// CurseForge's list of the mods it allows in packs.
#[tauri::command]
pub async fn look_up_override_candidates(state: State<'_, AppState>) -> CmdResult<()> {
    let pack = state.pack().map_err(err)?;
    let cache_path = state.cache_path();
    let mut cache = MetadataCache::load(&cache_path);
    publish::look_up_override_candidates(&pack, &mut cache).await.map_err(err)?;
    cache.save(&cache_path).map_err(err)?;
    let skews = publish::look_up_modrinth_skews(&pack, &mut cache).await;
    cache.save(&cache_path).map_err(err)?;
    if pack.config.targets_curseforge() {
        Allowlist::refresh(&state.allowlist_path()).await.map_err(err)?;
    }
    let found_twins = skews.map_err(err)?;
    if !found_twins.is_empty() {
        modsync::set_modrinth_files(&pack.dir, &found_twins).await.map_err(err)?;
    }
    Ok(())
}

#[tauri::command]
pub fn set_modrinth_author(state: State<'_, AppState>, author: String, allowed: bool) -> CmdResult<()> {
    let mut pack = state.pack().map_err(err)?;
    let authors = &mut pack.config.modrinth.allowed_authors;
    authors.retain(|a| !a.eq_ignore_ascii_case(&author));
    if allowed {
        authors.push(author);
        authors.sort_by_key(|a| a.to_lowercase());
    }
    pack.save_config().map_err(err)
}

/// Lists Modrinth's own build of a file's version in the Modrinth pack, or stops listing it. Your mods
/// folder and the CurseForge pack keep CurseForge's build either way.
#[tauri::command]
pub async fn use_modrinth_build(state: State<'_, AppState>, path: String, using: bool) -> CmdResult<()> {
    let pack = state.pack().map_err(err)?;
    let build = if using {
        let entry = pack.lock.files.iter().find(|e| e.path == path).ok_or("That file isn't in the mod list any more.")?;
        let cache = MetadataCache::load(&state.cache_path());
        let skew = cache.skew(&publish::skew_key(&pack, entry)).flatten();
        Some(skew.and_then(|s| s.modrinth_build.clone()).ok_or("packraven no longer knows Modrinth's build of it. Reopen Publish to check again.")?.file)
    } else {
        None
    };
    modsync::set_modrinth_files(&pack.dir, &[(path, build)]).await.map_err(err)
}

#[tauri::command]
pub async fn modrinth_project_title(id: String) -> CmdResult<Option<String>> {
    Ok(crate::modrinth::project(id.trim()).await.map_err(err)?.map(|p| p.title))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishedTo {
    curseforge: bool,
    modrinth: bool,
    server_pack_missing: Option<publish::ServerPackUpload>,
    server_pack_page: Option<String>,
    modrinth_error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnfinishedRelease {
    version: String,
    display_name: Option<String>,
    server_pack_missing: Option<publish::ServerPackUpload>,
    /// The Modrinth pack packraven built but couldn't upload.
    modrinth_missing: Option<String>,
    modrinth_error: Option<String>,
    pending: Vec<publish::PendingStep>,
    /// Whether the mods changed since the release shipped, so tagging it now would record a different mod list.
    pack_changed: bool,
}

// Reads only releases.json and the builds folder, so Publish can call it on every visit without waiting on CurseForge.
#[tauri::command(async)]
pub fn unfinished_releases(state: State<'_, AppState>) -> Vec<UnfinishedRelease> {
    let pack_data = state.pack_data();
    let pack = state.pack().ok();
    publish::load_releases(&pack_data.join("releases.json")).into_iter().rev().filter_map(|release| unfinished(pack.as_ref(), &pack_data, release)).collect()
}

fn unfinished(pack: Option<&Pack>, pack_data: &std::path::Path, release: publish::Release) -> Option<UnfinishedRelease> {
    let server_pack_missing = publish::awaiting_server_pack(pack, pack_data, &release);
    let modrinth_missing = pack.and_then(|pack| publish::awaiting_modrinth(pack, pack_data, &release)).and_then(|path| path.file_name().map(|n| n.to_string_lossy().into_owned()));
    (server_pack_missing.is_some() || modrinth_missing.is_some() || !release.pending.is_empty()).then(|| UnfinishedRelease {
        pack_changed: pack.is_some_and(|pack| publish::changed_since(pack, &release)),
        version: release.version,
        display_name: release.display_name,
        modrinth_error: release.modrinth_error.filter(|_| modrinth_missing.is_some()),
        server_pack_missing,
        modrinth_missing,
        pending: release.pending,
    })
}

#[tauri::command]
pub fn published_to(state: State<'_, AppState>, version: String) -> Option<PublishedTo> {
    let pack_data = state.pack_data();
    let releases = publish::load_releases(&pack_data.join("releases.json"));
    let release = releases.iter().find(|r| r.version == version)?;
    Some(PublishedTo {
        curseforge: release.curseforge.is_some(),
        modrinth: release.modrinth.is_some(),
        server_pack_missing: publish::awaiting_server_pack(state.pack().ok().as_ref(), &pack_data, release),
        server_pack_page: release.curseforge.as_ref().and_then(|c| c.server_file_id.map(|id| curseforge::file_page(c.project_id, id))),
        modrinth_error: release.modrinth.is_none().then(|| release.modrinth_error.clone()).flatten(),
    })
}

async fn classify_cached(cache_path: &std::path::Path, pack: &Pack) -> Result<Vec<ModSide>> {
    let mut cache = MetadataCache::load(cache_path);
    let sides = sides::classify(pack, &mut cache).await?;
    cache.save(cache_path)?;
    Ok(sides)
}

async fn build_server(state_data: (PathBuf, PathBuf, PathBuf), pack: &Pack, log: &crate::jobs::Log) -> Result<PathBuf> {
    let (data_dir, pack_data, cache_path) = state_data;
    let version = pack.version().unwrap_or_else(|| "dev".into());
    let sides = classify_cached(&cache_path, pack).await?;
    export::server_pack(pack, &sides, &version, export::ServerMods::Bundled, &pack_data.join("builds").join(&version), &data_dir, log).await
}

#[tauri::command]
pub fn build_server_pack(app: AppHandle, state: State<'_, AppState>) -> CmdResult<()> {
    let pack = state.pack().map_err(err)?;
    let data = (state.data_dir.clone(), state.pack_data(), state.cache_path());
    state.jobs.start(&app, "server-pack", move |log| async move {
        let dir = build_server(data, &pack, &log).await?;
        let version = pack.version().unwrap_or_else(|| "dev".into());
        let out_dir = dir.parent().context("server pack folder has no parent")?;
        Ok(export::zip_server_pack(&pack, &version, out_dir, &log).await?.to_string_lossy().into_owned())
    })
}

const MODS_CHANGING: &str = "packraven is changing this pack's mods. Wait for it to finish, then try again.";

fn mods_changing(state: &AppState) -> bool {
    state.jobs.running().iter().any(|job| job == "mods")
}

#[tauri::command]
pub fn start_test(app: AppHandle, state: State<'_, AppState>, recopy_all: bool, connect: bool) -> CmdResult<()> {
    if state.test.server.lock().unwrap().is_some() {
        return Err("the test server is already running".into());
    }
    if mods_changing(&state) {
        return Err(MODS_CHANGING.into());
    }
    let settings = state.settings();
    // We write eula=true into the test server folder, so you must accept the EULA yourself first.
    if !settings.minecraft_eula_accepted {
        return Err("Accept the Minecraft EULA before you start a test server.".into());
    }
    if connect && crate::minecraft_auth::account().is_none() {
        return Err(crate::minecraft_commands::SIGN_IN_FIRST.into());
    }
    let pack = state.pack().map_err(err)?;
    let java = launch::java(&settings).map_err(err)?;
    let (data_dir, cache_path) = (state.data_dir.clone(), state.cache_path());
    let test_dir = state.pack_data().join("test-server");
    let natives = state.pack_data().join("natives");
    let tested = testrun::JoinRecord::new(&state.pack_data(), &pack).map_err(err)?;
    let run_args = settings.combined_run_args(&settings.instance_dir);
    let jobs = state.jobs.clone();
    let run = state.test.clone();
    let minecraft = state.minecraft.clone();
    let job_app = app.clone();
    state.jobs.start(&app, "test", move |log| async move {
        // We check the sign-in before building anything, so a lapsed one fails fast.
        let player = if connect { Some(crate::minecraft_commands::signed_in_player(&minecraft).await?) } else { None };
        if let Some(player) = &player {
            testrun::announce_player(&job_app, player);
        }
        let version = pack.version().unwrap_or_else(|| "dev".into());
        let sides = classify_cached(&cache_path, &pack).await?;
        let plan = export::test_server_plan(&pack, &sides, &version, &java, &data_dir, &log).await?;
        log.info(if recopy_all { "Recopying every file in the test server folder" } else { "Syncing changed files to the test server folder" });
        let report = testrun::sync_server_dir(plan, &pack, &test_dir, recopy_all, &log)?;
        let online = testrun::online_mode(&test_dir);
        testrun::announce_server_online(&job_app, online);
        if !online {
            log.info("This pack's server.properties turns online mode off, so the test server won't check accounts");
        }
        log.info(report.to_string());
        let (ready_tx, mut ready_rx) = watch::channel(false);
        let server_pack = Pack::open(&pack.dir)?;
        let (server_app, server_run) = (job_app.clone(), run.clone());
        jobs.start(&job_app, "server", move |server_log| async move {
            testrun::run_server(server_app, server_run, &server_pack, java, test_dir, run_args, ready_tx, tested, server_log).await
        })
        .map_err(anyhow::Error::msg)?;
        let Some(player) = player else { return Ok("Server starting".into()) };
        log.info("Waiting for the server to finish starting");
        while !*ready_rx.borrow() {
            if ready_rx.changed().await.is_err() {
                anyhow::bail!("the server stopped before it finished starting");
            }
        }
        let client_pack = Pack::open(&pack.dir)?;
        let client_app = job_app.clone();
        jobs.start(&job_app, "client", move |client_log| async move {
            testrun::run_client(client_app, run, &settings, &client_pack, natives, true, player, client_log).await
        })
        .map_err(anyhow::Error::msg)?;
        Ok("Server ready, client launching".into())
    })
}

#[tauri::command]
pub fn start_client(app: AppHandle, state: State<'_, AppState>, join: bool) -> CmdResult<()> {
    if mods_changing(&state) {
        return Err(MODS_CHANGING.into());
    }
    if crate::minecraft_auth::account().is_none() {
        return Err(crate::minecraft_commands::SIGN_IN_FIRST.into());
    }
    let settings = state.settings();
    let pack = state.pack().map_err(err)?;
    let natives = state.pack_data().join("natives");
    let run = state.test.clone();
    let minecraft = state.minecraft.clone();
    let job_app = app.clone();
    state.jobs.start(&app, "client", move |log| async move {
        let player = crate::minecraft_commands::signed_in_player(&minecraft).await?;
        testrun::announce_player(&job_app, &player);
        testrun::run_client(job_app, run, &settings, &pack, natives, join, player, log).await
    })
}

#[tauri::command]
pub async fn server_command(state: State<'_, AppState>, line: String) -> CmdResult<()> {
    let server = state.test.server.lock().unwrap().clone().ok_or("the server is not running")?;
    server.send(&line).await.map_err(err)
}

#[tauri::command]
pub fn kill_process(state: State<'_, AppState>, which: String) -> CmdResult<()> {
    let slot = if which == "server" { &state.test.server } else { &state.test.client };
    if let Some(process) = slot.lock().unwrap().as_ref() {
        process.kill();
    }
    Ok(())
}

#[tauri::command]
pub fn publish(app: AppHandle, state: State<'_, AppState>, options: PublishOptions) -> CmdResult<()> {
    let pack = state.pack().map_err(err)?;
    let announce = pack.config.discord.announce_on_approval && state.settings().webhook(&pack.dir).is_some();
    let context = PublishContext {
        announce,
        pack,
        identity: state.settings().identity(),
        data_dir: state.data_dir.clone(),
        pack_data: state.pack_data(),
        cache_path: state.cache_path(),
    };
    state.jobs.start(&app, "publish", move |log| publish::publish(context, options, log))
}

#[tauri::command]
pub fn update_checker_setup(state: State<'_, AppState>) -> CmdResult<Option<crate::update_checker::Setup>> {
    Ok(crate::update_checker::setup(&state.pack().map_err(err)?))
}

#[tauri::command]
pub async fn update_checker_problem(state: State<'_, AppState>) -> CmdResult<Option<String>> {
    Ok(crate::update_checker::problem(&state.pack().map_err(err)?).await)
}

#[tauri::command]
pub async fn add_to_update_checker(state: State<'_, AppState>, version: String) -> CmdResult<String> {
    let pack = state.pack().map_err(err)?;
    crate::update_checker::add_release(&pack, &state.pack_data().join("releases.json"), &version).await.map_err(err)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseStatus {
    version: String,
    published_at: String,
    announce: bool,
    announced_at: Option<String>,
    display_name: Option<String>,
    release_type: Option<String>,
    curseforge: Option<CurseforgeStatus>,
    modrinth: Option<ModrinthStatus>,
    /// The Modrinth pack packraven would upload, when the pack is on Modrinth but this release isn't yet.
    modrinth_missing: Option<String>,
    server_pack_missing: Option<publish::ServerPackUpload>,
    /// Whether the mods changed since the release shipped, so a server pack built now wouldn't match it.
    pack_changed: bool,
    /// Whether packraven still has the client pack it published, which replacing the client pack reuses.
    client_zip_present: bool,
    update_checker: Option<crate::update_checker::Entry>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurseforgeStatus {
    project_id: u64,
    file_id: u64,
    status: String,
    server_status: String,
    server_pack_linked: Option<bool>,
    /// What each file's status means for you; None when CurseForge returned no file.
    state: Option<curseforge::FileState>,
    server_state: Option<curseforge::FileState>,
    /// When each file went up to CurseForge, so you can tell how long a review has taken.
    uploaded: Option<String>,
    server_uploaded: Option<String>,
    /// Each file's page in CurseForge's author dashboard, where you read its review and can delete it.
    page: String,
    server_page: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModrinthStatus {
    status: String,
    url: String,
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Attention {
    file: &'static str,
    state: curseforge::FileState,
}

/// Returns the first of a release's CurseForge files that waits on you, so Publish can point you at Releases.
#[tauri::command]
pub async fn release_attention(state: State<'_, AppState>, version: String) -> CmdResult<Option<Attention>> {
    let Some(release) = publish::load_releases(&state.pack_data().join("releases.json")).into_iter().find(|r| r.version == version) else { return Ok(None) };
    let recent = release.uploaded_to_curseforge_recently();
    let Some(published) = release.curseforge else { return Ok(None) };
    for (file, id) in [("client", Some(published.file_id)), ("server", published.server_file_id)] {
        let Some(id) = id else { continue };
        let (_, found) = describe_file(&curseforge::file(published.project_id, id).await, recent);
        if let Some(found) = found.filter(|s| s.needs_you()) {
            return Ok(Some(Attention { file, state: found }));
        }
    }
    Ok(None)
}

// CurseForge's API answers for files under review too, so a missing file was deleted, unless it went
// up minutes ago and CurseForge is still processing it. A failed lookup tells us nothing either way.
fn describe_file(lookup: &Result<Option<curseforge::CfFile>>, recent: bool) -> (String, Option<curseforge::FileState>) {
    match lookup {
        Ok(Some(file)) => (curseforge::file_status_name(file.file_status).into(), Some(curseforge::file_state(file.file_status))),
        Ok(None) if recent => ("Processing".into(), Some(curseforge::FileState::Processing)),
        Ok(None) => ("Gone from CurseForge".into(), Some(curseforge::FileState::Deleted)),
        Err(_) => ("Couldn't check".into(), None),
    }
}

#[tauri::command]
pub async fn releases(state: State<'_, AppState>) -> CmdResult<Vec<ReleaseStatus>> {
    let mut result = Vec::new();
    let pack = state.pack().ok();
    let pack_data = state.pack_data();
    let releases_path = pack_data.join("releases.json");
    let mut releases = publish::load_releases(&releases_path);
    let mut adopted = false;
    for release in releases.iter_mut().rev() {
        let curseforge = match &release.curseforge {
            Some(published) => {
                let file_lookup = curseforge::file(published.project_id, published.file_id).await;
                let file = file_lookup.as_ref().ok().cloned().flatten();
                let linked_server = file.as_ref().and_then(|f| f.server_pack_file_id);
                // A server pack you uploaded yourself shows up as the client pack's server pack once CurseForge makes the client pack public.
                if let (None, Some(id)) = (published.server_file_id, linked_server) {
                    publish::adopt_server_pack(release, id);
                    adopted = true;
                }
                let published = release.curseforge.as_ref().expect("checked above");
                let server_lookup = match published.server_file_id {
                    Some(id) => Some(curseforge::file(published.project_id, id).await),
                    None => None,
                };
                let recent = release.uploaded_to_curseforge_recently();
                let (status, state) = describe_file(&file_lookup, recent);
                let (server_status, server_state) = match (&server_lookup, published.server_pack_uploaded_by_you) {
                    (Some(lookup), _) => describe_file(lookup, recent),
                    (None, true) => ("Uploaded by you".into(), None),
                    (None, false) => ("Not uploaded".into(), None),
                };
                let server_file = server_lookup.as_ref().and_then(|l| l.as_ref().ok()).and_then(Option::as_ref);
                let uploaded = |lookup: Option<&Result<Option<curseforge::CfFile>>>| lookup.and_then(|l| l.as_ref().ok()).and_then(Option::as_ref).map(|f| f.file_date.clone()).filter(|d| !d.is_empty());
                Some(CurseforgeStatus {
                    project_id: published.project_id,
                    file_id: published.file_id,
                    status,
                    server_status,
                    server_pack_linked: published.server_file_id.and_then(|server_id| {
                        linked_server.map(|id| id == server_id).or_else(|| server_file.and_then(|f| f.is_server_pack))
                    }),
                    state,
                    server_state,
                    uploaded: uploaded(Some(&file_lookup)),
                    server_uploaded: uploaded(server_lookup.as_ref()),
                    page: curseforge::file_page(published.project_id, published.file_id),
                    server_page: published.server_file_id.map(|id| curseforge::file_page(published.project_id, id)),
                })
            }
            None => None,
        };
        let modrinth = match &release.modrinth {
            Some(published) => {
                let status = match crate::modrinth::version(&published.version_id).await {
                    Ok(Some(version)) => version.status.as_deref().map_or_else(|| "Listed".to_string(), capitalized),
                    Ok(None) => "Deleted".into(),
                    Err(_) => "Unknown".into(),
                };
                Some(ModrinthStatus { status, url: format!("https://modrinth.com/project/{}/version/{}", published.project_id, published.version_id) })
            }
            None => None,
        };
        result.push(ReleaseStatus {
            modrinth_missing: pack.as_ref().and_then(|pack| publish::awaiting_modrinth(pack, &pack_data, release)).and_then(|path| path.file_name().map(|n| n.to_string_lossy().into_owned())),
            server_pack_missing: publish::awaiting_server_pack(pack.as_ref(), &pack_data, release),
            pack_changed: pack.as_ref().is_some_and(|pack| publish::changed_since(pack, release)),
            client_zip_present: pack.as_ref().is_some_and(|pack| pack_data.join("builds").join(&release.version).join(pack.client_zip_name(&release.version)).exists()),
            display_name: release.display_name.clone(),
            release_type: release.release_type.clone(),
            version: release.version.clone(),
            published_at: release.published_at.clone(),
            announce: release.announce,
            announced_at: release.announced_at.clone(),
            update_checker: release.update_checker.clone(),
            curseforge,
            modrinth,
        });
    }
    if adopted {
        publish::save_releases(&releases_path, &releases).map_err(err)?;
    }
    Ok(result)
}

#[tauri::command]
pub fn output_dir(state: State<'_, AppState>) -> String {
    state.pack_data().join("builds").to_string_lossy().into_owned()
}
