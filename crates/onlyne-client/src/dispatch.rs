use anyhow::{Context, Result, anyhow};
use onlyne_adapter::AdapterIo;
use onlyne_proto::{AdapterMsg, AssignArgs, Capability, Envelope, HostOp, Outcome, Report};
use onlyne_session::{Bridge, SessionBackend, SessionRef, SpawnSpec, feed_created, feed_dispatched, feed_ready, feed_resource_closed, feed_turn_started, settle};
use onlyne_store::ClientStore;
use parking_lot::Mutex;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

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
}

#[derive(Clone)]
pub struct SessionSlot {
    pub session: SessionRef,
    pub family: String,
    pub task_id: Option<String>,
    pub ready: bool,
    pub capabilities: Vec<Capability>,
    pub io: Option<AdapterIo>,
}

impl DispatchState {
    pub fn new(role: impl Into<String>, workspace: impl Into<PathBuf>, command: Vec<String>, max_sessions: u32, reuse: bool, backend: Arc<dyn SessionBackend>, store: ClientStore) -> Self {
        Self { inner: Arc::new(Mutex::new(DispatchInner { role: role.into(), workspace: workspace.into(), command, max_sessions, reuse, backend, store, bridge: Bridge::new(), sessions: HashMap::new() })) }
    }

    pub fn session_count(&self) -> usize { self.inner.lock().sessions.len() }
    pub fn bind_adapter(&self, session_id: &str, io: AdapterIo, capabilities: Vec<Capability>) -> Result<()> {
        let mut inner = self.inner.lock();
        let slot = inner.sessions.values_mut().find(|slot| slot.session.backend_ref.get("id").and_then(|v| v.as_str()) == Some(session_id) || slot.session.task_id == session_id).ok_or_else(|| anyhow!("unknown session {session_id}"))?;
        slot.io = Some(io); slot.capabilities = capabilities; Ok(())
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
    if inner.reuse {
        if let Some(slot) = inner.sessions.values_mut().find(|slot| slot.task_id.is_none() && slot.family == family) {
            slot.task_id = Some(task_id.clone());
            return Ok(slot.session.clone());
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
    inner.sessions.insert(session_id, SessionSlot { session: session.clone(), family, task_id: Some(task_id), ready: false, capabilities: Vec::new(), io: None });
    Ok(session)
}

pub async fn on_ready(state: &DispatchState, task_id: &str, session_id: &str, generation: u64, io: AdapterIo, capabilities: Vec<Capability>, envelope: &Envelope, prose: &str) -> Result<()> {
    let assign = AssignArgs { envelope: Box::new(envelope.clone()), prose: prose.to_string(), task_id: task_id.to_string(), generation, parent: None };
    let target = {
        let mut inner = state.inner.lock();
        let slot = inner.sessions.values_mut().find(|slot| slot.session.task_id == task_id || slot.session.backend_ref.get("id").and_then(|v| v.as_str()) == Some(session_id)).ok_or_else(|| anyhow!("unknown session for {task_id}"))?;
        slot.ready = true; slot.io = Some(io.clone()); slot.capabilities = capabilities.clone();
        feed_ready(&inner.bridge, &inner.store, task_id)?;
        io
    };
    if capabilities.contains(&Capability::Inject) {
        target.notify(AdapterMsg::Host(HostOp::Assign(assign))).await.map_err(|e| anyhow!(e))?;
    } else {
        let text = envelope.body.text.clone().unwrap_or_default();
        target.notify(AdapterMsg::Host(HostOp::ConfigGet(onlyne_proto::ConfigGetArgs { key: format!("stdin:{text}") }))).await.map_err(|e| anyhow!(e))?;
    }
    Ok(())
}

pub fn on_out(state: &DispatchState, task_id: &str, outcome: Outcome, head: Option<String>) -> Result<()> {
    let inner = state.inner.lock();
    settle(&inner.bridge, &inner.store, task_id, match outcome { Outcome::Done => onlyne_session::Outcome::Done, Outcome::Failed => onlyne_session::Outcome::Failed, Outcome::Cancelled => onlyne_session::Outcome::Cancelled })?;
    inner.store.put_out_head(task_id, head.as_deref().unwrap_or(""))?;
    Ok(())
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
    match report {
        Report::Ready { task_id, .. } => { let inner = state.inner.lock(); feed_ready(&inner.bridge, &inner.store, &task_id)?; }
        Report::Heartbeat { task_id, .. } => { let inner = state.inner.lock(); feed_turn_started(&inner.bridge, &inner.store, &task_id)?; }
        Report::Complete { task_id, outcome, head, .. } => { on_out(state, &task_id, outcome, head)?; }
        Report::Fault { task_id: Some(task_id), kind, reason, .. } => { let inner = state.inner.lock(); onlyne_session::record_fault(&inner.store, &task_id, &kind, "plugin", &reason)?; }
        Report::Fault { task_id: None, .. } => {}
    }
    Ok(())
}

pub fn missing_capability(capabilities: &[Capability], capability: Capability) -> bool { !capabilities.contains(&capability) }

pub fn plugin_gap(capabilities: &[Capability]) -> Vec<onlyne_adapter::HostGap> { onlyne_adapter::degrade_for(&[Capability::Recycle, Capability::Report, Capability::Inject].iter().copied().filter(|cap| missing_capability(capabilities, *cap)).collect::<Vec<_>>()) }
