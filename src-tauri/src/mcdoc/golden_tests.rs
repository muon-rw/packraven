//! Golden conformance tests ported from Spyglass's `packages/mcdoc/test/runtime/checker.spec.ts`
//! plus Packraven-specific attribute, loader condition, and modded partner `.mcdoc` tests.

use super::checker::{check_json_with_type, resource_root_type, CheckEnv};
use super::fetch::{bridge_partner_dispatchers, PACKRAVEN_MCDOC};
use super::json_parser::{self, Diagnostic, Severity};
use super::parser::parse_and_bind;
use super::registries::{parse_data_file_path, KnownRegistries, RegistryIndex};
use super::types::{McdocType, SymbolTables};
use super::version::VersionContext;

fn run_case(mcdoc_def: &str, json_src: &str) -> Vec<Diagnostic> {
    let mut tables = SymbolTables::default();
    parse_and_bind(&format!("type Test = {mcdoc_def}"), "::test", &mut tables)
        .unwrap_or_else(|e| panic!("failed to parse mcdoc `{mcdoc_def}`: {e}"));
    let regs = RegistryIndex::default();
    let version = VersionContext { minecraft: Some("1.21.1".into()), neoforge: Some("21.1.265".into()) };
    let env = CheckEnv {
        tables: &tables,
        registries: &regs,
        version: &version,
        require_canonical: false,
        allow_missing_keys: false,
        primitives_read_as_strings: false,
        ignored_loaders: &[],
    };
    let parsed = json_parser::parse(json_src, false);
    assert!(parsed.diagnostics.is_empty(), "invalid test JSON `{json_src}`: {:?}", parsed.diagnostics);
    let root_type = McdocType::Reference { path: Some("::test::Test".into()), attributes: Vec::new() };
    check_json_with_type(parsed.root.as_ref().unwrap(), &root_type, &env).diagnostics
}

fn assert_ok(mcdoc_def: &str, json_src: &str) {
    let diags = run_case(mcdoc_def, json_src);
    assert!(diags.is_empty(), "expected 0 diagnostics for `{mcdoc_def}` with `{json_src}`, got: {diags:?}");
}

fn assert_has_msg(mcdoc_def: &str, json_src: &str, needle: &str) {
    let diags = run_case(mcdoc_def, json_src);
    assert!(
        diags.iter().any(|d| d.message.contains(needle)),
        "expected diagnostic containing `{needle}` for `{mcdoc_def}` with `{json_src}`, got: {diags:?}"
    );
}

#[test]
fn spyglass_golden_string_byte_int_double() {
    // string @ ..8
    assert_has_msg("string @ ..8", "1", "Expected a string");
    assert_ok("string @ ..8", r#""abc""#);
    assert_has_msg("string @ ..8", r#""abcdefghij""#, "string length in range ..8");

    // byte
    assert_ok("byte", "127");
    assert_ok("byte", "-128");
    assert_ok("byte", "5");
    assert_has_msg("byte", "255", "Expected an integer");
    assert_has_msg("byte", "-129", "Expected an integer");
    assert_has_msg("byte", "128", "Expected an integer");

    // int
    assert_ok("int", "255");
    assert_ok("int", "2147483647");
    assert_ok("int", "-2147483648");
    assert_has_msg("int", "2147483648", "Expected an integer");
    assert_has_msg("int", "-2147483649", "Expected an integer");
    // Vanilla reads integers through `Number::intValue`, and its datagen writes `"rolls": 1.0`.
    assert_ok("int", "1.0");
    assert_ok("int @ 1..", "2e1");
    assert_ok("(string | int | struct {})", "1.0");
    assert_has_msg("int", "1.5", "Expected an integer");
    assert_has_msg("byte", "128.0", "Expected an integer");
}

#[test]
fn integers_vanilla_truncates_or_wraps_load_with_a_warning() {
    let only = |def: &str, json: &str| {
        let diags = run_case(def, json);
        assert_eq!(diags.len(), 1, "`{def}` with `{json}`: {diags:?}");
        assert_eq!(diags[0].severity, Severity::Warning, "`{def}` with `{json}`: {diags:?}");
        diags[0].message.clone()
    };
    assert_eq!(only("int", "2.5"), "Expected an integer. Minecraft reads 2.5 as 2.");
    assert_eq!(only("int", "-2.5"), "Expected an integer. Minecraft reads -2.5 as -2.");
    // A structure set salt that Gson parses as a long and vanilla narrows to an int.
    assert_eq!(only("int @ 0..", "5004938900"), "Expected an integer. Minecraft reads 5004938900 as 709971604.");
    assert_eq!(only("byte", "255"), "Expected an integer. Minecraft reads 255 as -1.");
    // Vanilla checks ranges against the narrowed value, so that check still applies.
    assert!(run_case("int @ 1..", "0.5").iter().any(|d| d.severity == Severity::Error && d.message.contains("range 1..")));
    // A union member that reads the number exactly wins.
    assert_ok("(int | float)", "2.5");
    assert_eq!(only("(string | int)", "2.5"), "Expected an integer. Minecraft reads 2.5 as 2.");

    // double @ 3..6.2
    assert_has_msg("double @ 3..6.2", r#""hello""#, "Expected a number");
    assert_has_msg("double @ 3..6.2", "1", "in range 3..6.2");
    assert_ok("double @ 3..6.2", "3");
    assert_ok("double @ 3..6.2", "4");
    assert_ok("double @ 3..6.2", "6.2");
    assert_has_msg("double @ 3..6.2", "6.3", "in range 3..6.2");

    // double @ 2..<4
    assert_ok("double @ 2..<4", "2");
    assert_ok("double @ 2..<4", "3.99");
    assert_has_msg("double @ 2..<4", "4", "in range 2..<4");
}

#[test]
fn spyglass_golden_structs() {
    assert_ok("struct { test: double }", r#"{"test": 1}"#);
    assert_has_msg("struct { test: double }", r#"{"test": "hello"}"#, "Expected a number");

    let s = "struct { foo: string, bar: [double] }";
    assert_has_msg(s, "2", "Expected an object");
    assert_eq!(run_case(s, "{}").len(), 2);
    assert_has_msg(s, r#"{"foo": "hello"}"#, r#"Missing required key "bar""#);
    assert_has_msg(s, r#"{"foo": true, "bar": []}"#, "Expected a string");

    // Filtered field (`#[until="1.20"]` plays the role of Spyglass's custom `#[filter]` returning false).
    let filtered = r#"struct { foo: string, #[until="1.20"] bar: [double] }"#;
    assert_has_msg(filtered, "{}", r#"Missing required key "foo""#);
    assert_ok(filtered, r#"{"foo": "hello"}"#);
    assert_has_msg(filtered, r#"{"foo": "hello", "bar": [2]}"#, r#"Unknown key "bar""#);

    // Optional empty union `foo?: ()`
    assert_ok("struct { foo?: () }", "{}");
    assert_has_msg("struct { foo?: () }", r#"{"foo": "something"}"#, "Unexpected value");

    // Union of list types
    let pages = "struct { pages: ([struct { raw: string, filtered?: string }] | [string]) }";
    assert_ok(pages, r#"{"pages": []}"#);
    assert_ok(pages, r#"{"pages": ["foo", "bar"]}"#);
    assert_ok(pages, r#"{"pages": [{"raw": "foo"}, {"raw": "bar", "filtered": "baz"}]}"#);
    assert!(!run_case(pages, r#"{"pages": ["foo", {"raw": "bar"}]}"#).is_empty());

    // Spread with literal field override
    let spread_override = "struct { ...struct { foo: double, bar: boolean }, foo: string }";
    assert_eq!(run_case(spread_override, "{}").len(), 2);
    assert_has_msg(spread_override, r#"{"foo": "hello"}"#, r#"Missing required key "bar""#);
    assert_ok(spread_override, r#"{"foo": "hello", "bar": true}"#);
    assert_has_msg(spread_override, r#"{"foo": 4, "bar": false}"#, "Expected a string");

    // Spread with literal optional override (later optional overrides earlier required)
    let spread_opt_override = "struct { ...struct { foo: double, bar: boolean }, foo?: string }";
    assert_ok(spread_opt_override, r#"{"bar": true}"#);

    // Spread of a multi-member union. Spyglass adds nothing for it and reports `Unknown key "bar"`. We
    // expand it into one struct per member (with the members' fields optional), because vanilla-mcdoc
    // describes dispatch targets that changed shape between versions this way.
    let spread_union = "struct { foo: string, ...( struct { bar: string } | struct { baz: string } ) }";
    assert_has_msg(spread_union, "{}", r#"Missing required key "foo""#);
    assert_ok(spread_union, r#"{"foo": "hi"}"#);
    assert_has_msg(spread_union, r#"{"foo": "hi", "bar": 1}"#, "Expected a string, found integer 1.");
}

#[test]
fn spyglass_golden_unions() {
    let num_union = "(double @ 2..4 | double @ 8..)";
    assert_ok(num_union, "3");
    assert_has_msg(num_union, "5", "2..4 or 8..");
    assert_ok(num_union, "9");

    let struct_range = "(struct { foo: int @ 0..5 } | struct { foo: int @ 4..6 })";
    assert_ok(struct_range, r#"{"foo": 4}"#);
    assert_has_msg(struct_range, r#"{"foo": 9}"#, "0..5 or 4..6");

    let nested_range = "(struct { foo: (int @ 0..5 | int @ 20..25) } | struct { foo: int @ 4..6 })";
    assert_ok(nested_range, r#"{"foo": 4}"#);
    assert_has_msg(nested_range, r#"{"foo": 9}"#, "in range");
    assert_ok(nested_range, r#"{"foo": 23}"#);

    // Filtered union member
    let filtered_union = r#"(string | #[until="1.20"] int)"#;
    assert_ok(filtered_union, r#""hello""#);
    assert_has_msg(filtered_union, "1", "Expected a string");

    // Conflicting key combination across struct union
    let key_combo = "( struct { one?: string, two?: int, three?: boolean } | struct { one?: string, four?: int, five?: boolean })";
    assert_ok(key_combo, "{}");
    assert_ok(key_combo, r#"{"one": "something"}"#);
    assert_ok(key_combo, r#"{"two": 91}"#);
    assert_ok(key_combo, r#"{"two": 91, "three": true}"#);
    assert_has_msg(key_combo, r#"{"two": 91, "four": 91}"#, "Invalid key combination");
    assert_has_msg(key_combo, r#"{"one": 91}"#, "Expected a string");

    let open_union = "( struct { foo: int } | struct { bar: string, [any]: any } )";
    assert_ok(open_union, r#"{"foo": 91}"#);
    assert_ok(open_union, r#"{"bar": "bar"}"#);

    let disc = "( struct { bar: int } | struct { bar: string, baz: int } )";
    assert_ok(disc, r#"{"bar": 91}"#);
    assert_has_msg(disc, r#"{"bar": "bar"}"#, "Expected an integer");
    assert_ok(disc, r#"{"bar": "bar", "baz": 91}"#);

    let disc_open = "( struct { bar: int, baz: string } | struct { bar: string, [string]: any } )";
    assert_has_msg(disc_open, r#"{"bar": 91}"#, "Expected a string");
    assert_ok(disc_open, r#"{"bar": 91, "baz": "hi"}"#);
    assert_ok(disc_open, r#"{"bar": "str"}"#);
    assert_ok(disc_open, r#"{"bar": "str", "myFavoriteNumber": 91}"#);
    assert_ok(disc_open, r#"{"bar": "str", "darkOverlord": {"name": "NeunEinser", "powerLevel": 9001}}"#);
}

#[test]
fn spyglass_golden_lists_indexed_templates_references_dispatchers() {
    // Lists
    assert_ok("[int] @ 0..5", "[]");
    assert_ok("[int] @ 0..5", "[1, 2, 3]");
    assert_has_msg("[int] @ 0..5", "[1, 2, 3, 4, 5, 6]", "collection length in range 0..5");

    let list_range = "[double @ 0..1] @ 1..3";
    assert_has_msg(list_range, "[]", "collection length in range 1..3");
    assert_ok(list_range, "[0.3, 0.1]");
    assert_has_msg(list_range, "[0.2, 6]", "in range 0..1");
    assert_has_msg(list_range, "[0.3, 0.9, 0.1, 0.1]", "collection length in range 1..3");
    assert_eq!(run_case(list_range, "[2, 0.9, 0.1, 0.1]").len(), 2);

    // Indexed dynamic spread
    let idx_spread = "struct { id: string, ...struct { test: struct { config: double }, other: struct { baz: boolean } }[[id]] }";
    assert_has_msg(idx_spread, "{}", r#"Missing required key "id""#);
    assert_ok(idx_spread, r#"{"id": "fallback"}"#);
    assert_has_msg(idx_spread, r#"{"id": "test"}"#, r#"Missing required key "config""#);
    assert_has_msg(idx_spread, r#"{"id": "test", "config": "hello"}"#, "Expected a number");
    assert_ok(idx_spread, r#"{"id": "test", "config": 5}"#);
    assert_has_msg(idx_spread, r#"{"id": "test", "baz": true}"#, r#"Unknown key "baz""#);
    assert_ok(idx_spread, r#"{"id": "other", "baz": true}"#);

    // Indexed `%fallback`
    let idx_fb = "struct { test: struct { config: double }, other: struct { baz: boolean } }[%fallback]";
    assert_ok(idx_fb, r#"{"config": 5}"#);
    assert_ok(idx_fb, r#"{"baz": true}"#);
    assert_has_msg(idx_fb, r#"{"config": "hello"}"#, "Expected a number");

    // Templates
    let bounds = "Bounds<int @ 1..> type Bounds<T> = struct { min: T, max: T }";
    assert_eq!(run_case(bounds, "{}").len(), 2);
    assert_ok(bounds, r#"{"min": 2, "max": 5}"#);
    assert_eq!(run_case(bounds, r#"{"min": "hello", "max": -1}"#).len(), 2);

    let tag_t = "Tag<string> type Tag<V> = (V | [V])";
    assert_has_msg(tag_t, "2", "Expected a string or a list");
    assert_ok(tag_t, r#""hello""#);
    assert_ok(tag_t, "[]");
    assert_ok(tag_t, r#"["test"]"#);
    assert_has_msg(tag_t, "[10]", "Expected a string");

    let tuple_t = "Tag<string> type Tag<V> = [int, V]";
    assert_has_msg(tuple_t, r#""test""#, "Expected a list");
    assert_has_msg(tuple_t, "[4, 5]", "Expected a string");
    assert_ok(tuple_t, r#"[10, "test"]"#);
    assert_has_msg(tuple_t, r#"["foo", "test"]"#, "Expected an integer");
    assert_has_msg(tuple_t, "[10]", "collection length in range 2");

    let nested_t = "Outer<string> type Inner<B> = struct { bar: B } type Outer<A> = struct { foo: Inner<[A]> }";
    assert_has_msg(nested_t, r#"{"foo": 3}"#, "Expected an object");
    assert_has_msg(nested_t, r#"{"foo": {"bar": "hello"}}"#, "Expected a list");
    assert_ok(nested_t, r#"{"foo": {"bar": ["hello"]}}"#);
    assert_has_msg(nested_t, r#"{"foo": {"bar": [2]}}"#, "Expected a string");

    // References + EmptyUnion spread
    let ref_empty = "struct { foo: string, ...( struct { bar: string } | EmptyUnion ) } type EmptyUnion = ()";
    assert_ok(ref_empty, r#"{"foo": "hello", "bar": "world"}"#);
    assert_has_msg(ref_empty, r#"{"foo": "hi", "bar": 1}"#, "Expected a string");

    // Dispatchers + filtered dispatch entry
    let disp = r#"struct { id: string, tag?: minecraft:item[[id]] } dispatch minecraft:item[elytra] to struct { Damage: double } #[until="1.20"] dispatch minecraft:item[filtered] to struct { Test: string }"#;
    assert_ok(disp, r#"{"id": "diamond"}"#);
    assert_has_msg(disp, r#"{"id": "elytra", "tag": {}}"#, r#"Missing required key "Damage""#);
    assert_ok(disp, r#"{"id": "elytra", "tag": {"Damage": 20}}"#);
    assert_has_msg(disp, r#"{"id": "filtered", "tag": {"A": "abc"}}"#, "Unexpected value");
}

#[test]
fn validates_integer_bounds_fabric_conditions_text_component_and_partner_bridge() {
    // #[integer(min=1, max=128)]
    let icon_map = "struct { [#[integer(min=1, max=128)] string]: string }";
    assert_ok(icon_map, r#"{"64": "icon.png"}"#);
    assert_has_msg(icon_map, r#"{"0": "icon.png"}"#, "in range 1..128");
    assert_has_msg(icon_map, r#"{"256": "icon.png"}"#, "in range 1..128");
    assert_has_msg(icon_map, r#"{"abc": "icon.png"}"#, "Expected an integer string");

    // #[text_component] recursive JSON check
    let mut tables = SymbolTables::default();
    parse_and_bind(
        r#"
type Text = (string | struct { text: string, bold?: boolean })
"#,
        "::java::util::text",
        &mut tables,
    )
    .unwrap();
    parse_and_bind(
        r#"
struct Holder { label: #[text_component] string }
"#,
        "::test",
        &mut tables,
    )
    .unwrap();
    let regs = RegistryIndex::default();
    let version = VersionContext { minecraft: Some("1.21.1".into()), neoforge: None };
    let env = CheckEnv { tables: &tables, registries: &regs, version: &version, require_canonical: false, allow_missing_keys: false, primitives_read_as_strings: false, ignored_loaders: &[] };
    let holder_ty = McdocType::Reference { path: Some("::test::Holder".into()), attributes: Vec::new() };

    let good_tc = json_parser::parse(r#"{"label": "{\"text\":\"hi\",\"bold\":true}"}"#, false);
    assert!(check_json_with_type(good_tc.root.as_ref().unwrap(), &holder_ty, &env).diagnostics.is_empty());

    let bad_tc = json_parser::parse(r#"{"label": "{\"text\":\"hi\",\"bold\":123}"}"#, false);
    let bad_diags = check_json_with_type(bad_tc.root.as_ref().unwrap(), &holder_ty, &env).diagnostics;
    assert!(bad_diags.iter().any(|d| d.message.contains("Expected a boolean")), "{bad_diags:?}");

    // Fabric load conditions + partner recipe bridge
    let mut partner_tables = SymbolTables::default();
    parse_and_bind(PACKRAVEN_MCDOC, "::packraven", &mut partner_tables).unwrap();
    parse_and_bind(
        r#"
dispatch minecraft:resource["recipe"] to struct Recipe {
	type: #[id="recipe_serializer"] string,
	...minecraft:recipe_serializer[[type]],
}
dispatch create:recipes[create:cutting] to struct Cutting {
	processing_time: int @ 1..,
}
"#,
        "::partner::create",
        &mut partner_tables,
    )
    .unwrap();
    bridge_partner_dispatchers(&mut partner_tables, &[]);

    let mut pregs = RegistryIndex::default();
    pregs.insert_entries("recipe_serializer", ["create:cutting".into()]);
    pregs.insert_entries("fabric:condition_codec", ["fabric:all_mods_loaded".into()]);
    pregs.finalize();
    let penv = CheckEnv { tables: &partner_tables, registries: &pregs, version: &version, require_canonical: false, allow_missing_keys: false, primitives_read_as_strings: false, ignored_loaders: &[] };
    let rty = resource_root_type("recipe", &partner_tables).unwrap();

    let valid_create = json_parser::parse(
        r#"{
            "fabric:load_conditions": [{ "condition": "fabric:all_mods_loaded", "values": ["create"] }],
            "type": "create:cutting",
            "processing_time": 50
        }"#,
        false,
    );
    let ok_diags = check_json_with_type(valid_create.root.as_ref().unwrap(), &rty, &penv).diagnostics;
    assert!(!ok_diags.iter().any(|d| d.severity == Severity::Error), "{ok_diags:?}");

    let bad_create = json_parser::parse(
        r#"{
            "fabric:load_conditions": [{ "condition": "fabric:all_mods_loaded" }],
            "type": "create:cutting",
            "processing_time": 0
        }"#,
        false,
    );
    let err_diags = check_json_with_type(bad_create.root.as_ref().unwrap(), &rty, &penv).diagnostics;
    assert!(err_diags.iter().any(|d| d.message.contains(r#"Missing required key "values""#)), "{err_diags:?}");
    assert!(err_diags.iter().any(|d| d.message.contains("in range 1..")), "{err_diags:?}");

    // NeoForge data path mapping
    assert_eq!(
        parse_data_file_path("data/pack/neoforge/biome_modifier/add_ores.json", &vanilla_registries()),
        Some(("neoforge:biome_modifier".into(), "pack:add_ores".into()))
    );
    assert_eq!(
        parse_data_file_path("data/neoforge/data_maps/item/furnace_fuels.json", &vanilla_registries()),
        Some(("neoforge:data_map_furnace_fuels".into(), "neoforge:item/furnace_fuels".into()))
    );
}

/// Loads a file from `PACKRAVEN_MCDOCS_DIR` when set, otherwise fetches and caches it from `muon-rw/mcdocs`.
fn load_muon_mcdoc_for_test(file: &str) -> String {
    if let Ok(dir) = std::env::var("PACKRAVEN_MCDOCS_DIR") {
        return std::fs::read_to_string(std::path::Path::new(&dir).join(file))
            .unwrap_or_else(|e| panic!("reading {file} from PACKRAVEN_MCDOCS_DIR ({dir}): {e}"));
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(super::fetch::load_muon_mcdoc(file, false))
        .unwrap_or_else(|e| panic!("fetching/caching {file} from muon-rw/mcdocs: {e:#}"))
}

fn load_muon_manifest_for_test() -> super::fetch::MuonManifest {
    if let Ok(dir) = std::env::var("PACKRAVEN_MCDOCS_DIR") {
        let raw = std::fs::read_to_string(std::path::Path::new(&dir).join("manifest.json"))
            .unwrap_or_else(|e| panic!("reading manifest.json from PACKRAVEN_MCDOCS_DIR ({dir}): {e}"));
        return serde_json::from_str(&raw).expect("parsing local muon-rw/mcdocs manifest.json");
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(super::fetch::load_muon_manifest(false))
        .unwrap_or_else(|e| panic!("fetching/caching manifest.json from muon-rw/mcdocs: {e:#}"))
}

fn apotheosis_test_tables() -> SymbolTables {
    let mut tables = SymbolTables::default();
    parse_and_bind(PACKRAVEN_MCDOC, "::packraven", &mut tables).unwrap();
    let src = load_muon_mcdoc_for_test("mcdoc/apotheosis.mcdoc");
    parse_and_bind(&src, "::apotheosis", &mut tables).unwrap();
    tables
}

fn apotheosis_test_registries(tables: &SymbolTables) -> RegistryIndex {
    let mut regs = RegistryIndex::default();
    regs.insert_entries(
        "apotheosis:rarities",
        [
            "apotheosis:common".into(),
            "apotheosis:uncommon".into(),
            "apotheosis:rare".into(),
            "apotheosis:epic".into(),
            "apotheosis:mythic".into(),
        ],
    );
    regs.insert_entries(
        "apotheosis:affixes",
        ["apotheosis:melee/attribute/vampiric".into(), "apotheosis:breaker/ability/sandforming".into()],
    );
    regs.insert_entries("apotheosis:gems", ["apotheosis:core/splendor".into(), "apotheosis:core/ballast".into()]);
    regs.insert_entries("apotheosis:affix_loot_entries", ["apotheosis:bow".into()]);
    regs.insert_entries("placebo:gear_sets", ["apotheosis:ascent/diamond".into()]);
    regs.insert_entries(
        "tag/placebo:gear_sets",
        [
            "apotheosis:haven_ranged".into(),
            "apotheosis:frontier_ranged".into(),
            "apotheosis:pinnacle_melee".into(),
        ],
    );
    regs.insert_entries(
        "item",
        [
            "minecraft:bow".into(),
            "minecraft:diamond_sword".into(),
            "minecraft:iron_sword".into(),
            "minecraft:shulker_shell".into(),
            "minecraft:raw_gold".into(),
            "apotheosis:mysterious_scrap_metal".into(),
            "apotheosis:godforged_pearl".into(),
        ],
    );
    regs.insert_entries("tag/item", ["c:raw_materials/copper".into()]);
    regs.insert_entries("tag/block", ["apotheosis:stoneforming_candidates".into()]);
    regs.insert_entries(
        "attribute",
        [
            "minecraft:generic.armor".into(),
            "minecraft:armor".into(),
            "minecraft:generic.attack_damage".into(),
            "minecraft:attack_damage".into(),
            "minecraft:generic.max_health".into(),
            "minecraft:max_health".into(),
            "minecraft:generic.scale".into(),
            "minecraft:scale".into(),
        ],
    );
    regs.insert_entries(
        "mob_effect",
        ["minecraft:fire_resistance".into(), "minecraft:wither".into(), "minecraft:strength".into()],
    );
    regs.insert_entries("entity_type", ["minecraft:bogged".into(), "minecraft:goat".into(), "minecraft:sheep".into(), "minecraft:husk".into()]);
    regs.insert_entries("tag/entity_type", ["minecraft:skeletons".into()]);
    regs.insert_entries("dimension", ["minecraft:overworld".into(), "minecraft:the_end".into()]);
    regs.insert_entries("dimension_type", ["minecraft:overworld".into(), "twilightforest:twilight_forest_type".into()]);
    regs.insert_entries("loot_table", ["apotheosis:entity/boss_drops".into(), "apotheosis:chests/spawner_brutal".into()]);
    regs.insert_entries("loot_condition_type", ["apotheosis:matches_block".into()]);
    regs.insert_entries("sound_event", ["minecraft:intentionally_empty".into(), "apotheosis:invader_mythic".into()]);
    let version = VersionContext { minecraft: Some("1.21.1".into()), neoforge: Some("21.1.265".into()) };
    super::seed_condition_registries(&mut regs, tables, &version);
    regs
}

fn check_apotheosis_file(tables: &SymbolTables, regs: &RegistryIndex, mc: &str, path: &str, json_src: &str) -> Vec<Diagnostic> {
    let (cat, _) = parse_data_file_path(path, &vanilla_registries()).unwrap_or_else(|| panic!("unrecognized path {path}"));
    let root_type = resource_root_type(&cat, tables).unwrap_or_else(|| panic!("no root type for {cat}"));
    let parsed = json_parser::parse(json_src, false);
    assert!(parsed.diagnostics.is_empty(), "invalid JSON for {path}: {:?}", parsed.diagnostics);
    let version = VersionContext { minecraft: Some(mc.into()), neoforge: Some("21.1.265".into()) };
    let env = CheckEnv { tables, registries: regs, version: &version, require_canonical: false, allow_missing_keys: false, primitives_read_as_strings: false, ignored_loaders: &[] };
    check_json_with_type(parsed.root.as_ref().unwrap(), &root_type, &env).diagnostics
}

#[test]
fn parses_apotheosis_mcdoc_and_maps_datapack_paths() {
    let tables = apotheosis_test_tables();
    let resources = &tables.dispatchers["minecraft:resource"];
    for cat in [
        "apotheosis:affixes",
        "apotheosis:gems",
        "apotheosis:extra_gem_bonuses",
        "apotheosis:purity_weights",
        "apotheosis:affix_loot_entries",
        "apotheosis:rarities",
        "apotheosis:rarity_override",
        "apotheosis:apothic_invaders",
        "apotheosis:apothic_elites",
        "apotheosis:apothic_augments",
        "apotheosis:rogue_spawners",
        "apotheosis:tier_augments",
        "placebo:gear_sets",
        "apotheosis:data_map_invader_spawn_rules",
        "apotheosis:data_map_loot_category_overrides",
    ] {
        assert!(resources.contains_key(cat), "missing resource dispatch for {cat}");
    }
    assert_eq!(tables.dispatchers["apotheosis:affix"].len(), 20);
    assert_eq!(tables.dispatchers["apotheosis:gem_bonus"].len(), 18);
    assert_eq!(tables.dispatchers["apotheosis:loot_rule"].len(), 7);
    assert_eq!(tables.dispatchers["apotheosis:spawn_condition"].len(), 9);
    assert_eq!(tables.dispatchers["apotheosis:entity_modifier"].len(), 4);
    assert_eq!(tables.dispatchers["apotheosis:tier_augment"].len(), 2);

    // Both 1.21 flat paths and 26.1 namespaced paths resolve to the same categories.
    for (path, expected_cat, expected_id) in [
        ("data/apotheosis/affixes/armor/attribute/adamantine.json", "apotheosis:affixes", "apotheosis:armor/attribute/adamantine"),
        ("data/apotheosis/apotheosis/affixes/armor/attribute/adamantine.json", "apotheosis:affixes", "apotheosis:armor/attribute/adamantine"),
        ("data/pack/gems/core/ballast.json", "apotheosis:gems", "pack:core/ballast"),
        ("data/pack/apotheosis/gems/core/ballast.json", "apotheosis:gems", "pack:core/ballast"),
        ("data/pack/extra_gem_bonuses/splendor.json", "apotheosis:extra_gem_bonuses", "pack:splendor"),
        ("data/pack/apotheosis/extra_gem_bonuses/splendor.json", "apotheosis:extra_gem_bonuses", "pack:splendor"),
        ("data/apotheosis/purity_weights/weights.json", "apotheosis:purity_weights", "apotheosis:weights"),
        ("data/apotheosis/apotheosis/purity_weights/weights.json", "apotheosis:purity_weights", "apotheosis:weights"),
        ("data/apotheosis/affix_loot_entries/bow.json", "apotheosis:affix_loot_entries", "apotheosis:bow"),
        ("data/apotheosis/rarities/mythic.json", "apotheosis:rarities", "apotheosis:mythic"),
        ("data/apotheosis/rarity_override/apotheosis/shears.json", "apotheosis:rarity_override", "apotheosis:apotheosis/shears"),
        ("data/apotheosis/apothic_invaders/overworld/bogged.json", "apotheosis:apothic_invaders", "apotheosis:overworld/bogged"),
        ("data/apotheosis/apothic_elites/overworld/craig.json", "apotheosis:apothic_elites", "apotheosis:overworld/craig"),
        ("data/apotheosis/apothic_augments/random_affix_items.json", "apotheosis:apothic_augments", "apotheosis:random_affix_items"),
        ("data/apotheosis/rogue_spawners/brutal/husk.json", "apotheosis:rogue_spawners", "apotheosis:brutal/husk"),
        ("data/apotheosis/tier_augments/ascent/armor.json", "apotheosis:tier_augments", "apotheosis:ascent/armor"),
        ("data/apotheosis/apotheosis/tier_augments/ascent/armor.json", "apotheosis:tier_augments", "apotheosis:ascent/armor"),
        ("data/apotheosis/gear_sets/ascent/diamond.json", "placebo:gear_sets", "apotheosis:ascent/diamond"),
        ("data/apotheosis/placebo/gear_sets/ascent/diamond.json", "placebo:gear_sets", "apotheosis:ascent/diamond"),
        ("data/apotheosis/tags/placebo/gear_sets/ascent_melee.json", "tag/placebo:gear_sets", "apotheosis:ascent_melee"),
        ("data/apotheosis/data_maps/dimension_type/invader_spawn_rules.json", "apotheosis:data_map_invader_spawn_rules", "apotheosis:dimension_type/invader_spawn_rules"),
        ("data/apotheosis/data_maps/item/loot_category_overrides.json", "apotheosis:data_map_loot_category_overrides", "apotheosis:item/loot_category_overrides"),
    ] {
        assert_eq!(parse_data_file_path(path, &vanilla_registries()), Some((expected_cat.into(), expected_id.into())), "path={path}");
    }
}

#[test]
fn validates_real_apotheosis_samples_across_1_21_1_and_26_1() {
    let tables = apotheosis_test_tables();
    let regs = apotheosis_test_registries(&tables);

    let valid_121_cases: &[(&str, &str)] = &[
        (
            "data/apotheosis/affixes/armor/attribute/adamantine.json",
            r##"{
                "type": "apotheosis:attribute",
                "attribute": "minecraft:generic.armor",
                "categories": ["apotheosis:chestplate", "apotheosis:leggings"],
                "definition": {
                    "affix_type": "stat",
                    "exclusive_set": [],
                    "weights": { "quality": 0.1, "weight": 25 }
                },
                "operation": "add_multiplied_base",
                "values": {
                    "apotheosis:rare": { "min": 0.15, "max": 0.3 },
                    "apotheosis:mythic": { "min": 0.25, "max": 0.4 }
                }
            }"##,
        ),
        (
            "data/apotheosis/affixes/breaker/ability/stoneforming.json",
            r##"{
                "type": "apotheosis:stoneforming",
                "candidates": "#apotheosis:stoneforming_candidates",
                "categories": ["apotheosis:breaker"],
                "definition": {
                    "affix_type": "ability",
                    "exclusive_set": ["apotheosis:breaker/ability/sandforming"],
                    "weights": { "quality": 0.1, "weight": 25 }
                }
            }"##,
        ),
        (
            "data/apotheosis/gems/core/ballast.json",
            r##"{
                "type": "apotheosis:gem",
                "weights": { "quality": 0.1, "weight": 100 },
                "bonuses": [
                    {
                        "type": "apotheosis:attribute",
                        "attribute": "minecraft:generic.attack_damage",
                        "gem_class": { "key": "light_weapon", "types": ["apotheosis:melee_weapon", "apotheosis:trident"] },
                        "operation": "add_value",
                        "values": { "cracked": 1.0, "perfect": 10.0 }
                    },
                    {
                        "type": "apotheosis:drop_transform",
                        "conditions": [{ "condition": "apotheosis:matches_block", "valid_blocks": "#c:ores/copper" }],
                        "desc": "gem.apotheosis:overworld/royalty.bonus.pickaxe",
                        "gem_class": "apotheosis:breaker",
                        "inputs": { "tag": "c:raw_materials/copper" },
                        "output": { "count": 1, "id": "minecraft:raw_gold" },
                        "values": { "flawed": 0.15, "perfect": 0.4 }
                    }
                ]
            }"##,
        ),
        (
            "data/apotheosis/affix_loot_entries/bow.json",
            r##"{
                "type": "apotheosis:affix_loot_entry",
                "stack": { "count": 1, "id": "minecraft:bow" },
                "weights": { "quality": 1.0, "weight": 15 }
            }"##,
        ),
        (
            "data/apotheosis/rarities/mythic.json",
            r##"{
                "type": "apotheosis:rarity",
                "color": "#ED7014",
                "invader_sound": "apotheosis:invader_mythic",
                "material": "apotheosis:godforged_pearl",
                "render_data": {
                    "beam_height": 3.5,
                    "beam_radius": 0.035,
                    "beam_texture": "apotheosis:textures/rarity/beam.png",
                    "glow_radius": 0.065,
                    "glow_texture": "apotheosis:textures/rarity/glow.png",
                    "particle": { "enabled": true },
                    "shadow": { "alpha": 255, "frame_time": 1.5, "frames": 7, "size": 0.4, "texture": "apotheosis:textures/rarity/shadow_t4.png" }
                },
                "rules": [
                    { "type": "apotheosis:affix", "affix_type": "stat" },
                    { "type": "apotheosis:socket", "min": 1, "max": 3 },
                    {
                        "type": "apotheosis:select",
                        "chance": 0.99,
                        "if_true": { "type": "apotheosis:durability", "min": 0.45, "max": 0.75 },
                        "if_false": {
                            "type": "apotheosis:component",
                            "components": { "!apotheosis:durability_bonus": {}, "minecraft:unbreakable": {} }
                        }
                    }
                ],
                "sort_index": 700,
                "weights": { "pinnacle": { "weight": 200 } }
            }"##,
        ),
        (
            "data/apotheosis/apothic_invaders/overworld/bogged.json",
            r##"{
                "type": "apotheosis:invader",
                "basic_data": {
                    "bonus_loot": ["apotheosis:entity/boss_drops"],
                    "constraints": { "dimensions": ["minecraft:overworld"] },
                    "name": "use_name_generation",
                    "valid_gear_sets": { "haven": ["#haven_ranged"], "frontier": ["#frontier_ranged"] },
                    "weights": { "frontier": { "quality": 0.1, "weight": 50 } }
                },
                "entity": "minecraft:bogged",
                "size": { "height": 2.4, "width": 0.9 },
                "stats": {
                    "apotheosis:mythic": {
                        "attribute_modifiers": [
                            { "attribute": "minecraft:generic.max_health", "operation": "add_value", "value": { "min": 90.0, "max": 140.0 } }
                        ],
                        "effects": [{ "amplifier": 1.0, "effect": "minecraft:fire_resistance" }],
                        "enchant_chance": 1.0,
                        "enchantment_levels": { "primary": 100, "secondary": 80 }
                    }
                }
            }"##,
        ),
        (
            "data/apotheosis/apothic_elites/overworld/craig.json",
            r##"{
                "type": "apotheosis:elite",
                "basic_data": {
                    "constraints": { "dimensions": ["minecraft:overworld"] },
                    "name": { "color": "rainbow", "translate": "elite.apotheosis.craig" },
                    "nbt": { "IsScreamingGoat": 1 },
                    "spawn_conditions": [{
                        "type": "apotheosis:not",
                        "spawn_condition": { "type": "apotheosis:spawn_type", "spawn_types": ["spawn_egg"] }
                    }],
                    "supporting_entities": [{ "entity": "minecraft:sheep", "nbt": { "CustomName": "jeb_" } }],
                    "valid_gear_sets": { "pinnacle": ["#pinnacle_melee"] },
                    "weights": { "pinnacle": { "quality": 0.1, "weight": 100 } }
                },
                "entities": "minecraft:goat",
                "stats": {
                    "attribute_modifiers": [{ "attribute": "minecraft:generic.scale", "operation": "add_value", "value": 3.0 }],
                    "effects": [{ "amplifier": 1.0, "effect": "minecraft:fire_resistance" }],
                    "enchant_chance": 1.0,
                    "enchantment_levels": { "primary": 100, "secondary": 100 }
                },
                "success_chance": 0.005
            }"##,
        ),
        (
            "data/apotheosis/rogue_spawners/brutal/husk.json",
            r##"{
                "type": "apotheosis:rogue_spawner",
                "loot_table": "apotheosis:chests/spawner_brutal",
                "spawn_potentials": [{
                    "data": { "entity": { "Health": 30.0, "id": "minecraft:husk" } },
                    "weight": 10
                }],
                "stats": {
                    "apothic_spawners:max_delay": 125,
                    "apothic_spawners:min_delay": 50,
                    "apothic_spawners:req_player_range": 14,
                    "apothic_spawners:spawn_count": 2,
                    "apothic_spawners:spawn_range": 7,
                    "apothic_spawners:youthful": true
                },
                "weight": 30
            }"##,
        ),
        (
            "data/apotheosis/tier_augments/ascent/armor.json",
            r##"{
                "type": "apotheosis:attribute",
                "modifier": { "attribute": "minecraft:generic.armor", "operation": "add_value", "value": 8.0 },
                "modifier_id": "apotheosis:ascent/armor",
                "sort_index": 100,
                "target": "monsters",
                "tier": "ascent"
            }"##,
        ),
        (
            "data/apotheosis/apothic_augments/random_affix_items.json",
            r##"{
                "type": "apotheosis:augment",
                "application_chance": 0.12,
                "conditions": [
                    {
                        "type": "apotheosis:not",
                        "spawn_condition": { "type": "apotheosis:spawn_type", "spawn_types": ["spawner", "trial_spawner"] }
                    },
                    { "type": "apotheosis:is_monster" }
                ],
                "modifiers": [{ "type": "apotheosis:random_affix_item" }]
            }"##,
        ),
        (
            "data/apotheosis/data_maps/dimension_type/invader_spawn_rules.json",
            r##"{
                "values": {
                    "minecraft:overworld": {
                        "spawn_chances": { "haven": 0.0, "frontier": 0.018, "ascent": 0.02, "summit": 0.025, "pinnacle": 0.03 },
                        "surface_type": "needs_sky_or_same_vertical_slice",
                        "cursed": false,
                        "auto_aggro": true
                    },
                    "twilightforest:twilight_forest_type": {
                        "neoforge:conditions": [{ "type": "neoforge:mod_loaded", "modid": "twilightforest" }],
                        "spawn_chances": { "haven": 0.0, "frontier": 0.05, "ascent": 0.053, "summit": 0.06, "pinnacle": 0.063 },
                        "surface_type": "needs_surface"
                    }
                }
            }"##,
        ),
        (
            "data/apotheosis/data_maps/item/loot_category_overrides.json",
            r##"{
                "values": {
                    "minecraft:iron_sword": "apotheosis:melee_weapon",
                    "minecraft:shulker_shell": "apotheosis:none"
                }
            }"##,
        ),
        (
            "data/apotheosis/gear_sets/ascent/diamond.json",
            r##"{
                "type": "placebo:gear_set",
                "weight": 100,
                "quality": 5.0,
                "mainhands": [{ "weight": 10, "stack": { "count": 1, "id": "minecraft:diamond_sword" } }],
                "tags": ["ascent_melee"]
            }"##,
        ),
    ];

    for (path, json_src) in valid_121_cases {
        let diags = check_apotheosis_file(&tables, &regs, "1.21.1", path, json_src);
        assert!(diags.is_empty(), "expected 0 diagnostics for 1.21.1 {path}, got: {diags:?}");
    }

    // Verify 26.1 schema differences: namespaced paths, omitted `type`, HolderSet `valid_gear_sets`,
    // `spawn_item_use`, and `modifier_id` moved inside `modifier` on attribute tier augments.
    let valid_261_cases: &[(&str, &str)] = &[
        (
            "data/apotheosis/apotheosis/tier_augments/ascent/armor.json",
            r##"{
                "type": "apotheosis:attribute",
                "modifier": {
                    "attribute": "minecraft:armor",
                    "modifier_id": "apotheosis:ascent/armor",
                    "operation": "add_value",
                    "value": 8.0
                },
                "sort_index": 100,
                "target": "monsters",
                "tier": "ascent"
            }"##,
        ),
        (
            "data/apotheosis/apotheosis/apothic_elites/overworld/craig.json",
            r##"{
                "basic_data": {
                    "constraints": { "dimensions": ["minecraft:overworld"] },
                    "name": { "color": "rainbow", "translate": "elite.apotheosis.craig" },
                    "spawn_conditions": [{
                        "type": "apotheosis:not",
                        "spawn_condition": { "type": "apotheosis:spawn_type", "spawn_types": ["spawn_item_use"] }
                    }],
                    "valid_gear_sets": { "pinnacle": "#apotheosis:pinnacle_melee" },
                    "weights": { "pinnacle": { "quality": 0.1, "weight": 100 } }
                },
                "entities": "minecraft:goat",
                "stats": {
                    "attribute_modifiers": [{ "attribute": "minecraft:scale", "operation": "add_value", "value": 3.0 }],
                    "enchant_chance": 1.0,
                    "enchantment_levels": { "primary": 100, "secondary": 100 }
                },
                "success_chance": 0.005
            }"##,
        ),
        (
            "data/apotheosis/apotheosis/affix_loot_entries/bow.json",
            r##"{
                "stack": { "id": "minecraft:bow" },
                "weights": { "quality": 1.0, "weight": 15 }
            }"##,
        ),
    ];

    for (path, json_src) in valid_261_cases {
        let diags = check_apotheosis_file(&tables, &regs, "26.1", path, json_src);
        assert!(diags.is_empty(), "expected 0 diagnostics for 26.1 {path}, got: {diags:?}");
    }

    // Unknown addon affix types stay tolerant (no false errors on custom fields).
    let addon_affix = check_apotheosis_file(
        &tables,
        &regs,
        "1.21.1",
        "data/myaddon/affixes/custom.json",
        r##"{
            "type": "myaddon:custom_affix",
            "definition": { "affix_type": "ability", "exclusive_set": [], "weights": { "weight": 10 } },
            "custom_param": 42
        }"##,
    );
    assert!(!addon_affix.iter().any(|d| d.severity == Severity::Error), "addon affix should not error: {addon_affix:?}");

    // Invalid samples produce expected errors.
    let bad_affix = check_apotheosis_file(
        &tables,
        &regs,
        "1.21.1",
        "data/pack/affixes/broken.json",
        r##"{
            "type": "apotheosis:attribute",
            "attribute": "minecraft:generic.armor",
            "categories": ["apotheosis:chestplate"],
            "definition": { "affix_type": "invalid_pool", "exclusive_set": [], "weights": { "weight": 10 } },
            "operation": "multiply_wrong",
            "values": {}
        }"##,
    );
    assert!(bad_affix.iter().any(|d| d.severity == Severity::Error), "expected errors on bad affix: {bad_affix:?}");

    let bad_socket_rule = check_apotheosis_file(
        &tables,
        &regs,
        "1.21.1",
        "data/pack/rarities/broken.json",
        r##"{
            "color": "#FFFFFF",
            "material": "apotheosis:godforged_pearl",
            "weights": { "weight": 10 },
            "rules": [{ "type": "apotheosis:socket", "min": 0, "max": 99 }]
        }"##,
    );
    assert!(bad_socket_rule.iter().any(|d| d.message.contains("1..16")), "expected socket range error: {bad_socket_rule:?}");

    // 1.21 `spawn_egg` is rejected on 26.1, and missing `modifier_id` inside `modifier` is caught on 26.1.
    let bad_261_augment = check_apotheosis_file(
        &tables,
        &regs,
        "26.1",
        "data/apotheosis/apotheosis/tier_augments/ascent/armor.json",
        r##"{
            "type": "apotheosis:attribute",
            "modifier": { "attribute": "minecraft:armor", "operation": "add_value", "value": 8.0 },
            "modifier_id": "apotheosis:ascent/armor",
            "target": "monsters",
            "tier": "ascent"
        }"##,
    );
    assert!(
        bad_261_augment.iter().any(|d| d.message.contains(r#"Missing required key "modifier_id""#)),
        "expected nested modifier_id error on 26.1: {bad_261_augment:?}"
    );
    assert!(
        bad_261_augment.iter().any(|d| d.message.contains(r#"Unknown key "modifier_id""#)),
        "expected top-level modifier_id error on 26.1: {bad_261_augment:?}"
    );
}

#[test]
#[ignore]
fn parses_all_real_misode_partner_mcdoc_files() {
    let dir = std::env::var("PACKRAVEN_MISODE_MCDOC_DIR").expect("set PACKRAVEN_MISODE_MCDOC_DIR");
    let mut tables = SymbolTables::default();
    let mut count = 0usize;
    let mut failures = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap().flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("mcdoc") {
            continue;
        }
        let stem = path.file_stem().unwrap().to_string_lossy().to_string();
        let src = std::fs::read_to_string(&path).unwrap();
        count += 1;
        if let Err(err) = parse_and_bind(&src, &format!("::partner::{stem}"), &mut tables) {
            failures.push(format!("{stem}.mcdoc: {err}"));
        }
    }
    assert!(count >= 10, "expected misode mcdoc files in {dir}, found {count}");
    assert!(failures.is_empty(), "partner parse failures:\n{}", failures.join("\n"));
    eprintln!("Parsed all {count} misode partner .mcdoc files -> {} types, {} dispatchers", tables.mcdoc.len(), tables.dispatchers.len());
}

/// The vanilla registry names a real load reads from misode's mcmeta summary.
fn vanilla_registries() -> KnownRegistries {
    let mut known = KnownRegistries::default();
    for name in ["item", "block", "entity_type", "worldgen/biome", "recipe", "dimension_type", "mob_effect"] {
        known.insert(name);
    }
    known
}

#[test]
fn neoforge_custom_ingredients_use_the_key_each_version_reads() {
    let mut tables = SymbolTables::default();
    parse_and_bind("type Ingredient = struct { item: string }", "::java::data::recipe", &mut tables).unwrap();
    bridge_partner_dispatchers(&mut tables, &[]);
    let regs = RegistryIndex::default();
    let check = |minecraft: &str, json: &str| {
        let version = VersionContext { minecraft: Some(minecraft.into()), neoforge: Some("21.1.265".into()) };
        let env = CheckEnv {
            tables: &tables,
            registries: &regs,
            version: &version,
            require_canonical: false,
            allow_missing_keys: false,
            primitives_read_as_strings: false,
            ignored_loaders: &[],
        };
        let parsed = json_parser::parse(json, false);
        let root = McdocType::Reference { path: Some("::java::data::recipe::Ingredient".into()), attributes: Vec::new() };
        check_json_with_type(parsed.root.as_ref().unwrap(), &root, &env)
            .diagnostics
            .into_iter()
            .filter(|d| d.severity == Severity::Error)
            .count()
    };
    let old = r#"{ "type": "neoforge:components", "items": "x:y", "components": {} }"#;
    let new = r#"{ "neoforge:ingredient_type": "neoforge:components", "items": "x:y", "components": {} }"#;
    assert_eq!(check("1.21.1", old), 0);
    assert_eq!(check("26.1.2", new), 0);
    assert_eq!(check("1.21.1", new), 1);
}

#[test]
fn accepts_vanilla_types_a_mod_backports() {
    let mut tables = SymbolTables::default();
    parse_and_bind(
        r#"
type Feature = struct { type: #[id="worldgen/feature"] string, config: minecraft:feature_config[[type]] }
dispatch minecraft:feature_config[minecraft:fallen_tree] to #[since="1.21.5"] struct { log_length: int }
dispatch minecraft:feature_config[%unknown] to ()
"#,
        "::test",
        &mut tables,
    )
    .unwrap();
    let check = |backported: bool| {
        let mut regs = RegistryIndex::default();
        if backported {
            regs.insert_entries("worldgen/feature", ["minecraft:fallen_tree".into()]);
        }
        regs.finalize();
        let version = VersionContext { minecraft: Some("1.21.1".into()), neoforge: None };
        let env = CheckEnv {
            tables: &tables,
            registries: &regs,
            version: &version,
            require_canonical: false,
            allow_missing_keys: false,
            primitives_read_as_strings: false,
            ignored_loaders: &[],
        };
        let parsed = json_parser::parse(r#"{ "type": "minecraft:fallen_tree", "config": { "trunk": 1 } }"#, false);
        let root = McdocType::Reference { path: Some("::test::Feature".into()), attributes: Vec::new() };
        check_json_with_type(parsed.root.as_ref().unwrap(), &root, &env).diagnostics.into_iter().filter(|d| d.severity == Severity::Error).count()
    };
    assert!(check(false) > 0, "1.21.1 has no fallen_tree unless a mod adds it");
    assert_eq!(check(true), 0);
}

#[test]
fn picks_the_union_member_dfu_decodes_despite_extra_keys() {
    // Vanilla's HeightProvider: an inline vertical anchor, or a typed provider.
    let height = r#"(struct { absolute: int } | struct { above_bottom: int } | struct { type: "minecraft:constant", value: struct { absolute: int } })"#;
    let diags = run_case(height, r#"{ "type": "minecraft:constant", "absolute": 80 }"#);
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert_eq!(diags[0].severity, Severity::Warning);
    assert_eq!(diags[0].message, "Unknown key \"type\".");
    // With no member that decodes, the keys still conflict.
    assert_has_msg(height, r#"{ "absolute": 80, "above_bottom": 2 }"#, "Invalid key combination");
}

#[test]
fn state_predicates_with_values_a_block_lacks_load_but_never_match() {
    let mut tables = SymbolTables::default();
    parse_and_bind(
        "type BlockStateProperty = struct { block: string, properties?: mcdoc:block_states[[block]] }",
        "::java::data::loot::condition",
        &mut tables,
    )
    .unwrap();
    parse_and_bind("type BlockState = struct { Name: string, Properties?: mcdoc:block_states[[Name]] }", "::java::util", &mut tables).unwrap();
    super::checker::mark_block_state_predicates(&mut tables);
    let mut regs = RegistryIndex::default();
    regs.apply_vanilla_block_states(
        [("aether_ii:skyroot_twig".to_string(), [("twig_amount".to_string(), vec!["1".to_string(), "2".to_string()])].into_iter().collect())]
            .into_iter()
            .collect(),
    );
    let version = VersionContext { minecraft: Some("26.1.2".into()), neoforge: None };
    let env = CheckEnv {
        tables: &tables,
        registries: &regs,
        version: &version,
        require_canonical: false,
        allow_missing_keys: false,
        primitives_read_as_strings: false,
        ignored_loaders: &[],
    };
    let check = |path: &str, json: &str| {
        let parsed = json_parser::parse(json, false);
        check_json_with_type(parsed.root.as_ref().unwrap(), &McdocType::Reference { path: Some(path.into()), attributes: Vec::new() }, &env).diagnostics
    };
    // Aether II's skyroot_twig loot table still tests the twig amounts it used to have.
    let predicate = check("::java::data::loot::condition::BlockStateProperty", r#"{ "block": "aether_ii:skyroot_twig", "properties": { "twig_amount": "3" } }"#);
    assert_eq!(predicate.len(), 1, "{predicate:?}");
    assert_eq!(predicate[0].severity, Severity::Warning);
    assert_eq!(predicate[0].message, "This block has no \"3\" state for this property, so this never matches.");
    let state = check("::java::util::BlockState", r#"{ "Name": "aether_ii:skyroot_twig", "Properties": { "twig_amount": "3" } }"#);
    assert!(state.iter().any(|d| d.severity == Severity::Error), "{state:?}");
}

#[test]
fn loot_entries_accept_any_weight_like_vanilla() {
    let mut tables = SymbolTables::default();
    parse_and_bind("type SingletonPoolEntry = struct { weight?: int @ 1.., quality?: int }", "::java::data::loot", &mut tables).unwrap();
    super::checker::relax_loot_entry_weight(&mut tables);
    let regs = RegistryIndex::default();
    let version = VersionContext { minecraft: Some("26.1.2".into()), neoforge: None };
    let env = CheckEnv {
        tables: &tables,
        registries: &regs,
        version: &version,
        require_canonical: false,
        allow_missing_keys: false,
        primitives_read_as_strings: false,
        ignored_loaders: &[],
    };
    // Repurposed Structures disables an entry this way in its icy pyramid chests.
    let parsed = json_parser::parse(r#"{ "weight": 0 }"#, false);
    let root = McdocType::Reference { path: Some("::java::data::loot::SingletonPoolEntry".into()), attributes: Vec::new() };
    let diags = check_json_with_type(parsed.root.as_ref().unwrap(), &root, &env).diagnostics;
    assert!(diags.is_empty(), "{diags:?}");
}

#[test]
fn parses_manifest_loaders_and_new_modded_paths() {
    let manifest_json = r#"{
        "formatVersion": 1,
        "schemas": [{
            "id": "global_loot_modifiers",
            "file": "mcdoc/global_loot_modifiers.mcdoc",
            "module": "::global_loot_modifiers",
            "loaders": ["forge", "neoforge"],
            "modIds": ["porting_lib_loot"],
            "paths": [
                { "namespace": "neoforge", "exact": "loot_modifiers/global_loot_modifiers", "category": "neoforge:global_loot_modifiers" },
                { "folder": "loot_modifiers", "category": "neoforge:loot_modifier" }
            ]
        }]
    }"#;
    let manifest: super::fetch::MuonManifest = serde_json::from_str(manifest_json).unwrap();
    assert_eq!(manifest.schemas[0].loaders, vec!["forge", "neoforge"]);

    let known = KnownRegistries::default();
    assert_eq!(
        parse_data_file_path("data/neoforge/loot_modifiers/global_loot_modifiers.json", &known),
        Some(("neoforge:global_loot_modifiers".into(), "neoforge:loot_modifiers/global_loot_modifiers".into()))
    );
    assert_eq!(
        parse_data_file_path("data/hazennstuff/irons_jewelry/material/archery_rune.json", &known),
        Some(("irons_jewelry:material".into(), "hazennstuff:archery_rune".into()))
    );
    assert_eq!(
        parse_data_file_path("data/irons_spellbooks/irons_spellbooks/upgrade_orb_type/blood_power.json", &known),
        Some(("irons_spellbooks:upgrade_orb_type".into(), "irons_spellbooks:blood_power".into()))
    );
    assert_eq!(
        parse_data_file_path("data/apotheosis/wanderer_trades/beacon.json", &known),
        Some(("placebo:wanderer_trades".into(), "apotheosis:beacon".into()))
    );
    assert_eq!(
        parse_data_file_path("data/apothic_attributes/brewing_mixes/flying.json", &known),
        Some(("placebo:brewing_mixes".into(), "apothic_attributes:flying".into()))
    );
    assert_eq!(
        parse_data_file_path("data/apothic_enchanting/enchanting_stats/amethyst_cluster.json", &known),
        Some(("apothic_enchanting:enchanting_stats".into(), "apothic_enchanting:amethyst_cluster".into()))
    );
}

#[test]
fn parses_all_muon_mcdocs_from_manifest() {
    let manifest = load_muon_manifest_for_test();
    assert!(!manifest.schemas.is_empty(), "muon-rw/mcdocs manifest has no schemas");
    let mut tables = SymbolTables::default();
    parse_and_bind(PACKRAVEN_MCDOC, "::packraven", &mut tables).unwrap();
    for entry in &manifest.schemas {
        let src = load_muon_mcdoc_for_test(&entry.file);
        parse_and_bind(&src, &entry.module, &mut tables)
            .unwrap_or_else(|e| panic!("failed to bind {} ({}): {e}", entry.id, entry.file));
    }
}

#[test]
fn validates_apotheosis_trades_mixes_stats_and_spawner_recipes() {
    let tables = apotheosis_test_tables();
    if !tables.dispatchers.get("minecraft:resource").is_some_and(|r| r.contains_key("placebo:wanderer_trades")) {
        return;
    }
    let mut regs = RegistryIndex::default();
    regs.insert_entries("item", ["minecraft:clock".into(), "minecraft:beacon".into(), "minecraft:emerald".into()]);
    regs.insert_entries("block", ["minecraft:amethyst_cluster".into()]);
    regs.insert_entries("potion", ["minecraft:water".into(), "minecraft:awkward".into()]);
    regs.finalize();
    let version = VersionContext { minecraft: Some("1.21.1".into()), neoforge: Some("21.1.265".into()) };
    let env = CheckEnv {
        tables: &tables,
        registries: &regs,
        version: &version,
        require_canonical: false,
        allow_missing_keys: false,
        primitives_read_as_strings: false,
        ignored_loaders: &[],
    };

    for (cat, json) in [
        (
            "placebo:wanderer_trades",
            r#"{ "type": "placebo:basic_trade", "input_1": { "id": "minecraft:emerald", "count": 25 }, "output": { "id": "minecraft:beacon", "count": 1 }, "rare": true, "xp": 50 }"#,
        ),
        (
            "placebo:brewing_mixes",
            r#"{ "mix_type": "potion", "from": "minecraft:water", "ingredient": { "item": "minecraft:clock" }, "to": "minecraft:awkward" }"#,
        ),
        (
            "apothic_enchanting:enchanting_stats",
            r#"{ "block": "minecraft:amethyst_cluster", "stats": { "maxEterna": 40, "eterna": 1, "quanta": -1 } }"#,
        ),
    ] {
        let root = resource_root_type(cat, &tables).unwrap_or_else(|| panic!("missing root for {cat}"));
        let parsed = json_parser::parse(json, false);
        let diags = check_json_with_type(parsed.root.as_ref().unwrap(), &root, &env).diagnostics;
        assert!(diags.is_empty(), "{cat} failed: {diags:?}");
    }

    // Spawner modifier recipe via minecraft:recipe_serializer
    let spawner_schema = tables.dispatchers.get("minecraft:recipe_serializer").unwrap().get("apothic_spawners:spawner_modifier").unwrap();
    let spawner_json = r#"{ "mainhand": { "item": "minecraft:clock" }, "stat_changes": [{ "type": "apothic_spawners:max_delay", "value": -10, "min": 100, "max": -1 }] }"#;
    let parsed = json_parser::parse(spawner_json, false);
    let diags = check_json_with_type(parsed.root.as_ref().unwrap(), spawner_schema, &env).diagnostics;
    assert!(diags.is_empty(), "spawner_modifier failed: {diags:?}");
}
