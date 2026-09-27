//! The socket the TUI watches, resolved through the one resolver the verbs use.
//!
//! The board only reads the socket's path — the admin frame protocol needs no
//! surface — so this module is the CLI's [`resolve_socket`] narrowed to a path,
//! kept separate so the TUI does not depend on the verb vocabulary.

use crate::flags::GlobalFlags;
use crate::socket;
use std::path::PathBuf;

pub const NO_SOCKET_MESSAGE: &str = onlyne_proto::NO_SOCKET_MESSAGE;

#[derive(Debug, Clone, Default)]
pub struct SocketArgs {
    pub socket: Option<PathBuf>,
    pub server_root: Option<PathBuf>,
    pub workspace: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoSocket;

/// Resolve the socket to watch, in the same precedence order the verbs use:
/// `--socket`, then `ONLYNE_SOCKET`, then `--server-root`, then `--workspace`
/// or the current directory walking upward for the owner tree.
pub fn resolve_socket(args: &SocketArgs) -> Result<PathBuf, NoSocket> {
    // The board carries no surface hint of its own; the resolver reads the
    // surface off the registration file, and only the path is needed here.
    let flags = GlobalFlags::addressing(
        args.socket.clone(),
        args.server_root.clone(),
        args.workspace.clone(),
    );
    socket::resolve_socket(&flags)
        .map(|target| target.path)
        .map_err(|_| NoSocket)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::socket::runtime_dir_pinned;
    use onlyne_wire::socket::{RegistrationFile, socket_path, write_registration};
    use std::fs;
    use std::path::Path;

    /// An owner tree with `.onlyne/` and a started daemon: the registration file
    /// is what says the daemon is live and which surface it serves.
    fn started_workspace(dir: &Path) -> PathBuf {
        fs::create_dir_all(dir.join(".onlyne")).expect("owner dir");
        let socket = socket_path(dir).expect("socket path");
        fs::create_dir_all(socket.parent().expect("runtime dir")).expect("runtime dir");
        fs::write(&socket, "").expect("socket file");
        write_registration(dir, &RegistrationFile::client(dir).with_role("worker"))
            .expect("registration");
        dir.to_path_buf()
    }

    /// The flag is the operator's explicit answer and needs no tree behind it.
    #[test]
    fn socket_flag_wins() {
        let args = SocketArgs {
            socket: Some(PathBuf::from("/tmp/s")),
            server_root: Some(PathBuf::from("/tmp/root")),
            workspace: None,
        };
        assert_eq!(resolve_socket(&args).unwrap(), PathBuf::from("/tmp/s"));
    }

    /// A server root names the admin socket once its daemon is running, which is
    /// the same answer the verb path gives: the board watches a live socket, not
    /// a name it would only fail to connect to.
    #[test]
    fn server_root_maps_to_its_runtime_socket() {
        let dir = tempfile::tempdir().expect("temp dir");
        let _runtime = runtime_dir_pinned(dir.path());
        let root = dir.path().join("srv");
        fs::create_dir_all(root.join(".onlyne")).expect("owner dir");
        // The tree must exist before the digest is taken: a root that is absent
        // canonicalizes to a lexical spelling, and macOS reaches a temporary
        // tree through two of them.
        let socket = socket_path(&root).expect("socket path");
        fs::create_dir_all(socket.parent().expect("runtime dir")).expect("runtime dir");
        fs::write(&socket, "").expect("socket file");
        write_registration(&root, &RegistrationFile::server(&root)).expect("registration");
        let args = SocketArgs {
            socket: None,
            server_root: Some(root.clone()),
            workspace: None,
        };
        assert_eq!(resolve_socket(&args).unwrap(), socket);
    }

    /// The board is launched from wherever the operator is, so the walk upward
    /// is what makes `onlyne` work from a source subdirectory.
    #[test]
    fn discovers_workspace_upwards() {
        let dir = tempfile::tempdir().expect("temp dir");
        let _runtime = runtime_dir_pinned(dir.path());
        let root = started_workspace(&dir.path().join("a"));
        let nested = root.join("b/c");
        fs::create_dir_all(&nested).expect("nested dir");
        let args = SocketArgs {
            socket: None,
            server_root: None,
            workspace: Some(nested),
        };
        assert_eq!(
            resolve_socket(&args).unwrap(),
            socket_path(&root).expect("socket path")
        );
    }

    /// One owner tree resolves to one path however deep it nests: v2 derives
    /// the path from the root, so the length rule that moved a deep workspace's
    /// socket in v1 has no second spelling to disagree with.
    #[test]
    fn one_tree_answers_one_path_across_the_bind() {
        let dir = tempfile::tempdir().expect("temp dir");
        let _runtime = runtime_dir_pinned(dir.path());
        let mut root = dir.path().to_path_buf();
        for _ in 0..6 {
            root.push("role-workspace-with-a-long-name");
        }
        let args = SocketArgs {
            socket: None,
            server_root: None,
            workspace: Some(root.join("src")),
        };

        let deep = started_workspace(&root);
        let served = resolve_socket(&args).expect("a workspace names its socket");
        assert_eq!(served, socket_path(&deep).expect("socket path"));
        assert!(
            served.as_os_str().len() <= onlyne_wire::socket::UNIX_SOCKET_PATH_MAX,
            "the derived path fits the bound, got {}",
            served.as_os_str().len()
        );
    }

    /// A tree that has never been started has no socket to watch, and the
    /// board says so rather than sitting on a path nothing binds.
    #[test]
    fn a_tree_without_a_daemon_refuses() {
        let dir = tempfile::tempdir().expect("temp dir");
        let _runtime = runtime_dir_pinned(dir.path());
        let root = dir.path().join("ws");
        fs::create_dir_all(root.join(".onlyne")).expect("owner dir");
        let args = SocketArgs {
            socket: None,
            server_root: None,
            workspace: Some(root),
        };
        assert_eq!(resolve_socket(&args), Err(NoSocket));
    }
}
