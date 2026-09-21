//! Fixtures shared by the `local_cli` subject test files.

use std::path::{Path, PathBuf};

/// The config shape `onlyne-client init` writes, comments included.
pub(super) fn write_role_config(workspace: &Path) -> PathBuf {
    let onlyne = workspace.join(".onlyne");
    std::fs::create_dir_all(&onlyne).unwrap();
    let config = onlyne.join("config.toml");
    std::fs::write(
        &config,
        "role = \"planner\"\n# local plugin list\ncert_pin = \"sha256/pin\"\nkey_path = \"keys/role.key\"\nplugins = []\n\n[server]\nhost = \"127.0.0.1\"\nport = 9443\n",
    )
    .unwrap();
    config
}

pub(super) fn plugin_ids(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}
