//! The agent process: one per distinct rendered command, shared by every
//! session of that command, plus the permission responder that lives as long as
//! it does.
//!
//! Process discipline lives here — the handshake that decides whether a command
//! is a runtime at all, the reservation count that decides when a process is
//! surplus, and the thread that answers `session/request_permission` on a policy
//! rather than a fallback grant.

use crate::backend::*;
use crate::content::ContentWriter;
use onlyne_acp::{
    Agent, AgentOptions, ClientCapabilities, ClientInfo, Event, PermissionOption,
    PermissionOutcome, PermissionRequest,
};
use parking_lot::Mutex;
use std::path::Path;
use std::sync::Weak;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use super::journal::or_dash;
use super::state::{AcpBackend, AcpOptions, AgentSlot, State};

/// The client name reported in `initialize`.
const CLIENT_NAME: &str = "onlyne-client";

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
    pub(super) fn command_of(spec: &SpawnSpec) -> Result<Vec<String>> {
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
    pub(super) fn agent_for(
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
}

impl State {
    /// One session of this process went away. Past the last one the process goes
    /// with it, on a thread that can afford to wait for it: see [`State::reap`].
    pub(super) fn retire(&self, key: &str) {
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
