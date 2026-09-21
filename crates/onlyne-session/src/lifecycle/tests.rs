//! Reducer tests: exhaustive matrix, version gating, frozen sequences, host.
//!
//! The tuple carries four state dimensions plus the generation's liveness, so
//! the enumeration varies five. The task's result is not among them: it enters
//! as an argument to `project`, and every projection expectation here names the
//! [`TaskState`] it was computed with.

use super::*;

use crate::host::{HostRef, OrcaPane};

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
/// The projection's input. Not enumerated into the tuple — it is not a session
/// dimension — but every projection is computed over all four.
const TASK_STATES: [TaskState; 4] = [
    TaskState::Pending,
    TaskState::Done,
    TaskState::Failed,
    TaskState::Cancelled,
];
const LIVE: [bool; 2] = [true, false];

/// The five session dimensions the tuple varies: agent × delivery × resource ×
/// recovery × generation liveness = 5*5*4*4*2 = 400 combinations, at version
/// (1, 5).
fn all_observations() -> Vec<Observation> {
    let mut out = Vec::new();
    for &agent in &AGENTS {
        for &delivery in &DELIVERIES {
            for &resource in &RESOURCES {
                for &recovery in &RECOVERIES {
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
                        ));
                    }
                }
            }
        }
    }
    out
}

/// The public view a test expects for one tuple, given the task state its
/// caller holds. Every assertion about `public` goes through here, so the input
/// the tuple no longer carries stays visible in the test.
fn public_of(obs: &Observation, task_state: TaskState) -> PublicLifecycle {
    project(
        obs.agent,
        obs.delivery,
        obs.resource,
        obs.recovery,
        task_state,
    )
}

// clippy::redundant_closure: the filter keeps the shape it had in the vendored
// test this one came from. The enumeration itself is no longer byte-comparable
// to that source: it stopped varying the outcome dimension.
#[allow(clippy::redundant_closure)]
fn legal_observations() -> Vec<Observation> {
    all_observations()
        .into_iter()
        .filter(|o| is_legal(o))
        .collect()
}

/// One representative event per kind at a seq past every enumerated
/// watermark, in both same-generation and future-generation forms.
// clippy::redundant_closure: the `is_legal` filter keeps its vendored shape;
// see the note on [`legal_observations`].
#[allow(clippy::redundant_closure)]
fn events_at(v: Version) -> Vec<LifecycleEvent> {
    // The last candidate is illegal on purpose — an accepted receipt for a
    // process that never passed Ready — and the filter drops it, so the matrix
    // still covers an illegal heartbeat body through `illegal_heartbeat`.
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
        ),
    ]
    .into_iter()
    .filter(|o| is_legal(o))
    .collect();
    let body = legal_bodies[0].clone();
    let illegal_heartbeat = Observation::build(
        v,
        true,
        1,
        3,
        0,
        AgentState::Booting,
        DeliveryState::Accepted,
        ResourceState::Attached,
        RecoveryState::None,
    );
    assert!(
        !is_legal(&illegal_heartbeat),
        "the matrix needs an illegal heartbeat body: {illegal_heartbeat:?}"
    );
    let mut events = vec![
        LifecycleEvent::Created { v },
        LifecycleEvent::Ready { v },
        LifecycleEvent::TurnStarted { v },
        LifecycleEvent::TurnEnded { v },
        LifecycleEvent::Heartbeat {
            v,
            body: body.clone(),
        },
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
            body: body.clone(),
        },
        LifecycleEvent::Supersede {
            v,
            old_generation_dead: false,
            body: body.clone(),
        },
        LifecycleEvent::Heartbeat {
            v,
            body: illegal_heartbeat,
        },
    ];
    for candidate in legal_bodies.iter().skip(1) {
        events.push(LifecycleEvent::Heartbeat {
            v,
            body: candidate.clone(),
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
    assert!(
        checked >= observations.len() * 20,
        "matrix too small: {checked}"
    );
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
fn illegal_heartbeats_are_rejected_by_legality() {
    // Every tuple the legality rules refuse must be refused through the
    // heartbeat door too, so the rules gate what a client can publish rather
    // than only what the reducer can derive.
    for obs in all_observations() {
        if is_legal(&obs) {
            continue;
        }
        let verdict = apply(
            &live_working(),
            &LifecycleEvent::Heartbeat {
                v: Version::new(1, 9),
                body: obs.clone(),
            },
        );
        assert_eq!(
            verdict,
            Verdict::Rejected(RejectReason::IllegalObservation),
            "an illegal tuple slipped through the heartbeat door: {obs:?}"
        );
    }
}

#[test]
fn the_surviving_legality_rules_are_pinned() {
    // One row per rule `is_legal` kept, plus the tuples that only became legal
    // once the task's result left the tuple. The rules are the session's own;
    // nothing here can be satisfied by moving a task field.
    let tuple =
        |live: bool, agent: AgentState, delivery: DeliveryState, recovery: RecoveryState| {
            Observation::build(
                Version::new(1, 5),
                live,
                1,
                3,
                0,
                agent,
                delivery,
                ResourceState::Attached,
                recovery,
            )
        };
    let rows: Vec<(Observation, bool, &str)> = vec![
        (
            tuple(
                true,
                AgentState::Idle,
                DeliveryState::None,
                RecoveryState::None,
            ),
            true,
            "plain idle",
        ),
        (
            tuple(
                true,
                AgentState::Idle,
                DeliveryState::Accepted,
                RecoveryState::None,
            ),
            true,
            "an accepted receipt needs no task result beside it",
        ),
        (
            tuple(
                true,
                AgentState::Idle,
                DeliveryState::Pending,
                RecoveryState::Draining,
            ),
            true,
            "a drain with its intent still open",
        ),
        (
            tuple(
                true,
                AgentState::Idle,
                DeliveryState::Retrying,
                RecoveryState::IdleWaiting,
            ),
            true,
            "a re-prompt waiting on a retried intent",
        ),
        (
            tuple(
                true,
                AgentState::Running,
                DeliveryState::Exhausted,
                RecoveryState::None,
            ),
            true,
            "retries burned against a live turn",
        ),
        (
            tuple(
                false,
                AgentState::Gone,
                DeliveryState::Accepted,
                RecoveryState::None,
            ),
            true,
            "a settled post-mortem row",
        ),
        (
            tuple(
                true,
                AgentState::Idle,
                DeliveryState::Exhausted,
                RecoveryState::IdleFault,
            ),
            true,
            "an exhausted intent parked on a fault line",
        ),
        (
            Observation {
                isolate_after: 0,
                ..tuple(
                    true,
                    AgentState::Idle,
                    DeliveryState::None,
                    RecoveryState::None,
                )
            },
            false,
            "zero isolate_after",
        ),
        (
            Observation {
                terminate_after: 0,
                ..tuple(
                    true,
                    AgentState::Idle,
                    DeliveryState::None,
                    RecoveryState::None,
                )
            },
            false,
            "zero terminate_after",
        ),
        (
            tuple(
                false,
                AgentState::Idle,
                DeliveryState::None,
                RecoveryState::IdleFault,
            ),
            false,
            "a recovery substate on a dead generation",
        ),
        (
            tuple(
                true,
                AgentState::Gone,
                DeliveryState::None,
                RecoveryState::Draining,
            ),
            false,
            "a gone agent cannot still be draining",
        ),
        (
            tuple(
                true,
                AgentState::Running,
                DeliveryState::None,
                RecoveryState::IdleWaiting,
            ),
            false,
            "idle_waiting belongs to an idle agent",
        ),
        (
            tuple(
                true,
                AgentState::Ready,
                DeliveryState::None,
                RecoveryState::IdleFault,
            ),
            false,
            "idle_fault belongs to an idle agent",
        ),
        (
            tuple(
                true,
                AgentState::Ready,
                DeliveryState::None,
                RecoveryState::Draining,
            ),
            false,
            "draining belongs to an idle or running agent",
        ),
        (
            tuple(
                true,
                AgentState::Booting,
                DeliveryState::Accepted,
                RecoveryState::None,
            ),
            false,
            "a booting process delivered nothing",
        ),
        (
            tuple(
                true,
                AgentState::Ready,
                DeliveryState::Exhausted,
                RecoveryState::None,
            ),
            false,
            "exhausted needs an open turn exit",
        ),
    ];
    for (obs, want, why) in rows {
        assert_eq!(is_legal(&obs), want, "{why}: {obs:?}");
    }
}

#[test]
fn the_exit_arms_are_the_only_paths_to_exited() {
    // Over the whole legal space and all four task states: `Exited` comes from
    // exactly two rules — the agent is gone, or a done task's receipt landed —
    // and from nothing else.
    let mut cells = 0usize;
    for obs in legal_observations() {
        for &task_state in &TASK_STATES {
            let got = public_of(&obs, task_state);
            let exits = obs.agent == AgentState::Gone
                || (task_state == TaskState::Done && obs.delivery == DeliveryState::Accepted);
            assert_eq!(
                got == PublicLifecycle::Exited,
                exits,
                "exit rule mismatch for {obs:?} with task {task_state:?}: {got:?}"
            );
            if obs.agent != AgentState::Gone && obs.agent != AgentState::Booting {
                assert_ne!(
                    got,
                    PublicLifecycle::Created,
                    "only a booting agent projects created: {obs:?} + {task_state:?}"
                );
            }
            cells += 1;
        }
    }
    assert!(cells > 1_000, "enumeration too small: {cells}");
}

// ------------------------------------------------------------------
// projection table: the five inputs, one TaskState column
// ------------------------------------------------------------------

#[test]
fn projection_table_over_the_five_inputs() {
    #[derive(Debug)]
    struct Row {
        agent: AgentState,
        delivery: DeliveryState,
        resource: ResourceState,
        recovery: RecoveryState,
        task_state: TaskState,
        want: PublicLifecycle,
    }
    use PublicLifecycle::*;
    let rows = [
        Row {
            agent: AgentState::Booting,
            delivery: DeliveryState::None,
            resource: ResourceState::Detached,
            recovery: RecoveryState::None,
            task_state: TaskState::Pending,
            want: Created,
        },
        Row {
            agent: AgentState::Ready,
            delivery: DeliveryState::None,
            resource: ResourceState::Attached,
            recovery: RecoveryState::None,
            task_state: TaskState::Pending,
            want: Idle,
        },
        Row {
            agent: AgentState::Idle,
            delivery: DeliveryState::None,
            resource: ResourceState::Attached,
            recovery: RecoveryState::None,
            task_state: TaskState::Pending,
            want: Idle,
        },
        Row {
            agent: AgentState::Running,
            delivery: DeliveryState::None,
            resource: ResourceState::Attached,
            recovery: RecoveryState::None,
            task_state: TaskState::Pending,
            want: Working,
        },
        // An in-flight intent is open work whatever the task says next.
        Row {
            agent: AgentState::Idle,
            delivery: DeliveryState::Pending,
            resource: ResourceState::Attached,
            recovery: RecoveryState::IdleWaiting,
            task_state: TaskState::Pending,
            want: Working,
        },
        Row {
            agent: AgentState::Idle,
            delivery: DeliveryState::Retrying,
            resource: ResourceState::Attached,
            recovery: RecoveryState::IdleWaiting,
            task_state: TaskState::Failed,
            want: Working,
        },
        Row {
            agent: AgentState::Ready,
            delivery: DeliveryState::Exhausted,
            resource: ResourceState::Attached,
            recovery: RecoveryState::None,
            task_state: TaskState::Pending,
            want: Working,
        },
        // A recovery substate is an open line on its own.
        Row {
            agent: AgentState::Idle,
            delivery: DeliveryState::None,
            resource: ResourceState::Attached,
            recovery: RecoveryState::Draining,
            task_state: TaskState::Pending,
            want: Working,
        },
        Row {
            agent: AgentState::Idle,
            delivery: DeliveryState::None,
            resource: ResourceState::Attached,
            recovery: RecoveryState::IdleFault,
            task_state: TaskState::Cancelled,
            want: Working,
        },
        // A begun task reads working until its receipt lands: the result alone
        // is not an exit, and neither is a receipt alone.
        Row {
            agent: AgentState::Idle,
            delivery: DeliveryState::Pending,
            resource: ResourceState::Attached,
            recovery: RecoveryState::Draining,
            task_state: TaskState::Done,
            want: Working,
        },
        Row {
            agent: AgentState::Running,
            delivery: DeliveryState::Retrying,
            resource: ResourceState::Attached,
            recovery: RecoveryState::None,
            task_state: TaskState::Done,
            want: Working,
        },
        // A begun task reads working on its own: the result was reached, the
        // session has not finished carrying it out.
        Row {
            agent: AgentState::Idle,
            delivery: DeliveryState::None,
            resource: ResourceState::Attached,
            recovery: RecoveryState::None,
            task_state: TaskState::Done,
            want: Working,
        },
        Row {
            agent: AgentState::Idle,
            delivery: DeliveryState::Accepted,
            resource: ResourceState::Attached,
            recovery: RecoveryState::None,
            task_state: TaskState::Failed,
            want: Working,
        },
        Row {
            agent: AgentState::Idle,
            delivery: DeliveryState::Accepted,
            resource: ResourceState::Attached,
            recovery: RecoveryState::None,
            task_state: TaskState::Cancelled,
            want: Working,
        },
        Row {
            agent: AgentState::Idle,
            delivery: DeliveryState::Accepted,
            resource: ResourceState::Attached,
            recovery: RecoveryState::None,
            task_state: TaskState::Done,
            want: Exited,
        },
        // Gone exits whatever the task says: the process is not there any more.
        Row {
            agent: AgentState::Gone,
            delivery: DeliveryState::None,
            resource: ResourceState::Closed,
            recovery: RecoveryState::None,
            task_state: TaskState::Pending,
            want: Exited,
        },
        Row {
            agent: AgentState::Gone,
            delivery: DeliveryState::Accepted,
            resource: ResourceState::Closed,
            recovery: RecoveryState::None,
            task_state: TaskState::Failed,
            want: Exited,
        },
        // The resource is not read by the projection.
        Row {
            agent: AgentState::Idle,
            delivery: DeliveryState::None,
            resource: ResourceState::Closing,
            recovery: RecoveryState::None,
            task_state: TaskState::Pending,
            want: Idle,
        },
    ];
    for row in rows {
        let got = project(
            row.agent,
            row.delivery,
            row.resource,
            row.recovery,
            row.task_state,
        );
        assert_eq!(
            got, row.want,
            "{row:?} — expected {:?}, got {:?}",
            row.want, got
        );
    }
}

// ------------------------------------------------------------------
// the split the rebuild exists for
// ------------------------------------------------------------------

#[test]
fn a_done_task_with_its_intent_still_in_flight_stays_working() {
    // The payoff. Under the old tuple, `Done` could only sit next to an
    // accepted delivery: the reducer, the settle path, and the plugin all had
    // to invent a receipt for a result that had already arrived. Now the
    // result is an input, the receipt is its own fact, and the two are allowed
    // to disagree for as long as the send is open.
    let obs = apply(
        &live_working(),
        &LifecycleEvent::TurnEnded {
            v: Version::new(1, 4),
        },
    )
    .expect_applied("turn end");
    let obs = apply(
        &obs,
        &LifecycleEvent::Complete {
            v: Version::new(1, 5),
        },
    )
    .expect_applied("completion intent opened");
    assert_eq!(obs.delivery, DeliveryState::Pending);
    assert_eq!(obs.recovery, RecoveryState::Draining);

    // The task ledger says done. The session tuple does not hold that, and
    // saying it out loud changes nothing about the tuple's legality.
    assert!(
        is_legal(&obs),
        "a drain with its intent open must be legal: {obs:?}"
    );
    assert_eq!(
        public_of(&obs, TaskState::Done),
        PublicLifecycle::Working,
        "a done task whose receipt has not landed is still working"
    );

    let obs = apply(
        &obs,
        &LifecycleEvent::IntentReceipt {
            v: Version::new(1, 6),
        },
    )
    .expect_applied("receipt lands");
    assert_eq!(obs.delivery, DeliveryState::Accepted);
    assert_eq!(obs.recovery, RecoveryState::None);
    assert_eq!(
        public_of(&obs, TaskState::Done),
        PublicLifecycle::Exited,
        "the receipt is what closes the exit"
    );
}

#[test]
fn a_settled_receipt_without_a_task_result_is_not_an_exit() {
    // The other half of the split: the session can honestly report an accepted
    // receipt while its task is still open — a second turn on the same session
    // — and the row must read idle, not exited.
    let obs = Observation::build(
        Version::new(1, 2),
        true,
        1,
        3,
        0,
        AgentState::Idle,
        DeliveryState::Accepted,
        ResourceState::Attached,
        RecoveryState::None,
    );
    assert!(is_legal(&obs), "{obs:?}");
    assert_eq!(public_of(&obs, TaskState::Pending), PublicLifecycle::Idle);
    assert_eq!(public_of(&obs, TaskState::Done), PublicLifecycle::Exited);
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
fn a_cancel_reads_as_no_change_to_the_session() {
    // Cancelling settles the task's result in the ledger. The session tuple has
    // no dimension for it, so the reducer takes the cancel and writes nothing.
    let obs = live_working();
    let verdict = apply(
        &obs,
        &LifecycleEvent::Cancel {
            v: Version::new(1, 9),
        },
    );
    assert_eq!(verdict, Verdict::Ignored(IgnoredReason::NoOp));
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
    let obs = live_working();
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
    assert_eq!(adopted.delivery, DeliveryState::None);
    assert_eq!(
        public_of(&adopted, TaskState::Pending),
        PublicLifecycle::Created
    );
    // Events from the adopted generation now flow normally.
    let ready = apply(
        &adopted,
        &LifecycleEvent::Ready {
            v: Version::new(2, 1),
        },
    )
    .expect_applied("ready in new generation");
    assert_eq!(ready.agent, AgentState::Ready);
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
    );
    let refused = apply(
        &obs,
        &LifecycleEvent::Supersede {
            v: Version::new(3, 0),
            old_generation_dead: false,
            body: body.clone(),
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
    assert_eq!(
        public_of(&obs, TaskState::Pending),
        PublicLifecycle::Created
    );
    obs = apply(
        &obs,
        &LifecycleEvent::Ready {
            v: Version::new(1, 1),
        },
    )
    .expect_applied("ready");
    assert_eq!(public_of(&obs, TaskState::Pending), PublicLifecycle::Idle);
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
    assert_eq!(
        public_of(&obs, TaskState::Pending),
        PublicLifecycle::Working
    );
    // turn ends with the completion exit still open -> idle_waiting
    obs = apply(
        &obs,
        &LifecycleEvent::TurnEnded {
            v: Version::new(1, 4),
        },
    )
    .expect_applied("turn end");
    assert_eq!(obs.recovery, RecoveryState::IdleWaiting);
    assert_eq!(
        public_of(&obs, TaskState::Pending),
        PublicLifecycle::Working
    );
    // the reinforcement prompt fires: next turn start returns to working
    obs = apply(
        &obs,
        &LifecycleEvent::TurnStarted {
            v: Version::new(1, 5),
        },
    )
    .expect_applied("re-prompt turn start");
    assert_eq!(obs.recovery, RecoveryState::None);
    assert_eq!(
        public_of(&obs, TaskState::Pending),
        PublicLifecycle::Working
    );
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
    assert_eq!(
        public_of(&obs, TaskState::Pending),
        PublicLifecycle::Working
    );
    obs = apply(
        &obs,
        &LifecycleEvent::IntentReceipt {
            v: Version::new(1, 6),
        },
    )
    .expect_applied("completion receipt");
    assert_eq!(obs.delivery, DeliveryState::Accepted);
    // The receipt is the session's last word; the exit needs the task's too.
    assert_eq!(
        public_of(&obs, TaskState::Done),
        PublicLifecycle::Exited,
        "drained exit"
    );
    // And the process ending is an exit on its own, whatever the task said.
    obs = apply(
        &obs,
        &LifecycleEvent::AgentGone {
            v: Version::new(1, 7),
        },
    )
    .expect_applied("agent gone after the drain");
    assert_eq!(obs.agent, AgentState::Gone);
    assert_eq!(public_of(&obs, TaskState::Pending), PublicLifecycle::Exited);
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
    assert!(!obs.generation_live);
    assert_eq!(public_of(&obs, TaskState::Pending), PublicLifecycle::Exited);
}

#[test]
fn a_fault_opens_the_session_side_line_only() {
    // Fail used to write `failed` into the tuple. The session's own part of a
    // fault is the open recovery line, and only while its agent is idle.
    let idle = Observation::build(
        Version::new(1, 0),
        true,
        1,
        3,
        0,
        AgentState::Idle,
        DeliveryState::None,
        ResourceState::Attached,
        RecoveryState::None,
    );
    let faulted = apply(
        &idle,
        &LifecycleEvent::Fail {
            v: Version::new(1, 1),
        },
    )
    .expect_applied("fault on an idle session");
    assert_eq!(faulted.recovery, RecoveryState::IdleFault);
    assert_eq!(
        public_of(&faulted, TaskState::Failed),
        PublicLifecycle::Working
    );
    // A running session has no idle line to open: the fault is reported, and
    // the turn keeps its own facts.
    let running = apply(
        &live_working(),
        &LifecycleEvent::Fail {
            v: Version::new(1, 9),
        },
    );
    assert_eq!(running, Verdict::Ignored(IgnoredReason::NoOp));
}

#[test]
fn an_agent_death_leaves_an_accepted_receipt_alone() {
    // The post-mortem rewrite that turned an accepted receipt back into a
    // retrying intent existed only so the dead row could stay legal next to a
    // mirrored result. With the result out of the tuple, a receipt that landed
    // stays landed.
    let obs = Observation::build(
        Version::new(1, 0),
        true,
        1,
        3,
        0,
        AgentState::Idle,
        DeliveryState::Accepted,
        ResourceState::Attached,
        RecoveryState::None,
    );
    let gone = apply(
        &obs,
        &LifecycleEvent::AgentGone {
            v: Version::new(1, 1),
        },
    )
    .expect_applied("gone after the receipt");
    assert_eq!(gone.agent, AgentState::Gone);
    assert!(!gone.generation_live);
    assert_eq!(gone.delivery, DeliveryState::Accepted);
    assert_eq!(gone.resource, ResourceState::Closing);
    assert!(is_legal(&gone), "{gone:?}");
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
    )
}

// ------------------------------------------------------------------
// reported host (the pane a session process runs in)
// ------------------------------------------------------------------

fn orca_host(pane_key: &str) -> HostRef {
    HostRef {
        orca: Some(OrcaPane {
            pane_key: pane_key.to_string(),
            tab_id: Some("tab-1".to_string()),
            leaf_id: Some("leaf-1".to_string()),
            handle: Some("term_1".to_string()),
        }),
    }
}

#[test]
fn a_reported_host_round_trips_and_is_no_state_dimension() {
    let body = live_working().with_host(Some(orca_host("tab-1:leaf-1")));
    assert!(is_legal(&body), "a host never makes a tuple illegal");
    for &task_state in &TASK_STATES {
        assert_eq!(
            public_of(&body, task_state),
            public_of(&live_working(), task_state),
            "the projection ignores the host"
        );
    }

    let encoded = serde_json::to_string(&body).unwrap();
    assert!(
        encoded.contains(r#""host":{"orca":{"pane_key":"tab-1:leaf-1""#),
        "{encoded}"
    );
    assert_eq!(serde_json::from_str::<Observation>(&encoded).unwrap(), body);

    // A body that carries no host keeps the bytes it always had.
    let bare = serde_json::to_string(&live_working()).unwrap();
    assert!(!bare.contains("host"), "{bare}");
    assert_eq!(
        serde_json::from_str::<Observation>(&bare).unwrap(),
        live_working()
    );
}

#[test]
fn an_observation_serializes_the_session_dimensions_only() {
    // `observed_json` is the stored form, and it is now exactly the session's
    // own facts in declaration order: neither the task's result nor a derived
    // public view rides with it. Pinned as bytes because both stores and the
    // wire carry this text.
    assert_eq!(
        serde_json::to_string(&live_working()).unwrap(),
        r#"{"version":{"generation":1,"seq":3},"generation_live":true,"isolate_after":1,"terminate_after":3,"mismatch_count":0,"agent":"running","delivery":"none","resource":"attached","recovery":"none"}"#
    );
    assert_eq!(
        serde_json::to_string(&live_working().with_host(Some(orca_host("tab-1:leaf-1")))).unwrap(),
        r#"{"version":{"generation":1,"seq":3},"generation_live":true,"isolate_after":1,"terminate_after":3,"mismatch_count":0,"agent":"running","delivery":"none","resource":"attached","recovery":"none","host":{"orca":{"pane_key":"tab-1:leaf-1","tab_id":"tab-1","leaf_id":"leaf-1","handle":"term_1"}}}"#
    );
}

#[test]
fn a_binding_only_heartbeat_advances_the_row() {
    // The state is what the row already says; only the host is news, and the
    // row must take it — otherwise a panel scoped by the binding would never
    // learn the pane it belongs to.
    let obs = live_working();
    let bound = obs.clone().with_host(Some(orca_host("tab-1:leaf-1")));
    let v = Version::new(1, 4);
    let applied = apply(
        &obs,
        &LifecycleEvent::Heartbeat {
            v,
            body: bound.clone(),
        },
    );
    let next = applied.expect_applied("a heartbeat that only names its pane");
    assert_eq!(next.host, bound.host);
    assert_eq!(next.agent, obs.agent);
    assert_eq!(next.version, v);

    // The same body again is the idempotent replay it is.
    let replay = apply(
        &next,
        &LifecycleEvent::Heartbeat {
            v: Version::new(1, 5),
            body: bound,
        },
    );
    assert_eq!(replay, Verdict::Ignored(IgnoredReason::NoOp));

    // A body that drops the host is a change too: the process stopped saying
    // where it runs, and the row must stop claiming it.
    let dropped = apply(
        &next,
        &LifecycleEvent::Heartbeat {
            v: Version::new(1, 6),
            body: obs,
        },
    );
    assert_eq!(
        dropped.expect_applied("a heartbeat with no host").host,
        None
    );
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
            Verdict::Applied(o) => o.clone(),
            other => panic!("expected applied for {what}, got {other:?}"),
        }
    }
}
