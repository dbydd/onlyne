//! State vocabulary: the four orthogonal session dimensions, the task state the
//! projection reads as an input, version, observation.
//!
//! The four phase vocabularies are the wire's own: `SessionProjection` and the
//! reducer read and write the same words, so one definition serves both.
//! `Display`/`FromStr` are that vocabulary's only spelling table, and the ledger
//! columns already hold these words — renaming one is a migration, not an edit.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::host::HostRef;

/// The error arm the four phase [`std::str::FromStr`] impls share.
///
/// Every reader of a stored word has nowhere better to go than its own default,
/// and each takes it with `unwrap_or`, so a column this vocabulary cannot read
/// would settle into a plausible-looking row and leave no trace. The word is
/// logged here before the `Err` returns: the fallback stays the caller's choice,
/// the record is this crate's.
fn unknown_phase(kind: &str, word: &str) -> String {
    warn!(
        phase = kind,
        input = word,
        "phase word is unknown to the protocol; the caller answers with its fallback"
    );
    format!("unknown {kind} phase {word:?}")
}

/// Agent-side lifecycle fact: the agent dimension of a session, the word the
/// ledger column stores, and the word the wire projection carries. One
/// definition serves both sides — the reducer's twin `AgentPhase` is gone.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
    Default,
)]
#[serde(rename_all = "snake_case")]
pub enum AgentPhase {
    #[default]
    Booting,
    Ready,
    Running,
    Idle,
    Gone,
}

impl AgentPhase {
    /// Total variant count. The lifecycle tests pin their enumeration arrays
    /// to this so a new variant fails fast instead of silently shrinking the
    /// exhaustive matrix.
    pub const VARIANT_COUNT: usize = 5;
}

/// The name↔variant table for one agent phase. `Display` is the only place a
/// variant is named: it matches exhaustively, so a new variant fails to compile
/// here rather than silently losing its word in one direction of the ledger
/// column that stores it. `FromStr` is the reading side and answers `Err` for
/// any word it does not know, leaving the caller's fallback to decide; the word
/// it could not read is logged first, by the shared `unknown_phase` arm below.
impl std::fmt::Display for AgentPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            AgentPhase::Booting => "booting",
            AgentPhase::Ready => "ready",
            AgentPhase::Running => "running",
            AgentPhase::Idle => "idle",
            AgentPhase::Gone => "gone",
        })
    }
}

impl std::str::FromStr for AgentPhase {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Ok(match name {
            "booting" => AgentPhase::Booting,
            "ready" => AgentPhase::Ready,
            "running" => AgentPhase::Running,
            "idle" => AgentPhase::Idle,
            "gone" => AgentPhase::Gone,
            other => return Err(unknown_phase("agent", other)),
        })
    }
}

/// Intent delivery fact for the current turn exit: the delivery dimension of a
/// session. `none` is the word for `NoIntent`, which is also its serde word.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
    Default,
)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryPhase {
    #[serde(rename = "none")]
    #[default]
    NoIntent,
    Pending,
    Retrying,
    Accepted,
    Exhausted,
}

impl DeliveryPhase {
    /// Total variant count. The lifecycle tests pin their enumeration arrays
    /// to this so a new variant fails fast instead of silently shrinking the
    /// exhaustive matrix.
    pub const VARIANT_COUNT: usize = 5;
}

/// [`AgentPhase`]'s table for the delivery dimension. `none` is the word for
/// `NoIntent`, which is also its serde word.
impl std::fmt::Display for DeliveryPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            DeliveryPhase::NoIntent => "none",
            DeliveryPhase::Pending => "pending",
            DeliveryPhase::Retrying => "retrying",
            DeliveryPhase::Accepted => "accepted",
            DeliveryPhase::Exhausted => "exhausted",
        })
    }
}

impl std::str::FromStr for DeliveryPhase {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Ok(match name {
            "none" => DeliveryPhase::NoIntent,
            "pending" => DeliveryPhase::Pending,
            "retrying" => DeliveryPhase::Retrying,
            "accepted" => DeliveryPhase::Accepted,
            "exhausted" => DeliveryPhase::Exhausted,
            other => return Err(unknown_phase("delivery", other)),
        })
    }
}

/// Backend resource fact (pane / tab / terminal handle): the resource dimension
/// of a session.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
    Default,
)]
#[serde(rename_all = "snake_case")]
pub enum ResourcePhase {
    #[default]
    Detached,
    Attached,
    Closing,
    Closed,
}

impl ResourcePhase {
    /// Total variant count. The lifecycle tests pin their enumeration arrays
    /// to this so a new variant fails fast instead of silently shrinking the
    /// exhaustive matrix.
    pub const VARIANT_COUNT: usize = 4;
}

/// [`AgentPhase`]'s table for the resource dimension.
impl std::fmt::Display for ResourcePhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ResourcePhase::Detached => "detached",
            ResourcePhase::Attached => "attached",
            ResourcePhase::Closing => "closing",
            ResourcePhase::Closed => "closed",
        })
    }
}

impl std::str::FromStr for ResourcePhase {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Ok(match name {
            "detached" => ResourcePhase::Detached,
            "attached" => ResourcePhase::Attached,
            "closing" => ResourcePhase::Closing,
            "closed" => ResourcePhase::Closed,
            other => return Err(unknown_phase("resource", other)),
        })
    }
}

/// Recovery substate carried alongside an idle or draining session. `none` is
/// the word for `NoRecovery`, which is also its serde word.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
    Default,
)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryPhase {
    #[serde(rename = "none")]
    #[default]
    NoRecovery,
    IdleWaiting,
    IdleFault,
    Draining,
}

impl RecoveryPhase {
    /// Total variant count. The lifecycle tests pin their enumeration arrays
    /// to this so a new variant fails fast instead of silently shrinking the
    /// exhaustive matrix.
    pub const VARIANT_COUNT: usize = 4;
}

/// [`AgentPhase`]'s table for the recovery dimension. `none` is the word for
/// `NoRecovery`, which is also its serde word.
impl std::fmt::Display for RecoveryPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            RecoveryPhase::NoRecovery => "none",
            RecoveryPhase::IdleWaiting => "idle_waiting",
            RecoveryPhase::IdleFault => "idle_fault",
            RecoveryPhase::Draining => "draining",
        })
    }
}

impl std::str::FromStr for RecoveryPhase {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Ok(match name {
            "none" => RecoveryPhase::NoRecovery,
            "idle_waiting" => RecoveryPhase::IdleWaiting,
            "idle_fault" => RecoveryPhase::IdleFault,
            "draining" => RecoveryPhase::Draining,
            other => return Err(unknown_phase("recovery", other)),
        })
    }
}

/// The task's result. Not a session dimension: the task ledger owns it, and no
/// session tuple carries it. It is an input to `project`, which cannot decide
/// whether a session has finished without being told how its task ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    /// Work still in flight.
    Pending,
    /// Completion accepted.
    Done,
    /// Work terminated with a fault; recovery happens elsewhere.
    Failed,
    /// Work cancelled by operator or supervisor.
    Cancelled,
    /// Work waiting on something outside the delivery.
    ///
    /// The delivery's own verdict, never a synonym for `Failed`: a session that
    /// ended a turn without completing leaves the work waiting, and a board
    /// reads it as waiting rather than as failed.
    Blocked,
}

/// Event/observation version. Ordering is lexicographic: generation first,
/// then seq within a generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Version {
    pub generation: u64,
    pub seq: u64,
}

impl Version {
    pub fn new(generation: u64, seq: u64) -> Self {
        Self { generation, seq }
    }
}

/// The session's own orthogonal state tuple: what this session can prove about
/// its agent, its completion intent, its resource, and its recovery line, plus
/// the reconcile policy and counter that go with them.
///
/// The task's result is not in here, and neither is the public view. A caller
/// that wants `Lifecycle` calls [`project`](super::project::project) with
/// the [`TaskState`] it owns; a stored or replayed tuple can no longer claim a
/// projection nobody derived.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Observation {
    /// Version of the event that produced this observation.
    pub version: Version,
    /// Whether the generation at `version.generation` is still live. Gone
    /// observations flip this to false; it gates new-generation adoption.
    pub generation_live: bool,
    /// Reconcile policy: the m-th consecutive mismatch isolates (idle_fault).
    pub isolate_after: u32,
    /// Reconcile policy: the n-th consecutive mismatch terminates the work.
    pub terminate_after: u32,
    /// Consecutive reconcile mismatch counter. Reset by any matching check.
    pub mismatch_count: u32,
    pub agent: AgentPhase,
    pub delivery: DeliveryPhase,
    pub resource: ResourcePhase,
    pub recovery: RecoveryPhase,
    /// Where the reporting process runs, when its host can say (the Orca pane a
    /// pi session lives in). Not a state dimension: `project` never reads it and
    /// `is_legal` never constrains it, but it rides the comparison tuple, so a
    /// binding-only heartbeat still advances the row instead of reading as a
    /// no-op replay. Absent means the host said nothing, never "not submitted".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<HostRef>,
}

impl Observation {
    /// A fresh session: generation 1, booting, no intent, detached.
    pub fn initial(isolate_after: u32, terminate_after: u32) -> Self {
        Self::build(
            Version::new(1, 0),
            true,
            isolate_after,
            terminate_after,
            0,
            AgentPhase::Booting,
            DeliveryPhase::NoIntent,
            ResourcePhase::Detached,
            RecoveryPhase::NoRecovery,
        )
    }

    /// Construct an observation from the session's own dimensions.
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        version: Version,
        generation_live: bool,
        isolate_after: u32,
        terminate_after: u32,
        mismatch_count: u32,
        agent: AgentPhase,
        delivery: DeliveryPhase,
        resource: ResourcePhase,
        recovery: RecoveryPhase,
    ) -> Self {
        Self {
            version,
            generation_live,
            isolate_after,
            terminate_after,
            mismatch_count,
            agent,
            delivery,
            resource,
            recovery,
            host: None,
        }
    }

    /// Same tuple with the reporting host of an earlier observation carried on.
    /// The host is not part of any transition, so a tuple rebuilt from another
    /// one (`settle_body`) keeps the binding it was reported with.
    pub fn with_host(mut self, host: Option<HostRef>) -> Self {
        self.host = host;
        self
    }

    /// Same tuple with an advanced version watermark.
    pub(super) fn advanced(&self, version: Version) -> Self {
        let mut next = self.clone();
        next.version = version;
        next
    }

    /// Same tuple with the generation watermarks replaced after adoption.
    pub(super) fn adopted(&self, version: Version) -> Self {
        let mut next = self.advanced(version);
        next.generation_live = true;
        next.mismatch_count = 0;
        next
    }

    /// Same tuple with a mutated mismatch counter.
    pub(super) fn with_mismatch(&self, count: u32) -> Self {
        let mut next = self.clone();
        next.mismatch_count = count;
        next
    }
}

/// The comparison tuple: the state dimensions plus the reported host, version-
/// and policy-free. `host` is in it on purpose — a heartbeat that only now says
/// where its process runs is news, and comparing the dimensions alone would
/// read it as a no-op replay and drop the binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Tuple {
    agent: AgentPhase,
    delivery: DeliveryPhase,
    resource: ResourcePhase,
    recovery: RecoveryPhase,
    generation_live: bool,
    host: Option<HostRef>,
}

impl Observation {
    pub(super) fn tuple(&self) -> Tuple {
        Tuple {
            agent: self.agent,
            delivery: self.delivery,
            resource: self.resource,
            recovery: self.recovery,
            generation_live: self.generation_live,
            host: self.host.clone(),
        }
    }
}
