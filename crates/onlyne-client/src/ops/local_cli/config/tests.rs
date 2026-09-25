use super::*;
use crate::ops::local_cli::fixtures::plugin_ids;

#[test]
fn fold_plugin_blocks_removes_each_owned_block_and_reports_ids_in_order() {
    let text = r#"plugins = []
[[plugin]]
id = "demo"
path = "agent/demo"
[[plugin]]
id = "beta"
"#;
    let (out, ids, folded) = fold_plugin_blocks(text, None);
    assert_eq!(out, "plugins = []\n");
    assert_eq!(ids, plugin_ids(&["demo", "beta"]));
    assert_eq!(folded, 2);
}

#[test]
fn fold_plugin_blocks_only_removes_the_requested_id() {
    let text = r#"plugins = []
[[plugin]]
id = "demo"
[[plugin]]
id = "beta"
"#;
    let (out, ids, folded) = fold_plugin_blocks(text, Some("demo"));
    assert_eq!(
        out,
        r#"plugins = []
[[plugin]]
id = "beta"
"#
    );
    assert_eq!(ids, plugin_ids(&["demo"]));
    assert_eq!(folded, 1);
}

#[test]
fn fold_plugin_blocks_keeps_blocks_without_a_parsable_id() {
    let text = r#"plugins = []
[[plugin]]
path = "agent/demo"
[[plugin]]
id = "beta"
"#;
    let (out, ids, folded) = fold_plugin_blocks(text, None);
    assert_eq!(
        out,
        r#"plugins = []
[[plugin]]
path = "agent/demo"
"#
    );
    assert_eq!(ids, plugin_ids(&["beta"]));
    assert_eq!(folded, 1);
}

#[test]
fn fold_plugin_blocks_counts_duplicate_ids_without_repeating_them() {
    let text = r#"plugins = []
[[plugin]]
id = "demo"
[[plugin]]
id = "demo"
"#;
    let (out, ids, folded) = fold_plugin_blocks(text, None);
    assert_eq!(out, "plugins = []\n");
    assert_eq!(ids, plugin_ids(&["demo"]));
    assert_eq!(folded, 2);
}

#[test]
fn fold_plugin_blocks_folds_headers_with_spaces_and_trailing_comments() {
    let text = r#"plugins = []
[[ plugin ]] # legacy demo
id = "demo"
[[plugin]] # legacy beta
id = "beta"
"#;
    let (out, ids, folded) = fold_plugin_blocks(text, None);
    assert_eq!(out, "plugins = []\n");
    assert_eq!(ids, plugin_ids(&["demo", "beta"]));
    assert_eq!(folded, 2);
}

#[test]
fn find_plugins_ignores_keys_inside_tables() {
    let top_level = r#"plugins = ["demo"]
[table]
plugins = ["wrong"]
"#;
    assert_eq!(
        find_plugins(top_level).unwrap().unwrap().primary.ids,
        plugin_ids(&["demo"])
    );
    assert!(
        find_plugins("[table]\nplugins = [\"wrong\"]\n")
            .unwrap()
            .is_none()
    );
}

#[test]
fn parse_inline_array_reads_plain_ids_and_keeps_the_tail() {
    let (ids, tail) = parse_inline_array(r#"["a", "b"]   # keep"#).unwrap();
    assert_eq!(ids, plugin_ids(&["a", "b"]));
    assert_eq!(tail, "   # keep");
    let (empty, empty_tail) = parse_inline_array("[]").unwrap();
    assert!(empty.is_empty());
    assert_eq!(empty_tail, "");
}

#[test]
fn set_plugins_replaces_the_existing_line_and_preserves_its_tail() {
    let text = "role = \"planner\"\n  plugins = [\"old\"]   # keep\n";
    let updated = set_plugins(text, &plugin_ids(&["new"])).unwrap();
    assert_eq!(
        updated,
        "role = \"planner\"\n  plugins = [\"new\"]   # keep\n"
    );
}

#[test]
fn set_plugins_inserts_a_top_level_array_before_table_keys() {
    let text = "role = \"planner\"\n[other]\nkey_path = \"ignored\"\n";
    let updated = set_plugins(text, &plugin_ids(&["demo"])).unwrap();
    assert_eq!(
        updated,
        "role = \"planner\"\nplugins = [\"demo\"]\n[other]\nkey_path = \"ignored\"\n"
    );
    let value: toml::Value = updated.parse().unwrap();
    let root = value.as_table().unwrap();
    assert_eq!(root.get("plugins").unwrap().as_array().unwrap().len(), 1);
    assert_eq!(
        root.get("other")
            .and_then(|other| other.get("key_path"))
            .and_then(toml::Value::as_str),
        Some("ignored")
    );
}

#[test]
fn set_plugins_terminates_the_config_with_one_newline() {
    let updated = set_plugins("plugins = []", &plugin_ids(&["demo"])).unwrap();
    assert_eq!(updated, "plugins = [\"demo\"]\n");
}

#[test]
fn set_plugins_refuses_a_malformed_existing_array() {
    let error = set_plugins("plugins = \"demo\"\n", &plugin_ids(&["beta"])).unwrap_err();
    assert_eq!(
        error.to_string(),
        "onlyne: config.toml keeps `plugins` in a shape this verb cannot edit; write it as plugins = [\"id\"] (line 1)"
    );
}
