//! ACP session backend: run the role's agent as an Agent Client Protocol peer of
//! this client, instead of as a program inside a terminal.
//!
//! The other backends start a session command in a place a human can read —
//! a zellij pane, an Orca tab, a herdr split, an `exec` child whose stdout is a
//! log file — and a mounted plugin inside that place reports the session's state
//! back. An ACP agent has no place and mounts nothing: it is a process that
//! speaks a turn protocol on a pipe, so this backend owns both halves the plugin
//! would otherwise supply. It carries the payload itself
//! ([`SessionBackend::deliver`]) and it reports the ending itself
//! ([`SessionBackend::outcomes`]). The agent's half is one file: every prompt
//! names an absolute report path under the workspace, and the turn's end reads
//! it once, consumes it, and lets it stand in for the agent's closing words.
//!
//! Process discipline, which is what makes it different from a loop that just
//! calls [`onlyne_acp::Agent::prompt`]:
//!
//! * **one agent process per distinct rendered command, shared by every session
//!   of the role.** A role whose `session_command` renders the same argv for each
//!   task runs one agent and opens one ACP session per task on it. A command that
//!   interpolates `{task}` renders to a different key per session and gets a
//!   process of its own: legal, and the reason the key is the command and not the
//!   role. A shared process keeps the environment it was started with, so a second
//!   session rides the *first* session's `ONLYNE_*` identity variables — ACP has
//!   no per-session exec environment, so an agent that needs its own identity per
//!   task should render `{task}` into its command and take a process of its own.
//! * **a turn runs on its own thread.** [`onlyne_acp::Agent::prompt`] parks its
//!   caller until the agent ends the turn, which can be minutes;
//!   [`SessionBackend::deliver`] answers at once and the thread pushes a
//!   [`SessionOutcome`] when the turn ends. Settling from the delivery path would
//!   leave the role unable to serve a second session meanwhile.
//! * **permission asks are answered on a policy, never by a fallback grant.** One
//!   responder thread per process listens for `session/request_permission`:
//!   `reject_once` by default, `allow_once` for a client told to allow, and never
//!   `allow_always` — a blanket grant is an operator's decision, expressed through
//!   the session `mode`, not a client default.
//! * **the conversation is written down.** An ACP session owns no terminal, so
//!   `<workspace>/.onlyne/logs/session-<task>.log` (rendered, for `tail -f`) and
//!   `session-<task>.events.jsonl` (raw updates, plus this client's own
//!   `dispatch`, `payload` and `turn` records) are the whole human-visible
//!   surface. Both are best effort: a write that fails is a warning, never a
//!   failed turn.
//! * **closing does not wait on the dispatch lock.** A close asks the agent to
//!   stop, waits a short bounded moment for the turn, and hands a turn that is
//!   still running to a detached thread rather than parking its caller.

use super::*;
use crate::content::ContentWriter;
use chrono::SecondsFormat;
use onlyne_acp::{
    Agent, AgentOptions, ClientCapabilities, ClientInfo, ContentBlock, Event, PermissionOption,
    PermissionOutcome, PermissionRequest, PromptOutcome, Update,
};
use parking_lot::{Condvar, Mutex};
use serde_json::json;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Weak};
use std::thread;

/// The client name reported in `initialize`.
const CLIENT_NAME: &str = "onlyne-client";
/// How long the detached closer waits for a running turn before it lets the agent
/// decide the turn's end on its own.
const TURN_CLOSE_BUDGET: Duration = Duration::from_secs(60);
/// Refusals named in one fault record; the rest are counted, not listed.
const REFUSAL_LIST_LIMIT: usize = 3;

/// The `[client.acp]` table, in the shape this crate can hold without a config
/// dependency: every field already defaulted, and the permission mode reduced to
/// the one decision a backend makes with it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AcpOptions {
    /// `session/set_mode` id. Empty leaves the agent's own default.
    pub mode: String,
    /// `model` config option value. Empty leaves the agent's default.
    pub model: String,
    /// `reasoning_effort` config option value. Empty leaves the default.
    pub reasoning_effort: String,
    /// Whether this client answers an agent's permission request with a grant.
    /// Off by default: the refusal is recorded and a supervisor decides.
    pub allow_permissions: bool,
}

impl AcpOptions {
    /// The word a fault record names as the policy behind a refusal.
    fn policy(&self) -> &'static str {
        if self.allow_permissions {
            "allow"
        } else {
            "deny"
        }
    }
}

/// The backend: an agent per command key, an ACP session per task, one outcome
/// stream for the whole role.
#[derive(Clone)]
pub struct AcpBackend {
    options: AcpOptions,
    state: Arc<State>,
}

struct State {
    /// Live agent processes, keyed by the rendered command that started them.
    agents: Mutex<BTreeMap<String, Arc<AgentSlot>>>,
    /// Every session this client holds, keyed by the agent command and the id that
    /// agent chose for it: ACP ids are unique within a process, not across
    /// processes, and an id stays stable across a `reuse` hand-over where a task id
    /// does not.
    sessions: Mutex<BTreeMap<(String, String), Arc<SessionEntry>>>,
    sink: OutcomeSink,
    feed: OutcomeFeed,
    /// Serializes journal cursors across every task served by this role.
    content: ContentWriter,
}

struct AgentSlot {
    agent: Arc<Agent>,
    /// Sessions of this process still held by this client, so the last one to
    /// leave can take the process with it.
    live: AtomicUsize,
}

/// One ACP session, plus the turn state this client keeps for it.
struct SessionEntry {
    /// The task this session is serving right now. A `reuse` role moves a second
    /// task onto the same ACP conversation, and that task owns its own journal.
    task_id: Mutex<String>,
    /// The id the agent gave this session; every later request is keyed by it.
    id: String,
    /// The command key of the process serving this session.
    agent_key: String,
    /// The directory the agent runs in, which is where its journal lives.
    workdir: PathBuf,
    agent: Arc<Agent>,
    turn: Turn,
    /// Permission asks refused since the turn began, written by the responder
    /// thread and taken by the turn thread when the turn ends. Per turn, because
    /// the asks interleave with the updates of the one parked prompt they belong
    /// to, and a refusal has to name the turn that produced it.
    refusals: Mutex<Vec<String>>,
}

impl SessionEntry {
    fn current_task(&self) -> String {
        self.task_id.lock().clone()
    }
}

/// The turn bookkeeping of one session: whether a turn is in flight, and which
/// turn a waiter is waiting out.
struct Turn {
    phase: Mutex<TurnPhase>,
    ended: Condvar,
}

struct TurnPhase {
    live: bool,
    generation: u64,
}

impl Turn {
    fn new() -> Self {
        Turn {
            phase: Mutex::new(TurnPhase {
                live: false,
                generation: 0,
            }),
            ended: Condvar::new(),
        }
    }

    /// Claim the session for a turn, refusing one already in flight. Answers the
    /// generation a later waiter names.
    fn begin(&self) -> Option<u64> {
        let mut phase = self.phase.lock();
        if phase.live {
            return None;
        }
        phase.live = true;
        phase.generation += 1;
        Some(phase.generation)
    }

    /// Mark the turn over and wake anyone waiting out the close of a session.
    fn finish(&self) {
        self.phase.lock().live = false;
        self.ended.notify_all();
    }

    fn generation(&self) -> u64 {
        self.phase.lock().generation
    }

    /// Whether the turn named by `generation` is over: true when it ended, false
    /// when the wait ran out. A newer generation counts as ended too, because a
    /// turn cannot start before the one before it stopped.
    fn waited_out(&self, generation: u64, budget: Duration) -> bool {
        let mut phase = self.phase.lock();
        if !Turn::running(&phase, generation) {
            return true;
        }
        self.ended.wait_while_for(
            &mut phase,
            |phase| phase.live && phase.generation == generation,
            budget,
        );
        !Turn::running(&phase, generation)
    }

    fn running(phase: &TurnPhase, generation: u64) -> bool {
        phase.live && phase.generation == generation
    }
}

impl AcpBackend {
    pub fn new(options: AcpOptions) -> Self {
        let (sink, feed) = OutcomeFeed::channel();
        AcpBackend {
            options,
            state: Arc::new(State {
                agents: Mutex::new(BTreeMap::new()),
                sessions: Mutex::new(BTreeMap::new()),
                sink,
                feed,
                content: ContentWriter::default(),
            }),
        }
    }

    /// The argv this session's agent runs as: the role's rendered
    /// `session_command`, never a string handed to a shell.
    fn command_of(spec: &SpawnSpec) -> Result<Vec<String>> {
        match spec.command.first() {
            Some(program) if !program.trim().is_empty() => Ok(spec.command.clone()),
            _ => Err(anyhow::anyhow!(
                "acp: role has no session_command; there is nothing to run for task {}",
                spec.task_id
            )),
        }
    }

    /// The process serving this command, with one session already reserved on it
    /// so a shared agent cannot be torn down between two sessions handing over.
    ///
    /// The handshake runs under the agent map: it is the one slow step that must
    /// not be duplicated for a second session of the same command, and the map is
    /// what makes "first one here" decidable.
    fn agent_for(
        &self,
        key: &str,
        command: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<Arc<AgentSlot>> {
        let mut agents = self.state.agents.lock();
        if let Some(slot) = agents.get(key) {
            if !slot.agent.is_gone() {
                slot.live.fetch_add(1, Ordering::SeqCst);
                return Ok(Arc::clone(slot));
            }
            // A crashed process is not a runtime to share: drop it and start the
            // replacement the next session deserves.
            tracing::warn!(agent = %key, "acp: replacing an exited agent process");
            agents.remove(key);
        }
        let agent = Arc::new(Agent::start(AgentOptions {
            command: command.to_vec(),
            cwd: Some(cwd.to_path_buf()),
            env: env.clone(),
        })?);
        // The handshake is part of starting the process: an agent that refuses it
        // is not a runtime. The handle goes to a thread rather than being dropped
        // here because `AgentInner`'s teardown waits for the process it is ending,
        // and this call runs under the client's dispatch lock.
        if let Err(error) =
            agent.initialize(ClientInfo::new(CLIENT_NAME), ClientCapabilities::default())
        {
            let _ = thread::Builder::new()
                .name(format!("acp-drop {key}"))
                .spawn(move || drop(agent));
            return Err(anyhow::anyhow!("acp: {key} refused the handshake: {error}"));
        }
        let slot = Arc::new(AgentSlot {
            live: AtomicUsize::new(1),
            agent: Arc::clone(&agent),
        });
        spawn_responder(&slot, Arc::clone(&self.state), self.options.clone(), key);
        agents.insert(key.to_string(), Arc::clone(&slot));
        Ok(slot)
    }

    /// Open the ACP session and apply the configured mode and model, so a session
    /// is usable the moment this client reports it ready. A refusal of either
    /// setting fails the spawn: an operator asked for that mode, and a session
    /// running in a different one is not a partial success.
    fn open_session(
        &self,
        slot: &AgentSlot,
        spec: &SpawnSpec,
        key: &str,
    ) -> Result<Arc<SessionEntry>> {
        let start = slot.agent.new_session(&spec.cwd, Vec::new())?;
        if !self.options.mode.is_empty() {
            slot.agent
                .set_mode(&start.session_id, &self.options.mode)
                .map_err(|error| {
                    anyhow::anyhow!(
                        "acp: {} rejected mode {:?}: {error}",
                        start.session_id,
                        self.options.mode
                    )
                })?;
        }
        for (config, value) in [
            ("model", self.options.model.as_str()),
            ("reasoning_effort", self.options.reasoning_effort.as_str()),
        ] {
            if value.is_empty() {
                continue;
            }
            slot.agent
                .set_config_option(&start.session_id, config, value)
                .map_err(|error| {
                    anyhow::anyhow!(
                        "acp: {} rejected {config} {:?}: {error}",
                        start.session_id,
                        value
                    )
                })?;
        }
        let entry = Arc::new(SessionEntry {
            task_id: Mutex::new(spec.task_id.clone()),
            id: start.session_id,
            agent_key: key.to_string(),
            workdir: spec.cwd.clone(),
            agent: Arc::clone(&slot.agent),
            turn: Turn::new(),
            refusals: Mutex::new(Vec::new()),
        });
        self.state.sessions.lock().insert(
            (entry.agent_key.clone(), entry.id.clone()),
            Arc::clone(&entry),
        );
        tracing::info!(
            task = %spec.task_id,
            acp_session = %entry.id,
            pid = entry.agent.pid(),
            agent = %key,
            "acp session opened"
        );
        Ok(entry)
    }

    /// The live session a stored reference names: by the id the agent chose for it
    /// under its own command first, by task id for a reference that carries
    /// neither. ACP session ids are only unique within one agent process, so two
    /// processes may hand out the same `sess-1`, and the key carries the agent
    /// command for exactly that reason. A reference from a client run that no
    /// longer holds the process names nothing here.
    fn entry_of(&self, session: &SessionRef) -> Option<Arc<SessionEntry>> {
        let sessions = self.state.sessions.lock();
        let agent = session
            .backend_ref
            .get("agent")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if let Some(id) = session.backend_ref.get("id").and_then(Value::as_str)
            && let Some(found) = sessions.get(&(agent.to_string(), id.to_string()))
        {
            return Some(Arc::clone(found));
        }
        sessions
            .values()
            .find(|entry| entry.current_task() == session.task_id)
            .cloned()
    }

    fn take_entry(&self, session: &SessionRef) -> Option<Arc<SessionEntry>> {
        let found = self.entry_of(session)?;
        let key = (found.agent_key.clone(), found.id.clone());
        self.state.sessions.lock().remove(&key)
    }
}

impl State {
    /// One session of this process went away. Past the last one the process goes
    /// with it, on a thread that can afford to wait for it: see [`State::reap`].
    fn retire(&self, key: &str) {
        let slot = {
            let mut agents = self.agents.lock();
            match agents.get(key) {
                Some(slot) => {
                    if slot.live.fetch_sub(1, Ordering::SeqCst) > 1 {
                        return;
                    }
                    agents.remove(key)
                }
                // The process left on its own, and its bookkeeping went with the
                // notice. There is nothing here to release.
                None => return,
            }
        };
        if let Some(slot) = slot {
            self.reap(key, slot);
        }
    }

    /// End a process nobody's session needs any more.
    ///
    /// [`onlyne_acp::Agent::shutdown`] is a bounded wait — end of stdin, then
    /// `SIGTERM`, then `SIGKILL`, then the confirmation that the child was reaped
    /// — and every caller that releases a session holds the client's dispatch
    /// lock. The wait therefore belongs to its own thread. A handle another thread
    /// still shares is simply dropped here, and `AgentInner`'s teardown runs
    /// wherever the last share of it goes away.
    fn reap(&self, key: &str, slot: Arc<AgentSlot>) {
        let owned = key.to_string();
        match thread::Builder::new()
            .name(format!("acp-reap {owned}"))
            .spawn(move || reap_slot(owned, slot))
        {
            Ok(_) => {}
            // With no thread to wait on, the handle goes with this one, and its
            // own teardown runs inline. That is the same work on the slow path,
            // reachable only when the process cannot start a thread at all.
            Err(error) => tracing::warn!(
                error = %error,
                agent = %key,
                "acp: no reaper thread; the agent handle is dropped where it was released"
            ),
        }
    }

    /// The process left on its own. Every parked request of its sessions fails by
    /// themselves, so this only keeps a new session from opening on a corpse.
    fn note_gone(&self, key: &str) {
        let slot = self.agents.lock().remove(key);
        if let Some(slot) = slot {
            tracing::warn!(
                agent = %key,
                pid = slot.agent.pid(),
                sessions = slot.live.load(Ordering::SeqCst),
                "acp: agent process exited"
            );
        }
    }
}

/// Wait for the process to leave, on the thread that volunteered to.
fn reap_slot(key: String, slot: Arc<AgentSlot>) {
    match Arc::try_unwrap(slot) {
        Ok(slot) => match Arc::try_unwrap(slot.agent) {
            Ok(agent) => {
                if let Err(error) = agent.shutdown() {
                    tracing::warn!(agent = %key, error = %error, "acp: agent teardown reported");
                }
            }
            Err(_) => tracing::debug!(
                agent = %key,
                "acp: agent handle still held by a live turn; its teardown runs with that share"
            ),
        },
        Err(_) => tracing::debug!(
            agent = %key,
            "acp: agent slot still shared; its teardown follows the last handle"
        ),
    }
}

/// The permission responder, one per agent process.
///
/// It holds only a weak handle: the last strong one going away is what drops the
/// `Agent`, and a strong handle here would keep a closed agent from ever being
/// reaped, because the responder outlives the events it waits for.
fn spawn_responder(slot: &AgentSlot, state: Arc<State>, options: AcpOptions, key: &str) {
    let agent = Arc::downgrade(&slot.agent);
    let key = key.to_string();
    if let Err(error) = thread::Builder::new()
        .name(format!("acp-permissions {key}"))
        .spawn(move || answer_permissions(agent, state, options, key))
    {
        tracing::warn!(
            error = %error,
            agent = %slot.agent.pid(),
            "acp: no permission responder; an agent's request waits for its own timeout"
        );
    }
}

fn answer_permissions(agent: Weak<Agent>, state: Arc<State>, options: AcpOptions, key: String) {
    let Some(handle) = agent.upgrade() else {
        return;
    };
    let events = handle.subscribe();
    drop(handle);
    while let Ok(event) = events.recv() {
        match event {
            Event::Permission(request) => {
                let Some(handle) = agent.upgrade() else {
                    return;
                };
                let (outcome, refusal) = decide(&options, &request);
                if let Err(error) = handle.answer(request.request_id.clone(), outcome) {
                    tracing::warn!(
                        error = %error,
                        acp_session = %request.session_id,
                        "acp: the permission answer did not reach the agent"
                    );
                }
                drop(handle);
                if let Some(line) = refusal {
                    record_refusal(&state, &key, &request.session_id, &options, line);
                }
            }
            Event::Exited { detail } => {
                tracing::warn!(
                    agent = %key,
                    detail = %detail,
                    "acp: agent exited; its permission responder stops"
                );
                state.note_gone(&key);
                return;
            }
            // Updates belong to the turn threads, each of which holds its own
            // receiver; answering nothing is what keeps a turn's journal clean.
            Event::Update { .. } => {}
        }
    }
}

/// Pick the answer one permission request gets, and the refusal line to record
/// when that answer was not a grant.
fn decide(
    options: &AcpOptions,
    request: &PermissionRequest,
) -> (PermissionOutcome, Option<String>) {
    let chosen = if options.allow_permissions {
        request.option(PermissionOption::ALLOW_ONCE)
    } else {
        request
            .option(PermissionOption::REJECT_ONCE)
            .or_else(|| request.rejection_option())
    };
    match chosen {
        Some(option) => (
            PermissionOutcome::Selected {
                option_id: option.option_id.clone(),
            },
            (!options.allow_permissions).then(|| refusal_line("refused", request, Some(option))),
        ),
        // Nothing single-use to answer with. `allow_always` is never picked here, so
        // an agent that offers only a blanket grant gets no decision from this
        // client, which is its own refusal.
        None => (
            PermissionOutcome::Cancelled,
            Some(refusal_line("declined", request, None)),
        ),
    }
}

fn refusal_line(
    verb: &str,
    request: &PermissionRequest,
    option: Option<&PermissionOption>,
) -> String {
    let field = |key: &str| {
        request
            .tool_call
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let title = field("title");
    let subject = if title.is_empty() {
        format!("session {}", request.session_id)
    } else {
        title
    };
    format!(
        "{verb} {subject} [kind={}, option={}]",
        or_dash(field("kind")),
        option.map_or("-".to_string(), |option| or_dash(option.kind.clone())),
    )
}

fn or_dash(text: String) -> String {
    if text.is_empty() {
        "-".to_string()
    } else {
        text
    }
}

/// File one permission refusal under the session that was asked. The agent command
/// scopes the id, because two processes can choose the same one.
fn record_refusal(
    state: &State,
    agent_key: &str,
    session_id: &str,
    options: &AcpOptions,
    line: String,
) {
    let entry = state
        .sessions
        .lock()
        .get(&(agent_key.to_string(), session_id.to_string()))
        .cloned();
    match entry {
        Some(entry) => entry.refusals.lock().push(line),
        None => tracing::debug!(
            acp_session = %session_id,
            policy = options.policy(),
            "acp: refused a permission ask for a session this client already released"
        ),
    }
}

/// Everything one turn left on the pipe, split by who reads it.
#[derive(Default)]
struct Drained {
    /// Raw update params, verbatim, in arrival order, for the JSONL journal.
    lines: Vec<Value>,
    /// The same updates rendered for the operator's log.
    log: String,
    /// Assistant text, concatenated.
    message: String,
    /// The exit detail, when the process left during the turn.
    exited: Option<String>,
}

/// Read what the agent sent for one session.
///
/// The crate's single reader thread routes a notification before it wakes the
/// parked request that follows it on the wire, so once `prompt` returns every
/// update of that turn is already in this receiver and one drain is exact: no
/// collector thread and no wait-and-see heuristic. Fan-out is per process, so
/// another session's updates are dropped here — they belong to that turn's
/// journal and that turn's head.
fn drain(events: &Receiver<Event>, session_id: &str) -> Drained {
    let mut drained = Drained::default();
    let mut render = Render::default();
    while let Ok(event) = events.try_recv() {
        match event {
            Event::Update {
                session_id: id,
                update,
            } if id == session_id => {
                drained.lines.push(update.raw().clone());
                match &update {
                    Update::AgentMessageChunk(chunk) => {
                        drained.message.push_str(&chunk.text);
                        render.chunk(MESSAGE, &chunk.text);
                    }
                    Update::AgentThoughtChunk(chunk) => render.chunk(THOUGHT, &chunk.text),
                    Update::ToolCall(value) | Update::ToolCallUpdate(value) => {
                        render.line(tool_line(&update, value));
                    }
                    // `available_commands_update` is a multi-kilobyte dump of the
                    // agent's slash commands, the echoed user message repeats what
                    // this client just sent, and a plan wholesale-replaces a
                    // previous one: all three ride the JSONL and leave the log.
                    Update::UserMessageChunk(_) | Update::Plan(_) | Update::Unknown(_) => {}
                }
            }
            Event::Exited { detail } => drained.exited = Some(detail),
            _ => {}
        }
    }
    render.finish(&mut drained.log);
    drained
}

const MESSAGE: &str = "message";
const THOUGHT: &str = "thought";

/// Builds the rendered log: consecutive chunks of one kind become one block, and
/// a thought block is prefixed so an operator can tell reasoning from the answer.
#[derive(Default)]
struct Render {
    pending: Option<(&'static str, String)>,
    out: String,
}

impl Render {
    fn chunk(&mut self, kind: &'static str, text: &str) {
        match &mut self.pending {
            Some((found, buffer)) if *found == kind => buffer.push_str(text),
            _ => {
                self.flush();
                self.pending = Some((kind, text.to_string()));
            }
        }
    }

    fn line(&mut self, line: String) {
        self.flush();
        self.out.push_str(&line);
        self.out.push('\n');
    }

    fn flush(&mut self) {
        let Some((kind, text)) = self.pending.take() else {
            return;
        };
        let text = strip_status(&text);
        let text = text.trim_end();
        if text.is_empty() {
            return;
        }
        if kind == THOUGHT {
            for line in text.lines() {
                self.out.push_str("> ");
                self.out.push_str(line);
                self.out.push('\n');
            }
            return;
        }
        self.out.push_str(text);
        self.out.push('\n');
    }

    fn finish(mut self, into: &mut String) {
        self.flush();
        into.push_str(&self.out);
    }
}

fn tool_line(update: &Update, value: &Value) -> String {
    let field = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let id = or_dash(field("toolCallId"));
    match update {
        Update::ToolCall(_) => format!(
            "tool {id} {} kind={}",
            or_dash(field("title")),
            or_dash(field("kind"))
        ),
        _ => format!("tool {id} status={}", or_dash(field("status"))),
    }
}

/// Drop the `<status>…</status>` markers an agent stamps into its own answer.
///
/// An agent closes the marker with a matching tag and, in a stream cut short,
/// leaves it open — an unclosed marker runs to the end of the block, which is
/// where the agent puts it.
fn strip_status(text: &str) -> String {
    const OPEN: &str = "<status>";
    const CLOSE: &str = "</status>";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find(OPEN) {
        out.push_str(&rest[..open]);
        rest = &rest[open + OPEN.len()..];
        match rest.find(CLOSE) {
            Some(close) => rest = &rest[close + CLOSE.len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The line the receiving role reads as this turn's answer: the last non-empty
/// line of the agent's closing message, with its status markers gone.
fn completion_head(message: &str) -> Option<String> {
    let stripped = strip_status(message);
    if let Some(line) = stripped
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
    {
        return Some(line.to_string());
    }
    // A turn whose only text was a status marker still said something: report the
    // word the agent put in the marker rather than nothing at all.
    let marker = message.rfind("<status>")?;
    let word = stripped_at(message, marker);
    (!word.is_empty()).then_some(word)
}

/// The text after a status marker, markers stripped and whitespace trimmed.
fn stripped_at(message: &str, from: usize) -> String {
    strip_status(&message[from..]).trim().to_string()
}

/// Map a stop reason onto what the ledger records: the outcome, the fault note
/// when there is one, and the reason word the turn record carries.
fn settle_for(stop_reason: &str, answer: Option<&str>) -> (Outcome, Option<String>, String) {
    let named = if stop_reason.is_empty() {
        "(absent)".to_string()
    } else {
        stop_reason.to_string()
    };
    match stop_reason {
        PromptOutcome::END_TURN => (Outcome::Done, None, named),
        PromptOutcome::CANCELLED => (
            Outcome::Cancelled,
            Some("the client cancelled this session".to_string()),
            named,
        ),
        PromptOutcome::REFUSAL | PromptOutcome::MAX_TOKENS | PromptOutcome::MAX_TURN_REQUESTS => (
            Outcome::Failed,
            Some(format!("agent stopped the turn: {named}")),
            named,
        ),
        // An absent or unknown `stopReason` is not a success this client can read,
        // and neither is a turn that left no answer behind.
        _ => (
            Outcome::Failed,
            Some(match answer {
                Some(_) => format!("agent ended the turn with stopReason {named:?}"),
                None => format!("agent ended the turn with stopReason {named:?} and no answer"),
            }),
            named,
        ),
    }
}

/// Where one turn leaves its payload-v1 result report, derived from the
/// session's workspace and the task id. Both directions call this — the
/// directive that hands the agent its path and the ending that reads it — so
/// they cannot drift, and the path is absolute because a shared agent process
/// may run with some other session's directory as its cwd.
fn payload_dir(workdir: &Path) -> PathBuf {
    workdir.join(".onlyne").join("out")
}

fn payload_path(workdir: &Path, task_id: &str) -> PathBuf {
    payload_dir(workdir).join(format!("{task_id}.md"))
}

/// The block appended to every ACP prompt: where to report this task's
/// outcome, in what form, and the rule that settlement is ours. The agent is
/// not an onlyne client and is told so; the file is all it has to leave.
fn completion_directive(workdir: &Path, task_id: &str) -> String {
    format!(
        "\n\nResult report (write before you stop): {}\n\
         The file carries exactly one line: `hop-done: <the result in one \
         line>` or `hop-failed: <why the task failed, one sentence>`.\n\
         Create it under a temporary name in the same directory and rename it \
         into place, so no reader ever sees a half-written report.\n\
         Keep the detail in project files; the line may name them.\n\
         Do not run any `onlyne` command: this client reads the file and \
         settles the task itself.",
        payload_path(workdir, task_id).display()
    )
}

/// One turn's result report, as the ending found it.
enum Payload {
    /// No file, or one this client could not open: the turn settles on its
    /// stop reason alone, exactly as it did before reports existed.
    Absent,
    /// `hop-done:` — the agent's own account of the result, taken as the head.
    Done(String),
    /// `hop-failed:` — the agent's account of a failure, which this client has
    /// no reason to argue with.
    Failed(String),
    /// A file is there and is not a report. The reason names the category so
    /// the fault record can say what the client actually found.
    Invalid(String),
}

impl Payload {
    /// The word the turn's `payload` journal record carries under
    /// `payload_kind`; the record type `kind` is taken by the journal.
    fn payload_kind(&self) -> &'static str {
        match self {
            Payload::Absent => "absent",
            Payload::Done(_) => "done",
            Payload::Failed(_) => "failed",
            Payload::Invalid(_) => "invalid",
        }
    }
}

/// Read this turn's report and take it away. Deleting after reading is the
/// isolation a requeued task id needs: the next turn starts with no report
/// until an agent living through it writes one.
fn read_payload(workdir: &Path, task_id: &str) -> Payload {
    let path = payload_path(workdir, task_id);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(_) => return Payload::Absent,
    };
    if let Err(error) = std::fs::remove_file(&path) {
        tracing::warn!(error = %error, path = %path.display(), "acp: report file stayed behind");
    }
    match String::from_utf8(bytes) {
        Err(_) => Payload::Invalid("payload is not valid utf-8".to_string()),
        Ok(text) => parse_payload(&text),
    }
}

/// payload-v1 parsing: one non-empty line whose exact prefix decides the
/// verdict. The file's own terminating newline is allowed; anything past a
/// single report line is noise this client will not read a verdict from.
fn parse_payload(text: &str) -> Payload {
    let line = text.trim();
    if line.is_empty() {
        return Payload::Invalid("payload is empty".to_string());
    }
    if line.contains('\n') {
        return Payload::Invalid("payload is more than one line".to_string());
    }
    let (prefix, report) = match line.split_once(':') {
        Some((prefix, report)) => (prefix, report.trim()),
        None => {
            return Payload::Invalid("payload carries no report prefix".to_string());
        }
    };
    match (prefix, report.is_empty()) {
        ("hop-done", false) => Payload::Done(report.to_string()),
        ("hop-failed", false) => Payload::Failed(report.to_string()),
        ("hop-done", true) => Payload::Invalid("hop-done carries no result".to_string()),
        ("hop-failed", true) => Payload::Invalid("hop-failed carries no reason".to_string()),
        _ => Payload::Invalid(format!("unknown report prefix {prefix:?}")),
    }
}

/// One turn's journal files, named for the task it ran.
struct Journal {
    workspace: PathBuf,
    task_id: String,
    session_id: String,
    log: PathBuf,
    events: PathBuf,
    content: ContentWriter,
}

impl Journal {
    fn new(workdir: &Path, task_id: &str, session_id: &str, content: ContentWriter) -> Self {
        let logs = workdir.join(".onlyne").join("logs");
        Journal {
            workspace: workdir.to_path_buf(),
            task_id: task_id.to_string(),
            session_id: session_id.to_string(),
            log: logs.join(format!("session-{task_id}.log")),
            events: logs.join(format!("session-{task_id}.events.jsonl")),
            content,
        }
    }

    /// Append one record of our own to the JSONL journal.
    fn record(&self, kind: &str, fields: Vec<(&str, Value)>) {
        let at = now();
        let mut onlyne = json!({"kind": kind, "at": at});
        let map = onlyne.as_object_mut().expect("built above");
        for (key, value) in fields {
            map.insert(key.to_string(), value);
        }
        self.event(json!({"onlyne": onlyne}), &at);
    }

    /// Append one raw agent update without decorating the journal object.
    fn raw(&self, record: Value) {
        let at = now();
        self.event(record, &at);
    }

    fn event(&self, record: Value, at: &str) {
        if let Err(error) = self.content.append(
            &self.workspace,
            &self.task_id,
            Some(&self.session_id),
            &self.events,
            &record,
            at,
        ) {
            tracing::warn!(
                error = %error,
                file = %self.events.display(),
                "acp: content journal/index write failed"
            );
        }
    }
}

/// Append rendered text to a journal file, warning on failure. A call is a
/// record boundary, so the line terminator belongs here rather than at callers.
fn append(path: &Path, text: &str) {
    let body = text.trim_end_matches('\n');
    if body.is_empty() {
        return;
    }
    if let Err(error) = append_all(path, &format!("{body}\n")) {
        tracing::warn!(error = %error, file = %path.display(), "acp: journal write failed");
    }
}

fn append_all(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(text.as_bytes())?;
    file.flush()
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Run one turn and report how it ended. The thread this runs on is the only
/// reader of this session's ending, so the report happens exactly once.
fn run_turn(
    entry: Arc<SessionEntry>,
    sink: OutcomeSink,
    content: ContentWriter,
    task_id: String,
    prompt: String,
    warning: Option<String>,
    policy: &'static str,
) {
    let journal = Journal::new(&entry.workdir, &task_id, &entry.id, content);
    journal.record(
        "dispatch",
        vec![
            ("task_id", Value::from(task_id.clone())),
            ("prose", Value::from(prompt.clone())),
        ],
    );
    if let Some(detail) = &warning {
        journal.record(
            "warning",
            vec![
                ("task_id", Value::from(task_id.clone())),
                ("detail", Value::from(detail.clone())),
            ],
        );
    }
    let events = entry.agent.subscribe();
    let turn = entry
        .agent
        .prompt(&entry.id, vec![ContentBlock::text(&prompt)]);
    // The drain is exact only because every routed update is already queued when
    // the parked prompt wakes, and the agent's single reader thread is what puts
    // it there: see [`drain`].
    let drained = drain(&events, &entry.id);
    let refusals = take_refusals(&entry, policy);
    for record in drained.lines {
        journal.raw(record);
    }
    if !drained.log.is_empty() {
        append(&journal.log, &drained.log);
    }
    let head = completion_head(&drained.message);
    // payload-v1: a report the agent left stands in for what the closing
    // message said, and may only lower the turn's standing. `hop-done`
    // replaces the head while the stop reason still decides the outcome, so a
    // report can never promote a turn the agent was cut short on.
    let payload = read_payload(&entry.workdir, &task_id);
    let head = match &payload {
        Payload::Done(text) => Some(text.clone()),
        _ => head,
    };
    let (settled, note, stop_reason) = match &turn {
        Ok(outcome) => settle_for(&outcome.stop_reason, head.as_deref()),
        Err(error) => (
            Outcome::Failed,
            Some(death_note(error, drained.exited.as_deref())),
            "(error)".to_string(),
        ),
    };
    // A report that is present but not a report cancels the completion: the
    // client will not guess a verdict out of a file it asked for in one shape,
    // and the reason travels in the fault note rather than a head. A turn whose
    // process died or was cut short already carries a harder fact than any
    // report, so the verdict changes and the detail is added to that note.
    let payload_kind = payload.payload_kind();
    let (settled, head, note) = match payload {
        Payload::Failed(text) => {
            // The sender reads the agent's own line; where the turn was cut
            // short or its process died, that harder fact stays in the note.
            let note = match note {
                Some(found) => format!("{found}; the agent reported: {text}"),
                None => text.clone(),
            };
            (Outcome::Failed, Some(text), Some(note))
        }
        Payload::Invalid(reason) => {
            let detail = format!("acp payload invalid: {reason}");
            let note = match note {
                Some(found) => format!("{found}; {detail}"),
                None => detail,
            };
            (Outcome::Cancelled, None, Some(note))
        }
        _ => (settled, head, note),
    };
    journal.record(
        "payload",
        vec![
            ("task_id", Value::from(task_id.clone())),
            (
                "path",
                Value::from(payload_path(&entry.workdir, &task_id).display().to_string()),
            ),
            ("payload_kind", Value::from(payload_kind)),
            ("head", head.clone().map(Value::from).unwrap_or(Value::Null)),
        ],
    );
    journal.record(
        "turn",
        vec![
            ("task_id", Value::from(task_id.clone())),
            ("stop_reason", Value::from(stop_reason)),
            ("head", head.clone().map(Value::from).unwrap_or(Value::Null)),
        ],
    );
    // The session is free for a `reuse` role's next task the moment this releases,
    // and it must be released before the report is handed over: a role that takes
    // the next task between the two would otherwise be refused as busy.
    entry.turn.finish();
    sink.push(SessionOutcome {
        task_id,
        outcome: settled,
        head,
        note,
        refusals,
    });
}

/// Summarise this turn's refusals for the fault record, and reset the accumulator
/// for the next turn of the same session.
fn take_refusals(entry: &SessionEntry, policy: &str) -> Option<String> {
    let mut refused = std::mem::take(&mut *entry.refusals.lock());
    if refused.is_empty() {
        return None;
    }
    let count = refused.len();
    refused.truncate(REFUSAL_LIST_LIMIT);
    Some(format!(
        "{count} permission ask(s) refused (policy={policy}): {}",
        refused.join("; ")
    ))
}

/// The note for a turn that never got its answer. The parked request's own message
/// already names the exit status and the tail of stderr when the process died; the
/// exit event adds what that message could not.
fn death_note(error: &anyhow::Error, exited: Option<&str>) -> String {
    let detail = error.to_string();
    match exited {
        Some(exited) if !detail.contains(exited) => format!("{detail}; {exited}"),
        _ => detail,
    }
}

impl SessionBackend for AcpBackend {
    fn name(&self) -> &'static str {
        "acp"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            spawn: true,
            attach: true,
            probe: true,
            close: true,
            focus: false,
            // ACP v1 has no title path: naming a session is the terminal
            // backends' trick, and the agent's own session id is what this one
            // reports.
            rename: false,
        }
    }

    fn available(&self) -> Result<bool> {
        // Nothing to discover: the command is the role's own config, and a program
        // that is not installed fails its spawn naming the program.
        Ok(true)
    }

    fn self_driven(&self) -> bool {
        true
    }

    fn set_content_sink(&self, sink: Arc<dyn crate::content::ContentSink>) {
        self.state.content.set_sink(sink);
    }

    fn outcomes(&self) -> Option<OutcomeFeed> {
        Some(self.state.feed.clone())
    }

    fn spawn(&self, spec: SpawnSpec) -> Result<SessionRef> {
        let command = Self::command_of(&spec)?;
        let key = command.join(" ");
        let slot = self.agent_for(&key, &command, &spec.cwd, &spec.env)?;
        let entry = match self.open_session(&slot, &spec, &key) {
            Ok(entry) => entry,
            Err(error) => {
                self.state.retire(&key);
                return Err(error);
            }
        };
        let journal = Journal::new(
            &spec.cwd,
            &spec.task_id,
            &entry.id,
            self.state.content.clone(),
        );
        Ok(SessionRef {
            task_id: spec.task_id.clone(),
            backend: self.name().into(),
            backend_ref: json!({
                "id": entry.id,
                "pid": entry.agent.pid(),
                "agent": key,
                "log": journal.log.to_string_lossy(),
                "events": journal.events.to_string_lossy(),
            }),
            generation: 1,
        })
    }

    fn attach(&self, session: &SessionRef) -> Result<SessionRef> {
        match self.entry_of(session) {
            Some(entry) if !entry.agent.is_gone() => Ok(session.clone()),
            Some(entry) => Err(anyhow::anyhow!(
                "acp session {} is gone (agent {} exited)",
                entry.id,
                entry.agent_key
            )),
            None => Err(anyhow::anyhow!(
                "acp session {} is not held by this client",
                session.task_id
            )),
        }
    }

    fn probe(&self, session: &SessionRef) -> Result<ResourceProbe> {
        let Some(entry) = self.entry_of(session) else {
            // A reference this client no longer holds. An ACP agent cannot be
            // adopted across a client restart, so the process that answered for it
            // is gone with the pipe it spoke on.
            return Ok(ResourceProbe {
                alive: false,
                attached: false,
                detail: Some(json!({"reason": "no agent handle in this client"})),
            });
        };
        let gone = entry.agent.is_gone();
        Ok(ResourceProbe {
            alive: !gone,
            attached: !gone,
            detail: Some(json!({
                "pid": entry.agent.pid(),
                "acp_session": entry.id,
                "turn": entry.turn.phase.lock().live,
            })),
        })
    }

    fn deliver(&self, session: &SessionRef, task_id: &str, prose: &str) -> Result<()> {
        let entry = self.entry_of(session).ok_or_else(|| {
            anyhow::anyhow!(
                "acp: no live session for task {task_id}; its agent is not running here"
            )
        })?;
        if entry.agent.is_gone() {
            return Err(anyhow::anyhow!(
                "acp: agent {} exited before task {task_id} was delivered",
                entry.agent_key
            ));
        }
        // The claim happens before the thread starts, so a second delivery to a
        // session whose agent is still thinking is refused rather than queued
        // behind a turn it was never meant to join.
        if entry.turn.begin().is_none() {
            return Err(anyhow::anyhow!(
                "acp: session {} is still running a turn for task {}",
                entry.id,
                entry.current_task()
            ));
        }
        *entry.task_id.lock() = task_id.to_string();
        let thread_entry = Arc::clone(&entry);
        let sink = self.state.sink.clone();
        let content = self.state.content.clone();
        let task_id = task_id.to_string();
        // The prompt tells the agent where to leave its result before the turn
        // starts, so how a task ends never depends on the agent knowing that
        // onlyne exists. The whole text lands in the journal's dispatch
        // record, which is the operator's proof of what was asked. The agent
        // can only write where the directory already is, so the client makes
        // it first; a failure costs only the directive — the turn runs on the
        // task's prose, settles as a turn whose agent left no report, and
        // leaves a warning beside the dispatch record saying why.
        let warning = match std::fs::create_dir_all(payload_dir(&entry.workdir)) {
            Ok(()) => None,
            Err(error) => Some(format!("report directory not created: {error}")),
        };
        let prompt = match &warning {
            None => format!(
                "{}{}",
                prose,
                completion_directive(&entry.workdir, &task_id)
            ),
            Some(_) => prose.to_string(),
        };
        let policy = self.options.policy();
        if let Err(error) = thread::Builder::new()
            .name(format!("acp-turn {}", short(&task_id)))
            .spawn(move || {
                run_turn(
                    thread_entry,
                    sink,
                    content,
                    task_id,
                    prompt,
                    warning,
                    policy,
                )
            })
        {
            entry.turn.finish();
            *entry.task_id.lock() = session.task_id.clone();
            return Err(anyhow::anyhow!(
                "acp: task could not start its turn thread: {error}"
            ));
        }
        Ok(())
    }

    fn close(&self, session: &SessionRef, reason: CloseReason, _force: bool) -> Result<()> {
        let Some(entry) = self.take_entry(session) else {
            // Already closed, or closed by the client run that held the process
            // before this one. Either way there is nothing left to end.
            tracing::debug!(task = %session.task_id, ?reason, "acp session already gone");
            return Ok(());
        };
        let key = entry.agent_key.clone();
        let id = entry.id.clone();
        let generation = entry.turn.generation();
        if entry.turn.phase.lock().live {
            // A notification, so it cannot block: the agent ends the turn and
            // answers the parked prompt with `cancelled`, which is what lets the
            // session close on its own terms instead of being cut off mid-frame.
            if let Err(error) = entry.agent.cancel(&id) {
                tracing::warn!(error = %error, acp_session = %id, "acp: cancel was not sent");
            }
        }
        // Everything past this point waits on the agent, and every caller of
        // `close` in the client holds its one dispatch lock — a settled task
        // reaches here through `release_locked`, and so does the shutdown sweep,
        // whose whole budget exists because a slow backend must not park a
        // SIGTERM'd client. So the protocol close, the wait for the running turn,
        // and the process teardown all go to a thread that holds nothing.
        let state = Arc::clone(&self.state);
        let (closing, reap_key) = (id.clone(), key.clone());
        if let Err(error) = thread::Builder::new()
            .name(format!("acp-close {closing}"))
            .spawn(move || {
                if !entry.turn.waited_out(generation, TURN_CLOSE_BUDGET) {
                    tracing::warn!(
                        acp_session = %closing,
                        "acp: the turn outlasted its close budget; the agent decides its end"
                    );
                }
                end_session(&entry);
                state.retire(&reap_key);
            })
        {
            tracing::warn!(
                error = %error,
                acp_session = %id,
                "acp: no closer thread; the agent decides its own session's end"
            );
            self.state.retire(&key);
        }
        tracing::info!(
            task = %session.task_id,
            acp_session = %id,
            ?reason,
            "acp session closed"
        );
        Ok(())
    }
}

/// End the ACP session on the agent, leaving the process for its other sessions.
///
/// This runs on the closer thread, so nothing it reports can reach a caller: an
/// agent that will not end its own session is logged, not failed back, because the
/// client's bookkeeping has already let the session go.
fn end_session(entry: &SessionEntry) {
    if entry.agent.is_gone() {
        // The session went with the process.
        return;
    }
    if !entry
        .agent
        .negotiated()
        .is_some_and(|caps| caps.supports_close())
    {
        // The agent never advertised `session/close`. Our handle going away is all
        // this client can do; the agent decides its own session's fate.
        tracing::debug!(acp_session = %entry.id, "acp: agent offers no session/close");
        return;
    }
    match entry.agent.close_session(&entry.id) {
        Ok(()) => {}
        Err(_) if entry.agent.is_gone() => {}
        // A refusal that specific is an agent that already forgot the session,
        // which is the state this call was asked to reach.
        Err(error) if tolerated(&error) => {
            tracing::debug!(error = %error, acp_session = %entry.id, "acp: close refused");
        }
        Err(error) => {
            tracing::warn!(error = %error, acp_session = %entry.id, "acp: close reported");
        }
    }
}

/// Whether an error means the agent has nothing left for this call to close.
fn tolerated(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<onlyne_acp::RpcError>()
        .is_some_and(|error| {
            error.code == onlyne_acp::RpcError::METHOD_NOT_FOUND
                || error.code == onlyne_acp::RpcError::INVALID_PARAMS
        })
}

/// A task id is long, and a thread name is read by an operator.
fn short(id: &str) -> &str {
    let from = id.len().saturating_sub(8);
    id.get(from..).unwrap_or(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// A scripted ACP v1 agent, used as the role's `session_command`. It is a
    /// python child because the thing under test is a protocol on a pipe: a mock
    /// in this crate could not show that an id must be echoed back exactly, that a
    /// frame the agent never answers has to fail its caller, or that a process can
    /// die in the middle of a turn.
    ///
    /// Markers in the prompt text choose what the turn does, so one script covers
    /// every ending the backend has to map, and the agent's chosen answer is echoed
    /// back as its closing message — which is how a test reads what this client
    /// decided without a second channel.
    const FAKE_AGENT: &str = r##"#!/usr/bin/env python3
"""A scripted ACP v1 agent for the onlyne-session acp backend tests."""
import json, os, sys

TRACE = os.environ.get("ACP_TRACE", "")
SESSIONS = [0]


def trace(line):
    if TRACE:
        with open(TRACE, "a") as fh:
            fh.write(line + "\n")
            fh.flush()


def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def result(rid, value):
    send({"jsonrpc": "2.0", "id": rid, "result": value})


def failure(rid, code, message):
    send({"jsonrpc": "2.0", "id": rid, "error": {"code": code, "message": message}})


def update(sid, body):
    send({"jsonrpc": "2.0", "method": "session/update",
          "params": dict({"sessionId": sid}, **body)})


def chunk(sid, text):
    update(sid, {"sessionUpdate": "agent_message_chunk",
                 "content": {"type": "text", "text": text}})


def read_line():
    line = sys.stdin.readline()
    if not line:
        return None
    try:
        return json.loads(line)
    except ValueError:
        return {}


def await_reply(rid):
    """Read until the client answers our request. A single-threaded agent can
    afford to block on it, which is exactly what makes a refused ask observable."""
    while True:
        msg = read_line()
        if msg is None:
            sys.exit(0)
        if msg.get("id") == rid and ("result" in msg or "error" in msg):
            return msg


def turn(rid, sid, prompt):
    if "MARK:die" in prompt:
        # Say something on the way out: the journal of a turn the process did not
        # finish is still the operator's only record of it.
        update(sid, {"sessionUpdate": "agent_thought_chunk",
                     "content": {"type": "text", "text": "Reading the file.\n"}})
        sys.stderr.write("boom: the fake agent gives up\n")
        sys.stderr.flush()
        os._exit(7)
    closing = "I edited hello.py."
    if "MARK:ask" in prompt or "MARK:askalways" in prompt:
        if "MARK:askalways" in prompt:
            options = [{"optionId": "always", "kind": "allow_always", "name": "Always allow"}]
        else:
            options = [{"optionId": "once", "kind": "allow_once", "name": "Allow once"},
                       {"optionId": "always", "kind": "allow_always", "name": "Always"},
                       {"optionId": "no", "kind": "reject_once", "name": "Reject once"}]
        send({"jsonrpc": "2.0", "id": "perm-1", "method": "session/request_permission",
              "params": {"sessionId": sid,
                         "toolCall": {"toolCallId": "call_9", "title": "Edit hello.py",
                                      "kind": "edit", "status": "pending"},
                         "options": options}})
        reply = await_reply("perm-1")
        outcome = (reply.get("result") or {}).get("outcome") or {}
        chosen = outcome.get("optionId") or outcome.get("outcome") or "nothing"
        trace("answered %s" % chosen)
        closing = "answer=%s" % chosen
    update(sid, {"sessionUpdate": "agent_thought_chunk",
                 "content": {"type": "text", "text": "Let me "}})
    update(sid, {"sessionUpdate": "agent_thought_chunk",
                 "content": {"type": "text", "text": "try.\n"}})
    update(sid, {"sessionUpdate": "available_commands_update",
                 "availableCommands": [{"name": "/cmd%d" % i} for i in range(120)]})
    update(sid, {"sessionUpdate": "tool_call", "toolCallId": "call_9", "status": "pending",
                 "title": "Edit hello.py", "kind": "edit"})
    update(sid, {"sessionUpdate": "tool_call_update", "toolCallId": "call_9",
                 "status": "completed"})
    if "MARK:noise" in prompt:
        # Another session sharing this process: its text belongs to its journal.
        chunk("sess-other", "not this turn's answer")
    if "MARK:noanswer" not in prompt:
        chunk(sid, closing + "\n")
        chunk(sid, "<status>done")
    if "MARK:nostop" in prompt:
        return result(rid, {})
    stop = "end_turn"
    for marker, reason in (("MARK:cancel", "cancelled"), ("MARK:maxtokens", "max_tokens"),
                           ("MARK:refusal", "refusal"), ("MARK:weird", "stopped_by_hook")):
        if marker in prompt:
            stop = reason
    result(rid, {"stopReason": stop})


def dispatch(msg):
    method, rid = msg.get("method"), msg.get("id")
    params = msg.get("params") or {}
    if method == "initialize":
        caps = {"protocolVersion": 1, "agentInfo": {"name": "fake-acp", "version": "0"},
                "authMethods": []}
        if not os.environ.get("ACP_NO_CLOSE"):
            caps["agentCapabilities"] = {"sessionCapabilities": {"close": {}}}
        result(rid, caps)
    elif method == "session/new":
        SESSIONS[0] += 1
        sid = "sess-%d" % SESSIONS[0]
        trace("new %s" % sid)
        result(rid, {"sessionId": sid, "modes": {"currentModeId": "default"},
                     "models": {"currentModelId": "fast"}, "configOptions": []})
    elif method == "session/set_mode":
        trace("set_mode %s" % params.get("modeId"))
        if os.environ.get("ACP_REJECT_CONFIG"):
            failure(rid, -32602, "no such mode")
        else:
            result(rid, {})
    elif method == "session/set_config_option":
        trace("set_config_option %s=%s" % (params.get("configId"), params.get("value")))
        if os.environ.get("ACP_REJECT_CONFIG"):
            failure(rid, -32602, "no such option")
        else:
            result(rid, {})
    elif method == "session/close":
        trace("close %s" % params.get("sessionId"))
        result(rid, {})
    elif method == "session/prompt":
        prompt = "".join(block.get("text", "") for block in params.get("prompt") or [])
        trace("prompt %s" % prompt.replace("\n", " / "))
        turn(rid, params.get("sessionId"), prompt)
    elif method == "session/cancel":
        trace("cancel")
    elif rid is not None:
        failure(rid, -32601, "fake agent does not implement %s" % method)


def main():
    trace("start pid %d" % os.getpid())
    while True:
        msg = read_line()
        if msg is None:
            trace("eof")
            return
        if msg:
            dispatch(msg)


main()
"##;

    /// One tempdir per fake agent: the script, the trace, and the workspace whose
    /// `.onlyne/logs` the backend journals into.
    struct Fake {
        root: TempDir,
        script: PathBuf,
        trace: PathBuf,
    }

    impl Fake {
        fn new() -> Self {
            let root = TempDir::new().expect("a temp workspace for the fake agent");
            let script = root.path().join("fake_agent.py");
            fs::write(&script, FAKE_AGENT).expect("write the fake agent script");
            Fake {
                trace: root.path().join("agent.trace"),
                root,
                script,
            }
        }

        fn command(&self) -> Vec<String> {
            // argv, not a shell string. `-u` keeps python from buffering a frame
            // the test is waiting on.
            vec![
                "python3".to_string(),
                "-u".to_string(),
                self.script.to_string_lossy().to_string(),
            ]
        }

        fn spec(&self, task: &str) -> SpawnSpec {
            SpawnSpec {
                cwd: self.root.path().to_path_buf(),
                task_id: task.to_string(),
                command: self.command(),
                env: BTreeMap::from([("ACP_TRACE".to_string(), self.trace_display())]),
                focus: None,
                placement: None,
                rename: None,
            }
        }

        fn trace_display(&self) -> String {
            self.trace.to_string_lossy().to_string()
        }

        fn traced(&self) -> String {
            fs::read_to_string(&self.trace).unwrap_or_default()
        }
    }

    fn journal(session: &SessionRef, key: &str) -> PathBuf {
        PathBuf::from(
            session
                .backend_ref
                .get(key)
                .and_then(Value::as_str)
                .expect("the ref carries its journal paths"),
        )
    }

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
    }

    /// A turn that sent nothing renderable leaves no log at all, which is not a
    /// failure: the journal is created by its first write.
    fn read_or_empty(path: &Path) -> String {
        fs::read_to_string(path).unwrap_or_default()
    }

    fn jsonl(path: &Path) -> Vec<Value> {
        read(path)
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str(line)
                    .unwrap_or_else(|error| panic!("bad JSONL line {line}: {error}"))
            })
            .collect()
    }

    fn onlyne_records(lines: &[Value], kind: &str) -> Vec<Value> {
        lines
            .iter()
            .filter_map(|line| line.get("onlyne"))
            .filter(|record| record.get("kind").and_then(Value::as_str) == Some(kind))
            .cloned()
            .collect()
    }

    fn await_outcome(feed: &OutcomeFeed, task: &str) -> SessionOutcome {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if let Some(outcome) = feed.recv_timeout(Duration::from_millis(500)) {
                assert_eq!(outcome.task_id, task, "a fact arrived for another task");
                return outcome;
            }
        }
        panic!("no outcome for {task} within 20s");
    }

    /// Run one turn to its reported ending and hand back the outcome with both
    /// journal files as written for it.
    fn run_turn(
        backend: &AcpBackend,
        session: &SessionRef,
        task: &str,
        prose: &str,
    ) -> (SessionOutcome, Vec<Value>, String) {
        let feed = backend
            .outcomes()
            .expect("an acp backend reports its own endings");
        let events = journal(session, "events");
        let log = journal(session, "log");
        backend
            .deliver(session, task, prose)
            .unwrap_or_else(|error| panic!("deliver {task}: {error}"));
        let outcome = await_outcome(&feed, task);
        // The journal is written before the fact is handed over, so a complete
        // file is observable in the same moment the outcome is. `run_turn` ends
        // there, so the whole file is settled by the time the report is read.
        (outcome, jsonl(&events), read_or_empty(&log))
    }

    /// Close every session of one test and wait for its agents to go, so a fake
    /// agent process never outlives the test that started it.
    fn finish(backend: &AcpBackend, fake: &Fake, sessions: &[&SessionRef]) {
        for session in sessions {
            backend
                .close(session, CloseReason::Completed, false)
                .expect("close succeeds");
        }
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if backend.state.agents.lock().is_empty() && backend.state.sessions.lock().is_empty() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!(
            "agent processes outlived the test: {:?}\ntrace: {}",
            backend.state.agents.lock().keys().collect::<Vec<_>>(),
            fake.traced()
        );
    }

    #[test]
    fn a_spawned_acp_session_names_the_agent_it_runs_on() {
        let fake = Fake::new();
        let backend = AcpBackend::new(AcpOptions::default());
        let session = backend.spawn(fake.spec("t-1")).expect("spawn");
        assert_eq!(session.backend, "acp");
        assert_eq!(session.task_id, "t-1");
        assert_eq!(session.backend_ref["id"], "sess-1");
        assert!(session.backend_ref["pid"].as_u64().unwrap() > 1);
        assert_eq!(session.backend_ref["agent"], fake.command().join(" "));
        assert!(backend.self_driven());
        assert!(backend.outcomes().is_some());
        assert!(backend.probe(&session).unwrap().alive);
        assert!(!backend.capabilities().rename);
        assert!(backend.attach(&session).expect("the live session attaches") == session);
        finish(&backend, &fake, &[&session]);
    }

    #[test]
    fn a_turn_journals_its_asking_its_answer_and_its_ending() {
        let fake = Fake::new();
        let backend = AcpBackend::new(AcpOptions::default());
        let session = backend.spawn(fake.spec("t-journal")).unwrap();
        let (outcome, lines, log) = run_turn(&backend, &session, "t-journal", "fix the bug");

        assert_eq!(outcome.outcome, Outcome::Done);
        assert_eq!(outcome.head.as_deref(), Some("I edited hello.py."));
        assert!(outcome.note.is_none());
        assert!(outcome.refusals.is_none());

        // Our two records bracket the agent's stream, in the order a reader needs:
        // what was asked, everything the agent sent, how the turn ended.
        let dispatch = onlyne_records(&lines, "dispatch");
        assert_eq!(dispatch.len(), 1, "{lines:?}");
        assert_eq!(dispatch[0]["task_id"], "t-journal");
        // The dispatch record is the whole prompt: the task's prose with this
        // backend's completion directive appended.
        let prose = dispatch[0]["prose"].as_str().expect("a prompt is text");
        assert!(prose.starts_with("fix the bug"), "{prose}");
        assert!(
            prose.contains("Result report (write before you stop): "),
            "{prose}"
        );
        assert!(dispatch[0]["at"].as_str().unwrap().ends_with('Z'));
        assert!(lines[0].get("onlyne").is_some());
        let turn = onlyne_records(&lines, "turn");
        assert_eq!(turn.len(), 1);
        assert_eq!(turn[0]["stop_reason"], "end_turn");
        assert_eq!(turn[0]["head"], "I edited hello.py.");
        assert_eq!(lines.last().unwrap()["onlyne"]["kind"], "turn");
        assert_eq!(
            lines[1..lines.len() - 1]
                .iter()
                .filter(|line| line.get("onlyne").is_none())
                .count(),
            7,
            "every raw update of the turn is recorded: {lines:?}"
        );
        assert!(lines[1]["sessionUpdate"].is_string());
        // The raw frames keep the agent's own sessionId, which is what lets a
        // reader demultiplex a shared process.
        assert_eq!(lines[1]["sessionId"], "sess-1");
        assert!(
            lines
                .iter()
                .any(|line| line["sessionUpdate"] == "available_commands_update")
        );

        // The rendered surface an operator tails.
        assert!(log.contains("> Let me try.\n"), "{log}");
        assert!(
            log.contains("tool call_9 Edit hello.py kind=edit\n"),
            "{log}"
        );
        assert!(log.contains("tool call_9 status=completed\n"), "{log}");
        assert!(log.contains("I edited hello.py.\n"), "{log}");
        assert!(
            !log.contains("<status>"),
            "markers stay out of the log: {log}"
        );
        assert!(!log.contains("availableCommands"), "{log}");
        assert!(!log.contains("fix the bug"), "the log is the agent's half");
        finish(&backend, &fake, &[&session]);
    }

    #[test]
    fn another_sessions_text_stays_out_of_this_turns_journal() {
        let fake = Fake::new();
        let backend = AcpBackend::new(AcpOptions::default());
        let session = backend.spawn(fake.spec("t-noise")).unwrap();
        let (outcome, lines, log) = run_turn(&backend, &session, "t-noise", "MARK:noise go");
        assert_eq!(outcome.outcome, Outcome::Done);
        assert!(!log.contains("not this turn"), "{log}");
        assert!(
            !lines.iter().any(|line| line["sessionId"] == "sess-other"),
            "{lines:?}"
        );
        finish(&backend, &fake, &[&session]);
    }

    #[test]
    fn a_permission_ask_is_refused_and_the_turn_still_reports_its_answer() {
        let fake = Fake::new();
        let backend = AcpBackend::new(AcpOptions::default());
        let session = backend.spawn(fake.spec("t-deny")).unwrap();
        let (outcome, _lines, log) = run_turn(&backend, &session, "t-deny", "MARK:ask go");

        assert_eq!(outcome.outcome, Outcome::Done, "{outcome:?}");
        // The agent's own account of the answer is the closing line.
        assert_eq!(outcome.head.as_deref(), Some("answer=no"));
        assert!(log.contains("answer=no"), "{log}");
        let refusals = outcome.refusals.expect("the refusal is reported");
        assert!(
            refusals.contains("1 permission ask(s) refused"),
            "{refusals}"
        );
        assert!(refusals.contains("policy=deny"), "{refusals}");
        assert!(refusals.contains("Edit hello.py"), "{refusals}");
        assert!(refusals.contains("option=reject_once"), "{refusals}");
        assert!(fake.traced().contains("answered no"));
        finish(&backend, &fake, &[&session]);
    }

    #[test]
    fn an_allowing_client_grants_once_and_never_forever() {
        let fake = Fake::new();
        let backend = AcpBackend::new(AcpOptions {
            allow_permissions: true,
            ..AcpOptions::default()
        });
        let session = backend.spawn(fake.spec("t-allow")).unwrap();
        let (outcome, _lines, _log) = run_turn(&backend, &session, "t-allow", "MARK:ask go");
        assert_eq!(outcome.head.as_deref(), Some("answer=once"));
        assert!(outcome.refusals.is_none(), "{:?}", outcome.refusals);
        assert!(fake.traced().contains("answered once"));
        finish(&backend, &fake, &[&session]);
    }

    #[test]
    fn a_blanket_grant_is_never_this_clients_choice() {
        let fake = Fake::new();
        let backend = AcpBackend::new(AcpOptions {
            allow_permissions: true,
            ..AcpOptions::default()
        });
        let session = backend.spawn(fake.spec("t-always")).unwrap();
        let (outcome, _lines, _log) = run_turn(&backend, &session, "t-always", "MARK:askalways go");
        // Only `allow_always` was on offer, and an autonomy decision belongs to
        // the operator through the session mode, not to a client default.
        assert_eq!(outcome.head.as_deref(), Some("answer=cancelled"));
        let refusals = outcome.refusals.expect("declining is recorded");
        assert!(refusals.contains("declined Edit hello.py"), "{refusals}");
        assert!(refusals.contains("option=-"), "{refusals}");
        assert!(
            !fake.traced().contains("answered always"),
            "{}",
            fake.traced()
        );
        finish(&backend, &fake, &[&session]);
    }

    #[test]
    fn refusals_belong_to_the_turn_that_asked() {
        let fake = Fake::new();
        let backend = AcpBackend::new(AcpOptions::default());
        let session = backend.spawn(fake.spec("t-two")).unwrap();
        let (first, _lines, _log) = run_turn(&backend, &session, "t-two", "MARK:ask first");
        assert!(first.refusals.is_some());
        // A `reuse` role moves its next task onto the same conversation; the
        // previous task's refusal must not be reported against this one.
        let (second, _lines, _log) = run_turn(&backend, &session, "t-two", "second");
        assert_eq!(second.outcome, Outcome::Done);
        assert!(second.refusals.is_none(), "{:?}", second.refusals);
        finish(&backend, &fake, &[&session]);
    }

    #[test]
    fn every_stop_reason_that_is_not_an_answer_settles_a_turn() {
        // (prompt, what the ledger records, task suffix, the reason word the turn
        // record carries, and what the fault note has to say — `None` meaning the
        // ending is clean enough to need no explanation)
        let cases = [
            (
                "MARK:cancel",
                Outcome::Cancelled,
                "cancelled",
                "cancelled",
                Some("cancelled"),
            ),
            (
                "MARK:maxtokens",
                Outcome::Failed,
                "over-tokens",
                "max_tokens",
                Some("max_tokens"),
            ),
            (
                "MARK:refusal",
                Outcome::Failed,
                "refused",
                "refusal",
                Some("refusal"),
            ),
            (
                "MARK:weird",
                Outcome::Failed,
                "unknown-reason",
                "stopped_by_hook",
                Some("stopped_by_hook"),
            ),
            (
                "MARK:nostop",
                Outcome::Failed,
                "no-stop-reason",
                "(absent)",
                Some("(absent)"),
            ),
            (
                "MARK:noanswer MARK:nostop",
                Outcome::Failed,
                "silent-and-cut",
                "(absent)",
                Some("no answer"),
            ),
            (
                "MARK:noanswer",
                Outcome::Done,
                "silent-answer",
                "end_turn",
                None,
            ),
        ];
        let fake = Fake::new();
        let backend = AcpBackend::new(AcpOptions::default());
        for (marker, settled, name, reason, note) in cases {
            let task = format!("t-{name}");
            let session = backend.spawn(fake.spec(&task)).unwrap();
            let (outcome, lines, _log) = run_turn(&backend, &session, &task, marker);
            assert_eq!(outcome.outcome, settled, "{marker}");
            let record = &onlyne_records(&lines, "turn")[0];
            assert_eq!(record["stop_reason"], reason, "{marker}");
            // The head is what the receiving role reads as this turn's answer.
            let head = record.get("head").and_then(Value::as_str);
            if name.starts_with("silent") {
                assert_eq!(head, None, "{marker}");
            } else {
                assert_eq!(head, Some("I edited hello.py."), "{marker}");
            }
            match note {
                None => assert!(outcome.note.is_none(), "{marker}: {:?}", outcome.note),
                Some(want) => assert!(
                    outcome
                        .note
                        .as_deref()
                        .is_some_and(|got| got.contains(want)),
                    "{marker}: {:?}",
                    outcome.note
                ),
            }
            backend
                .close(&session, CloseReason::Completed, false)
                .expect("close");
        }
        // One process served all seven ACP sessions, because the command never
        // changed, and every session has now been handed back.
        assert_eq!(
            fake.traced().matches("new sess-").count(),
            7,
            "{}",
            fake.traced()
        );
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline && !backend.state.agents.lock().is_empty() {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(backend.state.agents.lock().is_empty());
        assert!(
            fake.traced().contains("eof"),
            "the agent left with its client"
        );
    }

    /// One row of the payload matrix: the bytes pre-placed at the report path
    /// (nothing for the absent rows, which must behave exactly as a turn
    /// without the contract), the prose marker that chooses the agent's stop
    /// reason, and everything the ending has to leave behind. The note
    /// expectations match as substrings; the head is compared exactly.
    struct Cell {
        name: &'static str,
        payload: Option<&'static [u8]>,
        marker: &'static str,
        outcome: Outcome,
        head: Option<&'static str>,
        note: Option<&'static str>,
        payload_kind: &'static str,
    }

    #[test]
    fn a_payload_report_replaces_the_head_and_can_only_lower_the_ending() {
        const DONE_TEXT: &str = "payload says the edit landed";
        const FAIL_TEXT: &str = "payload says the hop was abandoned";
        const DONE_FILE: &[u8] = b"hop-done: payload says the edit landed\n";
        const FAIL_FILE: &[u8] = b"hop-failed: payload says the hop was abandoned\n";
        let cells = [
            Cell {
                name: "absent-end",
                payload: None,
                marker: "go",
                outcome: Outcome::Done,
                head: Some("I edited hello.py."),
                note: None,
                payload_kind: "absent",
            },
            Cell {
                name: "absent-cut",
                payload: None,
                marker: "MARK:maxtokens go",
                outcome: Outcome::Failed,
                head: Some("I edited hello.py."),
                note: Some("max_tokens"),
                payload_kind: "absent",
            },
            Cell {
                name: "done-end",
                payload: Some(DONE_FILE),
                marker: "go",
                outcome: Outcome::Done,
                head: Some(DONE_TEXT),
                note: None,
                payload_kind: "done",
            },
            // The heart of the contract: a cheerful report cannot promote a
            // turn the stop reason already vetoed.
            Cell {
                name: "done-cannot-promote",
                payload: Some(DONE_FILE),
                marker: "MARK:maxtokens go",
                outcome: Outcome::Failed,
                head: Some(DONE_TEXT),
                note: Some("max_tokens"),
                payload_kind: "done",
            },
            Cell {
                name: "failed-end",
                payload: Some(FAIL_FILE),
                marker: "go",
                outcome: Outcome::Failed,
                head: Some(FAIL_TEXT),
                note: Some(FAIL_TEXT),
                payload_kind: "failed",
            },
            Cell {
                name: "failed-cut",
                payload: Some(FAIL_FILE),
                marker: "MARK:maxtokens go",
                outcome: Outcome::Failed,
                head: Some(FAIL_TEXT),
                // Both halves of a cut-short turn survive into the one note the
                // fault record carries: what the stop reason proved, then what
                // the agent said.
                note: Some("max_tokens; the agent reported: payload says the hop was abandoned"),
                payload_kind: "failed",
            },
            Cell {
                name: "empty-file",
                payload: Some(b""),
                marker: "go",
                outcome: Outcome::Cancelled,
                head: None,
                note: Some("acp payload invalid: payload is empty"),
                payload_kind: "invalid",
            },
            Cell {
                name: "empty-file-cut",
                payload: Some(b""),
                marker: "MARK:maxtokens go",
                outcome: Outcome::Cancelled,
                head: None,
                note: Some("acp payload invalid: payload is empty"),
                payload_kind: "invalid",
            },
            Cell {
                name: "two-lines",
                payload: Some(b"hop-done: one\nhop-failed: two\n"),
                marker: "go",
                outcome: Outcome::Cancelled,
                head: None,
                note: Some("acp payload invalid: payload is more than one line"),
                payload_kind: "invalid",
            },
            Cell {
                name: "unknown-prefix",
                payload: Some(b"hop-tripped: neither form\n"),
                marker: "MARK:maxtokens go",
                outcome: Outcome::Cancelled,
                head: None,
                note: Some("acp payload invalid: unknown report prefix"),
                payload_kind: "invalid",
            },
            Cell {
                name: "empty-report",
                payload: Some(b"hop-done:\n"),
                marker: "go",
                outcome: Outcome::Cancelled,
                head: None,
                note: Some("acp payload invalid: hop-done carries no result"),
                payload_kind: "invalid",
            },
            Cell {
                name: "bare-prose",
                payload: Some(b"the agent typed its answer into the file\n"),
                marker: "MARK:maxtokens go",
                outcome: Outcome::Cancelled,
                head: None,
                note: Some("acp payload invalid: payload carries no report prefix"),
                payload_kind: "invalid",
            },
            Cell {
                name: "not-utf8",
                payload: Some(&[0xff, 0xf7, 0x00]),
                marker: "go",
                outcome: Outcome::Cancelled,
                head: None,
                note: Some("acp payload invalid: payload is not valid utf-8"),
                payload_kind: "invalid",
            },
        ];
        let fake = Fake::new();
        let backend = AcpBackend::new(AcpOptions::default());
        for cell in cells {
            let task = format!("t-{}", cell.name);
            let path = payload_path(fake.root.path(), &task);
            if let Some(bytes) = cell.payload {
                fs::create_dir_all(path.parent().expect("the report path names a file"))
                    .expect("create the report directory");
                fs::write(&path, bytes).expect("pre-place the report");
            }
            let session = backend.spawn(fake.spec(&task)).unwrap();
            let (outcome, lines, _log) = run_turn(&backend, &session, &task, cell.marker);
            assert_eq!(outcome.outcome, cell.outcome, "{}", cell.name);
            assert_eq!(outcome.head.as_deref(), cell.head, "{}", cell.name);
            match cell.note {
                None => assert!(outcome.note.is_none(), "{}: {:?}", cell.name, outcome.note),
                Some(want) => assert!(
                    outcome
                        .note
                        .as_deref()
                        .is_some_and(|got| got.contains(want)),
                    "{}: {:?}",
                    cell.name,
                    outcome.note
                ),
            }
            // The report is consumed on the read: a requeued task id starts
            // from nothing rather than from the last turn's words.
            assert!(
                !path.exists(),
                "{}: the report outlived its read",
                cell.name
            );
            let records = onlyne_records(&lines, "payload");
            assert_eq!(records.len(), 1, "{}: {lines:?}", cell.name);
            assert_eq!(records[0]["task_id"], task, "{}", cell.name);
            assert_eq!(
                records[0]["payload_kind"], cell.payload_kind,
                "{}",
                cell.name
            );
            assert_eq!(
                records[0]["path"],
                Value::from(path.display().to_string()),
                "{}",
                cell.name
            );
            let head = records[0].get("head").and_then(Value::as_str);
            assert_eq!(head, cell.head, "{}: {lines:?}", cell.name);
            // The turn record closes the journal with the same head.
            assert_eq!(
                onlyne_records(&lines, "turn")[0]
                    .get("head")
                    .and_then(Value::as_str),
                cell.head,
                "{}",
                cell.name
            );
            backend
                .close(&session, CloseReason::Completed, false)
                .expect("close");
        }
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline && !backend.state.agents.lock().is_empty() {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(backend.state.agents.lock().is_empty());
    }

    #[test]
    fn the_prompt_hands_the_agent_the_path_the_ending_reads() {
        let fake = Fake::new();
        let backend = AcpBackend::new(AcpOptions::default());
        let session = backend.spawn(fake.spec("t-path")).unwrap();
        let (_outcome, lines, _log) = run_turn(&backend, &session, "t-path", "carry on");
        let path = payload_path(fake.root.path(), "t-path");
        let prose = onlyne_records(&lines, "dispatch")[0]["prose"]
            .as_str()
            .expect("the prompt is recorded")
            .to_string();
        // The directive carries the absolute path this backend will read, and
        // the agent received the same text it is told it received.
        assert!(
            prose.contains(&format!(
                "Result report (write before you stop): {}",
                path.display()
            )),
            "{prose}"
        );
        assert!(
            fake.traced().contains(&format!(
                "Result report (write before you stop): {}",
                path.display()
            )),
            "{}",
            fake.traced()
        );
        // The client makes the directory before the agent is asked to write
        // into it: a fresh workspace has no `.onlyne/out` of its own.
        assert!(
            payload_dir(fake.root.path()).is_dir(),
            "the report directory must stand ready for the agent"
        );
        finish(&backend, &fake, &[&session]);
    }

    #[test]
    fn an_unbuildable_report_directory_costs_only_the_directive() {
        let fake = Fake::new();
        // `out` is a regular file: neither `create_dir_all` nor a report path
        // under it can exist.
        let onlyne = fake.root.path().join(".onlyne");
        fs::create_dir_all(&onlyne).expect("create the workspace state dir");
        fs::write(onlyne.join("out"), "not a directory").expect("block the report dir");
        let backend = AcpBackend::new(AcpOptions::default());
        let session = backend.spawn(fake.spec("t-nowrite")).unwrap();
        let (outcome, lines, _log) = run_turn(&backend, &session, "t-nowrite", "carry on");
        // The turn settles on its stop reason alone, exactly the branch a
        // missing report takes: no cancel conjured by the failed bookkeeping.
        assert_eq!(outcome.outcome, Outcome::Done);
        assert_eq!(outcome.head.as_deref(), Some("I edited hello.py."));
        assert!(outcome.note.is_none(), "{:?}", outcome.note);
        let prose = onlyne_records(&lines, "dispatch")[0]["prose"]
            .as_str()
            .expect("the prompt is recorded")
            .to_string();
        assert_eq!(prose, "carry on", "no directive with an unreachable path");
        let warnings = onlyne_records(&lines, "warning");
        assert_eq!(warnings.len(), 1, "{lines:?}");
        assert!(
            warnings[0]["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("report directory not created")),
            "{warnings:?}"
        );
        assert_eq!(
            onlyne_records(&lines, "payload")[0]["payload_kind"],
            "absent"
        );
        finish(&backend, &fake, &[&session]);
    }

    #[test]
    fn one_agent_process_serves_every_session_of_its_command() {
        let fake = Fake::new();
        let backend = AcpBackend::new(AcpOptions::default());
        let first = backend.spawn(fake.spec("t-a")).unwrap();
        let mut second_spec = fake.spec("t-b");
        second_spec.env.insert("UNUSED".to_string(), "1".into());
        let second = backend.spawn(second_spec).unwrap();
        assert_eq!(
            first.backend_ref["pid"], second.backend_ref["pid"],
            "the same argv shares one process"
        );
        assert_eq!(first.backend_ref["id"], "sess-1");
        assert_eq!(second.backend_ref["id"], "sess-2");

        // A command that renders per task is a different key, and gets its own
        // agent: legal, and the cost of a per-task identity in the argv.
        let mut own = fake.spec("t-c");
        own.command.push("--task=t-c".to_string());
        let third = backend.spawn(own).unwrap();
        assert_ne!(third.backend_ref["pid"], first.backend_ref["pid"]);
        assert_eq!(backend.state.agents.lock().len(), 2);

        // Closing one session of a shared process leaves the other usable.
        backend
            .close(&first, CloseReason::Completed, false)
            .expect("close one");
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline && !fake.traced().contains("close sess-1") {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(fake.traced().contains("close sess-1"), "{}", fake.traced());
        assert!(backend.probe(&second).unwrap().alive);
        let (outcome, _lines, _log) = run_turn(&backend, &second, "t-b", "still here");
        assert_eq!(outcome.outcome, Outcome::Done);
        assert_eq!(outcome.head.as_deref(), Some("I edited hello.py."));
        finish(&backend, &fake, &[&second, &third]);
    }

    #[test]
    fn an_agent_that_dies_mid_turn_fails_its_task_with_the_detail() {
        let fake = Fake::new();
        let backend = AcpBackend::new(AcpOptions::default());
        let session = backend.spawn(fake.spec("t-die")).unwrap();
        let (outcome, lines, log) = run_turn(&backend, &session, "t-die", "MARK:die go");

        assert_eq!(outcome.outcome, Outcome::Failed);
        assert_eq!(outcome.head, None);
        let note = outcome.note.expect("the death is explained");
        assert!(note.contains("7"), "{note}");
        assert!(note.contains("boom"), "{note}");
        let record = &onlyne_records(&lines, "turn")[0];
        assert_eq!(record["stop_reason"], "(error)");
        // The turn's updates were already routed before the pipe closed, so the
        // journal keeps what the agent managed to say.
        assert!(
            lines
                .iter()
                .any(|line| line["sessionUpdate"] == "agent_thought_chunk"),
            "{lines:?}"
        );
        assert!(log.contains("> Reading the file.\n"), "{log}");
        assert!(!backend.probe(&session).unwrap().alive);
        assert!(
            !backend
                .attach(&session)
                .expect_err("a dead agent cannot be attached")
                .to_string()
                .contains("not held"),
        );
        assert!(backend.state.agents.lock().is_empty());
        let _ = backend.close(&session, CloseReason::Fault, false);
    }

    #[test]
    fn a_blocked_journal_never_loses_a_turn() {
        let fake = Fake::new();
        // The rendered log's own path is a directory before the session starts, so
        // every append to it fails.
        let blocked = fake
            .root
            .path()
            .join(".onlyne")
            .join("logs")
            .join("session-t-blocked.log");
        fs::create_dir_all(&blocked).expect("make the log path a directory");
        let backend = AcpBackend::new(AcpOptions::default());
        let session = backend.spawn(fake.spec("t-blocked")).unwrap();
        let (outcome, _lines, _log) = run_turn(&backend, &session, "t-blocked", "carry on");
        assert_eq!(outcome.outcome, Outcome::Done);
        assert_eq!(outcome.head.as_deref(), Some("I edited hello.py."));
        finish(&backend, &fake, &[&session]);
    }

    #[test]
    fn a_configured_mode_and_model_are_applied_before_the_session_is_ready() {
        let fake = Fake::new();
        let backend = AcpBackend::new(AcpOptions {
            mode: "acceptEdits".into(),
            model: "qfmodel".into(),
            reasoning_effort: "high".into(),
            allow_permissions: false,
        });
        let session = backend.spawn(fake.spec("t-config")).unwrap();
        let traced = fake.traced();
        assert!(traced.contains("set_mode acceptEdits"), "{traced}");
        assert!(
            traced.contains("set_config_option model=qfmodel"),
            "{traced}"
        );
        assert!(
            traced.contains("set_config_option reasoning_effort=high"),
            "{traced}"
        );
        finish(&backend, &fake, &[&session]);
    }

    #[test]
    fn an_agent_that_refuses_the_configured_mode_refuses_the_session() {
        let fake = Fake::new();
        let backend = AcpBackend::new(AcpOptions {
            mode: "yolo".into(),
            ..AcpOptions::default()
        });
        let mut spec = fake.spec("t-reject");
        spec.env.insert("ACP_REJECT_CONFIG".to_string(), "1".into());
        let error = backend
            .spawn(spec)
            .expect_err("a mode the agent will not take is not a session");
        assert!(error.to_string().contains("rejected mode"), "{error}");
        assert!(fake.traced().contains("set_mode yolo"), "{}", fake.traced());
        // The reservation the failed open took is given back, and with it the
        // process, so a refusing agent cannot accumulate.
        assert!(backend.state.agents.lock().is_empty());
        assert!(backend.state.sessions.lock().is_empty());
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline && !fake.traced().contains("eof") {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(fake.traced().contains("eof"), "{}", fake.traced());
    }

    #[test]
    fn an_agent_without_session_close_leaves_the_call_closed() {
        let fake = Fake::new();
        let backend = AcpBackend::new(AcpOptions::default());
        let mut spec = fake.spec("t-noclose");
        spec.env.insert("ACP_NO_CLOSE".to_string(), "1".into());
        let session = backend.spawn(spec).unwrap();
        backend
            .close(&session, CloseReason::Completed, false)
            .expect("close succeeds without the method");
        assert!(!fake.traced().contains("close sess-"), "{}", fake.traced());
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline && !backend.state.agents.lock().is_empty() {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(backend.state.agents.lock().is_empty());
    }

    #[test]
    fn closing_a_session_this_client_does_not_hold_is_not_an_error() {
        let fake = Fake::new();
        let backend = AcpBackend::new(AcpOptions::default());
        let session = backend.spawn(fake.spec("t-gone")).unwrap();
        backend
            .close(&session, CloseReason::Cancelled, false)
            .expect("the first close");
        backend
            .close(&session, CloseReason::Cancelled, false)
            .expect("a second close is a no-op");
        assert!(
            backend
                .deliver(&session, "t-gone", "anything")
                .expect_err("a released session takes no payload")
                .to_string()
                .contains("no live session"),
        );
        finish(&backend, &fake, &[]);
    }

    #[test]
    fn a_session_with_no_command_and_a_busy_session_are_refused() {
        let fake = Fake::new();
        let backend = AcpBackend::new(AcpOptions::default());
        let mut none = fake.spec("t-empty");
        none.command = Vec::new();
        assert!(
            backend
                .spawn(none)
                .expect_err("nothing to run is not a session")
                .to_string()
                .contains("no session_command"),
        );

        let session = backend.spawn(fake.spec("t-busy")).unwrap();
        let feed = backend.outcomes().unwrap();
        backend
            .deliver(&session, "t-busy", "MARK:ask slow turn")
            .expect("the first delivery");
        // The agent parks its turn on the ask and this client refuses it, so the
        // turn is long enough to observe: a second payload for the same session is
        // refused, not queued behind a turn it was never meant to join.
        let busy = backend
            .deliver(&session, "t-other", "second")
            .expect_err("one turn at a time");
        assert!(busy.to_string().contains("still running a turn"), "{busy}");
        let outcome = await_outcome(&feed, "t-busy");
        assert_eq!(outcome.outcome, Outcome::Done);
        finish(&backend, &fake, &[&session]);
    }
}
