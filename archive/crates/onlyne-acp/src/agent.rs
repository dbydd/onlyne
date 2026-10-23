//! The agent handle: one ACP-speaking child process, its threads, and the typed
//! methods over them.
//!
//! Process discipline follows `onlyne-session`'s exec backend, because that is the
//! shape this workspace already trusts for a session command:
//!
//! * the child gets **its own process group**, so a signal aimed at the client's
//!   group — an operator's terminal, a supervisor's `kill` — does not reach an
//!   agent behind it. Only [`Agent::shutdown`] ends an agent.
//! * shutdown is **graceful before it is lethal**: end of stdin, then `SIGTERM`
//!   to the group, then `SIGKILL`, reaping either way, and no signal is ever
//!   aimed at a process this crate has not confirmed is still unreaped.
//! * **stdout is protocol frames only.** A second thread drains stderr to
//!   `tracing` and keeps a short tail for the exit report, so an agent that writes
//!   its whole log to stderr cannot fill the pipe and block itself while we wait
//!   for its answer.
//! * stdin is a pipe this process holds open, never `/dev/null`: an RPC-mode agent
//!   reads end-of-stdin as "the client left" and exits, which is exactly the
//!   graceful half of shutdown and exactly what must not happen at startup.
//!
//! The `Child` handle belongs to the reader thread and to nobody else; every other
//! thread reaches the process by pid. That is what keeps `wait` single-caller,
//! `Event::Exited` single-shot, and the pid-vs-zombie question honest.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

use crate::rpc::{self, Conn};
use crate::types::{
    AgentCapabilities, ClientCapabilities, ClientInfo, ContentBlock, McpServer, PermissionOutcome,
    PromptOutcome, SessionStart,
};
use crate::wire::RequestId;

/// The ACP protocol version this client speaks. Refuse anything else at the door.
pub const PROTOCOL_VERSION: i64 = 1;

/// How long the agent gets to leave on its own after stdin closes.
const EOF_GRACE: Duration = Duration::from_millis(500);
/// How long `SIGTERM` gets.
const TERMINATE_GRACE: Duration = Duration::from_secs(5);
/// How long `SIGKILL` gets to be observed by the reaper.
const KILL_GRACE: Duration = Duration::from_secs(5);
/// Poll interval while waiting for the reaper thread to report.
const REAP_POLL: Duration = Duration::from_millis(20);
/// How long a thread gets to notice the process is gone before shutdown gives up
/// on it. A thread past this is blocked on a pipe some descendant still holds.
const JOIN_GRACE: Duration = Duration::from_secs(2);

/// How to start an agent. The default has no command, which [`Agent::start`]
/// refuses: there is nothing to run until a caller names one.
#[derive(Debug, Clone, Default)]
pub struct AgentOptions {
    /// The command, argv style: `["qoderclicn", "--acp"]`. The first entry is the
    /// program; nothing is run through a shell.
    pub command: Vec<String>,
    /// Working directory for the agent, which is also where a relative tool path
    /// resolves. `None` inherits this process's directory.
    pub cwd: Option<PathBuf>,
    /// Extra environment for the agent, layered on top of the inherited one: an
    /// agent needs `PATH` to find the tools it shells out for.
    pub env: BTreeMap<String, String>,
}

impl AgentOptions {
    /// A command and nothing else.
    pub fn new(command: Vec<String>) -> Self {
        AgentOptions {
            command,
            ..AgentOptions::default()
        }
    }
}

/// A running ACP agent process plus the sessions negotiated on it.
///
/// One handle, one process, many sessions: `session/new` is per task and the ids
/// it returns are what every later call is keyed by. Cloning is deliberately not
/// offered — the handle owns a process. Call [`Agent::shutdown`] to end it; if a
/// panic drops the handle instead, `Drop` runs the same teardown and logs what it
/// finds, so a client bug cannot leave an orphaned agent behind.
///
/// Call `subscribe()` **before** the first `prompt()`: `session/request_permission`
/// is a request, and the caller has to be listening to answer it.
#[derive(Debug)]
pub struct Agent {
    inner: Arc<AgentInner>,
}

struct AgentInner {
    conn: Arc<Conn>,
    pid: u32,
    label: String,
    threads: Mutex<Vec<JoinHandle<()>>>,
    torn_down: AtomicBool,
}

impl fmt::Debug for AgentInner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Agent")
            .field("label", &self.label)
            .field("pid", &self.pid)
            .field("gone", &self.conn.exited())
            .field("initialized", &self.conn.initialized.get().is_some())
            .field("outstanding", &self.conn.outstanding().len())
            .finish()
    }
}

impl Agent {
    /// Start the agent process and its three helper threads. Nothing is sent yet:
    /// the protocol handshake is [`Agent::initialize`], and an agent that never
    /// gets one still has to be shut down like any other child.
    pub fn start(options: AgentOptions) -> Result<Agent> {
        let Some((program, args)) = options.command.split_first() else {
            bail!("acp: AgentOptions::command is empty; there is nothing to run");
        };
        let label = options.command.join(" ");
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .envs(&options.env);
        if let Some(cwd) = &options.cwd {
            command.current_dir(cwd);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NEW_PROCESS_GROUP: CTRL_BREAK reaches this group, CTRL_C does not.
            command.creation_flags(0x0000_0200);
        }
        let mut child = command
            .spawn()
            .map_err(|error| anyhow!("acp: spawn {label}: {error}"))?;
        let pid = child.id();
        let (stdin, stdout, stderr) =
            match (child.stdin.take(), child.stdout.take(), child.stderr.take()) {
                (Some(stdin), Some(stdout), Some(stderr)) => (stdin, stdout, stderr),
                _ => {
                    // A pipe this crate asked for and did not get is a child it cannot
                    // speak to; leave nothing behind on that path.
                    let _ = child.kill();
                    let _ = child.wait();
                    bail!("acp: {label} did not get the three pipes its protocol needs");
                }
            };

        let (sender, inbox) = mpsc::channel::<String>();
        let conn = Conn::new(sender);
        // The child moves into the reader thread, which becomes its only owner.
        // The option exists so a thread that could not be started can still reap it.
        let handoff = Arc::new(Mutex::new(Some(child)));
        let mut threads: Vec<JoinHandle<()>> = Vec::new();

        let writer_conn = Arc::clone(&conn);
        if let Err(error) = spawn_into(&mut threads, "writer", move || {
            rpc::write_loop(stdin, inbox, writer_conn)
        }) {
            abandon(&handoff, &label);
            return Err(error);
        }
        let stderr_conn = Arc::clone(&conn);
        // The reader waits a bounded moment on this before it reports the exit, so
        // the last stderr lines are in the report instead of racing it.
        let (stderr_drained_tx, stderr_drained_rx) = mpsc::channel::<()>();
        if let Err(error) = spawn_into(&mut threads, "stderr", move || {
            rpc::drain_stderr(stderr, stderr_conn);
            let _ = stderr_drained_tx.send(());
        }) {
            abandon(&handoff, &label);
            return Err(error);
        }
        let reader_conn = Arc::clone(&conn);
        let reader_handoff = Arc::clone(&handoff);
        let reader_label = label.clone();
        if let Err(error) = spawn_into(&mut threads, "reader", move || {
            let child = take_owned(&reader_handoff);
            rpc::read_loop(stdout, child, reader_conn, reader_label, stderr_drained_rx);
        }) {
            abandon(&handoff, &label);
            return Err(error);
        }

        tracing::debug!(pid, command = %label, "acp: agent process started");
        Ok(Agent {
            inner: Arc::new(AgentInner {
                conn,
                pid,
                label,
                threads: Mutex::new(threads),
                torn_down: AtomicBool::new(false),
            }),
        })
    }

    /// The handshake, which must be the first call. Refuses a second one: an agent
    /// process has one negotiated capability set, and re-handshaking it would let
    /// two answers disagree about what the session may do.
    pub fn initialize(
        &self,
        client: ClientInfo,
        capabilities: ClientCapabilities,
    ) -> Result<AgentCapabilities> {
        self.inner.conn.require_live("initialize")?;
        if self.inner.conn.initialized.get().is_some() {
            bail!("acp: initialize was already completed on this agent process");
        }
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "clientInfo": client,
            "clientCapabilities": capabilities,
        });
        let result = self.inner.conn.request("initialize", params)?;
        let negotiated = AgentCapabilities::from_result(&result);
        if negotiated.protocol_version != PROTOCOL_VERSION {
            bail!(
                "acp: {} negotiated protocol version {}, this client speaks {PROTOCOL_VERSION}",
                self.inner.label,
                negotiated.protocol_version
            );
        }
        let _ = self.inner.conn.initialized.set(negotiated.clone());
        let agent = &negotiated.agent_info;
        let name = agent.get("name").and_then(Value::as_str).unwrap_or("?");
        let version = agent.get("version").and_then(Value::as_str).unwrap_or("?");
        tracing::info!(
            agent = %self.inner.label,
            name,
            version,
            auth_methods = negotiated.auth_methods.len(),
            "acp: handshake complete"
        );
        Ok(negotiated)
    }

    /// What the agent answered `initialize` with, if it has.
    pub fn negotiated(&self) -> Option<AgentCapabilities> {
        self.inner.conn.initialized.get().cloned()
    }

    /// Open a session on this agent process. `cwd` must be absolute: it is the
    /// agent's own working directory, and a relative one means something different
    /// on the far side of the pipe than it does here.
    pub fn new_session(&self, cwd: &Path, mcp_servers: Vec<McpServer>) -> Result<SessionStart> {
        let conn = self.inner.conn.as_ref();
        conn.require_initialized("session/new")?;
        if !cwd.is_absolute() {
            bail!(
                "acp: session/new needs an absolute cwd, got {}",
                cwd.display()
            );
        }
        let params = json!({
            "cwd": cwd.to_string_lossy().to_string(),
            "mcpServers": serde_json::to_value(&mcp_servers)?,
        });
        let result = conn.request("session/new", params)?;
        let start = SessionStart::parse(&result)?;
        conn.add_session(start.session_id.clone());
        tracing::info!(
            session_id = %start.session_id,
            agent = %self.inner.label,
            "acp: session opened"
        );
        Ok(start)
    }

    /// Hand a prompt to a session and wait for the turn to end. Stream the text
    /// out of [`Agent::subscribe`]; the returned [`PromptOutcome`] says only how
    /// the turn stopped.
    pub fn prompt(&self, session_id: &str, blocks: Vec<ContentBlock>) -> Result<PromptOutcome> {
        let conn = self.inner.conn.as_ref();
        conn.require_initialized("session/prompt")?;
        conn.require_session("session/prompt", session_id)?;
        conn.require_subscriber("session/prompt")?;
        if blocks.is_empty() {
            bail!("acp: session/prompt needs at least one content block for session {session_id}");
        }
        let params = json!({
            "sessionId": session_id,
            "prompt": serde_json::to_value(&blocks)?,
        });
        let result = conn.request("session/prompt", params)?;
        let outcome = PromptOutcome::parse(&result);
        tracing::debug!(
            session_id,
            stop_reason = %outcome.stop_reason,
            "acp: turn ended"
        );
        Ok(outcome)
    }

    /// Set one of the session's `configOptions` (a `mode`, `model`, or
    /// `reasoning_effort` select, by the id the agent reported).
    pub fn set_config_option(&self, session_id: &str, config_id: &str, value: &str) -> Result<()> {
        let conn = self.inner.conn.as_ref();
        conn.require_initialized("session/set_config_option")?;
        conn.require_session("session/set_config_option", session_id)?;
        conn.request(
            "session/set_config_option",
            json!({"sessionId": session_id, "configId": config_id, "value": value}),
        )?;
        Ok(())
    }

    /// Switch the session's mode (`default`, `acceptEdits`, `bypassPermissions`
    /// and friends). An agent that stops asking for permission in a permissive
    /// mode is why this is a separate call from [`Agent::set_config_option`].
    pub fn set_mode(&self, session_id: &str, mode_id: &str) -> Result<()> {
        let conn = self.inner.conn.as_ref();
        conn.require_initialized("session/set_mode")?;
        conn.require_session("session/set_mode", session_id)?;
        conn.request(
            "session/set_mode",
            json!({"sessionId": session_id, "modeId": mode_id}),
        )?;
        Ok(())
    }

    /// Tell the agent to stop the session's current turn. Deliberately ungated:
    /// cancellation stays available even before a handshake completes, and the
    /// agent is free to answer the parked `session/prompt` with
    /// [`PromptOutcome::CANCELLED`].
    pub fn cancel(&self, session_id: &str) -> Result<()> {
        self.inner
            .conn
            .notify("session/cancel", json!({"sessionId": session_id}))
    }

    /// Close a session, keeping the process for the others. An agent without the
    /// `close` session capability answers `-32601`; a caller that does not care can
    /// downcast [`crate::RpcError`] and check.
    pub fn close_session(&self, session_id: &str) -> Result<()> {
        let conn = self.inner.conn.as_ref();
        conn.require_initialized("session/close")?;
        conn.require_session("session/close", session_id)?;
        conn.request("session/close", json!({"sessionId": session_id}))?;
        conn.remove_session(session_id);
        Ok(())
    }

    /// Answer an [`crate::Event::Permission`] the caller received.
    pub fn answer(&self, request_id: RequestId, outcome: PermissionOutcome) -> Result<()> {
        self.inner.conn.reply(&request_id, outcome.to_result())
    }

    /// Ids of the requests this client is still waiting on. Pairs with
    /// [`Agent::cancel_request`] for abandoning one whose agent has gone quiet —
    /// a parked caller cannot name the request it is parked on.
    pub fn outstanding(&self) -> Vec<RequestId> {
        self.inner.conn.outstanding()
    }

    /// Withdraw one outstanding request. This is the escape hatch for a wedged
    /// agent; a turn the caller wants to stop early is [`Agent::cancel`].
    pub fn cancel_request(&self, request_id: RequestId) -> Result<()> {
        self.inner
            .conn
            .notify("$/cancel_request", json!({"requestId": request_id}))
    }

    /// A receiver for everything the agent sends that this crate does not decide.
    /// Every call returns a new independent receiver, so a second subscriber sees
    /// its own copy of each event.
    pub fn subscribe(&self) -> Receiver<crate::Event> {
        self.inner.conn.subscribe()
    }

    /// The agent's pid, for a caller that records which process served a session.
    pub fn pid(&self) -> u32 {
        self.inner.pid
    }

    /// The agent's process group, equal to [`Agent::pid`] after the group is set at
    /// spawn. `None` on a platform where this crate cannot ask, and the signal path
    /// has the same limit.
    pub fn process_group(&self) -> Option<u32> {
        process_group_of(self.inner.pid)
    }

    /// Whether the agent process has been reaped.
    pub fn is_gone(&self) -> bool {
        self.inner.conn.exited()
    }

    /// Close stdin, then `SIGTERM`, then `SIGKILL`, and do not return until the
    /// process is confirmed dead.
    ///
    /// Call it from a thread that is not parked on a request: shutdown waits for
    /// the reader thread to report the reap, and that thread may be the one
    /// holding the answer the parked caller is waiting for.
    pub fn shutdown(self) -> Result<()> {
        self.inner.teardown()
    }
}

impl AgentInner {
    fn teardown(&self) -> Result<()> {
        if self.torn_down.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.conn.close_output();
        // Each rung is strictly louder than the last, and none of them runs once
        // the reaper has reported: signalling a pid we already reaped risks
        // hitting a recycled process group.
        let rungs: [(Option<i32>, Duration); 3] = [
            (None, EOF_GRACE),
            (Some(SIGTERM), TERMINATE_GRACE),
            (Some(SIGKILL), KILL_GRACE),
        ];
        for (signal, budget) in rungs {
            if self.conn.exited() {
                break;
            }
            if let Some(signal) = signal {
                if !signal_group(self.pid, signal) {
                    signal_pid(self.pid, signal);
                }
            }
            if wait_for_exit(&self.conn, budget) {
                break;
            }
        }
        if !self.conn.exited() {
            bail!(
                "acp: {} (pid {}) is still alive after SIGKILL and is not confirmed \
                 dead{}",
                self.label,
                self.pid,
                if cfg!(unix) {
                    ""
                } else {
                    "; this platform offers this crate no way to force-kill a child, \
                     so an agent that ignores end-of-stdin must be stopped by its owner"
                }
            );
        }
        let mut threads = match self.threads.lock() {
            Ok(threads) => threads,
            Err(poisoned) => poisoned.into_inner(),
        };
        for handle in threads.drain(..) {
            let name = handle.thread().name().unwrap_or("unnamed").to_string();
            if !join_within(handle, JOIN_GRACE) {
                tracing::warn!(
                    agent = %self.label,
                    thread = %name,
                    "acp: helper thread is still blocked after the agent was reaped; \
                     leaving it detached"
                );
            }
        }
        tracing::info!(agent = %self.label, pid = self.pid, "acp: agent shut down");
        Ok(())
    }
}

impl Drop for AgentInner {
    fn drop(&mut self) {
        if self.torn_down.load(Ordering::SeqCst) {
            return;
        }
        if let Err(error) = self.teardown() {
            tracing::error!("acp: {error}");
        }
    }
}

// ------------------------------------------------------------------ threads

fn spawn_into(
    threads: &mut Vec<JoinHandle<()>>,
    name: &str,
    task: impl FnOnce() + Send + 'static,
) -> Result<()> {
    threads.push(spawn_thread(name, task)?);
    Ok(())
}

fn spawn_thread(name: &str, task: impl FnOnce() + Send + 'static) -> Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name(format!("onlyne-acp-{name}"))
        .spawn(task)
        .map_err(|error| anyhow!("acp: start the {name} thread for the agent: {error}"))
}

fn claim(handoff: &Mutex<Option<Child>>) -> Option<Child> {
    match handoff.lock() {
        Ok(mut guard) => guard.take(),
        Err(poisoned) => poisoned.into_inner().take(),
    }
}

/// The reader thread's hand-off. Exactly one thread ever calls this, and only
/// after every path that could have called [`abandon`] has returned.
fn take_owned(handoff: &Mutex<Option<Child>>) -> Child {
    claim(handoff).expect("the child handle is handed to exactly one thread, once")
}

/// A thread could not be started, so nobody will reap this child: kill it here.
fn abandon(handoff: &Mutex<Option<Child>>, label: &str) {
    let Some(mut child) = claim(handoff) else {
        return;
    };
    if let Err(error) = child.kill() {
        tracing::error!("acp: {label} survived a failed spawn and could not be killed: {error}");
    }
    let _ = child.wait();
}

fn wait_for_exit(conn: &Conn, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    loop {
        if conn.exited() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(REAP_POLL);
    }
}

fn join_within(handle: JoinHandle<()>, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    while !handle.is_finished() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(REAP_POLL);
    }
    match handle.join() {
        Ok(()) => true,
        Err(_) => {
            // Each thread body already survives its own panic except at the edges
            // this crate cannot predict; the process is reaped either way, so the
            // fact an operator needs is in the log, not in this return value.
            tracing::error!("acp: an agent helper thread panicked");
            false
        }
    }
}

// ------------------------------------------------------------------- signals

#[cfg(unix)]
unsafe extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
    fn getpgid(pid: i32) -> i32;
}

/// POSIX signal numbers, identical on the unix platforms this workspace targets.
const SIGTERM: i32 = 15;
const SIGKILL: i32 = 9;

/// `kill(2)` without a `libc` dependency: this crate is allowed the four workspace
/// dependencies and no more, and two syscalls do not earn a sixth crate in the
/// binary. The declaration stays private to this module on purpose — a second
/// crate that re-declared `kill` with a different signature would link, and the
/// resolution is unspecified.
#[cfg(unix)]
fn send_signal(target: i32, signal: i32) -> bool {
    // 0 means "every process in my group" on Darwin and -1 broadcasts; neither is
    // ever an agent child. A pid of 0 here also means the handle was already
    // reaped, so the call would target the wrong thing entirely.
    if target <= 1 {
        return false;
    }
    unsafe { kill(target, signal) == 0 }
}

#[cfg(not(unix))]
fn send_signal(_target: i32, _signal: i32) -> bool {
    false
}

/// Signal the agent's whole group, so the work it started goes with it.
fn signal_group(pid: u32, signal: i32) -> bool {
    let Ok(leader) = i32::try_from(pid) else {
        return false;
    };
    send_signal(-leader, signal)
}

/// Signal just the leader, for a child that is not its own group's leader.
fn signal_pid(pid: u32, signal: i32) -> bool {
    let Ok(leader) = i32::try_from(pid) else {
        return false;
    };
    send_signal(leader, signal)
}

fn process_group_of(pid: u32) -> Option<u32> {
    #[cfg(unix)]
    {
        let target = i32::try_from(pid).ok()?;
        let group = unsafe { getpgid(target) };
        if group < 0 {
            return None;
        }
        u32::try_from(group).ok()
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        None
    }
}
