use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::changelog;
use crate::curseforge;
use crate::export;
use crate::modrinth;
use crate::git;
use crate::jobs::Log;
use crate::git::Identity;
use crate::pack::{version_problem, LockEntry, ModLoader, ModrinthFile, Pack, Platform, CHANGELOG_ARCHIVE};
use crate::{links, twins};
use crate::sides::{self, MetadataCache};
use crate::modsync;
use crate::update_checker::{self, UpdateType};

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", from = "StoredRelease")]
pub struct Release {
    pub version: String,
    /// The display name the client pack shipped under, which a later Modrinth upload reuses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// The release type it shipped as, which a later Modrinth upload reuses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub curseforge: Option<CurseforgeRelease>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modrinth: Option<ModrinthRelease>,
    /// Why the Modrinth upload failed, kept until a later upload succeeds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modrinth_error: Option<String>,
    pub published_at: String,
    pub announce: bool,
    pub announced_at: Option<String>,
    /// The steps after the uploads that haven't happened yet, in order. Releases from before we kept these have none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub pending: Vec<PendingStep>,
    /// Identifies the mods that shipped, so finishing the release later can tell you the pack changed since.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mods_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update_checker: Option<update_checker::Entry>,
}

impl Release {
    /// Returns whether we uploaded a CurseForge file for this release in the last 15 minutes, which
    /// CurseForge's API may not find yet.
    pub fn uploaded_to_curseforge_recently(&self) -> bool {
        let Some(curseforge) = &self.curseforge else { return false };
        let at = curseforge.last_upload.as_deref().unwrap_or(&self.published_at);
        chrono::DateTime::parse_from_rfc3339(at).is_ok_and(|at| chrono::Utc::now().signed_duration_since(at) < chrono::Duration::minutes(15))
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", tag = "step")]
pub enum PendingStep {
    Tag { message: String },
    Push,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CurseforgeRelease {
    pub project_id: u64,
    pub file_id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_file_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unsent_server_pack: Option<UnsentServerPack>,
    /// You uploaded the server pack yourself. CurseForge only links it once the client pack is public, so until
    /// then we know of no server file, but mustn't offer to upload it again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub server_pack_uploaded_by_you: bool,
    /// When we last uploaded a file for this release, since CurseForge's API doesn't find a file for its
    /// first few minutes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_upload: Option<String>,
}

impl CurseforgeRelease {
    /// Returns whether the server pack still needs uploading, as far as we know.
    pub fn awaits_server_pack(&self) -> bool {
        self.server_file_id.is_none() && !self.server_pack_uploaded_by_you
    }
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct UnsentServerPack {
    pub names: UploadNames,
    pub error: String,
    #[serde(default)]
    pub too_large: bool,
}

/// What you need to upload a release's server pack yourself, after packraven's upload failed.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerPackUpload {
    /// The zip packraven built, if it's still in the builds folder.
    pub zip: Option<PathBuf>,
    pub upload_page: String,
    pub display_name: Option<String>,
    pub file_name: Option<String>,
    pub error: Option<String>,
    /// Whether packraven can try the upload again.
    pub retryable: bool,
    /// Whether a retry builds the server pack again first, because the zip is gone or was too large.
    pub rebuild: bool,
    /// Whether the mods changed since the release shipped, so a rebuilt server pack wouldn't match it.
    pub pack_changed: bool,
    /// The release's tag, which you check out to rebuild a server pack that's gone from the builds folder.
    pub tag: String,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ModrinthRelease {
    pub project_id: String,
    pub version_id: String,
}

// packraven 0.2.1 and earlier only published to CurseForge, and kept its IDs at the top level.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredRelease {
    version: String,
    display_name: Option<String>,
    release_type: Option<String>,
    curseforge: Option<CurseforgeRelease>,
    modrinth: Option<ModrinthRelease>,
    #[serde(default)]
    modrinth_error: Option<String>,
    project_id: Option<u64>,
    file_id: Option<u64>,
    server_file_id: Option<u64>,
    published_at: String,
    #[serde(default)]
    announce: bool,
    #[serde(default)]
    announced_at: Option<String>,
    #[serde(default)]
    pending: Vec<PendingStep>,
    #[serde(default)]
    mods_digest: Option<String>,
    #[serde(default)]
    update_checker: Option<update_checker::Entry>,
}

impl From<StoredRelease> for Release {
    fn from(stored: StoredRelease) -> Self {
        let legacy = match (stored.project_id, stored.file_id) {
            (Some(project_id), Some(file_id)) => Some(CurseforgeRelease { project_id, file_id, server_file_id: stored.server_file_id, unsent_server_pack: None, server_pack_uploaded_by_you: false, last_upload: None }),
            _ => None,
        };
        Self {
            version: stored.version,
            display_name: stored.display_name,
            release_type: stored.release_type,
            curseforge: stored.curseforge.or(legacy),
            modrinth: stored.modrinth,
            modrinth_error: stored.modrinth_error,
            published_at: stored.published_at,
            announce: stored.announce,
            announced_at: stored.announced_at,
            pending: stored.pending,
            mods_digest: stored.mods_digest,
            update_checker: stored.update_checker,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishOptions {
    pub version: String,
    pub message: String,
    pub push: bool,
    #[serde(default)]
    pub client: UploadNames,
    #[serde(default)]
    pub server: UploadNames,
    /// How the update checker describes the release, or None to leave it out of the update checker.
    #[serde(default)]
    pub update_checker: Option<UpdateType>,
}

#[derive(Serialize, Deserialize, Default, Debug, PartialEq, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct UploadNames {
    pub display_name: String,
    pub file_name: String,
}

pub fn load_releases(path: &Path) -> Vec<Release> {
    std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

pub fn save_releases(path: &Path, releases: &[Release]) -> Result<()> {
    std::fs::write(path, serde_json::to_string_pretty(releases)?)?;
    Ok(())
}

const VERSION_TOKEN: &str = "{version}";
// Windows rejects these in file names, and players download the packs onto Windows.
const UNSAFE_FILE_NAME_CHARS: [char; 9] = ['/', '\\', ':', '*', '?', '"', '<', '>', '|'];

pub fn default_client_names(pack: &Pack) -> UploadNames {
    UploadNames { display_name: format!("{} {VERSION_TOKEN}", pack.name()), file_name: pack.client_zip_name(VERSION_TOKEN) }
}

pub fn default_server_names(pack: &Pack) -> UploadNames {
    UploadNames { display_name: format!("Server Pack {VERSION_TOKEN}"), file_name: pack.server_zip_name(VERSION_TOKEN) }
}

fn upload_names(title: &str, overrides: &UploadNames, defaults: UploadNames, version: &str) -> Result<UploadNames> {
    let resolve = |name: &str, default: String| Some(name.trim()).filter(|n| !n.is_empty()).map_or(default, String::from).replace(VERSION_TOKEN, version);
    let display_name = resolve(&overrides.display_name, defaults.display_name);
    let mut file_name = resolve(&overrides.file_name, defaults.file_name);
    if file_name.contains(UNSAFE_FILE_NAME_CHARS) {
        bail!("The {title} file name can't contain any of / \\ : * ? \" < > |.");
    }
    if file_name.eq_ignore_ascii_case(".zip") {
        bail!("The {title} file name needs a name before .zip.");
    }
    if !file_name.to_ascii_lowercase().ends_with(".zip") {
        file_name.push_str(".zip");
    }
    Ok(UploadNames { display_name, file_name })
}

pub fn preflight_warnings(pack: &Pack, cache: &mut MetadataCache) -> Vec<String> {
    let disabled: Vec<&str> = pack.lock.files.iter().filter_map(|e| e.path.strip_suffix(".disabled")?.split_once('/').map(|(_, name)| name)).collect();
    let mut warnings = match disabled[..] {
        [] => vec![],
        [one] => vec![format!("{one} is disabled, so packraven will leave it out of the export.")],
        _ => vec![format!("{} files are disabled, so packraven will leave them out of the export: {}.", disabled.len(), disabled.join(", "))],
    };
    if pack.version().is_some() && !pack.has_bcc() {
        warnings.push("Better Compatibility Checker isn't in the mods folder, so the pack won't show its version in game.".into());
    }
    // We only know the tags we've cached, which is every mod once you've opened Server.
    for m in sides::classify_known(pack, cache).iter().filter(|m| m.breaks_server()) {
        let verb = if m.required_by.len() == 1 { "requires" } else { "require" };
        warnings.push(format!("{} is left off the server, but {} {verb} it, so the server pack won't start. Keep it on the server in Server.", m.name, sides::list(&m.required_by)));
    }
    warnings
}

/// Returns the git tag for a release. A version like v4.1.5 is its own tag, and the rest get a v.
pub fn release_tag(version: &str) -> String {
    // We find the last release by tags matching v*, so every tag has to start with a lowercase v.
    let already_tagged = version.strip_prefix('v').is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()));
    if already_tagged { version.to_string() } else { format!("v{version}") }
}

/// Returns the version a release tag belongs to. Releases we published record their version, and
/// for older tags we drop the v we would have added.
pub fn version_of_tag(tag: &str, releases: &[Release]) -> String {
    releases.iter().find(|r| release_tag(&r.version) == tag).map_or_else(|| tag.strip_prefix('v').unwrap_or(tag).to_string(), |r| r.version.clone())
}

pub fn tag_exists(pack: &Pack, version: &str) -> bool {
    git::run(&pack.dir, &["rev-parse", "-q", "--verify", &format!("refs/tags/{}", release_tag(version))]).is_ok()
}

pub fn preflight(pack: &Pack, cache: &MetadataCache, version: &str, unknown: &[String], modrinth_error: Option<&str>, identity: &Identity) -> Vec<String> {
    let mut problems = Vec::new();
    let config = &pack.config;
    if !config.targets_curseforge() && !config.targets_modrinth() {
        problems.push("Set a CurseForge or Modrinth project ID in Pack settings.".into());
    }
    if config.targets_curseforge() && curseforge::upload_token().is_none() {
        problems.push("Add a CurseForge upload token in App settings.".into());
    }
    if config.targets_modrinth() && modrinth::token().is_none() {
        problems.push("Add a Modrinth token in App settings.".into());
    }
    if curseforge::core_key().is_none() {
        problems.push("This packraven build has no CurseForge API key. Build it with PACKRAVEN_CF_API_KEY set.".into());
    }
    if pack.version().is_none() {
        problems.push(if pack.has_bcc() {
            "Better Compatibility Checker has no config yet, so the pack has no version. Create it in Changes.".into()
        } else {
            "Install Better Compatibility Checker in Changes. packraven keeps the pack version in BCC's config.".into()
        });
    } else if let Some(problem) = version_problem(version) {
        problems.push(format!("{problem} Change the version to publish."));
    } else if tag_exists(pack, version) {
        problems.push(format!("You already released {version} (tag {} exists). Change the version to publish a new release.", release_tag(version)));
    }
    let hosts = config.hosts();
    match unknown {
        [] => {}
        [path] => problems.push(format!("{path} isn't on {hosts}. In Changes, add it to Git or move it to the trash.")),
        paths => problems.push(format!("{} files aren't on {hosts}. In Changes, add them to Git or move them to the trash.", paths.len())),
    }
    problems.extend(export::duplicate_mods_problem(pack));
    // The commit comes after the upload, which you can't take back, so we check git can make it first.
    if !identity.is_usable(&pack.dir) {
        problems.push("Set your commit name and email in App settings.".into());
    }
    if config.targets_modrinth() {
        if let Some(error) = modrinth_error {
            problems.push(format!("Couldn't check which files Modrinth hosts: {error}"));
        }
    }
    for platform in [Platform::Curseforge, Platform::Modrinth].into_iter().filter(|p| config.targets(*p)) {
        let name = platform.name();
        match export::not_on(pack, cache, platform).as_deref() {
            Ok([]) => {}
            Ok([path]) => problems.push(format!("{path} isn't on {name}. Include it in the {name} pack or leave it out, below.")),
            Ok(paths) => problems.push(format!("{} files aren't on {name}. Include them in the {name} pack or leave them out, below.", paths.len())),
            Err(error) => problems.push(format!("Couldn't check which files the {name} pack would carry: {error:#}")),
        }
    }
    problems
}

/// Fetches the authors and licenses of the files a platform doesn't host, which we don't keep for every mod.
pub async fn look_up_override_candidates(pack: &Pack, cache: &mut MetadataCache) -> Result<()> {
    let mut keys = Vec::new();
    for platform in [Platform::Curseforge, Platform::Modrinth].into_iter().filter(|p| pack.config.targets(*p)) {
        keys.extend(export::not_on(pack, cache, platform)?.iter().filter_map(|path| pack.lock.files.iter().find(|e| e.path == *path)?.key()));
    }
    cache.ensure_names(&keys).await
}

pub fn skew_key(pack: &Pack, entry: &LockEntry) -> String {
    format!("{}:{}:{}", entry.sha1, pack.lock.minecraft.as_deref().unwrap_or_default(), pack.lock.loader.as_ref().map(ToString::to_string).unwrap_or_default())
}

/// Looks up which versions of each mod Modrinth has, for the CurseForge files it doesn't host.
/// Returns the files whose Modrinth upload turned out to hold the same contents, which the caller
/// records as twins.
pub async fn look_up_modrinth_skews(pack: &Pack, cache: &mut MetadataCache) -> Result<Vec<(String, Option<ModrinthFile>)>> {
    let Some(target) = twins::Target::of(&pack.lock).filter(|_| pack.config.targets_modrinth() && curseforge::core_key().is_some()) else { return Ok(Vec::new()) };
    let undecided = export::not_on(pack, cache, Platform::Modrinth)?;
    let wanting: Vec<&LockEntry> = pack.lock.files.iter().filter(|e| e.curseforge.is_some() && undecided.contains(&e.path) && cache.skew(&skew_key(pack, e)).is_none()).collect();
    let mut found_twins = Vec::new();
    if wanting.is_empty() {
        return Ok(found_twins);
    }
    let projects = curseforge::mods(&wanting.iter().filter_map(|e| e.curseforge_project()).collect::<Vec<_>>()).await?;
    let loader_type = curseforge::loader_type(&pack.lock.mod_loader()?);
    let cache_dir = pack.cache_dir()?;
    for entry in wanting {
        let Some(project) = entry.curseforge_project().and_then(|id| projects.get(&id)) else { continue };
        let loader = entry.path.starts_with("mods/").then_some(loader_type);
        let skew = links::skew(&target, loader, pack.config.updates.channel, &pack.dir, &cache_dir, entry, project).await?;
        // The twin search waits a day before it looks at a missed file again, and an older packraven
        // may have missed this one by stricter rules.
        if let Some(build) = skew.as_ref().and_then(|s| s.modrinth_build.as_ref()).filter(|b| b.differences.is_empty()) {
            found_twins.push((entry.path.clone(), Some(build.file.clone())));
            continue;
        }
        cache.remember_skew(skew_key(pack, entry), skew);
    }
    Ok(found_twins)
}

pub struct PublishContext {
    pub pack: Pack,
    pub identity: Identity,
    pub data_dir: PathBuf,
    pub pack_data: PathBuf,
    pub cache_path: PathBuf,
    pub announce: bool,
}

pub async fn publish(ctx: PublishContext, options: PublishOptions, log: Log) -> Result<String> {
    let PublishContext { pack, identity, data_dir, pack_data, cache_path, announce } = ctx;
    let version = options.version.trim().to_string();
    let _guard = modsync::MOD_LIST_GUARD.lock().await;
    let refresh = modsync::refresh_mod_list(&pack.dir).await?;
    let mut cache = MetadataCache::load(&cache_path);
    // The authors you allowed decide some files, so we need every undecided file's authors first.
    look_up_override_candidates(&pack, &mut cache).await?;
    cache.save(&cache_path)?;
    let problems = preflight(&pack, &cache, &version, &refresh.unknown, refresh.modrinth_error.as_deref(), &identity);
    if !problems.is_empty() {
        bail!("{}", problems.join("\n"));
    }
    if load_releases(&pack_data.join("releases.json")).iter().any(|r| r.version == version) {
        bail!("You already published {version}. Finish it from Publish if it's unfinished, or change the version.");
    }
    let pack = Pack::open(&pack.dir)?;
    let client_names = upload_names("client pack", &options.client, default_client_names(&pack), &version)?;
    let server_names = upload_names("server pack", &options.server, default_server_names(&pack), &version)?;

    pack.set_version(&version)?;
    log.info(format!("Set the pack version to {version}"));
    let out_dir = pack_data.join("builds").join(&version);
    let client_zip = pack.config.targets_curseforge().then(|| export::client_export(&pack, &cache, &version, &out_dir, &log)).transpose()?;

    let sides = sides::classify(&pack, &mut cache).await?;
    let baseline_path = pack_data.join(changelog::BASELINE_FILE);
    let diff = changelog::mod_diff(&pack, &pack.lock, &mut cache, &baseline_path).await?;
    cache.save(&cache_path)?;
    let modrinth_pack = pack.config.targets_modrinth().then(|| export::modrinth_export(&pack, &sides, &cache, &version, &out_dir, &log)).transpose()?;
    let notes = changelog::render(&changelog::read_notes(&pack), &diff);

    let message = Some(options.message.trim()).filter(|m| !m.is_empty()).map_or_else(|| format!("Release {version}"), String::from);
    let releases_path = pack_data.join("releases.json");
    let release_type = pack.config.curseforge.release_type.clone();
    let mut release = Release {
        version: version.clone(),
        display_name: Some(client_names.display_name.clone()),
        release_type: Some(release_type.clone()),
        curseforge: None,
        modrinth: None,
        // If packraven stops before the Modrinth upload, finishing the release offers it like a failed one.
        modrinth_error: modrinth_pack.as_ref().map(|_| STOPPED_EARLY.to_string()),
        published_at: chrono::Utc::now().to_rfc3339(),
        announce,
        announced_at: None,
        pending: [Some(PendingStep::Tag { message }), options.push.then_some(PendingStep::Push)].into_iter().flatten().collect(),
        mods_digest: Some(mods_digest(&pack)),
        update_checker: options.update_checker.filter(|_| update_checker::repo(&pack).is_some()).map(update_checker::Entry::new),
    };

    if let Some(client_zip) = client_zip {
        export::server_pack(&pack, &sides, &version, export::ServerMods::DownloadedOnStart, &out_dir, &data_dir, &log).await?;
        let server_zip = export::zip_server_pack(&pack, &version, &out_dir, &log).await?;
        curseforge::check_upload_size(&client_zip, "client pack")?;
        curseforge::check_upload_size(&server_zip, "server pack")?;
        let file_id = upload_client_pack(&pack, &client_zip, client_names.clone(), &notes, &log).await?;
        // We record the release the moment CurseForge has it, so a publish that stops later leaves a release to
        // finish rather than one we'd upload twice.
        let unsent = UnsentServerPack { names: server_names.clone(), error: STOPPED_EARLY.into(), too_large: false };
        let mut curseforge = CurseforgeRelease { project_id: pack.config.curseforge.project_id, file_id, server_file_id: None, unsent_server_pack: Some(unsent), server_pack_uploaded_by_you: false, last_upload: Some(chrono::Utc::now().to_rfc3339()) };
        release.curseforge = Some(curseforge.clone());
        record(&pack, &releases_path, &release, &notes, &baseline_path)?;
        if let Err(error) = upload_server_pack(&mut curseforge, &server_zip, server_names, &release_type, &log).await {
            log.stream("err", &format!("Couldn't upload the server pack: {error:#}"));
        }
        release.curseforge = Some(curseforge);
        save_release(&releases_path, &release)?;
    }
    let server_failure = release.curseforge.as_ref().and_then(|c| c.unsent_server_pack.as_ref()).map(|unsent| unsent.error.clone());
    // A CurseForge release already happened by now, so a failed Modrinth upload mustn't keep us from finishing it.
    let modrinth_failure = match &modrinth_pack {
        Some(path) => match upload_to_modrinth(&pack, &version, path, &client_names.display_name, &release_type, &notes, &log).await {
            Ok(uploaded) => {
                release.modrinth = Some(uploaded);
                release.modrinth_error = None;
                None
            }
            Err(error) if release.curseforge.is_some() => {
                release.modrinth_error = Some(format!("{error:#}"));
                Some(error)
            }
            Err(error) => return Err(error),
        },
        None => None,
    };
    if release.curseforge.is_some() {
        save_release(&releases_path, &release)?;
    } else {
        record(&pack, &releases_path, &release, &notes, &baseline_path)?;
    }
    let summary = [
        release.curseforge.as_ref().map(|c| match c.server_file_id {
            Some(server_file_id) => format!("CurseForge file {}, server pack {server_file_id}", c.file_id),
            None => format!("CurseForge file {}, without its server pack", c.file_id),
        }),
        release.modrinth.as_ref().map(|m| format!("Modrinth version {}", m.version_id)),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("; ");

    run_pending(&pack, &identity, &releases_path, &mut release, &log).await?;
    // The release happened, so the job succeeds, and Publish finds the missing uploads from releases.json.
    let server_failure = server_failure.map(|error| format!(" The server pack upload failed, so upload it yourself as an additional file on CurseForge: {error}"));
    let modrinth_failure = modrinth_failure.map(|error| format!(" The Modrinth upload failed, so upload it again from Releases: {error:#}"));
    Ok(format!("Published {version}: {summary}.{}{}", server_failure.unwrap_or_default(), modrinth_failure.unwrap_or_default()))
}

// Shown for an upload packraven never got to, as when the app closed mid-publish.
const STOPPED_EARLY: &str = "packraven stopped before it finished this upload";

fn save_release(releases_path: &Path, release: &Release) -> Result<()> {
    let mut releases = load_releases(releases_path);
    match releases.iter_mut().find(|r| r.version == release.version) {
        Some(stored) => *stored = release.clone(),
        None => releases.push(release.clone()),
    }
    save_releases(releases_path, &releases)?;
    crate::announcer::check_soon();
    Ok(())
}

// Recording a release also archives its notes and clears changelog.md for the next one.
fn record(pack: &Pack, releases_path: &Path, release: &Release, notes: &str, baseline_path: &Path) -> Result<()> {
    save_release(releases_path, release)?;
    let archive = pack.dir.join(CHANGELOG_ARCHIVE);
    std::fs::create_dir_all(&archive)?;
    std::fs::write(archive.join(format!("{}.md", release.version)), notes)?;
    changelog::write_notes(pack, "")?;
    let _ = std::fs::remove_file(baseline_path);
    Ok(())
}

/// Returns a digest of the jars and files the pack ships, which ignores lockfile changes that don't change a file.
pub fn mods_digest(pack: &Pack) -> String {
    use sha1::{Digest, Sha1};
    let mut files: Vec<(&str, &str)> = pack.lock.files.iter().map(|e| (e.path.as_str(), e.sha1.as_str())).collect();
    files.sort_unstable();
    let mut hasher = Sha1::new();
    for (path, sha1) in files {
        hasher.update(format!("{path}\t{sha1}\n"));
    }
    hex::encode(hasher.finalize())
}

/// Returns whether the pack's mods changed since the release shipped. Releases from before we kept a digest never count as changed.
pub fn changed_since(pack: &Pack, release: &Release) -> bool {
    release.mods_digest.as_ref().is_some_and(|digest| *digest != mods_digest(pack))
}

// We save after each step, so a step that fails stays pending and finishing the release picks up from it.
async fn run_pending(pack: &Pack, identity: &Identity, releases_path: &Path, release: &mut Release, log: &Log) -> Result<()> {
    let tag = release_tag(&release.version);
    while let Some(step) = release.pending.first().cloned() {
        match step {
            // A publish that stopped right after tagging left the tag, so we don't commit a second time.
            PendingStep::Tag { .. } if tag_exists(pack, &release.version) => log.info(format!("{tag} already exists, so packraven left it as it is")),
            // packraven built the packs from disk, so we commit everything, and the tag records what shipped.
            PendingStep::Tag { message } => {
                git::run(&pack.dir, &["add", "-A"])?;
                for command in [&["commit", "--allow-empty", "-m", &message][..], &["tag", "-a", &tag, "-m", &message]] {
                    let mut args = identity.config_args();
                    args.extend(command.iter().map(|a| a.to_string()));
                    git::run(&pack.dir, &args.iter().map(String::as_str).collect::<Vec<_>>())?;
                }
                log.info(format!("Committed \"{message}\" and tagged {tag}"));
            }
            PendingStep::Push => {
                crate::gitops::push(&pack.dir, log)
                    .await
                    .with_context(|| format!("packraven published {} and tagged {tag}, but couldn't push. Once you've fixed this, finish the release from Publish", release.version))?;
                log.info("Pushed the release commit and tag");
            }
        }
        release.pending.remove(0);
        save_release(releases_path, release)?;
    }
    Ok(())
}

async fn upload_client_pack(pack: &Pack, zip: &Path, names: UploadNames, notes: &str, log: &Log) -> Result<u64> {
    let game_versions = curseforge::game_version_ids(pack.lock.minecraft()?, pack.lock.mod_loader()?.name()).await?;
    log.info(format!("Uploading {} as {}", zip.display(), names.file_name));
    let file_id = curseforge::upload(
        pack.config.curseforge.project_id,
        zip,
        names.file_name,
        json!({
            "changelog": notes,
            "changelogType": "markdown",
            "displayName": names.display_name,
            "gameVersions": game_versions,
            "releaseType": pack.config.curseforge.release_type,
            "isMarkedForManualRelease": pack.config.curseforge.manual_release,
        }),
    )
    .await?;
    log.info(format!("Uploaded the client pack as file {file_id}"));
    Ok(file_id)
}

// Either way the outcome goes on the release: the server file, or why it isn't there.
async fn upload_server_pack(curseforge: &mut CurseforgeRelease, zip: &Path, names: UploadNames, release_type: &str, log: &Log) -> Result<u64> {
    log.info(format!("Uploading {} as {}", zip.display(), names.file_name));
    // We put the release notes on the client pack only; repeating them here just clutters the file list.
    let metadata = json!({
        "changelog": "",
        "displayName": names.display_name,
        "parentFileID": curseforge.file_id,        "releaseType": release_type,
    });
    match curseforge::upload(curseforge.project_id, zip, names.file_name.clone(), metadata).await {
        Ok(server_file_id) => {
            log.info(format!("Uploaded the server pack as file {server_file_id}"));
            curseforge.server_file_id = Some(server_file_id);
            curseforge.unsent_server_pack = None;
            curseforge.last_upload = Some(chrono::Utc::now().to_rfc3339());
            Ok(server_file_id)
        }
        Err(error) => {
            let too_large = error.downcast_ref::<curseforge::UploadTooLarge>().is_some();
            curseforge.unsent_server_pack = Some(UnsentServerPack { names, error: format!("{error:#}"), too_large });
            Err(error)
        }
    }
}

/// Returns what you need to upload a release's server pack yourself, when CurseForge has the client
/// pack but packraven doesn't know of a server pack for it.
pub fn awaiting_server_pack(pack: Option<&Pack>, pack_data: &Path, release: &Release) -> Option<ServerPackUpload> {
    let curseforge = release.curseforge.as_ref().filter(|c| c.awaits_server_pack())?;
    let unsent = curseforge.unsent_server_pack.as_ref();
    let zip = pack.map(|pack| server_zip(pack, pack_data, &release.version)).filter(|zip| zip.exists());
    Some(ServerPackUpload {
        retryable: pack.is_some(),
        rebuild: zip.is_none() || unsent.is_some_and(|u| u.too_large),
        pack_changed: pack.is_some_and(|pack| changed_since(pack, release)),
        zip,
        upload_page: curseforge::additional_file_page(curseforge.project_id, curseforge.file_id),
        display_name: unsent.map(|u| u.names.display_name.clone()),
        file_name: unsent.map(|u| u.names.file_name.clone()),
        error: unsent.map(|u| u.error.clone()),
        tag: release_tag(&release.version),
    })
}

fn server_zip(pack: &Pack, pack_data: &Path, version: &str) -> PathBuf {
    pack_data.join("builds").join(version).join(pack.server_zip_name(version))
}

/// Uploads a release's server pack after the publish that built it couldn't. When the zip is gone or was too large
/// to upload, we build it again from the pack as it is now.
pub async fn upload_release_server_pack(pack: &Pack, paths: &ReleasePaths, version: &str, rebuild: bool, log: &Log) -> Result<String> {
    let releases_path = paths.pack_data.join("releases.json");
    let mut release = load_releases(&releases_path).into_iter().find(|r| r.version == version).with_context(|| format!("you haven't published {version} from this machine"))?;
    let release_type = release.release_type.clone().unwrap_or_else(|| pack.config.curseforge.release_type.clone());
    let curseforge = release.curseforge.as_mut().with_context(|| format!("{version} isn't on CurseForge."))?;
    if !curseforge.awaits_server_pack() {
        bail!("{version} already has its server pack on CurseForge.");
    }
    let zip = server_zip(pack, &paths.pack_data, version);
    if rebuild || !zip.exists() || curseforge.unsent_server_pack.as_ref().is_some_and(|u| u.too_large) {
        log.info("Building the server pack again from the pack as it is now, with its mods downloaded on first start");
        let mut cache = MetadataCache::load(&paths.cache_path);
        let sides = sides::classify(pack, &mut cache).await?;
        cache.save(&paths.cache_path)?;
        let out_dir = paths.pack_data.join("builds").join(version);
        export::server_pack(pack, &sides, version, export::ServerMods::DownloadedOnStart, &out_dir, &paths.data_dir, log).await?;
        export::zip_server_pack(pack, version, &out_dir, log).await?;
    }
    curseforge::check_upload_size(&zip, "server pack")?;
    let names = curseforge.unsent_server_pack.as_ref().map_or_else(|| default_server_names(pack), |u| u.names.clone());
    let names = UploadNames { display_name: names.display_name.replace(VERSION_TOKEN, version), file_name: names.file_name.replace(VERSION_TOKEN, version) };
    let uploaded = upload_server_pack(curseforge, &zip, names, &release_type, log).await;
    save_release(&releases_path, &release)?;
    Ok(format!("Uploaded the {version} server pack to CurseForge as file {}", uploaded?))
}

#[derive(Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum PackFile {
    Client,
    Server,
}

// A file CurseForge rejected or deleted is out of the way, but one it still holds would sit beside the
// new upload, so we wait for you to delete it.
async fn ensure_gone(project_id: u64, file_id: u64, what: &str) -> Result<()> {
    if let Some(file) = curseforge::file(project_id, file_id).await?
        && !matches!(
            curseforge::file_state(file.file_status),
            curseforge::FileState::Deleted | curseforge::FileState::Rejected | curseforge::FileState::MalwareDetected | curseforge::FileState::FailedPublishing
        )
    {
        bail!("CurseForge still has the {what}. Delete it on CurseForge first, then try again.");
    }
    Ok(())
}

/// Uploads a release's client or server pack again, once CurseForge no longer has the old one. A new
/// client pack also needs a new server pack, since CurseForge removes a file's additional files with it.
pub async fn replace_curseforge_file(pack: &Pack, paths: &ReleasePaths, version: &str, which: PackFile, log: &Log) -> Result<String> {
    let releases_path = paths.pack_data.join("releases.json");
    let mut release = load_releases(&releases_path).into_iter().find(|r| r.version == version).with_context(|| format!("you haven't published {version} from this machine"))?;
    let display_name = release.display_name.clone().unwrap_or_else(|| default_client_names(pack).display_name.replace(VERSION_TOKEN, version));
    let curseforge = release.curseforge.as_mut().with_context(|| format!("{version} isn't on CurseForge."))?;
    let mut summary = Vec::new();
    if which == PackFile::Client {
        ensure_gone(curseforge.project_id, curseforge.file_id, "client pack").await?;
        let out_dir = paths.pack_data.join("builds").join(version);
        let mut zip = out_dir.join(pack.client_zip_name(version));
        if !zip.exists() {
            log.info("Building the client pack again from the pack as it is now");
            zip = export::client_export(pack, &MetadataCache::load(&paths.cache_path), version, &out_dir, log)?;
        }
        curseforge::check_upload_size(&zip, "client pack")?;
        let notes = std::fs::read_to_string(pack.dir.join(CHANGELOG_ARCHIVE).join(format!("{version}.md"))).unwrap_or_default();
        let file_name = zip.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| pack.client_zip_name(version));
        curseforge.file_id = upload_client_pack(pack, &zip, UploadNames { display_name, file_name }, &notes, log).await?;
        curseforge.last_upload = Some(chrono::Utc::now().to_rfc3339());
        summary.push(format!("Uploaded the {version} client pack to CurseForge as file {}", curseforge.file_id));
    } else if let Some(old) = curseforge.server_file_id {
        ensure_gone(curseforge.project_id, old, "server pack").await?;
    }
    log.info("CurseForge no longer has the old file");
    curseforge.server_file_id = None;
    curseforge.server_pack_uploaded_by_you = false;
    // We save the new client file before the server pack goes up, so a failed server upload leaves it to retry.
    save_release(&releases_path, &release)?;
    summary.push(upload_release_server_pack(pack, paths, version, true, log).await?);
    Ok(summary.join(". "))
}

pub struct ReleasePaths {
    pub data_dir: PathBuf,
    pub pack_data: PathBuf,
    pub cache_path: PathBuf,
}

/// Finishes a release a publish left unfinished: the server pack and Modrinth uploads it's missing, then the
/// commit, tag and push it hasn't done yet.
pub async fn finish_release(pack: &Pack, identity: &Identity, paths: &ReleasePaths, version: &str, log: &Log) -> Result<String> {
    let releases_path = paths.pack_data.join("releases.json");
    let find = || load_releases(&releases_path).into_iter().find(|r| r.version == version).with_context(|| format!("you haven't published {version} from this machine"));
    let release = find()?;
    let mut failures = Vec::new();
    if awaiting_server_pack(Some(pack), &paths.pack_data, &release).is_some()
        && let Err(error) = upload_release_server_pack(pack, paths, version, false, log).await
    {
        failures.push(format!("The server pack upload failed: {error:#}."));
    }
    if awaiting_modrinth(pack, &paths.pack_data, &release).is_some()
        && let Err(error) = upload_release_to_modrinth(pack, &paths.pack_data, version, log).await
    {
        failures.push(format!("The Modrinth upload failed: {error:#}."));
    }
    let mut release = find()?;
    if let Err(error) = run_pending(pack, identity, &releases_path, &mut release, log).await {
        failures.push(format!("{error:#}"));
    }
    if !failures.is_empty() {
        bail!("{}", failures.join(" "));
    }
    Ok(format!("Finished publishing {version}"))
}

/// Records that you uploaded a release's server pack yourself, so we stop offering to upload it.
pub fn mark_server_pack_uploaded(pack_data: &Path, version: &str) -> Result<()> {
    let releases_path = pack_data.join("releases.json");
    let mut release = load_releases(&releases_path).into_iter().find(|r| r.version == version).with_context(|| format!("you haven't published {version} from this machine"))?;
    release.curseforge.as_mut().with_context(|| format!("{version} isn't on CurseForge."))?.server_pack_uploaded_by_you = true;
    save_release(&releases_path, &release)
}

/// Returns whether the release's server pack is on CurseForge, or it has no CurseForge release.
/// CurseForge only reports the link once the client pack is public, and we record it then.
pub async fn link_server_pack(releases_path: &Path, release: &mut Release) -> bool {
    let Some(curseforge) = release.curseforge.as_ref().filter(|c| c.server_file_id.is_none()) else { return true };
    let linked = curseforge::file(curseforge.project_id, curseforge.file_id).await.ok().flatten().and_then(|file| file.server_pack_file_id);
    let Some(server_file_id) = linked else { return false };
    adopt_server_pack(release, server_file_id);
    let mut releases = load_releases(releases_path);
    if let Some(stored) = releases.iter_mut().find(|r| r.version == release.version) {
        adopt_server_pack(stored, server_file_id);
        let _ = save_releases(releases_path, &releases);
    }
    true
}

/// Records a server pack you uploaded yourself once CurseForge links it to the client pack.
pub fn adopt_server_pack(release: &mut Release, server_file_id: u64) {
    if let Some(curseforge) = release.curseforge.as_mut().filter(|c| c.server_file_id.is_none()) {
        curseforge.server_file_id = Some(server_file_id);
        curseforge.unsent_server_pack = None;
    }
}

async fn upload_to_modrinth(pack: &Pack, version: &str, path: &Path, display_name: &str, release_type: &str, notes: &str, log: &Log) -> Result<ModrinthRelease> {
    let wanted = pack.config.modrinth.project_id.trim();
    let project = modrinth::project(wanted).await?.with_context(|| format!("Modrinth has no project {wanted}. Check the Modrinth project in Pack settings."))?;
    let loader = match pack.lock.mod_loader()? {
        ModLoader::NeoForge(_) => "neoforge",
        ModLoader::Forge(_) => "forge",
        ModLoader::Fabric(_) => "fabric",
    };
    let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| pack.modrinth_pack_name(version));
    log.info(format!("Uploading {file_name} to Modrinth"));
    // The v2 API takes a modpack's real loaders and records them as the mrpack loader for us.
    let uploaded = modrinth::create_version(
        json!({
            "project_id": project.id,
            "name": display_name,
            "version_number": version,
            "changelog": notes,
            "dependencies": [],
            "game_versions": [pack.lock.minecraft()?],
            "version_type": release_type,
            "loaders": [loader],
            "featured": false,
            "status": "listed",
        }),
        path,
        file_name,
    )
    .await?;
    log.info(format!("Uploaded Modrinth version {}", uploaded.id));
    Ok(ModrinthRelease { project_id: project.id, version_id: uploaded.id })
}

/// Returns the Modrinth pack a publish built for this release but couldn't upload. Releases from
/// before the pack was on Modrinth have none, so we never offer to upload them.
pub fn awaiting_modrinth(pack: &Pack, pack_data: &Path, release: &Release) -> Option<PathBuf> {
    let path = pack_data.join("builds").join(&release.version).join(pack.modrinth_pack_name(&release.version));
    (pack.config.targets_modrinth() && release.modrinth.is_none() && path.exists()).then_some(path)
}

/// Uploads a release's Modrinth pack after the publish that built it couldn't.
pub async fn upload_release_to_modrinth(pack: &Pack, pack_data: &Path, version: &str, log: &Log) -> Result<String> {
    let releases_path = pack_data.join("releases.json");
    let mut releases = load_releases(&releases_path);
    let release = releases.iter_mut().find(|r| r.version == version).with_context(|| format!("you haven't published {version} from this machine"))?;
    if release.modrinth.is_some() {
        bail!("{version} is already on Modrinth.");
    }
    let path = pack_data.join("builds").join(version).join(pack.modrinth_pack_name(version));
    if !path.exists() {
        bail!("The Modrinth pack packraven built for {version} is gone from {}, so packraven can't upload it.", path.display());
    }
    let notes = std::fs::read_to_string(pack.dir.join(CHANGELOG_ARCHIVE).join(format!("{version}.md"))).unwrap_or_default();
    let display_name = release.display_name.clone().unwrap_or_else(|| default_client_names(pack).display_name.replace(VERSION_TOKEN, version));
    let release_type = release.release_type.clone().unwrap_or_else(|| pack.config.curseforge.release_type.clone());
    let uploaded = upload_to_modrinth(pack, version, &path, &display_name, &release_type, &notes, log).await?;
    let message = format!("Uploaded {version} to Modrinth as version {}", uploaded.version_id);
    release.modrinth = Some(uploaded);
    release.modrinth_error = None;
    save_releases(&releases_path, &releases)?;
    crate::announcer::check_soon();
    Ok(message)
}

#[cfg(test)]
mod tests {
    use super::{load_releases, release_tag, upload_names, version_of_tag, UploadNames};

    #[test]
    fn tags_a_version_once_with_a_v() {
        assert_eq!(release_tag("1.0.0"), "v1.0.0");
        assert_eq!(release_tag("v4.1.5hf"), "v4.1.5hf");
        assert_eq!(release_tag("vanilla-2"), "vvanilla-2");
        assert_eq!(release_tag("V2"), "vV2", "a tag has to start with a lowercase v for us to find it");
        assert_eq!(version_of_tag("v1.0.0", &[]), "1.0.0");
    }

    fn names(display_name: &str, file_name: &str) -> UploadNames {
        UploadNames { display_name: display_name.into(), file_name: file_name.into() }
    }

    fn resolve(overrides: UploadNames) -> anyhow::Result<UploadNames> {
        upload_names("client pack", &overrides, names("Pack {version}", "Pack-{version}.zip"), "1.0")
    }

    #[test]
    fn upload_names_fall_back_to_the_defaults_with_the_version_filled_in() {
        assert_eq!(resolve(names(" ", "")).unwrap(), names("Pack 1.0", "Pack-1.0.zip"));
    }

    #[test]
    fn upload_names_fill_the_version_into_overrides() {
        assert_eq!(resolve(names("Winter {version}", "winter-{version}")).unwrap(), names("Winter 1.0", "winter-1.0.zip"));
    }

    #[test]
    fn upload_names_trim_overrides_and_add_zip_to_file_names() {
        assert_eq!(resolve(names(" My Pack ", " my-pack ")).unwrap(), names("My Pack", "my-pack.zip"));
        assert_eq!(resolve(names("", "pack.ZIP")).unwrap(), names("Pack 1.0", "pack.ZIP"));
    }

    #[test]
    fn reads_releases_from_before_modrinth() {
        let path = std::env::temp_dir().join(format!("packraven-releases-{}.json", std::process::id()));
        std::fs::write(&path, r#"[{"version": "1.0", "projectId": 5, "fileId": 6, "serverFileId": 7, "publishedAt": "2026-10-01T00:00:00Z"}]"#).unwrap();
        let releases = load_releases(&path);
        std::fs::remove_file(&path).unwrap();
        let curseforge = releases[0].curseforge.as_ref().unwrap();
        assert_eq!((curseforge.project_id, curseforge.file_id, curseforge.server_file_id), (5, 6, Some(7)));
        assert!(releases[0].modrinth.is_none() && !releases[0].announce);
    }

    #[test]
    fn pending_steps_survive_a_save_and_older_releases_have_none() {
        use super::PendingStep;
        let path = std::env::temp_dir().join(format!("packraven-pending-{}.json", std::process::id()));
        std::fs::write(&path, r#"[{"version": "0.9", "publishedAt": "2026-10-01T00:00:00Z"}]"#).unwrap();
        assert!(load_releases(&path)[0].pending.is_empty());

        let mut release = load_releases(&path).remove(0);
        release.version = "1.0".into();
        release.pending = vec![PendingStep::Tag { message: "Release 1.0".into() }, PendingStep::Push];
        super::save_release(&path, &release).unwrap();
        release.pending.remove(0);
        super::save_release(&path, &release).unwrap();
        let releases = load_releases(&path);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(releases.iter().map(|r| r.version.as_str()).collect::<Vec<_>>(), ["0.9", "1.0"]);
        assert_eq!(releases[1].pending, [PendingStep::Push]);
    }

    #[test]
    fn a_release_without_its_server_pack_points_at_the_additional_file_page() {
        let json = r#"[{"version": "1.0", "curseforge": {"projectId": 5, "fileId": 6, "unsentServerPack": {"names": {"displayName": "Server Pack 1.0", "fileName": "pack-server.zip"}, "error": "too large"}}, "publishedAt": "2026-11-01T00:00:00Z", "announce": false, "announcedAt": null}]"#;
        let mut release = serde_json::from_str::<Vec<super::Release>>(json).unwrap().remove(0);
        let upload = super::awaiting_server_pack(None, std::path::Path::new("."), &release).unwrap();
        assert_eq!(upload.upload_page, "https://authors.curseforge.com/#/projects/5/files/6/create-additional-file");
        assert_eq!((upload.display_name.as_deref(), upload.error.as_deref(), upload.zip), (Some("Server Pack 1.0"), Some("too large"), None));
        super::adopt_server_pack(&mut release, 7);
        assert!(super::awaiting_server_pack(None, std::path::Path::new("."), &release).is_none());
        assert!(release.curseforge.unwrap().unsent_server_pack.is_none());
    }

    #[test]
    fn upload_names_reject_unsafe_file_names() {
        assert!(resolve(names("", "dir/pack")).is_err());
        assert!(resolve(names("", "pack?")).is_err());
        assert!(resolve(names("", ".zip")).is_err());
    }
}
