use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{bail, Context, Result};
use regex::Regex;
use reqwest::multipart::{Form, Part};
use serde::Serialize;
use serde_json::{json, Value};

use crate::curseforge::{self, FileState};
use crate::http::CLIENT;
use crate::modrinth;
use crate::pack::{DiscordConfig, Pack, CHANGELOG_ARCHIVE};
use crate::publish::{self, Release};

const DESCRIPTION_LIMIT: usize = 4000;
const TITLE_LIMIT: usize = 256;
const USERNAME_LIMIT: usize = 80;
const DISCOHOOK_SHARE: &str = "https://discohook.app/api/v1/share";
const MAX_ATTACHMENTS: usize = 4;
// Discord takes 10 MiB per file from a webhook unless the server is boosted, which we can't check.
const MAX_ATTACHMENT_BYTES: u64 = 10 * 1024 * 1024;
const IMAGE_TYPES: [(&str, &str); 5] = [("png", "image/png"), ("jpg", "image/jpeg"), ("jpeg", "image/jpeg"), ("gif", "image/gif"), ("webp", "image/webp")];

pub struct Announcement {
    pub name: String,
    pub version: String,
    pub changelog: String,
    pub file_url: String,
    pub project: Project,
    pub game: String,
    /// When the announcement is posted, as RFC 3339, which Discord shows in the embed footer.
    pub timestamp: String,
}

pub struct Project {
    pub site: &'static str,
    pub name: Option<String>,
    pub url: String,
    pub logo: Option<String>,
    pub author: Option<curseforge::CfAuthor>,
}

impl Announcement {
    fn fill(&self, template: &str) -> String {
        template
            .replace("{project_name}", self.project.name.as_deref().unwrap_or(&self.name))
            .replace("{name}", &self.name)
            .replace("{version}", &self.version)
            .replace("{file_url}", &self.file_url)
            .replace("{project_url}", &self.project.url)
            .trim()
            .to_string()
    }
}

pub fn validate_webhook(url: &str) -> Result<()> {
    let valid = ["https://discord.com/api/webhooks/", "https://discordapp.com/api/webhooks/", "https://ptb.discord.com/api/webhooks/", "https://canary.discord.com/api/webhooks/"]
        .iter()
        .any(|prefix| url.starts_with(prefix));
    if !valid {
        bail!("That isn't a Discord webhook URL. Copy it from Server Settings, Integrations, Webhooks.");
    }
    Ok(())
}

fn condense_mod_updates(changelog: &str) -> String {
    let lines: Vec<&str> = changelog.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut in_mod_changes = false;
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim();
        if trimmed == "## Mod changes" {
            in_mod_changes = true;
            out.push(lines[i].to_string());
            i += 1;
            continue;
        }
        if in_mod_changes && trimmed.starts_with("## ") {
            in_mod_changes = false;
        }
        if in_mod_changes && trimmed == "### Updated" {
            i += 1;
            let mut count = 0usize;
            while i < lines.len() {
                let next = lines[i].trim();
                if next.starts_with("## ") || next.starts_with("### ") {
                    break;
                }
                if next.starts_with("- ") {
                    count += 1;
                }
                i += 1;
            }
            if count > 0 {
                let noun = if count == 1 { "mod" } else { "mods" };
                out.push(format!("Updated {count} {noun}"));
                if i < lines.len() {
                    out.push(String::new());
                }
            }
            continue;
        }
        out.push(lines[i].to_string());
        i += 1;
    }
    out.join("\n").trim_end().to_string()
}

fn discord_markdown(changelog: &str) -> String {
    changelog
        .lines()
        .map(|line| match line.trim_start().split_once(' ') {
            Some((hashes, title)) if !hashes.is_empty() && hashes.chars().all(|c| c == '#') => format!("**{title}**"),
            _ => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// Discord doesn't document how it counts characters. UTF-16 units never undercount code points,
// so measuring in them keeps us under the limit either way.
fn discord_len(text: &str) -> usize {
    text.encode_utf16().count()
}

fn fitting_prefix(text: &str, limit: usize) -> &str {
    let mut used = 0;
    for (index, c) in text.char_indices() {
        used += c.len_utf16();
        if used > limit {
            return &text[..index];
        }
    }
    text
}

fn clamp(text: &str, limit: usize) -> String {
    if discord_len(text) <= limit {
        return text.to_string();
    }
    if limit == 0 {
        return String::new();
    }
    format!("{}…", fitting_prefix(text, limit - 1).trim_end())
}

fn shorten(text: &str, link: &str, limit: usize) -> String {
    if discord_len(text) <= limit {
        return text.to_string();
    }
    if discord_len(link) > limit {
        return String::new();
    }
    let kept = fitting_prefix(text, limit.saturating_sub(discord_len(link) + 2));
    let cut = kept.rfind('\n').unwrap_or(kept.len());
    match text[..cut].trim_end() {
        "" => link.to_string(),
        kept => format!("{kept}\n\n{link}"),
    }
}

static MARKDOWN_LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"\[([^\[\]\n]+)\]\(<?[^()\s]+>?(?:[ \t]+"[^"\n]*")?\)"#).expect("markdown link pattern"));

fn without_links(markdown: &str) -> String {
    MARKDOWN_LINK.replace_all(markdown, "$1").into_owned()
}

fn color(hex: &str) -> u32 {
    u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0x4d6b2f)
}

struct Description {
    text: String,
    links_stripped: bool,
    truncated: bool,
}

fn description(header: &str, changelog: &str, footer: &str, full_changelog_link: &str) -> Description {
    // The footer always goes out whole, so the header and the changelog share what it leaves.
    let footer = clamp(footer, DESCRIPTION_LIMIT);
    let header = clamp(header, DESCRIPTION_LIMIT.saturating_sub(discord_len(&footer) + 2));
    let room = DESCRIPTION_LIMIT.saturating_sub(discord_len(&header) + discord_len(&footer) + 4);
    let mut links_stripped = false;
    let mut truncated = false;
    let body = if changelog.is_empty() {
        clamp("A new version is out.", room)
    } else if discord_len(changelog) <= room {
        changelog.to_string()
    } else {
        let plain = without_links(changelog);
        links_stripped = plain != changelog;
        truncated = discord_len(&plain) > room;
        shorten(&plain, full_changelog_link, room)
    };
    let text = [header, body, footer].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join("\n\n");
    Description { text, links_stripped, truncated }
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Attachment {
    pub path: String,
    pub filename: String,
}

// Discord only resolves attachment:// for names made of letters, digits, dots, dashes and underscores,
// and two files in one message can't share a name.
fn attachments(images: &[String]) -> Vec<Attachment> {
    let mut taken = HashSet::new();
    images
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let name = path.rsplit(['/', '\\']).next().unwrap_or_default();
            let safe: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' }).collect();
            let filename = if taken.contains(&safe.to_ascii_lowercase()) { format!("{}-{safe}", index + 1) } else { safe };
            taken.insert(filename.to_ascii_lowercase());
            Attachment { path: path.clone(), filename }
        })
        .collect()
}

/// Everything one announcement sends: the JSON payload, and the files that go with it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Composed {
    pub payload: Value,
    pub attachments: Vec<Attachment>,
    /// Whether we turned the changelog's links into plain text to fit Discord's limit.
    pub links_stripped: bool,
    /// Whether we cut the changelog short and linked to the full one.
    pub truncated: bool,
}

/// Builds the announcement without reading files or the clock, so a preview shows exactly what we'd send.
pub fn compose(announcement: &Announcement, config: &DiscordConfig, images: &[String]) -> Composed {
    let role = config.role_id.trim();
    let raw_changelog = announcement.changelog.trim();
    let changelog = if config.condense_mod_updates { condense_mod_updates(raw_changelog) } else { raw_changelog.to_string() };
    let changelog = discord_markdown(&changelog);
    let full_changelog_link = format!("[Read the full changelog on {}]({})", announcement.project.site, announcement.file_url);
    let description = description(&announcement.fill(&config.header), &changelog, &announcement.fill(&config.footer), &full_changelog_link);
    let title = match announcement.fill(&config.title) {
        title if title.is_empty() => format!("{} {}", announcement.name, announcement.version),
        title => title,
    };
    let title = clamp(&title, TITLE_LIMIT);
    let mut embed = json!({
        "title": title,
        "url": announcement.file_url,
        "description": description.text,
        "color": color(&config.color),
        "footer": { "text": announcement.game },
        "timestamp": announcement.timestamp,
    });
    if let Some(logo) = announcement.project.logo.as_deref().filter(|u| !u.is_empty()) {
        embed["thumbnail"] = json!({ "url": logo });
    }
    if let Some(author) = announcement.project.author.as_ref().filter(|_| config.show_author) {
        embed["author"] = json!({ "name": author.name, "url": author.url, "icon_url": author.avatar_url });
    }
    let attachments = attachments(images);
    // Discord merges up to 4 embeds with the same URL into one image gallery inside the embed.
    if let Some(first) = attachments.first() {
        embed["image"] = json!({ "url": format!("attachment://{}", first.filename) });
    }
    let mut embeds = vec![embed];
    for extra in attachments.iter().skip(1) {
        embeds.push(json!({
            "url": announcement.file_url,
            "image": { "url": format!("attachment://{}", extra.filename) },
        }));
    }
    let mut payload = json!({
        "username": clamp(&announcement.name, USERNAME_LIMIT),
        "content": if role.is_empty() { String::new() } else { format!("<@&{role}>") },
        "allowed_mentions": { "parse": [], "roles": if role.is_empty() { vec![] } else { vec![role.to_string()] } },
        "embeds": embeds,
    });
    if !attachments.is_empty() {
        payload["attachments"] = attachments.iter().enumerate().map(|(id, attachment)| json!({ "id": id, "filename": attachment.filename })).collect();
    }
    Composed { payload, attachments, links_stripped: description.links_stripped, truncated: description.truncated }
}

fn image_type(path: &str) -> Option<&'static str> {
    let extension = Path::new(path).extension()?.to_str()?.to_ascii_lowercase();
    IMAGE_TYPES.iter().find(|(known, _)| *known == extension).map(|(_, mime)| *mime)
}

pub fn is_image(path: &str) -> bool {
    image_type(path).is_some()
}

pub fn check_images(images: &[String]) -> Result<()> {
    if images.len() > MAX_ATTACHMENTS {
        bail!("Discord takes up to {MAX_ATTACHMENTS} images in one embed, and you chose {}.", images.len());
    }
    for path in images {
        let name = Path::new(path).file_name().map_or_else(|| path.clone(), |n| n.to_string_lossy().into_owned());
        if !is_image(path) {
            bail!("{name} isn't a PNG, JPEG, GIF or WebP image.");
        }
        let size = std::fs::metadata(path).with_context(|| format!("Couldn't read {name}, so remove it or choose it again"))?.len();
        if size > MAX_ATTACHMENT_BYTES {
            bail!("{name} is {:.1} MB, and Discord takes files up to 10 MB. Choose a smaller image.", size as f64 / (1024.0 * 1024.0));
        }
    }
    Ok(())
}

pub async fn send(webhook: &str, payload: &Value, attachments: &[Attachment]) -> Result<String> {
    validate_webhook(webhook)?;
    let request = CLIENT.post(format!("{webhook}?wait=true"));
    let request = if attachments.is_empty() {
        request.json(payload)
    } else {
        check_images(&attachments.iter().map(|a| a.path.clone()).collect::<Vec<_>>())?;
        let mut form = Form::new().text("payload_json", payload.to_string());
        for (index, attachment) in attachments.iter().enumerate() {
            let bytes = tokio::fs::read(&attachment.path).await.with_context(|| format!("Couldn't read {}", attachment.path))?;
            let part = Part::bytes(bytes).file_name(attachment.filename.clone()).mime_str(image_type(&attachment.path).unwrap_or("application/octet-stream"))?;
            form = form.part(format!("files[{index}]"), part);
        }
        request.multipart(form)
    };
    let response = request.send().await?;
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        bail!("Discord rejected the message ({status}): {}", body.get("message").and_then(Value::as_str).unwrap_or("no details"));
    }
    Ok(body.get("id").and_then(Value::as_str).unwrap_or_default().to_string())
}

pub async fn discohook_link(message: &Value) -> Result<String> {
    let mut data = message.clone();
    if let Some(object) = data.as_object_mut() {
        object.remove("allowed_mentions");
    }
    let body = json!({ "data": { "version": "d2", "messages": [{ "data": data }] } });
    let response: Value = CLIENT.post(DISCOHOOK_SHARE).json(&body).send().await?.error_for_status()?.json().await?;
    response.get("url").and_then(Value::as_str).map(String::from).context("Discohook didn't return a link")
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Listing {
    Public,
    /// Still in CurseForge's automated processing or review, which usually takes minutes.
    Pending,
    /// Waiting on a moderator, on you, or on something we couldn't check.
    Held,
}

async fn curseforge_file_listing(project_id: u64, file_id: u64, recent: bool) -> Listing {
    match curseforge::file(project_id, file_id).await {
        Ok(Some(file)) => match curseforge::file_state(file.file_status) {
            FileState::Approved => Listing::Public,
            FileState::Processing | FileState::InReview => Listing::Pending,
            _ => Listing::Held,
        },
        _ if recent => Listing::Pending,
        _ => Listing::Held,
    }
}

/// Returns whether every platform the release went to lists it, taking the slowest platform's listing.
/// Modrinth usually lists a version on an approved project as soon as we upload it, but it can hold one
/// for review too.
pub async fn listing(release: &Release) -> Listing {
    let recent = release.uploaded_to_curseforge_recently();
    let on_curseforge = match &release.curseforge {
        Some(published) => {
            let client = curseforge_file_listing(published.project_id, published.file_id, recent).await;
            let server = match published.server_file_id {
                Some(server_id) => curseforge_file_listing(published.project_id, server_id, recent).await,
                None => Listing::Public,
            };
            client.max(server)
        }
        None => Listing::Public,
    };
    let on_modrinth = match &release.modrinth {
        Some(published) => match modrinth::version(&published.version_id).await {
            Ok(Some(version)) if version.status.as_deref().is_none_or(|status| status == "listed") => Listing::Public,
            _ => Listing::Held,
        },
        None => Listing::Public,
    };
    on_curseforge.max(on_modrinth)
}

pub async fn announcement(pack: &Pack, release: &Release) -> Announcement {
    let changelog = std::fs::read_to_string(pack.dir.join(CHANGELOG_ARCHIVE).join(format!("{}.md", release.version))).unwrap_or_default();
    let (project, file_url) = match (&release.curseforge, &release.modrinth) {
        (Some(published), _) => {
            let project = project(published.project_id, &pack.config.author).await;
            let file_url = format!("{}/files/{}", project.url, published.file_id);
            (project, file_url)
        }
        (None, Some(published)) => {
            let file_url = format!("https://modrinth.com/project/{}/version/{}", published.project_id, published.version_id);
            (modrinth_project(&published.project_id).await, file_url)
        }
        (None, None) => {
            let project = project(0, &pack.config.author).await;
            let file_url = project.url.clone();
            (project, file_url)
        }
    };
    Announcement {
        name: pack.name(),
        version: release.version.clone(),
        changelog,
        file_url,
        project,
        game: game(pack),
        timestamp: chrono::Utc::now().to_rfc3339(),
    }
}

/// Returns the project announcements link to: the CurseForge one when the pack is there, or else Modrinth's.
pub async fn pack_project(pack: &Pack) -> Project {
    if !pack.config.targets_curseforge() && pack.config.targets_modrinth() {
        modrinth_project(pack.config.modrinth.project_id.trim()).await
    } else {
        project(pack.config.curseforge.project_id, &pack.config.author).await
    }
}

async fn modrinth_project(id: &str) -> Project {
    let found = modrinth::project(id).await.ok().flatten();
    Project {
        site: "Modrinth",
        name: found.as_ref().map(|p| p.title.clone()),
        url: found.as_ref().map_or_else(|| format!("https://modrinth.com/project/{id}"), modrinth::Project::url),
        logo: found.and_then(|p| p.icon_url),
        author: None,
    }
}

pub async fn project(project_id: u64, preferred_author: &str) -> Project {
    if project_id == 0 {
        return Project { site: "CurseForge", name: None, url: "https://www.curseforge.com".into(), logo: None, author: None };
    }
    let found = curseforge::mods(&[project_id]).await.ok().and_then(|mut mods| mods.remove(&project_id));
    let authors = found.as_ref().map(|m| m.authors.clone()).unwrap_or_default();
    let author = authors.iter().find(|a| a.name.eq_ignore_ascii_case(preferred_author.trim())).or(authors.first()).cloned();
    Project {
        site: "CurseForge",
        author,
        url: found
            .as_ref()
            .and_then(|m| m.links.website_url.as_deref())
            .map(|url| url.trim_end_matches('/').to_string())
            .unwrap_or_else(|| format!("https://www.curseforge.com/projects/{project_id}")),
        logo: found.as_ref().and_then(|m| m.logo.as_ref()).and_then(curseforge::CfLogo::url),
        name: found.map(|m| m.name),
    }
}

pub fn game(pack: &Pack) -> String {
    format!("Minecraft {}, {}", pack.lock.minecraft.as_deref().unwrap_or("?"), pack.lock.loader.as_ref().map_or_else(|| "?".into(), ToString::to_string))
}

fn safe_cache_key(key: &str) -> String {
    let cleaned: String = key.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c }).collect();
    let trimmed = cleaned.trim_matches('.');
    if trimmed.is_empty() { "_".into() } else { trimmed.into() }
}

pub fn pending_images_dir(pack_data: &Path) -> PathBuf {
    pack_data.join("announcement-images").join("pending")
}

fn pending_manifest_path(pack_data: &Path) -> PathBuf {
    pack_data.join("announcement-images").join("pending.json")
}

pub fn release_images_dir(pack_data: &Path, version: &str) -> PathBuf {
    pack_data.join("announcement-images").join("releases").join(safe_cache_key(version))
}

pub fn load_pending_images(pack_data: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(pending_manifest_path(pack_data)) else { return Vec::new() };
    serde_json::from_str::<Vec<String>>(&text)
        .unwrap_or_default()
        .into_iter()
        .filter(|path| Path::new(path).is_file())
        .collect()
}

pub fn cache_images(dir: &Path, images: &[String]) -> Result<Vec<String>> {
    if images.is_empty() {
        if dir.exists() {
            let _ = std::fs::remove_dir_all(dir);
        }
        return Ok(Vec::new());
    }
    std::fs::create_dir_all(dir).with_context(|| format!("Couldn't create {}", dir.display()))?;
    let mut kept = HashSet::new();
    let mut cached = Vec::with_capacity(images.len());
    for (index, src) in images.iter().enumerate() {
        let src_path = Path::new(src);
        let in_cache = src_path.parent() == Some(dir) && src_path.is_file();
        let dest = if in_cache && !kept.contains(&src_path.to_path_buf()) {
            src_path.to_path_buf()
        } else {
            let file_name = src_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| format!("image-{index}"));
            let mut candidate = dir.join(&file_name);
            if kept.contains(&candidate) {
                candidate = dir.join(format!("{}-{file_name}", index + 1));
            }
            if src_path != candidate {
                std::fs::copy(src_path, &candidate).with_context(|| format!("Couldn't copy {file_name} into the announcement cache"))?;
            }
            candidate
        };
        kept.insert(dest.clone());
        cached.push(dest.to_string_lossy().into_owned());
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !kept.contains(&path) {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    Ok(cached)
}

pub fn save_pending_images(pack_data: &Path, images: &[String]) -> Result<Vec<String>> {
    let cached = cache_images(&pending_images_dir(pack_data), images)?;
    let manifest = pending_manifest_path(pack_data);
    if cached.is_empty() {
        let _ = std::fs::remove_file(&manifest);
    } else {
        if let Some(parent) = manifest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&manifest, serde_json::to_string(&cached)?)?;
    }
    Ok(cached)
}

pub fn clear_pending_images(pack_data: &Path) {
    let _ = std::fs::remove_dir_all(pending_images_dir(pack_data));
    let _ = std::fs::remove_file(pending_manifest_path(pack_data));
}

pub fn clear_release_images(pack_data: &Path, version: &str) {
    let _ = std::fs::remove_dir_all(release_images_dir(pack_data, version));
}

pub async fn announce(pack_dir: &Path, pack_data: &Path, releases_path: &Path, version: &str, webhook: &str) -> Result<()> {
    let pack = Pack::open(pack_dir)?;
    let mut releases = publish::load_releases(releases_path);
    let release = releases.iter_mut().find(|r| r.version == version).with_context(|| format!("you haven't published release {version} from this machine"))?;
    let composed = compose(&announcement(&pack, release).await, &pack.config.discord, &release.announcement_images);
    send(webhook, &composed.payload, &composed.attachments).await?;
    release.announced_at = Some(chrono::Utc::now().to_rfc3339());
    release.announcement_images.clear();
    publish::save_releases(releases_path, &releases)?;
    clear_release_images(pack_data, version);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headings_become_bold_and_long_text_links_out() {
        assert_eq!(discord_markdown("## Mod changes\n### Added\n- Foo"), "**Mod changes**\n**Added**\n- Foo");
        let long = "- línea 🎉\n".repeat(1000);
        let link = "[Read the full changelog on Modrinth](https://example.com/files/1)";
        let short = shorten(&long, link, DESCRIPTION_LIMIT);
        assert!(discord_len(&short) <= DESCRIPTION_LIMIT);
        assert!(short.ends_with(&format!("🎉\n\n{link}")));
    }

    #[test]
    fn oversized_templates_and_names_stay_within_discord_limits() {
        let announcement = Announcement {
            name: "P".repeat(100),
            version: "1.0".into(),
            changelog: "- line\n".repeat(1000),
            file_url: "https://cf/files/7".into(),
            project: Project { site: "CurseForge", name: None, url: "https://cf".into(), logo: None, author: None },
            game: String::new(),
            timestamp: String::new(),
        };
        let config = DiscordConfig { header: "é".repeat(5000), footer: "[Install]({file_url})".into(), title: "T".repeat(300), ..Default::default() };
        let message = compose(&announcement, &config, &[]).payload;
        let embed = &message["embeds"][0];
        assert!(discord_len(embed["description"].as_str().unwrap()) <= DESCRIPTION_LIMIT);
        assert_eq!(discord_len(embed["title"].as_str().unwrap()), TITLE_LIMIT);
        assert_eq!(discord_len(message["username"].as_str().unwrap()), USERNAME_LIMIT);
    }

    #[test]
    fn header_and_footer_fill_placeholders() {
        let announcement = Announcement {
            name: "Pack".into(),
            version: "1.0".into(),
            changelog: "- Foo".into(),
            file_url: "https://cf/files/7".into(),
            project: Project {
                site: "CurseForge",
                name: Some("Pack CF".into()),
                url: "https://cf".into(),
                logo: Some("https://logo".into()),
                author: Some(curseforge::CfAuthor { name: "MuonR".into(), url: "https://cf/members/muonr".into(), avatar_url: Some("https://avatar".into()) }),
            },
            game: String::new(),
            timestamp: String::new(),
        };
        let config = DiscordConfig {
            header: "{name} {version}".into(),
            footer: "[Install]({file_url})".into(),
            title: "{project_name} - v{version}".into(),
            ..Default::default()
        };
        let embed = &compose(&announcement, &config, &[]).payload["embeds"][0];
        assert_eq!(embed["title"], "Pack CF - v1.0");
        assert_eq!(embed["thumbnail"]["url"], "https://logo");
        assert_eq!(embed["author"]["icon_url"], "https://avatar");
        let description = embed["description"].as_str().unwrap().to_string();
        assert_eq!(description, "Pack 1.0\n\n- Foo\n\n[Install](https://cf/files/7)");
    }

    fn announcing(changelog: &str) -> Announcement {
        Announcement {
            name: "Pack".into(),
            version: "1.0".into(),
            changelog: changelog.into(),
            file_url: "https://cf/files/7".into(),
            project: Project { site: "CurseForge", name: None, url: "https://cf".into(), logo: None, author: None },
            game: "Minecraft 1.21.1, NeoForge".into(),
            timestamp: "2026-11-16T12:00:00+00:00".into(),
        }
    }

    fn with_footer(footer: &str) -> DiscordConfig {
        DiscordConfig { footer: footer.into(), ..Default::default() }
    }

    fn described(composed: &Composed) -> &str {
        composed.payload["embeds"][0]["description"].as_str().unwrap()
    }

    const FULL_CHANGELOG: &str = "[Read the full changelog on CurseForge](https://cf/files/7)";

    #[test]
    fn empty_footer_adds_nothing() {
        let composed = compose(&announcing("- Foo\n- [Bar](https://cf/bar) 1.0"), &with_footer(""), &[]);
        assert_eq!(described(&composed), "- Foo\n- [Bar](https://cf/bar) 1.0");
        assert!(!composed.links_stripped && !composed.truncated);
        assert!(composed.attachments.is_empty());
        assert!(composed.payload.get("attachments").is_none());
        assert!(composed.payload["embeds"][0].get("image").is_none());
        assert_eq!(composed.payload["embeds"][0]["timestamp"], "2026-11-16T12:00:00+00:00");
    }

    #[test]
    fn footer_survives_truncation_after_the_full_changelog_link() {
        let footer = "Thanks for playing!\nReport bugs in #support.";
        let composed = compose(&announcing(&"- line\n".repeat(1000)), &with_footer(footer), &[]);
        assert!(composed.truncated && !composed.links_stripped);
        assert!(discord_len(described(&composed)) <= DESCRIPTION_LIMIT);
        assert!(described(&composed).ends_with(&format!("- line\n\n{FULL_CHANGELOG}\n\n{footer}")));
    }

    #[test]
    fn stripping_links_can_avoid_truncation() {
        let changelog = "- [Some Mod](https://www.curseforge.com/minecraft/mc-mods/some-mod) 1.0 to 1.1\n".repeat(60);
        assert!(discord_len(&changelog) > DESCRIPTION_LIMIT);
        let composed = compose(&announcing(&changelog), &with_footer(""), &[]);
        assert!(composed.links_stripped && !composed.truncated);
        assert_eq!(described(&composed), "- Some Mod 1.0 to 1.1\n".repeat(60).trim());
    }

    #[test]
    fn stripping_links_comes_before_truncation() {
        let changelog = "- [Some Mod](https://www.curseforge.com/minecraft/mc-mods/some-mod) 1.0 to 1.1\n".repeat(400);
        let composed = compose(&announcing(&changelog), &with_footer(""), &[]);
        assert!(composed.links_stripped && composed.truncated);
        assert!(discord_len(described(&composed)) <= DESCRIPTION_LIMIT);
        assert!(described(&composed).ends_with(&format!("- Some Mod 1.0 to 1.1\n\n{FULL_CHANGELOG}")));
        assert_eq!(described(&composed).matches("](").count(), 1);
    }

    #[test]
    fn footer_and_header_links_are_kept_when_changelog_links_go() {
        let changelog = "- [Some Mod](https://www.curseforge.com/minecraft/mc-mods/some-mod) 1.0 to 1.1\n".repeat(60);
        let config = DiscordConfig { header: "[Wiki](https://wiki)".into(), footer: "[Install]({file_url})\n[Discord](https://discord.gg/x)".into(), ..Default::default() };
        let composed = compose(&announcing(&changelog), &config, &[]);
        assert!(composed.links_stripped && !composed.truncated);
        assert!(described(&composed).starts_with("[Wiki](https://wiki)\n\n- Some Mod 1.0"));
        assert!(described(&composed).ends_with("1.1\n\n[Install](https://cf/files/7)\n[Discord](https://discord.gg/x)"));
    }

    #[test]
    fn links_stay_when_the_changelog_fits() {
        let composed = compose(&announcing("- [Some Mod](https://cf/some-mod) 1.0 to 1.1"), &with_footer("Bye"), &[]);
        assert_eq!(described(&composed), "- [Some Mod](https://cf/some-mod) 1.0 to 1.1\n\nBye");
        assert!(!composed.links_stripped);
    }

    #[test]
    fn strips_only_whole_markdown_links() {
        assert_eq!(without_links("[A](https://a) and [B b](<https://b> \"title\"), not [c] (d) or [e]\n(f)"), "A and B b, not [c] (d) or [e]\n(f)");
    }

    #[test]
    fn truncation_never_splits_a_character() {
        let footer = "Merci 🎉";
        let ending = format!("\n\n{FULL_CHANGELOG}\n\n{footer}");
        // Each padding moves the cut to a different place among the two-unit emoji and the accented letters.
        for padding in 0..8 {
            let changelog = format!("{}\n{}", "x".repeat(padding), "🎉é\n".repeat(3000));
            let composed = compose(&announcing(&changelog), &with_footer(footer), &[]);
            let text = described(&composed);
            assert!(composed.truncated);
            assert!(discord_len(text) <= DESCRIPTION_LIMIT);
            let body = text.strip_suffix(&ending).expect("the link and the footer end the description");
            assert!(body.ends_with("🎉é"));
            assert!(body.chars().all(|c| matches!(c, 'x' | '\n' | '🎉' | 'é')));
        }
        let unbroken = compose(&announcing(&"🎉".repeat(3000)), &with_footer(footer), &[]);
        let body = described(&unbroken).strip_suffix(&ending).unwrap();
        assert!(discord_len(described(&unbroken)) <= DESCRIPTION_LIMIT);
        assert!(!body.is_empty() && body.chars().all(|c| c == '🎉'));
    }

    #[test]
    fn oversized_header_never_costs_the_footer() {
        let config = DiscordConfig { header: "é".repeat(5000), footer: "[Install]({file_url})".into(), ..Default::default() };
        let composed = compose(&announcing(&"- line\n".repeat(1000)), &config, &[]);
        assert!(discord_len(described(&composed)) <= DESCRIPTION_LIMIT);
        assert!(described(&composed).ends_with("\n\n[Install](https://cf/files/7)"));
    }

    #[test]
    fn images_share_the_embed_url_as_a_gallery() {
        let images = ["C:\\shots\\My shot (1).png".to_string(), "/home/me/b/my shot (1).png".to_string(), "/home/me/map.webp".to_string()];
        let composed = compose(&announcing("- Foo"), &with_footer(""), &images);
        let names: Vec<_> = composed.attachments.iter().map(|a| a.filename.as_str()).collect();
        assert_eq!(names, ["My_shot__1_.png", "2-my_shot__1_.png", "map.webp"]);
        assert_eq!(composed.attachments[0].path, images[0]);
        assert_eq!(composed.payload["embeds"][0]["image"]["url"], "attachment://My_shot__1_.png");
        assert_eq!(composed.payload["embeds"][1]["url"], "https://cf/files/7");
        assert_eq!(composed.payload["embeds"][1]["image"]["url"], "attachment://2-my_shot__1_.png");
        assert_eq!(composed.payload["embeds"][2]["url"], "https://cf/files/7");
        assert_eq!(composed.payload["embeds"][2]["image"]["url"], "attachment://map.webp");
        assert_eq!(composed.payload["attachments"], json!([{ "id": 0, "filename": "My_shot__1_.png" }, { "id": 1, "filename": "2-my_shot__1_.png" }, { "id": 2, "filename": "map.webp" }]));
    }

    #[test]
    fn rejects_too_many_oversized_or_non_image_files() {
        let dir = std::env::temp_dir().join(format!("packraven-discord-images-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = |name: &str| dir.join(name).to_string_lossy().into_owned();
        std::fs::write(path("ok.png"), [0u8; 16]).unwrap();
        std::fs::write(path("notes.txt"), [0u8; 16]).unwrap();
        std::fs::File::create(path("huge.png")).unwrap().set_len(MAX_ATTACHMENT_BYTES + 1024 * 1024).unwrap();
        std::fs::File::create(path("limit.PNG")).unwrap().set_len(MAX_ATTACHMENT_BYTES).unwrap();

        assert!(check_images(&[path("ok.png"), path("limit.PNG")]).is_ok());
        assert!(check_images(&vec![path("ok.png"); MAX_ATTACHMENTS]).is_ok());
        assert!(check_images(&vec![path("ok.png"); MAX_ATTACHMENTS + 1]).unwrap_err().to_string().contains("up to 4 images"));
        assert!(check_images(&[path("huge.png")]).unwrap_err().to_string().contains("huge.png is 11.0 MB"));
        assert!(check_images(&[path("notes.txt")]).unwrap_err().to_string().contains("isn't a PNG"));
        assert!(check_images(&[path("missing.png")]).unwrap_err().to_string().contains("Couldn't read missing.png"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_non_discord_urls() {
        assert!(validate_webhook("https://discord.com/api/webhooks/1/abc").is_ok());
        assert!(validate_webhook("https://example.com/hook").is_err());
    }

    #[test]
    fn condenses_mod_updates_section_when_enabled() {
        let changelog = "Notes\n\n## Mod changes\n\n### Added\n- Foo\n\n### Updated\n- Bar: 1.0 to 1.1\n- Baz: 2.0 to 2.1\n\n### Removed\n- Qux\n";
        let config = DiscordConfig { condense_mod_updates: true, ..Default::default() };
        let composed = compose(&announcing(changelog), &config, &[]);
        let desc = described(&composed);
        assert!(desc.contains("**Added**\n- Foo"));
        assert!(desc.contains("Updated 2 mods"));
        assert!(desc.contains("**Removed**\n- Qux"));
        assert!(!desc.contains("Bar: 1.0 to 1.1"));

        let single = "## Mod changes\n\n### Updated\n- Bar: 1.0 to 1.1\n";
        let single_composed = compose(&announcing(single), &config, &[]);
        let single_desc = described(&single_composed);
        assert_eq!(single_desc, "**Mod changes**\n\nUpdated 1 mod");
    }

    #[test]
    fn caches_images_and_cleans_up_removed_ones() {
        let root = std::env::temp_dir().join(format!("packraven-image-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src_dir = root.join("src");
        let pack_data = root.join("pack-data");
        std::fs::create_dir_all(&src_dir).unwrap();
        let a = src_dir.join("a.png");
        let b = src_dir.join("b.png");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();

        let cached = save_pending_images(&pack_data, &[a.to_string_lossy().into(), b.to_string_lossy().into()]).unwrap();
        assert_eq!(cached.len(), 2);
        // Deleting the source files doesn't break the cached copies.
        std::fs::remove_file(&a).unwrap();
        std::fs::remove_file(&b).unwrap();
        assert_eq!(load_pending_images(&pack_data), cached);

        let kept = save_pending_images(&pack_data, &[cached[0].clone()]).unwrap();
        assert_eq!(kept, vec![cached[0].clone()]);
        assert!(!Path::new(&cached[1]).exists());

        clear_pending_images(&pack_data);
        assert!(load_pending_images(&pack_data).is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
