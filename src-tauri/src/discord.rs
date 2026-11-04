use std::path::Path;

use anyhow::{bail, Context, Result};
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

pub struct Announcement {
    pub name: String,
    pub version: String,
    pub changelog: String,
    pub file_url: String,
    pub project: Project,
    pub game: String,
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
    format!("{}…", fitting_prefix(text, limit - 1).trim_end())
}

fn shorten(text: &str, link: &str, limit: usize) -> String {
    if discord_len(text) <= limit {
        return text.to_string();
    }
    let kept = fitting_prefix(text, limit.saturating_sub(discord_len(link) + 2));
    let cut = kept.rfind('\n').unwrap_or(kept.len());
    format!("{}\n\n{link}", text[..cut].trim_end())
}

fn color(hex: &str) -> u32 {
    u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0x4d6b2f)
}

pub fn message(announcement: &Announcement, config: &DiscordConfig) -> Value {
    let role = config.role_id.trim();
    let changelog = discord_markdown(announcement.changelog.trim());
    let header = announcement.fill(&config.header);
    let footer = announcement.fill(&config.footer);
    let body = if changelog.is_empty() {
        "A new version is out.".to_string()
    } else {
        let link = format!("[Read the full changelog on {}]({})", announcement.project.site, announcement.file_url);
        shorten(&changelog, &link, DESCRIPTION_LIMIT.saturating_sub(discord_len(&header) + discord_len(&footer) + 4))
    };
    // A header and footer that fill the limit on their own leave the changelog no room, so as a last
    // resort we cut the whole description rather than let Discord reject the post.
    let description = clamp(&[header, body, footer].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join("\n\n"), DESCRIPTION_LIMIT);
    let title = match announcement.fill(&config.title) {
        title if title.is_empty() => format!("{} {}", announcement.name, announcement.version),
        title => title,
    };
    let title = clamp(&title, TITLE_LIMIT);
    let mut embed = json!({
        "title": title,
        "url": announcement.file_url,
        "description": description,
        "color": color(&config.color),
        "footer": { "text": announcement.game },
        "timestamp": chrono::Utc::now().to_rfc3339(),
    });
    if let Some(logo) = &announcement.project.logo {
        embed["thumbnail"] = json!({ "url": logo });
    }
    if let Some(author) = announcement.project.author.as_ref().filter(|_| config.show_author) {
        embed["author"] = json!({ "name": author.name, "url": author.url, "icon_url": author.avatar_url });
    }
    json!({
        "username": clamp(&announcement.name, USERNAME_LIMIT),
        "content": if role.is_empty() { String::new() } else { format!("<@&{role}>") },
        "allowed_mentions": { "parse": [], "roles": if role.is_empty() { vec![] } else { vec![role.to_string()] } },
        "embeds": [embed],
    })
}

pub async fn send(webhook: &str, message: &Value) -> Result<String> {
    validate_webhook(webhook)?;
    let response = CLIENT.post(format!("{webhook}?wait=true")).json(message).send().await?;
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

/// Returns whether every platform the release went to lists it, taking the slowest platform's listing.
/// Modrinth usually lists a version on an approved project as soon as we upload it, but it can hold one
/// for review too.
pub async fn listing(release: &Release) -> Listing {
    let on_curseforge = match &release.curseforge {
        Some(published) => match curseforge::file(published.project_id, published.file_id).await {
            Ok(Some(file)) => match curseforge::file_state(file.file_status) {
                FileState::Approved => Listing::Public,
                FileState::Processing | FileState::InReview => Listing::Pending,
                _ => Listing::Held,
            },
            _ if release.uploaded_to_curseforge_recently() => Listing::Pending,
            _ => Listing::Held,
        },
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
        logo: found.as_ref().and_then(|m| m.logo.as_ref()).map(|logo| logo.thumbnail_url.clone()),
        name: found.map(|m| m.name),
    }
}

pub fn game(pack: &Pack) -> String {
    format!("Minecraft {}, {}", pack.lock.minecraft.as_deref().unwrap_or("?"), pack.lock.loader.as_ref().map_or_else(|| "?".into(), ToString::to_string))
}

pub async fn announce(pack_dir: &Path, releases_path: &Path, version: &str, webhook: &str) -> Result<()> {
    let pack = Pack::open(pack_dir)?;
    let mut releases = publish::load_releases(releases_path);
    let release = releases.iter_mut().find(|r| r.version == version).with_context(|| format!("you haven't published release {version} from this machine"))?;
    let message = message(&announcement(&pack, release).await, &pack.config.discord);
    send(webhook, &message).await?;
    release.announced_at = Some(chrono::Utc::now().to_rfc3339());
    publish::save_releases(releases_path, &releases)
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
        };
        let config = DiscordConfig { header: "é".repeat(5000), footer: "[Install]({file_url})".into(), title: "T".repeat(300), ..Default::default() };
        let message = message(&announcement, &config);
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
        };
        let config = DiscordConfig {
            header: "{name} {version}".into(),
            footer: "[Install]({file_url})".into(),
            title: "{project_name} - v{version}".into(),
            ..Default::default()
        };
        let embed = &message(&announcement, &config)["embeds"][0];
        assert_eq!(embed["title"], "Pack CF - v1.0");
        assert_eq!(embed["thumbnail"]["url"], "https://logo");
        assert_eq!(embed["author"]["icon_url"], "https://avatar");
        let description = embed["description"].as_str().unwrap().to_string();
        assert_eq!(description, "Pack 1.0\n\n- Foo\n\n[Install](https://cf/files/7)");
    }

    #[test]
    fn rejects_non_discord_urls() {
        assert!(validate_webhook("https://discord.com/api/webhooks/1/abc").is_ok());
        assert!(validate_webhook("https://example.com/hook").is_err());
    }
}
