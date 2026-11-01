use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Deserialize;
use serde_json::json;
use tokio::process::Command;
use walkdir::WalkDir;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::git;
use crate::http;
use crate::jobs::Log;
use crate::mirror::{mirror, FilePlan, Planned};
use crate::modrinth;
use crate::pack::{LockEntry, ModKey, ModLoader, Pack, Platform, SERVER_ICON};
use crate::sides::{MetadataCache, ModSide, ProjectName, Side};

const NEOFORGE_MAVEN: &str = "https://maven.neoforged.net/releases/net/neoforged/neoforge";
const FORGE_MAVEN: &str = "https://maven.minecraftforge.net/net/minecraftforge/forge";
const FABRIC_META: &str = "https://meta.fabricmc.net/v2/versions";
const FABRIC_LAUNCHER: &str = "fabric-server-launch.jar";
const RUN_BAT: &str = include_str!("scripts/run.bat");
const RUN_SH: &str = include_str!("scripts/run.sh");
const INSTALL_BAT: &str = include_str!("scripts/install.bat");
const INSTALL_SH: &str = include_str!("scripts/install.sh");
const DOWNLOAD_BAT: &str = include_str!("scripts/download.bat");
const DOWNLOAD_SH: &str = include_str!("scripts/download.sh");
const DOWNLOADER_JAR: &[u8] = include_bytes!("scripts/downloader/packraven-downloader.jar");
const DOWNLOADER: &str = "packraven-downloader.jar";
const DOWNLOAD_LIST: &str = "packraven-downloads.txt";

/// How a server pack gets the mod files packraven can download from CurseForge or Modrinth.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ServerMods {
    Bundled,
    DownloadedOnStart,
}

fn globs(patterns: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern).with_context(|| format!("invalid pattern {pattern}"))?);
    }
    Ok(builder.build()?)
}

pub fn override_files(pack: &Pack, extra_exclude: &[String]) -> Result<Vec<String>> {
    let excluded = globs(&[pack.config.export.exclude.clone(), extra_exclude.to_vec()].concat())?;
    Ok(git::tracked_files(&pack.dir)?.into_iter().filter(|path| !excluded.is_match(path) && pack.dir.join(path).is_file()).collect())
}

// We check these on every overview refresh, so we ask git for tracked jars instead of checking
// every override file. CurseForge rejects packs with jars it doesn't host in overrides. It doesn't
// check resource packs, shaders or data packs yet, so those go into the client pack without asking;
// if CurseForge starts enforcing them too, they belong in this list.
fn curseforge_override_jars(pack: &Pack) -> Result<Vec<String>> {
    let excluded = globs(&pack.config.export.exclude)?;
    let jars = git::run(&pack.dir, &["ls-files", "-z", "--", "*.jar"])?;
    let mut paths: Vec<String> = jars.split('\0').filter(|path| path.ends_with(".jar") && !excluded.is_match(path) && pack.dir.join(path).is_file()).map(String::from).collect();
    paths.extend(only_on_modrinth(pack).map(|e| e.path.clone()).filter(|path| path.ends_with(".jar")));
    Ok(paths)
}

// CurseForge's manifest can only list CurseForge's files, so the client pack carries these in overrides.
fn only_on_modrinth(pack: &Pack) -> impl Iterator<Item = &LockEntry> {
    pack.lock.files.iter().filter(|e| e.curseforge.is_none() && !e.path.ends_with(".disabled"))
}

// Modrinth's moderators check every jar and pack in an mrpack's overrides for permission to
// redistribute it, the way labrinth's modpack validator collects them.
fn is_moderated_override(path: &str) -> bool {
    ["mods/", "resourcepacks/", "shaderpacks/"].iter().any(|folder| path.starts_with(folder)) && (path.ends_with(".jar") || path.ends_with(".zip"))
}

// We record choices as the paths you picked, or as a pattern for every version of a file, and file
// names often hold [1.20.1] or {build}, so only * stays a wildcard in them.
fn literal_paths(patterns: &[String]) -> Vec<String> {
    let literal = |c: char| if matches!(c, '[' | ']' | '{' | '}' | '?') { format!("[{c}]") } else { c.to_string() };
    patterns.iter().map(|pattern| pattern.chars().map(literal).collect()).collect()
}

pub fn choice_covers(choice: &str, path: &str) -> bool {
    choice == path || (choice.contains('*') && globs(&literal_paths(&[choice.to_string()])).is_ok_and(|set| set.is_match(path)))
}

// Mod files change names with every version, as in Prominent-GLOBAL-MC1.20.1-4.1.5.jar, so we keep
// the name up to its first part with a digit in it, or up to the first digit after a letter. A file
// with no digits yet keeps its whole stem before *, so a later versioned drop still matches. We
// never cut before the first part, since names like 3dskinlayers start with one.
pub fn any_version_pattern(path: &str) -> Option<String> {
    let (dir, name) = path.rsplit_once('/').map_or(("", path), |(dir, name)| (dir, name));
    let (stem, extension) = name.rsplit_once('.')?;
    let is_separator = |c: char| matches!(c, '-' | '_' | '+' | ' ');
    let by_part = stem.char_indices().filter(|&(_, c)| is_separator(c)).map(|(i, _)| i + 1).find(|&start| {
        let part = stem[start..].split(is_separator).next().unwrap_or_default();
        part.chars().any(|c| c.is_ascii_digit())
    });
    let first_part = stem.split(is_separator).next().unwrap_or_default();
    let by_digit = first_part
        .starts_with(|c: char| c.is_ascii_alphabetic())
        .then(|| first_part.find(|c: char| c.is_ascii_digit()))
        .flatten();
    let cut = by_part.or(by_digit).or_else(|| (!stem.chars().any(|c| c.is_ascii_digit()) && stem.chars().any(|c| c.is_ascii_alphabetic())).then_some(stem.len()))?;
    let prefix = &stem[..cut];
    let dir = if dir.is_empty() { String::new() } else { format!("{dir}/") };
    Some(format!("{dir}{prefix}*.{extension}"))
}

// Licenses that let anyone redistribute the work. CurseForge names them in its own words, like "MIT
// License" or "GNU Lesser General Public License version 3".
pub fn is_open_license(license: &str) -> bool {
    static OPEN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)\b(MIT|Apache|BSD|ISC|Unlicense|CC0|zlib|MPL|L?GPL|AGPL)\b|General Public License|Mozilla Public|Eclipse Public|Public Domain").expect("valid regex")
    });
    OPEN.is_match(license)
}

/// Returns what we know about the project behind a file in the pack.
pub fn project_of<'a>(pack: &Pack, cache: &'a MetadataCache, path: &str) -> Option<&'a ProjectName> {
    pack.lock.files.iter().find(|e| e.path == path).and_then(LockEntry::key).and_then(|key| cache.known_name(&key))
}

pub struct OverrideFiles {
    /// Files the platform doesn't host that you haven't decided about yet.
    pub undecided: Vec<String>,
    /// Ones the platform's pack carries, with the author whose permission ships each, when that's why.
    pub shipped: Vec<(String, Option<String>)>,
    /// Ones the platform's pack leaves out.
    pub left_out: Vec<String>,
}

impl OverrideFiles {
    pub fn none() -> Self {
        Self { undecided: Vec::new(), shipped: Vec::new(), left_out: Vec::new() }
    }
}

/// Sorts the files the platform's pack would carry in overrides by what you decided about each.
pub fn override_files_for(pack: &Pack, cache: &MetadataCache, platform: Platform) -> Result<OverrideFiles> {
    let (shipped, kept_off) = pack.config.override_choices(platform);
    let allowed = globs(&literal_paths(shipped))?;
    let excluded = globs(&literal_paths(kept_off))?;
    let allowed_authors = pack.config.allowed_authors(platform);
    let permitting_author = |path: &str| {
        let authors = project_of(pack, cache, path).and_then(|p| p.authors.as_ref())?;
        authors.iter().find(|author| allowed_authors.iter().any(|allowed| allowed.eq_ignore_ascii_case(author))).cloned()
    };
    let mut paths = match platform {
        Platform::Curseforge => curseforge_override_jars(pack)?,
        Platform::Modrinth => {
            let mut paths: Vec<String> = pack.lock.files.iter().filter(|e| e.modrinth.is_none() && !e.path.ends_with(".disabled")).map(|e| e.path.clone()).collect();
            paths.extend(override_files(pack, &[])?.into_iter().filter(|path| is_moderated_override(path)));
            paths
        }
    };
    paths.sort();
    paths.dedup();
    let mut files = OverrideFiles::none();
    for path in paths {
        if excluded.is_match(&path) {
            files.left_out.push(path);
        } else if allowed.is_match(&path) {
            files.shipped.push((path, None));
        } else if let Some(author) = permitting_author(&path) {
            files.shipped.push((path, Some(author)));
        } else {
            files.undecided.push(path);
        }
    }
    Ok(files)
}

/// Returns the files the platform would get in overrides that you haven't shipped there or kept off it.
pub fn not_on(pack: &Pack, cache: &MetadataCache, platform: Platform) -> Result<Vec<String>> {
    Ok(override_files_for(pack, cache, platform)?.undecided)
}

// The CurseForge app can leave the old jar behind when it can't delete it during an update. Loaders
// try to load only the newest of two versions, but sometimes pick the wrong one, and the platforms
// reject a manifest that lists one project twice. Packs can hold several variants of one resource
// pack, so we only check mods.
pub fn duplicate_mods(pack: &Pack) -> Vec<Vec<String>> {
    let mut by_project: BTreeMap<ModKey, Vec<String>> = BTreeMap::new();
    for entry in pack.lock.files.iter().filter(|e| e.path.starts_with("mods/") && !e.path.ends_with(".disabled")) {
        if let Some(key) = entry.key() {
            by_project.entry(key).or_default().push(entry.path.trim_start_matches("mods/").to_string());
        }
    }
    by_project.into_values().filter(|paths| paths.len() > 1).collect()
}

pub fn duplicate_mods_problem(pack: &Pack) -> Option<String> {
    let groups = duplicate_mods(pack);
    let listed: Vec<String> = groups.iter().map(|files| files.join(" and ")).collect();
    match groups.len() {
        0 => None,
        1 => Some(format!("The mods folder has two versions of one mod: {}. In Mods, remove the one you don't want.", listed[0])),
        _ => Some(format!("The mods folder has two versions of {} mods: {}. In Mods, remove the ones you don't want.", groups.len(), listed.join("; "))),
    }
}

pub fn local_dev_jars(pack: &Pack) -> Vec<String> {
    std::fs::read_dir(pack.dir.join("mods"))
        .map(|dir| {
            dir.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|name| name.ends_with("-dev.jar"))
                .collect()
        })
        .unwrap_or_default()
}

fn options() -> SimpleFileOptions {
    SimpleFileOptions::default().compression_method(CompressionMethod::Deflated).large_file(true)
}

fn add_file(zip: &mut ZipWriter<File>, name: &str, source: &Path, executable: bool) -> Result<()> {
    let already_compressed = name.ends_with(".jar") || name.ends_with(".zip");
    let options = if already_compressed { options().compression_method(CompressionMethod::Stored) } else { options() };
    let options = if executable { options.unix_permissions(0o755) } else { options };
    zip.start_file(name, options)?;
    std::io::copy(&mut File::open(source).with_context(|| format!("reading {}", source.display()))?, zip)?;
    Ok(())
}

pub fn client_export(pack: &Pack, cache: &MetadataCache, version: &str, out_dir: &Path, log: &Log) -> Result<PathBuf> {
    std::fs::create_dir_all(out_dir)?;
    let path = out_dir.join(pack.client_zip_name(version));
    let files: Vec<_> = pack
        .lock
        .files
        .iter()
        .filter(|e| !e.path.ends_with(".disabled"))
        .filter_map(|e| Some((e, e.curseforge.as_ref()?)))
        .map(|(e, f)| json!({ "projectID": f.project_id, "fileID": f.file_id, "required": true, "isLocked": pack.config.updates.locked.iter().any(|key| e.has_key(key)) }))
        .collect();
    let manifest = json!({
        "minecraft": {
            "version": pack.lock.minecraft()?,
            "modLoaders": [{ "id": pack.lock.mod_loader()?.manifest_id(), "primary": true }],
            "recommendedRam": pack.config.export.recommended_ram_mb,
        },
        "manifestType": "minecraftModpack",
        "manifestVersion": 1,
        "name": pack.name(),
        "version": version,
        "author": pack.config.author,
        "files": files,
        "overrides": "overrides",
    });
    if let Some(problem) = duplicate_mods_problem(pack) {
        bail!(problem);
    }
    let blocked = not_on(pack, cache, Platform::Curseforge)?;
    if !blocked.is_empty() {
        bail!("CurseForge rejects packs with jars it doesn't host in overrides: {}. In Publish, include them in the CurseForge pack or leave them out.", blocked.join(", "));
    }
    let kept_off = literal_paths(&pack.config.curseforge.exclude);
    let left_out = globs(&kept_off)?;
    let overrides = override_files(pack, &[vec![SERVER_ICON.to_string()], kept_off].concat())?;
    let mut zip = ZipWriter::new(File::create(&path)?);
    zip.start_file("manifest.json", options())?;
    zip.write_all(serde_json::to_string_pretty(&manifest)?.as_bytes())?;
    for file in &overrides {
        let name = format!("overrides/{file}");
        match pack.config.export.properties.get(file) {
            Some(forced) => {
                zip.start_file(name, options())?;
                zip.write_all(force_properties(&std::fs::read_to_string(pack.dir.join(file))?, forced).as_bytes())?;
            }
            None => add_file(&mut zip, &name, &pack.dir.join(file), false)?,
        }
    }
    for entry in only_on_modrinth(pack).filter(|e| !left_out.is_match(&e.path)) {
        add_file(&mut zip, &format!("overrides/{}", entry.path), &pack.dir.join(&entry.path), false)?;
    }
    zip.finish()?;
    log.info(format!("Client export: {} CurseForge files, {} override files", files.len(), overrides.len()));
    Ok(path)
}

pub fn modrinth_export(pack: &Pack, sides: &[ModSide], cache: &MetadataCache, version: &str, out_dir: &Path, log: &Log) -> Result<PathBuf> {
    if let Some(problem) = duplicate_mods_problem(pack) {
        bail!(problem);
    }
    let blocked = not_on(pack, cache, Platform::Modrinth)?;
    if !blocked.is_empty() {
        bail!("Modrinth won't publish a pack with files it doesn't host unless you have permission to share them: {}. In Publish, include them in the Modrinth pack or leave them out.", blocked.join(", "));
    }
    let left_out = globs(&literal_paths(&pack.config.modrinth.exclude))?;
    let side_of: HashMap<&str, Side> = sides.iter().map(|m| (m.path.as_str(), m.side)).collect();
    let server_excluded = globs(&pack.config.server.exclude)?;
    let client_only = |path: &str| side_of.get(path) == Some(&Side::Client) || server_excluded.is_match(path);
    let mut files = Vec::new();
    let mut from_disk = Vec::new();
    for entry in pack.lock.files.iter().filter(|e| !e.path.ends_with(".disabled") && !left_out.is_match(&e.path)) {
        let Some(file) = &entry.modrinth else {
            from_disk.push(entry.path.as_str());
            continue;
        };
        check_pack_download(&file.url)?;
        let (client, server) = match (client_only(&entry.path), side_of.get(entry.path.as_str())) {
            (true, _) => ("required", "unsupported"),
            (false, Some(Side::Server)) => ("unsupported", "required"),
            _ => ("required", "required"),
        };
        files.push(json!({
            "path": entry.path,
            "hashes": { "sha1": file.sha1, "sha512": file.sha512 },
            "env": { "client": client, "server": server },
            "downloads": [file.url],
            "fileSize": file.size,
        }));
    }
    let loader = pack.lock.mod_loader()?;
    let (loader_key, loader_version) = match loader {
        ModLoader::NeoForge(version) => ("neoforge", version),
        ModLoader::Forge(version) => ("forge", version),
        ModLoader::Fabric(version) => ("fabric-loader", version),
    };
    let index = json!({
        "formatVersion": 1,
        "game": "minecraft",
        "versionId": version,
        "name": pack.name(),
        "files": files,
        "dependencies": { "minecraft": pack.lock.minecraft()?, loader_key: loader_version },
    });

    std::fs::create_dir_all(out_dir)?;
    let path = out_dir.join(pack.modrinth_pack_name(version));
    let mut zip = ZipWriter::new(File::create(&path)?);
    zip.start_file("modrinth.index.json", options())?;
    zip.write_all(serde_json::to_string_pretty(&index)?.as_bytes())?;
    let overrides = override_files(pack, &[vec![SERVER_ICON.to_string()], literal_paths(&pack.config.modrinth.exclude)].concat())?;
    // Modrinth's launchers apply client-overrides only to clients, so these stay off servers the way
    // they stay out of our server pack.
    let folder = |path: &str| if client_only(path) { "client-overrides" } else { "overrides" };
    for file in &overrides {
        let name = format!("{}/{file}", folder(file));
        match pack.config.export.properties.get(file) {
            Some(forced) => {
                zip.start_file(name, options())?;
                zip.write_all(force_properties(&std::fs::read_to_string(pack.dir.join(file))?, forced).as_bytes())?;
            }
            None => add_file(&mut zip, &name, &pack.dir.join(file), false)?,
        }
    }
    for file in &from_disk {
        add_file(&mut zip, &format!("{}/{file}", folder(file)), &pack.dir.join(file), false)?;
    }
    if git::tracked_files(&pack.dir)?.iter().any(|f| f == SERVER_ICON) {
        add_file(&mut zip, &format!("server-overrides/{SERVER_ICON}"), &pack.dir.join(SERVER_ICON), false)?;
    }
    zip.finish()?;
    log.info(format!("Modrinth pack: {} Modrinth files, {} override files", files.len(), overrides.len() + from_disk.len()));
    Ok(path)
}

// Modrinth refuses a pack whose downloads come from another host, or whose URLs aren't written the
// way its URL parser writes them back.
fn check_pack_download(url: &str) -> Result<()> {
    let parsed = reqwest::Url::parse(url).with_context(|| format!("{url} isn't a valid download URL"))?;
    if parsed.as_str() != url || parsed.scheme() != "https" || !parsed.host_str().is_some_and(|host| modrinth::PACK_DOWNLOAD_HOSTS.contains(&host)) {
        bail!("Modrinth won't accept {url} as a download in a modpack");
    }
    Ok(())
}

fn force_properties(text: &str, forced: &BTreeMap<String, String>) -> String {
    let mut remaining: Vec<&String> = forced.keys().collect();
    let mut out: Vec<String> = text
        .lines()
        .map(|line| {
            let key = line.split('=').next().unwrap_or_default().trim();
            match forced.get(key) {
                Some(value) if !line.trim_start().starts_with('#') => {
                    remaining.retain(|k| k.as_str() != key);
                    format!("{key}={value}")
                }
                _ => line.to_string(),
            }
        })
        .collect();
    out.extend(remaining.into_iter().map(|key| format!("{key}={}", forced[key])));
    out.join("\n") + "\n"
}

pub async fn ensure_cached(pack: &Pack, entry: &LockEntry) -> Result<PathBuf> {
    let path = pack.cache_dir()?.join(&entry.sha1);
    if !path.exists() {
        let url = entry.download_url().with_context(|| format!("packraven has no download for {}", entry.path))?;
        http::download(url, &path, Some(&entry.sha1)).await?;
    }
    Ok(path)
}

#[derive(Deserialize)]
struct FabricInstaller {
    url: String,
    version: String,
    stable: bool,
}

async fn stable_fabric_installer() -> Result<FabricInstaller> {
    let installers: Vec<FabricInstaller> = http::CLIENT.get(format!("{FABRIC_META}/installer")).send().await?.error_for_status()?.json().await?;
    installers.into_iter().find(|i| i.stable).context("Fabric lists no stable installer")
}

fn forge_installer_url(minecraft: &str, loader: &ModLoader) -> Option<String> {
    match loader {
        ModLoader::NeoForge(version) => Some(format!("{NEOFORGE_MAVEN}/{version}/neoforge-{version}-installer.jar")),
        ModLoader::Forge(version) => Some(format!("{FORGE_MAVEN}/{minecraft}-{version}/forge-{minecraft}-{version}-installer.jar")),
        ModLoader::Fabric(_) => None,
    }
}

fn url_file_name(url: &str) -> &str {
    url.rsplit('/').next().unwrap_or(url)
}

async fn download_once(url: &str, path: &Path, what: &str, log: &Log) -> Result<PathBuf> {
    if !path.exists() {
        log.info(format!("Downloading {what}"));
        http::download(url, path, None).await?;
    }
    Ok(path.to_path_buf())
}

async fn fabric_server_launcher(pack: &Pack, folder: &Path, log: &Log) -> Result<PathBuf> {
    let minecraft = pack.lock.minecraft()?;
    let ModLoader::Fabric(version) = pack.lock.mod_loader()? else { bail!("only Fabric packs have a Fabric server launcher") };
    let installer = stable_fabric_installer().await?.version;
    let url = format!("{FABRIC_META}/loader/{minecraft}/{version}/{installer}/server/jar");
    let cached = folder.join(format!("fabric-server-mc.{minecraft}-loader.{version}-launcher.{installer}.jar"));
    download_once(&url, &cached, "Fabric's server launcher", log).await
}

async fn loader_server_base(pack: &Pack, java: &Path, data_dir: &Path, log: &Log) -> Result<PathBuf> {
    let minecraft = pack.lock.minecraft()?;
    let loader = pack.lock.mod_loader()?;
    let folder = data_dir.join(loader.name().to_lowercase());
    let name = match loader {
        ModLoader::NeoForge(version) => format!("server-{version}"),
        ModLoader::Forge(version) | ModLoader::Fabric(version) => format!("server-{minecraft}-{version}"),
    };
    let base = folder.join(&name);
    if base.join("libraries").exists() {
        return Ok(base);
    }
    let (installer_url, args) = match (forge_installer_url(minecraft, &loader), &loader) {
        (Some(url), _) => (url, vec!["--installServer"]),
        (None, ModLoader::Fabric(version)) => (stable_fabric_installer().await?.url, vec!["server", "-mcversion", minecraft, "-loader", version, "-downloadMinecraft", "-dir"]),
        (None, _) => bail!("packraven doesn't know where to download the {} installer", loader.name()),
    };
    let installer = download_once(&installer_url, &folder.join(url_file_name(&installer_url)), &format!("the {} installer", loader.name()), log).await?;
    let staging = folder.join(format!("{name}-staging"));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    log.info(format!("Installing the {} server for Minecraft {minecraft}", loader.name()));
    let mut command = Command::new(java);
    command.current_dir(&staging).arg("-jar").arg(&installer).args(args).arg(&staging);
    crate::process::run_logged(command, log).await.with_context(|| format!("{} server install failed", loader.name()))?;
    if let ModLoader::Fabric(_) = loader {
        unpack_vanilla_bundler(&staging, java, log).await?;
    }
    for entry in std::fs::read_dir(&staging)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().ends_with(".log") {
            std::fs::remove_file(entry.path())?;
        }
    }
    std::fs::rename(&staging, &base)?;
    Ok(base)
}

// Since 1.18, vanilla's server.jar bundles its libraries and unpacks them on every start that
// finds them missing. The NeoForge and Forge installers unpack them up front, but Fabric's
// doesn't, so we do it here. Otherwise each test run's sync deletes them again.
async fn unpack_vanilla_bundler(dir: &Path, java: &Path, log: &Log) -> Result<()> {
    let bundled = File::open(dir.join("server.jar")).ok().and_then(|jar| zip::ZipArchive::new(jar).ok()).is_some_and(|mut jar| jar.by_name("META-INF/versions.list").is_ok());
    if !bundled {
        return Ok(());
    }
    log.info("Unpacking the vanilla server's libraries");
    let mut command = Command::new(java);
    // Vanilla's data generator prints its usage and exits once the bundler has unpacked everything.
    command.current_dir(dir).arg("-DbundlerMainClass=net.minecraft.data.Main").arg("-jar").arg("server.jar").arg("--help");
    crate::process::run_logged(command, log).await.context("unpacking the vanilla server failed")?;
    let _ = std::fs::remove_dir_all(dir.join("logs"));
    Ok(())
}

pub async fn test_server_plan(pack: &Pack, sides: &[ModSide], version: &str, java: &Path, data_dir: &Path, log: &Log) -> Result<FilePlan> {
    let base = loader_server_base(pack, java, data_dir, log).await?;
    let mut plan = FilePlan::new();
    for entry in WalkDir::new(&base) {
        let entry = entry?;
        if entry.file_type().is_file() {
            let relative = entry.path().strip_prefix(&base)?.to_string_lossy().replace('\\', "/");
            plan.insert(relative, Planned::Copy(entry.into_path()));
        }
    }
    plan.extend(pack_server_files(pack, sides, version, ServerMods::Bundled, log).await?);
    Ok(plan)
}

// The Minecraft EULA doesn't let us redistribute vanilla's server, so release server packs carry the
// loader's installer (or Fabric's server launcher) instead, and that downloads it from Mojang on first start.
async fn release_server_plan(pack: &Pack, sides: &[ModSide], version: &str, mods: ServerMods, data_dir: &Path, log: &Log) -> Result<FilePlan> {
    let mut plan = pack_server_files(pack, sides, version, mods, log).await?;
    if mods == ServerMods::DownloadedOnStart {
        plan.insert(DOWNLOADER.into(), Planned::Copy(downloader_jar(data_dir)?));
    }
    let loader = pack.lock.mod_loader()?;
    let folder = data_dir.join(loader.name().to_lowercase());
    match forge_installer_url(pack.lock.minecraft()?, &loader) {
        Some(url) => {
            let name = url_file_name(&url);
            let installer = download_once(&url, &folder.join(name), &format!("the {} installer", loader.name()), log).await?;
            plan.insert(name.to_string(), Planned::Copy(installer));
        }
        None => {
            plan.insert(FABRIC_LAUNCHER.into(), Planned::Copy(fabric_server_launcher(pack, &folder, log).await?));
        }
    }
    Ok(plan)
}

fn downloader_jar(data_dir: &Path) -> Result<PathBuf> {
    let path = data_dir.join(DOWNLOADER);
    if std::fs::read(&path).ok().as_deref() != Some(DOWNLOADER_JAR) {
        std::fs::create_dir_all(data_dir)?;
        std::fs::write(&path, DOWNLOADER_JAR)?;
    }
    Ok(path)
}

// Our downloader runs on whatever Java the server has, so we hand it URLs exactly as they go on the
// wire. edge.forgecdn.net only redirects to mediafilez.forgecdn.net, which serves every file without
// an API key, even when the author turned off third-party distribution.
fn server_download_url(entry: &LockEntry) -> Option<String> {
    let mut url = reqwest::Url::parse(entry.download_url()?).ok()?;
    if url.host_str() == Some("edge.forgecdn.net") {
        url.set_host(Some("mediafilez.forgecdn.net")).ok()?;
    }
    Some(url.to_string()).filter(|url| url.starts_with("https://") && !url.contains(['\t', '\n']))
}

async fn pack_server_files(pack: &Pack, sides: &[ModSide], version: &str, mods: ServerMods, log: &Log) -> Result<FilePlan> {
    let mut plan = FilePlan::new();
    let client_only: HashSet<&str> = sides.iter().filter(|m| m.side == Side::Client).map(|m| m.path.as_str()).collect();
    let server_files: Vec<&LockEntry> = pack
        .lock
        .files
        .iter()
        .filter(|e| !e.path.ends_with(".disabled") && !client_only.contains(e.path.as_str()))
        .filter(|e| e.path.starts_with("mods/") || e.path.starts_with("datapacks/"))
        .collect();
    log.info(format!("Including {} server files ({} client-only skipped)", server_files.len(), client_only.len()));
    let mut download_list = String::new();
    for entry in &server_files {
        match server_download_url(entry).filter(|_| mods == ServerMods::DownloadedOnStart) {
            Some(url) => download_list.push_str(&format!("{}\t{}\t{url}\t{}\n", entry.sha1, entry.size, entry.path)),
            None => {
                plan.insert(entry.path.clone(), Planned::Copy(ensure_cached(pack, entry).await?));
            }
        }
    }
    if mods == ServerMods::DownloadedOnStart {
        log.info(format!("The server downloads {} of them on first start", download_list.lines().count()));
        plan.insert(DOWNLOAD_LIST.into(), Planned::Write(download_list));
    }

    let overrides = override_files(pack, &pack.config.server.exclude)?;
    log.info(format!("Including {} config and override files", overrides.len()));
    for file in overrides {
        let source = pack.dir.join(&file);
        plan.insert(file, Planned::Copy(source));
    }
    // We ship the icon before it's committed, so a test run shows the one you just picked.
    let icon = pack.dir.join(SERVER_ICON);
    if icon.is_file() {
        plan.insert(SERVER_ICON.into(), Planned::Copy(icon));
    }

    let mut properties = pack.config.server.properties.clone();
    let motd = fill_motd(properties.get("motd").map_or(DEFAULT_MOTD, String::as_str), &pack.name(), version, pack.lock.minecraft.as_deref().unwrap_or_default());
    properties.insert("motd".into(), motd);
    plan.insert("server.properties".into(), Planned::Write(properties.iter().map(|(k, v)| format!("{k}={}\n", escape_property(v))).collect()));
    plan.insert("user_jvm_args.txt".into(), Planned::Write(user_jvm_args(pack)));
    // We replace the NeoForge and Forge installers' run scripts, which call whatever java is on
    // PATH, and Fabric's installer writes none.
    let graalvm = server_graalvm(pack)?;
    let args = &pack.config.server.args;
    let download = |step: &'static str| if mods == ServerMods::DownloadedOnStart { step } else { "" };
    let run_bat = start_script(RUN_BAT, download(DOWNLOAD_BAT), &install_step(pack, true)?, &loader_launch(pack, true)?, graalvm.as_deref(), &script_args(args, true)).replace('\n', "\r\n");
    plan.insert("run.bat".into(), Planned::Write(run_bat));
    plan.insert("run.sh".into(), Planned::Write(start_script(RUN_SH, download(DOWNLOAD_SH), &install_step(pack, false)?, &loader_launch(pack, false)?, graalvm.as_deref(), &script_args(args, false))));
    Ok(plan)
}

// Fabric's server launcher installs everything itself when it starts, so only NeoForge and Forge need this.
fn install_step(pack: &Pack, windows: bool) -> Result<String> {
    let loader = pack.lock.mod_loader()?;
    let Some(url) = forge_installer_url(pack.lock.minecraft()?, &loader) else { return Ok(String::new()) };
    let launch = loader_launch(pack, windows)?;
    let launched = launched_file(&launch);
    let (template, launched) = if windows { (INSTALL_BAT, launched.replace('/', "\\")) } else { (INSTALL_SH, launched.to_string()) };
    Ok(template.replace("{{LOADER}}", loader.name()).replace("{{INSTALLER}}", url_file_name(&url)).replace("{{LAUNCHED}}", &launched))
}

const DEFAULT_MOTD: &str = "{name} {version}";

fn fill_motd(template: &str, name: &str, version: &str, minecraft: &str) -> String {
    template.replace("{name}", name).replace("{version}", version).replace("{minecraft}", minecraft)
}

// Older Minecraft versions read server.properties as Latin-1, so a MOTD's section signs and other
// non-ASCII text only survive as Java's \uXXXX escapes.
fn escape_property(value: &str) -> String {
    value.encode_utf16().map(|unit| if unit < 0x80 { char::from(unit as u8).to_string() } else { format!("\\u{unit:04X}") }).collect()
}

fn user_jvm_args(pack: &Pack) -> String {
    let mut text = format!("-Xmx{}\n", pack.config.server.memory);
    for flag in pack.config.server.jvm_args.iter().map(|flag| flag.trim()).filter(|flag| !flag.is_empty()) {
        text.push_str(flag);
        text.push('\n');
    }
    text
}

fn script_args(args: &[String], windows: bool) -> String {
    args.iter()
        .map(|arg| arg.trim())
        .filter(|arg| !arg.is_empty())
        .map(|arg| {
            let plain = arg.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_=.,:/@+".contains(&b));
            match (plain, windows) {
                (true, _) => format!(" {arg}"),
                // cmd expands %VAR% even inside quotes, so we double the percent signs.
                (false, true) => format!(" \"{}\"", arg.replace('%', "%%").replace('"', "")),
                (false, false) => format!(" '{}'", arg.replace('\'', "'\\''")),
            }
        })
        .collect()
}

pub fn loader_launch(pack: &Pack, windows: bool) -> Result<String> {
    let args_file = if windows { "win_args.txt" } else { "unix_args.txt" };
    Ok(match pack.lock.mod_loader()? {
        ModLoader::NeoForge(version) => format!("@libraries/net/neoforged/neoforge/{version}/{args_file}"),
        ModLoader::Forge(version) => format!("@libraries/net/minecraftforge/forge/{}-{version}/{args_file}", pack.lock.minecraft()?),
        ModLoader::Fabric(_) => format!("-jar {FABRIC_LAUNCHER}"),
    })
}

fn launched_file(launch: &str) -> &str {
    launch.trim_start_matches('@').trim_start_matches("-jar ")
}

pub fn server_args(pack: &Pack, dir: &Path, extra_jvm: &[String], extra: &[String]) -> Result<Vec<String>> {
    let launch = loader_launch(pack, cfg!(windows))?;
    let launched_file = launched_file(&launch);
    if !dir.join(launched_file).exists() {
        bail!("the {} installer didn't create {launched_file}, so packraven can't start this server", pack.lock.mod_loader()?.name());
    }
    let args = pack.config.server.args.iter().map(|arg| arg.trim()).filter(|arg| !arg.is_empty());
    Ok(["@user_jvm_args.txt"]
        .into_iter()
        .chain(extra_jvm.iter().map(String::as_str))
        .chain(launch.split(' '))
        .chain(["nogui"])
        .chain(args)
        .chain(extra.iter().map(String::as_str))
        .map(String::from)
        .collect())
}

fn server_graalvm(pack: &Pack) -> Result<Option<String>> {
    let configured = pack.config.server.graalvm.trim();
    if configured.is_empty() {
        return Ok(crate::java::graalvm_for(crate::launch::java_for_minecraft(pack.lock.minecraft()?).1).map(|major| major.to_string()));
    }
    let parts: Vec<&str> = configured.split('.').collect();
    let valid = parts.len() <= 3 && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())) && parts[0].parse::<u32>().is_ok_and(|major| major >= 17);
    if !valid {
        bail!("server.graalvm in packraven.toml must be a GraalVM version like 21 or 21.0.7, found {configured}");
    }
    Ok(Some(configured.to_string()))
}

fn graalvm_url_prefix(version: &str) -> String {
    match version.split_once('.') {
        Some((major, _)) => format!("https://download.oracle.com/graalvm/{major}/archive/graalvm-jdk-{version}_"),
        None => format!("https://download.oracle.com/graalvm/{version}/latest/graalvm-jdk-{version}_"),
    }
}

fn start_script(template: &str, download: &str, install: &str, launch: &str, graalvm: Option<&str>, args: &str) -> String {
    template
        .replace("{{DOWNLOAD}}", download)
        .replace("{{INSTALL}}", install)
        .replace("\r\n", "\n")
        .replace("{{GRAALVM}}", graalvm.unwrap_or_default())
        .replace("{{URL_PREFIX}}", &graalvm.map(graalvm_url_prefix).unwrap_or_default())
        .replace("{{LAUNCH}}", launch)
        .replace("{{ARGS}}", args)
}

pub async fn server_pack(pack: &Pack, sides: &[ModSide], version: &str, mods: ServerMods, out_dir: &Path, data_dir: &Path, log: &Log) -> Result<PathBuf> {
    let dir = out_dir.join("server");
    let plan = release_server_plan(pack, sides, version, mods, data_dir, log).await?;
    log.info(mirror(&dir, &plan, &[], false)?.to_string());
    Ok(dir)
}

pub async fn zip_server_pack(pack: &Pack, version: &str, out_dir: &Path, log: &Log) -> Result<PathBuf> {
    let dir = out_dir.join("server");
    let zip_path = out_dir.join(pack.server_zip_name(version));
    let files = WalkDir::new(&dir).into_iter().filter_map(|e| e.ok()).filter(|e| e.file_type().is_file()).count();
    log.info(format!("Zipping the server pack ({files} files). This can take a few minutes."));
    let (source, dest) = (dir, zip_path.clone());
    tokio::task::spawn_blocking(move || zip_dir(&source, &dest)).await??;
    log.info(format!("Server pack: {}", zip_path.display()));
    Ok(zip_path)
}

fn zip_dir(source: &Path, dest: &Path) -> Result<()> {
    let mut zip = ZipWriter::new(File::create(dest)?);
    for entry in WalkDir::new(source).sort_by_file_name() {
        let entry = entry?;
        if entry.file_type().is_file() {
            let name = entry.path().strip_prefix(source)?.to_string_lossy().replace('\\', "/");
            add_file(&mut zip, &name, entry.path(), name.ends_with(".sh"))?;
        }
    }
    zip.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modrinth_file(name: &str) -> crate::pack::ModrinthFile {
        crate::pack::ModrinthFile {
            project_id: format!("{name}-project"),
            version_id: format!("{name}-version"),
            sha1: format!("{name}-sha1"),
            sha512: format!("{name}-sha512"),
            size: 7,
            url: format!("https://cdn.modrinth.com/data/{name}/versions/1/{name}.jar"),
        }
    }

    fn side(path: &str, side: Side) -> ModSide {
        ModSide { path: path.into(), key: None, name: path.into(), url: None, side, reason: String::new(), r#override: crate::sides::Override::Auto, required_by: Vec::new() }
    }

    #[test]
    fn builds_an_mrpack_that_keeps_client_files_off_servers() {
        let dir = std::env::temp_dir().join(format!("packraven-mrpack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (path, text) in [("mods/own.jar", "own"), ("config/a.toml", "a = 1"), ("options.txt", "fov:1"), ("server-icon.png", "png")] {
            std::fs::create_dir_all(dir.join(path).parent().unwrap()).unwrap();
            std::fs::write(dir.join(path), text).unwrap();
        }
        git::run(&dir, &["init", "-q"]).unwrap();
        git::run(&dir, &["add", "config/a.toml", "options.txt", "server-icon.png"]).unwrap();

        let on_modrinth = |path: &str, name: &str| LockEntry { path: path.into(), sha1: "disk".into(), size: 1, curseforge: None, modrinth: Some(modrinth_file(name)) };
        let own = LockEntry::curseforge("mods/own.jar".into(), "own".into(), 3, crate::pack::CurseforgeFile { project_id: 1, file_id: 2, url: String::new() });
        let ftb = LockEntry::curseforge("mods/ftb-library [1.21].jar".into(), "ftb".into(), 3, crate::pack::CurseforgeFile { project_id: 3, file_id: 4, url: String::new() });
        let files = vec![on_modrinth("mods/sodium.jar", "sodium"), on_modrinth("mods/create.jar", "create"), on_modrinth("resourcepacks/fa.zip", "fa"), own, ftb];
        let mut pack = Pack { dir: dir.clone(), config: Default::default(), lock: crate::pack::Lockfile { minecraft: Some("1.21.1".into()), loader: crate::pack::Loader::from_curseforge("neoforge-21.1.259"), files } };
        pack.config.modrinth.allowed_override_jars = vec!["mods/own.jar".into()];
        pack.config.modrinth.exclude = vec!["mods/ftb-library [1.21].jar".into()];
        let sides = [side("mods/sodium.jar", Side::Client), side("mods/create.jar", Side::Both), side("mods/own.jar", Side::Both)];

        let path = modrinth_export(&pack, &sides, &MetadataCache::default(), "1.0", &dir.join("out"), &Log::console("test")).unwrap();
        let mut archive = zip::ZipArchive::new(File::open(&path).unwrap()).unwrap();
        let mut names: Vec<String> = archive.file_names().map(|n| n.unwrap().into_owned()).collect();
        names.sort();
        assert_eq!(names, ["client-overrides/options.txt", "modrinth.index.json", "overrides/config/a.toml", "overrides/mods/own.jar", "server-overrides/server-icon.png"]);
        let index: serde_json::Value = serde_json::from_reader(archive.by_name("modrinth.index.json").unwrap()).unwrap();
        assert_eq!(index["dependencies"], json!({ "minecraft": "1.21.1", "neoforge": "21.1.259" }));
        let env = |path: &str| index["files"].as_array().unwrap().iter().find(|f| f["path"] == path).map(|f| f["env"].clone()).unwrap();
        assert_eq!(env("mods/sodium.jar"), json!({ "client": "required", "server": "unsupported" }));
        assert_eq!(env("mods/create.jar"), json!({ "client": "required", "server": "required" }));
        assert_eq!(env("resourcepacks/fa.zip"), json!({ "client": "required", "server": "unsupported" }), "the server pack leaves resource packs out");
        let sodium = index["files"].as_array().unwrap().iter().find(|f| f["path"] == "mods/sodium.jar").unwrap();
        assert_eq!(sodium["hashes"], json!({ "sha1": "sodium-sha1", "sha512": "sodium-sha512" }), "Modrinth's copy, not the jar on disk");

        pack.config.modrinth.allowed_override_jars.clear();
        let error = modrinth_export(&pack, &sides, &MetadataCache::default(), "1.0", &dir.join("out"), &Log::console("test")).unwrap_err();
        assert!(error.to_string().contains("mods/own.jar"), "{error}");
        assert_eq!(not_on(&pack, &MetadataCache::default(), Platform::Modrinth).unwrap(), ["mods/own.jar"]);
        assert_eq!(override_files_for(&pack, &MetadataCache::default(), Platform::Modrinth).unwrap().left_out, ["mods/ftb-library [1.21].jar"], "a left-out file never blocks the Modrinth pack");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn builds_a_curseforge_pack_with_the_jars_you_ship_there() {
        let dir = std::env::temp_dir().join(format!("packraven-cfpack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for path in ["mods/own.jar", "mods/only-modrinth.jar", "mods/kept-off [1.21].jar", "config/a.toml"] {
            std::fs::create_dir_all(dir.join(path).parent().unwrap()).unwrap();
            std::fs::write(dir.join(path), path).unwrap();
        }
        git::run(&dir, &["init", "-q"]).unwrap();
        git::run(&dir, &["add", "mods/own.jar", "config/a.toml"]).unwrap();
        let on_modrinth = |path: &str, name: &str| LockEntry { path: path.into(), sha1: "disk".into(), size: 1, curseforge: None, modrinth: Some(modrinth_file(name)) };
        let on_curseforge = LockEntry::curseforge("mods/create.jar".into(), "create".into(), 1, crate::pack::CurseforgeFile { project_id: 5, file_id: 6, url: String::new() });
        let files = vec![on_modrinth("mods/only-modrinth.jar", "only"), on_modrinth("mods/kept-off [1.21].jar", "kept"), on_modrinth("resourcepacks/fa.zip", "fa"), on_curseforge];
        let mut pack = Pack { dir: dir.clone(), config: Default::default(), lock: crate::pack::Lockfile { minecraft: Some("1.21.1".into()), loader: crate::pack::Loader::from_curseforge("neoforge-21.1.259"), files } };
        std::fs::create_dir_all(dir.join("resourcepacks")).unwrap();
        std::fs::write(dir.join("resourcepacks/fa.zip"), "fa").unwrap();

        let cache = MetadataCache::default();
        assert_eq!(not_on(&pack, &cache, Platform::Curseforge).unwrap(), ["mods/kept-off [1.21].jar", "mods/only-modrinth.jar", "mods/own.jar"], "CurseForge only rejects jars");
        assert!(client_export(&pack, &cache, "1.0", &dir.join("out"), &Log::console("test")).is_err());

        pack.config.curseforge.allowed_override_jars = vec!["mods/own.jar".into(), "mods/only-modrinth.jar".into()];
        pack.config.curseforge.exclude = vec!["mods/kept-off [1.21].jar".into()];
        let path = client_export(&pack, &cache, "1.0", &dir.join("out"), &Log::console("test")).unwrap();
        let mut archive = zip::ZipArchive::new(File::open(&path).unwrap()).unwrap();
        let mut names: Vec<String> = archive.file_names().map(|n| n.unwrap().into_owned()).collect();
        names.sort();
        assert_eq!(names, ["manifest.json", "overrides/config/a.toml", "overrides/mods/only-modrinth.jar", "overrides/mods/own.jar", "overrides/resourcepacks/fa.zip"]);
        let manifest: serde_json::Value = serde_json::from_reader(archive.by_name("manifest.json").unwrap()).unwrap();
        assert_eq!(manifest["files"][0]["projectID"], 5);
        let modrinth = override_files_for(&pack, &cache, Platform::Modrinth).unwrap();
        assert!(modrinth.undecided.contains(&"mods/own.jar".to_string()), "a choice on CurseForge doesn't decide the Modrinth pack");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn any_version_pattern_keeps_the_name_before_its_version() {
        assert_eq!(any_version_pattern("mods/Prominent-GLOBAL-MC1.20.1-4.1.5.jar").as_deref(), Some("mods/Prominent-GLOBAL-*.jar"));
        assert_eq!(any_version_pattern("mods/Prominent_OST-GLOBAL-MC1.20.1-1.0.0.jar").as_deref(), Some("mods/Prominent_OST-GLOBAL-*.jar"));
        assert_eq!(any_version_pattern("mods/sodium-extra-neoforge-0.6.6+mc1.21.1.jar").as_deref(), Some("mods/sodium-extra-neoforge-*.jar"));
        assert_eq!(any_version_pattern("mods/3dskinlayers-1.6.jar").as_deref(), Some("mods/3dskinlayers-*.jar"));
        assert_eq!(any_version_pattern("mods/ProgressiveVelocity1.2.jar").as_deref(), Some("mods/ProgressiveVelocity*.jar"));
        assert_eq!(any_version_pattern("resourcepacks/FreshAnimations-PT-BR.zip").as_deref(), Some("resourcepacks/FreshAnimations-PT-BR*.zip"));
        assert_eq!(any_version_pattern("mods/1.0.jar"), None);
    }

    #[test]
    fn a_pattern_choice_covers_other_versions_but_not_other_mods() {
        let pattern = "mods/Prominent-GLOBAL-*.jar";
        assert!(choice_covers(pattern, "mods/Prominent-GLOBAL-MC1.20.1-4.1.6.jar"));
        assert!(!choice_covers(pattern, "mods/Prominent_OST-GLOBAL-MC1.20.1-1.0.0.jar"));
        assert!(choice_covers("mods/ftb-library [1.21]-*.jar", "mods/ftb-library [1.21]-2.0.jar"), "only * is a wildcard");
        assert!(!choice_covers("mods/a?.jar", "mods/ab.jar"));
    }

    #[test]
    fn recognizes_licenses_that_let_anyone_share() {
        for open in ["MIT License", "Apache License version 2.0", "GNU Lesser General Public License version 3 (LGPLv3)", "GNU General Public License version 3 (GPLv3)", "Mozilla Public License 2.0 (MPL-2.0)", "BSD 3-Clause", "The Unlicense", "Public Domain"] {
            assert!(is_open_license(open), "{open}");
        }
        for closed in ["All Rights Reserved", "Custom License", "", "Creative Commons Attribution-NonCommercial 4.0"] {
            assert!(!is_open_license(closed), "{closed}");
        }
    }

    #[test]
    fn ships_every_file_by_an_author_who_gave_permission() {
        let file = |path: &str, project_id| LockEntry::curseforge(path.into(), String::new(), 0, crate::pack::CurseforgeFile { project_id, file_id: 0, url: String::new() });
        let dir = std::env::temp_dir().join(format!("packraven-authors-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        git::run(&dir, &["init", "-q"]).unwrap();
        let mut pack = Pack { dir: dir.clone(), config: Default::default(), lock: crate::pack::Lockfile { minecraft: None, loader: None, files: vec![file("mods/a.jar", 1), file("mods/b.jar", 2)] } };
        let mut cache = MetadataCache::default();
        for (id, author) in [(1, "ElocinDev"), (2, "Someone")] {
            let name = ProjectName { name: format!("Mod {id}"), url: None, authors: Some(vec![author.into()]), logo: None, license: Some(String::new()) };
            cache.remember(ModKey::Curseforge(id), name);
        }
        pack.config.modrinth.allowed_authors = vec!["elocindev".into()];
        let files = override_files_for(&pack, &cache, Platform::Modrinth).unwrap();
        assert_eq!(files.shipped, [("mods/a.jar".to_string(), Some("ElocinDev".to_string()))]);
        assert_eq!(files.undecided, ["mods/b.jar"]);
        pack.config.modrinth.exclude = vec!["mods/a.jar".into()];
        assert_eq!(override_files_for(&pack, &cache, Platform::Modrinth).unwrap().left_out, ["mods/a.jar"], "keeping a file off beats its author's permission");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn accepts_only_downloads_modrinth_accepts() {
        assert!(check_pack_download("https://cdn.modrinth.com/data/AANobbMI/versions/x/sodium.jar").is_ok());
        assert!(check_pack_download("https://github.com/a/b/releases/download/1/b.jar").is_ok());
        for refused in ["https://edge.forgecdn.net/files/1/2/a.jar", "https://gitlab.com/a/b.jar", "https://cdn.modrinth.com/data/a b.jar", "http://cdn.modrinth.com/a.jar"] {
            assert!(check_pack_download(refused).is_err(), "{refused}");
        }
    }

    #[test]
    fn force_properties_replaces_and_appends() {
        let forced: BTreeMap<String, String> = [("enableShaders", "false"), ("added", "1")].into_iter().map(|(k, v)| (k.into(), v.into())).collect();
        let text = "#enableShaders=true\nenableShaders=true\nshaderPack=x\n";
        assert_eq!(force_properties(text, &forced), "#enableShaders=true\nenableShaders=false\nshaderPack=x\nadded=1\n");
    }

    #[test]
    fn fills_motd_placeholders() {
        assert_eq!(fill_motd(DEFAULT_MOTD, "Ravens", "1.2.0", "1.21.1"), "Ravens 1.2.0");
        assert_eq!(fill_motd("{name} for {minecraft}\\nv{version}", "Ravens", "1.2.0", "1.21.1"), "Ravens for 1.21.1\\nv1.2.0");
    }

    #[test]
    fn escapes_non_ascii_properties() {
        assert_eq!(escape_property("\u{a7}aRavens \u{2694}\\n1.0"), "\\u00A7aRavens \\u2694\\n1.0");
    }

    #[test]
    fn quotes_script_args_that_need_it() {
        let args: Vec<String> = ["--world", "My World", "", "100%"].map(String::from).to_vec();
        assert_eq!(script_args(&args, true), " --world \"My World\" \"100%%\"");
        assert_eq!(script_args(&args, false), " --world 'My World' '100%'");
        assert_eq!(script_args(&["it's".into()], false), " 'it'\\''s'");
    }

    fn loader_pack(minecraft: &str, loader: &str) -> Pack {
        Pack { dir: PathBuf::new(), config: Default::default(), lock: crate::pack::Lockfile { minecraft: Some(minecraft.into()), loader: crate::pack::Loader::from_curseforge(loader), files: Vec::new() } }
    }

    #[test]
    fn neoforge_scripts_install_on_first_start() {
        let pack = loader_pack("1.21.1", "neoforge-21.1.259");
        let bat = install_step(&pack, true).unwrap();
        assert!(bat.contains(r#"if exist "libraries\net\neoforged\neoforge\21.1.259\win_args.txt" goto run"#), "{bat}");
        assert!(bat.contains(r#""%JAVA%" -jar "neoforge-21.1.259-installer.jar" --installServer .install"#), "{bat}");
        let sh = install_step(&pack, false).unwrap();
        assert!(sh.contains(r#"if [ ! -f "libraries/net/neoforged/neoforge/21.1.259/unix_args.txt" ]"#), "{sh}");
        let script = start_script(RUN_SH, "", &sh, &loader_launch(&pack, false).unwrap(), None, "");
        assert!(!script.contains("{{"), "{script}");
    }

    #[test]
    fn slim_server_packs_download_mods_before_installing_the_loader() {
        let pack = loader_pack("1.21.1", "neoforge-21.1.259");
        for (template, download, windows) in [(RUN_BAT, DOWNLOAD_BAT, true), (RUN_SH, DOWNLOAD_SH, false)] {
            let script = start_script(template, download, &install_step(&pack, windows).unwrap(), &loader_launch(&pack, windows).unwrap(), None, "");
            let downloads = script.find(DOWNLOADER).expect(&script);
            assert!(downloads < script.find("--installServer").unwrap(), "{script}");
        }
    }

    #[test]
    fn server_downloads_skip_the_key_only_curseforge_host() {
        let curseforge = |url: &str| LockEntry::curseforge("mods/a.jar".into(), "abc".into(), 1, crate::pack::CurseforgeFile { project_id: 1, file_id: 2, url: url.into() });
        let edge = curseforge("https://edge.forgecdn.net/files/7053/458/DnT-desert-temple-overhaul-v2.1%20%5bFabric%5d.jar");
        assert_eq!(server_download_url(&edge).as_deref(), Some("https://mediafilez.forgecdn.net/files/7053/458/DnT-desert-temple-overhaul-v2.1%20%5bFabric%5d.jar"));
        assert_eq!(server_download_url(&curseforge("https://edge.forgecdn.net/files/1/2/a b.jar")).as_deref(), Some("https://mediafilez.forgecdn.net/files/1/2/a%20b.jar"));
        assert_eq!(server_download_url(&curseforge("")), None);
        let modrinth = LockEntry { path: "mods/sodium.jar".into(), sha1: "sodium-sha1".into(), size: 1, curseforge: None, modrinth: Some(modrinth_file("sodium")) };
        assert_eq!(server_download_url(&modrinth).as_deref(), modrinth.download_url());
    }

    #[test]
    fn downloader_jar_is_built_from_the_current_source() {
        let mut jar = zip::ZipArchive::new(std::io::Cursor::new(DOWNLOADER_JAR)).unwrap();
        let mut shipped = String::new();
        std::io::Read::read_to_string(&mut jar.by_name("PackravenDownloader.java").unwrap(), &mut shipped).unwrap();
        let source = include_str!("scripts/downloader/PackravenDownloader.java");
        assert!(shipped.replace("\r\n", "\n") == source.replace("\r\n", "\n"), "the downloader jar is stale; run npm run build:downloader");
    }

    #[test]
    fn fabric_scripts_have_no_install_step() {
        assert_eq!(install_step(&loader_pack("1.21.1", "fabric-0.16.14-1.21.1"), true).unwrap(), "");
    }

    #[test]
    fn finds_two_enabled_versions_of_one_mod() {
        let entry = |path: &str, project_id| LockEntry::curseforge(path.into(), String::new(), 0, crate::pack::CurseforgeFile { project_id, file_id: 0, url: String::new() });
        let files = vec![
            entry("mods/sodium-0.6.14.jar", 1),
            entry("mods/sodium-0.6.15.jar", 1),
            entry("mods/iris-1.8.jar", 2),
            entry("mods/iris-1.7.jar.disabled", 2),
            entry("resourcepacks/x-dark.zip", 3),
            entry("resourcepacks/x-light.zip", 3),
        ];
        let pack = Pack { dir: PathBuf::new(), config: Default::default(), lock: crate::pack::Lockfile { minecraft: None, loader: None, files } };
        assert_eq!(duplicate_mods(&pack), [["sodium-0.6.14.jar", "sodium-0.6.15.jar"]]);
    }
}
