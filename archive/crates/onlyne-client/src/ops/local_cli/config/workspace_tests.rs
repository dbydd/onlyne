use super::*;
use crate::ops::local_cli::fixtures::{plugin_ids, write_role_config};
use std::path::PathBuf;
use tempfile::tempdir;

#[test]
fn heal_folds_legacy_blocks_into_an_existing_array() {
    // The exact state the 1.2.1 installer left behind: the init file with
    // `plugins = []` plus an appended `[[plugin]]` block whose id no later
    // build reads, so the plugin it names silently never loads.
    let workspace = tempdir().unwrap();
    let config = write_role_config(workspace.path());
    std::fs::write(
        &config,
        format!(
            "{}[[plugin]]\nid = \"demo\"\n",
            std::fs::read_to_string(&config).unwrap()
        ),
    )
    .unwrap();
    assert_eq!(
        onlyne_config::ClientConfig::load(&config).unwrap().plugins,
        Vec::<String>::new(),
        "the id inside the legacy block is invisible to the loader"
    );
    assert_eq!(migrate_plugin_blocks(workspace.path()).unwrap(), 1);
    let parsed = onlyne_config::ClientConfig::load(&config).unwrap();
    assert_eq!(parsed.plugins, vec!["demo".to_string()]);
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(!text.contains("[[plugin]]"));
    assert!(text.contains("plugins = [\"demo\"]"));
    assert!(text.contains("# local plugin list"));
    // Healing a healed workspace is a no-op.
    assert_eq!(migrate_plugin_blocks(workspace.path()).unwrap(), 0);
}

#[test]
fn heal_inserts_the_array_when_the_legacy_file_has_none() {
    // Pre-array workspaces: two blocks above a `[server]` table and no
    // `plugins` line at all. Migration keeps the blocks' order, inserts
    // the line after `key_path`, and drops both tables.
    let workspace = tempdir().unwrap();
    let onlyne = workspace.path().join(".onlyne");
    std::fs::create_dir_all(&onlyne).unwrap();
    let config = onlyne.join("config.toml");
    std::fs::write(
        &config,
        "role = \"planner\"\ncert_pin = \"sha256/pin\"\nkey_path = \"keys/role.key\"\n\n[server]\nhost = \"127.0.0.1\"\nport = 9443\n\n[[plugin]]\nid = \"demo\"\n\n[[plugin]]\nid = \"beta\"\n",
    )
    .unwrap();
    assert_eq!(
        onlyne_config::ClientConfig::load(&config).unwrap().plugins,
        Vec::<String>::new(),
        "the ids live in blocks the loader ignores"
    );
    assert_eq!(migrate_plugin_blocks(workspace.path()).unwrap(), 2);
    let parsed = onlyne_config::ClientConfig::load(&config).unwrap();
    assert_eq!(parsed.plugins, vec!["demo".to_string(), "beta".to_string()]);
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(!text.contains("[[plugin]]"));
}

#[test]
fn heal_leaves_unrelated_config_errors_untouched() {
    // A failure not caused by `[[plugin]]` must reach the operator as the
    // serde error it is: no rewrite, no swallowed report.
    let workspace = tempdir().unwrap();
    let text = r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
reconnect_grace_secs = "soon"
plugins = []
[server]
host = "127.0.0.1"
port = 9443
"#;
    let config = write_config(workspace.path(), text);
    assert_eq!(migrate_plugin_blocks(workspace.path()).unwrap(), 0);
    assert_eq!(std::fs::read_to_string(&config).unwrap(), text);
    let error = onlyne_config::ClientConfig::load(&config).unwrap_err();
    assert!(
        error.to_string().contains("invalid type"),
        "the serde refusal must survive: {error}"
    );
}

#[test]
fn an_unknown_client_key_no_longer_fails_the_load() {
    // A key no field declares is dropped by the parse and named once, so the
    // operator reads which default arrived in its place.
    let workspace = tempdir().unwrap();
    let text = r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
bogus = true
plugins = []
[server]
host = "127.0.0.1"
port = 9443
"#;
    let config = write_config(workspace.path(), text);
    let parsed = onlyne_config::ClientConfig::load(&config).expect("an unknown key is ignored");
    assert_eq!(parsed.role, "planner");
    assert_eq!(
        onlyne_config::keys::unknown_client_keys(text).unwrap(),
        ["bogus"],
        "the ignored key is named for the warning line"
    );
}

/// The ids inside a legacy `[[plugin]]` block are invisible to the loader:
/// the top-level array is the only place a plugin id is read from, so the
/// fold is what activates them.
fn assert_loader_reads_only_the_array(config: &Path, ids: &[&str]) {
    assert_eq!(
        onlyne_config::ClientConfig::load(config)
            .expect("a legacy block leaves a workspace the loader accepts")
            .plugins,
        plugin_ids(ids),
        "the loader reads the top-level array alone"
    );
}

fn write_config(workspace: &Path, text: &str) -> PathBuf {
    let config = RoleWorkspace::resolve(workspace).config_path();
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, text).unwrap();
    config
}

fn assert_no_temp_file(config: &Path) {
    let name = format!("{}.tmp", config.file_name().unwrap().to_string_lossy());
    let temp = config.with_file_name(name);
    assert!(
        !temp.exists(),
        "atomic config replacement left {} behind",
        temp.display()
    );
}

fn assert_migration_refused(workspace: &Path, text: &str, reason: &str) {
    let config = write_config(workspace, text);
    let error = migrate_plugin_blocks(workspace).unwrap_err();
    assert_eq!(error.to_string(), reason);
    assert_eq!(std::fs::read_to_string(&config).unwrap(), text);
    assert_no_temp_file(&config);
}

#[test]
fn migrate_plugin_blocks_preserves_existing_ids_and_deduplicates_blocks() {
    let workspace = tempdir().unwrap();
    let text = r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
plugins = ["alpha"] # operator note

[[plugin]]
id = "demo"
[[plugin]]
id = "alpha"
[[plugin]]
id = "beta"
[server]
host = "127.0.0.1"
port = 9443
"#;
    let config = write_config(workspace.path(), text);
    assert_loader_reads_only_the_array(&config, &["alpha"]);
    assert_eq!(migrate_plugin_blocks(workspace.path()).unwrap(), 3);
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
plugins = ["alpha", "demo", "beta"] # operator note

[server]
host = "127.0.0.1"
port = 9443
"#
    );
    let parsed = onlyne_config::ClientConfig::load(&config).unwrap();
    assert_eq!(parsed.plugins, plugin_ids(&["alpha", "demo", "beta"]));
    assert_no_temp_file(&config);
}

#[test]
fn migrate_plugin_blocks_is_idempotent_and_leaves_no_temp_file() {
    let workspace = tempdir().unwrap();
    let config = write_config(
        workspace.path(),
        r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
plugins = []

[[plugin]]
id = "demo"
[[plugin]]
id = "beta"
[server]
host = "127.0.0.1"
port = 9443
"#,
    );
    assert_loader_reads_only_the_array(&config, &[]);
    assert_eq!(migrate_plugin_blocks(workspace.path()).unwrap(), 2);
    let migrated = std::fs::read_to_string(&config).unwrap();
    assert_no_temp_file(&config);
    assert_eq!(migrate_plugin_blocks(workspace.path()).unwrap(), 0);
    assert_eq!(std::fs::read_to_string(&config).unwrap(), migrated);
    assert_no_temp_file(&config);
}

#[test]
fn migrate_plugin_blocks_inserts_the_array_after_key_path() {
    let workspace = tempdir().unwrap();
    let config = write_config(
        workspace.path(),
        r#"role = "planner"
key_path = "keys/role.key"
cert_pin = "sha256/pin"
[server]
host = "127.0.0.1"
port = 9443
[[plugin]]
id = "demo"
"#,
    );
    assert_loader_reads_only_the_array(&config, &[]);
    assert_eq!(migrate_plugin_blocks(workspace.path()).unwrap(), 1);
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        r#"role = "planner"
key_path = "keys/role.key"
plugins = ["demo"]
cert_pin = "sha256/pin"
[server]
host = "127.0.0.1"
port = 9443
"#
    );
    let parsed = onlyne_config::ClientConfig::load(&config)
        .expect("migration must leave the workspace loadable");
    assert_eq!(parsed.plugins, plugin_ids(&["demo"]));
    assert_no_temp_file(&config);
}

#[test]
fn migrate_plugin_blocks_keeps_a_block_without_an_id() {
    let workspace = tempdir().unwrap();
    let text = r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
plugins = []
[[plugin]]
path = "agent/demo"
[server]
host = "127.0.0.1"
port = 9443
"#;
    let config = write_config(workspace.path(), text);
    // A block with no id is one the fold cannot carry into `plugins`, so the
    // file stays exactly as the operator wrote it.
    assert_eq!(migrate_plugin_blocks(workspace.path()).unwrap(), 0);
    assert_eq!(std::fs::read_to_string(&config).unwrap(), text);
    onlyne_config::ClientConfig::load(&config).expect("an unknown table is ignored");
    let ignored = onlyne_config::keys::unknown_client_keys(text).unwrap();
    assert!(
        ignored.iter().any(|path| path.starts_with("plugin")),
        "the retained table is named for the warning line: {ignored:?}"
    );
    assert_no_temp_file(&config);
}

#[test]
fn migrate_plugin_blocks_refuses_unsafely_formatted_arrays_without_writing() {
    let workspace = tempdir().unwrap();
    assert_migration_refused(
        workspace.path(),
        "plugins = [\n  \"alpha\",\n]\n[[plugin]]\nid = \"demo\"\n",
        "onlyne: config.toml splits the `plugins` array across lines; put every id on one line (line 1)",
    );
    assert_migration_refused(
        workspace.path(),
        "plugins = \"alpha\"\n[[plugin]]\nid = \"demo\"\n",
        "onlyne: config.toml keeps `plugins` in a shape this verb cannot edit; write it as plugins = [\"id\"] (line 1)",
    );
    assert_migration_refused(
        workspace.path(),
        "plugins = [\"alpha\", beta]\n[[plugin]]\nid = \"demo\"\n",
        "onlyne: config.toml `plugins` array holds a value this verb cannot edit (line 1)",
    );
    assert_migration_refused(
        workspace.path(),
        "plugins = [\"alpha\"] junk\n[[plugin]]\nid = \"demo\"\n",
        "onlyne: config.toml has trailing text on the `plugins` line: [\"alpha\"] junk (line 1)",
    );
}

#[test]
fn migrate_plugin_blocks_ignores_an_unsafely_formatted_array_when_no_block_needs_migrating() {
    let workspace = tempdir().unwrap();
    let text = "plugins = [\n  \"alpha\",\n]\n";
    let config = write_config(workspace.path(), text);
    assert_eq!(migrate_plugin_blocks(workspace.path()).unwrap(), 0);
    assert_eq!(std::fs::read_to_string(&config).unwrap(), text);
    assert_no_temp_file(&config);
}

#[test]
fn heal_workspace_config_migrates_a_legacy_workspace() {
    let workspace = tempdir().unwrap();
    let config = write_config(
        workspace.path(),
        r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
plugins = []
[[plugin]]
id = "demo"
[server]
host = "127.0.0.1"
port = 9443
"#,
    );
    assert_loader_reads_only_the_array(&config, &[]);
    heal_workspace_config(workspace.path());
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
plugins = ["demo"]
[server]
host = "127.0.0.1"
port = 9443
"#
    );
    let parsed = onlyne_config::ClientConfig::load(&config)
        .expect("the startup hook must leave the healed workspace loadable");
    assert_eq!(parsed.plugins, plugin_ids(&["demo"]));
    assert_no_temp_file(&config);
}

#[test]
fn heal_workspace_config_leaves_a_healthy_workspace_untouched() {
    let workspace = tempdir().unwrap();
    let text = r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
plugins = ["demo"]
[server]
host = "127.0.0.1"
port = 9443
"#;
    let config = write_config(workspace.path(), text);
    heal_workspace_config(workspace.path());
    assert_eq!(std::fs::read_to_string(&config).unwrap(), text);
    assert_no_temp_file(&config);
}

#[test]
fn heal_workspace_config_leaves_an_unfixable_workspace_untouched() {
    let workspace = tempdir().unwrap();
    let text = "plugins = [\n  \"alpha\",\n]\n[[plugin]]\nid = \"demo\"\n";
    let config = write_config(workspace.path(), text);
    heal_workspace_config(workspace.path());
    assert_eq!(std::fs::read_to_string(&config).unwrap(), text);
    assert_no_temp_file(&config);
}

#[test]
fn heal_workspace_config_creates_no_config_for_a_workspace_without_one() {
    let workspace = tempdir().unwrap();
    let config = RoleWorkspace::resolve(workspace.path()).config_path();
    heal_workspace_config(workspace.path());
    assert!(!config.exists());
    assert_no_temp_file(&config);
}

#[test]
fn migrate_plugin_blocks_folds_a_plugin_header_with_a_trailing_comment() {
    let workspace = tempdir().unwrap();
    let text = r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
plugins = []
[[plugin]] # id block
id = "demo"
[server]
host = "127.0.0.1"
port = 9443
"#;
    let config = write_config(workspace.path(), text);
    assert_loader_reads_only_the_array(&config, &[]);
    assert_eq!(migrate_plugin_blocks(workspace.path()).unwrap(), 1);
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
plugins = ["demo"]
[server]
host = "127.0.0.1"
port = 9443
"#
    );
    let parsed = onlyne_config::ClientConfig::load(&config)
        .expect("a folded comment-headed legacy block must leave a loadable workspace");
    assert_eq!(parsed.plugins, plugin_ids(&["demo"]));
    assert_no_temp_file(&config);
}

#[test]
fn migrate_plugin_blocks_merges_duplicate_top_level_plugins_lines() {
    let workspace = tempdir().unwrap();
    let text = r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
plugins = ["alpha"]
plugins = ["beta"]
[[plugin]]
id = "demo"
[server]
host = "127.0.0.1"
port = 9443
"#;
    let config = write_config(workspace.path(), text);
    assert!(
        onlyne_config::ClientConfig::load(&config).is_err(),
        "duplicate top-level plugins keys must make the fixture unloadable"
    );
    assert_eq!(migrate_plugin_blocks(workspace.path()).unwrap(), 1);
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
plugins = ["alpha", "beta", "demo"]
[server]
host = "127.0.0.1"
port = 9443
"#
    );
    let parsed = onlyne_config::ClientConfig::load(&config)
        .expect("merged duplicate plugins lines must leave one loadable top-level key");
    assert_eq!(parsed.plugins, plugin_ids(&["alpha", "beta", "demo"]));
    assert_no_temp_file(&config);
}

#[test]
fn migrate_plugin_blocks_merges_duplicate_plugins_lines_without_legacy_blocks() {
    let workspace = tempdir().unwrap();
    let text = r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
plugins = ["alpha"] # first line
plugins = ["beta", "alpha"]
[server]
host = "127.0.0.1"
port = 9443
"#;
    let config = write_config(workspace.path(), text);
    assert_eq!(migrate_plugin_blocks(workspace.path()).unwrap(), 0);
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
plugins = ["alpha", "beta"] # first line
[server]
host = "127.0.0.1"
port = 9443
"#
    );
    let parsed = onlyne_config::ClientConfig::load(&config)
        .expect("duplicate-only plugins lines must heal into one loadable array");
    assert_eq!(parsed.plugins, plugin_ids(&["alpha", "beta"]));
    assert_no_temp_file(&config);
}

#[test]
fn migrate_plugin_blocks_refuses_an_unsafe_duplicate_plugins_line_without_writing() {
    let workspace = tempdir().unwrap();
    let text = r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
plugins = ["alpha"]
plugins = ["beta", gamma]
[[plugin]]
id = "demo"
[server]
host = "127.0.0.1"
port = 9443
"#;
    let config = write_config(workspace.path(), text);
    let error = migrate_plugin_blocks(workspace.path())
        .expect_err("an unsafe duplicate plugins line must abort the whole merge");
    assert_eq!(
        error.to_string(),
        "onlyne: config.toml `plugins` array holds a value this verb cannot edit (line 5)"
    );
    assert_eq!(std::fs::read_to_string(&config).unwrap(), text);
    assert_no_temp_file(&config);
}

#[test]
fn append_plugin_entry_merges_duplicates_when_id_is_already_registered() {
    let workspace = tempdir().unwrap();
    let config = write_config(
        workspace.path(),
        r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
plugins = ["alpha"]
plugins = ["beta"]
[server]
host = "127.0.0.1"
port = 9443
"#,
    );
    let ids = append_plugin_entry(workspace.path(), "alpha").unwrap();
    assert_eq!(ids, plugin_ids(&["alpha", "beta"]));
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
plugins = ["alpha", "beta"]
[server]
host = "127.0.0.1"
port = 9443
"#
    );
    let parsed = onlyne_config::ClientConfig::load(&config)
        .expect("append must leave one loadable merged array");
    assert_eq!(parsed.plugins, plugin_ids(&["alpha", "beta"]));
}

#[test]
fn remove_plugin_entry_merges_duplicate_lines_while_dropping_the_id() {
    let workspace = tempdir().unwrap();
    let config = write_config(
        workspace.path(),
        r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
plugins = ["alpha"]
plugins = ["beta"]
[server]
host = "127.0.0.1"
port = 9443
"#,
    );
    remove_plugin_entry(workspace.path(), "beta").unwrap();
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        r#"role = "planner"
cert_pin = "sha256/pin"
key_path = "keys/role.key"
plugins = ["alpha"]
[server]
host = "127.0.0.1"
port = 9443
"#
    );
    let parsed = onlyne_config::ClientConfig::load(&config)
        .expect("removal must leave one loadable merged array");
    assert_eq!(parsed.plugins, plugin_ids(&["alpha"]));
}
