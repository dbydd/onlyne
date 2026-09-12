//! Session projection: the client's durable mirror of one session (§10).
//!
//! Every accepted write passes the `(generation, seq)` monotonic gate and then
//! publishes one durable `session_state` event.

use crate::faults::{self, FaultDraft};
use crate::state::State;
use anyhow::Context;
use chrono::Utc;
use onlyne_proto::{
    AgentPhase, DeliveryPhase, Event, Lifecycle, Outcome, QuerySessionsArgs, RecoveryPhase, Report,
    ResourcePhase, SessionProjection, SessionRow, SessionStateEvent, SessionSyncArgs,
};
use onlyne_store::{ServerSessionRow, SessionWrite};
use serde_json::Value;

/// The result of one projection write.
#[derive(Debug, Clone)]
pub struct ProjectionOutcome {
    pub applied: bool,
    pub row: Option<SessionRow>,
}

impl ProjectionOutcome {
    fn skipped() -> Self {
        ProjectionOutcome {
            applied: false,
            row: None,
        }
    }
}

/// Apply one client report to the session table.
pub fn report(state: &State, role: &str, report: &Report) -> anyhow::Result<ProjectionOutcome> {
    match report {
        Report::Ready {
            task_id,
            session_id,
            generation,
            seq,
            cluster_ref,
        } => {
            let origin = cluster_ref.clone();
            let projection = SessionProjection {
                lifecycle: Lifecycle::Working,
                agent: AgentPhase::Ready,
                delivery: DeliveryPhase::NoIntent,
                resource: ResourcePhase::Attached,
                recovery: RecoveryPhase::NoRecovery,
                outcome: None,
                observed: origin.map(|cluster| serde_json::json!({ "cluster_ref": cluster })),
            };
            write(
                state,
                role,
                task_id,
                session_id,
                *generation,
                *seq,
                projection,
                None,
            )
        }
        Report::Heartbeat {
            task_id,
            generation,
            seq,
            observed,
            cluster_ref,
        } => {
            let observed = merged_observation(observed, cluster_ref.as_ref());
            let projection = SessionProjection {
                lifecycle: Lifecycle::Working,
                agent: AgentPhase::Running,
                delivery: DeliveryPhase::Pending,
                resource: ResourcePhase::Attached,
                recovery: RecoveryPhase::NoRecovery,
                outcome: None,
                observed: Some(observed.clone()),
            };
            write(
                state,
                role,
                task_id,
                "",
                *generation,
                *seq,
                projection,
                Some(observed),
            )
        }
        Report::Complete {
            task_id,
            outcome,
            head,
            reply_to,
            cluster_ref,
        } => {
            let origin = cluster_ref.clone();
            let stored = state.ledger.get_session_row(task_id)?;
            let mut observation = serde_json::json!({
                "head": head,
                "reply_to": reply_to,
                "cluster_ref": origin,
            });
            // A completion replaces the snapshot, but placement is a property of
            // the process rather than of the event that ends its run: erasing the
            // pane here would leave a supervisor unable to say where a finished
            // session ran.
            if let Some(host) = stored
                .as_ref()
                .and_then(|row| serde_json::from_str::<Value>(&row.observed_json).ok())
                .and_then(|projection| projection.pointer("/observed/host").cloned())
            {
                observation["host"] = host;
            }
            let projection = SessionProjection {
                lifecycle: Lifecycle::Exited,
                agent: AgentPhase::Gone,
                delivery: DeliveryPhase::Accepted,
                resource: ResourcePhase::Closed,
                recovery: RecoveryPhase::NoRecovery,
                outcome: Some(*outcome),
                observed: Some(observation),
            };
            let (generation, seq) = match &stored {
                Some(row) => (row.generation.max(0) as u64, row.seq.max(0) as u64 + 1),
                None => (1, 1),
            };
            let session_id = stored
                .as_ref()
                .map(|row| row.session_id.clone())
                .unwrap_or_default();
            write(
                state,
                role,
                task_id,
                &session_id,
                generation,
                seq,
                projection,
                None,
            )
        }
        Report::Fault {
            task_id,
            session_id,
            generation,
            seq,
            kind,
            reason,
            desired,
            observed,
        } => {
            let mut draft = FaultDraft::new(kind.clone(), reason.clone()).with_role(role);
            if let Some(task_id) = task_id {
                draft = draft.with_task(task_id);
            }
            if let Some(session_id) = session_id {
                draft = draft.with_session(session_id);
            }
            if let Some(generation) = generation {
                draft = draft.with_generation(*generation);
            }
            if let Some(seq) = seq {
                draft = draft.with_seq(*seq);
            }
            if let Some(desired) = desired {
                draft = draft.with_desired(desired.clone());
            }
            if let Some(observed) = observed {
                draft = draft.with_observed(observed.clone());
            }
            faults::record(state, draft)?;
            Ok(ProjectionOutcome::skipped())
        }
    }
}

/// The reducer tuple a heartbeat carries, with the relayed cluster merged in as
/// a sibling key. The tuple is stored *as* the row's observation rather than
/// wrapped in one, so `sessions --json` — and with it every reader of
/// `observed.host` — sees a single shape whichever client path wrote the row.
fn merged_observation(observed: &Value, cluster_ref: Option<&String>) -> Value {
    let mut value = observed.clone();
    if let (Some(cluster), Some(object)) = (cluster_ref, value.as_object_mut()) {
        object.insert("cluster_ref".into(), Value::String(cluster.clone()));
    }
    value
}

/// Apply one `session_sync` request to the session table.
pub fn session_sync(
    state: &State,
    role: &str,
    args: &SessionSyncArgs,
) -> anyhow::Result<ProjectionOutcome> {
    write(
        state,
        role,
        &args.task_id,
        &args.session_id,
        args.generation,
        args.seq,
        args.projection.clone(),
        None,
    )
}

/// Write one projection behind the `(generation, seq)` gate.
#[allow(clippy::too_many_arguments)]
pub fn write(
    state: &State,
    role: &str,
    task_id: &str,
    session_id: &str,
    generation: u64,
    seq: u64,
    projection: SessionProjection,
    desired: Option<Value>,
) -> anyhow::Result<ProjectionOutcome> {
    let stored = state.ledger.get_session_row(task_id)?;
    if let Some(row) = &stored {
        let watermark = (row.generation.max(0) as u64, row.seq.max(0) as u64);
        if (generation, seq) <= watermark {
            return Ok(ProjectionOutcome::skipped());
        }
    }
    let session_id = if session_id.is_empty() {
        stored
            .as_ref()
            .map(|row| row.session_id.clone())
            .unwrap_or_else(|| task_id.to_string())
    } else {
        session_id.to_string()
    };
    let write = SessionWrite {
        task_id: task_id.to_string(),
        role: role.to_string(),
        session_id,
        generation: generation as i64,
        seq: seq as i64,
        public_lifecycle: lifecycle_name(projection.lifecycle).to_string(),
        agent_state: agent_name(projection.agent).to_string(),
        delivery_state: delivery_name(projection.delivery).to_string(),
        resource_state: resource_name(projection.resource).to_string(),
        recovery_substate: recovery_name(projection.recovery).to_string(),
        desired_json: serde_json::to_string(&desired.unwrap_or(Value::Null))?,
        observed_json: serde_json::to_string(&projection)?,
        mismatch_count: 0,
        updated_at: Utc::now().timestamp(),
    };
    let applied = state.ledger.project_session(&write)?;
    if !applied {
        return Ok(ProjectionOutcome::skipped());
    }
    state.emit(Event::SessionState(SessionStateEvent {
        task_id: write.task_id.clone(),
        role: write.role.clone(),
        session_id: write.session_id.clone(),
        generation,
        seq,
        projection,
    }))?;
    Ok(ProjectionOutcome {
        applied: true,
        row: Some(row_from_write(&write)),
    })
}

/// Read the session table.
pub fn sessions(state: &State, query: QuerySessionsArgs) -> anyhow::Result<Vec<SessionRow>> {
    let rows = state.ledger.list_sessions(query)?;
    Ok(rows.iter().map(row_from_write).collect())
}

/// Project one stored row onto the wire type.
pub fn row_from_write(row: &ServerSessionRow) -> SessionRow {
    let projection = projection_from_write(row);
    SessionRow {
        task_id: row.task_id.clone(),
        role: Some(row.role.clone()),
        session_id: row.session_id.clone(),
        generation: row.generation.max(0) as u64,
        seq: row.seq.max(0) as u64,
        public_lifecycle: parse_lifecycle(&row.public_lifecycle),
        outcome: projection.outcome,
        projection,
        updated_at: Some(row.updated_at.to_string()),
    }
}

/// Rebuild the published projection of one stored row.
pub fn projection_from_write(row: &ServerSessionRow) -> SessionProjection {
    if let Ok(projection) = serde_json::from_str::<SessionProjection>(&row.observed_json) {
        return projection;
    }
    SessionProjection {
        lifecycle: parse_lifecycle(&row.public_lifecycle),
        agent: parse_agent(&row.agent_state),
        delivery: parse_delivery(&row.delivery_state),
        resource: parse_resource(&row.resource_state),
        recovery: parse_recovery(&row.recovery_substate),
        outcome: None,
        observed: None,
    }
}

/// The stored projection with one outcome applied.
pub fn projection_with_outcome(row: &ServerSessionRow, outcome: Outcome) -> SessionProjection {
    let mut projection = projection_from_write(row);
    projection.outcome = Some(outcome);
    projection.lifecycle = Lifecycle::Exited;
    projection
}

/// The `sessions` row of one task, when it exists.
pub fn session_row(state: &State, task_id: &str) -> anyhow::Result<Option<SessionRow>> {
    Ok(state
        .ledger
        .get_session_row(task_id)?
        .as_ref()
        .map(row_from_write))
}

/// Parse a stored lifecycle name.
pub fn parse_lifecycle(name: &str) -> Lifecycle {
    match name {
        "working" => Lifecycle::Working,
        "idle" => Lifecycle::Idle,
        "exited" => Lifecycle::Exited,
        _ => Lifecycle::Created,
    }
}

fn parse_agent(name: &str) -> AgentPhase {
    match name {
        "ready" => AgentPhase::Ready,
        "running" => AgentPhase::Running,
        "idle" => AgentPhase::Idle,
        "gone" => AgentPhase::Gone,
        _ => AgentPhase::Booting,
    }
}

fn parse_delivery(name: &str) -> DeliveryPhase {
    match name {
        "pending" => DeliveryPhase::Pending,
        "retrying" => DeliveryPhase::Retrying,
        "accepted" => DeliveryPhase::Accepted,
        "exhausted" => DeliveryPhase::Exhausted,
        _ => DeliveryPhase::NoIntent,
    }
}

fn parse_resource(name: &str) -> ResourcePhase {
    match name {
        "attached" => ResourcePhase::Attached,
        "closing" => ResourcePhase::Closing,
        "closed" => ResourcePhase::Closed,
        _ => ResourcePhase::Detached,
    }
}

fn parse_recovery(name: &str) -> RecoveryPhase {
    match name {
        "idle_waiting" => RecoveryPhase::IdleWaiting,
        "idle_fault" => RecoveryPhase::IdleFault,
        "draining" => RecoveryPhase::Draining,
        _ => RecoveryPhase::NoRecovery,
    }
}

/// Wire name of one lifecycle value.
pub fn lifecycle_name(lifecycle: Lifecycle) -> &'static str {
    match lifecycle {
        Lifecycle::Created => "created",
        Lifecycle::Working => "working",
        Lifecycle::Idle => "idle",
        Lifecycle::Exited => "exited",
    }
}

fn agent_name(phase: AgentPhase) -> &'static str {
    match phase {
        AgentPhase::Booting => "booting",
        AgentPhase::Ready => "ready",
        AgentPhase::Running => "running",
        AgentPhase::Idle => "idle",
        AgentPhase::Gone => "gone",
    }
}

fn delivery_name(phase: DeliveryPhase) -> &'static str {
    match phase {
        DeliveryPhase::NoIntent => "none",
        DeliveryPhase::Pending => "pending",
        DeliveryPhase::Retrying => "retrying",
        DeliveryPhase::Accepted => "accepted",
        DeliveryPhase::Exhausted => "exhausted",
    }
}

fn resource_name(phase: ResourcePhase) -> &'static str {
    match phase {
        ResourcePhase::Detached => "detached",
        ResourcePhase::Attached => "attached",
        ResourcePhase::Closing => "closing",
        ResourcePhase::Closed => "closed",
    }
}

fn recovery_name(phase: RecoveryPhase) -> &'static str {
    match phase {
        RecoveryPhase::NoRecovery => "none",
        RecoveryPhase::IdleWaiting => "idle_waiting",
        RecoveryPhase::IdleFault => "idle_fault",
        RecoveryPhase::Draining => "draining",
    }
}

/// Record a probe gap as a fault through the projection surface.
pub fn note_report_gap(
    state: &State,
    role: &str,
    task_id: &str,
    expected: u64,
    seen: u64,
) -> anyhow::Result<()> {
    let draft = FaultDraft::report_gap(role, task_id, expected, seen);
    faults::record(state, draft).context("record the report gap")?;
    Ok(())
}
