use super::*;
use crate::session::dispatch::transport::NUDGE_TEXT;
use crate::session::dispatch::turn_end::{DELIVERY_BLOCKED, TURN_END_WITHOUT_COMPLETE};
use onlyne_proto::new_task_id;
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
        Arc::new(crate::backend::fake::FakeBackend::new()),
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
            keeps_idle: false,
            family: None,
            idle_since: None,
            suspended: false,
            opened_at: Instant::now(),
            command: Vec::new(),
            resume_handle: None,
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
            tools_token: String::new(),
            delivered_roles: BTreeSet::new(),
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

/// A session that serves a second delivery keeps its process, so its reporter's
/// sequence still counts from the first delivery while this client's own feeds
/// (bind, ready, settle) have moved the row's watermark past it. The beat of the
/// new turn is a new fact and has to reach the row, because the settle door
/// reads the row to learn that a turn ran.
#[tokio::test]
async fn a_beat_below_the_rows_watermark_still_records_a_new_turn() {
    let dir = tempdir().expect("tempdir");
    let task = new_task_id();
    let state = staged_state(&dir, &task);
    seeded_ready(&state, &task);
    {
        let inner = state.inner.lock();
        let row = inner
            .store
            .get_session(&task)
            .expect("read the row")
            .expect("the seeded row");
        inner
            .store
            .bump_session_version(
                &task,
                row.generation.max(0) as u64,
                row.seq.max(0) as u64 + 2000,
            )
            .expect("the client's own feeds moved the watermark");
    }

    beat(&state, &task, "running", 1001).await;

    let inner = state.inner.lock();
    let row = inner.store.get_session(&task).expect("read the row");
    let observed = stored_observation(&inner.store, row.as_ref());
    assert_eq!(
        observed.agent,
        AgentPhase::Running,
        "a new beat whose sequence sits below the client's own watermark must still move the row"
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
/// §3c's rule at the client's own door: the beat that ends a turn over an open
/// task hands the plugin the one sentence — verbatim, naming its task — and the
/// second turn that ends the same way settles the delivery `blocked`.
///
/// The frame is the whole of what an ending sends, so a session that never
/// completes reads exactly one nudge and then a verdict, and the two steps are
/// published in that order (`docs/v2-CONTRACT.md` §3c).
#[tokio::test]
async fn a_turn_ending_over_an_open_task_nudges_once_and_then_settles() {
    let dir = tempdir().expect("tempdir");
    let task = new_task_id();
    let state = staged_state(&dir, &task);
    seeded_ready(&state, &task);
    opened_task(&state, &task);
    serving_slot(&state, &task, "msg-open");
    // The session's plugin: a real connection that declared `inject`, which is
    // what makes the sentence deliverable at all. The test's own handle is kept
    // for the length of the test, because the reader behind it ends with it.
    let (client_side, test_side) = tokio::io::duplex(4096);
    let serving = AdapterIo::new(client_side, Duration::from_secs(5), Duration::from_secs(5));
    let (_plugin, mut inbound) =
        AdapterIo::new_with_inbound(test_side, Duration::from_secs(5), Duration::from_secs(5));
    {
        let mut inner = state.inner.lock();
        inner
            .transports
            .insert(task.clone(), (serving, vec![Capability::Inject]));
    }
    // The rule's own publications are read where they are queued: the durable
    // outbound queue, on their way to the server that owns the fact.
    let queued = || -> Vec<ClientOp> {
        let inner = state.inner.lock();
        inner
            .store
            .due_intents(chrono::Utc::now(), 64)
            .expect("read the outbound queue")
            .iter()
            .map(crate::runtime::intent::op_for_intent)
            .collect::<Result<Vec<_>>>()
            .expect("decode the queued ops")
    };

    // The turn runs, then ends without a completion: the sentence leaves, once.
    beat(&state, &task, "running", 1005).await;
    beat(&state, &task, "idle", 1006).await;

    match inbound
        .recv()
        .await
        .expect("the ending hands its plugin the one sentence")
        .msg
    {
        AdapterMsg::Host(HostOp::Nudge {
            task_id: nudged,
            text,
        }) => {
            assert_eq!(nudged, task, "the sentence names the task it is about");
            assert_eq!(text, NUDGE_TEXT, "the sentence is 3c's, verbatim");
        }
        other => panic!("a nudge is what an ending sends: {other:?}"),
    }
    assert_eq!(
        verdict(&state, &task).0,
        TaskState::Pending,
        "a nudge is not a verdict: the delivery stays open"
    );

    // The agent works again and ends a second turn without a completion. The
    // nudge is spent, so this ending settles the delivery blocked.
    beat(&state, &task, "running", 1007).await;
    beat(&state, &task, "idle", 1008).await;
    assert_eq!(
        verdict(&state, &task).0,
        TaskState::Blocked,
        "the second turn that ends without a completion settles it blocked"
    );
    while let Ok(frame) = inbound.try_recv() {
        assert!(
            !matches!(frame.msg, AdapterMsg::Host(HostOp::Nudge { .. })),
            "one nudge per delivery, and no second try: {:?}",
            frame.msg
        );
    }

    // Each step of the rule is published, in order, on its way to the server.
    let rule: Vec<String> = queued()
        .iter()
        .filter_map(|op| match op {
            ClientOp::PublishEvent(args) => Some(args.class.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        rule,
        vec![
            TURN_END_WITHOUT_COMPLETE.to_string(),
            TURN_END_WITHOUT_COMPLETE.to_string(),
            DELIVERY_BLOCKED.to_string(),
        ],
        "every step of the rule is published, in order: {rule:?}"
    );
    // The nudge is a fact about a turn, not a verdict: the first publication
    // carries `nudge: true` and the second `nudge: false`, so a hook bound to
    // the class reads which ending spent the delivery's one sentence.
    let nudges: Vec<bool> = queued()
        .iter()
        .filter_map(|op| match op {
            ClientOp::PublishEvent(args) if args.class == TURN_END_WITHOUT_COMPLETE => args
                .payload
                .get("nudge")
                .and_then(serde_json::Value::as_bool),
            _ => None,
        })
        .collect();
    assert_eq!(nudges, vec![true, false], "one nudge, then the settlement");

    // The settlement names why. A hook bound to `delivery_blocked` is handed the
    // payload verbatim and gets nothing else to go on: the completion row this
    // settlement writes has an empty `out_head`, because a session that never
    // completed has no head, so the event is the only place the reason can be.
    let why: Vec<String> = queued()
        .iter()
        .filter_map(|op| match op {
            ClientOp::PublishEvent(args) if args.class == DELIVERY_BLOCKED => args
                .payload
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            _ => None,
        })
        .collect();
    assert_eq!(why.len(), 1, "the settlement publishes one reason");
    assert!(
        !why[0].is_empty(),
        "a settlement that names no reason leaves a hook with the fact and none of the cause"
    );
}
