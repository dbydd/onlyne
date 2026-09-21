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

/// Stderr line and [`NoSupportedHost`] display when no host is selected. The
/// two hint lines name the config key and its accepted values, because the
/// failure an operator actually hits is a workspace configured for no host.
pub const NO_SUPPORTED_HOST: &str = "onlyne: no supported host detected; run inside herdr, orca, or zellij, or set ONLYNE_BACKEND\n\
onlyne: or set the backend in the client config: `backend = \"acp\"`, accepted: herdr|orca|zellij|exec|headless|acp|fake|auto (empty probes, ONLYNE_BACKEND wins)\n\
onlyne: an acp backend reads its agent from the `[acp]` table: mode, model, reasoning_effort, permission";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendName {
    Herdr,
    Orca,
    Zellij,
    Exec,
    Acp,
    Fake,
}

/// Every name an operator may put in `backend` or `ONLYNE_BACKEND`, the `auto`
/// probe and the `headless` alias included. A rejection names this list, because
/// the miss it answers is a typo the operator cannot otherwise see.
pub const BACKEND_NAMES: &str = "herdr|orca|zellij|exec|headless|acp|fake|auto";

impl BackendName {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Herdr => "herdr",
            Self::Orca => "orca",
            Self::Zellij => "zellij",
            Self::Exec => "exec",
            Self::Acp => "acp",
            Self::Fake => "fake",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "herdr" => Some(Self::Herdr),
            "orca" => Some(Self::Orca),
            "zellij" => Some(Self::Zellij),
            // `headless` is the operator-facing alias; projections keep `exec`.
            "exec" | "headless" => Some(Self::Exec),
            "acp" => Some(Self::Acp),
            "fake" => Some(Self::Fake),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionSource {
    Explicit,
    Env,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostDetection {
    pub backend: Option<BackendName>,
    pub source: SelectionSource,
    pub explicit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoSupportedHost;

impl std::fmt::Display for NoSupportedHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(NO_SUPPORTED_HOST)
    }
}

impl std::error::Error for NoSupportedHost {}

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
    pub outcome: TaskState,
    /// The agent's closing text, already stripped of the status markers an agent
    /// stamps into its own stream. Becomes the completion head.
    pub head: Option<String>,
    /// Which verdict line [`SessionOutcome::head`] came from: `done`, `failed`,
    /// or `blocked`. `None` when the head is the agent's closing message rather
    /// than a report line, which is every session that left no report. The
    /// reader that hands work on needs the word: a blocked task finished
    /// nothing, so its handoff lines route nowhere.
    #[serde(default)]
    pub head_kind: Option<String>,
    /// Fault detail for the ledger on a failure. An agent process that died
    /// mid-turn names its exit status and the tail of its stderr here.
    pub note: Option<String>,
    /// One-line summary of the permission asks this client refused during the
    /// turn, `None` when the agent asked for nothing. A refusal is recorded on
    /// the session row as a fault and settles nothing by itself: the turn kept
    /// running without the thing the agent wanted.
    pub refusals: Option<String>,
    /// The handoff lines this turn's report asked for, with a blocked verdict's
    /// lines already dropped. Empty is the common case: a report with no
    /// handoff line, or no report at all.
    #[serde(default)]
    pub handoffs: Vec<onlyne_proto::payload::Handoff>,
}
