//! Liveness for one role workspace.
//!
//! The client never detaches itself: `run` is the only launch verb, and
//! backgrounding is the operator's job — a visible terminal tab, `launchd`, or
//! `nohup`. So no pid file is written and nothing signals a process by number.
//! `status` asks the workspace socket instead: a client is running when its
//! adapter socket answers the admin `hello` probe, and the socket file's mtime
//! dates that client.

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
    let Some(connected) = crate::adapter_socket::server_link_state(&socket).await else {
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
mod tests {
    use super::*;
    use crate::adapter_socket::AdapterSocket;
    use crate::dispatch::DispatchState;
    use onlyne_session::backend::fake::FakeBackend;
    use onlyne_store::ClientStore;
    use std::sync::Arc;
    use tempfile::tempdir;

    /// Serve the workspace socket the way `run` does, and answer with the
    /// runtime whose link state the probe reads.
    async fn serve_workspace(
        dir: &Path,
    ) -> (DispatchState, tokio::task::JoinHandle<anyhow::Result<()>>) {
        let layout = RoleWorkspace::resolve(dir);
        layout.bootstrap().unwrap();
        let store = ClientStore::open(layout.client_db_path()).unwrap();
        let state = DispatchState::new(
            "planner",
            dir,
            vec!["agent".into()],
            1,
            false,
            Arc::new(FakeBackend::new()),
            store,
        );
        let adapter = AdapterSocket {
            workspace: dir.to_path_buf(),
            role: "planner".into(),
            cluster: "c".into(),
            server: "s".into(),
            dispatch: state.clone(),
        };
        let host = tokio::spawn(adapter.serve());
        for _ in 0..100 {
            if layout.socket_path().exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            layout.socket_path().exists(),
            "the host bound the workspace socket"
        );
        (state, host)
    }

    #[tokio::test]
    async fn a_workspace_without_a_socket_has_no_status() {
        let dir = tempdir().unwrap();
        assert_eq!(status(dir.path()).await.unwrap(), None);
    }

    /// A socket file an unclean exit left behind answers nothing, so the verb
    /// refuses it like any other workspace with no client.
    #[tokio::test]
    async fn a_socket_file_nobody_answers_has_no_status() {
        let dir = tempdir().unwrap();
        let layout = RoleWorkspace::resolve(dir.path());
        layout.bootstrap().unwrap();
        std::fs::write(layout.socket_path(), "left over from an unclean exit\n").unwrap();
        assert_eq!(status(dir.path()).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_serving_client_reports_socket_uptime_and_faults() {
        let dir = tempdir().unwrap();
        let layout = RoleWorkspace::resolve(dir.path());
        let (state, host) = serve_workspace(dir.path()).await;
        state.set_link_up(true);

        let report = status(dir.path())
            .await
            .unwrap()
            .expect("a serving socket is a running client");
        assert_eq!(report.socket, layout.socket_path());
        assert_eq!(report.faults, 0);
        assert!(report.connected);
        assert_eq!(report.exit_code(), 0);
        assert!(report.line().contains("faults 0"));
        assert!(!report.line().contains("pid"));

        // The socket file's mtime is the uptime's source, so it grows with the
        // wall clock while the same client keeps serving.
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let later = status(dir.path())
            .await
            .unwrap()
            .expect("the client still serves");
        assert!(later.uptime >= report.uptime + Duration::from_secs(1));

        // The client stays running with its server link down; the verb still
        // refuses, because its work cannot reach the cluster.
        state.set_link_up(false);
        let down = status(dir.path())
            .await
            .unwrap()
            .expect("a client without a link is still running");
        assert!(!down.connected);
        assert_eq!(down.exit_code(), 2);
        host.abort();
    }
}
