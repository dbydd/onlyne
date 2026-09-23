//! Sweep the mirror rows whose own task has already settled.
//!
//! A session row whose stored projection still reads `working` while the task's
//! own ledger row reached a terminal state is a fossil. The ledger settled, and
//! the mirror kept the older bytes. One pass reads the working rows, joins each
//! to its task's ledger row, and writes the verdict that row already carries
//! onto the mirror. Every row the pass moves lands in `ghost_sweeps` with the
//! ledger state that justified it.
//!
//! A `working` row whose owner role is offline while its task is still open
//! stays out of this pass. Settling it would put a verdict on live work and
//! swallow the requeue that work is owed, so `stale_working` stays its only
//! output, and the supervisor and the `repair_*` verbs keep the call.
//!
//! The pass reads rows the server already holds, and writes the mirror row
//! alone.

use crate::State;
use crate::faults;
use crate::relay::TASK_ORIGIN_ROW_LIMIT;
use chrono::Utc;
use onlyne_proto::{GhostSweep, LedgerState, Lifecycle, MsgKind, Outcome, QuerySessionsArgs};
use onlyne_store::{GhostSweepRow, ServerSessionRow};
use std::sync::Arc;

/// Evidence tag written into `ghost_sweeps.evidence`.
///
/// The stored text is the tag plus the ledger state behind it, so a reader
/// matches on this prefix and reads the state off the rest.
pub const EVIDENCE_TASK_SETTLED: &str = "task_settled";

/// Working rows one pass reads from the mirror, the bound the observer uses.
const SWEEP_ROW_LIMIT: u32 = 500;

/// The evidence text for one sweep: the tag plus the ledger state behind it.
pub fn evidence(state: LedgerState) -> String {
    format!("{EVIDENCE_TASK_SETTLED}:{state}")
}

/// The reason a swept settlement carries onto the ledger rows it touches.
fn sweep_reason(state: LedgerState) -> String {
    format!("ghost sweep: the task's ledger row reads {state}")
}

/// The verdict a terminal ledger row carries, when it carries one.
///
/// `acked` reads as `done`. `rejected` and `expired` read as `failed`. A row
/// still `queued` or `in_flight` has no verdict to read, so the pass leaves its
/// mirror row alone.
pub fn settled_outcome(state: LedgerState) -> Option<Outcome> {
    match state {
        LedgerState::Acked => Some(Outcome::Done),
        LedgerState::Rejected | LedgerState::Expired => Some(Outcome::Failed),
        LedgerState::Queued | LedgerState::InFlight => None,
    }
}

/// The state of a task's own ledger row: the row whose kind is `task`.
///
/// `ServerLedger::ledger_task` reads in insertion order, so the first `task` row
/// is the dispatch row that created the work. That is the row
/// [`crate::relay::task_origin`] reads for its own question. A task with no such
/// row answers `None`, and the pass then has no evidence to act on.
fn task_ledger_state(state: &State, task_id: &str) -> anyhow::Result<Option<LedgerState>> {
    let rows = state.ledger.ledger_task(task_id, TASK_ORIGIN_ROW_LIMIT)?;
    Ok(rows
        .iter()
        .find(|row| row.kind == MsgKind::Task)
        .map(|row| row.state))
}

/// One pass over the working mirror rows.
///
/// The answer holds one audit row per mirror row the pass moved, in the order it
/// moved them.
pub fn sweep_once(state: &Arc<State>) -> anyhow::Result<Vec<GhostSweepRow>> {
    let working = state.ledger.list_sessions(QuerySessionsArgs {
        lifecycle: Some(Lifecycle::Working),
        limit: SWEEP_ROW_LIMIT,
        ..QuerySessionsArgs::default()
    })?;
    let mut swept = Vec::new();
    for row in &working {
        if let Some(audit) = sweep_row(state, row)? {
            swept.push(audit);
        }
    }
    Ok(swept)
}

/// Settle one working mirror row whose task ledger row already reached a verdict.
///
/// The settlement goes through [`crate::faults::settle_task`], the audited write
/// that rewrites `observed_json`, bumps `seq`, persists the row and emits
/// `session_state`. The audit row names the write the pass made, so a row that
/// moved between this pass's read and its write records nothing here.
fn sweep_row(state: &Arc<State>, row: &ServerSessionRow) -> anyhow::Result<Option<GhostSweepRow>> {
    let Some(ledger_state) = task_ledger_state(state, &row.task_id)? else {
        return Ok(None);
    };
    let Some(outcome) = settled_outcome(ledger_state) else {
        return Ok(None);
    };
    let settled = faults::settle_task(state, &row.task_id, outcome, &sweep_reason(ledger_state))?;
    let Some(settlement) = settled else {
        return Ok(None);
    };
    if settlement.generation != row.generation || settlement.seq_before != row.seq {
        tracing::debug!(
            task = %row.task_id,
            generation = settlement.generation,
            seq_before = settlement.seq_before,
            "a write landed between the sweep's read and its write, so the pass records no audit row"
        );
        return Ok(None);
    }
    let audit = GhostSweepRow {
        id: 0,
        task_id: row.task_id.clone(),
        role: row.role.clone(),
        session_id: row.session_id.clone(),
        generation: settlement.generation,
        seq_before: settlement.seq_before,
        seq_after: settlement.seq_after,
        outcome,
        evidence: evidence(ledger_state),
        swept_at: Utc::now().timestamp(),
    };
    let id = state.ledger.record_ghost_sweep(&audit)?;
    Ok(Some(GhostSweepRow { id, ..audit }))
}

/// One store audit row as `query_ghost_sweeps` answers it.
pub fn entry_from_row(row: &GhostSweepRow) -> GhostSweep {
    GhostSweep {
        id: row.id,
        task_id: row.task_id.clone(),
        role: row.role.clone(),
        session_id: row.session_id.clone(),
        generation: row.generation.max(0) as u64,
        seq_before: row.seq_before.max(0) as u64,
        seq_after: row.seq_after.max(0) as u64,
        outcome: row.outcome,
        evidence: row.evidence.clone(),
        swept_at: row.swept_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One ledger state reads as one verdict, and an open row reads as none.
    #[test]
    fn the_ledger_state_decides_the_outcome() {
        assert_eq!(settled_outcome(LedgerState::Acked), Some(Outcome::Done));
        assert_eq!(
            settled_outcome(LedgerState::Rejected),
            Some(Outcome::Failed)
        );
        assert_eq!(settled_outcome(LedgerState::Expired), Some(Outcome::Failed));
        assert_eq!(settled_outcome(LedgerState::Queued), None);
        assert_eq!(settled_outcome(LedgerState::InFlight), None);
    }
}
