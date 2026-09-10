//! Pure session-lifecycle core for swarm sessions.
//!
//! This module holds the orthogonal state dimensions, the versioned event type,
//! and a total reducer over them. It is deliberately free of scheduler,
//! database, transport, and backend dependencies: callers feed observations
//! and events, the reducer returns a verdict.
//!
//! State dimensions (docs/SWARM-REFACTOR-GRILLME.md §2.2):
//! - `AgentState`     — process-side agent fact reported by Pi/pi-onlyne.
//! - `DeliveryState`  — intent delivery fact for the current turn exit.
//! - `ResourceState`  — backend resource fact (pane/tab/terminal).
//! - `RecoveryState`  — recovery substate of an idle/draining session.
//! - `Outcome`        — task result dimension (pending/done/failed/cancelled).
//!
//! Public projection (`PublicLifecycle`) is derived from the five dimensions by
//! `project`. The projection is part of every observation, so persisted and
//! wire-visible state always carries its verified public view.
//!
//! Versioning (§2.3): every observation and event carries `(generation, seq)`.
//! The reducer gates events on the current version watermark:
//! - stale generation or stale/duplicate seq: `Ignored` with a diagnostic,
//!   current state kept.
//! - same-generation duplicate event id: `Ignored` (idempotent replay).
//! - a newer generation arrives only through `AdoptNewGeneration` (old
//!   generation already gone) or `Supersede` (operator repair with attested
//!   dead old generation); every other newer-generation event is `Rejected`.
//! - a new generation while the previous one is still live is `Rejected`
//!   (duplicate live generation); the core keeps the first generation.
//!
//! §3.3: same-task duplicate Pi processes are gated by these adoption rules;
//! post-`Gone` sessions only accept adoption, supersede, and heartbeat.

use serde::{Deserialize, Serialize};

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

/// Task result dimension. Owned by the task ledger; mirrored here so the
/// reducer stays total over the same tuple.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
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

/// The full orthogonal state tuple plus its derived public projection.
///
/// `public` is always `project(...)` of the other fields; `is_legal` enforces
/// that so a hand-written or replayed observation cannot smuggle a
/// contradictory public view through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    pub outcome: Outcome,
    /// Derived public projection, stored for persistence and display.
    pub public: PublicLifecycle,
}

impl Observation {
    /// A fresh session: generation 1, booting, detached, task pending.
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
            Outcome::Pending,
        )
    }

    /// Construct an observation, deriving the public projection.
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
        outcome: Outcome,
    ) -> Self {
        let public = project(agent, delivery, resource, recovery, outcome);
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
            outcome,
            public,
        }
    }

    /// Same tuple with an advanced version watermark.
    fn advanced(&self, version: Version) -> Self {
        let mut next = *self;
        next.version = version;
        next
    }

    /// Same tuple with the generation watermarks replaced after adoption.
    fn adopted(&self, version: Version) -> Self {
        let mut next = self.advanced(version);
        next.generation_live = true;
        next.mismatch_count = 0;
        next
    }

    /// Same tuple with a mutated mismatch counter.
    fn with_mismatch(&self, count: u32) -> Self {
        let mut next = *self;
        next.mismatch_count = count;
        next
    }
}

/// Derived public projection (§2.2).
///
/// Rules, evaluated in order:
/// - Gone exits. Done with an accepted delivery exits (receipt closed the
///   drain).
/// - A begun outcome (Done/Failed/Cancelled), an in-flight intent
///   (Pending/Retrying/Exhausted), draining, running, and both recovery
///   substates all project `working`: the session still owns its task and has
///   open work or an open fault line.
/// - Ready/Idle with no open line projects `idle`; Booting projects `created`.
pub fn project(
    agent: AgentState,
    delivery: DeliveryState,
    _resource: ResourceState,
    recovery: RecoveryState,
    outcome: Outcome,
) -> PublicLifecycle {
    if agent == AgentState::Gone {
        return PublicLifecycle::Exited;
    }
    if outcome == Outcome::Done && delivery == DeliveryState::Accepted {
        return PublicLifecycle::Exited;
    }
    match outcome {
        Outcome::Done | Outcome::Failed | Outcome::Cancelled => return PublicLifecycle::Working,
        Outcome::Pending => {}
    }
    match delivery {
        DeliveryState::Pending | DeliveryState::Retrying | DeliveryState::Exhausted => {
            return PublicLifecycle::Working;
        }
        DeliveryState::None | DeliveryState::Accepted => {}
    }
    if recovery == RecoveryState::Draining {
        return PublicLifecycle::Working;
    }
    match agent {
        AgentState::Running => PublicLifecycle::Working,
        AgentState::Idle | AgentState::Ready => match recovery {
            RecoveryState::IdleWaiting | RecoveryState::IdleFault => PublicLifecycle::Working,
            _ => PublicLifecycle::Idle,
        },
        AgentState::Booting | AgentState::Gone => PublicLifecycle::Created,
    }
}

/// Legality of a state tuple: projection consistency plus the dimension
/// cross-constraints frozen in §2.2.
pub fn is_legal(obs: &Observation) -> bool {
    if obs.public
        != project(
            obs.agent,
            obs.delivery,
            obs.resource,
            obs.recovery,
            obs.outcome,
        )
    {
        return false;
    }
    if obs.isolate_after == 0 || obs.terminate_after == 0 {
        return false;
    }
    // Recovery substates belong to live generations only.
    if !obs.generation_live && obs.recovery != RecoveryState::None {
        return false;
    }
    // Post-mortem shape: Gone keeps only Failed outcomes, closed-shape
    // resources, and delivery that has settled.
    if obs.agent == AgentState::Gone {
        if obs.outcome == Outcome::Cancelled || obs.outcome == Outcome::Done {
            if obs.outcome == Outcome::Cancelled {
                return false;
            }
        }
        if obs.recovery != RecoveryState::None {
            return false;
        }
        if obs.delivery == DeliveryState::Accepted && obs.outcome != Outcome::Done {
            return false;
        }
    }
    if obs.outcome == Outcome::Cancelled && obs.delivery == DeliveryState::Exhausted {
        return false;
    }
    match obs.recovery {
        RecoveryState::IdleWaiting | RecoveryState::IdleFault => {
            if obs.agent != AgentState::Idle {
                return false;
            }
        }
        RecoveryState::Draining => {
            if obs.agent != AgentState::Idle && obs.agent != AgentState::Running {
                return false;
            }
        }
        RecoveryState::None => {}
    }
    // Done has passed its receipt gate; Accepted is a post-turn fact.
    if obs.outcome == Outcome::Done {
        if obs.delivery != DeliveryState::Accepted {
            return false;
        }
        if obs.agent == AgentState::Idle && obs.recovery != RecoveryState::Draining {
            return false;
        }
    }
    if obs.delivery == DeliveryState::Accepted && matches!(obs.agent, AgentState::Booting) {
        return false;
    }
    // Exhausted means retries burned against an open turn exit.
    if obs.delivery == DeliveryState::Exhausted && obs.agent == AgentState::Ready {
        return false;
    }
    true
}

/// Lifecycle event. Each event carries the version that produced it; version
/// gating happens centrally in `apply` before semantic rules run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Cancel accepted: task result becomes cancelled; exit proceeds.
    Cancel { v: Version },
    /// Fault accepted: work terminates and recovery happens as a new task.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// Apply one event to one observation. Total function: no panics, and every
/// branch yields Applied/Ignored/Rejected.
pub fn apply(obs: &Observation, event: &LifecycleEvent) -> Verdict {
    let v = event_version(event);

    // --- version gate (§2.3), uniform over every event kind ---
    if v.generation < obs.version.generation {
        return Verdict::Ignored(IgnoredReason::StaleGeneration);
    }
    let is_adoption = matches!(
        event,
        LifecycleEvent::AdoptNewGeneration { .. } | LifecycleEvent::Supersede { .. }
    );
    if v.generation == obs.version.generation {
        if v.seq <= obs.version.seq && !is_adoption {
            let reason = if v.seq == obs.version.seq {
                IgnoredReason::StaleOrDuplicateSeq
            } else {
                IgnoredReason::StaleOrDuplicateSeq
            };
            return Verdict::Ignored(reason);
        }
    } else if !is_adoption {
        return Verdict::Rejected(RejectReason::UnadoptedGeneration);
    }

    // --- generation gate: live duplicate adoption is rejected (§3.3) ---
    if let LifecycleEvent::AdoptNewGeneration { .. } = event {
        if obs.generation_live {
            return Verdict::Rejected(RejectReason::DuplicateLiveGeneration);
        }
    }
    if let LifecycleEvent::Supersede {
        old_generation_dead,
        ..
    } = event
    {
        if !old_generation_dead {
            return Verdict::Rejected(RejectReason::OldGenerationLive);
        }
    }

    // --- semantic rules ---
    match event {
        LifecycleEvent::Created { .. } | LifecycleEvent::Ready { .. } => {
            let agent = match event {
                LifecycleEvent::Created { .. } => AgentState::Booting,
                _ => AgentState::Ready,
            };
            step(obs, v, |o| transition(o, agent))
        }
        LifecycleEvent::TurnStarted { .. } => step(obs, v, |o| transition(o, AgentState::Running)),
        LifecycleEvent::TurnEnded { .. } => step(obs, v, |o| transition(o, AgentState::Idle)),
        LifecycleEvent::Heartbeat { body, .. } => {
            if !is_legal(body) {
                return Verdict::Rejected(RejectReason::IllegalObservation);
            }
            if body_is_no_op(obs, body) {
                return Verdict::Ignored(IgnoredReason::NoOp);
            }
            if v.generation != obs.version.generation {
                return Verdict::Rejected(RejectReason::UnadoptedGeneration);
            }
            let mut next = *body;
            next.version = v;
            next.generation_live = obs.generation_live;
            finish(obs, next)
        }
        LifecycleEvent::Complete { .. } => {
            let mut next = obs.advanced(v);
            next.delivery = DeliveryState::Pending;
            if obs.agent == AgentState::Idle {
                next.recovery = RecoveryState::Draining;
            }
            if next == *obs {
                return Verdict::Ignored(IgnoredReason::NoOp);
            }
            finish(obs, next)
        }
        LifecycleEvent::IntentPending { .. } => {
            let mut next = obs.advanced(v);
            match obs.delivery {
                DeliveryState::None | DeliveryState::Retrying | DeliveryState::Exhausted => {
                    next.delivery = DeliveryState::Pending;
                }
                DeliveryState::Pending | DeliveryState::Accepted => {
                    return Verdict::Ignored(IgnoredReason::NoOp);
                }
            }
            finish(obs, next)
        }
        LifecycleEvent::IntentRetry { .. } => {
            let mut next = obs.advanced(v);
            match obs.delivery {
                DeliveryState::Pending => {
                    next.delivery = DeliveryState::Retrying;
                    if obs.recovery == RecoveryState::IdleWaiting {
                        next.recovery = RecoveryState::None;
                    }
                }
                DeliveryState::None
                | DeliveryState::Retrying
                | DeliveryState::Accepted
                | DeliveryState::Exhausted => {
                    return Verdict::Ignored(IgnoredReason::NoOp);
                }
            }
            finish(obs, next)
        }
        LifecycleEvent::IntentReceipt { .. } => {
            let mut next = obs.advanced(v);
            match obs.delivery {
                DeliveryState::Pending | DeliveryState::Retrying => {
                    next.delivery = DeliveryState::Accepted;
                    next.recovery = RecoveryState::None;
                }
                DeliveryState::None | DeliveryState::Accepted | DeliveryState::Exhausted => {
                    return Verdict::Ignored(IgnoredReason::NoOp);
                }
            }
            finish(obs, next)
        }
        LifecycleEvent::IntentExhausted { .. } => {
            let mut next = obs.advanced(v);
            match obs.delivery {
                DeliveryState::Pending | DeliveryState::Retrying => {
                    next.delivery = DeliveryState::Exhausted;
                    if obs.recovery == RecoveryState::IdleWaiting {
                        next.recovery = RecoveryState::IdleFault;
                    }
                }
                DeliveryState::None | DeliveryState::Accepted | DeliveryState::Exhausted => {
                    return Verdict::Ignored(IgnoredReason::NoOp);
                }
            }
            finish(obs, next)
        }
        LifecycleEvent::ResourceAttach { .. } => {
            let mut next = obs.advanced(v);
            match obs.resource {
                ResourceState::Detached => next.resource = ResourceState::Attached,
                ResourceState::Attached => return Verdict::Ignored(IgnoredReason::NoOp),
                ResourceState::Closing | ResourceState::Closed => {
                    return Verdict::Rejected(RejectReason::UndefinedTransition);
                }
            }
            finish(obs, next)
        }
        LifecycleEvent::ResourceCloseRequested { .. } | LifecycleEvent::ResourceClosed { .. } => {
            let mut next = obs.advanced(v);
            match (event, obs.resource) {
                (LifecycleEvent::ResourceCloseRequested { .. }, ResourceState::Detached)
                | (LifecycleEvent::ResourceCloseRequested { .. }, ResourceState::Closed) => {
                    return Verdict::Ignored(IgnoredReason::NoOp);
                }
                (LifecycleEvent::ResourceCloseRequested { .. }, _) => {
                    next.resource = ResourceState::Closing;
                }
                (LifecycleEvent::ResourceClosed { .. }, ResourceState::Detached)
                | (LifecycleEvent::ResourceClosed { .. }, ResourceState::Closed) => {
                    return Verdict::Ignored(IgnoredReason::NoOp);
                }
                (LifecycleEvent::ResourceClosed { .. }, _) => {
                    next.resource = ResourceState::Closed;
                    // A closed resource kills the agent fact it hosted.
                    if obs.generation_live {
                        next.agent = AgentState::Gone;
                        next.generation_live = false;
                        next.recovery = RecoveryState::None;
                        if obs.outcome == Outcome::Pending || obs.outcome == Outcome::Cancelled {
                            next.outcome = match obs.outcome {
                                Outcome::Cancelled => Outcome::Cancelled,
                                _ => Outcome::Failed,
                            };
                        }
                    }
                }
                _ => return Verdict::Rejected(RejectReason::UndefinedTransition),
            }
            finish(obs, next)
        }
        LifecycleEvent::AgentGone { .. } => {
            let mut next = obs.advanced(v);
            if obs.agent == AgentState::Gone {
                return Verdict::Ignored(IgnoredReason::NoOp);
            }
            next.agent = AgentState::Gone;
            next.generation_live = false;
            next.recovery = RecoveryState::None;
            next.resource = match obs.resource {
                ResourceState::Attached | ResourceState::Closing => ResourceState::Closing,
                r => r,
            };
            if obs.outcome == Outcome::Pending || obs.outcome == Outcome::Cancelled {
                next.outcome = Outcome::Failed;
            }
            if obs.delivery == DeliveryState::Accepted && obs.outcome != Outcome::Done {
                next.delivery = DeliveryState::Retrying;
            }
            finish(obs, next)
        }
        LifecycleEvent::Cancel { .. } => {
            let mut next = obs.advanced(v);
            match obs.outcome {
                Outcome::Pending => next.outcome = Outcome::Cancelled,
                Outcome::Done | Outcome::Failed | Outcome::Cancelled => {
                    return Verdict::Ignored(IgnoredReason::NoOp);
                }
            }
            finish(obs, next)
        }
        LifecycleEvent::Fail { .. } => {
            let mut next = obs.advanced(v);
            next.outcome = Outcome::Failed;
            if obs.agent == AgentState::Idle {
                next.recovery = RecoveryState::IdleFault;
            }
            if next.tuple() == obs.tuple() {
                return Verdict::Ignored(IgnoredReason::NoOp);
            }
            finish(obs, next)
        }
        LifecycleEvent::ReconcileMismatch { .. } => {
            let count = obs.mismatch_count.saturating_add(1);
            if count > obs.terminate_after {
                return Verdict::Ignored(IgnoredReason::GenerationFinished);
            }
            if count >= obs.terminate_after {
                let mut next = obs.with_mismatch(count).advanced(v);
                next.generation_live = false;
                next.agent = AgentState::Gone;
                next.recovery = RecoveryState::None;
                next.outcome = Outcome::Failed;
                next.resource = match obs.resource {
                    ResourceState::Attached | ResourceState::Closing => ResourceState::Closing,
                    r => r,
                };
                if obs.delivery == DeliveryState::Accepted {
                    next.delivery = DeliveryState::Retrying;
                }
                return finish(obs, next);
            }
            if count >= obs.isolate_after {
                let mut next = obs.with_mismatch(count).advanced(v);
                if next.agent == AgentState::Idle {
                    next.recovery = RecoveryState::IdleFault;
                }
                return finish(obs, next);
            }
            Verdict::Applied(obs.with_mismatch(count).advanced(v))
        }
        LifecycleEvent::ReconcileOk { .. } => {
            let mut next = obs.advanced(v);
            next.mismatch_count = 0;
            if obs.recovery == RecoveryState::IdleFault {
                next.recovery = RecoveryState::None;
            }
            if next.tuple() == obs.tuple() {
                return Verdict::Ignored(IgnoredReason::NoOp);
            }
            finish(obs, next)
        }
        LifecycleEvent::AdoptNewGeneration { .. } => {
            // Content carries forward; the generation watermark moves. The
            // adopted shape is a live booting generation at the new version.
            let mut next = obs.adopted(v);
            next.agent = AgentState::Booting;
            next.delivery = DeliveryState::None;
            next.recovery = RecoveryState::None;
            next.outcome = Outcome::Pending;
            finish(obs, next)
        }
        LifecycleEvent::Supersede { body, .. } => {
            if !is_legal(body) {
                return Verdict::Rejected(RejectReason::IllegalObservation);
            }
            let mut next = *body;
            next.version = v;
            next.generation_live = true;
            finish(obs, next)
        }
    }
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

/// Apply an agent-state transition with the recovery/delivery coupling frozen
/// in §2.2. `None` means no rule covers the transition.
fn transition(obs: &Observation, agent: AgentState) -> Option<Observation> {
    let v = event_version(&LifecycleEvent::TurnStarted { v: obs.version });
    let _ = v;
    let mut next = *obs;
    match agent {
        AgentState::Booting => {
            if obs.agent == AgentState::Booting {
                return None;
            }
            next.agent = AgentState::Booting;
            next.delivery = DeliveryState::None;
            next.recovery = RecoveryState::None;
        }
        AgentState::Ready => {
            if obs.agent == AgentState::Gone {
                return None;
            }
            next.agent = AgentState::Ready;
            if obs.recovery == RecoveryState::Draining {
                next.recovery = RecoveryState::None;
            }
        }
        AgentState::Running => {
            if obs.agent == AgentState::Gone {
                return None;
            }
            // Turn start is the working evidence: it heals recovery substates.
            next.agent = AgentState::Running;
            next.recovery = RecoveryState::None;
        }
        AgentState::Idle => {
            if obs.agent == AgentState::Gone {
                return None;
            }
            next.agent = AgentState::Idle;
            next.recovery = match obs.recovery {
                RecoveryState::Draining => RecoveryState::Draining,
                RecoveryState::IdleFault => RecoveryState::IdleFault,
                _ => match obs.delivery {
                    DeliveryState::Pending | DeliveryState::Retrying => RecoveryState::IdleWaiting,
                    _ => RecoveryState::None,
                },
            };
        }
        AgentState::Gone => {
            if obs.agent == AgentState::Gone {
                return None;
            }
            next.agent = AgentState::Gone;
            next.generation_live = false;
            next.recovery = RecoveryState::None;
            next.resource = match obs.resource {
                ResourceState::Attached | ResourceState::Closing => ResourceState::Closing,
                r => r,
            };
            if obs.outcome == Outcome::Pending {
                next.outcome = Outcome::Failed;
            }
            if obs.delivery == DeliveryState::Accepted && obs.outcome != Outcome::Done {
                next.delivery = DeliveryState::Retrying;
            }
        }
    }
    Some(next)
}

/// Shared path for transitions produced by `transition`: no-op detection and
/// legality verification, with the version watermark advanced.
fn step(obs: &Observation, v: Version, f: impl Fn(&Observation) -> Option<Observation>) -> Verdict {
    let Some(mut next) = f(obs) else {
        if obs.agent == AgentState::Gone {
            return Verdict::Ignored(IgnoredReason::GenerationFinished);
        }
        return Verdict::Rejected(RejectReason::UndefinedTransition);
    };
    next.version = v;
    finish(obs, next)
}

/// Verify a produced tuple and compare it (ignoring the version) with the
/// current one for no-op detection.
fn finish(obs: &Observation, mut next: Observation) -> Verdict {
    next.public = project(
        next.agent,
        next.delivery,
        next.resource,
        next.recovery,
        next.outcome,
    );
    if !is_legal(&next) {
        return Verdict::Rejected(RejectReason::IllegalObservation);
    }
    if next.tuple() == obs.tuple() && next.mismatch_count == obs.mismatch_count {
        return Verdict::Ignored(IgnoredReason::NoOp);
    }
    Verdict::Applied(next)
}

/// The orthogonal dimension tuple, version- and policy-free, for comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Tuple {
    agent: AgentState,
    delivery: DeliveryState,
    resource: ResourceState,
    recovery: RecoveryState,
    outcome: Outcome,
    generation_live: bool,
}

impl Observation {
    fn tuple(&self) -> Tuple {
        Tuple {
            agent: self.agent,
            delivery: self.delivery,
            resource: self.resource,
            recovery: self.recovery,
            outcome: self.outcome,
            generation_live: self.generation_live,
        }
    }
}

/// True when a heartbeat body repeats the current tuple (idempotent replay).
fn body_is_no_op(obs: &Observation, body: &Observation) -> bool {
    obs.tuple() == body.tuple()
        && obs.mismatch_count == body.mismatch_count
        && obs.isolate_after == body.isolate_after
        && obs.terminate_after == body.terminate_after
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // exhaustive enumeration helpers
    // ------------------------------------------------------------------

    const AGENTS: [AgentState; 5] = [
        AgentState::Booting,
        AgentState::Ready,
        AgentState::Running,
        AgentState::Idle,
        AgentState::Gone,
    ];
    const DELIVERIES: [DeliveryState; 5] = [
        DeliveryState::None,
        DeliveryState::Pending,
        DeliveryState::Retrying,
        DeliveryState::Accepted,
        DeliveryState::Exhausted,
    ];
    const RESOURCES: [ResourceState; 4] = [
        ResourceState::Detached,
        ResourceState::Attached,
        ResourceState::Closing,
        ResourceState::Closed,
    ];
    const RECOVERIES: [RecoveryState; 4] = [
        RecoveryState::None,
        RecoveryState::IdleWaiting,
        RecoveryState::IdleFault,
        RecoveryState::Draining,
    ];
    const OUTCOMES: [Outcome; 4] = [
        Outcome::Pending,
        Outcome::Done,
        Outcome::Failed,
        Outcome::Cancelled,
    ];
    const LIVE: [bool; 2] = [true, false];

    /// All 5*5*4*4*4*2 = 3200 dimension combinations at version (1, 5).
    fn all_observations() -> Vec<Observation> {
        let mut out = Vec::new();
        for &agent in &AGENTS {
            for &delivery in &DELIVERIES {
                for &resource in &RESOURCES {
                    for &recovery in &RECOVERIES {
                        for &outcome in &OUTCOMES {
                            for &live in &LIVE {
                                out.push(Observation::build(
                                    Version::new(1, 5),
                                    live,
                                    1,
                                    3,
                                    0,
                                    agent,
                                    delivery,
                                    resource,
                                    recovery,
                                    outcome,
                                ));
                            }
                        }
                    }
                }
            }
        }
        out
    }

    fn legal_observations() -> Vec<Observation> {
        all_observations()
            .into_iter()
            .filter(|o| is_legal(o))
            .collect()
    }

    /// One representative event per kind at a seq past every enumerated
    /// watermark, in both same-generation and future-generation forms.
    fn events_at(v: Version) -> Vec<LifecycleEvent> {
        let legal_bodies: Vec<Observation> = [
            Observation::initial(1, 3),
            Observation::build(
                v,
                true,
                1,
                3,
                0,
                AgentState::Idle,
                DeliveryState::Pending,
                ResourceState::Attached,
                RecoveryState::IdleWaiting,
                Outcome::Pending,
            ),
        ]
        .into_iter()
        .filter(|o| is_legal(o))
        .collect();
        let body = legal_bodies[0];
        let mut events = vec![
            LifecycleEvent::Created { v },
            LifecycleEvent::Ready { v },
            LifecycleEvent::TurnStarted { v },
            LifecycleEvent::TurnEnded { v },
            LifecycleEvent::Heartbeat { v, body },
            LifecycleEvent::Complete { v },
            LifecycleEvent::IntentPending { v },
            LifecycleEvent::IntentRetry { v },
            LifecycleEvent::IntentReceipt { v },
            LifecycleEvent::IntentExhausted { v },
            LifecycleEvent::ResourceAttach { v },
            LifecycleEvent::ResourceCloseRequested { v },
            LifecycleEvent::ResourceClosed { v },
            LifecycleEvent::AgentGone { v },
            LifecycleEvent::Cancel { v },
            LifecycleEvent::Fail { v },
            LifecycleEvent::ReconcileMismatch { v },
            LifecycleEvent::ReconcileOk { v },
            LifecycleEvent::AdoptNewGeneration { v },
            LifecycleEvent::Supersede {
                v,
                old_generation_dead: true,
                body,
            },
            LifecycleEvent::Supersede {
                v,
                old_generation_dead: false,
                body,
            },
            LifecycleEvent::Heartbeat {
                v,
                body: Observation {
                    public: PublicLifecycle::Idle,
                    ..body
                },
            },
        ];
        for candidate in legal_bodies.iter().skip(1) {
            events.push(LifecycleEvent::Heartbeat {
                v,
                body: *candidate,
            });
        }
        events
    }

    // ------------------------------------------------------------------
    // exhaustive matrix: totality, no panic, legal-result closure
    // ------------------------------------------------------------------

    #[test]
    fn every_combination_times_every_event_yields_a_verdict_without_panic() {
        let observations = all_observations();
        let same_gen = events_at(Version::new(1, 9));
        let future_gen = events_at(Version::new(2, 9));
        let stale_gen = events_at(Version::new(0, 9));
        let mut checked = 0usize;
        for obs in &observations {
            for events in [&same_gen, &future_gen, &stale_gen] {
                for event in events {
                    let verdict = apply(obs, event);
                    checked += 1;
                    match verdict {
                        Verdict::Applied(next) => {
                            assert!(
                                is_legal(&next),
                                "illegal result {next:?} from {obs:?} + {event:?}"
                            );
                            assert!(
                                next.version > obs.version
                                    || (next.version.generation > obs.version.generation),
                                "applied result must advance the watermark: {obs:?} + {event:?}",
                            );
                        }
                        Verdict::Ignored(_) | Verdict::Rejected(_) => {
                            // Current state kept verbatim by contract; the
                            // reducer returns the verdict only. Replaying the
                            // event yields the same verdict (determinism).
                            assert_eq!(apply(obs, event).discriminant(), verdict.discriminant());
                        }
                    }
                }
            }
        }
        assert!(checked >= 3200 * 20, "matrix too small: {checked}");
    }

    #[test]
    fn reducer_is_deterministic_on_legal_states() {
        for obs in legal_observations() {
            for event in events_at(Version::new(1, 9)) {
                let a = apply(&obs, &event);
                let b = apply(&obs, &event);
                assert_eq!(a.discriminant(), b.discriminant());
                if let (Verdict::Applied(x), Verdict::Applied(y)) = (a, b) {
                    assert_eq!(x, y);
                }
            }
        }
    }

    #[test]
    fn every_legal_combination_projects_consistently() {
        let legal = legal_observations();
        for obs in &legal {
            assert_eq!(
                obs.public,
                project(
                    obs.agent,
                    obs.delivery,
                    obs.resource,
                    obs.recovery,
                    obs.outcome
                ),
                "legal observation must carry its derived projection: {obs:?}"
            );
        }
        // The full-space filter is meaningful only when both legal and
        // illegal tuples exist in the 3200-cell product.
        assert!(!legal.is_empty() && legal.len() < all_observations().len());
    }

    // ------------------------------------------------------------------
    // version gating: stale / duplicate / adoption / live duplicate
    // ------------------------------------------------------------------

    #[test]
    fn stale_generation_and_stale_or_duplicate_seq_are_ignored() {
        let obs = live_working();
        let stale_gen = apply(
            &obs,
            &LifecycleEvent::TurnEnded {
                v: Version::new(0, 99),
            },
        );
        assert_eq!(stale_gen, Verdict::Ignored(IgnoredReason::StaleGeneration));
        let stale_seq = apply(
            &obs,
            &LifecycleEvent::TurnEnded {
                v: Version::new(1, 2),
            },
        );
        assert_eq!(
            stale_seq,
            Verdict::Ignored(IgnoredReason::StaleOrDuplicateSeq)
        );
        let dup_seq = apply(
            &obs,
            &LifecycleEvent::TurnEnded {
                v: Version::new(1, 3),
            },
        );
        assert_eq!(
            dup_seq,
            Verdict::Ignored(IgnoredReason::StaleOrDuplicateSeq)
        );
    }

    #[test]
    fn duplicate_event_id_is_idempotent_noop() {
        // A newer seq carrying a transition with no state effect is Ignored
        // (idempotent), keeping the tuple intact.
        let obs = live_working();
        let again = apply(
            &obs,
            &LifecycleEvent::TurnStarted {
                v: Version::new(1, 9),
            },
        );
        assert_eq!(again, Verdict::Ignored(IgnoredReason::NoOp));
        assert_eq!(obs.agent, AgentState::Running);
    }

    #[test]
    fn newer_generation_without_adoption_is_rejected() {
        let obs = live_working();
        let verdict = apply(
            &obs,
            &LifecycleEvent::TurnEnded {
                v: Version::new(2, 1),
            },
        );
        assert_eq!(
            verdict,
            Verdict::Rejected(RejectReason::UnadoptedGeneration)
        );
    }

    #[test]
    fn new_generation_adopts_after_old_one_is_gone() {
        let mut obs = live_working();
        let gone = apply(
            &obs,
            &LifecycleEvent::AgentGone {
                v: Version::new(1, 6),
            },
        )
        .expect_applied("gone");
        assert!(!gone.generation_live);
        let adopted = apply(
            &gone,
            &LifecycleEvent::AdoptNewGeneration {
                v: Version::new(2, 0),
            },
        )
        .expect_applied("adoption");
        assert_eq!(adopted.version, Version::new(2, 0));
        assert!(adopted.generation_live);
        assert_eq!(adopted.agent, AgentState::Booting);
        assert_eq!(adopted.public, PublicLifecycle::Created);
        // Events from the adopted generation now flow normally.
        let ready = apply(
            &adopted,
            &LifecycleEvent::Ready {
                v: Version::new(2, 1),
            },
        )
        .expect_applied("ready in new generation");
        assert_eq!(ready.agent, AgentState::Ready);
        obs.version = Version::new(1, 6);
        let _ = obs;
    }

    #[test]
    fn duplicate_live_generation_adoption_is_rejected() {
        let obs = live_working();
        let verdict = apply(
            &obs,
            &LifecycleEvent::AdoptNewGeneration {
                v: Version::new(2, 0),
            },
        );
        assert_eq!(
            verdict,
            Verdict::Rejected(RejectReason::DuplicateLiveGeneration)
        );
    }

    #[test]
    fn supersede_requires_dead_old_generation() {
        let obs = live_working();
        let body = Observation::build(
            Version::new(3, 0),
            true,
            1,
            3,
            0,
            AgentState::Ready,
            DeliveryState::None,
            ResourceState::Attached,
            RecoveryState::None,
            Outcome::Pending,
        );
        let refused = apply(
            &obs,
            &LifecycleEvent::Supersede {
                v: Version::new(3, 0),
                old_generation_dead: false,
                body,
            },
        );
        assert_eq!(refused, Verdict::Rejected(RejectReason::OldGenerationLive));
        let replaced = apply(
            &obs,
            &LifecycleEvent::Supersede {
                v: Version::new(3, 0),
                old_generation_dead: true,
                body,
            },
        );
        let next = replaced.expect_applied("operator supersede");
        assert_eq!(next.agent, AgentState::Ready);
        assert_eq!(next.version, Version::new(3, 0));
    }

    // ------------------------------------------------------------------
    // frozen main sequences
    // ------------------------------------------------------------------

    #[test]
    fn frozen_sequence_created_working_idle_waiting_working() {
        let mut obs = Observation::initial(1, 3);
        assert_eq!(obs.public, PublicLifecycle::Created);
        obs = apply(
            &obs,
            &LifecycleEvent::Ready {
                v: Version::new(1, 1),
            },
        )
        .expect_applied("ready");
        assert_eq!(obs.public, PublicLifecycle::Idle);
        obs = apply(
            &obs,
            &LifecycleEvent::Complete {
                v: Version::new(1, 2),
            },
        )
        .expect_applied("task delivered");
        assert_eq!(obs.delivery, DeliveryState::Pending);
        obs = apply(
            &obs,
            &LifecycleEvent::TurnStarted {
                v: Version::new(1, 3),
            },
        )
        .expect_applied("turn start");
        assert_eq!(obs.public, PublicLifecycle::Working);
        // turn ends with the completion exit still open -> idle_waiting
        obs = apply(
            &obs,
            &LifecycleEvent::TurnEnded {
                v: Version::new(1, 4),
            },
        )
        .expect_applied("turn end");
        assert_eq!(obs.recovery, RecoveryState::IdleWaiting);
        assert_eq!(obs.public, PublicLifecycle::Working);
        // the reinforcement prompt fires: next turn start returns to working
        obs = apply(
            &obs,
            &LifecycleEvent::TurnStarted {
                v: Version::new(1, 5),
            },
        )
        .expect_applied("re-prompt turn start");
        assert_eq!(obs.recovery, RecoveryState::None);
        assert_eq!(obs.public, PublicLifecycle::Working);
    }

    #[test]
    fn frozen_sequence_draining_to_exited() {
        let mut obs = live_working();
        obs = apply(
            &obs,
            &LifecycleEvent::TurnEnded {
                v: Version::new(1, 4),
            },
        )
        .expect_applied("turn end");
        obs = apply(
            &obs,
            &LifecycleEvent::Complete {
                v: Version::new(1, 5),
            },
        )
        .expect_applied("completion intent");
        assert_eq!(obs.recovery, RecoveryState::Draining);
        assert_eq!(obs.public, PublicLifecycle::Working);
        obs = apply(
            &obs,
            &LifecycleEvent::IntentReceipt {
                v: Version::new(1, 6),
            },
        )
        .expect_applied("completion receipt");
        assert_eq!(obs.delivery, DeliveryState::Accepted);
        obs = apply(
            &obs,
            &LifecycleEvent::AgentGone {
                v: Version::new(1, 7),
            },
        )
        .expect_applied("drained exit");
        assert_eq!(obs.public, PublicLifecycle::Exited);
    }

    #[test]
    fn idle_fault_recovers_and_m_n_terminate_on_third_mismatch() {
        let mut obs = Observation::build(
            Version::new(1, 0),
            true,
            1,
            3,
            0,
            AgentState::Idle,
            DeliveryState::None,
            ResourceState::Attached,
            RecoveryState::None,
            Outcome::Pending,
        );
        obs = apply(
            &obs,
            &LifecycleEvent::ReconcileMismatch {
                v: Version::new(1, 1),
            },
        )
        .expect_applied("first mismatch isolates");
        assert_eq!(obs.mismatch_count, 1);
        assert_eq!(obs.recovery, RecoveryState::IdleFault);
        obs = apply(
            &obs,
            &LifecycleEvent::ReconcileOk {
                v: Version::new(1, 2),
            },
        )
        .expect_applied("matching evidence recovers");
        assert_eq!(obs.mismatch_count, 0);
        assert_eq!(obs.recovery, RecoveryState::None);
        obs = apply(
            &obs,
            &LifecycleEvent::ReconcileMismatch {
                v: Version::new(1, 3),
            },
        )
        .expect_applied("mismatch one");
        obs = apply(
            &obs,
            &LifecycleEvent::ReconcileMismatch {
                v: Version::new(1, 4),
            },
        )
        .expect_applied("mismatch two");
        obs = apply(
            &obs,
            &LifecycleEvent::ReconcileMismatch {
                v: Version::new(1, 5),
            },
        )
        .expect_applied("mismatch three terminates");
        assert_eq!(obs.mismatch_count, 3);
        assert_eq!(obs.agent, AgentState::Gone);
        assert_eq!(obs.outcome, Outcome::Failed);
        assert_eq!(obs.public, PublicLifecycle::Exited);
    }

    // ------------------------------------------------------------------
    // helpers
    // ------------------------------------------------------------------

    fn live_working() -> Observation {
        Observation::build(
            Version::new(1, 3),
            true,
            1,
            3,
            0,
            AgentState::Running,
            DeliveryState::None,
            ResourceState::Attached,
            RecoveryState::None,
            Outcome::Pending,
        )
    }

    impl Verdict {
        fn discriminant(&self) -> u8 {
            match self {
                Verdict::Applied(_) => 0,
                Verdict::Ignored(_) => 1,
                Verdict::Rejected(_) => 2,
            }
        }
        fn expect_applied(&self, what: &str) -> Observation {
            match self {
                Verdict::Applied(o) => *o,
                other => panic!("expected applied for {what}, got {other:?}"),
            }
        }
    }
}
