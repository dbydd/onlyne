//! `reconcile` — the bridge between the pure lifecycle reducer and the durable
//! session ledger, plus the passes that make the two agree.
//!
//! `lifecycle::apply` is the authority on what a legal session tuple is;
//! `db::sessions` is the authority on what survived a restart. This module is
//! the only place that carries a verdict from one into the other:
//!
//! * [`apply_persist`] reduces one event and persists it. Writes happen on
//!   `Applied` only, so a stale or illegal report cannot move the ledger
//!   backwards, and every `Ignored`/`Rejected` is logged with its reason.
//! * the `feed_*` helpers allocate the next version from the stored watermark,
//!   so a scheduler-internal observation (a `swarm_ready` seen on the socket, a
//!   handoff consumed from the out channel) enters the reducer through the same
//!   idempotence gate as a `---swarm-report` frame from the agent.
//! * [`startup_reconcile`] and [`periodic_reconcile`] compare every open session
//!   against the live backend and the task ledger, and record the divergences
//!   that survived a crash into `db::faults`.
//!
//! Two boundaries hold throughout:
//!
//! * `Outcome` belongs to the task ledger. A terminal ledger result is mirrored
//!   through the reducer's authoritative `Heartbeat` snapshot (see [`settle`]),
//!   never by editing projected columns.
//! * an unprovable resource stays unknown. `sched::session_alive`'s rule — a
//!   stub handle, an empty handle, or a probe that failed on transport is
//!   unknown-alive — is reused verbatim, so a dead socket cannot terminate a
//!   session that is still working.

use std::sync::Arc;

use serde::Serialize;
use serde_json::json;

use crate::db::{FaultRecord, IntentRecord, SessionRecord, TaskRow, TaskState, VersionedSession};
use crate::lifecycle::{
    self, AgentState, DeliveryState, IgnoredReason, LifecycleEvent, Observation, Outcome,
    PublicLifecycle, RecoveryState, ResourceState, Verdict, Version,
};
use crate::proto::{self, LifecycleKind, LifecycleReport, ParseResult};
use crate::runtime::{ResourceProbe, SessionRef};
// Both names belong to the test module: it names one rejected verdict by
// variant and implements a fake backend. A `dyn Trait` method call in the
// shipped binary needs neither import.
#[cfg(test)]
use crate::lifecycle::RejectReason;
#[cfg(test)]
use crate::runtime::SessionBackend;
use crate::sched::{self, Sched};

/// Consecutive reconcile mismatches tolerated before the session is isolated
/// into `idle_fault`. A scheduler-created session has no per-task policy yet,
/// so the row carries the defaults the reducer's own tests pin.
pub const DEFAULT_ISOLATE_AFTER: u32 = 1;
/// Consecutive mismatches tolerated before the generation is terminated and the
/// unsettled work is recorded as a fault.
pub const DEFAULT_TERMINATE_AFTER: u32 = 3;

fn initial_observation() -> Observation {
    Observation::initial(DEFAULT_ISOLATE_AFTER, DEFAULT_TERMINATE_AFTER)
}

/// Serialize a lifecycle enum into the short tag its ledger column stores.
/// Every state enum here is a fieldless variant set with
/// `rename_all = "snake_case"`, so a non-string means the schema drifted.
fn tag<T: Serialize>(value: &T) -> anyhow::Result<String> {
    match serde_json::to_value(value)? {
        serde_json::Value::String(text) => Ok(text),
        other => anyhow::bail!("lifecycle state must serialize to a string: {other}"),
    }
}

/// Ledger counters are `i64`, the reducer's are `u64` bounded by
/// `proto::MAX_COUNTER` (2^53), so the conversion is lossless for anything the
/// protocol accepts and saturates instead of wrapping on a corrupt row.
fn counter(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

pub(crate) fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

fn short(task_id: &str) -> &str {
    &task_id[..8.min(task_id.len())]
}

/// Project a reducer observation into one ledger row. `desired_json` records the
/// event that was accepted: the reducer has no separate desired model yet, so
/// the accepted intent is the honest audit entry next to the observed tuple.
pub fn to_versioned(
    obs: &Observation,
    backend_ref: &str,
    desired_json: &str,
) -> anyhow::Result<VersionedSession> {
    Ok(VersionedSession {
        agent_state: tag(&obs.agent)?,
        delivery_state: tag(&obs.delivery)?,
        resource_state: tag(&obs.resource)?,
        public_lifecycle: tag(&obs.public)?,
        recovery_substate: tag(&obs.recovery)?,
        desired_json: desired_json.to_string(),
        observed_json: serde_json::to_string(obs)?,
        generation: counter(obs.version.generation),
        seq: counter(obs.version.seq),
        backend_ref: backend_ref.to_string(),
        mismatch_count: counter(u64::from(obs.mismatch_count)),
        updated_at: now_unix(),
    })
}

/// `backend_ref` names the backend resource the tuple was observed on. The live
/// in-memory `SessionRef` is the freshest answer and is stored whole, so a
/// restart can rebuild a probe target from the row alone. Without an in-memory
/// session the previous reference survives: an event never invents a resource.
fn backend_ref_json(sched: &Sched, task_id: &str, row: Option<&SessionRecord>) -> String {
    if let Some(session) = sched.sessions.lock().unwrap().get(task_id) {
        if let Ok(json) = serde_json::to_string(session) {
            return json;
        }
    }
    if let Some(row) = row {
        let stored = row.backend_ref.trim();
        if !stored.is_empty() && stored != "{}" {
            return stored.to_string();
        }
    }
    if let Some(handle) = sched.terminals.lock().unwrap().get(task_id) {
        return json!({ "handle": handle }).to_string();
    }
    "{}".to_string()
}

/// Decode the stored tuple. A row whose `observed_json` is unparsable or is not
/// a legal observation is rebuilt from `Observation::initial` **at the row's own
/// watermark** rather than from scratch: the reducer's protection against stale
/// events is the watermark, and rewinding it would let an old report overwrite
/// newer truth. The dimensions the corruption destroyed (`outcome`,
/// `generation_live`) come back with the next probe or heartbeat.
pub(crate) fn stored_observation(sched: &Arc<Sched>, row: Option<&SessionRecord>) -> Observation {
    let Some(row) = row else {
        return initial_observation();
    };
    match serde_json::from_str::<Observation>(&row.observed_json) {
        Ok(obs) if lifecycle::is_legal(&obs) => obs,
        Ok(obs) => corrupt_observation(sched, row, &format!("illegal tuple {obs:?}")),
        Err(err) => corrupt_observation(sched, row, &err.to_string()),
    }
}

fn corrupt_observation(sched: &Arc<Sched>, row: &SessionRecord, reason: &str) -> Observation {
    tracing::warn!(
        task = %row.task_id,
        reason,
        "session row is corrupt; rebuilt the reducer watermark from its columns"
    );
    sched.note_alert(format!(
        "session {} observed_json is corrupt",
        short(&row.task_id)
    ));
    sched.emit(
        "lifecycle_corrupt",
        json!({"task_id": row.task_id, "reason": reason}),
    );
    Observation::build(
        Version::new(
            if row.generation > 0 {
                row.generation as u64
            } else {
                1
            },
            row.seq.max(0) as u64,
        ),
        true,
        DEFAULT_ISOLATE_AFTER,
        DEFAULT_TERMINATE_AFTER,
        row.mismatch_count.clamp(0, i64::from(u32::MAX)) as u32,
        AgentState::Booting,
        DeliveryState::None,
        ResourceState::Detached,
        RecoveryState::None,
        Outcome::Pending,
    )
}

/// The version an event carries. `lifecycle::event_version` is private, and the
/// persistence boundary needs it for the `Created` seed.
fn version_of(event: &LifecycleEvent) -> Version {
    match event {
        LifecycleEvent::Created { v }
        | LifecycleEvent::Ready { v }
        | LifecycleEvent::TurnStarted { v }
        | LifecycleEvent::TurnEnded { v }
        | LifecycleEvent::Heartbeat { v, .. }
        | LifecycleEvent::Complete { v }
        | LifecycleEvent::IntentPending { v }
        | LifecycleEvent::IntentRetry { v }
        | LifecycleEvent::IntentReceipt { v }
        | LifecycleEvent::IntentExhausted { v }
        | LifecycleEvent::ResourceAttach { v }
        | LifecycleEvent::ResourceCloseRequested { v }
        | LifecycleEvent::ResourceClosed { v }
        | LifecycleEvent::AgentGone { v }
        | LifecycleEvent::Cancel { v }
        | LifecycleEvent::Fail { v }
        | LifecycleEvent::ReconcileMismatch { v }
        | LifecycleEvent::ReconcileOk { v }
        | LifecycleEvent::AdoptNewGeneration { v }
        | LifecycleEvent::Supersede { v, .. } => *v,
    }
}

fn event_name(event: &LifecycleEvent) -> &'static str {
    match event {
        LifecycleEvent::Created { .. } => "created",
        LifecycleEvent::Ready { .. } => "ready",
        LifecycleEvent::TurnStarted { .. } => "turn_started",
        LifecycleEvent::TurnEnded { .. } => "turn_ended",
        LifecycleEvent::Heartbeat { .. } => "heartbeat",
        LifecycleEvent::Complete { .. } => "complete",
        LifecycleEvent::IntentPending { .. } => "intent_pending",
        LifecycleEvent::IntentRetry { .. } => "intent_retry",
        LifecycleEvent::IntentReceipt { .. } => "intent_receipt",
        LifecycleEvent::IntentExhausted { .. } => "intent_exhausted",
        LifecycleEvent::ResourceAttach { .. } => "resource_attach",
        LifecycleEvent::ResourceCloseRequested { .. } => "resource_close_requested",
        LifecycleEvent::ResourceClosed { .. } => "resource_closed",
        LifecycleEvent::AgentGone { .. } => "agent_gone",
        LifecycleEvent::Cancel { .. } => "cancel",
        LifecycleEvent::Fail { .. } => "fail",
        LifecycleEvent::ReconcileMismatch { .. } => "reconcile_mismatch",
        LifecycleEvent::ReconcileOk { .. } => "reconcile_ok",
        LifecycleEvent::AdoptNewGeneration { .. } => "adopt_new_generation",
        LifecycleEvent::Supersede { .. } => "supersede",
    }
}

/// Reduce `event` against the stored tuple for `task_id` and persist the result.
///
/// `Applied` writes the row (unless a concurrent writer already carries a newer
/// watermark) and emits one `lifecycle` event describing the transition.
/// `Ignored` and `Rejected` leave the ledger untouched and are logged with the
/// reducer's reason.
///
/// `Created` is the one event whose meaning at this boundary is the row itself:
/// `Observation::initial` already *is* the post-`Created` tuple, so for a task
/// with no stored row it seeds the row at the event's version. Use
/// [`feed_created`] for the idempotent form.
pub fn apply_persist(
    sched: &Arc<Sched>,
    task_id: &str,
    event: &LifecycleEvent,
) -> anyhow::Result<Verdict> {
    let row = sched.db.get_session(task_id)?;
    if row.is_none() && !session_is_known(sched, task_id)? {
        tracing::warn!(
            task = %task_id,
            event = event_name(event),
            "lifecycle event for a session the scheduler never tracked; nothing persisted"
        );
        anyhow::bail!("unknown session {task_id}");
    }
    let current = stored_observation(sched, row.as_ref());
    if row.is_none() && matches!(event, LifecycleEvent::Created { .. }) {
        return Ok(Verdict::Applied(seed_created(
            sched,
            task_id,
            version_of(event),
        )?));
    }
    let verdict = lifecycle::apply(&current, event);
    record_verdict(sched, task_id, row.as_ref(), event, &current, verdict)?;
    Ok(verdict)
}

/// A session row may only be created for something the scheduler itself tracked:
/// a task in the ledger, or a session still held in memory. That keeps a stray
/// `---swarm-report` from conjuring state for a task that never existed.
fn session_is_known(sched: &Arc<Sched>, task_id: &str) -> anyhow::Result<bool> {
    if sched.sessions.lock().unwrap().contains_key(task_id) {
        return Ok(true);
    }
    Ok(sched.db.get(task_id)?.is_some())
}

/// Write the `Created` seed row. The reducer defines no transition into
/// `Booting` from `Booting` — `initial()` is already the created tuple — so the
/// seed is written directly at the event's version.
fn seed_created(
    sched: &Arc<Sched>,
    task_id: &str,
    version: Version,
) -> anyhow::Result<Observation> {
    let mut seeded = initial_observation();
    seeded.version = version;
    let backend_ref = backend_ref_json(sched, task_id, None);
    let desired = serde_json::to_string(&LifecycleEvent::Created { v: version })?;
    let stored = to_versioned(&seeded, &backend_ref, &desired)?;
    if sched.db.upsert_session(task_id, &stored)? {
        sched.emit(
            "lifecycle",
            transition_payload(task_id, &seeded, &seeded, "created"),
        );
    } else {
        tracing::warn!(
            task = %task_id,
            "a newer session row appeared while seeding Created; kept the newer watermark"
        );
    }
    Ok(seeded)
}

/// One transition on the bus: both projections plus the full tuple the reducer
/// settled on, so a subscriber never has to re-derive `public`.
fn transition_payload(
    task_id: &str,
    from: &Observation,
    to: &Observation,
    event: &str,
) -> serde_json::Value {
    json!({
        "task_id": task_id,
        "from": from.public,
        "to": to.public,
        "agent": to.agent,
        "delivery": to.delivery,
        "resource": to.resource,
        "recovery": to.recovery,
        "outcome": to.outcome,
        "generation": to.version.generation,
        "seq": to.version.seq,
        "event": event,
    })
}

fn record_verdict(
    sched: &Arc<Sched>,
    task_id: &str,
    row: Option<&SessionRecord>,
    event: &LifecycleEvent,
    current: &Observation,
    verdict: Verdict,
) -> anyhow::Result<()> {
    match verdict {
        Verdict::Applied(ref next) => {
            let backend_ref = backend_ref_json(sched, task_id, row);
            let desired = serde_json::to_string(event)?;
            let stored = to_versioned(next, &backend_ref, &desired)?;
            if !sched.db.upsert_session(task_id, &stored)? {
                tracing::warn!(
                    task = %task_id,
                    generation = next.version.generation,
                    seq = next.version.seq,
                    "session write lost to a newer watermark; left the row alone"
                );
                return Ok(());
            }
            tracing::debug!(
                task = %task_id,
                from = ?current.public,
                to = ?next.public,
                generation = next.version.generation,
                seq = next.version.seq,
                "lifecycle transition applied"
            );
            sched.emit(
                "lifecycle",
                transition_payload(task_id, current, next, event_name(event)),
            );
        }
        Verdict::Ignored(reason) => {
            tracing::debug!(
                task = %task_id,
                reason = ?reason,
                event = event_name(event),
                generation = current.version.generation,
                seq = current.version.seq,
                "lifecycle event ignored; ledger left as-is"
            );
        }
        Verdict::Rejected(reason) => {
            tracing::warn!(
                task = %task_id,
                reason = ?reason,
                event = event_name(event),
                generation = current.version.generation,
                seq = current.version.seq,
                "lifecycle event rejected; ledger left as-is"
            );
        }
    }
    Ok(())
}

/// The next version for a session the scheduler observes locally: same
/// generation, one sequence past the stored watermark.
pub fn next_version(sched: &Arc<Sched>, task_id: &str) -> anyhow::Result<Version> {
    let row = sched.db.get_session(task_id)?;
    let current = stored_observation(sched, row.as_ref());
    Ok(Version::new(
        current.version.generation,
        current.version.seq.saturating_add(1),
    ))
}

/// Reduce an event whose version the scheduler allocates itself. Every feed
/// helper below goes through here, so an event that arrives twice — or twice
/// across a restart — is dropped by the reducer's watermark instead of running
/// its effect twice.
pub fn apply_at_next(
    sched: &Arc<Sched>,
    task_id: &str,
    make: impl FnOnce(Version) -> LifecycleEvent,
) -> anyhow::Result<Verdict> {
    let version = next_version(sched, task_id)?;
    apply_persist(sched, task_id, &make(version))
}

/// Best-effort feed for the scheduler's own observation points. A ledger write
/// failure is loud in the log and the next reconcile pass repairs the row; it
/// never changes what the caller already committed to the task ledger.
pub fn try_feed(sched: &Arc<Sched>, task_id: &str, make: impl FnOnce(Version) -> LifecycleEvent) {
    if let Err(err) = apply_at_next(sched, task_id, make) {
        tracing::warn!(
            task = %task_id,
            error = %err,
            "could not record a lifecycle event in the session ledger"
        );
    }
}

/// Seed the session row for a task the scheduler just gave a resource to.
/// Idempotent: a row that already exists keeps its own history, because the
/// reducer defines no transition that re-boots a live generation.
pub fn feed_created(sched: &Arc<Sched>, task_id: &str) -> anyhow::Result<Verdict> {
    if sched.db.get_session(task_id)?.is_some() {
        tracing::debug!(task = %task_id, "session row already exists; created is a no-op");
        return Ok(Verdict::Ignored(IgnoredReason::NoOp));
    }
    apply_at_next(sched, task_id, |v| LifecycleEvent::Created { v })
}

/// The dispatch path proved both facts at once: the session is bound to its task
/// and the backend resource is attached.
pub fn feed_dispatched(sched: &Arc<Sched>, task_id: &str) {
    if let Err(err) = feed_created(sched, task_id) {
        tracing::warn!(task = %task_id, error = %err, "could not seed the session row");
    }
    match feed_resource_attached(sched, task_id) {
        Ok(verdict) => {
            if matches!(verdict, Verdict::Rejected(_)) {
                tracing::warn!(task = %task_id, ?verdict, "resource attach refused for a dispatched session");
            }
        }
        Err(err) => {
            tracing::warn!(task = %task_id, error = %err, "could not record the resource attach")
        }
    }
}

/// The backend resource for this session exists.
pub fn feed_resource_attached(sched: &Arc<Sched>, task_id: &str) -> anyhow::Result<Verdict> {
    apply_at_next(sched, task_id, |v| LifecycleEvent::ResourceAttach { v })
}

/// The agent finished booting and its channel is reachable (`swarm_ready`).
pub fn feed_ready(sched: &Arc<Sched>, task_id: &str) -> anyhow::Result<Verdict> {
    apply_at_next(sched, task_id, |v| LifecycleEvent::Ready { v })
}

/// The current turn went from "may change state" to "may emit an artifact".
pub fn feed_turn_started(sched: &Arc<Sched>, task_id: &str) -> anyhow::Result<Verdict> {
    apply_at_next(sched, task_id, |v| LifecycleEvent::TurnStarted { v })
}

/// The current turn ended with no completion receipt yet.
pub fn feed_turn_ended(sched: &Arc<Sched>, task_id: &str) -> anyhow::Result<Verdict> {
    apply_at_next(sched, task_id, |v| LifecycleEvent::TurnEnded { v })
}

/// The tab was recycled: the resource is closed and the generation is over.
pub fn feed_resource_closed(sched: &Arc<Sched>, task_id: &str) -> anyhow::Result<Verdict> {
    apply_at_next(sched, task_id, |v| LifecycleEvent::ResourceClosed { v })
}

/// The agent process is gone while the ledger may still owe a result.
pub fn feed_agent_gone(sched: &Arc<Sched>, task_id: &str) -> anyhow::Result<Verdict> {
    apply_at_next(sched, task_id, |v| LifecycleEvent::AgentGone { v })
}

/// Work the scheduler gave up on: the outcome becomes failed while the
/// generation stays open, so a later resource close still finalizes the row.
pub fn feed_fail(sched: &Arc<Sched>, task_id: &str) -> anyhow::Result<Verdict> {
    apply_at_next(sched, task_id, |v| LifecycleEvent::Fail { v })
}

/// Work the operator cancelled: the result is settled, the exit proceeds.
pub fn feed_cancel(sched: &Arc<Sched>, task_id: &str) -> anyhow::Result<Verdict> {
    apply_at_next(sched, task_id, |v| LifecycleEvent::Cancel { v })
}

/// A reconcile fact disagreed with the tuple (probe, heartbeat, snapshot).
pub fn feed_mismatch(sched: &Arc<Sched>, task_id: &str) -> anyhow::Result<Verdict> {
    apply_at_next(sched, task_id, |v| LifecycleEvent::ReconcileMismatch { v })
}

/// A reconcile fact confirmed the tuple, which is what clears `idle_waiting` and
/// `idle_fault` without moving any other dimension.
pub fn feed_reconcile_ok(sched: &Arc<Sched>, task_id: &str) -> anyhow::Result<Verdict> {
    apply_at_next(sched, task_id, |v| LifecycleEvent::ReconcileOk { v })
}

/// The out handoff was consumed: the completion intent opened and its receipt
/// came back accepted. Then the ledger's `done` result is mirrored, which is
/// what lets the later resource close settle instead of contradicting itself.
pub fn feed_delivered(sched: &Arc<Sched>, task_id: &str) -> anyhow::Result<Verdict> {
    let written = apply_at_next(sched, task_id, |v| LifecycleEvent::Complete { v })?;
    if matches!(written, Verdict::Applied(_)) {
        apply_at_next(sched, task_id, |v| LifecycleEvent::IntentReceipt { v })?;
    }
    settle(sched, task_id, Outcome::Done)
}

/// Mirror a settled task result into the session tuple.
///
/// `Outcome` is the ledger's dimension and the reducer accepts it only inside a
/// full `Heartbeat` snapshot, so the scheduler reports the tuple it just proved
/// instead of editing projected columns behind the reducer's back. Legality
/// stays the reducer's decision; [`settle_body`] normalises the two edges that
/// would otherwise be refused outright: `Accepted` cannot sit on a booting
/// agent, and a `Done` result on an idle agent must be draining.
pub fn settle(sched: &Arc<Sched>, task_id: &str, outcome: Outcome) -> anyhow::Result<Verdict> {
    let row = sched.db.get_session(task_id)?;
    let current = stored_observation(sched, row.as_ref());
    let version = Version::new(
        current.version.generation,
        current.version.seq.saturating_add(1),
    );
    let body = settle_body(&current, outcome);
    apply_persist(
        sched,
        task_id,
        &LifecycleEvent::Heartbeat { v: version, body },
    )
}

fn settle_body(obs: &Observation, outcome: Outcome) -> Observation {
    let agent = if obs.agent == AgentState::Booting {
        AgentState::Idle
    } else {
        obs.agent
    };
    let delivery = if outcome == Outcome::Done {
        DeliveryState::Accepted
    } else {
        obs.delivery
    };
    let recovery = if outcome == Outcome::Done && agent == AgentState::Idle && obs.generation_live {
        RecoveryState::Draining
    } else {
        RecoveryState::None
    };
    Observation::build(
        obs.version,
        obs.generation_live,
        obs.isolate_after,
        obs.terminate_after,
        obs.mismatch_count,
        agent,
        delivery,
        obs.resource,
        recovery,
        outcome,
    )
}

/// Record one divergence that survived a crash. Deduplicated on
/// `(task_id, kind, generation)` so a repeated pass cannot bury the queue.
pub fn record_fault(
    sched: &Arc<Sched>,
    task_id: &str,
    kind: &str,
    intent: &str,
    reason: &str,
) -> anyhow::Result<FaultOutcome> {
    let row = sched.db.get_session(task_id)?;
    let generation = row.as_ref().map(|r| r.generation).unwrap_or(0);
    if sched
        .db
        .list_faults(Some(task_id))?
        .iter()
        .any(|f| f.kind == kind && f.generation == generation)
    {
        tracing::debug!(task = %task_id, kind, "this generation's fault is already in the queue");
        return Ok(FaultOutcome::default());
    }
    let attempt = sched
        .db
        .get(task_id)?
        .map(|task| i64::from(task.attempt))
        .unwrap_or(0);
    let keep = |pick: fn(&SessionRecord) -> String| {
        row.as_ref()
            .map(pick)
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| "{}".to_string())
    };
    let fault = FaultRecord {
        id: 0,
        task_id: task_id.to_string(),
        session_id: task_id.to_string(),
        generation,
        seq: row.as_ref().map(|r| r.seq).unwrap_or(0),
        desired_json: keep(|r| r.desired_json.clone()),
        observed_json: keep(|r| r.observed_json.clone()),
        intent: intent.to_string(),
        attempt,
        backend_ref: keep(|r| r.backend_ref.clone()),
        kind: kind.to_string(),
        reason: reason.to_string(),
        state: "open".to_string(),
        recovery_task_id: None,
        created_at: now_unix(),
    };
    let id = sched.db.insert_fault(&fault)?;
    let fault = FaultRecord { id, ..fault };
    tracing::warn!(task = %task_id, fault_id = id, kind, intent, reason, "recorded session fault");
    sched.note_alert(format!("session fault {kind} on {}", short(task_id)));
    sched.emit(
        "session_fault",
        json!({
            "fault_id": id,
            "task_id": task_id,
            "kind": kind,
            "intent": intent,
            "reason": reason,
            "generation": generation,
        }),
    );
    let recovery_task_id = match open_recovery_task(sched, &fault) {
        Ok(task_id) => task_id,
        Err(err) => {
            // The fault row is already durable, so a failed recovery write
            // stays visible: the queue holds it and `repair retry` can open it
            // later. Loud here, silent never.
            sched.note_alert(format!(
                "recovery for fault {id} on {} failed: {err}",
                short(task_id)
            ));
            tracing::warn!(task = %task_id, fault_id = id, error = %err, "recovery task could not be opened");
            None
        }
    };
    Ok(FaultOutcome {
        fault_id: Some(id),
        recovery_task_id,
    })
}

/// What recording one fault produced: the queue row and, when the lost work
/// still has an owner, the recovery task opened for it.
#[derive(Debug, Clone, Default, Serialize)]
pub struct FaultOutcome {
    /// `None` when the dedupe gate found this generation already queued.
    pub fault_id: Option<i64>,
    /// `None` when the fault needed no recovery task, or the single-layer gate
    /// left it in the root fault queue.
    pub recovery_task_id: Option<String>,
}

impl FaultOutcome {
    fn recorded(&self) -> bool {
        self.fault_id.is_some()
    }
}

/// Fault kinds whose lost work still has somebody waiting for a result. A
/// protocol parse fault and an operator's own `repair fail` sit outside this
/// list: nothing there is work a fresh session could hand back.
pub const RECOVERY_FAULT_KINDS: &[&str] = &[
    "probe_dead",
    "mismatch_terminate",
    "agent_reported",
    "intent_exhausted",
];

/// Give a fault an owner. The recovery task is a normal task — `sched`'s own
/// dispatch path runs it and the task ledger stays the single source of truth —
/// aimed at the faulted task's direct parent role, so the supervisor that owns
/// the lost hop is the one told about it.
///
/// Two gates keep the tree flat: only the kinds in [`RECOVERY_FAULT_KINDS`]
/// recover, and a task that is itself a recovery never spawns another one. Its
/// fault stays `open` in the root queue for an operator.
pub fn open_recovery_task(
    sched: &Arc<Sched>,
    fault: &FaultRecord,
) -> anyhow::Result<Option<String>> {
    if !RECOVERY_FAULT_KINDS.contains(&fault.kind.as_str()) {
        return Ok(None);
    }
    let Some(failed) = sched.db.get(&fault.task_id)? else {
        tracing::debug!(task = %fault.task_id, "fault names no scheduled task; nothing to recover");
        return Ok(None);
    };
    if outcome_of_task_state(failed.state).is_some() {
        tracing::debug!(
            task = %fault.task_id,
            state = failed.state.as_str(),
            "the ledger already settled this task's result; recovery would repeat finished work"
        );
        return Ok(None);
    }
    if failed.kind == "recovery" || failed.failure_of.is_some() {
        let origin = failed
            .failure_of
            .clone()
            .unwrap_or_else(|| "an older fault".to_string());
        sched.note_alert(format!(
            "recovery for fault {} stays in the root queue: {} already recovers fault {origin}",
            fault.id,
            short(&fault.task_id)
        ));
        sched.emit(
            "recovery_suppressed",
            json!({
                "fault_id": fault.id,
                "task_id": fault.task_id,
                "kind": fault.kind,
                "origin_fault_id": origin,
                "reason": "single layer: a recovery task's own fault stays with the root supervisor",
            }),
        );
        return Ok(None);
    }
    let to = parent_role(sched, &failed)?;
    let recovery_id = uuid::Uuid::new_v4().to_string();
    let payload = recovery_payload(fault, &failed);
    if !sched
        .db
        .insert_task(&recovery_id, &failed.to_ws, &to, "", 1, &payload)?
    {
        anyhow::bail!("recovery task {recovery_id} collided with an existing row");
    }
    sched.db.link_fault_recovery(fault.id, &recovery_id)?;
    tracing::warn!(
        fault_id = fault.id,
        task = %fault.task_id,
        recovery_id = %short(&recovery_id),
        to = %to,
        "opened a recovery task for a fault"
    );
    sched.emit(
        "recovery_created",
        json!({
            "fault_id": fault.id,
            "failed_task_id": fault.task_id,
            "recovery_task_id": recovery_id,
            "to": to,
            "kind": fault.kind,
            "reason": fault.reason,
        }),
    );
    // The task row and the fault link are durable before the send, so a spawn
    // that dies here leaves an undispatched recovery task that `repair retry`
    // can pick up, instead of a fault with no owner at all.
    if let Err(err) = sched::dispatch_public(sched, &recovery_id, &to) {
        sched.note_alert(format!(
            "recovery task {} awaits repair retry: {err}",
            short(&recovery_id)
        ));
        tracing::warn!(recovery_id = %short(&recovery_id), error = %err, "recovery dispatch failed; the task row stays pending");
    }
    Ok(Some(recovery_id))
}

/// The direct parent role: the upstream task's own target when this task was
/// handed off, otherwise whoever submitted it, otherwise the root supervisor.
fn parent_role(sched: &Arc<Sched>, failed: &TaskRow) -> anyhow::Result<String> {
    if !failed.transfer_send_to.is_empty() {
        if let Some(upstream) = sched.db.get(&failed.transfer_send_to)? {
            if !upstream.to_ws.is_empty() {
                return Ok(upstream.to_ws);
            }
        }
    }
    if !failed.from_ws.is_empty() {
        return Ok(failed.from_ws.clone());
    }
    Ok(".".to_string())
}

/// The report a parent role reads before it decides anything. Every field that
/// identifies the lost generation is here, and the tuple JSON stays verbatim so
/// a repair can be argued from this payload alone.
fn recovery_payload(fault: &FaultRecord, failed: &TaskRow) -> String {
    let lines = vec![
        format!("swarm recovery for fault {}", fault.id),
        format!("kind: {}", fault.kind),
        format!("failed_task: {}", fault.task_id),
        format!("role: {}", failed.to_ws),
        format!("generation: {}", fault.generation),
        format!("seq: {}", fault.seq),
        format!("intent: {}", fault.intent),
        format!("attempt: {}", fault.attempt),
        format!("backend: {}", fault.backend_ref.replace('\n', " ")),
        format!("desired: {}", fault.desired_json),
        format!("observed: {}", fault.observed_json),
        format!("reason: {}", fault.reason),
        String::new(),
        format!(
            "The task above lost its generation while {} was owed. Decide one outcome and report it on this task: re-dispatch the work to a fresh session, mark it failed with a reason, or fold it into another hop. The lost session's own work stays unproven.",
            failed.state.as_str()
        ),
    ];
    lines.join("\n")
}

// --- durable intents: at-least-once for the downlink actions this daemon owns --
//
// The rule from `docs/SWARM-REFACTOR-GRILLME.md` §2.4 is persistence before
// confirmation: write the `intents` row, then send, then write the receipt. A
// crash between the write and the send leaves a `pending` row that
// [`pump_intents`] re-attempts on the next tick; a crash between the send and
// the receipt re-sends, and the daemon's own `op_id` idempotency key (derived
// from the wire text in `ipc::send_loopback_rpc`) absorbs the duplicate.
//
// The reducer's `delivery` dimension is the mirror of the same line of work:
//
// | moment                                | reducer event              |
// |---------------------------------------|----------------------------|
// | intent persisted                      | `IntentPending`            |
// | send failed, attempts remain          | `IntentRetry`              |
// | receipt persisted                     | `IntentReceipt`            |
// | attempts spent                        | `IntentExhausted`          |
//
// The four [`feed_intent_*`] helpers below are that mapping, and they are the
// only path from an intent row into a session tuple.

/// Ask a session to recycle its own tab. The only intent kind this daemon sends.
pub const INTENT_RECYCLE: &str = "recycle";

/// Attempts granted to one intent before it lands in the fault queue. The spec
/// default backoff for those attempts is 1s, 2s, 4s.
pub const DEFAULT_INTENT_MAX_ATTEMPTS: i64 = 3;

/// Intents one pump tick will attempt, so a wedged daemon cannot starve the
/// reap loop that drives it.
pub const MAX_INTENTS_PER_TICK: usize = 16;

/// What one pump tick did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct IntentCounts {
    /// Intents claimed and sent at least once.
    pub attempted: usize,
    /// Receipts written.
    pub delivered: usize,
    /// Sends that failed with attempts still in hand.
    pub retried: usize,
    /// Sends that spent the last attempt.
    pub exhausted: usize,
    /// Rows this tick left for a later tick.
    pub deferred: usize,
}

impl IntentCounts {
    fn any(&self) -> bool {
        self.attempted > 0
    }
}

/// The wait after a failed attempt: the spec's 1s, 2s, 4s ladder. Only the
/// first [`DEFAULT_INTENT_MAX_ATTEMPTS`] slots matter; the clamp keeps a
/// hand-inserted row with a large attempt count from scheduling a send past the
/// heat death of the daemon.
fn backoff_secs(attempt: i64) -> i64 {
    1i64 << (attempt.clamp(1, 5) - 1)
}

/// One intent line per task per kind per generation: a second recycle request
/// inside the same generation is the same request, and a new Pi generation
/// starts a fresh line of work.
fn intent_op_id(kind: &str, task_id: &str, generation: i64) -> String {
    format!("{kind}:{task_id}:g{generation}")
}

/// The role a task's session is running in, `"."` when the ledger is thin.
pub fn task_role(sched: &Arc<Sched>, task_id: &str) -> String {
    let role = sched
        .db
        .get(task_id)
        .ok()
        .flatten()
        .map(|task| task.to_ws)
        .unwrap_or_default();
    if role.is_empty() {
        ".".to_string()
    } else {
        role
    }
}

/// Open an intent and attempt it once inline, so the common case keeps the
/// latency the scheduler expects while the row makes the send durable.
///
/// Returns the op id when this call owns a live attempt. An op id that already
/// exists is left alone: `pending`/`claimed` rows belong to whoever wrote them
/// and [`super::repair`] re-arms a spent row explicitly.
pub fn open_intent(
    sched: &Arc<Sched>,
    task_id: &str,
    kind: &str,
    payload: &serde_json::Value,
) -> anyhow::Result<Option<String>> {
    let now = now_unix();
    let generation = sched
        .db
        .get_session(task_id)?
        .map(|row| row.generation)
        .unwrap_or(0);
    let op_id = intent_op_id(kind, task_id, generation);
    if sched.db.get_intent(&op_id)?.is_some() {
        tracing::debug!(task = %task_id, op_id = %op_id, "intent line already open");
        return Ok(None);
    }
    sched
        .db
        .insert_intent(&op_id, task_id, kind, payload, now, now)?;
    feed_intent_pending(sched, task_id);
    sched.emit(
        "intent",
        json!({"event": "pending", "op_id": op_id, "task_id": task_id, "kind": kind}),
    );
    match sched.db.claim_intent_op(&op_id, now) {
        Ok(Some(intent)) => {
            run_intent(sched, &intent, now);
            Ok(Some(op_id))
        }
        // Another worker claimed it between the insert and the claim; the pump
        // owns the line from here.
        Ok(None) => Ok(None),
        Err(err) => {
            tracing::warn!(op_id = %op_id, error = %err, "intent claim failed; the pump will retry");
            Ok(None)
        }
    }
}

/// `recycle` with the wire built here. Called by `sched::signal_recycle` so the
/// control frame survives a crash between decision and send.
pub fn request_recycle(sched: &Arc<Sched>, task_id: &str, reason: &str) -> Option<String> {
    let wire = proto::render_ctl(task_id, reason);
    let payload = json!({
        "workspace": task_role(sched, task_id),
        "wire": wire,
        "reason": reason,
    });
    match open_intent(sched, task_id, INTENT_RECYCLE, &payload) {
        Ok(op_id) => op_id,
        Err(err) => {
            // The intent row itself is what makes this recoverable, so a failure
            // to write it is reported the way the old inline send was.
            sched.note_alert(format!("loopback control failed: {task_id}: {err}"));
            tracing::warn!(task = %task_id, error = %err, "loopback control failed");
            None
        }
    }
}

/// Claim and send every intent that is due, oldest first. Driven once per
/// reconcile pass, which the reap loop runs every tick.
pub fn pump_intents(sched: &Arc<Sched>, now: i64) -> IntentCounts {
    let mut counts = IntentCounts::default();
    loop {
        if counts.attempted >= MAX_INTENTS_PER_TICK
            || sched.shutdown.load(std::sync::atomic::Ordering::SeqCst)
        {
            match sched.db.list_intents(Some("pending")) {
                Ok(left) => counts.deferred = left.len(),
                Err(err) => tracing::warn!(error = %err, "intent backlog unread"),
            }
            break;
        }
        let claimed = match sched.db.claim_intent(now) {
            Ok(Some(intent)) => intent,
            Ok(None) => break,
            Err(err) => {
                tracing::warn!(error = %err, "intent claim failed");
                break;
            }
        };
        counts.attempted += 1;
        match run_intent(sched, &claimed, now) {
            IntentMoment::Receipt => counts.delivered += 1,
            IntentMoment::Retry => counts.retried += 1,
            IntentMoment::Exhausted => counts.exhausted += 1,
        }
    }
    counts
}

/// The moment an attempt ended at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IntentMoment {
    Receipt,
    Retry,
    Exhausted,
}

/// Send one already-claimed intent and record where it ended. Every bookkeeping
/// write happens before the matching event is emitted, so a subscriber can never
/// observe a receipt that the ledger does not hold.
///
/// `now` is the caller's clock — the pump's tick or the moment the producer
/// persisted the row. Reading the wall clock here instead would schedule the
/// next backoff slot against a different clock than the one that decided the
/// attempt was due, and the ladder would drift by up to a whole second.
fn run_intent(sched: &Arc<Sched>, intent: &IntentRecord, now: i64) -> IntentMoment {
    let payload: serde_json::Value =
        serde_json::from_str(&intent.payload_json).unwrap_or_else(|_| serde_json::json!({}));
    let moment = match deliver_intent(sched, intent, &payload) {
        Ok(data) => {
            let receipt = json!({"at": now, "response": data});
            match sched.db.receipt_intent(&intent.op_id, &receipt, now) {
                Ok(_) => {
                    feed_intent_receipt(sched, &intent.task_id);
                    tracing::info!(op_id = %intent.op_id, attempt = intent.attempt, "intent delivered");
                    IntentMoment::Receipt
                }
                Err(err) => {
                    // The send landed and the receipt did not: retrying is the
                    // at-least-once answer, and the daemon dedupes the resend.
                    tracing::warn!(op_id = %intent.op_id, error = %err, "intent receipt unwritable");
                    IntentMoment::Retry
                }
            }
        }
        Err(err) if intent.attempt >= DEFAULT_INTENT_MAX_ATTEMPTS => {
            let reason = err.to_string();
            if let Err(write) = sched.db.exhaust_intent(&intent.op_id, &reason, now) {
                tracing::warn!(op_id = %intent.op_id, error = %write, "intent exhaustion unwritable");
            }
            feed_intent_exhausted(sched, &intent.task_id);
            tracing::warn!(op_id = %intent.op_id, error = %reason, "intent exhausted");
            let intent_line = format!("intent:{}", intent.kind);
            match record_fault(
                sched,
                &intent.task_id,
                "intent_exhausted",
                &intent_line,
                &format!("{}: {reason}", intent.op_id),
            ) {
                Ok(outcome) => {
                    if let Some(fault_id) = outcome.fault_id {
                        sched.emit(
                            "intent",
                            json!({
                                "event": "fault_queued",
                                "op_id": intent.op_id,
                                "task_id": intent.task_id,
                                "kind": intent.kind,
                                "fault_id": fault_id,
                                "recovery_task_id": outcome.recovery_task_id,
                            }),
                        );
                    }
                }
                Err(err) => {
                    sched.note_alert(format!(
                        "intent fault queue write failed: {}: {err}",
                        intent.op_id
                    ));
                    tracing::warn!(op_id = %intent.op_id, error = %err, "intent fault unwritable");
                }
            }
            IntentMoment::Exhausted
        }
        Err(err) => {
            let reason = err.to_string();
            let next = now + backoff_secs(intent.attempt);
            if let Err(write) = sched.db.retry_intent(&intent.op_id, next, &reason, now) {
                tracing::warn!(op_id = %intent.op_id, error = %write, "intent retry unwritable");
            }
            feed_intent_retry(sched, &intent.task_id);
            tracing::debug!(op_id = %intent.op_id, attempt = intent.attempt, error = %reason, "intent retry scheduled");
            IntentMoment::Retry
        }
    };
    let event = match moment {
        IntentMoment::Receipt => "receipt",
        IntentMoment::Retry => "retry",
        IntentMoment::Exhausted => "exhausted",
    };
    sched.emit(
        "intent",
        json!({
            "event": event,
            "op_id": intent.op_id,
            "task_id": intent.task_id,
            "kind": intent.kind,
            "attempt": intent.attempt,
        }),
    );
    moment
}

/// Push one intent's wire through the workspace daemon's loopback channel.
///
/// Boundary note, kept explicit because it is the one intent family this daemon
/// does *not* send: `complete`, `send`, and `fault_report` are written by the
/// agent-side plugin (`harness/pi-onlyne`), which owns the Pi turn and its
/// `swarm_complete` exit. This daemon observes their arrival — an `out` channel
/// message reaches [`feed_delivered`], a `---swarm-report` frame reaches
/// [`apply_report`] — and never produces them, so no protocol for a Pi-side
/// retry is invented here. An unknown kind fails its attempt loudly.
fn deliver_intent(
    sched: &Arc<Sched>,
    intent: &IntentRecord,
    payload: &serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    match intent.kind.as_str() {
        INTENT_RECYCLE => {
            let workspace = payload
                .get("workspace")
                .and_then(|v| v.as_str())
                .unwrap_or(".");
            let wire = payload
                .get("wire")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("recycle intent {} has no wire", intent.op_id))?;
            let ws = crate::root::resolve_instance(&sched.root, workspace);
            crate::ipc::send_loopback_rpc(sched, &intent.task_id, &ws, wire)
        }
        other => anyhow::bail!(
            "intent kind {other:?} has no sender here; the Pi-side kinds are complete/send/fault_report, owned by harness/pi-onlyne"
        ),
    }
}

/// The tuple only moves for a session this daemon actually tracks: a ledger
/// `IntentPending` on an unknown task would seed a phantom generation.
fn feed_intent(sched: &Arc<Sched>, task_id: &str, make: impl FnOnce(Version) -> LifecycleEvent) {
    match sched.db.get_session(task_id) {
        Ok(Some(_)) => try_feed(sched, task_id, make),
        Ok(None) => {
            tracing::debug!(task = %task_id, "intent event for an untracked session; delivery stays with the intent row")
        }
        Err(err) => {
            tracing::warn!(task = %task_id, error = %err, "intent event skipped over an unreadable session row")
        }
    }
}

/// The reducer mapping for the table above: an open intent is a pending exit.
pub fn feed_intent_pending(sched: &Arc<Sched>, task_id: &str) {
    feed_intent(sched, task_id, |v| LifecycleEvent::IntentPending { v });
}

/// A failed send with attempts remaining.
pub fn feed_intent_retry(sched: &Arc<Sched>, task_id: &str) {
    feed_intent(sched, task_id, |v| LifecycleEvent::IntentRetry { v });
}

/// The receipt is durable; the exit line is answered.
pub fn feed_intent_receipt(sched: &Arc<Sched>, task_id: &str) {
    feed_intent(sched, task_id, |v| LifecycleEvent::IntentReceipt { v });
}

/// The line is spent. The session goes to `RecoveryState::Failed` so the
/// supervisor's fault queue and the tuple agree.
pub fn feed_intent_exhausted(sched: &Arc<Sched>, task_id: &str) {
    feed_intent(sched, task_id, |v| LifecycleEvent::IntentExhausted { v });
}

/// What one reconcile pass touched. Emitted with every pass that did something,
/// so the TUI and `swarm status` can see reconciliation running.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ReconcileSummary {
    /// Session rows read.
    pub scanned: usize,
    /// Rows still open (public projection short of `exited`).
    pub open: usize,
    /// Probes that confirmed the tuple.
    pub ok: usize,
    /// Probes that proved the resource gone.
    pub dead: usize,
    /// Probes that proved nothing: stub handle, no reference, transport error.
    pub unknown: usize,
    /// Facts that disagreed with the stored tuple.
    pub mismatch: usize,
    /// Transitions written to the ledger.
    pub applied: usize,
    /// Events dropped by the reducer's watermark or as no-ops.
    pub ignored: usize,
    /// Events the reducer refused.
    pub rejected: usize,
    /// Fault rows created.
    pub faults: usize,
    /// Recovery tasks opened by this pass for the faults it recorded.
    pub recoveries: usize,
    /// Durable intents this pass sent at least once.
    pub intents: usize,
    /// Intents this pass receipted.
    pub intent_receipts: usize,
    /// Intents this pass spent its last attempt on.
    pub intent_exhausted: usize,
    /// Intents still waiting for a later tick when this pass ended.
    pub intents_deferred: usize,
    /// Rows this pass could not process.
    pub errors: usize,
}

impl ReconcileSummary {
    fn touched(&self) -> bool {
        self.ok
            + self.dead
            + self.unknown
            + self.mismatch
            + self.applied
            + self.faults
            + self.recoveries
            + self.intents
            + self.intent_receipts
            + self.intent_exhausted
            > 0
    }
}

/// Per-row counters, merged into a [`ReconcileSummary`].
#[derive(Debug, Default, Clone, Copy)]
struct RowCounts {
    ok: usize,
    dead: usize,
    unknown: usize,
    mismatch: usize,
    applied: usize,
    ignored: usize,
    rejected: usize,
    faults: usize,
    recoveries: usize,
}

impl ReconcileSummary {
    fn merge(&mut self, counts: RowCounts) {
        self.ok += counts.ok;
        self.dead += counts.dead;
        self.unknown += counts.unknown;
        self.mismatch += counts.mismatch;
        self.applied += counts.applied;
        self.ignored += counts.ignored;
        self.rejected += counts.rejected;
        self.faults += counts.faults;
        self.recoveries += counts.recoveries;
    }
}

fn count_verdict(counts: &mut RowCounts, verdict: &Verdict) {
    match verdict {
        Verdict::Applied(_) => counts.applied += 1,
        Verdict::Ignored(_) => counts.ignored += 1,
        Verdict::Rejected(_) => counts.rejected += 1,
    }
}

/// Reconcile every open session once, before the scheduler starts making new
/// decisions. `ipc::serve` calls this ahead of `reap_previous_run`, so an
/// orphaned session row meets the live backend while the task ledger is still
/// deciding what to adopt: a row whose resource is alive becomes
/// `ReconcileOk` — the same verdict adoption will reach — and a row whose
/// resource is gone fails its unsettled work and lands in `faults`.
pub fn startup_reconcile(sched: &Arc<Sched>) -> ReconcileSummary {
    let summary = reconcile_pass(sched, Phase::Startup);
    tracing::info!(?summary, "startup session reconcile finished");
    summary
}

/// Reconcile every open session against the live backend and the task ledger.
pub fn periodic_reconcile(sched: &Arc<Sched>) -> ReconcileSummary {
    reconcile_pass(sched, Phase::Periodic)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Startup,
    Periodic,
}

impl Phase {
    fn label(self) -> &'static str {
        match self {
            Phase::Startup => "startup",
            Phase::Periodic => "periodic",
        }
    }
}

fn reconcile_pass(sched: &Arc<Sched>, phase: Phase) -> ReconcileSummary {
    let mut summary = ReconcileSummary::default();
    let rows = match sched.db.list_sessions() {
        Ok(rows) => rows,
        Err(err) => {
            let reason = err.to_string();
            tracing::warn!(error = %reason, "reconcile could not read the session ledger");
            sched.note_alert(format!("session reconcile read failed: {reason}"));
            summary.errors += 1;
            return summary;
        }
    };
    for row in rows {
        if sched.shutdown.load(std::sync::atomic::Ordering::SeqCst) {
            tracing::info!(phase = phase.label(), "reconcile pass stopped by shutdown");
            break;
        }
        summary.scanned += 1;
        let stored = stored_observation(sched, Some(&row));
        if stored.public == PublicLifecycle::Exited {
            // A terminal tuple is history. Only the repair CLI may reopen it;
            // reconcile never resurrects a closed generation.
            continue;
        }
        summary.open += 1;
        match reconcile_session(sched, &row, phase) {
            Ok(counts) => summary.merge(counts),
            Err(err) => {
                summary.errors += 1;
                tracing::warn!(task = %row.task_id, error = %err, "reconcile could not process a session row");
            }
        }
    }
    // The session walk above can open intents (an exhausted repair, a recovery
    // dispatch) and `open_intent` attempts inline, so the pump below only sees
    // what a send left behind. Running it last keeps a retry from racing the
    // probe that scheduled it.
    let intents = pump_intents(sched, now_unix());
    summary.intents = intents.attempted;
    summary.intent_receipts = intents.delivered;
    summary.intent_exhausted = intents.exhausted;
    summary.intents_deferred = intents.deferred;
    if summary.touched() || summary.errors > 0 || intents.any() {
        sched.emit(
            "reconcile_pass",
            json!({"phase": phase.label(), "summary": summary}),
        );
    }
    summary
}

fn reconcile_session(
    sched: &Arc<Sched>,
    row: &SessionRecord,
    phase: Phase,
) -> anyhow::Result<RowCounts> {
    let mut counts = RowCounts::default();
    let task_id = row.task_id.clone();
    let mut current = stored_observation(sched, Some(row));
    // The task ledger owns the result. A crash between its write and this row's
    // leaves an open tuple on a finished task, so mirror the result first: the
    // probe below must never relabel a delivered hop as a lost generation.
    if current.outcome == Outcome::Pending {
        if let Some(task) = sched.db.get(&task_id)? {
            if let Some(outcome) = outcome_of_task_state(task.state) {
                let verdict = settle(sched, &task_id, outcome)?;
                count_verdict(&mut counts, &verdict);
                tracing::debug!(
                    task = %task_id,
                    state = task.state.as_str(),
                    verdict = ?verdict,
                    "mirrored the terminal task state into the session tuple"
                );
                let refreshed = sched.db.get_session(&task_id)?;
                current = stored_observation(sched, refreshed.as_ref());
            }
        }
    }
    if current.public == PublicLifecycle::Exited {
        return Ok(counts);
    }
    let Some(session) = probe_target(sched, &task_id, row) else {
        counts.unknown += 1;
        tracing::debug!(
            task = %task_id,
            phase = phase.label(),
            "session row names no backend resource; left the tuple alone"
        );
        return Ok(counts);
    };
    let handle = sched::session_display_handle(&session);
    let backend = match sched.require_backend() {
        Ok(backend) => backend,
        Err(err) => {
            counts.unknown += 1;
            tracing::warn!(task = %task_id, error = %err, "reconcile has no session backend; left the tuple alone");
            return Ok(counts);
        }
    };
    let probe = match backend.probe(&session) {
        Ok(probe) => probe,
        // A transport error says nothing about the agent, so the tuple stays
        // exactly where it is — the verdict `session_alive` reaches for free.
        Err(err) => {
            counts.unknown += 1;
            tracing::warn!(
                task = %task_id,
                handle = %handle,
                error = %err,
                "session probe was inconclusive; not judging the generation dead"
            );
            return Ok(counts);
        }
    };
    let detail = probe_detail(&probe);
    if !probe.alive {
        counts.dead += 1;
        reconcile_dead(sched, &task_id, &current, &detail, &mut counts)?;
        return Ok(counts);
    }
    if !current.generation_live || current.agent == AgentState::Gone {
        counts.mismatch += 1;
        reconcile_mismatch(sched, &task_id, &current, &detail, &mut counts)?;
        return Ok(counts);
    }
    if current.resource == ResourceState::Attached && !probe.attached {
        counts.mismatch += 1;
        reconcile_mismatch(sched, &task_id, &current, &detail, &mut counts)?;
        return Ok(counts);
    }
    if current.resource == ResourceState::Detached {
        let verdict = feed_resource_attached(sched, &task_id)?;
        count_verdict(&mut counts, &verdict);
    }
    counts.ok += 1;
    if current.mismatch_count > 0 || current.recovery != RecoveryState::None {
        let verdict = feed_reconcile_ok(sched, &task_id)?;
        count_verdict(&mut counts, &verdict);
    }
    Ok(counts)
}

pub(crate) fn outcome_of_task_state(state: TaskState) -> Option<Outcome> {
    match state {
        TaskState::Done | TaskState::Closed => Some(Outcome::Done),
        TaskState::Failed => Some(Outcome::Failed),
        TaskState::Cancelled => Some(Outcome::Cancelled),
        TaskState::Pending | TaskState::Running => None,
    }
}

fn probe_detail(probe: &ResourceProbe) -> String {
    match &probe.detail {
        Some(value) => value.to_string(),
        None => format!("alive={} attached={}", probe.alive, probe.attached),
    }
}

/// The resource is provably gone. Unsettled work fails first, then the
/// generation ends: `Fail` keeps the recovery dimensions intact and
/// `AgentGone` closes the generation.
fn reconcile_dead(
    sched: &Arc<Sched>,
    task_id: &str,
    obs: &Observation,
    detail: &str,
    counts: &mut RowCounts,
) -> anyhow::Result<()> {
    let reason = format!("backend resource gone: {detail}");
    tracing::warn!(task = %task_id, %reason, "open session lost its agent");
    let owed = obs.outcome == Outcome::Pending;
    // The task ledger already accounts for a hop it failed on purpose (dispatch
    // error, timeout, a handoff that declared `hop-failed`). A fault row is for
    // the divergence the ledger cannot describe: work still owed when the
    // generation ended. Record it BEFORE the tuple flips to gone, so the queue
    // shows the operator what was actually running when the resource vanished.
    let settled = sched
        .db
        .get(task_id)?
        .map(|task| {
            matches!(
                task.state,
                TaskState::Done | TaskState::Failed | TaskState::Cancelled | TaskState::Closed
            )
        })
        .unwrap_or(false);
    if owed && !settled {
        let outcome = record_fault(
            sched,
            task_id,
            "probe_dead",
            "reconcile:probe_dead",
            &reason,
        )?;
        if outcome.recorded() {
            counts.faults += 1;
        }
        if outcome.recovery_task_id.is_some() {
            counts.recoveries += 1;
        }
    }
    if owed {
        let verdict = feed_fail(sched, task_id)?;
        count_verdict(counts, &verdict);
    }
    let verdict = feed_agent_gone(sched, task_id)?;
    count_verdict(counts, &verdict);
    Ok(())
}

/// The resource is alive and disagrees with the tuple. The reducer's own ladder
/// decides when to isolate and when to end the generation.
fn reconcile_mismatch(
    sched: &Arc<Sched>,
    task_id: &str,
    obs: &Observation,
    detail: &str,
    counts: &mut RowCounts,
) -> anyhow::Result<()> {
    let reason = format!("backend disagrees with the session tuple: {detail}");
    tracing::warn!(
        task = %task_id,
        mismatch_count = obs.mismatch_count,
        isolate_after = obs.isolate_after,
        terminate_after = obs.terminate_after,
        %reason,
        "session reconcile mismatch"
    );
    let verdict = feed_mismatch(sched, task_id)?;
    count_verdict(counts, &verdict);
    let at_the_limit = obs.mismatch_count.saturating_add(1) >= obs.terminate_after;
    let terminated = matches!(&verdict, Verdict::Applied(next)
        if next.public == PublicLifecycle::Exited && next.outcome == Outcome::Failed);
    if at_the_limit && terminated {
        let outcome = record_fault(
            sched,
            task_id,
            "mismatch_terminate",
            "reconcile:mismatch",
            &reason,
        )?;
        if outcome.recorded() {
            counts.faults += 1;
        }
        if outcome.recovery_task_id.is_some() {
            counts.recoveries += 1;
        }
    }
    Ok(())
}

/// The backend resource the stored tuple names, in decreasing freshness: the
/// live in-memory session, then the row's own `backend_ref` (a whole
/// `SessionRef`, or the `{"handle": ...}` shape), then the terminal binding the
/// scheduler or the task row still holds. A JSON blob is never used as a
/// terminal id, and a stub or empty handle stays unknown-alive.
pub(crate) fn probe_target(
    sched: &Arc<Sched>,
    task_id: &str,
    row: &SessionRecord,
) -> Option<SessionRef> {
    if let Some(session) = sched.sessions.lock().unwrap().get(task_id) {
        return Some(session.clone());
    }
    if let Ok(session) = serde_json::from_str::<SessionRef>(&row.backend_ref) {
        if session.task_id == task_id {
            return Some(session);
        }
        tracing::warn!(
            task = %task_id,
            stored = %session.task_id,
            "session backend_ref names another task; refusing to probe it"
        );
        return None;
    }
    let handle = json_handle(&row.backend_ref)
        .or_else(|| plain_handle(&row.backend_ref))
        .or_else(|| sched.terminals.lock().unwrap().get(task_id).cloned())
        .or_else(|| {
            sched
                .db
                .get(task_id)
                .ok()
                .flatten()
                .and_then(|task| plain_handle(&task.terminal))
        })?;
    if handle.trim().is_empty() || handle.starts_with("stub-") {
        return None;
    }
    sched::session_for_handle(sched, task_id, &handle)
}

fn json_handle(text: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let handle = value.get("handle")?.as_str()?;
    (!handle.trim().is_empty()).then(|| handle.to_string())
}

fn plain_handle(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.starts_with('{') || trimmed.starts_with('[') {
        return None;
    }
    Some(trimmed.to_string())
}

/// Reduce a `---swarm-report` frame through the same gate as an internal
/// observation. The report carries the agent's own `(generation, seq)`, so a
/// frame that arrives twice, or late across a restart, is dropped by the
/// watermark instead of re-running its effect.
/// Reduce one `---swarm-report` frame from the agent plugin.
///
/// `heartbeat` and `snapshot` carry no body: they are liveness evidence, so they
/// fold into `ReconcileOk`, which heals a mismatch counter and otherwise reduces
/// to `Ignored(NoOp)` — a report that agrees with the stored tuple deliberately
/// leaves `seq` alone. That is the intended shape, and it is why a Pi heartbeat
/// cannot starve the internal ladder for watermark slots.
pub fn apply_report(sched: &Arc<Sched>, report: &LifecycleReport) -> anyhow::Result<Verdict> {
    let version = Version::new(report.generation, report.seq);
    let event = match report.kind {
        LifecycleKind::Ready => LifecycleEvent::Ready { v: version },
        LifecycleKind::TurnStarted => LifecycleEvent::TurnStarted { v: version },
        LifecycleKind::Complete => LifecycleEvent::Complete { v: version },
        LifecycleKind::Fault => LifecycleEvent::Fail { v: version },
        // The report wire carries no snapshot body yet, so a heartbeat or a
        // snapshot is exactly the liveness evidence `ReconcileOk` means: it
        // clears `idle_waiting`/`idle_fault` and moves no other dimension.
        LifecycleKind::Heartbeat | LifecycleKind::Snapshot => {
            LifecycleEvent::ReconcileOk { v: version }
        }
    };
    let verdict = apply_persist(sched, &report.task_id, &event)?;
    if matches!(report.kind, LifecycleKind::Fault) {
        let reason = format!(
            "agent reported a fault at generation {} seq {}",
            report.generation, report.seq
        );
        record_fault(
            sched,
            &report.task_id,
            "agent_reported",
            "report:fault",
            &reason,
        )?;
    }
    Ok(verdict)
}

/// Consume a `---swarm-report` frame found in `text`. Returns `true` when the
/// text was a report frame at all — applied, dropped by the watermark, or
/// turned into a protocol diagnostic — so the caller stops routing it to the
/// task paths. A frame that fails `parse_report` never reaches the ledger.
pub fn try_report(sched: &Arc<Sched>, workspace: &str, text: &str) -> bool {
    if !text.starts_with(proto::REPORT_PREFIX) {
        return false;
    }
    match proto::parse_report(text) {
        ParseResult::Parsed(report) => {
            let task_id = report.task_id.clone();
            let kind = report.kind.as_str();
            match apply_report(sched, &report) {
                Ok(verdict) => tracing::debug!(
                    task = %task_id,
                    kind,
                    verdict = ?verdict,
                    "reduced a swarm lifecycle report"
                ),
                Err(err) => {
                    let reason = err.to_string();
                    tracing::warn!(task = %task_id, kind, error = %reason, "swarm lifecycle report failed");
                    sched.note_alert(format!(
                        "lifecycle report for {} failed: {reason}",
                        short(&task_id)
                    ));
                    sched.emit(
                        "lifecycle_report_failed",
                        json!({"task_id": task_id, "kind": kind, "workspace": workspace, "error": reason}),
                    );
                }
            }
        }
        ParseResult::ProtocolFault(err) => {
            let reason = err.to_string();
            tracing::warn!(workspace = %workspace, error = %reason, "swarm lifecycle report is malformed");
            sched.note_alert(format!(
                "malformed lifecycle report in {workspace}: {reason}"
            ));
            sched.emit(
                "lifecycle_protocol_fault",
                json!({"workspace": workspace, "error": reason, "frame": frame_head(text)}),
            );
        }
        ParseResult::NotSwarm => return false,
    }
    true
}

fn frame_head(text: &str) -> String {
    let mut head = String::new();
    for ch in text.chars() {
        if head.len() >= 200 {
            head.push_str("...[truncated]");
            break;
        }
        head.push(ch);
    }
    head
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::runtime::{fake::FakeBackend, Capabilities, CloseReason, SpawnSpec};
    use std::sync::atomic::Ordering;

    /// A temp-rooted scheduler whose backend the test controls. The tempdir is
    /// leaked so `Db::open` keeps a valid path for the test's lifetime, matching
    /// `sched::tests`'s convention.
    fn sched_with(backend: Arc<dyn SessionBackend>) -> Arc<Sched> {
        let dir = tempfile::tempdir().unwrap();
        let root: &'static std::path::Path = Box::leak(dir.path().join("root").into_boxed_path());
        std::fs::create_dir_all(root).unwrap();
        let _ = Box::leak(Box::new(dir));
        let db = Db::open(root).unwrap();
        Sched::new_with_backend(root.to_path_buf(), db, Some(backend), None)
    }

    fn live_sched() -> (Arc<Sched>, Arc<Probe>) {
        let probe = Arc::new(Probe::new());
        let sched = sched_with(probe.clone());
        (sched, probe)
    }

    /// A fake resource the test can bring alive and kill, with switches for the
    /// two inconclusive shapes: a probe that errors, and a probe that reports a
    /// live terminal with no pane attached.
    #[derive(Default)]
    struct Probe {
        fake: FakeBackend,
        probe_errors: std::sync::atomic::AtomicBool,
        pane_detached: std::sync::atomic::AtomicBool,
    }

    impl Probe {
        fn new() -> Self {
            Self::default()
        }
        fn bring_up(&self, task: &str) {
            self.fake
                .spawn(SpawnSpec {
                    cwd: ".".into(),
                    task_id: task.into(),
                    command: vec!["pi".into()],
                    env: Default::default(),
                    focus: None,
                    rename: None,
                })
                .unwrap();
        }
        fn kill(&self, task: &str) {
            let session = SessionRef {
                task_id: task.into(),
                backend: "fake".into(),
                backend_ref: json!({"id": task}),
                generation: 1,
            };
            self.fake.close(&session, CloseReason::Fault, true).unwrap();
        }
        fn fail_probes(&self) {
            self.probe_errors
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        fn detach_panes(&self) {
            self.pane_detached
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    impl SessionBackend for Probe {
        fn name(&self) -> &'static str {
            "fake"
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities {
                spawn: true,
                attach: true,
                probe: true,
                close: true,
                focus: true,
                rename: true,
            }
        }
        fn available(&self) -> anyhow::Result<bool> {
            Ok(true)
        }
        fn spawn(&self, spec: SpawnSpec) -> anyhow::Result<SessionRef> {
            self.fake.spawn(spec)
        }
        fn attach(&self, session: &SessionRef) -> anyhow::Result<SessionRef> {
            self.fake.attach(session)
        }
        fn probe(&self, session: &SessionRef) -> anyhow::Result<ResourceProbe> {
            if self.probe_errors.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(anyhow::anyhow!("orca socket is not answering"));
            }
            let probe = self.fake.probe(session)?;
            Ok(ResourceProbe {
                attached: !self.pane_detached.load(std::sync::atomic::Ordering::SeqCst),
                detail: (!probe.alive).then(|| json!({"status": "exited"})),
                ..probe
            })
        }
        fn close(
            &self,
            session: &SessionRef,
            reason: CloseReason,
            force: bool,
        ) -> anyhow::Result<()> {
            self.fake.close(session, reason, force)
        }
    }

    fn outcome_of(sched: &Arc<Sched>, task_id: &str) -> String {
        let row = sched.db.get_session(task_id).unwrap().unwrap();
        let obs: Observation = serde_json::from_str(&row.observed_json).unwrap();
        tag(&obs.outcome).unwrap()
    }

    /// A task whose session is booted, attached, and working, with a live fake
    /// resource behind it.
    fn live_session(sched: &Arc<Sched>, probe: &Probe, task: &str, state: TaskState) {
        sched
            .db
            .insert_task(task, ".", "worker", "", 1, "payload")
            .unwrap();
        sched.db.set_state(task, state).unwrap();
        probe.bring_up(task);
        sched.sessions.lock().unwrap().insert(
            task.into(),
            SessionRef {
                task_id: task.into(),
                backend: "fake".into(),
                backend_ref: json!({"handle": format!("term-{task}")}),
                generation: 1,
            },
        );
        feed_created(sched, task).unwrap();
        feed_resource_attached(sched, task).unwrap();
        feed_ready(sched, task).unwrap();
        feed_turn_started(sched, task).unwrap();
    }

    fn row(sched: &Arc<Sched>, task: &str) -> SessionRecord {
        sched.db.get_session(task).unwrap().unwrap()
    }

    #[test]
    fn created_ready_working_chain_applies_and_persists() {
        let (sched, _probe) = live_sched();
        sched
            .db
            .insert_task("chain-1", ".", "worker", "", 1, "payload")
            .unwrap();
        let created = feed_created(&sched, "chain-1").unwrap();
        let booting = match created {
            Verdict::Applied(obs) => obs,
            other => panic!("created must seed the row, got {other:?}"),
        };
        assert_eq!(booting.public, PublicLifecycle::Created);
        let first = row(&sched, "chain-1");
        assert_eq!(first.public_lifecycle, "created");
        assert_eq!((first.generation, first.seq), (1, 1));
        // No resource existed when the row was seeded.
        assert_eq!(first.backend_ref, "{}");

        sched.sessions.lock().unwrap().insert(
            "chain-1".into(),
            SessionRef {
                task_id: "chain-1".into(),
                backend: "fake".into(),
                backend_ref: json!({"handle": "term-chain"}),
                generation: 1,
            },
        );
        feed_ready(&sched, "chain-1").unwrap();
        let second = row(&sched, "chain-1");
        assert_eq!(second.public_lifecycle, "idle");
        assert_eq!(second.seq, 2);
        // The session row is the only place a resource reference survives a
        // restart, so it must follow the live session.
        assert!(
            second.backend_ref.contains("term-chain"),
            "{}",
            second.backend_ref
        );
        feed_resource_attached(&sched, "chain-1").unwrap();
        feed_turn_started(&sched, "chain-1").unwrap();
        let third = row(&sched, "chain-1");
        assert_eq!(third.agent_state, "running");
        assert_eq!(third.resource_state, "attached");
        assert_eq!(third.public_lifecycle, "working");
        assert_eq!(third.seq, 4);
        let obs: Observation = serde_json::from_str(&third.observed_json).unwrap();
        assert!(lifecycle::is_legal(&obs));
        // The audit trail names the event that moved the row (serde tag).
        assert!(
            third.desired_json.contains("turn_started"),
            "{}",
            third.desired_json
        );

        let mut rx = sched.bus.sender().subscribe();
        feed_turn_ended(&sched, "chain-1").unwrap();
        let event = rx.try_recv().expect("transition emitted on the bus");
        assert_eq!(event.typ, "lifecycle");
        assert_eq!(event.data["task_id"], "chain-1");
        assert_eq!(event.data["from"], "working");
        assert_eq!(event.data["to"], "idle");
        assert_eq!(event.data["recovery"], "none");
        assert_eq!(event.data["outcome"], "pending");
        assert_eq!(event.data["generation"], 1);
        assert_eq!(event.data["seq"], 5);
        assert_eq!(event.data["event"], "turn_ended");
    }

    #[test]
    fn duplicate_and_stale_events_never_write() {
        let (sched, _probe) = live_sched();
        sched
            .db
            .insert_task("dup-1", ".", "worker", "", 1, "payload")
            .unwrap();
        feed_created(&sched, "dup-1").unwrap();
        // A second Created for a row that exists is a no-op, not a re-boot.
        assert!(matches!(
            feed_created(&sched, "dup-1").unwrap(),
            Verdict::Ignored(IgnoredReason::NoOp)
        ));
        let before = row(&sched, "dup-1");
        assert_eq!(before.seq, 1);
        // A late report at a sequence the row already passed is dropped and
        // leaves the ledger byte-identical.
        let verdict = apply_persist(
            &sched,
            "dup-1",
            &LifecycleEvent::Ready {
                v: Version::new(1, 1),
            },
        )
        .unwrap();
        assert!(matches!(
            verdict,
            Verdict::Ignored(IgnoredReason::StaleOrDuplicateSeq)
        ));
        let after = row(&sched, "dup-1");
        assert_eq!(after.observed_json, before.observed_json);
        assert_eq!(after.updated_at, before.updated_at);
        // The gate is the ledger's own: an equal or lower watermark cannot write.
        let stale = VersionedSession {
            seq: 0,
            generation: 0,
            ..to_versioned(
                &serde_json::from_str(&after.observed_json).unwrap(),
                &after.backend_ref,
                "{}",
            )
            .unwrap()
        };
        assert!(!sched.db.upsert_session("dup-1", &stale).unwrap());
        assert_eq!(row(&sched, "dup-1").observed_json, before.observed_json);
    }

    #[test]
    fn unadopted_generation_is_rejected_without_writing() {
        let (sched, probe) = live_sched();
        live_session(&sched, &probe, "gen-1", TaskState::Running);
        let before = row(&sched, "gen-1");
        // Generation 2 was never adopted, so the reducer refuses it and the row
        // keeps its own watermark instead of jumping ahead.
        let verdict = apply_persist(
            &sched,
            "gen-1",
            &LifecycleEvent::Ready {
                v: Version::new(2, 1),
            },
        )
        .unwrap();
        assert!(matches!(
            verdict,
            Verdict::Rejected(RejectReason::UnadoptedGeneration)
        ));
        let after = row(&sched, "gen-1");
        assert_eq!(after.generation, before.generation);
        assert_eq!(after.seq, before.seq);
        assert_eq!(after.observed_json, before.observed_json);
    }

    #[test]
    fn corrupt_row_keeps_its_watermark_and_reports_itself() {
        let (sched, _probe) = live_sched();
        sched
            .db
            .insert_task("cr-1", ".", "worker", "", 1, "payload")
            .unwrap();
        // A session row may only be created fresh or advanced past its current
        // watermark (`upsert_session` drops equal/lower seq writes), so inject
        // the corruption on a task that has no session row yet: the INSERT
        // branch writes unconditionally and leaves a usable-but-corrupt row.
        let mut broken = to_versioned(&Observation::initial(1, 3), "{}", "{}").unwrap();
        broken.generation = 1;
        broken.seq = 5;
        broken.observed_json = "{ not json".into();
        assert!(sched.db.upsert_session("cr-1", &broken).unwrap());
        let before = row(&sched, "cr-1");
        assert_eq!(before.seq, 5);
        // Subscribe first: reading the corrupt row during the feed is what
        // emits the `lifecycle_corrupt` frame and heals the tuple.
        let mut rx = sched.bus.sender().subscribe();
        // The row is still usable: the event applies just past the watermark.
        let verdict = feed_turn_ended(&sched, "cr-1").unwrap();
        assert!(matches!(verdict, Verdict::Applied(_)), "{verdict:?}");
        let after = row(&sched, "cr-1");
        assert_eq!(after.generation, before.generation);
        assert_eq!(after.seq, before.seq + 1);
        // The corruption was reported as an alert and on the bus.
        assert!(
            sched
                .recent_alerts()
                .iter()
                .any(|line| line.contains("cr-1") && line.contains("corrupt")),
            "{:?}",
            sched.recent_alerts()
        );
        let mut saw_corrupt = false;
        while let Ok(event) = rx.try_recv() {
            saw_corrupt |= event.typ == "lifecycle_corrupt";
        }
        assert!(saw_corrupt, "corruption must reach the bus");
    }

    #[test]
    fn probe_dead_fails_the_session_and_records_one_fault() {
        let (sched, probe) = live_sched();
        live_session(&sched, &probe, "dead-1", TaskState::Running);
        let mut rx = sched.bus.sender().subscribe();
        probe.kill("dead-1");
        let summary = startup_reconcile(&sched);
        assert_eq!(summary.scanned, 1);
        assert_eq!(summary.open, 1);
        assert_eq!(summary.dead, 1, "{summary:?}");
        assert!(summary.applied >= 1, "{summary:?}");
        assert_eq!(summary.faults, 1, "{summary:?}");
        // The fault was not left ownerless: a recovery task went to the parent role.
        assert_eq!(summary.recoveries, 1, "{summary:?}");
        let session = row(&sched, "dead-1");
        assert_eq!(session.agent_state, "gone");
        assert_eq!(session.resource_state, "closing");
        assert_eq!(session.public_lifecycle, "exited");
        assert_eq!(outcome_of(&sched, "dead-1"), "failed");
        // Exactly one fault row: a repeated pass must not bury the queue.
        let faults = sched.db.list_faults(Some("dead-1")).unwrap();
        assert_eq!(faults.len(), 1, "{faults:?}");
        assert_eq!(faults[0].kind, "probe_dead");
        assert_eq!(faults[0].intent, "reconcile:probe_dead");
        // `link_fault_recovery` owns the state transition: an `open` row here
        // would mean the fault has no owner, which is the bug this pass exists
        // to prevent.
        assert_eq!(faults[0].state, "recovery_created");
        assert_eq!(faults[0].attempt, 1);
        assert_eq!(faults[0].generation, session.generation);
        assert!(
            faults[0].observed_json.contains("running"),
            "{}",
            faults[0].observed_json
        );
        assert!(faults[0].backend_ref.contains("term-dead-1"));
        assert!(faults[0].reason.contains("exited"), "{}", faults[0].reason);
        // The recovery line is durable and traceable: the fault names the task,
        // the task names the fault, and it aims at the direct parent role —
        // `dead-1` has no upstream handoff, so that is the root supervisor.
        let recovery_id = faults[0]
            .recovery_task_id
            .clone()
            .expect("a probe_dead fault on owed work must name its recovery task");
        let recovery = sched.db.get(&recovery_id).unwrap().expect("recovery row");
        assert_eq!(recovery.kind, "recovery");
        assert_eq!(
            recovery.failure_of.as_deref(),
            Some(faults[0].id.to_string().as_str())
        );
        assert_eq!(recovery.to_ws, ".");
        assert_eq!(recovery.from_ws, "worker");
        assert_eq!(recovery.attempt, 1);
        assert_eq!(
            recovery.transfer_send_to, "",
            "recovery reports through the parent's own out channel, so it joins no family"
        );
        assert!(recovery.payload.contains("dead-1"), "{}", recovery.payload);
        assert!(
            recovery.payload.contains("probe_dead"),
            "{}",
            recovery.payload
        );
        // A second pass over the same fault neither re-queues it nor spawns a
        // second recovery task: dedupe covers the whole recovery write.
        startup_reconcile(&sched);
        let again = sched.db.list_faults(Some("dead-1")).unwrap();
        assert_eq!(again.len(), 1);
        assert_eq!(
            again[0].recovery_task_id.as_deref(),
            Some(recovery_id.as_str())
        );
        assert_eq!(sched.db.list_faults(None).unwrap().len(), 1);
        let mut saw_fault = false;
        let mut saw_recovery = false;
        let mut saw_pass = false;
        while let Ok(event) = rx.try_recv() {
            saw_fault |= event.typ == "session_fault";
            saw_recovery |= event.typ == "recovery_created";
            saw_pass |= event.typ == "reconcile_pass";
        }
        assert!(saw_recovery, "recovery creation must reach the bus");
        assert!(saw_fault, "the fault must reach the bus");
        assert!(saw_pass, "the pass must reach the bus");
        assert!(
            sched
                .recent_alerts()
                .iter()
                .any(|line| line.contains("probe_dead")),
            "{:?}",
            sched.recent_alerts()
        );
    }

    #[test]
    fn inconclusive_probe_never_judges_a_generation_dead() {
        // Shape one: the row names no backend resource at all. Seed a fresh,
        // open session row with an empty backend_ref so the probe has nothing
        // to resolve (upsert on a task with no session row writes unconditionally).
        let (sched, _probe) = live_sched();
        sched
            .db
            .insert_task("unk-1", ".", "worker", "", 1, "payload")
            .unwrap();
        let seed = to_versioned(&Observation::initial(1, 3), "{}", "{}").unwrap();
        assert!(sched.db.upsert_session("unk-1", &seed).unwrap());
        let before = row(&sched, "unk-1");
        let summary = startup_reconcile(&sched);
        assert_eq!(summary.unknown, 1, "{summary:?}");
        assert_eq!(summary.dead, 0);
        assert_eq!(summary.faults, 0);
        let after = row(&sched, "unk-1");
        assert_eq!(after.observed_json, before.observed_json);
        assert!(sched.db.list_faults(Some("unk-1")).unwrap().is_empty());

        // Shape two: a real handle whose probe call itself failed.
        let (sched, probe) = live_sched();
        live_session(&sched, &probe, "err-1", TaskState::Running);
        probe.fail_probes();
        let before = row(&sched, "err-1");
        let summary = periodic_reconcile(&sched);
        assert_eq!(summary.unknown, 1, "{summary:?}");
        assert_eq!(summary.dead, 0);
        assert_eq!(summary.faults, 0);
        let after = row(&sched, "err-1");
        assert_eq!(after.public_lifecycle, "working");
        assert_eq!(after.observed_json, before.observed_json);
        assert!(sched.db.list_faults(Some("err-1")).unwrap().is_empty());
        // The pass still said so on the bus.
        assert!(summary.errors == 0);
    }

    #[test]
    fn probe_confirms_and_clears_a_stuck_recovery() {
        let (sched, probe) = live_sched();
        live_session(&sched, &probe, "ok-1", TaskState::Running);
        // A mismatch isolates an idle turn (DEFAULT_ISOLATE_AFTER is 1); the
        // reducer only moves an Idle agent to idle_fault, so end the turn
        // first. The next confirming probe is what heals the isolated idle.
        feed_turn_ended(&sched, "ok-1").unwrap();
        feed_mismatch(&sched, "ok-1").unwrap();
        let stuck = row(&sched, "ok-1");
        assert_eq!(stuck.recovery_substate, "idle_fault");
        assert_eq!(stuck.mismatch_count, 1);
        assert_eq!(stuck.public_lifecycle, "working");
        let summary = periodic_reconcile(&sched);
        assert_eq!(summary.ok, 1, "{summary:?}");
        assert_eq!(summary.mismatch, 0);
        assert_eq!(summary.dead, 0);
        let healed = row(&sched, "ok-1");
        assert_eq!(healed.recovery_substate, "none");
        assert_eq!(healed.mismatch_count, 0);
        // A healed idle hop with no open delivery projects plain `idle`.
        assert_eq!(healed.public_lifecycle, "idle");
        // A clean tuple is confirmed without another write.
        let before = healed.seq;
        let summary = periodic_reconcile(&sched);
        assert_eq!(summary.ok, 1, "{summary:?}");
        assert_eq!(row(&sched, "ok-1").seq, before);
    }

    #[test]
    fn mismatch_isolates_then_terminates_into_the_fault_queue() {
        let (sched, probe) = live_sched();
        live_session(&sched, &probe, "mm-1", TaskState::Running);
        probe.detach_panes();
        let summary = periodic_reconcile(&sched);
        assert_eq!(summary.mismatch, 1, "{summary:?}");
        let first = row(&sched, "mm-1");
        assert_eq!(first.mismatch_count, 1);
        // The tuple was working (agent running), so isolating it lands the
        // mismatch counter without an idle fault substate.
        assert_eq!(first.public_lifecycle, "working");
        assert!(sched.db.list_faults(Some("mm-1")).unwrap().is_empty());
        periodic_reconcile(&sched);
        assert_eq!(row(&sched, "mm-1").mismatch_count, 2);
        let summary = periodic_reconcile(&sched);
        assert_eq!(summary.faults, 1, "{summary:?}");
        let last = row(&sched, "mm-1");
        assert_eq!(last.public_lifecycle, "exited");
        assert_eq!(last.agent_state, "gone");
        assert_eq!(outcome_of(&sched, "mm-1"), "failed");
        let faults = sched.db.list_faults(Some("mm-1")).unwrap();
        assert_eq!(faults.len(), 1, "{faults:?}");
        assert_eq!(faults[0].kind, "mismatch_terminate");
        assert_eq!(faults[0].intent, "reconcile:mismatch");
        // The terminate fault opened its own recovery session on the parent role,
        // so this pass has a second row to read. `probe.detach_panes()` is a
        // backend-wide switch, so the fresh recovery resource reports "not
        // attached" too and disagrees with its own tuple once — exactly what a
        // recovery whose resource never attaches should look like. The source
        // task stays out of it: its row is history.
        let recovery_id = faults[0]
            .recovery_task_id
            .clone()
            .expect("a terminate fault must name its recovery task");
        let summary = periodic_reconcile(&sched);
        assert_eq!(summary.scanned, 2, "{summary:?}");
        assert_eq!(summary.open, 1, "{summary:?}");
        assert_eq!(summary.mismatch, 1, "{summary:?}");
        assert_eq!(summary.faults, 0, "{summary:?}");
        assert_eq!(summary.recoveries, 0, "{summary:?}");
        let recovery = row(&sched, &recovery_id);
        assert_eq!(
            recovery.public_lifecycle, "created",
            "the recovery row is the only open one: {:?}",
            summary
        );
        assert_eq!(recovery.task_id, recovery_id);
        assert_eq!(recovery.mismatch_count, 1);
        assert_eq!(recovery.agent_state, "booting");
        // Nothing about the recovery write reached back into the faulted
        // generation: same watermark, same tuple, same single fault line.
        let after = row(&sched, "mm-1");
        assert_eq!(after.seq, last.seq);
        assert_eq!(after.generation, last.generation);
        assert_eq!(after.observed_json, last.observed_json);
        assert_eq!(after.public_lifecycle, "exited");
        let faults = sched.db.list_faults(Some("mm-1")).unwrap();
        assert_eq!(faults.len(), 1);
        assert_eq!(faults[0].state, "recovery_created");
        assert_eq!(
            faults[0].recovery_task_id.as_deref(),
            Some(recovery_id.as_str())
        );
        assert_eq!(sched.db.list_faults(None).unwrap().len(), 1);
    }

    #[test]
    fn idle_mismatch_ladder_isolates_into_idle_fault() {
        let (sched, probe) = live_sched();
        live_session(&sched, &probe, "iso-1", TaskState::Running);
        // Turn ended with no open delivery intent settles to plain idle
        // (`recovery: none`), not idle_waiting; idle_waiting needs a pending
        // intent. Isolating comes from the mismatch on this idle agent.
        feed_turn_ended(&sched, "iso-1").unwrap();
        assert_eq!(row(&sched, "iso-1").recovery_substate, "none");
        feed_mismatch(&sched, "iso-1").unwrap();
        let isolated = row(&sched, "iso-1");
        assert_eq!(isolated.recovery_substate, "idle_fault");
        assert_eq!(isolated.public_lifecycle, "working");
        feed_reconcile_ok(&sched, "iso-1").unwrap();
        let healed = row(&sched, "iso-1");
        assert_eq!(healed.recovery_substate, "none");
        assert_eq!(healed.mismatch_count, 0);
    }

    /// The shape a hop reaches after `Complete` with no receipt yet.
    fn feed_complete_and_wait(sched: &Arc<Sched>, task: &str) {
        apply_at_next(sched, task, |v| LifecycleEvent::Complete { v }).unwrap();
    }

    #[test]
    fn delivered_hop_settles_as_done_before_the_resource_closes() {
        let (sched, probe) = live_sched();
        live_session(&sched, &probe, "out-1", TaskState::Running);
        let verdict = feed_delivered(&sched, "out-1").unwrap();
        assert!(matches!(verdict, Verdict::Applied(_)), "{verdict:?}");
        let session = row(&sched, "out-1");
        assert_eq!(session.delivery_state, "accepted");
        assert_eq!(outcome_of(&sched, "out-1"), "done");
        assert_eq!(session.public_lifecycle, "exited");
        // The tab is still open when the handoff lands; the recycle ack closes
        // the resource afterwards without rewriting the result.
        let verdict = feed_resource_closed(&sched, "out-1").unwrap();
        assert!(matches!(verdict, Verdict::Applied(_)), "{verdict:?}");
        let session = row(&sched, "out-1");
        assert_eq!(session.resource_state, "closed");
        assert_eq!(session.agent_state, "gone");
        assert_eq!(outcome_of(&sched, "out-1"), "done");
        // A duplicate out frame cannot reopen or rewrite it.
        let verdict = feed_delivered(&sched, "out-1").unwrap();
        assert!(
            matches!(verdict, Verdict::Ignored(_) | Verdict::Rejected(_)),
            "{verdict:?}"
        );
        let session = row(&sched, "out-1");
        assert_eq!(outcome_of(&sched, "out-1"), "done");
        assert_eq!(session.resource_state, "closed");
        // Nothing about this session is open, so reconcile skips the row.
        let summary = periodic_reconcile(&sched);
        assert_eq!(summary.open, 0, "{summary:?}");
    }

    #[test]
    fn failed_hop_keeps_the_fault_until_the_resource_closes() {
        let (sched, probe) = live_sched();
        live_session(&sched, &probe, "exit-1", TaskState::Running);
        feed_fail(&sched, "exit-1").unwrap();
        let session = row(&sched, "exit-1");
        assert_eq!(outcome_of(&sched, "exit-1"), "failed");
        assert_eq!(session.public_lifecycle, "working");
        feed_resource_closed(&sched, "exit-1").unwrap();
        let session = row(&sched, "exit-1");
        assert_eq!(session.public_lifecycle, "exited");
        assert_eq!(session.agent_state, "gone");
        assert_eq!(outcome_of(&sched, "exit-1"), "failed");
        assert_eq!(session.resource_state, "closed");
    }

    #[test]
    fn terminal_task_state_is_mirrored_before_the_probe_decides() {
        // The crash shape: the ledger recorded a delivered handoff, this row
        // never got it. reconcile settles the tuple as `done`, and the dead
        // resource that follows must not relabel a completed hop as a failure.
        let (sched, probe) = live_sched();
        live_session(&sched, &probe, "mir-1", TaskState::Closed);
        probe.kill("mir-1");
        let summary = startup_reconcile(&sched);
        assert_eq!(summary.faults, 0, "{summary:?}");
        assert_eq!(outcome_of(&sched, "mir-1"), "done");
        assert_eq!(row(&sched, "mir-1").public_lifecycle, "exited");
        assert!(sched.db.list_faults(Some("mir-1")).unwrap().is_empty());
        assert_eq!(
            sched.db.get("mir-1").unwrap().unwrap().state,
            TaskState::Closed
        );
        // The ledger's own failure is not a divergence worth a fault row.
        let (sched, probe) = live_sched();
        live_session(&sched, &probe, "mir-2", TaskState::Failed);
        probe.kill("mir-2");
        let summary = startup_reconcile(&sched);
        assert_eq!(summary.faults, 0, "{summary:?}");
        assert_eq!(outcome_of(&sched, "mir-2"), "failed");
        assert_eq!(row(&sched, "mir-2").agent_state, "gone");
    }

    #[test]
    fn unknown_task_is_an_explicit_failure_and_stores_nothing() {
        let (sched, _probe) = live_sched();
        let ghost = "f0000000-0000-4000-8000-000000000000";
        let verdict = apply_persist(
            &sched,
            ghost,
            &LifecycleEvent::ReconcileOk {
                v: Version::new(1, 1),
            },
        );
        assert!(verdict.is_err(), "an unknown session fails loudly");
        assert!(sched.db.get_session(ghost).unwrap().is_none());
        assert!(sched.db.list_faults(Some(ghost)).unwrap().is_empty());
        // A heartbeat report for an unknown task is a diagnostic too.
        let frame = proto::render_report(LifecycleKind::Heartbeat, ghost, 1, 1);
        assert!(frame.contains(ghost));
        assert!(try_report(&sched, "worker", &frame));
        assert!(sched.db.get_session(ghost).unwrap().is_none());
        assert!(
            sched
                .recent_alerts()
                .iter()
                .any(|line| line.contains("lifecycle report")),
            "{:?}",
            sched.recent_alerts()
        );
    }

    #[test]
    fn report_wire_is_reduced_by_its_own_version_and_bad_frames_are_diagnostics() {
        let (sched, _probe) = live_sched();
        let task = "11111111-1111-4111-8111-111111111111";
        sched
            .db
            .insert_task(task, ".", "worker", "", 1, "payload")
            .unwrap();
        feed_created(&sched, task).unwrap();
        let frame = proto::render_report(LifecycleKind::Ready, task, 1, 2);
        assert!(try_report(&sched, "worker", &frame));
        assert_eq!(row(&sched, task).public_lifecycle, "idle");
        // The same frame again: the watermark drops it.
        assert!(try_report(&sched, "worker", &frame));
        assert_eq!(row(&sched, task).seq, 2);
        // A heartbeat is liveness evidence only: with nothing to heal the
        // reducer folds it into a no-op, so it is consumed without regressing
        // or advancing the watermark.
        let frame = proto::render_report(LifecycleKind::Heartbeat, task, 1, 3);
        assert!(try_report(&sched, "worker", &frame));
        assert_eq!(row(&sched, task).seq, 2);
        // A turn_started report is the working evidence.
        let frame = proto::render_report(LifecycleKind::TurnStarted, task, 1, 4);
        assert!(try_report(&sched, "worker", &frame));
        assert_eq!(row(&sched, task).agent_state, "running");
        // A fault report lands the outcome and one fault row.
        let frame = proto::render_report(LifecycleKind::Fault, task, 1, 5);
        assert!(try_report(&sched, "worker", &frame));
        assert_eq!(outcome_of(&sched, task), "failed");
        let faults = sched.db.list_faults(Some(task)).unwrap();
        assert_eq!(faults.len(), 1, "{faults:?}");
        assert_eq!(faults[0].kind, "agent_reported");
        assert_eq!(faults[0].intent, "report:fault");
        assert_eq!(faults[0].generation, 1);
        // A malformed frame never reaches the ledger.
        let broken = format!(
            "{}task_id: not-a-uuid\ngeneration: x\n",
            proto::REPORT_PREFIX
        );
        assert!(matches!(
            proto::parse_report(&broken),
            ParseResult::ProtocolFault(_)
        ));
        let before = row(&sched, task);
        assert!(try_report(&sched, "worker", &broken));
        assert_eq!(row(&sched, task).observed_json, before.observed_json);
        assert_eq!(
            sched.db.list_faults(None).unwrap().len(),
            1,
            "a malformed frame writes no fault"
        );
        assert!(
            sched
                .recent_alerts()
                .iter()
                .any(|line| line.contains("malformed lifecycle report")),
            "{:?}",
            sched.recent_alerts()
        );
        // Text that is not a report at all is left for the other routes.
        assert!(!try_report(
            &sched,
            "worker",
            "---swarm\nout\nto: a\n\nbody"
        ));
        assert!(!try_report(&sched, "worker", "plain prose"));
    }

    #[test]
    fn pass_honours_shutdown_and_refuses_a_foreign_backend_ref() {
        let (sched, probe) = live_sched();
        live_session(&sched, &probe, "sd-1", TaskState::Running);
        live_session(&sched, &probe, "sd-2", TaskState::Running);
        sched.shutdown.store(true, Ordering::SeqCst);
        let summary = startup_reconcile(&sched);
        // The drain signal stops the walk instead of processing every row.
        assert_eq!(summary.scanned, 0, "{summary:?}");
        assert_eq!(summary.open, 0);
        sched.shutdown.store(false, Ordering::SeqCst);
        // A row whose reference names another task is refused, never probed.
        let first = row(&sched, "sd-1");
        let foreign = json!({
            "task_id": "someone-else",
            "backend": "fake",
            "backend_ref": {"handle": "term-x"},
            "generation": 1,
        })
        .to_string();
        let obs: Observation = serde_json::from_str(&first.observed_json).unwrap();
        let mut stored = to_versioned(&obs, &foreign, "{}").unwrap();
        stored.seq = first.seq + 1;
        sched.db.upsert_session("sd-1", &stored).unwrap();
        sched.sessions.lock().unwrap().remove("sd-1");
        let summary = periodic_reconcile(&sched);
        assert_eq!(summary.dead, 0, "{summary:?}");
        assert_eq!(summary.scanned, 2, "{summary:?}");
        assert!(summary.unknown >= 1, "{summary:?}");
        assert_eq!(row(&sched, "sd-1").agent_state, "running");
    }

    #[test]
    fn empty_ledger_produces_a_quiet_pass() {
        let (sched, _probe) = live_sched();
        let mut rx = sched.bus.sender().subscribe();
        let summary = startup_reconcile(&sched);
        assert_eq!(summary, ReconcileSummary::default());
        assert!(
            rx.try_recv().is_err(),
            "a pass that touched nothing stays off the bus"
        );
        assert!(sched.recent_alerts().is_empty());
    }

    #[test]
    fn a_session_never_tracked_by_the_scheduler_stays_out_of_the_ledger() {
        // `feed_*` allocates its own version, so the guard has to hold there too.
        let (sched, _probe) = live_sched();
        let mut rx = sched.bus.sender().subscribe();
        try_feed(&sched, "nobody", |v| LifecycleEvent::Ready { v });
        assert!(sched.db.get_session("nobody").unwrap().is_none());
        while let Ok(event) = rx.try_recv() {
            assert_ne!(event.typ, "lifecycle", "stray session: {:?}", event);
        }
    }

    // -----------------------------------------------------------------------
    // durable intents, recovery lineage, watermark sharing, hop chain
    // -----------------------------------------------------------------------

    /// Everything the stand-in daemon (`ipc::test_daemon`) remembers.
    type DaemonState = crate::ipc::DaemonState;

    fn fake_daemon(sched: &Arc<Sched>, role: &str) -> Arc<std::sync::Mutex<DaemonState>> {
        crate::ipc::test_daemon(&sched.root, role)
    }

    /// Wait briefly for a write the daemon threads may still be making: the
    /// test double answers the ack poll on its own connection, so the history
    /// line lands a moment after the send returns.
    fn waits_for(mut check: impl FnMut() -> bool) -> bool {
        for _ in 0..80 {
            if check() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        false
    }

    fn intent(sched: &Arc<Sched>, op_id: &str) -> crate::db::IntentRecord {
        sched
            .db
            .get_intent(op_id)
            .unwrap()
            .unwrap_or_else(|| panic!("no intent row {op_id}"))
    }

    #[test]
    fn a_recycle_intent_receipts_once_and_the_line_dedupes_within_the_generation() {
        let (sched, probe) = live_sched();
        let daemon = fake_daemon(&sched, "worker");
        live_session(&sched, &probe, "rec-1", TaskState::Running);
        let mut rx = sched.bus.sender().subscribe();
        let op_id = crate::reconcile::request_recycle(&sched, "rec-1", "close")
            .expect("a fresh recycle intent");
        assert_eq!(
            op_id,
            format!("recycle:rec-1:g{}", row(&sched, "rec-1").generation)
        );
        // Persist-first: the row and its receipt are both durable by the time
        // the call returns, and the receipt carries the daemon's own answer.
        let record = intent(&sched, &op_id);
        assert_eq!(record.state, "succeeded", "{record:?}");
        assert_eq!(record.attempt, 1);
        assert!(record.receipt_json.contains("accepted"), "{record:?}");
        assert!(waits_for(|| daemon.lock().unwrap().requests.len() >= 1));
        let sent = &daemon.lock().unwrap().requests[0];
        assert_eq!(sent["op"], "loopback");
        assert!(
            sent["text"].as_str().unwrap().contains("op: recycle"),
            "{sent:?}"
        );
        assert_eq!(row(&sched, "rec-1").delivery_state, "accepted");
        // The same request twice inside one generation is one request: a second
        // control frame would be a second close of an already-closed tab.
        assert!(
            crate::reconcile::request_recycle(&sched, "rec-1", "quit").is_none(),
            "the generation's recycle line is already answered"
        );
        assert_eq!(sched.db.list_intents(None).unwrap().len(), 1);
        let mut saw_receipt = false;
        while let Ok(event) = rx.try_recv() {
            if event.typ == "intent" && event.data["event"] == "receipt" {
                saw_receipt = true;
            }
        }
        assert!(saw_receipt, "the receipt must reach the bus");
    }

    #[test]
    fn an_unreachable_daemon_backs_off_spends_its_budget_and_queues_a_fault() {
        let (sched, probe) = live_sched();
        // No daemon is bound for "worker": every attempt fails on connect,
        // which is the only honest way to watch the retry ladder.
        live_session(&sched, &probe, "ex-1", TaskState::Running);
        let opened = crate::reconcile::request_recycle(&sched, "ex-1", "close")
            .expect("the intent row is written before the send");
        let first = intent(&sched, &opened);
        assert_eq!(first.state, "pending", "{first:?}");
        assert_eq!(first.attempt, 1);
        assert!(
            first.next_attempt_at > first.created_at,
            "a failed send must carry a backoff slot: {first:?}"
        );
        // Tick 1: due, fails, backs off further. `now` is the pump's clock, so
        // the test moves it forward instead of sleeping.
        let counts = pump_intents(&sched, first.next_attempt_at);
        assert_eq!(counts.retried, 1, "{counts:?}");
        let second = intent(&sched, &opened);
        assert_eq!(second.state, "pending", "{second:?}");
        assert_eq!(second.attempt, 2);
        assert!(
            second.next_attempt_at > first.next_attempt_at,
            "the backoff must grow: {first:?} -> {second:?}"
        );
        assert_eq!(
            second.next_attempt_at - first.next_attempt_at,
            backoff_secs(2),
            "1s then 2s then 4s"
        );
        // A tick before the slot is due must not spend an attempt.
        assert_eq!(
            pump_intents(&sched, second.next_attempt_at - 1).attempted,
            0
        );
        // Tick 2 is the last attempt: the row goes exhausted, the tuple says so,
        // and the fault queue gains exactly one line.
        let counts = pump_intents(&sched, second.next_attempt_at);
        assert_eq!(counts.exhausted, 1, "{counts:?}");
        let spent = intent(&sched, &opened);
        assert_eq!(spent.state, "exhausted", "{spent:?}");
        assert_eq!(spent.attempt, 3);
        assert!(
            spent.last_error.contains("No such file") || spent.last_error.contains("refused"),
            "the row must keep the transport's own reason: {spent:?}"
        );
        let session = row(&sched, "ex-1");
        assert_eq!(session.delivery_state, "exhausted", "{session:?}");
        let faults = sched.db.list_faults(Some("ex-1")).unwrap();
        assert_eq!(faults.len(), 1, "{faults:?}");
        assert_eq!(faults[0].kind, "intent_exhausted");
        assert_eq!(faults[0].intent, "intent:recycle");
        assert_eq!(faults[0].state, "recovery_created");
        // The exhausted intent is not the end of the line: the owed work gets a
        // recovery task on the parent role.
        let recovery_id = faults[0]
            .recovery_task_id
            .clone()
            .expect("an exhausted intent on owed work recovers");
        let recovery = sched.db.get(&recovery_id).unwrap().expect("recovery row");
        assert_eq!(recovery.kind, "recovery");
        assert_eq!(recovery.to_ws, ".");
        // Nothing left to pump: an exhausted row is never re-claimed.
        assert_eq!(pump_intents(&sched, i64::MAX / 2).attempted, 0);
    }

    #[test]
    fn a_recovery_tasks_own_fault_stays_with_the_root_supervisor_and_siblings_hold() {
        let (sched, probe) = live_sched();
        live_session(&sched, &probe, "root-1", TaskState::Running);
        live_session(&sched, &probe, "sibling-1", TaskState::Running);
        let mut rx = sched.bus.sender().subscribe();
        // First layer: root-1 dies and gets a recovery task.
        probe.kill("root-1");
        let summary = periodic_reconcile(&sched);
        assert_eq!(summary.faults, 1, "{summary:?}");
        assert_eq!(summary.recoveries, 1, "{summary:?}");
        let first = &sched.db.list_faults(Some("root-1")).unwrap()[0];
        let recovery_id = first.recovery_task_id.clone().unwrap();
        let recovery = sched.db.get(&recovery_id).unwrap().unwrap();
        assert_eq!(recovery.kind, "recovery");
        assert_eq!(
            recovery.failure_of.as_deref(),
            Some(first.id.to_string().as_str())
        );
        let sibling_before = row(&sched, "sibling-1");
        let sibling_task_before = sched.db.get("sibling-1").unwrap().unwrap();
        // Second layer: the recovery task itself dies. It must not spawn a
        // recovery of its own; the fault stays in the root queue unlinked.
        probe.kill(&recovery_id);
        let summary = periodic_reconcile(&sched);
        assert_eq!(summary.faults, 1, "{summary:?}");
        assert_eq!(summary.recoveries, 0, "{summary:?}");
        let recovery_faults = sched.db.list_faults(Some(&recovery_id)).unwrap();
        assert_eq!(recovery_faults.len(), 1, "{recovery_faults:?}");
        assert_eq!(
            recovery_faults[0].state, "open",
            "a suppressed recovery leaves its fault open"
        );
        assert!(recovery_faults[0].recovery_task_id.is_none());
        assert_eq!(
            sched.db.list_faults(None).unwrap().len(),
            2,
            "both faults stay visible in the root queue"
        );
        // The sibling's tuple and ledger are untouched by any of this.
        assert_eq!(
            row(&sched, "sibling-1").observed_json,
            sibling_before.observed_json
        );
        assert_eq!(row(&sched, "sibling-1").seq, sibling_before.seq);
        let sibling_task_after = sched.db.get("sibling-1").unwrap().unwrap();
        assert_eq!(sibling_task_after.state, sibling_task_before.state);
        assert_eq!(sibling_task_after.kind, "normal");
        assert!(sibling_task_after.failure_of.is_none());
        let mut saw_suppressed = false;
        while let Ok(event) = rx.try_recv() {
            if event.typ == "recovery_suppressed" {
                saw_suppressed = true;
                assert_eq!(event.data["task_id"], recovery_id.as_str());
                assert_eq!(event.data["fault_id"], recovery_faults[0].id);
            }
        }
        assert!(saw_suppressed, "the single-layer gate must announce itself");
        assert!(
            sched
                .recent_alerts()
                .iter()
                .any(|line| line.contains("root queue")),
            "{:?}",
            sched.recent_alerts()
        );
    }

    // -----------------------------------------------------------------------
    // the shared (generation, seq) watermark: internal feeds and Pi reports
    // -----------------------------------------------------------------------

    fn report(
        kind: LifecycleKind,
        task: &str,
        generation: u64,
        seq: u64,
    ) -> crate::proto::LifecycleReport {
        crate::proto::LifecycleReport {
            kind,
            task_id: task.into(),
            generation,
            seq,
        }
    }

    /// Pi and this daemon each count `(generation, seq)` on their own, and both
    /// write the same ledger column. The reducer keeps exactly one of them: the
    /// higher seq. This test pins that consequence by name, in both directions,
    /// because it decides what a Pi-side restart costs.
    #[test]
    fn pi_reports_and_internal_feeds_share_one_watermark_and_the_later_writer_loses() {
        let (sched, probe) = live_sched();
        live_session(&sched, &probe, "wm-1", TaskState::Running);
        let internal = row(&sched, "wm-1");
        // The scheduler's own ladder wrote four facts: created, resource attach,
        // ready, turn started. Pi knows nothing about them.
        assert_eq!(
            (internal.generation, internal.seq),
            (1, 4),
            "the internal ladder owns seq 1..=4: {internal:?}"
        );
        // Pi's report for the same generation carries Pi's own counter. At seq 4
        // it collides with what is already stored, so the reducer drops it — the
        // completion exit never opens, and only the out channel can save the hop.
        let verdict = apply_report(&sched, &report(LifecycleKind::Complete, "wm-1", 1, 4))
            .expect("a well-formed frame must reach the reducer");
        assert!(
            matches!(
                verdict,
                Verdict::Ignored(IgnoredReason::StaleOrDuplicateSeq)
            ),
            "Pi's colliding seq must be dropped by name, not silently: {verdict:?}"
        );
        let dropped = row(&sched, "wm-1");
        assert_eq!(dropped.seq, 4);
        assert_eq!(
            dropped.delivery_state, "none",
            "the dropped report is exactly the lost completion intent"
        );
        // The other direction: an internal feed allocates stored.seq + 1, so it
        // happily overwrites a Pi report that got there first.
        let verdict = apply_report(&sched, &report(LifecycleKind::Complete, "wm-1", 1, 5))
            .expect("seq 5 is ahead of the watermark");
        assert!(matches!(verdict, Verdict::Applied(_)), "{verdict:?}");
        assert_eq!(row(&sched, "wm-1").delivery_state, "pending");
        let next = next_version(&sched, "wm-1").unwrap();
        assert_eq!(
            next,
            Version::new(1, 6),
            "internal feeds follow Pi's number"
        );
        feed_turn_ended(&sched, "wm-1").unwrap();
        let after = row(&sched, "wm-1");
        assert_eq!(after.seq, 6);
        assert_eq!(
            after.agent_state, "idle",
            "the internal event won the slot and rewrote the tuple"
        );
        // So a Pi frame replayed at a seq the daemon has already passed is
        // dropped for good: no reconciliation, no queueing, no second chance.
        let verdict = apply_report(&sched, &report(LifecycleKind::Snapshot, "wm-1", 1, 5))
            .expect("parsed and reduced");
        assert!(
            matches!(
                verdict,
                Verdict::Ignored(IgnoredReason::StaleOrDuplicateSeq)
            ),
            "{verdict:?}"
        );
        assert_eq!(row(&sched, "wm-1").seq, 6);
    }

    /// Pi increments `generation` every time it claims a session start. A report
    /// from a generation the ledger has not adopted is refused outright, so a Pi
    /// restart that outruns the scheduler parks the whole generation until the
    /// old one is proven gone and `repair adopt` moves the watermark.
    #[test]
    fn an_unadopted_pi_generation_is_refused_until_reconcile_and_repair_let_it_in() {
        let (sched, probe) = live_sched();
        live_session(&sched, &probe, "wm-2", TaskState::Running);
        let verdict = apply_report(&sched, &report(LifecycleKind::Ready, "wm-2", 2, 1))
            .expect("parsed and reduced");
        assert!(
            matches!(
                verdict,
                Verdict::Rejected(RejectReason::UnadoptedGeneration)
            ),
            "generation 2 has no adoption path yet: {verdict:?}"
        );
        let held = row(&sched, "wm-2");
        assert_eq!(held.generation, 1);
        assert_eq!(held.seq, 4, "a refused generation writes nothing at all");
        // Adoption needs proof the old generation is gone. While its resource is
        // alive, `repair adopt` refuses, so there is no way to let Pi in early.
        let err = crate::repair::adopt(&sched, "wm-2")
            .err()
            .expect("a live generation must block adoption")
            .to_string();
        assert!(err.contains("still live"), "{err}");
        assert_eq!(row(&sched, "wm-2").generation, 1);
        // The designed door: reconcile proves the resource gone, and the fault
        // ladder fails the owed work and opens its recovery line.
        probe.kill("wm-2");
        let summary = periodic_reconcile(&sched);
        assert_eq!(summary.dead, 1, "{summary:?}");
        let gone = row(&sched, "wm-2");
        assert_eq!(gone.agent_state, "gone");
        assert!(
            !crate::reconcile::stored_observation(&sched, Some(&gone)).generation_live,
            "the ledger must stop claiming the old generation is live"
        );
        // Now the operator can adopt, and Pi's own generation is writable.
        let detail = crate::repair::adopt(&sched, "wm-2").expect("adoption after proof");
        assert_eq!(detail["generation"], 2, "{detail}");
        let adopted = row(&sched, "wm-2");
        assert_eq!((adopted.generation, adopted.seq), (2, 0), "{adopted:?}");
        assert_eq!(adopted.agent_state, "booting");
        assert_eq!(outcome_of(&sched, "wm-2"), "pending");
        let verdict = apply_report(&sched, &report(LifecycleKind::Ready, "wm-2", 2, 1))
            .expect("the adopted generation accepts Pi's counter");
        assert!(matches!(verdict, Verdict::Applied(_)), "{verdict:?}");
        assert_eq!(row(&sched, "wm-2").public_lifecycle, "idle");
        // And a frame from *behind* the adopted watermark is dropped, by a
        // different name: an older generation is stale, a newer one is
        // unadopted. Pi's previous-generation reports can never land again.
        let verdict =
            apply_report(&sched, &report(LifecycleKind::Ready, "wm-2", 1, 9)).expect("parsed");
        assert!(
            matches!(verdict, Verdict::Ignored(IgnoredReason::StaleGeneration)),
            "generation 1 is behind the watermark, so it is stale: {verdict:?}"
        );
        assert_eq!(row(&sched, "wm-2").generation, 2);
    }

    // -----------------------------------------------------------------------
    // the whole hop chain, over a real backend and a real daemon socket
    // -----------------------------------------------------------------------

    #[test]
    fn a_dispatched_hop_walks_created_ready_busy_done_recycled_in_order() {
        let (sched, _probe) = live_sched();
        let daemon = fake_daemon(&sched, "worker");
        let task = "e2e2e2e2-e2e2-42e2-82e2-e2e2e2e2e2e2";
        sched
            .db
            .insert_task(task, ".", "worker", "", 1, "read GOAL.md and summarise")
            .unwrap();
        let mut rx = sched.bus.sender().subscribe();
        // 1. dispatch: the backend spawns, the ledger seeds created + attached.
        sched::dispatch_public(&sched, task, "worker").expect("dispatch");
        let handle = sched
            .terminals
            .lock()
            .unwrap()
            .get(task)
            .cloned()
            .expect("dispatch tracks the handle");
        let created = row(&sched, task);
        assert_eq!((created.generation, created.seq), (1, 2), "{created:?}");
        assert_eq!(created.agent_state, "booting");
        assert_eq!(created.resource_state, "attached");
        assert_eq!(created.public_lifecycle, "created");
        assert!(created.backend_ref.contains("fake"), "{created:?}");
        assert_eq!(
            sched.db.get(task).unwrap().unwrap().state,
            TaskState::Running
        );
        assert_eq!(sched.db.get(task).unwrap().unwrap().hop_state, "dispatched");
        // 2. ready: the handshake matches the reservation, the payload is
        // written through the daemon, and the tuple becomes idle.
        sched::on_ready(&sched, "worker", &handle).expect("on_ready");
        let ready = row(&sched, task);
        assert_eq!(ready.public_lifecycle, "idle");
        assert_eq!(
            ready.agent_state, "ready",
            "the ready barrier is its own agent state"
        );
        assert_eq!(ready.delivery_state, "none");
        assert_eq!(
            ready.seq, 3,
            "created, resource attach and ready are the three facts so far: {ready:?}"
        );
        let task_row = sched.db.get(task).unwrap().unwrap();
        assert_eq!(task_row.hop_state, "busy", "the payload write owns busy");
        assert_eq!(task_row.terminal, handle);
        let wrote_payload = waits_for(|| {
            daemon.lock().unwrap().requests.iter().any(|r| {
                r["op"] == "loopback"
                    && r["text"]
                        .as_str()
                        .is_some_and(|t| t.contains("read GOAL.md"))
            })
        });
        assert!(wrote_payload, "the task payload must reach the daemon");
        // 3. busy: the agent's own `turn_started` report is the working
        // evidence. The scheduler's hop already moved to busy on the payload
        // write, so a busy report from the plugin is a designed no-op, and Pi's
        // frame lands at seq 4 because the internal ladder stopped at 3. (The
        // shared watermark that decides which of the two wins has its own test.)
        let busy_report = proto::render_report(LifecycleKind::TurnStarted, task, 1, 4);
        assert!(try_report(&sched, "worker", &busy_report));
        let working = row(&sched, task);
        assert_eq!(working.agent_state, "running");
        assert_eq!(working.public_lifecycle, "working");
        assert_eq!(working.seq, 4);
        sched::on_hop_activity(&sched, task, true, false).unwrap();
        assert_eq!(
            row(&sched, task).seq,
            4,
            "busy -> busy stays inert, as the plugin is ahead of the scheduler here"
        );
        // 4. out: the handoff settles the result through the completion intent.
        let msg = crate::proto::SwarmMessage {
            header: crate::proto::SwarmHeader {
                task_id: task.into(),
                from: "worker".into(),
                transfer_send_to: String::new(),
                attempt: 1,
            },
            payload: "here is the summary".into(),
        };
        sched::on_out(&sched, "worker", &msg).expect("on_out");
        let done = row(&sched, task);
        assert_eq!(done.public_lifecycle, "exited", "{done:?}");
        assert_eq!(done.delivery_state, "accepted");
        assert_eq!(outcome_of(&sched, task), "done");
        assert_eq!(
            sched.db.get(task).unwrap().unwrap().state,
            TaskState::Closed
        );
        assert_eq!(sched.db.get(task).unwrap().unwrap().ledger_state, "done");
        // The recycle control frame was a durable intent, and it was answered
        // once by the daemon — no bare best-effort write is left on this path.
        let intents = sched.db.list_intents(None).unwrap();
        assert_eq!(intents.len(), 1, "{intents:?}");
        assert_eq!(intents[0].state, "succeeded", "{:?}", intents[0]);
        assert_eq!(intents[0].attempt, 1);
        assert!(
            daemon.lock().unwrap().requests.iter().any(|r| {
                r["text"]
                    .as_str()
                    .is_some_and(|t| t.contains("op: recycle") && t.contains(task))
            }),
            "the recycle frame must cross the socket: {:?}",
            daemon.lock().unwrap().requests
        );
        // 5. the session's own exit ack: the resource closes and the generation
        // ends. The result stays `done` underneath it.
        sched::on_recycled(&sched, task, "quit").expect("on_recycled");
        let closed = row(&sched, task);
        assert_eq!(closed.resource_state, "closed");
        assert_eq!(closed.agent_state, "gone");
        assert_eq!(closed.public_lifecycle, "exited");
        assert_eq!(outcome_of(&sched, task), "done");
        assert!(
            sched.terminals.lock().unwrap().get(task).is_none(),
            "the reclaimed tab must leave the handle map"
        );
        // The same fact an operator would read: the backend says the resource is
        // gone, the ledger says the generation exited, and the chain left exactly
        // one answered intent behind.
        let seen = crate::repair::inspect(&sched, task).expect("inspect after the chain");
        assert_eq!(seen["probe"]["state"], "gone", "{seen}");
        assert_eq!(seen["tuple"]["public_lifecycle"], "exited");
        assert_eq!(seen["tuple"]["outcome"], "done");
        assert_eq!(seen["task"]["state"], "closed");
        assert_eq!(seen["intents"].as_array().map(Vec::len), Some(1));
        assert_eq!(seen["faults"].as_array().map(Vec::len), Some(0));
        // The applied sequence, in order, with no gap and no extra write.
        let mut applied = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if event.typ == "lifecycle" {
                applied.push(event.data["event"].as_str().unwrap_or("?").to_string());
            }
        }
        assert_eq!(
            applied,
            vec![
                "created",
                "resource_attach",
                "ready",
                "turn_started",
                "complete",
                "intent_receipt",
                "heartbeat",
                "resource_closed",
            ],
            "the hop's whole lifecycle, as the reducer applied it"
        );
    }

    /// The ladder running on a generation that already has an answered recycle
    /// intent: the ceiling pass still terminates, the terminate still queues its
    /// fault, and the answered intent line stays on disk untouched — a verdict
    /// on the tuple never rewrites transport history.
    #[test]
    fn a_ladder_that_climbs_while_an_intent_is_live_still_terminates() {
        let (sched, probe) = live_sched();
        let daemon = fake_daemon(&sched, "worker");
        live_session(&sched, &probe, "mm-4", TaskState::Running);
        // This generation has already asked the daemon for a recycle, and the
        // daemon answered: one live-then-answered intent, one wire.
        let op = crate::reconcile::request_recycle(&sched, "mm-4", "close").expect("fresh intent");
        let summary = periodic_reconcile(&sched);
        assert_eq!(
            summary.intents, 0,
            "the answered line is not re-spent: {summary:?}"
        );
        let answered = intent(&sched, &op);
        assert_eq!(
            (answered.state.as_str(), answered.attempt),
            ("succeeded", 1),
            "{answered:?}"
        );
        assert_eq!(answered.kind, "recycle");
        assert_eq!(daemon.lock().unwrap().wires().len(), 1);
        assert_eq!(
            row(&sched, "mm-4").mismatch_count,
            0,
            "the tuple agrees with the probe"
        );
        // Now the resource starts disagreeing, and it never settles.
        probe.detach_panes();
        for round in 1..DEFAULT_TERMINATE_AFTER {
            let summary = periodic_reconcile(&sched);
            assert_eq!(summary.mismatch, 1, "round {round}: {summary:?}");
            assert_eq!(row(&sched, "mm-4").mismatch_count, round as i64);
            assert_eq!(
                row(&sched, "mm-4").agent_state,
                "running",
                "isolation is evidence about the resource, never a verdict on the result"
            );
            assert!(
                sched.db.list_faults(Some("mm-4")).unwrap().is_empty(),
                "no fault before the ceiling: round {round}"
            );
            assert_eq!(
                intent(&sched, &op).state,
                "succeeded",
                "the ladder must not re-spend an answered intent"
            );
            assert_eq!(
                daemon.lock().unwrap().wires().len(),
                1,
                "no second wire either"
            );
        }
        let before = row(&sched, "mm-4");
        let summary = periodic_reconcile(&sched);
        assert_eq!(summary.faults, 1, "{summary:?}");
        let after = row(&sched, "mm-4");
        assert_eq!(
            (after.agent_state.as_str(), after.public_lifecycle.as_str()),
            ("gone", "exited"),
            "{after:?}"
        );
        assert_eq!(outcome_of(&sched, "mm-4"), "failed");
        // A ceiling pass is a single applied mismatch: at `terminate_after` the
        // reducer also carries the tuple to exited+failed inside that same
        // write, so the session watermark moves exactly one slot. The terminate
        // then only books the fault row and the recovery task, which live in
        // their own tables and never advance the session seq again.
        assert_eq!(
            after.seq,
            before.seq + 1,
            "the ceiling mismatch is one applied write: {} -> {}",
            before.seq,
            after.seq
        );
        let faults = sched.db.list_faults(Some("mm-4")).unwrap();
        assert_eq!(faults.len(), 1, "{faults:?}");
        assert_eq!(faults[0].kind, "mismatch_terminate");
        assert_eq!(faults[0].state, "recovery_created");
        let kept = intent(&sched, &op);
        assert_eq!(
            (
                kept.state.as_str(),
                kept.attempt,
                kept.receipt_json.as_str()
            ),
            ("succeeded", 1, answered.receipt_json.as_str()),
            "the answered line is still exactly what it was"
        );
        // The pass after the terminate: mm-4 is closed history, and the only open
        // row is the recovery the fault created.
        let recovery_id = faults[0]
            .recovery_task_id
            .clone()
            .expect("a terminate fault must name its recovery task");
        let summary = periodic_reconcile(&sched);
        assert_eq!(summary.scanned, 2, "mm-4 and its recovery row: {summary:?}");
        assert_eq!(
            summary.open, 1,
            "the recovery row is the only open one: {summary:?}"
        );
        assert_eq!(
            summary.faults, 0,
            "a terminated task cannot fault twice: {summary:?}"
        );
        let closed = row(&sched, "mm-4");
        assert_eq!(closed.seq, after.seq, "nothing more is written to mm-4");
        assert_eq!(closed.mismatch_count, DEFAULT_TERMINATE_AFTER as i64);
        let recovery = row(&sched, &recovery_id);
        assert_eq!(recovery.public_lifecycle, "created", "{recovery:?}");
        assert_eq!(recovery.agent_state, "booting");
        assert_eq!(sched.db.list_faults(Some("mm-4")).unwrap().len(), 1);
    }

    /// #5 boundary, on stored rows: a Pi report whose version the internal ladder
    /// already spent is dropped by name, and a heartbeat that agrees with the
    /// tuple is `NoOp` — both leave the stored bytes identical, so liveness
    /// traffic cannot starve the ladder for watermark slots.
    #[test]
    fn a_report_the_ladder_already_answered_and_a_matched_heartbeat_both_write_nothing() {
        let (sched, probe) = live_sched();
        live_session(&sched, &probe, "sub-1", TaskState::Running);
        feed_turn_ended(&sched, "sub-1").unwrap();
        let done = row(&sched, "sub-1");
        assert_eq!(outcome_of(&sched, "sub-1"), "pending");
        // Pi's `complete` frame for the very version the ladder wrote.
        let verdict = apply_report(
            &sched,
            &report(
                LifecycleKind::Complete,
                "sub-1",
                done.generation as u64,
                done.seq as u64,
            ),
        )
        .expect("a well-formed frame must reach the reducer");
        assert!(
            matches!(
                verdict,
                Verdict::Ignored(IgnoredReason::StaleOrDuplicateSeq)
            ),
            "the colliding report must be named, not silently dropped: {verdict:?}"
        );
        let after = row(&sched, "sub-1");
        assert_eq!(
            (
                after.seq,
                after.observed_json.as_str(),
                after.desired_json.as_str()
            ),
            (
                done.seq,
                done.observed_json.as_str(),
                done.desired_json.as_str()
            ),
            "a dropped report writes nothing at all"
        );
        // A heartbeat one slot ahead: the reducer has no body to fold in, the
        // tuple already agrees with itself, and the verdict says `NoOp`.
        let next = next_version(&sched, "sub-1").unwrap();
        let verdict = apply_report(
            &sched,
            &report(LifecycleKind::Heartbeat, "sub-1", next.generation, next.seq),
        )
        .expect("parsed and reduced");
        assert!(
            matches!(verdict, Verdict::Ignored(IgnoredReason::NoOp)),
            "an empty-body liveness frame that changes no dimension is a no-op: {verdict:?}"
        );
        let quiet = row(&sched, "sub-1");
        assert_eq!(
            (
                quiet.seq,
                quiet.observed_json.as_str(),
                quiet.desired_json.as_str(),
                quiet.mismatch_count
            ),
            (
                after.seq,
                after.observed_json.as_str(),
                after.desired_json.as_str(),
                after.mismatch_count
            ),
            "and it costs no watermark slot"
        );
        // Isolation changes that answer: with a mismatch recorded, the same
        // heartbeat is a real write — it heals the counter.
        feed_mismatch(&sched, "sub-1").unwrap();
        let next = next_version(&sched, "sub-1").unwrap();
        let verdict = apply_report(
            &sched,
            &report(LifecycleKind::Snapshot, "sub-1", next.generation, next.seq),
        )
        .expect("parsed and reduced");
        assert!(matches!(verdict, Verdict::Applied(_)), "{verdict:?}");
        let healed = row(&sched, "sub-1");
        assert_eq!(healed.mismatch_count, 0, "the snapshot healed the counter");
        assert_eq!(healed.seq, next.seq as i64, "the heal write spent the slot");
    }

    /// #5 boundary: the two coupling rules the frozen reducer owns, exercised on
    /// a tuple this crate stores. Cancel with a delivery in flight is
    /// `cancel_running` and never touches the result; adoption carries the
    /// pending delivery into the new generation.
    #[test]
    fn cancel_leaves_an_already_settled_result_alone() {
        let (sched, probe) = live_sched();
        live_session(&sched, &probe, "cxl-1", TaskState::Running);
        feed_turn_started(&sched, "cxl-1").unwrap();
        // The out handoff is complete: the ledger holds `done` and the delivery
        // was consumed (Complete + IntentReceipt + settle(Done)).
        feed_delivered(&sched, "cxl-1").unwrap();
        let before = stored_observation(&sched, Some(&row(&sched, "cxl-1")));
        assert!(matches!(before.outcome, Outcome::Done), "{before:?}");
        assert_eq!(outcome_of(&sched, "cxl-1"), "done");
        // §2.3: a cancel that arrives after the result already settled is a
        // no-op. The reducer's Cancel arm only moves a Pending outcome, so a
        // Done result survives untouched and nothing is written to the ledger.
        let cancelled = feed_cancel(&sched, "cxl-1").unwrap();
        assert!(
            matches!(cancelled, Verdict::Ignored(IgnoredReason::NoOp)),
            "cancel on a settled result must be a no-op: {cancelled:?}"
        );
        let after = row(&sched, "cxl-1");
        assert_eq!(
            after.seq, before.version.seq as i64,
            "the no-op writes no slot"
        );
        let reread = stored_observation(&sched, Some(&after));
        assert_eq!(
            (reread.version.generation, reread.version.seq),
            (before.version.generation, before.version.seq),
            "the no-op advances no watermark"
        );
        assert_eq!(
            reread.delivery, before.delivery,
            "the tuple delivery is unchanged"
        );
        assert!(matches!(reread.outcome, Outcome::Done), "{reread:?}");
        assert_eq!(
            outcome_of(&sched, "cxl-1"),
            "done",
            "the ledger result is unchanged"
        );
    }

    #[test]
    fn adoption_starts_a_clean_generation_and_releases_the_dead_delivery() {
        let (sched, probe) = live_sched();
        live_session(&sched, &probe, "cxl-2", TaskState::Running);
        // An out handoff is owed but unanswered: the delivery is open (pending)
        // while the result is still pending, which is the only legal home for a
        // pending delivery.
        feed_intent_pending(&sched, "cxl-2");
        assert_eq!(row(&sched, "cxl-2").delivery_state, "pending");
        // §2.3: cancel closes the result dimension only. The reducer's Cancel arm
        // moves Pending -> Cancelled and leaves the open delivery exactly where
        // it was, so the owed handoff is still owed after the cancel.
        let cancelled = feed_cancel(&sched, "cxl-2").unwrap();
        let next = match cancelled {
            Verdict::Applied(next) => next,
            other => panic!("cancel on a pending result must apply: {other:?}"),
        };
        assert!(matches!(next.outcome, Outcome::Cancelled), "{next:?}");
        assert_eq!(
            next.delivery,
            DeliveryState::Pending,
            "cancel leaves the open delivery alone"
        );
        assert_eq!(row(&sched, "cxl-2").delivery_state, "pending");
        // Prove the generation dead, then adopt it forward. Adoption opens a
        // fresh, clean generation: the frozen reducer moves the watermark and
        // resets the volatile dimensions, so the dead generation's owed handoff
        // does NOT ride into the new one (its resend is owned by the intent and
        // recycle machinery, not the session tuple).
        probe.kill("cxl-2");
        feed_agent_gone(&sched, "cxl-2").unwrap();
        // Adoption opens the next generation at seq 0, the same version repair's
        // `adopt` action allocates (Version::new(generation + 1, 0)).
        let gen = row(&sched, "cxl-2").generation;
        let version = Version::new(gen.max(0) as u64 + 1, 0);
        let Verdict::Applied(adopted) = apply_persist(
            &sched,
            "cxl-2",
            &LifecycleEvent::AdoptNewGeneration { v: version },
        )
        .expect("persist adoption") else {
            panic!("adoption must apply on a proven-dead generation");
        };
        assert!(
            matches!(adopted.agent, AgentState::Booting),
            "the new generation starts clean: {adopted:?}"
        );
        assert_eq!(
            adopted.delivery,
            DeliveryState::None,
            "adoption releases the dead generation's open delivery"
        );
        assert!(matches!(adopted.outcome, Outcome::Pending), "{adopted:?}");
        let after = row(&sched, "cxl-2");
        assert_eq!(
            (after.generation, after.seq),
            (2, 0),
            "adoption moves the watermark"
        );
        assert_eq!(
            after.delivery_state, "none",
            "the new generation owes no handoff yet"
        );
    }

    /// The #18 acceptance walk: one FakeBackend scheduler exercises the public
    /// dispatch/report/daemon path, crash adoption, the mismatch ladder, intent
    /// exhaustion, recovery lineage, and report watermark idempotence in one
    /// durable ledger.
    #[test]
    fn fake_backend_reconcile_end_to_end_walks_the_supervision_matrix() {
        let (sched, probe) = live_sched();
        let daemon = fake_daemon(&sched, "worker");
        let mut rx = sched.bus.sender().subscribe();
        sched
            .db
            .insert_task("e2e-worker", ".", "worker", "", 1, "finish e2e")
            .unwrap();
        sched
            .db
            .insert_task("e2e-sibling", ".", "worker", "", 1, "keep running")
            .unwrap();
        sched::dispatch_public(&sched, "e2e-worker", "worker").expect("dispatch worker");
        sched::dispatch_public(&sched, "e2e-sibling", "worker").expect("dispatch sibling");
        let worker_handle = sched
            .terminals
            .lock()
            .unwrap()
            .get("e2e-worker")
            .cloned()
            .expect("worker handle");

        // Startup sees the two backend-live sessions and adopts them instead of
        // calling their pre-ready transition residue failed.
        let startup = startup_reconcile(&sched);
        assert_eq!(startup.scanned, 2, "{startup:?}");
        assert_eq!(startup.open, 2, "{startup:?}");
        assert_eq!(startup.faults, 0, "{startup:?}");
        assert_eq!(sched.db.counts().unwrap()["running"], 2);

        // Ready -> turn started -> out -> recycle ack follows public events.
        sched::on_ready(&sched, "worker", &worker_handle).expect("worker ready");
        let ready = row(&sched, "e2e-worker");
        assert_eq!(ready.public_lifecycle, "idle", "{ready:?}");
        assert_eq!(ready.resource_state, "attached");
        assert_eq!(ready.agent_state, "ready");
        let turn_version = next_version(&sched, "e2e-worker").unwrap();
        let turn = feed_turn_started(&sched, "e2e-worker").expect("worker turn started");
        assert!(matches!(turn, Verdict::Applied(_)), "{turn:?}");
        let working = row(&sched, "e2e-worker");
        assert_eq!(working.public_lifecycle, "working", "{working:?}");
        assert_eq!(working.agent_state, "running");
        assert_eq!(working.seq, turn_version.seq as i64);

        // A replay and a stale generation cost no watermark slot and no write.
        let working_before_replay = working.observed_json.clone();
        let replay = feed_turn_started(&sched, "e2e-worker").expect("duplicate turn");
        assert!(matches!(replay, Verdict::Ignored(_)), "{replay:?}");
        assert_eq!(
            row(&sched, "e2e-worker").observed_json,
            working_before_replay,
            "a duplicate report is idempotent"
        );
        let stale = apply_report(
            &sched,
            &report(LifecycleKind::TurnStarted, "e2e-worker", 0, 99),
        )
        .expect("stale generation report");
        assert!(matches!(stale, Verdict::Ignored(_)), "{stale:?}");
        assert_eq!(
            row(&sched, "e2e-worker").observed_json,
            working_before_replay,
            "a stale generation is idempotent"
        );

        // The handoff is consumed by the task ledger and settled by reconcile.
        let out = crate::proto::SwarmMessage {
            header: crate::proto::SwarmHeader {
                task_id: "e2e-worker".into(),
                from: "worker".into(),
                transfer_send_to: String::new(),
                attempt: 1,
            },
            payload: "e2e result".into(),
        };
        sched::on_out(&sched, "worker", &out).expect("worker out");
        assert_eq!(
            sched.db.get("e2e-worker").unwrap().unwrap().state,
            TaskState::Closed
        );
        let mirror_version = next_version(&sched, "e2e-worker").unwrap();
        let mirror = apply_report(
            &sched,
            &report(
                LifecycleKind::Heartbeat,
                "e2e-worker",
                mirror_version.generation,
                mirror_version.seq,
            ),
        )
        .expect("heartbeat frame");
        assert!(
            matches!(mirror, Verdict::Ignored(_)),
            "the closed tuple rejects later evidence: {mirror:?}"
        );
        assert_eq!(outcome_of(&sched, "e2e-worker"), "done");

        sched::on_recycled(&sched, "e2e-worker", "e2e done").expect("worker recycle ack");
        let closed = row(&sched, "e2e-worker");
        assert_eq!(closed.public_lifecycle, "exited", "{closed:?}");
        assert_eq!(closed.agent_state, "gone");
        assert_eq!(closed.resource_state, "closed");
        assert!(sched.terminals.lock().unwrap().get("e2e-worker").is_none());
        assert!(sched.db.list_faults(Some("e2e-worker")).unwrap().is_empty());

        // A live sibling remains independent while the next generation enters
        // the mismatch ladder. The switch is backend-wide, so first settle the
        // sibling; its result becomes history and cannot be counted twice.
        let sibling_handle = sched
            .terminals
            .lock()
            .unwrap()
            .get("e2e-sibling")
            .cloned()
            .expect("sibling handle");
        sched::on_ready(&sched, "worker", &sibling_handle).expect("sibling ready");
        let sibling_out = crate::proto::SwarmMessage {
            header: crate::proto::SwarmHeader {
                task_id: "e2e-sibling".into(),
                from: "worker".into(),
                transfer_send_to: String::new(),
                attempt: 1,
            },
            payload: "sibling result".into(),
        };
        sched::on_out(&sched, "worker", &sibling_out).expect("sibling out");
        sched::on_recycled(&sched, "e2e-sibling", "sibling done").expect("sibling recycle");
        assert_eq!(outcome_of(&sched, "e2e-sibling"), "done");

        sched
            .db
            .insert_task("e2e-retry", ".", "worker", "", 1, "retry e2e")
            .unwrap();
        sched::dispatch_public(&sched, "e2e-retry", "worker").expect("dispatch retry");
        probe.detach_panes();
        for round in 1..DEFAULT_TERMINATE_AFTER {
            let summary = periodic_reconcile(&sched);
            assert_eq!(summary.mismatch, 1, "round {round}: {summary:?}");
            assert_eq!(row(&sched, "e2e-retry").mismatch_count, round as i64);
        }
        let ceiling = periodic_reconcile(&sched);
        assert_eq!(ceiling.faults, 1, "{ceiling:?}");
        assert_eq!(ceiling.recoveries, 1, "{ceiling:?}");
        let retry = row(&sched, "e2e-retry");
        assert_eq!(retry.public_lifecycle, "exited", "{retry:?}");
        assert_eq!(outcome_of(&sched, "e2e-retry"), "failed");
        let faults = sched.db.list_faults(Some("e2e-retry")).unwrap();
        assert_eq!(faults.len(), 1, "{faults:?}");
        assert_eq!(faults[0].kind, "mismatch_terminate");
        assert_eq!(faults[0].state, "recovery_created");
        let recovery_id = faults[0]
            .recovery_task_id
            .clone()
            .expect("terminate creates recovery");
        let recovery = sched.db.get(&recovery_id).unwrap().expect("recovery task");
        assert_eq!(recovery.kind, "recovery");
        assert_eq!(
            recovery.failure_of.as_deref(),
            Some(faults[0].id.to_string().as_str())
        );

        // The recovery task's daemon is gone, so its recycle intent burns the
        // full transport budget and the single-layer guard leaves its own fault
        // for the root supervisor.
        probe.bring_up(&recovery_id);
        sched.sessions.lock().unwrap().insert(
            recovery_id.clone(),
            SessionRef {
                task_id: recovery_id.clone(),
                backend: "fake".into(),
                backend_ref: serde_json::json!({"handle": format!("term-{recovery_id}")}),
                generation: 1,
            },
        );
        let recovery_op = request_recycle(&sched, &recovery_id, "close").expect("recovery intent");
        let first_attempt = intent(&sched, &recovery_op);
        assert_eq!(first_attempt.attempt, 1);
        let second = pump_intents(&sched, first_attempt.next_attempt_at);
        assert_eq!(second.retried, 1, "{second:?}");
        let spent = pump_intents(&sched, intent(&sched, &recovery_op).next_attempt_at);
        assert_eq!(spent.exhausted, 1, "{spent:?}");
        let exhausted = intent(&sched, &recovery_op);
        assert_eq!(exhausted.state, "exhausted", "{exhausted:?}");
        assert_eq!(exhausted.attempt, 3);
        assert_eq!(row(&sched, &recovery_id).delivery_state, "exhausted");
        assert_eq!(
            daemon
                .lock()
                .unwrap()
                .requests
                .iter()
                .filter(|request| {
                    request["op"] == "loopback"
                        && request["text"]
                            .as_str()
                            .is_some_and(|text| text.contains("op: recycle"))
                })
                .count(),
            2,
            "three successful deliveries and three exhausted budget attempts reach the daemon"
        );
        let recovery_faults = sched.db.list_faults(Some(&recovery_id)).unwrap();
        assert_eq!(recovery_faults.len(), 1, "{recovery_faults:?}");
        assert_eq!(recovery_faults[0].kind, "intent_exhausted");
        assert_eq!(recovery_faults[0].state, "open");
        assert!(recovery_faults[0].recovery_task_id.is_none());
        assert_eq!(sched.db.list_faults(None).unwrap().len(), 2);

        // A second Pi generation stays parked until reconcile proves the old
        // resource gone, adoption resets its watermark, and repair stamps the
        // operator action.
        let later = apply_report(&sched, &report(LifecycleKind::Ready, "e2e-retry", 2, 1))
            .expect("generation report");
        assert!(
            matches!(
                later,
                Verdict::Rejected(crate::lifecycle::RejectReason::UnadoptedGeneration)
            ),
            "{later:?}"
        );
        assert_eq!(row(&sched, "e2e-retry").generation, 1);
        let summary = periodic_reconcile(&sched);
        assert_eq!(summary.dead, 0, "previous detach already counted this row");
        probe.kill("e2e-retry");
        let seen = crate::repair::inspect(&sched, "e2e-retry").expect("inspect generation");
        assert_eq!(seen["probe"]["state"], "gone", "{seen}");
        let adopted = crate::repair::adopt(&sched, "e2e-retry").expect("adopt generation");
        assert_eq!(adopted["generation"], 2, "{adopted}");
        let accepted = apply_report(&sched, &report(LifecycleKind::Ready, "e2e-retry", 2, 1))
            .expect("post-adopt generation report");
        assert!(matches!(accepted, Verdict::Applied(_)), "{accepted:?}");
        let adopted_row = row(&sched, "e2e-retry");
        assert_eq!((adopted_row.generation, adopted_row.seq), (2, 1));
        assert_eq!(adopted_row.agent_state, "ready");
        assert!(
            sched
                .db
                .operator_revision("e2e-retry")
                .unwrap()
                .is_some_and(|stamp| stamp.contains("adopt")),
            "adoption must leave an operator revision"
        );

        let mut lifecycle = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if event.typ == "lifecycle" {
                lifecycle.push((
                    event.data["task_id"].as_str().unwrap_or("?").to_string(),
                    event.data["from"].as_str().unwrap_or("?").to_string(),
                    event.data["to"].as_str().unwrap_or("?").to_string(),
                ));
            }
        }
        assert!(
            lifecycle.contains(&("e2e-worker".into(), "working".into(), "exited".into())),
            "{lifecycle:?}"
        );
        assert!(
            lifecycle
                .iter()
                .any(|(task, from, to)| task == "e2e-retry" && from == "created" && to == "exited"),
            "{lifecycle:?}"
        );
        assert!(
            lifecycle.contains(&("e2e-retry".into(), "exited".into(), "created".into())),
            "{lifecycle:?}"
        );
        assert_eq!(
            sched.db.get("e2e-worker").unwrap().unwrap().state,
            TaskState::Closed
        );
        assert_eq!(
            sched.db.get("e2e-sibling").unwrap().unwrap().state,
            TaskState::Closed
        );
    }
}
