//! The `SessionBackend` surface: bring a session up on a shared agent process,
//! report what it is, hand it one delivery, and let it go without parking the
//! caller.
//!
//! Every method here answers its caller and nothing else; the facts a session
//! produces arrive on [`crate::OutcomeSink`] from the turn and closer threads.

use crate::backend::*;
use parking_lot::Mutex;
use serde_json::json;
use std::fs;
use std::path::Path;
use std::thread;
use std::time::Duration;

use super::journal::Journal;
use super::mcp;
use super::state::{AcpBackend, AgentSlot, SessionEntry, Turn};
use super::turn::run_turn;

/// How long the detached closer waits for a running turn before it lets the agent
/// decide the turn's end on its own.
const TURN_CLOSE_BUDGET: Duration = Duration::from_secs(60);

/// The instruction file a role's prose is written into.
///
/// The `agents.md` convention's own name, and the file this repository's own
/// world uses. It is a choice rather than a certainty: `onlyne-acp` carries no
/// instruction field, so a file is the only vehicle, and an agent that reads
/// another name — claude-code reads `CLAUDE.md` — will not see the prose until
/// the filename is wired to the spec's agent package, which is its own slice
/// (`docs/v2-CONTRACT.md` §3b, the known gap). Whoever wires it meets the
/// choice here, at the line that makes it.
const PROSE_FILE: &str = "AGENTS.md";

/// The marker pair one client-owned prose block sits between.
///
/// The block is this client's: the file belongs to the operator, and the two
/// markers are the only bytes of it this code may find and replace.
const PROSE_BEGIN: &str = "<!-- onlyne:role-prose:begin -->";
const PROSE_END: &str = "<!-- onlyne:role-prose:end -->";

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
        // The role's prose goes into the workspace before the session opens: an
        // agent that starts working the moment `session/new` returns still reads
        // the instructions this client put there for it.
        write_role_prose(&spec.cwd, &spec.prose)?;
        let start = slot.agent.new_session(&spec.cwd, vec![mcp::mount(spec)?])?;
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
            task_id: spec.task_id.clone(),
            id: start.session_id,
            agent_key: key.to_string(),
            process: slot.process,
            workdir: spec.cwd.clone(),
            agent: Arc::clone(&slot.agent),
            turn: Turn::new(),
            refusals: Mutex::new(Vec::new()),
        });
        self.state
            .sessions
            .lock()
            .insert(entry.key(), Arc::clone(&entry));
        tracing::info!(
            task = %spec.task_id,
            acp_session = %entry.id,
            pid = entry.agent.pid(),
            process = entry.process,
            agent = %key,
            "acp session opened"
        );
        Ok(entry)
    }

    /// The live session a stored reference names: by the id the agent chose for it
    /// under its own command and process first, by task id for a reference that
    /// carries neither. ACP session ids are only unique within one agent process,
    /// so two processes may hand out the same `sess-1` — a replacement of a
    /// crashed agent included — and the key carries the command and this client's
    /// name for the process for exactly that reason. A reference from a client run
    /// that no longer holds the process names nothing here.
    fn entry_of(&self, session: &SessionRef) -> Option<Arc<SessionEntry>> {
        let sessions = self.state.sessions.lock();
        let agent = session
            .backend_ref
            .get("agent")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let process = session.backend_ref.get("process").and_then(Value::as_u64);
        if let (Some(id), Some(process)) = (
            session.backend_ref.get("id").and_then(Value::as_str),
            process,
        ) && let Some(found) = sessions.get(&(agent.to_string(), process, id.to_string()))
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
        let key = found.key();
        self.state.sessions.lock().remove(&key)
    }

    /// The live session one delivery or nudge is addressed to.
    ///
    /// One session serves one task, and the id it serves was bound when it
    /// opened. A turn naming another task would journal itself under a task the
    /// agent was never assigned, so the two ids can never be reconciled
    /// afterwards; refuse it here, where the caller can still hear about it.
    fn live_entry(&self, session: &SessionRef, task_id: &str) -> Result<Arc<SessionEntry>> {
        let entry = self.entry_of(session).ok_or_else(|| {
            anyhow::anyhow!(
                "acp: no live session for task {task_id}; its agent is not running here"
            )
        })?;
        if entry.current_task() != task_id {
            return Err(anyhow::anyhow!(
                "acp: session {} serves task {}; task {task_id} needs a session of its own",
                entry.id,
                entry.current_task(),
            ));
        }
        if entry.agent.is_gone() {
            return Err(anyhow::anyhow!(
                "acp: agent {} exited before task {task_id} was delivered",
                entry.agent_key
            ));
        }
        Ok(entry)
    }

    /// Start one turn on its own thread, with `prompt` verbatim as the text the
    /// agent reads.
    ///
    /// The turn is claimed before the thread starts, so a second turn offered to
    /// a session whose agent is still thinking is refused rather than queued
    /// behind a turn it was never meant to join. `record` is the journal record
    /// this turn leaves — `dispatch` for a delivery, `nudge` for the sentence
    /// §3c sends — so an operator reading the file can tell which one it was.
    fn start_turn(
        &self,
        entry: &Arc<SessionEntry>,
        task_id: &str,
        prompt: String,
        record: &'static str,
    ) -> Result<()> {
        if entry.turn.begin().is_none() {
            return Err(anyhow::anyhow!(
                "acp: session {} is still running a turn for task {}",
                entry.id,
                entry.current_task()
            ));
        }
        let thread_entry = Arc::clone(entry);
        let sink = self.state.sink.clone();
        let content = self.state.content.clone();
        let policy = self.options.policy();
        if let Err(error) = thread::Builder::new()
            .name(format!("acp-turn {}", short(task_id)))
            .spawn(move || run_turn(thread_entry, sink, content, prompt, record, policy))
        {
            entry.turn.finish();
            return Err(anyhow::anyhow!(
                "acp: task could not start its turn thread: {error}"
            ));
        }
        Ok(())
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
                // The reservation this spawn took on the chosen process goes back
                // before the error does, so an agent that refuses its config is
                // not left running for a session that never existed.
                self.state.retire(&key, slot.process);
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
                "process": entry.process,
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

    /// Hand one delivery to its session as a turn.
    ///
    /// The prompt is the rendered delivery text and nothing else: this client
    /// appends no directive of its own, and the agent's own tools mount carries
    /// whatever it owes back. The whole text lands in the journal's `dispatch`
    /// record, which is the operator's proof of what was asked.
    fn deliver(&self, session: &SessionRef, task_id: &str, prompt: &str) -> Result<()> {
        let entry = self.live_entry(session, task_id)?;
        let task_id = entry.current_task();
        self.start_turn(&entry, &task_id, prompt.to_string(), "dispatch")
    }

    /// Tell a session whose turn ended without a completion, in the one words
    /// this client owns (`docs/v2-CONTRACT.md` §3c).
    ///
    /// An ACP agent has no injection channel besides `session/prompt`, so the
    /// sentence arrives as the next prompt, verbatim. It is not a delivery: no
    /// payload, prose, or attachment is re-sent, nothing of the session's turn
    /// bookkeeping is reset, and the journal names the record `nudge` so the two
    /// are told apart in the operator's file.
    fn nudge(&self, session: &SessionRef, task_id: &str, text: &str) -> Result<()> {
        let entry = self.live_entry(session, task_id)?;
        let task_id = entry.current_task();
        self.start_turn(&entry, &task_id, text.to_string(), "nudge")
    }

    fn close(&self, session: &SessionRef, reason: CloseReason, _force: bool) -> Result<()> {
        let Some(entry) = self.take_entry(session) else {
            // Already closed, or closed by the client run that held the process
            // before this one. Either way there is nothing left to end.
            tracing::debug!(task = %session.task_id, ?reason, "acp session already gone");
            return Ok(());
        };
        let key = entry.agent_key.clone();
        let process = entry.process;
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
                state.retire(&reap_key, process);
            })
        {
            tracing::warn!(
                error = %error,
                acp_session = %id,
                "acp: no closer thread; the agent decides its own session's end"
            );
            self.state.retire(&key, process);
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

/// Write the role's prose into the workspace's instruction file.
///
/// The block between the markers is this client's and the rest of the file is
/// the operator's: a block that is there is replaced where it stands, a file
/// without one gets it appended as its own paragraph, and not a byte outside
/// the markers is touched either way. Prose the role no longer has takes its
/// block out, because a block that stays is prose this client would be standing
/// behind after it stopped.
///
/// A read that fails for any reason but absence is an error: this runs while a
/// session opens, and a spawn that cannot write the instructions it was asked
/// to write must not report a session that will never read them.
fn write_role_prose(workdir: &Path, prose: &str) -> Result<()> {
    let path = workdir.join(PROSE_FILE);
    let existing = match fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(anyhow::anyhow!(
                "acp: {} could not be read: {error}",
                path.display()
            ));
        }
    };
    let block = (!prose.trim().is_empty()).then(|| format!("{PROSE_BEGIN}\n{prose}\n{PROSE_END}"));
    let next = match existing {
        None => match block {
            Some(block) => format!("{block}\n"),
            None => return Ok(()),
        },
        Some(text) => match block_span(&text) {
            Some((start, stop)) => match block {
                Some(block) => format!("{}{block}{}", &text[..start], &text[stop..]),
                None => format!("{}{}", &text[..start], &text[stop..]),
            },
            None => match block {
                Some(block) => append_block(&text, &block),
                None => return Ok(()),
            },
        },
    };
    fs::write(&path, next)
        .map_err(|error| anyhow::anyhow!("acp: {} could not be written: {error}", path.display()))
}

/// The byte span one client-owned block occupies, its markers and their own
/// line endings included.
///
/// The span starts at the beginning of the line the begin marker sits on, so a
/// replacement cannot leave the marker's own indentation behind. A begin marker
/// with no end marker after it owns the rest of the file: everything from this
/// client's own marker on is text this code wrote, and a half-written block is
/// replaced rather than doubled.
fn block_span(text: &str) -> Option<(usize, usize)> {
    let begin = text.find(PROSE_BEGIN)?;
    let start = text[..begin].rfind('\n').map(|at| at + 1).unwrap_or(0);
    let stop = match text[begin..].find(PROSE_END) {
        Some(at) => {
            let end = begin + at + PROSE_END.len();
            text[end..]
                .find('\n')
                .map(|at| end + at + 1)
                .unwrap_or(text.len())
        }
        None => text.len(),
    };
    Some((start, stop))
}

/// The file with the block added as its own paragraph.
fn append_block(text: &str, block: &str) -> String {
    let gap = if text.is_empty() || text.ends_with("\n\n") {
        ""
    } else if text.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    format!("{text}{gap}{block}\n")
}
