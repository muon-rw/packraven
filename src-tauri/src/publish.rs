use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::changelog;
use crate::curseforge;
use crate::export;
use crate::http;
use crate::modrinth;
use crate::git;
use crate::jobs::Log;
use crate::git::Identity;
use crate::pack::{version_problem, ChangelogType, LockEntry, ModLoader, ModrinthFile, Pack, Platform, CHANGELOG_ARCHIVE};
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
    /// The images on your disk that go out with the Discord announcement.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub announcement_images: Vec<String>,
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
    /// The zip is over the upload API's limit, or the API turned it down as too large.
    #[serde(default)]
    pub too_large: bool,
    /// CurseForge received the whole zip but never confirmed it, so it may hold the server pack already.
    #[serde(default)]
    pub unconfirmed: bool,
}

pub enum ServerPackOutcome {
    Uploaded(u64),
    TooLarge(u64),
}

impl ServerPackOutcome {
    pub fn summary(&self, version: &str) -> String {
        match self {
            Self::Uploaded(file_id) => format!("Uploaded the {version} server pack to CurseForge as file {file_id}"),
            Self::TooLarge(size) => format!("The {version} server pack is {}, more than packraven can upload to CurseForge, so upload it yourself on CurseForge's website", curseforge::gigabytes(*size)),
        }
    }
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
    pub changelog: Option<String>,
    pub changelog_type: ChangelogType,
    pub error: Option<String>,
    /// Whether packraven can try the upload again.
    pub retryable: bool,
    /// Whether a retry builds the server pack again first, because the zip is gone.
    pub rebuild: bool,
    /// Whether the zip is too large for CurseForge's upload API, so only CurseForge's website takes it.
    pub too_large: bool,
    /// Whether CurseForge may already have the last upload, so you check before uploading again.
    pub unconfirmed: bool,
    pub client_page: String,
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
    announcement_images: Vec<String>,
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
            announcement_images: stored.announcement_images,
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
    #[serde(default)]
    pub announcement_images: Vec<String>,
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

const NO_CHAT_REPORTS_CF_ID: u64 = 634_062;
const NO_CHAT_REPORTS_MR_ID: &str = "qQyHxfxd";

fn ships_no_chat_reports_to_curseforge(pack: &Pack) -> bool {
    let excluded = |path: &str| path.ends_with(".disabled") || pack.config.curseforge.exclude.iter().any(|p| p == path);
    let is_ncr_path = |path: &str| {
        path.strip_prefix("mods/")
            .is_some_and(|name| name.to_ascii_lowercase().starts_with("nochatreports"))
    };
    let in_lock = pack.lock.files.iter().any(|e| {
        !excluded(&e.path)
            && (e.curseforge_project() == Some(NO_CHAT_REPORTS_CF_ID)
                || e.modrinth.as_ref().is_some_and(|m| m.project_id == NO_CHAT_REPORTS_MR_ID)
                || is_ncr_path(&e.path))
    });
    if in_lock {
        return true;
    }
    git::run(&pack.dir, &["ls-files", "-z", "--", "mods/*.jar"]).is_ok_and(|out| {
        out.split('\0')
            .any(|path| !path.is_empty() && !excluded(path) && is_ncr_path(path) && pack.dir.join(path).is_file())
    })
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
    if config.targets_curseforge() && ships_no_chat_reports_to_curseforge(pack) {
        problems.push("No Chat Reports is in the pack, and CurseForge rejects modpacks that include it. Remove it in Mods before publishing to CurseForge.".into());
    }
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
    crate::discord::check_images(&options.announcement_images)?;
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
    let cached_images = crate::discord::cache_images(&crate::discord::release_images_dir(&pack_data, &version), &options.announcement_images)?;
    crate::discord::clear_pending_images(&pack_data);
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
        announcement_images: cached_images,
        pending: [Some(PendingStep::Tag { message }), options.push.then_some(PendingStep::Push)].into_iter().flatten().collect(),
        mods_digest: Some(mods_digest(&pack)),
        update_checker: options.update_checker.filter(|_| update_checker::repo(&pack).is_some()).map(update_checker::Entry::new),
    };

    if let Some(client_zip) = client_zip {
        export::server_pack(&pack, &sides, &version, &out_dir, &data_dir, &log).await?;
        let server_zip = export::zip_server_pack(&pack, &version, &out_dir, &log).await?;
        curseforge::check_upload_size(&client_zip, "client pack")?;
        // A server pack too large for the upload API can still go up through CurseForge's website.
        curseforge::check_website_upload_size(&server_zip, "server pack")?;
        let file_id = upload_client_pack(&pack, &client_zip, client_names.clone(), &notes, &log).await?;
        // We record the release the moment CurseForge has it, so a publish that stops later leaves a release to
        // finish rather than one we'd upload twice.
        let unsent = UnsentServerPack { names: server_names.clone(), error: STOPPED_EARLY.into(), too_large: false, unconfirmed: false };
        let mut curseforge = CurseforgeRelease { project_id: pack.config.curseforge.project_id, file_id, server_file_id: None, unsent_server_pack: Some(unsent), server_pack_uploaded_by_you: false, last_upload: Some(chrono::Utc::now().to_rfc3339()) };
        release.curseforge = Some(curseforge.clone());
        record(&pack, &releases_path, &release, &notes, &baseline_path)?;
        if let Err(error) = upload_server_pack(
            &mut curseforge,
            &server_zip,
            server_names,
            &release_type,
            &pack.config.curseforge.server_changelog,
            pack.config.curseforge.server_changelog_type,
            &log,
        )
        .await
        {
            log.stream("err", &format!("Couldn't upload the server pack: {error:#}"));
        }
        release.curseforge = Some(curseforge);
        save_release(&releases_path, &release)?;
    }
    let server_failure = release.curseforge.as_ref().and_then(|c| c.unsent_server_pack.clone());
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
    let server_failure = server_failure.map(|unsent| match unsent.too_large {
        true => format!(" {}, so upload it yourself as an additional file on CurseForge's website, where CurseForge accepts modpack files up to about 5 GB.", unsent.error),
        false => format!(" The server pack upload failed, so upload it yourself as an additional file on CurseForge: {}", unsent.error),
    });
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
        log,
    )
    .await
    .map_err(|error| warn_if_unconfirmed(error, pack.config.curseforge.project_id, "client pack"))?;
    log.info(format!("Uploaded the client pack as file {file_id}"));
    Ok(file_id)
}

// We never record a client pack CurseForge didn't confirm, so trying again would upload a second copy if it kept this one.
fn warn_if_unconfirmed(error: anyhow::Error, project_id: u64, what: &str) -> anyhow::Error {
    match error.is::<http::UploadUnconfirmed>() {
        true => anyhow::anyhow!("{error:#}. Check {} for the {what} before you try again, and delete it there if CurseForge kept it.", curseforge::files_page(project_id)),
        false => error,
    }
}

fn server_pack_metadata(names: &UploadNames, parent_file_id: u64, release_type: &str, changelog: &str, changelog_type: ChangelogType) -> serde_json::Value {
    json!({
        "changelog": changelog,
        "changelogType": changelog_type.as_str(),
        "displayName": names.display_name,
        "parentFileID": parent_file_id,
        "releaseType": release_type,
    })
}

// Either way the outcome goes on the release: the server file, or why it isn't there.
async fn upload_server_pack(
    curseforge: &mut CurseforgeRelease,
    zip: &Path,
    names: UploadNames,
    release_type: &str,
    changelog: &str,
    changelog_type: ChangelogType,
    log: &Log,
) -> Result<ServerPackOutcome> {
    let size = std::fs::metadata(zip)?.len();
    if size > curseforge::UPLOAD_LIMIT {
        let error = format!("The server pack is {}, more than packraven can upload to CurseForge", curseforge::gigabytes(size));
        log.info(format!("{error}, so you'll upload it on CurseForge's website"));
        curseforge.unsent_server_pack = Some(UnsentServerPack { names, error, too_large: true, unconfirmed: false });
        return Ok(ServerPackOutcome::TooLarge(size));
    }
    log.info(format!("Uploading {} as {}", zip.display(), names.file_name));
    let metadata = server_pack_metadata(&names, curseforge.file_id, release_type, changelog, changelog_type);
    match curseforge::upload(curseforge.project_id, zip, names.file_name.clone(), metadata, log).await {
        Ok(server_file_id) => {
            log.info(format!("Uploaded the server pack as file {server_file_id}"));
            curseforge.server_file_id = Some(server_file_id);
            curseforge.unsent_server_pack = None;
            curseforge.last_upload = Some(chrono::Utc::now().to_rfc3339());
            Ok(ServerPackOutcome::Uploaded(server_file_id))
        }
        Err(error) => {
            let too_large = error.is::<curseforge::UploadTooLarge>();
            let unconfirmed = error.is::<http::UploadUnconfirmed>();
            curseforge.unsent_server_pack = Some(UnsentServerPack { names, error: format!("{error:#}"), too_large, unconfirmed });
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
    let size = zip.as_ref().and_then(|zip| std::fs::metadata(zip).ok()).map(|m| m.len());
    let too_large = size.is_some_and(|size| size > curseforge::UPLOAD_LIMIT || unsent.is_some_and(|u| u.too_large));
    Some(ServerPackUpload {
        retryable: pack.is_some() && !too_large,
        rebuild: zip.is_none(),
        too_large,
        unconfirmed: !too_large && unsent.is_some_and(|u| u.unconfirmed),
        client_page: curseforge::file_page(curseforge.project_id, curseforge.file_id),
        pack_changed: pack.is_some_and(|pack| changed_since(pack, release)),
        zip,
        upload_page: curseforge::additional_file_page(curseforge.project_id, curseforge.file_id),
        display_name: unsent.map(|u| u.names.display_name.clone()),
        file_name: unsent.map(|u| u.names.file_name.clone()),
        changelog: pack.map(|p| p.config.curseforge.server_changelog.as_str()).filter(|s| !s.trim().is_empty()).map(str::to_string),
        changelog_type: pack.map(|p| p.config.curseforge.server_changelog_type).unwrap_or_default(),
        error: unsent.map(|u| u.error.clone()),
        tag: release_tag(&release.version),
    })
}

fn server_zip(pack: &Pack, pack_data: &Path, version: &str) -> PathBuf {
    pack_data.join("builds").join(version).join(pack.server_zip_name(version))
}

/// Uploads a release's server pack after the publish that built it couldn't. When the zip is gone, we build it again
/// from the pack as it is now.
pub async fn upload_release_server_pack(pack: &Pack, paths: &ReleasePaths, version: &str, rebuild: bool, log: &Log) -> Result<ServerPackOutcome> {
    let releases_path = paths.pack_data.join("releases.json");
    let mut release = load_releases(&releases_path).into_iter().find(|r| r.version == version).with_context(|| format!("you haven't published {version} from this machine"))?;
    let release_type = release.release_type.clone().unwrap_or_else(|| pack.config.curseforge.release_type.clone());
    let curseforge = release.curseforge.as_mut().with_context(|| format!("{version} isn't on CurseForge."))?;
    if !curseforge.awaits_server_pack() {
        bail!("{version} already has its server pack on CurseForge.");
    }
    let zip = server_zip(pack, &paths.pack_data, version);
    if rebuild || !zip.exists() {
        log.info("Building the server pack again from the pack as it is now");
        let mut cache = MetadataCache::load(&paths.cache_path);
        let sides = sides::classify(pack, &mut cache).await?;
        cache.save(&paths.cache_path)?;
        let out_dir = paths.pack_data.join("builds").join(version);
        export::server_pack(pack, &sides, version, &out_dir, &paths.data_dir, log).await?;
        export::zip_server_pack(pack, version, &out_dir, log).await?;
    }
    curseforge::check_website_upload_size(&zip, "server pack")?;
    let names = curseforge.unsent_server_pack.as_ref().map_or_else(|| default_server_names(pack), |u| u.names.clone());
    let names = UploadNames { display_name: names.display_name.replace(VERSION_TOKEN, version), file_name: names.file_name.replace(VERSION_TOKEN, version) };
    let outcome = upload_server_pack(
        curseforge,
        &zip,
        names,
        &release_type,
        &pack.config.curseforge.server_changelog,
        pack.config.curseforge.server_changelog_type,
        log,
    )
    .await;
    save_release(&releases_path, &release)?;
    outcome
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
    summary.push(upload_release_server_pack(pack, paths, version, true, log).await?.summary(version));
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
    // Only you can upload a server pack too large for the upload API, and only you can check whether CurseForge kept
    // an unconfirmed one, so finishing leaves those to you.
    let mut left_to_you = None;
    let too_large = "which is more than packraven can upload to CurseForge. Upload it yourself on CurseForge's website";
    let unconfirmed = "which CurseForge may already have. Check the client pack on CurseForge before you upload it again";
    match awaiting_server_pack(Some(pack), &paths.pack_data, &release) {
        Some(upload) if upload.too_large => left_to_you = Some(too_large),
        Some(upload) if upload.unconfirmed => left_to_you = Some(unconfirmed),
        Some(_) => match upload_release_server_pack(pack, paths, version, false, log).await {
            Ok(ServerPackOutcome::TooLarge(_)) => left_to_you = Some(too_large),
            Ok(ServerPackOutcome::Uploaded(_)) => {}
            Err(error) => failures.push(format!("The server pack upload failed: {error:#}.")),
        },
        None => {}
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
    Ok(match left_to_you {
        Some(why) => format!("Finished publishing {version}, except its server pack, {why}"),
        None => format!("Finished publishing {version}"),
    })
}

/// Records that you uploaded a release's server pack yourself. When you give us its CurseForge
/// download link or file ID, we store the file ID so packraven tracks its review directly.
pub fn mark_server_pack_uploaded(pack_data: &Path, version: &str, link: Option<&str>) -> Result<Option<u64>> {
    let releases_path = pack_data.join("releases.json");
    let mut release = load_releases(&releases_path).into_iter().find(|r| r.version == version).with_context(|| format!("you haven't published {version} from this machine"))?;
    let curseforge = release.curseforge.as_mut().with_context(|| format!("{version} isn't on CurseForge."))?;
    let file_id = match link.map(str::trim).filter(|s| !s.is_empty()) {
        Some(text) => {
            let id = curseforge::parse_file_id(text)?;
            if id == curseforge.file_id {
                bail!("That's the client pack's file. Copy the download link from the server pack instead.");
            }
            curseforge.server_file_id = Some(id);
            curseforge.unsent_server_pack = None;
            curseforge.server_pack_uploaded_by_you = false;
            curseforge.last_upload = Some(chrono::Utc::now().to_rfc3339());
            Some(id)
        }
        None => {
            curseforge.server_pack_uploaded_by_you = true;
            None
        }
    };
    save_release(&releases_path, &release)?;
    Ok(file_id)
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
        log,
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

async fn uploaded_modrinth_version(pack: &Pack, version: &str) -> Result<Option<ModrinthRelease>> {
    let Some(project) = modrinth::project(pack.config.modrinth.project_id.trim()).await? else { return Ok(None) };
    let versions = modrinth::project_versions(&project.id, &[], pack.lock.minecraft()?).await?;
    Ok(versions.into_iter().find(|v| v.version_number == version).map(|v| ModrinthRelease { project_id: project.id.clone(), version_id: v.id }))
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
    // An earlier upload may have reached Modrinth before its answer got lost, and Modrinth lists a version as soon as it
    // has it, so we adopt that one rather than upload a second.
    let (uploaded, message) = match uploaded_modrinth_version(pack, version).await? {
        Some(found) => {
            let message = format!("Modrinth already had {version} as version {}, so packraven recorded it", found.version_id);
            (found, message)
        }
        None => {
            let notes = std::fs::read_to_string(pack.dir.join(CHANGELOG_ARCHIVE).join(format!("{version}.md"))).unwrap_or_default();
            let display_name = release.display_name.clone().unwrap_or_else(|| default_client_names(pack).display_name.replace(VERSION_TOKEN, version));
            let release_type = release.release_type.clone().unwrap_or_else(|| pack.config.curseforge.release_type.clone());
            let uploaded = upload_to_modrinth(pack, version, &path, &display_name, &release_type, &notes, log).await?;
            let message = format!("Uploaded {version} to Modrinth as version {}", uploaded.version_id);
            (uploaded, message)
        }
    };
    log.info(&message);
    release.modrinth = Some(uploaded);
    release.modrinth_error = None;
    save_releases(&releases_path, &releases)?;
    crate::announcer::check_soon();
    Ok(message)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AbortReleaseResult {
    pub version: String,
    pub tag: String,
    pub local_tag_deleted: bool,
    pub remote_tag_deleted: bool,
    pub remote_tag_warning: Option<String>,
    pub notes_restored: bool,
}

fn extract_notes(rendered: &str) -> &str {
    if rendered.starts_with("## Mod changes") {
        ""
    } else if let Some((notes, _)) = rendered.split_once("\n\n## Mod changes") {
        notes
    } else if let Some((notes, _)) = rendered.split_once("\n## Mod changes") {
        notes
    } else {
        rendered
    }
}

fn delete_remote_tag(dir: &Path, tag: &str) -> (bool, Option<String>) {
    let branch = git::branch(dir).unwrap_or_default();
    let remote = git::run(dir, &["config", "--get", &format!("branch.{branch}.remote")])
        .ok()
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| "origin".into());
    if git::run(dir, &["remote", "get-url", &remote]).is_err() {
        return (false, None);
    }
    let ref_name = format!("refs/tags/{tag}");
    match crate::remote::run(dir, &["ls-remote", "--tags", &remote, &ref_name]) {
        Ok(out) if out.trim().is_empty() => (false, None),
        Ok(_) => match crate::remote::run(dir, &["push", "--delete", &remote, tag]) {
            Ok(_) => (true, None),
            Err(error) => (false, Some(format!("Couldn't remove tag {tag} from {remote}: {error:#}"))),
        },
        Err(error) => (false, Some(format!("Couldn't check whether {remote} has tag {tag}: {error:#}"))),
    }
}

/// Clears a release's git tag, record in releases.json, and cached build so you can publish the same version fresh.
pub async fn abort_release(pack: &Pack, pack_data: &Path, version: &str) -> Result<AbortReleaseResult> {
    let version = version.trim();
    if version.is_empty() {
        bail!("No version given.");
    }
    let tag = release_tag(version);
    let releases_path = pack_data.join("releases.json");
    let mut releases = load_releases(&releases_path);
    if let Some(index) = releases.iter().position(|r| r.version == version) {
        let removed = releases.remove(index);
        save_releases(&releases_path, &releases)?;
        if !removed.announcement_images.is_empty() && crate::discord::load_pending_images(pack_data).is_empty() {
            let _ = crate::discord::save_pending_images(pack_data, &removed.announcement_images);
        }
    }
    crate::discord::clear_release_images(pack_data, version);
    let _ = std::fs::remove_dir_all(pack_data.join("builds").join(version));

    let archive_path = pack.dir.join(CHANGELOG_ARCHIVE).join(format!("{version}.md"));
    let mut notes_restored = false;
    if archive_path.exists() {
        if changelog::read_notes(pack).trim().is_empty()
            && let Ok(archived) = std::fs::read_to_string(&archive_path)
        {
            let notes = extract_notes(&archived).trim();
            if !notes.is_empty() {
                changelog::write_notes(pack, &format!("{notes}\n"))?;
                notes_restored = true;
            }
        }
        let _ = std::fs::remove_file(&archive_path);
    }

    let mut local_tag_deleted = false;
    if tag_exists(pack, version) {
        git::run(&pack.dir, &["tag", "-d", &tag])?;
        local_tag_deleted = true;
    }

    let dir = pack.dir.clone();
    let tag_for_remote = tag.clone();
    let (remote_tag_deleted, remote_tag_warning) = tokio::task::spawn_blocking(move || delete_remote_tag(&dir, &tag_for_remote)).await?;

    Ok(AbortReleaseResult {
        version: version.to_string(),
        tag,
        local_tag_deleted,
        remote_tag_deleted,
        remote_tag_warning,
        notes_restored,
    })
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

    #[tokio::test]
    async fn leaves_a_server_pack_too_large_for_the_upload_api_to_you() {
        let path = std::env::temp_dir().join(format!("packraven-large-server-{}.zip", std::process::id()));
        std::fs::File::create(&path).unwrap().set_len(crate::curseforge::UPLOAD_LIMIT + 1).unwrap();
        let mut curseforge = super::CurseforgeRelease { project_id: 5, file_id: 6, server_file_id: None, unsent_server_pack: None, server_pack_uploaded_by_you: false, last_upload: None };
        let names = super::UploadNames { display_name: "Server Pack 1.0".into(), file_name: "pack-server.zip".into() };
        let outcome = super::upload_server_pack(
            &mut curseforge,
            &path,
            names,
            "release",
            "",
            super::ChangelogType::Markdown,
            &crate::jobs::Log::console("test"),
        )
        .await
        .unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(matches!(outcome, super::ServerPackOutcome::TooLarge(size) if size == crate::curseforge::UPLOAD_LIMIT + 1));
        let unsent = curseforge.unsent_server_pack.as_ref().unwrap();
        assert!(unsent.too_large && unsent.error == "The server pack is 1.07 GB, more than packraven can upload to CurseForge", "{}", unsent.error);
        assert!(curseforge.awaits_server_pack());
    }

    #[test]
    fn server_pack_metadata_includes_changelog_and_type() {
        let names = super::UploadNames { display_name: "Server Pack 1.0".into(), file_name: "pack-server.zip".into() };
        let md = super::server_pack_metadata(&names, 42, "beta", "Made with [packraven](https://packraven.app)", super::ChangelogType::Markdown);
        assert_eq!(md["changelog"], "Made with [packraven](https://packraven.app)");
        assert_eq!(md["changelogType"], "markdown");
        assert_eq!(md["parentFileID"], 42);
        assert_eq!(md["releaseType"], "beta");

        let html = super::server_pack_metadata(&names, 42, "release", "<p>Custom</p>", super::ChangelogType::Html);
        assert_eq!(html["changelog"], "<p>Custom</p>");
        assert_eq!(html["changelogType"], "html");
    }

    #[test]
    fn upload_names_reject_unsafe_file_names() {
        assert!(resolve(names("", "dir/pack")).is_err());
        assert!(resolve(names("", "pack?")).is_err());
        assert!(resolve(names("", ".zip")).is_err());
    }

    #[test]
    fn mark_server_pack_uploaded_records_file_id_from_link_and_rejects_client_file() {
        let dir = std::env::temp_dir().join(format!("packraven-mark-server-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("releases.json");
        let json = r#"[{"version": "0.3.0", "curseforge": {"projectId": 1764125, "fileId": 9660100, "unsentServerPack": {"names": {"displayName": "Server Pack 0.3.0", "fileName": "server.zip"}, "error": "too large"}}, "publishedAt": "2026-11-01T00:00:00Z", "announce": false, "announcedAt": null}]"#;
        std::fs::write(&path, json).unwrap();

        assert!(super::mark_server_pack_uploaded(&dir, "0.3.0", Some("https://edge.forgecdn.net/files/9660/100/client.zip")).is_err());
        let id = super::mark_server_pack_uploaded(&dir, "0.3.0", Some("https://edge.forgecdn.net/files/9660/156/Ravens-High-Fantasy-0.3.0-server.zip")).unwrap();
        assert_eq!(id, Some(9_660_156));

        let release = load_releases(&path).remove(0);
        let cf = release.curseforge.unwrap();
        assert_eq!(cf.server_file_id, Some(9_660_156));
        assert!(cf.unsent_server_pack.is_none());
        assert!(!cf.server_pack_uploaded_by_you);
        assert!(cf.last_upload.is_some());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn extract_notes_strips_mod_changes_section() {
        assert_eq!(super::extract_notes("Big fix\n\n## Mod changes\n\n### Added\n- Foo\n"), "Big fix");
        assert_eq!(super::extract_notes("## Mod changes\n\n### Added\n- Foo\n"), "");
        assert_eq!(super::extract_notes("Just notes\n"), "Just notes\n");
    }

    #[tokio::test]
    async fn abort_release_clears_tag_record_builds_and_restores_notes() {
        let root = std::env::temp_dir().join(format!("packraven-abort-release-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let pack_dir = root.join("pack");
        let pack_data = root.join("data");
        std::fs::create_dir_all(pack_dir.join("changelogs")).unwrap();
        std::fs::create_dir_all(pack_data.join("builds").join("0.3.1")).unwrap();
        std::fs::write(pack_dir.join("packraven.toml"), "name = \"Test\"\n").unwrap();
        std::fs::write(pack_dir.join("mods.lock.json"), r#"{"files":[]}"#).unwrap();
        std::fs::write(pack_dir.join("changelog.md"), "").unwrap();
        std::fs::write(pack_dir.join("changelogs").join("0.3.1.md"), "Fixed crash\n\n## Mod changes\n\n### Added\n- Foo\n").unwrap();
        std::fs::write(pack_data.join("builds").join("0.3.1").join("client.zip"), b"zip").unwrap();
        std::fs::write(
            pack_data.join("releases.json"),
            r#"[{"version":"0.3.0","publishedAt":"2026-11-01T00:00:00Z"},{"version":"0.3.1","publishedAt":"2026-11-02T00:00:00Z"}]"#,
        )
        .unwrap();

        crate::git::run(&pack_dir, &["init"]).unwrap();
        crate::git::run(&pack_dir, &["config", "user.name", "Test"]).unwrap();
        crate::git::run(&pack_dir, &["config", "user.email", "test@example.com"]).unwrap();
        crate::git::run(&pack_dir, &["add", "-A"]).unwrap();
        crate::git::run(&pack_dir, &["commit", "-m", "Release 0.3.1"]).unwrap();
        crate::git::run(&pack_dir, &["tag", "-a", "v0.3.1", "-m", "Release 0.3.1"]).unwrap();

        let pack = crate::pack::Pack::open(&pack_dir).unwrap();
        assert!(super::tag_exists(&pack, "0.3.1"));

        let result = super::abort_release(&pack, &pack_data, "0.3.1").await.unwrap();
        assert_eq!(result.tag, "v0.3.1");
        assert!(result.local_tag_deleted);
        assert!(!result.remote_tag_deleted);
        assert!(result.remote_tag_warning.is_none());
        assert!(result.notes_restored);
        assert!(!super::tag_exists(&pack, "0.3.1"));
        assert_eq!(load_releases(&pack_data.join("releases.json")).iter().map(|r| r.version.as_str()).collect::<Vec<_>>(), ["0.3.0"]);
        assert!(!pack_data.join("builds").join("0.3.1").exists());
        assert!(!pack_dir.join("changelogs").join("0.3.1.md").exists());
        assert_eq!(std::fs::read_to_string(pack_dir.join("changelog.md")).unwrap(), "Fixed crash\n");

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn detects_no_chat_reports_unless_disabled_or_excluded_from_curseforge() {
        let dir = std::env::temp_dir().join(format!("packraven-ncr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("packraven.toml"), "name = \"Test\"\n").unwrap();
        std::fs::write(dir.join("mods.lock.json"), r#"{"files":[]}"#).unwrap();
        let mut pack = crate::pack::Pack::open(&dir).unwrap();

        assert!(!super::ships_no_chat_reports_to_curseforge(&pack));

        pack.lock.files.push(crate::pack::LockEntry::curseforge(
            "mods/NoChatReports-NEOFORGE-1.21.1-v2.9.1.jar".into(),
            "abc".into(),
            10,
            crate::pack::CurseforgeFile { project_id: 634_062, file_id: 1, url: String::new() },
        ));
        assert!(super::ships_no_chat_reports_to_curseforge(&pack));

        pack.config.curseforge.exclude.push("mods/NoChatReports-NEOFORGE-1.21.1-v2.9.1.jar".into());
        assert!(!super::ships_no_chat_reports_to_curseforge(&pack));

        pack.config.curseforge.exclude.clear();
        pack.lock.files[0].path.push_str(".disabled");
        assert!(!super::ships_no_chat_reports_to_curseforge(&pack));

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
