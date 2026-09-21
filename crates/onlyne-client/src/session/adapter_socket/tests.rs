use super::*;
use onlyne_proto::MountKind;

#[cfg(unix)]
use crate::session::dispatch::DispatchState;
#[cfg(unix)]
use onlyne_layout::RoleWorkspace;
#[cfg(unix)]
use onlyne_session::backend::fake::FakeBackend;
#[cfg(unix)]
use onlyne_store::ClientStore;
#[cfg(unix)]
use std::path::Path;
#[cfg(unix)]
use tempfile::tempdir;

#[cfg(unix)]
fn dispatch_state(workspace: &Path) -> DispatchState {
    let layout = RoleWorkspace::resolve(workspace);
    layout.bootstrap().unwrap();
    let store = ClientStore::open(layout.client_db_path()).unwrap();
    DispatchState::new(
        "planner",
        workspace,
        vec!["agent".into()],
        1,
        std::sync::Arc::new(FakeBackend::new()),
        store,
    )
}

/// A workspace whose canonical socket spelling is over the unix bound binds
/// the short path, names it in the marker, and answers for it through the one
/// accessor the clients use.
///
/// The hand-joined canonical leaf is the shape this case replaces: past the
/// bound it fails to bind, and the client keeps a server link while its local
/// surface stays shut. Windows keeps the canonical spelling as the bound
/// spelling, so the premise lives on unix.
#[cfg(unix)]
#[tokio::test]
async fn a_deep_workspace_serves_the_short_endpoint() {
    use onlyne_layout::UNIX_SOCKET_PATH_MAX;
    let segment = "deep-workspace-segment-aaaaaaaaaaaaaaaaaaaaaa";
    let dir = tempdir().unwrap();
    let workspace = dir.path().join(segment).join(segment).join("leaf");
    std::fs::create_dir_all(&workspace).unwrap();
    let layout = RoleWorkspace::resolve(&workspace);
    let adapter = AdapterSocket {
        workspace: workspace.clone(),
        role: "planner".into(),
        cluster: "c".into(),
        server: "s".into(),
        dispatch: dispatch_state(&workspace),
    };
    assert!(
        layout.socket_path_natural().as_os_str().len() > UNIX_SOCKET_PATH_MAX,
        "the premise: {} bytes at {}",
        layout.socket_path_natural().as_os_str().len(),
        layout.socket_path_natural().display(),
    );

    let (listener, endpoint) = adapter.bind().await.unwrap();
    assert!(
        endpoint.short(),
        "a canonical path over the bound moves the socket: {}",
        endpoint.actual().display(),
    );
    assert!(
        endpoint.actual().as_os_str().len() <= UNIX_SOCKET_PATH_MAX,
        "the served path fits the bound: {} bytes at {}",
        endpoint.actual().as_os_str().len(),
        endpoint.actual().display(),
    );
    assert_eq!(
        adapter.path(),
        endpoint.actual().to_path_buf(),
        "the accessor answers the path that was bound",
    );
    assert_eq!(
        std::fs::read_to_string(endpoint.marker()).unwrap().trim(),
        endpoint.actual().to_string_lossy().as_ref(),
        "the marker names the served path",
    );
    assert!(
        !endpoint.natural().exists(),
        "the canonical leaf stays empty: {}",
        endpoint.natural().display(),
    );
    drop(listener);
    let _ = std::fs::remove_file(endpoint.actual());
    let _ = std::fs::remove_dir(endpoint.actual().parent().unwrap());
}

#[test]
fn admin_probe_mounts_without_a_marker() {
    let agent = onlyne_proto::Mount::Agent(onlyne_proto::AgentMount {
        role: "planner".to_string(),
        ..Default::default()
    });
    assert!(mount_allowed(Some(&agent), MountKind::Agent, "planner"));
    assert!(!mount_allowed(Some(&agent), MountKind::Agent, "reviewer"));
    assert!(mount_allowed(None, MountKind::Admin, "planner"));
    assert!(!mount_allowed(None, MountKind::Agent, "planner"));
}
#[test]
fn terminated_register_requests_bye() {
    assert!(should_bye_on_register("terminated"));
    assert!(!should_bye_on_register("live-session"));
}
