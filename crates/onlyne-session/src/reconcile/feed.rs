use crate::lifecycle::{
    DeliveryState, IgnoredReason, LifecycleEvent, Observation, RecoveryState, Verdict, Version,
};

use super::bridge::{Bridge, apply_at_next, apply_persist};
use super::ledger::SessionLedger;
use super::record::stored_observation;

/// Seed the session row for a task just given a resource. Idempotent: a row
/// that already exists keeps its own history.
pub fn feed_created(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
) -> anyhow::Result<Verdict> {
    if ledger.get_session(task_id)?.is_some() {
        tracing::debug!(task = %task_id, "session row already exists; created is a no-op");
        return Ok(Verdict::Ignored(IgnoredReason::NoOp));
    }
    apply_at_next(bridge, ledger, task_id, |v| LifecycleEvent::Created { v })
}

/// The dispatch path proved both facts at once: the session is bound to its task
/// and the backend resource is attached.
pub fn feed_dispatched(bridge: &Bridge, ledger: &dyn SessionLedger, task_id: &str) {
    if let Err(err) = feed_created(bridge, ledger, task_id) {
        tracing::warn!(task = %task_id, error = %err, "could not seed the session row");
    }
    match feed_resource_attached(bridge, ledger, task_id) {
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
pub fn feed_resource_attached(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
) -> anyhow::Result<Verdict> {
    apply_at_next(bridge, ledger, task_id, |v| {
        LifecycleEvent::ResourceAttach { v }
    })
}

/// The agent finished booting and its channel is reachable.
pub fn feed_ready(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
) -> anyhow::Result<Verdict> {
    apply_at_next(bridge, ledger, task_id, |v| LifecycleEvent::Ready { v })
}

/// The current turn went from "may change state" to "may emit an artifact".
pub fn feed_turn_started(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
) -> anyhow::Result<Verdict> {
    apply_at_next(bridge, ledger, task_id, |v| LifecycleEvent::TurnStarted {
        v,
    })
}

/// The current turn ended with no completion receipt yet.
pub fn feed_turn_ended(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
) -> anyhow::Result<Verdict> {
    apply_at_next(bridge, ledger, task_id, |v| LifecycleEvent::TurnEnded { v })
}

/// The server answered the intent this session's completion is riding on.
///
/// The durable queue is the only witness of that answer, so the receipt reaches
/// the tuple as an observation rather than as a body the client composed for
/// itself: the drain closes because the server took the envelope, which is what
/// `DeliveryState::Accepted` means. A drain that is not open is the reducer's
/// own no-op, so a late answer for a completion the session already settled
/// reopens nothing.
pub fn feed_intent_receipt(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
) -> anyhow::Result<Verdict> {
    apply_at_next(bridge, ledger, task_id, |v| LifecycleEvent::IntentReceipt {
        v,
    })
}

/// The resource was recycled: the resource is closed and the generation is over.
pub fn feed_resource_closed(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
) -> anyhow::Result<Verdict> {
    apply_at_next(bridge, ledger, task_id, |v| {
        LifecycleEvent::ResourceClosed { v }
    })
}

/// The agent process is gone while the ledger may still owe a result.
pub fn feed_agent_gone(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
) -> anyhow::Result<Verdict> {
    apply_at_next(bridge, ledger, task_id, |v| LifecycleEvent::AgentGone { v })
}

/// Work given up on: while the agent is idle the session opens a fault line,
/// and the generation stays open so a later resource close still finalizes the
/// row. How the work ended is written to the task ledger, not here.
pub fn feed_fail(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
) -> anyhow::Result<Verdict> {
    apply_at_next(bridge, ledger, task_id, |v| LifecycleEvent::Fail { v })
}

/// Work the operator cancelled. The result settles in the task ledger; the
/// session tuple keeps whatever its agent, intent, and resource facts were, so
/// this feed reports the cancellation and the reducer answers that nothing in
/// the session changed.
pub fn feed_cancel(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
) -> anyhow::Result<Verdict> {
    apply_at_next(bridge, ledger, task_id, |v| LifecycleEvent::Cancel { v })
}

/// A reconcile fact disagreed with the tuple (probe, heartbeat, snapshot).
pub fn feed_mismatch(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
) -> anyhow::Result<Verdict> {
    apply_at_next(bridge, ledger, task_id, |v| {
        LifecycleEvent::ReconcileMismatch { v }
    })
}

/// A reconcile fact confirmed the tuple, which is what clears `idle_waiting` and
/// `idle_fault` without moving any other dimension.
pub fn feed_reconcile_ok(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
) -> anyhow::Result<Verdict> {
    apply_at_next(bridge, ledger, task_id, |v| LifecycleEvent::ReconcileOk {
        v,
    })
}

/// The handoff was consumed: the completion intent opened, and its settlement
/// closed the session's side of it.
pub fn feed_delivered(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
) -> anyhow::Result<Verdict> {
    apply_at_next(bridge, ledger, task_id, |v| LifecycleEvent::Complete { v })?;
    settle(bridge, ledger, task_id)
}

/// Settle the session's side of a finished handoff: the completion intent's
/// receipt landed, so the delivery drain closes.
///
/// That is all a settlement writes here. The task's result belongs to the task
/// ledger and has no dimension in this tuple, so this function can no longer
/// mirror a `done` into the session row — and no longer needs a receipt
/// invented for one. The tuple still travels as a full `Heartbeat` snapshot,
/// the only event that carries an observation, and legality stays the
/// reducer's decision: a session that never passed Ready has no turn to have
/// delivered, and the reducer refuses that tuple.
pub fn settle(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
) -> anyhow::Result<Verdict> {
    let row = ledger.get_session(task_id)?;
    let current = stored_observation(ledger, row.as_ref());
    let version = Version::new(
        current.version.generation,
        current.version.seq.saturating_add(1),
    );
    let body = settle_body(&current);
    apply_persist(
        bridge,
        ledger,
        task_id,
        &LifecycleEvent::Heartbeat { v: version, body },
    )
}

/// The tuple a settlement leaves behind: the same session with its intent
/// accepted and its completion drain closed. The agent, the resource, the
/// policy and the mismatch counter are not this call's to touch, and the host
/// rides along because a completed turn still runs in the pane it was reported
/// from.
fn settle_body(obs: &Observation) -> Observation {
    let recovery = match obs.recovery {
        RecoveryState::Draining | RecoveryState::IdleWaiting => RecoveryState::None,
        other => other,
    };
    Observation::build(
        obs.version,
        obs.generation_live,
        obs.isolate_after,
        obs.terminate_after,
        obs.mismatch_count,
        obs.agent,
        DeliveryState::Accepted,
        obs.resource,
        recovery,
    )
    .with_host(obs.host.clone())
}
