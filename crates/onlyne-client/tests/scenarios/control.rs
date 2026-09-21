//! The control plane: `cancel` reaching the session process, and `cancel`, `focus`, and
//! `recycle` for a task this role never held.

use crate::common::{
    ReasonBackend, RecordingOutbox, deliver, mount_plugin, sample_envelope, serve_role_socket,
    task_delivery,
};
use onlyne_client::session::dispatch::{DispatchState, dispatch, on_control};
use onlyne_proto::{ControlOp, new_task_id};
use onlyne_store::ClientStore;
use std::sync::Arc;
use std::time::Duration;
use tempfile::tempdir;

/// Whether one pid still names a process this test can signal.
#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// The control plane's reason to exist, in the shape the field reported it: an
/// operator settles a task, and the process doing the work goes with it. A
/// delivered `control` row used to reach no consumer at all, so the agent kept
/// writing the shared surface minutes after `repair close` reported `exited`.
#[cfg(unix)]
#[tokio::test]
async fn a_delivered_cancel_stops_the_session_process() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["sleep".into(), "120".into()],
        1,
        Arc::new(onlyne_session::backend::exec::ExecBackend::new()),
        store,
    );

    let envelope = sample_envelope("planner", "run the batch");
    let task_id = envelope.task_id().unwrap().to_string();
    let session = dispatch(&state, &envelope).unwrap();
    let pid = session.backend_ref["pid"]
        .as_u64()
        .expect("the exec reference carries its pid") as u32;
    assert!(pid_alive(pid), "the session process is running");

    let held = on_control(
        &state,
        &ControlOp::Cancel {
            task_id: task_id.clone(),
            reason: "operator close".into(),
        },
    )
    .await
    .expect("the command is applied");
    assert!(held, "the command names a session this client holds");

    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    while pid_alive(pid) && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!pid_alive(pid), "pid {pid} outlived its cancelled task");
    assert_eq!(state.session_count(), 0, "the slot came back");
}

/// A command for a task this role never held has nothing to act on. It settles
/// as delivered rather than staying in flight: an operator's `control` reporting
/// itself as undelivered forever is a worse answer than the true one.
#[tokio::test]
async fn a_cancel_for_an_unknown_task_creates_no_session() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["sleep".into(), "120".into()],
        1,
        Arc::new(onlyne_session::backend::exec::ExecBackend::new()),
        store,
    );

    let held = on_control(
        &state,
        &ControlOp::Recycle {
            task_id: new_task_id(),
            reason: "nothing to retire".into(),
        },
    )
    .await
    .expect("a command with no subject is applied, not refused");
    assert!(!held, "this role holds no such task");
    assert_eq!(state.session_count(), 0, "a command spawns no session");
}

/// A focus command for a task with no live session still settles. The control
/// plane answers the row instead of panicking or leaving it in flight.
#[tokio::test]
async fn a_focus_for_an_unknown_task_still_settles() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["sleep".into(), "120".into()],
        1,
        Arc::new(onlyne_session::backend::exec::ExecBackend::new()),
        store,
    );

    let held = on_control(
        &state,
        &ControlOp::Focus {
            task_id: new_task_id(),
        },
    )
    .await
    .expect("a focus with no live session is applied, not refused");
    assert!(!held, "this role holds no such task");
    assert_eq!(
        state.session_count(),
        0,
        "a focus command spawns no session"
    );
}

/// A slot that `control recycle` left behind is gone, and the next task spawns a
/// session of its own.
///
/// The recycle arm closes the backend resource and releases the task binding, so
/// the slot leaves the map even while its agent is still attached. A payload
/// staged onto that slot would have nothing behind it to run it: the spawn path
/// would stay unreachable, the server row would sit `in_flight`, and the role
/// would still look like it had room because an exited row spends no capacity.
#[tokio::test]
async fn a_recycled_slot_is_gone_and_the_next_task_spawns_its_own() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(ReasonBackend::default());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["agent".into()],
        2,
        backend.clone(),
        store.clone(),
    );
    state.attach_outbox(Arc::new(RecordingOutbox::default()));
    let (socket, host) = serve_role_socket(&state, dir.path()).await;

    let first_task = deliver(&state, &task_delivery("task A")).await;
    let (_io, mut assigns) = mount_plugin(&socket, Some(&first_task)).await;
    assert_eq!(assigns.recv().await.as_deref(), Some(first_task.as_str()));

    on_control(
        &state,
        &ControlOp::Recycle {
            task_id: first_task.clone(),
            reason: "operator recycle".into(),
        },
    )
    .await
    .expect("the command is applied");
    assert_eq!(
        backend.closed_sessions.lock().len(),
        1,
        "the recycled session's resource is closed"
    );
    assert_eq!(state.session_count(), 0, "the recycled slot leaves the map");

    let second_task = deliver(&state, &task_delivery("task B")).await;
    assert_eq!(
        state.session_count(),
        1,
        "the next task spawns a session of its own"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(300), assigns.recv())
            .await
            .is_err(),
        "the closed session is handed no assignment"
    );
    let (_second_io, mut second_assigns) = mount_plugin(&socket, Some(&second_task)).await;
    assert_eq!(
        second_assigns.recv().await.as_deref(),
        Some(second_task.as_str()),
        "the fresh session carries the task the recycled slot refused"
    );
    host.abort();
}
