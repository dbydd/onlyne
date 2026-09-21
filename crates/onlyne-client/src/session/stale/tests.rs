use super::*;
use onlyne_proto::{Body, Causality, ClientOp, Envelope, MsgKind, new_envelope, new_task_id};

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-09-12T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn row(task: &str, to: &str, age_secs: i64, state: LedgerState) -> WorkingRow {
    WorkingRow {
        task_id: task.to_string(),
        to: Some(to.to_string()),
        state,
        updated_at: now() - chrono::Duration::seconds(age_secs),
    }
}

#[test]
fn live_slot_is_skipped() {
    let rows = [row("t-live", "planner", 9_000, LedgerState::Acked)];
    let live = HashSet::from(["t-live".to_string()]);
    assert!(reconcile(&rows, &live, now(), 300, "planner").is_empty());
}

#[test]
fn age_equal_to_grace_stays() {
    let rows = [row("t-eq", "planner", 300, LedgerState::Acked)];
    assert!(reconcile(&rows, &HashSet::new(), now(), 300, "planner").is_empty());
}

#[test]
fn mixed_rows_converge_only_the_stale_ones() {
    let rows = [
        row("t-live", "planner", 9_000, LedgerState::Acked),
        row("t-young", "planner", 10, LedgerState::Acked),
        row("t-stale", "planner", 301, LedgerState::Acked),
        row("t-other", "builder", 9_000, LedgerState::Acked),
        row("t-inflight", "planner", 9_000, LedgerState::InFlight),
    ];
    let live = HashSet::from(["t-live".to_string()]);
    let out = reconcile(&rows, &live, now(), 300, "planner");
    assert_eq!(
        out.iter().map(|c| c.task_id.as_str()).collect::<Vec<_>>(),
        ["t-stale"]
    );
    let report = out[0].report();
    match report {
        Report::Complete {
            outcome,
            head,
            task_id,
            ..
        } => {
            assert_eq!(task_id, "t-stale");
            assert_eq!(outcome, Outcome::Failed);
            assert_eq!(head.as_deref(), Some(SESSION_DEAD));
        }
        other => panic!("expected a complete report, got {other:?}"),
    }
}

#[test]
fn pending_terminal_intent_takes_precedence() {
    let task = new_task_id();
    let envelope: Envelope = new_envelope(
        MsgKind::Task,
        Principal::role("planner"),
        Principal::role("planner"),
        Body::text("x"),
        Some(Causality::root(task.clone())),
    )
    .unwrap();
    let _ = envelope;
    let report = Report::Complete {
        task_id: task.clone(),
        outcome: Outcome::Failed,
        head: Some(SESSION_DEAD.to_string()),
        reply_to: None,
        cluster_ref: None,
    };
    assert!(pending_terminal_for(&task, &[ClientOp::Report(report)]));
    assert!(!pending_terminal_for(
        &task,
        &[ClientOp::Ack(onlyne_proto::AckArgs {
            msg_id: "m".into(),
            op_id: None,
            accepted: true,
            reason: None,
        })]
    ));
}
