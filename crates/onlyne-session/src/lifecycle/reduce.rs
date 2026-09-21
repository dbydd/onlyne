//! The total reducer: `apply` and the transition helpers behind it.

use super::event::{IgnoredReason, LifecycleEvent, RejectReason, Verdict, event_version};
use super::project::is_legal;
use super::state::{AgentState, DeliveryState, Observation, RecoveryState, ResourceState, Version};

/// Apply one event to one observation. Total function: no panics, and every
/// branch yields Applied/Ignored/Rejected.
// clippy::if_same_then_else: the version gate keeps the duplicated arms of the
// vendored reducer this item came from. Note that the item is no longer
// byte-comparable to that source: the tuple rebuild took the task-outcome arms
// out of it. The allowance stays because the shape that needs it stayed too.
#[allow(clippy::if_same_then_else)]
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
            let mut next = body.clone();
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
            // What the task ended as is the ledger's fact, not this tuple's, so
            // an accepted receipt stays accepted: the drain closed before the
            // process went.
            finish(obs, next)
        }
        LifecycleEvent::Cancel { .. } => {
            // Cancelling settles the task's result, which is not a session
            // dimension. The exit still has to drain: the agent is alive, its
            // intent is where it was, and the reducer has nothing to write.
            Verdict::Ignored(IgnoredReason::NoOp)
        }
        LifecycleEvent::Fail { .. } => {
            let mut next = obs.advanced(v);
            // The session-side part of a fault is the open fault line; what the
            // work ended as belongs to the task ledger.
            if obs.agent == AgentState::Idle {
                next.recovery = RecoveryState::IdleFault;
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
                next.resource = match obs.resource {
                    ResourceState::Attached | ResourceState::Closing => ResourceState::Closing,
                    r => r,
                };
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
            finish(obs, next)
        }
        LifecycleEvent::Supersede { body, .. } => {
            if !is_legal(body) {
                return Verdict::Rejected(RejectReason::IllegalObservation);
            }
            let mut next = body.clone();
            next.version = v;
            next.generation_live = true;
            finish(obs, next)
        }
    }
}

/// Apply an agent-state transition with the recovery/delivery coupling frozen
/// in §2.2. `None` means no rule covers the transition.
fn transition(obs: &Observation, agent: AgentState) -> Option<Observation> {
    let v = event_version(&LifecycleEvent::TurnStarted { v: obs.version });
    let _ = v;
    let mut next = obs.clone();
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
/// current one for no-op detection. The tuple carries no derived view, so there
/// is nothing here to recompute before the legality check.
fn finish(obs: &Observation, next: Observation) -> Verdict {
    if !is_legal(&next) {
        return Verdict::Rejected(RejectReason::IllegalObservation);
    }
    if next.tuple() == obs.tuple() && next.mismatch_count == obs.mismatch_count {
        return Verdict::Ignored(IgnoredReason::NoOp);
    }
    Verdict::Applied(next)
}

/// True when a heartbeat body repeats the current tuple (idempotent replay).
fn body_is_no_op(obs: &Observation, body: &Observation) -> bool {
    obs.tuple() == body.tuple()
        && obs.mismatch_count == body.mismatch_count
        && obs.isolate_after == body.isolate_after
        && obs.terminate_after == body.terminate_after
}
