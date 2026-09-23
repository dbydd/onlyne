use super::*;

use super::env::missing_capability;
use super::outbound::queue_outbound_locked;
use super::retire::{retire_idle_locked, stored_close_reason};
use super::state::{
    DispatchInner, DispatchState, FrameGuard, SessionSlot, has_attached_transport,
    rebase_generation, slot_key_named, slot_key_serving_task, slot_task,
};

/// The name one held frame is addressed to.
///
/// A role recipient keeps its own name, which is the role the merged relay is
/// addressed to. Any other recipient keeps the spelling the operator reads in a
/// session listing, and the merged relay addressed to it is refused and recorded
/// rather than quietly dropped: a read-only session cannot answer a conversation
/// it no longer serves.
fn held_recipient(to: &Principal) -> String {
    to.role_name()
        .map(str::to_string)
        .unwrap_or_else(|| to.to_string())
}

/// Whether one session slot is the session an adapter mount named.
///
/// The mount carries the id the client spawned the plugin with
/// (`ONLYNE_SESSION_ID`), which is the slot's key and its stored reference.
/// Each task gets its own session, so those spellings all name one session; the
/// extra checks stay because a slot keeps the id its session was born with even
/// after its task binding changes.
pub(super) fn names_session(key: &str, slot: &SessionSlot, session_id: &str) -> bool {
    key == session_id
        || slot.session.task_id == session_id
        || slot.task_id.as_deref() == Some(session_id)
}

/// Whether a connection other than `io` is the one serving one slot.
fn attached_to_other(inner: &DispatchInner, key: &str, slot: &SessionSlot, io: &AdapterIo) -> bool {
    inner.transports.iter().any(|(session_id, (live, _))| {
        !live.same_connection(io) && names_session(key, slot, session_id)
    })
}

/// Whether `io` is the connection serving one session, as the binding rules left
/// it.
///
/// A connection this client holds read-only is never the answer: a mount that
/// finds its session already served lands in `revived` and never in
/// `transports`, which is the map this reads. Nothing else about the connection
/// is consulted — a frame carries its sender, so a connection serving one
/// session cannot answer for another by naming it.
pub(super) fn serves_session(inner: &DispatchInner, session_id: &str, io: &AdapterIo) -> bool {
    let Some(key) = slot_key_named(inner, session_id) else {
        return false;
    };
    let Some(slot) = inner.sessions.get(&key) else {
        return false;
    };
    inner.transports.iter().any(|(served, (transport, _))| {
        transport.same_connection(io) && names_session(&key, slot, served)
    })
}

/// Whether one connection is held read-only: it mounted a session this client
/// already serves through a different live connection.
pub(super) fn is_revived_connection(inner: &DispatchInner, io: &AdapterIo) -> bool {
    inner
        .revived
        .iter()
        .any(|(_, revived, _)| revived.same_connection(io))
}

/// Whether one slot is held by a connection this client keeps read-only.
///
/// A slot demoted by [`note_binding_locked`] — its task taken by a newer session
/// — is served by no transport at all: the connection that came back for it
/// waits in `revived` under the name it mounted with, which is the name that
/// names this slot. While that connection is there the slot has an owner:
/// `retire_revived` retires it with the completion that answers what the held
/// connection wrote. A demoted slot whose held connection has since gone has no
/// such owner left, so the answer is read off the held names rather than off the
/// demotion alone — and it is read by asking which slots a name names, never by
/// asking which slot a name resolves to, because two slots can answer to one
/// name and only one of them is the held connection's.
pub(super) fn held_read_only(inner: &DispatchInner, key: &str, slot: &SessionSlot) -> bool {
    slot.read_only
        && inner
            .revived
            .iter()
            .any(|(name, _, _)| names_session(key, slot, name))
}

/// Record a returning connection as read-only, once per connection.
pub(super) fn record_revived_connection(
    inner: &mut DispatchInner,
    session_id: &str,
    io: AdapterIo,
    capabilities: Vec<Capability>,
) {
    if !is_revived_connection(inner, &io) {
        inner
            .revived
            .push((session_id.to_string(), io, capabilities));
    }
}

/// Give one session back to the oldest connection that was held read-only for it.
///
/// A held connection is read-only only while another connection serves its
/// session, so the moment that connection goes is the moment the held one becomes
/// the session's only transport. Without this, a plugin that redials while the
/// client still holds the dead socket behind it is silenced for the rest of the
/// session: the assignment it came for is written to a connection nobody reads,
/// and nothing promotes it later. The demotion lifts with the promotion, so the
/// reconnected agent keeps both its session and its delivery rights.
fn promote_held_connection(inner: &mut DispatchInner, key: &str) {
    let attached = inner
        .sessions
        .get(key)
        .is_some_and(|slot| has_attached_transport(inner, key, slot));
    if attached {
        return;
    }
    let held = inner.revived.iter().position(|(name, _, _)| {
        slot_key_named(inner, name).is_some_and(|held_key| held_key == key)
    });
    let Some(index) = held else { return };
    let (name, io, capabilities) = inner.revived.remove(index);
    if let Some(slot) = inner.sessions.get_mut(key) {
        slot.read_only = false;
    }
    tracing::info!(
        session = %name,
        "a held connection takes the session its predecessor left"
    );
    attach_transport_locked(inner, &name, io, capabilities);
}

/// Settle what one mounting connection means for the session it names.
///
/// A mount that finds nothing serving its session takes it and clears the clock
/// [`DispatchState::release_connection`] started: that is the agent that came
/// back inside the reconnect grace, and the always-running agent serving task
/// after task lives in this path. A mount that finds the session already served
/// takes nothing — either another connection holds that very slot, or the task it
/// names now answers from a slot of its own, which is the case where a newer
/// session was spawned to retry the work while the old agent's process came back.
/// Such a connection is recorded read-only, and the slot it names is demoted too
/// when it owns a slot of its own.
///
/// Every binding path runs this one judgement, including the ready report, which
/// reaches an agent without writing a transport. Concurrency is what decides, not
/// the drop clock: the retry that claims an unclaimed session is served, and the
/// connection that returns to a session already served is held, whichever of the
/// two mounted first. [`DispatchState::release_connection`] promotes a held
/// connection when the live one it waited behind goes away, so a plugin that
/// redials over a socket the client has not yet seen die still gets its session.
pub(super) fn note_binding_locked(
    inner: &mut DispatchInner,
    session_id: &str,
    io: &AdapterIo,
) -> bool {
    if is_revived_connection(inner, io) {
        return false;
    }
    let Some(key) = slot_key_named(inner, session_id) else {
        return true;
    };
    let Some(slot) = inner.sessions.get(&key) else {
        return true;
    };
    let task = slot_task(slot);
    let taken = attached_to_other(inner, &key, slot, io);
    let moved_on = !taken
        && inner.sessions.iter().any(|(other, other_slot)| {
            *other != key
                && slot_task(other_slot) == task
                && attached_to_other(inner, other, other_slot, io)
        });
    let revived = taken || moved_on;
    // A mount that takes a session whose death clock was running is the agent
    // coming back inside the reconnect grace: the barrier had already passed, so
    // `ready` says the plugin spoke once, and the clock says the connection it
    // spoke through has since ended. That is the only shape this rebase is for.
    // A first mount has no clock running over a session that ever spoke, and a
    // settled session owes no work its reporter would answer for.
    let returning = !revived && slot.dropped_at.is_some() && slot.ready && slot.task_id.is_some();
    if let Some(slot) = inner.sessions.get_mut(&key) {
        if revived {
            // Only the spelling where this name's own slot is still served by
            // the newer connection leaves a slot of its own to silence; when it
            // is, the slot belongs to that live connection and keeps its rights.
            slot.read_only = moved_on;
        } else {
            slot.dropped_at = None;
            slot.read_only = false;
            // The mount is a frame this session's agent sent, and taking it is
            // what says the agent is here now. Without this the liveness stamp
            // would still read the moment before the drop, and the sweep's
            // silence arm would judge a returning agent that has not beaten yet
            // on the age of a frame from the process before it.
            slot.last_beat = Some(Instant::now());
        }
    }
    if revived {
        tracing::warn!(
            session = %session_id,
            task = %task,
            "a plugin mounted a session this client already serves; it is held read-only"
        );
        return false;
    }
    if returning {
        rebase_returned_reporter(inner, &key);
    }
    true
}

/// Rebase the watermark of a session whose agent came back, so its next frame
/// lands.
///
/// A plugin restarts its own sequence at its base and its generation is a
/// constant, while the watermark the row holds is the last sequence the process
/// that left reached. Left alone, every frame the returning reporter sends reads
/// at or below that watermark and is dropped as a stale duplicate until its
/// sequence climbs past it — for a session that had been running a while, the
/// whole rest of its work, reported into a tuple that never moves.
///
/// The rebase itself is [`rebase_generation`]'s: a new generation over the tuple
/// the client already holds, because the generation is the half the reporter
/// cannot be talked out of — its `generation` field is a constant it never
/// raises, so stamping the beat with the session's own generation is what lets a
/// frame from the new generation through at all, and the sequence starts again
/// under it.
///
/// The body is the stored tuple with the agent dimension put back to `Booting`
/// and the recovery line beside it dropped. That is not a guess about the agent:
/// the connection that witnessed the last agent fact is the one that ended, and
/// the plugin that just mounted has reported no turn fact yet, which is exactly
/// what `Booting` means. `recovery` goes because the reducer's own coupling
/// refuses a recovery substate beside a booting agent, and an accepted receipt
/// goes to `Pending` for the same reason — the repair `compose_observation`
/// already makes for a plugin that reports a booting process over a closed
/// drain. Everything else the client owns — the delivery drain, the reconcile
/// tuning, the counter — rides forward untouched, and so does the resource.
///
/// The generation being replaced is the one whose connection ended: this client
/// is the only authority on its own connection bookkeeping, and it carries the
/// observation content forward rather than replacing it, which is what the
/// attestation the reducer asks for is guarding against. A reporter from a
/// connection this client holds read-only never reaches the reducer at all —
/// `serves_session` refuses its frames before the gate — so lowering the
/// watermark here admits a returning reporter and nothing else.
fn rebase_returned_reporter(inner: &mut DispatchInner, key: &str) {
    let Some(slot) = inner.sessions.get(key) else {
        return;
    };
    let task_id = slot.session.task_id.clone();
    let verdict = rebase_generation(inner, &task_id, |stored| {
        let mut body = stored.clone();
        body.agent = AgentState::Booting;
        body.recovery = RecoveryState::None;
        if body.delivery == DeliveryState::Accepted {
            body.delivery = DeliveryState::Pending;
        }
        body
    });
    match verdict {
        Ok(Some(Verdict::Applied(next))) => tracing::info!(
            task = %task_id,
            generation = next.version.generation,
            "a returning plugin's watermark was rebased onto a new generation"
        ),
        Ok(Some(verdict)) => tracing::warn!(
            task = %task_id,
            ?verdict,
            "the returning plugin's watermark was not rebased"
        ),
        Ok(None) => tracing::warn!(
            task = %task_id,
            "the returning plugin's row was not there to rebase"
        ),
        Err(error) => tracing::warn!(
            task = %task_id,
            error = %error,
            "the returning plugin's watermark was not rebased"
        ),
    }
}

/// Attach one plugin connection to the session it names, or hold it read-only.
///
/// This is the only place a mount becomes a transport, so the read-only
/// connection of §1 (b) never lands in `transports` and never steals the
/// assignment, delivery, or note addressed to the connection that serves the
/// session now. Answers whether the connection took the session.
fn attach_transport_locked(
    inner: &mut DispatchInner,
    session_id: &str,
    io: AdapterIo,
    capabilities: Vec<Capability>,
) -> bool {
    if !note_binding_locked(inner, session_id, &io) {
        record_revived_connection(inner, session_id, io, capabilities);
        return false;
    }
    inner
        .transports
        .insert(session_id.to_string(), (io, capabilities));
    true
}

impl DispatchState {
    /// Hold `io` for as long as one of its inbound frames is being handled.
    pub fn hold_frame(&self, io: &AdapterIo) -> FrameGuard<'_> {
        self.inner.lock().in_frame.push(io.clone());
        FrameGuard {
            state: self,
            io: io.clone(),
        }
    }

    /// Bind one adapter connection to the session it named.
    ///
    /// The name is the session id the client spawned the plugin with, which is
    /// enough on its own: a plugin that mounts before the client staged its
    /// session is remembered here and takes the payload the moment it is
    /// staged, and a plugin that mounts after finds its session waiting.
    pub fn bind_adapter(&self, session_id: &str, io: AdapterIo, capabilities: Vec<Capability>) {
        attach_transport_locked(&mut self.inner.lock(), session_id, io, capabilities);
    }

    /// Remember the delivery handle for one task.
    ///
    /// The handle goes to the session serving the task, not to a read-only slot
    /// that came back for it, so the ack this earns answers the live delivery.
    pub fn attach_msg_id(&self, task_id: &str, msg_id: &str) {
        let mut inner = self.inner.lock();
        let Some(key) = slot_key_serving_task(&inner, task_id) else {
            return;
        };
        if let Some(slot) = inner.sessions.get_mut(&key) {
            slot.msg_id = Some(msg_id.to_string());
        }
    }

    /// Take one plugin `send` frame and answer what the plugin is told.
    ///
    /// A live connection's envelope goes to the durable outbound queue exactly as
    /// it always has, and the answer keeps the shape the plugin reads. A frame
    /// from a connection this client holds read-only is held instead (§1 (c)): it
    /// leaves as part of the merged handoff its task's completion routes, so the
    /// recipient sees one message per downstream role and can tell which session
    /// wrote which half of it.
    pub fn plugin_send(&self, io: &AdapterIo, envelope: &Envelope) -> Result<serde_json::Value> {
        let mut inner = self.inner.lock();
        let Some(session_id) = inner
            .revived
            .iter()
            .find(|(_, revived, _)| revived.same_connection(io))
            .map(|(session_id, _, _)| session_id.clone())
        else {
            let op_id = queue_outbound_locked(&mut inner, envelope)?;
            return Ok(serde_json::json!({"queued": true, "op_id": op_id}));
        };
        let task = slot_key_named(&inner, &session_id)
            .and_then(|key| inner.sessions.get(&key))
            .map(slot_task)
            .unwrap_or(session_id);
        let held = Handoff {
            to_role: held_recipient(&envelope.to),
            text: Some(envelope.body.text.clone().unwrap_or_default()),
        };
        tracing::warn!(
            task = %task,
            to = %held.to_role,
            "a read-only session's send is held for that task's completion"
        );
        inner.held_handoffs.entry(task).or_default().push(held);
        Ok(serde_json::json!({"queued": true, "held": true}))
    }

    /// Park one plugin connection as this role's waiting agent.
    ///
    /// Only a mount that names no session parks: it is a plugin that attached
    /// before any work existed, so it takes the next session this role stages
    /// (plan §6 line 285).
    pub fn park_transport(&self, io: AdapterIo, capabilities: Vec<Capability>) {
        self.inner.lock().parked = Some((io, capabilities));
    }

    /// Claim this role's waiting agent for one staged session.
    ///
    /// An always-running plugin mounts naming no session, so the park holds the
    /// only connection that can serve the session staged next (plan §6 line 285).
    /// The claim binds that connection to the session it takes. A claim left
    /// unbound strands the staged work: the session has a payload and this
    /// client holds no record of the socket that serves it.
    pub(super) fn claim_parked_transport(
        &self,
        session_id: &str,
    ) -> Option<(AdapterIo, Vec<Capability>)> {
        let mut inner = self.inner.lock();
        let (io, capabilities) = inner.parked.take()?;
        if !attach_transport_locked(&mut inner, session_id, io.clone(), capabilities.clone()) {
            // The waiting agent is an older connection returning for a session
            // the role already serves: it holds the socket and takes nothing.
            return None;
        }
        Some((io, capabilities))
    }

    /// The connection that serves one session, when its plugin is attached.
    ///
    /// A plugin names the session it was spawned for, and the slot's key is the
    /// other spelling worth trying.
    pub fn session_transport(&self, session_id: &str) -> Option<(AdapterIo, Vec<Capability>)> {
        let inner = self.inner.lock();
        if let Some(transport) = inner.transports.get(session_id) {
            return Some(transport.clone());
        }
        let key = inner
            .sessions
            .iter()
            .find(|(key, slot)| names_session(key, slot, session_id))
            .map(|(key, _)| key.clone())?;
        inner.transports.get(&key).cloned()
    }

    /// Tell the plugin serving one session to tear itself down, when that plugin
    /// implements `recycle`. A plugin without the capability is skipped: the
    /// caller's backend close stops the process either way.
    pub async fn recycle_plugin(&self, task_id: &str, reason: &str, outcome: Option<Outcome>) {
        let Some((io, capabilities)) = self.session_transport(task_id) else {
            return;
        };
        if missing_capability(&capabilities, Capability::Recycle) {
            tracing::debug!(
                task = %task_id,
                "plugin does not implement recycle; the host closes the resource"
            );
            return;
        }
        let args = RecycleArgs {
            task_id: task_id.to_string(),
            reason: reason.to_string(),
            outcome,
        };
        if let Err(error) = io.notify(AdapterMsg::Host(HostOp::Recycle(args))).await {
            tracing::warn!(error = %error, task = %task_id, "recycle frame did not reach the plugin");
        }
    }

    /// Ask the plugin serving one session for a fresh observation.
    ///
    /// The plugin answers with a heartbeat report, which is the reducer's
    /// evidence and the projection the operator reads. Answers whether a probe
    /// frame actually went out, and `false` says nothing was asked: a session no
    /// connection serves has no plugin to put the question to, and a `notify` that
    /// failed left the frame in this process. The caller must not record either
    /// as a probe that landed, because the answer a live plugin would have given
    /// never existed — the projection that says a plugin is gone is written when
    /// the session ends, never by this call.
    pub async fn probe_plugin(&self, task_id: &str) -> bool {
        let Some((io, _)) = self.session_transport(task_id) else {
            tracing::warn!(task = %task_id, "probe found no plugin connection to ask");
            return false;
        };
        let request = serde_json::json!({"task_id": task_id});
        if let Err(error) = io.notify(AdapterMsg::Host(HostOp::Probe(request))).await {
            tracing::warn!(error = %error, task = %task_id, "probe frame did not reach the plugin");
            return false;
        }
        true
    }

    /// Release the bindings served by one plugin connection.
    ///
    /// A graceful detach retires each idle session because the agent that owned
    /// it has left. An attached transport preserves the idle resource because
    /// the same agent is still reachable. A connection ending through
    /// another path preserves the slot and resource for an agent reconnection and
    /// starts the reconnect clock on it, which is what bounds how long a session
    /// waits for an agent that is never coming back. Every released binding
    /// retires its task progress clock. A slot carrying work remains under
    /// lifecycle ownership, and it carries that same clock: a goodbye and a
    /// silent drop both leave no heartbeat coming for the task it owes, and the
    /// window is what ends a session whose agent never returns.
    pub fn release_connection(
        &self,
        session_id: Option<&str>,
        io: &AdapterIo,
        graceful_detach: bool,
    ) {
        let mut inner = self.inner.lock();
        // A read-only connection ending is not the session losing its agent: the
        // live connection still serves it, and its drop clock stays untouched.
        let revived_connection = {
            let before = inner.revived.len();
            inner
                .revived
                .retain(|(_, revived, _)| !revived.same_connection(io));
            before != inner.revived.len()
        };
        if inner
            .parked
            .as_ref()
            .is_some_and(|(parked, _)| parked.same_connection(io))
        {
            inner.parked = None;
        }
        let released: Vec<String> = inner
            .transports
            .iter()
            .filter(|(session, (transport, _))| {
                transport.same_connection(io)
                    && session_id.is_none_or(|mounted| mounted == session.as_str())
            })
            .map(|(session, _)| session.clone())
            .collect();
        let served_tasks: Vec<String> = released
            .iter()
            .map(|session| {
                inner
                    .sessions
                    .iter()
                    .find(|(key, slot)| names_session(key, slot, session))
                    .map(|(_, slot)| {
                        slot.task_id
                            .clone()
                            .unwrap_or_else(|| slot.session.task_id.clone())
                    })
                    .unwrap_or_else(|| session.clone())
            })
            .collect();
        for task_id in served_tasks {
            inner.stall.forget(&task_id);
        }
        for session in &released {
            inner.transports.remove(session);
        }
        if !revived_connection {
            // The window of `[client] reconnect_grace_secs` starts wherever a
            // session loses the connection that would have sent its next
            // heartbeat and no other one is attached: the agent left without
            // saying so — every session that connection served — or it said
            // goodbye while its session still owed a task, which leaves no beat
            // coming either. The session keeps its slot and its resource for the
            // window, and the mount that returns inside it clears the stamp. A
            // gracefully detached session holding no task needs no window: the
            // idle retirement below takes that slot now.
            let now = Instant::now();
            for session in &released {
                if let Some((_, slot)) = inner.sessions.iter_mut().find(|(key, slot)| {
                    names_session(key, slot, session)
                        && (!graceful_detach || slot.task_id.is_some())
                }) {
                    slot.dropped_at = Some(now);
                }
            }
        }
        for session in &released {
            // Nothing serves this name any more, so the first connection that
            // mounted it read-only behind the one that just went becomes its
            // transport; a session with no such connection keeps waiting out the
            // reconnect grace, which is the sweep's to answer.
            if let Some(key) = slot_key_named(&inner, session) {
                promote_held_connection(&mut inner, &key);
            }
        }
        if graceful_detach {
            let idle: Vec<String> = released
                .iter()
                .filter_map(|session| {
                    inner
                        .sessions
                        .iter()
                        .find(|(key, slot)| {
                            names_session(key, slot, session) && slot.task_id.is_none()
                        })
                        .map(|(key, _)| key.clone())
                })
                .collect();
            for key in idle {
                let reason = inner
                    .sessions
                    .get(&key)
                    .and_then(|slot| stored_close_reason(&inner, &slot.session.task_id))
                    .unwrap_or(onlyne_session::CloseReason::Completed);
                retire_idle_locked(&mut inner, &key, reason);
            }
        }
    }
}

#[cfg(test)]
mod tests;
