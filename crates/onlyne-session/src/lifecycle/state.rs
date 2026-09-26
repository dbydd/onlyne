//! State vocabulary: the four orthogonal session dimensions, the task state the
//! projection reads as an input, version, observation.

use serde::{Deserialize, Serialize};

use crate::host::HostRef;

/// Agent-side lifecycle fact observed from Pi/pi-onlyne.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    /// Process started, session binding not finished yet.
    Booting,
    /// Ready barrier passed, waiting for input.
    Ready,
    /// A turn is in progress.
    Running,
    /// Turn ended, agent waiting again.
    Idle,
    /// Process exited / unreachable and proven dead.
    Gone,
}
impl AgentState {
    /// Total variant count. The lifecycle tests pin their enumeration arrays
    /// to this so a new variant fails fast instead of silently shrinking the
    /// exhaustive matrix.
    pub const VARIANT_COUNT: usize = 5;
}

/// Intent delivery fact for the current completion exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    /// No intent has been created for the current exit.
    None,
    /// Intent sent, awaiting receipt.
    Pending,
    /// Intent retried at least once, awaiting receipt.
    Retrying,
    /// Receipt observed; the intent is delivered.
    Accepted,
    /// Retries exhausted; the intent moved to the fault path.
    Exhausted,
}
impl DeliveryState {
    /// See [`AgentState::VARIANT_COUNT`].
    pub const VARIANT_COUNT: usize = 5;
}

/// Backend resource fact (pane / tab / terminal handle).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceState {
    /// No resource attached to this session yet.
    Detached,
    /// Resource alive and attached.
    Attached,
    /// Close requested, resource still present.
    Closing,
    /// Resource confirmed closed.
    Closed,
}
impl ResourceState {
    /// See [`AgentState::VARIANT_COUNT`].
    pub const VARIANT_COUNT: usize = 4;
}

/// Recovery substate carried alongside an idle or draining agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryState {
    /// No recovery substate.
    None,
    /// Active task ended its turn without a completion exit; pi-onlyne will
    /// re-prompt once and the next turn-start returns to working.
    IdleWaiting,
    /// Fact mismatch (heartbeat, snapshot, generation, resource, delivery).
    /// Recovery evidence returns to working; otherwise the terminate path runs.
    IdleFault,
    /// Turn ended and completion is in asynchronous send. Public projection
    /// stays `working` until the receipt arrives.
    Draining,
}
impl RecoveryState {
    /// See [`AgentState::VARIANT_COUNT`].
    pub const VARIANT_COUNT: usize = 4;
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
}

/// Public lifecycle projection consumed by the TUI and external observers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicLifecycle {
    Created,
    Working,
    Idle,
    Exited,
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
/// that wants `PublicLifecycle` calls [`project`](super::project::project) with
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
    pub agent: AgentState,
    pub delivery: DeliveryState,
    pub resource: ResourceState,
    pub recovery: RecoveryState,
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
            AgentState::Booting,
            DeliveryState::None,
            ResourceState::Detached,
            RecoveryState::None,
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
        agent: AgentState,
        delivery: DeliveryState,
        resource: ResourceState,
        recovery: RecoveryState,
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
    agent: AgentState,
    delivery: DeliveryState,
    resource: ResourceState,
    recovery: RecoveryState,
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

// Each of the four dimension vocabularies names itself once, in one pair of
// impls. `Display` matches exhaustively, so a new variant cannot be added
// without gaining its word, and `FromStr` is the reading half of that same
// table; a word it does not know answers `Err` and leaves the fallback to the
// caller. These are the words the ledger columns already hold — renaming one is
// a migration, not an edit. The wire's twin vocabulary (`onlyne_proto`'s
// `AgentPhase` and siblings) spells the same words for the same facts and
// carries the same pair of impls.

impl std::fmt::Display for AgentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            AgentState::Booting => "booting",
            AgentState::Ready => "ready",
            AgentState::Running => "running",
            AgentState::Idle => "idle",
            AgentState::Gone => "gone",
        })
    }
}

impl std::str::FromStr for AgentState {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Ok(match name {
            "booting" => AgentState::Booting,
            "ready" => AgentState::Ready,
            "running" => AgentState::Running,
            "idle" => AgentState::Idle,
            "gone" => AgentState::Gone,
            other => return Err(format!("unknown agent state {other:?}")),
        })
    }
}

impl std::fmt::Display for DeliveryState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            DeliveryState::None => "none",
            DeliveryState::Pending => "pending",
            DeliveryState::Retrying => "retrying",
            DeliveryState::Accepted => "accepted",
            DeliveryState::Exhausted => "exhausted",
        })
    }
}

impl std::str::FromStr for DeliveryState {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Ok(match name {
            "none" => DeliveryState::None,
            "pending" => DeliveryState::Pending,
            "retrying" => DeliveryState::Retrying,
            "accepted" => DeliveryState::Accepted,
            "exhausted" => DeliveryState::Exhausted,
            other => return Err(format!("unknown delivery state {other:?}")),
        })
    }
}

impl std::fmt::Display for ResourceState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ResourceState::Detached => "detached",
            ResourceState::Attached => "attached",
            ResourceState::Closing => "closing",
            ResourceState::Closed => "closed",
        })
    }
}

impl std::str::FromStr for ResourceState {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Ok(match name {
            "detached" => ResourceState::Detached,
            "attached" => ResourceState::Attached,
            "closing" => ResourceState::Closing,
            "closed" => ResourceState::Closed,
            other => return Err(format!("unknown resource state {other:?}")),
        })
    }
}

impl std::fmt::Display for RecoveryState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            RecoveryState::None => "none",
            RecoveryState::IdleWaiting => "idle_waiting",
            RecoveryState::IdleFault => "idle_fault",
            RecoveryState::Draining => "draining",
        })
    }
}

impl std::str::FromStr for RecoveryState {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Ok(match name {
            "none" => RecoveryState::None,
            "idle_waiting" => RecoveryState::IdleWaiting,
            "idle_fault" => RecoveryState::IdleFault,
            "draining" => RecoveryState::Draining,
            other => return Err(format!("unknown recovery state {other:?}")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{AgentState, DeliveryState, RecoveryState, ResourceState};
    use std::str::FromStr;

    /// Every dimension word is both written and read by the same table, so a
    /// variant must survive the round trip onto its own word and back. A word
    /// that does not come back is a dimension that silently resets to the
    /// freshly created one — which is exactly how a stored row ages out.
    #[test]
    fn state_roundtrip() {
        let agents = [
            (AgentState::Booting, "booting"),
            (AgentState::Ready, "ready"),
            (AgentState::Running, "running"),
            (AgentState::Idle, "idle"),
            (AgentState::Gone, "gone"),
        ];
        for (state, word) in agents {
            assert_eq!(state.to_string(), word, "the stored agent word");
            assert_eq!(AgentState::from_str(word).unwrap(), state, "{word}");
        }
        let deliveries = [
            (DeliveryState::None, "none"),
            (DeliveryState::Pending, "pending"),
            (DeliveryState::Retrying, "retrying"),
            (DeliveryState::Accepted, "accepted"),
            (DeliveryState::Exhausted, "exhausted"),
        ];
        for (state, word) in deliveries {
            assert_eq!(state.to_string(), word, "the stored delivery word");
            assert_eq!(DeliveryState::from_str(word).unwrap(), state, "{word}");
        }
        let resources = [
            (ResourceState::Detached, "detached"),
            (ResourceState::Attached, "attached"),
            (ResourceState::Closing, "closing"),
            (ResourceState::Closed, "closed"),
        ];
        for (state, word) in resources {
            assert_eq!(state.to_string(), word, "the stored resource word");
            assert_eq!(ResourceState::from_str(word).unwrap(), state, "{word}");
        }
        let recoveries = [
            (RecoveryState::None, "none"),
            (RecoveryState::IdleWaiting, "idle_waiting"),
            (RecoveryState::IdleFault, "idle_fault"),
            (RecoveryState::Draining, "draining"),
        ];
        for (state, word) in recoveries {
            assert_eq!(state.to_string(), word, "the stored recovery word");
            assert_eq!(RecoveryState::from_str(word).unwrap(), state, "{word}");
        }
    }
}
