use super::{apply_role_info, scan_stalls};
use crate::runtime::intent::op_for_intent;
use crate::runtime::runloop::test_support::{role_info, test_state};
use anyhow::Result;
use onlyne_proto::{ClientOp, Report};
use onlyne_session::{SessionLedger, VersionedSession};
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
