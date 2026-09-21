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
