use crate::backend::{
    AcpOptions, ProcessRunner, Runner, SessionBackend, SessionPlacement, WorktreePolicy,
    backend_for, detect_placement, process_env,
};
use crate::runtime::intent::IntentMachine;
use crate::session::dispatch::DispatchState;
use anyhow::Result;
use onlyne_net::backoff::Backoff;
use onlyne_proto::Welcome;
use onlyne_store::ClientStore;
use std::path::PathBuf;
use std::sync::{Arc, atomic::AtomicBool};
use std::time::Duration;
use tokio::sync::Mutex;

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
    /// Seconds a running session may sit without Applied progress before a stall
    /// fault is reported. Zero disables the report.
    pub stall_report_secs: u64,
    /// Seconds a dropped plugin connection may stay away before this client
    /// retires the task-free session it left behind. Zero disables the sweep.
    pub reconnect_grace_secs: u64,
    /// The placement this run was told to use: `onlyne-client run` passes the
    /// workspace `config.toml`'s `placement` key, and an embedding passes
    /// whatever it resolved — the scenario suite passes the in-process
    /// `fake` runtime. `None` probes herdr, orca, zellij in that order and
    /// falls back to `headless`. `ONLYNE_BACKEND` in the process environment
    /// takes precedence when it names a placement.
    pub placement: Option<SessionPlacement>,
    /// The workspace config's `[acp]` table. Only the ACP session backend reads
    /// it: the mode, model and reasoning effort handed to the agent when a
    /// session opens, and what to answer when the agent asks for permission.
    pub acp: onlyne_config::AcpSection,
    /// The workspace config's `[client.session]` table: which deliveries one
    /// session of this role serves, and how long an idle one may wait before
    /// this client releases its process (plan §10).
    pub session: onlyne_config::SessionPolicy,
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
            stall_report_secs: onlyne_config::DEFAULT_STALL_REPORT_SECS,
            reconnect_grace_secs: onlyne_config::DEFAULT_RECONNECT_GRACE_SECS,
            placement: None,
            acp: onlyne_config::AcpSection::default(),
            session: onlyne_config::SessionPolicy::default(),
        }
    }

    /// Adopt the `[orca] worktree` policy the workspace config carries.
    pub fn with_orca_worktree(mut self, worktree: impl Into<String>) -> Self {
        self.orca_worktree = worktree.into();
        self
    }
    pub fn with_stall_report_secs(mut self, secs: u64) -> Self {
        self.stall_report_secs = secs;
        self
    }
    /// Adopt the `[client] reconnect_grace_secs` value the workspace config carries.
    pub fn with_reconnect_grace_secs(mut self, secs: u64) -> Self {
        self.reconnect_grace_secs = secs;
        self
    }
    /// Adopt the `placement` this run was told to use, if it was told one.
    pub fn with_placement(mut self, placement: Option<SessionPlacement>) -> Self {
        self.placement = placement;
        self
    }
    /// Adopt the `[acp]` table the workspace config carries.
    pub fn with_acp(mut self, acp: onlyne_config::AcpSection) -> Self {
        self.acp = acp;
        self
    }
    /// Adopt the `[client.session]` table the workspace config carries.
    pub fn with_session(mut self, session: onlyne_config::SessionPolicy) -> Self {
        self.session = session;
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
        allow_permissions: acp.permission == "allow",
    }
}

/// What picking a session backend needs from this machine.
///
/// The placement is the machine's half of the pair and is known at startup; the
/// drive is the runtime's half and arrives later, with `welcome`. Keeping the
/// two apart here is the point of the slice: the backend is chosen when both
/// halves are known, instead of by one fused value read out of one file.
#[derive(Clone)]
pub struct BackendSelector {
    pub placement: SessionPlacement,
    pub runner: Arc<dyn Runner>,
    pub worktree: WorktreePolicy,
    pub acp: AcpOptions,
}

impl BackendSelector {
    /// The backend this role's drive and this machine's placement select.
    ///
    /// Refuses a pair the rule does not allow — `acp` anywhere but `headless` —
    /// so a caller never gets a backend that would run the agent where its
    /// channel cannot follow it.
    pub fn build(&self, drive: onlyne_config::Drive) -> Result<Arc<dyn SessionBackend>> {
        backend_for(
            drive,
            self.placement,
            Arc::clone(&self.runner),
            self.worktree.clone(),
            &self.acp,
        )
        .map(Arc::from)
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
    /// Seconds a dropped plugin connection may stay away before this client
    /// retires the task-free session it left behind. Zero disables the sweep.
    pub reconnect_grace_secs: u64,
    /// The machine's half of the runtime pair: the placement a role's drive is
    /// resolved against, and the settings only a backend reads.
    pub selector: BackendSelector,
}

impl RunState {
    pub fn new(init: &ClientInit, store: ClientStore) -> Result<Self> {
        let detected = detect_placement(&process_env(), init.placement)?;
        let selector = BackendSelector {
            placement: detected.placement,
            runner: Arc::new(ProcessRunner),
            worktree: WorktreePolicy::from_config(&init.orca_worktree),
            acp: acp_options(&init.acp),
        };
        tracing::info!(
            placement = %selector.placement,
            source = ?detected.source,
            explicit = ?detected.explicit,
            "placement resolved"
        );
        // The backend a role's drive selects is installed when the drive
        // arrives with `welcome`. Until then the default drive's backend is in
        // place, because a dispatcher always holds one and no session can open
        // before the first `hello`.
        let backend = selector.build(onlyne_config::Drive::Plugin)?;
        let dispatch = DispatchState::new(
            init.role.clone(),
            init.workspace.clone(),
            Vec::new(),
            1,
            Arc::clone(&backend),
            store.clone(),
        )
        .with_session_policy(init.session.clone())
        .with_placement(selector.placement);
        dispatch.set_drive(onlyne_config::Drive::Plugin, None);
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
            reconnect_grace_secs: init.reconnect_grace_secs,
            selector,
        })
    }

    /// Adopt the role slice the server sent with `welcome`.
    pub(super) async fn adopt(&self, welcome: &Welcome) {
        let slice = crate::session::slice::RoleSlice::from_welcome(welcome);
        // The backend a drive selects is installed before the slice lands, so
        // the command that arrives with the slice is never handed to the
        // backend the previous drive left behind.
        self.install_runtime(slice.drive);
        self.dispatch.reconfigure(slice);
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

    /// Install the backend this role's drive selects on this machine.
    ///
    /// Runs on every `welcome` and on every spec reload, and does nothing when
    /// the drive has not moved. Three answers:
    ///
    /// * the backend is installed, and the registration is republished with the
    ///   name it reports;
    /// * the drive moved while this role holds live sessions, so nothing moves:
    ///   their panes, tabs, and children are the installed backend's to close
    ///   and to probe, and the next attempt lands once the role is quiet;
    /// * the drive and this machine's placement cannot be paired at all, which
    ///   is recorded so every delivery meets the sentence instead of a backend
    ///   the previous drive left behind.
    pub(super) fn install_runtime(&self, drive: onlyne_config::Drive) {
        if self.dispatch.drive() == Some(drive) {
            return;
        }
        match self.selector.build(drive) {
            Ok(backend) => {
                if self.dispatch.set_backend(backend) {
                    self.dispatch.set_drive(drive, None);
                    tracing::info!(
                        drive = %drive,
                        placement = %self.selector.placement,
                        backend = %self.dispatch.session_backend(),
                        "session backend selected"
                    );
                    self.republish_registration();
                } else {
                    tracing::warn!(
                        drive = %drive,
                        placement = %self.selector.placement,
                        live = self.dispatch.session_count(),
                        held = %self.dispatch.session_backend(),
                        "the role's drive changed while it holds live sessions: they keep the \
                         backend they were opened under, and the new drive lands once the role \
                         is quiet"
                    );
                }
            }
            Err(error) => {
                tracing::error!(
                    drive = %drive,
                    placement = %self.selector.placement,
                    error = %error,
                    "the drive this role's spec declares cannot run under this machine's \
                     placement; every delivery for this role will be refused with that sentence"
                );
                self.dispatch.set_drive(drive, Some(error.to_string()));
            }
        }
    }

    /// Republish this client's registration.
    ///
    /// The bind wrote one before the role's drive was known, so the `runtime`
    /// field it carries is the default drive's backend. An external runtime's
    /// plugin reads that file to find the clients it serves, and a stale name
    /// there is the same class of fact this slice exists to stop trusting.
    fn republish_registration(&self) {
        if let Err(error) = crate::session::adapter_socket::republish_registration(
            &self.dispatch.workspace(),
            &self.dispatch.role(),
            self.dispatch.session_backend(),
            self.dispatch.placement_name(),
        ) {
            tracing::warn!(error = %error, "the client registration was not republished");
        }
    }

    /// Durable event cursor for the next `subscribe`. Zero asks the server for
    /// its current head.
    pub(super) fn cursor(&self) -> u64 {
        self.store
            .config(EVENT_CURSOR_KEY)
            .ok()
            .flatten()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0)
    }

    pub(super) fn set_cursor(&self, seq: u64) {
        if let Err(error) = self.store.put_config(EVENT_CURSOR_KEY, &seq.to_string()) {
            tracing::warn!(error = %error, "event cursor was not stored");
        }
    }
}
