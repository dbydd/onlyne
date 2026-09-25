//! The connection that serves a session: each task's own session and connection, a parked
//! agent claiming the session it serves, and a session handed its payload on mount.

use crate::common::{
    RecordingOutbox, assert_settled, deliver, eventually, mount_plugin, run_a_turn,
    serve_role_socket, task_delivery,
};
use onlyne_adapter::AdapterIo;
use onlyne_client::session::dispatch::{DispatchState, on_plugin_report};
use onlyne_proto::{
    AdapterMsg, DetachArgs, HelloArgs, Mount, MountKind, Outcome, PROTOCOL_VERSION, PluginOp,
    Report,
};
use onlyne_session::backend::fake::FakeBackend;
use onlyne_store::ClientStore;
use std::sync::Arc;
use std::time::Duration;
use tempfile::tempdir;

/// A second task gets a session and a connection of its own, spawned by the
/// client that is already running.
///
/// Before this case passed, a plugin's connection was remembered as the whole
/// role's transport (`DispatchInner::plugin_transport`), so the assignment of
/// the second task was written to the finished session's socket: the first
/// process answered it and no second session was ever spawned, which is the
/// live defect (task B's `onlyne-assign` entry inside task A's pi session
/// file, a single tab, two session rows).
///
/// The client itself is not part of a session's lifecycle: it stays up after
/// the session settles and serves the next task, which the admin hello at the
/// end asserts on the same socket.
#[tokio::test]
async fn the_second_task_gets_its_own_session_and_connection() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(FakeBackend::new());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["agent".into()],
        2,
        backend.clone(),
        store.clone(),
    );
    let outbox = Arc::new(RecordingOutbox::default());
    state.attach_outbox(outbox.clone());
    let (socket, host) = serve_role_socket(&state, dir.path()).await;

    // Task A: the session is staged, then its plugin mounts under that id.
    let first_task = deliver(&state, &task_delivery("task A")).await;
    // The connection stays open on purpose: the session is settled while its
    // process is still reachable, which is the state the live defect left.
    let (_first_io, mut first_assigns) = mount_plugin(&socket, Some(&first_task)).await;
    let assigned = tokio::time::timeout(Duration::from_secs(2), first_assigns.recv())
        .await
        .expect("the mounted session is handed its payload")
        .expect("the plugin connection is still open");
    assert_eq!(assigned, first_task);

    // It completes while its attached transport keeps the resource tracked,
    // and the next task is reserved for a session of its own.
    run_a_turn(&state, &first_task).await;
    on_plugin_report(
        &state,
        None,
        Report::Complete {
            task_id: first_task.clone(),
            outcome: Outcome::Done,
            head: Some("A done".into()),
            reply_to: None,
            cluster_ref: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        state.session_count(),
        1,
        "the attached settled resource stays tracked until its agent leaves"
    );
    assert_settled(&store, &first_task);
    assert!(
        !state.live_task_ids().contains(&first_task),
        "the client stops routing to a settled session"
    );

    // Task B: a second session is spawned, and its assignment rides its own
    // connection rather than the finished one.
    let second_task = deliver(&state, &task_delivery("task B")).await;
    assert_ne!(second_task, first_task);
    assert_eq!(
        backend.sessions().len(),
        2,
        "the second task spawns a second resource"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(300), first_assigns.recv())
            .await
            .is_err(),
        "the finished session's connection is never handed another task"
    );
    let (_second_io, mut second_assigns) = mount_plugin(&socket, Some(&second_task)).await;
    let assigned = tokio::time::timeout(Duration::from_secs(2), second_assigns.recv())
        .await
        .expect("the second session is handed its payload")
        .expect("the second plugin connection is still open");
    assert_eq!(
        assigned, second_task,
        "the second assign names the second task"
    );

    // The client is not reaped by a session ending: its socket still answers.
    let admin = HelloArgs {
        protocol: PROTOCOL_VERSION,
        plugin: "onlyne-client-cli:test".into(),
        version: "1.0.0".into(),
        kind: MountKind::Admin,
        capabilities: Vec::new(),
        mount: Some(Mount::Admin),
    };
    let stream = onlyne_layout::connect_local(&socket).await.unwrap();
    let io = AdapterIo::new(stream, Duration::from_secs(2), Duration::from_secs(2));
    let body = io
        .request(AdapterMsg::Plugin(PluginOp::Hello(admin)))
        .await
        .expect("the client socket answers after a session settled");
    assert!(
        body.ok,
        "the client keeps serving for the next task: {body:?}"
    );

    let publishes = outbox.projection_publishes().await;
    assert!(
        publishes.len() >= 2,
        "both sessions published a projection over the live link: {}",
        publishes.len()
    );
    host.abort();
}

/// An always-running agent's claim records the socket it arrived on.
///
/// The plugin that mounts naming no session is the only connection this role has
/// for the session staged next, so the claim that hands it the first task also
/// has to record the socket it arrived on. A claim that takes the connection and
/// binds nothing hands out one assignment and then forgets the path: the
/// session's payload reaches nobody and the plugin waits on `assign` forever.
/// The claim serves the one session it took, and a later task runs in a session
/// of its own with a plugin of its own mounted for it.
#[tokio::test]
async fn a_parked_agent_serves_the_session_it_claimed() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(FakeBackend::new());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["agent".into()],
        1,
        backend.clone(),
        store.clone(),
    );
    state.attach_outbox(Arc::new(RecordingOutbox::default()));
    let (socket, host) = serve_role_socket(&state, dir.path()).await;

    // The agent is up before any work exists, naming no session.
    let (io, mut assigns) = mount_plugin(&socket, None).await;

    let first_task = deliver(&state, &task_delivery("task A")).await;
    let assigned = tokio::time::timeout(Duration::from_secs(2), assigns.recv())
        .await
        .expect("the staged session reaches the waiting agent")
        .expect("the plugin connection is open");
    assert_eq!(assigned, first_task);

    run_a_turn(&state, &first_task).await;
    on_plugin_report(
        &state,
        None,
        Report::Complete {
            task_id: first_task.clone(),
            outcome: Outcome::Done,
            head: Some("A done".into()),
            reply_to: None,
            cluster_ref: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        state.session_count(),
        1,
        "the settled slot stays while its agent is attached"
    );

    // The agent leaves with the task it served, so its session retires.
    io.notify(AdapterMsg::Plugin(PluginOp::Detach(DetachArgs {
        reason: "task A done".into(),
    })))
    .await
    .unwrap();
    eventually(|| state.session_count() == 0, "the settled slot to go").await;

    // The next task runs in a session of its own, and its assignment never rides
    // the connection that served task A.
    let second_task = deliver(&state, &task_delivery("task B")).await;
    assert_ne!(second_task, first_task);
    assert_eq!(
        backend.sessions().len(),
        1,
        "the next task spawns a resource of its own"
    );
    assert!(backend.sessions().contains_key(&second_task));
    assert!(
        tokio::time::timeout(Duration::from_millis(200), assigns.recv())
            .await
            .is_err(),
        "the finished session's connection is handed nothing more"
    );
    let _second_io = mount_plugin(&socket, Some(&second_task)).await;
    host.abort();
}

/// A task that arrives ahead of its always-running agent waits for the mount.
///
/// Staging the session finds no connection, and the mount that parks is that
/// connection: the hand-off runs from the mount as well as from the delivery, so
/// the wait ends when the agent arrives.
#[tokio::test]
async fn a_session_staged_before_its_agent_mounts_is_handed_its_payload() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(FakeBackend::new());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["agent".into()],
        1,
        backend.clone(),
        store.clone(),
    );
    state.attach_outbox(Arc::new(RecordingOutbox::default()));
    let (socket, host) = serve_role_socket(&state, dir.path()).await;

    let task = deliver(&state, &task_delivery("task A")).await;
    assert!(
        state.hand_staged(&task).await.is_ok(),
        "a staged session with no connection is a wait, not an error"
    );
    assert!(
        state.staged_without_transport().is_some(),
        "the payload is held until the agent mounts"
    );

    let (_io, mut assigns) = mount_plugin(&socket, None).await;
    let assigned = tokio::time::timeout(Duration::from_secs(2), assigns.recv())
        .await
        .expect("the mount hands over the session it was waiting for")
        .expect("the plugin connection is open");
    assert_eq!(assigned, task);
    assert!(
        state.staged_without_transport().is_none(),
        "the hand-off consumed the wait"
    );
    host.abort();
}
