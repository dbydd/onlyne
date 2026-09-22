use super::*;

use super::outbound::store_ack;
use super::projection::stored_task_state;
use super::state::{
    DispatchInner, DispatchState, has_attached_transport, session_exited, slot_key_serving_task,
};
use super::transport::{held_read_only, names_session};

/// Retire one session. The stored tuple decides whether a live resource
/// remains to close, and the caller's reason reaches the backend unchanged, so an
/// operator cancel stops reporting itself as a completion.
pub fn on_recycled(
    state: &DispatchState,
    task_id: &str,
    reason: onlyne_session::CloseReason,
) -> Result<()> {
    let mut inner = state.inner.lock();
    release_locked(&mut inner, task_id, Some(reason))
}

/// Why the task of one session ended, as the retirement reason the task table
/// records.
///
/// The task's own row is the only place a verdict lives: the session tuple says
/// nothing about how its work ended, and an open task — `pending`, or no row at
/// all, which is the same reading — has no reason to retire anything.
pub(super) fn stored_close_reason(
    inner: &DispatchInner,
    task_id: &str,
) -> Option<onlyne_session::CloseReason> {
    match stored_task_state(inner, task_id) {
        TaskState::Pending => None,
        TaskState::Done => Some(onlyne_session::CloseReason::Completed),
        TaskState::Failed => Some(onlyne_session::CloseReason::Fault),
        TaskState::Cancelled => Some(onlyne_session::CloseReason::Cancelled),
    }
}

/// The reason one session the reconnect grace retires is closed with, read from
/// what this client holds rather than from a settle that never came.
///
/// The id is the session's own (`slot.session.task_id`), never the task binding
/// beside it: the sweep is what feeds that session's row and closes the resource
/// its agent was holding, and the two ids part company exactly where a slot's
/// binding is not the session it was born for. A session still owing work closes
/// as that task's own record reads: a `done` task is a `Completed`, a
/// `cancelled` one is a `Cancelled`, and a `failed` task — like one that never
/// settled at all — is a `Fault`, because the work was still owed when the agent
/// left.
fn grace_close_reason(inner: &DispatchInner, task_id: &str) -> onlyne_session::CloseReason {
    match stored_task_state(inner, task_id) {
        TaskState::Done => onlyne_session::CloseReason::Completed,
        TaskState::Pending | TaskState::Failed => onlyne_session::CloseReason::Fault,
        TaskState::Cancelled => onlyne_session::CloseReason::Cancelled,
    }
}

/// Whether one slot is due for the reconnect grace to take it.
///
/// The window as it always was: the connection that would have sent this
/// session's next heartbeat has ended, and the window runs from the moment it
/// left. A slot a live connection serves is not this arm's to end, because an
/// attached transport is the one thing that says the agent is still reachable.
fn dropped_past_window(
    inner: &DispatchInner,
    key: &str,
    slot: &SessionSlot,
    now: Instant,
    window: Duration,
) -> bool {
    slot.dropped_at.is_some_and(|dropped| {
        !has_attached_transport(inner, key, slot)
            && now
                .checked_duration_since(dropped)
                .is_some_and(|away| away >= window)
    })
}

/// Whether one session's own agent has gone quiet on a socket that is still up.
///
/// The window's other door, and it exists precisely because an attached
/// transport — the one thing the arm above trusts — can lie. A socket that is
/// still up proves the connection survived; it says nothing about the agent
/// behind it, and a plugin whose event loop is blocked holds its socket and
/// stops beating. Nothing the client reads before this could see that: no socket
/// ends, so no drop clock ever starts, and the session keeps its slot, its
/// projected row and its host resource for as long as the client runs.
///
/// So the reading is taken off the frames themselves. A session whose task is
/// still bound and unsettled and whose last accepted frame is older than the
/// protocol's heartbeat interval by [`HEARTBEAT_SILENCE_MARGIN`] has no agent
/// behind its socket.
///
/// Why no task bound is excluded: the plugin stops its heartbeat loop with the
/// last task it was given, so a task-free session that has gone quiet is an
/// agent waiting for work by design — the ordinary shape between deliveries.
/// Sweeping it would retire the very connection the next payload is staged onto.
/// The unsettled check beside the binding says the same thing about work that
/// already landed: a settled task has nothing left for its session to answer,
/// and `release_locked` has already given the binding back.
///
/// Why the drop clock's arm never reads this stamp: the two are mutually
/// exclusive by construction. A re-mount clears `dropped_at`, so a session that
/// came back is judged by its beat alone, and a session whose connection went
/// away is judged by the clock alone and never by a stamp its agent can no
/// longer refresh.
fn silent_past_window(inner: &DispatchInner, key: &str, slot: &SessionSlot, now: Instant) -> bool {
    if slot.dropped_at.is_some() || !has_attached_transport(inner, key, slot) {
        return false;
    }
    let Some(task_id) = slot.task_id.as_deref() else {
        return false;
    };
    if stored_task_state(inner, task_id) != TaskState::Pending {
        return false;
    }
    let quiet = HEARTBEAT_INTERVAL * HEARTBEAT_SILENCE_MARGIN;
    slot.last_beat.is_some_and(|beat| {
        now.checked_duration_since(beat)
            .is_some_and(|away| away >= quiet)
    })
}

/// Stop holding a session's connection as its transport.
///
/// The act a socket ending performs, run here on the client's own verdict
/// instead: a session judged dead by its silence still holds its socket, and the
/// connection is not going to end on its own while the agent behind it is
/// blocked. The binding goes now, so the retirement below runs as the ordinary
/// one, and a frame from that connection afterwards is refused the way every
/// frame from a connection no session answers for is — it is served no state,
/// which is the same door a stale reporter already comes to.
fn unbind_transports(inner: &mut DispatchInner, key: &str, slot: &SessionSlot) {
    inner
        .transports
        .retain(|served, _| !names_session(key, slot, served));
}

/// Retire one task-free session after its transport set becomes empty.
///
/// The idle slot releases its backend resource because the agent able to run
/// another task in it has left. An attached transport keeps the resource because
/// that agent remains reachable. The dispatch lock serializes the final transport check, reference
/// refresh, lifecycle projection, backend close, and slot removal with adapter
/// binding.
pub(super) fn retire_idle_locked(
    inner: &mut DispatchInner,
    key: &str,
    reason: onlyne_session::CloseReason,
) -> bool {
    let Some(slot) = inner.sessions.get(key) else {
        return false;
    };
    if slot.task_id.is_some() || has_attached_transport(inner, key, slot) {
        return false;
    }

    let original = slot.session.clone();
    let task_id = original.task_id.clone();
    let resource = inner
        .store
        .get_session(&task_id)
        .ok()
        .flatten()
        .map(|row| row.resource_state)
        .unwrap_or_else(|| "detached".to_string());
    if resource != "detached" && resource != "closed" {
        let session = match inner.backend.attach(&original) {
            Ok(refreshed) => {
                if refreshed != original {
                    inner.bridge.track_live(refreshed.clone());
                    if let Some(slot) = inner.sessions.get_mut(key) {
                        slot.session = refreshed.clone();
                    }
                }
                refreshed
            }
            Err(_) => original,
        };
        tracing::info!(
            task = %task_id,
            backend = %session.backend,
            resource = %session.backend_ref,
            ?reason,
            "retiring idle session resource"
        );
        if let Err(error) = feed_resource_closed(&inner.bridge, &inner.store, &task_id) {
            tracing::warn!(
                task = %task_id,
                backend = %session.backend,
                resource = %session.backend_ref,
                error = %error,
                "session resource close projection failed"
            );
        }
        if let Err(error) = inner.backend.close(&session, reason, false) {
            tracing::warn!(
                task = %task_id,
                backend = %session.backend,
                resource = %session.backend_ref,
                error = %error,
                "session resource retirement failed"
            );
        }
    }
    inner.bridge.untrack_live(&task_id);
    inner.sessions.remove(key);
    true
}

/// Give one session's task slot back. Settled tasks enter idle retirement, and
/// explicit reasons drive the control-close path.
pub(super) fn release_locked(
    inner: &mut DispatchInner,
    task_id: &str,
    reason: Option<onlyne_session::CloseReason>,
) -> Result<()> {
    let resource = inner
        .store
        .get_session(task_id)?
        .map(|row| row.resource_state)
        .unwrap_or_else(|| "detached".to_string());
    if let Some((key, slot)) = slot_key_serving_task(inner, task_id)
        .and_then(|key| inner.sessions.get(&key).map(|slot| (key, slot.clone())))
    {
        if let Some(reason) = reason {
            if resource != "detached" && resource != "closed" {
                feed_resource_closed(&inner.bridge, &inner.store, task_id)?;
                inner.backend.close(&slot.session, reason, false)?;
            }
            inner.bridge.untrack_live(task_id);
            inner.sessions.remove(&key);
        } else {
            if let Some(session) = inner.sessions.get_mut(&key) {
                session.task_id = None;
                session.ready = false;
            }
            retire_idle_locked(inner, &key, onlyne_session::CloseReason::Completed);
        }
    }
    inner.stall.forget(task_id);
    if reason.is_some() && resource == "detached" {
        inner
            .store
            .note_alert(format!("session recycled {task_id}"));
    }
    Ok(())
}

/// Close every live session's resource with `reason` and forget the slots.
///
/// This is the shutdown path: a stopped client must not leave resources behind
/// that only it can address, and each backend's own record of the resource —
/// the Orca tab map included — ends with the session. `budget` bounds the whole
/// sweep, because an operator's SIGTERM must not turn into a hang while a slow
/// backend CLI exits; whatever the budget cuts off is reported and dropped
/// anyway.
pub fn close_all(state: &DispatchState, reason: onlyne_session::CloseReason, budget: Duration) {
    let started = Instant::now();
    let mut inner = state.inner.lock();
    let sessions: Vec<(String, SessionRef)> = inner
        .sessions
        .iter()
        .map(|(key, slot)| (key.clone(), slot.session.clone()))
        .collect();
    for (key, session) in sessions {
        if started.elapsed() > budget {
            tracing::warn!(
                task = %session.task_id,
                "shutdown close budget reached; the resource is left behind"
            );
        } else if let Err(error) = inner.backend.close(&session, reason, false) {
            tracing::warn!(
                task = %session.task_id,
                error = %error,
                "session close failed during shutdown"
            );
        }
        inner.bridge.untrack_live(&session.task_id);
        inner.sessions.remove(&key);
    }
}

impl DispatchState {
    /// Retire tracked resources whose stored lifecycle has reached `Exited`.
    ///
    /// The periodic readiness tick calls this after completed work becomes an
    /// idle slot. Task-free sessions with an attached transport stay bound to
    /// their host resource, and task-free sessions whose agent has left release it.
    pub fn reclaim_exited_resources(&self) {
        let mut inner = self.inner.lock();
        let candidates: Vec<(String, onlyne_session::CloseReason)> = inner
            .sessions
            .iter()
            .filter(|(key, slot)| {
                slot.task_id.is_none()
                    && session_exited(&inner, &slot.session.task_id)
                    && !has_attached_transport(&inner, key, slot)
            })
            .filter_map(|(key, slot)| {
                stored_close_reason(&inner, &slot.session.task_id)
                    .map(|reason| (key.clone(), reason))
            })
            .collect();
        for (key, reason) in candidates {
            retire_idle_locked(&mut inner, &key, reason);
        }
    }

    /// Retire the sessions whose plugin connection dropped and never came back,
    /// and answer how many left.
    ///
    /// A connection that ends without a `detach` frame leaves its session tracked
    /// so an agent that restarts inside `[client] reconnect_grace_secs` finds the
    /// resource it was using. That promise has to expire: a process that is
    /// simply gone would otherwise hold a slot, a projected `idle` row, and a live
    /// host resource forever, and on a role with `max_sessions = 1` it stops every
    /// later delivery. The window answers for the agent itself, so a session still
    /// bound to a task goes with it: the plugin connection that would have
    /// reported the ending is the one that dropped. The agent-gone feed is what
    /// says the process left — the session's own tuple reaches `Exited` through
    /// `AgentState::Gone` rather than through a task result — and the reason the
    /// backend is handed is the one `grace_close_reason` reads off what the slot
    /// still owes.
    ///
    /// What the slot owed is settled too: the task a bound session was serving
    /// ends `failed` here, because the agent that would have reported its ending
    /// is the one that left. A task with no verdict stays open for the server to
    /// re-offer and for `open_tasks` to keep reading, and no later caller exists
    /// to write one.
    ///
    /// A slot this client holds read-only is not this sweep's to end, agent gone
    /// or not: the session id it would feed is the task id, so the ghost's death
    /// would take the live session's mirror and its delivery row down with it.
    /// That retirement belongs to `retire_revived`, which runs when the
    /// completion that answers the held connection merges.
    ///
    /// The window has a second way to open, and it is the one a socket cannot
    /// report: a plugin whose event loop is blocked keeps its connection and
    /// stops beating, so no socket ends and no clock this sweep could read
    /// before moved. What such a session leaves behind is a stamp going stale
    /// while its task stays bound and unsettled, and that is the reading this
    /// sweep takes now. It is the same window and the same verdict — one clock,
    /// one retirement, no second threshold beside `[client]
    /// reconnect_grace_secs` and no fault row of the kind `stall_report_secs`
    /// records and leaves behind.
    pub fn retire_dropped_ghosts(&self, now: Instant, grace_secs: u64) -> usize {
        if grace_secs == 0 {
            return 0;
        }
        let window = Duration::from_secs(grace_secs);
        let mut inner = self.inner.lock();
        let due: Vec<String> = inner
            .sessions
            .iter()
            .filter(|(key, slot)| {
                dropped_past_window(&inner, key, slot, now, window)
                    || silent_past_window(&inner, key, slot, now)
            })
            .map(|(key, _)| key.clone())
            .collect();
        let mut retired = 0;
        for key in due {
            let Some(slot) = inner.sessions.get(&key).cloned() else {
                continue;
            };
            // A slot this client holds read-only is not this sweep's to end, for
            // the reason the doc above gives. The demotion alone does not decide
            // it: the held connection that owns the slot can go without the task
            // ever completing, and a slot nothing owns any more is what this
            // window is for.
            if held_read_only(&inner, &key, &slot) {
                continue;
            }
            // A session judged dead on its silence is the one case where the
            // connection is still there: the socket has not ended and will not
            // while the agent behind it is blocked, so the client's own verdict
            // has to take the binding the way the death of the socket would have.
            // The retirement below refuses a slot an attached transport still
            // serves, and that refusal is what this unbinding answers: the
            // verdict has already been reached here, so the binding goes rather
            // than the death of the socket that would normally take it.
            if slot.dropped_at.is_none() {
                unbind_transports(&mut inner, &key, &slot);
            }
            let task_id = slot.session.task_id.clone();
            // Both the feed and the reason name the session's own task, so the id
            // that travels is the one the row and the resource are keyed by.
            let reason = grace_close_reason(&inner, &task_id);
            // The work this session still owed ends here, and this sweep is the
            // only writer left to say so: the plugin connection that would have
            // reported the ending is the one that dropped. A task nobody answers
            // stays `settled_at IS NULL` forever, so `open_tasks` keeps reading
            // it and the server keeps re-offering a delivery no client can take.
            // The binding is what the slot owed — a slot past its window with no
            // task bound owes nothing — and `failed` is the verdict the close
            // reason above already carries for it. A row an earlier verdict
            // settled keeps that one: `settle_task` updates only where
            // `settled_at IS NULL` and answers `false`.
            //
            // The write runs before the agent-gone feed and before the binding
            // hand-back, both of which end this slot's turn through the sweep:
            // the id is captured here, and the verdict is on disk before the
            // only handle on it goes away.
            if let Some(owed) = slot.task_id.clone() {
                if let Err(error) = inner.store.settle_task(&owed, TaskState::Failed) {
                    tracing::warn!(
                        task = %owed,
                        error = %error,
                        "the task of a retired ghost was not settled"
                    );
                }
                // The delivery handle this session was holding is spent as a
                // refusal that names the death, and it is the ledger half of the
                // verdict above. A row left `in_flight` is handed to a pull no
                // longer — `pull` passes by a row whose ticket is armed, and a
                // role-level pull's ticket carries no session id for the release
                // path to match — so nothing would answer for this task until the
                // link dropped, and an operator reading `onlyne ledger` would see
                // a session that has been buried as one still holding its
                // delivery. The reason is the one the residual account already
                // carries (`session::stale::SESSION_DEAD`), and a refusal is
                // terminal: the work comes back through `repair retry`, not by
                // itself.
                let handle = inner
                    .sessions
                    .get_mut(&key)
                    .and_then(|slot| slot.msg_id.take());
                if let Some(msg_id) = handle {
                    store_ack(
                        &inner,
                        AckArgs {
                            msg_id,
                            op_id: None,
                            accepted: false,
                            reason: Some(crate::session::stale::SESSION_DEAD.to_string()),
                        },
                    );
                }
            }
            if let Err(error) = feed_agent_gone(&inner.bridge, &inner.store, &task_id) {
                tracing::warn!(
                    task = %task_id,
                    error = %error,
                    "agent-gone projection failed for a retired ghost"
                );
            }
            // The agent left, so the session owes no task any more: the binding
            // goes back before the idle retirement takes the slot.
            if let Some(slot) = inner.sessions.get_mut(&key) {
                slot.task_id = None;
            }
            if retire_idle_locked(&mut inner, &key, reason) {
                retired += 1;
            }
        }
        retired
    }
}

#[cfg(test)]
mod tests;
