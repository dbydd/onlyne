//! §3c's turn-end rule: one neutral nudge, then the settlement.
//!
//! A turn that ends without a completion earns exactly one nudge, and the
//! delivery settles when a second turn ends the same way. The client owns the
//! rule because the client owns the turn's bookkeeping: a plugin's private
//! ladder was per-plugin policy, and the two drives this client runs — a plugin
//! behind an adapter socket and a backend that owns its agent — have to answer
//! one way (`docs/v2-CONTRACT.md` §3c).
//!
//! The witness is a heartbeat. A beat that moves a session's agent from
//! `running` to `idle` while the delivery it serves is still open, with no
//! completion exit standing anywhere, is a turn that ended without one. An
//! `idle` beat on its own is not: a waiting session keeps beating, so the
//! transition is what counts. The same fact is what `reports`'s composition
//! writes into the tuple as `idle_waiting`.
//!
//! What the nudge can be is the drive's answer, and a drive that cannot be
//! nudged is never told it was: a plugin that mounted without `inject`, a
//! connection that has already left, and a backend whose agent is gone all
//! settle on the first ending instead. A nudge that was composed but did not
//! land — a send whose connection was already gone, a backend that could not
//! start the turn — settles the same way. There is one try, and no retry,
//! alert, or supervisor callback behind it (§3c).
//!
//! Each step publishes a client event, in order: [`TURN_END_WITHOUT_COMPLETE`],
//! [`DELIVERY_BLOCKED`], [`HANDOFF`]. They are sent to the server via
//! `ClientOp::PublishEvent`, which the server appends to its stream and answers
//! nothing. The server's stream is the single owner of these facts
//! (`AGENTS.md` §9); a client that keeps a second copy owns nothing that
//! anything reads, so the local copy is gone (`docs/v2-CONTRACT.md` §"Slice 7").

use super::*;

use super::projection::stored_task_state;
use super::settle::{SettleAuthority, on_out};
use super::state::{DispatchInner, slot_key_serving_task};
use super::transport::NUDGE_TEXT;

/// A turn ended with its task still open. Payload: `task_id`, `session_id`,
/// `role`, and `nudge` — whether this ending is the one that spends the
/// delivery's single nudge.
pub const TURN_END_WITHOUT_COMPLETE: &str = "turn_end_without_complete";

/// A delivery settled blocked. Payload: `task_id`, `session_id`, `role`.
pub const DELIVERY_BLOCKED: &str = "delivery_blocked";

/// One turn handed work on instead of finishing. Payload: `task_id`,
/// `session_id`, `role`, `to_role`, `hop`, and the text the recipient reads.
pub const HANDOFF: &str = "handoff";

/// Where one task's delivery stands with this rule.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum TurnEnd {
    /// No turn has ended without a completion yet: the next ending hands the
    /// one nudge over.
    #[default]
    Fresh,
    /// The nudge is spent: the next ending settles the delivery.
    Nudged,
    /// This delivery already settled at this door.
    Blocked,
}

/// 3c's turn-end bookkeeping, keyed by the task whose delivery it belongs to.
///
/// A task id is minted per delivery, so the key is the delivery and not the
/// session: a `task` or `role` session that serves a second delivery in one
/// conversation owes that delivery its own nudge. Entries whose delivery is over
/// are dropped on the next call ([`prune`]).
#[derive(Debug, Default)]
pub(super) struct TurnEndWatch {
    states: HashMap<String, TurnEnd>,
}

impl TurnEndWatch {
    /// Where one task's delivery stands, `Fresh` for a task never seen here.
    fn of(&self, task_id: &str) -> TurnEnd {
        self.states.get(task_id).copied().unwrap_or_default()
    }

    fn set(&mut self, task_id: &str, state: TurnEnd) {
        self.states.insert(task_id.to_string(), state);
    }

    fn forget(&mut self, task_id: &str) {
        self.states.remove(task_id);
    }

    /// Every task this watch holds a word for.
    fn tasks(&self) -> impl Iterator<Item = &String> + '_ {
        self.states.keys()
    }
}

/// Drop the entries whose delivery is over: no session serves the task any
/// more, or its row already carries a verdict.
///
/// A settled task is never re-offered under the same id, but the *slot* that
/// served it outlives the row, and a latch left behind would settle the next
/// delivery bound to that session before its first turn ended.
fn prune(inner: &mut DispatchInner) {
    let stale: Vec<String> = inner
        .turn_end
        .tasks()
        .filter(|task_id| {
            !inner
                .sessions
                .values()
                .any(|slot| slot.task_id.as_deref() == Some(task_id.as_str()))
                || stored_task_state(inner, task_id) != TaskState::Pending
        })
        .cloned()
        .collect();
    for task_id in stale {
        inner.turn_end.forget(&task_id);
    }
}

/// Where a nudge goes for one live session.
enum Nudge {
    /// The plugin that mounted for the session, reached through
    /// [`DispatchState::nudge_plugin`] — the one owner of the capability check
    /// and the frame.
    Plugin,
    /// The backend that owns the agent; the client hands the sentence over as
    /// the session's next turn.
    Backend {
        backend: Arc<dyn SessionBackend>,
        session: SessionRef,
    },
}

/// What one ending earns, decided under the lock and carried out without it.
enum Step {
    /// No step: no session serves the task, its row already carries a verdict,
    /// or this delivery settled at this door before.
    Quiet,
    /// Hand the one nudge to the drive.
    Nudge(Nudge),
    /// Settle the delivery blocked. The word is the operator's half, which the
    /// log carries and no payload does.
    Settle(&'static str),
}

/// Run 3c's rule for one turn that ended without a completion exit.
///
/// `closing` is the ending turn's own last line when the drive that witnessed
/// it has one — a self-driven backend reports the agent's closing text — and
/// `None` for a plugin beat, which carries no words.
pub async fn on_turn_end(
    state: &DispatchState,
    task_id: &str,
    closing: Option<String>,
) -> Result<()> {
    let (step, ending) = {
        let mut inner = state.inner.lock();
        decide(&mut inner, task_id)?
    };
    if let Some(op) = ending {
        state.enqueue_op(&op)?;
    }
    match step {
        Step::Quiet => Ok(()),
        // The frame, the `inject` check, and the send are `nudge_plugin`'s, so
        // the sentence has one owner and one way out. `false` is a drive this
        // ending cannot be nudged through — no connection, a plugin that never
        // declared `inject`, or a send that did not leave — and there is no
        // second try (§3c).
        Step::Nudge(Nudge::Plugin) => {
            if state.nudge_plugin(task_id).await {
                Ok(())
            } else {
                settle(
                    state,
                    task_id,
                    closing,
                    "the nudge did not reach the plugin",
                )
                .await
            }
        }
        Step::Nudge(Nudge::Backend { backend, session }) => {
            // A self-driven drive has no heartbeats, so the dispatch path
            // feeds the turn-started fact here — the same fact a plugin's
            // beat would carry — before the backend starts the turn the
            // nudge asked for.
            state.feed_turn_started(task_id);
            match backend.nudge(&session, task_id, NUDGE_TEXT) {
                Ok(()) => Ok(()),
                Err(error) => {
                    settle(
                        state,
                        task_id,
                        closing,
                        &format!("the nudge did not reach the agent: {error}"),
                    )
                    .await
                }
            }
        }
        Step::Settle(why) => settle(state, task_id, closing, why).await,
    }
}

/// Settle one delivery blocked, publishing the step's event first.
///
/// `blocked` is the verdict `Outcome::Blocked` stands for: the work waits on
/// something outside the delivery, and a board reads it as waiting rather than
/// as failed. The settlement itself is `on_out`'s — the verdict, the receipt,
/// the ack, and what a scoped session does with its binding are all the one
/// settle path's business — and this client is the witness of the ending it
/// reports, so the authority is its own.
async fn settle(
    state: &DispatchState,
    task_id: &str,
    closing: Option<String>,
    why: &str,
) -> Result<()> {
    let op = {
        let mut inner = state.inner.lock();
        inner.turn_end.set(task_id, TurnEnd::Blocked);
        let session_id =
            slot_key_serving_task(&inner, task_id).unwrap_or_else(|| task_id.to_string());
        record_blocked(&inner, task_id, &session_id, why)
    };
    state.enqueue_op(&op)?;
    tracing::info!(task = %task_id, reason = why, "a delivery settled blocked at its turn end");
    on_out(
        state,
        task_id,
        Outcome::Blocked,
        closing,
        None,
        SettleAuthority::ClientOwned,
    )
    .await
}

/// Decide what one ending earns, and publish the ending itself.
///
/// The event goes out under the same lock that advances the latch, so two
/// endings of one delivery cannot interleave their records and the operator
/// reads them in the order the rule took them.
fn decide(inner: &mut DispatchInner, task_id: &str) -> Result<(Step, Option<ClientOp>)> {
    prune(inner);
    let Some(key) = slot_key_serving_task(inner, task_id) else {
        tracing::debug!(task = %task_id, "a turn ended for a task no session of this role serves");
        return Ok((Step::Quiet, None));
    };
    // A read-only slot serves no state (§1 (b)) and a suspended one has no
    // process or agent at the other end of a sentence.
    let session = inner
        .sessions
        .get(&key)
        .filter(|slot| !slot.read_only && !slot.suspended)
        .map(|slot| slot.session.clone());
    let Some(session) = session else {
        tracing::debug!(task = %task_id, session = %key, "a turn ended for a session that serves nothing");
        return Ok((Step::Quiet, None));
    };
    if stored_task_state(inner, task_id) != TaskState::Pending {
        // The verdict landed first: this ending is the receipt's business.
        inner.turn_end.forget(task_id);
        return Ok((Step::Quiet, None));
    }
    let step = match inner.turn_end.of(task_id) {
        // The first ending spends the delivery's one nudge. Whether the drive
        // can take it is that drive's answer when the sentence would leave —
        // the plugin's `inject` is checked there, an agent that is already gone
        // answers an error — and a drive that cannot take it settles then.
        TurnEnd::Fresh => {
            let target = if inner.backend.self_driven() {
                Nudge::Backend {
                    backend: inner.backend.clone(),
                    session,
                }
            } else {
                Nudge::Plugin
            };
            inner.turn_end.set(task_id, TurnEnd::Nudged);
            Step::Nudge(target)
        }
        // The nudge is spent: a second turn ended without a completion.
        TurnEnd::Nudged => Step::Settle("a second turn ended without a completion"),
        // This delivery already settled here, and nothing about it is left to
        // decide — nor a second record of the same ending to write.
        TurnEnd::Blocked => return Ok((Step::Quiet, None)),
    };
    let op = record_ending(inner, task_id, &key, matches!(step, Step::Nudge(_)));
    Ok((step, Some(op)))
}

/// Publish 3c's `handoff` event for work one turn handed on.
///
/// Called where a handoff is accepted — the plugin's `handoff` frame and the
/// tools mount's own op — so a turn's three steps read in order on the
/// server's stream. `hop` is the depth the handed-on task sits at in its
/// family, and `text` is what its recipient will read.
pub fn record_handoff(
    state: &DispatchState,
    task_id: &str,
    to_role: &str,
    hop: u32,
    text: &str,
) -> ClientOp {
    let inner = state.inner.lock();
    let mut payload = serde_json::json!({
        "task_id": task_id,
        "role": inner.role.as_str(),
        "to_role": to_role,
        "hop": hop,
        "text": text,
    });
    if let Some(session_id) = slot_key_serving_task(&inner, task_id) {
        payload["session_id"] = serde_json::Value::String(session_id);
    }
    drop(inner);
    publish(HANDOFF, payload)
}

/// The ending's own op: which task and session it happened in, and whether
/// this is the ending that spends the delivery's single nudge.
fn record_ending(inner: &DispatchInner, task_id: &str, session_id: &str, nudge: bool) -> ClientOp {
    publish(
        TURN_END_WITHOUT_COMPLETE,
        serde_json::json!({
            "task_id": task_id,
            "session_id": session_id,
            "role": inner.role.as_str(),
            "nudge": nudge,
        }),
    )
}

/// The settlement's own op: which task and session it happened in, and why.
///
/// The reason is the one thing an operator reading a `delivery_blocked` event
/// cannot get anywhere else. The caller has it in hand — it is what this
/// client's own log line names — and a hook bound to the class is handed the
/// payload verbatim, so leaving it out made the event answer *that* a delivery
/// stopped and not *why*, which is the half that needs a human. A completion
/// row says the same thing the other way round: its `out_head` is empty, because
/// a session that never completed has no head to write.
fn record_blocked(inner: &DispatchInner, task_id: &str, session_id: &str, why: &str) -> ClientOp {
    publish(
        DELIVERY_BLOCKED,
        serde_json::json!({
            "task_id": task_id,
            "session_id": session_id,
            "role": inner.role.as_str(),
            "reason": why,
        }),
    )
}

/// The one op that carries a client-owned fact to the server, built here so
/// the three call sites cannot drift into three spellings of it.
///
/// It leaves through the durable intent queue rather than the live link. What
/// it replaced was a row in the client's own store, so the fact survived a
/// crash; a frame written to a socket would not, and the client would then owe
/// the server a fact nobody ever hears. The queue keeps that guarantee — a
/// publish is on disk before it is sent — and hands the op to the link the
/// moment the link is up (`docs/v2-CONTRACT.md` §"Slice 7").
fn publish(class: &str, payload: serde_json::Value) -> ClientOp {
    ClientOp::PublishEvent(onlyne_proto::PublishEventArgs {
        class: class.to_string(),
        payload,
    })
}
