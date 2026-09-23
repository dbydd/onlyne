//! How many sessions a role runs: `max_sessions`, exited rows, redelivery, and the
//! ready barrier that orders an assign after readiness.

use crate::common::{
    ReasonBackend, RecordingOutbox, complete_plugin, deliver, mount_plugin, plugin_beat,
    published_projection, run_a_turn, sample_envelope, serve_role_socket, task_delivery,
};
use onlyne_adapter::AdapterIo;
use onlyne_client::session::dispatch::{
    DispatchState, ReadyNotice, dispatch, on_plugin_report, on_ready,
};
use onlyne_proto::{AdapterMsg, Capability, HostOp, Lifecycle, Outcome, Report, new_task_id};
use onlyne_session::backend::fake::FakeBackend;
use onlyne_store::ClientStore;
use std::sync::Arc;
use std::time::Duration;
use tempfile::tempdir;

/// Each task runs in a session of its own, and `max_sessions` caps the live
/// ones.
///
/// A second task gets a second session and a connection of its own rather than
/// riding the first task's agent. While both rows are live the cap refuses a
/// third, and the first task's row reaching `exited` gives that capacity back:
/// the next task is accepted and spawns its own session, while the settled slot
/// stays tracked for the agent still attached to it.
#[tokio::test]
async fn each_task_gets_its_own_session_and_max_sessions_caps_the_live_ones() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(ReasonBackend::default());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["echo".into()],
        2,
        backend.clone(),
        store.clone(),
    );
    state.attach_outbox(Arc::new(RecordingOutbox::default()));
    let (socket, host) = serve_role_socket(&state, dir.path()).await;

    let first_task = deliver(&state, &task_delivery("task 1")).await;
    let (first_io, mut first_assigns) = mount_plugin(&socket, Some(&first_task)).await;
    assert_eq!(
        first_assigns.recv().await.as_deref(),
        Some(first_task.as_str())
    );
    let second_task = deliver(&state, &task_delivery("task 2")).await;
    assert_ne!(second_task, first_task);
    let (second_io, mut second_assigns) = mount_plugin(&socket, Some(&second_task)).await;
    assert_eq!(
        second_assigns.recv().await.as_deref(),
        Some(second_task.as_str())
    );
    assert_eq!(state.session_count(), 2);
    assert_eq!(
        backend.inner.sessions().len(),
        2,
        "each task spawned a resource of its own"
    );

    // A third task meets the cap while both rows are live.
    let mut overflow = sample_envelope("planner", "task 3");
    overflow.causality = Some(onlyne_proto::Causality {
        task: new_task_id(),
        parent_task: None,
        reply_to: None,
        hop: 0,
        attempt: 0,
    });
    let err = dispatch(&state, &overflow).expect_err("the role is at its cap");
    assert_eq!(err.to_string(), "max_sessions reached");

    // Task 1 ends on its own connection. Its settled slot keeps the attached
    // resource and spends no capacity, so the next task is accepted.
    complete_plugin(&first_io, &first_task, Outcome::Done).await;
    assert!(
        backend.closed_sessions.lock().is_empty(),
        "the attached resource stays open"
    );
    let third_task = deliver(&state, &task_delivery("task 3")).await;
    assert_eq!(
        backend.inner.sessions().len(),
        3,
        "the accepted task spawns a resource of its own"
    );
    assert!(backend.inner.sessions().contains_key(&third_task));
    assert!(
        tokio::time::timeout(Duration::from_millis(200), first_assigns.recv())
            .await
            .is_err(),
        "the settled session's agent is handed nothing"
    );
    assert_eq!(
        state.session_count(),
        3,
        "the settled slot stays while its agent is attached"
    );
    drop(second_io);
    host.abort();
}

/// A settled session spends no concurrency.
///
/// §5's `max_sessions` caps the sessions a role has running, and the rows of
/// the sessions it has ended stay in `client.db` as the role's own history.
/// The live ring held six and seven exited rows per role against a cap of two,
/// and a role whose count reached the cap stops pulling: every later task parks
/// `in_flight` on the server with nothing on the client side saying why. Both
/// exit routes reach that state — the completion report, and the settled
/// observation the plugin sends as its last heartbeat — so neither holds
/// capacity, and the exited rows stay queryable.
#[tokio::test]
async fn exited_sessions_do_not_hold_the_capacity_cap() {
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

    // Task 1 ends through the completion report, which gives its slot back.
    let first = deliver(&state, &task_delivery("task 1")).await;
    run_a_turn(&state, &first).await;
    on_plugin_report(
        &state,
        None,
        Report::Complete {
            task_id: first.clone(),
            outcome: Outcome::Done,
            head: Some("done".into()),
            reply_to: None,
            cluster_ref: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        published_projection(&store, &first).lifecycle,
        Lifecycle::Exited,
        "a completion report settles the task and exits the session"
    );

    // Tasks 2 and 3 end with their agent proven dead, on the last heartbeat of a
    // session whose process is gone: the tuple itself projects exited while the
    // slot the client staged stays where it is. No task verdict is involved —
    // that is the other half of the split, and it is what keeps these two rows
    // from being read as one.
    for text in ["task 2", "task 3"] {
        let task_id = deliver(&state, &task_delivery(text)).await;
        let settled = onlyne_session::Observation::build(
            onlyne_session::Version::new(1, 3),
            true,
            onlyne_session::DEFAULT_ISOLATE_AFTER,
            onlyne_session::DEFAULT_TERMINATE_AFTER,
            0,
            onlyne_session::AgentState::Gone,
            onlyne_session::DeliveryState::Accepted,
            onlyne_session::ResourceState::Attached,
            onlyne_session::RecoveryState::None,
        );
        on_plugin_report(
            &state,
            None,
            plugin_beat(&task_id, 1, 3, serde_json::to_value(&settled).unwrap()),
        )
        .await
        .unwrap();
        assert_eq!(
            published_projection(&store, &task_id).lifecycle,
            Lifecycle::Exited,
            "{text} exits on its settled observation"
        );
        assert!(
            store
                .task(&task_id)
                .unwrap()
                .is_some_and(|record| record.task_state == onlyne_session::TaskState::Pending),
            "{text} opened its task but settled nothing: the beat proves the agent gone, not the work done"
        );
    }

    // The two slots whose settled observation arrived last are still staged,
    // which is the role at its cap with every one of them exited.
    assert_eq!(state.session_count(), 2);
    assert!(state.has_capacity());
    let fourth = deliver(&state, &task_delivery("task 4")).await;
    assert_ne!(fourth, first);
    assert_eq!(
        backend.sessions().len(),
        3,
        "the completion resource retired and each bound exited task kept its resource"
    );
    // The rows behind the cap spend nothing and stay readable.
    assert_eq!(
        published_projection(&store, &first).lifecycle,
        Lifecycle::Exited,
        "the settled task's row still answers exited after the cap moved on"
    );
}

#[test]
fn redelivered_task_keeps_its_one_session() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(FakeBackend::new());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["echo".into()],
        2,
        backend.clone(),
        store,
    );

    let envelope = sample_envelope("planner", "task 1");
    let first = dispatch(&state, &envelope).unwrap();
    let again = dispatch(&state, &envelope).unwrap();

    assert_eq!(first.task_id, again.task_id);
    assert_eq!(
        backend.sessions().len(),
        1,
        "a redelivery must not spawn a second resource"
    );
    assert_eq!(state.session_count(), 1);
}

#[tokio::test]
async fn ready_barrier_orders_assign_after_ready() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("client.db");
    let store = ClientStore::open(&db).unwrap();
    let backend = Arc::new(FakeBackend::new());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["echo".into()],
        2,
        backend,
        store,
    );

    let env = sample_envelope("planner", "task 1");
    let task_id = env.task_id().unwrap().to_string();
    let session = dispatch(&state, &env).unwrap();
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let (io_client, mut client_inbound) =
        AdapterIo::new_with_inbound(client_io, Duration::from_secs(2), Duration::from_secs(2));
    let (io_server, _server_inbound) =
        AdapterIo::new_with_inbound(server_io, Duration::from_secs(2), Duration::from_secs(2));

    let (record_tx, mut record_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        while let Some(frame) = client_inbound.recv().await {
            if let AdapterMsg::Host(HostOp::Assign(assign)) = frame.msg {
                let _ = record_tx.send(format!("assign:{}", assign.task_id));
            }
        }
    });

    on_ready(
        &state,
        ReadyNotice {
            task_id: task_id.clone(),
            session_id: session.task_id.clone(),
            generation: 1,
            io: Some(io_server),
            capabilities: vec![Capability::Inject],
        },
        "prose",
    )
    .await
    .unwrap();

    let recorded = tokio::time::timeout(Duration::from_secs(2), record_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recorded, format!("assign:{}", task_id));
    drop(io_client);
    assert!(record_rx.try_recv().is_err());
}
