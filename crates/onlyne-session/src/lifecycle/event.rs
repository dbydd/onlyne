//! Versioned lifecycle events and the verdict vocabulary of the reducer.

use serde::{Deserialize, Serialize};

use super::state::{Observation, Version};

/// Lifecycle event. Each event carries the version that produced it; version
/// gating happens centrally in `apply` before semantic rules run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleEvent {
    /// Generation-1 session created and bound to its task.
    Created { v: Version },
    /// Ready barrier passed (`swarm_ready`).
    Ready { v: Version },
    /// Turn started (`swarm_turn_started`) — the working evidence.
    TurnStarted { v: Version },
    /// Turn ended (`agent_end`) without a completion receipt yet.
    TurnEnded { v: Version },
    /// Full state report; the authoritative source replaces the snapshot.
    /// Rejected when the reported tuple is illegal.
    Heartbeat { v: Version, body: Observation },
    /// Complete intent opened for the current turn exit.
    Complete { v: Version },
    /// Sent intent awaiting receipt.
    IntentPending { v: Version },
    /// Sent intent retried.
    IntentRetry { v: Version },
    /// Receipt observed for the pending intent.
    IntentReceipt { v: Version },
    /// Retries exhausted; the intent moved to the fault queue.
    IntentExhausted { v: Version },
    /// Backend resource attached.
    ResourceAttach { v: Version },
    /// Close requested on the backend resource.
    ResourceCloseRequested { v: Version },
    /// Backend confirmed the resource closed.
    ResourceClosed { v: Version },
    /// Agent process observed gone.
    AgentGone { v: Version },
    /// Cancel accepted. The result settles in the task ledger; no session
    /// dimension changes, so the reducer has nothing to write and the event
    /// reads as a no-op against the tuple.
    Cancel { v: Version },
    /// Fault accepted: the session opens a fault line while its agent is idle.
    /// How the work ended belongs to the task ledger, and recovery happens as a
    /// new task.
    Fail { v: Version },
    /// Reconcile probe found a mismatch: count it, isolate at m, terminate at n.
    ReconcileMismatch { v: Version },
    /// Reconcile probe found a match: reset the consecutive mismatch counter and
    /// heal `idle_fault` when fault was the only outstanding mismatch.
    ReconcileOk { v: Version },
    /// A new Pi generation reports in while the old generation is proven gone.
    /// Rejected as a duplicate live generation while the old one is still live.
    AdoptNewGeneration { v: Version },
    /// Operator repair: replace the current generation with `v` while carrying
    /// the observation content forward. Requires attestation that the old
    /// generation is dead.
    Supersede {
        v: Version,
        old_generation_dead: bool,
        body: Observation,
    },
}

/// Reducer verdict. Every (observation, event) pair yields exactly one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// State advanced to the inner observation.
    Applied(Observation),
    /// Current state kept; the event was stale, duplicate, or a no-op.
    Ignored(IgnoredReason),
    /// Current state kept; the event contradicts a state invariant.
    Rejected(RejectReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IgnoredReason {
    /// Event generation older than the current watermark.
    StaleGeneration,
    /// Event seq at or below the current watermark within the generation.
    StaleOrDuplicateSeq,
    /// The transition would not change the tuple (idempotent no-op).
    NoOp,
    /// The current generation is terminal; lifecycle detail is inert after it.
    GenerationFinished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// New generation announced while the previous one is still live.
    DuplicateLiveGeneration,
    /// A newer-generation event arrived without going through adoption.
    UnadoptedGeneration,
    /// Supersede without proof that the old generation is dead.
    OldGenerationLive,
    /// The proposed replacement tuple is illegal.
    IllegalObservation,
    /// No rule defines this transition from the current state.
    UndefinedTransition,
}

/// Version carried by an event.
pub fn event_version(event: &LifecycleEvent) -> Version {
    match event {
        LifecycleEvent::Created { v }
        | LifecycleEvent::Ready { v }
        | LifecycleEvent::TurnStarted { v }
        | LifecycleEvent::TurnEnded { v }
        | LifecycleEvent::Heartbeat { v, .. }
        | LifecycleEvent::Complete { v }
        | LifecycleEvent::IntentPending { v }
        | LifecycleEvent::IntentRetry { v }
        | LifecycleEvent::IntentReceipt { v }
        | LifecycleEvent::IntentExhausted { v }
        | LifecycleEvent::ResourceAttach { v }
        | LifecycleEvent::ResourceCloseRequested { v }
        | LifecycleEvent::ResourceClosed { v }
        | LifecycleEvent::AgentGone { v }
        | LifecycleEvent::Cancel { v }
        | LifecycleEvent::Fail { v }
        | LifecycleEvent::ReconcileMismatch { v }
        | LifecycleEvent::ReconcileOk { v }
        | LifecycleEvent::AdoptNewGeneration { v }
        | LifecycleEvent::Supersede { v, .. } => *v,
    }
}
