//! Unix socket resolution and the surface it carries.

use crate::flags::{AsArg, GlobalFlags, SOCKET_ENV};
use onlyne_layout::{RoleWorkspace, ServerRoot};
use std::path::{Path, PathBuf};

/// Relative location of the served socket inside a server root or a role workspace.
pub const SOCKET_RELATIVE: &str = ".onlyne/run/s";

/// Relative location of the endpoint marker inside a server root or a role
/// workspace. The file carries the absolute path the daemon bound plus a
/// trailing newline.
pub const SOCKET_MARKER_RELATIVE: &str = ".onlyne/run/socket";

/// The surface a request travels on, which selects the op vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// Local admin socket, `<server-root>/.onlyne/run/s`.
    Admin,
    /// Role workspace socket, `<workspace>/.onlyne/run/s`.
    Client,
}

/// A resolved socket path together with the surface it serves.
#[derive(Debug, Clone)]
pub struct SocketTarget {
    pub path: PathBuf,
    pub surface: Surface,
}

/// No socket was found by any of the three resolution rules.
#[derive(Debug, Clone, Copy)]
pub struct NoSocket;

impl NoSocket {
    /// The canonical hint, owned by `onlyne_proto::text`.
    pub const MESSAGE: &'static str = onlyne_proto::NO_SOCKET_MESSAGE;
}

fn hint_surface(hint: AsArg) -> Option<Surface> {
    match hint {
        AsArg::Admin => Some(Surface::Admin),
        AsArg::Client => Some(Surface::Client),
        AsArg::Auto => None,
    }
}

/// Infer the surface of a socket given by `--socket` with no `--as` hint. The
/// tree beside the socket answers first; the canonical `.onlyne/run/s` suffix is
/// the fallback for a path whose owning database is not on disk yet.
fn infer_surface(path: &Path) -> Surface {
    surface_beside(path).unwrap_or_else(|| {
        if path.to_string_lossy().ends_with(SOCKET_RELATIVE) {
            Surface::Admin
        } else {
            Surface::Client
        }
    })
}

/// The surface a socket answers, read from the daemon that owns the tree.
///
/// Both daemons name their socket `.onlyne/run/s`, so the path cannot tell them
/// apart — and the two vocabularies differ, which is what a mis-read surface
/// turns into an `unknown_op` refusal for a verb that exists. Each daemon keeps
/// its own database in the `.onlyne/` directory it listens from: `state.db` for
/// the server, `client.db` for a role client. `None` says the path is not either
/// layout, and the caller falls back to the path's spelling.
fn surface_beside(socket: &Path) -> Option<Surface> {
    let run = socket.parent()?;
    let onlyne = run.parent()?;
    if run.file_name().and_then(|name| name.to_str()) != Some("run")
        || onlyne.file_name().and_then(|name| name.to_str()) != Some(".onlyne")
    {
        return None;
    }
    if onlyne.join("client.db").exists() {
        return Some(Surface::Client);
    }
    if onlyne.join("state.db").exists() {
        return Some(Surface::Admin);
    }
    None
}

/// The surface the flags select for a socket path carrying no other signal.
/// `--as` answers first, then the flag that names the owning tree; `fallback`
/// covers a bare path.
fn flagged_surface(flags: &GlobalFlags, fallback: impl FnOnce() -> Surface) -> Surface {
    hint_surface(flags.surface_hint)
        .or_else(|| flags.server_root.as_ref().map(|_| Surface::Admin))
        .or_else(|| flags.workspace.as_ref().map(|_| Surface::Client))
        .unwrap_or_else(fallback)
}

/// The socket named by `ONLYNE_SOCKET`, verbatim, when it carries a path.
fn env_socket() -> Option<PathBuf> {
    let raw = std::env::var(SOCKET_ENV).ok()?;
    let path = raw.trim();
    (!path.is_empty()).then(|| PathBuf::from(path))
}

/// The socket a workspace directory serves, or `None` when `dir` holds neither
/// the canonical socket nor the marker naming the served path.
///
/// The served socket can sit outside the tree, so the marker file is what a
/// caller has to find, and the surface is a property of the tree that owns it:
/// the canonical spelling names the owner, the databases beside it name the
/// vocabulary.
fn workspace_target(dir: &Path) -> Option<SocketTarget> {
    let natural = dir.join(SOCKET_RELATIVE);
    if !natural.exists() && !dir.join(SOCKET_MARKER_RELATIVE).exists() {
        return None;
    }
    Some(SocketTarget {
        path: RoleWorkspace::resolve(dir).socket_path(),
        surface: surface_beside(&natural).unwrap_or(Surface::Client),
    })
}

/// Resolve the socket path and its surface, in precedence order: `--socket`,
/// then `ONLYNE_SOCKET`, then `--server-root`, then `--workspace` or the current
/// directory walking upward for a directory that owns `.onlyne/run/s` or
/// `.onlyne/run/socket`.
pub fn resolve_socket(flags: &GlobalFlags) -> Result<SocketTarget, NoSocket> {
    if let Some(path) = &flags.socket {
        return Ok(SocketTarget {
            path: path.clone(),
            surface: flagged_surface(flags, || infer_surface(path)),
        });
    }
    // A session the client spawns is the caller that carries the variable, and
    // the socket it names is its own role client socket, so the client surface
    // is the answer when no flag names a tree.
    if let Some(path) = env_socket() {
        return Ok(SocketTarget {
            path,
            surface: flagged_surface(flags, || Surface::Client),
        });
    }
    if let Some(root) = &flags.server_root {
        return Ok(SocketTarget {
            path: ServerRoot::resolve(root.clone()).socket_path(),
            surface: Surface::Admin,
        });
    }
    let start = flags
        .workspace
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    for dir in start.ancestors() {
        if let Some(target) = workspace_target(dir) {
            return Ok(target);
        }
    }
    Err(NoSocket)
}

#[cfg(test)]
mod tests {
    use super::{NoSocket, SocketTarget, Surface, resolve_socket, surface_beside};
    use crate::Cli;
    use crate::flags::{GlobalFlags, SOCKET_ENV};
    use clap::Parser as _;
    use onlyne_layout::{RoleWorkspace, UNIX_SOCKET_PATH_MAX};
    use std::ffi::OsStr;
    use std::fs;
    use std::path::Path;

    /// Both daemons name their socket `run/s`, so the database sitting beside it
    /// is the only thing that says which vocabulary a verb should speak. The
    /// incident this guards: `onlyne handoff` from inside a server root wrote
    /// `query_ledger` into the admin socket and got `unknown op query_ledger`
    /// back for a verb that exists.
    #[test]
    fn the_tree_beside_the_socket_picks_the_surface() {
        let dir = tempfile::tempdir().expect("temp dir");
        let onlyne = dir.path().join(".onlyne");
        fs::create_dir_all(onlyne.join("run")).expect("run dir");
        let socket = onlyne.join("run").join("s");
        fs::write(&socket, "").expect("socket file");

        assert_eq!(
            surface_beside(&socket),
            None,
            "a socket with neither database beside it resolves no surface"
        );

        fs::write(onlyne.join("client.db"), "").expect("client db");
        assert_eq!(
            surface_beside(&socket),
            Some(Surface::Client),
            "a role workspace owns the socket its client binds"
        );
        fs::remove_file(onlyne.join("client.db")).expect("remove client db");

        fs::write(onlyne.join("state.db"), "").expect("state db");
        assert_eq!(
            surface_beside(&socket),
            Some(Surface::Admin),
            "a server root owns the socket its server binds"
        );

        assert_eq!(
            surface_beside(std::path::Path::new("/tmp/custom/agent.sock")),
            None,
            "a socket outside either layout leaves the spelling to the caller"
        );
    }

    /// `ONLYNE_SOCKET` is process-wide state, so every case that resolves takes
    /// this lock and hands the variable the value the case names.
    static SOCKET_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// The flags one command line carries, parsed by the same clap definitions
    /// the shipped binary uses.
    fn flags_for(args: &[&str]) -> GlobalFlags {
        let mut argv = vec!["onlyne"];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv).expect("global flags parse").flags
    }

    /// Resolve with `ONLYNE_SOCKET` holding `value`, which is the whole state
    /// the environment branch of the resolver reads.
    fn resolve_with_env(
        value: Option<&Path>,
        flags: &GlobalFlags,
    ) -> Result<SocketTarget, NoSocket> {
        let guard = SOCKET_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        unsafe {
            match value {
                Some(path) => std::env::set_var(SOCKET_ENV, path),
                None => std::env::remove_var(SOCKET_ENV),
            }
        }
        let resolved = resolve_socket(flags);
        unsafe { std::env::remove_var(SOCKET_ENV) };
        drop(guard);
        resolved
    }

    /// Resolve with the variable absent, the state a hand-run operator has.
    fn resolve(flags: &GlobalFlags) -> Result<SocketTarget, NoSocket> {
        resolve_with_env(None, flags)
    }

    /// A daemon that moved its socket outside the tree leaves `run/socket`
    /// behind, and that file is the only thing in the tree naming the live path.
    /// The incident this guards: a caller joined `.onlyne/run/s` itself and
    /// connected to a file the daemon never binds.
    #[test]
    fn marker_only_workspace_resolves_the_published_path() {
        let dir = tempfile::tempdir().expect("temp dir");
        let workspace = dir.path().join("ws");
        fs::create_dir_all(workspace.join(".onlyne/run")).expect("run dir");
        fs::create_dir_all(workspace.join("src/deep")).expect("nested dir");
        let served = dir.path().join("onlyne-short").join("s");
        fs::write(
            workspace.join(".onlyne/run/socket"),
            format!("{}\n", served.display()),
        )
        .expect("marker");

        let target = resolve(&flags_for(&[
            "--workspace",
            &workspace.join("src/deep").to_string_lossy(),
        ]))
        .expect("the marker makes the workspace an owner");
        assert_eq!(target.path, served, "the published path is the answer");
        assert_eq!(target.surface, Surface::Client);
    }

    /// A generated role workspace sits three levels below its server root, so
    /// the canonical spelling can pass [`UNIX_SOCKET_PATH_MAX`]. The resolver
    /// answers with the short served path in both moments that matter: before a
    /// daemon binds, where the length rule is the only source, and after it,
    /// where the marker is.
    #[test]
    fn long_workspace_resolves_the_short_served_path() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut workspace = dir.path().to_path_buf();
        for _ in 0..6 {
            workspace.push("role-workspace-with-a-long-name");
        }
        let run = workspace.join(".onlyne/run");
        fs::create_dir_all(run.join("src")).expect("run dir");
        let natural = run.join("s");
        fs::write(&natural, "").expect("canonical placeholder");
        assert!(
            natural.as_os_str().len() > UNIX_SOCKET_PATH_MAX,
            "the case needs a canonical spelling over the bound, got {}",
            natural.as_os_str().len()
        );
        let flags = flags_for(&["--workspace", &run.join("src").to_string_lossy()]);

        let before = resolve(&flags).expect("a deep workspace still names a socket");
        assert_eq!(before.surface, Surface::Client);
        assert_ne!(before.path, natural, "the canonical path is unbindable");
        assert!(
            before.path.as_os_str().len() <= UNIX_SOCKET_PATH_MAX,
            "the served path fits the bound, got {}",
            before.path.as_os_str().len()
        );
        assert_eq!(before.path.file_name(), Some(OsStr::new("s")));
        assert!(
            before
                .path
                .parent()
                .and_then(|dir| dir.file_name())
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("onlyne-")),
            "the short path lives in the derived home, got {}",
            before.path.display()
        );

        let endpoint = RoleWorkspace::resolve(&workspace).socket_endpoint();
        endpoint.publish().expect("publish the bound path");
        let after = resolve(&flags).expect("the marker names the socket");
        assert_eq!(after.path, endpoint.actual());
        assert!(endpoint.short(), "the case is about a moved endpoint");
        assert_eq!(
            after.path, before.path,
            "one owner tree resolves to one path across the bind"
        );
        assert_eq!(after.surface, Surface::Client);
    }

    /// The client injects `ONLYNE_SOCKET` into every session it spawns, so a
    /// verb run inside a session reaches the socket of its own role workspace,
    /// including the deep ones whose served path left the tree. `--socket` is
    /// the operator's explicit answer and outranks the environment.
    #[test]
    fn onlyne_socket_names_the_target_and_socket_flag_outranks_it() {
        let dir = tempfile::tempdir().expect("temp dir");
        let workspace = dir.path().join("ws");
        fs::create_dir_all(workspace.join(".onlyne/run")).expect("run dir");
        let canonical = workspace.join(".onlyne/run/s");
        fs::write(&canonical, "").expect("socket placeholder");
        let served = dir.path().join("session.sock");

        let target = resolve_with_env(Some(&served), &flags_for(&[]))
            .expect("the environment names a socket");
        assert_eq!(target.path, served);
        assert_eq!(
            target.surface,
            Surface::Client,
            "a session speaks to its role client"
        );

        let flagged = resolve_with_env(
            Some(&served),
            &flags_for(&[
                "--socket",
                &dir.path().join("explicit.sock").to_string_lossy(),
            ]),
        )
        .expect("the flag names a socket");
        assert_eq!(flagged.path, dir.path().join("explicit.sock"));

        let hinted = resolve_with_env(Some(&served), &flags_for(&["--as", "admin"]))
            .expect("the environment names a socket");
        assert_eq!(hinted.path, served);
        assert_eq!(
            hinted.surface,
            Surface::Admin,
            "`--as` keeps selecting the vocabulary"
        );

        let rooted = resolve_with_env(
            Some(&served),
            &flags_for(&["--server-root", &workspace.to_string_lossy()]),
        )
        .expect("the environment names a socket");
        assert_eq!(
            rooted.path, served,
            "the environment reaches the caller's own socket"
        );
        assert_eq!(rooted.surface, Surface::Admin, "`--server-root` names it");

        let blank = resolve_with_env(
            Some(Path::new("   ")),
            &flags_for(&["--workspace", &workspace.to_string_lossy()]),
        )
        .expect("a workspace owns the socket");
        assert_eq!(
            blank.path, canonical,
            "an empty value carries no path, so the walk answers"
        );
    }

    /// `--server-root` selects the server through the same endpoint machinery, so
    /// a cluster that moved its admin socket is addressed by its root directory.
    #[test]
    fn server_root_resolves_the_published_admin_path() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().join("srv");
        fs::create_dir_all(root.join(".onlyne/run")).expect("run dir");
        let served = dir.path().join("onlyne-admin").join("s");
        fs::write(
            root.join(".onlyne/run/socket"),
            format!("{}\n", served.display()),
        )
        .expect("marker");

        let target = resolve(&flags_for(&["--server-root", &root.to_string_lossy()]))
            .expect("the root names the server socket");
        assert_eq!(target.path, served);
        assert_eq!(target.surface, Surface::Admin);
    }
}
