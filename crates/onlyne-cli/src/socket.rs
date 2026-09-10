//! Unix socket resolution and the surface it carries.

use crate::flags::{AsArg, GlobalFlags};
use std::path::{Path, PathBuf};

/// Relative location of the daemon socket inside a server root or a role workspace.
pub const SOCKET_RELATIVE: &str = ".onlyne/run/s";

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

/// `auto` infers the admin surface from the canonical socket suffix, since that
/// path is what `--server-root` produces. Any other path is a client socket.
fn infer_surface(path: &Path) -> Surface {
    if path.to_string_lossy().ends_with(SOCKET_RELATIVE) {
        Surface::Admin
    } else {
        Surface::Client
    }
}

/// Resolve the socket path and its surface, in precedence order:
/// `--socket`, then `--server-root`, then `--workspace` or the current
/// directory walking upward for `.onlyne/run/s`.
pub fn resolve_socket(flags: &GlobalFlags) -> Result<SocketTarget, NoSocket> {
    if let Some(path) = &flags.socket {
        let surface = hint_surface(flags.surface_hint)
            .or_else(|| flags.server_root.as_ref().map(|_| Surface::Admin))
            .or_else(|| flags.workspace.as_ref().map(|_| Surface::Client))
            .unwrap_or_else(|| infer_surface(path));
        return Ok(SocketTarget {
            path: path.clone(),
            surface,
        });
    }
    if let Some(root) = &flags.server_root {
        let path = root.join(SOCKET_RELATIVE);
        return Ok(SocketTarget {
            path,
            surface: Surface::Admin,
        });
    }
    let start = flags
        .workspace
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let mut dir = start;
    loop {
        let candidate = dir.join(SOCKET_RELATIVE);
        if candidate.exists() {
            return Ok(SocketTarget {
                path: candidate,
                surface: Surface::Client,
            });
        }
        if !dir.pop() {
            break;
        }
    }
    Err(NoSocket)
}
