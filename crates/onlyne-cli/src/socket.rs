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
                surface: surface_beside(&candidate).unwrap_or(Surface::Client),
                path: candidate,
            });
        }
        if !dir.pop() {
            break;
        }
    }
    Err(NoSocket)
}

#[cfg(test)]
mod tests {
    use super::{Surface, surface_beside};
    use std::fs;

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
}
