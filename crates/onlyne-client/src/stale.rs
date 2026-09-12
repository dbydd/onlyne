//! Startup residual-account reconcile for this role's in-flight work.
//!
//! After a crash the server still holds `working` rows that this role owned.
//! The client is the lifecycle authority: it reports `failed{reason:
//! "session_dead"}` for rows whose task is no longer live here and whose age
//! exceeds the configured grace. Nothing here mutates the server ledger.

use chrono::{DateTime, Utc};
use onlyne_proto::{LedgerEntry, LedgerState, Outcome, Principal, Report};
use std::collections::HashSet;

/// Reason stamped on a residual-account terminal report.
pub const SESSION_DEAD: &str = "session_dead";

/// One working ledger row considered for residual-account reconcile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkingRow {
    pub task_id: String,
    pub to: Option<String>,
    pub state: LedgerState,
    pub updated_at: DateTime<Utc>,
}

impl WorkingRow {
    /// Project one `query_ledger` row onto the residual-account view.
    pub fn from_entry(entry: &LedgerEntry) -> Option<Self> {
        let task_id = entry.task.clone()?;
        Some(Self {
            task_id,
            to: role_of(&entry.to),
            state: entry.state,
            updated_at: entry.acked_at.unwrap_or(entry.enqueued_at),
        })
    }
}

fn role_of(principal: &Principal) -> Option<String> {
    principal.role_name().map(str::to_string)
}

/// One residual row that should be reported `failed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Convergence {
    pub task_id: String,
}

impl Convergence {
    /// The terminal report the client sends for this residual row.
    pub fn report(&self) -> Report {
        Report::Complete {
            task_id: self.task_id.clone(),
            outcome: Outcome::Failed,
            head: Some(SESSION_DEAD.to_string()),
            reply_to: None,
            cluster_ref: None,
        }
    }
}

/// Decide which working rows this role should fail as `session_dead`.
///
/// A row is skipped when:
/// * it is not `acked` (the residual case is a delivered-and-acked task
///   whose session is gone),
/// * it is not addressed to `role`,
/// * `task_id` currently has a live slot,
/// * its age is at most `grace_secs` (equality stays, so the next pass
///   reconsiders it).
pub fn reconcile(
    working_rows: &[WorkingRow],
    live_task_ids: &HashSet<String>,
    now: DateTime<Utc>,
    grace_secs: u64,
    role: &str,
) -> Vec<Convergence> {
    let grace = chrono::Duration::seconds(grace_secs as i64);
    working_rows
        .iter()
        .filter(|row| row.state == LedgerState::Acked)
        .filter(|row| row.to.as_deref() == Some(role))
        .filter(|row| !live_task_ids.contains(&row.task_id))
        .filter(|row| now.signed_duration_since(row.updated_at) > grace)
        .map(|row| Convergence {
            task_id: row.task_id.clone(),
        })
        .collect()
}

/// Whether the durable intent queue still holds a terminal report for `task_id`.
///
/// Pending reports take precedence over residual-account judgement: a crash
/// that queued `failed`/`complete` and never flushed it must replay that
/// intent before inventing a `session_dead` report.
pub fn pending_terminal_for(task_id: &str, intent_ops: &[onlyne_proto::ClientOp]) -> bool {
    intent_ops.iter().any(|op| match op {
        onlyne_proto::ClientOp::Report(Report::Complete {
            task_id: reported, ..
        }) => reported == task_id,
        _ => false,
    })
}

#[cfg(test)]
mod tests {
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
}
