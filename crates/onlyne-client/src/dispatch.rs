use crate::runloop::ClientInit;
use anyhow::{Context, Result, anyhow};
use onlyne_adapter::AdapterIo;
use onlyne_net::conn::{ClientConn, ConnReadiness, dial};
use onlyne_net::{ConnSettings, KeyPair, NetError};
use onlyne_proto::{
    AckArgs, AdapterMsg, AgentPhase, AssignArgs, Body, Capability, Causality, ClientOp,
    DeliveryPhase, Envelope, Frame, HandshakeArgs, HostOp, Lifecycle, MsgKind, Outcome,
    PROTOCOL_VERSION, Principal, RecoveryPhase, Report, ResBody, ResourcePhase, SessionProjection,
    SessionSyncArgs, Welcome, new_envelope,
};
use onlyne_session::{
    Bridge, IgnoredReason, LifecycleEvent, Observation, SessionBackend, SessionLedger,
    SessionRecord, SessionRef, SpawnSpec, Verdict, Version, apply_persist, feed_created,
    feed_dispatched, feed_ready, feed_resource_closed, settle,
};
use onlyne_store::ClientStore;
use parking_lot::Mutex;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

#[derive(Clone)]
pub struct DispatchState {
    inner: Arc<Mutex<DispatchInner>>,
}
struct DispatchInner {
    pub role: String,
    pub workspace: PathBuf,
    pub command: Vec<String>,
    pub max_sessions: u32,
    pub reuse: bool,
    pub backend: Arc<dyn SessionBackend>,
    pub store: ClientStore,
    pub bridge: Bridge,
    pub sessions: HashMap<String, SessionSlot>,
    /// Live link, installed by the runloop while the connection is up.
    pub outbox: Option<Arc<dyn Outbox>>,
    /// Flag the runloop and the dispatcher share while the link is down.
    pub accept_new: Arc<AtomicBool>,
    /// Aggregate name this role supervises, empty for a plain role.
    pub cluster_ref: String,
    /// The adapter transport this role's agent attached, held for every session
    /// the role runs. A plugin the client spawned learns its session from the
    /// assignment, so the client keeps the connection and hands each staged
    /// session to it in turn (plan §6 line 285).
    pub plugin_transport: Option<(AdapterIo, Vec<Capability>)>,
}

#[derive(Clone)]
pub struct SessionSlot {
    pub session: SessionRef,
    pub family: String,
    pub task_id: Option<String>,
    pub ready: bool,
    pub capabilities: Vec<Capability>,
    pub io: Option<AdapterIo>,
    /// Payload held until the adapter reports ready, which keeps the ready
    /// barrier of §6 ahead of the `assign` frame.
    pub payload: Option<Envelope>,
    /// Delivery handle, owed back to the server as one `ack`.
    pub msg_id: Option<String>,
    /// Sender of the payload this session serves, kept for its `Completion`.
    pub origin: Option<Principal>,
}

/// Write one ack into the durable intent queue.
fn store_ack(inner: &DispatchInner, mut ack: AckArgs) {
    if ack.op_id.is_none() {
        ack.op_id = Some(onlyne_proto::new_op_id());
    }
    let Some(op_id) = ack.op_id.clone() else {
        return;
    };
    match serde_json::to_value(ClientOp::Ack(ack)) {
        Ok(value) => {
            if let Err(error) = inner.store.enqueue_intent(&op_id, &value) {
                tracing::warn!(error = %error, "settled ack was not stored");
            }
        }
        Err(error) => tracing::warn!(error = %error, "settled ack did not serialize"),
    }
}

/// A staged session that a plugin connection can serve.
pub struct PendingSlot {
    pub task_id: String,
    pub session_id: String,
    pub generation: u64,
}

impl DispatchState {
    pub fn new(
        role: impl Into<String>,
        workspace: impl Into<PathBuf>,
        command: Vec<String>,
        max_sessions: u32,
        reuse: bool,
        backend: Arc<dyn SessionBackend>,
        store: ClientStore,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(DispatchInner {
                role: role.into(),
                workspace: workspace.into(),
                command,
                max_sessions,
                reuse,
                backend,
                store,
                bridge: Bridge::new(),
                sessions: HashMap::new(),
                outbox: None,
                accept_new: Arc::new(AtomicBool::new(true)),
                cluster_ref: String::new(),
                plugin_transport: None,
            })),
        }
    }

    pub fn session_count(&self) -> usize {
        self.inner.lock().sessions.len()
    }
    pub fn role(&self) -> String {
        self.inner.lock().role.clone()
    }
    pub fn command(&self) -> Vec<String> {
        self.inner.lock().command.clone()
    }
    pub fn backend_name(&self, task_id: &str) -> Option<String> {
        self.inner
            .lock()
            .sessions
            .values()
            .find(|slot| slot.task_id.as_deref() == Some(task_id))
            .map(|slot| slot.session.backend.clone())
    }
    /// Bind one adapter transport to the session serving `task_id`.
    ///
    /// The plan keys cross-machine identity on `task_id`, so no backend-specific
    /// reference spelling enters this lookup.
    pub fn bind_adapter(
        &self,
        task_id: &str,
        io: AdapterIo,
        capabilities: Vec<Capability>,
    ) -> Result<()> {
        let mut inner = self.inner.lock();
        let slot = inner
            .sessions
            .values_mut()
            .find(|slot| slot.task_id.as_deref() == Some(task_id))
            .ok_or_else(|| anyhow!("unknown session {task_id}"))?;
        slot.io = Some(io);
        slot.capabilities = capabilities;
        Ok(())
    }

    /// Remember the delivery handle for one task.
    pub fn attach_msg_id(&self, task_id: &str, msg_id: &str) {
        let mut inner = self.inner.lock();
        if let Some(slot) = inner
            .sessions
            .values_mut()
            .find(|slot| slot.task_id.as_deref() == Some(task_id))
        {
            slot.msg_id = Some(msg_id.to_string());
        }
    }

    /// The session a fresh plugin connection should serve.
    ///
    /// An exact session match wins. A mount that names no session takes the
    /// staged session still waiting for a plugin, which is the case §6 line 285
    /// describes: the payload waits in the client until the adapter arrives,
    /// because a plugin the client spawned learns its session only from the
    /// assignment.
    pub fn pending_plugin_slot(&self, session_id: Option<&str>) -> Option<PendingSlot> {
        let inner = self.inner.lock();
        let mut staged: Vec<&SessionSlot> = inner
            .sessions
            .values()
            .filter(|slot| slot.payload.is_some() && !slot.ready)
            .collect();
        staged.sort_by(|left, right| left.session.task_id.cmp(&right.session.task_id));
        let slot = match session_id {
            Some(wanted) => staged
                .into_iter()
                .find(|slot| slot.session.task_id == wanted)?,
            None => staged.into_iter().next()?,
        };
        Some(PendingSlot {
            task_id: slot.session.task_id.clone(),
            session_id: slot.session.task_id.clone(),
            generation: slot.session.generation,
        })
    }

    /// Queue an ack the client owes the server.
    ///
    /// Record an ack the client owes the server.
    ///
    /// The ack is durable: D11's control plane is at-least-once, and a settled
    /// session whose ack is lost leaves the row in flight forever. The intent
    /// queue carries it across a link that is down, and the flusher is the
    /// sender.
    pub fn push_settled(&self, ack: AckArgs) {
        store_ack(&self.inner.lock(), ack);
    }

    /// Adopt the role slice the server sent with `welcome`.
    pub fn reconfigure(&self, command: Vec<String>, max_sessions: u32, reuse: bool) {
        let mut inner = self.inner.lock();
        inner.command = command;
        inner.max_sessions = max_sessions;
        inner.reuse = reuse;
    }

    /// Role prose last cached from `welcome`.
    pub fn role_prose(&self) -> String {
        let inner = self.inner.lock();
        inner
            .store
            .prose(&inner.role)
            .ok()
            .flatten()
            .map(|(prose, _)| prose)
            .unwrap_or_default()
    }

    /// Queue an outbound envelope before its first write and answer its op_id.
    pub fn enqueue_outbound(&self, envelope: &Envelope) -> Result<String> {
        envelope
            .validate()
            .map_err(|error| anyhow!(error.to_string()))?;
        let op_id = envelope
            .op_id
            .clone()
            .context("outbound envelope missing op_id")?;
        self.inner
            .lock()
            .store
            .enqueue_intent(&op_id, &serde_json::to_value(envelope)?)?;
        Ok(op_id)
    }

    /// The flag the runloop and the dispatcher share.
    pub fn accept_new(&self) -> Arc<AtomicBool> {
        self.inner.lock().accept_new.clone()
    }

    /// Aggregate name this role supervises, empty for a plain role.
    pub fn cluster_ref(&self) -> String {
        self.inner.lock().cluster_ref.clone()
    }

    /// Record the aggregate name once, so every report keeps the same value
    /// across a reconnect.
    pub fn set_cluster_ref(&self, aggregate: impl Into<String>) {
        self.inner.lock().cluster_ref = aggregate.into();
    }

    /// Remember the transport this role's agent attached.
    pub fn set_plugin_transport(&self, io: AdapterIo, capabilities: Vec<Capability>) {
        self.inner.lock().plugin_transport = Some((io, capabilities));
    }

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
        self.bind_adapter(task_id, io.clone(), capabilities.clone())?;
        let prose = self.role_prose();
        on_ready(
            self,
            ReadyNotice {
                task_id: task_id.to_string(),
                session_id: task_id.to_string(),
                generation: self.session_generation(task_id).unwrap_or(1),
                io,
                capabilities,
            },
            &prose,
        )
        .await
    }

    /// Generation the reducer holds for one task, before any hand-off.
    pub fn session_generation(&self, task_id: &str) -> Option<u64> {
        self.inner
            .lock()
            .sessions
            .values()
            .find(|slot| slot.task_id.as_deref() == Some(task_id))
            .map(|slot| slot.session.generation)
    }

    /// Whether one more delivery fits the role's concurrency.
    ///
    /// §5's `max_sessions` caps concurrent sessions, so a delivery that arrives
    /// at the cap waits on the server rather than being refused: the row stays
    /// in flight and the next pull offers it again once a session frees.
    /// Whether a delivery has somewhere to run: a role below its limit, or an
    /// idle session a finished task handed back (§5 `max_sessions`/`reuse`).
    pub fn has_capacity(&self) -> bool {
        let inner = self.inner.lock();
        inner.sessions.len() < inner.max_sessions as usize
            || inner.sessions.values().any(|slot| slot.task_id.is_none())
    }

    /// The transport to hand a staged session to, when an agent is attached.
    pub fn plugin_transport(&self) -> Option<(AdapterIo, Vec<Capability>)> {
        self.inner.lock().plugin_transport.clone()
    }

    /// Ask the server one question over the live link.
    ///
    /// Err means the link is down, never a refusal: a refusal arrives as an
    /// `Ok` body carrying `ok: false`, which is what the local CLI shows.
    pub async fn request(&self, op: ClientOp) -> Result<ResBody, NetError> {
        let outbox = { self.inner.lock().outbox.clone() };
        let Some(outbox) = outbox else {
            return Err(NetError::NotReady);
        };
        outbox.request(op).await
    }

    /// Install the live link as the outbound path.
    pub fn attach_outbox(&self, outbox: Arc<dyn Outbox>) {
        self.inner.lock().outbox = Some(outbox);
    }

    /// Remove the outbound path; lifecycle frames then queue as intents.
    pub fn detach_outbox(&self) {
        self.inner.lock().outbox = None;
    }

    fn outbox(&self) -> Option<Arc<dyn Outbox>> {
        self.inner.lock().outbox.clone()
    }

    /// Queue one client op in the durable intent table and answer its op_id.
    pub fn enqueue_op(&self, op: &ClientOp) -> Result<String> {
        let op_id = onlyne_proto::new_id();
        self.inner
            .lock()
            .store
            .enqueue_intent(&op_id, &serde_json::to_value(op)?)?;
        Ok(op_id)
    }
}

fn family_of(envelope: &Envelope) -> String {
    envelope
        .causality
        .as_ref()
        .and_then(|c| c.parent_task.clone())
        .or_else(|| envelope.task_id().map(str::to_string))
        .unwrap_or_else(|| envelope.id.clone())
}

fn render_tokens(tokens: &[String], session: &str, task: &str) -> Vec<String> {
    tokens
        .iter()
        .map(|token| token.replace("{session}", session).replace("{task}", task))
        .collect()
}

pub fn dispatch(state: &DispatchState, envelope: &Envelope) -> Result<SessionRef> {
    let task_id = envelope
        .task_id()
        .context("task envelope missing causality.task")?
        .to_string();
    let family = family_of(envelope);
    let mut inner = state.inner.lock();
    if let Some(slot) = inner
        .sessions
        .values_mut()
        .find(|slot| slot.task_id.as_deref() == Some(task_id.as_str()))
    {
        if slot.payload.is_none() {
            slot.payload = Some(envelope.clone());
        }
        return Ok(slot.session.clone());
    }
    if inner.reuse {
        // An idle session with no bound task takes the next task, preferring
        // one from the same family; §5's `reuse` is what makes a second task
        // share a session instead of waiting for a new slot.
        let same_family = inner
            .sessions
            .iter()
            .find(|(_, slot)| slot.task_id.is_none() && slot.family == family)
            .map(|(key, _)| key.clone());
        let idle = same_family.or_else(|| {
            inner
                .sessions
                .iter()
                .find(|(_, slot)| slot.task_id.is_none())
                .map(|(key, _)| key.clone())
        });
        let reused = idle
            .and_then(|key| inner.sessions.get_mut(&key))
            .map(|slot| {
                slot.task_id = Some(task_id.clone());
                slot.payload = Some(envelope.clone());
                slot.ready = false;
                let session = SessionRef {
                    task_id: task_id.clone(),
                    ..slot.session.clone()
                };
                slot.session = session.clone();
                session
            });
        if let Some(session) = reused {
            inner.bridge.track_live(session.clone());
            feed_created(&inner.bridge, &inner.store, &task_id)?;
            feed_dispatched(&inner.bridge, &inner.store, &task_id);
            return Ok(session);
        }
    }
    if inner.sessions.len() >= inner.max_sessions as usize {
        return Err(anyhow!("max_sessions reached"));
    }
    let session_id = task_id.clone();
    let command = render_tokens(&inner.command, &session_id, &task_id);
    let mut env = BTreeMap::new();
    env.insert("ONLYNE_SESSION_ID".into(), session_id.clone());
    env.insert("ONLYNE_TASK_ID".into(), task_id.clone());
    env.insert("ONLYNE_ROLE".into(), inner.role.clone());
    let session = inner.backend.spawn(SpawnSpec {
        cwd: inner.workspace.clone(),
        task_id: task_id.clone(),
        command,
        env,
        focus: None,
        rename: None,
    })?;
    inner.bridge.track_live(session.clone());
    feed_created(&inner.bridge, &inner.store, &task_id)?;
    feed_dispatched(&inner.bridge, &inner.store, &task_id);
    inner.sessions.insert(
        session_id,
        SessionSlot {
            session: session.clone(),
            family,
            task_id: Some(task_id),
            ready: false,
            capabilities: Vec::new(),
            io: None,
            payload: Some(envelope.clone()),
            msg_id: None,
            origin: Some(envelope.from.clone()),
        },
    );
    Ok(session)
}

/// Adapter facts that make a session usable for its task.
pub struct ReadyNotice {
    pub task_id: String,
    pub session_id: String,
    pub generation: u64,
    pub io: AdapterIo,
    pub capabilities: Vec<Capability>,
}

/// Report the session ready and hand its held payload to the adapter. The
/// `ready` row reaches the ledger before the `assign` frame leaves, which is
/// the causal order §6 requires.
pub async fn on_ready(state: &DispatchState, notice: ReadyNotice, prose: &str) -> Result<()> {
    let ReadyNotice {
        task_id,
        session_id,
        generation,
        io,
        capabilities,
    } = notice;
    let (payload, target, version) = {
        let mut inner = state.inner.lock();
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
        // The hand-off runs once per session: a plugin that reports ready
        // after the assignment already left finds the payload gone.
        let Some(payload) = slot.payload.take() else {
            return Ok(());
        };
        slot.origin = Some(payload.from.clone());
        slot.ready = true;
        slot.io = Some(io.clone());
        slot.capabilities = capabilities.clone();
        let verdict = feed_ready(&inner.bridge, &inner.store, &task_id)?;
        let version = note_verdict(&verdict, &task_id).unwrap_or(Version::new(generation, 0));
        (payload, io, version)
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
    if capabilities.contains(&Capability::Inject) {
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
            .map_err(|e| anyhow!(e))?;
    } else {
        target
            .notify(AdapterMsg::Host(HostOp::ConfigGet(
                onlyne_proto::ConfigGetArgs {
                    key: format!("stdin:{text}"),
                },
            )))
            .await
            .map_err(|e| anyhow!(e))?;
    }
    Ok(())
}

pub async fn on_out(
    state: &DispatchState,
    task_id: &str,
    outcome: Outcome,
    head: Option<String>,
) -> Result<()> {
    let (verdict, receipt) = {
        let mut inner = state.inner.lock();
        let verdict = settle(
            &inner.bridge,
            &inner.store,
            task_id,
            match outcome {
                Outcome::Done => onlyne_session::Outcome::Done,
                Outcome::Failed => onlyne_session::Outcome::Failed,
                Outcome::Cancelled => onlyne_session::Outcome::Cancelled,
            },
        )?;
        inner
            .store
            .put_out_head(task_id, head.as_deref().unwrap_or(""))?;
        let slot = inner
            .sessions
            .values_mut()
            .find(|slot| slot.task_id.as_deref() == Some(task_id));
        let origin = slot.as_ref().and_then(|slot| slot.origin.clone());
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

        (
            verdict,
            completion_envelope(&inner.role, origin, task_id, head.as_deref()),
        )
    };
    note_verdict(&verdict, task_id);
    // The terminal receipt leaves as its own envelope, so the origin — a role
    // or a gateway conversation — learns the outcome (plan §3 `Completion`).
    // It rides the intent queue, which is what makes a completion survive the
    // disconnect rules of §6 line 289.
    if let Some(envelope) = receipt {
        transport_envelope(state, &envelope).await?;
    }
    sync_session(state, task_id).await
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
    let body = match head {
        Some(text) if !text.is_empty() => Body::text(text.to_string()),
        _ => Body::default(),
    };
    let causality = Causality {
        task: task_id.to_string(),
        parent_task: None,
        reply_to: None,
        hop: 0,
        attempt: 0,
    };
    let mut envelope = new_envelope(
        MsgKind::Completion,
        Principal::role(role),
        origin,
        body,
        Some(causality),
    )
    .ok()?;
    envelope.body.text = envelope.body.text.or_else(|| Some(String::new()));
    envelope.validate().ok()?;
    Some(envelope)
}

/// Hand one envelope to the live link, or to the intent queue when it is down.
async fn transport_envelope(state: &DispatchState, envelope: &Envelope) -> Result<()> {
    let op = ClientOp::Send(Box::new(envelope.clone()));
    let outbox = { state.inner.lock().outbox.clone() };
    match outbox {
        Some(outbox) => match outbox.send(op).await {
            Ok(()) => Ok(()),
            Err(error) => {
                tracing::warn!(error = %error, "completion fell back to the intent queue");
                state.enqueue_outbound(envelope).map(|_| ())
            }
        },
        None => state.enqueue_outbound(envelope).map(|_| ()),
    }
}

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

/// Give one session's slot back, closing a live resource when the caller named a
/// reason. A settled task calls this with no reason: §5's `reuse` keeps the slot
/// for the next task of the role, and `max_sessions` counts what the map holds,
/// so a role at its limit gets capacity back as its tasks end.
fn release_locked(
    inner: &mut DispatchInner,
    task_id: &str,
    reason: Option<onlyne_session::CloseReason>,
) -> Result<()> {
    let resource = inner
        .store
        .get_session(task_id)?
        .map(|row| row.resource_state)
        .unwrap_or_else(|| "detached".to_string());
    if let Some((key, slot)) = inner
        .sessions
        .iter()
        .find(|(_, slot)| slot.task_id.as_deref() == Some(task_id))
        .map(|(k, s)| (k.clone(), s.clone()))
    {
        if let Some(reason) = reason {
            if resource != "detached" && resource != "closed" {
                feed_resource_closed(&inner.bridge, &inner.store, task_id)?;
                inner.backend.close(&slot.session, reason, false)?;
            }
        }
        inner.bridge.untrack_live(task_id);
        if inner.reuse {
            if let Some(s) = inner.sessions.get_mut(&key) {
                s.task_id = None;
                s.ready = false;
            }
        } else {
            inner.sessions.remove(&key);
        }
    }
    if reason.is_some() && resource == "detached" {
        inner
            .store
            .note_alert(format!("session recycled {task_id}"));
    }
    Ok(())
}

/// Whether the session serving `task_id` still has a live resource.
///
/// `attach` runs first because a backend can re-resolve a resource whose
/// stored reference went stale (Orca mints a new terminal handle per PTY
/// incarnation). A refreshed reference is written back to the slot and the
/// bridge, which is what makes the next probe, close, or ledger write target
/// the current resource. An attach the backend cannot answer is not proof of
/// death, so the probe decides.
pub fn session_alive(state: &DispatchState, task_id: &str) -> bool {
    let mut inner = state.inner.lock();
    let Some(key) = inner
        .sessions
        .iter()
        .find(|(_, slot)| slot.task_id.as_deref() == Some(task_id))
        .map(|(key, _)| key.clone())
    else {
        return false;
    };
    let session = inner.sessions[&key].session.clone();
    let session = match inner.backend.attach(&session) {
        Ok(refreshed) => {
            if refreshed != session {
                inner.bridge.track_live(refreshed.clone());
                if let Some(slot) = inner.sessions.get_mut(&key) {
                    slot.session = refreshed.clone();
                }
            }
            refreshed
        }
        Err(_) => session,
    };
    inner
        .backend
        .probe(&session)
        .map(|probe| probe.alive)
        .unwrap_or(false)
}

/// Close every live session's resource with `reason` and forget the slots.
///
/// This is the shutdown path: a stopped client must not leave resources behind
/// that only it can address, and each backend's own record of the resource —
/// the Orca tab map included — ends with the session. `budget` bounds the whole
/// sweep, because `onlyne-client stop` waits 10 seconds for the process to
/// leave and a slow backend CLI must not turn a stop into a hang; whatever the
/// budget cuts off is reported and dropped anyway.
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

pub async fn on_plugin_report(state: &DispatchState, report: Report) -> Result<()> {
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
            generation,
            seq,
            observed,
            ..
        } => {
            let verdict = match serde_json::from_value::<Observation>(observed) {
                Ok(body) => {
                    let inner = state.inner.lock();
                    apply_persist(
                        &inner.bridge,
                        &inner.store,
                        &task_id,
                        &LifecycleEvent::Heartbeat {
                            v: Version::new(generation, seq),
                            body,
                        },
                    )?
                }
                Err(error) => {
                    tracing::warn!(task = %task_id, error = %error, "heartbeat carries no readable observation; liveness only");
                    Verdict::Ignored(IgnoredReason::NoOp)
                }
            };
            note_verdict(&verdict, &task_id).is_some()
        }
        Report::Complete {
            task_id,
            outcome,
            head,
            ..
        } => {
            on_out(state, &task_id, outcome, head).await?;
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

/// Task a state-carrying report names, for the projection publish.
fn task_id_of(report: &Report) -> &str {
    match report {
        Report::Ready { task_id, .. } | Report::Heartbeat { task_id, .. } => task_id,
        Report::Complete { task_id, .. } => task_id,
        Report::Fault { .. } => "",
    }
}

pub fn missing_capability(capabilities: &[Capability], capability: Capability) -> bool {
    !capabilities.contains(&capability)
}

pub fn plugin_gap(capabilities: &[Capability]) -> Vec<onlyne_adapter::HostGap> {
    onlyne_adapter::degrade_for(
        &[Capability::Recycle, Capability::Report, Capability::Inject]
            .iter()
            .copied()
            .filter(|cap| missing_capability(capabilities, *cap))
            .collect::<Vec<_>>(),
    )
}

/// Agent name this binary reports during the handshake.
const AGENT: &str = "onlyne-client";
/// Wait bound for one request round trip.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Server heartbeat interval from the observation rules of §4.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);

/// One authenticated server link.
///
/// The first five methods are the whole transport surface the runloop uses, so
/// a change of transport touches this struct alone. The remaining two read the
/// supervision state of the connection the handle keeps across redials.
#[derive(Clone)]
pub struct ClientLink {
    handle: ClientConn,
    welcome: Arc<Welcome>,
    hello: HandshakeArgs,
}

impl ClientLink {
    /// Dial, verify the certificate pin, sign the server challenge, then read
    /// the role slice with `hello`.
    pub async fn connect(init: &ClientInit) -> Result<Self, NetError> {
        let keypair = KeyPair::load(&init.key_path)?;
        let settings = ConnSettings {
            agent: AGENT.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            ..ConnSettings::new(PROTOCOL_VERSION)
        };
        let handle: ClientConn =
            dial(&init.server, &keypair, &init.cert_pin, &init.role, settings).await?;
        let hello = HandshakeArgs {
            protocol: PROTOCOL_VERSION,
            role: init.role.clone(),
            key: keypair.public_str(),
            signature: String::new(),
            agent: AGENT.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            aggregate: false,
        };
        let body = handle
            .request(
                Frame::req(String::new(), ClientOp::Hello(hello.clone())),
                REQUEST_TIMEOUT,
            )
            .await?;
        if !body.ok {
            let error = body.error.clone().unwrap_or(onlyne_proto::ErrorPayload {
                code: onlyne_proto::ErrorCode::Internal,
                message: "hello refused".to_string(),
                field: None,
            });
            return Err(NetError::Rejected {
                code: wire_code(error.code),
                message: error.message,
            });
        }
        let data = body.data().cloned().ok_or(NetError::BadFrame)?;
        let welcome: Welcome = serde_json::from_value(data).map_err(|_| NetError::BadFrame)?;
        Ok(Self {
            handle,
            welcome: Arc::new(welcome),
            hello,
        })
    }

    /// Send the routed `hello` again on the connection this link now holds.
    ///
    /// The net layer redials on its own, and a fresh connection carries no role
    /// binding until this frame lands, so a caller replays it whenever readiness
    /// returns (plan §7 line 310).
    pub async fn authenticate(&self) -> Result<(), NetError> {
        let body = self
            .handle
            .request(
                Frame::req(String::new(), ClientOp::Hello(self.hello.clone())),
                REQUEST_TIMEOUT,
            )
            .await?;
        if !body.ok {
            let error = body.error.clone().unwrap_or(onlyne_proto::ErrorPayload {
                code: onlyne_proto::ErrorCode::Internal,
                message: "hello refused".to_string(),
                field: None,
            });
            return Err(NetError::Rejected {
                code: wire_code(error.code),
                message: error.message,
            });
        }
        Ok(())
    }

    /// Role slice the server bound this link to.
    pub fn welcome(&self) -> &Welcome {
        &self.welcome
    }

    /// Send one request frame and answer with its body. A refusal from the
    /// server arrives inside the body, which keeps the retry decision in the
    /// intent machine.
    pub async fn request(&self, op: ClientOp) -> Result<ResBody, NetError> {
        self.handle
            .request(Frame::req(String::new(), op), REQUEST_TIMEOUT)
            .await
    }

    /// Clone the server observation stream.
    pub fn events(&self) -> broadcast::Receiver<Frame<ClientOp>> {
        self.handle.events()
    }

    /// Send `bye` and drain in-flight requests.
    pub async fn close(&self) -> Result<(), NetError> {
        self.handle.close().await
    }

    /// Liveness of the connection behind this link.
    pub fn readiness(&self) -> ConnReadiness {
        self.handle.readiness()
    }
    /// Reason the supervisor stopped redialing.
    pub async fn failure(&self) -> Option<NetError> {
        self.handle.failure().await
    }
}

/// Wire code of a refusal, in the snake_case spelling both sides share.
fn wire_code(code: onlyne_proto::ErrorCode) -> String {
    serde_json::to_value(code)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "internal".to_string())
}

/// Stamp the origin cluster on the state-carrying report kinds.
///
/// `cluster_ref` names the cluster whose supervisor observed this projection
/// (`docs/v1-PLAN.md` line 248, the `aggregate` annotation of the role's own
/// spec entry, which is `Principal::Cluster` on the wire at line 122). A plain
/// role leaves the field unset, which is the `skip_serializing_if` shape the
/// byte-identical replay rule at line 502 depends on.
pub fn with_cluster(state: &DispatchState, report: Report) -> Report {
    let cluster = state.cluster_ref();
    if cluster.is_empty() {
        return report;
    }
    match report {
        Report::Ready {
            task_id,
            session_id,
            generation,
            seq,
            ..
        } => Report::Ready {
            task_id,
            session_id,
            generation,
            seq,
            cluster_ref: Some(cluster),
        },
        Report::Heartbeat {
            task_id,
            generation,
            seq,
            observed,
            ..
        } => Report::Heartbeat {
            task_id,
            generation,
            seq,
            observed,
            cluster_ref: Some(cluster),
        },
        Report::Complete {
            task_id,
            outcome,
            head,
            reply_to,
            ..
        } => Report::Complete {
            task_id,
            outcome,
            head,
            reply_to,
            cluster_ref: Some(cluster),
        },
        other => other,
    }
}

/// Where lifecycle frames leave the dispatcher.
///
/// `send` completes once the frame is on the wire. The ready report awaits this
/// before the payload reaches the agent, which is the causal order §6 fixes.
pub trait Outbox: Send + Sync {
    fn send(&self, op: ClientOp)
    -> Pin<Box<dyn Future<Output = Result<(), NetError>> + Send + '_>>;

    /// One request round trip, for a caller that needs the server's answer
    /// rather than a queued frame: the local CLI reports the verdict a send got.
    fn request(
        &self,
        op: ClientOp,
    ) -> Pin<Box<dyn Future<Output = Result<ResBody, NetError>> + Send + '_>>;
}

impl Outbox for ClientLink {
    fn send(
        &self,
        op: ClientOp,
    ) -> Pin<Box<dyn Future<Output = Result<(), NetError>> + Send + '_>> {
        Box::pin(async move { self.request(op).await.map(|_| ()) })
    }

    fn request(
        &self,
        op: ClientOp,
    ) -> Pin<Box<dyn Future<Output = Result<ResBody, NetError>> + Send + '_>> {
        Box::pin(async move { ClientLink::request(self, op).await })
    }
}

/// Deliver one lifecycle frame, and queue it durably when the link is down.
///
/// §6 line 289: a running session reaches its terminal state while the outbound
/// work waits in `client.db` intents for the flusher.
pub async fn send_frame(state: &DispatchState, op: ClientOp) -> Result<()> {
    let op = match op {
        ClientOp::Report(report) => ClientOp::Report(with_cluster(state, report)),
        other => other,
    };
    if let Some(outbox) = state.outbox() {
        if outbox.send(op.clone()).await.is_ok() {
            return Ok(());
        }
    }
    state.accept_new().store(false, Ordering::SeqCst);
    state.enqueue_op(&op)?;
    Ok(())
}

/// The wire projection of one stored session row.
pub fn projection_of(row: &SessionRecord) -> SessionProjection {
    let observed: Option<serde_json::Value> = serde_json::from_str(&row.observed_json).ok();
    // The reducer records the terminal outcome inside the observation, and the
    // plan's session read publishes it beside the lifecycle (line 498).
    let outcome = observed
        .as_ref()
        .and_then(|value| value.get("outcome"))
        .and_then(|value| serde_json::from_value::<Outcome>(value.clone()).ok());
    SessionProjection {
        lifecycle: phase(&row.public_lifecycle, Lifecycle::Created),
        agent: phase(&row.agent_state, AgentPhase::Booting),
        delivery: phase(&row.delivery_state, DeliveryPhase::NoIntent),
        resource: phase(&row.resource_state, ResourcePhase::Detached),
        recovery: phase(&row.recovery_substate, RecoveryPhase::NoRecovery),
        outcome,
        observed,
    }
}

/// Decode one stored enum word, falling back to the freshly created phase.
fn phase<T: serde::de::DeserializeOwned>(word: &str, fallback: T) -> T {
    serde_json::from_value(serde_json::Value::String(word.to_string())).unwrap_or(fallback)
}

/// Publish the current projection of one session.
pub async fn sync_session(state: &DispatchState, task_id: &str) -> Result<()> {
    let row = { state.inner.lock().store.get_session(task_id)? };
    let Some(row) = row else { return Ok(()) };
    let args = SessionSyncArgs {
        task_id: row.task_id.clone(),
        session_id: row.task_id.clone(),
        generation: row.generation.max(0) as u64,
        seq: row.seq.max(0) as u64,
        projection: projection_of(&row),
    };
    send_frame(state, ClientOp::SessionSync(args)).await
}

/// Log a reducer verdict and answer the version it advanced to.
pub fn note_verdict(verdict: &Verdict, task_id: &str) -> Option<Version> {
    match verdict {
        Verdict::Applied(observation) => Some(observation.version),
        Verdict::Ignored(reason) => {
            tracing::debug!(task = %task_id, ?reason, "lifecycle event ignored");
            None
        }
        Verdict::Rejected(reason) => {
            tracing::warn!(task = %task_id, ?reason, "lifecycle event rejected; the ledger kept its state");
            None
        }
    }
}
