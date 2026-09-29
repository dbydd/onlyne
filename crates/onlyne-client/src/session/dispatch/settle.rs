use super::*;

use super::outbound::{store_ack, transport_envelope};
use super::projection::{note_verdict, phase, sync_session};
use super::retire::{PendingClose, close_retired, release_locked, retire_idle_locked};
use super::state::{
    DispatchInner, DispatchState, slot_key_named, slot_key_serving_task, slot_task,
};
use super::transport::{names_session, serves_session};
use onlyne_proto::ErrorCode;
use onlyne_proto::adapter::HandoffArgs;

/// Fault kind for a completion this client refused for want of a turn. The word
/// is what `onlyne faults` and `onlyne-client status` carry, so it names the
/// reading that refused the frame.
pub const SETTLE_WITHOUT_TURN: &str = "settle_without_turn";

/// Who asked for one settle, which is what decides whether the never-ran guard
/// reads it.
///
/// The guard exists for the door where the claim and the claimant are the same
/// party: a plugin reports its own ending, and a session whose agent never ran
/// can report one too. The other two doors carry the client's own act, so their
/// evidence is already in this process — a self-driven backend that watched its
/// agent end, or an operator's `control` command that asked for the ending.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettleAuthority {
    /// A `complete` report from a plugin connection. Guarded.
    PluginReport,
    /// A terminal fact this client reached through its own eyes: the ending a
    /// self-driven backend reported through `outcome_loop`. Unguarded: this
    /// client is the witness of the work it watched.
    ClientOwned,
    /// The answer to a `control recycle` or `control cancel` this client issued
    /// for the task. Unguarded: the operator asked for this ending, and the
    /// plugin's report and the retirement this command runs race over the row,
    /// so the row's phase at the moment the frame lands decides nothing.
    ControlDriven,
}

/// Whether one session's own row carries a turn, and the agent-phase word it
/// holds for the operator's reading.
///
/// The row is this client's record: a beat's `agent` dimension reaches it
/// through the `(generation, seq)` gate in `apply_persist`, and the ready
/// barrier's `feed_ready` writes `Ready` into the same column. `Running` and
/// `Idle` are the two phases a turn puts the tuple through
/// (`crates/onlyne-session/src/lifecycle/state.rs:11`), so one of them is the
/// answer. `Booting`, `Ready` and `Gone` are each a session that has run nothing
/// this client can point at: `Gone` is written by `AgentGone` and
/// `ResourceClosed` from any live phase, which leaves the death of an agent that
/// never started reading exactly like the death of one that worked. A task this
/// client holds no row for answers the same way, with its own word in the reason.
///
/// One read answers both halves, and both come off the row's own column: the
/// word the operator is handed is the word `projection_of` publishes, so the
/// refusal cannot name a phase other than the reading that caused it. The tuple
/// inside `observed_json` is a second source for the same dimension, and a row
/// whose bytes are unparsable rebuilds to `Booting` beside a column still
/// reading `running` — a guard that decided on one and reported the other, and
/// a read-only door that wrote an alert and a ledger event while answering.
fn turn_recorded(inner: &DispatchInner, task_id: &str) -> (bool, String) {
    let Ok(Some(row)) = inner.store.get_session(task_id) else {
        return (false, "no session row".to_string());
    };
    let agent = phase(&row.agent_state, AgentPhase::Booting);
    (
        matches!(agent, AgentPhase::Running | AgentPhase::Idle),
        row.agent_state,
    )
}

/// Settle one finished task: publish the verdict, retire what answered for it,
/// and let the receipt leave.
///
/// No handoff is routed here. A session's handoffs travel as its own `handoff`
/// frames (`handoff.rs`), answered where the session sends them, so a
/// settlement is only the completion's half of the account: the verdict, the
/// receipt, and the resources this task was holding.
///
/// A replayed delivery for work whose first verdict already settled is ordinary
/// at-least-once traffic. The first verdict stands. The replay returns the task
/// binding. The session that took the replay lets go of its slot.
/// The first receipt, `out_head`, and handoff relay remain attached to that
/// first settlement.
///
/// A plugin report with no turn behind it is refused whole the same way, ahead of
/// every write this call makes: the drain opens over work that never ran, so the
/// completion intent, the verdict, the `out_head` line and the delivery ack all
/// stay unwritten and the row is left for the server's requeue. The frame is
/// answered as an applied one — a plugin treats a failed report as a link that
/// died, and sends the same terminal fact again — and the fault the refusal
/// leaves behind names the reading that refused.
pub async fn on_out(
    state: &DispatchState,
    task_id: &str,
    outcome: Outcome,
    head: Option<String>,
    details: Option<String>,
    asked: SettleAuthority,
) -> Result<()> {
    if asked == SettleAuthority::PluginReport {
        let inner = state.inner.lock();
        let (turn, phase) = turn_recorded(&inner, task_id);
        if !turn {
            let reason = format!(
                "no turn ran: the agent phase this client holds for the session reads {phase}"
            );
            crate::reconcile::record_fault(
                &inner.store,
                task_id,
                SETTLE_WITHOUT_TURN,
                "client",
                &reason,
            )?;
            tracing::warn!(
                task = %task_id,
                ?outcome,
                phase = %phase,
                "a completion arrived for a session that never ran a turn; the task stays open"
            );
            return Ok(());
        }
    }
    let (settled, session_id) = {
        let mut inner = state.inner.lock();
        if take_verdict(&inner, task_id, outcome)? {
            inner
                .store
                .put_out_head(task_id, head.as_deref().unwrap_or(""))?;
            // The handle and the chain this answer travels on belong to the session
            // serving the task, not to a read-only one that came back for it.
            let key = slot_key_serving_task(&inner, task_id);
            let slot = key.as_deref().and_then(|key| inner.sessions.get_mut(key));
            let origin = slot.as_ref().and_then(|slot| slot.origin.clone());
            let causality = slot.as_ref().map(|slot| slot.causality.clone());
            let msg_id = slot.and_then(|slot| slot.msg_id.take());
            // The publish that follows names the session, not the delivery: a
            // scoped session outlives this delivery and its row has to read as a
            // live session serving nothing rather than as a session that exited.
            let session_id = key.unwrap_or_else(|| task_id.to_string());
            if let Some(msg_id) = msg_id {
                store_ack(
                    &inner,
                    AckArgs {
                        msg_id,
                        op_id: None,
                        accepted: true,
                        reason: None,
                    },
                );
            }
            // A settled session gives its capacity back, so a role at
            // `max_sessions` takes the next row instead of holding finished slots.
            release_locked(&mut inner, task_id, None)?;
            (
                Some((completion_envelope(
                    &inner.role,
                    origin,
                    task_id,
                    head.as_deref(),
                    details.as_deref(),
                    causality.as_ref(),
                ),)),
                session_id,
            )
        } else {
            tracing::warn!(
                task = %task_id,
                ?outcome,
                "a second verdict arrived for a settled task; the first one stands"
            );
            // The replayed session still owns the task binding until this
            // release, so the standing verdict travels with the client's own
            // post-release tuple and capacity returns to the role.
            let session_id =
                slot_key_serving_task(&inner, task_id).unwrap_or_else(|| task_id.to_string());
            release_locked(&mut inner, task_id, None)?;
            (None, session_id)
        }
    };
    // The refused branch carries no receipt: the task account remains the first
    // verdict, and the client's own row is published now that the replay session
    // has returned its binding and completed its retirement.
    let Some((receipt,)) = settled else {
        return sync_session(state, &session_id).await;
    };
    // A connection that came back for this task has now had its ending answered,
    // so it is retired here. The settled account above is the whole settlement:
    // nothing here settles or releases this task a second time.
    retire_revived(state, task_id).await;
    // The terminal receipt leaves as its own envelope, so the origin — a role
    // or a gateway conversation — learns the outcome (plan §3 `Completion`).
    // It rides the intent queue, which is what makes a completion survive the
    // disconnect rules of §6 line 289.
    if let Some(envelope) = receipt {
        transport_envelope(state, &envelope).await?;
    }
    sync_session(state, &session_id).await
}

/// Drain one session's completion and file its task's verdict, answered with
/// whether this report is the first verdict the task took.
///
/// The drain runs first and the verdict lands behind it, because the order is the
/// row's own need: `settle` closes the completion intent, and a settled task beside
/// a delivery that never drained is the pair `project` cannot read as `exited`. A
/// report arriving behind a standing verdict therefore still drains the session
/// that sent it — the retried row whose task the grace sweep had already answered,
/// and the replayed session the caller's release retires — and only its own verdict
/// is refused. Which verdict the task keeps is `settle_task`'s answer either way:
/// it writes where `settled_at IS NULL` and refuses to move one that is stamped.
///
/// A drain with no subject is the one thing this order cannot carry. A task this
/// client holds no row for has no intent to close, and `settle` answers the attempt
/// with `unknown session`, which used to fail the whole report ahead of the verdict
/// its record is still entitled to take — the shape a `client.db` replaced under a
/// live role, or a foreign task reported into one, arrives in. The row is read for
/// that alone, and the verdict is filed whatever the reading says.
fn take_verdict(inner: &DispatchInner, task_id: &str, outcome: Outcome) -> Result<bool> {
    let drain = inner
        .store
        .get_session(task_id)?
        .map(|_| settle(&inner.bridge, &inner.store, task_id))
        .transpose()?;
    let first = inner.store.settle_task(task_id, task_state_of(outcome))?;
    if let Some(verdict) = drain {
        note_verdict(&verdict, task_id);
    }
    Ok(first)
}

/// Retire the read-only connections and slots a settled task has just answered.
///
/// A connection that came back for a session another connection serves is
/// dropped from that session's record and its agent is told to leave, because
/// the ending it reported is the whole of what it still had to say. A slot that
/// lost its task to a newer session has the transport naming it dropped, its
/// task binding released,
/// and is retired as `Replaced`: the resource its agent was holding is this
/// client's to close, and the newer session answers for the task. The account for
/// the task is the settlement above.
///
/// A connection inside its own inbound frame is left alone. `adapter_socket`
/// awaits the handler before it answers the frame, so a bye written here would
/// leave ahead of that connection's own response, and the plugin's bye handler
/// drops the socket and rejects every request awaiting an answer — a completion
/// the ledger already holds would reach the agent as a failure it retries. The
/// entry stays in `revived`: the connection's `detach` frame or its socket end
/// retires it through `release_connection`, and the plugin that just completed
/// ends its own session either way.
///
/// The name a connection mounted with is how this sweep judges which task that
/// connection came back for, and the connection is what it removes. Two held
/// connections can carry one name — `record_revived_connection` dedups per
/// connection, and an agent that redials twice while another serves its session
/// is held twice — and the one inside its own frame is left in the buffer by the
/// rule above. Dropping held entries by name would then take that connection's
/// entry with it: no bye reached it, nothing promoted it, and `release_connection`
/// could no longer find the socket it still holds, which is a held connection
/// neither silenced nor served.
///
/// A name that resolves to no slot is the other half of the judgement, and the
/// answer there is the name itself: `dispatch` mints a session id from the task,
/// so a held connection whose slot has already retired is one that came back for
/// this task and for no other.
async fn retire_revived(state: &DispatchState, task_id: &str) {
    let (leaving, pending) = {
        let mut inner = state.inner.lock();
        let mut leaving: Vec<AdapterIo> = Vec::new();
        let mut pending: Vec<PendingClose> = Vec::new();
        for (session_id, io, _) in inner.revived.iter() {
            if inner.in_frame.iter().any(|busy| busy.same_connection(io)) {
                continue;
            }
            let reaches = slot_key_named(&inner, session_id)
                .and_then(|key| {
                    inner
                        .sessions
                        .get(&key)
                        .map(|slot| slot_task(slot) == task_id)
                })
                .unwrap_or_else(|| session_id == task_id);
            if reaches {
                leaving.push(io.clone());
            }
        }
        inner
            .revived
            .retain(|(_, revived, _)| !leaving.iter().any(|io| io.same_connection(revived)));
        let silenced: Vec<String> = inner
            .sessions
            .iter()
            .filter(|(_, slot)| slot.read_only && slot_task(slot) == task_id)
            .map(|(key, _)| key.clone())
            .collect();
        for key in silenced {
            let Some(slot) = inner.sessions.get(&key).cloned() else {
                continue;
            };
            if slot.payload.is_some() {
                tracing::warn!(
                    session = %key,
                    task = %task_id,
                    "a read-only session retires with a payload it was never handed"
                );
            }
            let served: Vec<String> = inner
                .transports
                .keys()
                .filter(|served| names_session(&key, &slot, served))
                .cloned()
                .collect();
            for session_id in served {
                inner.transports.remove(&session_id);
            }
            if let Some(current) = inner.sessions.get_mut(&key) {
                current.task_id = None;
                current.ready = false;
                current.read_only = false;
                current.dropped_at = None;
            }
            retire_idle_locked(
                &mut inner,
                &key,
                crate::backend::CloseReason::Replaced,
                &mut pending,
            );
        }
        (leaving, pending)
    };
    // The replaced sessions' resources are this client's to give back, and the
    // hosts take their time about it: the sweep already wrote every row and took
    // every slot, so the close runs off the lock, ahead of the byes below that
    // await the network on each held connection.
    close_retired(pending);
    for io in leaving {
        let notice = AdapterMsg::Host(HostOp::Bye(onlyne_proto::ByeNotice {
            reason: "the session that took this task answered for yours".into(),
        }));
        if let Err(error) = io.notify(notice).await {
            tracing::debug!(error = %error, "the read-only connection had already left");
        }
    }
}

/// The receipt for one finished task, or `None` when its sender is unknown.
///
/// Every settled task answers its sender, the role that sent the task included:
/// §3's `Completion` is the durable record that the work ended, and a role
/// reading its own receipt ack is what settles the row.
///
/// The receipt names the task it answers and carries that task's own family
/// figures — the family id, the hop budget, the origin, the deadline, and the
/// labels — so a run's tasks and its completions print the same arc in
/// `onlyne ledger`. It sits at the depth of the task it answers, and it is no
/// link in the chain: it names no parent and replies to nothing. A task whose
/// slot the client no longer holds, which is a row an older build opened, keeps
/// the shape of a bare receipt.
fn completion_envelope(
    role: &str,
    origin: Option<Principal>,
    task_id: &str,
    head: Option<&str>,
    details: Option<&str>,
    causality: Option<&Causality>,
) -> Option<Envelope> {
    let origin = origin?;
    // The body carries the full result when the report named one, falling back
    // to the one-line summary, and an empty string when neither is present — a
    // turn that left no result still ends its task, and the sender still gets
    // its answer as `text: Some("")`, which the validator accepts. The summary
    // rides alongside in `head`, so a store that keeps a one-line preview
    // shows it rather than the first clusters of the result.
    let body = Body {
        text: Some(details.or(head).unwrap_or_default().to_string()),
        head: head.map(str::to_string),
        image: None,
    };
    let mut causality = causality.cloned().unwrap_or_default();
    causality.task = task_id.to_string();
    causality.parent_task = None;
    causality.reply_to = None;
    // A receipt is written here, so it carries no redelivery count of its own.
    causality.attempt = 0;
    // `new_envelope` validates every protocol rule on the way out, so a receipt
    // that cannot be addressed to its sender is the only one that goes unsent.
    new_envelope(
        MsgKind::Completion,
        Principal::role(role),
        origin,
        body,
        Some(causality),
    )
    .ok()
}

impl DispatchState {
    /// Take one plugin `handoff` frame and answer what the plugin is told.
    ///
    /// The frame names the task the session is handing on and the role it goes
    /// to. The child is minted here, through the builder the report-driven path
    /// uses, so the family id and the family's figures ride along and the depth
    /// grows by one hop. The envelope leaves on the queue the plugin `send` op
    /// writes to.
    ///
    /// The answer names the child:
    /// `{"task_id": "<uuid>", "hop": 3, "queued": true, "op_id": "<uuid>"}`
    /// (`onlyne_proto::HandoffArgs`).
    ///
    /// A frame is answered only for the connection serving the task it names.
    /// An unknown task and a foreign connection earn the same code and the same
    /// field, and their messages say which of the two refused the frame.
    pub fn plugin_handoff(&self, io: &AdapterIo, args: HandoffArgs) -> ResBody {
        let (role, parent) = {
            let inner = self.inner.lock();
            let found = slot_key_serving_task(&inner, &args.task_id).and_then(|key| {
                inner
                    .sessions
                    .get(&key)
                    .map(|slot| (key, slot.causality.clone()))
            });
            let Some((key, parent)) = found else {
                return ResBody::err(
                    ErrorCode::Invalid,
                    format!("no session serves task {}", args.task_id),
                    Some("task_id".into()),
                );
            };
            if !serves_session(&inner, &key, io) {
                return ResBody::err(
                    ErrorCode::Invalid,
                    format!("this connection does not serve task {}", args.task_id),
                    Some("task_id".into()),
                );
            }
            (inner.role.clone(), parent)
        };
        let (envelope, child) =
            match handoff::relay(&role, &parent, &args.to, &args.text, args.image) {
                Ok(built) => built,
                Err(message) => return ResBody::err(ErrorCode::Invalid, message, None),
            };
        let queued = match self.plugin_send(io, &envelope) {
            Ok(queued) => queued,
            Err(error) => return ResBody::err(ErrorCode::Internal, error.to_string(), None),
        };
        // §4's durable class for this op: which handoff left this client, for
        // whom, and on which hop of the chain. The enqueue above already
        // succeeded, so the event cannot describe a handoff that never was; a
        // failure here is the intent table's own, and the relay has left. The
        // op goes to the server's stream, which is the fact's one owner; the
        // durable queue is what makes the handoff event survive a crash the way
        // the client's own row used to (`docs/v2-CONTRACT.md` §"Slice 7").
        let op =
            super::turn_end::record_handoff(self, &args.task_id, &args.to, child.hop, &args.text);
        if let Err(error) = self.enqueue_op(&op) {
            tracing::warn!(
                task = %args.task_id,
                to = %args.to,
                error = %error,
                "the handoff event was not queued"
            );
        }
        // The queue path answers with the frame's `op_id`: a connection this
        // client holds read-only serves no session, and the capability check
        // inside `plugin_send` refuses that connection before this line.
        ResBody::ok(serde_json::json!({
            "task_id": child.task,
            "hop": child.hop,
            "queued": true,
            "op_id": queued["op_id"],
        }))
    }
}

#[cfg(test)]
mod tests;
