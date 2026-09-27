//! Public projection of the state dimensions, and tuple legality.

use super::state::{
    AgentPhase, DeliveryPhase, Observation, RecoveryPhase, ResourcePhase, TaskState,
};

use crate::event::Lifecycle;

/// Derived public projection (§2.2).
///
/// `task_state` is an input, not a dimension. The session tuple says nothing
/// about how the task it is serving ended, so the caller that owns the task
/// ledger hands its verdict in here, and the projection decides with it.
///
/// Rules, evaluated in order:
/// - Gone exits. A done task with an accepted delivery exits (receipt closed
///   the drain).
/// - A begun task (Done/Failed/Cancelled), an in-flight intent
///   (Pending/Retrying/Exhausted), draining, running, and both recovery
///   substates all project `working`: the session still has open work or an
///   open fault line. A `Done` task whose receipt has not landed is open work —
///   the exit waits for the receipt, it is not conjured from the result.
/// - Ready/Idle with no open line projects `idle`; Booting projects `created`.
pub fn project(
    agent: AgentPhase,
    delivery: DeliveryPhase,
    _resource: ResourcePhase,
    recovery: RecoveryPhase,
    task_state: TaskState,
) -> Lifecycle {
    if agent == AgentPhase::Gone {
        return Lifecycle::Exited;
    }
    if task_state == TaskState::Done && delivery == DeliveryPhase::Accepted {
        return Lifecycle::Exited;
    }
    match task_state {
        TaskState::Done | TaskState::Failed | TaskState::Cancelled => {
            return Lifecycle::Working;
        }
        TaskState::Pending => {}
    }
    match delivery {
        DeliveryPhase::Pending | DeliveryPhase::Retrying | DeliveryPhase::Exhausted => {
            return Lifecycle::Working;
        }
        DeliveryPhase::NoIntent | DeliveryPhase::Accepted => {}
    }
    if recovery == RecoveryPhase::Draining {
        return Lifecycle::Working;
    }
    match agent {
        AgentPhase::Running => Lifecycle::Working,
        AgentPhase::Idle | AgentPhase::Ready => match recovery {
            RecoveryPhase::IdleWaiting | RecoveryPhase::IdleFault => Lifecycle::Working,
            _ => Lifecycle::Idle,
        },
        AgentPhase::Booting | AgentPhase::Gone => Lifecycle::Created,
    }
}

/// Legality of a session tuple: the dimension cross-constraints frozen in §2.2
/// that the session can actually judge.
///
/// Nothing here mentions the task's result, and nothing here re-derives the
/// public view. Both sets of rules existed only because the tuple carried a
/// mirrored task outcome and a stored projection: the outcome could only sit
/// next to a delivery it was welded to, and a derived value could only be
/// checked against itself. With the two gone from the tuple there is nothing
/// left to protect, so a `Done` task with an intent still in flight is a legal
/// session state and projects `working` until the receipt lands.
// clippy::collapsible_if: the post-mortem arm keeps the nested shape of the
// vendored reducer this item came from. The vendor source is no longer
// byte-comparable to it — the tuple rebuild removed the outcome arms — but the
// nesting that needs the allowance survived, so the attribute stays.
#[allow(clippy::collapsible_if)]
pub fn is_legal(obs: &Observation) -> bool {
    if obs.isolate_after == 0 || obs.terminate_after == 0 {
        return false;
    }
    // Recovery substates belong to live generations only.
    if !obs.generation_live && obs.recovery != RecoveryPhase::NoRecovery {
        return false;
    }
    // Post-mortem shape: a gone agent keeps no recovery substate.
    if obs.agent == AgentPhase::Gone {
        if obs.recovery != RecoveryPhase::NoRecovery {
            return false;
        }
    }
    match obs.recovery {
        RecoveryPhase::IdleWaiting | RecoveryPhase::IdleFault => {
            if obs.agent != AgentPhase::Idle {
                return false;
            }
        }
        RecoveryPhase::Draining => {
            if obs.agent != AgentPhase::Idle && obs.agent != AgentPhase::Running {
                return false;
            }
        }
        RecoveryPhase::NoRecovery => {}
    }
    // Accepted is a post-turn fact: a process that never passed Ready has no
    // turn to have delivered.
    if obs.delivery == DeliveryPhase::Accepted && matches!(obs.agent, AgentPhase::Booting) {
        return false;
    }
    // Exhausted means retries burned against an open turn exit.
    if obs.delivery == DeliveryPhase::Exhausted && obs.agent == AgentPhase::Ready {
        return false;
    }
    true
}
