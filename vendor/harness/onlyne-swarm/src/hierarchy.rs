use std::process::Command;

/// Orca node cleanup: folder-kind nodes are Orca-side metadata only and are
/// NOT reclaimed when their directories disappear. Live field evidence
/// (0.3.0 rollout): nodes neither appear in `worktree list` nor resolve via
/// `worktree show`, and tabs created `--worktree path:<node>` are absent from
/// `terminal list` — the UI shows Unknown groups instead. So registration is
/// disabled: this module only removes leftover nodes. Visibility lives in the
/// TUI tree (`f` focuses the hop tab under the root worktree).

fn orca() -> Command {
    let bin = std::env::var("ORCA_CLI_COMMAND").unwrap_or_else(|_| "orca".into());
    Command::new(bin)
}

fn run_json(mut cmd: Command) -> Option<serde_json::Value> {
    cmd.arg("--json");
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

/// True when the Orca runtime answers. Cleanup is skipped entirely when
/// Orca is unreachable (headless e2e, non-macOS, no Orca).
pub fn reachable() -> bool {
    let mut cmd = orca();
    cmd.args(["status"]);
    run_json(cmd).is_some()
}

/// Probe whether a dir is currently an addressable Orca worktree.
/// Canonicalizes first: Orca resolves symlinked prefixes (macOS
/// /tmp -> /private/tmp); an uncanonicalized selector selector_not_founds
/// even when the node exists.
pub fn show_worktree_id(dir: &std::path::Path) -> Option<String> {
    let canon = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let mut cmd = orca();
    cmd.args(["worktree", "show", "--worktree"]);
    cmd.arg(format!("path:{}", canon.display()));
    let v = run_json(cmd)?;
    if v.get("ok").and_then(|o| o.as_bool()) != Some(true) {
        return None;
    }
    v.pointer("/result/worktree/id")
        .and_then(|id| id.as_str())
        .map(String::from)
}

/// Display name for a workspace node: `swarm:<tree-path>`, root is `swarm:.`.
/// Kept for the TUI tree labels and tab titles (same namespace, no Orca call).
pub fn node_display_name(tree_path: &str) -> String {
    if tree_path.is_empty() {
        "swarm:.".to_string()
    } else {
        format!("swarm:{tree_path}")
    }
}

/// Outcome of cleaning one workspace node.
pub enum HierarchyOutcome {
    /// No node existed, or the leftover was removed.
    Clean,
    /// Removal failed; logged, never fatal to sync.
    Stale { detail: String },
    /// Orca unreachable; caller logs and continues.
    Skipped,
}

/// Remove the folder-kind node for one workspace dir if present.
/// Best effort; never fails sync.
pub fn ensure_child(
    _project: Option<&str>,
    dir: &std::path::Path,
    _display_name: &str,
) -> HierarchyOutcome {
    if !reachable() {
        return HierarchyOutcome::Skipped;
    }
    let Some(id) = show_worktree_id(dir) else {
        return HierarchyOutcome::Clean;
    };
    // Derive the setup id: `show` does not return it directly, so list
    // setups and match by canonical path.
    let setup_id = list_setup_for(dir);
    if let Some(setup) = setup_id {
        let mut cmd = orca();
        cmd.args(["project", "setup-delete", "--setup", &setup]);
        match run_json(cmd) {
            Some(v) if v.get("ok").and_then(|o| o.as_bool()) == Some(true) => {
                return HierarchyOutcome::Clean
            }
            _ => {
                return HierarchyOutcome::Stale {
                    detail: format!("{id} setup-delete refused"),
                }
            }
        }
    }
    // Addressable worktree but no matching setup: leave it, report stale.
    HierarchyOutcome::Stale {
        detail: format!("{id} has no matching project setup"),
    }
}

fn list_setup_for(dir: &std::path::Path) -> Option<String> {
    let canon = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let mut cmd = orca();
    cmd.args(["project", "setups"]);
    let v = run_json(cmd)?;
    let setups = v.pointer("/result/setups")?.as_array()?;
    for s in setups {
        let p = s.get("path")?.as_str()?;
        let canon_p = std::fs::canonicalize(p).unwrap_or_else(|_| std::path::PathBuf::from(p));
        if canon_p == canon {
            return s.get("id")?.as_str().map(String::from);
        }
    }
    None
}

/// Derive the Orca project id for the swarm root from its git origin.
/// Falls back to None (caller substitutes a default); never fails.
/// Currently unused (registration disabled); kept with its test so a
/// future Orca release with folder-child support plugs back in.
#[allow(dead_code)]
pub fn root_project(root: &std::path::Path) -> Option<String> {
    let out = Command::new("git")
        .args(["-C"])
        .arg(root)
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // https://github.com/OWNER/REPO(.git) or git@github.com:OWNER/REPO(.git)
    let path = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
        .or_else(|| url.strip_prefix("git@github.com:"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))?;
    let path = path.strip_suffix(".git").unwrap_or(path);
    let path = path.trim_matches('/');
    if path.is_empty() || !path.contains('/') {
        return None;
    }
    Some(format!("github:{path}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_display_name_shapes() {
        assert_eq!(node_display_name(""), "swarm:.");
        assert_eq!(node_display_name("model"), "swarm:model");
        assert_eq!(node_display_name("a/b"), "swarm:a/b");
    }

    #[test]
    fn root_project_parses_github_origins() {
        // No git repo here: None, never panic.
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(root_project(dir.path()), None);
    }
}
