use anyhow::{Context, Result, anyhow};
use onlyne_adapter::AdapterIo;
use onlyne_net::conn::{ClientConn, ConnReadiness, dial};
use onlyne_net::{ConnSettings, KeyPair, NetError};
use onlyne_proto::{AckArgs, AdapterMsg, AgentPhase, AssignArgs, Capability, ClientOp, DeliveryPhase, Envelope, Frame, HandshakeArgs, HostOp, Lifecycle, Outcome, PROTOCOL_VERSION, RecoveryPhase, Report, ResBody, ResourcePhase, SessionProjection, SessionSyncArgs, Welcome};
use onlyne_session::{Bridge, IgnoredReason, LifecycleEvent, Observation, SessionBackend, SessionLedger, SessionRecord, SessionRef, SpawnSpec, Verdict, Version, apply_persist, feed_created, feed_dispatched, feed_ready, feed_resource_closed, settle};
use onlyne_store::ClientStore;
use parking_lot::Mutex;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::broadcast;
use crate::runloop::ClientInit;

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
    /// Ack owed to the server for every delivery whose local work finished.
    pub settled: Vec<AckArgs>,
    /// Live link, installed by the runloop while the connection is up.
    pub outbox: Option<Arc<dyn Outbox>>,
    /// Flag the runloop and the dispatcher share while the link is down.
    pub accept_new: Arc<AtomicBool>,
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
}

impl DispatchState {
    pub fn new(role: impl Into<String>, workspace: impl Into<PathBuf>, command: Vec<String>, max_sessions: u32, reuse: bool, backend: Arc<dyn SessionBackend>, store: ClientStore) -> Self {
        Self { inner: Arc::new(Mutex::new(DispatchInner { role: role.into(), workspace: workspace.into(), command, max_sessions, reuse, backend, store, bridge: Bridge::new(), sessions: HashMap::new(), settled: Vec::new(), outbox: None, accept_new: Arc::new(AtomicBool::new(true)) })) }
    }

    pub fn session_count(&self) -> usize { self.inner.lock().sessions.len() }
    pub fn role(&self) -> String { self.inner.lock().role.clone() }
    pub fn command(&self) -> Vec<String> { self.inner.lock().command.clone() }
    pub fn backend_name(&self, task_id: &str) -> Option<String> { self.inner.lock().sessions.values().find(|slot| slot.task_id.as_deref() == Some(task_id)).map(|slot| slot.session.backend.clone()) }
    /// Bind one adapter transport to the session serving `task_id`.
    ///
    /// The plan keys cross-machine identity on `task_id`, so no backend-specific
    /// reference spelling enters this lookup.
    pub fn bind_adapter(&self, task_id: &str, io: AdapterIo, capabilities: Vec<Capability>) -> Result<()> {
        let mut inner = self.inner.lock();
        let slot = inner.sessions.values_mut().find(|slot| slot.task_id.as_deref() == Some(task_id)).ok_or_else(|| anyhow!("unknown session {task_id}"))?;
        slot.io = Some(io); slot.capabilities = capabilities; Ok(())
    }

    /// Remember the delivery handle for one task.
    pub fn attach_msg_id(&self, task_id: &str, msg_id: &str) {
        let mut inner = self.inner.lock();
        if let Some(slot) = inner.sessions.values_mut().find(|slot| slot.task_id.as_deref() == Some(task_id)) {
            slot.msg_id = Some(msg_id.to_string());
        }
    }

    /// Queue an ack the pull-ack task owes the server.
    pub fn push_settled(&self, ack: AckArgs) {
        self.inner.lock().settled.push(ack);
    }

    /// Take every queued ack.
    pub fn take_settled(&self) -> Vec<AckArgs> {
        std::mem::take(&mut self.inner.lock().settled)
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
        inner.store.prose(&inner.role).ok().flatten().map(|(prose, _)| prose).unwrap_or_default()
    }

    /// Queue an outbound envelope before its first write and answer its op_id.
    pub fn enqueue_outbound(&self, envelope: &Envelope) -> Result<String> {
        envelope.validate().map_err(|error| anyhow!(error.to_string()))?;
        let op_id = envelope.op_id.clone().context("outbound envelope missing op_id")?;
        self.inner.lock().store.enqueue_intent(&op_id, &serde_json::to_value(envelope)?)?;
        Ok(op_id)
    }

    /// The flag the runloop and the dispatcher share.
    pub fn accept_new(&self) -> Arc<AtomicBool> {
        self.inner.lock().accept_new.clone()
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
        self.inner.lock().store.enqueue_intent(&op_id, &serde_json::to_value(op)?)?;
        Ok(op_id)
    }

}

fn family_of(envelope: &Envelope) -> String {
    envelope.causality.as_ref().and_then(|c| c.parent_task.clone()).or_else(|| envelope.task_id().map(str::to_string)).unwrap_or_else(|| envelope.id.clone())
}

fn render_tokens(tokens: &[String], session: &str, task: &str) -> Vec<String> {
    tokens.iter().map(|token| token.replace("{session}", session).replace("{task}", task)).collect()
}

pub fn dispatch(state: &DispatchState, envelope: &Envelope) -> Result<SessionRef> {
    let task_id = envelope.task_id().context("task envelope missing causality.task")?.to_string();
    let family = family_of(envelope);
    let mut inner = state.inner.lock();
    if let Some(slot) = inner.sessions.values_mut().find(|slot| slot.task_id.as_deref() == Some(task_id.as_str())) {
        if slot.payload.is_none() { slot.payload = Some(envelope.clone()); }
        return Ok(slot.session.clone());
    }
    if inner.reuse {
        let reused = inner.sessions.values_mut().find(|slot| slot.task_id.is_none() && slot.family == family).map(|slot| {
            slot.task_id = Some(task_id.clone());
            slot.payload = Some(envelope.clone());
            slot.ready = false;
            let session = SessionRef { task_id: task_id.clone(), ..slot.session.clone() };
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
    if inner.sessions.len() >= inner.max_sessions as usize { return Err(anyhow!("max_sessions reached")); }
    let session_id = task_id.clone();
    let command = render_tokens(&inner.command, &session_id, &task_id);
    let mut env = BTreeMap::new();
    env.insert("ONLYNE_SESSION_ID".into(), session_id.clone());
    env.insert("ONLYNE_TASK_ID".into(), task_id.clone());
    env.insert("ONLYNE_ROLE".into(), inner.role.clone());
    let session = inner.backend.spawn(SpawnSpec { cwd: inner.workspace.clone(), task_id: task_id.clone(), command, env, focus: None, rename: None })?;
    inner.bridge.track_live(session.clone());
    feed_created(&inner.bridge, &inner.store, &task_id)?;
    feed_dispatched(&inner.bridge, &inner.store, &task_id);
    inner.sessions.insert(session_id, SessionSlot { session: session.clone(), family, task_id: Some(task_id), ready: false, capabilities: Vec::new(), io: None, payload: Some(envelope.clone()), msg_id: None });
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
    let ReadyNotice { task_id, session_id, generation, io, capabilities } = notice;
    let (payload, target, version) = {
        let mut inner = state.inner.lock();
        let slot = inner.sessions.values_mut().find(|slot| slot.session.task_id == task_id || slot.session.backend_ref.get("id").and_then(|value| value.as_str()) == Some(session_id.as_str())).ok_or_else(|| anyhow!("unknown session for {task_id}"))?;
        let payload = slot.payload.take().ok_or_else(|| anyhow!("session for {task_id} has no held payload"))?;
        slot.ready = true; slot.io = Some(io.clone()); slot.capabilities = capabilities.clone();
        let verdict = feed_ready(&inner.bridge, &inner.store, &task_id)?;
        let version = note_verdict(&verdict, &task_id).unwrap_or(Version::new(generation, 0));
        (payload, io, version)
    };
    // The ready report reaches the server before the payload reaches the agent.
    send_frame(state, ClientOp::Report(Report::Ready { task_id: task_id.clone(), session_id: session_id.clone(), generation: version.generation, seq: version.seq, cluster_ref: None })).await?;
    sync_session(state, &task_id).await?;
    let text = payload.body.text.clone().unwrap_or_default();
    if capabilities.contains(&Capability::Inject) {
        let assign = AssignArgs { envelope: Box::new(payload), prose: prose.to_string(), task_id, generation, parent: None };
        target.notify(AdapterMsg::Host(HostOp::Assign(assign))).await.map_err(|e| anyhow!(e))?;
    } else {
        target.notify(AdapterMsg::Host(HostOp::ConfigGet(onlyne_proto::ConfigGetArgs { key: format!("stdin:{text}") }))).await.map_err(|e| anyhow!(e))?;
    }
    Ok(())
}

pub async fn on_out(state: &DispatchState, task_id: &str, outcome: Outcome, head: Option<String>) -> Result<()> {
    let verdict = {
        let mut inner = state.inner.lock();
        let verdict = settle(&inner.bridge, &inner.store, task_id, match outcome { Outcome::Done => onlyne_session::Outcome::Done, Outcome::Failed => onlyne_session::Outcome::Failed, Outcome::Cancelled => onlyne_session::Outcome::Cancelled })?;
        inner.store.put_out_head(task_id, head.as_deref().unwrap_or(""))?;
        let msg_id = inner.sessions.values_mut().find(|slot| slot.task_id.as_deref() == Some(task_id)).and_then(|slot| slot.msg_id.take());
        if let Some(msg_id) = msg_id {
            inner.settled.push(AckArgs { msg_id, op_id: None, accepted: true, reason: None });
        }
        verdict
    };
    note_verdict(&verdict, task_id);
    sync_session(state, task_id).await
}

pub fn on_recycled(state: &DispatchState, task_id: &str, reason: &str) -> Result<()> {
    let mut inner = state.inner.lock();
    if let Some((key, slot)) = inner.sessions.iter().find(|(_, slot)| slot.task_id.as_deref() == Some(task_id)).map(|(k,s)|(k.clone(),s.clone())) {
        feed_resource_closed(&inner.bridge, &inner.store, task_id)?;
        inner.backend.close(&slot.session, onlyne_session::CloseReason::Completed, false)?;
        inner.bridge.untrack_live(task_id);
        if inner.reuse { if let Some(s) = inner.sessions.get_mut(&key) { s.task_id = None; s.ready = false; } } else { inner.sessions.remove(&key); }
    }
    if reason.is_empty() { inner.store.note_alert(format!("session recycled {task_id}")); }
    Ok(())
}

pub fn session_alive(state: &DispatchState, task_id: &str) -> bool {
    let inner = state.inner.lock();
    inner.sessions.values().find(|slot| slot.task_id.as_deref() == Some(task_id)).map(|slot| inner.backend.probe(&slot.session).map(|p| p.alive).unwrap_or(false)).unwrap_or(false)
}

pub async fn on_plugin_report(state: &DispatchState, report: Report) -> Result<()> {
    let subject = task_id_of(&report).to_string();
    let touched = match report {
        Report::Ready { task_id, .. } => {
            let verdict = { let inner = state.inner.lock(); feed_ready(&inner.bridge, &inner.store, &task_id)? };
            note_verdict(&verdict, &task_id).is_some()
        }
        Report::Heartbeat { task_id, generation, seq, observed, .. } => {
            let verdict = match serde_json::from_value::<Observation>(observed) {
                Ok(body) => {
                    let inner = state.inner.lock();
                    apply_persist(&inner.bridge, &inner.store, &task_id, &LifecycleEvent::Heartbeat { v: Version::new(generation, seq), body })?
                }
                Err(error) => {
                    tracing::warn!(task = %task_id, error = %error, "heartbeat carries no readable observation; liveness only");
                    Verdict::Ignored(IgnoredReason::NoOp)
                }
            };
            note_verdict(&verdict, &task_id).is_some()
        }
        Report::Complete { task_id, outcome, head, .. } => { on_out(state, &task_id, outcome, head).await?; false }
        Report::Fault { task_id: Some(task_id), kind, reason, .. } => {
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

pub fn missing_capability(capabilities: &[Capability], capability: Capability) -> bool { !capabilities.contains(&capability) }

pub fn plugin_gap(capabilities: &[Capability]) -> Vec<onlyne_adapter::HostGap> { onlyne_adapter::degrade_for(&[Capability::Recycle, Capability::Report, Capability::Inject].iter().copied().filter(|cap| missing_capability(capabilities, *cap)).collect::<Vec<_>>()) }

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
}

impl ClientLink {
    /// Dial, verify the certificate pin, sign the server challenge, then read
    /// the role slice with `hello`.
    pub async fn connect(init: &ClientInit) -> Result<Self, NetError> {
        let keypair = KeyPair::load(&init.key_path)?;
        let settings = ConnSettings { agent: AGENT.to_string(), version: env!("CARGO_PKG_VERSION").to_string(), ..ConnSettings::new(PROTOCOL_VERSION) };
        let handle: ClientConn = dial(&init.server, &keypair, &init.cert_pin, &init.role, settings).await?;
        let hello = HandshakeArgs { protocol: PROTOCOL_VERSION, role: init.role.clone(), key: keypair.public_str(), signature: String::new(), agent: AGENT.to_string(), version: env!("CARGO_PKG_VERSION").to_string(), aggregate: false };
        let body = handle.request(Frame::req(String::new(), ClientOp::Hello(hello)), REQUEST_TIMEOUT).await?;
        if !body.ok {
            let error = body.error.clone().unwrap_or(onlyne_proto::ErrorPayload { code: onlyne_proto::ErrorCode::Internal, message: "hello refused".to_string(), field: None });
            return Err(NetError::Rejected { code: wire_code(error.code), message: error.message });
        }
        let data = body.data().cloned().ok_or(NetError::BadFrame)?;
        let welcome: Welcome = serde_json::from_value(data).map_err(|_| NetError::BadFrame)?;
        Ok(Self { handle, welcome: Arc::new(welcome) })
    }

    /// Role slice the server bound this link to.
    pub fn welcome(&self) -> &Welcome {
        &self.welcome
    }

    /// Send one request frame and answer with its body. A refusal from the
    /// server arrives inside the body, which keeps the retry decision in the
    /// intent machine.
    pub async fn request(&self, op: ClientOp) -> Result<ResBody, NetError> {
        self.handle.request(Frame::req(String::new(), op), REQUEST_TIMEOUT).await
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
    serde_json::to_value(code).ok().and_then(|value| value.as_str().map(str::to_string)).unwrap_or_else(|| "internal".to_string())
}


/// Where lifecycle frames leave the dispatcher.
///
/// `send` completes once the frame is on the wire. The ready report awaits this
/// before the payload reaches the agent, which is the causal order §6 fixes.
pub trait Outbox: Send + Sync {
    fn send(&self, op: ClientOp) -> Pin<Box<dyn Future<Output = Result<(), NetError>> + Send + '_>>;
}

impl Outbox for ClientLink {
    fn send(&self, op: ClientOp) -> Pin<Box<dyn Future<Output = Result<(), NetError>> + Send + '_>> {
        Box::pin(async move { self.request(op).await.map(|_| ()) })
    }
}

/// Deliver one lifecycle frame, and queue it durably when the link is down.
///
/// §6 line 289: a running session reaches its terminal state while the outbound
/// work waits in `client.db` intents for the flusher.
pub async fn send_frame(state: &DispatchState, op: ClientOp) -> Result<()> {
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
    SessionProjection {
        lifecycle: phase(&row.public_lifecycle, Lifecycle::Created),
        agent: phase(&row.agent_state, AgentPhase::Booting),
        delivery: phase(&row.delivery_state, DeliveryPhase::NoIntent),
        resource: phase(&row.resource_state, ResourcePhase::Detached),
        recovery: phase(&row.recovery_substate, RecoveryPhase::NoRecovery),
        outcome: None,
        observed: serde_json::from_str(&row.observed_json).ok(),
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
    let args = SessionSyncArgs { task_id: row.task_id.clone(), session_id: row.task_id.clone(), generation: row.generation.max(0) as u64, seq: row.seq.max(0) as u64, projection: projection_of(&row) };
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