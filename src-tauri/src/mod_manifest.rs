use std::io::{Read, Seek};
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// The mod IDs a jar declares and the ones it can't load without, read from its loader metadata.
#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModManifest {
    pub ids: Vec<String>,
    // Libraries bundled inside the jar satisfy dependencies, but if a server mod needs one we'd
    // rather ship the library's own jar than this whole mod, so we keep them apart from `ids`.
    pub bundled_ids: Vec<String>,
    pub requires: Vec<String>,
    // Fabric won't load a jar whose fabric.mod.json says `"environment": "client"` on a server.
    #[serde(default)]
    pub client_only: bool,
}

// Fabric nests libraries in META-INF/jars, (Neo)Forge in META-INF/jarjar, and some of those
// libraries nest their own.
const NESTED_JAR_DEPTH: usize = 2;

pub fn read(path: &Path) -> Result<ModManifest> {
    read_jar(std::fs::File::open(path)?)
}

fn read_jar<R: Read + Seek>(jar: R) -> Result<ModManifest> {
    let mut manifest = ModManifest::default();
    read_into(&mut zip::ZipArchive::new(jar)?, 0, &mut manifest)?;
    for list in [&mut manifest.ids, &mut manifest.bundled_ids, &mut manifest.requires] {
        list.sort();
        list.dedup();
    }
    Ok(manifest)
}

fn read_into<R: Read + Seek>(archive: &mut zip::ZipArchive<R>, depth: usize, manifest: &mut ModManifest) -> Result<()> {
    let mut ids = Vec::new();
    if let Some(fabric) = text_of(archive, "fabric.mod.json").and_then(|text| serde_json::from_str::<FabricMod>(&text).ok()) {
        manifest.client_only |= depth == 0 && fabric.environment.as_deref() == Some("client");
        ids.push(fabric.id);
        ids.extend(fabric.provides);
        // We only follow `depends`: Fabric loads a mod without its `recommends` and `suggests`.
        manifest.requires.extend(fabric.depends.into_keys());
    }
    for name in ["META-INF/neoforge.mods.toml", "META-INF/mods.toml"] {
        if let Some(text) = text_of(archive, name) {
            read_forge(&text, &mut ids, &mut manifest.requires);
        }
    }
    if depth == 0 { &mut manifest.ids } else { &mut manifest.bundled_ids }.extend(ids);
    if depth >= NESTED_JAR_DEPTH {
        return Ok(());
    }
    let nested: Vec<String> = archive.file_names().filter_map(Result::ok).filter(|name| (name.starts_with("META-INF/jars/") || name.starts_with("META-INF/jarjar/")) && name.ends_with(".jar")).map(|name| name.into_owned()).collect();
    for name in nested {
        let mut bytes = Vec::new();
        archive.by_name(&name)?.read_to_end(&mut bytes)?;
        if let Ok(mut inner) = zip::ZipArchive::new(std::io::Cursor::new(bytes)) {
            read_into(&mut inner, depth + 1, manifest)?;
        }
    }
    Ok(())
}

fn text_of<R: Read + Seek>(archive: &mut zip::ZipArchive<R>, name: &str) -> Option<String> {
    let mut text = String::new();
    archive.by_name(name).ok()?.read_to_string(&mut text).ok()?;
    Some(text.trim_start_matches('\u{feff}').to_string())
}

#[derive(Deserialize)]
struct FabricMod {
    id: String,
    #[serde(default)]
    provides: Vec<String>,
    #[serde(default)]
    depends: std::collections::BTreeMap<String, serde_json::Value>,
    environment: Option<String>,
}

fn tables(value: Option<&toml::Value>) -> Vec<&toml::Table> {
    value.and_then(toml::Value::as_array).into_iter().flatten().filter_map(toml::Value::as_table).collect()
}

// Forge marks a required dependency with `mandatory = true`, NeoForge with `type = "required"`
// (its default). A dependency with side CLIENT only applies on the client.
fn read_forge(text: &str, ids: &mut Vec<String>, requires: &mut Vec<String>) {
    let Ok(toml) = text.parse::<toml::Table>() else { return };
    let string = |table: &toml::Table, key: &str| table.get(key).and_then(toml::Value::as_str).map(str::to_string);
    ids.extend(tables(toml.get("mods")).into_iter().filter_map(|m| string(m, "modId")));
    let Some(dependencies) = toml.get("dependencies").and_then(toml::Value::as_table) else { return };
    for dependency in dependencies.values().flat_map(|list| tables(Some(list))) {
        let required = match (string(dependency, "type"), dependency.get("mandatory").and_then(toml::Value::as_bool)) {
            (Some(kind), _) => kind.eq_ignore_ascii_case("required"),
            (None, Some(mandatory)) => mandatory,
            (None, None) => true,
        };
        let on_server = !string(dependency, "side").is_some_and(|side| side.eq_ignore_ascii_case("CLIENT"));
        if let (true, true, Some(id)) = (required, on_server, string(dependency, "modId")) {
            requires.push(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn jar(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, bytes) in files {
            zip.start_file(*name, SimpleFileOptions::default()).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    fn read_bytes(bytes: Vec<u8>) -> ModManifest {
        read_jar(std::io::Cursor::new(bytes)).unwrap()
    }

    #[test]
    fn fabric_mods_require_only_their_depends() {
        let manifest = read_bytes(jar(&[(
            "fabric.mod.json",
            br#"{"id": "forgeconfigscreens", "provides": ["fcs"], "depends": {"fabricloader": ">=0.14", "modmenu": "*"}, "recommends": {"jei": "*"}}"#,
        )]));
        assert_eq!(manifest.ids, ["fcs", "forgeconfigscreens"]);
        assert_eq!(manifest.requires, ["fabricloader", "modmenu"]);
        assert!(!manifest.client_only);
        assert!(read_bytes(jar(&[("fabric.mod.json", br#"{"id": "modmenu", "environment": "client"}"#)])).client_only);
    }

    #[test]
    fn forge_mods_skip_optional_and_client_dependencies() {
        let toml = br#"
            [[mods]]
            modId = "chipped"
            [[dependencies.chipped]]
            modId = "athena"
            mandatory = true
            side = "BOTH"
            [[dependencies.chipped]]
            modId = "jei"
            mandatory = false
            side = "BOTH"
            [[dependencies.chipped]]
            modId = "configured"
            type = "required"
            side = "CLIENT"
            [[dependencies.chipped]]
            modId = "resourcefullib"
            side = "BOTH"
        "#;
        let manifest = read_bytes(jar(&[("META-INF/neoforge.mods.toml", toml)]));
        assert_eq!(manifest.ids, ["chipped"]);
        assert_eq!(manifest.requires, ["athena", "resourcefullib"]);
    }

    #[test]
    fn bundled_libraries_count_apart_from_the_mod() {
        let library = jar(&[("fabric.mod.json", br#"{"id": "mixinextras", "depends": {"fabricloader": "*"}}"#)]);
        let manifest = read_bytes(jar(&[("fabric.mod.json", br#"{"id": "enderscape", "depends": {"trimpatcher": "*"}}"#), ("META-INF/jars/mixinextras.jar", &library)]));
        assert_eq!(manifest.ids, ["enderscape"]);
        assert_eq!(manifest.bundled_ids, ["mixinextras"]);
        assert_eq!(manifest.requires, ["fabricloader", "trimpatcher"]);
    }
}
