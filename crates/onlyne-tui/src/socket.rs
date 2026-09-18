use onlyne_layout::{RoleWorkspace, ServerRoot};
use std::path::{Path, PathBuf};

/// Relative location of the served socket inside a server root or a role workspace.
pub const SOCKET_RELATIVE: &str = ".onlyne/run/s";

/// Relative location of the endpoint marker inside a server root or a role
/// workspace. The file carries the absolute path the daemon bound plus a
/// trailing newline.
pub const SOCKET_MARKER_RELATIVE: &str = ".onlyne/run/socket";

pub const NO_SOCKET_MESSAGE: &str = onlyne_proto::NO_SOCKET_MESSAGE;

#[derive(Debug, Clone, Default)]
pub struct SocketArgs {
    pub socket: Option<PathBuf>,
    pub server_root: Option<PathBuf>,
    pub workspace: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoSocket;

/// Resolve the socket to watch, in precedence order: `--socket`, then
/// `--server-root`, then `--workspace` or the current directory walking upward
/// for a directory that owns `.onlyne/run/s` or `.onlyne/run/socket`.
pub fn resolve_socket(args: &SocketArgs) -> Result<PathBuf, NoSocket> {
    if let Some(path) = &args.socket {
        return Ok(path.clone());
    }
    if let Some(root) = &args.server_root {
        return Ok(ServerRoot::resolve(root.clone()).socket_path());
    }
    let start = args
        .workspace
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    for dir in start.ancestors() {
        if let Some(path) = workspace_socket(dir) {
            return Ok(path);
        }
    }
    Err(NoSocket)
}

/// The socket a workspace directory serves, or `None` when `dir` holds neither
/// the canonical socket nor the marker naming the served path.
///
/// The served socket can sit outside the tree, so the marker file is what a
/// caller has to find.
fn workspace_socket(dir: &Path) -> Option<PathBuf> {
    let natural = dir.join(SOCKET_RELATIVE);
    if !natural.exists() && !dir.join(SOCKET_MARKER_RELATIVE).exists() {
        return None;
    }
    Some(RoleWorkspace::resolve(dir).socket_path())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn socket_flag_wins() {
        let args = SocketArgs {
            socket: Some(PathBuf::from("/tmp/s")),
            server_root: Some(PathBuf::from("/tmp/root")),
            workspace: None,
        };
        assert_eq!(resolve_socket(&args).unwrap(), PathBuf::from("/tmp/s"));
    }

    #[test]
    fn server_root_maps_to_admin_socket() {
        let args = SocketArgs {
            socket: None,
            server_root: Some(PathBuf::from("srv")),
            workspace: None,
        };
        assert_eq!(
            resolve_socket(&args).unwrap(),
            PathBuf::from("srv/.onlyne/run/s")
        );
    }

    /// A cluster that moved its admin socket off the canonical spelling is still
    /// addressed by its root directory, which is what the operator types.
    #[cfg(unix)]
    #[test]
    fn server_root_reads_the_published_path() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().join("srv");
        fs::create_dir_all(root.join(".onlyne/run")).expect("run dir");
        let served = root.join("admin-short").join("s");
        fs::write(
            root.join(".onlyne/run/socket"),
            format!("{}\n", served.display()),
        )
        .expect("marker");
        let args = SocketArgs {
            socket: None,
            server_root: Some(root),
            workspace: None,
        };
        assert_eq!(resolve_socket(&args).unwrap(), served);
    }

    #[test]
    fn discovers_workspace_upwards() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("a");
        let nested = root.join("b/c");
        std::fs::create_dir_all(root.join(".onlyne/run")).unwrap();
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(root.join(".onlyne/run/s"), b"not-a-socket").unwrap();
        let args = SocketArgs {
            socket: None,
            server_root: None,
            workspace: Some(nested),
        };
        assert_eq!(resolve_socket(&args).unwrap(), root.join(".onlyne/run/s"));
    }

    /// The marker alone makes a directory an owner: a daemon whose socket lives
    /// outside the tree leaves the live path in `run/socket`, and a watcher that
    /// joined `run/s` itself would sit on a file nobody binds.
    #[cfg(unix)]
    #[test]
    fn marker_only_workspace_resolves_the_published_path() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let root = tmp.path().join("ws");
        fs::create_dir_all(root.join(".onlyne/run")).expect("run dir");
        fs::create_dir_all(root.join("src/deep")).expect("nested dir");
        let served = tmp.path().join("onlyne-short").join("s");
        fs::write(
            root.join(".onlyne/run/socket"),
            format!("{}\n", served.display()),
        )
        .expect("marker");
        let args = SocketArgs {
            socket: None,
            server_root: None,
            workspace: Some(root.join("src/deep")),
        };
        assert_eq!(resolve_socket(&args).unwrap(), served);
    }

    /// A generated role workspace nests three levels below its server root, so
    /// the canonical spelling can pass `UNIX_SOCKET_PATH_MAX` and the bound path
    /// is a short derived one. Both moments answer one path: the length rule
    /// before a daemon binds, the marker after.
    #[cfg(unix)]
    #[test]
    fn long_workspace_resolves_the_short_served_path() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let mut root = tmp.path().to_path_buf();
        for _ in 0..6 {
            root.push("role-workspace-with-a-long-name");
        }
        let run = root.join(".onlyne/run");
        fs::create_dir_all(run.join("src")).expect("run dir");
        let natural = run.join("s");
        fs::write(&natural, "").expect("canonical placeholder");
        assert!(
            natural.as_os_str().len() > onlyne_layout::UNIX_SOCKET_PATH_MAX,
            "the case needs a canonical spelling over the bound, got {}",
            natural.as_os_str().len()
        );
        let args = SocketArgs {
            socket: None,
            server_root: None,
            workspace: Some(run.join("src")),
        };

        let before = resolve_socket(&args).expect("a deep workspace names a socket");
        assert_ne!(before, natural, "the canonical path is unbindable");
        assert!(
            before.as_os_str().len() <= onlyne_layout::UNIX_SOCKET_PATH_MAX,
            "the served path fits the bound, got {}",
            before.as_os_str().len()
        );

        let endpoint = RoleWorkspace::resolve(&root).socket_endpoint();
        endpoint.publish().expect("publish the bound path");
        let after = resolve_socket(&args).expect("the marker names the socket");
        assert_eq!(after, endpoint.actual());
        assert_eq!(
            after, before,
            "one owner tree resolves to one path across the bind"
        );
    }

    /// Windows binds no `sun_path`: `<run>/s` is a regular marker file naming the
    /// NPFS pipe the daemon holds, so the canonical spelling needs no length rule
    /// and there is no `run/socket` to publish. The same deep workspace still
    /// answers one canonical path before and after endpoint publication.
    #[cfg(not(unix))]
    #[test]
    fn long_workspace_resolves_the_canonical_spelling() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let mut root = tmp.path().to_path_buf();
        for _ in 0..6 {
            root.push("role-workspace-with-a-long-name");
        }
        let run = root.join(".onlyne/run");
        fs::create_dir_all(run.join("src")).expect("run dir");
        let natural = run.join("s");
        fs::write(&natural, "").expect("canonical placeholder");
        assert!(
            natural.as_os_str().len() > onlyne_layout::UNIX_SOCKET_PATH_MAX,
            "the case needs a canonical spelling over the bound, got {}",
            natural.as_os_str().len()
        );
        let args = SocketArgs {
            socket: None,
            server_root: None,
            workspace: Some(run.join("src")),
        };

        let before = resolve_socket(&args).expect("a deep workspace names a socket");
        assert_eq!(
            before, natural,
            "the canonical spelling is the one bound here"
        );

        let endpoint = RoleWorkspace::resolve(&root).socket_endpoint();
        endpoint.publish().expect("publish the endpoint");
        assert_eq!(endpoint.actual(), endpoint.natural());
        assert!(!endpoint.short(), "the canonical path needs no stand-in");
        let after = resolve_socket(&args).expect("the owner tree still names the socket");
        assert_eq!(after, endpoint.actual());
        assert_eq!(
            after, before,
            "one owner tree resolves to one path across the bind"
        );
    }
}
