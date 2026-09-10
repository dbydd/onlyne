//! Fault recording, reading, and the explicit `repair_*` transitions (§8).
//!
//! Detection records a fact. Nothing here retries, escalates, or times out on
//! its own: a fault stays `open` until an operator runs one `repair_*` verb.

use crate::relay::RelayReject;
use crate::state::State;
use chrono::Utc;
use onlyne_proto::{AdminOp, ErrorCode, Event, FaultEvent, Lifecycle, Outcome, QueryFaultsArgs};
use onlyne_store::{FaultQuery, ServerFaultRow};
use serde_json::{Value, json};
use std::sync::Arc;

/// Fault kind recorded when a probe reports a resource that is gone.
pub const KIND_PROBE_FAILURE: &str = "probe_failure";
/// Fault kind recorded when a client reports a gap in its own observations.
pub const KIND_REPORT_GAP: &str = "report_gap";
/// Fault kind recorded when an adapter never sent its `hello`.
pub const KIND_HELLO_TIMEOUT: &str = "hello_timeout";
/// Fault kind recorded when `spec.toml` fails to reload.
pub const KIND_SPEC_RELOAD_FAILED: &str = "spec_reload_failed";

/// The state every freshly recorded fault starts in.
pub const STATE_OPEN: &str = "open";

/// One fault about to be recorded.
#[derive(Debug, Clone, Default)]
pub struct FaultDraft {
    pub kind: String,
    pub reason: String,
    pub task_id: Option<String>,
    pub role: Option<String>,
    pub session_id: Option<String>,
    pub generation: Option<u64>,
    pub seq: Option<u64>,
    pub desired: Option<Value>,
    pub observed: Option<Value>,
    pub intent: Option<String>,
    pub attempt: Option<u64>,
    pub backend_ref: Option<Value>,
}

impl FaultDraft {
    pub fn new(kind: impl Into<String>, reason: impl Into<String>) -> Self {
        FaultDraft {
            kind: kind.into(),
            reason: reason.into(),
            ..FaultDraft::default()
        }
    }

    pub fn with_role(mut self, role: &str) -> Self {
        self.role = Some(role.to_string());
        self
    }

    pub fn with_task(mut self, task_id: &str) -> Self {
        self.task_id = Some(task_id.to_string());
        self
    }

    pub fn with_session(mut self, session_id: &str) -> Self {
        self.session_id = Some(session_id.to_string());
        self
    }

    pub fn with_generation(mut self, generation: u64) -> Self {
        self.generation = Some(generation);
        self
    }

    pub fn with_seq(mut self, seq: u64) -> Self {
        self.seq = Some(seq);
        self
    }

    pub fn with_observed(mut self, observed: Value) -> Self {
        self.observed = Some(observed);
        self
    }

    pub fn with_desired(mut self, desired: Value) -> Self {
        self.desired = Some(desired);
        self
    }

    pub fn with_intent(mut self, intent: &str) -> Self {
        self.intent = Some(intent.to_string());
        self
    }

    pub fn with_attempt(mut self, attempt: u64) -> Self {
        self.attempt = Some(attempt);
        self
    }

    pub fn with_backend_ref(mut self, backend_ref: Value) -> Self {
        self.backend_ref = Some(backend_ref);
        self
    }

    /// A probe reported a resource that is not there.
    pub fn probe_failure(role: &str, task_id: &str, detail: &str) -> Self {
        FaultDraft::new(KIND_PROBE_FAILURE, detail)
            .with_role(role)
            .with_task(task_id)
    }

    /// A client reported a watermark gap in its own observation stream.
    pub fn report_gap(role: &str, task_id: &str, expected: u64, seen: u64) -> Self {
        FaultDraft::new(
            KIND_REPORT_GAP,
            format!("report watermark {seen} is behind the accepted generation {expected}"),
        )
        .with_role(role)
        .with_task(task_id)
        .with_observed(json!({ "expected": expected, "seen": seen }))
    }

    /// An adapter connection closed before its `hello` arrived.
    pub fn hello_timeout(role: &str) -> Self {
        FaultDraft::new(
            KIND_HELLO_TIMEOUT,
            format!("{role} sent no hello within the handshake window"),
        )
        .with_role(role)
    }

    /// `spec.toml` was rejected during a reload.
    pub fn spec_reload_failed(detail: &str) -> Self {
        FaultDraft::new(KIND_SPEC_RELOAD_FAILED, detail)
    }
}

/// Persist one fault and publish it on the observation plane.
pub fn record(state: &State, draft: FaultDraft) -> anyhow::Result<FaultEvent> {
    let created_at = Utc::now().timestamp();
    let id = state.ledger.record_fault(&ServerFaultRow {
        id: 0,
        task_id: draft.task_id.clone(),
        role: draft.role.clone(),
        session_id: draft.session_id.clone(),
        generation: draft.generation.map(|value| value as i64),
        seq: draft.seq.map(|value| value as i64),
        desired_json: draft
            .desired
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?,
        observed_json: draft
            .observed
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?,
        intent: draft.intent.clone(),
        attempt: draft.attempt.map(|value| value as i64),
        backend_ref: draft
            .backend_ref
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?,
        kind: draft.kind.clone(),
        reason: draft.reason.clone(),
        state: STATE_OPEN.to_string(),
        created_at,
    })?;
    let event = FaultEvent {
        id,
        task_id: draft.task_id,
        role: draft.role,
        session_id: draft.session_id,
        generation: draft.generation,
        seq: draft.seq,
        kind: draft.kind,
        reason: draft.reason,
        desired: draft.desired,
        observed: draft.observed,
        intent: draft.intent,
        attempt: draft.attempt,
        backend_ref: draft.backend_ref,
        state: Some(STATE_OPEN.to_string()),
        created_at: Some(created_at),
    };
    state.emit(Event::Fault(event.clone()))?;
    Ok(event)
}

/// Project one stored fault row onto the wire type.
pub fn event_from_row(row: &ServerFaultRow, state: Option<&str>) -> FaultEvent {
    FaultEvent {
        id: row.id,
        task_id: row.task_id.clone(),
        role: row.role.clone(),
        session_id: row.session_id.clone(),
        generation: row.generation.map(|value| value.max(0) as u64),
        seq: row.seq.map(|value| value.max(0) as u64),
        kind: row.kind.clone(),
        reason: row.reason.clone(),
        desired: row
            .desired_json
            .as_deref()
            .and_then(|text| serde_json::from_str(text).ok()),
        observed: row
            .observed_json
            .as_deref()
            .and_then(|text| serde_json::from_str(text).ok()),
        intent: row.intent.clone(),
        attempt: row.attempt.map(|value| value.max(0) as u64),
        backend_ref: row
            .backend_ref
            .as_deref()
            .and_then(|text| serde_json::from_str(text).ok()),
        state: Some(state.unwrap_or(row.state.as_str()).to_string()),
        created_at: Some(row.created_at),
    }
}

/// Read recorded faults.
pub fn query(state: &State, args: &QueryFaultsArgs) -> anyhow::Result<Vec<FaultEvent>> {
    let rows = state.ledger.faults_query_proto(args.clone())?;
    Ok(rows.iter().map(|row| event_from_row(row, None)).collect())
}

/// Run one `repair_*` verb as an explicit transition.
///
/// `RepairInspect` reads and emits nothing; every other verb moves fault state
/// and publishes a `fault` event carrying the new state.
pub fn repair(state: &Arc<State>, op: &AdminOp) -> anyhow::Result<Result<Value, RelayReject>> {
    match op {
        AdminOp::RepairInspect(target) => {
            let session = state
                .ledger
                .get_session_row(&target.task_id)?
                .map(|row| crate::projection::row_from_write(&row));
            let faults = query(
                state,
                &QueryFaultsArgs {
                    task_id: Some(target.task_id.clone()),
                    open_only: false,
                    limit: 100,
                    ..QueryFaultsArgs::default()
                },
            )?;
            Ok(Ok(
                json!({ "task_id": target.task_id, "session": session, "faults": faults }),
            ))
        }
        AdminOp::RepairAdopt(adopt) => {
            let Some(row) = state.ledger.get_session_row(&adopt.task_id)? else {
                return Ok(Err(unknown_task(&adopt.task_id)));
            };
            let mut next = row.clone();
            next.desired_json = serde_json::to_string(&json!({
                "backend": adopt.backend,
                "backend_ref": adopt.backend_ref,
                "reason": adopt.reason,
            }))?;
            next.seq = next.seq.saturating_add(1);
            next.updated_at = Utc::now().timestamp();
            state.ledger.project_session(&next)?;
            let moved = transition_task_faults(state, &adopt.task_id, "adopted", &adopt.reason)?;
            Ok(Ok(json!({ "task_id": adopt.task_id, "faults": moved })))
        }
        AdminOp::RepairRebind(rebind) => {
            let Some(row) = state.ledger.get_session_row(&rebind.task_id)? else {
                return Ok(Err(unknown_task(&rebind.task_id)));
            };
            let mut next = row.clone();
            let generation = next.generation.saturating_add(1);
            next.generation = generation;
            next.seq = 0;
            next.session_id = rebind.session_id.clone();
            next.desired_json = serde_json::to_string(&json!({
                "backend": rebind.backend,
                "backend_ref": rebind.backend_ref,
                "reason": rebind.reason,
            }))?;
            next.updated_at = Utc::now().timestamp();
            if state.ledger.project_session(&next)? {
                let event = Event::SessionState(onlyne_proto::SessionStateEvent {
                    task_id: next.task_id.clone(),
                    role: next.role.clone(),
                    session_id: next.session_id.clone(),
                    generation: generation.max(0) as u64,
                    seq: 0,
                    projection: crate::projection::projection_from_write(&next),
                });
                state.emit(event)?;
            }
            let moved = transition_task_faults(state, &rebind.task_id, "rebound", &rebind.reason)?;
            Ok(Ok(json!({
                "task_id": rebind.task_id,
                "generation": generation,
                "faults": moved
            })))
        }
        AdminOp::RepairRetry(target) => {
            let rows = state.ledger.ledger_query(onlyne_proto::LedgerQuery {
                task: Some(target.task_id.clone()),
                limit: 32,
                ..onlyne_proto::LedgerQuery::default()
            })?;
            let mut requeued = 0usize;
            let mut settled = false;
            for row in &rows {
                match row.state {
                    onlyne_proto::LedgerState::InFlight => {
                        state.ledger.requeue_one(&row.msg_id)?;
                        requeued += 1;
                    }
                    onlyne_proto::LedgerState::Queued => {}
                    _ => settled = true,
                }
            }
            if requeued == 0 && settled {
                return Ok(Err(RelayReject::new(
                    ErrorCode::Conflict,
                    format!(
                        "task {} is settled; the frozen ledger transition table has no edge back to queued",
                        target.task_id
                    ),
                    Some("task_id"),
                )));
            }
            let reason = target
                .reason
                .clone()
                .unwrap_or_else(|| "operator retry".to_string());
            transition_task_faults(state, &target.task_id, "retried", &reason)?;
            Ok(Ok(
                json!({ "task_id": target.task_id, "requeued": requeued }),
            ))
        }
        AdminOp::RepairFail(fail) => {
            let reason = fail.reason.clone();
            settle_task(
                state,
                &fail.task_id,
                Lifecycle::Exited,
                Outcome::Failed,
                &reason,
            )?;
            transition_task_faults(state, &fail.task_id, "failed", &reason)?;
            Ok(Ok(json!({ "task_id": fail.task_id, "outcome": "failed" })))
        }
        AdminOp::RepairClose(target) => {
            let reason = target
                .reason
                .clone()
                .unwrap_or_else(|| "operator close".to_string());
            settle_task(
                state,
                &target.task_id,
                Lifecycle::Exited,
                Outcome::Cancelled,
                &reason,
            )?;
            transition_task_faults(state, &target.task_id, "closed", &reason)?;
            Ok(Ok(
                json!({ "task_id": target.task_id, "lifecycle": "exited" }),
            ))
        }
        AdminOp::RepairAck(ack) => {
            let closed = state.ledger.ack_fault(ack.fault_id)?;
            if !closed {
                return Ok(Err(RelayReject::new(
                    ErrorCode::Invalid,
                    format!("fault {} is unknown or already closed", ack.fault_id),
                    Some("fault_id"),
                )));
            }
            let row = state
                .ledger
                .faults_query(FaultQuery {
                    limit: 1,
                    ..FaultQuery::default()
                })?
                .into_iter()
                .find(|row| row.id == ack.fault_id);
            let event = match row {
                Some(row) => event_from_row(&row, Some("acked")),
                None => FaultEvent {
                    id: ack.fault_id,
                    state: Some("acked".to_string()),
                    reason: ack.reason.clone(),
                    ..FaultEvent::default()
                },
            };
            state.emit(Event::Fault(event))?;
            Ok(Ok(json!({ "fault_id": ack.fault_id, "state": "acked" })))
        }
        _ => Ok(Err(RelayReject::new(
            ErrorCode::UnknownOp,
            "not a repair verb",
            Some("op"),
        ))),
    }
}

fn unknown_task(task_id: &str) -> RelayReject {
    RelayReject::new(
        ErrorCode::Invalid,
        format!("no session row for task {task_id}"),
        Some("task_id"),
    )
}

/// Settle one task on the session projection and the ledger.
fn settle_task(
    state: &Arc<State>,
    task_id: &str,
    lifecycle: Lifecycle,
    outcome: Outcome,
    reason: &str,
) -> anyhow::Result<()> {
    if let Some(row) = state.ledger.get_session_row(task_id)? {
        let mut next = row.clone();
        let projection = crate::projection::projection_with_outcome(&next, outcome);
        next.public_lifecycle = lifecycle_name(lifecycle).to_string();
        next.seq = next.seq.saturating_add(1);
        next.observed_json = serde_json::to_string(&projection)?;
        next.updated_at = Utc::now().timestamp();
        if state.ledger.project_session(&next)? {
            let event = Event::SessionState(onlyne_proto::SessionStateEvent {
                task_id: next.task_id.clone(),
                role: next.role.clone(),
                session_id: next.session_id.clone(),
                generation: next.generation.max(0) as u64,
                seq: next.seq.max(0) as u64,
                projection,
            });
            state.emit(event)?;
        }
    }
    for row in state.ledger.ledger_query(onlyne_proto::LedgerQuery {
        task: Some(task_id.to_string()),
        limit: 32,
        ..onlyne_proto::LedgerQuery::default()
    })? {
        match row.state {
            onlyne_proto::LedgerState::Queued => {
                state.ledger.mark_rejected(&row.msg_id, reason)?;
                let event = Event::LedgerState(crate::relay::ledger_event(
                    &row,
                    onlyne_proto::LedgerState::Rejected,
                    Some(outcome),
                    Some(reason.to_string()),
                ));
                state.emit(event)?;
            }
            onlyne_proto::LedgerState::InFlight => {
                state.ledger.mark_rejected(&row.msg_id, reason)?;
                let event = Event::LedgerState(crate::relay::ledger_event(
                    &row,
                    onlyne_proto::LedgerState::Rejected,
                    Some(outcome),
                    Some(reason.to_string()),
                ));
                state.emit(event)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn lifecycle_name(lifecycle: Lifecycle) -> &'static str {
    match lifecycle {
        Lifecycle::Created => "created",
        Lifecycle::Working => "working",
        Lifecycle::Idle => "idle",
        Lifecycle::Exited => "exited",
    }
}

/// Move every open fault of a task to `next_state`.
///
/// `ServerLedger::update_fault_state` writes the rows and one `fault` event per
/// row in one transaction, so the observation plane sees each transition once.
pub fn transition_task_faults(
    state: &State,
    task_id: &str,
    next_state: &str,
    reason: &str,
) -> anyhow::Result<usize> {
    let moved = state
        .ledger
        .update_fault_state(task_id, next_state, reason)?;
    Ok(moved.len())
}
