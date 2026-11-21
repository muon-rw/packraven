mod modules;
mod parser;
mod query;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Instant, UNIX_EPOCH};

use serde::Serialize;
use sha1::{Digest, Sha1};
use walkdir::WalkDir;

use modules::ModuleTable;
use parser::{AliasDef, GlobalSymbol, Member, MemberKind, Param, ParsedChunk, TypeDef, TypeKind};

#[derive(Serialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProbeStatus {
    pub available: bool,
    pub file_count: usize,
    pub total_bytes: u64,
    pub type_count: usize,
    pub string_entries: usize,
    pub load_ms: u64,
    pub failed_files: Vec<String>,
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CompletionItem {
    pub label: String,
    pub kind: &'static str,
    pub detail: Option<String>,
    pub signature: Option<String>,
    pub docs: Option<String>,
    pub insert_text: Option<String>,
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CompletionResult {
    pub from: usize,
    pub to: usize,
    pub items: Vec<CompletionItem>,
    pub in_string: bool,
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct HoverInfo {
    pub from: usize,
    pub to: usize,
    pub signature: String,
    pub docs: String,
}

pub struct ProbeIndex {
    pub(crate) types: HashMap<String, TypeDef>,
    pub(crate) globals: HashMap<String, GlobalSymbol>,
    pub(crate) aliases: HashMap<String, AliasDef>,
    pub(crate) modules: ModuleTable,
    pub(crate) event_sides: BTreeMap<String, Vec<String>>,
}

impl ProbeIndex {
    fn build(dump: ProbeDump) -> (ProbeStatus, Option<Self>) {
        let started = Instant::now();
        let entries = dump.files;
        if entries.is_empty() {
            return (ProbeStatus::default(), None);
        }

        let file_count = entries.len();
        let total_bytes: u64 = entries.values().map(|(_, len)| *len).sum();
        // Java classes repeat Object's methods and carry mixin members, so we drop those from
        // completions. KubeJS documents its own classes, so we keep their docs.
        let work: Vec<(u32, String, PathBuf, bool, bool)> = entries
            .into_iter()
            .enumerate()
            .map(|(i, (rel, (path, _)))| {
                let is_internal = rel.contains("/internals/") || rel.contains("/@package/") || rel.contains("/packages/");
                let keep_docs = !is_internal || rel.contains("/@package/dev/latvian/");
                (i as u32, rel, path, is_internal, keep_docs)
            })
            .collect();

        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(8).min(work.len().max(1));
        let chunk_size = work.len().div_ceil(threads);

        // We catch a parser panic per file, so one odd declaration costs only its own file.
        type Parsed = (Vec<ParsedChunk>, Vec<String>, BTreeMap<String, Vec<String>>);
        let parsed: Vec<Parsed> = std::thread::scope(|scope| {
            let mut handles = Vec::with_capacity(threads);
            for slice in work.chunks(chunk_size) {
                handles.push(scope.spawn(move || {
                    let mut files = Vec::with_capacity(slice.len());
                    let mut sides = BTreeMap::new();
                    let mut failed = Vec::new();
                    for (file_id, rel, path, is_internal, keep_docs) in slice {
                        let Ok(bytes) = std::fs::read(path) else {
                            failed.push(rel.clone());
                            continue;
                        };
                        let text = String::from_utf8_lossy(&bytes);
                        match std::panic::catch_unwind(|| parser::parse_dts(&text, *file_id, *is_internal, *keep_docs)) {
                            Ok(chunk) => {
                                record_side_only_events(&chunk, rel, &mut sides);
                                files.push(chunk);
                            }
                            Err(_) => failed.push(rel.clone()),
                        }
                    }
                    (files, failed, sides)
                }));
            }
            handles.into_iter().map(|h| h.join().expect("parser panics are caught per file")).collect()
        });
        let mut files = Vec::with_capacity(file_count);
        let mut failed_files = Vec::new();
        let mut side_only_events: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (chunks, failed, sides) in parsed {
            for (event, found) in sides {
                let known = side_only_events.entry(event).or_default();
                for side in found {
                    if !known.contains(&side) {
                        known.push(side);
                    }
                }
            }
            files.extend(chunks);
            failed_files.extend(failed);
        }

        // Each file's references depend on which names collide anywhere in the dump, so we
        // requalify only once every file is parsed.
        let modules = ModuleTable::build(&files);
        if modules.collision_count() > 0 {
            let per_thread = files.len().div_ceil(threads);
            std::thread::scope(|scope| {
                for slice in files.chunks_mut(per_thread.max(1)) {
                    let modules = &modules;
                    scope.spawn(move || {
                        for chunk in slice {
                            let names = modules.requalified_names(chunk);
                            modules::requalify(chunk, &names);
                        }
                    });
                }
            });
        }
        let mut merged = ParsedChunk::default();
        for chunk in files {
            chunk.merge_into(&mut merged);
        }
        modules.add_fallbacks(&mut merged);
        failed_files.sort_unstable();
        for file in &failed_files {
            eprintln!("probejs: couldn't read {file}, so its typings are missing from completions");
        }
        seed_js_builtins(&mut merged);
        let mut event_sides = event_sides(&merged);
        event_sides.extend(side_only_events);

        let type_count = merged.types.len();
        let string_entries = merged
            .aliases
            .values()
            .map(|a| match a {
                AliasDef::StringUnion(list) => list.len(),
                AliasDef::Refs { literals, .. } => literals.len(),
            })
            .sum();

        let status = ProbeStatus {
            available: true,
            file_count,
            total_bytes,
            type_count,
            string_entries,
            load_ms: started.elapsed().as_millis() as u64,
            failed_files,
        };
        (
            status,
            Some(Self {
                types: merged.types,
                globals: merged.globals,
                aliases: merged.aliases,
                modules,
                event_sides,
            }),
        )
    }

    pub fn complete(&self, text: &str, utf16_offset: usize) -> CompletionResult {
        query::complete(self, text, utf16_offset)
    }

    pub fn hover(&self, text: &str, utf16_offset: usize) -> Option<HoverInfo> {
        query::hover(self, text, utf16_offset)
    }

    pub fn event_sides(&self) -> &BTreeMap<String, Vec<String>> {
        &self.event_sides
    }
}

// ProbeJS 8 writes each script folder's events into its own `@side-only/<side>` folder, so an event
// group's functions there tell us which folders can hook them.
fn record_side_only_events(chunk: &ParsedChunk, rel: &str, sides: &mut BTreeMap<String, Vec<String>>) {
    let Some(side) = ["server", "client", "startup"].into_iter().find(|side| rel.contains(&format!("/@side-only/{side}/"))) else {
        return;
    };
    for (group, def) in &chunk.types {
        if def.kind != TypeKind::Namespace || group.contains('.') || !group.ends_with("Events") {
            continue;
        }
        for member in def.members.iter().filter(|member| member.kind == MemberKind::Method) {
            let known = sides.entry(format!("{group}.{}", member.name)).or_default();
            if !known.iter().any(|known| known == side) {
                known.push(side.to_string());
            }
        }
    }
}

// ProbeJS 6 tags each event with the script folders it fires in, as in `@at *server, client*`, so
// we can tell you when a script hooks an event its folder never sees. That covers every addon's
// events, not just the ones we know about.
fn event_sides(chunk: &ParsedChunk) -> BTreeMap<String, Vec<String>> {
    let mut sides = BTreeMap::new();
    for (group, global) in &chunk.globals {
        let Some(def) = chunk.types.get(&global.ty) else { continue };
        for member in &def.members {
            let Some(at) = member.docs.as_deref().and_then(|docs| docs.split("@at *").nth(1)).and_then(|rest| rest.split('*').next()) else {
                continue;
            };
            let list: Vec<String> = at.split(',').map(|side| side.trim().to_string()).filter(|side| matches!(side.as_str(), "server" | "client" | "startup")).collect();
            if !list.is_empty() {
                sides.insert(format!("{group}.{}", member.name), list);
            }
        }
    }
    sides
}

struct CachedProbe {
    root: PathBuf,
    fingerprint: String,
    status: ProbeStatus,
    index: Option<Arc<ProbeIndex>>,
}

#[derive(Default)]
pub struct ProbeCache {
    inner: Mutex<Option<CachedProbe>>,
}

impl ProbeCache {
    pub fn ensure(&self, root: &Path, force: bool) -> (ProbeStatus, Option<Arc<ProbeIndex>>) {
        let dump = find_dump(root);
        let fingerprint = fingerprint_of(&dump.files);

        if !force
            && let Some(cached) = self.inner.lock().unwrap().as_ref()
            && cached.root == root
            && cached.fingerprint == fingerprint
        {
            return (cached.status.clone(), cached.index.clone());
        }

        let (status, built) = ProbeIndex::build(dump);
        let arc = built.map(Arc::new);
        *self.inner.lock().unwrap() = Some(CachedProbe {
            root: root.to_path_buf(),
            fingerprint,
            status: status.clone(),
            index: arc.clone(),
        });
        (status, arc)
    }

    pub fn get(&self, root: &Path) -> Option<Arc<ProbeIndex>> {
        let guard = self.inner.lock().unwrap();
        guard.as_ref().filter(|c| c.root == root).and_then(|c| c.index.clone())
    }
}

type DtsFiles = BTreeMap<String, (PathBuf, u64)>;

const PROBEJS_6_DIR: &str = "kubejs/probe";
const PROBEJS_7_DIR: &str = ".probe";

struct ProbeDump {
    files: DtsFiles,
}

// A pack upgraded between ProbeJS 6 and 7 keeps its older dump, so we go by whichever dump is newer
// rather than mixing the two.
fn find_dump(root: &Path) -> ProbeDump {
    let probejs6 = dts_files_in(root, PROBEJS_6_DIR);
    let probejs7 = dts_files_in(root, PROBEJS_7_DIR);
    if !probejs7.is_empty() && last_modified(&probejs7) >= last_modified(&probejs6) {
        ProbeDump { files: probejs7 }
    } else {
        ProbeDump { files: probejs6 }
    }
}

fn dts_files_in(root: &Path, dir: &str) -> DtsFiles {
    let mut out = BTreeMap::new();
    for entry in WalkDir::new(root.join(dir)).into_iter().flatten() {
        if !entry.file_type().is_file() || !entry.file_name().to_string_lossy().to_lowercase().ends_with(".d.ts") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(rel) = entry.path().strip_prefix(root) else { continue };
        let rel = rel.to_string_lossy().replace('\\', "/");
        out.insert(rel, (entry.into_path(), meta.len()));
    }
    out
}

fn last_modified(files: &DtsFiles) -> Option<std::time::SystemTime> {
    files.values().filter_map(|(path, _)| std::fs::metadata(path).and_then(|m| m.modified()).ok()).max()
}

fn fingerprint_of(entries: &DtsFiles) -> String {
    let mut hasher = Sha1::new();
    for (rel, (path, len)) in entries {
        hasher.update(rel.as_bytes());
        hasher.update([0]);
        hasher.update(len.to_le_bytes());
        let stamp = std::fs::metadata(path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| (d.as_secs(), d.subsec_nanos()))
            .unwrap_or((0, 0));
        hasher.update(stamp.0.to_le_bytes());
        hasher.update(stamp.1.to_le_bytes());
    }
    hex::encode(hasher.finalize())
}

fn seed_js_builtins(chunk: &mut ParsedChunk) {
    let method = |name: &str, params: &[(&str, &str)], ret: &str| Member {
        name: name.to_string(),
        kind: MemberKind::Method,
        is_static: false,
        params: params
            .iter()
            .map(|(n, t)| Param {
                name: (*n).to_string(),
                ty: (*t).to_string(),
                optional: false,
                rest: false,
                literal: None,
            })
            .collect(),
        return_type: ret.to_string(),
        docs: None,
    };

    chunk.types.entry("Console".to_string()).or_insert_with(|| TypeDef {
        kind: TypeKind::Interface,
        extends: Vec::new(),
        members: vec![
            method("log", &[("...data", "any[]")], "void"),
            method("info", &[("...data", "any[]")], "void"),
            method("warn", &[("...data", "any[]")], "void"),
            method("error", &[("...data", "any[]")], "void"),
            method("debug", &[("...data", "any[]")], "void"),
        ],
        docs: None,
    });
    chunk.globals.entry("console".to_string()).or_insert_with(|| GlobalSymbol {
        name: "console".to_string(),
        kind: "variable",
        ty: "Console".to_string(),
        is_class_value: false,
        signature: None,
        docs: None,
    });

    chunk.types.entry("Math".to_string()).or_insert_with(|| TypeDef {
        kind: TypeKind::Interface,
        extends: Vec::new(),
        members: vec![
            method("floor", &[("x", "number")], "number"),
            method("ceil", &[("x", "number")], "number"),
            method("round", &[("x", "number")], "number"),
            method("abs", &[("x", "number")], "number"),
            method("max", &[("...values", "number[]")], "number"),
            method("min", &[("...values", "number[]")], "number"),
            method("pow", &[("x", "number"), ("y", "number")], "number"),
            method("sqrt", &[("x", "number")], "number"),
            method("random", &[], "number"),
        ],
        docs: None,
    });
    chunk.globals.entry("Math".to_string()).or_insert_with(|| GlobalSymbol {
        name: "Math".to_string(),
        kind: "variable",
        ty: "Math".to_string(),
        is_class_value: false,
        signature: None,
        docs: None,
    });

    chunk.types.entry("JSON".to_string()).or_insert_with(|| TypeDef {
        kind: TypeKind::Interface,
        extends: Vec::new(),
        members: vec![
            method("parse", &[("text", "string")], "any"),
            method("stringify", &[("value", "any")], "string"),
        ],
        docs: None,
    });
    chunk.globals.entry("JSON".to_string()).or_insert_with(|| GlobalSymbol {
        name: "JSON".to_string(),
        kind: "variable",
        ty: "JSON".to_string(),
        is_class_value: false,
        signature: None,
        docs: None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("packraven-probejs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reports_unavailable_when_no_typings_exist() {
        let root = fresh("missing");
        std::fs::create_dir_all(root.join("kubejs/server_scripts")).unwrap();
        let (status, idx) = ProbeIndex::build(find_dump(&root));
        assert!(!status.available);
        assert!(idx.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn picks_newer_probejs_7_dump_without_mixing_probejs_6() {
        let root = fresh("probejs7");
        std::fs::create_dir_all(root.join("kubejs/probe/generated")).unwrap();
        std::fs::write(root.join("kubejs/probe/generated/events.d.ts"), "declare const ServerEvents: { oldEvent(): void }").unwrap();
        let (status, idx) = ProbeIndex::build(find_dump(&root));
        assert!(status.available);
        assert!(idx.unwrap().complete("ServerEvents.", "ServerEvents.".len()).items.iter().any(|i| i.label == "oldEvent"));

        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::create_dir_all(root.join(".probe/@side-only/server/events")).unwrap();
        std::fs::write(
            root.join(".probe/@side-only/server/events/index.d.ts"),
            r#"
import { $RecipesKubeEvent } from "@package/dev/latvian/mods/kubejs/recipe";
declare module "@side-only/server/events" {
    export class TagEvent<T> {
        add(tag: RegistryTypes.ResolveTag<T>, ...values: RegistryTypes.ResolveObject<T>[]): void;
    }
}
export {};
declare global {
    namespace ServerEvents {
        function recipes(handler: ((event: $RecipesKubeEvent) => void)): void;
        function tags(type: "item", handler: ((event: TagEvent<$Item>) => void)): void;
    }
}
"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join(".probe/@side-only/server/bindings")).unwrap();
        std::fs::write(
            root.join(".probe/@side-only/server/bindings/index.d.ts"),
            r#"
declare global {
    let Item: typeof $ItemWrapper;
}
"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join(".probe/@special/types")).unwrap();
        std::fs::write(
            root.join(".probe/@special/types/index.d.ts"),
            r#"
declare module "@special/types" {
    export namespace RegistryTypes {
        type Item = "minecraft:diamond" | "minecraft:stone";
        type ItemTag = "c:ingots/iron" | "c:gems/diamond";
    }
    export namespace SpecialTypes {
        type ModId = "minecraft" | "create";
    }
}
"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join(".probe/@package/dev/latvian/mods/kubejs/recipe")).unwrap();
        std::fs::write(
            root.join(".probe/@package/dev/latvian/mods/kubejs/recipe/index.d.ts"),
            r#"
declare module "@package/dev/latvian/mods/kubejs/recipe" {
    export class $ItemWrapper {
        static of(item: $ItemStack_): $ItemStack;
    }
    export type $ItemStack_ = RegistryTypes.Item;
    export type $Ingredient_ = $ItemStack_ | `#${RegistryTypes.ItemTag}` | `@${SpecialTypes.ModId}`;
    export class $RecipesKubeEvent {
        get recipes(): DocumentedRecipes;
        remove(filter: $RecipeFilter_): void;
    }
}
declare module "@side-only/server/events/recipes" {
    export class DocumentedRecipes {
        create: {
            cutting(results: $List_<$ItemStack_>, ingredients: $List_<$Ingredient_>): Create$Cutting;
        }
    }
    export class Create$Cutting {
        processingTime(ticks: number): this;
    }
}
"#,
        )
        .unwrap();

        let (status, idx) = ProbeIndex::build(find_dump(&root));
        assert!(status.available);
        let idx = idx.unwrap();

        let evs = idx.complete("ServerEvents.", "ServerEvents.".len());
        assert!(evs.items.iter().any(|i| i.label == "recipes"));
        assert!(!evs.items.iter().any(|i| i.label == "oldEvent"));

        let s_chain = "ServerEvents.recipes(e => {\n  e.recipes.create.cutting(['minecraft:d'], ['#c:ingots/']).\n})";
        let cut_off = s_chain.find("minecraft:d").unwrap() + "minecraft:d".len();
        assert!(idx.complete(s_chain, cut_off).items.iter().any(|i| i.label == "minecraft:diamond"));
        let tag_off = s_chain.find("#c:ingots/").unwrap() + "#c:ingots/".len();
        assert!(idx.complete(s_chain, tag_off).items.iter().any(|i| i.label == "#c:ingots/iron"));
        let fluent_off = s_chain.find("]).").unwrap() + "]).".len();
        assert!(idx.complete(s_chain, fluent_off).items.iter().any(|i| i.label == "processingTime"));

        let s_tags = "ServerEvents.tags('item', e => {\n  e.add('c:gems/', 'minecraft:d')\n})";
        let t_off = s_tags.find("c:gems/").unwrap() + "c:gems/".len();
        assert!(idx.complete(s_tags, t_off).items.iter().any(|i| i.label == "c:gems/diamond"));
        let i_off = s_tags.find("minecraft:d").unwrap() + "minecraft:d".len();
        assert!(idx.complete(s_tags, i_off).items.iter().any(|i| i.label == "minecraft:diamond"));

        let item_static = idx.complete("Item.", "Item.".len());
        assert!(item_static.items.iter().any(|i| i.label == "of"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reads_which_script_folders_each_probejs_6_event_fires_in() {
        let root = fresh("event-sides");
        std::fs::create_dir_all(root.join("kubejs/probe/generated")).unwrap();
        std::fs::write(
            root.join("kubejs/probe/generated/events.d.ts"),
            r#"
declare const ClientEvents: {
    /**
     * @at *startup*
    */
init(handler: (event: Internal.ClientInitEventJS) => void):void,
    /**
     * Invoked when the debug info is rendered.
     *
     * @at *client*
    */
rightDebugInfo(handler: (event: Internal.DebugInfoEventJS) => void):void,
};
declare const NetworkEvents: {
    /**
     * @at *server, client*
    */
dataReceived(handler: (event: Internal.NetworkEventJS) => void):void,
};
"#,
        )
        .unwrap();
        let (_, idx) = ProbeIndex::build(find_dump(&root));
        let sides = idx.unwrap().event_sides().clone();
        assert_eq!(sides.get("ClientEvents.init"), Some(&vec!["startup".to_string()]));
        assert_eq!(sides.get("ClientEvents.rightDebugInfo"), Some(&vec!["client".to_string()]));
        assert_eq!(sides.get("NetworkEvents.dataReceived"), Some(&vec!["server".to_string(), "client".to_string()]));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn keeps_same_named_classes_from_different_probejs_7_modules_apart() {
        let root = fresh("probejs7-collisions");
        let write = |path: &str, text: &str| {
            let path = root.join(".probe").join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write(
            "@package/net/minecraft/world/level/index.d.ts",
            r#"
declare module "@package/net/minecraft/world/level" {
    export class $Level {
        isClientSide(): boolean;
    }
    export type $Level_ = $Level;
}
"#,
        );
        write(
            "@package/org/apache/logging/log4j/index.d.ts",
            r#"
declare module "@package/org/apache/logging/log4j" {
    export class $Level {
        static valueOf(name: string): $Level;
        intLevel(): number;
    }
}
"#,
        );
        write(
            "@package/dev/latvian/mods/kubejs/level/index.d.ts",
            r#"
import { $Level as $Level$1 } from "@package/net/minecraft/world/level";
import { $Level } from "@package/org/apache/logging/log4j";
declare module "@package/dev/latvian/mods/kubejs/level" {
    export class $LevelKubeEvent {
        /** The level this event fired in. */
        get level(): $Level$1;
        get logLevel(): $Level;
    }
}
"#,
        );
        write(
            "@side-only/server/events/index.d.ts",
            r#"
import { $LevelKubeEvent } from "@package/dev/latvian/mods/kubejs/level";
declare global {
    namespace LevelEvents {
        function tick(handler: ((event: $LevelKubeEvent) => void)): void;
    }
}
export {};
"#,
        );

        let (status, idx) = ProbeIndex::build(find_dump(&root));
        assert!(status.available);
        let idx = idx.unwrap();
        let labels = |text: &str, at: &str| -> Vec<String> {
            let offset = text.find(at).unwrap() + at.len();
            idx.complete(text, offset).items.into_iter().map(|i| i.label).collect()
        };

        let level = labels("LevelEvents.tick(e => {\n  e.level.\n})", "e.level.");
        assert!(level.contains(&"isClientSide".to_string()), "{level:?}");
        assert!(!level.contains(&"intLevel".to_string()), "{level:?}");
        let log_level = labels("LevelEvents.tick(e => {\n  e.logLevel.\n})", "e.logLevel.");
        assert!(log_level.contains(&"intLevel".to_string()), "{log_level:?}");
        assert!(!log_level.contains(&"isClientSide".to_string()), "{log_level:?}");

        let event = idx.complete("LevelEvents.tick(e => {\n  e.\n})", "LevelEvents.tick(e => {\n  e.".len());
        let detail = event.items.iter().find(|i| i.label == "level").and_then(|i| i.detail.clone()).unwrap_or_default();
        assert!(detail.contains("$Level") && !detail.contains("__m"), "{detail}");
        assert!(event.items.iter().any(|i| i.docs.as_deref() == Some("The level this event fired in.")));

        let loaded = labels("const Log4jLevel = Java.loadClass('org.apache.logging.log4j.Level')\nLog4jLevel.", "Log4jLevel.");
        assert!(loaded.contains(&"valueOf".to_string()), "{loaded:?}");

        assert!(!labels("$Lev", "$Lev").iter().any(|l| l.starts_with("$Level")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn resolves_events_chained_recipes_tags_and_string_unions() {
        let root = fresh("present");
        std::fs::create_dir_all(root.join("kubejs/probe/generated/internals")).unwrap();
        std::fs::write(
            root.join("kubejs/probe/generated/events.d.ts"),
            r#"
declare const ServerEvents: {
    /** Register or modify recipes. */
    recipes(handler: (event: Internal.RecipesEventJS) => void): void,
    tags(type: "item", handler: (event: TagEvent.Item) => void): void
    tags(type: "block", handler: (event: TagEvent.Block) => void): void
}
"#,
        )
        .unwrap();
        std::fs::write(
            root.join("kubejs/probe/generated/tag_events.d.ts"),
            r#"
declare namespace TagEvent {
    class Item {
        add(tag: Special.ItemTag, ...id: Special.Item[]): void
    }
    class Block {
        add(tag: Special.BlockTag, ...id: Special.Block[]): void
    }
}
"#,
        )
        .unwrap();
        std::fs::write(
            root.join("kubejs/probe/generated/globals.d.ts"),
            r#"
declare namespace Special {
    type Item = "minecraft:diamond" | "minecraft:diamond_block" | "minecraft:stone";
    type ItemTag = "forge:ingots/iron" | "forge:gems/diamond";
    type Block = "minecraft:stone";
    type BlockTag = "minecraft:mineable/pickaxe";
    type Mod = "minecraft" | "gtceu";
    class DocumentedRecipes {
        gtceu: {
            assembler(id: string): Special.Recipes.AssemblerGtceu
        }
    }
    declare namespace Recipes {
        class AssemblerGtceu {
            itemInputs(input: InputItem_): this
            duration(ticks: number): this
        }
    }
}
type InputItem_ = `#${Special.ItemTag}` | `@${Special.Mod}` | Special.Item;
"#,
        )
        .unwrap();
        std::fs::write(
            root.join("kubejs/probe/generated/internals/internal_1.d.ts"),
            r#"
declare namespace Internal {
    class RecipesEventJS {
        shapeless(output: Special.Item, inputs: InputItem_[]): void;
        recipes: Special.DocumentedRecipes;
        remove(filter: { output?: Special.Item, mod?: Special.Mod }): void;
    }
}
"#,
        )
        .unwrap();

        let cache = ProbeCache::default();
        let (status, idx) = cache.ensure(&root, false);
        assert!(status.available);
        assert_eq!(status.file_count, 4);
        let idx = idx.unwrap();

        // 1. Top-level ServerEvents.
        let s1 = "ServerEvents.";
        let c1 = idx.complete(s1, s1.len());
        assert!(c1.items.iter().any(|i| i.label == "recipes"));
        assert!(c1.items.iter().any(|i| i.label == "tags"));

        // 2. Callback param + chained recipes.gtceu.assembler(...).
        let s2 = "ServerEvents.recipes(e => {\n  e.recipes.gtceu.assembler('id').\n})";
        let off2 = s2.find("('id').").unwrap() + "('id').".len();
        let c2 = idx.complete(s2, off2);
        assert!(c2.items.iter().any(|i| i.label == "itemInputs"));
        assert!(c2.items.iter().any(|i| i.label == "duration"));

        // 3. String union completion inside shapeless('minecraft:d')
        let s3 = "ServerEvents.recipes(e => {\n  e.shapeless('minecraft:d', [])\n})";
        let off3 = s3.find("minecraft:d").unwrap() + "minecraft:d".len();
        let c3 = idx.complete(s3, off3);
        assert!(c3.in_string);
        assert!(c3.items.iter().any(|i| i.label == "minecraft:diamond"));

        // 4. Tag event overload selection: ServerEvents.tags('item', e => { e.add('forge:') })
        let s4 = "ServerEvents.tags('item', e => {\n  e.add('forge:')\n})";
        let off4 = s4.find("forge:").unwrap() + "forge:".len();
        let c4 = idx.complete(s4, off4);
        assert!(c4.items.iter().any(|i| i.label == "forge:ingots/iron"));

        // 5. Object key completion: e.remove({ output: 'minecraft:d' })
        let s5 = "ServerEvents.recipes(e => { e.remove({ output: 'minecraft:d' }) })";
        let off5 = s5.find("minecraft:d").unwrap() + "minecraft:d".len();
        let c5 = idx.complete(s5, off5);
        assert!(c5.items.iter().any(|i| i.label == "minecraft:diamond"));

        // 6. Hover on ServerEvents.recipes
        let h = idx.hover("ServerEvents.recipes(e => {})", "ServerEvents.rec".len()).unwrap();
        assert!(h.signature.contains("recipes("));
        assert!(h.docs.contains("Register or modify recipes"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[ignore]
    fn bench_real_probejs_6_dump() {
        let Some(root) = std::env::var_os("PACKRAVEN_PROBEJS_BENCH").map(PathBuf::from) else {
            eprintln!("Set PACKRAVEN_PROBEJS_BENCH to an ATM9 Expert instance folder to run this bench");
            return;
        };
        let (status, idx) = ProbeIndex::build(find_dump(&root));
        let idx = idx.unwrap();
        let script = std::fs::read_to_string(root.join("kubejs/server_scripts/custom.js")).unwrap();

        let t0 = Instant::now();
        let pos1 = script.find("ServerEvents.").unwrap() + "ServerEvents.".len();
        let c1 = idx.complete(&script, pos1);
        let t1 = t0.elapsed();

        let pos2 = script.find("allthemods.shapeless").unwrap() + "allthemods.".len();
        let c2 = idx.complete(&script, pos2);
        let t2 = t0.elapsed() - t1;

        let pos3 = script.find("allthemods.recipes.gtceu.chemical_reactor").unwrap() + "allthemods.recipes.gtceu.".len();
        let c3 = idx.complete(&script, pos3);
        let t3 = t0.elapsed() - t1 - t2;

        let pos4 = script.find("minecraft:netherite_scrap").unwrap() + "minecraft:nether".len();
        let c4 = idx.complete(&script, pos4);
        let t4 = t0.elapsed() - t1 - t2 - t3;

        let h = idx.hover(&script, script.find("allthemods.shapeless").unwrap() + "allthemods.shape".len());

        eprintln!(
            "ProbeIndex: {} files, {:.1} MB, {} types, {} string entries, built in {} ms",
            status.file_count,
            status.total_bytes as f64 / 1024.0 / 1024.0,
            status.type_count,
            status.string_entries,
            status.load_ms
        );
        eprintln!("  ServerEvents.: {} items in {:?}", c1.items.len(), t1);
        eprintln!("  allthemods.: {} items in {:?}", c2.items.len(), t2);
        eprintln!("  allthemods.recipes.gtceu.: {} items in {:?}", c3.items.len(), t3);
        eprintln!("  'minecraft:nether': {} items in {:?} (first: {:?})", c4.items.len(), t4, c4.items.first().map(|i| &i.label));
        eprintln!("  hover shapeless: {:?}", h.as_ref().map(|x| &x.signature));

        assert!(c1.items.iter().any(|i| i.label == "recipes"));
        assert!(c2.items.iter().any(|i| i.label == "shapeless"));
        assert!(c3.items.iter().any(|i| i.label == "chemical_reactor"));
        assert!(c4.items.iter().any(|i| i.label == "minecraft:netherite_scrap"));
        assert!(h.is_some());

        let sides = idx.event_sides();
        eprintln!("  {} events with sides; ClientEvents.init: {:?}", sides.len(), sides.get("ClientEvents.init"));
        assert_eq!(sides.get("ClientEvents.init"), Some(&vec!["startup".to_string()]));
        assert!(sides.keys().any(|event| event.starts_with("GTCEuServerEvents.")));
    }

    #[test]
    #[ignore]
    fn bench_real_probejs_7_dump() {
        let Some(root) = std::env::var_os("PACKRAVEN_PROBEJS7_BENCH").map(PathBuf::from) else {
            eprintln!("Set PACKRAVEN_PROBEJS7_BENCH to a 1.21.1 ProbeJS 7 instance folder to run this bench");
            return;
        };
        let (status, idx) = ProbeIndex::build(find_dump(&root));
        assert!(status.available);
        let idx = idx.unwrap();

        let script = r#"
ServerEvents.recipes(event => {
  event.recipes.minecraft.crafting_shapeless('minecraft:dia', ['#c:ingots/'])
  event.recipes.create.cutting(['minecraft:iron'], ['minecraft:stone']).
  event.remove({ mod: 'create' })
})

ServerEvents.tags('item', event => {
  event.add('c:dyes/', 'minecraft:dia')
})

Item.
"#;

        let t0 = Instant::now();
        let pos_se = script.find("ServerEvents.").unwrap() + "ServerEvents.".len();
        let c_se = idx.complete(script, pos_se);
        let t1 = t0.elapsed();

        let pos_ev = script.find("event.recipes").unwrap() + "event.".len();
        let c_ev = idx.complete(script, pos_ev);
        let t2 = t0.elapsed() - t1;

        let pos_mc = script.find("event.recipes.minecraft.").unwrap() + "event.recipes.minecraft.".len();
        let c_mc = idx.complete(script, pos_mc);
        let t3 = t0.elapsed() - t1 - t2;

        let pos_item = script.find("minecraft:dia").unwrap() + "minecraft:dia".len();
        let c_item = idx.complete(script, pos_item);
        let t4 = t0.elapsed() - t1 - t2 - t3;

        let pos_tag = script.find("#c:ingots/").unwrap() + "#c:ingots/".len();
        let c_tag = idx.complete(script, pos_tag);

        let pos_cut = script.find("]).").unwrap() + "]).".len();
        let c_cut = idx.complete(script, pos_cut);

        let pos_tag_ev = script.find("c:dyes/").unwrap() + "c:dyes/".len();
        let c_tag_ev = idx.complete(script, pos_tag_ev);

        let pos_static = script.rfind("Item.").unwrap() + "Item.".len();
        let c_static = idx.complete(script, pos_static);

        let h = idx.hover(script, script.find("crafting_shapeless").unwrap() + 4);

        eprintln!(
            "ProbeJS 7 Index: {} files, {:.1} MB, {} types, {} string entries, {} failed, built in {} ms",
            status.file_count,
            status.total_bytes as f64 / 1024.0 / 1024.0,
            status.type_count,
            status.string_entries,
            status.failed_files.len(),
            status.load_ms
        );
        eprintln!("  ServerEvents.: {} items in {:?}", c_se.items.len(), t1);
        eprintln!("  event.: {} items in {:?}", c_ev.items.len(), t2);
        eprintln!("  event.recipes.minecraft.: {} items in {:?}", c_mc.items.len(), t3);
        eprintln!("  'minecraft:dia': {} items in {:?} (first: {:?})", c_item.items.len(), t4, c_item.items.first().map(|i| &i.label));
        eprintln!("  '#c:ingots/': {} items (first: {:?})", c_tag.items.len(), c_tag.items.first().map(|i| &i.label));
        eprintln!("  create.cutting().: {} items (first: {:?})", c_cut.items.len(), c_cut.items.first().map(|i| &i.label));
        eprintln!("  tags('item') 'c:dyes/': {} items (first: {:?})", c_tag_ev.items.len(), c_tag_ev.items.first().map(|i| &i.label));
        eprintln!("  Item.: {} items (first: {:?})", c_static.items.len(), c_static.items.first().map(|i| &i.label));
        eprintln!("  hover crafting_shapeless: {:?}", h.as_ref().map(|x| &x.signature));

        assert!(c_se.items.iter().any(|i| i.label == "recipes"));
        assert!(c_ev.items.iter().any(|i| i.label == "recipes"));
        assert!(c_mc.items.iter().any(|i| i.label == "crafting_shapeless"));
        assert!(c_item.items.iter().any(|i| i.label == "minecraft:diamond"));
        assert!(c_tag.items.iter().any(|i| i.label.starts_with("#c:ingots/")));
        // Only packs with the KubeJS Create addon document Create's recipes.
        if idx.types.keys().any(|k| k.ends_with("Create$Cutting")) {
            assert!(c_cut.items.iter().any(|i| i.label == "processingTime"));
        }
        let sides = idx.event_sides();
        eprintln!("  {} events with sides; ServerEvents.recipes: {:?}", sides.len(), sides.get("ServerEvents.recipes"));
        if let Some(out) = std::env::var_os("PACKRAVEN_EVENT_SIDES_OUT") {
            std::fs::write(out, serde_json::to_string(sides).unwrap()).unwrap();
        }
        assert!(c_tag_ev.items.iter().any(|i| i.label.starts_with("c:dyes/")));
        assert!(c_static.items.iter().any(|i| i.label == "of"));
        assert!(h.is_some());

        // Vanilla's Level and log4j's Level share a name, and so do 4,000-odd other classes.
        let labels = |text: &str, at: &str| -> Vec<String> {
            let offset = text.find(at).unwrap() + at.len();
            idx.complete(text, offset).items.into_iter().map(|i| i.label).collect()
        };
        let level = labels("LevelEvents.tick(event => {\n  event.level.\n})", "event.level.");
        let loaded = labels("const $LevelClass = Java.loadClass('net.minecraft.world.level.Level')\n$LevelClass.", "$LevelClass.");
        let unresolved = unresolved_dollar_types(&idx);
        eprintln!(
            "  {} colliding names; event.level.: {} items (clientSide: {}, intLevel: {}); loadClass Level: {} items; {} unresolved $ types (e.g. {:?})",
            idx.modules.collision_count(),
            level.len(),
            level.iter().any(|l| l == "clientSide"),
            level.iter().any(|l| l == "intLevel"),
            loaded.len(),
            unresolved.len(),
            unresolved.iter().take(8).collect::<Vec<_>>()
        );
        // A renamed import like `$Function$1` or a module key that resolves nowhere means our
        // requalifying missed; anything else is a class ProbeJS didn't dump.
        let misresolved: Vec<&String> = unresolved
            .iter()
            .filter(|n| n.contains("__m") || n.rsplit('$').next().is_some_and(|tail| !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit())))
            .collect();
        assert!(misresolved.is_empty(), "{misresolved:?}");
        assert!(level.iter().any(|l| l == "clientSide"));
        assert!(!level.iter().any(|l| l == "intLevel"));
        assert!(!loaded.is_empty());

        let rv = labels("RecipeViewerEvents.removeEntriesCompletely('item', e => {\n  e.remove('minecraft:dia')\n})", "minecraft:dia");
        let dye = labels("let $DyeColor = Java.loadClass('net.minecraft.world.item.DyeColor')\n$DyeColor.", "$DyeColor.");
        eprintln!("  RecipeViewerEvents remove 'minecraft:dia': {} items; DyeColor.: {} items", rv.len(), dye.len());
        assert!(rv.iter().any(|l| l == "minecraft:diamond"));
        assert!(dye.iter().any(|l| l == "values"));
    }

    // Counts the `$` type names our members point at that no type or alias answers to, as a rough
    // measure of how many references fell through.
    fn unresolved_dollar_types(idx: &ProbeIndex) -> std::collections::BTreeSet<String> {
        let mut missing = std::collections::BTreeSet::new();
        let mut check = |ty: &str| {
            for word in ty.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$' || c == '.')) {
                let name = word.trim_matches('.');
                if name.contains('$') && !idx.types.contains_key(name) && !idx.aliases.contains_key(name) && !idx.types.contains_key(name.trim_end_matches('_')) {
                    missing.insert(name.to_string());
                }
            }
        };
        for def in idx.types.values() {
            for member in &def.members {
                check(&member.return_type);
                member.params.iter().for_each(|p| check(&p.ty));
            }
        }
        missing
    }
}
