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
        return socket_path(&root)
            .ok()
            .filter(|path| path.exists())
            .map(|_| root);
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
