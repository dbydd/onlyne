//! What an ACP backend holds: the client's options, the handle that owns them,
//! and the per-process and per-session bookkeeping underneath it. No I/O.
//!
//! A session is named by the pair (agent command, agent-chosen id): ACP ids are
//! unique within one agent process, not across them.

use crate::backend::{OutcomeFeed, OutcomeSink};
use crate::content::ContentWriter;
use onlyne_acp::Agent;
use parking_lot::{Condvar, Mutex};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;

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
    pub(super) fn policy(&self) -> &'static str {
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
    pub(super) options: AcpOptions,
    pub(super) state: Arc<State>,
}

pub(super) struct State {
    /// Live agent processes, keyed by the rendered command that started them.
    pub(super) agents: Mutex<BTreeMap<String, Arc<AgentSlot>>>,
    /// Every session this client holds, keyed by the agent command and the id that
    /// agent chose for it: ACP ids are unique within a process, not across
    /// processes, so the pair is the stable name, where a task id is not.
    pub(super) sessions: Mutex<BTreeMap<(String, String), Arc<SessionEntry>>>,
    pub(super) sink: OutcomeSink,
    pub(super) feed: OutcomeFeed,
    /// Serializes journal cursors across every task served by this role.
    pub(super) content: ContentWriter,
}

pub(super) struct AgentSlot {
    pub(super) agent: Arc<Agent>,
    /// Sessions of this process still held by this client, so the last one to
    /// leave can take the process with it.
    pub(super) live: AtomicUsize,
}

/// One ACP session, plus the turn state this client keeps for it.
pub(super) struct SessionEntry {
    /// The task this session serves. One session runs one task, and that task
    /// owns its own journal.
    pub(super) task_id: Mutex<String>,
    /// The id the agent gave this session; every later request is keyed by it.
    pub(super) id: String,
    /// The command key of the process serving this session.
    pub(super) agent_key: String,
    /// The directory the agent runs in, which is where its journal lives.
    pub(super) workdir: PathBuf,
    pub(super) agent: Arc<Agent>,
    pub(super) turn: Turn,
    /// Permission asks refused since the turn began, written by the responder
    /// thread and taken by the turn thread when the turn ends. Per turn, because
    /// the asks interleave with the updates of the one parked prompt they belong
    /// to, and a refusal has to name the turn that produced it.
    pub(super) refusals: Mutex<Vec<String>>,
}

impl SessionEntry {
    pub(super) fn current_task(&self) -> String {
        self.task_id.lock().clone()
    }
}

/// The turn bookkeeping of one session: whether a turn is in flight, and which
/// turn a waiter is waiting out.
pub(super) struct Turn {
    pub(super) phase: Mutex<TurnPhase>,
    ended: Condvar,
}

pub(super) struct TurnPhase {
    pub(super) live: bool,
    generation: u64,
}

impl Turn {
    pub(super) fn new() -> Self {
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
    pub(super) fn begin(&self) -> Option<u64> {
        let mut phase = self.phase.lock();
        if phase.live {
            return None;
        }
        phase.live = true;
        phase.generation += 1;
        Some(phase.generation)
    }

    /// Mark the turn over and wake anyone waiting out the close of a session.
    pub(super) fn finish(&self) {
        self.phase.lock().live = false;
        self.ended.notify_all();
    }

    pub(super) fn generation(&self) -> u64 {
        self.phase.lock().generation
    }

    /// Whether the turn named by `generation` is over: true when it ended, false
    /// when the wait ran out. A newer generation counts as ended too, because a
    /// turn cannot start before the one before it stopped.
    pub(super) fn waited_out(&self, generation: u64, budget: Duration) -> bool {
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
