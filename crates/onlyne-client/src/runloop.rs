use anyhow::{Context, Result, anyhow};
use onlyne_frame::{read_frame, write_frame};
use onlyne_net::TlsConn;
use onlyne_proto::{ClientOp, Frame, HandshakeArgs, PullArgs, PullReply, Welcome, PROTOCOL_VERSION};
use onlyne_session::default_backend;
use onlyne_store::ClientStore;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::time::{Duration, sleep};
use crate::accept::AcceptPath;
use crate::dispatch::DispatchState;
use crate::intent::IntentMachine;

#[derive(Debug, Clone)]
pub struct ClientInit {
    pub workspace: PathBuf,
    pub role: String,
    pub server: String,
    pub key: String,
    pub cert_pin: String,
}

impl ClientInit {
    pub fn new(workspace: impl Into<PathBuf>, role: impl Into<String>, server: impl Into<String>, key: impl Into<String>, cert_pin: impl Into<String>) -> Self { Self { workspace: workspace.into(), role: role.into(), server: server.into(), key: key.into(), cert_pin: cert_pin.into() } }
}

type Pending = Arc<Mutex<HashMap<String, oneshot::Sender<onlyne_proto::ResBody>>>>;

#[derive(Clone)]
pub struct RunState {
    pub accept_new: Arc<AtomicBool>,
    pub store: ClientStore,
    pub intents: IntentMachine,
    pub dispatch: DispatchState,
    pub welcome: Arc<Mutex<Option<Welcome>>>,
}

impl RunState {
    pub fn new(init: &ClientInit, store: ClientStore) -> Result<Self> {
        let backend = default_backend()?;
        let dispatch = DispatchState::new(init.role.clone(), init.workspace.clone(), Vec::new(), 1, false, Arc::from(backend), store.clone());
        Ok(Self { accept_new: Arc::new(AtomicBool::new(true)), intents: IntentMachine::new(store.clone(), 3, vec![1_000, 2_000, 4_000]), store, dispatch, welcome: Arc::new(Mutex::new(None)) })
    }
}

pub async fn run_connection<S>(init: ClientInit, stream: S, state: RunState) -> Result<Welcome>
where S: AsyncRead + AsyncWrite + Unpin + Send + 'static {
    let (mut reader, mut writer) = tokio::io::split(stream);
    let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
    let (inbound_tx, mut inbound_rx) = mpsc::channel::<Frame<ClientOp>>(128);
    let pending_reader = pending.clone();
    tokio::spawn(async move { loop { match read_frame::<_, Frame<ClientOp>>(&mut reader).await { Ok(Some(Frame::Res { id, body })) => { if let Some(waiter) = pending_reader.lock().await.remove(&id) { let _ = waiter.send(body); } }, Ok(Some(frame)) => { if inbound_tx.send(frame).await.is_err() { break; } }, Ok(None) | Err(_) => break } } });
    let hello_id = onlyne_proto::new_id();
    let hello = HandshakeArgs { protocol: PROTOCOL_VERSION, role: init.role.clone(), key: init.key.clone(), signature: String::new(), agent: "onlyne-client".into(), version: env!("CARGO_PKG_VERSION").into(), aggregate: false };
    let (hello_tx, hello_rx) = oneshot::channel();
    pending.lock().await.insert(hello_id.clone(), hello_tx);
    write_frame(&mut writer, &Frame::req(hello_id, ClientOp::Hello(hello))).await?;
    let body = hello_rx.await.map_err(|_| anyhow!("hello response channel closed"))?;
    if !body.ok { return Err(anyhow!("server rejected hello: {:?}", body.error)); }
    let welcome: Welcome = serde_json::from_value(body.data.context("hello response missing data")?)?;
    state.store.put_prose(&welcome.role, &welcome.prose, &welcome.spec_hash)?;
    *state.welcome.lock().await = Some(welcome.clone());
    let (out_tx, mut out_rx) = mpsc::channel::<Frame<ClientOp>>(128);
    let writer_task = tokio::spawn(async move { while let Some(frame) = out_rx.recv().await { write_frame(&mut writer, &frame).await?; } Ok::<(), anyhow::Error>(()) });
    let flush_state = state.clone(); let flush_tx = out_tx.clone(); let flush_pending = pending.clone();
    tokio::spawn(async move { let _ = flush_intents(flush_state, flush_tx, flush_pending).await; });
    let pull_state = state.clone(); let pull_tx = out_tx.clone(); let pull_role = init.role.clone();
    tokio::spawn(async move { loop { if !pull_state.accept_new.load(Ordering::SeqCst) { sleep(Duration::from_millis(100)).await; continue; } let id = onlyne_proto::new_id(); if pull_tx.send(Frame::req(id, ClientOp::Pull(PullArgs { role: Some(pull_role.clone()), limit: 32, hold_ms: Some(1_000) }))).await.is_err() { break; } sleep(Duration::from_millis(50)).await; } });
    while let Some(frame) = inbound_rx.recv().await { if matches!(frame, Frame::Bye { .. }) { state.accept_new.store(false, Ordering::SeqCst); break; } }
    state.accept_new.store(false, Ordering::SeqCst); writer_task.abort(); Ok(welcome)
}

async fn flush_intents(state: RunState, tx: mpsc::Sender<Frame<ClientOp>>, pending: Pending) -> Result<()> {
    for row in state.intents.pending()? {
        let envelope = serde_json::from_value(row.env_json.clone())?;
        let id = onlyne_proto::new_id(); let (response_tx, response_rx) = oneshot::channel(); pending.lock().await.insert(id.clone(), response_tx);
        tx.send(Frame::req(id, ClientOp::Send(Box::new(envelope)))).await.map_err(|_| anyhow!("writer closed"))?;
        if let Ok(body) = response_rx.await { let frame = Frame::Res { id: String::new(), body }; let _ = state.intents.attempt(&row, Some(&frame))?; }
    }
    Ok(())
}

pub async fn reconnect_ladder() { for seconds in [1_u64, 2, 4, 8, 16, 32, 60] { sleep(Duration::from_secs(seconds)).await; } }

pub async fn run(init: ClientInit) -> Result<()> {
    let workspace = onlyne_layout::RoleWorkspace::resolve(&init.workspace);
    let store = ClientStore::open(workspace.client_db_path())?;
    let state = RunState::new(&init, store)?;
    let mut conn = TlsConn::connect(&init.server, &init.cert_pin).await.map_err(|e| anyhow!(e))?;
    let hello_id = onlyne_proto::new_id();
    let hello = HandshakeArgs { protocol: PROTOCOL_VERSION, role: init.role, key: init.key, signature: String::new(), agent: "onlyne-client".into(), version: env!("CARGO_PKG_VERSION").into(), aggregate: false };
    conn.send_frame(&Frame::req(hello_id, ClientOp::Hello(hello))).await.map_err(|e| anyhow!(e))?;
    let _: Option<Frame<ClientOp>> = conn.recv_frame().await.map_err(|e| anyhow!(e))?;
    Ok(())
}

pub fn accept_path(state: &RunState) -> Result<AcceptPath> { Ok(AcceptPath::new(state.dispatch.clone(), state.welcome.blocking_lock().as_ref().map(|w| w.prose.clone()).unwrap_or_default())) }
