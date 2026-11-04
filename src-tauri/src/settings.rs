use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::pack::LOCKFILE;
use crate::setup::same_dir;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub instance_dir: PathBuf,
    pub packs: Vec<PathBuf>,
    pub install_dir: PathBuf,
    #[serde(rename = "java", skip_serializing)]
    pub legacy_java: Option<PathBuf>,
    pub java_by_major: BTreeMap<u32, PathBuf>,
    pub pack_java: BTreeMap<String, PathBuf>,
    pub run_args: RunArgs,
    pub pack_run_args: BTreeMap<String, RunArgs>,
    pub defender_excluded: Vec<PathBuf>,
    pub defender_dismissed: bool,
    pub minecraft_eula_accepted: bool,
    pub discord_webhooks: BTreeMap<String, String>,
    /// Files larger than this many megabytes open only in the simplified editor, if you ask.
    pub editor_ask_mb: u32,
    /// Files larger than this many megabytes don't open in the editor at all.
    pub editor_max_mb: u32,
    pub git_name: String,
    pub git_email: String,
    pub auto_update: bool,
    pub discord_shows_pack: bool,
    pub updated_from: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct RunArgs {
    pub client_jvm_args: Vec<String>,
    pub client_args: Vec<String>,
    pub server_jvm_args: Vec<String>,
    pub server_args: Vec<String>,
}

// JVM flag lines split at spaces, as Java splits the lines of user_jvm_args.txt, while each
// argument line stays whole. Commented-out lines are kept so you can turn a flag back on.
impl RunArgs {
    pub fn client_jvm(&self) -> Vec<String> {
        active(&self.client_jvm_args).flat_map(crate::launch::split_args).collect()
    }

    pub fn client(&self) -> Vec<String> {
        active(&self.client_args).map(String::from).collect()
    }

    pub fn server_jvm(&self) -> Vec<String> {
        active(&self.server_jvm_args).flat_map(crate::launch::split_args).collect()
    }

    pub fn server(&self) -> Vec<String> {
        active(&self.server_args).map(String::from).collect()
    }

    fn is_empty(&self) -> bool {
        [&self.client_jvm_args, &self.client_args, &self.server_jvm_args, &self.server_args].iter().all(|lines| lines.iter().all(|l| l.trim().is_empty()))
    }
}

fn active(lines: &[String]) -> impl Iterator<Item = &str> {
    lines.iter().map(|line| line.trim()).filter(|line| !line.is_empty() && !line.starts_with('#'))
}

fn curseforge_root() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_default();
    if cfg!(target_os = "macos") {
        home.join("Documents").join("curseforge").join("minecraft")
    } else {
        home.join("curseforge").join("minecraft")
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self { instance_dir: PathBuf::new(), packs: Vec::new(), install_dir: curseforge_root().join("Install"), legacy_java: None, java_by_major: BTreeMap::new(), pack_java: BTreeMap::new(), run_args: RunArgs::default(), pack_run_args: BTreeMap::new(), defender_excluded: Vec::new(), defender_dismissed: false, minecraft_eula_accepted: false, discord_webhooks: BTreeMap::new(), editor_ask_mb: (crate::files::Limits::DEFAULT.ask / 1_000_000) as u32, editor_max_mb: (crate::files::Limits::DEFAULT.max / 1_000_000) as u32, git_name: String::new(), git_email: String::new(), auto_update: true, discord_shows_pack: true, updated_from: None }
    }
}

impl Settings {
    pub fn load(path: &PathBuf) -> Self {
        let mut settings: Self = std::fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default();
        // packraven only ran NeoForge 1.21 packs when it had a single Java setting.
        if let Some(java) = settings.legacy_java.take() {
            settings.java_by_major.entry(21).or_insert(java);
        }
        if settings.packs.is_empty() && settings.instance_dir.join(LOCKFILE).exists() {
            settings.packs.push(settings.instance_dir.clone());
        }
        settings
    }

    // macOS raises one privacy prompt per access to a protected folder like Documents until you answer,
    // so we touch them once here before the watcher and the UI read them in parallel.
    pub fn request_folder_access(&self) {
        for dir in self.packs.iter().chain([&self.install_dir]) {
            let _ = std::fs::read_dir(dir);
        }
    }

    pub fn save(&self, path: &PathBuf) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?).context("writing settings")
    }

    pub fn editor_limits(&self) -> crate::files::Limits {
        let ask = u64::from(self.editor_ask_mb.max(1)) * 1_000_000;
        crate::files::Limits { ask, max: (u64::from(self.editor_max_mb) * 1_000_000).max(ask) }
    }

    pub fn identity(&self) -> crate::git::Identity {
        crate::git::Identity { name: self.git_name.clone(), email: self.git_email.clone() }
    }

    pub fn webhook(&self, pack: &Path) -> Option<String> {
        self.discord_webhooks.get(&pack_key(pack)).filter(|w| !w.is_empty()).cloned()
    }

    pub fn set_webhook(&mut self, pack: &Path, url: &str) {
        let url = url.trim();
        if url.is_empty() {
            self.discord_webhooks.remove(&pack_key(pack));
        } else {
            self.discord_webhooks.insert(pack_key(pack), url.to_string());
        }
    }

    pub fn pack_java(&self, pack: &Path) -> Option<&PathBuf> {
        self.pack_java.get(&pack_key(pack))
    }

    pub fn set_pack_java(&mut self, pack: &Path, java: Option<PathBuf>) {
        match java {
            Some(java) => self.pack_java.insert(pack_key(pack), java),
            None => self.pack_java.remove(&pack_key(pack)),
        };
    }

    pub fn pack_run_args(&self, pack: &Path) -> RunArgs {
        self.pack_run_args.get(&pack_key(pack)).cloned().unwrap_or_default()
    }

    pub fn set_pack_run_args(&mut self, pack: &Path, args: RunArgs) {
        if args.is_empty() {
            self.pack_run_args.remove(&pack_key(pack));
        } else {
            self.pack_run_args.insert(pack_key(pack), args);
        }
    }

    /// Returns the extra arguments for every pack followed by the pack's own, so the pack's win.
    pub fn combined_run_args(&self, pack: &Path) -> RunArgs {
        let own = self.pack_run_args(pack);
        let join = |all: &[String], own: &[String]| all.iter().chain(own).cloned().collect();
        RunArgs {
            client_jvm_args: join(&self.run_args.client_jvm_args, &own.client_jvm_args),
            client_args: join(&self.run_args.client_args, &own.client_args),
            server_jvm_args: join(&self.run_args.server_jvm_args, &own.server_jvm_args),
            server_args: join(&self.run_args.server_args, &own.server_args),
        }
    }

    pub fn instances_root(&self) -> PathBuf {
        self.install_dir.parent().map(|p| p.join("Instances")).unwrap_or_else(|| curseforge_root().join("Instances"))
    }

    pub fn activate(&mut self, dir: &Path) {
        if !self.packs.iter().any(|p| same_dir(p, dir)) {
            self.packs.push(dir.to_path_buf());
        }
        self.instance_dir = dir.to_path_buf();
    }

    pub fn remove(&mut self, dir: &Path) {
        self.packs.retain(|p| !same_dir(p, dir));
        if same_dir(&self.instance_dir, dir) {
            self.instance_dir = self.packs.first().cloned().unwrap_or_default();
        }
    }
}

fn pack_key(pack: &Path) -> String {
    pack.to_string_lossy().trim_end_matches(['\\', '/']).replace('\\', "/").to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_args_split_flags_but_keep_arguments_whole() {
        let lines = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let args = RunArgs {
            client_jvm_args: lines(&["--add-opens java.base/java.lang=ALL-UNNAMED", "# -Dmixin.debug.export=true", "", r#"-javaagent:"C:/Program Files/spark.jar""#]),
            client_args: lines(&["--quickPlaySingleplayer", "New World", "#--demo"]),
            ..RunArgs::default()
        };
        assert_eq!(args.client_jvm(), ["--add-opens", "java.base/java.lang=ALL-UNNAMED", "-javaagent:C:/Program Files/spark.jar"]);
        assert_eq!(args.client(), ["--quickPlaySingleplayer", "New World"]);
        assert!(args.server_jvm().is_empty());
        assert!(RunArgs { server_args: lines(&["  "]), ..RunArgs::default() }.is_empty());
    }

    #[test]
    fn pack_run_args_come_after_the_ones_for_every_pack() {
        let mut settings = Settings { run_args: RunArgs { client_jvm_args: vec!["-Da=1".into()], ..RunArgs::default() }, ..Settings::default() };
        let pack = Path::new("C:/Instances/Pack");
        settings.set_pack_run_args(pack, RunArgs { client_jvm_args: vec!["-Da=2".into()], ..RunArgs::default() });
        assert_eq!(settings.combined_run_args(Path::new(r"c:\instances\pack\")).client_jvm(), ["-Da=1", "-Da=2"]);
        settings.set_pack_run_args(pack, RunArgs::default());
        assert!(settings.pack_run_args.is_empty());
    }
}
