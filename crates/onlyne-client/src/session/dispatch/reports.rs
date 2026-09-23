use super::*;

use super::projection::{note_verdict, sync_session};
use super::retire::on_recycled;
use super::settle::{SettleAuthority, on_out};
use super::state::{ControlWord, note_beat};
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
            // The command asks this session's plugin for its own ending, so the
            // completion that answers it travels the plugin's door carrying the
            // client's authority. The note goes on before the frame leaves: the
            // report and the retirement below race over the row, and the note is
            // the half of that race this client decides. A recycle prescribes the
            // plugin no outcome, so the verdict the note stands for is the one a
            // session that died holding a task leaves: `failed`.
            state.owe_controlled_settle(task_id, ControlWord::Recycle, Instant::now());
            state.recycle_plugin(task_id, reason, None).await;
            on_recycled(state, task_id, onlyne_session::CloseReason::Operator)?;
        }
        ControlOp::Cancel { reason, .. } => {
            // The same note for the same reason: a cancel ends the task on the
            // operator's word, and the plugin's `cancelled` report answers it.
            state.owe_controlled_settle(task_id, ControlWord::Cancel, Instant::now());
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
                // Its liveness half does travel, and that half is what the
                // agent's life depends on: the stamp the silence arm reads is
                // about the socket behind it, and both copies of a plugin loaded
                // into one agent process — the shape a workspace that installs
                // the plugin twice produces — beat on their own connections.
                // Letting a refused frame leave the stamp alone starves the
                // session's clock, and the sweep then retires an agent that is
                // alive and working. The stamp is read only for a session whose
                // task is still bound and unsettled, so the demotion keeps its
                // own retirement.
                note_beat(&mut inner, &task_id, Instant::now());
                tracing::warn!(
                    task = %task_id,
                    "a beat from a connection held read-only refreshes the liveness stamp and applies no state"
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
                // The task this beat speaks for, as this client holds it. No
                // record means nothing was ever opened here for that id, which is
                // not the same fact as an open task: the composition labels open
                // work, and there is none to label without a record.
                let task_state = inner
                    .store
                    .task(&task_id)
                    .ok()
                    .flatten()
                    .map(|record| record.task_state);
                let verdict = match serde_json::from_value::<Observation>(observed) {
                    Ok(body) => apply_persist(
                        &inner.bridge,
                        &inner.store,
                        &task_id,
                        &LifecycleEvent::Heartbeat {
                            v: Version::new(generation, seq),
                            body: compose_observation(&stored, body, task_state),
                        },
                    )?,
                    Err(error) => {
                        tracing::warn!(task = %task_id, error = %error, "heartbeat carries no readable observation; liveness only");
                        // The log line's own promise: a frame whose observation no
                        // client can read still proves the agent is alive, so the
                        // stamp goes on and the tuple stays where it stood.
                        note_beat(&mut inner, &task_id, Instant::now());
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
                    Verdict::Ignored(IgnoredReason::NoOp) => {
                        // A beat that moves no dimension is still the agent saying it is
                        // alive, and this is where the ordinary long turn lands: a model
                        // streaming for minutes reports `agent: running` over and over, the
                        // tuple never changes, and every one of those beats is a no-op to the
                        // reducer. Leaving the stamp alone for them starves the clock the
                        // silence arm reads, and the sweep then closes the pane under an agent
                        // that is working — the shape a live role died of at thirty seconds
                        // into a turn. The version still advances through the bump below, and
                        // the tuple stays exactly as the last accepted write left it.
                        note_beat(&mut inner, &task_id, Instant::now());
                        inner
                            .store
                            .bump_session_version(&task_id, generation, seq)?
                    }
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
            //
            // The one completion this client has asked for by name arrives on the
            // same frame, so the note is what tells the two apart. Everything else
            // that leaves this arm is the plugin's own claim about work it did, and
            // `on_out` reads the session's row for that claim, refused whole when
            // the row says no turn ran.
            let asked = if state.take_controlled_settle(&task_id) {
                SettleAuthority::ControlDriven
            } else {
                SettleAuthority::PluginReport
            };
            on_out(state, &task_id, outcome, head, None, &[], asked).await?;
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
/// `task_state` is the task record's own verdict, read from the store beside the
/// row (`None` when this client holds no record for the task). It is here
/// because `idle_waiting` is a fact about the work, not about the tuple: the
/// label says a turn ended while an open task had no completion exit.
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
fn compose_observation(
    client: &Observation,
    mut body: Observation,
    task_state: Option<TaskState>,
) -> Observation {
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
    // A recovery substate rides only the agents `is_legal` allows it on, which is
    // the reducer's own coupling for those transitions (`reduce.rs::transition`),
    // applied here because a heartbeat body bypasses it: `Idle` for the two
    // waiting labels, `Idle` or `Running` for `Draining` — a completion in
    // asynchronous send outlives the turn that reported it — and none of them on
    // `Booting`, `Ready` or `Gone`, whose own definitions say no turn is open to
    // be waiting on or draining from. The `Running` arm is not decoration: the
    // beat of a turn that started after a reminder is exactly that pair, and
    // dropping the label is what keeps the reducer from refusing the beat whole —
    // the frame that says the session went back to work would be the one lost.
    let held = match body.recovery {
        RecoveryState::IdleWaiting | RecoveryState::IdleFault => body.agent == AgentState::Idle,
        RecoveryState::Draining => matches!(body.agent, AgentState::Idle | AgentState::Running),
        RecoveryState::None => true,
    };
    if !held {
        body.recovery = RecoveryState::None;
    }
    // An idle agent whose task is still open and whose exit has no receipt is
    // the design's `idle_waiting` (§2.2, §4.2): the turn ended without a
    // completion exit, and the plugin's answer is to send the assignment again.
    // No other writer reaches the label on a live session — the reducer's own
    // turn-end rule (`crates/onlyne-session/src/lifecycle/reduce.rs`,
    // `transition`) needs a `TurnEnded` event that no plugin-backed session
    // feeds, and a beat carries the agent state as its only news — so the
    // composition is where that rule has to land or `idle_waiting` never
    // appears at all. A stronger label the client already holds is left alone:
    // `draining` says the exit is in asynchronous send and `idle_fault` says a
    // fact disagreed, while `idle_waiting` means there is no exit yet.
    if body.agent == AgentState::Idle
        && task_state == Some(TaskState::Pending)
        && body.delivery != DeliveryState::Accepted
        && body.recovery == RecoveryState::None
    {
        body.recovery = RecoveryState::IdleWaiting;
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
