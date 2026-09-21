use super::*;
use onlyne_proto::new_task_id;
use onlyne_session::{feed_turn_ended, feed_turn_started, is_legal};
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
