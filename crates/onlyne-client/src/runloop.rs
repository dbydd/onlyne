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
    AckArgs, ClientOp, Delivery, EventTier, Frame, LedgerEntry, LedgerQuery, LedgerState, MsgKind,
    PullArgs, PullReply, QueryRolesArgs, RoleInfo, Subscribe, Welcome,
};
use onlyne_session::{
    AcpOptions, ProcessRunner, SessionOutcome, WorktreePolicy, backend_for_env, process_env,
};
use onlyne_store::ClientStore;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
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
/// Bound on the session sweep the SIGTERM/SIGINT handler runs, short enough
/// that an operator's own grace period still sees the process leave.
pub const SHUTDOWN_CLOSE_BUDGET: Duration = Duration::from_secs(8);
/// Key holding the durable event cursor in `config_cache`.
pub const EVENT_CURSOR_KEY: &str = "event_seq";
/// Retry delay for a request that the transport answered `NotReady`.
pub const NOT_READY_PAUSE_MS: u64 = 200;
/// Poll cadence for terminal facts emitted by a self-driven session backend.
pub const OUTCOME_POLL_MS: u64 = 100;

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
    /// Seconds a residual working row may age before this client reports it dead.
    pub stale_grace_secs: u64,
    /// Seconds a running session may sit without Applied progress before a stall
    /// fault is reported. Zero disables the report.
    pub stall_report_secs: u64,
    /// Workspace `config.toml` `backend`. Empty means auto. `ONLYNE_BACKEND`
    /// in the process environment takes precedence when it is nonempty.
    pub backend: String,
    /// Open a journal viewer pane beside each supported session.
    pub tui: bool,
    /// The workspace config's `[acp]` table. Only the ACP session backend reads
    /// it: the mode, model and reasoning effort handed to the agent when a
    /// session opens, and what to answer when the agent asks for permission.
    pub acp: onlyne_config::AcpSection,
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
            stale_grace_secs: onlyne_config::DEFAULT_STALE_GRACE_SECS,
            stall_report_secs: onlyne_config::DEFAULT_STALL_REPORT_SECS,
            backend: String::new(),
            tui: false,
            acp: onlyne_config::AcpSection::default(),
        }
    }

    /// Adopt the `[orca] worktree` policy the workspace config carries.
    pub fn with_orca_worktree(mut self, worktree: impl Into<String>) -> Self {
        self.orca_worktree = worktree.into();
        self
    }
    pub fn with_stale_grace_secs(mut self, secs: u64) -> Self {
        self.stale_grace_secs = secs;
        self
    }
    pub fn with_stall_report_secs(mut self, secs: u64) -> Self {
        self.stall_report_secs = secs;
        self
    }
    pub fn with_backend(mut self, backend: impl Into<String>) -> Self {
        self.backend = backend.into();
        self
    }
    pub fn with_tui(mut self, tui: bool) -> Self {
        self.tui = tui;
        self
    }
    /// Adopt the `[acp]` table the workspace config carries.
    pub fn with_acp(mut self, acp: onlyne_config::AcpSection) -> Self {
        self.acp = acp;
        self
    }
}

/// The `[acp]` table in the shape a session backend can read without a config
/// dependency. The only decision made here is the one the backend acts on: the
/// permission word, already validated by the config loader, becomes whether this
/// client grants an agent's request.
pub fn acp_options(acp: &onlyne_config::AcpSection) -> AcpOptions {
    AcpOptions {
        mode: acp.mode.clone(),
        model: acp.model.clone(),
        reasoning_effort: acp.reasoning_effort.clone(),
        tui: false,
        allow_permissions: acp.permission == "allow",
    }
}

#[derive(Clone)]
pub struct RunState {
    pub accept_new: Arc<AtomicBool>,
    pub store: ClientStore,
    pub intents: Arc<parking_lot::Mutex<IntentMachine>>,
    pub dispatch: DispatchState,
    pub welcome: Arc<Mutex<Option<Welcome>>>,
    pub stall_report_secs: u64,
}

impl RunState {
    pub fn new(init: &ClientInit, store: ClientStore) -> Result<Self> {
        let requested = std::env::var("ONLYNE_BACKEND")
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| init.backend.clone());
        let mut acp = acp_options(&init.acp);
        acp.tui = init.tui;
        let backend = backend_for_env(
            &requested,
            &process_env(),
            Arc::new(ProcessRunner),
            WorktreePolicy::from_config(&init.orca_worktree),
            &acp,
        )?;
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
            stall_report_secs: init.stall_report_secs,
        })
    }

    /// Adopt the role slice the server sent with `welcome`.
    async fn adopt(&self, welcome: &Welcome) {
        self.dispatch
            .reconfigure(crate::slice::RoleSlice::from_welcome(welcome));
        // The topology name is the address the host backends group sessions
        // under, so it is recorded with the rest of what the server says about
        // this role. `welcome.cluster` is the server's own `[server] name`.
        self.dispatch.set_topology(&welcome.cluster);
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

/// Run the role runtime until a permanent handshake failure or a dead local
/// surface stops it.
///
/// The acceptor task owns the workspace socket bind, and its failure ends this
/// run the same way a permanent link failure does: a client that keeps a TLS
/// link while its socket is unbound reads as connected from the server while
/// every verb the workspace issues fails, so the exit status and the message on
/// stderr are the operator's only notice.
pub async fn run(init: ClientInit) -> Result<()> {
    let workspace = RoleWorkspace::resolve(&init.workspace);
    let store = ClientStore::open(workspace.client_db_path())?;
    let state = RunState::new(&init, store)?;
    let mut acceptor = tokio::spawn(acceptor(init.clone(), state.clone()));
    let closing = tokio::spawn(close_on_signal(state.dispatch.clone()));
    let mut outcomes = tokio::spawn(outcome_loop(state.clone()));
    let outcome = tokio::select! {
        link = link_loop(&init, &state) => link,
        served = &mut acceptor => match served {
            Ok(Ok(())) => Err(anyhow!("the adapter surface stopped serving")),
            Ok(Err(error)) => Err(error),
            Err(error) => Err(anyhow!("adapter acceptor task ended: {error}")),
        },
        pumped = &mut outcomes => match pumped {
            Ok(Ok(())) => Err(anyhow!("the session outcome pump stopped")),
            Ok(Err(error)) => Err(error),
            Err(error) => Err(anyhow!("session outcome pump task ended: {error}")),
        },
    };
    acceptor.abort();
    closing.abort();
    outcomes.abort();
    outcome
}

/// Drain terminal facts emitted by a backend that owns its agent.
///
/// The backend queue is synchronous and destructive. Each fact is moved out
/// before this task awaits the ordinary settlement path, so neither its queue
/// lock nor the dispatch lock can survive into session teardown.
async fn outcome_loop(state: RunState) -> Result<()> {
    let Some(feed) = state.dispatch.outcome_feed() else {
        return std::future::pending::<Result<()>>().await;
    };
    loop {
        while let Some(outcome) = feed.try_recv() {
            settle_session_outcome(&state, outcome).await?;
        }
        sleep(Duration::from_millis(OUTCOME_POLL_MS)).await;
    }
}

/// Feed one self-driven ending through the same fault and settlement paths an
/// adapter report uses.
async fn settle_session_outcome(state: &RunState, outcome: SessionOutcome) -> Result<()> {
    let SessionOutcome {
        task_id,
        outcome,
        head,
        note,
        refusals,
    } = outcome;
    let terminal = match outcome {
        onlyne_session::Outcome::Done => onlyne_proto::Outcome::Done,
        onlyne_session::Outcome::Failed => onlyne_proto::Outcome::Failed,
        onlyne_session::Outcome::Cancelled => onlyne_proto::Outcome::Cancelled,
        onlyne_session::Outcome::Pending => {
            return Err(anyhow!(
                "self-driven backend reported a non-terminal outcome for task {task_id}"
            ));
        }
    };
    if let Some(reason) = refusals.as_deref() {
        onlyne_session::record_fault(&state.store, &task_id, "permission", "acp", reason)?;
    }
    if outcome == onlyne_session::Outcome::Failed
        && let Some(reason) = note.as_deref()
    {
        onlyne_session::record_fault(&state.store, &task_id, "acp", "acp", reason)?;
    }
    dispatch::on_out(&state.dispatch, &task_id, terminal, head).await
}

/// Keep the server link up until a permanent failure ends the run.
///
/// Every pass from `Reconnecting` back to `Ready` runs the order the plan fixes
/// for a reconnect inside [`run_link`], and the ladder caps at the last rung so
/// a server that stays down costs one dial per minute.
async fn link_loop(init: &ClientInit, state: &RunState) -> Result<()> {
    let mut backoff = reconnect_backoff();
    let accept_new = state.dispatch.accept_new();
    loop {
        match ClientLink::connect(init, state.dispatch.hello_live_tasks()).await {
            Ok(link) => {
                backoff.reset();
                state.dispatch.attach_outbox(Arc::new(link.clone()));
                state.dispatch.set_link_up(true);
                match run_link(init, &link, state).await {
                    Ok(()) => tracing::info!(role = %init.role, "server link ended"),
                    Err(error) => tracing::warn!(error = %error, "server link failed"),
                }
                state.dispatch.detach_outbox();
                state.dispatch.set_link_up(false);
                accept_new.store(false, Ordering::SeqCst);
                if let Some(failure) = link.failure().await {
                    if is_permanent(&failure) {
                        return Err(anyhow!("{failure}"));
                    }
                }
            }
            Err(error) if is_permanent(&error) => return Err(anyhow!("{error}")),
            Err(error) => tracing::warn!(error = %error, "connect failed"),
        }
        let delay = backoff.next();
        tracing::info!(seconds = delay.as_secs(), "reconnecting");
        sleep(delay).await;
    }
}

/// Close live sessions when the operator stops the client.
///
/// `SIGTERM` ends the foreground client, and the default disposition would
/// kill the process with every tab it opened still running: the resources
/// would outlive the only thing that can address them. Each session closes with
/// [`onlyne_session::CloseReason::Shutdown`] first, so the backend record and
/// the plugin-facing tab map end truthfully.
#[cfg(unix)]
async fn close_on_signal(dispatch: DispatchState) {
    use tokio::signal::unix::{SignalKind, signal};
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

/// Close live sessions when the operator stops the client.
///
/// Windows has no SIGTERM; operators use `onlyne shutdown` for a graceful
/// daemon stop. Ctrl-C is the console interrupt, and it runs the same
/// close_all budget the unix SIGINT path uses.
#[cfg(windows)]
async fn close_on_signal(dispatch: DispatchState) {
    // `ctrl_c()` installs synchronously and hands back the watch stream; the
    // await belongs on `recv`, which yields once per console interrupt.
    let mut interrupt = match tokio::signal::windows::ctrl_c() {
        Ok(stream) => stream,
        Err(error) => {
            tracing::warn!(error = %error, "Ctrl-C handler was not installed");
            return;
        }
    };
    interrupt.recv().await;
    tracing::info!("Ctrl-C: closing live sessions");
    dispatch::close_all(
        &dispatch,
        onlyne_session::CloseReason::Shutdown,
        SHUTDOWN_CLOSE_BUDGET,
    );
    std::process::exit(0);
}

/// Bind the adapter socket and serve it for the life of the process.
///
/// The bind is the first act, and its error leaves this task: a socket that
/// never opened is a workspace whose plugins cannot mount, and only the exit
/// status says so. Once the listener exists the surface stays up — a failed
/// `accept` is logged and retried by [`AdapterSocket::accept_loop`] — so this
/// task ends the run exactly when the local surface could not start.
async fn acceptor(init: ClientInit, state: RunState) -> Result<()> {
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
    let (listener, _endpoint) = socket.bind().await?;
    socket.accept_loop(listener).await
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
    // The residual sweep can wait out `stale_grace_secs` for a plugin mount, so
    // it sits behind the four loops: a restarted role pulls, flushes, and reads
    // events from the moment its link is up. Its reports are advisory, so it
    // runs detached and logs its own failure (field report: a client spawned at
    // 13:46:51 logged `server link ready` at 13:51:51).
    let sweep_init = init.clone();
    let sweep_link = link.clone();
    let sweep_state = state.clone();
    tokio::spawn(async move {
        if let Err(error) = reconcile_residuals(&sweep_init, &sweep_link, &sweep_state).await {
            tracing::warn!(error = %error, "residual reconcile ended with an error");
        }
    });
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
        state.dispatch.reclaim_exited_resources();
        scan_stalls(&state).await;
        match link.readiness() {
            ConnReadiness::Ready => {
                if !ready {
                    ready = true;
                    // The link redials on its own, so its fresh connection needs
                    // the routed `hello` before any queued frame reaches it.
                    link.authenticate(state.dispatch.hello_live_tasks()).await?;
                    state.accept_new.store(true, Ordering::SeqCst);
                    state.dispatch.set_link_up(true);
                    flush_intents(&link, &state).await;
                    subscribe(&link, state.cursor()).await?;
                    tracing::info!("server link restored; intents flushed");
                }
            }
            ConnReadiness::Reconnecting => {
                if ready {
                    ready = false;
                    state.accept_new.store(false, Ordering::SeqCst);
                    state.dispatch.set_link_up(false);
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
        // A role at `max_sessions` stops asking for work it has nowhere to run
        // (plan §5), and the same pause must not stop it hearing the command that
        // frees a slot: a full role is exactly the one whose operator wants to
        // `recycle` or `focus`. Control rows still travel on the ordinary pull
        // when the role has capacity.
        let control_only = !state.dispatch.has_capacity();
        let reply = match link
            .request(ClientOp::Pull(PullArgs {
                role: Some(init.role.clone()),
                limit: PULL_LIMIT,
                hold_ms: Some(PULL_HOLD_MS),
                control_only: control_only.then_some(true),
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
    // A control command acts on the work the role already holds, so it answers
    // before the capacity gate and before the `accept_new` gate: a role at
    // `max_sessions` is exactly the role whose operator wants to free.
    if delivery.envelope.kind == onlyne_proto::MsgKind::Control {
        settle_control(state, delivery).await;
        return;
    }
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

/// Apply one delivered control command and settle its row.
///
/// The row settles whether or not this role still holds the task it names. A
/// command whose session already ended has nothing left to act on, and leaving
/// the row in flight would report an operator's `control` as undelivered.
async fn settle_control(state: &RunState, delivery: &Delivery) {
    let ack = |accepted: bool, reason: Option<String>| AckArgs {
        msg_id: delivery.msg_id.clone(),
        op_id: None,
        accepted,
        reason,
    };
    let Some(op) = delivery.envelope.control.clone() else {
        tracing::warn!(msg_id = %delivery.msg_id, "control delivery carried no command");
        state.dispatch.push_settled(ack(
            false,
            Some("control delivery carried no control op".to_string()),
        ));
        return;
    };
    match dispatch::on_control(&state.dispatch, &op).await {
        Ok(held) => {
            tracing::info!(
                op = op.name(),
                task = %op.task_id(),
                held,
                "control command applied"
            );
            state.dispatch.push_settled(ack(true, None));
        }
        Err(error) => {
            tracing::warn!(error = %error, op = op.name(), "control command refused");
            state
                .dispatch
                .push_settled(ack(false, Some(error.to_string())));
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
                        let spec_reloaded =
                            matches!(event.as_ref(), onlyne_proto::Event::SpecReloaded(_));
                        let body = serde_json::to_value(&event)?;
                        let kind = body
                            .get("type")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("event");
                        state.store.append_event(kind, &body)?;
                        state.set_cursor(seq);
                        if spec_reloaded {
                            refresh_role_slice(&link, &state).await?;
                        }
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

async fn reconcile_residuals(init: &ClientInit, link: &ClientLink, state: &RunState) -> Result<()> {
    let reply = link
        .request(ClientOp::QueryLedger(LedgerQuery {
            role: Some(init.role.clone()),
            state: Some(LedgerState::Acked),
            kind: Some(MsgKind::Task),
            limit: 500,
            ..LedgerQuery::default()
        }))
        .await?;
    if !reply.ok {
        tracing::warn!(error = ?reply.error, "residual reconcile ledger query refused");
        return Ok(());
    }
    let entries: Vec<LedgerEntry> = reply
        .data
        .as_ref()
        .and_then(|value| value.get("ledger"))
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();
    let mut rows: Vec<_> = entries
        .iter()
        .filter_map(crate::stale::WorkingRow::from_entry)
        .collect();
    if rows.is_empty() {
        return Ok(());
    }
    let pending_ops = pending_intent_ops(state)?;
    rows.retain(|row| !crate::stale::pending_terminal_for(&row.task_id, &pending_ops));
    if rows.is_empty() {
        return Ok(());
    }
    wait_for_mount_or_grace(state, init.stale_grace_secs).await;
    let convergences = crate::stale::reconcile(
        &rows,
        &state.dispatch.live_task_ids(),
        chrono::Utc::now(),
        init.stale_grace_secs,
        &init.role,
    );
    for convergence in convergences {
        dispatch::send_frame(&state.dispatch, ClientOp::Report(convergence.report())).await?;
    }
    Ok(())
}

fn pending_intent_ops(state: &RunState) -> Result<Vec<ClientOp>> {
    let rows = state.intents.lock().pending()?;
    rows.iter().map(op_for_intent).collect()
}

/// Report running sessions whose Applied clock has exceeded the stall
/// threshold. The fault is observation-only; the ledger row stays as stored.
async fn scan_stalls(state: &RunState) {
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

async fn wait_for_mount_or_grace(state: &RunState, grace_secs: u64) {
    if state.dispatch.has_mounted_adapter() || grace_secs == 0 {
        return;
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(grace_secs);
    while std::time::Instant::now() < deadline {
        if state.dispatch.has_mounted_adapter() {
            return;
        }
        sleep(Duration::from_millis(100)).await;
    }
}

async fn refresh_role_slice(link: &ClientLink, state: &RunState) -> Result<()> {
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

fn apply_role_info(state: &RunState, info: &RoleInfo) -> Vec<&'static str> {
    let current = state.dispatch.role_slice();
    let next = crate::slice::RoleSlice::from_role_info(info, &current);
    let Some((applied, fields)) = crate::slice::apply_if_changed(&current, next) else {
        return Vec::new();
    };
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
mod tests {
    use super::*;
    use onlyne_net::NetError;
    use onlyne_proto::{
        Body, Causality, Outcome, Presence, Principal, Report, ResBody, new_envelope, new_task_id,
    };
    use onlyne_session::backend::fake::FakeBackend;
    use onlyne_session::{AcpBackend, SessionLedger, VersionedSession};
    use std::future::Future;
    use std::pin::Pin;
    use tempfile::tempdir;

    fn test_state(max_sessions: u32, reuse: bool, command: Vec<String>) -> (RunState, ClientStore) {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("client.db");
        let store = ClientStore::open(path).expect("client store");
        let dispatch = DispatchState::new(
            "planner",
            dir.path(),
            command,
            max_sessions,
            reuse,
            Arc::new(FakeBackend::new()),
            store.clone(),
        );
        let intents = IntentMachine::new(
            store.clone(),
            DEFAULT_INTENT_ATTEMPTS,
            default_intent_backoff(),
        );
        let state = RunState {
            accept_new: dispatch.accept_new(),
            store: store.clone(),
            intents: Arc::new(parking_lot::Mutex::new(intents)),
            dispatch,
            welcome: Arc::new(Mutex::new(None)),
            stall_report_secs: 1,
        };
        (state, store)
    }

    /// Real ACP v1 peer used by the client-level delivery test below. The ready
    /// marker is written by the test outbox when the Ready report leaves; the
    /// child checks it at the instant it receives the prompt, making the causal
    /// order observable across the process boundary.
    const CLIENT_ACP_FAKE: &str = r##"import json, os, sys

TRACE = sys.argv[1]
READY = sys.argv[2]


def trace(line):
    with open(TRACE, "a", encoding="utf-8") as fh:
        fh.write(line + "\n")
        fh.flush()


def send(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


def result(rid, value):
    send({"jsonrpc": "2.0", "id": rid, "result": value})


def failure(rid, code, message):
    send({"jsonrpc": "2.0", "id": rid,
          "error": {"code": code, "message": message}})


def read_message():
    line = sys.stdin.readline()
    if not line:
        return None
    return json.loads(line)


def wait_for(rid):
    while True:
        message = read_message()
        if message is None:
            return None
        if message.get("id") == rid and ("result" in message or "error" in message):
            return message


trace("start pid %d" % os.getpid())
while True:
    message = read_message()
    if message is None:
        trace("eof")
        break
    method = message.get("method")
    rid = message.get("id")
    params = message.get("params") or {}
    if method == "initialize":
        result(rid, {"protocolVersion": 1,
                     "agentInfo": {"name": "onlyne-client-test", "version": "0"},
                     "authMethods": [],
                     "agentCapabilities": {"sessionCapabilities": {"close": {}}}})
    elif method == "session/new":
        result(rid, {"sessionId": "client-e2e-session",
                     "modes": {"currentModeId": "default"},
                     "models": {"currentModelId": "fast"},
                     "configOptions": []})
    elif method == "session/prompt":
        prompt = "".join(block.get("text", "") for block in params.get("prompt") or [])
        session = params.get("sessionId")
        trace("prompt " + prompt)
        trace("ready-before-prompt %s" % os.path.exists(READY))
        send({"jsonrpc": "2.0", "id": "permission-1",
              "method": "session/request_permission",
              "params": {"sessionId": session,
                         "toolCall": {"toolCallId": "call-1", "title": "Edit file",
                                      "kind": "edit", "status": "pending"},
                         "options": [{"optionId": "once", "kind": "allow_once",
                                      "name": "Allow once"},
                                     {"optionId": "no", "kind": "reject_once",
                                      "name": "Reject once"}]}})
        reply = wait_for("permission-1") or {}
        chosen = ((reply.get("result") or {}).get("outcome") or {}).get("optionId", "none")
        trace("permission " + chosen)
        send({"jsonrpc": "2.0", "method": "session/update",
              "params": {"sessionId": session,
                         "sessionUpdate": "agent_message_chunk",
                         "content": {"type": "text", "text": "permission denied\n"}}})
        result(rid, {"stopReason": "refusal"})
    elif method == "session/close":
        trace("close " + str(params.get("sessionId")))
        result(rid, {})
    elif method == "session/cancel":
        trace("cancel")
    elif rid is not None:
        failure(rid, -32601, "unsupported " + str(method))
"##;

    #[derive(Clone)]
    struct ReadyMarkerOutbox {
        marker: PathBuf,
        frames: Arc<parking_lot::Mutex<Vec<ClientOp>>>,
    }

    impl crate::dispatch::Outbox for ReadyMarkerOutbox {
        fn send(
            &self,
            op: ClientOp,
        ) -> Pin<Box<dyn Future<Output = Result<(), NetError>> + Send + '_>> {
            let marker = self.marker.clone();
            let frames = Arc::clone(&self.frames);
            Box::pin(async move {
                if matches!(&op, ClientOp::Report(Report::Ready { .. })) {
                    std::fs::write(marker, b"ready").expect("write the ready marker");
                }
                frames.lock().push(op);
                Ok(())
            })
        }

        fn request(
            &self,
            _op: ClientOp,
        ) -> Pin<Box<dyn Future<Output = Result<ResBody, NetError>> + Send + '_>> {
            Box::pin(async { Ok(ResBody::ok(serde_json::Value::Null)) })
        }
    }

    fn role_info(max_sessions: u32, reuse: bool, command: Vec<String>) -> RoleInfo {
        RoleInfo {
            name: "planner".into(),
            admin: false,
            max_sessions,
            reuse,
            session_command: command,
            spec_hash: "hash".into(),
            prose: None,
            state: Presence::Online,
            sessions: 0,
            detail: None,
            edges: Vec::new(),
            aggregate: None,
            relay_required: None,
            relay_count: None,
        }
    }

    /// A workspace socket that cannot be bound ends the run with an error.
    ///
    /// The silent 0.5s restart this replaces kept a process alive that held a
    /// server link while the local surface stayed shut, so `onlyne` verbs from the
    /// workspace failed and the server still counted the role connected. The
    /// message is the bind context naming the canonical spelling, and the cause
    /// carries the served path with each length.
    #[tokio::test]
    async fn an_unbindable_socket_ends_the_run_with_an_error() {
        let dir = tempdir().unwrap();
        let workspace = RoleWorkspace::resolve(dir.path());
        workspace.bootstrap().unwrap();
        #[cfg(unix)]
        std::fs::write(
            workspace.run_dir().join("socket"),
            "/nonexistent-dir-onlyne-for-this-test/sock\n",
        )
        .unwrap();
        // Windows resolves the natural path regardless of the Unix endpoint
        // marker. Hold the production NPFS listener instead: a second bind to
        // that live name is the platform's EADDRINUSE equivalent.
        #[cfg(windows)]
        let (_held_listener, _endpoint) =
            onlyne_layout::bind_socket(workspace.root(), &workspace.run_dir()).unwrap();
        let init = ClientInit::new(
            dir.path(),
            "planner",
            "127.0.0.1:1",
            workspace.key_path(),
            "sha256/0000000000000000000000000000000000000000000000000000000000000000",
        )
        .with_backend("fake");
        let outcome = tokio::time::timeout(Duration::from_secs(10), run(init))
            .await
            .expect("the bind failure ends the run well inside the timeout");
        let error = outcome.expect_err("an unbindable socket is an error");
        assert!(
            error.to_string().contains("bind the workspace socket"),
            "{error}"
        );
    }

    /// A real ACP child takes a pulled task without an adapter mount, observes
    /// the payload only after Ready left, and reports its refusal through the
    /// ordinary client fault, settlement, head, and delivery-ack paths.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_acp_delivery_reaches_the_agent_and_settles_through_the_client() {
        let dir = tempdir().expect("ACP client workspace");
        let workspace = RoleWorkspace::resolve(dir.path());
        workspace.bootstrap().expect("bootstrap workspace");
        let script = dir.path().join("onlyne_client_acp_fake_agent.py");
        let trace_path = dir.path().join("agent.trace");
        let ready_marker = dir.path().join("ready.reported");
        std::fs::write(&script, CLIENT_ACP_FAKE).expect("write ACP fake agent");

        let store = ClientStore::open(workspace.client_db_path()).expect("client store");
        store
            .put_prose("planner", "Act as the planner.", "spec-hash")
            .expect("cache role prose");
        let backend = Arc::new(AcpBackend::new(AcpOptions::default()));
        let dispatch = DispatchState::new(
            "planner",
            dir.path(),
            vec![
                "python3".into(),
                "-u".into(),
                script.to_string_lossy().into_owned(),
                trace_path.to_string_lossy().into_owned(),
                ready_marker.to_string_lossy().into_owned(),
            ],
            1,
            false,
            backend,
            store.clone(),
        );
        let frames = Arc::new(parking_lot::Mutex::new(Vec::new()));
        dispatch.attach_outbox(Arc::new(ReadyMarkerOutbox {
            marker: ready_marker,
            frames: Arc::clone(&frames),
        }));
        let state = RunState {
            accept_new: dispatch.accept_new(),
            store: store.clone(),
            intents: Arc::new(parking_lot::Mutex::new(IntentMachine::new(
                store.clone(),
                DEFAULT_INTENT_ATTEMPTS,
                default_intent_backoff(),
            ))),
            dispatch,
            welcome: Arc::new(Mutex::new(None)),
            stall_report_secs: 0,
        };
        let pump = tokio::spawn(outcome_loop(state.clone()));

        let task_id = new_task_id();
        let delivery = Delivery {
            msg_id: "msg-acp-client-e2e".into(),
            envelope: Box::new(
                new_envelope(
                    MsgKind::Task,
                    Principal::role("sender"),
                    Principal::role("planner"),
                    Body::text("repair the failing widget"),
                    Some(Causality::root(task_id.clone())),
                )
                .expect("task envelope"),
            ),
        };
        accept_delivery(&state, &delivery).await;

        let settled = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if let Some(row) = store.get_session(&task_id).expect("read session")
                    && row.public_lifecycle == "exited"
                {
                    break row;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        let row = match settled {
            Ok(row) => row,
            Err(error) => {
                crate::dispatch::close_all(
                    &state.dispatch,
                    onlyne_session::CloseReason::Shutdown,
                    Duration::from_secs(1),
                );
                pump.abort();
                panic!(
                    "ACP task did not settle: {error}; trace={:?}",
                    std::fs::read_to_string(&trace_path)
                );
            }
        };

        // `on_out` removes the slot and asks the ACP backend to close. Wait for
        // the child to observe EOF before asserting, so even a failed assertion
        // below cannot leave the fake agent behind.
        let trace = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let trace = std::fs::read_to_string(&trace_path).unwrap_or_default();
                if trace.contains("eof") {
                    break trace;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the ACP fake agent exits after settlement");
        pump.abort();
        let _ = pump.await;

        assert!(
            trace.contains("prompt repair the failing widget"),
            "the task payload crossed the real ACP pipe: {trace}"
        );
        assert!(
            trace.contains("ready-before-prompt True"),
            "Ready must leave before the agent sees the payload: {trace}"
        );
        assert!(trace.contains("permission no"), "{trace}");
        assert!(trace.contains("close client-e2e-session"), "{trace}");
        assert!(!state.dispatch.has_mounted_adapter());
        assert_eq!(state.dispatch.session_count(), 0);

        let observed: serde_json::Value =
            serde_json::from_str(&row.observed_json).expect("stored observation JSON");
        assert_eq!(observed["outcome"], "failed");
        assert_eq!(
            store
                .out_head(&task_id)
                .expect("read completion head")
                .as_deref(),
            Some("permission denied")
        );
        let faults = store.list_faults(&task_id).expect("read ACP faults");
        assert_eq!(
            faults
                .iter()
                .map(|fault| fault.kind.as_str())
                .collect::<Vec<_>>(),
            vec!["permission", "acp"],
            "{faults:?}"
        );
        assert!(faults[0].reason.contains("permission ask(s) refused"));
        assert!(faults[1].reason.contains("agent stopped the turn: refusal"));

        assert!(
            frames.lock().iter().any(|op| {
                matches!(
                    op,
                    ClientOp::Report(Report::Ready { task_id: ready, .. })
                        if ready == &task_id
                )
            }),
            "the existing Ready report path was used"
        );
        let intents = store
            .due_intents(chrono::Utc::now() + chrono::Duration::seconds(1), 100)
            .expect("read durable intents");
        assert!(
            intents.iter().any(|row| {
                matches!(
                    serde_json::from_value::<ClientOp>(row.env_json.clone()),
                    Ok(ClientOp::Ack(ack)) if ack.msg_id == "msg-acp-client-e2e" && ack.accepted
                )
            }),
            "the delivery ack was queued through the existing settlement path: {intents:?}"
        );
    }

    #[test]
    fn spec_reloaded_role_slice_change_updates_dispatch_gate() {
        let (state, _store) = test_state(1, false, vec!["old".into()]);
        let changed = apply_role_info(&state, &role_info(2, true, vec!["new".into()]));
        assert_eq!(changed, vec!["session_command", "max_sessions", "reuse"]);
        let applied = state.dispatch.role_slice();
        assert_eq!(applied.max_sessions, 2);
        assert!(applied.reuse);
        assert_eq!(applied.command, vec!["new"]);
    }

    #[test]
    fn spec_reloaded_identical_role_slice_is_noop() {
        let (state, _store) = test_state(2, true, vec!["pi".into()]);
        let changed = apply_role_info(&state, &role_info(2, true, vec!["pi".into()]));
        assert!(changed.is_empty());
        assert_eq!(state.dispatch.role_slice().max_sessions, 2);
    }

    /// A reload that arms or disarms the guard has to reach a live connection,
    /// which never sees a second `welcome`: the role row is the only carrier,
    /// and the next spawn reads the policy off the dispatcher.
    #[test]
    fn a_relay_policy_from_the_role_row_is_adopted() {
        let (state, _store) = test_state(2, true, vec!["pi".into()]);
        let mut armed = role_info(2, true, vec!["pi".into()]);
        armed.relay_required = Some(vec!["writer".into()]);
        armed.relay_count = Some(2);
        let changed = apply_role_info(&state, &armed);
        assert_eq!(changed, vec!["relay_required", "relay_count"]);
        let applied = state.dispatch.role_slice();
        assert_eq!(applied.relay_required, vec!["writer".to_string()]);
        assert_eq!(applied.relay_count, Some(2));

        let disarmed = role_info(2, true, vec!["pi".into()]);
        let changed = apply_role_info(&state, &disarmed);
        assert_eq!(changed, vec!["relay_required", "relay_count"]);
        let applied = state.dispatch.role_slice();
        assert!(applied.relay_required.is_empty());
        assert_eq!(applied.relay_count, None);
    }

    #[tokio::test]
    async fn startup_residual_report_uses_durable_report_path() {
        let (state, store) = test_state(1, false, Vec::new());
        let convergence = crate::stale::Convergence {
            task_id: "task-dead".into(),
        };
        dispatch::send_frame(&state.dispatch, ClientOp::Report(convergence.report()))
            .await
            .expect("report queues without a link");
        let rows = store.flush_order().expect("pending intents");
        let ops = rows
            .iter()
            .map(op_for_intent)
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert!(ops.iter().any(|op| matches!(
            op,
            ClientOp::Report(Report::Complete {
                task_id,
                outcome: Outcome::Failed,
                head: Some(reason),
                ..
            }) if task_id == "task-dead" && reason == crate::stale::SESSION_DEAD
        )));
    }

    #[tokio::test]
    async fn stall_scan_queues_one_fault_until_applied_resets() {
        let (state, store) = test_state(1, false, Vec::new());
        let past = Instant::now()
            .checked_sub(Duration::from_secs(5))
            .expect("clock");
        state.dispatch.note_stall_assigned("task-frozen", past);
        scan_stalls(&state).await;
        let ops = store
            .flush_order()
            .expect("pending intents")
            .iter()
            .map(op_for_intent)
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let stalled = ops
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    ClientOp::Report(Report::Fault { task_id: Some(task), kind, .. })
                        if task == "task-frozen" && kind == crate::stall::STALLED
                )
            })
            .count();
        assert_eq!(stalled, 1, "one freeze episode reports once: {ops:?}");

        scan_stalls(&state).await;
        let ops = store
            .flush_order()
            .expect("pending intents")
            .iter()
            .map(op_for_intent)
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let stalled = ops
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    ClientOp::Report(Report::Fault { kind, .. }) if kind == crate::stall::STALLED
                )
            })
            .count();
        assert_eq!(stalled, 1, "the freeze is not re-reported: {ops:?}");

        state
            .dispatch
            .note_stall_applied("task-frozen", Instant::now());
        let past = Instant::now()
            .checked_sub(Duration::from_secs(5))
            .expect("clock");
        state.dispatch.note_stall_assigned("task-frozen", past);
        // Applied cleared the episode bit; an already-elapsed clock reports again.
        state.dispatch.note_stall_applied("task-frozen", past);
        scan_stalls(&state).await;
        let ops = store
            .flush_order()
            .expect("pending intents")
            .iter()
            .map(op_for_intent)
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let stalled = ops
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    ClientOp::Report(Report::Fault { kind, .. }) if kind == crate::stall::STALLED
                )
            })
            .count();
        assert_eq!(stalled, 2, "Applied starts a new freeze episode: {ops:?}");
    }

    #[tokio::test]
    async fn exited_session_clock_is_forgotten_without_a_fault_frame() {
        let (state, store) = test_state(1, false, Vec::new());
        let task_id = "task-finished";
        store
            .upsert_session(
                task_id,
                &VersionedSession {
                    agent_state: "idle".into(),
                    delivery_state: "accepted".into(),
                    resource_state: "attached".into(),
                    public_lifecycle: "exited".into(),
                    recovery_substate: "draining".into(),
                    desired_json: "{}".into(),
                    observed_json: serde_json::json!({"outcome": "done"}).to_string(),
                    generation: 1,
                    seq: 3,
                    backend_ref: "{}".into(),
                    mismatch_count: 0,
                    updated_at: 0,
                },
            )
            .unwrap();
        let past = Instant::now()
            .checked_sub(Duration::from_secs(5))
            .expect("clock");
        state.dispatch.note_stall_assigned(task_id, past);

        scan_stalls(&state).await;

        assert!(
            store.flush_order().expect("pending intents").is_empty(),
            "an exited task emits no stalled fault"
        );
        state.dispatch.note_stall_applied(task_id, past);
        assert!(
            state.dispatch.stall_due(Instant::now(), 1).is_empty(),
            "the exited task leaves the progress clock"
        );
    }

    #[tokio::test]
    async fn stall_scan_stays_quiet_when_disabled() {
        let (mut state, store) = test_state(1, false, Vec::new());
        state.stall_report_secs = 0;
        let past = Instant::now()
            .checked_sub(Duration::from_secs(5))
            .expect("clock");
        state.dispatch.note_stall_assigned("task-frozen", past);
        scan_stalls(&state).await;
        assert!(
            store.flush_order().expect("pending intents").is_empty(),
            "zero disables stall reports"
        );
    }
}
