//! Session projection: the client's durable mirror of one session (§10).
//!
//! The client publishes that mirror inside its heartbeat report, and the
//! readiness and completion reports land beside it. Every accepted write passes
//! the `(generation, seq)` monotonic gate and then publishes one durable
//! `session_state` event.

use crate::faults::{self, FaultDraft};
use crate::relay;
use crate::stale;
use crate::state::State;
use anyhow::Context;
use chrono::{DateTime, Utc};
use onlyne_proto::{
    AgentPhase, ControlOp, DeliveryPhase, Event, Frame, FreshRead, Lifecycle, Outcome, Principal,
    QuerySessionsArgs, RecoveryPhase, Report, ResourcePhase, SessionProjection, SessionRow,
    SessionStateEvent,
};
use onlyne_store::{FaultQuery, ServerSessionRow, SessionWrite};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::broadcast;

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
    settle(state, role, None, report)
}

/// Apply one report an operator filed over the admin surface on a session's
/// behalf. It takes the path a session's own report takes, so the row keeps
/// the role that owns the task, and the `session_state` event names the
/// operator. A task with no row yet is written under the operator's role, as a
/// self-report from that role would be.
pub fn report_as_admin(
    state: &State,
    from: &str,
    report: &Report,
) -> anyhow::Result<ProjectionOutcome> {
    let owner = report
        .task_id()
        .and_then(|task_id| relay::task_owner(state, task_id))
        .unwrap_or_else(|| from.to_string());
    let admin = Principal::role(from);
    settle(state, &owner, Some(&admin), report)
}

fn settle(
    state: &State,
    role: &str,
    admin: Option<&Principal>,
    report: &Report,
) -> anyhow::Result<ProjectionOutcome> {
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
                admin,
            )
        }
        Report::Heartbeat {
            task_id,
            session_id,
            generation,
            seq,
            observed,
            projection,
            cluster_ref,
        } => {
            // Two frames share this kind. A beat carrying the client's
            // projection *is* the state publish: the projection lands in the row
            // verbatim and `desired` stays empty, which is what the projection
            // mirror has always written. A bare beat is liveness only, and the
            // server keeps the tuple it infers from it.
            let (projection, desired) = match projection {
                Some(projection) => (projection.clone(), None),
                None => {
                    let observed = merged_observation(observed, cluster_ref.as_ref());
                    (
                        SessionProjection {
                            lifecycle: Lifecycle::Working,
                            agent: AgentPhase::Running,
                            delivery: DeliveryPhase::Pending,
                            resource: ResourcePhase::Attached,
                            recovery: RecoveryPhase::NoRecovery,
                            outcome: None,
                            observed: Some(observed.clone()),
                        },
                        Some(observed),
                    )
                }
            };
            let exited = projection.lifecycle == Lifecycle::Exited;
            let outcome = write(
                state,
                role,
                task_id,
                session_id,
                *generation,
                *seq,
                projection,
                desired,
                admin,
            )?;
            // An applied `exited` write is the pane dying while the link is
            // still up. A claimed in-flight row for this session goes back on
            // the queue so the next pull can open a replacement. An
            // already-acked row is left alone. A bare beat never gets here: the
            // tuple inferred above is always `working`.
            if outcome.applied && exited {
                relay::release_exited_delivery(state, task_id, session_id)?;
            }
            Ok(outcome)
        }
        Report::Complete {
            task_id,
            outcome,
            head,
            reply_to,
            cluster_ref,
            // `details` and `files` are the client's to deliver to the next hop
            // and the originator; the ledger row keeps the head.
            ..
        } => {
            let origin = cluster_ref.clone();
            let stored = state.ledger.session_row_for_task(task_id)?;
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
                admin,
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

/// The reducer tuple a liveness-only heartbeat carries, with the relayed cluster
/// merged in as a sibling key. The tuple is stored *as* the row's observation
/// rather than wrapped in one, so `sessions --json` — and with it every reader
/// of `observed.host` — sees a single shape whichever client path wrote the row.
fn merged_observation(observed: &Value, cluster_ref: Option<&String>) -> Value {
    let mut value = observed.clone();
    if let (Some(cluster), Some(object)) = (cluster_ref, value.as_object_mut()) {
        object.insert("cluster_ref".into(), Value::String(cluster.clone()));
    }
    value
}

/// Whether a late publish adds the task verdict to an otherwise identical
/// stored projection.
fn adds_only_outcome(stored: &SessionProjection, incoming: &SessionProjection) -> bool {
    let SessionProjection {
        lifecycle: stored_lifecycle,
        agent: stored_agent,
        delivery: stored_delivery,
        resource: stored_resource,
        recovery: stored_recovery,
        outcome: stored_outcome,
        observed: stored_observed,
    } = stored;
    let SessionProjection {
        lifecycle: incoming_lifecycle,
        agent: incoming_agent,
        delivery: incoming_delivery,
        resource: incoming_resource,
        recovery: incoming_recovery,
        outcome: incoming_outcome,
        observed: incoming_observed,
    } = incoming;

    stored_outcome.is_none()
        && incoming_outcome.is_some()
        && stored_lifecycle == incoming_lifecycle
        && stored_agent == incoming_agent
        && stored_delivery == incoming_delivery
        && stored_resource == incoming_resource
        && stored_recovery == incoming_recovery
        && stored_observed == incoming_observed
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
    admin: Option<&Principal>,
) -> anyhow::Result<ProjectionOutcome> {
    // A publish from a session between deliveries names no delivery at all:
    // `task_id` is a `String` on the wire, so the empty string is the only
    // spelling it has for "none". Such a row is still the session's own, so it
    // is read by its session id, and the write binds the session to no task.
    let names_delivery = !task_id.is_empty();
    let stored = if names_delivery {
        state.ledger.session_row_for_task(task_id)?
    } else {
        state.ledger.get_session_row(session_id)?
    };
    if let Some(row) = &stored {
        let watermark = (row.generation.max(0) as u64, row.seq.max(0) as u64);
        if (generation, seq) <= watermark {
            let Ok(stored_projection) = serde_json::from_str(&row.observed_json) else {
                return Ok(ProjectionOutcome::skipped());
            };
            if !adds_only_outcome(&stored_projection, &projection) {
                // A beat that observed nothing new is liveness and nothing
                // else: no store write, no event, and the tuple and
                // `updated_at` stay exactly where they were. The beat moves
                // `last_seen` in the store's memory, where every read of the
                // row picks it up, and reaches the row itself only once the
                // row is a whole interval behind — which is why this path is
                // not the per-beat write v1 turned it into (plan §"网络与并发",
                // v1 finding 6).
                state.ledger.beat_session(
                    &row.session_id,
                    Utc::now().timestamp(),
                    last_seen_flush_secs(state),
                )?;
                return Ok(ProjectionOutcome::skipped());
            }
            let mut write = row.clone();
            write.observed_json = serde_json::to_string(&projection)?;
            write.updated_at = Utc::now().timestamp();
            if !state.ledger.publish_mirror_outcome(
                &write.session_id,
                &write.observed_json,
                &row.observed_json,
                write.updated_at,
            )? {
                return Ok(ProjectionOutcome::skipped());
            }
            if names_delivery {
                state.note_session_write(task_id);
            }
            emit_session_state(
                state,
                &write,
                row.generation.max(0) as u64,
                row.seq.max(0) as u64,
                projection,
                admin,
            )?;
            return Ok(ProjectionOutcome {
                applied: true,
                row: Some(row_from_write(&write)),
            });
        }
    }
    // The fault below is about a delivery's task: a client reporting a heartbeat
    // for a task whose completion already landed. A session between deliveries
    // has no task to contradict, so it is never asked.
    let revival = names_delivery
        && stored.as_ref().is_some_and(|row| {
            projection_from_write(row).lifecycle == Lifecycle::Exited
                && row.generation.max(0) as u64 == generation
                && matches!(
                    projection.lifecycle,
                    Lifecycle::Working | Lifecycle::Created
                )
        });
    let landing = lifecycle_name(projection.lifecycle);
    let session_id = if session_id.is_empty() {
        stored
            .as_ref()
            .map(|row| row.session_id.clone())
            .unwrap_or_else(|| task_id.to_string())
    } else {
        session_id.to_string()
    };
    let write = SessionWrite {
        session_id,
        task_id: names_delivery.then(|| task_id.to_string()),
        role: role.to_string(),
        generation: generation as i64,
        seq: seq as i64,
        agent_state: agent_name(projection.agent),
        delivery_state: delivery_name(projection.delivery),
        resource_state: resource_name(projection.resource),
        recovery_substate: recovery_name(projection.recovery),
        desired_json: serde_json::to_string(&desired.unwrap_or(Value::Null))?,
        observed_json: serde_json::to_string(&projection)?,
        mismatch_count: 0,
        last_seen: Utc::now().timestamp(),
        updated_at: Utc::now().timestamp(),
    };
    let applied = state.ledger.project_session(&write)?;
    if !applied {
        return Ok(ProjectionOutcome::skipped());
    }
    if names_delivery {
        state.note_session_write(task_id);
    }
    if revival {
        let open = state.ledger.faults_query(FaultQuery {
            task_id: Some(task_id.to_string()),
            kind: Some(stale::KIND_HEARTBEAT_AFTER_COMPLETE.to_string()),
            open_only: true,
            limit: 1,
            ..FaultQuery::default()
        })?;
        if open.is_empty() {
            faults::record(
                state,
                FaultDraft::new(
                    stale::KIND_HEARTBEAT_AFTER_COMPLETE,
                    format!(
                        "session {task_id} moved from exited to {landing} after a heartbeat while role {role} stays connected"
                    ),
                )
                .with_role(role)
                .with_task(task_id),
            )?;
        }
    }
    emit_session_state(state, &write, generation, seq, projection, admin)?;
    Ok(ProjectionOutcome {
        applied: true,
        row: Some(row_from_write(&write)),
    })
}

fn emit_session_state(
    state: &State,
    write: &SessionWrite,
    generation: u64,
    seq: u64,
    projection: SessionProjection,
    admin: Option<&Principal>,
) -> anyhow::Result<()> {
    state.emit(Event::SessionState(SessionStateEvent {
        task_id: write.task_id.clone(),
        role: write.role.clone(),
        session_id: write.session_id.clone(),
        generation,
        seq,
        projection,
        admin: admin.cloned(),
    }))?;
    Ok(())
}

/// Derived answer flag: a working row this process has seen, older than grace.
pub fn heartbeat_stale(
    lifecycle: Lifecycle,
    updated_at: DateTime<Utc>,
    now: DateTime<Utc>,
    grace_secs: u64,
    seen_since_open: bool,
) -> bool {
    lifecycle == Lifecycle::Working
        && seen_since_open
        && now.signed_duration_since(updated_at) > chrono::Duration::seconds(grace_secs as i64)
}

fn heartbeat_grace_secs(state: &State) -> u64 {
    state
        .spec_snapshot()
        .map(|spec| spec.server.heartbeat_grace_secs)
        .unwrap_or(onlyne_config::DEFAULT_HEARTBEAT_GRACE_SECS)
}

/// How far a live session's persisted `last_seen` may lag its beats, in seconds.
///
/// This is the reader's worst-case staleness, and the only reason a no-op beat
/// touches the table at all. A reader of the row is handed the live value while
/// this process holds one and the persisted value otherwise, so what this
/// bounds is what a reader sees when the live value is not there: after a
/// restart, or from anything reading the file rather than the cluster. Within
/// one presence window the answer is still a fact the live server agrees with;
/// past it, the row says nothing the cluster has not already outlived.
///
/// The number is the presence window the cluster already answers presence
/// with, `[server] heartbeat_timeout_ms` — thirty seconds unless the spec says
/// otherwise, which is three of the client's ten-second beats and the same
/// margin the client's own reconnect sweep takes. Taking the configured window
/// rather than a constant of our own keeps two answers from existing for one
/// question: a cluster that beats slowly persists slowly, and one that beats
/// fast is not handed a row that reads as silence.
///
/// Floored at one second: a spec that configures a window below that is asking
/// for more writes than the beats themselves, which is the v1 cadence this
/// slice exists to remove.
fn last_seen_flush_secs(state: &State) -> i64 {
    let millis = state
        .spec_snapshot()
        .map(|spec| spec.server.heartbeat_timeout_ms)
        .unwrap_or(onlyne_config::DEFAULT_HEARTBEAT_TIMEOUT_MS);
    (millis / 1000).max(1) as i64
}

/// Read the session table.
pub fn sessions(state: &State, query: QuerySessionsArgs) -> anyhow::Result<Vec<SessionRow>> {
    let rows = state.ledger.list_sessions(query)?;
    Ok(rows.iter().map(|row| answer_row(state, row)).collect())
}

/// Read the session table, probing the named task first when the caller asked.
///
/// A plain read (`fresh_wait_ms` absent) is exactly [`sessions`]: the stored
/// mirror, no control frame, nothing waited on. A fresh read names one task,
/// asks its owning client for a fresh observation through the control frame
/// that path already carries, and waits inside the caller's own bound for that
/// task's row to move. Either way the answer is the stored rows and a
/// [`FreshRead`] per row saying what the probe did; the read never turns a
/// probe that did not land into an error, and it never waits past its bound.
pub async fn sessions_read(
    state: &State,
    query: QuerySessionsArgs,
    admin: bool,
) -> anyhow::Result<Vec<SessionRow>> {
    let Some(wait_ms) = query.fresh_wait_ms else {
        return sessions(state, query);
    };
    let outcome = match query.task_id.clone() {
        Some(task_id) => probe_task(state, &task_id, wait_ms, admin).await,
        // No named task: there is no client to ask, and answering a mirror
        // under a marker that says so is the honest read.
        None => FreshRead::Offline,
    };
    let mut rows = sessions(state, query)?;
    for row in &mut rows {
        row.fresh = Some(outcome);
    }
    Ok(rows)
}

/// Ask one task's owning client for a fresh observation, and say how that went.
///
/// The ask is the `probe` control the client already answers: the owning role
/// on both ends of the envelope, no new op verb, and no client change. The wait
/// watches the durable `session_state` event the projection write publishes, so
/// what it returns on is the write that landed rather than a promise of one.
async fn probe_task(state: &State, task_id: &str, wait_ms: u64, admin: bool) -> FreshRead {
    let Some(row) = state.ledger.session_row_for_task(task_id).ok().flatten() else {
        return FreshRead::Offline;
    };
    let watermark = (row.generation.max(0) as u64, row.seq.max(0) as u64);
    let owner = row.role;
    if !state.is_connected(&owner) {
        return FreshRead::Offline;
    }
    // Subscribe before the probe leaves: the client's republish can land
    // between the send and a later subscription, and that republish is the
    // whole answer.
    let mut frames = state.subscribe_frames();
    let envelope = crate::router::control_envelope(
        &owner,
        &ControlOp::Probe {
            task_id: task_id.to_string(),
        },
        None,
    );
    match relay::send(state, &envelope, admin, Some(&owner)) {
        Ok(relay::RelayReply::Accepted(_)) | Ok(relay::RelayReply::Duplicate(_)) => {}
        // Nothing went out, so the row is the mirror and says so.
        _ => return FreshRead::Offline,
    }
    let bound = std::time::Duration::from_millis(wait_ms);
    match tokio::time::timeout(bound, await_advance(state, &mut frames, task_id, watermark)).await {
        Ok(true) => FreshRead::Probed,
        _ => FreshRead::Unanswered,
    }
}

/// Wait for the probed task's row to move past `watermark`.
///
/// The client republishes inside the heartbeat path, so the wait reads the
/// frame that write already broadcasts. A lagging receiver has missed frames
/// rather than seen a quiet row, so it re-reads the stored row per gap instead
/// of waiting out the bound; a closed channel cannot report an advance.
async fn await_advance(
    state: &State,
    frames: &mut broadcast::Receiver<Arc<Frame>>,
    task_id: &str,
    watermark: (u64, u64),
) -> bool {
    loop {
        match frames.recv().await {
            Ok(frame) => {
                if advances(&frame, task_id, watermark) {
                    return true;
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {
                if row_version(state, task_id).is_some_and(|moved| moved > watermark) {
                    return true;
                }
            }
            Err(broadcast::error::RecvError::Closed) => return false,
        }
    }
}

/// Whether one broadcast frame is the probed task's row moving past its watermark.
fn advances(frame: &Frame, task_id: &str, watermark: (u64, u64)) -> bool {
    let Frame::Ev { event, .. } = frame else {
        return false;
    };
    let Event::SessionState(moved) = &**event else {
        return false;
    };
    moved.task_id.as_deref() == Some(task_id) && (moved.generation, moved.seq) > watermark
}

/// The `(generation, seq)` the stored row of one task carries, when it exists.
fn row_version(state: &State, task_id: &str) -> Option<(u64, u64)> {
    state
        .ledger
        .session_row_for_task(task_id)
        .ok()
        .flatten()
        .map(|row| (row.generation.max(0) as u64, row.seq.max(0) as u64))
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
        public_lifecycle: projection.lifecycle,
        outcome: projection.outcome,
        projection,
        updated_at: Some(row.updated_at.to_string()),
        last_seen: Some(row.last_seen.to_string()),
        heartbeat_stale: false,
        // A fresh read stamps this per answer; the stored row knows nothing of
        // one, and a plain read claims nothing about its freshness.
        fresh: None,
    }
}

fn answer_row(state: &State, row: &ServerSessionRow) -> SessionRow {
    let mut out = row_from_write(row);
    let now = Utc::now();
    let updated_at = DateTime::from_timestamp(row.updated_at, 0).unwrap_or(now);
    out.heartbeat_stale = heartbeat_stale(
        out.public_lifecycle,
        updated_at,
        now,
        heartbeat_grace_secs(state),
        row.task_id
            .as_deref()
            .is_some_and(|task| state.has_seen_session(task)),
    );
    out
}

/// Rebuild the published projection of one stored row.
///
/// The mirror holds the client's projection whole, and the lifecycle is a key
/// inside those bytes — there is no column beside them to fall back to. When the
/// bytes do not parse, every field here takes the freshly created reading, the
/// lifecycle included, which is the same reading the store's lifecycle filter
/// gives such a row.
pub fn projection_from_write(row: &ServerSessionRow) -> SessionProjection {
    if let Ok(projection) = serde_json::from_str::<SessionProjection>(&row.observed_json) {
        return projection;
    }
    SessionProjection {
        lifecycle: Lifecycle::Created,
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
        .session_row_for_task(task_id)?
        .as_ref()
        .map(|row| answer_row(state, row)))
}

/// One stored agent word, read as the phase it names. A word the vocabulary does
/// not know is a row written before this one existed, and reads as the freshly
/// created phase — the same answer [`projection_from_write`] gives for a row with
/// no projection at all.
fn parse_agent(name: &str) -> AgentPhase {
    name.parse().unwrap_or(AgentPhase::Booting)
}

/// [`parse_agent`] for the delivery dimension.
fn parse_delivery(name: &str) -> DeliveryPhase {
    name.parse().unwrap_or(DeliveryPhase::NoIntent)
}

/// [`parse_agent`] for the resource dimension.
fn parse_resource(name: &str) -> ResourcePhase {
    name.parse().unwrap_or(ResourcePhase::Detached)
}

/// [`parse_agent`] for the recovery dimension.
fn parse_recovery(name: &str) -> RecoveryPhase {
    name.parse().unwrap_or(RecoveryPhase::NoRecovery)
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

/// The stored word for one agent phase. Every one of these four writers is the
/// phase's own `Display`, so [`parse_agent`] and its siblings read exactly what
/// they wrote: a variant added to the vocabulary names itself in one place.
fn agent_name(phase: AgentPhase) -> String {
    phase.to_string()
}

/// [`agent_name`] for the delivery dimension.
fn delivery_name(phase: DeliveryPhase) -> String {
    phase.to_string()
}

/// [`agent_name`] for the resource dimension.
fn resource_name(phase: ResourcePhase) -> String {
    phase.to_string()
}

/// [`agent_name`] for the recovery dimension.
fn recovery_name(phase: RecoveryPhase) -> String {
    phase.to_string()
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
