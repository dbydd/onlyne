use super::*;

use super::projection::{note_verdict, sync_session};
use super::retire::on_recycled;
use super::settle::on_out;
use super::state::note_beat;
use super::transport::serves_session;

/// Act on one control command that arrived as a delivery.
///
/// `recycle` and `cancel` reach the agent first, so it ends its own turn and
/// sends its terminal report, and the backend close follows whatever the plugin
/// did: a session whose adapter is gone still loses its process, which is the
/// half of §7's recovery ladder the operator drives by hand otherwise. `probe`
/// asks the plugin for a fresh observation and, when that question went out,
/// republishes the projection the reducer already holds, `snapshot` republishes
/// alone, and `focus` asks the backend to bring the live session to the front.
///
/// Answers whether the command named a session this client holds. A `false` is
/// the honest answer for a task this role does not own, and the caller still
/// settles the row: re-offering a command no client can act on spends the
/// delivery forever.
pub async fn on_control(state: &DispatchState, op: &ControlOp) -> Result<bool> {
    let task_id = op.task_id();
    let held = state.holds_task(task_id);
    match op {
        ControlOp::Recycle { reason, .. } => {
            state.recycle_plugin(task_id, reason, None).await;
            on_recycled(state, task_id, onlyne_session::CloseReason::Operator)?;
        }
        ControlOp::Cancel { reason, .. } => {
            state
                .recycle_plugin(task_id, reason, Some(Outcome::Cancelled))
                .await;
            on_recycled(state, task_id, onlyne_session::CloseReason::Cancelled)?;
        }
        ControlOp::Probe { .. } => {
            // A probe that found no transport asked nothing, so there is no fresh
            // observation to publish: republishing here would write the projection
            // an answer would have earned for a session nobody serves.
            if state.probe_plugin(task_id).await {
                sync_session(state, task_id).await?;
            }
        }
        ControlOp::Snapshot { .. } => sync_session(state, task_id).await?,
        ControlOp::Focus { .. } => {
            let (backend, session) = {
                let inner = state.inner.lock();
                let session = inner
                    .sessions
                    .values()
                    .find(|slot| slot.task_id.as_deref() == Some(task_id))
                    .map(|slot| slot.session.clone());
                (inner.backend.clone(), session)
            };
            match session {
                Some(session_ref) => {
                    if let Err(error) = backend.focus(&session_ref) {
                        tracing::warn!(error = %error, task = %task_id, "focus refused");
                        if let Err(fault_error) = on_plugin_report(
                            state,
                            // The client files this fault itself, so no plugin
                            // connection is its sender.
                            None,
                            Report::Fault {
                                task_id: Some(task_id.to_string()),
                                session_id: None,
                                generation: None,
                                seq: None,
                                kind: "focus".into(),
                                reason: error.to_string(),
                                desired: None,
                                observed: None,
                            },
                        )
                        .await
                        {
                            tracing::warn!(
                                error = %fault_error,
                                task = %task_id,
                                "focus refused"
                            );
                        }
                    }
                }
                None => {
                    tracing::warn!(task = %task_id, "focus has no live session");
                }
            }
        }
    }
    Ok(held)
}

/// `from` is the connection the frame arrived on, which is what decides whether
/// the report may move the session it names: only the connection serving a
/// session speaks for it, and a connection this client holds read-only is served
/// no state at all (§1 (b)). `None` is a report this client composes for itself —
/// the fault a refused `focus` files — and it is the session's own authority.
///
/// A refusal is not an error the sender reads: the frame is answered exactly as
/// an applied one is, because a plugin treats a failed report as a link that
/// died and sends the same thing again. What a refused state frame does instead
/// of being applied is leave the tuple where the serving connection left it and
/// say so here, in the log the operator reads.
pub async fn on_plugin_report(
    state: &DispatchState,
    from: Option<&AdapterIo>,
    report: Report,
) -> Result<()> {
    let subject = task_id_of(&report).to_string();
    let touched = match report {
        Report::Ready { task_id, .. } => {
            let verdict = {
                let inner = state.inner.lock();
                feed_ready(&inner.bridge, &inner.store, &task_id)?
            };
            note_verdict(&verdict, &task_id).is_some()
        }
        Report::Heartbeat {
            task_id,
            // The plugin's own generation is deliberately unread. It is a
            // constant the plugin never raises, and the beat's version takes the
            // generation the session's tuple holds instead — see the stamp
            // below.
            generation: _,
            seq,
            observed,
            ..
        } => {
            // The beat itself is the liveness fact. The server times
            // heartbeats. A quiet, alive session keeps landing fresh rows.
            let mut inner = state.inner.lock();
            if from.is_some_and(|io| !serves_session(&inner, &task_id, io)) {
                // A connection the client holds read-only is not this session's
                // transport, so its observation is not this session's state: the
                // whole point of the demotion is that the task answers through
                // the connection serving it now. The beat travels no further than
                // this refusal — a beat has no recipient to hold it for, unlike
                // the `send` of §1 (c) — and it is logged rather than dropped in
                // silence, because a supervisor reading the trail of a session
                // whose tuple stopped moving wants to see who was talking.
                tracing::warn!(
                    task = %task_id,
                    "a beat from a connection held read-only is not applied"
                );
                false
            } else {
                // The beat's version is the session's own generation beside the
                // reporter's sequence, never the plugin's generation field. That
                // field is a constant a plugin never raises, so taking it
                // verbatim made every frame of a session whose generation had
                // moved — which is what a returning agent's rebase does — read
                // as a stale generation and be dropped. The session's tuple is
                // the authority on which generation is reporting, and the
                // sequence stays the reporter's own: it is the only part of the
                // watermark the plugin is the witness of.
                let row = inner.store.get_session(&task_id).ok().flatten();
                let stored = stored_observation(&inner.store, row.as_ref());
                let generation = stored.version.generation;
                let verdict = match serde_json::from_value::<Observation>(observed) {
                    Ok(body) => apply_persist(
                        &inner.bridge,
                        &inner.store,
                        &task_id,
                        &LifecycleEvent::Heartbeat {
                            v: Version::new(generation, seq),
                            body: compose_observation(&stored, body),
                        },
                    )?,
                    Err(error) => {
                        tracing::warn!(task = %task_id, error = %error, "heartbeat carries no readable observation; liveness only");
                        Verdict::Ignored(IgnoredReason::NoOp)
                    }
                };
                note_verdict(&verdict, &task_id);
                match &verdict {
                    Verdict::Applied(_) => {
                        note_beat(&mut inner, &task_id, Instant::now());
                        inner.stall.note_applied(&task_id, Instant::now());
                        true
                    }
                    Verdict::Ignored(IgnoredReason::NoOp) => inner
                        .store
                        .bump_session_version(&task_id, generation, seq)?,
                    Verdict::Ignored(_) | Verdict::Rejected(_) => false,
                }
            }
        }
        Report::Complete {
            task_id,
            outcome,
            head,
            ..
        } => {
            // A plugin reports its own ending, and it hands nothing on: the
            // report file is the only place handoff lines are put down, and that
            // is a route a plugin-backed session does not have.
            on_out(state, &task_id, outcome, head, None, &[]).await?;
            false
        }
        Report::Fault {
            task_id: Some(task_id),
            kind,
            reason,
            ..
        } => {
            let inner = state.inner.lock();
            onlyne_session::record_fault(&inner.store, &task_id, &kind, "plugin", &reason)?;
            false
        }
        Report::Fault { task_id: None, .. } => false,
    };
    if touched {
        sync_session(state, &subject).await?;
    }
    Ok(())
}

/// Compose one plugin heartbeat into the tuple the reducer is allowed to read.
///
/// `client` is the stored tuple the caller already read through
/// `stored_observation` (`client.db`'s `sessions` row) — the same read the beat's
/// version is stamped from, so one frame costs one read of the row it names.
///
/// A heartbeat is authority about exactly three dimensions: the `agent` state
/// the plugin's own turn hooks witnessed, the `resource` it knows because its
/// process is live in it, and the `host` — the pane only this process can name
/// from inside. `delivery`, `recovery`, `generation_live`, `isolate_after`,
/// `terminate_after` and `mismatch_count` are not in that set. The completion
/// intent is created, retried and receipted by this client's own settle path
/// (`settle.rs`, fed from `report.complete`; `onlyne-session` exposes no
/// `Intent*` feed to any other caller), the recovery substate is the label
/// that drain and the reconcile loop carry, and the reconcile tuning with the
/// counter beside it is the role's own configuration — so all six come from the
/// stored row this client already holds, and never from the beat.
/// That row is not the reducer's last state: a beat the reducer ignored is still
/// liveness, so `bump_session_version` raises the row's `(generation, seq)`
/// watermark with no reduce behind it, and the columns can stand ahead of the
/// observation written beside them. Nothing here reads the version — the copy is
/// the six dimensions alone. What the body claims for them is discarded, never
/// read: a plugin that says `delivery: none` because it cannot see the drain
/// must not be able to clear an intent the server has not receipted.
///
/// The repairs after the copy are what makes the result legal by construction,
/// and they are independent: one reads the drain, the other the label beside it,
/// and a beat can break both pairings at once. The reducer replaces the agent
/// wholesale and `is_legal` is the tuple's own cross-constraint (§2.2), so a
/// client drain carried onto a dimension the plugin just moved can pair two
/// facts no transition would ever have written together. Each arm below names a
/// pairing the reducer's own rules forbid and downgrades the *client* dimension
/// to the state of that drain the plugin's claim proves stale — never to a
/// value invented for the moment. The consequence at this door is that a beat is
/// composed into legality rather than refused by it; `RejectReason::IllegalObservation`
/// is still the reducer's answer to every other feed, the reconcile loop's own
/// heartbeats included.
fn compose_observation(client: &Observation, mut body: Observation) -> Observation {
    body.delivery = client.delivery;
    body.recovery = client.recovery;
    // The reconcile tuning and its counter are the client's as well, and for the
    // same reason: `isolate_after` and `terminate_after` are the role's own
    // thresholds and `mismatch_count` is what this client's loop increments each
    // time a fact disagrees. A plugin witnesses none of them — the shape it sends
    // carries the defaults and a constant `generation_live` — so a beat that
    // carried them would reset a running isolation ladder on every heartbeat.
    body.generation_live = client.generation_live;
    body.isolate_after = client.isolate_after;
    body.terminate_after = client.terminate_after;
    body.mismatch_count = client.mismatch_count;
    // `Exhausted` means retries burned against an open turn exit, and `Ready`
    // says no turn exit is open. A plugin that moves a session with an exhausted
    // intent back to `ready` has begun a new assignment: the old drain was
    // consumed by whatever settled it, or by the fault path that took the work,
    // and only the exit it hung on is gone.
    if body.agent == AgentState::Ready && body.delivery == DeliveryState::Exhausted {
        body.delivery = DeliveryState::None;
    }
    // `Accepted` is a post-turn fact and `Booting` is the word a plugin uses for
    // a process that is not there any more (see `observationFor`'s `gone`
    // mapping). The receipt the client wrote cannot belong to a turn that
    // stopped existing, so the drain stays where the plugin left it: open,
    // unacknowledged.
    if body.agent == AgentState::Booting && body.delivery == DeliveryState::Accepted {
        body.delivery = DeliveryState::Pending;
    }
    // A recovery substate describes an idle or draining agent. `Booting` and
    // `Ready` are neither, and `Gone` keeps no recovery line at all — that is the
    // reducer's own coupling for those transitions (`reduce.rs::transition`),
    // applied here because a heartbeat body bypasses it.
    if body.recovery != RecoveryState::None
        && matches!(
            body.agent,
            AgentState::Booting | AgentState::Ready | AgentState::Gone
        )
    {
        body.recovery = RecoveryState::None;
    }
    body
}

/// Task a state-carrying report names, for the projection publish.
fn task_id_of(report: &Report) -> &str {
    match report {
        Report::Ready { task_id, .. } | Report::Heartbeat { task_id, .. } => task_id,
        Report::Complete { task_id, .. } => task_id,
        Report::Fault { .. } => "",
    }
}

#[cfg(test)]
mod tests;
