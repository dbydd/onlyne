use super::*;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub spawn: bool,
    pub attach: bool,
    pub probe: bool,
    pub close: bool,
    pub focus: bool,
    pub rename: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpawnSpec {
    pub cwd: PathBuf,
    pub task_id: String,
    pub command: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// The tools-mount token this client minted for the session being opened.
    ///
    /// A session the client drives itself has no plugin to carry its
    /// obligations, so this token is what binds the `onlyne mcp` child back to
    /// this one session (`docs/v2-CONTRACT.md` §3b). It travels here and not
    /// inside `env`, because `env` is the agent process's own environment: a
    /// capability the model can print is no capability, and the mount's env
    /// belongs to the tool child alone.
    #[serde(default)]
    pub tools_token: String,
    /// The role's control-plane prose, as the slice `welcome` brought it.
    ///
    /// A runtime with a system-prompt extension point is handed this through
    /// that point. An ACP session has none, so this is what the client writes
    /// into the workspace instruction file before the session opens
    /// (`AGENTS.md` §12).
    #[serde(default)]
    pub prose: String,
    #[serde(default)]
    pub focus: Option<bool>,
    #[serde(default)]
    pub placement: Option<PanePlacement>,
    #[serde(default)]
    pub rename: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitDirection {
    Right,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PanePlacement {
    pub direction: SplitDirection,
    pub ratio: f64,
}

impl SplitDirection {
    pub fn as_herdr(self) -> &'static str {
        match self {
            Self::Right => "right",
            Self::Down => "down",
        }
    }
}

impl PanePlacement {
    /// A split that brings the pane count to a power of two goes right.
    /// Every other split goes down. Ratio is always 0.5.
    pub fn from_pane_count(pane_count: usize) -> Self {
        let direction = if (pane_count + 1).is_power_of_two() {
            SplitDirection::Right
        } else {
            SplitDirection::Down
        };
        Self {
            direction,
            ratio: 0.5,
        }
    }
}

/// Where a session of this client runs: the placement the machine resolved, or
/// the in-process `fake` runtime.
///
/// Placement is a property of the machine and comes from the workspace
/// `config.toml` or from `ONLYNE_BACKEND`; a drive is a property of the runtime
/// and comes from the role's spec. `fake` is the backend that owns no process
/// and needs no external tool, which is how the scenario suite and every e2e
/// case run a real client on a machine with no terminal host at all. Only the
/// environment and an embedding that resolved the placement itself can name it:
/// a workspace config names a placement, and a runtime that starts nothing is
/// not one an operator should reach by typo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPlacement {
    Named(onlyne_config::Placement),
    Fake,
}

impl SessionPlacement {
    /// Two pre-split spellings are accepted here and nowhere else: `fake`
    /// selects the in-process test runtime, and `exec` is the name a v1
    /// `ONLYNE_BACKEND` used for a session the client runs in the background,
    /// which is the `headless` placement. A workspace config names neither: its
    /// key is `placement`, and the five names that key accepts are the ones
    /// `onlyne_config::PLACEMENT_NAMES` lists.
    pub fn parse(name: &str) -> Option<Self> {
        let trimmed = name.trim();
        if trimmed.eq_ignore_ascii_case("fake") {
            return Some(Self::Fake);
        }
        if trimmed.eq_ignore_ascii_case("exec") {
            return Some(Self::Named(onlyne_config::Placement::Headless));
        }
        onlyne_config::Placement::parse(trimmed).map(Self::Named)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Named(placement) => placement.as_str(),
            Self::Fake => "fake",
        }
    }

    /// The placement itself, `None` for the in-process runtime, which names no
    /// place on the machine.
    pub fn named(self) -> Option<onlyne_config::Placement> {
        match self {
            Self::Named(placement) => Some(placement),
            Self::Fake => None,
        }
    }
}

impl std::fmt::Display for SessionPlacement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The refusal an explicit `placement` name that matches nothing gets. It names
/// the accepted set and where each source sits in the precedence, because the
/// miss it answers is a typo the operator cannot otherwise see.
pub fn unknown_placement(name: &str) -> String {
    format!(
        "onlyne: `{name}` is not a placement; accepted: {} \
         (absent probes {}, then headless; ONLYNE_BACKEND wins over the workspace's `placement` key)",
        onlyne_config::PLACEMENT_NAMES,
        onlyne_config::PLACEMENT_PROBE_ORDER
            .map(|placement| placement.as_str())
            .join(", ")
    )
}

/// An explicit placement name this client does not know. `onlyne-client run`
/// answers it with exit 5, the code for "no host the client could use".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownPlacement(pub String);

impl std::fmt::Display for UnknownPlacement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&unknown_placement(&self.0))
    }
}

impl std::error::Error for UnknownPlacement {}

/// Which of the four sources answered the placement question. `doctor` reports
/// the word, because "which machine fact chose this" is the question an
/// operator actually has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionSource {
    /// A nonempty `ONLYNE_BACKEND` named it.
    Explicit,
    /// The run itself declared it: the role workspace's `config.toml`
    /// `placement` key, or what an embedding passed to `ClientInit`.
    Declared,
    /// A pane host answered the probe.
    Probe,
    /// No host answered, so the placement is `headless`.
    Fallback,
}

/// The placement this client resolved, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementDetection {
    pub placement: SessionPlacement,
    pub source: SelectionSource,
    /// The raw `ONLYNE_BACKEND` value when it named the placement, so a reader
    /// can see the spelling that won.
    pub explicit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRef {
    pub task_id: String,
    pub backend: String,
    pub backend_ref: Value,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceProbe {
    pub alive: bool,
    pub attached: bool,
    pub detail: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseReason {
    Completed,
    Cancelled,
    Fault,
    Shutdown,
    Replaced,
    Operator,
}

/// One terminal fact a self-driven backend observed for itself.
///
/// A session served by an adapter reports its own ending, and the client only
/// has to write it down. A backend that owns its agent has no reporter, so it
/// states the fact here: which task ended, how, and what the receiving role
/// should read as its closing line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionOutcome {
    pub task_id: String,
    /// How the turn ended, when ending the turn *is* the fact the client
    /// records.
    ///
    /// `None` is the ordinary ending of a session whose obligations travel as
    /// tool calls: the agent stopped asking for work (`end_turn`) and this
    /// backend has no way to know whether the model reported the task done,
    /// because the completion arrives on the session's own tools connection
    /// rather than through this process's pipe. Ending a turn without a
    /// completion is the state the client's turn-end rule owns
    /// (`docs/v2-CONTRACT.md` §3c): it nudges once, and the *second* ending is
    /// what settles the delivery. `Some` is a standing this backend did observe
    /// for itself — a cancelled, refused, or dead turn — and it settles the
    /// delivery where it lands.
    pub outcome: Option<TaskState>,
    /// The agent's closing text, already stripped of the status markers an agent
    /// stamps into its own stream. Becomes the completion head.
    pub head: Option<String>,
    /// Fault detail for the ledger on a failure. An agent process that died
    /// mid-turn names its exit status and the tail of its stderr here.
    pub note: Option<String>,
    /// One-line summary of the permission asks this client refused during the
    /// turn, `None` when the agent asked for nothing. A refusal is recorded on
    /// the session row as a fault and settles nothing by itself: the turn kept
    /// running without the thing the agent wanted.
    pub refusals: Option<String>,
}
