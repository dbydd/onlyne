//! Ending a session's resources: retirement on detach, a completed session whose agent
//! stays attached, the stall clock of an ended connection, and the periodic reclaim.

use crate::common::{
    ReasonBackend, RecordingOutbox, assert_settled, complete_plugin, complete_raw_plugin, deliver,
    eventually, mount_plugin, mount_raw_plugin, published_projection, sample_envelope,
    serve_role_socket, task_delivery,
};
use onlyne_client::session::dispatch::{DispatchState, dispatch, on_plugin_report};
use onlyne_proto::{AdapterMsg, DetachArgs, Lifecycle, Outcome, PluginOp, Report};
use onlyne_session::backend::fake::FakeBackend;
use onlyne_store::ClientStore;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::tempdir;

/// A settled session lives only as long as its agent is attached. A plugin
/// that detached — the shape a session process that exited itself leaves
/// behind, and the shape an operator `/onlyne disconnect` leaves — takes its
/// slot out of the map, so the next task spawns a new session instead of
/// writing into a dead connection.
#[tokio::test]
async fn a_settled_session_whose_plugin_left_is_retired() {
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
    state.attach_outbox(Arc::new(RecordingOutbox::default()));
    let (socket, host) = serve_role_socket(&state, dir.path()).await;

    let first_task = deliver(&state, &task_delivery("task A")).await;
    let (first_io, mut first_assigns) = mount_plugin(&socket, Some(&first_task)).await;
    tokio::time::timeout(Duration::from_secs(2), first_assigns.recv())
        .await
        .expect("the mounted session is handed its payload");

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
    assert_settled(&store, &first_task);
    assert_eq!(
        state.session_count(),
        1,
        "the settled slot stays while its agent is attached"
    );

    // The plugin leaves: the connection it served on is over.
    first_io
        .notify(AdapterMsg::Plugin(PluginOp::Detach(DetachArgs {
            reason: "plugin left".into(),
        })))
        .await
        .unwrap();
    eventually(|| state.session_count() == 0, "the idle slot to go").await;

    let second_task = deliver(&state, &task_delivery("task B")).await;
    assert_eq!(
        backend.sessions().len(),
        1,
        "the retired resource leaves one live replacement"
    );
    assert!(backend.sessions().contains_key(&second_task));
    let _second_io = mount_plugin(&socket, Some(&second_task)).await;
    host.abort();
}

#[tokio::test]
async fn automatic_retirement_survives_a_backend_close_failure() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(ReasonBackend::default());
    backend
        .fail_close
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["agent".into()],
        1,
        backend.clone(),
        store.clone(),
    );
    state.attach_outbox(Arc::new(RecordingOutbox::default()));

    let envelope = sample_envelope("planner", "task A");
    let task_id = envelope.task_id().unwrap().to_string();
    dispatch(&state, &envelope).unwrap();
    on_plugin_report(
        &state,
        None,
        Report::Complete {
            task_id: task_id.clone(),
            outcome: Outcome::Done,
            head: Some("done".into()),
            reply_to: None,
            cluster_ref: None,
        },
    )
    .await
    .expect("a retirement close failure stays local to cleanup");

    assert_eq!(backend.closed_sessions.lock().len(), 1);
    assert_eq!(state.session_count(), 0, "the unusable slot leaves routing");
    assert_eq!(
        published_projection(&store, &task_id).resource,
        onlyne_proto::ResourcePhase::Closed
    );
}

#[tokio::test]
async fn graceful_detach_retires_the_completed_session_resource() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(ReasonBackend::default());
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

    let task_id = deliver(&state, &task_delivery("task A")).await;
    let (io, mut assigns) = mount_plugin(&socket, Some(&task_id)).await;
    assert_eq!(assigns.recv().await.as_deref(), Some(task_id.as_str()));
    complete_plugin(&io, &task_id, Outcome::Done).await;
    assert!(backend.closed_sessions.lock().is_empty());

    io.notify(AdapterMsg::Plugin(PluginOp::Detach(DetachArgs {
        reason: "session complete".into(),
    })))
    .await
    .unwrap();
    eventually(
        || backend.closed_sessions.lock().len() == 1,
        "the detached session resource to close",
    )
    .await;

    let closed = backend.closed_sessions.lock();
    assert_eq!(closed[0].task_id, task_id);
    assert_eq!(closed[0].backend_ref["refreshed"], true);
    assert_eq!(
        backend.reasons.lock().as_slice(),
        [onlyne_session::CloseReason::Completed]
    );
    assert_eq!(state.session_count(), 0);
    host.abort();
}

#[tokio::test]
async fn completion_keeps_the_resource_while_the_plugin_is_attached() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(ReasonBackend::default());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["agent".into()],
        1,
        backend.clone(),
        store,
    );
    state.attach_outbox(Arc::new(RecordingOutbox::default()));
    let (socket, host) = serve_role_socket(&state, dir.path()).await;

    let task_id = deliver(&state, &task_delivery("task A")).await;
    let (io, mut assigns) = mount_plugin(&socket, Some(&task_id)).await;
    assert_eq!(assigns.recv().await.as_deref(), Some(task_id.as_str()));
    complete_plugin(&io, &task_id, Outcome::Done).await;

    assert!(backend.closed_sessions.lock().is_empty());
    assert_eq!(
        state.session_count(),
        1,
        "the attached resource stays tracked"
    );
    assert!(state.session_transport(&task_id).is_some());
    host.abort();
}

#[tokio::test]
async fn connection_loss_without_detach_keeps_the_completed_resource() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(ReasonBackend::default());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["agent".into()],
        1,
        backend.clone(),
        store,
    );
    state.attach_outbox(Arc::new(RecordingOutbox::default()));
    let (socket, host) = serve_role_socket(&state, dir.path()).await;

    let task_id = deliver(&state, &task_delivery("task A")).await;
    let mut stream = mount_raw_plugin(&socket, &task_id).await;
    complete_raw_plugin(&mut stream, &task_id, Outcome::Done).await;
    drop(stream);
    eventually(
        || state.session_transport(&task_id).is_none(),
        "the dropped connection binding to clear",
    )
    .await;

    assert!(backend.closed_sessions.lock().is_empty());
    assert_eq!(
        state.session_count(),
        1,
        "the reconnectable resource stays tracked"
    );
    host.abort();
}

#[tokio::test]
async fn ended_connection_forgets_its_inflight_stall_clock() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(ReasonBackend::default());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["agent".into()],
        1,
        backend.clone(),
        store,
    );
    state.attach_outbox(Arc::new(RecordingOutbox::default()));
    let (socket, host) = serve_role_socket(&state, dir.path()).await;

    let task_id = deliver(&state, &task_delivery("task A")).await;
    let (io, mut assigns) = mount_plugin(&socket, Some(&task_id)).await;
    assert_eq!(assigns.recv().await.as_deref(), Some(task_id.as_str()));
    io.notify(AdapterMsg::Plugin(PluginOp::SessionRegister(
        onlyne_proto::SessionRegisterArgs {
            session_id: "terminated".into(),
            pid: None,
            generation: 1,
            title: None,
            task_id: Some(task_id.clone()),
        },
    )))
    .await
    .unwrap();
    eventually(
        || state.session_transport(&task_id).is_none(),
        "the ended connection binding to clear",
    )
    .await;

    assert!(
        state
            .stall_due(Instant::now() + Duration::from_secs(3600), 1)
            .is_empty(),
        "the ended transport leaves no future stall report"
    );
    assert_eq!(state.session_count(), 1, "the in-flight slot stays tracked");
    assert!(backend.closed_sessions.lock().is_empty());
    host.abort();
}

#[tokio::test]
async fn periodic_reclaim_closes_an_exited_session_after_connection_loss() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(ReasonBackend::default());
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

    let task_id = deliver(&state, &task_delivery("task A")).await;
    let (io, mut assigns) = mount_plugin(&socket, Some(&task_id)).await;
    assert_eq!(assigns.recv().await.as_deref(), Some(task_id.as_str()));
    complete_plugin(&io, &task_id, Outcome::Done).await;
    assert_eq!(
        published_projection(&store, &task_id).lifecycle,
        Lifecycle::Exited
    );
    io.notify(AdapterMsg::Plugin(PluginOp::SessionRegister(
        onlyne_proto::SessionRegisterArgs {
            session_id: "terminated".into(),
            pid: None,
            generation: 1,
            title: None,
            task_id: Some(task_id.clone()),
        },
    )))
    .await
    .unwrap();
    eventually(
        || state.session_transport(&task_id).is_none(),
        "the ended connection binding to clear",
    )
    .await;
    assert!(backend.closed_sessions.lock().is_empty());

    state.reclaim_exited_resources();

    assert_eq!(backend.closed_sessions.lock()[0].task_id, task_id);
    assert_eq!(
        backend.reasons.lock().as_slice(),
        [onlyne_session::CloseReason::Completed]
    );
    assert_eq!(state.session_count(), 0);
    host.abort();
}
