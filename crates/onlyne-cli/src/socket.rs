//! Unix socket resolution and the surface it carries.
//!
//! v2 moves every socket out of the workspace into the machine-level runtime
//! directory, `<runtime_dir>/<digest>.sock`, where the digest covers the
//! canonical workspace root. One path per owner tree, no `run/s` vs derived-path
//! split, and no `sun_path` length rule at all.
//!
//! The CLI's job is to find the owner tree and hand back the path the daemon
//! actually bound. The tree is found by walking upward for a `.onlyne/`
//! directory; the surface comes from the registration file that sits beside the
//! socket, which is the one place the daemon states which vocabulary it speaks.

use crate::flags::{AsArg, GlobalFlags, SOCKET_ENV};
#[cfg(test)]
use onlyne_wire::socket::RUNTIME_DIR_ENV;
use onlyne_wire::socket::{RegistrationKind, read_registration, socket_path};
use std::path::{Path, PathBuf};

/// The surface a request travels on, which selects the op vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// The local admin socket, bound by the server daemon.
    Admin,
    /// The role workspace socket, bound by a role client daemon.
    Client,
}

/// A resolved socket path together with the surface it serves.
#[derive(Debug, Clone)]
pub struct SocketTarget {
    pub path: PathBuf,
    pub surface: Surface,
}

/// No socket was found by any of the resolution rules.
#[derive(Debug, Clone, Copy)]
pub struct NoSocket;

impl NoSocket {
    /// The canonical hint, owned by `onlyne_proto::text`.
    pub const MESSAGE: &'static str = onlyne_proto::NO_SOCKET_MESSAGE;
}

/// The directory that marks a tree as a onlyne owner, walked upward from a
/// starting directory.
const OWNER_DIR: &str = ".onlyne";

/// Pin the runtime directory for the duration of a test, inside `temp` so the
/// tempdir's own removal takes the sockets and registrations with it.
///
/// `ONLYNE_RUNTIME_DIR` is process-global and `runtime_dir()` reads it at every
/// call, never caching, so a test that left it set would hand the next
/// concurrent test a directory some earlier test had already populated. The
/// lock is crate-wide for the same reason: the variable is one process-wide
/// name shared by every socket test in this binary, so two modules holding
/// private locks would still race.
#[cfg(test)]
pub(crate) fn runtime_dir_pinned(temp: &Path) -> PinnedRuntimeDir {
    // The guard is returned holding the lock, so the variable stays pinned for
    // the whole test. Re-acquiring the lock only in `Drop` would leave every
    // concurrent test sharing one process-wide name.
    let guard = RUNTIME_DIR_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    unsafe { std::env::set_var(RUNTIME_DIR_ENV, temp.join("runtime")) };
    PinnedRuntimeDir { _guard: guard }
}

#[cfg(test)]
static RUNTIME_DIR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Held for the whole resolve/publish/assert phase of a test; dropping it
/// restores the environment and releases the lock.
#[cfg(test)]
pub(crate) struct PinnedRuntimeDir {
    _guard: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl Drop for PinnedRuntimeDir {
    fn drop(&mut self) {
        // The lock is still held here, so unsetting cannot erase a directory
        // the next test has already pinned.
        unsafe { std::env::remove_var(RUNTIME_DIR_ENV) };
    }
}

fn hint_surface(hint: AsArg) -> Option<Surface> {
    match hint {
        AsArg::Admin => Some(Surface::Admin),
        AsArg::Client => Some(Surface::Client),
        AsArg::Auto => None,
    }
}

/// The surface a registration file states, `None` when the file is absent or
/// unreadable.
///
/// The registration file is the daemon's own declaration of which vocabulary it
/// speaks, so it answers before any flag hint: a tree whose daemon has
/// registered is a known surface, not a guess. A malformed file reads as no
/// answer, and the caller's flag hint stands in.
fn registered_surface(root: &Path) -> Option<Surface> {
    let registration = read_registration(root).ok().flatten()?;
    Some(match registration.kind {
        RegistrationKind::Server => Surface::Admin,
        RegistrationKind::Client => Surface::Client,
    })
}

/// The owner tree a flag names, or `None` when the caller named none and the
/// current directory is the walk's starting point.
fn root_from_flags(flags: &GlobalFlags) -> Option<PathBuf> {
    if let Some(root) = &flags.server_root {
        return Some(root.clone());
    }
    flags.workspace.clone()
}

/// The surface for a socket whose owner tree is `root`.
///
/// The registration file answers first, then `--as`, then the flag that named
/// the tree, then the standing assumption that an unflagged tree is a role
/// workspace.
fn surface_for(flags: &GlobalFlags, root: &Path) -> Surface {
    registered_surface(root)
        .or_else(|| hint_surface(flags.surface_hint))
        .or_else(|| flags.server_root.as_ref().map(|_| Surface::Admin))
        .or_else(|| flags.workspace.as_ref().map(|_| Surface::Client))
        .unwrap_or(Surface::Client)
}

/// The owner tree `dir` belongs to, or `None` when `dir` is not inside one.
///
/// A directory is an owner when it holds `.onlyne/`; the walk then continues
/// upward, so a workspace subdirectory resolves to the same socket its root
/// does. The registration file beside the socket is the authority on whether a
/// daemon is live — a tree that exists but has never been started has no
/// socket, and a path the CLI cannot connect to is a worse answer than a clear
/// refusal.
fn owner_root(dir: &Path) -> Option<PathBuf> {
    if dir.join(OWNER_DIR).is_dir() {
        let root = dir.to_path_buf();
        return socket_path(&root).ok().filter(|path| path.exists()).map(|_| root);
    }
    dir.parent()
        .and_then(|parent| (parent != dir).then(|| owner_root(parent)).flatten())
}

/// The socket for the owner tree containing the current directory, or
/// [`NoSocket`] when the caller's tree has no daemon running.
pub fn resolve_socket_from_cwd() -> Result<SocketTarget, NoSocket> {
    let cwd = std::env::current_dir().map_err(|_| NoSocket)?;
    resolve_socket_from(&cwd)
}

/// [`resolve_socket_from_cwd`], with the starting directory given explicitly so
/// the walk is testable without moving the process.
fn resolve_socket_from(start: &Path) -> Result<SocketTarget, NoSocket> {
    let root = owner_root(start).ok_or(NoSocket)?;
    let path = socket_path(&root).map_err(|_| NoSocket)?;
    Ok(SocketTarget {
        path,
        // No flag names the tree, so the registration file is the only thing
        // that can say which vocabulary the caller should speak; a workspace
        // that has never registered is a role client.
        surface: registered_surface(&root).unwrap_or(Surface::Client),
    })
}

/// A `--socket` value as a path: absolute values are used verbatim, relative
/// ones resolve against the current directory, which is the shell's contract
/// for a path argument.
fn flag_socket(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

/// The socket named by `ONLYNE_SOCKET`, verbatim, when it carries a path.
fn env_socket() -> Option<PathBuf> {
    let raw = std::env::var(SOCKET_ENV).ok()?;
    let path = raw.trim();
    (!path.is_empty()).then(|| PathBuf::from(path))
}

/// Resolve the socket path and its surface, in precedence order: `--socket`,
/// then `ONLYNE_SOCKET`, then `--server-root`, then `--workspace` or the
/// current directory walking upward for the owner tree.
pub fn resolve_socket(flags: &GlobalFlags) -> Result<SocketTarget, NoSocket> {
    if let Some(path) = &flags.socket {
        let path = flag_socket(path);
        // An explicit path names a socket the caller's tree need not own, so
        // there is no registration file to read and the hint stands in.
        let surface = hint_surface(flags.surface_hint).unwrap_or(Surface::Client);
        return Ok(SocketTarget { path, surface });
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
    if let Some(start) = root_from_flags(flags) {
        let start = std::path::absolute(&start).unwrap_or(start);
        if let Some(root) = owner_root(&start) {
            let path = socket_path(&root).map_err(|_| NoSocket)?;
            return Ok(SocketTarget {
                path,
                surface: surface_for(flags, &root),
            });
        }
        return Err(NoSocket);
    }
    resolve_socket_from_cwd()
}

/// The surface the flags select for a socket path carrying no other signal.
fn flagged_surface(flags: &GlobalFlags, fallback: impl FnOnce() -> Surface) -> Surface {
    hint_surface(flags.surface_hint)
        .or_else(|| flags.server_root.as_ref().map(|_| Surface::Admin))
        .or_else(|| flags.workspace.as_ref().map(|_| Surface::Client))
        .unwrap_or_else(fallback)
}

#[cfg(test)]
mod tests {
    use super::{
        NoSocket, SocketTarget, Surface, owner_root, resolve_socket, resolve_socket_from,
        resolve_socket_from_cwd,
    };
    use crate::Cli;
    use crate::flags::{GlobalFlags, SOCKET_ENV};
    use crate::socket::runtime_dir_pinned;
    use clap::Parser as _;
    use onlyne_wire::socket::{
        RegistrationFile, RegistrationKind, socket_path, write_registration,
    };
    use std::fs;
    use std::path::{Path, PathBuf};

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

    /// An owner tree with `.onlyne/` present, holding a started daemon.
    ///
    /// The registration file is written beside the socket because it is what
    /// says the daemon is live and which surface it serves; a tree with only
    /// `.onlyne/` is the "not started" case the tests below want to see refused.
    fn started_workspace(dir: &Path, kind: RegistrationKind) -> PathBuf {
        fs::create_dir_all(dir.join(".onlyne")).expect("owner dir");
        let socket = socket_path(dir).expect("socket path");
        // The daemon creates the runtime directory when it binds; the resolver
        // only reads it, so a test that plays the daemon's part makes it.
        fs::create_dir_all(socket.parent().expect("runtime dir")).expect("runtime dir");
        fs::write(&socket, "").expect("socket file");
        write_registration(
            dir,
            &RegistrationFile {
                kind,
                role: None,
                root: dir.to_path_buf(),
                pid: std::process::id(),
                version: "test".to_string(),
                runtime: None,
            },
        )
        .expect("registration");
        dir.to_path_buf()
    }

    /// A workspace subdirectory resolves to the same socket as its root. The
    /// incident this guards: a caller run from `crates/foo/` finds no socket
    /// because the walk gave up at the first directory without `run/s`.
    #[test]
    fn a_workspace_subdirectory_resolves_its_roots_socket() {
        let dir = tempfile::tempdir().expect("temp dir");
        let _runtime = runtime_dir_pinned(dir.path());
        let root = started_workspace(dir.path(), RegistrationKind::Client);
        let nested = root.join("crates/onlyne-cli/src");
        fs::create_dir_all(&nested).expect("nested dir");

        assert_eq!(owner_root(&nested), Some(root.clone()));
        let target = resolve_socket_from(&nested).expect("the walk reaches the owner tree");
        assert_eq!(target.path, socket_path(&root).expect("socket path"));
        assert_eq!(target.surface, Surface::Client);

        let sibling = root.join("plugins");
        fs::create_dir_all(&sibling).expect("sibling dir");
        assert_eq!(
            resolve_socket_from(&sibling).expect("a sibling resolves too").path,
            target.path,
            "one owner tree answers every directory inside it with one path"
        );
    }

    /// The registration file states the surface, so an admin verb aimed at a
    /// server root never has to guess from the path. The incident this guards:
    /// `onlyne handoff` from inside a server root wrote `query_ledger` into the
    /// admin socket and got `unknown op query_ledger` for a verb that exists.
    #[test]
    fn the_registration_file_picks_the_surface() {
        let dir = tempfile::tempdir().expect("temp dir");
        let _runtime = runtime_dir_pinned(dir.path());
        let root = started_workspace(dir.path(), RegistrationKind::Server);
        let deep = root.join("src/deep");
        fs::create_dir_all(&deep).expect("nested dir");

        let target = resolve_socket_from(&deep).expect("the owner tree names a socket");
        assert_eq!(
            target.surface,
            Surface::Admin,
            "a server root's registration states the admin vocabulary"
        );

        let client = started_workspace(&dir.path().join("ws"), RegistrationKind::Client);
        assert_eq!(
            resolve_socket_from(&client.join(".")).expect("client tree").surface,
            Surface::Client
        );
    }

    /// A tree that has never been started has `.onlyne/` and no socket. The
    /// resolver must say so rather than hand back a path nothing listens on,
    /// because the caller's next step is a connect error with no explanation.
    #[test]
    fn a_tree_without_a_daemon_is_refused_with_the_canonical_message() {
        let dir = tempfile::tempdir().expect("temp dir");
        let _runtime = runtime_dir_pinned(dir.path());
        let root = dir.path().join("ws");
        fs::create_dir_all(root.join(".onlyne")).expect("owner dir");

        assert_eq!(owner_root(&root), None);
        assert!(matches!(resolve_socket_from(&root), Err(NoSocket)));
        assert!(NoSocket::MESSAGE.contains("--server-root"));
    }

    /// A directory outside any onlyne tree refuses the same way, and
    /// `resolve_socket_from_cwd` is the entry point an operator hits, so it is
    /// the one that must not panic when the process has no usable directory.
    #[test]
    fn a_directory_outside_any_tree_refuses() {
        let dir = tempfile::tempdir().expect("temp dir");
        let _runtime = runtime_dir_pinned(dir.path());
        // A temp dir has no `.onlyne` above it under `/var/folders/...`, so the
        // walk runs to the filesystem root and finds nothing.
        if let Ok(target) = resolve_socket_from(dir.path()) {
            assert!(target.path.exists(), "only a real owner tree resolves");
        }
        let _ = resolve_socket_from_cwd();
    }

    /// The client injects `ONLYNE_SOCKET` into every session it spawns, so a
    /// verb run inside a session reaches the socket of its own role workspace.
    /// `--socket` is the operator's explicit answer and outranks it, and a
    /// relative `--socket` resolves against the current directory.
    #[test]
    fn onlyne_socket_names_the_target_and_socket_flag_outranks_it() {
        let dir = tempfile::tempdir().expect("temp dir");
        let _runtime = runtime_dir_pinned(dir.path());
        let root = started_workspace(dir.path(), RegistrationKind::Client);
        let served = dir.path().join("session.sock");
        fs::write(&served, "").expect("served placeholder");

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

        let blank = resolve_with_env(
            Some(Path::new("   ")),
            &flags_for(&["--workspace", &root.to_string_lossy()]),
        )
        .expect("a workspace owns the socket");
        assert_eq!(
            blank.path,
            socket_path(&root).expect("socket path"),
            "an empty value carries no path, so the walk answers"
        );
    }

    /// A relative `--socket` is resolved against the current directory, which is
    /// what an operator passing `run/s` from inside a tree expects. The
    /// assertion is on the join, not on any particular working directory.
    #[test]
    fn a_relative_socket_flag_resolves_against_the_current_directory() {
        let relative = "relative.sock";
        let target = resolve(&flags_for(&["--socket", relative])).expect("the flag names a socket");
        let expected = std::env::current_dir()
            .expect("cwd")
            .join(relative);
        assert_eq!(target.path, expected);
        assert!(
            target.path.is_absolute(),
            "a relative flag value reaches the caller as an absolute path"
        );

        let absolute = resolve(&flags_for(&["--socket", "/tmp/absolute.sock"]))
            .expect("an absolute flag names a socket");
        assert_eq!(absolute.path, Path::new("/tmp/absolute.sock"));
    }
}
