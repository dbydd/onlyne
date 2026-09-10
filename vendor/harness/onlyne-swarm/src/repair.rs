//! `repair` — the operator surface over the session ledger.
//!
//! Reconcile and the scheduler only ever move a session forward from evidence
//! they can see. When that evidence is missing — a backend that will not probe,
//! a pane that was closed by hand, a wedged workspace daemon — the tuple is
//! stuck and the work stays owed. This module is the one door a human can walk
//! through, and it is deliberately narrow:
//!
//! * every action writes the ledger first and answers `ok` afterwards, so a
//!   response can never describe a state a crash undid.
//! * `adopt` and `rebind` move a generation only on probe evidence: the old
//!   generation must be proven gone, a replacement handle must be proven alive.
//!   A probe that errors proves nothing and is refused, never treated as a
//!   convenient default.
//! * task results stay owned by the existing scheduler handlers. `fail` and
//!   `close` route through `sched::on_early_exit` / `sched::on_recycled`; no
//!   repair action writes `tasks.state` directly, and `tasks.operator_revision`
//!   is the only ledger column this module touches.
//! * every action emits a `"repair"` event with its arguments and its verdict,
//!   and stamps `operator_revision` on the task row, so a second operator sees
//!   what the first one already did.
//!
//! IPC: `{"op":"repair","action":"<action>",...}` → [`repair_request`]. The CLI
//! `swarm repair <action>` sends the same request and exits non-zero when the
//! response says `ok: false`.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::db::{SessionRecord, TaskRow};
use crate::lifecycle::{
    self, AgentState, DeliveryState, LifecycleEvent, Observation, Outcome, RecoveryState,
    ResourceState, Verdict, Version,
};
use crate::runtime::SessionRef;
use crate::sched::{self, Sched};

/// Every action this door knows, in the order `--help` lists them.
pub const ACTIONS: &[&str] = &[
    "inspect", "adopt", "rebind", "retry", "fail", "close", "ack",
];

fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

/// The reducer's state names as the ledger stores them: one serde pass, so a
/// repair response and a `sessions` column can never disagree by spelling.
fn name<T: serde::Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "?".to_string())
}

/// One entry point for both transports. Argument extraction lives here so the
/// CLI, the socket, and the tests all exercise the same contract.
pub fn repair_request(sched: &Arc<Sched>, req: &Value) -> anyhow::Result<Value> {
    let action = req
        .get("action")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("repair needs one of: {}", ACTIONS.join(", ")))?;
    match action {
        "inspect" => inspect(sched, &require_task(req, action)?),
        "adopt" => adopt(sched, &require_task(req, action)?),
        "rebind" => rebind(sched, &require_task(req, action)?, &require_handle(req)?),
        "retry" => retry(sched, &require_task(req, action)?),
        "fail" => fail(
            sched,
            &require_task(req, action)?,
            req.get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("operator repair fail"),
        ),
        "close" => close(sched, &require_task(req, action)?),
        "ack" => {
            let fault_id = req
                .get("fault_id")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| anyhow::anyhow!("repair ack needs a numeric fault_id"))?;
            ack(sched, fault_id)
        }
        other => anyhow::bail!(
            "unknown repair action {other:?}; this daemon knows: {}",
            ACTIONS.join(", ")
        ),
    }
}

fn require_task(req: &Value, action: &str) -> anyhow::Result<String> {
    req.get("task_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("repair {action} needs a task_id"))
}

fn require_handle(req: &Value) -> anyhow::Result<String> {
    req.get("handle")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("repair rebind needs a --handle"))
}

// ---------------------------------------------------------------------------
// shared reads
// ---------------------------------------------------------------------------

/// What a probe proved, in the three states this module acts on. `Unknown`
/// covers a missing reference and a transport error alike: neither is
/// evidence of anything, and no repair action may treat it as either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Proof {
    Live,
    Gone,
    Unknown,
}

impl Proof {
    fn label(self) -> &'static str {
        match self {
            Proof::Live => "live",
            Proof::Gone => "gone",
            Proof::Unknown => "unknown",
        }
    }
}

/// Probe one backend reference. A backend that cannot be asked, or answers with
/// an error, yields [`Proof::Unknown`].
fn probe(sched: &Arc<Sched>, session: Option<&SessionRef>) -> (Proof, Value) {
    let Some(session) = session else {
        return (Proof::Unknown, json!({"detail": "no backend reference"}));
    };
    let backend = match sched.require_backend() {
        Ok(backend) => backend,
        Err(err) => return (Proof::Unknown, json!({"detail": err.to_string()})),
    };
    match backend.probe(session) {
        Ok(report) => {
            let proof = if report.alive {
                Proof::Live
            } else {
                Proof::Gone
            };
            (
                proof,
                json!({
                    "alive": report.alive,
                    "attached": report.attached,
                    "detail": report.detail,
                }),
            )
        }
        Err(err) => (Proof::Unknown, json!({"detail": err.to_string()})),
    }
}

/// The backend reference for a handle an operator named.
///
/// `sched::session_for_handle` answers with the session this daemon already
/// tracks, which is the right answer for the *old* reference and the wrong one
/// for a rebind: the whole point is a handle the ledger has never seen. The
/// `{"handle": ...}` shape is the same one that helper falls back to.
fn ref_for_handle(sched: &Arc<Sched>, task_id: &str, handle: &str, generation: u64) -> SessionRef {
    SessionRef {
        task_id: task_id.to_string(),
        backend: sched
            .backend
            .as_ref()
            .map(|backend| backend.name().to_string())
            .unwrap_or_default(),
        backend_ref: json!({"handle": handle}),
        generation,
    }
}

/// The session row or an explicit refusal. Every mutating action starts here
/// so "the scheduler never tracked this task" is never reported as success.
fn tracked(sched: &Arc<Sched>, task_id: &str, action: &str) -> anyhow::Result<SessionRecord> {
    sched.db.get_session(task_id)?.ok_or_else(|| {
        anyhow::anyhow!(
            "repair {action}: {} names no session row, so there is no generation to move",
            short(task_id)
        )
    })
}

/// The tuple as stored, plus the projection recomputed from its own columns:
/// a mismatch between the two is ledger corruption, and `inspect` is where an
/// operator should see it.
fn tuple_report(row: &SessionRecord, obs: &Observation) -> Value {
    let projected = lifecycle::project(
        obs.agent,
        obs.delivery,
        obs.resource,
        obs.recovery,
        obs.outcome,
    );
    json!({
        "generation": obs.version.generation,
        "seq": obs.version.seq,
        "agent_state": name(obs.agent),
        "delivery_state": name(obs.delivery),
        "resource_state": name(obs.resource),
        "recovery_substate": name(obs.recovery),
        "public_lifecycle": name(obs.public),
        "projected_lifecycle": name(projected),
        "consistent": projected == obs.public,
        "outcome": name(obs.outcome),
        "generation_live": obs.generation_live,
        "mismatch_count": row.mismatch_count,
        "isolate_after": obs.isolate_after,
        "terminate_after": obs.terminate_after,
        "backend_ref": row.backend_ref,
        "stored_observed_json": row.observed_json,
        "stored_desired_json": row.desired_json,
    })
}

/// Persist the operator stamp and report what the row now holds. `None` means
/// the task ledger holds no row for this session — surfaced, never hidden.
fn stamp(sched: &Arc<Sched>, task_id: &str, action: &str) -> Option<String> {
    let revision = match sched.db.bump_operator_revision(task_id, action, now_unix()) {
        Ok(revision) => revision,
        Err(err) => {
            tracing::warn!(task = %task_id, action, error = %err, "operator revision stamp unwritable");
            None
        }
    };
    if revision.is_none() {
        tracing::warn!(task = %short(task_id), action, "repair touched a session with no task row");
    }
    revision
}

fn emit_repair(sched: &Arc<Sched>, data: Value) {
    sched.emit("repair", data.clone());
}

// ---------------------------------------------------------------------------
// actions
// ---------------------------------------------------------------------------

/// Read everything an operator needs to decide, in one structured response:
/// stored tuple plus its recomputed projection, what the backend says right
/// now, the task row, the fault queue, and the open intent lines.
pub fn inspect(sched: &Arc<Sched>, task_id: &str) -> anyhow::Result<Value> {
    let row = sched.db.get_session(task_id)?.ok_or_else(|| {
        anyhow::anyhow!("repair inspect: {} names no session row", short(task_id))
    })?;
    let obs = crate::reconcile::stored_observation(sched, Some(&row));
    let target = crate::reconcile::probe_target(sched, task_id, &row);
    let (proof, probe_detail) = probe(sched, target.as_ref());
    let task = sched.db.get(task_id)?;
    let faults = sched.db.list_faults(Some(task_id))?;
    let intents: Vec<_> = sched
        .db
        .list_intents(None)?
        .into_iter()
        .filter(|intent| intent.task_id == task_id)
        .collect();
    let data = json!({
        "action": "inspect",
        "ok": true,
        "task_id": task_id,
        "tuple": tuple_report(&row, &obs),
        "next_version": { "generation": obs.version.generation, "seq": obs.version.seq + 1 },
        "probe": { "state": proof.label(), "target": target.map(|s| s.backend_ref), "detail": probe_detail },
        "task": task,
        "faults": faults,
        "intents": intents,
        "operator_revision": sched.db.operator_revision(task_id)?,
        "terminal_handle": sched.terminals.lock().unwrap().get(task_id).cloned(),
    });
    emit_repair(
        sched,
        json!({"action": "inspect", "task_id": task_id, "probe": proof.label()}),
    );
    Ok(data)
}

/// Move the task onto a new Pi generation after proving the old one dead.
///
/// The reference stays where it is: adoption answers "which generation owns this
/// task", and nothing more. Re-seating a *different* handle belongs to `rebind`,
/// because the frozen reducer refuses `ResourceAttach` on a resource that
/// `AgentGone` already moved to `closing` (§2.3) — so a generation move cannot
/// also carry a live attached body, and pretending otherwise would strand the
/// tuple.
///
/// The gone-probe doubles as the `AgentGone` evidence the reducer demands before
/// it admits a new generation, so `adopt` records that fact itself when the tuple
/// still claims the old generation is live.
pub fn adopt(sched: &Arc<Sched>, task_id: &str) -> anyhow::Result<Value> {
    let row = tracked(sched, task_id, "adopt")?;
    let obs = crate::reconcile::stored_observation(sched, Some(&row));
    let target = crate::reconcile::probe_target(sched, task_id, &row);
    let (proof, detail) = probe(sched, target.as_ref());
    if proof == Proof::Live {
        anyhow::bail!(
            "repair adopt: generation {} is still live on {} ({}); recycle it or wait for reconcile to prove it gone",
            row.generation,
            short(task_id),
            detail
        );
    }
    if proof == Proof::Unknown {
        anyhow::bail!(
            "repair adopt: the backend could not prove generation {} dead ({})",
            row.generation,
            detail
        );
    }
    // The reducer admits a new generation only once the stored tuple stops
    // claiming the old one is live. The probe above is exactly that evidence, so
    // write it here rather than making the operator wait for a reconcile pass;
    // when reconcile already proved it, the flag says so and nothing is written
    // twice.
    let recorded_gone = if obs.generation_live {
        match crate::reconcile::feed_agent_gone(sched, task_id)? {
            Verdict::Applied(_) => true,
            other => anyhow::bail!(
                "repair adopt: the probe proved {} gone but the reducer kept the tuple as it was ({other:?})",
                short(task_id)
            ),
        }
    } else {
        false
    };
    let version = Version::new(row.generation.max(0) as u64 + 1, 0);
    let verdict = crate::reconcile::apply_persist(
        sched,
        task_id,
        &LifecycleEvent::AdoptNewGeneration { v: version },
    )?;
    require_applied("adopt", task_id, &verdict)?;
    let revision = stamp(sched, task_id, "adopt");
    let after = tracked(sched, task_id, "adopt")?;
    let detail = json!({
        "action": "adopt",
        "ok": true,
        "task_id": task_id,
        "from_generation": row.generation,
        "from_seq": obs.version.seq,
        "generation": after.generation,
        "seq": after.seq,
        "agent_gone_recorded": recorded_gone,
        "backend_ref": after.backend_ref,
        "next": "rebind --handle <h> seats a different live resource on the new generation",
        "operator_revision": revision,
    });
    emit_repair(sched, detail.clone());
    Ok(detail)
}

/// Attach a live handle as a new generation, carrying the tuple forward.
///
/// This is the door for the case `adopt` cannot reach: a tuple the reducer will
/// not move on its own (an isolated or wedged generation that still reports
/// live in the ledger). Evidence rules are the same in reverse — the new handle
/// must probe alive, and the old reference must not probe live. A still-live
/// old generation is refused, because superseding it would leave two sessions
/// that both believe they own the task.
pub fn rebind(sched: &Arc<Sched>, task_id: &str, handle: &str) -> anyhow::Result<Value> {
    if handle.trim().is_empty() {
        anyhow::bail!("repair rebind needs a --handle");
    }
    let row = tracked(sched, task_id, "rebind")?;
    let obs = crate::reconcile::stored_observation(sched, Some(&row));
    let old = crate::reconcile::probe_target(sched, task_id, &row);
    let (old_proof, old_detail) = probe(sched, old.as_ref());
    if old_proof == Proof::Live {
        anyhow::bail!(
            "repair rebind: generation {} is live on {} ({old_detail}); a rebind would leave two owners. Recycle the current session first",
            row.generation,
            short(task_id)
        );
    }
    let session = ref_for_handle(sched, task_id, handle, row.generation.max(0) as u64);
    let (new_proof, new_detail) = probe(sched, Some(&session));
    if new_proof != Proof::Live {
        anyhow::bail!(
            "repair rebind: refusing to bind {} to handle {handle:?} that probes {} ({new_detail})",
            short(task_id),
            new_proof.label()
        );
    }
    let version = Version::new(row.generation.max(0) as u64 + 1, obs.version.seq + 1);
    let body = Observation::build(
        version,
        true,
        obs.isolate_after,
        obs.terminate_after,
        0,
        AgentState::Booting,
        DeliveryState::None,
        ResourceState::Attached,
        RecoveryState::None,
        Outcome::Pending,
    );
    let mut next = session;
    next.generation = version.generation;
    {
        let mut sessions = sched.sessions.lock().unwrap();
        sessions.insert(task_id.to_string(), next);
    }
    {
        let mut terminals = sched.terminals.lock().unwrap();
        terminals.insert(task_id.to_string(), handle.to_string());
    }
    let verdict = crate::reconcile::apply_persist(
        sched,
        task_id,
        &LifecycleEvent::Supersede {
            v: version,
            old_generation_dead: true,
            body,
        },
    )?;
    require_applied("rebind", task_id, &verdict)?;
    sched.db.set_terminal(
        task_id,
        &json!({"handle": handle, "generation": version.generation}).to_string(),
    )?;
    let revision = stamp(sched, task_id, "rebind");
    let after = tracked(sched, task_id, "rebind")?;
    let after_obs = crate::reconcile::stored_observation(sched, Some(&after));
    let detail = json!({
        "action": "rebind",
        "ok": true,
        "task_id": task_id,
        "from_generation": row.generation,
        "generation": after.generation,
        "seq": after.seq,
        "handle": handle,
        "old_generation_probe": old_proof.label(),
        "new_generation_probe": new_proof.label(),
        "public_lifecycle": name(after_obs.public),
        "tuple": tuple_report(&after, &after_obs),
        "operator_revision": revision,
        "note": if after_obs.outcome == Outcome::Pending {
            "the ledger owns the result: if the task row is already terminal, the next reconcile pass mirrors it back"
        } else { "rebound" },
    });
    emit_repair(sched, detail.clone());
    Ok(detail)
}

/// Re-dispatch a fault's recovery task and re-arm its spent intents.
///
/// The fault stays in place — a retry that then fails should still be visible
/// to the next operator — and a fault whose recovery was never opened gets one
/// now, through the same gate reconcile uses.
pub fn retry(sched: &Arc<Sched>, task_id: &str) -> anyhow::Result<Value> {
    let faults = sched.db.list_faults(Some(task_id))?;
    let fault = faults
        .iter()
        .rev()
        .find(|f| f.state != "acked")
        .cloned()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "repair retry: {} has no un-acked fault to retry ({} queued, all acked)",
                short(task_id),
                faults.len()
            )
        })?;
    let recovery_id = match fault.recovery_task_id.clone() {
        Some(id) => id,
        None => crate::reconcile::open_recovery_task(sched, &fault)?.ok_or_else(|| {
            anyhow::anyhow!(
                "repair retry: fault {} (kind {}) opens no recovery task: its work is already settled, or the single-layer gate left it with the root supervisor",
                fault.id, fault.kind
            )
        })?,
    };
    let rearmed: Vec<String> = sched
        .db
        .list_intents(None)?
        .into_iter()
        .filter(|intent| {
            (intent.task_id == task_id || intent.task_id == recovery_id)
                && intent.state == "exhausted"
        })
        .map(|intent| intent.op_id)
        .collect();
    for op_id in &rearmed {
        sched.db.reset_intent(op_id, now_unix())?;
    }
    let dispatched = match sched::dispatch_public(sched, &recovery_id, &{
        sched
            .db
            .get(&recovery_id)?
            .map(|task| task.to_ws)
            .unwrap_or_else(|| ".".to_string())
    }) {
        Ok(()) => true,
        Err(err) => {
            sched.note_alert(format!(
                "repair retry could not dispatch recovery {}: {err}",
                short(&recovery_id)
            ));
            tracing::warn!(recovery_id = %short(&recovery_id), error = %err, "retry dispatch failed");
            false
        }
    };
    let revision = stamp(sched, task_id, "retry");
    let detail = json!({
        "action": "retry",
        "ok": dispatched,
        "task_id": task_id,
        "fault_id": fault.id,
        "recovery_task_id": recovery_id,
        "rearmed_intents": rearmed,
        "dispatched": dispatched,
        "operator_revision": revision,
    });
    emit_repair(sched, detail.clone());
    if !dispatched {
        anyhow::bail!(
            "repair retry: recovery task {} stays pending — the backend refused the spawn (see alerts)",
            short(&recovery_id)
        );
    }
    Ok(detail)
}

/// Settle the work as failed on an operator's judgement.
///
/// The session tuple, the task ledger, and the fault queue all move: `feed_fail`
/// for the tuple, `sched::on_early_exit` for the ledger result and the tab close
/// (it owns those writes), and a `operator_fail` fault so the queue carries the
/// reason. `operator_fail` sits outside `RECOVERY_FAULT_KINDS`: an operator who
/// chose to kill the work is not asking for a fresh session.
pub fn fail(sched: &Arc<Sched>, task_id: &str, reason: &str) -> anyhow::Result<Value> {
    let row = tracked(sched, task_id, "fail")?;
    let verdict = crate::reconcile::feed_fail(sched, task_id)?;
    sched::on_early_exit(sched, task_id, reason)?;
    let outcome = crate::reconcile::record_fault(
        sched,
        task_id,
        "operator_fail",
        "repair:fail",
        &format!("operator repair fail: {reason}"),
    )?;
    let revision = stamp(sched, task_id, "fail");
    let after = sched
        .db
        .get(task_id)?
        .map(|task| task.state.as_str().to_string());
    let detail = json!({
        "action": "fail",
        "ok": true,
        "task_id": task_id,
        "reason": reason,
        "session_verdict": format!("{verdict:?}"),
        "from_generation": row.generation,
        "fault_id": outcome.fault_id,
        "recovery_task_id": outcome.recovery_task_id,
        "task_state": after,
        "operator_revision": revision,
    });
    emit_repair(sched, detail.clone());
    Ok(detail)
}

/// Close the resource and settle the session's bookkeeping, reusing the ack
/// path the scheduler already trusts. A task that still owed work is marked
/// failed by the same handler, exactly as an agent-acked recycle would.
pub fn close(sched: &Arc<Sched>, task_id: &str) -> anyhow::Result<Value> {
    let row = tracked(sched, task_id, "close")?;
    sched::on_recycled(sched, task_id, "repair close")?;
    let revision = stamp(sched, task_id, "close");
    let after = tracked(sched, task_id, "close")?;
    let after_obs = crate::reconcile::stored_observation(sched, Some(&after));
    let detail = json!({
        "action": "close",
        "ok": true,
        "task_id": task_id,
        "from_generation": row.generation,
        "generation": after.generation,
        "seq": after.seq,
        "resource_state": name(after_obs.resource),
        "agent_state": name(after_obs.agent),
        "public_lifecycle": name(after_obs.public),
        "task_state": sched.db.get(task_id)?.map(|task| task.state.as_str().to_string()),
        "operator_revision": revision,
    });
    emit_repair(sched, detail.clone());
    Ok(detail)
}

/// Take a fault off the queue. The row stays, flipped to `acked`, so the
/// evidence outlives the acknowledgement.
pub fn ack(sched: &Arc<Sched>, fault_id: i64) -> anyhow::Result<Value> {
    let fault = sched
        .db
        .get_fault(fault_id)?
        .ok_or_else(|| anyhow::anyhow!("repair ack: no fault {fault_id} in this workspace"))?;
    if fault.state == "acked" {
        anyhow::bail!(
            "repair ack: fault {fault_id} was already acked (task {}, kind {})",
            short(&fault.task_id),
            fault.kind
        );
    }
    if !sched.db.ack_fault(fault_id)? {
        anyhow::bail!("repair ack: fault {fault_id} changed underneath this request; re-inspect it")
    }
    let revision = stamp(sched, &fault.task_id, "ack");
    let detail = json!({
        "action": "ack",
        "ok": true,
        "fault_id": fault_id,
        "task_id": fault.task_id,
        "kind": fault.kind,
        "state": "acked",
        "recovery_task_id": fault.recovery_task_id,
        "operator_revision": revision,
    });
    emit_repair(sched, detail.clone());
    Ok(detail)
}

/// A repair that the frozen reducer refused stays refused: the operator gets
/// the verdict and the reason instead of a silent no-op answer.
fn require_applied(action: &str, task_id: &str, verdict: &Verdict) -> anyhow::Result<()> {
    match verdict {
        Verdict::Applied(_) => Ok(()),
        other => anyhow::bail!(
            "repair {action}: the reducer refused the write for {} ({other:?}); the ledger is unchanged",
            short(task_id)
        ),
    }
}

/// The task row summary `inspect` and the CLI print together.
pub fn task_line(task: Option<&TaskRow>) -> Value {
    match task {
        Some(task) => json!({
            "task_id": task.task_id,
            "state": task.state.as_str(),
            "kind": task.kind,
            "failure_of": task.failure_of,
            "to_ws": task.to_ws,
            "attempt": task.attempt,
        }),
        None => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::reconcile;
    use crate::runtime::{Capabilities, CloseReason, ResourceProbe, SessionBackend, SpawnSpec};
    use std::collections::HashSet;
    use std::sync::Mutex;

    /// A backend whose identity is the handle, so "the old generation is gone
    /// and this new one is alive" is a state a test can actually build.
    /// `FakeBackend` keys on the task id, which would make that combination
    /// unrepresentable.
    #[derive(Default)]
    struct Handles {
        live: Mutex<HashSet<String>>,
    }

    impl Handles {
        fn up(&self, handle: &str) {
            self.live.lock().unwrap().insert(handle.to_string());
        }
        fn down(&self, handle: &str) {
            self.live.lock().unwrap().remove(handle);
        }
    }

    impl SessionBackend for Handles {
        fn name(&self) -> &'static str {
            "handle"
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities {
                spawn: true,
                attach: true,
                probe: true,
                close: true,
                focus: false,
                rename: false,
            }
        }
        fn available(&self) -> anyhow::Result<bool> {
            Ok(true)
        }
        fn spawn(&self, spec: SpawnSpec) -> anyhow::Result<SessionRef> {
            let handle = format!("term-{}", spec.task_id);
            self.up(&handle);
            Ok(SessionRef {
                task_id: spec.task_id,
                backend: "handle".into(),
                backend_ref: json!({"handle": handle}),
                generation: 1,
            })
        }
        fn attach(&self, session: &SessionRef) -> anyhow::Result<SessionRef> {
            Ok(session.clone())
        }
        fn probe(&self, session: &SessionRef) -> anyhow::Result<ResourceProbe> {
            let handle = session
                .backend_ref
                .get("handle")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Ok(ResourceProbe {
                alive: self.live.lock().unwrap().contains(&handle),
                attached: true,
                detail: Some(json!({"handle": handle})),
            })
        }
        fn close(
            &self,
            session: &SessionRef,
            _reason: CloseReason,
            _force: bool,
        ) -> anyhow::Result<()> {
            if let Some(handle) = session.backend_ref.get("handle").and_then(|v| v.as_str()) {
                self.down(handle);
            }
            Ok(())
        }
    }

    fn setup() -> (Arc<Sched>, Arc<Handles>) {
        let dir = tempfile::tempdir().unwrap();
        let root: &'static std::path::Path = Box::leak(dir.path().join("root").into_boxed_path());
        std::fs::create_dir_all(root).unwrap();
        let _ = Box::leak(Box::new(dir));
        let backend = Arc::new(Handles::default());
        let db = Db::open(root).unwrap();
        let sched = Sched::new_with_backend(root.to_path_buf(), db, Some(backend.clone()), None);
        (sched, backend)
    }

    /// A session that is live, owed work, and tracked in memory: the shape a
    /// running hop reaches before anything goes wrong.
    fn running(sched: &Arc<Sched>, backend: &Arc<Handles>, task: &str) -> String {
        sched
            .db
            .insert_task(task, ".", "worker", "", 1, "payload")
            .unwrap();
        sched
            .db
            .set_state(task, crate::db::TaskState::Running)
            .unwrap();
        let session = sched
            .require_backend()
            .unwrap()
            .spawn(SpawnSpec {
                cwd: sched.root.clone(),
                task_id: task.into(),
                command: vec!["pi".into()],
                env: Default::default(),
                focus: None,
                rename: None,
            })
            .expect("spawn");
        let handle = session.backend_ref["handle"].as_str().unwrap().to_string();
        backend.up(&handle);
        sched.sessions.lock().unwrap().insert(task.into(), session);
        sched
            .terminals
            .lock()
            .unwrap()
            .insert(task.into(), handle.clone());
        sched.db.set_terminal(task, &handle).unwrap();
        reconcile::feed_created(sched, task).unwrap();
        reconcile::feed_resource_attached(sched, task).unwrap();
        reconcile::feed_ready(sched, task).unwrap();
        handle
    }

    fn row(sched: &Arc<Sched>, task: &str) -> crate::db::SessionRecord {
        sched.db.get_session(task).unwrap().unwrap()
    }

    fn tuple(sched: &Arc<Sched>, task: &str) -> Value {
        inspect(sched, task).unwrap()["tuple"].clone()
    }

    #[test]
    fn inspect_shows_the_stored_tuple_the_live_probe_and_both_open_lines() {
        let (sched, backend) = setup();
        let handle = running(&sched, &backend, "ins-1");
        let fault = reconcile::record_fault(&sched, "ins-1", "probe_dead", "test:one", "why")
            .unwrap()
            .fault_id
            .expect("fault row");
        let seen = inspect(&sched, "ins-1").unwrap();
        assert_eq!(seen["action"], "inspect");
        assert_eq!(seen["ok"], true);
        assert_eq!(seen["tuple"]["generation"], 1);
        assert_eq!(seen["tuple"]["seq"], 3);
        assert_eq!(seen["tuple"]["public_lifecycle"], "idle");
        assert_eq!(seen["tuple"]["consistent"], true, "{}", seen["tuple"]);
        let stored_ref: Value =
            serde_json::from_str(seen["tuple"]["backend_ref"].as_str().unwrap()).unwrap();
        assert_eq!(
            stored_ref["backend_ref"]["handle"],
            handle.as_str(),
            "{stored_ref}"
        );
        assert_eq!(stored_ref["task_id"], "ins-1");
        assert_eq!(stored_ref["backend"], "handle");
        assert_eq!(seen["next_version"], json!({"generation": 1, "seq": 4}));
        assert_eq!(seen["probe"]["state"], "live");
        assert_eq!(seen["task"]["state"], "running");
        assert_eq!(seen["task"]["kind"], "normal");
        assert!(seen["task"]["failure_of"].is_null());
        assert_eq!(
            seen["operator_revision"],
            Value::Null,
            "nothing repaired yet"
        );
        // A fault with no recovery line yet is visible as its own state, and
        // the intents array is the other half of "what is this daemon waiting
        // on" — here, empty.
        let faults = seen["faults"].as_array().unwrap().clone();
        assert_eq!(faults.len(), 1, "{faults:?}");
        assert_eq!(faults[0]["id"], fault);
        assert!(seen["intents"].as_array().unwrap().is_empty());
        // Inspect is read-only: the watermark and the stamp are untouched.
        assert_eq!(row(&sched, "ins-1").seq, 3);
        assert_eq!(sched.db.operator_revision("ins-1").unwrap(), None);
    }

    #[test]
    fn adopt_needs_a_proven_dead_generation_and_moves_the_watermark_only_then() {
        let (sched, backend) = setup();
        let handle = running(&sched, &backend, "ad-1");
        let mut rx = sched.bus.sender().subscribe();
        // Alive: adoption is refused, and the tuple is left exactly as it was.
        let err = adopt(&sched, "ad-1")
            .err()
            .expect("a live gen blocks adopt");
        assert!(err.to_string().contains("still live"), "{err}");
        assert_eq!(row(&sched, "ad-1").generation, 1);
        assert_eq!(row(&sched, "ad-1").seq, 3);
        // Proven gone: the watermark moves, the shape resets to a booting
        // generation, and the reference stays on the handle the ledger held.
        backend.down(&handle);
        let detail = adopt(&sched, "ad-1").expect("adoption after proof");
        assert_eq!(detail["generation"], 2, "{detail}");
        assert_eq!(detail["from_seq"], 3);
        assert_eq!(
            detail["agent_gone_recorded"], true,
            "the probe is the evidence, so adopt writes the gone fact itself"
        );
        let after = row(&sched, "ad-1");
        assert_eq!((after.generation, after.seq), (2, 0));
        assert_eq!(after.agent_state, "booting");
        assert_eq!(after.public_lifecycle, "created");
        assert_eq!(
            after.resource_state, "closing",
            "the gone fact moved the resource with the agent; re-seating a live handle is rebind's job"
        );
        assert_eq!(after.mismatch_count, 0);
        let kept: Value = serde_json::from_str(after.backend_ref.trim()).unwrap();
        assert_eq!(kept["backend_ref"]["handle"], handle.as_str(), "{kept}");
        let stamped = sched.db.operator_revision("ad-1").unwrap().expect("stamp");
        assert!(stamped.starts_with("repair adopt at "), "{stamped}");
        let seen = tuple(&sched, "ad-1");
        assert_eq!(
            seen["outcome"], "pending",
            "adoption hands the result back to the ledger owner"
        );
        // The two repairs compose into the whole story: adoption moved the
        // generation, and a live handle now rides on it through `rebind`, which
        // is the only door allowed to carry an attached body.
        backend.up("term-fresh");
        let reseated = rebind(&sched, "ad-1", "term-fresh").expect("rebind after adopt");
        assert_eq!(reseated["generation"], 3, "{reseated}");
        assert_eq!(reseated["old_generation_probe"], "gone");
        let final_row = row(&sched, "ad-1");
        assert_eq!(
            (final_row.generation, final_row.resource_state.as_str()),
            (3, "attached")
        );
        assert_eq!(final_row.public_lifecycle, "created");
        assert!(
            final_row.backend_ref.contains("term-fresh"),
            "{final_row:?}"
        );
        // Every door announces itself on the bus, read-only ones included.
        let mut actions = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if event.typ == "repair" {
                actions.push(event.data["action"].as_str().unwrap_or("?").to_string());
                assert_eq!(event.data["task_id"], "ad-1", "{:?}", event.data);
            }
        }
        assert!(actions.contains(&"adopt".to_string()), "{actions:?}");
        assert!(actions.contains(&"rebind".to_string()), "{actions:?}");
        assert!(actions.contains(&"inspect".to_string()), "{actions:?}");
    }

    #[test]
    fn adopt_records_the_gone_fact_only_once_when_reconcile_proved_it_first() {
        let (sched, backend) = setup();
        let handle = running(&sched, &backend, "ad-2");
        // Reconcile has already proven the generation dead and written the
        // AgentGone fact, so the stored tuple no longer claims a live generation.
        backend.down(&handle);
        let gone = crate::reconcile::feed_agent_gone(&sched, "ad-2").unwrap();
        assert!(matches!(gone, Verdict::Applied(_)), "{gone:?}");
        let before = crate::reconcile::stored_observation(&sched, Some(&row(&sched, "ad-2")));
        assert!(
            !before.generation_live,
            "reconcile already cleared the live flag: {before:?}"
        );
        // Adoption still needs the resource to probe dead, then it moves the
        // watermark on its own. The gone fact is already on the ledger, so the
        // flag reports `false`: adoption writes AgentGone exactly once.
        let detail = adopt(&sched, "ad-2").expect("adoption after reconcile proved gone");
        assert_eq!(
            detail["agent_gone_recorded"], false,
            "the gone fact was already recorded; adopt must not write it twice: {detail}"
        );
        let after = row(&sched, "ad-2");
        assert_eq!(
            (after.generation, after.seq),
            (2, 0),
            "adoption still moves the watermark exactly once: {after:?}"
        );
        assert_eq!(after.agent_state, "booting");
    }

    #[test]
    fn rebind_supersedes_onto_a_live_handle_and_refuses_everything_weaker() {
        let (sched, backend) = setup();
        let old = running(&sched, &backend, "rb-1");
        // No handle, nothing to bind.
        let err = rebind(&sched, "rb-1", "")
            .err()
            .expect("rebind needs a handle");
        assert!(err.to_string().contains("--handle"), "{err}");
        // The old generation is still live: a rebind would seat two owners.
        backend.up("term-fresh");
        let err = rebind(&sched, "rb-1", "term-fresh")
            .err()
            .expect("a live old generation blocks a rebind")
            .to_string();
        assert!(err.contains("two owners"), "{err}");
        assert_eq!(row(&sched, "rb-1").generation, 1);
        // A handle that is not alive cannot be bound either.
        backend.down(&old);
        let err = rebind(&sched, "rb-1", "term-absent")
            .err()
            .expect("a dead handle blocks a rebind")
            .to_string();
        assert!(err.contains("probes gone"), "{err}");
        assert_eq!(row(&sched, "rb-1").generation, 1);
        // Gone old, live new: the generation moves and the new handle is the
        // reference the ledger carries from here.
        let detail = rebind(&sched, "rb-1", "term-fresh").expect("rebind onto a live handle");
        assert_eq!(detail["generation"], 2, "{detail}");
        assert_eq!(detail["old_generation_probe"], "gone");
        assert_eq!(detail["new_generation_probe"], "live");
        let after = row(&sched, "rb-1");
        assert_eq!(
            (after.generation, after.seq),
            (2, 4),
            "seq continues, not restarts"
        );
        assert_eq!(after.agent_state, "booting");
        assert_eq!(after.resource_state, "attached");
        assert_eq!(after.public_lifecycle, "created");
        assert!(after.backend_ref.contains("term-fresh"), "{after:?}");
        assert_eq!(
            sched
                .sessions
                .lock()
                .unwrap()
                .get("rb-1")
                .unwrap()
                .generation,
            2,
            "the in-memory ref must carry the new generation too"
        );
        assert_eq!(
            sched
                .terminals
                .lock()
                .unwrap()
                .get("rb-1")
                .map(String::as_str),
            Some("term-fresh")
        );
        assert_eq!(after.generation, 2);
        assert!(sched
            .db
            .get("rb-1")
            .unwrap()
            .unwrap()
            .terminal
            .contains("term-fresh"));
        assert!(sched
            .db
            .operator_revision("rb-1")
            .unwrap()
            .unwrap()
            .contains("rebind"));
    }

    #[test]
    fn retry_redispatches_the_recovery_line_and_rearms_a_spent_intent() {
        let (sched, backend) = setup();
        running(&sched, &backend, "rt-1");
        // The intent for the first recycle round is already spent, and the fault
        // it caused has its recovery task.
        let outcome =
            reconcile::record_fault(&sched, "rt-1", "probe_dead", "test:one", "gone").unwrap();
        assert!(outcome.recovery_task_id.is_some(), "{outcome:?}");
        let recovery = outcome.recovery_task_id.clone().unwrap();
        let spent = format!("recycle:rt-1:g{}", row(&sched, "rt-1").generation);
        sched
            .db
            .insert_intent(
                &spent,
                "rt-1",
                "recycle",
                &json!({"workspace": "worker"}),
                1,
                1,
            )
            .unwrap();
        sched
            .db
            .exhaust_intent(&spent, "connect refused", 2)
            .unwrap();
        let detail = retry(&sched, "rt-1").expect("retry re-dispatches");
        assert_eq!(detail["ok"], true, "{detail}");
        assert_eq!(detail["recovery_task_id"], recovery.as_str());
        assert_eq!(detail["rearmed_intents"].as_array().unwrap().len(), 1);
        let rearmed = sched.db.get_intent(&spent).unwrap().unwrap();
        assert_eq!(rearmed.state, "pending", "{rearmed:?}");
        assert_eq!(rearmed.attempt, 0, "a fresh round gets a fresh budget");
        // The recovery task is now a running session on the parent role.
        let task = sched.db.get(&recovery).unwrap().unwrap();
        assert_eq!(task.state, crate::db::TaskState::Running);
        assert_eq!(task.kind, "recovery");
        assert!(row(&sched, &recovery).public_lifecycle == "created");
        assert!(sched
            .db
            .operator_revision("rt-1")
            .unwrap()
            .unwrap()
            .contains("retry"));
        // The re-armed intent is live again, so the pump has work to do.
        let counts = reconcile::pump_intents(&sched, rearmed.next_attempt_at);
        assert_eq!(counts.attempted, 1, "{counts:?}");
        // A task with no fault line has nothing to retry, and that is an error.
        let err = retry(&sched, "no-such-task")
            .err()
            .expect("no fault, no retry");
        assert!(err.to_string().contains("no un-acked fault"), "{err}");
    }

    #[test]
    fn fail_close_and_ack_record_the_operators_decision() {
        let (sched, backend) = setup();
        // A live daemon on the role's socket: `fail` runs the scheduler's own
        // early-exit path, which sends the recycle control frame as a durable
        // intent and waits for the session's ack.
        let daemon = crate::ipc::test_daemon(&sched.root, "worker");
        running(&sched, &backend, "fc-1");
        running(&sched, &backend, "fc-2");
        let detail = fail(&sched, "fc-1", "gone sideways").expect("fail");
        assert_eq!(detail["ok"], true);
        assert_eq!(detail["task_state"].as_str(), Some("failed"));
        let task = sched.db.get("fc-1").unwrap().unwrap();
        assert_eq!(task.state, crate::db::TaskState::Failed);
        assert_eq!(task.ledger_state, "failed");
        assert!(task.reason.contains("gone sideways"), "{task:?}");
        let faults = sched.db.list_faults(Some("fc-1")).unwrap();
        assert_eq!(faults.len(), 1, "{faults:?}");
        assert_eq!(faults[0].kind, "operator_fail");
        assert_eq!(faults[0].intent, "repair:fail");
        assert_eq!(faults[0].reason, "operator repair fail: gone sideways");
        // An operator's own fail asks for no fresh session: it is outside the
        // recovery kinds by design, and the response says so with a null.
        assert!(faults[0].recovery_task_id.is_none(), "{:?}", faults[0]);
        assert_eq!(detail["recovery_task_id"], Value::Null);
        // The handler chain, as it really lands: the outcome is the operator's
        // `failed`, the tab is released, the backend resource is closed, and the
        // recycle intent that `close_terminal` opened was answered once.
        let after = row(&sched, "fc-1");
        assert_eq!(
            after.agent_state, "ready",
            "fail moves the outcome, never the agent's own state"
        );
        let seen = inspect(&sched, "fc-1").expect("inspect after fail");
        assert_eq!(seen["tuple"]["outcome"], "failed");
        assert_eq!(seen["tuple"]["public_lifecycle"], "working");
        assert_eq!(seen["probe"]["state"], "gone", "{seen}");
        assert!(sched.terminals.lock().unwrap().get("fc-1").is_none());
        let intents = sched.db.list_intents(None).unwrap();
        assert_eq!(intents.len(), 1, "{intents:?}");
        assert_eq!(intents[0].state, "succeeded", "{:?}", intents[0]);
        assert_eq!(intents[0].task_id, "fc-1");
        assert!(daemon
            .lock()
            .unwrap()
            .wires()
            .iter()
            .any(|w| w.contains("op: recycle")));
        // ack: the queue line is closed but stays readable, twice is refused.
        let acked = ack(&sched, faults[0].id).expect("ack");
        assert_eq!(acked["state"], "acked");
        assert_eq!(
            sched.db.get_fault(faults[0].id).unwrap().unwrap().state,
            "acked"
        );
        assert!(ack(&sched, faults[0].id)
            .err()
            .unwrap()
            .to_string()
            .contains("already acked"));
        assert!(ack(&sched, 999_999)
            .err()
            .unwrap()
            .to_string()
            .contains("no fault"));
        // close: the second task ends through the recycle ack path, and its owed
        // work is failed by the same handler the scheduler uses.
        let closed = close(&sched, "fc-2").expect("close");
        assert_eq!(closed["resource_state"], "closed");
        assert_eq!(closed["agent_state"], "gone");
        assert_eq!(closed["public_lifecycle"], "exited");
        assert_eq!(
            sched.db.get("fc-2").unwrap().unwrap().state,
            crate::db::TaskState::Failed
        );
        assert!(sched
            .db
            .get("fc-2")
            .unwrap()
            .unwrap()
            .reason
            .contains("swarm-recycled"));
        assert!(sched
            .db
            .operator_revision("fc-2")
            .unwrap()
            .unwrap()
            .contains("close"));
        // A repair on a task this daemon never tracked is a refusal on every
        // door, not a silent success.
        let refusals = [
            ("inspect", inspect(&sched, "never-seen").map(|_| ())),
            ("adopt", adopt(&sched, "never-seen").map(|_| ())),
            ("rebind", rebind(&sched, "never-seen", "term-x").map(|_| ())),
            ("fail", fail(&sched, "never-seen", "x").map(|_| ())),
            ("close", close(&sched, "never-seen").map(|_| ())),
        ];
        for (name, result) in refusals {
            let message = result
                .err()
                .unwrap_or_else(|| panic!("repair {name} accepted an unknown task"))
                .to_string();
            assert!(message.contains("no session row"), "{name}: {message}");
        }
    }

    #[test]
    fn the_request_surface_names_every_action_and_refuses_the_rest() {
        let (sched, backend) = setup();
        running(&sched, &backend, "rq-1");
        let seen = repair_request(
            &sched,
            &json!({"op": "repair", "action": "inspect", "task_id": "rq-1"}),
        )
        .expect("the socket shape works end to end");
        assert_eq!(seen["action"], "inspect");
        assert_eq!(seen["task_id"], "rq-1");
        // Missing action, unknown action, missing task_id, wrong fault_id type:
        // each gets its own reason.
        let err = repair_request(&sched, &json!({"op": "repair"}))
            .err()
            .unwrap();
        for action in ACTIONS {
            assert!(err.to_string().contains(action), "{err}");
        }
        assert!(
            repair_request(&sched, &json!({"action": "resurrect", "task_id": "rq-1"}))
                .err()
                .unwrap()
                .to_string()
                .contains("unknown repair action")
        );
        assert!(repair_request(&sched, &json!({"action": "close"}))
            .err()
            .unwrap()
            .to_string()
            .contains("needs a task_id"));
        assert!(
            repair_request(&sched, &json!({"action": "ack", "fault_id": "seven"}))
                .err()
                .unwrap()
                .to_string()
                .contains("numeric fault_id")
        );
        assert!(
            repair_request(&sched, &json!({"action": "", "task_id": "rq-1"}))
                .err()
                .unwrap()
                .to_string()
                .contains("repair needs one of")
        );
    }
}
