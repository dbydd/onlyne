use super::*;
use crate::session::dispatch::SETTLE_WITHOUT_TURN;
use onlyne_proto::new_task_id;
use onlyne_session::{feed_resource_attached, feed_turn_ended, feed_turn_started, is_legal};
use serde_json::Value;
use tempfile::{TempDir, tempdir};

/// The observation shape the pi plugin sends: the three dimensions it can
/// witness, with `delivery` and `recovery` as placeholders.
fn plugin_beat(agent: &str, delivery: &str, recovery: &str) -> Value {
    serde_json::json!({
        "version": { "generation": 1, "seq": 1005 },
        "generation_live": true,
        "isolate_after": 1,
        "terminate_after": 3,
        "mismatch_count": 0,
        "agent": agent,
        "delivery": delivery,
        "resource": "attached",
        "recovery": recovery,
    })
}

/// A fresh dispatch state holding one session row, tracked live so the reducer
/// accepts events for it.
fn staged_state(dir: &TempDir, task: &str) -> DispatchState {
    let store = ClientStore::open(dir.path().join("client.db")).expect("client store");
    let state = DispatchState::new(
        "planner",
        dir.path(),
        Vec::new(),
        8,
        Arc::new(onlyne_session::backend::fake::FakeBackend::new()),
        store,
    );
    state.inner.lock().bridge.track_live(SessionRef {
        task_id: task.to_string(),
        backend: "fake".into(),
        backend_ref: Value::Null,
        generation: 1,
    });
    state
}

/// Take one client row through the dispatch seed and its ready barrier.
fn seeded_ready(state: &DispatchState, task: &str) {
    let inner = state.inner.lock();
    feed_created(&inner.bridge, &inner.store, task).expect("seed the session row");
    feed_ready(&inner.bridge, &inner.store, task).expect("ready");
}

/// The client's own tuple for one task, as its reducer wrote it.
fn client_tuple(state: &DispatchState, task: &str) -> Observation {
    let inner = state.inner.lock();
    let row = inner
        .store
        .get_session(task)
        .expect("read row")
        .expect("seeded row");
    stored_observation(&inner.store, Some(&row))
}

/// The client's records for a session whose completion intent is in flight: the
/// drain the completion report opened, and no receipt has landed.
fn draining_row(state: &DispatchState, task: &str) {
    seeded_ready(state, task);
    let inner = state.inner.lock();
    apply_persist(
        &inner.bridge,
        &inner.store,
        task,
        &LifecycleEvent::Complete {
            v: Version::new(1, 3),
        },
    )
    .expect("open the completion drain");
    drop(inner);
    let row = client_tuple(state, task);
    assert_eq!(row.agent, AgentState::Ready);
    assert_eq!(row.delivery, DeliveryState::Pending);
    assert_eq!(row.recovery, RecoveryState::None);
}

/// The client's records one turn after the drain opened: the agent ended its
/// turn, so the reducer's own labelling puts the session in `draining`.
fn waiting_row(state: &DispatchState, task: &str) {
    seeded_ready(state, task);
    {
        let inner = state.inner.lock();
        feed_turn_started(&inner.bridge, &inner.store, task).expect("a turn ran");
        feed_turn_ended(&inner.bridge, &inner.store, task).expect("the turn ended");
    }
    let inner = state.inner.lock();
    apply_persist(
        &inner.bridge,
        &inner.store,
        task,
        &LifecycleEvent::Complete {
            v: Version::new(1, 5),
        },
    )
    .expect("open the completion drain");
    drop(inner);
    let row = client_tuple(state, task);
    assert_eq!(row.agent, AgentState::Idle);
    assert_eq!(row.delivery, DeliveryState::Pending);
    assert_eq!(row.recovery, RecoveryState::Draining);
}

/// The client's records once the completion's receipt has landed: the settlement
/// closed the drain and the label beside it.
fn settled_row(state: &DispatchState, task: &str) {
    waiting_row(state, task);
    let inner = state.inner.lock();
    settle(&inner.bridge, &inner.store, task).expect("the receipt landed");
    drop(inner);
    let row = client_tuple(state, task);
    assert_eq!(row.delivery, DeliveryState::Accepted);
    assert_eq!(row.recovery, RecoveryState::None);
}

/// The row's recovery line moved to a fault, the way the reconcile loop moves it
/// at its isolation threshold.
fn faulted_row(state: &DispatchState, task: &str) {
    seeded_ready(state, task);
    {
        let inner = state.inner.lock();
        feed_turn_started(&inner.bridge, &inner.store, task).expect("a turn ran");
        feed_turn_ended(&inner.bridge, &inner.store, task).expect("the turn ended");
    }
    let inner = state.inner.lock();
    apply_persist(
        &inner.bridge,
        &inner.store,
        task,
        &LifecycleEvent::ReconcileMismatch {
            v: Version::new(1, 5),
        },
    )
    .expect("a reconcile fact disagreed");
    drop(inner);
    let row = client_tuple(state, task);
    assert_eq!(row.agent, AgentState::Idle);
    assert_eq!(row.recovery, RecoveryState::IdleFault);
}

/// Burn the open drain's retries beside an already-open fault line, the way the
/// intent machine does when the server keeps refusing.
fn exhausted_row(state: &DispatchState, task: &str) {
    faulted_row(state, task);
    let inner = state.inner.lock();
    apply_persist(
        &inner.bridge,
        &inner.store,
        task,
        &LifecycleEvent::IntentPending {
            v: Version::new(1, 6),
        },
    )
    .expect("the completion intent opened");
    apply_persist(
        &inner.bridge,
        &inner.store,
        task,
        &LifecycleEvent::IntentRetry {
            v: Version::new(1, 7),
        },
    )
    .expect("one retry");
    apply_persist(
        &inner.bridge,
        &inner.store,
        task,
        &LifecycleEvent::IntentExhausted {
            v: Version::new(1, 8),
        },
    )
    .expect("the retries are spent");
    drop(inner);
    let row = client_tuple(state, task);
    assert_eq!(row.delivery, DeliveryState::Exhausted);
    assert_eq!(row.recovery, RecoveryState::IdleFault);
}

/// The task state the composition's caller hands it: the task record's own
/// verdict, or `None` when this client holds no record for the task. Deliberately
/// not `stored_task_state`, which reads the same row but answers `pending` for a
/// task nobody opened — the projection's convention, not the composition's.
fn stored_task(state: &DispatchState, task: &str) -> Option<TaskState> {
    let inner = state.inner.lock();
    inner
        .store
        .task(task)
        .expect("read the task record")
        .map(|record| record.task_state)
}

/// Compose one plugin beat against the client's stored row.
fn compose(state: &DispatchState, task: &str, spelling: &str) -> Observation {
    let body: Observation =
        serde_json::from_value(plugin_beat(spelling, "none", "none")).expect("plugin tuple");
    compose_observation(&client_tuple(state, task), body, stored_task(state, task))
}

/// Drive one plugin beat through the client's own door, the way a serving
/// connection's frame arrives. No connection is the sender: the test composes
/// one the way the client composes its own reports.
async fn beat(state: &DispatchState, task: &str, spelling: &str, seq: u64) {
    on_plugin_report(
        state,
        None,
        Report::Heartbeat {
            task_id: task.to_string(),
            session_id: String::new(),
            generation: 1,
            seq,
            observed: plugin_beat(spelling, "none", "none"),
            projection: None,
            cluster_ref: None,
        },
    )
    .await
    .expect("the beat is handled");
}

/// The client's row for one task, as its columns hold it.
fn row_of(state: &DispatchState, task: &str) -> SessionRecord {
    let inner = state.inner.lock();
    inner
        .store
        .get_session(task)
        .expect("read row")
        .expect("seeded row")
}

/// The task record one settle would answer for: opened by this client, and still
/// owed its verdict.
fn opened_task(state: &DispatchState, task: &str) {
    let inner = state.inner.lock();
    inner
        .store
        .open_task(&Causality::root(task.to_string()), "root")
        .expect("open the task record");
}

/// One slot serving the task with its delivery handle and its sender. This is the
/// pair a settle spends: the ack the completed report owes the server, and the
/// receipt the task's sender reads.
fn serving_slot(state: &DispatchState, task: &str, msg_id: &str) {
    let mut inner = state.inner.lock();
    inner.sessions.insert(
        task.to_string(),
        SessionSlot {
            session: SessionRef {
                task_id: task.to_string(),
                backend: "fake".into(),
                backend_ref: Value::Null,
                generation: 1,
            },
            task_id: Some(task.to_string()),
            ready: true,
            payload: None,
            msg_id: Some(msg_id.to_string()),
            origin: Some(Principal::role("reviewer")),
            causality: Causality::root(task.to_string()),
            dropped_at: None,
            last_beat: None,
            read_only: false,
        },
    );
}

/// The verdict column one task carries here, with the head line beside it.
fn verdict(state: &DispatchState, task: &str) -> (TaskState, Option<String>) {
    let inner = state.inner.lock();
    let record = inner
        .store
        .task(task)
        .expect("read the task record")
        .expect("opened task record");
    (
        record.task_state,
        inner.store.out_head(task).expect("read the head column"),
    )
}

/// The frames the settle path owes the server and has queued: a delivery ack and a
/// completion receipt both live here until the link carries them, the ack as one of
/// this client's own ops and the receipt as the envelope the flusher sends.
fn queued_ops(state: &DispatchState) -> Vec<ClientOp> {
    let inner = state.inner.lock();
    inner
        .store
        .flush_order()
        .expect("read the intent queue")
        .iter()
        .map(|row| crate::runtime::intent::op_for_intent(row).expect("queued frame"))
        .collect()
}

/// The fault queue one task carries, as kind and reason pairs.
fn faults(state: &DispatchState, task: &str) -> Vec<(String, String)> {
    let inner = state.inner.lock();
    inner
        .store
        .list_faults(task)
        .expect("read the fault queue")
        .into_iter()
        .map(|fault| (fault.kind, fault.reason))
        .collect()
}

/// Drive one plugin's `complete` report through the door a serving connection's
/// frame arrives at, and assert the frame was answered. Which connection filed the
/// report is a separate judgement; the settle door reads the session's own row.
async fn complete_report(
    state: &DispatchState,
    task: &str,
    outcome: Outcome,
    head: Option<String>,
) {
    on_plugin_report(
        state,
        None,
        Report::Complete {
            task_id: task.to_string(),
            outcome,
            head,
            reply_to: None,
            cluster_ref: None,
        },
    )
    .await
    .expect("a refused settle is answered the way an applied one is");
}

/// The row of a session whose resource closed while its agent reported nothing:
/// the barrier passed, the attach landed, and this client then closed the resource,
/// which is the write that turns the agent dimension to `Gone`.
fn closed_row(state: &DispatchState, task: &str) {
    seeded_ready(state, task);
    let inner = state.inner.lock();
    feed_resource_attached(&inner.bridge, &inner.store, task).expect("attach the resource");
    feed_resource_closed(&inner.bridge, &inner.store, task).expect("close the resource");
    drop(inner);
    assert_eq!(
        client_tuple(state, task).agent,
        AgentState::Gone,
        "a closed resource kills the agent fact it hosted"
    );
}

/// An idle beat over an open task with no receipt is the design's
/// `idle_waiting` (§2.2, §4.2): the turn ended without a completion exit, and
/// the plugin answers by sending the assignment again.
///
/// Nothing else writes that label on a live session. The reducer's own turn-end
/// rule (`crates/onlyne-session/src/lifecycle/reduce.rs`, `transition`) needs a
/// `TurnEnded` event, and a plugin-backed session feeds none: its news is the
/// `agent` dimension of a beat. So the composition is where the rule has to
/// land, or a session that finished a turn owing a completion still reads as a
/// plain idle one — and the beat of the turn that follows has to carry the
/// label off again, which is the `working → idle_waiting → working` cycle the
/// design names.
#[tokio::test]
async fn an_idle_beat_over_an_open_task_is_composed_idle_waiting() {
    let dir = tempdir().expect("tempdir");
    let task = new_task_id();
    let state = staged_state(&dir, &task);
    // The two records the dispatch writes for delivered work, and the turn whose
    // end the beat reports.
    {
        let inner = state.inner.lock();
        inner
            .store
            .open_task(&Causality::root(task.as_str()), "root")
            .expect("open the task record");
    }
    seeded_ready(&state, &task);
    {
        let inner = state.inner.lock();
        feed_turn_started(&inner.bridge, &inner.store, &task).expect("a turn ran");
        feed_turn_ended(&inner.bridge, &inner.store, &task).expect("the turn ended");
    }
    let client = client_tuple(&state, &task);
    assert_eq!(
        client.agent,
        AgentState::Idle,
        "the turn that ran has ended"
    );
    assert_eq!(client.delivery, DeliveryState::None, "no exit was reported");
    assert_eq!(
        client.recovery,
        RecoveryState::None,
        "and no label is stored"
    );

    let idle = compose(&state, &task, "idle");
    assert!(is_legal(&idle), "{idle:?}");
    assert_eq!(
        idle.recovery,
        RecoveryState::IdleWaiting,
        "an idle agent over an open task with no receipt is waiting for its exit"
    );
    assert_eq!(
        idle.delivery,
        DeliveryState::None,
        "the label invents no intent: the plugin reported no completion"
    );

    // The same beat through the client's door lands the label in the row.
    beat(&state, &task, "idle", 1005).await;
    let row = row_of(&state, &task);
    assert_eq!(row.agent_state, "idle");
    assert_eq!(row.recovery_substate, "idle_waiting");

    // The next turn starts. A waiting label cannot ride a running agent —
    // `is_legal` couples it to `Idle` — so the composition has to carry it off
    // the tuple, or the reducer refuses the very beat that says work resumed and
    // the row keeps a wait nobody is in.
    beat(&state, &task, "running", 1006).await;
    let row = row_of(&state, &task);
    assert_eq!(row.agent_state, "running", "the turn's own news is applied");
    assert_eq!(row.recovery_substate, "none", "and the wait is over");

    // The label needs the open task. A session this client holds no record for
    // has no work to be waiting on, and `stored_task_state` reads that case as
    // `pending` for the projection's sake — so the composition takes the record
    // itself, and nothing is labelled for a task nobody opened.
    let task = new_task_id();
    let state = staged_state(&dir, &task);
    seeded_ready(&state, &task);
    {
        let inner = state.inner.lock();
        feed_turn_started(&inner.bridge, &inner.store, &task).expect("a turn ran");
        feed_turn_ended(&inner.bridge, &inner.store, &task).expect("the turn ended");
    }
    let idle = compose(&state, &task, "idle");
    assert!(is_legal(&idle), "{idle:?}");
    assert_eq!(
        idle.recovery,
        RecoveryState::None,
        "no task record means no open work to wait for: {idle:?}"
    );
}

/// A beat that disowns the drain must not clear it.
///
/// The plugin cannot see the completion intent: the client created it when the
/// completion was reported and the server has not receipted it. Before the
/// composition, the beat's `delivery: none` was the reducer's input verbatim, so
/// one arriving heartbeat rewrote the session's open line to "no intent" and the
/// drain was lost. Now that dimension is the client's own, and the row says so
/// whatever the plugin claimed beside it.
#[tokio::test]
async fn a_beat_disowning_the_drain_leaves_the_open_intent_in_place() {
    let dir = tempdir().expect("tempdir");
    let task = new_task_id();
    let state = staged_state(&dir, &task);
    draining_row(&state, &task);
    let before = {
        let inner = state.inner.lock();
        let row = inner
            .store
            .get_session(&task)
            .expect("read row")
            .expect("seeded row");
        assert_eq!(row.delivery_state, "pending", "the drain is open");
        Version::new(row.generation.max(0) as u64, row.seq.max(0) as u64)
    };

    on_plugin_report(
        &state,
        // No connection is this beat's sender: the test composes one the way the
        // client composes its own reports.
        None,
        Report::Heartbeat {
            task_id: task.clone(),
            session_id: String::new(),
            generation: 1,
            seq: 1005,
            observed: plugin_beat("running", "none", "none"),
            projection: None,
            cluster_ref: None,
        },
    )
    .await
    .expect("the beat is handled");

    let inner = state.inner.lock();
    let row = inner
        .store
        .get_session(&task)
        .expect("read row")
        .expect("seeded row");
    assert_eq!(
        row.delivery_state, "pending",
        "a plugin claiming no intent does not close the client's drain"
    );
    assert_eq!(row.agent_state, "running", "the agent is the plugin's");
    assert_eq!(row.resource_state, "attached", "and so is the resource");
    assert!(
        Version::new(row.generation.max(0) as u64, row.seq.max(0) as u64) > before,
        "the beat still advanced the watermark: {row:?}"
    );
}

/// An open drain survives every agent the plugin can report, and the composed
/// tuple is legal without asking the reducer to tolerate anything.
#[test]
fn a_beat_onto_an_open_drain_keeps_the_client_dimension_and_stays_legal() {
    let dir = tempdir().expect("tempdir");
    let task = new_task_id();
    let state = staged_state(&dir, &task);
    draining_row(&state, &task);

    let cases = [
        ("booting", AgentState::Booting),
        ("ready", AgentState::Ready),
        ("running", AgentState::Running),
        ("idle", AgentState::Idle),
        ("gone", AgentState::Gone),
    ];
    for (spelling, agent) in cases {
        let tuple = compose(&state, &task, spelling);
        assert!(is_legal(&tuple), "{spelling}: {tuple:?}");
        assert_eq!(tuple.agent, agent, "{spelling}: the agent is the plugin's");
        assert_eq!(
            tuple.resource,
            ResourceState::Attached,
            "{spelling}: the resource is the plugin's"
        );
        assert_eq!(
            tuple.delivery,
            DeliveryState::Pending,
            "{spelling}: an unacknowledged intent stays in flight whatever the \
             plugin says about the agent"
        );
        assert_eq!(
            tuple.recovery,
            RecoveryState::None,
            "{spelling}: the substate is the client's, and it had none to give"
        );
    }
}

/// A labelled recovery line rides onto the plugin's agent for the states that
/// can hold it, and is dropped for the states whose own definition forbids it.
#[test]
fn a_draining_label_follows_the_agent_that_can_hold_it() {
    let dir = tempdir().expect("tempdir");
    let task = new_task_id();
    let state = staged_state(&dir, &task);
    waiting_row(&state, &task);

    let idle = compose(&state, &task, "idle");
    assert!(is_legal(&idle), "{idle:?}");
    assert_eq!(
        idle.recovery,
        RecoveryState::Draining,
        "an idle agent keeps the label its drain carries"
    );
    let running = compose(&state, &task, "running");
    assert!(is_legal(&running), "{running:?}");
    assert_eq!(
        running.recovery,
        RecoveryState::Draining,
        "so does a running one: the completion is still in asynchronous send"
    );
    for spelling in ["ready", "booting", "gone"] {
        let tuple = compose(&state, &task, spelling);
        assert!(is_legal(&tuple), "{spelling}: {tuple:?}");
        assert_eq!(
            tuple.recovery,
            RecoveryState::None,
            "{spelling}: no agent of that shape carries a recovery substate"
        );
        assert_eq!(
            tuple.delivery,
            DeliveryState::Pending,
            "{spelling}: dropping the label does not close the intent"
        );
    }
}

/// A fault line is dropped by the same rule, and the `ready` pairing the reducer
/// forbids outright — an exhausted intent beside a ready agent — comes back as a
/// drain the reported turn can no longer be holding.
#[test]
fn a_fault_line_and_an_exhausted_intent_are_repaired_to_what_the_agent_can_hold() {
    let dir = tempdir().expect("tempdir");
    let task = new_task_id();
    let state = staged_state(&dir, &task);
    faulted_row(&state, &task);

    let idle = compose(&state, &task, "idle");
    assert!(is_legal(&idle), "{idle:?}");
    assert_eq!(
        idle.recovery,
        RecoveryState::IdleFault,
        "the fault line belongs to an idle agent, which is what the client holds"
    );
    let ready = compose(&state, &task, "ready");
    assert!(is_legal(&ready), "{ready:?}");
    assert_eq!(
        ready.recovery,
        RecoveryState::None,
        "a ready agent is not an idle one, so the reducer could not take the pair"
    );

    let task = new_task_id();
    let state = staged_state(&dir, &task);
    exhausted_row(&state, &task);
    let idle = compose(&state, &task, "idle");
    assert!(is_legal(&idle), "{idle:?}");
    assert_eq!(
        idle.delivery,
        DeliveryState::Exhausted,
        "an exhausted intent beside an idle agent is the fault path the client is on"
    );
    let ready = compose(&state, &task, "ready");
    assert!(is_legal(&ready), "{ready:?}");
    assert_eq!(
        ready.delivery,
        DeliveryState::None,
        "a ready agent has no turn exit for those retries to have burned against"
    );
}

/// A closed drain survives every agent too, and the one pairing the reducer
/// forbids — an accepted receipt on a process that has never passed `ready` —
/// comes back as the honest claim that the intent is still outstanding.
#[test]
fn a_beat_onto_a_settled_drain_repairs_only_the_pairing_the_reducer_forbids() {
    let dir = tempdir().expect("tempdir");
    let task = new_task_id();
    let state = staged_state(&dir, &task);
    settled_row(&state, &task);

    let cases = [
        ("booting", AgentState::Booting, DeliveryState::Pending),
        ("ready", AgentState::Ready, DeliveryState::Accepted),
        ("running", AgentState::Running, DeliveryState::Accepted),
        ("idle", AgentState::Idle, DeliveryState::Accepted),
        ("gone", AgentState::Gone, DeliveryState::Accepted),
    ];
    for (spelling, agent, delivery) in cases {
        let tuple = compose(&state, &task, spelling);
        assert!(is_legal(&tuple), "{spelling}: {tuple:?}");
        assert_eq!(tuple.agent, agent, "{spelling}: the agent is the plugin's");
        assert_eq!(
            tuple.delivery, delivery,
            "{spelling}: the drain is the client's"
        );
        assert_eq!(
            tuple.recovery,
            RecoveryState::None,
            "{spelling}: the settlement closed the substate"
        );
    }
}

/// The reconcile tuning and its counters are the client's, never the beat's.
///
/// A plugin witnesses neither the role's isolation thresholds nor the mismatch
/// counter, and the shape it sends carries the defaults beside a constant
/// `generation_live`. Taken verbatim, one heartbeat reset a ladder the client's
/// own loop had been climbing.
#[test]
fn a_beat_cannot_reset_the_reconcile_policy_or_its_counters() {
    let dir = tempdir().expect("temp dir");
    let task = new_task_id();
    let state = staged_state(&dir, &task);
    faulted_row(&state, &task);
    let client = client_tuple(&state, &task);
    assert_eq!(client.mismatch_count, 1, "the ladder is one rung up");

    let body: Observation = serde_json::from_value(serde_json::json!({
        "version": { "generation": 1, "seq": 1005 },
        "generation_live": false,
        "isolate_after": 9,
        "terminate_after": 9,
        "mismatch_count": 7,
        "agent": "idle",
        "delivery": "none",
        "resource": "attached",
        "recovery": "none",
    }))
    .expect("plugin tuple");
    let composed = compose_observation(
        &client_tuple(&state, &task),
        body,
        stored_task(&state, &task),
    );

    assert_eq!(composed.generation_live, client.generation_live);
    assert_eq!(composed.isolate_after, client.isolate_after);
    assert_eq!(composed.terminate_after, client.terminate_after);
    assert_eq!(composed.mismatch_count, client.mismatch_count);
    assert!(is_legal(&composed), "legal by construction: {composed:?}");
}

/// A beat that arrives on the connection the client holds read-only still buys
/// its liveness stamp, and buys no state. The stamp is what the reconnect
/// sweep's silence arm reads: both copies of a plugin loaded into one agent
/// process beat on their own connections, and the copy the client demoted used
/// to leave the session's clock starving, which retired an agent that was alive
/// and working.
#[tokio::test]
async fn a_beat_from_a_held_connection_refreshes_liveness_and_applies_no_state() {
    let dir = tempdir().expect("temp dir");
    let task = new_task_id();
    let state = staged_state(&dir, &task);
    seeded_ready(&state, &task);
    let (serving_stream, _serving_peer) = tokio::io::duplex(1024);
    let serving = AdapterIo::new(
        serving_stream,
        Duration::from_secs(5),
        Duration::from_secs(5),
    );
    let (held_stream, _held_peer) = tokio::io::duplex(1024);
    let held = AdapterIo::new(held_stream, Duration::from_secs(5), Duration::from_secs(5));
    {
        let mut inner = state.inner.lock();
        let session = SessionRef {
            task_id: task.clone(),
            backend: "fake".into(),
            backend_ref: serde_json::Value::Null,
            generation: 1,
        };
        // One session, served by one connection; the other connection is held
        // read-only, which is what a second mount of the same session lands in.
        inner
            .transports
            .insert(task.clone(), (serving.clone(), Vec::new()));
        inner.sessions.insert(
            task.clone(),
            SessionSlot {
                session,
                task_id: Some(task.clone()),
                ready: true,
                payload: None,
                msg_id: None,
                origin: None,
                causality: Causality::root(task.to_string()),
                dropped_at: None,
                last_beat: None,
                read_only: false,
            },
        );
        inner.revived.push((task.clone(), held.clone(), Vec::new()));
    }

    let beat = Report::Heartbeat {
        task_id: task.clone(),
        generation: 1,
        seq: 1006,
        observed: plugin_beat("running", "none", "none"),
        projection: None,
        session_id: task.clone(),
        cluster_ref: None,
    };
    on_plugin_report(&state, Some(&held), beat)
        .await
        .expect("a refused beat is still answered as an applied one");

    let inner = state.inner.lock();
    let stamp = inner
        .sessions
        .get(&task)
        .and_then(|slot| slot.last_beat)
        .expect("the liveness stamp is stamped even though the state is not");
    assert!(
        stamp.elapsed() < Duration::from_secs(5),
        "the stamp is this moment's, not the session's creation"
    );
    drop(inner);
    let tuple = client_tuple(&state, &task);
    assert_eq!(
        tuple.agent,
        AgentState::Ready,
        "the held connection's observation is not this session's state"
    );
}

/// A beat that changes no dimension still refreshes the session's liveness stamp.
/// This is the ordinary shape of a long turn: a model streaming for minutes reports
/// the same `agent: running` every ten seconds, so every one of those beats is a
/// no-op to the reducer, and the silence arm of the reconnect sweep reads nothing
/// but this stamp. A beat that leaves it unstamped starves the clock of a working
/// agent, and the sweep closes the pane under it.
#[tokio::test]
async fn a_no_op_beat_still_stamps_the_liveness_clock() {
    let dir = tempdir().expect("temp dir");
    let task = new_task_id();
    let state = staged_state(&dir, &task);
    seeded_ready(&state, &task);
    let (stream, _peer) = tokio::io::duplex(1024);
    let serving = AdapterIo::new(stream, Duration::from_secs(5), Duration::from_secs(5));
    {
        let mut inner = state.inner.lock();
        let session = SessionRef {
            task_id: task.clone(),
            backend: "fake".into(),
            backend_ref: serde_json::Value::Null,
            generation: 1,
        };
        inner
            .transports
            .insert(task.clone(), (serving.clone(), Vec::new()));
        inner.sessions.insert(
            task.clone(),
            SessionSlot {
                session,
                task_id: Some(task.clone()),
                ready: true,
                payload: None,
                msg_id: None,
                origin: None,
                causality: Causality::root(task.to_string()),
                dropped_at: None,
                last_beat: None,
                read_only: false,
            },
        );
    }
    let beat = |seq: u64| Report::Heartbeat {
        task_id: task.clone(),
        generation: 1,
        seq,
        observed: plugin_beat("running", "none", "none"),
        projection: None,
        session_id: task.clone(),
        cluster_ref: None,
    };

    on_plugin_report(&state, Some(&serving), beat(1006))
        .await
        .expect("the first beat lands");
    {
        let inner = state.inner.lock();
        assert!(
            inner.sessions[&task].last_beat.is_some(),
            "a beat the reducer applied stamps the clock"
        );
    }

    // The turn keeps running: the same observation, a newer sequence, so the
    // reducer has nothing to change. Rewind the clock the silence arm reads to
    // just past its threshold — three intervals of ten seconds — and let that
    // no-op beat answer it.
    {
        let mut inner = state.inner.lock();
        let slot = inner.sessions.get_mut(&task).expect("the slot");
        slot.last_beat =
            Some(Instant::now() - Duration::from_secs(31) - Duration::from_millis(200));
    }
    on_plugin_report(&state, Some(&serving), beat(1007))
        .await
        .expect("the no-op beat is answered like any other");

    let inner = state.inner.lock();
    let stamp = inner.sessions[&task]
        .last_beat
        .expect("the no-op beat stamped the liveness clock");
    assert!(
        stamp.elapsed() < Duration::from_secs(5),
        "a beat that changed nothing left a working agent's clock at {:?}",
        stamp.elapsed()
    );
    drop(inner);
    assert_eq!(
        client_tuple(&state, &task).agent,
        AgentState::Running,
        "the first beat's dimension is the one that stands"
    );
}

/// The field shape the settle door exists for: a plugin reports the ending of a
/// task whose agent never ran. The ready barrier passed and the assignment left for
/// this session, and no frame since has put its tuple through a turn, so the verdict
/// this report would write belongs to work nobody did. The frame is answered as an
/// applied one — a plugin treats a failed report as a link that died and sends the
/// same terminal fact again — and every write the settle owns stays unwritten: the
/// completion intent, the verdict, the head line, the delivery ack. The fault the
/// refusal files names the reading that refused, which is what an operator sorting
/// this task's row reads in `onlyne faults`.
#[tokio::test]
async fn a_completion_with_no_turn_behind_it_leaves_the_task_open() {
    let dir = tempdir().expect("temp dir");
    let task = new_task_id();
    let state = staged_state(&dir, &task);
    seeded_ready(&state, &task);
    opened_task(&state, &task);
    serving_slot(&state, &task, "msg-open-task");

    complete_report(&state, &task, Outcome::Done, None).await;

    let (task_state, head) = verdict(&state, &task);
    assert_eq!(
        task_state,
        TaskState::Pending,
        "a settle with no turn behind it writes no verdict"
    );
    assert_eq!(head, None, "the refusal writes no head line");
    assert!(
        !queued_ops(&state)
            .iter()
            .any(|op| matches!(op, ClientOp::Ack(ack) if ack.accepted)),
        "the delivery handle this session holds stays unspent: {:?}",
        queued_ops(&state)
    );
    assert_eq!(
        faults(&state, &task),
        vec![(
            SETTLE_WITHOUT_TURN.to_string(),
            "no turn ran: the agent phase this client holds for the session reads ready"
                .to_string(),
        )],
        "one fault names the reading that refused the frame"
    );
    let tuple = client_tuple(&state, &task);
    assert_eq!(
        tuple.agent,
        AgentState::Ready,
        "the tuple stays where the barrier left it"
    );
    assert_eq!(
        tuple.delivery,
        DeliveryState::None,
        "a completion for work that never ran opens no drain"
    );
}

/// A completion for a task this client holds no session row for carries no more
/// authority than one for a session that never left boot: there is nothing here to
/// have worked. The row is the record the door reads, and its absence is the answer.
#[tokio::test]
async fn a_completion_for_a_task_this_client_holds_no_row_for_leaves_no_verdict() {
    let dir = tempdir().expect("temp dir");
    let task = new_task_id();
    let state = staged_state(&dir, &task);
    opened_task(&state, &task);

    complete_report(
        &state,
        &task,
        Outcome::Done,
        Some("shaped like a result".into()),
    )
    .await;

    let (task_state, head) = verdict(&state, &task);
    assert_eq!(task_state, TaskState::Pending, "no verdict lands");
    assert_eq!(head, None, "no head line lands");
    let refused = faults(&state, &task);
    assert_eq!(refused.len(), 1, "the refusal files one fault: {refused:?}");
    assert_eq!(refused[0].0, SETTLE_WITHOUT_TURN);
    assert!(
        refused[0].1.contains("no session row"),
        "the fault names the absence it read: {:?}",
        refused[0].1
    );
}

/// The pi plugin's ordinary completion: the model called `onlyne_complete` inside a
/// turn, and the turn's own beat is in the row this client wrote. An empty head
/// travels with it — the tool call carried no text, and `onlyne complete
/// --head-from ledger` reads a row whose head column is blank — and the empty head
/// is a legitimate answer: the guard reads the session's turn, and a completion
/// after one settles with whatever line it brings.
#[tokio::test]
async fn a_completion_after_a_running_beat_settles_with_an_empty_head() {
    let dir = tempdir().expect("temp dir");
    let task = new_task_id();
    let state = staged_state(&dir, &task);
    seeded_ready(&state, &task);
    opened_task(&state, &task);
    serving_slot(&state, &task, "msg-worked");
    assert_eq!(
        client_tuple(&state, &task).agent,
        AgentState::Ready,
        "the barrier alone is the shape the refusal turns away"
    );

    beat(&state, &task, "running", 1005).await;
    complete_report(&state, &task, Outcome::Done, None).await;

    let (task_state, head) = verdict(&state, &task);
    assert_eq!(task_state, TaskState::Done, "the verdict lands");
    assert_eq!(
        head,
        Some(String::new()),
        "a completion with nothing to summarise still writes its head line"
    );
    assert!(
        faults(&state, &task).is_empty(),
        "a settle the turn authorises files no fault"
    );
    let queued = queued_ops(&state);
    assert!(
        queued.iter().any(
            |op| matches!(op, ClientOp::Ack(ack) if ack.msg_id == "msg-worked" && ack.accepted)
        ),
        "the settle spends the delivery handle it found: {queued:?}"
    );
    assert!(
        queued.iter().any(|op| matches!(
            op,
            ClientOp::Send(envelope) if envelope.kind == MsgKind::Completion
        )),
        "the task's sender is answered with its receipt: {queued:?}"
    );
}

/// The delivery answer is enqueued before the exit publish leaves.
///
/// A published `exited` runs the server's `release_exited_delivery`, and every
/// row of that task still in flight is one it hands back to the queue. The
/// completion's own delivery row is in flight until the ack is enqueued, and a
/// row the server re-offers is work dispatched a second time — what a live
/// `recycle` run did before `42fc685` fixed the control door's order. The
/// settle spends the handle inside the dispatch lock and publishes after the
/// relay and the receipt, so the queue reads the answer first and the exit
/// second, whichever of the two frames the link takes live.
#[tokio::test]
async fn the_settle_queues_the_delivery_answer_before_the_exit_publish() {
    let dir = tempdir().expect("temp dir");
    let task = new_task_id();
    let state = staged_state(&dir, &task);
    seeded_ready(&state, &task);
    opened_task(&state, &task);
    serving_slot(&state, &task, "msg-worked");
    beat(&state, &task, "running", 1005).await;

    complete_report(&state, &task, Outcome::Done, Some("done".into())).await;

    let queued = queued_ops(&state);
    let answer = queued
        .iter()
        .position(
            |op| matches!(op, ClientOp::Ack(ack) if ack.msg_id == "msg-worked" && ack.accepted),
        )
        .expect("the settle answers the delivery row it found");
    let publish = queued
        .iter()
        .position(|op| {
            matches!(
                op,
                ClientOp::Report(Report::Heartbeat {
                    task_id,
                    projection: Some(projection),
                    ..
                }) if task_id == &task && projection.lifecycle == Lifecycle::Exited
            )
        })
        .expect("the settle publishes the exit");
    assert!(
        answer < publish,
        "the delivery answer is enqueued before the exit publish: {queued:?}"
    );
}

/// The idle ladder's own exit: the agent ended its turn, the rungs were spent, and
/// the plugin files `failed` for the work it never completed. That report travels the
/// same door as a `done`, and the turn that ended is in the row — so the ladder's
/// verdict lands, with the head line naming how many rungs were walked.
#[tokio::test]
async fn the_idle_ladders_own_failure_settles_after_its_turn_ended() {
    let dir = tempdir().expect("temp dir");
    let task = new_task_id();
    let state = staged_state(&dir, &task);
    seeded_ready(&state, &task);
    opened_task(&state, &task);
    beat(&state, &task, "running", 1005).await;
    beat(&state, &task, "idle", 1006).await;

    complete_report(
        &state,
        &task,
        Outcome::Failed,
        Some("no completion after 3 idle reminders".into()),
    )
    .await;

    let (task_state, head) = verdict(&state, &task);
    assert_eq!(task_state, TaskState::Failed, "the ladder's verdict lands");
    assert_eq!(
        head.as_deref(),
        Some("no completion after 3 idle reminders"),
        "the head line names the rungs"
    );
    assert!(
        faults(&state, &task).is_empty(),
        "a turn that ended is a turn that ran"
    );
}

/// The recycle path: an operator's `control recycle` is this client's own command,
/// and the plugin's report answers it. The command's frame and the retirement it
/// triggers race over the row, so the authority is the note `on_control` wrote before
/// the frame left, and the verdict lands on a session that never ran a turn because
/// the command asked for that ending.
#[tokio::test]
async fn a_completion_that_answers_the_clients_own_recycle_settles_with_no_turn() {
    let dir = tempdir().expect("temp dir");
    let task = new_task_id();
    let state = staged_state(&dir, &task);
    seeded_ready(&state, &task);
    opened_task(&state, &task);
    serving_slot(&state, &task, "msg-recycled");

    let held = on_control(
        &state,
        &ControlOp::Recycle {
            task_id: task.clone(),
            reason: "workspace moved".into(),
        },
    )
    .await
    .expect("the command is applied");
    assert!(held, "the command named a task this client holds");
    complete_report(&state, &task, Outcome::Done, Some("recycled".into())).await;

    let (task_state, head) = verdict(&state, &task);
    assert_eq!(
        task_state,
        TaskState::Done,
        "the operator's own command answers for a settle with no turn behind it"
    );
    assert_eq!(head.as_deref(), Some("recycled"));
    assert!(
        faults(&state, &task).is_empty(),
        "a settle the client asked for files no fault"
    );
}

/// The two commands that note a word note the word they give.
///
/// The note is what this client settles a task by once no report answers the
/// command, and the word it carries is the whole of what that fallback writes: a
/// `cancel` stands for the `cancelled` it names, and a `recycle` — which asks the
/// plugin for its own ending and prescribes it nothing — stands for the `failed` a
/// session that died holding the task leaves behind.
#[tokio::test]
async fn a_control_command_notes_the_word_it_gives() {
    let cancelled_dir = tempdir().expect("temp dir");
    let cancelled = new_task_id();
    let state = staged_state(&cancelled_dir, &cancelled);
    seeded_ready(&state, &cancelled);
    serving_slot(&state, &cancelled, "msg-cancelled");
    on_control(
        &state,
        &ControlOp::Cancel {
            task_id: cancelled.clone(),
            reason: "operator cancel".into(),
        },
    )
    .await
    .expect("the command is applied");

    let recycled_dir = tempdir().expect("temp dir");
    let recycled = new_task_id();
    let second = staged_state(&recycled_dir, &recycled);
    seeded_ready(&second, &recycled);
    serving_slot(&second, &recycled, "msg-recycled");
    on_control(
        &second,
        &ControlOp::Recycle {
            task_id: recycled.clone(),
            reason: "workspace moved".into(),
        },
    )
    .await
    .expect("the command is applied");

    // A reading an hour on: the notes were written just now, and a sweep that ran
    // at once would be reading a command an operator had this instant given.
    let later = Instant::now() + Duration::from_secs(3600);
    let notes = state.control_settles_due(later);
    assert_eq!(notes.len(), 1, "one note per task: {notes:?}");
    assert_eq!(notes[0].task_id, cancelled);
    assert_eq!(
        notes[0].word,
        ControlWord::Cancel,
        "a cancel notes the word it is"
    );
    let notes = second.control_settles_due(later);
    assert_eq!(notes.len(), 1, "one note per task: {notes:?}");
    assert_eq!(notes[0].task_id, recycled);
    assert_eq!(
        notes[0].word,
        ControlWord::Recycle,
        "a recycle notes the word it is, which prescribes the plugin no outcome"
    );
}

/// A row the client closed is a row that ran nothing. `AgentGone` and
/// `ResourceClosed` both write `Gone`, and either one reaches it from a session that
/// never passed its ready barrier, so the death of an agent that never started reads
/// like the death of one that worked, and the phase carries no answer either way. A
/// plugin report landing on such a row is refused; the reconnect sweep's own settle is
/// the write that answers a session whose agent left, and the operator's `control`
/// command is the other.
#[tokio::test]
async fn a_death_with_no_report_owed_is_no_turn_behind_a_settle() {
    let dir = tempdir().expect("temp dir");
    let task = new_task_id();
    let state = staged_state(&dir, &task);
    closed_row(&state, &task);
    opened_task(&state, &task);

    complete_report(&state, &task, Outcome::Done, None).await;

    let (task_state, head) = verdict(&state, &task);
    assert_eq!(
        task_state,
        TaskState::Pending,
        "a closed row writes no verdict"
    );
    assert_eq!(head, None, "and no head line");
    let refused = faults(&state, &task);
    assert_eq!(refused.len(), 1, "one fault names the refusal: {refused:?}");
    assert!(
        refused[0].1.contains("gone"),
        "the fault names the phase it read: {:?}",
        refused[0].1
    );
}
