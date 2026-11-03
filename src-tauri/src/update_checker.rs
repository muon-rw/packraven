use std::path::Path;
use std::sync::LazyLock;

use anyhow::{bail, Context, Result};
use regex::Regex;
use reqwest::{RequestBuilder, Response, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::credentials::{self, Credential};
use crate::http::CLIENT;
use crate::pack::{Pack, CHANGELOG_ARCHIVE};
use crate::publish;

pub const CONFIG_PATH: &str = "config/modpack-update-checker/config.json";
const PROJECT_ID: u64 = 847414;
const JAR_PREFIX: &str = "modpack-update-checker";
// MPUC's checker type 2 reads meta.json and versions/<id>/changelog.txt from under updateCheckerId.
const META_CHECKER: u64 = 2;
const RAW_HOST: &str = "https://raw.githubusercontent.com/";
const API: &str = "https://api.github.com";

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum UpdateType {
    Minor,
    #[default]
    MinorBreaking,
    Major,
    Incompatible,
}

/// A release's place in the update checker repo: the type you picked when you published it, and how adding it went.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub update_type: UpdateType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Entry {
    pub fn new(update_type: UpdateType) -> Self {
        Self { update_type, added_at: None, error: None }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Repo {
    pub owner: String,
    pub name: String,
    pub branch: String,
    /// Empty, or a path ending in a slash.
    pub folder: String,
    /// The folder on github.com.
    pub page: String,
}

impl Repo {
    // GitHub's raw URLs don't mark where a branch name ends, so we take a branch with a slash in its name for
    // the start of the folder. MPUC's own examples only use plain branch names.
    fn parse(url: &str) -> Option<Self> {
        let rest = url.trim().strip_prefix(RAW_HOST)?;
        let mut parts = rest.split('/');
        let owner = parts.next().filter(|p| !p.is_empty())?.to_string();
        let name = parts.next().filter(|p| !p.is_empty())?.to_string();
        let mut branch = parts.next().filter(|p| !p.is_empty())?.to_string();
        if branch == "refs" {
            parts.next().filter(|p| *p == "heads")?;
            branch = parts.next().filter(|p| !p.is_empty())?.to_string();
        }
        let folder: String = parts.filter(|p| !p.is_empty()).map(|p| format!("{p}/")).collect();
        let page = format!("https://github.com/{owner}/{name}/tree/{branch}/{}", folder.trim_end_matches('/'));
        Some(Self { owner, name, branch, folder, page })
    }

    fn api(&self, path: &str) -> String {
        format!("{API}/repos/{}/{}/{path}", self.owner, self.name)
    }
}

/// How the pack has Modpack Update Checker set up, as far as packraven can add releases to it.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Setup {
    Github {
        repo: Repo,
        /// Whether MPUC still opens worlds from before an incompatible update, after asking for a backup.
        #[serde(rename = "forceCompatible")]
        force_compatible: bool,
    },
    /// The reason ends with what to change in the config.
    Unsupported {
        reason: String,
        #[serde(rename = "configPath")]
        config_path: &'static str,
    },
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct MpucConfig {
    update_checker_id: String,
    // MPUC's configs before schema 9 call the address githubRepo, and MPUC still reads them.
    github_repo: String,
    force_modpack_compatible: bool,
    advanced: Advanced,
}

impl MpucConfig {
    fn checker_address(&self) -> &str {
        Some(self.update_checker_id.trim()).filter(|id| !id.is_empty()).unwrap_or(self.github_repo.trim())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Advanced {
    update_checker_type: u64,
    bcc_enabled: bool,
}

impl Default for Advanced {
    fn default() -> Self {
        Self { update_checker_type: META_CHECKER, bcc_enabled: true }
    }
}

fn read_config(dir: &Path) -> Option<Result<MpucConfig>> {
    let text = std::fs::read_to_string(dir.join(CONFIG_PATH)).ok()?;
    Some(serde_json::from_str(&text).with_context(|| format!("{CONFIG_PATH} isn't valid JSON")))
}

fn has_mod(pack: &Pack) -> bool {
    pack.lock.files.iter().filter(|e| !e.path.ends_with(".disabled")).any(|e| {
        e.curseforge_project() == Some(PROJECT_ID) || e.path.strip_prefix("mods/").is_some_and(|name| name.to_ascii_lowercase().starts_with(JAR_PREFIX))
    })
}

/// Returns None when the pack doesn't use Modpack Update Checker.
pub fn setup(pack: &Pack) -> Option<Setup> {
    if !has_mod(pack) {
        return None;
    }
    let unsupported = |reason: String| Some(Setup::Unsupported { reason, config_path: CONFIG_PATH });
    let config = match read_config(&pack.dir) {
        None => return unsupported("Modpack Update Checker hasn't written its config yet. Start the game once, then set updateCheckerId in its config.".into()),
        Some(Err(error)) => return unsupported(format!("{error:#}. Fix the file so packraven can read it.")),
        Some(Ok(config)) => config,
    };
    if config.advanced.update_checker_type != META_CHECKER {
        return unsupported(format!(
            "Modpack Update Checker is set to update checker type {}, and packraven can only add releases for type {META_CHECKER}. Set updateCheckerType to {META_CHECKER} in its config to use a GitHub repository.",
            config.advanced.update_checker_type
        ));
    }
    match Repo::parse(config.checker_address()) {
        Some(repo) => Some(Setup::Github { repo, force_compatible: config.force_modpack_compatible }),
        None if config.checker_address().is_empty() => unsupported(format!("Modpack Update Checker has no repository to read releases from. Set updateCheckerId in its config to a {RAW_HOST} address.")),
        None => unsupported(format!("packraven can only add releases to a GitHub repository. Set updateCheckerId in Modpack Update Checker's config to a {RAW_HOST} address to use one.")),
    }
}

pub fn repo(pack: &Pack) -> Option<Repo> {
    match setup(pack)? {
        Setup::Github { repo, .. } => Some(repo),
        Setup::Unsupported { .. } => None,
    }
}

static CURRENT_VERSION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"("currentVersion"\s*:\s*)"(?:[^"\\]|\\.)*""#).expect("valid regex"));

/// MPUC reads the version from BCC unless you turned that off, in which case we keep its own copy in step.
pub fn sync_current_version(dir: &Path, version: &str) -> Result<()> {
    let Some(Ok(config)) = read_config(dir) else { return Ok(()) };
    if config.advanced.bcc_enabled {
        return Ok(());
    }
    let path = dir.join(CONFIG_PATH);
    let text = std::fs::read_to_string(&path)?;
    let encoded = serde_json::to_string(version)?;
    let updated = CURRENT_VERSION.replace(&text, |caps: &regex::Captures| format!("{}{encoded}", &caps[1]));
    if updated != text {
        std::fs::write(&path, updated.as_ref()).with_context(|| format!("writing {CONFIG_PATH}"))?;
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Meta {
    #[serde(default = "schema_version")]
    schema_version: Value,
    #[serde(default)]
    hotfixes_format: Value,
    #[serde(default)]
    versions: Vec<MetaVersion>,
    #[serde(flatten)]
    rest: Map<String, Value>,
}

fn schema_version() -> Value {
    json!(1)
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
// We only ever write our own versions' fields, so we keep the ones you wrote by hand as they are.
struct MetaVersion {
    id: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    released_at: Value,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    update_type: Value,
    #[serde(flatten)]
    rest: Map<String, Value>,
}

impl Meta {
    fn empty() -> Self {
        Self { schema_version: schema_version(), hotfixes_format: Value::Null, versions: Vec::new(), rest: Map::new() }
    }

    // A version you added by hand keeps its date and anything else you gave it, and takes the update type you published with.
    fn add(&mut self, id: &str, released_at: i64, update_type: UpdateType) {
        match self.versions.iter_mut().find(|v| v.id == id) {
            Some(existing) => existing.update_type = json!(update_type),
            None => self.versions.push(MetaVersion { id: id.to_string(), released_at: json!(released_at), update_type: json!(update_type), rest: Map::new() }),
        }
        if let Some(latest) = self.rest.get_mut("latestVersion") {
            *latest = json!(id);
        }
    }
}

fn github(request: RequestBuilder, token: &str) -> RequestBuilder {
    request.bearer_auth(token).header("Accept", "application/vnd.github+json").header("X-GitHub-Api-Version", "2022-11-28")
}

const TOKEN_REJECTED: &str = "GitHub turned down your GitHub token. Check that it hasn't expired, then replace it in App settings.";

fn cannot_write(repo: &Repo) -> String {
    format!("Your GitHub token can't write to {}/{}. Give it Contents: Read and write access to that repository.", repo.owner, repo.name)
}

/// Returns what keeps your token from adding releases to the repo, as far as GitHub will tell without a write.
/// GitHub reports your own access to the repo, which a fine-grained token can still lack.
pub async fn access_problem(repo: &Repo, token: &str) -> Result<Option<String>> {
    let full_name = format!("{}/{}", repo.owner, repo.name);
    let response = github(CLIENT.get(repo.api("").trim_end_matches('/').to_string()), token).send().await?;
    match response.status() {
        StatusCode::UNAUTHORIZED => return Ok(Some(TOKEN_REJECTED.into())),
        StatusCode::NOT_FOUND => return Ok(Some(format!("Your GitHub token can't see {full_name}. Give it access to that repository."))),
        _ => {}
    }
    let details: Value = response.error_for_status()?.json().await?;
    if details["permissions"]["push"] == json!(false) {
        return Ok(Some(cannot_write(repo)));
    }
    let branch = github(CLIENT.get(repo.api(&format!("branches/{}", repo.branch))), token).send().await?;
    if branch.status() == StatusCode::NOT_FOUND {
        return Ok(Some(format!(
            "{full_name} has no {} branch. Fix the updateCheckerId address in {CONFIG_PATH}, or create the branch on GitHub.",
            repo.branch
        )));
    }
    branch.error_for_status()?;
    Ok(None)
}

async fn check(response: Response, repo: &Repo, token: &str) -> Result<Value> {
    let status = response.status();
    if status.is_success() {
        return Ok(response.json().await?);
    }
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND) {
        let problem = access_problem(repo, token).await.ok().flatten().unwrap_or_else(|| cannot_write(repo));
        bail!("{problem}");
    }
    let message = response.json::<Value>().await.ok().and_then(|body| body.get("message")?.as_str().map(String::from)).unwrap_or_default();
    bail!("GitHub couldn't take the change to {}/{} ({status}). Try again in a few minutes. {message}", repo.owner, repo.name)
}

/// Returns why packraven couldn't add a release to the pack's update checker repo right now, if anything stops it.
pub async fn problem(pack: &Pack) -> Option<String> {
    let repo = repo(pack)?;
    let Some(token) = credentials::get(Credential::Github) else {
        return Some(format!("Add a GitHub token in App settings so packraven can add this release to {}/{}.", repo.owner, repo.name));
    };
    match access_problem(&repo, &token).await {
        Ok(problem) => problem,
        Err(error) => Some(format!("Couldn't check your GitHub token's access to {}/{}: {error:#}", repo.owner, repo.name)),
    }
}

async fn read_meta(repo: &Repo, commit: &str, token: &str) -> Result<Meta> {
    let url = repo.api(&format!("contents/{}meta.json", repo.folder));
    let response = github(CLIENT.get(url).query(&[("ref", commit)]), token).header("Accept", "application/vnd.github.raw+json").send().await?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(Meta::empty());
    }
    let response = response.error_for_status().with_context(|| format!("reading {}meta.json from {}/{}", repo.folder, repo.owner, repo.name))?;
    serde_json::from_str(&response.text().await?).with_context(|| format!("{}meta.json in {}/{} isn't valid update checker JSON", repo.folder, repo.owner, repo.name))
}

enum Committed {
    Done,
    // Someone pushed to the branch between our read and our write.
    BranchMoved,
}

async fn commit_release(repo: &Repo, token: &str, version: &str, update_type: UpdateType, changelog: &str, message: &str) -> Result<Committed> {
    let branch = github(CLIENT.get(repo.api(&format!("git/ref/heads/{}", repo.branch))), token).send().await?;
    let parent = check(branch, repo, token).await?["object"]["sha"].as_str().context("GitHub returned a branch without a commit")?.to_string();
    let parent_commit = check(github(CLIENT.get(repo.api(&format!("git/commits/{parent}"))), token).send().await?, repo, token).await?;
    let base_tree = parent_commit["tree"]["sha"].as_str().context("GitHub returned a commit without a tree")?.to_string();

    let mut meta = read_meta(repo, &parent, token).await?;
    meta.add(version, chrono::Utc::now().timestamp_millis(), update_type);
    let meta_text = format!("{}\n", serde_json::to_string_pretty(&meta)?);
    let file = |path: String, content: &str| json!({ "path": path, "mode": "100644", "type": "blob", "content": content });
    let tree_body = json!({
        "base_tree": base_tree,
        "tree": [file(format!("{}meta.json", repo.folder), &meta_text), file(format!("{}versions/{version}/changelog.txt", repo.folder), changelog)],
    });
    let tree = check(github(CLIENT.post(repo.api("git/trees")), token).json(&tree_body).send().await?, repo, token).await?;
    let tree_sha = tree["sha"].as_str().context("GitHub didn't return the new tree")?;
    let commit_body = json!({ "message": message, "tree": tree_sha, "parents": [parent] });
    let commit = check(github(CLIENT.post(repo.api("git/commits")), token).json(&commit_body).send().await?, repo, token).await?;
    let commit_sha = commit["sha"].as_str().context("GitHub didn't return the new commit")?;

    let update = github(CLIENT.patch(repo.api(&format!("git/refs/heads/{}", repo.branch))), token).json(&json!({ "sha": commit_sha, "force": false })).send().await?;
    if update.status() == StatusCode::UNPROCESSABLE_ENTITY {
        return Ok(Committed::BranchMoved);
    }
    check(update, repo, token).await?;
    Ok(Committed::Done)
}

async fn add(pack: &Pack, version: &str, update_type: UpdateType) -> Result<Repo> {
    let repo = match setup(pack) {
        Some(Setup::Github { repo, .. }) => repo,
        Some(Setup::Unsupported { reason, .. }) => bail!("{reason}"),
        None => bail!("Modpack Update Checker isn't in the pack anymore."),
    };
    let token = credentials::get(Credential::Github).context("Add a GitHub token in App settings so packraven can add releases to the update checker.")?;
    // The archive is the changelog CurseForge got, which we wrote when you published.
    let changelog = std::fs::read_to_string(pack.dir.join(CHANGELOG_ARCHIVE).join(format!("{version}.md"))).unwrap_or_default();
    let message = format!("Add {} {version}", pack.name());
    for _ in 0..3 {
        if let Committed::Done = commit_release(&repo, &token, version, update_type, &changelog, &message).await? {
            return Ok(repo);
        }
    }
    bail!("The {} branch of {}/{} kept changing while packraven added {version}. Try again in a minute.", repo.branch, repo.owner, repo.name)
}

/// Adds a published release to the pack's update checker repo, and records how that went on the release.
pub async fn add_release(pack: &Pack, releases_path: &Path, version: &str) -> Result<String> {
    let release = publish::load_releases(releases_path).into_iter().find(|r| r.version == version).with_context(|| format!("you haven't published {version} from this machine"))?;
    let update_type = release.update_checker.as_ref().map_or_else(|| pack.config.update_checker.update_type, |e| e.update_type);
    let outcome = add(pack, version, update_type).await;
    // We reload because the announcer may have saved the release while we talked to GitHub.
    let mut releases = publish::load_releases(releases_path);
    if let Some(stored) = releases.iter_mut().find(|r| r.version == version) {
        let entry = stored.update_checker.get_or_insert_with(|| Entry::new(update_type));
        match &outcome {
            Ok(_) => {
                entry.added_at = Some(chrono::Utc::now().to_rfc3339());
                entry.error = None;
            }
            Err(error) => entry.error = Some(format!("{error:#}")),
        }
        publish::save_releases(releases_path, &releases)?;
    }
    let repo = outcome?;
    Ok(format!("Added {version} to {}/{}", repo.owner, repo.name))
}

/// Returns whether a release still waits to go into the update checker repo, including after a failed attempt.
pub fn awaits(release: &publish::Release) -> bool {
    release.update_checker.as_ref().is_some_and(|e| e.added_at.is_none())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_repo_from_a_raw_github_address() {
        let repo = Repo::parse("https://raw.githubusercontent.com/muon-rw/MPUC/main/").unwrap();
        assert_eq!((repo.owner.as_str(), repo.name.as_str(), repo.branch.as_str(), repo.folder.as_str()), ("muon-rw", "MPUC", "main", ""));
        let nested = Repo::parse("https://raw.githubusercontent.com/LunaPixelStudios/Modpack-Update-Checker/new/mmc5forge/").unwrap();
        assert_eq!((nested.branch.as_str(), nested.folder.as_str()), ("new", "mmc5forge/"));
        assert_eq!(nested.page, "https://github.com/LunaPixelStudios/Modpack-Update-Checker/tree/new/mmc5forge");
        let refs = Repo::parse("https://raw.githubusercontent.com/a/b/refs/heads/dev/packs/one").unwrap();
        assert_eq!((refs.branch.as_str(), refs.folder.as_str()), ("dev", "packs/one/"));
        assert!(Repo::parse("https://example.com/mpuc/").is_none());
        assert!(Repo::parse("https://raw.githubusercontent.com/a/b").is_none());
    }

    #[test]
    fn reads_the_address_from_configs_before_schema_9() {
        let old = r#"{"schemaVersion": 6, "githubRepo": "https://raw.githubusercontent.com/nvb-uy/ProminenceII-HasturianEra/main/", "advanced": {"updateCheckerType": 2}}"#;
        let config: MpucConfig = serde_json::from_str(old).unwrap();
        assert_eq!(Repo::parse(config.checker_address()).unwrap().name, "ProminenceII-HasturianEra");
        let both = r#"{"updateCheckerId": "https://raw.githubusercontent.com/a/new/main/", "githubRepo": "https://raw.githubusercontent.com/a/old/main/"}"#;
        assert_eq!(serde_json::from_str::<MpucConfig>(both).unwrap().checker_address(), "https://raw.githubusercontent.com/a/new/main/");
    }

    #[test]
    fn adding_a_version_keeps_the_rest_of_meta_json() {
        let text = r#"{"schemaVersion": 1, "hotfixesFormat": null, "requiresMpuc": 3, "versions": [{"id": "0.1.0", "releasedAt": 5, "updateType": "minor_breaking", "promotions": {"curseforge": "x"}}]}"#;
        let mut meta: Meta = serde_json::from_str(text).unwrap();
        meta.add("0.1.1", 9, UpdateType::Major);
        meta.add("0.1.0", 7, UpdateType::Minor);
        let written: Value = serde_json::to_value(&meta).unwrap();
        assert_eq!(written["requiresMpuc"], 3);
        assert_eq!(written["versions"][0], json!({"id": "0.1.0", "releasedAt": 5, "updateType": "minor", "promotions": {"curseforge": "x"}}));
        assert_eq!(written["versions"][1], json!({"id": "0.1.1", "releasedAt": 9, "updateType": "major"}));
        let pretty = serde_json::to_string_pretty(&meta).unwrap();
        assert!(pretty.find("schemaVersion").unwrap() < pretty.find("versions").unwrap(), "MPUC's own field order comes first");
    }

    #[test]
    fn keeps_mpucs_own_version_in_step_only_when_it_ignores_bcc() {
        let dir = std::env::temp_dir().join(format!("packraven-mpuc-{}", std::process::id()));
        let path = dir.join(CONFIG_PATH);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{\n  \"currentVersion\": \"0.1.0\",\n  \"advanced\": {\n    \"bccEnabled\": true\n  }\n}").unwrap();
        sync_current_version(&dir, "0.2.0").unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("\"0.1.0\""));
        std::fs::write(&path, "{\n  \"currentVersion\": \"0.1.0\",\n  \"advanced\": {\n    \"bccEnabled\": false\n  }\n}").unwrap();
        sync_current_version(&dir, "0.2.0").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(text, "{\n  \"currentVersion\": \"0.2.0\",\n  \"advanced\": {\n    \"bccEnabled\": false\n  }\n}");
    }
}
