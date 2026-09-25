use super::*;
use crate::ops::local_cli::fixtures::write_role_config;
use tempfile::tempdir;

/// A flat package holding the single `onlyne-agent-<id>` binary the
/// installer demands.
fn write_package(package: &Path, id: &str) {
    std::fs::write(
        package.join(format!("onlyne-agent-{id}")),
        "#!/bin/sh\nexit 0\n",
    )
    .unwrap();
}

#[test]
fn agent_install_and_uninstall_round_trip() {
    let workspace = tempdir().unwrap();
    let package = tempdir().unwrap();
    write_package(package.path(), "demo");
    let lines =
        agent_install(workspace.path(), package.path(), "demo", Some("demo-agent")).unwrap();
    assert_eq!(lines.len(), 3);
    let target = agent_package_dir(workspace.path(), "demo");
    assert!(target.join("onlyne-agent-demo").exists());
    assert!(target.join("plugin.toml").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(target.join("onlyne-agent-demo"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o755);
    }
    // The workspace had no config.toml before, so the installer's line
    // says which file it created; a plugins-only file is what lands.
    let config = std::fs::read_to_string(workspace.path().join(".onlyne/config.toml")).unwrap();
    assert!(config.contains("plugins = [\"demo\"]"));
    assert!(!config.contains("[[plugin]]"));
    let removed = agent_uninstall(workspace.path(), "demo").unwrap();
    assert_eq!(removed.len(), 2);
    assert_eq!(removed[0], "deregistered plugin demo from plugins");
    assert!(!target.exists());
}

#[test]
fn install_registers_plugin_id_in_the_plugins_array() {
    let workspace = tempdir().unwrap();
    let package = tempdir().unwrap();
    let config = write_role_config(workspace.path());
    write_package(package.path(), "demo");
    let lines = agent_install(workspace.path(), package.path(), "demo", None).unwrap();
    assert_eq!(lines[2], "registered plugin demo in plugins = [\"demo\"]");
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(text.contains("plugins = [\"demo\"]"));
    assert!(!text.contains("[[plugin]]"));
    let parsed = onlyne_config::ClientConfig::load(&config).unwrap();
    assert_eq!(parsed.plugins, vec!["demo".to_string()]);
    // A second plugin extends the same single-line array.
    let other = tempdir().unwrap();
    write_package(other.path(), "beta");
    agent_install(workspace.path(), other.path(), "beta", None).unwrap();
    let parsed = onlyne_config::ClientConfig::load(&config).unwrap();
    assert_eq!(parsed.plugins, vec!["demo".to_string(), "beta".to_string()]);
}

#[test]
fn reinstall_refuses_and_config_stays_already_installed_state() {
    // A drifted workspace: the package directory is gone but the array
    // still registers the id. Reinstalling refuses with the existing
    // "already installed" message and changes no byte.
    let workspace = tempdir().unwrap();
    let package = tempdir().unwrap();
    let config = write_role_config(workspace.path());
    write_package(package.path(), "demo");
    agent_install(workspace.path(), package.path(), "demo", None).unwrap();
    let before = std::fs::read_to_string(&config).unwrap();
    std::fs::remove_dir_all(agent_package_dir(workspace.path(), "demo")).unwrap();
    let error = agent_install(workspace.path(), package.path(), "demo", None).unwrap_err();
    assert_eq!(error.to_string(), "onlyne: plugin demo already installed");
    assert_eq!(std::fs::read_to_string(&config).unwrap(), before);
}

#[test]
fn uninstall_drops_the_id_and_keeps_every_other_byte() {
    let workspace = tempdir().unwrap();
    let config = write_role_config(workspace.path());
    let demo = tempdir().unwrap();
    write_package(demo.path(), "demo");
    agent_install(workspace.path(), demo.path(), "demo", None).unwrap();
    let beta = tempdir().unwrap();
    write_package(beta.path(), "beta");
    agent_install(workspace.path(), beta.path(), "beta", None).unwrap();
    let before = std::fs::read_to_string(&config).unwrap();
    agent_uninstall(workspace.path(), "demo").unwrap();
    let after = std::fs::read_to_string(&config).unwrap();
    let before_lines: Vec<&str> = before.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();
    assert_eq!(before_lines.len(), after_lines.len());
    let mut plugins_line = None;
    for (old, new) in before_lines.iter().zip(&after_lines) {
        if old.starts_with("plugins") {
            assert_ne!(old, new);
            plugins_line = Some(new.to_string());
        } else {
            assert_eq!(old, new);
        }
    }
    assert_eq!(plugins_line.unwrap(), "plugins = [\"beta\"]");
    let parsed = onlyne_config::ClientConfig::load(&config).unwrap();
    assert_eq!(parsed.plugins, vec!["beta".to_string()]);
}

#[test]
fn uninstall_folds_a_legacy_block_the_array_missed() {
    // In a 1.2.1 workspace the id can live only inside `[[plugin]]`: the
    // uninstall must drop that block even though the array never held it.
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
    let target = agent_package_dir(workspace.path(), "demo");
    std::fs::create_dir_all(&target).unwrap();
    agent_uninstall(workspace.path(), "demo").unwrap();
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(!text.contains("[[plugin]]"));
    assert!(text.contains("plugins = []"));
    assert!(!target.exists());
}

#[test]
fn agent_bad_id_touches_no_path() {
    let workspace = tempdir().unwrap();
    let package = tempdir().unwrap();
    let result = agent_install(workspace.path(), package.path(), "Bad/Id", None);
    assert!(result.is_err());
    assert!(!workspace.path().join(".onlyne/agent").exists());
}

#[test]
fn agent_missing_id_reports_not_installed() {
    let workspace = tempdir().unwrap();
    let result = agent_uninstall(workspace.path(), "ghost");
    assert_eq!(
        result.unwrap_err().to_string(),
        "onlyne: no plugin ghost installed"
    );
}
