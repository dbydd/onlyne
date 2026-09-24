use super::{apply_role_info, scan_control_settles, scan_reclaimed_resources, scan_stalls};
use crate::runtime::intent::op_for_intent;
use crate::runtime::runloop::RunState;
use crate::runtime::runloop::test_support::{role_info, test_state};
use crate::session::dispatch::{
    CONTROL_SETTLE_BOUND, ControlWord, dispatch, on_plugin_report, on_recycled,
};
use anyhow::Result;
use onlyne_adapter::AdapterIo;
use onlyne_proto::{
    AgentPhase, Body, Capability, Causality, ClientOp, Lifecycle, MsgKind, Outcome, Principal,
    Report, ResourcePhase, SessionProjection, new_envelope, new_task_id,
};
use onlyne_session::{SessionLedger, TaskState, VersionedSession};
use std::time::{Duration, Instant};

#[test]
fn spec_reloaded_role_slice_change_updates_dispatch_gate() {
    let (state, _store) = test_state(1, vec!["old".into()]);
    let changed = apply_role_info(&state, &role_info(2, vec!["new".into()]));
    assert_eq!(changed, vec!["session_command", "max_sessions"]);
    let applied = state.dispatch.role_slice();
    assert_eq!(applied.max_sessions, 2);
    assert_eq!(applied.command, vec!["new"]);
}

#[test]
fn spec_reloaded_identical_role_slice_is_noop() {
    let (state, _store) = test_state(2, vec!["pi".into()]);
    let changed = apply_role_info(&state, &role_info(2, vec!["pi".into()]));
    assert!(changed.is_empty());
    assert_eq!(state.dispatch.role_slice().max_sessions, 2);
}

/// A reload that arms or disarms the guard has to reach a live connection,
/// which never sees a second `welcome`: the role row is the only carrier,
/// and the next spawn reads the policy off the dispatcher.
#[test]
fn a_relay_policy_from_the_role_row_is_adopted() {
    let (state, _store) = test_state(2, vec!["pi".into()]);
    let mut armed = role_info(2, vec!["pi".into()]);
    armed.relay_required = Some(vec!["writer".into()]);
    armed.relay_count = Some(2);
    let changed = apply_role_info(&state, &armed);
    assert_eq!(changed, vec!["relay_required", "relay_count"]);
    let applied = state.dispatch.role_slice();
    assert_eq!(applied.relay_required, vec!["writer".to_string()]);
    assert_eq!(applied.relay_count, Some(2));

    let disarmed = role_info(2, vec!["pi".into()]);
    let changed = apply_role_info(&state, &disarmed);
    assert_eq!(changed, vec!["relay_required", "relay_count"]);
    let applied = state.dispatch.role_slice();
    assert!(applied.relay_required.is_empty());
    assert_eq!(applied.relay_count, None);
}

#[tokio::test]
async fn stall_scan_queues_one_fault_until_applied_resets() {
    let (state, store) = test_state(1, Vec::new());
    let past = Instant::now()
        .checked_sub(Duration::from_secs(5))
        .expect("clock");
    state.dispatch.note_stall_assigned("task-frozen", past);
    scan_stalls(&state).await;
    let ops = store
        .flush_order()
        .expect("pending intents")
        .iter()
        .map(op_for_intent)
        .collect::<Result<Vec<_>>>()
        .unwrap();
    let stalled = ops
        .iter()
        .filter(|op| {
            matches!(
                op,
                ClientOp::Report(Report::Fault { task_id: Some(task), kind, .. })
                    if task == "task-frozen" && kind == crate::session::stall::STALLED
            )
        })
        .count();
    assert_eq!(stalled, 1, "one freeze episode reports once: {ops:?}");

    scan_stalls(&state).await;
    let ops = store
        .flush_order()
        .expect("pending intents")
        .iter()
        .map(op_for_intent)
        .collect::<Result<Vec<_>>>()
        .unwrap();
    let stalled = ops
        .iter()
        .filter(|op| {
            matches!(
                op,
                ClientOp::Report(Report::Fault { kind, .. }) if kind == crate::session::stall::STALLED
            )
        })
        .count();
    assert_eq!(stalled, 1, "the freeze is not re-reported: {ops:?}");

    state
        .dispatch
        .note_stall_applied("task-frozen", Instant::now());
    let past = Instant::now()
        .checked_sub(Duration::from_secs(5))
        .expect("clock");
    state.dispatch.note_stall_assigned("task-frozen", past);
    // Applied cleared the episode bit; an already-elapsed clock reports again.
    state.dispatch.note_stall_applied("task-frozen", past);
    scan_stalls(&state).await;
    let ops = store
        .flush_order()
        .expect("pending intents")
        .iter()
        .map(op_for_intent)
        .collect::<Result<Vec<_>>>()
        .unwrap();
    let stalled = ops
        .iter()
        .filter(|op| {
            matches!(
                op,
                ClientOp::Report(Report::Fault { kind, .. }) if kind == crate::session::stall::STALLED
            )
        })
        .count();
    assert_eq!(stalled, 2, "Applied starts a new freeze episode: {ops:?}");
}

#[tokio::test]
async fn exited_session_clock_is_forgotten_without_a_fault_frame() {
    let (state, store) = test_state(1, Vec::new());
    let task_id = "task-finished";
    // The tuple says the agent is gone, which is the session's own proof that
    // the session is over; no task verdict is needed for the clock to be
    // forgotten, and none is claimed by this row.
    store
        .upsert_session(
            task_id,
            &VersionedSession {
                agent_state: "gone".into(),
                delivery_state: "accepted".into(),
                resource_state: "attached".into(),
                recovery_substate: "none".into(),
                desired_json: "{}".into(),
                observed_json: serde_json::json!({"agent": "gone"}).to_string(),
                generation: 1,
                seq: 3,
                backend_ref: "{}".into(),
                mismatch_count: 0,
                updated_at: 0,
            },
        )
        .unwrap();
    let past = Instant::now()
        .checked_sub(Duration::from_secs(5))
        .expect("clock");
    state.dispatch.note_stall_assigned(task_id, past);

    scan_stalls(&state).await;

    assert!(
        store.flush_order().expect("pending intents").is_empty(),
        "an exited task emits no stalled fault"
    );
    state.dispatch.note_stall_applied(task_id, past);
    assert!(
        state.dispatch.stall_due(Instant::now(), 1).is_empty(),
        "the exited task leaves the progress clock"
    );
}

#[tokio::test]
async fn stall_scan_stays_quiet_when_disabled() {
    let (mut state, store) = test_state(1, Vec::new());
    state.stall_report_secs = 0;
    let past = Instant::now()
        .checked_sub(Duration::from_secs(5))
        .expect("clock");
    state.dispatch.note_stall_assigned("task-frozen", past);
    scan_stalls(&state).await;
    assert!(
        store.flush_order().expect("pending intents").is_empty(),
        "zero disables stall reports"
    );
}

/// One session staged for a task, holding the delivery row the server handed it.
///
/// The state an operator's `control` command leaves when nothing took the slot
/// with it: the client holds a live session row, and the slot still holds the
/// handle the completion that would have answered the command was going to spend.
fn staged_delivery(state: &RunState, task: &str, msg_id: &str) {
    dispatch(
        &state.dispatch,
        &new_envelope(
            MsgKind::Task,
            Principal::role("sender"),
            Principal::role("planner"),
            Body::text("repair the failing widget"),
            Some(Causality::root(task.to_string())),
        )
        .expect("task envelope"),
    )
    .expect("the delivery takes a session");
    state.dispatch.attach_msg_id(task, msg_id);
}

/// The frames this client owes the server, as the durable queue holds them.
fn queued_ops(state: &RunState) -> Vec<ClientOp> {
    state
        .store
        .flush_order()
        .expect("read the intent queue")
        .iter()
        .map(|row| op_for_intent(row).expect("queued frame"))
        .collect()
}

/// Every delivery ack one message id carries, as accepted and reason.
fn acks_for(state: &RunState, msg_id: &str) -> Vec<(bool, Option<String>)> {
    queued_ops(state)
        .into_iter()
        .filter_map(|op| match op {
            ClientOp::Ack(ack) if ack.msg_id == msg_id => Some((ack.accepted, ack.reason)),
            _ => None,
        })
        .collect()
}

/// The verdict one task's own record carries, with the head line beside it.
fn verdict(state: &RunState, task: &str) -> (TaskState, Option<String>) {
    let record = state
        .store
        .task(task)
        .expect("read the task record")
        .expect("the task has a record");
    (
        record.task_state,
        state.store.out_head(task).expect("read the head column"),
    )
}

/// The state this client last reported for one session, when the queue holds it.
fn published_projection(state: &RunState, task: &str) -> Option<SessionProjection> {
    queued_ops(state).into_iter().find_map(|op| match op {
        ClientOp::Report(Report::Heartbeat {
            task_id,
            projection,
            ..
        }) if task_id == task => projection,
        _ => None,
    })
}

/// Whether the queue holds this client's own report of one session's state.
fn published(state: &RunState, task: &str) -> bool {
    published_projection(state, task).is_some()
}

/// The last frame this client published for one session, as the queue holds it.
fn last_published(state: &RunState, task: &str) -> SessionProjection {
    queued_ops(state)
        .into_iter()
        .rev()
        .find_map(|op| match op {
            ClientOp::Report(Report::Heartbeat {
                task_id,
                projection: Some(projection),
                ..
            }) if task_id == task => Some(projection),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the session's own last publish"))
}

/// The version the last frame for one session carried, off the same queued frame.
fn last_published_seq(state: &RunState, task: &str) -> u64 {
    queued_ops(state)
        .into_iter()
        .rev()
        .find_map(|op| match op {
            ClientOp::Report(Report::Heartbeat {
                task_id,
                seq,
                projection: Some(_),
                ..
            }) if task_id == task => Some(seq),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the session's own last publish"))
}

/// One plugin beat reporting a running agent, in the tuple shape the reducer reads.
fn running_beat(task: &str) -> Report {
    let seq = 1005;
    Report::Heartbeat {
        task_id: task.to_string(),
        session_id: String::new(),
        generation: 1,
        seq,
        observed: serde_json::json!({
            "version": { "generation": 1, "seq": seq },
            "generation_live": true,
            "isolate_after": 1,
            "terminate_after": 3,
            "mismatch_count": 0,
            "agent": "running",
            "delivery": "none",
            "resource": "attached",
            "recovery": "none",
        }),
        projection: None,
        cluster_ref: None,
    }
}

/// One completed session whose plugin left without a goodbye, staged the way the
/// census found ten of them: the work is settled over a connection this client
/// still holds, and then that connection's socket ends.
///
/// The settle turn's release declines the retirement while the connection serves
/// the session — the agent is still reachable, and its resource stays open for
/// it — so the turn publishes the row as it reads then: `exited` beside an agent
/// still `running` and a resource still `attached`. A socket that ends without a
/// `detach` frame leaves that idle session and its open resource behind, and the
/// reclaim is the sweep that ends it.
async fn lingering_completed_session(state: &RunState, task: &str) {
    let (stream, _peer) = tokio::io::duplex(1024);
    let io = AdapterIo::new(stream, Duration::from_secs(5), Duration::from_secs(5));
    staged_delivery(state, task, "msg-lingering");
    state.dispatch.bind_adapter(
        task,
        io.clone(),
        vec![Capability::Report, Capability::Inject],
    );
    on_plugin_report(&state.dispatch, Some(&io), running_beat(task))
        .await
        .expect("the turn behind the completion is handled");
    on_plugin_report(
        &state.dispatch,
        Some(&io),
        Report::Complete {
            task_id: task.to_string(),
            outcome: Outcome::Done,
            head: Some("done".into()),
            reply_to: None,
            cluster_ref: None,
        },
    )
    .await
    .expect("the completion settles the task");
    // The socket ends, and no `detach` frame says the agent meant to go.
    state.dispatch.release_connection(Some(task), &io, false);
}

/// The reclaim of a lingering completed resource publishes the row it wrote.
///
/// The reclaim runs on every readiness tick, ahead of the reconnect window, so a
/// completed session whose plugin left is its work: the resource closes, the
/// agent goes, and both writes move the row the settle turn had already reported
/// one version earlier. The server mirrors only what this client reports, and
/// this client had reported that row as `exited` beside an agent still `running`
/// and a resource still `attached` — the reading a peer's census found on every
/// completed session, with the client's own row one version ahead of the mirror.
/// The frame that carries the retired row is what this reads, off the queue this
/// client sends on. Two of them linger at once, because the sweep answers with
/// every session it retired and one tick's answer reaches the server whole.
#[tokio::test]
async fn the_reclaim_publishes_the_exit_of_a_lingering_completed_resource() {
    let (state, store) = test_state(3, Vec::new());
    let tasks = [new_task_id(), new_task_id()];
    for task in &tasks {
        lingering_completed_session(&state, task).await;
        let settled = last_published(&state, task);
        assert_eq!(
            (settled.lifecycle, settled.agent, settled.resource),
            (
                Lifecycle::Exited,
                AgentPhase::Running,
                ResourcePhase::Attached
            ),
            "the settle turn published the row while the connection still served it: {settled:?}"
        );
    }
    assert_eq!(
        state.dispatch.session_count(),
        tasks.len(),
        "both idle slots linger with their resources open"
    );

    scan_reclaimed_resources(&state).await;

    assert_eq!(
        state.dispatch.session_count(),
        0,
        "the retired slot left the map"
    );
    for task in &tasks {
        let published = last_published(&state, task);
        assert_eq!(
            published.lifecycle,
            Lifecycle::Exited,
            "the exit the reclaim wrote: {published:?}"
        );
        assert_eq!(
            published.agent,
            AgentPhase::Gone,
            "the reclaim fed the agent's exit, and the frame says so: {published:?}"
        );
        assert_eq!(
            published.resource,
            ResourcePhase::Closed,
            "and the resource close beside it: {published:?}"
        );
        assert_eq!(
            published.outcome,
            Some(Outcome::Done),
            "with the outcome the task settled: {published:?}"
        );
        let row = store
            .get_session(task)
            .expect("read the row")
            .expect("the session keeps its row");
        assert_eq!(
            last_published_seq(&state, task),
            row.seq.max(0) as u64,
            "the frame carries the version the reclaim left behind, so the mirror and the \
             client's own row read one version apart no more"
        );
    }
}

/// A cancel no plugin ever answered settles the task on the operator's word.
///
/// The command asks the plugin for its own ending, and the report that would have
/// settled the task is a frame of the plugin's; a plugin that goes with the
/// command's frame never sends one. Three things are left unanswered — the task,
/// the delivery row this client was handed, and the mirrored row an operator is
/// watching — and nothing else in this process will answer any of them: the close
/// the command ran took the session's own row to its ending already, so the sweep
/// that retires a dead session finds no window open on this one.
///
/// This is the shape where the delivery row is still held: the close left the slot
/// standing, which is what a backend that refused the close leaves, or the task
/// was staged again while the word was still owed. The row is refused with the
/// operator's own word, since that is what the column has to read; the shape the
/// live run left, where the close took the handle with it, is the case below.
///
/// The bound runs from the instant the note carries, so a word given past it is
/// due without this test waiting.
#[tokio::test]
async fn an_unanswered_cancel_settles_the_task_and_refuses_its_delivery() {
    let (state, store) = test_state(2, Vec::new());
    let task = new_task_id();
    staged_delivery(&state, &task, "msg-cancelled");
    let given = Instant::now()
        .checked_sub(CONTROL_SETTLE_BOUND + Duration::from_secs(1))
        .expect("an instant past the bound");
    state
        .dispatch
        .owe_controlled_settle(&task, ControlWord::Cancel, given);

    scan_control_settles(&state).await;

    let (task_state, head) = verdict(&state, &task);
    assert_eq!(
        task_state,
        TaskState::Cancelled,
        "the operator's word stands with no report behind it"
    );
    assert_eq!(head, None, "and the sweep writes no head line of its own");
    assert_eq!(
        acks_for(&state, "msg-cancelled"),
        vec![(false, Some("operator cancel".to_string()))],
        "the delivery row this client still held is refused with the operator's own word: {:?}",
        queued_ops(&state)
    );
    assert!(
        published(&state, &task),
        "the settled session's exit is published: {:?}",
        queued_ops(&state)
    );
    assert!(
        store.open_tasks(10).expect("open tasks").is_empty(),
        "nothing is left open for the server to re-offer"
    );
}

/// A recycle no plugin ever answered settles the task `failed`.
///
/// A recycle prescribes the plugin no outcome: the plugin's own report is the
/// verdict when one comes, and `failed` is what stands when none does — the
/// resource and the slot are gone and the work cannot continue, which is the
/// verdict the reconnect sweep writes for a session that died holding a task. The
/// refusal carries the command's own word beside it, because `failed` alone would
/// not say which of the operator's words went unanswered.
#[tokio::test]
async fn an_unanswered_recycle_settles_the_task_failed() {
    let (state, store) = test_state(2, Vec::new());
    let task = new_task_id();
    staged_delivery(&state, &task, "msg-recycled");
    let given = Instant::now()
        .checked_sub(CONTROL_SETTLE_BOUND + Duration::from_secs(1))
        .expect("an instant past the bound");
    state
        .dispatch
        .owe_controlled_settle(&task, ControlWord::Recycle, given);

    scan_control_settles(&state).await;

    let (task_state, _) = verdict(&state, &task);
    assert_eq!(
        task_state,
        TaskState::Failed,
        "a recycle nothing answered leaves the failure its fallback names"
    );
    assert_eq!(
        acks_for(&state, "msg-recycled"),
        vec![(false, Some("operator recycle".to_string()))],
        "and the delivery row carries the command's own word: {:?}",
        queued_ops(&state)
    );
    assert!(
        published(&state, &task),
        "the settled session's exit is published: {:?}",
        queued_ops(&state)
    );
    assert!(
        store.open_tasks(10).expect("open tasks").is_empty(),
        "nothing is left open for the server to re-offer"
    );
}

/// A cancel the plugin answered settles once, through the report it sent.
///
/// The note is the record the sweep reads and it belongs to whoever spends it
/// first: a completion that answers the word travels the settle door, which takes
/// the note, and the verdict that stands is the one that report filed. A sweep of
/// its own would write `failed` over a `cancelled` the plugin itself reported —
/// and here the note is already past the bound while the report is on its way, so
/// nothing but the note's own consumption keeps the second verdict off the row.
#[tokio::test]
async fn a_cancel_answered_by_the_plugin_settles_through_the_report_alone() {
    let (state, _store) = test_state(2, Vec::new());
    let task = new_task_id();
    staged_delivery(&state, &task, "msg-cancelled");
    let given = Instant::now()
        .checked_sub(CONTROL_SETTLE_BOUND + Duration::from_secs(1))
        .expect("an instant past the bound");
    state
        .dispatch
        .owe_controlled_settle(&task, ControlWord::Cancel, given);

    // The plugin's own ending arrives before the sweep looks.
    on_plugin_report(
        &state.dispatch,
        None,
        Report::Complete {
            task_id: task.clone(),
            outcome: Outcome::Cancelled,
            head: Some("the operator's word".into()),
            reply_to: None,
            cluster_ref: None,
        },
    )
    .await
    .expect("a refused settle is answered the way an applied one is");
    let after_report = queued_ops(&state);

    scan_control_settles(&state).await;

    assert_eq!(
        verdict(&state, &task),
        (
            TaskState::Cancelled,
            Some("the operator's word".to_string())
        ),
        "the report's own verdict and head line stand"
    );
    assert_eq!(
        acks_for(&state, "msg-cancelled"),
        vec![(true, None)],
        "the completion spends the delivery row it answered: {:?}",
        queued_ops(&state)
    );
    assert_eq!(
        queued_ops(&state).len(),
        after_report.len(),
        "the sweep says nothing the report had not already said: {:?}",
        queued_ops(&state)
    );
}

/// A word given this instant is still the operator's own to have answered.
///
/// The bound is what makes the sweep a watchdog rather than a second settle path:
/// a plugin that is merely slow still gets to report the ending itself, and the
/// verdict it files is the one the task keeps. A sweep that read the note without
/// reading its instant passes both cases above and fails here.
#[tokio::test]
async fn a_cancel_inside_the_bound_settles_nothing() {
    let (state, store) = test_state(2, Vec::new());
    let task = new_task_id();
    staged_delivery(&state, &task, "msg-cancelled");
    state
        .dispatch
        .owe_controlled_settle(&task, ControlWord::Cancel, Instant::now());

    scan_control_settles(&state).await;

    assert_eq!(
        verdict(&state, &task),
        (TaskState::Pending, None),
        "the word the operator gave this instant is still open"
    );
    assert!(
        acks_for(&state, "msg-cancelled").is_empty(),
        "the delivery row is left for the report: {:?}",
        queued_ops(&state)
    );
    assert!(
        !published(&state, &task),
        "and nothing is published about a session still running: {:?}",
        queued_ops(&state)
    );
    assert_eq!(
        store.open_tasks(10).expect("open tasks").len(),
        1,
        "the task stays open for the plugin's own ending"
    );
}

/// The shape a control close now leaves: the close refuses the row it still
/// held, and the publish is what settles the task.
///
/// The close an operator's command runs ends the session's own row and takes the
/// slot with it, delivery handle and all, so the handle is the close's to spend
/// and it spends it before the slot leaves the map: the row is refused with the
/// operator's own word. What this client still holds is its record of the ending
/// — the closed session beside the verdict the word stands for — and publishing
/// it is the write that moves the mirror: a row whose session syncs `exited` is
/// the server's own signal, so the operator no longer has to reach for a
/// `repair` verb.
///
/// The refusal is what this expectation gained, and the shape it lost was the
/// live run's own: a close whose handle died with the slot refused nothing, and
/// an unanswered row is not a decision the server can read — the pull that would
/// have taken it passes, the release of a session the server judges gone hands it
/// back to the queue, and the task is dispatched again and run again for as long
/// as the cycle repeats. Exactly one ack is the whole contract: the close spends
/// the handle, and the sweep that settles the task below finds no handle left to
/// answer the same row.
#[tokio::test]
async fn a_cancel_whose_close_refused_the_delivery_settles_and_publishes_alone() {
    let (state, store) = test_state(2, Vec::new());
    let task = new_task_id();
    staged_delivery(&state, &task, "msg-cancelled");
    let given = Instant::now()
        .checked_sub(CONTROL_SETTLE_BOUND + Duration::from_secs(1))
        .expect("an instant past the bound");
    state
        .dispatch
        .owe_controlled_settle(&task, ControlWord::Cancel, given);
    // The close the command runs, which is where the delivery handle is spent.
    on_recycled(
        &state.dispatch,
        &task,
        onlyne_session::CloseReason::Cancelled,
    )
    .expect("the close runs");
    assert_eq!(state.dispatch.session_count(), 0, "the slot is gone");

    scan_control_settles(&state).await;

    assert_eq!(
        verdict(&state, &task).0,
        TaskState::Cancelled,
        "the task is settled where the live run left it open"
    );
    let published = published_projection(&state, &task).expect("the ending is published");
    assert_eq!(
        published.lifecycle,
        Lifecycle::Exited,
        "and it reads exited, which is the ending the server mirrors: {published:?}"
    );
    assert_eq!(
        acks_for(&state, "msg-cancelled"),
        vec![(false, Some("operator cancel".to_string()))],
        "the close refused the row it still held, with the operator's own word: {:?}",
        queued_ops(&state)
    );
    assert!(
        store.open_tasks(10).expect("open tasks").is_empty(),
        "nothing is left open for the server to re-offer"
    );
}
