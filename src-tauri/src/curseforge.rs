use std::collections::HashMap;
use std::path::Path;
use std::sync::LazyLock;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::credentials::{self, Credential};
use crate::http::CLIENT;
use crate::pack::ModLoader;

const CORE_API: &str = "https://api.curseforge.com/v1";
const UPLOAD_API: &str = "https://minecraft.curseforge.com/api";
const MINECRAFT_GAME_ID: u64 = 432;

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CfMod {
    pub id: u64,
    pub name: String,
    pub slug: String,
    #[serde(default)]
    pub links: CfLinks,
    #[serde(default)]
    pub logo: Option<CfLogo>,
    #[serde(default)]
    pub authors: Vec<CfAuthor>,
    #[serde(default)]
    pub class_id: Option<u64>,
    #[serde(default)]
    pub latest_files_indexes: Vec<CfFileIndex>,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub download_count: u64,
    #[serde(default)]
    pub categories: Vec<CfCategory>,
    #[serde(default)]
    pub date_modified: String,
    #[serde(default)]
    pub license_type: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CfCategory {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub parent_category_id: Option<u64>,
}

pub struct SearchQuery<'a> {
    pub text: &'a str,
    pub class_id: u64,
    pub category_id: Option<u64>,
    pub minecraft: &'a str,
    pub loader_type: Option<u32>,
    pub sort_field: u32,
    pub index: u32,
    pub page_size: u32,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CfFileIndex {
    pub game_version: String,
    pub file_id: u64,
    pub filename: String,
    pub release_type: u8,
    pub mod_loader: Option<u32>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CfAuthor {
    pub name: String,
    pub url: String,
    pub avatar_url: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CfLogo {
    pub thumbnail_url: String,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct CfLinks {
    pub website_url: Option<String>,
    #[serde(default)]
    pub source_url: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CfFile {
    pub id: u64,
    pub mod_id: u64,
    pub display_name: String,
    pub file_name: String,
    #[serde(default)]
    pub game_versions: Vec<String>,
    #[serde(default)]
    pub file_status: u32,
    #[serde(default)]
    pub is_server_pack: Option<bool>,
    #[serde(default)]
    pub server_pack_file_id: Option<u64>,
    #[serde(default)]
    pub release_type: u8,
    #[serde(default)]
    pub file_date: String,
    #[serde(default)]
    pub file_length: u64,
    #[serde(default)]
    pub download_url: Option<String>,
    #[serde(default)]
    pub hashes: Vec<CfHash>,
    #[serde(default)]
    pub dependencies: Vec<CfDependency>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct CfHash {
    pub value: String,
    pub algo: u32,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CfDependency {
    pub mod_id: u64,
    pub relation_type: u32,
}

const SHA1_ALGO: u32 = 1;
const REQUIRED_DEPENDENCY: u32 = 3;

impl CfFile {
    pub fn sha1(&self) -> Option<&str> {
        self.hashes.iter().find(|h| h.algo == SHA1_ALGO).map(|h| h.value.as_str())
    }

    pub fn required_projects(&self) -> impl Iterator<Item = u64> + '_ {
        self.dependencies.iter().filter(|d| d.relation_type == REQUIRED_DEPENDENCY).map(|d| d.mod_id)
    }
}

// CurseForge's ModLoaderType enum.
pub fn loader_type(loader: &ModLoader) -> u32 {
    match loader {
        ModLoader::Forge(_) => 1,
        ModLoader::Fabric(_) => 4,
        ModLoader::NeoForge(_) => 6,
    }
}

pub const NO_CORE_KEY: &str = "this packraven build has no CurseForge API key; build it with PACKRAVEN_CF_API_KEY set";
const NO_UPLOAD_TOKEN: &str = "no CurseForge upload token is set; add one in App settings";

// We mask the key in build.rs (embed_curseforge_key).
static CORE_KEY: LazyLock<Option<String>> = LazyLock::new(|| {
    let masked = hex::decode(option_env!("PACKRAVEN_CF_KEY_MASKED")?).ok()?;
    let mask = hex::decode(option_env!("PACKRAVEN_CF_KEY_MASK")?).ok()?;
    String::from_utf8(masked.iter().zip(&mask).map(|(k, m)| k ^ m).collect()).ok()
});

pub fn core_key() -> Option<&'static str> {
    CORE_KEY.as_deref()
}

pub fn upload_token() -> Option<String> {
    credentials::get(Credential::Upload)
}

async fn core_post(path: &str, body: Value) -> Result<Value> {
    let key = core_key().context(NO_CORE_KEY)?;
    let response = CLIENT.post(format!("{CORE_API}{path}")).header("x-api-key", key).json(&body).send().await?;
    Ok(response.error_for_status()?.json::<Value>().await?["data"].take())
}

async fn core_get(path: &str) -> Result<Value> {
    let key = core_key().context(NO_CORE_KEY)?;
    let response = CLIENT.get(format!("{CORE_API}{path}")).header("x-api-key", key).send().await?;
    Ok(response.error_for_status()?.json::<Value>().await?["data"].take())
}

/// Finds a project by its slug, which is unique only within one class, like mods or resource packs.
pub async fn mods_with_slug(slug: &str) -> Result<Vec<CfMod>> {
    let data = core_get(&format!("/mods/search?gameId={MINECRAFT_GAME_ID}&slug={}", slug_characters(slug))).await?;
    Ok(serde_json::from_value(data)?)
}

// Slugs are lowercase letters, digits and hyphens, so anything else can't match one.
fn slug_characters(slug: &str) -> String {
    slug.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect()
}

pub async fn mods(ids: &[u64]) -> Result<HashMap<u64, CfMod>> {
    let mut result = HashMap::new();
    for chunk in ids.chunks(500) {
        let data = core_post("/mods", json!({ "modIds": chunk, "filterPcOnly": true })).await?;
        for m in serde_json::from_value::<Vec<CfMod>>(data)? {
            result.insert(m.id, m);
        }
    }
    Ok(result)
}

pub async fn files(ids: &[u64]) -> Result<HashMap<u64, CfFile>> {
    let mut result = HashMap::new();
    for chunk in ids.chunks(500) {
        let data = core_post("/mods/files", json!({ "fileIds": chunk })).await?;
        for f in serde_json::from_value::<Vec<CfFile>>(data)? {
            result.insert(f.id, f);
        }
    }
    Ok(result)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FingerprintFile {
    pub id: u64,
    pub file_name: String,
    pub file_fingerprint: u32,
    pub download_url: Option<String>,
}

#[derive(Deserialize)]
pub struct FingerprintMatch {
    pub id: u64,
    pub file: FingerprintFile,
}

pub async fn fingerprint_matches(fingerprints: &[u32]) -> Result<Vec<FingerprintMatch>> {
    let mut data = core_post(&format!("/fingerprints/{MINECRAFT_GAME_ID}"), json!({ "fingerprints": fingerprints })).await?;
    Ok(serde_json::from_value(data["exactMatches"].take())?)
}

pub async fn file(project_id: u64, file_id: u64) -> Result<Option<CfFile>> {
    match core_get(&format!("/mods/{project_id}/files/{file_id}")).await {
        Ok(data) => Ok(Some(serde_json::from_value(data)?)),
        Err(error) if error.downcast_ref::<reqwest::Error>().and_then(|e| e.status()).is_some_and(|s| s.as_u16() == 404) => Ok(None),
        Err(error) => Err(error),
    }
}

pub async fn project_files(project_id: u64, minecraft: &str, loader_type: Option<u32>) -> Result<Vec<CfFile>> {
    let loader = loader_type.map(|l| format!("&modLoaderType={l}")).unwrap_or_default();
    let data = core_get(&format!("/mods/{project_id}/files?gameVersion={minecraft}{loader}&pageSize=50")).await?;
    Ok(serde_json::from_value(data)?)
}

// CurseForge rejects a search whose index plus page size passes 10,000.
pub const MAX_SEARCH_RESULTS: u32 = 10_000;

pub async fn search(query: &SearchQuery<'_>) -> Result<(Vec<CfMod>, u32)> {
    let key = core_key().context(NO_CORE_KEY)?;
    let mut params: Vec<(&str, String)> = vec![
        ("gameId", MINECRAFT_GAME_ID.to_string()),
        ("classId", query.class_id.to_string()),
        ("gameVersion", query.minecraft.to_string()),
        ("sortField", query.sort_field.to_string()),
        ("sortOrder", "desc".into()),
        ("index", query.index.to_string()),
        ("pageSize", query.page_size.to_string()),
    ];
    if !query.text.trim().is_empty() {
        params.push(("searchFilter", query.text.trim().to_string()));
    }
    if let Some(category) = query.category_id {
        params.push(("categoryId", category.to_string()));
    }
    if let Some(loader) = query.loader_type {
        params.push(("modLoaderType", loader.to_string()));
    }
    let response = CLIENT.get(format!("{CORE_API}/mods/search")).query(&params).header("x-api-key", key).send().await?;
    let mut body = response.error_for_status()?.json::<Value>().await?;
    let total = body["pagination"]["totalCount"].as_u64().unwrap_or_default().min(u64::from(MAX_SEARCH_RESULTS)) as u32;
    Ok((serde_json::from_value(body["data"].take())?, total))
}

pub async fn categories(class_id: u64) -> Result<Vec<CfCategory>> {
    let data = core_get(&format!("/categories?gameId={MINECRAFT_GAME_ID}&classId={class_id}")).await?;
    let mut categories: Vec<CfCategory> = serde_json::from_value(data)?;
    categories.retain(|c| c.parent_category_id == Some(class_id));
    categories.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(categories)
}

pub async fn description(project_id: u64) -> Result<String> {
    Ok(core_get(&format!("/mods/{project_id}/description")).await?.as_str().unwrap_or_default().to_string())
}

pub async fn changelog(project_id: u64, file_id: u64) -> Result<String> {
    Ok(core_get(&format!("/mods/{project_id}/files/{file_id}/changelog")).await?.as_str().unwrap_or_default().to_string())
}

pub fn file_status_name(status: u32) -> &'static str {
    match status {
        1 => "Processing",
        2 => "Changes required",
        3 => "Under review",
        4 => "Approved",
        5 => "Rejected",
        6 => "Malware detected",
        7 => "Deleted",
        8 => "Archived",
        9 => "Testing",
        10 => "Released",
        11 => "Ready for review",
        12 => "Deprecated",
        13 => "Baking",
        14 => "Awaiting publishing",
        15 => "Failed publishing",
        _ => "Unknown",
    }
}

#[derive(Deserialize)]
struct GameVersion {
    id: u64,
    #[serde(rename = "gameVersionTypeID")]
    type_id: u64,
    name: String,
}

#[derive(Deserialize)]
struct GameVersionType {
    id: u64,
    slug: String,
}

pub async fn game_version_ids(minecraft: &str, loader: &str) -> Result<Vec<u64>> {
    let token = upload_token().context(NO_UPLOAD_TOKEN)?;
    let types: Vec<GameVersionType> = CLIENT
        .get(format!("{UPLOAD_API}/game/version-types"))
        .header("X-Api-Token", &token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let versions: Vec<GameVersion> = CLIENT
        .get(format!("{UPLOAD_API}/game/versions"))
        .header("X-Api-Token", &token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let minor: String = minecraft.split('.').take(2).collect::<Vec<_>>().join("-");
    let minecraft_type = types.iter().find(|t| t.slug == format!("minecraft-{minor}")).with_context(|| format!("no CurseForge version type for {minecraft}"))?;
    let modloader_type = types.iter().find(|t| t.slug == "modloader").context("no CurseForge modloader version type")?;
    let find = |type_id: u64, name: &str| versions.iter().find(|v| v.type_id == type_id && v.name.eq_ignore_ascii_case(name)).map(|v| v.id);
    let minecraft_id = find(minecraft_type.id, minecraft).with_context(|| format!("CurseForge has no game version {minecraft}"))?;
    let loader_id = find(modloader_type.id, loader).with_context(|| format!("CurseForge has no loader {loader}"))?;
    Ok(vec![minecraft_id, loader_id])
}

pub async fn upload(project_id: u64, path: &Path, file_name: String, metadata: Value) -> Result<u64> {
    let token = upload_token().context(NO_UPLOAD_TOKEN)?;
    let bytes = tokio::fs::read(path).await?;
    let size = bytes.len() as u64;
    let part = reqwest::multipart::Part::bytes(bytes).file_name(file_name).mime_str("application/zip")?;
    let form = reqwest::multipart::Form::new().text("metadata", metadata.to_string()).part("file", part);
    let response = CLIENT
        .post(format!("{UPLOAD_API}/projects/{project_id}/upload-file"))
        .header("X-Api-Token", token)
        .multipart(form)
        .send()
        .await?;
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if status == reqwest::StatusCode::PAYLOAD_TOO_LARGE {
        return Err(UploadTooLarge(size).into());
    }
    if !status.is_success() {
        match &body {
            Value::Null => bail!("CurseForge rejected the upload ({status})"),
            body => bail!("CurseForge rejected the upload ({status}): {}", body["errorMessage"].as_str().map_or_else(|| body.to_string(), String::from)),
        }
    }
    body["id"].as_u64().with_context(|| format!("CurseForge upload returned no file id: {body}"))
}

// CurseForge's support articles still say 2 GB, but the upload API sent back a 413 for a 1.29 GB server pack,
// and a CurseForge moderator says the limit is now 1 GB. We assume the binary gigabyte, so we never refuse a
// file CurseForge would take; one between 1 GB and 1 GiB still gets the 413.
pub const UPLOAD_LIMIT: u64 = 1 << 30;

/// Fails when a file is too large for the upload API, so a publish can stop before any upload makes it public.
pub fn check_upload_size(path: &Path, what: &str) -> Result<()> {
    let size = std::fs::metadata(path)?.len();
    if size > UPLOAD_LIMIT {
        bail!(
            "The {what} is {:.2} GB, and CurseForge's upload API takes files up to about 1 GB, so packraven stopped before uploading anything. Check what fills {}, then publish again.",
            size as f64 / 1e9,
            path.display()
        );
    }
    Ok(())
}

/// A 413 from the upload API, so callers can tell a file that will never go through from a failure worth retrying.
#[derive(Debug)]
pub struct UploadTooLarge(pub u64);

impl std::fmt::Display for UploadTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CurseForge's upload API turned down the {:.2} GB file as too large", self.0 as f64 / 1e9)
    }
}

impl std::error::Error for UploadTooLarge {}

/// Returns the authors console page for uploading an additional file, like a server pack, to an existing file.
pub fn additional_file_page(project_id: u64, parent_file_id: u64) -> String {
    format!("https://authors.curseforge.com/#/projects/{project_id}/files/{parent_file_id}/create-additional-file")
}

#[cfg(test)]
mod tests {
    use super::{check_upload_size, UPLOAD_LIMIT};

    #[test]
    fn stops_only_files_over_the_upload_limit() {
        let path = std::env::temp_dir().join(format!("packraven-upload-size-{}.zip", std::process::id()));
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(UPLOAD_LIMIT).unwrap();
        assert!(check_upload_size(&path, "server pack").is_ok());
        file.set_len(UPLOAD_LIMIT + 1).unwrap();
        let error = check_upload_size(&path, "server pack").unwrap_err().to_string();
        drop(file);
        std::fs::remove_file(&path).unwrap();
        assert!(error.starts_with("The server pack is 1.07 GB"), "{error}");
    }
}
