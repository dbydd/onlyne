//! The total reducer: `apply` and the transition helpers behind it.

use super::event::{IgnoredReason, LifecycleEvent, RejectReason, Verdict, event_version};
use super::project::is_legal;
use super::state::{AgentPhase, DeliveryPhase, Observation, RecoveryPhase, ResourcePhase, Version};

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
                LifecycleEvent::Created { .. } => AgentPhase::Booting,
                _ => AgentPhase::Ready,
            };
            step(obs, v, |o| transition(o, agent))
        }
        LifecycleEvent::TurnStarted { .. } => step(obs, v, |o| transition(o, AgentPhase::Running)),
        LifecycleEvent::TurnEnded { .. } => step(obs, v, |o| transition(o, AgentPhase::Idle)),
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
            next.delivery = DeliveryPhase::Pending;
            if obs.agent == AgentPhase::Idle {
                next.recovery = RecoveryPhase::Draining;
            }
            if next == *obs {
                return Verdict::Ignored(IgnoredReason::NoOp);
            }
            finish(obs, next)
        }
        LifecycleEvent::IntentPending { .. } => {
            let mut next = obs.advanced(v);
            match obs.delivery {
                DeliveryPhase::NoIntent | DeliveryPhase::Retrying | DeliveryPhase::Exhausted => {
                    next.delivery = DeliveryPhase::Pending;
                }
                DeliveryPhase::Pending | DeliveryPhase::Accepted => {
                    return Verdict::Ignored(IgnoredReason::NoOp);
                }
            }
            finish(obs, next)
        }
        LifecycleEvent::IntentRetry { .. } => {
            let mut next = obs.advanced(v);
            match obs.delivery {
                DeliveryPhase::Pending => {
                    next.delivery = DeliveryPhase::Retrying;
                    if obs.recovery == RecoveryPhase::IdleWaiting {
                        next.recovery = RecoveryPhase::NoRecovery;
                    }
                }
                DeliveryPhase::NoIntent
                | DeliveryPhase::Retrying
                | DeliveryPhase::Accepted
                | DeliveryPhase::Exhausted => {
                    return Verdict::Ignored(IgnoredReason::NoOp);
                }
            }
            finish(obs, next)
        }
        LifecycleEvent::IntentReceipt { .. } => {
            let mut next = obs.advanced(v);
            match obs.delivery {
                DeliveryPhase::Pending | DeliveryPhase::Retrying => {
                    next.delivery = DeliveryPhase::Accepted;
                    next.recovery = RecoveryPhase::NoRecovery;
                }
                DeliveryPhase::NoIntent | DeliveryPhase::Accepted | DeliveryPhase::Exhausted => {
                    return Verdict::Ignored(IgnoredReason::NoOp);
                }
            }
            finish(obs, next)
        }
        LifecycleEvent::IntentExhausted { .. } => {
            let mut next = obs.advanced(v);
            match obs.delivery {
                DeliveryPhase::Pending | DeliveryPhase::Retrying => {
                    next.delivery = DeliveryPhase::Exhausted;
                    if obs.recovery == RecoveryPhase::IdleWaiting {
                        next.recovery = RecoveryPhase::IdleFault;
                    }
                }
                DeliveryPhase::NoIntent | DeliveryPhase::Accepted | DeliveryPhase::Exhausted => {
                    return Verdict::Ignored(IgnoredReason::NoOp);
                }
            }
            finish(obs, next)
        }
        LifecycleEvent::ResourceAttach { .. } => {
            let mut next = obs.advanced(v);
            match obs.resource {
                ResourcePhase::Detached => next.resource = ResourcePhase::Attached,
                ResourcePhase::Attached => return Verdict::Ignored(IgnoredReason::NoOp),
                ResourcePhase::Closing | ResourcePhase::Closed => {
                    return Verdict::Rejected(RejectReason::UndefinedTransition);
                }
            }
            finish(obs, next)
        }
        LifecycleEvent::ResourceCloseRequested { .. } | LifecycleEvent::ResourceClosed { .. } => {
            let mut next = obs.advanced(v);
            match (event, obs.resource) {
                (LifecycleEvent::ResourceCloseRequested { .. }, ResourcePhase::Detached)
                | (LifecycleEvent::ResourceCloseRequested { .. }, ResourcePhase::Closed) => {
                    return Verdict::Ignored(IgnoredReason::NoOp);
                }
                (LifecycleEvent::ResourceCloseRequested { .. }, _) => {
                    next.resource = ResourcePhase::Closing;
                }
                (LifecycleEvent::ResourceClosed { .. }, ResourcePhase::Detached)
                | (LifecycleEvent::ResourceClosed { .. }, ResourcePhase::Closed) => {
                    return Verdict::Ignored(IgnoredReason::NoOp);
                }
                (LifecycleEvent::ResourceClosed { .. }, _) => {
                    next.resource = ResourcePhase::Closed;
                    // A closed resource kills the agent fact it hosted.
                    if obs.generation_live {
                        next.agent = AgentPhase::Gone;
                        next.generation_live = false;
                        next.recovery = RecoveryPhase::NoRecovery;
                    }
                }
                _ => return Verdict::Rejected(RejectReason::UndefinedTransition),
            }
            finish(obs, next)
        }
        LifecycleEvent::AgentGone { .. } => {
            let mut next = obs.advanced(v);
            if obs.agent == AgentPhase::Gone {
                return Verdict::Ignored(IgnoredReason::NoOp);
            }
            next.agent = AgentPhase::Gone;
            next.generation_live = false;
            next.recovery = RecoveryPhase::NoRecovery;
            next.resource = match obs.resource {
                ResourcePhase::Attached | ResourcePhase::Closing => ResourcePhase::Closing,
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
            if obs.agent == AgentPhase::Idle {
                next.recovery = RecoveryPhase::IdleFault;
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
                next.agent = AgentPhase::Gone;
                next.recovery = RecoveryPhase::NoRecovery;
                next.resource = match obs.resource {
                    ResourcePhase::Attached | ResourcePhase::Closing => ResourcePhase::Closing,
                    r => r,
                };
                return finish(obs, next);
            }
            if count >= obs.isolate_after {
                let mut next = obs.with_mismatch(count).advanced(v);
                if next.agent == AgentPhase::Idle {
                    next.recovery = RecoveryPhase::IdleFault;
                }
                return finish(obs, next);
            }
            Verdict::Applied(obs.with_mismatch(count).advanced(v))
        }
        LifecycleEvent::ReconcileOk { .. } => {
            let mut next = obs.advanced(v);
            next.mismatch_count = 0;
            if obs.recovery == RecoveryPhase::IdleFault {
                next.recovery = RecoveryPhase::NoRecovery;
            }
            // No early no-op here: the comparison tuple carries the four
            // dimensions, not the counter, and resetting the counter is often
            // this event's whole job. A mismatch counted while the agent was
            // running opens no fault line (`isolate` only marks an idle one), so
            // a later confirming probe moves nothing but the counter — which the
            // tuple test read as a replay and dropped. The ladder then only ever
            // climbed: a session could reach `terminate_after` on stale evidence
            // long after its last real disagreement. `finish` is the no-op gate
            // that sees the counter.
            finish(obs, next)
        }
        LifecycleEvent::AdoptNewGeneration { .. } => {
            // Content carries forward; the generation watermark moves. The
            // adopted shape is a live booting generation at the new version.
            let mut next = obs.adopted(v);
            next.agent = AgentPhase::Booting;
            next.delivery = DeliveryPhase::NoIntent;
            next.recovery = RecoveryPhase::NoRecovery;
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
fn transition(obs: &Observation, agent: AgentPhase) -> Option<Observation> {
    let v = event_version(&LifecycleEvent::TurnStarted { v: obs.version });
    let _ = v;
    let mut next = obs.clone();
    match agent {
        AgentPhase::Booting => {
            if obs.agent == AgentPhase::Booting {
                return None;
            }
            next.agent = AgentPhase::Booting;
            next.delivery = DeliveryPhase::NoIntent;
            next.recovery = RecoveryPhase::NoRecovery;
        }
        AgentPhase::Ready => {
            if obs.agent == AgentPhase::Gone {
                return None;
            }
            next.agent = AgentPhase::Ready;
            if obs.recovery == RecoveryPhase::Draining {
                next.recovery = RecoveryPhase::NoRecovery;
            }
        }
        AgentPhase::Running => {
            if obs.agent == AgentPhase::Gone {
                return None;
            }
            // Turn start is the working evidence: it heals recovery substates.
            next.agent = AgentPhase::Running;
            next.recovery = RecoveryPhase::NoRecovery;
        }
        AgentPhase::Idle => {
            if obs.agent == AgentPhase::Gone {
                return None;
            }
            next.agent = AgentPhase::Idle;
            next.recovery = match obs.recovery {
                RecoveryPhase::Draining => RecoveryPhase::Draining,
                RecoveryPhase::IdleFault => RecoveryPhase::IdleFault,
                _ => match obs.delivery {
                    DeliveryPhase::Pending | DeliveryPhase::Retrying => RecoveryPhase::IdleWaiting,
                    _ => RecoveryPhase::NoRecovery,
                },
            };
        }
        AgentPhase::Gone => {
            if obs.agent == AgentPhase::Gone {
                return None;
            }
            next.agent = AgentPhase::Gone;
            next.generation_live = false;
            next.recovery = RecoveryPhase::NoRecovery;
            next.resource = match obs.resource {
                ResourcePhase::Attached | ResourcePhase::Closing => ResourcePhase::Closing,
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
        if obs.agent == AgentPhase::Gone {
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
