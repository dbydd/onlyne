//! Ending a session's resources: retirement on detach, the stall clock of an ended
//! connection, and the periodic reclaim.

use crate::common::{
    Published, ReasonBackend, RecordingOutbox, assert_settled, complete_plugin, deliver,
    eventually, mount_plugin, published_projection, run_a_turn, sample_envelope, serve_role_socket,
    task_delivery,
};
use onlyne_client::session::dispatch::{DispatchState, dispatch, on_plugin_report};
use onlyne_proto::{
    AdapterMsg, AgentPhase, DetachArgs, Lifecycle, Outcome, PluginOp, Report, ResourcePhase,
};
use onlyne_session::SessionLedger;
use onlyne_session::backend::fake::FakeBackend;
use onlyne_store::ClientStore;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::tempdir;

/// The projection publishes this client has sent, once that many have left or the
/// wait is spent.
///
/// A goodbye runs on the plugin's own task, so its frame can land a moment after
/// the retirement this test just watched. The wait hands back whatever the queue
/// holds either way, so a frame that never comes fails on the assertion below
/// with the whole list in its message.
async fn published_within(outbox: &RecordingOutbox, want: usize) -> Vec<Published> {
    for _ in 0..400 {
        let frames = outbox.projection_publishes().await;
        if frames.len() >= want {
            return frames;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    outbox.projection_publishes().await
}

/// The last frame this client published for one session.
fn last_publish<'a>(frames: &'a [Published], task_id: &str) -> &'a Published {
    frames
        .iter()
        .rev()
        .find(|frame| frame.task_id == task_id)
        .expect("the session's own last publish")
}

/// A completed session's goodbye publishes the row its retirement wrote.
///
/// This is the census shape: the plugin completes its work over a connection
/// that is still up, which is where the settle turn publishes, and says goodbye
/// when it is done. That turn's publish is true while its own connection serves
/// the session — the row reads `exited` beside an agent still `running` and a
/// resource still `attached` — and the goodbye then closes the resource and
/// feeds the agent's exit, moving the row one version ahead of everything the
/// server was told. The exit that retirement wrote is what has to travel, and it
/// travels as the report an ordinary ending travels: this client's own frame,
/// read here off the queue and never off the stored row.
///
/// A peer's census of completed sessions read `exited` beside `running` and
/// `attached` on every one of them, with the client's own row one version ahead
/// of the mirror: the goodbye's writes, the only writes this client has of the
/// agent's end, reaching nobody.
#[tokio::test]
async fn a_completed_sessions_goodbye_publishes_the_row_its_retirement_wrote() {
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
    let outbox = Arc::new(RecordingOutbox::default());
    state.attach_outbox(outbox.clone());
    let (socket, host) = serve_role_socket(&state, dir.path()).await;

    let task_id = deliver(&state, &task_delivery("task A")).await;
    let (io, mut assigns) = mount_plugin(&socket, Some(&task_id)).await;
    assert_eq!(assigns.recv().await.as_deref(), Some(task_id.as_str()));
    complete_plugin(&io, &task_id, Outcome::Done).await;
    let before = outbox.projection_publishes().await;
    let settled = last_publish(&before, &task_id);
    assert_eq!(
        (
            settled.projection.lifecycle,
            settled.projection.agent,
            settled.projection.resource
        ),
        (
            Lifecycle::Exited,
            AgentPhase::Running,
            ResourcePhase::Attached
        ),
        "the settle turn publishes the row while its own connection serves the session: \
         {settled:?}"
    );

    io.notify(AdapterMsg::Plugin(PluginOp::Detach(DetachArgs {
        reason: "session complete".into(),
    })))
    .await
    .unwrap();
    eventually(
        || state.session_count() == 0,
        "the detached session to leave the map",
    )
    .await;
    let frames = published_within(&outbox, before.len() + 1).await;

    let published = last_publish(&frames, &task_id);
    assert_eq!(
        published.projection.lifecycle,
        Lifecycle::Exited,
        "the exit the retirement wrote: {published:?}"
    );
    assert_eq!(
        published.projection.agent,
        AgentPhase::Gone,
        "the goodbye fed the agent's exit, and the frame says so: {published:?}"
    );
    assert_eq!(
        published.projection.resource,
        ResourcePhase::Closed,
        "and the retirement closed the resource: {published:?}"
    );
    assert_eq!(
        published.projection.outcome,
        Some(Outcome::Done),
        "beside the outcome the task settled: {published:?}"
    );
    let row = store
        .get_session(&task_id)
        .unwrap()
        .expect("the session keeps its row");
    assert_eq!(
        published.seq,
        row.seq.max(0) as u64,
        "the frame carries the version the retirement left behind, so the mirror and the \
         client's own row read one version apart no more"
    );
    host.abort();
}

/// A replayed delivery for a settled task returns the one slot it took.
///
/// Delivery is at-least-once, so a row can arrive after this client has already
/// filed the task's first verdict. The replay receives a session and a turn. Its
/// completion reaches the same settlement door, where the standing verdict makes
/// the second answer a refusal. The refusal owns the session release and the
/// client-row publish. The first settlement keeps the task account.
#[tokio::test]
async fn a_replayed_settled_task_returns_the_one_slot_it_took() {
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
    let outbox = Arc::new(RecordingOutbox::default());
    state.attach_outbox(outbox.clone());

    let first_delivery = task_delivery("task A");
    let task = deliver(&state, &first_delivery).await;
    run_a_turn(&state, &task).await;
    on_plugin_report(
        &state,
        None,
        Report::Complete {
            task_id: task.clone(),
            outcome: Outcome::Done,
            head: Some("first verdict".into()),
            reply_to: None,
            cluster_ref: None,
        },
    )
    .await
    .unwrap();

    assert_settled(&store, &task);
    let first_settled_at = store.task(&task).unwrap().unwrap().settled_at;
    assert_eq!(
        store.out_head(&task).unwrap().as_deref(),
        Some("first verdict")
    );
    assert_eq!(state.session_count(), 0);
    assert_eq!(backend.closed_sessions.lock().len(), 1);
    assert!(backend.inner.sessions().is_empty());

    outbox.clear().await;
    let mut replay = first_delivery.clone();
    replay.msg_id = "msg-replay".into();
    assert_eq!(deliver(&state, &replay).await, task);
    assert_eq!(state.session_count(), 1, "the replay takes the one slot");
    assert_eq!(backend.inner.sessions().len(), 1);

    run_a_turn(&state, &task).await;
    on_plugin_report(
        &state,
        None,
        Report::Complete {
            task_id: task.clone(),
            outcome: Outcome::Failed,
            head: Some("replayed verdict".into()),
            reply_to: None,
            cluster_ref: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(
        state.session_count(),
        0,
        "the refused replay returns the one slot"
    );
    assert!(
        backend.inner.sessions().is_empty(),
        "the replay resource closes"
    );
    assert_eq!(backend.closed_sessions.lock().len(), 2);
    assert_eq!(
        backend.reasons.lock().as_slice(),
        [
            onlyne_session::CloseReason::Completed,
            onlyne_session::CloseReason::Completed,
        ]
    );
    let record = store.task(&task).unwrap().unwrap();
    assert_eq!(record.task_state, onlyne_session::TaskState::Done);
    assert_eq!(record.settled_at, first_settled_at);
    assert_eq!(
        store.out_head(&task).unwrap().as_deref(),
        Some("first verdict")
    );

    let row = store.get_session(&task).unwrap().unwrap();
    assert_eq!(
        (row.agent_state.as_str(), row.resource_state.as_str()),
        ("gone", "closed")
    );
    let publishes = outbox.projection_publishes().await;
    let publish = publishes.last().expect("the refused path publishes");
    assert_eq!(publish.projection, published_projection(&store, &task));
    assert_eq!(publish.projection.lifecycle, Lifecycle::Exited);
    assert_eq!(publish.projection.agent, onlyne_proto::AgentPhase::Gone);
    assert_eq!(
        publish.projection.resource,
        onlyne_proto::ResourcePhase::Closed
    );
    assert_eq!(
        publish.projection.delivery,
        onlyne_proto::DeliveryPhase::Accepted
    );
    assert_eq!(
        publish.projection.recovery,
        onlyne_proto::RecoveryPhase::NoRecovery
    );
    assert_eq!(publish.projection.outcome, Some(Outcome::Done));
    assert_eq!(publish.generation, row.generation as u64);
    assert_eq!(publish.seq, row.seq.max(0) as u64);
}

/// The settle turn's own publish already carries the row its retirement left.
///
/// The release runs inside the dispatch lock and the turn's publish runs after
/// it, so the frame leaves already carrying the agent's exit and the closed
/// resource, at the version the row now holds. A second publish inside the turn
/// would repeat that frame verbatim, and this is what says so: one frame carries
/// the retired row, and it carries the stored row's own version. The deferred
/// retirement — the shape above, where the row moves after the turn has
/// published — is the case that needs a publish of its own.
#[tokio::test]
async fn the_settles_own_publish_already_carries_its_retirement() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["agent".into()],
        2,
        Arc::new(FakeBackend::new()),
        store.clone(),
    );
    let outbox = Arc::new(RecordingOutbox::default());
    state.attach_outbox(outbox.clone());

    let envelope = sample_envelope("planner", "task A");
    let task_id = envelope.task_id().unwrap().to_string();
    dispatch(&state, &envelope).unwrap();
    run_a_turn(&state, &task_id).await;
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
    .unwrap();

    let frames = outbox.projection_publishes().await;
    let retired: Vec<&Published> = frames
        .iter()
        .filter(|frame| {
            frame.projection.agent == AgentPhase::Gone
                && frame.projection.resource == ResourcePhase::Closed
        })
        .collect();
    assert_eq!(
        retired.len(),
        1,
        "the turn's own publish carries the retirement, and it is the one frame that does: \
         {frames:?}"
    );
    let published = retired[0];
    assert_eq!(
        published.projection.lifecycle,
        Lifecycle::Exited,
        "the retired session reads exited: {published:?}"
    );
    assert_eq!(
        published.projection.outcome,
        Some(Outcome::Done),
        "with the task's own outcome beside it: {published:?}"
    );
    let row = store
        .get_session(&task_id)
        .unwrap()
        .expect("the session keeps its row");
    assert_eq!(
        published.seq,
        row.seq.max(0) as u64,
        "and the frame carries the row the retirement wrote, so no write the turn made is \
         left unpublished"
    );
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
    run_a_turn(&state, &task_id).await;
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
    let row = store
        .get_session(&task_id)
        .unwrap()
        .expect("the session row");
    assert_eq!(
        row.agent_state, "gone",
        "the completed agent left with its resource"
    );
    assert_eq!(
        published_projection(&store, &task_id).lifecycle,
        Lifecycle::Exited
    );
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

    let reclaimed = state.reclaim_exited_resources();

    assert_eq!(
        reclaimed,
        vec![task_id.clone()],
        "the sweep answers with the session whose row it wrote"
    );
    assert_eq!(backend.closed_sessions.lock()[0].task_id, task_id);
    assert_eq!(
        backend.reasons.lock().as_slice(),
        [onlyne_session::CloseReason::Completed]
    );
    let row = store
        .get_session(&task_id)
        .unwrap()
        .expect("the session row");
    assert_eq!(row.agent_state, "gone", "the reclaimed agent is gone");
    assert_eq!(
        published_projection(&store, &task_id).lifecycle,
        Lifecycle::Exited
    );
    assert_eq!(state.session_count(), 0);
    host.abort();
}
