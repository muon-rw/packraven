use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::instance::Instance;
use crate::pack::Pack;
use crate::settings::Settings;

pub struct ClientLaunch {
    pub java: PathBuf,
    pub args: Vec<String>,
}

fn os_name() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "osx"
    } else {
        "linux"
    }
}

fn rule_matches(rule: &Value, features: &HashMap<&str, bool>) -> bool {
    if let Some(os) = rule.get("os") {
        if os.get("name").and_then(Value::as_str).is_some_and(|name| name != os_name()) {
            return false;
        }
        if os.get("arch").and_then(Value::as_str) == Some("x86") {
            return false;
        }
    }
    if let Some(wanted) = rule.get("features").and_then(Value::as_object) {
        return wanted.iter().all(|(k, v)| features.get(k.as_str()).copied().unwrap_or(false) == v.as_bool().unwrap_or(false));
    }
    true
}

fn allowed(rules: Option<&Value>, features: &HashMap<&str, bool>) -> bool {
    let Some(rules) = rules.and_then(Value::as_array) else { return true };
    let mut result = false;
    for rule in rules {
        if rule_matches(rule, features) {
            result = rule.get("action").and_then(Value::as_str) == Some("allow");
        }
    }
    result
}

fn maven_path(name: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = name.split(':').collect();
    let (group, artifact, version) = (parts.first()?, parts.get(1)?, parts.get(2)?);
    let classifier = parts.get(3).map(|c| format!("-{c}")).unwrap_or_default();
    let key = format!("{group}:{artifact}{classifier}");
    Some((key, format!("{}/{artifact}/{version}/{artifact}-{version}{classifier}.jar", group.replace('.', "/"))))
}

fn read_version(install: &Path, id: &str) -> Result<Value> {
    let path = install.join("versions").join(id).join(format!("{id}.json"));
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("{} is missing; launch the profile once from CurseForge so it installs {id}", path.display()))?;
    Ok(serde_json::from_str(&text)?)
}

pub struct Player {
    pub name: String,
    pub uuid: String,
    pub access_token: String,
}

pub fn split_args(text: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut has_token = false;
    for c in text.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                has_token = true;
            }
            c if c.is_whitespace() && !quoted => {
                if has_token {
                    args.push(std::mem::take(&mut current));
                    has_token = false;
                }
            }
            c => {
                current.push(c);
                has_token = true;
            }
        }
    }
    if has_token {
        args.push(current);
    }
    args
}

struct LauncherProfile {
    java: Option<PathBuf>,
    java_args: Option<String>,
}

fn launcher_profile(install: &Path, instance: &Path) -> LauncherProfile {
    let profiles: Option<Value> = std::fs::read_to_string(install.join("launcher_profiles.json")).ok().and_then(|t| serde_json::from_str(&t).ok());
    let wanted = instance.to_string_lossy().trim_end_matches(['\\', '/']).to_lowercase();
    let profile = profiles.as_ref().and_then(|p| p.get("profiles")).and_then(Value::as_object).and_then(|all| {
        all.values().find(|p| {
            p.get("gameDir").and_then(Value::as_str).is_some_and(|dir| dir.trim_end_matches(['\\', '/']).to_lowercase() == wanted)
        })
    });
    LauncherProfile {
        java: profile.and_then(|p| p.get("javaDir")).and_then(Value::as_str).map(PathBuf::from),
        java_args: profile.and_then(|p| p.get("javaArgs")).and_then(Value::as_str).map(String::from),
    }
}

fn console_java(path: PathBuf) -> PathBuf {
    let console = path.with_file_name(if cfg!(windows) { "java.exe" } else { "java" });
    if console.exists() { console } else { path }
}

pub fn java(settings: &Settings) -> Result<PathBuf> {
    match settings.pack_java(&settings.instance_dir) {
        Some(java) => Ok(java.clone()),
        None => app_java(settings),
    }
}

/// Returns the Java the selected pack runs on when Pack settings doesn't choose one.
pub fn app_java(settings: &Settings) -> Result<PathBuf> {
    let (component, major) = required_java(settings);
    if let Some(java) = settings.java_by_major.get(&major) {
        return Ok(java.clone());
    }
    if let Some(java) = launcher_profile(&settings.install_dir, &settings.instance_dir).java.filter(|p| p.exists()) {
        return Ok(console_java(java));
    }
    let platform = if cfg!(windows) { "windows-x64" } else if cfg!(target_arch = "aarch64") { "mac-os-arm64" } else { "mac-os" };
    // Current CurseForge installs runtimes to java/<component>; older versions used the Mojang launcher's runtime layout.
    let roots = [
        settings.install_dir.join("java").join(&component),
        settings.install_dir.join("runtime").join(&component).join(platform).join(&component),
    ];
    let executable = if cfg!(windows) { "java.exe" } else { "java" };
    let candidates = roots.iter().flat_map(|root| {
        [root.join("bin").join(executable), root.join("jre.bundle").join("Contents").join("Home").join("bin").join(executable)]
    });
    for java in candidates {
        if java.exists() {
            return Ok(java);
        }
    }
    if let Some(java) = crate::java::automatic(&crate::java::detect(&settings.install_dir), major) {
        return Ok(java.path.clone());
    }
    anyhow::bail!("This pack needs Java {major}, and packraven didn't find one on this computer. Download or choose a Java {major} under App settings, or launch the pack once from CurseForge so it installs one.")
}

pub fn required_java(settings: &Settings) -> (String, u32) {
    let minecraft = Instance::read(&settings.instance_dir).ok().and_then(|i| i.minecraft).unwrap_or_default();
    let declared = read_version(&settings.install_dir, &minecraft).ok().and_then(|version| {
        let java = version.get("javaVersion")?;
        Some((java.get("component")?.as_str()?.to_string(), u32::try_from(java.get("majorVersion")?.as_u64()?).ok()?))
    });
    // CurseForge only has the version file once someone launches the pack, so we fall back to what Mojang ships.
    declared.unwrap_or_else(|| {
        let (component, major) = java_for_minecraft(&minecraft);
        (component.to_string(), major)
    })
}

pub fn java_for_minecraft(minecraft: &str) -> (&'static str, u32) {
    let numbers: Vec<u32> = minecraft.split('.').map_while(|part| part.parse().ok()).collect();
    match numbers.as_slice() {
        [1, minor, ..] if *minor < 17 => ("jre-legacy", 8),
        [1, 17, ..] => ("java-runtime-alpha", 16),
        [1, 18 | 19, ..] | [1, 20] => ("java-runtime-gamma", 17),
        [1, 20, patch, ..] if *patch < 5 => ("java-runtime-gamma", 17),
        [1, ..] => ("java-runtime-delta", 21),
        _ => ("java-runtime-epsilon", 25),
    }
}

fn raw_args<'a>(version: &'a Value, kind: &str, features: &HashMap<&str, bool>) -> Vec<&'a str> {
    let mut out = Vec::new();
    for arg in version.pointer(&format!("/arguments/{kind}")).and_then(Value::as_array).into_iter().flatten() {
        match arg {
            Value::String(s) => out.push(s.as_str()),
            Value::Object(_) if allowed(arg.get("rules"), features) => match arg.get("value") {
                Some(Value::String(s)) => out.push(s.as_str()),
                Some(Value::Array(list)) => out.extend(list.iter().filter_map(Value::as_str)),
                _ => {}
            },
            _ => {}
        }
    }
    out
}

// Forge and NeoForge list ${version_name}.jar in -DignoreList so BootstrapLauncher discards the
// vanilla client jar that Mojang's launcher unconditionally puts on -cp. When CurseForge hasn't
// launched a version yet, versions/<id>/<id>.jar doesn't exist; falling back to <parent_id>.jar
// bypasses -DignoreList and collides with NeoForge's minecraft module.
fn ignores_version_jar(version: &Value, id: &str, features: &HashMap<&str, bool>) -> bool {
    let jar_name = format!("{id}.jar");
    raw_args(version, "jvm", features).into_iter().any(|arg| {
        let Some(list) = arg.strip_prefix("-DignoreList=") else { return false };
        list.replace("${version_name}", id).split(',').any(|entry| !entry.is_empty() && jar_name.starts_with(entry))
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn picks_mojangs_java_for_each_minecraft_version() {
        assert_eq!(java_for_minecraft("1.16.5").1, 8);
        assert_eq!(java_for_minecraft("1.17.1").1, 16);
        assert_eq!(java_for_minecraft("1.20").1, 17);
        assert_eq!(java_for_minecraft("1.20.1").1, 17);
        assert_eq!(java_for_minecraft("1.20.4").1, 17);
        assert_eq!(java_for_minecraft("1.20.5").1, 21);
        assert_eq!(java_for_minecraft("1.21.1").1, 21);
        assert_eq!(java_for_minecraft("26.1.2").1, 25);
    }

    #[test]
    fn detects_when_bootstrap_launcher_ignores_version_jar() {
        let features = HashMap::new();
        let neoforge_21 = json!({
            "arguments": { "jvm": ["-DignoreList=client-extra,${version_name}.jar"] }
        });
        let forge_20 = json!({
            "arguments": { "jvm": ["-DignoreList=bootstraplauncher,forge-,${version_name}.jar,forge-47.4.20"] }
        });
        let neoforge_26 = json!({
            "arguments": { "jvm": ["-DlibraryDirectory=${library_directory}"] }
        });
        let other_ignore = json!({
            "arguments": { "jvm": ["-DignoreList=asm,securejarhandler"] }
        });
        let fabric = json!({ "arguments": { "jvm": ["-DFabricMcEmu= net.minecraft.client.main.Main "] } });

        assert!(ignores_version_jar(&neoforge_21, "neoforge-21.1.266", &features));
        assert!(ignores_version_jar(&forge_20, "forge-47.4.20", &features));
        assert!(!ignores_version_jar(&neoforge_26, "neoforge-26.1.2.12-beta", &features));
        assert!(!ignores_version_jar(&other_ignore, "neoforge-21.1.266", &features));
        assert!(!ignores_version_jar(&fabric, "fabric-loader-0.20.1-1.21.1", &features));
    }
}

// A CurseForge profile's own Java arguments replace our memory setting, as they do when CurseForge launches it.
fn memory_args(java_args: Option<String>, pack: &Pack) -> Vec<String> {
    match java_args {
        Some(java_args) => split_args(&java_args),
        None => vec![format!("-Xmx{}", pack.config.test.client_memory)],
    }
}

pub fn client_heap_args(settings: &Settings, pack: &Pack) -> Vec<String> {
    let mut args = memory_args(launcher_profile(&settings.install_dir, &settings.instance_dir).java_args, pack);
    args.extend(settings.combined_run_args(&settings.instance_dir).client_jvm());
    args
}

/// Launches the selected pack's instance with its game files read from `game_dir`, which can be a copy of the instance.
pub fn client(settings: &Settings, pack: &Pack, player: &Player, server: Option<&str>, game_dir: &Path, natives_dir: &Path) -> Result<ClientLaunch> {
    let install = &settings.install_dir;
    let id = Instance::read(&settings.instance_dir)?.version_id.context("this profile has no mod loader; choose one in the CurseForge app")?;
    let child = read_version(install, &id)?;
    let parent_id = child.get("inheritsFrom").and_then(Value::as_str).unwrap_or(&id).to_string();
    let parent = if parent_id == id { child.clone() } else { read_version(install, &parent_id)? };

    let features: HashMap<&str, bool> = [("is_quick_play_multiplayer", server.is_some())].into_iter().collect();
    let libraries_dir = install.join("libraries");
    let mut order: Vec<String> = Vec::new();
    let mut libraries: HashMap<String, PathBuf> = HashMap::new();
    for version in [&child, &parent] {
        for library in version.get("libraries").and_then(Value::as_array).into_iter().flatten() {
            if !allowed(library.get("rules"), &features) {
                continue;
            }
            let Some(name) = library.get("name").and_then(Value::as_str) else { continue };
            let Some((key, derived)) = maven_path(name) else { continue };
            if libraries.contains_key(&key) {
                continue;
            }
            let relative = library.pointer("/downloads/artifact/path").and_then(Value::as_str).map(String::from).unwrap_or(derived);
            order.push(key.clone());
            libraries.insert(key, libraries_dir.join(relative));
        }
    }
    let separator = if cfg!(windows) { ";" } else { ":" };
    let mut classpath: Vec<String> = order.iter().map(|k| libraries[k].to_string_lossy().into_owned()).collect();
    if !ignores_version_jar(&child, &id, &features) {
        let version_jar = install.join("versions").join(&id).join(format!("{id}.jar"));
        let jar = if version_jar.exists() { version_jar } else { install.join("versions").join(&parent_id).join(format!("{parent_id}.jar")) };
        classpath.push(jar.to_string_lossy().into_owned());
    }

    let assets_index = parent.get("assets").and_then(Value::as_str).unwrap_or_default().to_string();
    let values: HashMap<&str, String> = [
        ("auth_player_name", player.name.clone()),
        ("version_name", id.clone()),
        ("game_directory", game_dir.to_string_lossy().into_owned()),
        ("assets_root", install.join("assets").to_string_lossy().into_owned()),
        ("assets_index_name", assets_index),
        ("auth_uuid", player.uuid.clone()),
        ("auth_access_token", player.access_token.clone()),
        // Vanilla only reads these two for telemetry.
        ("clientid", "0".into()),
        ("auth_xuid", "0".into()),
        ("user_type", "msa".into()),
        ("version_type", "release".into()),
        ("quickPlayMultiplayer", server.unwrap_or_default().to_string()),
        ("natives_directory", natives_dir.to_string_lossy().into_owned()),
        ("launcher_name", "packraven".into()),
        ("launcher_version", env!("CARGO_PKG_VERSION").into()),
        ("classpath", classpath.join(separator)),
        ("library_directory", libraries_dir.to_string_lossy().into_owned()),
        ("classpath_separator", separator.into()),
    ]
    .into_iter()
    .collect();
    let substitute = |arg: &str| {
        let mut out = arg.to_string();
        for (key, value) in &values {
            out = out.replace(&format!("${{{key}}}"), value);
        }
        out
    };
    let collect = |kind: &str, out: &mut Vec<String>| {
        for version in [&parent, &child] {
            out.extend(raw_args(version, kind, &features).into_iter().map(substitute));
        }
    };

    let profile = launcher_profile(install, &settings.instance_dir);
    let run_args = settings.combined_run_args(&settings.instance_dir);
    let mut args: Vec<String> = Vec::new();
    collect("jvm", &mut args);
    if crate::java::needs_java4shell_flag(required_java(settings).1) {
        args.push(crate::java::JAVA4SHELL_FLAG.into());
    }
    args.extend(memory_args(profile.java_args, pack));
    args.extend(run_args.client_jvm());
    if let Some(logging) = parent.pointer("/logging/client") {
        let file = logging.pointer("/file/id").and_then(Value::as_str).unwrap_or_default();
        let config = install.join("assets").join("log_configs").join(file);
        if let (true, Some(argument)) = (config.exists(), logging.get("argument").and_then(Value::as_str)) {
            args.push(argument.replace("${path}", &config.to_string_lossy()));
        }
    }
    args.push(child.get("mainClass").or(parent.get("mainClass")).and_then(Value::as_str).context("version has no main class")?.to_string());
    collect("game", &mut args);
    args.extend(run_args.client());
    Ok(ClientLaunch { java: java(settings)?, args })
}
