use std::collections::{HashMap, HashSet};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::curseforge::{self, CfFile, CfFileIndex, CfMod};
use crate::modrinth::{self, Version};
use crate::modsync::{self, Placement};
use crate::pack::{CurseforgeFile, LockEntry, ModKey, ModLoader, Pack, Platform, ReleaseType};
use crate::sides::MetadataCache;

const DISABLED_SUFFIX: &str = ".disabled";
const MAX_DEPENDENCY_DEPTH: usize = 5;
const MODRINTH_VERSION_TYPES: [&str; 3] = ["release", "beta", "alpha"];

/// A file on one platform, written cf:<file ID> or mr:<version ID>.
#[derive(Clone, Debug, PartialEq)]
pub enum FileRef {
    Curseforge(u64),
    Modrinth(String),
}

impl std::fmt::Display for FileRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileRef::Curseforge(id) => write!(f, "cf:{id}"),
            FileRef::Modrinth(id) => write!(f, "mr:{id}"),
        }
    }
}

impl std::str::FromStr for FileRef {
    type Err = String;

    // Files use the same platform prefixes as projects.
    fn from_str(text: &str) -> std::result::Result<Self, String> {
        match text.parse::<ModKey>().map_err(|_| format!("{text} isn't a file like cf:5000000 or mr:uMOpc5uV"))? {
            ModKey::Curseforge(id) => Ok(FileRef::Curseforge(id)),
            ModKey::Modrinth(id) => Ok(FileRef::Modrinth(id)),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledMod {
    path: String,
    key: Option<String>,
    /// The project on every platform we know it from, so a search on either can say it's installed.
    keys: Vec<String>,
    file: Option<String>,
    name: String,
    url: Option<String>,
    authors: Vec<String>,
    logo: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledMods {
    mods: Vec<InstalledMod>,
    locked: Vec<String>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ModFile {
    file: String,
    file_name: String,
    release_type: ReleaseType,
    file_date: Option<String>,
    /// In a pack on Modrinth, whether Modrinth has this CurseForge file too. None when we can't tell.
    on_modrinth: Option<bool>,
}

impl ModFile {
    fn curseforge(id: u64, file_name: &str, release: u8, date: Option<String>) -> Self {
        Self { file: FileRef::Curseforge(id).to_string(), file_name: file_name.to_string(), release_type: release_type(release), file_date: date, on_modrinth: None }
    }

    fn curseforge_id(&self) -> u64 {
        match self.file.parse::<FileRef>() {
            Ok(FileRef::Curseforge(id)) => id,
            _ => 0,
        }
    }

    fn modrinth(version: &Version) -> Option<Self> {
        let file = version.primary_file()?;
        Some(Self {
            file: FileRef::Modrinth(version.id.clone()).to_string(),
            file_name: file.filename.clone(),
            release_type: modrinth_release_type(&version.version_type),
            file_date: Some(version.date_published.clone()),
            on_modrinth: Some(true),
        })
    }
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ModCheck {
    path: String,
    key: String,
    logo: Option<String>,
    release_type: Option<ReleaseType>,
    requires: Vec<String>,
    newer: Vec<ModFile>,
}

/// Names a file to place, or a project whose newest fitting file we place.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModRequest {
    pub file: Option<String>,
    pub project: Option<String>,
    /// For a project, the kind of addon you browsed for, since one Modrinth project can be a mod and a data pack.
    #[serde(default)]
    pub kind: Option<AddonKind>,
    pub replaces: Option<String>,
}

pub struct ModPlan {
    pub placements: Vec<Placement>,
    pub installs: Vec<String>,
    pub dependencies: Vec<String>,
    pub unavailable: Vec<String>,
}

pub fn release_type(curseforge: u8) -> ReleaseType {
    match curseforge {
        1 => ReleaseType::Release,
        2 => ReleaseType::Beta,
        _ => ReleaseType::Alpha,
    }
}

fn modrinth_release_type(version_type: &str) -> ReleaseType {
    match version_type {
        "release" => ReleaseType::Release,
        "beta" => ReleaseType::Beta,
        _ => ReleaseType::Alpha,
    }
}

struct Target {
    minecraft: String,
    loaders: Vec<u32>,
    modrinth_loaders: Vec<&'static str>,
}

impl Target {
    fn of(pack: &Pack) -> Result<Self> {
        let minecraft = pack.lock.minecraft()?.to_string();
        let loader = pack.lock.mod_loader()?;
        let mut loaders = vec![curseforge::loader_type(&loader)];
        // NeoForge 1.20.1 still loads Forge mods, and many authors only tag those Forge.
        if matches!(loader, ModLoader::NeoForge(_)) && minecraft == "1.20.1" {
            loaders.push(curseforge::loader_type(&ModLoader::Forge("")));
        }
        let modrinth_loaders = crate::twins::Target::of(&pack.lock).map(|t| t.mod_loaders).unwrap_or_default();
        Ok(Self { minecraft, loaders, modrinth_loaders })
    }

    // Resource packs and shaders have no loader, so an index without one fits any pack.
    fn fits(&self, index: &CfFileIndex) -> bool {
        index.game_version == self.minecraft && index.mod_loader.is_none_or(|l| self.loaders.contains(&l))
    }

    fn newest<'a>(&self, project: &'a CfMod, channel: ReleaseType) -> Option<&'a CfFileIndex> {
        project.latest_files_indexes.iter().filter(|i| self.fits(i) && release_type(i.release_type) <= channel).max_by_key(|i| i.file_id)
    }

    // Modrinth tags data packs with a "datapack" loader, and resource packs and shaders with none we filter by.
    fn modrinth_loaders_for(&self, folder: &str) -> Vec<&'static str> {
        match folder {
            "mods" => self.modrinth_loaders.clone(),
            "datapacks" => vec!["datapack"],
            _ => Vec::new(),
        }
    }
}

fn folder_of(path: &str) -> &str {
    path.split('/').next().unwrap_or(path)
}

pub async fn installed(pack: &Pack, cache: &mut MetadataCache) -> InstalledMods {
    let keys: Vec<_> = pack.lock.files.iter().filter_map(LockEntry::key).collect();
    // Offline, we still list every file, under its file name.
    let _ = cache.ensure_names(&keys).await;
    let mut mods: Vec<InstalledMod> = pack
        .lock
        .files
        .iter()
        .map(|entry| {
            let key = entry.key();
            let name = key.as_ref().and_then(|key| cache.known_name(key));
            let file = match (&entry.curseforge, &entry.modrinth) {
                (Some(file), _) => Some(FileRef::Curseforge(file.file_id)),
                (None, Some(file)) => Some(FileRef::Modrinth(file.version_id.clone())),
                (None, None) => None,
            };
            InstalledMod {
                path: entry.path.clone(),
                key: key.as_ref().map(ToString::to_string),
                keys: project_keys(entry).iter().map(ToString::to_string).collect(),
                file: file.map(|f| f.to_string()),
                name: name.map_or_else(|| file_name(&entry.path).to_string(), |n| n.name.clone()),
                url: name.and_then(|n| n.url.clone()),
                authors: name.and_then(|n| n.authors.clone()).unwrap_or_default(),
                logo: name.and_then(|n| n.logo.clone()),
            }
        })
        .collect();
    mods.sort_by_key(|m| m.name.to_lowercase());
    InstalledMods { mods, locked: pack.config.updates.locked.iter().map(ToString::to_string).collect() }
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

pub async fn check(pack: &Pack) -> Result<Vec<ModCheck>> {
    let target = Target::of(pack)?;
    let mut checks = curseforge_checks(pack, &target).await?;
    if pack.config.targets_modrinth() {
        mark_modrinth_copies(pack, &target, &mut checks).await?;
    }
    checks.extend(modrinth_checks(pack, &target).await?);
    Ok(checks)
}

async fn curseforge_checks(pack: &Pack, target: &Target) -> Result<Vec<ModCheck>> {
    let entries: Vec<(&LockEntry, &CurseforgeFile)> = pack.lock.files.iter().filter_map(|e| Some((e, e.curseforge.as_ref()?))).collect();
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let mut per_project: HashMap<u64, usize> = HashMap::new();
    for (_, file) in &entries {
        *per_project.entry(file.project_id).or_default() += 1;
    }
    let project_ids: Vec<u64> = per_project.keys().copied().collect();
    let file_ids: Vec<u64> = entries.iter().map(|(_, f)| f.file_id).collect();
    let projects = curseforge::mods(&project_ids).await?;
    let files = curseforge::files(&file_ids).await?;

    Ok(entries
        .into_iter()
        .map(|(entry, file)| {
            let project = projects.get(&file.project_id);
            let installed = files.get(&file.file_id);
            // A pack can hold several files of one project, like resource pack variants, and we
            // can't tell which of them an update would replace.
            let newer = match project {
                Some(project) if per_project[&file.project_id] == 1 => newer_files(target, project, file.file_id),
                _ => Vec::new(),
            };
            ModCheck {
                path: entry.path.clone(),
                key: ModKey::Curseforge(file.project_id).to_string(),
                logo: project.and_then(|p| p.logo.as_ref()).and_then(curseforge::CfLogo::url),
                release_type: installed.map(|f| release_type(f.release_type)),
                requires: installed.map(|f| f.required_projects().map(|id| ModKey::Curseforge(id).to_string()).collect()).unwrap_or_default(),
                newer,
            }
        })
        .collect())
}

fn newer_files(target: &Target, project: &CfMod, installed_file_id: u64) -> Vec<ModFile> {
    let mut newest: HashMap<u8, &CfFileIndex> = HashMap::new();
    for index in project.latest_files_indexes.iter().filter(|i| target.fits(i) && i.file_id > installed_file_id) {
        let slot = newest.entry(index.release_type).or_insert(index);
        if index.file_id > slot.file_id {
            *slot = index;
        }
    }
    let mut indexes: Vec<&CfFileIndex> = newest.into_values().collect();
    indexes.sort_by_key(|i| std::cmp::Reverse(i.file_id));
    indexes.into_iter().map(|i| ModFile::curseforge(i.file_id, &i.filename, i.release_type, None)).collect()
}

// Hashes grouped by the Modrinth loaders their folder needs, since one request filters by one set.
fn by_loaders<'a>(target: &Target, files: impl Iterator<Item = (&'a str, String)>) -> HashMap<Vec<&'static str>, Vec<String>> {
    let mut groups: HashMap<Vec<&'static str>, Vec<String>> = HashMap::new();
    for (path, sha1) in files {
        groups.entry(target.modrinth_loaders_for(folder_of(path))).or_default().push(sha1);
    }
    groups
}

/// Returns Modrinth's newest version of each type for each file, by the file's sha1.
async fn newest_on_modrinth(target: &Target, files: Vec<(&str, String)>) -> Result<HashMap<String, Vec<Version>>> {
    let mut newest: HashMap<String, Vec<Version>> = HashMap::new();
    for (loaders, hashes) in by_loaders(target, files.into_iter()) {
        for version_type in MODRINTH_VERSION_TYPES {
            for (sha1, version) in modrinth::latest_by_sha1(&hashes, &loaders, &target.minecraft, version_type).await? {
                newest.entry(sha1).or_default().push(version);
            }
        }
    }
    Ok(newest)
}

// An update only counts as on both platforms when Modrinth has a file of the same name. When none of
// CurseForge's newer files are on Modrinth yet, we look for the newest CurseForge file that is.
async fn mark_modrinth_copies(pack: &Pack, target: &Target, checks: &mut [ModCheck]) -> Result<()> {
    let twins: Vec<(&LockEntry, &crate::pack::ModrinthFile)> = pack.lock.files.iter().filter(|e| e.curseforge.is_some()).filter_map(|e| Some((e, e.modrinth.as_ref()?))).collect();
    if twins.is_empty() {
        return Ok(());
    }
    let newest = newest_on_modrinth(target, twins.iter().map(|(e, f)| (e.path.as_str(), f.sha1.to_ascii_lowercase())).collect()).await?;
    let mut wanting_older: Vec<(usize, u64, HashSet<String>)> = Vec::new();
    for (index, check) in checks.iter_mut().enumerate() {
        let Some((entry, twin)) = twins.iter().find(|(e, _)| e.path == check.path) else { continue };
        let versions = newest.get(&twin.sha1.to_ascii_lowercase()).map(Vec::as_slice).unwrap_or_default();
        let names = modrinth_file_names(versions);
        mark_on_modrinth(&mut check.newer, &names);
        let modrinth_moved_on = versions.iter().any(|v| v.id != twin.version_id);
        if !check.newer.is_empty() && modrinth_moved_on && !check.newer.iter().any(|f| f.on_modrinth == Some(true)) {
            let installed = entry.curseforge.as_ref().map_or(0, |f| f.file_id);
            wanting_older.push((index, installed, names));
        }
    }
    for (index, installed, names) in wanting_older {
        let Some(project) = checks[index].key.parse::<ModKey>().ok().and_then(|k| k.curseforge_id()) else { continue };
        let loader = (folder_of(&checks[index].path) == "mods").then_some(target.loaders[0]);
        let files = curseforge::project_files(project, &target.minecraft, loader).await?;
        if let Some(file) = files.iter().filter(|f| f.id > installed && names.contains(&f.file_name.to_lowercase())).max_by_key(|f| f.id) {
            let mut shared = ModFile::curseforge(file.id, &file.file_name, file.release_type, Some(file.file_date.clone()));
            shared.on_modrinth = Some(true);
            checks[index].newer.push(shared);
            checks[index].newer.sort_by_key(|f| std::cmp::Reverse(f.curseforge_id()));
        }
    }
    Ok(())
}

fn modrinth_file_names(versions: &[Version]) -> HashSet<String> {
    versions.iter().flat_map(|v| &v.files).map(|f| f.filename.to_lowercase()).collect()
}

fn mark_on_modrinth(newer: &mut [ModFile], modrinth_names: &HashSet<String>) {
    for file in newer {
        file.on_modrinth = Some(modrinth_names.contains(&file.file_name.to_lowercase()));
    }
}

async fn modrinth_checks(pack: &Pack, target: &Target) -> Result<Vec<ModCheck>> {
    let entries: Vec<(&LockEntry, &crate::pack::ModrinthFile)> = pack.lock.files.iter().filter(|e| e.curseforge.is_none()).filter_map(|e| Some((e, e.modrinth.as_ref()?))).collect();
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let newest = newest_on_modrinth(target, entries.iter().map(|(e, f)| (e.path.as_str(), f.sha1.to_ascii_lowercase())).collect()).await?;
    let current = modrinth::versions(&entries.iter().map(|(_, f)| f.version_id.clone()).collect::<Vec<_>>()).await?;
    let mut project_ids: Vec<String> = entries.iter().map(|(_, f)| f.project_id.clone()).collect();
    project_ids.sort();
    project_ids.dedup();
    let projects = modrinth::projects(&project_ids).await?;
    let mut per_project: HashMap<&str, usize> = HashMap::new();
    for (_, file) in &entries {
        *per_project.entry(file.project_id.as_str()).or_default() += 1;
    }
    Ok(entries
        .iter()
        .map(|(entry, file)| {
            let installed = current.get(&file.version_id);
            let newer: Vec<ModFile> = match (installed, per_project[file.project_id.as_str()]) {
                (Some(installed), 1) => newest
                    .get(&file.sha1.to_ascii_lowercase())
                    .into_iter()
                    .flatten()
                    .filter(|v| v.id != installed.id && v.date_published > installed.date_published)
                    .filter_map(ModFile::modrinth)
                    .collect(),
                _ => Vec::new(),
            };
            ModCheck {
                path: entry.path.clone(),
                key: ModKey::Modrinth(file.project_id.clone()).to_string(),
                logo: projects.get(&file.project_id).and_then(|p| p.icon_url.clone()),
                release_type: installed.map(|v| modrinth_release_type(&v.version_type)),
                requires: installed
                    .map(|v| v.dependencies.iter().filter(|d| d.is_required()).filter_map(|d| d.project_id.clone()).map(|id| ModKey::Modrinth(id).to_string()).collect())
                    .unwrap_or_default(),
                newer,
            }
        })
        .collect())
}

pub async fn versions(pack: &Pack, path: &str, key: &str) -> Result<Vec<ModFile>> {
    let target = Target::of(pack)?;
    match key.parse::<ModKey>().map_err(anyhow::Error::msg)? {
        ModKey::Curseforge(project_id) => {
            let loader = path.starts_with("mods/").then_some(target.loaders[0]);
            let files = curseforge::project_files(project_id, &target.minecraft, loader).await?;
            Ok(files.into_iter().map(|f| ModFile::curseforge(f.id, &f.file_name, f.release_type, Some(f.file_date))).collect())
        }
        ModKey::Modrinth(project_id) => {
            let mut versions = modrinth::project_versions(&project_id, &target.modrinth_loaders_for(folder_of(path)), &target.minecraft).await?;
            versions.sort_by(|a, b| b.date_published.cmp(&a.date_published));
            Ok(versions.iter().filter_map(ModFile::modrinth).collect())
        }
    }
}

#[derive(Deserialize, Serialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum AddonKind {
    Mods,
    ResourcePacks,
    ShaderPacks,
    DataPacks,
}

// CurseForge's class IDs for Minecraft, and Modrinth's project types.
const KINDS: [(AddonKind, u64, &str); 4] =
    [(AddonKind::Mods, 6, "mods"), (AddonKind::ResourcePacks, 12, "resourcepacks"), (AddonKind::ShaderPacks, 6552, "shaderpacks"), (AddonKind::DataPacks, 6945, "datapacks")];

impl AddonKind {
    fn class_id(self) -> u64 {
        KINDS.iter().find(|(kind, ..)| *kind == self).map_or(6, |(_, id, _)| *id)
    }

    fn folder(self) -> &'static str {
        KINDS.iter().find(|(kind, ..)| *kind == self).map_or("mods", |(.., folder)| folder)
    }

    // Modrinth lists data packs as mods with a "datapack" loader.
    fn modrinth_project_type(self) -> &'static str {
        match self {
            AddonKind::Mods | AddonKind::DataPacks => "mod",
            AddonKind::ResourcePacks => "resourcepack",
            AddonKind::ShaderPacks => "shader",
        }
    }
}

fn folder_for(class_id: Option<u64>) -> &'static str {
    KINDS.iter().find(|(_, id, _)| Some(*id) == class_id).map_or("mods", |(.., folder)| folder)
}

fn modrinth_folder(project_type: &str) -> &'static str {
    match project_type {
        "resourcepack" => "resourcepacks",
        "shader" => "shaderpacks",
        _ => "mods",
    }
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum SearchSort {
    Relevance,
    Popularity,
    Downloads,
    Newest,
    Updated,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchRequest {
    platform: Platform,
    text: String,
    kind: AddonKind,
    category: Option<String>,
    sort: SearchSort,
    index: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    key: String,
    name: String,
    summary: String,
    authors: Vec<String>,
    downloads: u64,
    updated: String,
    logo: Option<String>,
    url: Option<String>,
    /// The file we'd install, when the search tells us. For the rest we pick one when you install.
    file: Option<ModFile>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchPage {
    results: Vec<SearchResult>,
    next_index: Option<u32>,
}

#[derive(Serialize)]
pub struct Category {
    id: String,
    name: String,
}

const SEARCH_PAGE_SIZE: u32 = 20;
// CurseForge's largest page.
const RELEVANCE_POOL: u32 = 50;

pub async fn search(pack: &Pack, request: &SearchRequest) -> Result<SearchPage> {
    match request.platform {
        Platform::Curseforge => search_curseforge(pack, request).await,
        Platform::Modrinth => search_modrinth(pack, request).await,
    }
}

async fn search_curseforge(pack: &Pack, request: &SearchRequest) -> Result<SearchPage> {
    let target = Target::of(pack)?;
    // CurseForge's public API has no relevance sort, so we rank its most popular matches by how
    // closely their names match, and page through the rest by popularity.
    let rank = matches!(request.sort, SearchSort::Relevance) && request.index == 0 && !request.text.trim().is_empty();
    let page_size = if rank { RELEVANCE_POOL } else { SEARCH_PAGE_SIZE };
    let query = curseforge::SearchQuery {
        text: &request.text,
        class_id: request.kind.class_id(),
        category_id: request.category.as_deref().and_then(|c| c.parse().ok()),
        minecraft: &target.minecraft,
        // Only mods have a loader. CurseForge returns no resource packs if we ask for one.
        loader_type: matches!(request.kind, AddonKind::Mods).then_some(target.loaders[0]),
        sort_field: match request.sort {
            SearchSort::Relevance | SearchSort::Popularity => 2,
            SearchSort::Updated => 3,
            SearchSort::Downloads => 6,
            SearchSort::Newest => 11,
        },
        index: request.index,
        page_size: page_size.min(curseforge::MAX_SEARCH_RESULTS.saturating_sub(request.index)),
    };
    let (mut projects, total) = curseforge::search(&query).await?;
    let next_index = request.index + projects.len() as u32;
    if rank {
        let text = request.text.trim();
        projects.sort_by_key(|p| name_match(&p.name, text));
    }
    let channel = pack.config.updates.channel;
    // CurseForge's version filter can match a project by a file for another loader, so we drop
    // projects we have no file to install from.
    let results = projects
        .into_iter()
        .filter_map(|project| {
            let index = target.newest(&project, channel).or_else(|| target.newest(&project, ReleaseType::Alpha))?;
            let file = ModFile::curseforge(index.file_id, &index.filename, index.release_type, None);
            Some(SearchResult {
                key: ModKey::Curseforge(project.id).to_string(),
                authors: project.authors.iter().map(|a| a.name.clone()).collect(),
                logo: project.logo.as_ref().and_then(curseforge::CfLogo::url),
                url: project.links.website_url,
                name: project.name,
                summary: project.summary,
                downloads: project.download_count,
                updated: project.date_modified,
                file: Some(file),
            })
        })
        .collect();
    Ok(SearchPage { results, next_index: (next_index < total).then_some(next_index) })
}

async fn search_modrinth(pack: &Pack, request: &SearchRequest) -> Result<SearchPage> {
    let target = Target::of(pack)?;
    let mut facets = vec![vec![format!("project_type:{}", request.kind.modrinth_project_type())], vec![format!("versions:{}", target.minecraft)]];
    let loaders = target.modrinth_loaders_for(request.kind.folder());
    if !loaders.is_empty() {
        facets.push(loaders.iter().map(|l| format!("categories:{l}")).collect());
    }
    if let Some(category) = &request.category {
        facets.push(vec![format!("categories:{category}")]);
    }
    let query = modrinth::SearchQuery {
        text: &request.text,
        facets,
        index: match request.sort {
            SearchSort::Relevance => "relevance",
            SearchSort::Popularity => "follows",
            SearchSort::Downloads => "downloads",
            SearchSort::Newest => "newest",
            SearchSort::Updated => "updated",
        },
        offset: request.index,
        limit: SEARCH_PAGE_SIZE,
    };
    let (hits, total) = modrinth::search_page(&query).await?;
    let next_index = request.index + hits.len() as u32;
    let results = hits
        .into_iter()
        .map(|hit| SearchResult {
            key: ModKey::Modrinth(hit.project_id).to_string(),
            url: Some(format!("https://modrinth.com/project/{}", hit.slug)),
            name: hit.title,
            summary: hit.description,
            authors: vec![hit.author],
            downloads: hit.downloads,
            updated: hit.date_modified,
            logo: hit.icon_url,
            file: None,
        })
        .collect();
    Ok(SearchPage { results, next_index: (next_index < total).then_some(next_index) })
}

fn name_match(name: &str, text: &str) -> u8 {
    let simplify = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect::<String>();
    let (name, query) = (simplify(name), simplify(text));
    if name == query {
        0
    } else if name.starts_with(&query) {
        1
    } else if name.contains(&query) {
        2
    } else {
        3
    }
}

pub async fn categories(platform: Platform, kind: AddonKind) -> Result<Vec<Category>> {
    match platform {
        Platform::Curseforge => Ok(curseforge::categories(kind.class_id()).await?.into_iter().map(|c| Category { id: c.id.to_string(), name: c.name }).collect()),
        Platform::Modrinth => {
            let project_type = kind.modrinth_project_type();
            let mut categories: Vec<Category> = modrinth::categories()
                .await?
                .into_iter()
                .filter(|c| c.project_type == project_type && matches!(c.header.as_str(), "categories" | "features"))
                .map(|c| Category { name: title_case(&c.name), id: c.name })
                .collect();
            categories.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(categories)
        }
    }
}

// Modrinth names categories like "game-mechanics".
fn title_case(name: &str) -> String {
    let words = name.replace('-', " ");
    let mut chars = words.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// A description or changelog. CurseForge writes HTML and Modrinth writes Markdown.
#[derive(Serialize)]
pub struct RemoteText {
    pub body: String,
    pub markdown: bool,
}

pub async fn description(key: &str) -> Result<RemoteText> {
    match key.parse::<ModKey>().map_err(anyhow::Error::msg)? {
        ModKey::Curseforge(id) => Ok(RemoteText { body: curseforge::description(id).await?, markdown: false }),
        ModKey::Modrinth(id) => Ok(RemoteText { body: modrinth::project(&id).await?.context("Modrinth has no such project")?.body, markdown: true }),
    }
}

pub async fn changelog(key: &str, file: &str) -> Result<RemoteText> {
    match (key.parse::<ModKey>().map_err(anyhow::Error::msg)?, file.parse::<FileRef>().map_err(anyhow::Error::msg)?) {
        (ModKey::Curseforge(project), FileRef::Curseforge(file)) => Ok(RemoteText { body: curseforge::changelog(project, file).await?, markdown: false }),
        (_, FileRef::Modrinth(version)) => Ok(RemoteText { body: modrinth::version(&version).await?.and_then(|v| v.changelog).unwrap_or_default(), markdown: true }),
        (ModKey::Modrinth(_), FileRef::Curseforge(_)) => bail!("{file} isn't a file of {key}"),
    }
}

// A project or version we're about to place, with what we need to name it and find what it requires.
enum Chosen {
    Curseforge(CfFile, &'static str),
    Modrinth(Version, &'static str),
}

pub async fn plan(pack: &Pack, requests: &[ModRequest]) -> Result<ModPlan> {
    let target = Target::of(pack)?;
    let channel = pack.config.updates.channel;
    let mut present: HashSet<ModKey> = pack.lock.files.iter().flat_map(project_keys).collect();
    let mut placements = Vec::new();
    let mut installs = Vec::new();
    let mut chosen = Vec::new();

    for request in requests {
        let folder = match &request.replaces {
            Some(path) => path.rsplit_once('/').map(|(folder, _)| folder).with_context(|| format!("{path} isn't in a pack folder"))?,
            None => request.kind.map_or("", AddonKind::folder),
        };
        let (pick, name) = choose(&target, channel, request, folder).await?;
        let key = match &pick {
            Chosen::Curseforge(file, _) => ModKey::Curseforge(file.mod_id),
            Chosen::Modrinth(version, _) => ModKey::Modrinth(version.project_id.clone()),
        };
        let entry = match &request.replaces {
            Some(path) => entry_for(&pick, folder, path.ends_with(DISABLED_SUFFIX))?,
            None => {
                if present.contains(&key) {
                    bail!("{name} is already in this pack");
                }
                installs.push(name);
                entry_for(&pick, "", false)?
            }
        };
        placements.push(Placement { entry, replaces: request.replaces.clone() });
        present.insert(key);
        chosen.push(pick);
    }

    let mut dependencies = Vec::new();
    let mut unavailable = Vec::new();
    for _ in 0..MAX_DEPENDENCY_DEPTH {
        let wanted = missing_dependencies(&chosen, &present);
        if wanted.is_empty() {
            break;
        }
        present.extend(wanted.iter().cloned());
        chosen = Vec::new();
        for key in wanted {
            match newest_of(&target, channel, &key, "mods").await? {
                Some((pick, name)) => {
                    placements.push(Placement { entry: entry_for(&pick, "", false)?, replaces: None });
                    dependencies.push(name);
                    chosen.push(pick);
                }
                None => unavailable.push(project_name(&key).await),
            }
        }
    }
    Ok(ModPlan { placements, installs, dependencies, unavailable })
}

fn project_keys(entry: &LockEntry) -> Vec<ModKey> {
    entry.curseforge.iter().map(|f| ModKey::Curseforge(f.project_id)).chain(entry.modrinth.iter().map(|f| ModKey::Modrinth(f.project_id.clone()))).collect()
}

async fn choose(target: &Target, channel: ReleaseType, request: &ModRequest, folder: &str) -> Result<(Chosen, String)> {
    if let Some(file) = &request.file {
        return match file.parse::<FileRef>().map_err(anyhow::Error::msg)? {
            FileRef::Curseforge(id) => {
                let file = curseforge::files(&[id]).await?.remove(&id).with_context(|| format!("CurseForge has no file {id}"))?;
                let project = curseforge::mods(&[file.mod_id]).await?.remove(&file.mod_id).with_context(|| format!("CurseForge has no project {}", file.mod_id))?;
                let folder = if folder.is_empty() { folder_for(project.class_id) } else { leak_folder(folder) };
                Ok((Chosen::Curseforge(file, folder), project.name))
            }
            FileRef::Modrinth(id) => {
                let version = modrinth::version(&id).await?.with_context(|| format!("Modrinth has no version {id}"))?;
                let project = modrinth::project(&version.project_id).await?.with_context(|| format!("Modrinth has no project {}", version.project_id))?;
                let folder = if folder.is_empty() { modrinth_folder(&project.project_type) } else { leak_folder(folder) };
                Ok((Chosen::Modrinth(version, folder), project.title))
            }
        };
    }
    let key: ModKey = request.project.as_deref().context("a mod request names neither a file nor a project")?.parse().map_err(anyhow::Error::msg)?;
    let folder = if folder.is_empty() { request.kind.unwrap_or(AddonKind::Mods).folder() } else { leak_folder(folder) };
    match newest_of(target, channel, &key, folder).await? {
        Some(found) => Ok(found),
        None => bail!("{} has no file for this pack's Minecraft version and loader", project_name(&key).await),
    }
}

// Placements outlive the request, and pack folders are a small fixed set.
fn leak_folder(folder: &str) -> &'static str {
    ["mods", "resourcepacks", "shaderpacks", "datapacks"].into_iter().find(|f| *f == folder).unwrap_or("mods")
}

/// Finds a project's newest file that fits the pack, preferring the update channel. A missing
/// dependency breaks the pack, so we take a less stable file over none.
async fn newest_of(target: &Target, channel: ReleaseType, key: &ModKey, folder: &'static str) -> Result<Option<(Chosen, String)>> {
    match key {
        ModKey::Curseforge(id) => {
            let Some(project) = curseforge::mods(&[*id]).await?.remove(id) else { return Ok(None) };
            let Some(index) = target.newest(&project, channel).or_else(|| target.newest(&project, ReleaseType::Alpha)) else { return Ok(None) };
            let file = curseforge::files(&[index.file_id]).await?.remove(&index.file_id).with_context(|| format!("CurseForge has no file {}", index.file_id))?;
            let folder = if folder == "mods" { folder_for(project.class_id) } else { folder };
            Ok(Some((Chosen::Curseforge(file, folder), project.name)))
        }
        ModKey::Modrinth(id) => {
            let Some(project) = modrinth::project(id).await? else { return Ok(None) };
            let folder = if folder == "mods" { modrinth_folder(&project.project_type) } else { folder };
            let versions = modrinth::project_versions(&project.id, &target.modrinth_loaders_for(folder), &target.minecraft).await?;
            let newest = |allowed: ReleaseType| versions.iter().filter(|v| modrinth_release_type(&v.version_type) <= allowed).max_by(|a, b| a.date_published.cmp(&b.date_published));
            let Some(version) = newest(channel).or_else(|| newest(ReleaseType::Alpha)) else { return Ok(None) };
            Ok(Some((Chosen::Modrinth(version.clone(), folder), project.title)))
        }
    }
}

async fn project_name(key: &ModKey) -> String {
    let name = match key {
        ModKey::Curseforge(id) => curseforge::mods(&[*id]).await.ok().and_then(|mut m| m.remove(id)).map(|p| p.name),
        ModKey::Modrinth(id) => modrinth::project(id).await.ok().flatten().map(|p| p.title),
    };
    name.unwrap_or_else(|| format!("Project {key}"))
}

fn missing_dependencies(chosen: &[Chosen], present: &HashSet<ModKey>) -> Vec<ModKey> {
    let mut missing: Vec<ModKey> = chosen
        .iter()
        .flat_map(|pick| match pick {
            Chosen::Curseforge(file, _) => file.required_projects().map(ModKey::Curseforge).collect::<Vec<_>>(),
            Chosen::Modrinth(version, _) => version.dependencies.iter().filter(|d| d.is_required()).filter_map(|d| d.project_id.clone()).map(ModKey::Modrinth).collect(),
        })
        .filter(|key| !present.contains(key))
        .collect();
    missing.sort();
    missing.dedup();
    missing
}

fn entry_for(pick: &Chosen, folder: &str, disabled: bool) -> Result<LockEntry> {
    match pick {
        Chosen::Curseforge(file, default) => lock_entry(file, if folder.is_empty() { default } else { folder }, disabled),
        Chosen::Modrinth(version, default) => modrinth_entry(version, if folder.is_empty() { default } else { folder }, disabled),
    }
}

fn safe_file_name<'a>(name: &'a str, platform: &str) -> Result<&'a str> {
    if name.contains(['/', '\\']) || name.starts_with('.') || name.is_empty() {
        bail!("{platform} returned an unsafe file name: {name}");
    }
    Ok(name)
}

pub(crate) fn lock_entry(file: &CfFile, folder: &str, disabled: bool) -> Result<LockEntry> {
    let name = safe_file_name(&file.file_name, "CurseForge")?;
    let sha1 = file.sha1().with_context(|| format!("CurseForge has no SHA-1 for {name}"))?;
    let suffix = if disabled { DISABLED_SUFFIX } else { "" };
    let url = file.download_url.clone().unwrap_or_else(|| modsync::cdn_url(file.id, name));
    Ok(LockEntry::curseforge(format!("{folder}/{name}{suffix}"), sha1.to_string(), file.file_length, CurseforgeFile { project_id: file.mod_id, file_id: file.id, url }))
}

fn modrinth_entry(version: &Version, folder: &str, disabled: bool) -> Result<LockEntry> {
    let file = version.primary_file().with_context(|| format!("Modrinth's version {} has no files", version.id))?;
    let name = safe_file_name(&file.filename, "Modrinth")?;
    let suffix = if disabled { DISABLED_SUFFIX } else { "" };
    Ok(LockEntry { path: format!("{folder}/{name}{suffix}"), sha1: file.hashes.sha1.to_ascii_lowercase(), size: file.size, curseforge: None, modrinth: Some(version.lock_file(file)) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(file_id: u64, release_type: u8, mod_loader: Option<u32>) -> CfFileIndex {
        CfFileIndex { game_version: "1.21.1".into(), file_id, filename: format!("f{file_id}.jar"), release_type, mod_loader }
    }

    fn project(indexes: Vec<CfFileIndex>) -> CfMod {
        serde_json::from_value(serde_json::json!({ "id": 1, "name": "A", "slug": "a" })).map(|m: CfMod| CfMod { latest_files_indexes: indexes, ..m }).unwrap()
    }

    fn target() -> Target {
        Target { minecraft: "1.21.1".into(), loaders: vec![6], modrinth_loaders: vec!["neoforge"] }
    }

    #[test]
    fn offers_the_newest_file_of_each_release_type() {
        let fabric = index(900, 1, Some(4));
        let other_version = CfFileIndex { game_version: "1.20.1".into(), ..index(950, 1, Some(6)) };
        let project = project(vec![index(500, 1, Some(6)), index(400, 2, Some(6)), index(300, 3, Some(6)), fabric, other_version]);
        let newer: Vec<(String, ReleaseType)> = newer_files(&target(), &project, 350).iter().map(|f| (f.file.clone(), f.release_type)).collect();
        assert_eq!(newer, [("cf:500".into(), ReleaseType::Release), ("cf:400".into(), ReleaseType::Beta)]);
    }

    #[test]
    fn newest_respects_the_channel() {
        let project = project(vec![index(500, 1, Some(6)), index(600, 2, Some(6)), index(700, 3, None)]);
        assert_eq!(target().newest(&project, ReleaseType::Release).map(|i| i.file_id), Some(500));
        assert_eq!(target().newest(&project, ReleaseType::Beta).map(|i| i.file_id), Some(600));
        assert_eq!(target().newest(&project, ReleaseType::Alpha).map(|i| i.file_id), Some(700));
    }

    #[test]
    fn marks_the_curseforge_files_modrinth_has_too() {
        let mut newer = vec![ModFile::curseforge(600, "A-2.0.jar", 1, None), ModFile::curseforge(500, "a-1.9.jar", 1, None)];
        mark_on_modrinth(&mut newer, &["a-1.9.jar".to_string()].into_iter().collect());
        assert_eq!(newer.iter().map(|f| f.on_modrinth).collect::<Vec<_>>(), [Some(false), Some(true)]);
    }

    #[test]
    fn reads_files_from_either_platform() {
        assert_eq!("cf:5000000".parse::<FileRef>(), Ok(FileRef::Curseforge(5_000_000)));
        assert_eq!("mr:uMOpc5uV".parse::<FileRef>(), Ok(FileRef::Modrinth("uMOpc5uV".into())));
        assert_eq!(FileRef::Modrinth("uMOpc5uV".into()).to_string(), "mr:uMOpc5uV");
        assert!("xx:1".parse::<FileRef>().is_err());
    }

    #[test]
    fn places_modrinth_files_by_their_primary_file() {
        let version: Version = serde_json::from_value(serde_json::json!({
            "id": "v1", "project_id": "p1", "version_type": "beta",
            "files": [
                { "hashes": { "sha1": "SRC", "sha512": "s" }, "url": "https://cdn.modrinth.com/a-sources.jar", "filename": "a-sources.jar", "size": 1, "primary": false },
                { "hashes": { "sha1": "ABC", "sha512": "x" }, "url": "https://cdn.modrinth.com/a.jar", "filename": "a.jar", "size": 2, "primary": true }
            ]
        }))
        .unwrap();
        let entry = modrinth_entry(&version, "mods", true).unwrap();
        assert_eq!(entry.path, "mods/a.jar.disabled");
        assert_eq!(entry.sha1, "abc");
        assert_eq!(entry.modrinth.as_ref().map(|f| f.version_id.as_str()), Some("v1"));
        assert_eq!(ModFile::modrinth(&version).map(|f| (f.file, f.release_type)), Some(("mr:v1".into(), ReleaseType::Beta)));
    }

    #[test]
    fn names_modrinth_categories_for_people() {
        assert_eq!(title_case("game-mechanics"), "Game mechanics");
    }

    #[test]
    fn ranks_names_by_how_closely_they_match() {
        let mut names = vec!["Sophisticated Backpacks Create Integration", "Backpacked", "Sophisticated Backpacks", "Create: Sophisticated Backpacks Compat"];
        names.sort_by_key(|n| name_match(n, "sophisticated backpacks"));
        assert_eq!(names, ["Sophisticated Backpacks", "Sophisticated Backpacks Create Integration", "Create: Sophisticated Backpacks Compat", "Backpacked"]);
    }

    #[test]
    fn keeps_disabled_files_disabled() {
        let file: CfFile = serde_json::from_value(serde_json::json!({
            "id": 5, "modId": 1, "displayName": "A", "fileName": "a-2.jar",
            "hashes": [{ "value": "md5", "algo": 2 }, { "value": "abc", "algo": 1 }]
        }))
        .unwrap();
        let entry = lock_entry(&file, "mods", true).unwrap();
        assert_eq!(entry.path, "mods/a-2.jar.disabled");
        assert_eq!(entry.sha1, "abc");
        assert_eq!(entry.download_url(), Some("https://edge.forgecdn.net/files/0/5/a-2.jar"));
    }
}
