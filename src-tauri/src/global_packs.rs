use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::pack::Lockfile;

// We read a folder from a jar into memory before copying it, so we cap its total size.
const COPY_LIMIT: u64 = 256 * 1024 * 1024;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum PackKind {
    Data,
    Resource,
}

impl PackKind {
    /// Returns the kind of pack that can override `inside`, a path inside a mod's jar.
    pub fn of(inside: &str) -> Option<Self> {
        let mut parts = inside.split('/').filter(|p| !p.is_empty());
        let kind = match parts.next()? {
            "data" => PackKind::Data,
            "assets" => PackKind::Resource,
            _ => return None,
        };
        parts.next().map(|_| kind)
    }

    fn noun(self) -> &'static str {
        match self {
            PackKind::Data => "data pack",
            PackKind::Resource => "resource pack",
        }
    }
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PackEntry {
    pub path: String,
    pub name: String,
    pub zipped: bool,
    /// Whether options.txt turns on a pack in the instance's resourcepacks folder. None
    /// elsewhere, and when there's no options.txt to tell.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PackFolder {
    pub path: String,
    pub exists: bool,
    // Global Packs can list a single pack instead of a folder of packs, and we can't add a new
    // pack to one of those.
    pub can_create: bool,
    pub packs: Vec<PackEntry>,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Loader {
    pub id: &'static str,
    pub name: &'static str,
    pub installed: bool,
    pub project_ids: &'static [u64],
    pub folders: Vec<PackFolder>,
}

struct Known {
    id: &'static str,
    name: &'static str,
    project_ids: &'static [u64],
    jar_prefixes: &'static [&'static str],
}

// CurseForge splits Paxi into a Fabric project, a Forge/NeoForge one, and a NeoForge one for
// 1.20.4 to 1.21.1.
const KNOWN: [Known; 4] = [
    Known { id: "paxi", name: "Paxi", project_ids: &[418881, 515708, 1015157], jar_prefixes: &["paxi-"] },
    Known { id: "openloader", name: "Open Loader", project_ids: &[354339], jar_prefixes: &["openloader", "open-loader", "open_loader"] },
    Known { id: "globalpacks", name: "Global Packs", project_ids: &[317134], jar_prefixes: &["globalpacks", "global-packs", "global_packs"] },
    Known { id: "kubejs", name: "KubeJS", project_ids: &[crate::pack::KUBEJS_PROJECT_ID], jar_prefixes: &["kubejs-", "kubejs_"] },
];

pub fn is_kubejs_pack(pack: &str) -> bool {
    normalize(pack) == "kubejs"
}

fn installed(root: &Path, jars: &[String], lock: Option<&Lockfile>, known: &Known) -> bool {
    if known.id == "kubejs" && root.join("kubejs").is_dir() {
        return true;
    }
    let by_project = lock.is_some_and(|lock| lock.files.iter().any(|e| e.curseforge_project().is_some_and(|id| known.project_ids.contains(&id)) && !e.path.ends_with(".disabled")));
    by_project || jars.iter().any(|name| known.jar_prefixes.iter().any(|p| name.starts_with(p)))
}

/// Returns the pack loaders we know, with the folders each loads `kind` packs from.
pub fn loaders(root: &Path, lock: Option<&Lockfile>, kind: PackKind) -> Vec<Loader> {
    let jars: Vec<String> = std::fs::read_dir(root.join("mods"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_lowercase())
        .filter(|name| name.ends_with(".jar"))
        .collect();
    KNOWN
        .iter()
        .map(|known| {
            let folders = if known.id == "kubejs" {
                let target = match kind {
                    PackKind::Data => "kubejs/data",
                    PackKind::Resource => "kubejs/assets",
                };
                vec![PackFolder {
                    path: target.into(),
                    exists: root.join("kubejs").is_dir(),
                    can_create: false,
                    packs: vec![PackEntry { path: "kubejs".into(), name: "KubeJS".into(), zipped: false, enabled: None }],
                }]
            } else {
                let paths = match known.id {
                    "paxi" => paxi_folders(root, kind),
                    "openloader" => open_loader_folders(root, kind),
                    _ => global_packs_folders(root, kind),
                };
                let mut seen = std::collections::HashSet::new();
                paths.into_iter().filter(|f| seen.insert(f.to_lowercase())).filter_map(|f| folder(root, &f).ok()).collect()
            };
            Loader { id: known.id, name: known.name, installed: installed(root, &jars, lock, known), project_ids: known.project_ids, folders }
        })
        .collect()
}

fn paxi_folders(root: &Path, kind: PackKind) -> Vec<String> {
    match kind {
        PackKind::Resource => vec!["config/paxi/resourcepacks".into()],
        PackKind::Data => {
            let mut folders = vec!["config/paxi/datapacks".to_string()];
            if paxi_loads_base_datapacks(root) {
                folders.push("datapacks".into());
            }
            folders
        }
    }
}

// Paxi 5.1.3 and later can also load the instance's datapacks folder, which CurseForge's own
// data pack installs use. The key is spelled differently on Fabric and on NeoForge.
fn paxi_loads_base_datapacks(root: &Path) -> bool {
    let pattern = regex::Regex::new(r#"(?m)^\s*(loadFromBaseDatapacksDirectory|"Load from base 'datapacks' directory")\s*=\s*true\b"#).unwrap();
    std::fs::read_dir(root.join("config")).into_iter().flatten().flatten().any(|entry| {
        let name = entry.file_name().to_string_lossy().to_lowercase();
        name.starts_with("paxi-") && name.ends_with(".toml") && std::fs::read_to_string(entry.path()).is_ok_and(|text| pattern.is_match(&text))
    })
}

// Open Loader moved its folders under config in 1.17, so a folder at the instance root only
// matters when an older pack still has one.
fn open_loader_folders(root: &Path, kind: PackKind) -> Vec<String> {
    let (leaf, section) = match kind {
        PackKind::Data => ("data", "dataPacks"),
        PackKind::Resource => ("resources", "resourcePacks"),
    };
    let current = format!("config/openloader/{leaf}");
    let legacy = format!("openloader/{leaf}");
    let mut folders = vec![if !root.join(&current).exists() && root.join(&legacy).is_dir() { legacy } else { current }];
    let options = std::fs::read_to_string(root.join("config/openloader/advanced_options.json")).ok().and_then(|text| serde_json::from_str::<Value>(&text).ok());
    if let Some(extra) = options.as_ref().and_then(|o| o.pointer(&format!("/{section}/additionalFolders"))).and_then(Value::as_array) {
        folders.extend(extra.iter().filter_map(Value::as_str).map(normalize));
    }
    folders
}

fn global_packs_folders(root: &Path, kind: PackKind) -> Vec<String> {
    let section = match kind {
        PackKind::Data => "datapacks",
        PackKind::Resource => "resourcepacks",
    };
    let config = std::fs::read_to_string(root.join("config/global_packs.toml")).ok().and_then(|text| text.parse::<toml::Table>().ok());
    let Some(config) = config else {
        // Global Packs writes its config on first launch; these are the folders it lists then.
        return match kind {
            PackKind::Data => vec!["global_packs/required_data".into(), "global_packs/optional_data".into()],
            PackKind::Resource => vec!["global_packs/required_resources".into()],
        };
    };
    let lists = config.get(section).and_then(|s| s.as_table());
    let mut folders: Vec<String> = ["required", "optional"]
        .iter()
        .filter_map(|key| lists.and_then(|l| l.get(*key)).and_then(|v| v.as_array()))
        .flatten()
        .filter_map(|v| v.as_str())
        .map(normalize)
        .collect();
    // Its default data list starts with the vanilla datapacks and resourcepacks folders, but a
    // folder of its own is the better home for a new pack, so we put those first.
    folders.sort_by_key(|f| !f.starts_with("global_packs/"));
    folders
}

fn normalize(path: &str) -> String {
    path.replace('\\', "/").trim_matches('/').to_string()
}

fn is_pack_dir(path: &Path) -> bool {
    path.join("pack.mcmeta").is_file()
}

/// The folder Minecraft itself lists resource packs from, in Options, Resource Packs.
pub const VANILLA_RESOURCE_PACKS: &str = "resourcepacks";

// Minecraft saves the packs you turn on as "file/<name>" in options.txt and leaves out the
// rest, so a pack missing from the list is off.
fn enabled_resource_packs(root: &Path) -> Option<std::collections::HashSet<String>> {
    let text = std::fs::read_to_string(root.join("options.txt")).ok()?;
    let list = text.lines().find_map(|line| line.strip_prefix("resourcePacks:"))?;
    let packs: Vec<String> = serde_json::from_str(list.trim()).ok()?;
    Some(packs.iter().filter_map(|p| p.strip_prefix("file/")).map(String::from).collect())
}

/// Lists the packs in a pack-relative folder, or the folder itself when it's a single pack.
pub fn folder(root: &Path, relative: &str) -> Result<PackFolder> {
    let relative = normalize(relative);
    if relative.is_empty() {
        bail!("Pick a folder inside the pack");
    }
    let dir = crate::files::resolve(root, &relative)?;
    let enabled_packs = enabled_resource_packs(root);
    let pack_entry = |path: String, zipped: bool| {
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", &path));
        let enabled = (parent == VANILLA_RESOURCE_PACKS).then(|| enabled_packs.as_ref().map(|on| on.contains(name))).flatten();
        PackEntry { name: name.to_string(), path: path.clone(), zipped, enabled }
    };
    if dir.is_file() || is_pack_dir(&dir) {
        return Ok(PackFolder { path: relative.clone(), exists: true, can_create: false, packs: vec![pack_entry(relative, dir.is_file())] });
    }
    let mut packs: Vec<PackEntry> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let file_name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            let zipped = path.is_file() && file_name.to_lowercase().ends_with(".zip");
            (zipped || is_pack_dir(&path)).then(|| pack_entry(format!("{relative}/{file_name}"), zipped))
        })
        .collect();
    packs.sort_by_key(|p| p.name.to_lowercase());
    Ok(PackFolder { path: relative, exists: dir.is_dir(), can_create: true, packs })
}

/// Builds pack.mcmeta for a new pack from the pack format in Minecraft's own version.json.
pub fn pack_meta(install: &Path, minecraft: &str, kind: PackKind, description: &str) -> Result<String> {
    let jar = install.join("versions").join(minecraft).join(format!("{minecraft}.jar"));
    if !jar.is_file() {
        bail!("We couldn't find Minecraft {minecraft} in {}, so we don't know which pack format to write. Launch the pack once from CurseForge, then try again.", install.display());
    }
    let version: Value = serde_json::from_slice(&crate::archive::read(&jar, "version.json", 1024 * 1024)?).context("reading Minecraft's version.json")?;
    let formats = version.get("pack_version").context("Minecraft's version.json has no pack_version")?;
    let key = match kind {
        PackKind::Data => "data",
        PackKind::Resource => "resource",
    };
    let format = |name: &str| formats.get(name).and_then(Value::as_u64);
    let mut pack = json!({ "description": description });
    // Minecraft 1.21.9 replaced pack_format with major and minor versions, written as a range.
    if let Some(major) = format(&format!("{key}_major")) {
        let minor = format(&format!("{key}_minor")).unwrap_or(0);
        pack["min_format"] = if minor == 0 { json!(major) } else { json!([major, minor]) };
        pack["max_format"] = json!(major);
    } else {
        pack["pack_format"] = json!(format(key).context("Minecraft's version.json has no pack format")?);
    }
    Ok(serde_json::to_string_pretty(&json!({ "pack": pack }))? + "\n")
}

#[derive(Serialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Copied {
    /// The pack-relative paths of the copies of what you picked.
    pub paths: Vec<String>,
    pub created: Vec<String>,
    pub existing: usize,
}

/// Returns the path inside the innermost jar, which is also the path a pack uses to override it.
pub fn inside_path(relative: &str) -> Option<&str> {
    let parts: Vec<&str> = relative.split('/').collect();
    let last = parts.iter().rposition(|p| crate::archive::is_archive(p))?;
    let offset: usize = parts[..=last].iter().map(|p| p.len() + 1).sum();
    relative.get(offset..)
}

/// Copies files or folders from inside mod jars into a pack, never replacing a file that's
/// already there. With `meta`, writes it as the pack's pack.mcmeta when the pack has none.
pub fn copy_into_pack(root: &Path, sources: &[impl AsRef<str>], pack: &str, meta: Option<&str>) -> Result<Copied> {
    let first = sources.first().context("Pick something in a mod's jar to copy into a pack")?.as_ref();
    let first_inside = inside_path(first).context("Only files inside a mod's jar can be copied into a pack")?;
    let kind = PackKind::of(first_inside).context("Only files under data/ or assets/ belong in a pack")?;
    let mut items = Vec::with_capacity(sources.len());
    for source in sources {
        let source = source.as_ref();
        let (file, inner) = crate::archive::split(root, source).filter(|(_, inner)| !inner.is_empty()).context("Only files inside a mod's jar can be copied into a pack")?;
        let inside = inside_path(source).context("Only files inside a mod's jar can be copied into a pack")?;
        let item_kind = PackKind::of(inside).context("Only files under data/ or assets/ belong in a pack")?;
        if item_kind != kind {
            bail!("Data pack files and resource pack files can't go in the same pack");
        }
        items.push((file, inner, inside));
    }
    let pack = normalize(pack);
    let kubejs = is_kubejs_pack(&pack);
    let pack_dir = crate::files::resolve(root, &pack)?;
    if pack_dir.is_file() {
        bail!("{pack} is a zipped pack. Extract it to add files to it.");
    }
    if !kubejs && !is_pack_dir(&pack_dir) && meta.is_none() {
        bail!("{pack} isn't a {}: it has no pack.mcmeta", kind.noun());
    }
    let mut entries = Vec::new();
    let mut paths = Vec::with_capacity(items.len());
    let mut budget = COPY_LIMIT;
    for (file, inner, inside) in items {
        let tree = crate::archive::read_tree(&file, &inner, budget)?;
        let used: u64 = tree.iter().map(|(_, bytes)| bytes.len() as u64).sum();
        budget = budget.saturating_sub(used);
        entries.extend(tree);
        paths.push(format!("{pack}/{inside}"));
    }
    let mut created = Vec::new();
    if !kubejs && !is_pack_dir(&pack_dir) {
        let Some(meta) = meta else { bail!("{pack} isn't a {}: it has no pack.mcmeta", kind.noun()) };
        let fresh = !pack_dir.exists();
        std::fs::create_dir_all(&pack_dir).with_context(|| format!("creating {pack}"))?;
        std::fs::write(pack_dir.join("pack.mcmeta"), meta).context("writing pack.mcmeta")?;
        created.push(if fresh { pack.clone() } else { format!("{pack}/pack.mcmeta") });
    }
    let fresh_pack = created.first() == Some(&pack);
    let mut existing = 0;
    let mut written = std::collections::HashSet::new();
    for (name, bytes) in entries {
        let target = crate::files::resolve(&pack_dir, &name)?;
        if !written.insert(target.clone()) {
            continue;
        }
        if target.exists() {
            existing += 1;
            continue;
        }
        std::fs::create_dir_all(target.parent().context("a pack file has no folder")?)?;
        std::fs::write(&target, bytes).with_context(|| format!("writing {name}"))?;
        if !fresh_pack {
            created.push(format!("{pack}/{name}"));
        }
    }
    Ok(Copied { paths, created, existing })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    fn jar(path: &Path, entries: &[(&str, &[u8])]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut zip = ZipWriter::new(File::create(path).unwrap());
        for (name, bytes) in entries {
            zip.start_file(*name, SimpleFileOptions::default()).unwrap();
            std::io::Write::write_all(&mut zip, bytes).unwrap();
        }
        zip.finish().unwrap();
    }

    fn fresh(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn tells_data_from_resources() {
        assert_eq!(PackKind::of("data/example/recipe/a.json"), Some(PackKind::Data));
        assert_eq!(PackKind::of("assets/example"), Some(PackKind::Resource));
        assert_eq!(PackKind::of("data"), None);
        assert_eq!(PackKind::of("com/example/A.class"), None);
        assert_eq!(inside_path("mods/a.jar/META-INF/jars/b.jar/data/b/x.json"), Some("data/b/x.json"));
        assert_eq!(inside_path("config/a.json"), None);
    }

    #[test]
    fn reads_each_loaders_folders() {
        let root = fresh("packraven-global-packs-test");
        jar(&root.join("mods/Paxi-1.21.1-NeoForge-5.1.3.jar"), &[("x", b"")]);
        std::fs::create_dir_all(root.join("config/paxi/datapacks/Mine")).unwrap();
        std::fs::write(root.join("config/paxi/datapacks/Mine/pack.mcmeta"), "{}").unwrap();
        std::fs::write(root.join("config/paxi/datapacks/Zipped.zip"), "").unwrap();
        std::fs::create_dir_all(root.join("config/paxi/datapacks/not a pack")).unwrap();
        std::fs::write(root.join("config/paxi-neoforge-1_21.toml"), "[Paxi.General]\n\t\t\"Load from base 'datapacks' directory\" = true\n").unwrap();
        std::fs::write(
            root.join("config/global_packs.toml"),
            "[resourcepacks]\n\trequired = [\"global_packs/required_resources/\"]\n[datapacks]\n\trequired = [\"datapacks/\", \"global_packs/required_data/\"]\n\toptional = [\"global_packs/optional_data/\"]\n",
        )
        .unwrap();

        let data = loaders(&root, None, PackKind::Data);
        let paxi = &data[0];
        assert!(paxi.installed);
        assert_eq!(paxi.folders.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["config/paxi/datapacks", "datapacks"]);
        let packs: Vec<_> = paxi.folders[0].packs.iter().map(|p| (p.name.as_str(), p.zipped)).collect();
        assert_eq!(packs, [("Mine", false), ("Zipped.zip", true)]);
        assert!(!data[1].installed);
        assert_eq!(data[1].folders[0].path, "config/openloader/data");
        assert_eq!(data[2].folders.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["global_packs/required_data", "global_packs/optional_data", "datapacks"]);
        assert_eq!(loaders(&root, None, PackKind::Resource)[2].folders[0].path, "global_packs/required_resources");

        std::fs::write(root.join("config/paxi-neoforge-1_21.toml"), "\"Load from base 'datapacks' directory\" = false\n").unwrap();
        assert_eq!(loaders(&root, None, PackKind::Data)[0].folders.len(), 1);
    }

    #[test]
    fn reads_which_resource_packs_options_turns_on() {
        let root = fresh("packraven-enabled-packs-test");
        for pack in ["resourcepacks/Raven's Resources", "resourcepacks/Off", "config/paxi/resourcepacks/Global"] {
            std::fs::create_dir_all(root.join(pack)).unwrap();
            std::fs::write(root.join(pack).join("pack.mcmeta"), "{}").unwrap();
        }
        std::fs::write(root.join("resourcepacks/Zipped.zip"), "").unwrap();
        let enabled = |folder: &PackFolder| folder.packs.iter().map(|p| (p.name.clone(), p.enabled)).collect::<Vec<_>>();
        assert_eq!(enabled(&folder(&root, "resourcepacks").unwrap()), [("Off".into(), None), ("Raven's Resources".into(), None), ("Zipped.zip".into(), None)]);

        let options = r#"version:3955
resourcePacks:["vanilla","file/Raven\u0027s Resources","file/Zipped.zip"]
lang:en_us
"#;
        std::fs::write(root.join("options.txt"), options).unwrap();
        assert_eq!(enabled(&folder(&root, "resourcepacks").unwrap()), [("Off".into(), Some(false)), ("Raven's Resources".into(), Some(true)), ("Zipped.zip".into(), Some(true))]);
        assert_eq!(folder(&root, "resourcepacks/Raven's Resources").unwrap().packs[0].enabled, Some(true));
        assert_eq!(folder(&root, "config/paxi/resourcepacks").unwrap().packs[0].enabled, None);
    }

    #[test]
    fn writes_the_pack_format_minecraft_expects() {
        let install = fresh("packraven-pack-meta-test");
        jar(&install.join("versions/1.21.1/1.21.1.jar"), &[("version.json", br#"{"pack_version": {"resource": 34, "data": 48}}"#)]);
        jar(&install.join("versions/26.1.2/26.1.2.jar"), &[("version.json", br#"{"pack_version": {"resource_major": 84, "resource_minor": 0, "data_major": 101, "data_minor": 1}}"#)]);
        let parse = |text: String| serde_json::from_str::<Value>(&text).unwrap()["pack"].clone();
        assert_eq!(parse(pack_meta(&install, "1.21.1", PackKind::Data, "d").unwrap())["pack_format"], 48);
        let new = parse(pack_meta(&install, "26.1.2", PackKind::Data, "d").unwrap());
        assert_eq!((new["min_format"].clone(), new["max_format"].clone()), (json!([101, 1]), json!(101)));
        assert_eq!(parse(pack_meta(&install, "26.1.2", PackKind::Resource, "r").unwrap())["min_format"], 84);
        assert!(pack_meta(&install, "1.20.1", PackKind::Data, "d").is_err());
    }

    #[test]
    fn copies_files_and_folders_without_replacing_any() {
        let root = fresh("packraven-copy-into-pack-test");
        jar(&root.join("mods/a.jar"), &[("data/a/recipe/one.json", b"1"), ("data/a/recipe/two.json", b"2"), ("assets/a/lang/en_us.json", b"{}")]);
        jar(&root.join("mods/b.jar"), &[("data/b/recipe/three.json", b"3")]);

        let copied = copy_into_pack(&root, &["mods/a.jar/data/a/recipe/one.json"], "datapacks/Mine", Some("{}")).unwrap();
        assert_eq!(copied, Copied { paths: vec!["datapacks/Mine/data/a/recipe/one.json".into()], created: vec!["datapacks/Mine".into()], existing: 0 });
        assert_eq!(std::fs::read_to_string(root.join("datapacks/Mine/pack.mcmeta")).unwrap(), "{}");

        std::fs::write(root.join("datapacks/Mine/data/a/recipe/one.json"), "edited").unwrap();
        let copied = copy_into_pack(&root, &["mods/a.jar/data/a/recipe", "mods/b.jar/data/b/recipe/three.json"], "datapacks/Mine", None).unwrap();
        assert_eq!(
            copied,
            Copied {
                paths: vec!["datapacks/Mine/data/a/recipe".into(), "datapacks/Mine/data/b/recipe/three.json".into()],
                created: vec!["datapacks/Mine/data/a/recipe/two.json".into(), "datapacks/Mine/data/b/recipe/three.json".into()],
                existing: 1,
            }
        );
        assert_eq!(std::fs::read_to_string(root.join("datapacks/Mine/data/a/recipe/one.json")).unwrap(), "edited");

        assert!(copy_into_pack(&root, &["mods/a.jar/data/a/recipe/one.json", "mods/a.jar/assets/a/lang/en_us.json"], "datapacks/Mine", None).is_err());
        assert!(copy_into_pack(&root, &["mods/a.jar/assets/a/lang/en_us.json"], "resourcepacks/None", None).is_err());
        assert!(copy_into_pack(&root, &["mods/a.jar/data/a/recipe/one.json"], "../outside", Some("{}")).is_err());
    }

    #[test]
    fn recognizes_kubejs_and_copies_without_pack_mcmeta() {
        let root = fresh("packraven-kubejs-pack-test");
        jar(&root.join("mods/a.jar"), &[("data/a/recipe/one.json", b"1"), ("assets/a/lang/en_us.json", b"{}")]);
        assert!(!loaders(&root, None, PackKind::Data).iter().find(|l| l.id == "kubejs").unwrap().installed);

        std::fs::create_dir_all(root.join("kubejs")).unwrap();
        let data_loader = loaders(&root, None, PackKind::Data).into_iter().find(|l| l.id == "kubejs").unwrap();
        assert!(data_loader.installed);
        assert_eq!(data_loader.folders[0].path, "kubejs/data");
        assert!(!data_loader.folders[0].can_create);
        assert_eq!(data_loader.folders[0].packs[0].path, "kubejs");

        let copied = copy_into_pack(&root, &["mods/a.jar/data/a/recipe/one.json"], "kubejs", None).unwrap();
        assert_eq!(copied.paths, ["kubejs/data/a/recipe/one.json"]);
        assert!(root.join("kubejs/data/a/recipe/one.json").is_file());
        assert!(!root.join("kubejs/pack.mcmeta").exists());

        let copied_assets = copy_into_pack(&root, &["mods/a.jar/assets/a/lang/en_us.json"], "kubejs", None).unwrap();
        assert_eq!(copied_assets.paths, ["kubejs/assets/a/lang/en_us.json"]);
        assert!(root.join("kubejs/assets/a/lang/en_us.json").is_file());
        assert!(!root.join("kubejs/pack.mcmeta").exists());
    }
}
