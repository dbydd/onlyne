use super::config::{OUTCOME_POLL_MS, RunState};
use super::run::settle_control;
use crate::backend::SessionOutcome;
use crate::session::accept::AcceptPath;
use crate::session::dispatch::{self, ClientLink};
use anyhow::{Result, anyhow};
use onlyne_proto::{AckArgs, ClientOp, Delivery, QueryRolesArgs, RoleInfo};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tokio::time::sleep;

/// Drain terminal facts emitted by a backend that owns its agent.
///
/// The backend queue is synchronous and destructive. Each fact is moved out
/// before this task awaits the ordinary settlement path, so neither its queue
/// lock nor the dispatch lock can survive into session teardown.
pub(super) async fn outcome_loop(state: RunState) -> Result<()> {
    loop {
        // The feed belongs to the backend the role's drive installed, and that
        // backend moves with the drive: at startup this client holds the
        // default drive's backend, and the role's own drive arrives later with
        // `welcome`. So the feed is read every round instead of being latched
        // once. A backend that reports no endings of its own — a pane host, a
        // child process, the in-process test runtime — answers `None`, which is
        // a round with nothing to drain and not the end of this task: waiting
        // on the first answer forever is how a session whose drive turns out to
        // be `acp` would run a whole turn with nobody listening for its ending.
        let Some(feed) = state.dispatch.outcome_feed() else {
            sleep(Duration::from_millis(OUTCOME_POLL_MS)).await;
            continue;
        };
        while let Some(outcome) = feed.try_recv() {
            settle_session_outcome(&state, outcome).await?;
        }
        sleep(Duration::from_millis(OUTCOME_POLL_MS)).await;
    }
}

/// Feed one self-driven ending through the same fault and settlement paths an
/// adapter report uses.
///
/// A backend that owns its agent reports two shapes of ending. A verdict — how
/// the work ended — settles here, through the same door a plugin's completion
/// takes. A turn that simply ended settles nothing: an agent that stopped its
/// turn without calling `onlyne_complete` is the case §3c's client-owned rule
/// answers, and `None` is exactly that shape.
pub(super) async fn settle_session_outcome(
    state: &RunState,
    outcome: SessionOutcome,
) -> Result<()> {
    let SessionOutcome {
        task_id,
        outcome,
        head,
        note,
        refusals,
    } = outcome;
    if let Some(reason) = refusals.as_deref() {
        crate::reconcile::record_fault(&state.store, &task_id, "permission", "acp", reason)?;
    }
    // The turn ended and its drive asked for nothing: §3c's rule owns the rest
    // — the one nudge, and the settlement when a second turn ends the same way.
    // The agent's own closing line rides along, because the settlement this call
    // may reach writes it into the task's head.
    let Some(outcome) = outcome else {
        return dispatch::on_turn_end(&state.dispatch, &task_id, head).await;
    };
    // A self-driven backend reports in the task's own vocabulary, and the
    // settlement travels in the wire's. `pending` is the absence of a verdict,
    // which is nothing this loop can settle: a terminal outcome is what an
    // ending reports.
    let terminal = dispatch::task_outcome_of(outcome).ok_or_else(|| {
        anyhow!("self-driven backend reported a non-terminal outcome for task {task_id}")
    })?;
    if outcome == onlyne_proto::TaskState::Failed
        && let Some(reason) = note.as_deref()
    {
        crate::reconcile::record_fault(&state.store, &task_id, "acp", "acp", reason)?;
    }
    dispatch::on_out(
        &state.dispatch,
        &task_id,
        terminal,
        head,
        // The ending came from this client's own backend, which watched the agent
        // it is reporting: the never-ran guard belongs to the plugin's door, where
        // the claimant and the claim are the same party.
        dispatch::SettleAuthority::ClientOwned,
    )
    .await
}

/// One delivery becomes a session, or an immediate refusal ack.
///
/// A plugin mounted before any work existed is parked in the dispatcher, so the
/// session staged here hands straight over to it. That is the order an
/// always-running agent takes: it attaches first and receives its assignment
/// when a task arrives (plan §6 line 285).
///
/// Only a `Task` costs a session slot, so only a `Task` waits at the capacity
/// gate. A `Completion` is a terminal receipt and a `Note` wakes a session that
/// already exists; neither opens a slot, and refusing one because the role is
/// full would hold back work that is already done (the verdict of a task) or
/// traffic that has no session to create.
pub(super) async fn accept_delivery(state: &RunState, delivery: &Delivery) {
    // A control command acts on the work the role already holds, so it answers
    // before the capacity gate and before the `accept_new` gate: a role at
    // `max_sessions` is exactly the role whose operator wants to free.
    if delivery.envelope.kind == onlyne_proto::MsgKind::Control {
        settle_control(state, delivery).await;
        return;
    }
    // A task this role already finished is not new work. The server re-offers an
    // unacknowledged row after a link flap or an operator repair, and a
    // completion that was in flight when the link dropped can land after the
    // requeue, so this row's task may already be `Done` here. Dispatching it
    // again would stage its payload on whichever session is idle — one chain's
    // task running inside another conversation, with a second answer aimed at
    // the ledger row the first one settled. Acknowledge the row and run nothing.
    if delivery.envelope.kind == onlyne_proto::MsgKind::Task
        && let Some(task_id) = delivery.envelope.task_id()
        && state.dispatch.task_completed_here(task_id)
    {
        tracing::warn!(
            msg_id = %delivery.msg_id,
            task = %task_id,
            "redelivery of a finished task settled without running it"
        );
        state.dispatch.push_settled(AckArgs {
            msg_id: delivery.msg_id.clone(),
            op_id: None,
            accepted: true,
            reason: Some("task already completed by this role".to_string()),
        });
        return;
    }
    // A `Completion` is a terminal receipt, so it settles the row it names and
    // starts no session (plan §3 line 152's `Completion`).
    if delivery.envelope.kind == onlyne_proto::MsgKind::Completion {
        state.dispatch.push_settled(AckArgs {
            msg_id: delivery.msg_id.clone(),
            op_id: None,
            accepted: true,
            reason: None,
        });
        return;
    }
    // A `Note` names no task, so it starts no session: it is the wake-up a role
    // sends to a running agent (§3), and an agent that does not exist yet has
    // nothing to wake. §5's `note_queue` keeps one out of the queue when its
    // role is offline, and this is the matching half on the receiving side.
    if delivery.envelope.kind == onlyne_proto::MsgKind::Note {
        let injected = state.dispatch.inject_note(&delivery.envelope).await;
        state.dispatch.push_settled(AckArgs {
            msg_id: delivery.msg_id.clone(),
            op_id: None,
            accepted: injected,
            reason: (!injected).then(|| "note has no live session to wake".to_string()),
        });
        return;
    }
    // What is left creates a session, so this is where `max_sessions` bites.
    if !state.dispatch.has_capacity() {
        // The row stays in flight on the server, which offers it again when a
        // session frees (plan §5 `max_sessions`).
        tracing::debug!(msg_id = %delivery.msg_id, "delivery waits for a free session");
        return;
    }
    let accept_new = state.accept_new.load(Ordering::SeqCst);
    let path = AcceptPath::new(state.dispatch.clone(), state.dispatch.role_prose());
    match path.accept_new(delivery, accept_new) {
        Ok(Some(session)) => {
            if let Some(task_id) = delivery.envelope.task_id() {
                state.dispatch.attach_msg_id(task_id, &delivery.msg_id);
            }
            // A plugin attached to this session takes the payload now, or the
            // one parked for the role does; a session whose own plugin is
            // still starting waits for its mount to hand it over.
            if let Err(error) = state.dispatch.hand_staged(&session.task_id).await {
                tracing::warn!(error = %error, task = %session.task_id, "staged hand-off refused");
            }
        }
        // The gate is the connection's own (`watch_readiness` shuts it when the
        // link leaves `Ready` and opens it when the redial lands), so a delivery
        // the pull already had in hand when the link flapped arrives here with the
        // gate shut. That answer is not this client's to give: a refusal settles
        // the row `rejected`, which is terminal, and the row the teardown's
        // requeue would have brought back is destroyed instead. The row stays in
        // flight — unanswered is not a decision — and the next `hello` that does
        // not claim it is what puts it back on the queue.
        //
        // The same answer covers the scope's own wait: a delivery whose family's
        // session is mid-delivery, or one that arrives with every slot spent,
        // belongs to a session this role will free. It waits the same way.
        Ok(None) => tracing::debug!(
            msg_id = %delivery.msg_id,
            "no session takes the delivery yet; it stays in flight"
        ),
        Err(error) => {
            tracing::warn!(error = %error, msg_id = %delivery.msg_id, "delivery refused");
            state.dispatch.push_settled(AckArgs {
                msg_id: delivery.msg_id.clone(),
                op_id: None,
                accepted: false,
                reason: Some(error.to_string()),
            });
        }
    }
}

/// Report running sessions whose Applied clock has exceeded the stall
/// threshold. The fault is observation-only; the ledger row stays as stored.
pub(super) async fn scan_stalls(state: &RunState) {
    if state.stall_report_secs == 0 {
        return;
    }
    let due = state
        .dispatch
        .stall_due(Instant::now(), state.stall_report_secs);
    for task_id in due {
        let Some(report) = state.dispatch.stall_report(&task_id) else {
            continue;
        };
        match dispatch::send_frame(&state.dispatch, ClientOp::Report(report)).await {
            Ok(()) => state.dispatch.mark_stalled(&task_id),
            Err(error) => {
                tracing::warn!(error = %error, task = %task_id, "stall fault was not sent")
            }
        }
    }
}

/// Reclaim the resources of sessions this client has already put past their work,
/// and publish each one's exit.
///
/// The reclaim runs on every readiness tick, ahead of the reconnect window, so a
/// completed session whose plugin left without a goodbye is this sweep's to end:
/// its resource is still open, its stored lifecycle already reads `Exited`, and
/// the sweep below waits out a grace the completion itself did not ask for. What
/// the reclaim writes is the agent's exit and the resource close, and the server
/// mirrors only what this client reports — so each session it retired travels the
/// report an ordinary ending travels, once the lock has been given back and the
/// stored row is final. Without it the mirror keeps the reading the settle
/// published, `exited` beside an agent still `running` and a resource still
/// `attached`, which is what a peer's census of completed sessions found.
///
/// A published exit runs the server's `release_exited_delivery` for that task, and
/// the task's own delivery row was answered when the completion settled it, so
/// there is no in-flight row of that task for the release to hand back. The
/// publish is also after the reclaim rather than around it because the row the
/// server mirrors is the row the reclaim writes.
pub(super) async fn scan_reclaimed_resources(state: &RunState) {
    for session_id in state.dispatch.reclaim_exited_resources() {
        if let Err(error) = dispatch::sync_session(&state.dispatch, &session_id).await {
            tracing::warn!(
                session = %session_id,
                error = %error,
                "a reclaimed session's exit was not published"
            );
        }
    }
}

/// Retire the sessions whose plugin connection dropped and did not come back
/// within `[client] reconnect_grace_secs`, and publish each one's exit. The tick
/// sweeps every session the window expired on, bound to a task or not: a plugin
/// that never came back is an agent that is gone, whether or not its work was
/// still owed.
///
/// The publish is the half of the ending the sweep cannot write: the retirement
/// feeds the session's own tuple to `Exited` and files the verdict, and the server
/// only learns either from what this client reports. Without it the mirrored row
/// keeps reading `working` until the server's own observer records a
/// `stale_working` or `heartbeat_missing` fault — a reader waits for a fault, which
/// names the silence and moves no row, to hear what this client already knew. So
/// each session that left travels the report an ordinary ending already travels,
/// once the lock has been given back and the stored row is final. Only the durable
/// queue refusing the frame reaches the log: a live send that gave up is
/// `sync_session`'s own fallback to that queue, not a lost publish.
pub(super) async fn scan_reconnect_grace(state: &RunState) {
    if state.reconnect_grace_secs == 0 {
        return;
    }
    let retired = state
        .dispatch
        .retire_dropped_ghosts(Instant::now(), state.reconnect_grace_secs);
    if retired.is_empty() {
        return;
    }
    // One line per retirement, and it names the arm: two readings close this window — the
    // connection ended and stayed away, or the connection held while nothing the client
    // accepted arrived — and a single count with one threshold made an operator reading the
    // log guess. The ages are the sweep's own inputs, so the line settles whether the agent
    // left or merely stopped reporting.
    for retired in retired {
        tracing::info!(
            session = %retired.session_id,
            arm = retired.arm.word(),
            quiet_secs = retired.quiet_secs,
            away_secs = retired.away_secs,
            silence_window_secs =
                dispatch::HEARTBEAT_INTERVAL.as_secs() * dispatch::HEARTBEAT_SILENCE_MARGIN as u64,
            grace_secs = state.reconnect_grace_secs,
            "session retired past its window"
        );
        if let Err(error) = dispatch::sync_session(&state.dispatch, &retired.session_id).await {
            tracing::warn!(
                session = %retired.session_id,
                error = %error,
                "a retired session's exit was not published"
            );
        }
    }
}

/// Settle the work an operator's word left open unanswered, and publish each
/// one's exit.
///
/// `recycle` and `cancel` ask a session's plugin for its own ending, and the
/// completion that answers the command is a frame of the plugin's. A plugin that
/// never sends one — it left with the command's frame, or implements no
/// `recycle` at all — leaves the task open, the mirrored row reading `working`,
/// and the delivery row this client was handed in flight, and nothing in this
/// process is left to answer any of the three: the close the command ran is what
/// ended the session's own row already, so the sweep above finds no window left
/// open on it and no work of it to settle.
///
/// What answers the word is the client's own record of it, held past
/// `dispatch::CONTROL_SETTLE_BOUND`. The note is read under the dispatch lock and
/// spent one at a time behind it, so a completion that arrives in between
/// settles the task through the report it came on and this sweep writes nothing;
/// the verdict, the refusal of the delivery row and the report behind it are
/// [`DispatchState::settle_unanswered_control`]'s. The publish is this sweep's,
/// for the reason the retirement above publishes: the server mirrors what this
/// client reports, and that is what moves the row an operator is reading.
///
/// [`DispatchState::settle_unanswered_control`]:
///     crate::session::dispatch::DispatchState::settle_unanswered_control
pub(super) async fn scan_control_settles(state: &RunState) {
    let now = Instant::now();
    for note in state.dispatch.control_settles_due(now) {
        // The note is spent through the one door that spends it: a completion
        // that answered this word between the reading above and this call takes
        // the note first, and its verdict is the one that stands.
        if !state.dispatch.settle_unanswered_control(&note) {
            continue;
        }
        tracing::info!(
            task = %note.task_id,
            outcome = ?note.word.outcome(),
            waited_secs = now.saturating_duration_since(note.noted_at).as_secs(),
            "a task was settled on an operator's word no plugin answered"
        );
        if let Err(error) = dispatch::sync_session(&state.dispatch, &note.task_id).await {
            tracing::warn!(
                task = %note.task_id,
                error = %error,
                "a settled task's exit was not published"
            );
        }
    }
}

/// Release the process of every idle session whose scope bound has expired, and
/// publish each one's row.
///
/// The suspension is [`DispatchState::suspend_idle_sessions`]'s: the session is
/// marked suspended, its row moves through the `Suspend` event, and one backend
/// close is collected per session. What this sweep adds is the report — a
/// released process sends no frame of its own, so the row it just left would sit
/// on the mirror as the last thing the living session published. A session whose
/// runtime cannot resume is left exactly where it is, which is the "process
/// alive, session alive" degradation the scope table names: this client will not
/// release a process it cannot bring back.
pub(super) async fn scan_idle_sessions(state: &RunState) {
    for session_id in state.dispatch.suspend_idle_sessions(Instant::now()) {
        if let Err(error) = dispatch::sync_session(&state.dispatch, &session_id).await {
            tracing::warn!(
                session = %session_id,
                error = %error,
                "a suspended session's row was not published"
            );
        }
    }
}

pub(super) async fn refresh_role_slice(link: &ClientLink, state: &RunState) -> Result<()> {
    let role = state.dispatch.role();
    let reply = link
        .request(ClientOp::QueryRoles(QueryRolesArgs { role: Some(role) }))
        .await?;
    if !reply.ok {
        tracing::warn!(error = ?reply.error, "role slice refresh query refused");
        return Ok(());
    }
    let rows: Vec<RoleInfo> = reply
        .data
        .as_ref()
        .and_then(|value| value.get("roles"))
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();
    if let Some(info) = rows.first() {
        apply_role_info(state, info);
    }
    Ok(())
}

pub(super) fn apply_role_info(state: &RunState, info: &RoleInfo) -> Vec<&'static str> {
    let current = state.dispatch.role_slice();
    let next = crate::session::slice::RoleSlice::from_role_info(info, &current);
    let Some((applied, fields)) = crate::session::slice::apply_if_changed(&current, next) else {
        return Vec::new();
    };
    // A drive can move under a live link, and the backend it selects has to move
    // with it before the new command is handed to the old one. A drive this
    // machine cannot host is recorded as a refusal rather than silently kept.
    state.install_runtime(applied.drive);
    state.dispatch.reconfigure(applied);
    fields
}

/// The local accept path for the current role slice.
pub fn accept_path(state: &RunState) -> Result<AcceptPath> {
    Ok(AcceptPath::new(
        state.dispatch.clone(),
        state.dispatch.role_prose(),
    ))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod scan_tests;
