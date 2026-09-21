use super::*;
use crate::session::adapter_socket::AdapterSocket;
use crate::session::dispatch::DispatchState;
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
