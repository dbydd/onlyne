//! The `SessionBackend` surface: bring a session up on a shared agent process,
//! report what it is, hand it a payload, and let it go without parking the
//! caller.
//!
//! Every method here answers its caller and nothing else; the facts a session
//! produces arrive on [`crate::OutcomeSink`] from the turn and closer threads.

use crate::backend::*;
use parking_lot::Mutex;
use serde_json::json;
use std::thread;
use std::time::Duration;

use super::journal::Journal;
use super::state::{AcpBackend, AgentSlot, SessionEntry, Turn};
use super::turn::{completion_directive, payload_dir, run_turn};

/// How long the detached closer waits for a running turn before it lets the agent
/// decide the turn's end on its own.
const TURN_CLOSE_BUDGET: Duration = Duration::from_secs(60);

impl AcpBackend {
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
