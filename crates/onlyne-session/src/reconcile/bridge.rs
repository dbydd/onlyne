use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::json;

use crate::backend::SessionRef;
use crate::lifecycle::{self, LifecycleEvent, Observation, Verdict, Version};

use super::fault::{DEFAULT_ISOLATE_AFTER, DEFAULT_TERMINATE_AFTER};
use super::ledger::SessionLedger;
use super::record::{SessionRecord, backend_ref_json, stored_observation, to_versioned};

/// A bridge instance: the reducer facts plus the ledger and the live session
/// map it resolves probe targets from.
#[derive(Debug, Default)]
pub struct Bridge {
    pub(super) live: Mutex<HashMap<String, SessionRef>>,
}

impl Bridge {
    pub fn new() -> Self {
        Self::default()
    }

    /// Remember a live session ref. The bridge prefers it over the stored
    /// reference when it names the backend resource.
    pub fn track_live(&self, session: SessionRef) {
        self.live
            .lock()
            .unwrap()
            .insert(session.task_id.clone(), session);
    }

    /// Forget a live session ref.
    pub fn untrack_live(&self, task_id: &str) {
        self.live.lock().unwrap().remove(task_id);
    }
}

pub(super) fn initial_observation() -> Observation {
    Observation::initial(DEFAULT_ISOLATE_AFTER, DEFAULT_TERMINATE_AFTER)
}

/// The version an event carries.
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
/// `Observation::initial` already is the post-`Created` tuple, so for a task
/// with no stored row it seeds the row at the event's version. Use
/// [`feed_created`] for the idempotent form.
pub fn apply_persist(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
    event: &LifecycleEvent,
) -> anyhow::Result<Verdict> {
    let row = ledger.get_session(task_id)?;
    if row.is_none() {
        let known =
            ledger.task_is_known(task_id)? || bridge.live.lock().unwrap().contains_key(task_id);
        if !known {
            tracing::warn!(
                task = %task_id,
                event = event_name(event),
                "lifecycle event for a session never tracked; nothing persisted"
            );
            anyhow::bail!("unknown session {task_id}");
        }
    }
    let current = stored_observation(ledger, row.as_ref());
    if row.is_none() && matches!(event, LifecycleEvent::Created { .. }) {
        return Ok(Verdict::Applied(seed_created(
            bridge,
            ledger,
            task_id,
            version_of(event),
        )?));
    }
    let verdict = lifecycle::apply(&current, event);
    record_verdict(
        bridge,
        ledger,
        task_id,
        row.as_ref(),
        event,
        &current,
        &verdict,
    )?;
    Ok(verdict)
}

/// Write the `Created` seed row. The reducer defines no transition into
/// `Booting` from `Booting` — `initial()` is already the created tuple — so the
/// seed is written directly at the event's version.
fn seed_created(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
    version: Version,
) -> anyhow::Result<Observation> {
    let mut seeded = initial_observation();
    seeded.version = version;
    let backend_ref = backend_ref_json(bridge, task_id, None);
    let desired = serde_json::to_string(&LifecycleEvent::Created { v: version })?;
    let stored = to_versioned(&seeded, &backend_ref, &desired)?;
    if ledger.upsert_session(task_id, &stored)? {
        ledger.emit("lifecycle", transition_payload(task_id, &seeded, "created"));
    } else {
        tracing::warn!(
            task = %task_id,
            "a newer session row appeared while seeding Created; kept the newer watermark"
        );
    }
    Ok(seeded)
}

/// One transition on the bus: the full session tuple the reducer settled on.
/// No public view rides it — that needs the task state, which is not a session
/// fact, so a subscriber that shows one derives it from these dimensions plus
/// the task it is tracking.
fn transition_payload(task_id: &str, to: &Observation, event: &str) -> serde_json::Value {
    json!({
        "task_id": task_id,
        "agent": to.agent,
        "delivery": to.delivery,
        "resource": to.resource,
        "recovery": to.recovery,
        "generation": to.version.generation,
        "seq": to.version.seq,
        "event": event,
    })
}

#[allow(clippy::too_many_arguments)]
fn record_verdict(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
    row: Option<&SessionRecord>,
    event: &LifecycleEvent,
    current: &Observation,
    verdict: &Verdict,
) -> anyhow::Result<()> {
    match verdict {
        Verdict::Applied(next) => {
            let backend_ref = backend_ref_json(bridge, task_id, row);
            let desired = serde_json::to_string(event)?;
            let stored = to_versioned(next, &backend_ref, &desired)?;
            if !ledger.upsert_session(task_id, &stored)? {
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
                agent = ?next.agent,
                delivery = ?next.delivery,
                resource = ?next.resource,
                recovery = ?next.recovery,
                generation = next.version.generation,
                seq = next.version.seq,
                "lifecycle transition applied"
            );
            ledger.emit(
                "lifecycle",
                transition_payload(task_id, next, event_name(event)),
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

/// The next version for a session observed locally: same generation, one
/// sequence past the stored watermark.
pub fn next_version(ledger: &dyn SessionLedger, task_id: &str) -> anyhow::Result<Version> {
    let row = ledger.get_session(task_id)?;
    let current = stored_observation(ledger, row.as_ref());
    Ok(Version::new(
        current.version.generation,
        current.version.seq.saturating_add(1),
    ))
}

/// Reduce an event whose version the caller allocates itself. Every feed helper
/// below goes through here, so an event that arrives twice is dropped by the
/// reducer's watermark.
pub fn apply_at_next(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
    make: impl FnOnce(Version) -> LifecycleEvent,
) -> anyhow::Result<Verdict> {
    let version = next_version(ledger, task_id)?;
    apply_persist(bridge, ledger, task_id, &make(version))
}

/// Best-effort feed for local observation points.
pub fn try_feed(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
    make: impl FnOnce(Version) -> LifecycleEvent,
) {
    if let Err(err) = apply_at_next(bridge, ledger, task_id, make) {
        tracing::warn!(
            task = %task_id,
            error = %err,
            "could not record a lifecycle event in the session ledger"
        );
    }
}
