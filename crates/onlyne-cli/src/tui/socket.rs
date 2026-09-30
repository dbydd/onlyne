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
