use super::run;
use crate::runtime::runloop::config::ClientInit;
use onlyne_layout::RoleWorkspace;
use std::time::Duration;
use tempfile::tempdir;

/// A workspace socket that cannot be bound ends the run with an error.
///
/// The silent 0.5s restart this replaces kept a process alive that held a
/// server link while the local surface stayed shut, so `onlyne` verbs from the
/// workspace failed and the server still counted the role connected. The
/// message is the bind context naming the canonical spelling, and the cause
/// carries the served path with each length.
#[tokio::test]
async fn an_unbindable_socket_ends_the_run_with_an_error() {
    let dir = tempdir().unwrap();
    let workspace = RoleWorkspace::resolve(dir.path());
    workspace.bootstrap().unwrap();
    #[cfg(unix)]
    std::fs::write(
        workspace.run_dir().join("socket"),
        "/nonexistent-dir-onlyne-for-this-test/sock\n",
    )
    .unwrap();
    // Windows resolves the natural path regardless of the Unix endpoint
    // marker. Hold the production NPFS listener instead: a second bind to
    // that live name is the platform's EADDRINUSE equivalent.
    #[cfg(windows)]
    let (_held_listener, _endpoint) =
        onlyne_layout::bind_socket(workspace.root(), &workspace.run_dir()).unwrap();
    let init = ClientInit::new(
        dir.path(),
        "planner",
        "127.0.0.1:1",
        workspace.key_path(),
        "sha256/0000000000000000000000000000000000000000000000000000000000000000",
    )
    .with_backend("fake");
    let outcome = tokio::time::timeout(Duration::from_secs(10), run(init))
        .await
        .expect("the bind failure ends the run well inside the timeout");
    let error = outcome.expect_err("an unbindable socket is an error");
    assert!(
        error.to_string().contains("bind the workspace socket"),
        "{error}"
    );
}
