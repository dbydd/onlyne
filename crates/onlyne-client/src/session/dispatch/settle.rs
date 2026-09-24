use super::*;

use super::outbound::{store_ack, transport_envelope};
use super::projection::{note_verdict, sync_session};
use super::retire::{release_locked, retire_idle_locked};
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
fn turn_recorded(inner: &DispatchInner, task_id: &str) -> (bool, String) {
    let Ok(Some(row)) = inner.store.get_session(task_id) else {
        return (false, "no session row".to_string());
    };
    let agent = stored_observation(&inner.store, Some(&row)).agent;
    (
        matches!(agent, AgentState::Running | AgentState::Idle),
        row.agent_state,
    )
}

/// Settle one finished task: relay what its report asked to hand on, publish the
/// verdict, and answer the sender.
///
/// The relay runs first and on purpose. A role that takes the handed-on task
/// must find the chain already pointing at it when the completion receipt
/// arrives, and a handoff that outlives this call has no caller left to record
/// its refusal.
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
    head_kind: Option<&str>,
    handoffs: &[Handoff],
    asked: SettleAuthority,
) -> Result<()> {
    if asked == SettleAuthority::PluginReport {
        let inner = state.inner.lock();
        let (turn, phase) = turn_recorded(&inner, task_id);
        if !turn {
            let reason = format!(
                "no turn ran: the agent phase this client holds for the session reads {phase}"
            );
            onlyne_session::record_fault(
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
    let settled = {
        let mut inner = state.inner.lock();
        let verdict = settle(&inner.bridge, &inner.store, task_id)?;
        // The verdict lands in the task's own record, after the delivery drain
        // that makes it `accepted`. `settle` invents no receipt, so a session
        // whose task is settled and whose delivery drained is the pair `project`
        // reads as `exited`; writing the task record first would leave a verdict
        // beside a delivery that never drained if the drain failed, and the
        // report that would fix it has already been answered.
        if !inner.store.settle_task(task_id, task_state_of(outcome))? {
            tracing::warn!(
                task = %task_id,
                ?outcome,
                "a second verdict arrived for a settled task; the first one stands"
            );
            note_verdict(&verdict, task_id);
            // The replayed session still owns the task binding until this
            // release, so the standing verdict travels with the client's own
            // post-release tuple and capacity returns to the role.
            release_locked(&mut inner, task_id, None)?;
            None
        } else {
            inner
                .store
                .put_out_head(task_id, head.as_deref().unwrap_or(""))?;
            // The handle and the chain this answer travels on belong to the session
            // serving the task, not to a read-only one that came back for it.
            let slot =
                slot_key_serving_task(&inner, task_id).and_then(|key| inner.sessions.get_mut(&key));
            let origin = slot.as_ref().and_then(|slot| slot.origin.clone());
            let causality = slot.as_ref().map(|slot| slot.causality.clone());
            let msg_id = slot.and_then(|slot| slot.msg_id.take());
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
            // Whatever a read-only connection held for this task is answered by this
            // completion, so it leaves the buffer here and travels beside the report.
            let held = inner.held_handoffs.remove(task_id);

            Some((
                verdict,
                completion_envelope(
                    &inner.role,
                    origin,
                    task_id,
                    head.as_deref(),
                    causality.as_ref(),
                ),
                inner.role.clone(),
                causality,
                held,
            ))
        }
    };
    // The refused branch carries the release result out of the lock. The task
    // account remains the first verdict. The client row is published after the
    // replay session has returned its binding and completed its retirement.
    let Some((verdict, receipt, role, causality, held)) = settled else {
        return sync_session(state, task_id).await;
    };
    note_verdict(&verdict, task_id);
    // Every relay is answered before the verdict travels, and none of them
    // moves it: a refused handoff is a record on the settled task, not a
    // different outcome for it. The merge happens on the way in, so a downstream
    // role reads one envelope for this task, not two.
    let routed = merged_handoffs(
        handoffs,
        held.as_deref(),
        head.as_deref().unwrap_or_default(),
    );
    // A settle can race the retirement of the session it serves, and a task no
    // slot answers for is still a task the relay may name: the fallback is the
    // task itself as the root of its own family, which is the child link a
    // missing chain would have produced for it.
    let parent = causality.unwrap_or_else(|| Causality::root(task_id));
    let denied = handoff::route(
        state,
        &role,
        &parent,
        head_kind,
        head.as_deref().unwrap_or_default(),
        &routed,
    )
    .await;
    record_denials(state, task_id, &denied)?;
    // The merged relay has left, so the read-only session that wrote its half of
    // it is retired. The settled account above is the whole settlement: nothing
    // here settles or releases this task a second time.
    retire_revived(state, task_id).await;
    // The terminal receipt leaves as its own envelope, so the origin — a role
    // or a gateway conversation — learns the outcome (plan §3 `Completion`).
    // It rides the intent queue, which is what makes a completion survive the
    // disconnect rules of §6 line 289.
    if let Some(envelope) = receipt {
        transport_envelope(state, &envelope).await?;
    }
    sync_session(state, task_id).await
}

/// One relay per downstream role, carrying this completion's own lines and the
/// ones a read-only connection held for the same task.
///
/// Each line keeps the marker of the session that wrote it — `[retry]` for the
/// session that finished and `[zombie]` for the one that came back for the task
/// and was held — so the recipient can tell the two accounts apart inside the one
/// envelope. Line order follows the report's own order, with the held lines of the
/// same role below them. Nothing held is the ordinary case, and it routes the
/// report's lines without copying them.
fn merged_handoffs<'a>(
    own: &'a [Handoff],
    held: Option<&'a [Handoff]>,
    head: &str,
) -> Cow<'a, [Handoff]> {
    let held: &[Handoff] = match held {
        Some(held) if !held.is_empty() => held,
        _ => return Cow::Borrowed(own),
    };
    let mut order: Vec<String> = Vec::new();
    let mut segments: HashMap<String, Vec<String>> = HashMap::new();
    for (marker, group) in [("[retry]", own), ("[zombie]", held)] {
        for handoff in group {
            let line = format!("{marker} {}", handoff.text_or(head));
            if !segments.contains_key(handoff.to_role.as_str()) {
                order.push(handoff.to_role.clone());
            }
            segments
                .entry(handoff.to_role.clone())
                .or_default()
                .push(line);
        }
    }
    Cow::Owned(
        order
            .into_iter()
            .map(|to_role| Handoff {
                text: Some(
                    segments
                        .remove(to_role.as_str())
                        .unwrap_or_default()
                        .join("\n"),
                ),
                to_role,
            })
            .collect(),
    )
}

/// Retire the read-only connections and slots a merged handoff has just answered.
///
/// A connection that came back for a session another connection serves is
/// dropped from that session's record and its agent is told to leave, since what
/// it had to say travelled with the relay above. A slot that lost its task to a
/// newer session has the transport naming it dropped, its task binding released,
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
async fn retire_revived(state: &DispatchState, task_id: &str) {
    let leaving = {
        let mut inner = state.inner.lock();
        let mut leaving: Vec<AdapterIo> = Vec::new();
        let mut silent: Vec<String> = Vec::new();
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
                silent.push(session_id.clone());
            }
        }
        inner
            .revived
            .retain(|(session_id, _, _)| !silent.contains(session_id));
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
            retire_idle_locked(&mut inner, &key, onlyne_session::CloseReason::Replaced);
        }
        leaving
    };
    for io in leaving {
        let notice = AdapterMsg::Host(HostOp::Bye(onlyne_proto::ByeNotice {
            reason: "the session that took this task answered for yours".into(),
        }));
        if let Err(error) = io.notify(notice).await {
            tracing::debug!(error = %error, "the read-only connection had already left");
        }
    }
}

/// Write down the relays this role could not send.
///
/// Each refusal gets an event of its own, because that is the plane a supervisor
/// reads to see which handoff line died. The fault queue dedups on
/// `(task, kind, generation)`, so the first refusal of a turn is also the one
/// the task's fault row names; the rest stay in the events.
fn record_denials(state: &DispatchState, task_id: &str, denied: &[Denial]) -> Result<()> {
    if denied.is_empty() {
        return Ok(());
    }
    let inner = state.inner.lock();
    for refusal in denied {
        tracing::warn!(
            task = %task_id,
            to_role = %refusal.to_role,
            error = %refusal.reason,
            "handoff denied"
        );
        inner.store.append_event(
            "handoff_denied",
            &serde_json::json!({
                "task_id": task_id,
                "to_role": refusal.to_role,
                "text": refusal.text,
                "error": refusal.reason,
            }),
        )?;
        onlyne_session::record_fault(
            &inner.store,
            task_id,
            "handoff_denied",
            "acp",
            &format!("{}: {}", refusal.to_role, refusal.reason),
        )?;
    }
    Ok(())
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
    causality: Option<&Causality>,
) -> Option<Envelope> {
    let origin = origin?;
    // A turn that left no result line still ends its task, and the sender still
    // gets its answer: an empty body travels as `text: Some("")`, which the
    // validator accepts, where an absent body would drop the receipt and leave
    // the origin waiting on a task this role has already retired.
    let body = Body::text(head.unwrap_or_default());
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
        // The queue path answers with the frame's `op_id`: a connection this
        // client holds read-only serves no session, and the check above refused
        // that connection before this line.
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
