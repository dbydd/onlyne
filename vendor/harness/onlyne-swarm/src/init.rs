use std::path::{Path, PathBuf};

pub const ROOT_CONFIG: &str = r#"#:schema ./onlyne-config.schema.json
[workspace]
name = "swarm-root"

[logging]
level = "info"

[io]
in_format = "markdown"
out_content = "latest_only"
out_cursor = "consume"
history_context_messages = 20

[loopback.io]
in_format = "markdown"
out_content = "latest_only"
out_cursor = "consume"
history_context_messages = 20

[adapters.telegram]
enabled = false

[adapters.feishu]
enabled = false

[adapters.qqbot]
enabled = false

[adapters.wechat]
enabled = false

[swarm]
enabled = true

[swarm.transport]
mode = "rpc"
fifo = false
"#;

pub const ROOT_DOTENV: &str = r#"# Swarm root workspace-local secrets.
# Fill only the adapters you enable at the root; generated agent
# workspaces stay loopback-only.
# TELEGRAM_BOT_TOKEN=
"#;

pub const ROOT_TEMPLATE: &str = r#"{
  // Full starter template. Every key ships with its default; delete
  // anything unchanged. Editors should resolve $schema against the
  // repository copy at the onlyne-swarm repo root.
  "$schema": "https://raw.githubusercontent.com/dbydd/onlyne-swarm/main/template.workspace.schema.json",
  // Workspace name. Defaults to the template directory name when empty.
  "name": "planner",
  // Inline system prompt delivered to the session before the task payload.
  "role": "You are the planner. Read the incoming task, decide whether it needs a child workspace, and return a concise result.",
  // Session launcher metadata kept verbatim in the merged snapshot.
  "model": { "provider": "", "model": "", "effort": "" },
  // Callback targets as tree paths, relative to this workspace.
  // Cyclic edges are allowed; missing targets fail sync.
  "back_edges": []
}
"#;

pub const ROOT_WORKSPACE_JSONC: &str = r#"{
  // Root workspace description. Merged as the base layer for every
  // generated agent workspace. Keep shared role/model defaults here.
  "$schema": "https://raw.githubusercontent.com/dbydd/onlyne-swarm/main/template.workspace.schema.json",
  "name": ".",
  "role": "",
  "model": { "provider": "", "model": "", "effort": "" },
  "back_edges": []
}
"#;

pub const STARTER_CHILD: &str = "planner";

fn write_if_missing(path: &Path, body: &str) -> anyhow::Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, body)?;
    Ok(true)
}

/// Place `[swarm] enabled = true` and the loopback-RPC transport contract into
/// an existing root config. Returns true when the file was created or modified.
fn ensure_swarm_enabled(cfg_path: &Path) -> anyhow::Result<bool> {
    if !cfg_path.exists() {
        if let Some(parent) = cfg_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(cfg_path, ROOT_CONFIG)?;
        return Ok(true);
    }
    let text = std::fs::read_to_string(cfg_path)?;
    if text.contains("[swarm.transport]") {
        // Existing transport settings are supervisor-owned. Initialization only
        // fills missing contract fields.
        return Ok(false);
    }
    let mut text = text;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    if !text.contains("[swarm]") {
        text.push_str("\n[swarm]\nenabled = true\n");
    }
    text.push_str("\n[swarm.transport]\nmode = \"rpc\"\nfifo = false\n");
    std::fs::write(cfg_path, text)?;
    Ok(true)
}

/// Initialize the current directory as a swarm root.
///
/// - Missing `.onlyne/config.toml` is created loopback-only with swarm on and
///   RPC transport. An existing config keeps adapter, swarm-enable, and
///   transport values; only missing contract sections are appended.
/// - Missing schedule starter, root workspace jsonc, and `.env` are created.
/// - Existing files are never overwritten.
pub fn run_init(cwd: &Path) -> anyhow::Result<PathBuf> {
    let root = crate::root::cwd_root(cwd);
    ensure_swarm_enabled(&root.join(".onlyne/config.toml"))?;
    write_if_missing(&root.join(".onlyne/.env"), ROOT_DOTENV)?;
    write_if_missing(
        &root.join(".onlyne/swarm.workspace.jsonc"),
        ROOT_WORKSPACE_JSONC,
    )?;
    write_if_missing(
        &root
            .join(".agents/.schedule")
            .join(STARTER_CHILD)
            .join("template.workspace.jsonc"),
        ROOT_TEMPLATE,
    )?;
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_creates_full_starter() {
        let dir = tempfile::tempdir().unwrap();
        let root = run_init(dir.path()).unwrap();
        assert_eq!(root, dir.path());
        let cfg = std::fs::read_to_string(root.join(".onlyne/config.toml")).unwrap();
        assert!(cfg.contains("[swarm]"));
        assert!(cfg.contains("enabled = true"));
        assert!(cfg.contains("[swarm.transport]"));
        assert!(cfg.contains("mode = \"rpc\""));
        assert!(cfg.contains("fifo = false"));
        let tpl = std::fs::read_to_string(
            root.join(".agents/.schedule/planner/template.workspace.jsonc"),
        )
        .unwrap();
        assert!(tpl.contains("\"$schema\""));
        assert!(tpl.contains("\"back_edges\""));
        assert!(tpl.contains("\"model\""));
        let ws = std::fs::read_to_string(root.join(".onlyne/swarm.workspace.jsonc")).unwrap();
        assert!(ws.contains("\"back_edges\""));
        // Second run is a no-op for hand-edited files.
        std::fs::write(
            root.join(".agents/.schedule/planner/template.workspace.jsonc"),
            "{\"edited\": true}",
        )
        .unwrap();
        run_init(dir.path()).unwrap();
        let tpl = std::fs::read_to_string(
            root.join(".agents/.schedule/planner/template.workspace.jsonc"),
        )
        .unwrap();
        assert!(tpl.contains("edited"));
    }

    #[test]
    fn init_converts_existing_onlyne_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let onlyne = dir.path().join(".onlyne");
        std::fs::create_dir_all(&onlyne).unwrap();
        std::fs::write(
            onlyne.join("config.toml"),
            "[workspace]\nname = \"mine\"\n\n[adapters.telegram]\nenabled = true\n",
        )
        .unwrap();
        run_init(dir.path()).unwrap();
        let cfg = std::fs::read_to_string(onlyne.join("config.toml")).unwrap();
        assert!(cfg.contains("name = \"mine\""));
        assert!(cfg.contains("enabled = true\n\n[swarm]") || cfg.contains("[swarm]"));
        assert!(cfg.contains("[swarm]"));
        assert!(cfg.contains("[swarm.transport]"));
        assert!(cfg.contains("mode = \"rpc\""));
        assert!(cfg.contains("fifo = false"));
    }

    #[test]
    fn init_preserves_supervisor_swarm_disable() {
        let dir = tempfile::tempdir().unwrap();
        let onlyne = dir.path().join(".onlyne");
        std::fs::create_dir_all(&onlyne).unwrap();
        std::fs::write(
            onlyne.join("config.toml"),
            "[workspace]\nname = \"mine\"\n\n[swarm]\nenabled = false\n",
        )
        .unwrap();
        run_init(dir.path()).unwrap();
        let cfg = std::fs::read_to_string(onlyne.join("config.toml")).unwrap();
        assert!(cfg.contains("enabled = false"));
        assert!(!cfg.contains("enabled = true"));
        assert!(cfg.contains("[swarm.transport]"));
        assert!(cfg.contains("mode = \"rpc\""));
        assert!(cfg.contains("fifo = false"));
    }
}
