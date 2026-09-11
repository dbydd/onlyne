//! Foreground role runtime: one authenticated server link, four tasks, and the
//! adapter socket that keeps serving plugins across reconnects.
//!
//! The transport handles death and redialing behind one handle, so this loop
//! follows its readiness. Every pass from `Reconnecting` back to `Ready` runs
//! the order the plan fixes for a reconnect: welcome, intent flush, event
//! resume, pull.

use crate::accept::AcceptPath;
use crate::adapter_socket::AdapterSocket;
use crate::dispatch::{self, ClientLink, DispatchState};
use crate::intent::{IntentMachine, op_for_intent};
use anyhow::{Result, anyhow};
use onlyne_layout::RoleWorkspace;
use onlyne_net::backoff::Backoff;
use onlyne_net::conn::ConnReadiness;
use onlyne_net::is_permanent;
use onlyne_proto::{
    AckArgs, ClientOp, Delivery, EventTier, Frame, PullArgs, PullReply, Subscribe, Welcome,
};
use onlyne_session::{WorktreePolicy, default_backend};
use onlyne_store::ClientStore;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::Mutex;
use tokio::time::sleep;

/// Attempt ceiling used until `welcome` carries the role's own value.
pub const DEFAULT_INTENT_ATTEMPTS: u32 = 3;
/// Reconnect ladder in seconds (§6: 1/2/4/8/16/32/60).
pub const RECONNECT_LADDER_SECONDS: [u64; 7] = [1, 2, 4, 8, 16, 32, 60];
/// Pause between pull attempts that returned nothing.
pub const PULL_PAUSE_MS: u64 = 200;
/// Pause between intent flush passes.
pub const FLUSH_PAUSE_MS: u64 = 200;
/// Long-poll window the client asks the server for.
pub const PULL_HOLD_MS: u64 = 1_000;
/// Deliveries drained per pull.
pub const PULL_LIMIT: u32 = 32;
/// Poll interval for the readiness watcher.
pub const READINESS_POLL_MS: u64 = 250;
/// Bound on the session sweep the SIGTERM/SIGINT handler runs, under the
/// 10-second wait `onlyne-client stop` allows the process to leave.
pub const SHUTDOWN_CLOSE_BUDGET: Duration = Duration::from_secs(8);
/// Key holding the durable event cursor in `config_cache`.
pub const EVENT_CURSOR_KEY: &str = "event_seq";
/// Retry delay for a request that the transport answered `NotReady`.
pub const NOT_READY_PAUSE_MS: u64 = 200;

/// Ladder used until `welcome` carries the role's own values.
pub fn default_intent_backoff() -> Vec<u64> {
    vec![1_000, 2_000, 4_000]
}

/// Reconnect ladder as durations, capped at the last rung.
pub fn reconnect_backoff() -> Backoff {
    Backoff::with_limits(
        Duration::from_secs(RECONNECT_LADDER_SECONDS[0]),
        Duration::from_secs(RECONNECT_LADDER_SECONDS[6]),
    )
}

#[derive(Debug, Clone)]
pub struct ClientInit {
    pub workspace: PathBuf,
    pub role: String,
    pub server: String,
    pub key_path: PathBuf,
    pub cert_pin: String,
    /// The workspace config's `[orca] worktree` value: `host`, `inherit`, or a
    /// literal Orca worktree selector. Only an Orca session backend reads it.
    pub orca_worktree: String,
}

impl ClientInit {
    pub fn new(
        workspace: impl Into<PathBuf>,
        role: impl Into<String>,
        server: impl Into<String>,
        key_path: impl Into<PathBuf>,
        cert_pin: impl Into<String>,
    ) -> Self {
        Self {
            workspace: workspace.into(),
            role: role.into(),
            server: server.into(),
            key_path: key_path.into(),
            cert_pin: cert_pin.into(),
            orca_worktree: "host".to_string(),
        }
    }

    /// Adopt the `[orca] worktree` policy the workspace config carries.
    pub fn with_orca_worktree(mut self, worktree: impl Into<String>) -> Self {
        self.orca_worktree = worktree.into();
        self
    }
}

#[derive(Clone)]
pub struct RunState {
    pub accept_new: Arc<AtomicBool>,
    pub store: ClientStore,
    pub intents: Arc<parking_lot::Mutex<IntentMachine>>,
    pub dispatch: DispatchState,
    pub welcome: Arc<Mutex<Option<Welcome>>>,
}

impl RunState {
    pub fn new(init: &ClientInit, store: ClientStore) -> Result<Self> {
        let backend = default_backend(WorktreePolicy::from_config(&init.orca_worktree))?;
        let dispatch = DispatchState::new(
            init.role.clone(),
            init.workspace.clone(),
            Vec::new(),
            1,
            false,
            Arc::from(backend),
            store.clone(),
        );
        let intents = IntentMachine::new(
            store.clone(),
            DEFAULT_INTENT_ATTEMPTS,
            default_intent_backoff(),
        );
        let accept_new = dispatch.accept_new();
        Ok(Self {
            accept_new,
            store,
            intents: Arc::new(parking_lot::Mutex::new(intents)),
            dispatch,
            welcome: Arc::new(Mutex::new(None)),
        })
    }

    /// Adopt the role slice the server sent with `welcome`.
    async fn adopt(&self, welcome: &Welcome) {
        self.dispatch.reconfigure(
            welcome.session_command.clone().unwrap_or_default(),
            welcome.max_sessions,
            welcome.reuse,
        );
        {
            let mut intents = self.intents.lock();
            if let Some(attempts) = welcome.intent_attempts {
                intents.attempts = attempts;
            }
            if let Some(ladder) = welcome
                .intent_backoff_ms
                .as_ref()
                .filter(|ladder| !ladder.is_empty())
            {
                intents.backoff_ms = ladder.clone();
            }
        }
        *self.welcome.lock().await = Some(welcome.clone());
    }

    /// Durable event cursor for the next `subscribe`. Zero asks the server for
    /// its current head.
    fn cursor(&self) -> u64 {
        self.store
            .config(EVENT_CURSOR_KEY)
            .ok()
            .flatten()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0)
    }

    fn set_cursor(&self, seq: u64) {
        if let Err(error) = self.store.put_config(EVENT_CURSOR_KEY, &seq.to_string()) {
            tracing::warn!(error = %error, "event cursor was not stored");
        }
    }
}

/// Run the role runtime until a permanent handshake failure stops it.
pub async fn run(init: ClientInit) -> Result<()> {
    let workspace = RoleWorkspace::resolve(&init.workspace);
    let store = ClientStore::open(workspace.client_db_path())?;
    let state = RunState::new(&init, store)?;
    let acceptor = tokio::spawn(acceptor(init.clone(), state.clone()));
    let closing = tokio::spawn(close_on_signal(state.dispatch.clone()));
    let mut backoff = reconnect_backoff();
    let accept_new = state.dispatch.accept_new();
    let outcome = loop {
        match ClientLink::connect(&init).await {
            Ok(link) => {
                backoff.reset();
                state.dispatch.attach_outbox(Arc::new(link.clone()));
                match run_link(&init, &link, &state).await {
                    Ok(()) => tracing::info!(role = %init.role, "server link ended"),
                    Err(error) => tracing::warn!(error = %error, "server link failed"),
                }
                state.dispatch.detach_outbox();
                accept_new.store(false, Ordering::SeqCst);
                if let Some(failure) = link.failure().await {
                    if is_permanent(&failure) {
                        break Err(anyhow!("{failure}"));
                    }
                }
            }
            Err(error) if is_permanent(&error) => break Err(anyhow!("{error}")),
            Err(error) => tracing::warn!(error = %error, "connect failed"),
        }
        let delay = backoff.next();
        tracing::info!(seconds = delay.as_secs(), "reconnecting");
        sleep(delay).await;
    };
    acceptor.abort();
    closing.abort();
    outcome
}

/// Close live sessions when the operator stops the client.
///
/// `onlyne-client stop` sends SIGTERM, and the default disposition would kill
/// the process with every tab it opened still running: the resources would
/// outlive the only thing that can address them. Each session closes with
/// [`onlyne_session::CloseReason::Shutdown`] first, so the backend record and
/// the plugin-facing tab map end truthfully; the exit code stays 0, which is
/// what `stop` reads as a clean stop.
async fn close_on_signal(dispatch: DispatchState) {
    let mut terminate = match signal(SignalKind::terminate()) {
        Ok(stream) => stream,
        Err(error) => {
            tracing::warn!(error = %error, "SIGTERM handler was not installed");
            return;
        }
    };
    let mut interrupt = match signal(SignalKind::interrupt()) {
        Ok(stream) => stream,
        Err(error) => {
            tracing::warn!(error = %error, "SIGINT handler was not installed");
            return;
        }
    };
    tokio::select! {
        _ = terminate.recv() => tracing::info!("SIGTERM: closing live sessions"),
        _ = interrupt.recv() => tracing::info!("SIGINT: closing live sessions"),
    }
    dispatch::close_all(
        &dispatch,
        onlyne_session::CloseReason::Shutdown,
        SHUTDOWN_CLOSE_BUDGET,
    );
    std::process::exit(0);
}

/// Serve the adapter socket for the life of the process.
async fn acceptor(init: ClientInit, state: RunState) -> Result<()> {
    loop {
        let workspace = RoleWorkspace::resolve(&init.workspace);
        let cluster = state
            .store
            .config("cluster")
            .ok()
            .flatten()
            .unwrap_or_default();
        let socket = AdapterSocket {
            workspace: workspace.root().to_path_buf(),
            role: init.role.clone(),
            cluster,
            server: init.server.clone(),
            dispatch: state.dispatch.clone(),
        };
        match socket.serve().await {
            Ok(()) => return Ok(()),
            Err(error) => tracing::warn!(error = %error, "adapter socket restarting"),
        }
        sleep(Duration::from_millis(500)).await;
    }
}

/// Drive one live link: welcome, flush, resume, then the four tasks.
async fn run_link(init: &ClientInit, link: &ClientLink, state: &RunState) -> Result<()> {
    let welcome = link.welcome().clone();
    state
        .store
        .put_prose(&welcome.role, &welcome.prose, &welcome.spec_hash)?;
    state.store.put_config("role", &welcome.role)?;
    state.store.put_config("cluster", &welcome.cluster)?;
    state.adopt(&welcome).await;
    // §5 line 248: the aggregate name is a property of the spec entry the
    // server bound this link to, so it can only come from the welcome. A plain
    // role receives `None` and keeps sending reports without the key.
    state
        .dispatch
        .set_cluster_ref(welcome.aggregate.clone().unwrap_or_default());
    flush_intents(link, state).await;
    subscribe(link, state.cursor()).await?;
    state.accept_new.store(true, Ordering::SeqCst);
    let mut pull = tokio::spawn(pull_ack_loop(init.clone(), link.clone(), state.clone()));
    let mut flusher = tokio::spawn(flush_loop(link.clone(), state.clone()));
    let mut reader = tokio::spawn(read_events(link.clone(), state.clone()));
    let mut watcher = tokio::spawn(watch_readiness(link.clone(), state.clone()));
    tracing::info!(role = %init.role, "server link ready");
    tokio::select! {
        _ = &mut pull => {}
        _ = &mut flusher => {}
        _ = &mut reader => {}
        _ = &mut watcher => {}
    }
    pull.abort();
    flusher.abort();
    reader.abort();
    watcher.abort();
    Ok(())
}

/// Follow the connection state and re-run the post-reconnect order.
async fn watch_readiness(link: ClientLink, state: RunState) -> Result<()> {
    let mut ready = true;
    loop {
        sleep(Duration::from_millis(READINESS_POLL_MS)).await;
        match link.readiness() {
            ConnReadiness::Ready => {
                if !ready {
                    ready = true;
                    // The link redials on its own, so its fresh connection needs
                    // the routed `hello` before any queued frame reaches it.
                    link.authenticate().await?;
                    state.accept_new.store(true, Ordering::SeqCst);
                    flush_intents(&link, &state).await;
                    subscribe(&link, state.cursor()).await?;
                    tracing::info!("server link restored; intents flushed");
                }
            }
            ConnReadiness::Reconnecting => {
                if ready {
                    ready = false;
                    state.accept_new.store(false, Ordering::SeqCst);
                    tracing::warn!("server link lost; sessions settle and intents keep queuing");
                }
            }
            ConnReadiness::Closed => return Ok(()),
        }
    }
}

/// The pull-ack task: drain what the server queued, then settle what the local
/// side finished.
async fn pull_ack_loop(init: ClientInit, link: ClientLink, state: RunState) -> Result<()> {
    loop {
        if !state.accept_new.load(Ordering::SeqCst) {
            sleep(Duration::from_millis(PULL_PAUSE_MS)).await;
            continue;
        }
        if !state.dispatch.has_capacity() {
            // A full role stops asking for work, so the server keeps the next
            // row in `queued` and offers it when a session frees (plan §5
            // `max_sessions`). Pulling anyway would leave a row in flight with
            // nowhere to run.
            sleep(Duration::from_millis(PULL_PAUSE_MS)).await;
            continue;
        }
        let reply = match link
            .request(ClientOp::Pull(PullArgs {
                role: Some(init.role.clone()),
                limit: PULL_LIMIT,
                hold_ms: Some(PULL_HOLD_MS),
            }))
            .await
        {
            Ok(reply) => reply,
            Err(error) if transient(&error) => {
                sleep(Duration::from_millis(NOT_READY_PAUSE_MS)).await;
                continue;
            }
            Err(error) => return Err(anyhow!(error)),
        };
        if !reply.ok {
            tracing::warn!(error = ?reply.error, "pull refused");
            sleep(Duration::from_millis(PULL_PAUSE_MS)).await;
            continue;
        }
        let Some(data) = reply.data else { continue };
        let pulled: PullReply = serde_json::from_value(data)?;
        for delivery in pulled.deliveries {
            accept_delivery(&state, &delivery).await;
        }
        state.set_cursor(pulled.seq);
        sleep(Duration::from_millis(PULL_PAUSE_MS)).await;
    }
}

/// Whether the transport answered from a fresh link or a live one.
fn transient(error: &onlyne_net::NetError) -> bool {
    matches!(
        error,
        onlyne_net::NetError::NotReady
            | onlyne_net::NetError::Disconnected(_)
            | onlyne_net::NetError::RequestTimeout
    )
}

/// One delivery becomes a session, or an immediate refusal ack.
///
/// A plugin mounted before any work existed is parked in the dispatcher, so the
/// session staged here hands straight over to it. That is the order an
/// always-running agent takes: it attaches first and receives its assignment
/// when a task arrives (plan §6 line 285).
async fn accept_delivery(state: &RunState, delivery: &Delivery) {
    if !state.dispatch.has_capacity() {
        // The row stays in flight on the server, which offers it again when a
        // session frees (plan §5 `max_sessions`).
        tracing::debug!(msg_id = %delivery.msg_id, "delivery waits for a free session");
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
        Ok(None) => state.dispatch.push_settled(AckArgs {
            msg_id: delivery.msg_id.clone(),
            op_id: None,
            accepted: false,
            reason: Some("client is not accepting new work".to_string()),
        }),
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

/// The flusher task: push the durable intent queue at the server.
async fn flush_loop(link: ClientLink, state: RunState) -> Result<()> {
    loop {
        // The link redials behind the runtime's back, and a frame sent into that
        // fresh connection is refused until its `hello` lands, so the queue waits
        // for a ready link rather than spending a round trip on the refusal.
        if link.readiness() == ConnReadiness::Ready {
            flush_intents(&link, &state).await;
        }
        sleep(Duration::from_millis(FLUSH_PAUSE_MS)).await;
    }
}

/// Send each pending intent once and record the answer.
async fn flush_intents(link: &ClientLink, state: &RunState) {
    let rows = match state.intents.lock().pending() {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(error = %error, "intent queue unreadable");
            return;
        }
    };
    for row in rows {
        let op = match op_for_intent(&row) {
            Ok(op) => op,
            Err(error) => {
                tracing::warn!(error = %error, op_id = %row.op_id, "intent payload unreadable");
                continue;
            }
        };
        // §5 line 248: a supervisor's report names its cluster on the durable
        // path too, so the flusher stamps the same rule `send_frame` applies.
        let op = match op {
            ClientOp::Report(report) => {
                ClientOp::Report(crate::dispatch::with_cluster(&state.dispatch, report))
            }
            other => other,
        };
        match link.request(op).await {
            Ok(body) => {
                let machine = state.intents.lock();
                match machine.attempt(&row, Some(&body)) {
                    Ok(crate::intent::IntentResult::Dropped(code, reason)) => {
                        // Dropping is terminal for the row, so the reason stays in
                        // the log rather than only in the deleted row (plan §6).
                        tracing::warn!(
                            op_id = %row.op_id,
                            ?code,
                            reason = %reason,
                            "intent dropped by a permanent answer"
                        );
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(error = %error, op_id = %row.op_id, "intent answer not recorded");
                    }
                }
            }
            Err(error) => {
                // The link is down rather than the server refusing, so this row
                // waits for the reconnect with its retry budget intact.
                let machine = state.intents.lock();
                if let Err(record) = machine.defer(&row, "connection unavailable") {
                    tracing::warn!(error = %record, op_id = %row.op_id, "intent deferral not recorded");
                }
                tracing::warn!(error = %error, op_id = %row.op_id, "intent send failed");
                return;
            }
        }
    }
}

/// Start, or resume, the observation stream.
async fn subscribe(link: &ClientLink, since_seq: u64) -> Result<()> {
    let request = Subscribe {
        since_seq,
        tiers: vec![EventTier::Durable, EventTier::Advisory],
        kinds: Vec::new(),
        roles: Vec::new(),
    };
    let reply = link.request(ClientOp::Subscribe(request)).await?;
    if !reply.ok {
        return Err(anyhow!("subscribe refused: {:?}", reply.error));
    }
    Ok(())
}

/// The reader task: mirror the server event stream locally and resync on loss.
async fn read_events(link: ClientLink, state: RunState) -> Result<()> {
    let mut events = link.events();
    loop {
        match events.recv().await {
            Ok(frame) => {
                if let Some(count) = onlyne_net::resync_lag_of(&frame) {
                    tracing::warn!(count, "event queue overflowed; resuming from the cursor");
                    subscribe(&link, state.cursor()).await?;
                    continue;
                }
                match frame {
                    Frame::Ev { seq, event } => {
                        let body = serde_json::to_value(&event)?;
                        let kind = body
                            .get("type")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("event");
                        state.store.append_event(kind, &body)?;
                        state.set_cursor(seq);
                    }
                    Frame::Pong { server_seq, .. } => {
                        state.set_cursor(server_seq.max(state.cursor()))
                    }
                    _ => {}
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::warn!(skipped, "event reader lagged; resuming from the cursor");
                subscribe(&link, state.cursor()).await?;
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
        }
    }
}

/// The local accept path for the current role slice.
pub fn accept_path(state: &RunState) -> Result<AcceptPath> {
    Ok(AcceptPath::new(
        state.dispatch.clone(),
        state.dispatch.role_prose(),
    ))
}
