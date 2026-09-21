use super::*;

use super::outbound::{store_ack, transport_envelope};
use super::projection::{note_verdict, sync_session};
use super::retire::{release_locked, retire_idle_locked};
use super::state::{DispatchState, slot_key_named, slot_key_serving_task, slot_task};
use super::transport::names_session;

/// Settle one finished task: relay what its report asked to hand on, publish the
/// verdict, and answer the sender.
///
/// The relay runs first and on purpose. A role that takes the handed-on task
/// must find the chain already pointing at it when the completion receipt
/// arrives, and a handoff that outlives this call has no caller left to record
/// its refusal.
///
/// A second verdict for a task whose record is already settled is refused whole:
/// the first verdict stands and this one leaves no receipt, no delivery ack and
/// no binding hand-back behind.
pub async fn on_out(
    state: &DispatchState,
    task_id: &str,
    outcome: Outcome,
    head: Option<String>,
    head_kind: Option<&str>,
    handoffs: &[Handoff],
) -> Result<()> {
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
            let hop = slot.as_ref().map(|slot| slot.hop).unwrap_or(0);
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
                completion_envelope(&inner.role, origin, task_id, head.as_deref()),
                inner.role.clone(),
                hop,
                held,
            ))
        }
    };
    // A refused verdict refuses everything else this call would do, and that is
    // what the `None` above carries out of the lock. The receipt would answer the
    // origin for work the first verdict already answered; the delivery handle
    // would be spent from the session that is still serving the task; and
    // `release_locked` resolves through `slot_key_serving_task`, so it would hand
    // back that session's binding and leave its own later completion with no
    // handle to ack — the server's row would stay in flight. A refused verdict
    // does none of it.
    //
    // The one write that did happen is `settle`'s above, and on a task already
    // settled it reduces the tuple to what is already stored: the row stays where
    // the first verdict put it, and its publish still travels, because state
    // committed and left unpublished is the mismatch the ordering above exists to
    // prevent.
    let Some((verdict, receipt, role, hop, held)) = settled else {
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
    let denied = handoff::route(
        state,
        &role,
        task_id,
        hop,
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
fn completion_envelope(
    role: &str,
    origin: Option<Principal>,
    task_id: &str,
    head: Option<&str>,
) -> Option<Envelope> {
    let origin = origin?;
    // A turn that left no result line still ends its task, and the sender still
    // gets its answer: an empty body travels as `text: Some("")`, which the
    // validator accepts, where an absent body would drop the receipt and leave
    // the origin waiting on a task this role has already retired.
    let body = Body::text(head.unwrap_or_default());
    let causality = Causality {
        task: task_id.to_string(),
        parent_task: None,
        reply_to: None,
        hop: 0,
        attempt: 0,
    };
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

#[cfg(test)]
mod tests;
