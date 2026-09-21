//! Liveness for one role workspace.
//!
//! The client never detaches itself: `run` is the only launch verb, and
//! backgrounding is the operator's job — a visible terminal tab, `launchd`, or
//! `nohup`. So no pid file is written and nothing signals a process by number.
//! `status` asks the workspace socket instead: a client is running when its
//! adapter socket answers the admin `hello` probe, and the socket file's mtime
//! dates that client. On Windows the path is a marker file, which still carries
//! an mtime, so uptime stays the age of the bound name.

use anyhow::Result;
use onlyne_layout::RoleWorkspace;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Byte-exact answer for every verb that needs a live client.
pub const NOT_RUNNING: &str = "onlyne: client not running";
/// Byte-exact answer for a live client whose server link is down.
pub const NOT_CONNECTED: &str = "onlyne: client not connected";
/// Event window scanned when `status` counts recorded faults.
pub const FAULT_SCAN_LIMIT: u32 = 10_000;

/// Facts `status` reports for a live client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusReport {
    pub uptime: Duration,
    pub socket: PathBuf,
    pub faults: usize,
    /// Whether the client holds a ready server link.
    pub connected: bool,
}

impl StatusReport {
    /// One operator line carrying every reported field.
    pub fn line(&self) -> String {
        format!(
            "onlyne: client running uptime {}s socket {} faults {}",
            self.uptime.as_secs(),
            self.socket.display(),
            self.faults
        )
    }

    /// Process exit code for the `status` verb: zero for a client that is up
    /// and connected to its server, and the refusal code otherwise.
    pub fn exit_code(&self) -> i32 {
        if self.connected { 0 } else { 2 }
    }
}

/// Report the client serving `workspace`, and `None` when none is running.
///
/// A client is running when its adapter socket answers an `admin` `hello`: a
/// socket file no process answers is what an unclean exit leaves behind, and
/// this verb refuses it exactly as it refuses a missing socket. The link state
/// is the fact the answering client holds, and the uptime is the age of the
/// socket file that client bound.
pub async fn status(workspace: &Path) -> Result<Option<StatusReport>> {
    let layout = RoleWorkspace::resolve(workspace);
    let socket = layout.socket_path();
    let Some(connected) = crate::session::adapter_socket::server_link_state(&socket).await else {
        return Ok(None);
    };
    let uptime = std::fs::metadata(&socket)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| std::time::SystemTime::now().duration_since(modified).ok())
        .unwrap_or_default();
    Ok(Some(StatusReport {
        uptime,
        faults: fault_count(&layout)?,
        connected,
        socket,
    }))
}

/// Count the `session_fault` events recorded in the client database.
pub fn fault_count(layout: &RoleWorkspace) -> Result<usize> {
    let path = layout.client_db_path();
    if !path.exists() {
        return Ok(0);
    }
    let store = onlyne_store::ClientStore::open(&path)?;
    let events = store.events_since(0, FAULT_SCAN_LIMIT)?;
    Ok(events
        .iter()
        .filter(|event| event.kind == "session_fault")
        .count())
}

#[cfg(test)]
mod tests;
