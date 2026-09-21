//! Public projection of the state dimensions, and tuple legality.

use super::state::{
    AgentState, DeliveryState, Observation, PublicLifecycle, RecoveryState, ResourceState,
    TaskState,
};

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
    agent: AgentState,
    delivery: DeliveryState,
    _resource: ResourceState,
    recovery: RecoveryState,
    task_state: TaskState,
) -> PublicLifecycle {
    if agent == AgentState::Gone {
        return PublicLifecycle::Exited;
    }
    if task_state == TaskState::Done && delivery == DeliveryState::Accepted {
        return PublicLifecycle::Exited;
    }
    match task_state {
        TaskState::Done | TaskState::Failed | TaskState::Cancelled => {
            return PublicLifecycle::Working;
        }
        TaskState::Pending => {}
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
    if !obs.generation_live && obs.recovery != RecoveryState::None {
        return false;
    }
    // Post-mortem shape: a gone agent keeps no recovery substate.
    if obs.agent == AgentState::Gone {
        if obs.recovery != RecoveryState::None {
            return false;
        }
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
    // Accepted is a post-turn fact: a process that never passed Ready has no
    // turn to have delivered.
    if obs.delivery == DeliveryState::Accepted && matches!(obs.agent, AgentState::Booting) {
        return false;
    }
    // Exhausted means retries burned against an open turn exit.
    if obs.delivery == DeliveryState::Exhausted && obs.agent == AgentState::Ready {
        return false;
    }
    true
}
