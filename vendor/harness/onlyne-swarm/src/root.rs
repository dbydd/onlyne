use anyhow::{bail, Context};
use std::path::{Path, PathBuf};

/// The scheduler's startup cwd is the swarm root. No upward search.
pub fn cwd_root(cwd: &Path) -> PathBuf {
    cwd.to_path_buf()
}

/// Resolve the swarm root for client commands launched from a generated
/// workspace. The scheduler remains single-instance at the root socket;
/// nested `.ws/<role>` directories only borrow that control plane.
pub fn client_root(cwd: &Path) -> PathBuf {
    let mut cur = cwd.to_path_buf();
    loop {
        if cur.join(".onlyne/run/swarm.sock").exists()
            || cur.join(".onlyne/swarm.db").exists()
            || (cur.join(".ws").is_dir() && cur.join(".agents/.schedule").is_dir())
        {
            return cur;
        }
        match cur.parent() {
            Some(parent) if parent != cur => cur = parent.to_path_buf(),
            _ => return cwd.to_path_buf(),
        }
    }
}

/// Refuse to start a scheduler in a nested directory: any ancestor containing
/// `.ws` or `.onlyne/swarm.db` means we are inside another tree.
pub fn ensure_root(cwd: &Path, force: bool) -> anyhow::Result<PathBuf> {
    if !force {
        let mut cur = cwd.to_path_buf();
        loop {
            if cur.join(".ws").is_dir() || cur.join(".onlyne/swarm.db").exists() {
                if cur != cwd {
                    bail!(
                        "nested swarm start refused: {} is inside swarm tree at {}. Run `onlyne-swarm attach` there, or start at the root, or pass --force",
                        cwd.display(),
                        cur.display()
                    );
                }
                break;
            }
            match cur.parent() {
                Some(p) => cur = p.to_path_buf(),
                None => break,
            }
        }
    }
    let onlyne = cwd.join(".onlyne");
    std::fs::create_dir_all(onlyne.join("run")).with_context(|| "create .onlyne/run")?;
    Ok(cwd.to_path_buf())
}

pub fn schedule_dir(root: &Path) -> PathBuf {
    root.join(".agents/.schedule")
}

pub fn instances_dir(root: &Path) -> PathBuf {
    root.join(".ws")
}

pub fn swarm_db(root: &Path) -> PathBuf {
    root.join(".onlyne/swarm.db")
}

pub fn swarm_sock(root: &Path) -> PathBuf {
    root.join(".onlyne/run/swarm.sock")
}

/// R4 --detach: the scheduler pid file. `stop` reads it; a stale file (pid
/// gone) is treated exactly like a missing one.
pub fn swarm_pid(root: &Path) -> PathBuf {
    root.join(".onlyne/run/scheduler.pid")
}

/// R4 --detach: stdout/stderr sink for the detached scheduler.
pub fn swarm_log(root: &Path) -> PathBuf {
    root.join(".onlyne/logs/scheduler.log")
}

/// Resolve an instance workspace dir from a tree-relative target ("a/b", "." = root itself).
pub fn resolve_instance(root: &Path, target: &str) -> PathBuf {
    if target == "." || target.is_empty() {
        root.to_path_buf()
    } else {
        instances_dir(root).join(target)
    }
}

pub fn loopback_in(ws_root: &Path) -> PathBuf {
    ws_root.join(".onlyne/channels/loopback/in")
}

pub fn onlyne_sock(ws_root: &Path) -> PathBuf {
    ws_root.join(".onlyne/run/s")
}

pub fn swarm_ws_config(ws_root: &Path) -> PathBuf {
    ws_root.join(".onlyne/swarm.workspace.jsonc")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn root_is_cwd_no_upward_search() {
        let p = PathBuf::from("/tmp/x/y");
        assert_eq!(cwd_root(&p), p);
    }

    #[test]
    fn nested_client_commands_resolve_root_socket() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let worker = root.join(".ws/bench");
        std::fs::create_dir_all(worker.join(".onlyne/run")).unwrap();
        std::fs::create_dir_all(root.join(".onlyne/run")).unwrap();
        std::fs::create_dir_all(root.join(".agents/.schedule")).unwrap();
        std::fs::write(root.join(".onlyne/run/swarm.sock"), "").unwrap();
        assert_eq!(client_root(&worker), root);
        assert_eq!(client_root(&root), root);
        let unrelated = dir.path().join("unrelated");
        std::fs::create_dir_all(&unrelated).unwrap();
        assert_eq!(client_root(&unrelated), unrelated);
    }

    #[test]
    fn nested_start_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(root.join(".ws")).unwrap();
        let child = root.join(".ws/a");
        std::fs::create_dir_all(&child).unwrap();
        let err = ensure_root(&child, false).unwrap_err();
        assert!(err.to_string().contains("nested swarm start refused"));
        assert!(ensure_root(&child, true).is_ok());
    }
}
