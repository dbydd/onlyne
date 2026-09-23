use super::*;

use super::env::{missing_capability, reject_protocol_command_in_pane, served_socket, session_env};
use super::outbound::send_frame;
use super::projection::{note_verdict, sync_session};
use super::state::{
    DispatchInner, DispatchState, SessionSlot, live_sessions, note_beat, rebase_generation,
    render_tokens, slot_key_serving_task,
};
use super::transport::{is_revived_connection, note_binding_locked, record_revived_connection};

pub fn dispatch(state: &DispatchState, envelope: &Envelope) -> Result<SessionRef> {
    let causality = envelope
        .causality
        .as_ref()
        .context("task envelope missing causality.task")?;
    let task_id = causality.task.clone();
    let mut inner = state.inner.lock();
    // A task this role already serves rides its own slot, and the slot that
    // still holds delivery rights is the one that serves it: staging the payload
    // on a read-only revival would hand the work to an agent that may answer for
    // it but may be handed nothing.
    if let Some(session) = slot_key_serving_task(&inner, &task_id)
        .and_then(|key| inner.sessions.get_mut(&key))
        .map(|slot| {
            if slot.payload.is_none() {
                slot.payload = Some(envelope.clone());
                slot.causality = causality.clone();
            }
            slot.session.clone()
        })
    {
        inner.stall.note_assigned(&task_id, Instant::now());
        return Ok(session);
    }
    if live_sessions(&inner) >= inner.max_sessions as usize {
        return Err(anyhow!("max_sessions reached"));
    }
    let session_id = task_id.clone();
    let command = render_tokens(&inner.command, &session_id, &task_id);
    reject_protocol_command_in_pane(inner.backend.name(), &command)?;
    let env = session_env(
        &inner.role,
        &session_id,
        &task_id,
        &inner.relay_required,
        inner.relay_count,
        &inner.topology,
        // One tree answers both halves of this spawn: the cwd below and the
        // socket the plugin dials, so a session whose workspace resolves to a
        // short endpoint is handed the served path directly.
        &served_socket(&inner.workspace),
    );
    let session = inner.backend.spawn(SpawnSpec {
        cwd: inner.workspace.clone(),
        task_id: task_id.clone(),
        command,
        env,
        focus: None,
        placement: None,
        rename: None,
    })?;
    inner.bridge.track_live(session.clone());
    // The task's own record opens with the session that serves it, out of the
    // causality that named the task. A redelivery that found a slot already
    // serving above never reaches this line, so the chain columns are the chain
    // the session opened on; a re-dispatch after retirement refreshes them.
    inner.store.open_task(causality, task_cause(causality))?;
    // A row this task already carries is the record of the session that served it
    // before, and the session staged here is born onto it: `rebase_born_session`
    // moves that row to a generation of its own, and a task with no row keeps the
    // plain seed below. Either way the row the feeds land on starts at
    // `Booting`/`Detached`, under a watermark this session's own count can clear.
    rebase_born_session(&inner, &task_id);
    feed_created(&inner.bridge, &inner.store, &task_id)?;
    feed_dispatched(&inner.bridge, &inner.store, &task_id);
    // A plugin-mode session's liveness is the heartbeat its connection sends and
    // nothing else, and that connection has not spoken yet: the window of
    // `[client] reconnect_grace_secs` starts at birth, so a spawn whose plugin
    // never dials is a ghost the sweep can see rather than a slot that holds its
    // resource forever. The mount that attaches clears the stamp. A self-driven
    // backend owns its agent and answers no adapter socket, so it never has a
    // heartbeat to read and its lifecycle, not this clock, is what ends it.
    let dropped_at = (!inner.backend.self_driven()).then(Instant::now);
    // The liveness stamp starts with the slot, so a session whose plugin mounts
    // and then never sends a frame is readable as silent rather than as a
    // session nobody can judge.
    let last_beat = Some(Instant::now());
    inner.sessions.insert(
        session_id,
        SessionSlot {
            session: session.clone(),
            task_id: Some(task_id.clone()),
            ready: false,
            payload: Some(envelope.clone()),
            msg_id: None,
            origin: Some(envelope.from.clone()),
            causality: causality.clone(),
            dropped_at,
            last_beat,
            read_only: false,
        },
    );
    inner.stall.note_assigned(&task_id, Instant::now());
    Ok(session)
}

/// How a delivery reached this role, which is what the task record's `kind`
/// column holds. A task with a parent above it was handed down from another
/// session's work; one without was given to this role directly. The envelope's
/// own message kind is not that answer: only a task-shaped delivery ever reaches
/// a session, so the kind says nothing the chain does not.
fn task_cause(causality: &Causality) -> &'static str {
    if causality.parent_task.is_some() {
        "relay"
    } else {
        "root"
    }
}

/// Move the row a re-dispatched task already carries onto the generation of the
/// session this dispatch stages.
///
/// This client keeps `client.db` across a restart and a session row is keyed by
/// its task, so the session staged here is born onto whatever row that task
/// already carries; a first dispatch is the only shape with none. That row is the
/// record of the session that served the task before, one no slot of this process
/// holds any more, and none of what it holds can be inherited. Its phase and its
/// resource describe a session that no longer exists, and the feeds below would
/// have to move them from states that refuse them: `resource_attach` from a
/// closed resource is `UndefinedTransition`, which is how the log reads when a
/// ghost was swept before the task came back. Its watermark is worse, because a
/// row it stands on accepts nothing that reads older: the client's own feeds and
/// the plugin's beats share the one counter, and the plugin is a new process
/// whose sequence starts at its base again, below anything a session that lived a
/// while left. Every frame the new session sends is then dropped as a stale
/// duplicate, the turn its agent really ran never reaches the row, and the settle
/// door refuses the completion of work that happened.
///
/// The new generation itself is [`rebase_generation`]'s; the body here is the
/// born tuple of any session — the same `Observation::initial` a fresh row is
/// seeded from, with the role's reconcile policy carried over, so the two ways a
/// session's row comes into being cannot drift. What attests the old generation
/// dead is this client's own bookkeeping: this call is reached only because no
/// slot of this process serves the task, so nothing it holds speaks for that row
/// any more.
fn rebase_born_session(inner: &DispatchInner, task_id: &str) {
    let verdict = rebase_generation(inner, task_id, |stored| {
        Observation::initial(stored.isolate_after, stored.terminate_after)
    });
    match verdict {
        Ok(Some(Verdict::Applied(next))) => tracing::info!(
            task = %task_id,
            generation = next.version.generation,
            "the row a re-dispatched session is born onto was rebased onto a new generation"
        ),
        Ok(Some(verdict)) => tracing::warn!(
            task = %task_id,
            ?verdict,
            "the row of a re-dispatched session was not rebased"
        ),
        Ok(None) => {}
        Err(error) => tracing::warn!(
            task = %task_id,
            error = %error,
            "the row of a re-dispatched session was not rebased"
        ),
    }
}

/// Adapter facts that make a session usable for its task.
pub struct ReadyNotice {
    pub task_id: String,
    pub session_id: String,
    pub generation: u64,
    /// Adapter transport for plugin-driven backends. A self-driven backend owns
    /// its agent and therefore reports ready without a socket.
    pub io: Option<AdapterIo>,
    pub capabilities: Vec<Capability>,
}

/// Report the session ready and hand its held payload to its agent. The `ready`
/// row reaches the ledger before either the backend delivery or adapter frame,
/// which is the causal order §6 requires.
pub async fn on_ready(state: &DispatchState, notice: ReadyNotice, prose: &str) -> Result<()> {
    let ReadyNotice {
        task_id,
        session_id,
        generation,
        io,
        capabilities,
    } = notice;
    let (payload, target, session, backend, version) = {
        let mut inner = state.inner.lock();
        let backend = Arc::clone(&inner.backend);
        // A ready report binds its connection to the session as much as a mount
        // does, so it runs the same judgement §1 (b) hangs on: a connection that
        // returns to a session a newer connection already serves takes nothing,
        // leaves nothing marked ready, and is held for that task's completion.
        if let Some(connection) = io.as_ref() {
            if is_revived_connection(&inner, connection) {
                return Ok(());
            }
            if !note_binding_locked(&mut inner, &session_id, connection) {
                record_revived_connection(
                    &mut inner,
                    &session_id,
                    connection.clone(),
                    capabilities.clone(),
                );
                return Ok(());
            }
        }
        let slot = inner
            .sessions
            .values_mut()
            .find(|slot| {
                slot.session.task_id == task_id
                    || slot
                        .session
                        .backend_ref
                        .get("id")
                        .and_then(|value| value.as_str())
                        == Some(session_id.as_str())
            })
            .ok_or_else(|| anyhow!("unknown session for {task_id}"))?;
        if slot.read_only {
            return Ok(());
        }
        // The hand-off runs once per session: a plugin that reports ready
        // after the assignment already left finds the payload gone.
        let Some(payload) = slot.payload.take() else {
            return Ok(());
        };
        slot.origin = Some(payload.from.clone());
        slot.ready = true;
        let session = slot.session.clone();
        let verdict = feed_ready(&inner.bridge, &inner.store, &task_id)?;
        if matches!(verdict, Verdict::Applied(_)) {
            // The ready report is a frame this session sent that the reducer
            // took, so it is liveness like any other: the sweep reads the stamp
            // rather than the socket, and a session whose plugin passed the
            // barrier and then went quiet has to be readable as quiet.
            note_beat(&mut inner, &task_id, Instant::now());
        }
        let version = note_verdict(&verdict, &task_id).unwrap_or(Version::new(generation, 0));
        (payload, io, session, backend, version)
    };
    // The ready report reaches the server before the payload reaches the agent.
    send_frame(
        state,
        ClientOp::Report(Report::Ready {
            task_id: task_id.clone(),
            session_id: session_id.clone(),
            generation: version.generation,
            seq: version.seq,
            cluster_ref: None,
        }),
    )
    .await?;
    sync_session(state, &task_id).await?;
    let text = payload.body.text.clone().unwrap_or_default();
    match (backend.self_driven(), target) {
        (true, None) => backend.deliver(&session, &task_id, &text),
        (true, Some(_)) => Err(anyhow!(
            "self-driven session {session_id} unexpectedly has an adapter transport"
        )),
        (false, Some(target)) if capabilities.contains(&Capability::Inject) => {
            let assign = AssignArgs {
                envelope: Box::new(payload),
                prose: prose.to_string(),
                task_id,
                generation,
                parent: None,
            };
            target
                .notify(AdapterMsg::Host(HostOp::Assign(assign)))
                .await
                .map_err(|e| anyhow!(e))
        }
        (false, Some(target)) => target
            .notify(AdapterMsg::Host(HostOp::ConfigGet(
                onlyne_proto::ConfigGetArgs {
                    key: format!("stdin:{text}"),
                },
            )))
            .await
            .map_err(|e| anyhow!(e)),
        (false, None) => Err(anyhow!(
            "adapter-backed session {session_id} reported ready without a transport"
        )),
    }
}

impl DispatchState {
    /// Bind a plugin transport to one staged session and hand it the payload.
    ///
    /// Both hand-over paths run through here: a plugin that mounted first, and a
    /// session staged first. The report that marks the session ready leaves
    /// before the assignment, which is the causal order §6 line 285 fixes.
    pub async fn hand_session(
        &self,
        task_id: &str,
        io: AdapterIo,
        capabilities: Vec<Capability>,
    ) -> Result<()> {
        let prose = self.role_prose();
        on_ready(
            self,
            ReadyNotice {
                task_id: task_id.to_string(),
                session_id: task_id.to_string(),
                generation: self.session_generation(task_id).unwrap_or(1),
                io: Some(io),
                capabilities,
            },
            &prose,
        )
        .await
    }

    /// Route one staged session to the thing that serves it.
    ///
    /// A self-driven backend owns its agent and takes the payload immediately,
    /// without an adapter socket. Otherwise the session's own connection comes
    /// first: a plugin the client spawned mounts with this session's id in
    /// `ONLYNE_SESSION_ID`, and a plugin that reconnected mounts with it again,
    /// so its assignment rides that socket alone. A plugin parked for the role
    /// takes the next staged session, once. A plugin-driven session with neither
    /// waits for its mount. Answers whether the payload had somewhere to go.
    pub async fn hand_staged(&self, session_id: &str) -> Result<bool> {
        let self_driven = self.inner.lock().backend.self_driven();
        if self_driven {
            let prose = self.role_prose();
            on_ready(
                self,
                ReadyNotice {
                    task_id: session_id.to_string(),
                    session_id: session_id.to_string(),
                    generation: self.session_generation(session_id).unwrap_or(1),
                    io: None,
                    capabilities: Vec::new(),
                },
                &prose,
            )
            .await?;
            return Ok(true);
        }
        let transport = self
            .session_transport(session_id)
            .or_else(|| self.claim_parked_transport(session_id));
        let Some((io, capabilities)) = transport else {
            return Ok(false);
        };
        self.hand_session(session_id, io, capabilities).await?;
        Ok(true)
    }

    /// Hand one note to the session already serving this role's work.
    ///
    /// A note carries no task, so it owns no session: §3's note is a message to
    /// an agent that is already running, and the plan refuses one whose role is
    /// offline (`note_queue` off). A role with no running agent has nothing to
    /// answer it, which is what the caller reports. Answers whether an agent
    /// took the note.
    pub async fn inject_note(&self, envelope: &Envelope) -> bool {
        let Some((task_id, session_id)) = self.ready_session() else {
            return false;
        };
        let Some((io, capabilities)) = self.session_transport(&session_id) else {
            return false;
        };
        if missing_capability(&capabilities, Capability::Inject) {
            tracing::debug!(task = %task_id, "plugin takes no message mid-task");
            return false;
        }
        let generation = self.session_generation(&task_id).unwrap_or(1);
        let assign = AssignArgs {
            envelope: Box::new(envelope.clone()),
            prose: self.role_prose(),
            task_id,
            generation,
            parent: None,
        };
        io.notify(AdapterMsg::Host(HostOp::Assign(assign)))
            .await
            .is_ok()
    }

    /// The session a mid-task message can join: a ready slot serving a task,
    /// answered as (task, session key).
    fn ready_session(&self) -> Option<(String, String)> {
        let inner = self.inner.lock();
        inner
            .sessions
            .iter()
            .find(|(_, slot)| slot.ready && slot.task_id.is_some() && !slot.read_only)
            .map(|(key, slot)| {
                (
                    slot.task_id.clone().unwrap_or_else(|| key.clone()),
                    key.clone(),
                )
            })
    }
}
