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

#[derive(Clone, PartialEq)]
struct Wanted {
    pack: Option<String>,
    window_visible: bool,
    since: i64,
}

static WANTED: Mutex<Wanted> = Mutex::new(Wanted { pack: None, window_visible: false, since: 0 });
static CHANGED: Condvar = Condvar::new();

pub fn set_pack(name: Option<String>) {
    update(|w| w.pack = name);
}

pub fn set_window_visible(visible: bool) {
    update(|w| w.window_visible = visible);
}

fn wanted() -> MutexGuard<'static, Wanted> {
    WANTED.lock().unwrap_or_else(|e| e.into_inner())
}

fn update(change: impl FnOnce(&mut Wanted)) {
    let mut wanted = wanted();
    let before = (wanted.window_visible, wanted.pack.clone());
    change(&mut wanted);
    if (wanted.window_visible, &wanted.pack) != (before.0, &before.1) {
        wanted.since = chrono::Utc::now().timestamp_millis();
    }
    CHANGED.notify_all();
}

pub fn start() {
    std::thread::spawn(|| {
        let mut client: Option<DiscordIpcClient> = None;
        loop {
            let snapshot = wanted().clone();
            if snapshot.window_visible {
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

fn show(client: &mut DiscordIpcClient, wanted: &Wanted) -> Result<(), Error> {
    let details = wanted.pack.as_ref().map(|pack| format!("Working on {pack}"));
    let mut activity = Activity::new()
        .assets(Assets::new().large_image(LOGO_ASSET).large_text("packraven"))
        .timestamps(Timestamps::new().start(wanted.since));
    if let Some(details) = &details {
        activity = activity.details(details.as_str());
    }
    client.set_activity(activity)?;
    // Discord replies to every command, so we read each reply to keep the pipe from filling up.
    client.recv().map(|_| ())
}
