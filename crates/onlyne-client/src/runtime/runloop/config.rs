use crate::runtime::intent::IntentMachine;
use crate::session::dispatch::DispatchState;
use anyhow::Result;
use onlyne_net::backoff::Backoff;
use onlyne_proto::Welcome;
use onlyne_session::{AcpOptions, ProcessRunner, WorktreePolicy, backend_for_env, process_env};
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
    /// Workspace `config.toml` `backend`. Empty means auto. `ONLYNE_BACKEND`
    /// in the process environment takes precedence when it is nonempty.
    pub backend: String,
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
            stall_report_secs: onlyne_config::DEFAULT_STALL_REPORT_SECS,
            reconnect_grace_secs: onlyne_config::DEFAULT_RECONNECT_GRACE_SECS,
            backend: String::new(),
            acp: onlyne_config::AcpSection::default(),
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
    pub fn with_backend(mut self, backend: impl Into<String>) -> Self {
        self.backend = backend.into();
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
    /// Seconds a dropped plugin connection may stay away before this client
    /// retires the task-free session it left behind. Zero disables the sweep.
    pub reconnect_grace_secs: u64,
}

impl RunState {
    pub fn new(init: &ClientInit, store: ClientStore) -> Result<Self> {
        let requested = std::env::var("ONLYNE_BACKEND")
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| init.backend.clone());
        let backend = backend_for_env(
            &requested,
            &process_env(),
            Arc::new(ProcessRunner),
            WorktreePolicy::from_config(&init.orca_worktree),
            &acp_options(&init.acp),
        )?;
        let dispatch = DispatchState::new(
            init.role.clone(),
            init.workspace.clone(),
            Vec::new(),
            1,
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
            reconnect_grace_secs: init.reconnect_grace_secs,
        })
    }

    /// Adopt the role slice the server sent with `welcome`.
    pub(super) async fn adopt(&self, welcome: &Welcome) {
        self.dispatch
            .reconfigure(crate::session::slice::RoleSlice::from_welcome(welcome));
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
