use std::collections::HashMap;

use parking_lot::{Mutex, MutexGuard};

use serde_json::json;

use crate::backend::SessionRef;
use onlyne_proto::lifecycle::{self, LifecycleEvent, Observation, Verdict, Version};

use super::fault::{DEFAULT_ISOLATE_AFTER, DEFAULT_TERMINATE_AFTER};
use onlyne_store::session::{SessionLedger, SessionRecord};

use super::record::{backend_ref_json, short, stored_observation, to_versioned};

/// A bridge instance: the reducer facts plus the ledger and the live session
/// map it resolves probe targets from.
#[derive(Debug, Default)]
pub struct Bridge {
    pub(super) live: Mutex<HashMap<String, SessionRef>>,
    /// One session transaction at a time: the stored watermark is read, reduced
    /// and written back while this is held, so the writers that share a bridge —
    /// the plugin's beat, the stall clock, the settle, the sweep — cannot be
    /// overtaken inside one another's window. The section spans two ledger
    /// round-trips, which the store serializes anyway. A lock taken before any
    /// other bridge lock and never taken from inside a ledger call, so the
    /// ordering `applying` → `live` is the only one that can form. Unpoisoned by
    /// construction: a panic inside a ledger call leaves the gate behind rather
    /// than turning the next apply into a second panic.
    applying: Mutex<()>,
}

impl Bridge {
    pub fn new() -> Self {
        Self::default()
    }

    /// Enter a reduce-and-persist transaction. Every public entry point in this
    /// module takes the gate exactly once, at its own boundary, and the inner
    /// rounds never re-enter it.
    fn applying(&self) -> MutexGuard<'_, ()> {
        self.applying.lock()
    }

    /// Remember a live session ref. The bridge prefers it over the stored
    /// reference when it names the backend resource.
    pub fn track_live(&self, session: SessionRef) {
        self.live.lock().insert(session.task_id.clone(), session);
    }

    /// Forget a live session ref.
    pub fn untrack_live(&self, task_id: &str) {
        self.live.lock().remove(task_id);
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
        | LifecycleEvent::Suspend { v }
        | LifecycleEvent::Resume { v }
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
        LifecycleEvent::Suspend { .. } => "suspend",
        LifecycleEvent::Resume { .. } => "resume",
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
/// The read, the reduce and the write are one transaction on the bridge's apply
/// gate, so a writer that shares this bridge cannot take the watermark between
/// them. A loss to a writer this bridge cannot see — a second client on the same
/// store — is not swallowed: the caller handed over its own version, so there is
/// no newer one to allocate on its behalf, and the loss is escalated instead
/// (see [`report_lost_write`]).
///
/// `Applied` writes the row (unless a newer watermark already carries it) and emits one
/// `lifecycle` event describing the transition.
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
    let (verdict, landed) = apply_persist_reported(bridge, ledger, task_id, event)?;
    if !landed {
        report_lost_write(ledger, task_id, event, &verdict, 1);
    }
    Ok(verdict)
}

/// Reduce and persist, reporting alongside the verdict whether the write landed.
///
/// One implementation of the write, shared by every path. `false` means a
/// transition the reducer accepted whose write lost the watermark; a verdict of
/// `Ignored` or `Rejected` has nothing to write and reports `true`.
fn apply_persist_reported(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
    event: &LifecycleEvent,
) -> anyhow::Result<(Verdict, bool)> {
    let _gate = bridge.applying();
    let row = ledger.get_session(task_id)?;
    apply_round(bridge, ledger, task_id, row.as_ref(), event)
}

/// One reduce-and-persist round against `row`, the watermark the caller read
/// inside the same transaction. The gate is held by the caller, so this is the
/// only place the write's answer is produced.
fn apply_round(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
    row: Option<&SessionRecord>,
    event: &LifecycleEvent,
) -> anyhow::Result<(Verdict, bool)> {
    if row.is_none() {
        let known = ledger.task_is_known(task_id)? || bridge.live.lock().contains_key(task_id);
        if !known {
            tracing::warn!(
                task = %task_id,
                event = event_name(event),
                "lifecycle event for a session never tracked; nothing persisted"
            );
            anyhow::bail!("unknown session {task_id}");
        }
    }
    let current = stored_observation(ledger, row);
    if row.is_none() && matches!(event, LifecycleEvent::Created { .. }) {
        let (seeded, landed) = seed_created(bridge, ledger, task_id, version_of(event))?;
        return Ok((Verdict::Applied(seeded), landed));
    }
    // An adoption is the one event the reducer exempts from the sequence gate,
    // and the exemption does not reach the ledger: `upsert_session` refuses
    // anything not strictly newer. Restamping is what makes the two gates agree.
    let stamped = if row.is_some() {
        stamp_adoption(event, &current)
    } else {
        None
    };
    if let Some(adopting) = stamped.as_ref() {
        tracing::warn!(
            task = %task_id,
            event = event_name(event),
            from = version_of(event).seq,
            to = version_of(adopting).seq,
            watermark = current.version.seq,
            "adoption carried a sequence the stored watermark had passed; advanced it past the gate"
        );
    }
    let event = stamped.as_ref().unwrap_or(event);
    let verdict = lifecycle::apply(&current, event);
    let landed = record_verdict(bridge, ledger, task_id, row, event, &current, &verdict)?;
    Ok((verdict, landed))
}

/// Re-version an adoption that the write gate would refuse.
///
/// `AdoptNewGeneration` and `Supersede` claim a generation, not a point in its
/// sequence: the reducer says so by letting them through the same-generation
/// sequence gate. A generation that is already the stored one is the case where
/// that leaves the version at or behind the watermark the ledger compares, so
/// the reducer answers `Applied` and `upsert_session` refuses the row — the
/// adoption lands nowhere and the caller is told it did. Moving it one sequence
/// past the stored watermark keeps the claim (same generation, newer sequence)
/// and gives the gate an answer it can take. A generation strictly behind the
/// stored one is not restamped: the reducer's own staleness gate is the right
/// answer there.
fn stamp_adoption(event: &LifecycleEvent, current: &Observation) -> Option<LifecycleEvent> {
    let v = version_of(event);
    let adoption = matches!(
        event,
        LifecycleEvent::AdoptNewGeneration { .. } | LifecycleEvent::Supersede { .. }
    );
    if !adoption || v.generation != current.version.generation || v.seq > current.version.seq {
        return None;
    }
    let newer = Version::new(v.generation, current.version.seq.saturating_add(1));
    Some(match event {
        LifecycleEvent::Supersede {
            old_generation_dead,
            body,
            ..
        } => LifecycleEvent::Supersede {
            v: newer,
            old_generation_dead: *old_generation_dead,
            body: body.clone(),
        },
        _ => LifecycleEvent::AdoptNewGeneration { v: newer },
    })
}

/// Escalate an accepted transition whose write the ledger refused, once retries
/// cannot save it. The warn naming the loss is `record_verdict`'s; this is the
/// part that reaches outside the process: an operator line on the client's alert
/// surface and one event on the bus, so the server projecting this row learns
/// the tuple it is showing is not the tuple the reducer settled on.
fn report_lost_write(
    ledger: &dyn SessionLedger,
    task_id: &str,
    event: &LifecycleEvent,
    verdict: &Verdict,
    attempts: usize,
) {
    let v = version_of(event);
    tracing::warn!(
        task = %task_id,
        event = event_name(event),
        generation = v.generation,
        seq = v.seq,
        attempts,
        applied = matches!(verdict, Verdict::Applied(_)),
        "session transition did not land; the row keeps the older tuple"
    );
    ledger.note_alert(format!(
        "session {} lost its {} write to a newer watermark",
        short(task_id),
        event_name(event)
    ));
    ledger.emit(
        "lifecycle_write_lost",
        json!({
            "task_id": task_id,
            "event": event_name(event),
            "generation": v.generation,
            "seq": v.seq,
            "attempts": attempts,
        }),
    );
}

/// Write the `Created` seed row. The reducer defines no transition into
/// `Booting` from `Booting` — `initial()` is already the created tuple — so the
/// seed is written directly at the event's version.
fn seed_created(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
    version: Version,
) -> anyhow::Result<(Observation, bool)> {
    let mut seeded = initial_observation();
    seeded.version = version;
    let backend_ref = backend_ref_json(bridge, task_id, None);
    let desired = serde_json::to_string(&LifecycleEvent::Created { v: version })?;
    let stored = to_versioned(&seeded, &backend_ref, &desired)?;
    if ledger.upsert_session(task_id, &stored)? {
        ledger.emit("lifecycle", transition_payload(task_id, &seeded, "created"));
        Ok((seeded, true))
    } else {
        tracing::warn!(
            task = %task_id,
            "a newer session row appeared while seeding Created; kept the newer watermark"
        );
        Ok((seeded, false))
    }
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
) -> anyhow::Result<bool> {
    match verdict {
        Verdict::Applied(next) => {
            let backend_ref = backend_ref_json(bridge, task_id, row);
            let desired = serde_json::to_string(event)?;
            let stored = to_versioned(next, &backend_ref, &desired)?;
            if !ledger.upsert_session(task_id, &stored)? {
                tracing::warn!(
                    task = %task_id,
                    event = event_name(event),
                    generation = next.version.generation,
                    seq = next.version.seq,
                    "session write lost to a newer watermark; left the row alone"
                );
                return Ok(false);
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
            Ok(true)
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
            Ok(true)
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
            Ok(true)
        }
    }
}

/// The next version for a session observed locally: same generation, one
/// sequence past the stored watermark.
///
/// A standalone read, and the caller's write is its own transaction: a writer
/// can take the watermark between the two. The bridge's local writers allocate
/// inside their transaction instead of going through here.
pub fn next_version(ledger: &dyn SessionLedger, task_id: &str) -> anyhow::Result<Version> {
    let row = ledger.get_session(task_id)?;
    let current = stored_observation(ledger, row.as_ref());
    Ok(next_after(&current))
}

/// One sequence past a watermark, in the watermark's own generation.
fn next_after(current: &Observation) -> Version {
    Version::new(
        current.version.generation,
        current.version.seq.saturating_add(1),
    )
}

/// Reduce an event whose version is the stored watermark plus one. Every feed
/// helper below goes through here, so an event that arrives twice is dropped by
/// the reducer's watermark.
pub fn apply_at_next(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
    make: impl FnMut(Version) -> LifecycleEvent,
) -> anyhow::Result<Verdict> {
    let mut make = make;
    apply_locally(bridge, ledger, task_id, |_, v| make(v))
}

/// Reduce an event the caller composes from the stored tuple.
///
/// For a caller whose body is derived from the row — a settlement replays the
/// tuple it read with one dimension closed — reading outside the transaction is
/// the race: the tuple it composed against can be the one the watermark has
/// already left, and the reducer then answers a version its own reader never
/// saw. Here the tuple and the version come from the same read that holds the
/// apply gate, which is the only read that can promise either.
pub fn apply_from_stored(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
    make: impl FnMut(&Observation, Version) -> LifecycleEvent,
) -> anyhow::Result<Verdict> {
    apply_locally(bridge, ledger, task_id, make)
}

/// The local transaction: read the row, let the caller compose its event from
/// what that read shows, reduce, and write — all of it inside the apply gate, so
/// the writers sharing this bridge cannot overtake each other between the read
/// and the write. A write that still loses was taken by a writer this bridge
/// cannot see (a second client on the same store); the round is replayed on the
/// fresher row, bounded by [`APPLY_ATTEMPTS`], and the loss is escalated when the
/// budget is spent. The reducer's own gate is untouched: a duplicate or an older
/// event is still dropped, and a verdict of `Ignored` or `Rejected` has nothing
/// to replay.
fn apply_locally(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
    mut make: impl FnMut(&Observation, Version) -> LifecycleEvent,
) -> anyhow::Result<Verdict> {
    let _gate = bridge.applying();
    let mut attempt = 1;
    loop {
        let row = ledger.get_session(task_id)?;
        let current = stored_observation(ledger, row.as_ref());
        let event = make(&current, next_after(&current));
        let (verdict, landed) = apply_round(bridge, ledger, task_id, row.as_ref(), &event)?;
        if landed {
            return Ok(verdict);
        }
        if attempt == APPLY_ATTEMPTS {
            report_lost_write(ledger, task_id, &event, &verdict, attempt);
            return Ok(verdict);
        }
        tracing::debug!(
            task = %task_id,
            event = event_name(&event),
            attempt,
            "write lost to a competing watermark; replaying the round on the fresher row"
        );
        attempt += 1;
    }
}

/// Attempts one local event gets to land its write. Four is the number that
/// covers the writers that can share one store — the plugin's beat, the stall
/// clock, the settle, and the sweep — one retry each, after which the loss is
/// escalated rather than looped over.
const APPLY_ATTEMPTS: usize = 4;

/// Best-effort feed for local observation points.
pub fn try_feed(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
    make: impl FnMut(Version) -> LifecycleEvent,
) {
    if let Err(err) = apply_at_next(bridge, ledger, task_id, make) {
        tracing::warn!(
            task = %task_id,
            error = %err,
            "could not record a lifecycle event in the session ledger"
        );
    }
}
