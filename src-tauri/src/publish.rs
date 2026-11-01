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
    pub published_at: String,
    pub announce: bool,
    pub announced_at: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CurseforgeRelease {
    pub project_id: u64,
    pub file_id: u64,
    pub server_file_id: u64,
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
    project_id: Option<u64>,
    file_id: Option<u64>,
    server_file_id: Option<u64>,
    published_at: String,
    #[serde(default)]
    announce: bool,
    #[serde(default)]
    announced_at: Option<String>,
}

impl From<StoredRelease> for Release {
    fn from(stored: StoredRelease) -> Self {
        let legacy = match (stored.project_id, stored.file_id) {
            (Some(project_id), Some(file_id)) => Some(CurseforgeRelease { project_id, file_id, server_file_id: stored.server_file_id.unwrap_or_default() }),
            _ => None,
        };
        Self {
            version: stored.version,
            display_name: stored.display_name,
            release_type: stored.release_type,
            curseforge: stored.curseforge.or(legacy),
            modrinth: stored.modrinth,
            published_at: stored.published_at,
            announce: stored.announce,
            announced_at: stored.announced_at,
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

pub fn preflight_warnings(pack: &Pack) -> Vec<String> {
    let disabled: Vec<&str> = pack.lock.files.iter().filter_map(|e| e.path.strip_suffix(".disabled")?.split_once('/').map(|(_, name)| name)).collect();
    let mut warnings = match disabled[..] {
        [] => vec![],
        [one] => vec![format!("{one} is disabled, so packraven will leave it out of the export.")],
        _ => vec![format!("{} files are disabled, so packraven will leave them out of the export: {}.", disabled.len(), disabled.join(", "))],
    };
    if pack.version().is_some() && !pack.has_bcc() {
        warnings.push("Better Compatibility Checker isn't in the mods folder, so the pack won't show its version in game.".into());
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

    let curseforge = match client_zip {
        Some(client_zip) => {
            export::server_pack(&pack, &sides, &version, export::ServerMods::DownloadedOnStart, &out_dir, &data_dir, &log).await?;
            let server_zip = export::zip_server_pack(&pack, &version, &out_dir, &log).await?;
            Some(upload_to_curseforge(&pack, (&client_zip, client_names.clone()), (&server_zip, server_names), &notes, &log).await?)
        }
        None => None,
    };
    // A CurseForge release already happened by now, so a failed Modrinth upload mustn't keep us from recording it.
    let (modrinth, modrinth_failure) = match &modrinth_pack {
        Some(path) => match upload_to_modrinth(&pack, &version, path, &client_names.display_name, &pack.config.curseforge.release_type, &notes, &log).await {
            Ok(release) => (Some(release), None),
            Err(error) if curseforge.is_some() => (None, Some(error)),
            Err(error) => return Err(error),
        },
        None => (None, None),
    };
    let summary = [
        curseforge.as_ref().map(|c| format!("CurseForge file {}, server pack {}", c.file_id, c.server_file_id)),
        modrinth.as_ref().map(|m| format!("Modrinth version {}", m.version_id)),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("; ");

    let releases_path = pack_data.join("releases.json");
    let mut releases = load_releases(&releases_path);
    let display_name = Some(client_names.display_name.clone());
    let release_type = Some(pack.config.curseforge.release_type.clone());
    releases.push(Release { version: version.clone(), display_name, release_type, curseforge, modrinth, published_at: chrono::Utc::now().to_rfc3339(), announce, announced_at: None });
    save_releases(&releases_path, &releases)?;

    let archive = pack.dir.join(CHANGELOG_ARCHIVE);
    std::fs::create_dir_all(&archive)?;
    std::fs::write(archive.join(format!("{version}.md")), &notes)?;
    changelog::write_notes(&pack, "")?;
    let _ = std::fs::remove_file(&baseline_path);
    // packraven built the packs from disk, so we commit everything, and the tag records what shipped.
    git::run(&pack.dir, &["add", "-A"])?;
    let message = Some(options.message.trim()).filter(|m| !m.is_empty()).map_or_else(|| format!("Release {version}"), String::from);
    let tag = release_tag(&version);
    for command in [&["commit", "--allow-empty", "-m", &message][..], &["tag", "-a", &tag, "-m", &message]] {
        let mut args = identity.config_args();
        args.extend(command.iter().map(|a| a.to_string()));
        git::run(&pack.dir, &args.iter().map(String::as_str).collect::<Vec<_>>())?;
    }
    log.info(format!("Committed \"{message}\" and tagged {tag}"));
    if options.push {
        crate::gitops::push(&pack.dir, &log)
            .await
            .with_context(|| format!("packraven published {version} and tagged {}, but couldn't push. Once you've fixed this, push from Git", release_tag(&version)))?;
        log.info("Pushed the release commit and tag");
    }
    // The release happened, so the job succeeds, and Publish finds the missing Modrinth upload from releases.json.
    Ok(match modrinth_failure {
        Some(error) => format!("Published {version}: {summary}. The Modrinth upload failed, so upload it again from Releases: {error:#}"),
        None => format!("Published {version}: {summary}"),
    })
}

async fn upload_to_curseforge(pack: &Pack, (client_zip, client): (&Path, UploadNames), (server_zip, server): (&Path, UploadNames), notes: &str, log: &Log) -> Result<CurseforgeRelease> {
    let project_id = pack.config.curseforge.project_id;
    let game_versions = curseforge::game_version_ids(pack.lock.minecraft()?, pack.lock.mod_loader()?.name()).await?;
    let release_type = pack.config.curseforge.release_type.clone();
    log.info(format!("Uploading {} as {}", client_zip.display(), client.file_name));
    let file_id = curseforge::upload(
        project_id,
        client_zip,
        client.file_name,
        json!({
            "changelog": notes,
            "changelogType": "markdown",
            "displayName": client.display_name,
            "gameVersions": game_versions,
            "releaseType": release_type,
            "isMarkedForManualRelease": pack.config.curseforge.manual_release,
        }),
    )
    .await?;
    log.info(format!("Uploaded the client pack as file {file_id}"));
    log.info(format!("Uploading {} as {}", server_zip.display(), server.file_name));
    let server_file_id = curseforge::upload(
        project_id,
        server_zip,
        server.file_name,
        json!({
            "changelog": notes,
            "changelogType": "markdown",
            "displayName": server.display_name,
            "parentFileID": file_id,
            "releaseType": release_type,
        }),
    )
    .await
    .context("packraven uploaded the client pack, but the server pack upload failed; upload the server pack as an additional file on CurseForge")?;
    log.info(format!("Uploaded the server pack as file {server_file_id}"));
    Ok(CurseforgeRelease { project_id, file_id, server_file_id })
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
pub async fn upload_release_to_modrinth(pack: Pack, pack_data: PathBuf, version: String, log: Log) -> Result<String> {
    let releases_path = pack_data.join("releases.json");
    let mut releases = load_releases(&releases_path);
    let release = releases.iter_mut().find(|r| r.version == version).with_context(|| format!("you haven't published {version} from this machine"))?;
    if release.modrinth.is_some() {
        bail!("{version} is already on Modrinth.");
    }
    let path = pack_data.join("builds").join(&version).join(pack.modrinth_pack_name(&version));
    if !path.exists() {
        bail!("The Modrinth pack packraven built for {version} is gone from {}, so packraven can't upload it.", path.display());
    }
    let notes = std::fs::read_to_string(pack.dir.join(CHANGELOG_ARCHIVE).join(format!("{version}.md"))).unwrap_or_default();
    let display_name = release.display_name.clone().unwrap_or_else(|| default_client_names(&pack).display_name.replace(VERSION_TOKEN, &version));
    let release_type = release.release_type.clone().unwrap_or_else(|| pack.config.curseforge.release_type.clone());
    let uploaded = upload_to_modrinth(&pack, &version, &path, &display_name, &release_type, &notes, &log).await?;
    let message = format!("Uploaded {version} to Modrinth as version {}", uploaded.version_id);
    release.modrinth = Some(uploaded);
    save_releases(&releases_path, &releases)?;
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
        assert_eq!((curseforge.project_id, curseforge.file_id, curseforge.server_file_id), (5, 6, 7));
        assert!(releases[0].modrinth.is_none() && !releases[0].announce);
    }

    #[test]
    fn upload_names_reject_unsafe_file_names() {
        assert!(resolve(names("", "dir/pack")).is_err());
        assert!(resolve(names("", "pack?")).is_err());
        assert!(resolve(names("", ".zip")).is_err());
    }
}
