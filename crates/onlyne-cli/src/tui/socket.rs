//! The socket the board watches, resolved through the one resolver the verbs use.
//!
//! The board speaks the admin vocabulary — the five reads, `subscribe`, and the
//! ops its operator asks for — so it needs the socket a server binds, not the
//! one a role's client binds. The path is resolved exactly as every verb
//! resolves it, and the surface the registration file states is what decides
//! whether this board may speak: [`NEEDS_ADMIN`] is the same refusal `onlyne
//! status` gives for the same mistake.

use crate::flags::{AsArg, GlobalFlags};
use crate::socket::{self, NoSocket, SocketTarget};
use std::path::PathBuf;

/// The board's own `--socket`, `--server-root`, `--workspace`, and `--as`.
#[derive(Debug, Clone, Default)]
pub struct SocketArgs {
    pub socket: Option<PathBuf>,
    pub server_root: Option<PathBuf>,
    pub workspace: Option<PathBuf>,
    pub surface_hint: AsArg,
}

/// Resolve the socket to watch, in the same precedence order the verbs use:
/// `--socket`, then `ONLYNE_SOCKET`, then `--server-root`, then `--workspace`
/// or the current directory walking upward for the owner tree.
pub fn resolve_socket(args: &SocketArgs) -> Result<SocketTarget, NoSocket> {
    let mut flags = GlobalFlags::addressing(
        args.socket.clone(),
        args.server_root.clone(),
        args.workspace.clone(),
    );
    flags.surface_hint = args.surface_hint;
    socket::resolve_socket(&flags)
}

/// Refusal text is a contract, and this one is `onlyne status`'s word for word:
/// an operator who reaches a client socket gets the same sentence whichever
/// noun they used.
pub const NO_SOCKET_MESSAGE: &str = onlyne_proto::NO_SOCKET_MESSAGE;

/// What the board says when the socket it found is not the admin surface.
pub const NEEDS_ADMIN: &str = "onlyne: tui needs the admin surface; pass --server-root <dir>, \
                                or --socket <path> with --as admin";

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
            ..SocketArgs::default()
        };
        assert_eq!(resolve_socket(&args).unwrap().path, PathBuf::from("/tmp/s"));
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
            server_root: Some(root.clone()),
            ..SocketArgs::default()
        };
        let target = resolve_socket(&args).unwrap();
        assert_eq!(target.path, socket);
        assert_eq!(
            target.surface,
            crate::socket::Surface::Admin,
            "a server root is the surface the board needs"
        );
    }

    /// The board is launched from wherever the operator is, so the walk upward
    /// is what makes `onlyne tui` work from a source subdirectory.
    #[test]
    fn discovers_workspace_upwards() {
        let dir = tempfile::tempdir().expect("temp dir");
        let _runtime = runtime_dir_pinned(dir.path());
        let root = started_workspace(&dir.path().join("a"));
        let nested = root.join("b/c");
        fs::create_dir_all(&nested).expect("nested dir");
        let args = SocketArgs {
            workspace: Some(nested),
            ..SocketArgs::default()
        };
        assert_eq!(
            resolve_socket(&args).unwrap().path,
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
            workspace: Some(root.join("src")),
            ..SocketArgs::default()
        };

        let deep = started_workspace(&root);
        let served = resolve_socket(&args)
            .expect("a workspace names its socket")
            .path;
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
            workspace: Some(root),
            ..SocketArgs::default()
        };
        assert!(resolve_socket(&args).is_err());
    }
}
