use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

use discord_rich_presence::activity::{Activity, Assets, Timestamps};
use discord_rich_presence::error::Error;
use discord_rich_presence::{DiscordIpc, DiscordIpcClient};

// Discord titles the activity with the name of the developer portal application that owns this ID.
const CLIENT_ID: &str = "1567526554920288318";
const LOGO_ASSET: &str = "logo";
// We resend the activity this often, which is how we notice Discord starting or restarting.
const REFRESH_INTERVAL: Duration = Duration::from_secs(15);
const MAX_FIELD_CHARS: usize = 128;
const MAX_IMAGE_URL_LEN: usize = 256;

#[derive(Clone, PartialEq, Debug)]
pub struct PackPresence {
    pub name: String,
    pub icon_url: Option<String>,
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct ActivityDetail {
    pub specific: Option<String>,
    pub generic: Option<String>,
}

#[derive(Clone, PartialEq)]
struct Wanted {
    enabled: bool,
    shows_pack: bool,
    pack: Option<PackPresence>,
    activity: ActivityDetail,
    window_visible: bool,
    since: i64,
}

static WANTED: Mutex<Wanted> = Mutex::new(Wanted {
    enabled: true,
    shows_pack: true,
    pack: None,
    activity: ActivityDetail { specific: None, generic: None },
    window_visible: false,
    since: 0,
});
static CHANGED: Condvar = Condvar::new();

pub fn set_settings(enabled: bool, shows_pack: bool) {
    update(|w| {
        w.enabled = enabled;
        w.shows_pack = shows_pack;
    });
}

pub fn set_pack(pack: Option<PackPresence>) {
    update(|w| w.pack = pack);
}

pub fn set_pack_icon(pack_name: &str, icon_url: Option<String>) {
    update(|w| {
        if let Some(pack) = &mut w.pack
            && pack.name == pack_name
        {
            pack.icon_url = icon_url;
        }
    });
}

pub fn set_activity(activity: ActivityDetail) {
    update(|w| w.activity = activity);
}

pub fn set_window_visible(visible: bool) {
    update(|w| w.window_visible = visible);
}

fn wanted() -> MutexGuard<'static, Wanted> {
    WANTED.lock().unwrap_or_else(|e| e.into_inner())
}

fn active_pack_name(w: &Wanted) -> Option<&str> {
    w.shows_pack.then(|| w.pack.as_ref().map(|p| p.name.as_str())).flatten()
}

fn update(change: impl FnOnce(&mut Wanted)) {
    let mut wanted = wanted();
    let was_active = wanted.enabled && wanted.window_visible;
    let prev_pack = active_pack_name(&wanted).map(String::from);
    change(&mut wanted);
    let now_active = wanted.enabled && wanted.window_visible;
    let next_pack = active_pack_name(&wanted);
    if now_active && (!was_active || next_pack != prev_pack.as_deref()) {
        wanted.since = chrono::Utc::now().timestamp_millis();
    }
    CHANGED.notify_all();
}

pub fn start() {
    std::thread::spawn(|| {
        let mut client: Option<DiscordIpcClient> = None;
        loop {
            let snapshot = wanted().clone();
            if snapshot.enabled && snapshot.window_visible {
                let connected = client.take().or_else(connect);
                client = connected.and_then(|mut c| show(&mut c, &snapshot).ok().map(|()| c));
                drop(CHANGED.wait_timeout_while(wanted(), REFRESH_INTERVAL, |w| *w == snapshot));
            } else {
                if let Some(mut c) = client.take() {
                    let _ = c.close();
                }
                drop(CHANGED.wait_while(wanted(), |w| *w == snapshot));
            }
        }
    });
}

fn connect() -> Option<DiscordIpcClient> {
    let mut client = DiscordIpcClient::new(CLIENT_ID);
    client.connect().ok().map(|()| client)
}

#[derive(Debug, PartialEq)]
struct BuiltActivity {
    details: Option<String>,
    state: Option<String>,
    large_image: String,
    large_text: String,
    small_image: Option<&'static str>,
    small_text: Option<&'static str>,
}

fn clamp_text(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let count = trimmed.chars().count();
    if count < 2 {
        return None;
    }
    if count <= MAX_FIELD_CHARS {
        return Some(trimmed.to_string());
    }
    let mut out: String = trimmed.chars().take(MAX_FIELD_CHARS - 1).collect();
    out.push('…');
    Some(out)
}

fn valid_icon_url(url: &str) -> bool {
    let trimmed = url.trim();
    trimmed.len() <= MAX_IMAGE_URL_LEN && (trimmed.starts_with("https://") || trimmed.starts_with("http://"))
}

fn build(wanted: &Wanted) -> BuiltActivity {
    let pack = wanted.shows_pack.then_some(wanted.pack.as_ref()).flatten();
    let details = pack.and_then(|p| clamp_text(&format!("Working on {}", p.name)));
    let raw_state = if wanted.shows_pack {
        wanted.activity.specific.as_deref().or(wanted.activity.generic.as_deref())
    } else {
        wanted.activity.generic.as_deref()
    };
    let state = raw_state.and_then(clamp_text);

    let pack_icon = pack
        .and_then(|p| p.icon_url.as_deref())
        .map(str::trim)
        .filter(|u| valid_icon_url(u));

    let (large_image, large_text, small_image, small_text) = match (pack, pack_icon) {
        (Some(pack), Some(icon_url)) => (
            icon_url.to_string(),
            clamp_text(&pack.name).unwrap_or_else(|| "packraven".into()),
            Some(LOGO_ASSET),
            Some("packraven"),
        ),
        _ => (LOGO_ASSET.into(), "packraven".into(), None, None),
    };

    BuiltActivity { details, state, large_image, large_text, small_image, small_text }
}

fn show(client: &mut DiscordIpcClient, wanted: &Wanted) -> Result<(), Error> {
    let built = build(wanted);
    let mut assets = Assets::new().large_image(built.large_image.as_str()).large_text(built.large_text.as_str());
    if let Some(small) = built.small_image {
        assets = assets.small_image(small);
    }
    if let Some(small_text) = built.small_text {
        assets = assets.small_text(small_text);
    }
    let mut activity = Activity::new().assets(assets).timestamps(Timestamps::new().start(wanted.since));
    if let Some(details) = &built.details {
        activity = activity.details(details.as_str());
    }
    if let Some(state) = &built.state {
        activity = activity.state(state.as_str());
    }
    client.set_activity(activity)?;
    // Discord replies to every command, so we read each reply to keep the pipe from filling up.
    client.recv().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_wanted() -> Wanted {
        Wanted {
            enabled: true,
            shows_pack: true,
            pack: Some(PackPresence { name: "ATM10".into(), icon_url: Some("https://media.forgecdn.net/avatars/thumbnails/1/2/256/256/icon.png".into()) }),
            activity: ActivityDetail { specific: Some("Editing bhmenu-general.properties".into()), generic: Some("Editing a file".into()) },
            window_visible: true,
            since: 1000,
        }
    }

    #[test]
    fn builds_full_activity_with_pack_icon_and_raven_badge() {
        let built = build(&base_wanted());
        assert_eq!(built.details.as_deref(), Some("Working on ATM10"));
        assert_eq!(built.state.as_deref(), Some("Editing bhmenu-general.properties"));
        assert_eq!(built.large_image, "https://media.forgecdn.net/avatars/thumbnails/1/2/256/256/icon.png");
        assert_eq!(built.large_text, "ATM10");
        assert_eq!(built.small_image, Some("logo"));
        assert_eq!(built.small_text, Some("packraven"));
    }

    #[test]
    fn falls_back_to_raven_when_pack_has_no_icon() {
        let mut w = base_wanted();
        w.pack.as_mut().unwrap().icon_url = None;
        let built = build(&w);
        assert_eq!(built.details.as_deref(), Some("Working on ATM10"));
        assert_eq!(built.large_image, "logo");
        assert_eq!(built.large_text, "packraven");
        assert_eq!(built.small_image, None);
    }

    #[test]
    fn hides_pack_name_icon_and_specific_activity_when_pack_is_private() {
        let mut w = base_wanted();
        w.shows_pack = false;
        let built = build(&w);
        assert_eq!(built.details, None);
        assert_eq!(built.state.as_deref(), Some("Editing a file"));
        assert_eq!(built.large_image, "logo");
        assert_eq!(built.large_text, "packraven");
        assert_eq!(built.small_image, None);
    }

    #[test]
    fn clamps_long_activity_strings_and_drops_invalid_icon_urls() {
        let mut w = base_wanted();
        w.pack.as_mut().unwrap().icon_url = Some("C:/local/icon.png".into());
        w.activity.specific = Some("Editing ".to_string() + &"a".repeat(200));
        let built = build(&w);
        assert_eq!(built.large_image, "logo");
        let state = built.state.unwrap();
        assert_eq!(state.chars().count(), MAX_FIELD_CHARS);
        assert!(state.ends_with('…'));
    }
}
