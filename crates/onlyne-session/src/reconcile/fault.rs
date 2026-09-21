use serde_json::json;

use crate::backend::SessionRef;
use crate::lifecycle::{
    AgentState, DeliveryState, Observation, RecoveryState, ResourceState, Verdict,
};

use super::bridge::Bridge;
use super::feed::{
    feed_agent_gone, feed_fail, feed_mismatch, feed_reconcile_ok, feed_resource_attached,
};
use super::ledger::SessionLedger;
use super::record::{
    FaultOutcome, FaultRecord, SessionRecord, now_unix, short, stored_observation,
};

/// Consecutive reconcile mismatches tolerated before the session is isolated.
pub const DEFAULT_ISOLATE_AFTER: u32 = 1;
/// Consecutive mismatches tolerated before the generation is terminated and the
/// unsettled work is recorded as a fault.
pub const DEFAULT_TERMINATE_AFTER: u32 = 3;

/// Record one divergence that survived a crash. Deduplicated on
/// `(task_id, kind, generation)` so a repeated pass cannot bury the queue.
/// Recovery task creation is out of scope here: the supervisor opens it as an
/// explicit `control` op from the recorded fault.
pub fn record_fault(
    ledger: &dyn SessionLedger,
    task_id: &str,
    kind: &str,
    intent: &str,
    reason: &str,
) -> anyhow::Result<FaultOutcome> {
    let row = ledger.get_session(task_id)?;
    let generation = row.as_ref().map(|r| r.generation).unwrap_or(0);
    if ledger
        .list_faults(task_id)?
        .iter()
        .any(|f| f.kind == kind && f.generation == generation)
    {
        tracing::debug!(task = %task_id, kind, "this generation's fault is already in the queue");
        return Ok(FaultOutcome::default());
    }
    let attempt = ledger.task_attempt(task_id)?;
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
        created_at: now_unix(),
    };
    let id = ledger.insert_fault(&fault)?;
    tracing::warn!(task = %task_id, fault_id = id, kind, intent, reason, "recorded session fault");
    ledger.note_alert(format!("session fault {kind} on {}", short(task_id)));
    ledger.emit(
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
    Ok(FaultOutcome { fault_id: Some(id) })
}

/// The resource is provably gone. Work whose receipt never landed faults first,
/// then the generation ends. Fault recording covers the divergence the ledger
/// cannot describe: a completion still owed when the generation ended. Returns
/// true when a fault row was recorded.
pub fn reconcile_dead(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
    obs: &Observation,
    detail: &str,
) -> anyhow::Result<bool> {
    let reason = format!("backend resource gone: {detail}");
    tracing::warn!(task = %task_id, %reason, "open session lost its agent");
    let mut faulted = false;
    // The session's own evidence that it still owes a completion is an intent
    // that never came back accepted. What the task ended as is the ledger's
    // business and is not in this tuple.
    if obs.delivery != DeliveryState::Accepted {
        let outcome = record_fault(
            ledger,
            task_id,
            "probe_dead",
            "reconcile:probe_dead",
            &reason,
        )?;
        faulted = outcome.recorded();
        let verdict = feed_fail(bridge, ledger, task_id)?;
        let _ = verdict;
    }
    let verdict = feed_agent_gone(bridge, ledger, task_id)?;
    let _ = verdict;
    Ok(faulted)
}

/// The resource is alive and disagrees with the tuple. The reducer's own ladder
/// decides when to isolate and when to end the generation. Returns true when a
/// fault row was recorded at the terminate ceiling.
pub fn reconcile_mismatch(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    task_id: &str,
    obs: &Observation,
    detail: &str,
) -> anyhow::Result<bool> {
    let reason = format!("backend disagrees with the session tuple: {detail}");
    tracing::warn!(
        task = %task_id,
        mismatch_count = obs.mismatch_count,
        isolate_after = obs.isolate_after,
        terminate_after = obs.terminate_after,
        %reason,
        "session reconcile mismatch"
    );
    let verdict = feed_mismatch(bridge, ledger, task_id)?;
    let at_the_limit = obs.mismatch_count.saturating_add(1) >= obs.terminate_after;
    // The ladder's terminate arm ends the generation; that is the whole of what
    // the session can report about it.
    let terminated = matches!(&verdict, Verdict::Applied(next)
        if next.agent == AgentState::Gone);
    if at_the_limit && terminated {
        let outcome = record_fault(
            ledger,
            task_id,
            "mismatch_terminate",
            "reconcile:mismatch",
            &reason,
        )?;
        return Ok(outcome.recorded());
    }
    Ok(false)
}

/// Resolve the backend resource the stored tuple names: the live in-memory
/// session first, then the row's own `backend_ref` when it parses as a whole
/// `SessionRef` for this task. Anything else is inconclusive and yields `None`.
pub fn probe_target(bridge: &Bridge, task_id: &str, row: &SessionRecord) -> Option<SessionRef> {
    if let Some(session) = bridge.live.lock().unwrap().get(task_id) {
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
    None
}

/// Reconcile one open session row against a probe result. A probe error, or a
/// row that names no resource, is inconclusive: the tuple stays exactly where
/// it is. This is the fault-recording branch of the old `hop_timeouts` shape:
/// a dead resource faults an undelivered completion and records the divergence;
/// replay lives with the supervisor now.
///
/// A finished generation is not probed. A session whose agent is still alive is
/// probed whatever its delivery says: whether the task it was serving is over
/// is a question the session tuple no longer answers, so an accepted receipt on
/// its own is not a reason to stop looking at the resource.
pub fn reconcile_probe(
    bridge: &Bridge,
    ledger: &dyn SessionLedger,
    backend: &dyn crate::backend::SessionBackend,
    task_id: &str,
) -> anyhow::Result<ProbeVerdict> {
    let Some(row) = ledger.get_session(task_id)? else {
        return Ok(ProbeVerdict::Unknown);
    };
    let current = stored_observation(ledger, Some(&row));
    if current.agent == AgentState::Gone {
        return Ok(ProbeVerdict::Exited);
    }
    let Some(session) = probe_target(bridge, task_id, &row) else {
        return Ok(ProbeVerdict::Unknown);
    };
    let probe = match backend.probe(&session) {
        Ok(probe) => probe,
        Err(err) => {
            tracing::warn!(
                task = %task_id,
                error = %err,
                "session probe was inconclusive; not judging the generation dead"
            );
            return Ok(ProbeVerdict::Unknown);
        }
    };
    let detail = match &probe.detail {
        Some(value) => value.to_string(),
        None => format!("alive={} attached={}", probe.alive, probe.attached),
    };
    if !probe.alive {
        let faulted = reconcile_dead(bridge, ledger, task_id, &current, &detail)?;
        return Ok(if faulted {
            ProbeVerdict::DeadFaulted
        } else {
            ProbeVerdict::Dead
        });
    }
    if !current.generation_live {
        let faulted = reconcile_mismatch(bridge, ledger, task_id, &current, &detail)?;
        return Ok(if faulted {
            ProbeVerdict::MismatchFaulted
        } else {
            ProbeVerdict::Mismatch
        });
    }
    if current.resource == ResourceState::Attached && !probe.attached {
        let faulted = reconcile_mismatch(bridge, ledger, task_id, &current, &detail)?;
        return Ok(if faulted {
            ProbeVerdict::MismatchFaulted
        } else {
            ProbeVerdict::Mismatch
        });
    }
    if current.resource == ResourceState::Detached {
        feed_resource_attached(bridge, ledger, task_id)?;
    }
    if current.mismatch_count > 0 || current.recovery != RecoveryState::None {
        feed_reconcile_ok(bridge, ledger, task_id)?;
    }
    Ok(ProbeVerdict::Ok)
}

/// What one [`reconcile_probe`] call found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeVerdict {
    /// The tuple and the resource agree; any fault line the probes opened is
    /// healed.
    Ok,
    /// The resource is gone and the session owed nothing: an accepted receipt
    /// was already on the tuple.
    Dead,
    /// The resource is gone and the completion it owed never landed.
    DeadFaulted,
    /// The resource is alive and disagrees with the tuple.
    Mismatch,
    /// A disagreement at the terminate ceiling, recorded as a fault.
    MismatchFaulted,
    /// Nothing could be concluded: no row, no probeable resource, or a probe
    /// that failed.
    Unknown,
    /// The stored generation is already over — its agent is gone — so there is
    /// nothing left to probe. Whether the task it served finished is not a
    /// question this enum answers.
    Exited,
}
