//! Reducer tests: exhaustive matrix, version gating, frozen sequences, host.
//!
//! The tuple carries four state dimensions plus the generation's liveness, so
//! the enumeration varies five. The task's result is not among them: it enters
//! as an argument to `project`, and every projection expectation here names the
//! [`TaskState`] it was computed with.

use super::*;

use super::host::{HostRef, OrcaPane};

// ------------------------------------------------------------------
// exhaustive enumeration helpers
// ------------------------------------------------------------------

const AGENTS: [AgentPhase; 5] = [
    AgentPhase::Booting,
    AgentPhase::Ready,
    AgentPhase::Running,
    AgentPhase::Idle,
    AgentPhase::Gone,
];
const DELIVERIES: [DeliveryPhase; 5] = [
    DeliveryPhase::NoIntent,
    DeliveryPhase::Pending,
    DeliveryPhase::Retrying,
    DeliveryPhase::Accepted,
    DeliveryPhase::Exhausted,
];
const RESOURCES: [ResourcePhase; 4] = [
    ResourcePhase::Detached,
    ResourcePhase::Attached,
    ResourcePhase::Closing,
    ResourcePhase::Closed,
];
const RECOVERIES: [RecoveryPhase; 4] = [
    RecoveryPhase::NoRecovery,
    RecoveryPhase::IdleWaiting,
    RecoveryPhase::IdleFault,
    RecoveryPhase::Draining,
];
/// The projection's input. Not enumerated into the tuple — it is not a session
/// dimension — but every projection is computed over all five.
const TASK_STATES: [TaskState; 5] = [
    TaskState::Pending,
    TaskState::Done,
    TaskState::Failed,
    TaskState::Cancelled,
    TaskState::Blocked,
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
fn public_of(obs: &Observation, task_state: TaskState) -> Lifecycle {
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
            AgentPhase::Idle,
            DeliveryPhase::Pending,
            ResourcePhase::Attached,
            RecoveryPhase::IdleWaiting,
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
        AgentPhase::Booting,
        DeliveryPhase::Accepted,
        ResourcePhase::Attached,
        RecoveryPhase::NoRecovery,
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
        LifecycleEvent::Suspend { v },
        LifecycleEvent::Resume { v },
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
    assert_eq!(
        AGENTS.len(),
        AgentPhase::VARIANT_COUNT,
        "update AGENTS when AgentPhase gains a variant"
    );
    assert_eq!(
        DELIVERIES.len(),
        DeliveryPhase::VARIANT_COUNT,
        "update DELIVERIES when DeliveryPhase gains a variant"
    );
    assert_eq!(
        RESOURCES.len(),
        ResourcePhase::VARIANT_COUNT,
        "update RESOURCES when ResourcePhase gains a variant"
    );
    assert_eq!(
        RECOVERIES.len(),
        RecoveryPhase::VARIANT_COUNT,
        "update RECOVERIES when RecoveryPhase gains a variant"
    );
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
fn legality_rules_match_spec_section_2_2() {
    // One row per cross-constraint §2.2 of the design freezes and `is_legal`
    // keeps, plus the tuples that only became legal once the task's result left
    // the tuple. The rules are the session's own; nothing here can be satisfied
    // by moving a task field.
    let tuple =
        |live: bool, agent: AgentPhase, delivery: DeliveryPhase, recovery: RecoveryPhase| {
            Observation::build(
                Version::new(1, 5),
                live,
                1,
                3,
                0,
                agent,
                delivery,
                ResourcePhase::Attached,
                recovery,
            )
        };
    let rows: Vec<(Observation, bool, &str)> = vec![
        (
            tuple(
                true,
                AgentPhase::Idle,
                DeliveryPhase::NoIntent,
                RecoveryPhase::NoRecovery,
            ),
            true,
            "§2.2 rule: a plain idle tuple — no intent, no recovery substate — binds no cross-constraint",
        ),
        (
            tuple(
                true,
                AgentPhase::Idle,
                DeliveryPhase::Accepted,
                RecoveryPhase::NoRecovery,
            ),
            true,
            "§2.2 rule: an accepted receipt needs no task result beside it — accepted with an idle agent is legal",
        ),
        (
            tuple(
                true,
                AgentPhase::Idle,
                DeliveryPhase::Pending,
                RecoveryPhase::Draining,
            ),
            true,
            "§2.2 rule: draining belongs to an idle agent — a drain with its intent still open",
        ),
        (
            tuple(
                true,
                AgentPhase::Idle,
                DeliveryPhase::Retrying,
                RecoveryPhase::IdleWaiting,
            ),
            true,
            "§2.2 rule: idle_waiting belongs to an idle agent — a re-prompt waiting on a retried intent",
        ),
        (
            tuple(
                true,
                AgentPhase::Running,
                DeliveryPhase::Exhausted,
                RecoveryPhase::NoRecovery,
            ),
            true,
            "§2.2 rule: exhausted needs an open turn exit — retries burned against a live running turn",
        ),
        (
            tuple(
                false,
                AgentPhase::Gone,
                DeliveryPhase::Accepted,
                RecoveryPhase::NoRecovery,
            ),
            true,
            "§2.2 rule: post-mortem row: gone agent, accepted delivery",
        ),
        (
            tuple(
                true,
                AgentPhase::Idle,
                DeliveryPhase::Exhausted,
                RecoveryPhase::IdleFault,
            ),
            true,
            "§2.2 rule: idle_fault belongs to an idle agent — an exhausted intent parked on a fault line",
        ),
        (
            Observation {
                isolate_after: 0,
                ..tuple(
                    true,
                    AgentPhase::Idle,
                    DeliveryPhase::NoIntent,
                    RecoveryPhase::NoRecovery,
                )
            },
            false,
            "§2.2 rule: reconcile policy counters are nonzero — zero isolate_after",
        ),
        (
            Observation {
                terminate_after: 0,
                ..tuple(
                    true,
                    AgentPhase::Idle,
                    DeliveryPhase::NoIntent,
                    RecoveryPhase::NoRecovery,
                )
            },
            false,
            "§2.2 rule: reconcile policy counters are nonzero — zero terminate_after",
        ),
        (
            tuple(
                false,
                AgentPhase::Idle,
                DeliveryPhase::NoIntent,
                RecoveryPhase::IdleFault,
            ),
            false,
            "§2.2 rule: recovery substates belong to live generations only",
        ),
        (
            tuple(
                true,
                AgentPhase::Gone,
                DeliveryPhase::NoIntent,
                RecoveryPhase::Draining,
            ),
            false,
            "§2.2 rule: a gone agent keeps no recovery substate — a gone agent cannot still be draining",
        ),
        (
            tuple(
                true,
                AgentPhase::Running,
                DeliveryPhase::NoIntent,
                RecoveryPhase::IdleWaiting,
            ),
            false,
            "§2.2 rule: idle_waiting belongs to an idle agent",
        ),
        (
            tuple(
                true,
                AgentPhase::Ready,
                DeliveryPhase::NoIntent,
                RecoveryPhase::IdleFault,
            ),
            false,
            "§2.2 rule: idle_fault belongs to an idle agent",
        ),
        (
            tuple(
                true,
                AgentPhase::Ready,
                DeliveryPhase::NoIntent,
                RecoveryPhase::Draining,
            ),
            false,
            "§2.2 rule: draining belongs to an idle or running agent",
        ),
        (
            tuple(
                true,
                AgentPhase::Booting,
                DeliveryPhase::Accepted,
                RecoveryPhase::NoRecovery,
            ),
            false,
            "§2.2 rule: accepted is a post-turn fact — a booting process delivered nothing",
        ),
        (
            tuple(
                true,
                AgentPhase::Ready,
                DeliveryPhase::Exhausted,
                RecoveryPhase::NoRecovery,
            ),
            false,
            "§2.2 rule: exhausted needs an open turn exit — never a ready agent",
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
            let exits = obs.agent == AgentPhase::Gone
                || (task_state == TaskState::Done && obs.delivery == DeliveryPhase::Accepted);
            assert_eq!(
                got == Lifecycle::Exited,
                exits,
                "exit rule mismatch for {obs:?} with task {task_state:?}: {got:?}"
            );
            if obs.agent != AgentPhase::Gone && obs.agent != AgentPhase::Booting {
                assert_ne!(
                    got,
                    Lifecycle::Created,
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
        agent: AgentPhase,
        delivery: DeliveryPhase,
        resource: ResourcePhase,
        recovery: RecoveryPhase,
        task_state: TaskState,
        want: Lifecycle,
    }
    use Lifecycle::*;
    let rows = [
        Row {
            agent: AgentPhase::Booting,
            delivery: DeliveryPhase::NoIntent,
            resource: ResourcePhase::Detached,
            recovery: RecoveryPhase::NoRecovery,
            task_state: TaskState::Pending,
            want: Created,
        },
        Row {
            agent: AgentPhase::Ready,
            delivery: DeliveryPhase::NoIntent,
            resource: ResourcePhase::Attached,
            recovery: RecoveryPhase::NoRecovery,
            task_state: TaskState::Pending,
            want: Idle,
        },
        Row {
            agent: AgentPhase::Idle,
            delivery: DeliveryPhase::NoIntent,
            resource: ResourcePhase::Attached,
            recovery: RecoveryPhase::NoRecovery,
            task_state: TaskState::Pending,
            want: Idle,
        },
        Row {
            agent: AgentPhase::Running,
            delivery: DeliveryPhase::NoIntent,
            resource: ResourcePhase::Attached,
            recovery: RecoveryPhase::NoRecovery,
            task_state: TaskState::Pending,
            want: Working,
        },
        // An in-flight intent is open work whatever the task says next.
        Row {
            agent: AgentPhase::Idle,
            delivery: DeliveryPhase::Pending,
            resource: ResourcePhase::Attached,
            recovery: RecoveryPhase::IdleWaiting,
            task_state: TaskState::Pending,
            want: Working,
        },
        Row {
            agent: AgentPhase::Idle,
            delivery: DeliveryPhase::Retrying,
            resource: ResourcePhase::Attached,
            recovery: RecoveryPhase::IdleWaiting,
            task_state: TaskState::Failed,
            want: Working,
        },
        Row {
            agent: AgentPhase::Ready,
            delivery: DeliveryPhase::Exhausted,
            resource: ResourcePhase::Attached,
            recovery: RecoveryPhase::NoRecovery,
            task_state: TaskState::Pending,
            want: Working,
        },
        // A recovery substate is an open line on its own.
        Row {
            agent: AgentPhase::Idle,
            delivery: DeliveryPhase::NoIntent,
            resource: ResourcePhase::Attached,
            recovery: RecoveryPhase::Draining,
            task_state: TaskState::Pending,
            want: Working,
        },
        Row {
            agent: AgentPhase::Idle,
            delivery: DeliveryPhase::NoIntent,
            resource: ResourcePhase::Attached,
            recovery: RecoveryPhase::IdleFault,
            task_state: TaskState::Cancelled,
            want: Working,
        },
        // A begun task reads working until its receipt lands: the result alone
        // is not an exit, and neither is a receipt alone.
        Row {
            agent: AgentPhase::Idle,
            delivery: DeliveryPhase::Pending,
            resource: ResourcePhase::Attached,
            recovery: RecoveryPhase::Draining,
            task_state: TaskState::Done,
            want: Working,
        },
        Row {
            agent: AgentPhase::Running,
            delivery: DeliveryPhase::Retrying,
            resource: ResourcePhase::Attached,
            recovery: RecoveryPhase::NoRecovery,
            task_state: TaskState::Done,
            want: Working,
        },
        // A begun task reads working on its own: the result was reached, the
        // session has not finished carrying it out.
        Row {
            agent: AgentPhase::Idle,
            delivery: DeliveryPhase::NoIntent,
            resource: ResourcePhase::Attached,
            recovery: RecoveryPhase::NoRecovery,
            task_state: TaskState::Done,
            want: Working,
        },
        Row {
            agent: AgentPhase::Idle,
            delivery: DeliveryPhase::Accepted,
            resource: ResourcePhase::Attached,
            recovery: RecoveryPhase::NoRecovery,
            task_state: TaskState::Failed,
            want: Working,
        },
        Row {
            agent: AgentPhase::Idle,
            delivery: DeliveryPhase::Accepted,
            resource: ResourcePhase::Attached,
            recovery: RecoveryPhase::NoRecovery,
            task_state: TaskState::Cancelled,
            want: Working,
        },
        Row {
            agent: AgentPhase::Idle,
            delivery: DeliveryPhase::Accepted,
            resource: ResourcePhase::Attached,
            recovery: RecoveryPhase::NoRecovery,
            task_state: TaskState::Done,
            want: Exited,
        },
        // Gone exits whatever the task says: the process is not there any more.
        Row {
            agent: AgentPhase::Gone,
            delivery: DeliveryPhase::NoIntent,
            resource: ResourcePhase::Closed,
            recovery: RecoveryPhase::NoRecovery,
            task_state: TaskState::Pending,
            want: Exited,
        },
        Row {
            agent: AgentPhase::Gone,
            delivery: DeliveryPhase::Accepted,
            resource: ResourcePhase::Closed,
            recovery: RecoveryPhase::NoRecovery,
            task_state: TaskState::Failed,
            want: Exited,
        },
        // The resource is not read by the projection.
        Row {
            agent: AgentPhase::Idle,
            delivery: DeliveryPhase::NoIntent,
            resource: ResourcePhase::Closing,
            recovery: RecoveryPhase::NoRecovery,
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
    assert_eq!(obs.delivery, DeliveryPhase::Pending);
    assert_eq!(obs.recovery, RecoveryPhase::Draining);

    // The task ledger says done. The session tuple does not hold that, and
    // saying it out loud changes nothing about the tuple's legality.
    assert!(
        is_legal(&obs),
        "a drain with its intent open must be legal: {obs:?}"
    );
    assert_eq!(
        public_of(&obs, TaskState::Done),
        Lifecycle::Working,
        "a done task whose receipt has not landed is still working"
    );

    let obs = apply(
        &obs,
        &LifecycleEvent::IntentReceipt {
            v: Version::new(1, 6),
        },
    )
    .expect_applied("receipt lands");
    assert_eq!(obs.delivery, DeliveryPhase::Accepted);
    assert_eq!(obs.recovery, RecoveryPhase::NoRecovery);
    assert_eq!(
        public_of(&obs, TaskState::Done),
        Lifecycle::Exited,
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
        AgentPhase::Idle,
        DeliveryPhase::Accepted,
        ResourcePhase::Attached,
        RecoveryPhase::NoRecovery,
    );
    assert!(is_legal(&obs), "{obs:?}");
    assert_eq!(public_of(&obs, TaskState::Pending), Lifecycle::Idle);
    assert_eq!(public_of(&obs, TaskState::Done), Lifecycle::Exited);
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
    assert_eq!(obs.agent, AgentPhase::Running);
}

#[test]
fn suspend_idle_attached_closes_resource_and_keeps_generation_live() {
    let obs = Observation::build(
        Version::new(1, 3),
        true,
        1,
        3,
        0,
        AgentPhase::Idle,
        DeliveryPhase::NoIntent,
        ResourcePhase::Attached,
        RecoveryPhase::NoRecovery,
    );
    let suspended = apply(
        &obs,
        &LifecycleEvent::Suspend {
            v: Version::new(1, 4),
        },
    )
    .expect_applied("suspend idle attached");
    assert_eq!(suspended.resource, ResourcePhase::Closed);
    assert!(suspended.generation_live);
    assert_eq!(suspended.agent, AgentPhase::Idle);
    assert_eq!(public_of(&suspended, TaskState::Pending), Lifecycle::Idle);
    assert!(is_legal(&suspended), "{suspended:?}");
}

#[test]
fn suspend_rejects_detached_and_running_and_is_noop_when_closed() {
    let detached = Observation::build(
        Version::new(1, 3),
        true,
        1,
        3,
        0,
        AgentPhase::Idle,
        DeliveryPhase::NoIntent,
        ResourcePhase::Detached,
        RecoveryPhase::NoRecovery,
    );
    assert_eq!(
        apply(
            &detached,
            &LifecycleEvent::Suspend {
                v: Version::new(1, 4),
            },
        ),
        Verdict::Rejected(RejectReason::UndefinedTransition)
    );
    let running = live_working();
    assert_eq!(
        apply(
            &running,
            &LifecycleEvent::Suspend {
                v: Version::new(1, 4),
            },
        ),
        Verdict::Rejected(RejectReason::UndefinedTransition)
    );
    let closed = Observation::build(
        Version::new(1, 3),
        true,
        1,
        3,
        0,
        AgentPhase::Idle,
        DeliveryPhase::NoIntent,
        ResourcePhase::Closed,
        RecoveryPhase::NoRecovery,
    );
    assert_eq!(
        apply(
            &closed,
            &LifecycleEvent::Suspend {
                v: Version::new(1, 4),
            },
        ),
        Verdict::Ignored(IgnoredReason::NoOp)
    );
}

#[test]
fn resume_attaches_closed_resource_and_preserves_idle_projection() {
    let closed = Observation::build(
        Version::new(1, 3),
        true,
        1,
        3,
        0,
        AgentPhase::Idle,
        DeliveryPhase::NoIntent,
        ResourcePhase::Closed,
        RecoveryPhase::NoRecovery,
    );
    let resumed = apply(
        &closed,
        &LifecycleEvent::Resume {
            v: Version::new(1, 4),
        },
    )
    .expect_applied("resume closed");
    assert_eq!(resumed.resource, ResourcePhase::Attached);
    assert_eq!(resumed.agent, AgentPhase::Idle);
    assert!(resumed.generation_live);
    assert_eq!(public_of(&resumed, TaskState::Pending), Lifecycle::Idle);
    assert!(is_legal(&resumed), "{resumed:?}");
}

#[test]
fn resume_attached_is_noop_and_detached_or_closing_is_rejected() {
    let attached = Observation::build(
        Version::new(1, 3),
        true,
        1,
        3,
        0,
        AgentPhase::Idle,
        DeliveryPhase::NoIntent,
        ResourcePhase::Attached,
        RecoveryPhase::NoRecovery,
    );
    assert_eq!(
        apply(
            &attached,
            &LifecycleEvent::Resume {
                v: Version::new(1, 4),
            },
        ),
        Verdict::Ignored(IgnoredReason::NoOp)
    );
    for resource in [ResourcePhase::Detached, ResourcePhase::Closing] {
        let obs = Observation::build(
            Version::new(1, 3),
            true,
            1,
            3,
            0,
            AgentPhase::Idle,
            DeliveryPhase::NoIntent,
            resource,
            RecoveryPhase::NoRecovery,
        );
        assert_eq!(
            apply(
                &obs,
                &LifecycleEvent::Resume {
                    v: Version::new(1, 4),
                },
            ),
            Verdict::Rejected(RejectReason::UndefinedTransition)
        );
    }
}

#[test]
fn closed_resource_with_live_generation_is_legal() {
    let obs = Observation::build(
        Version::new(1, 3),
        true,
        1,
        3,
        0,
        AgentPhase::Idle,
        DeliveryPhase::NoIntent,
        ResourcePhase::Closed,
        RecoveryPhase::NoRecovery,
    );
    assert!(is_legal(&obs), "{obs:?}");
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
    assert_eq!(adopted.agent, AgentPhase::Booting);
    assert_eq!(adopted.delivery, DeliveryPhase::NoIntent);
    assert_eq!(public_of(&adopted, TaskState::Pending), Lifecycle::Created);
    // Events from the adopted generation now flow normally.
    let ready = apply(
        &adopted,
        &LifecycleEvent::Ready {
            v: Version::new(2, 1),
        },
    )
    .expect_applied("ready in new generation");
    assert_eq!(ready.agent, AgentPhase::Ready);
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
        AgentPhase::Ready,
        DeliveryPhase::NoIntent,
        ResourcePhase::Attached,
        RecoveryPhase::NoRecovery,
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
    assert_eq!(next.agent, AgentPhase::Ready);
    assert_eq!(next.version, Version::new(3, 0));
}

// ------------------------------------------------------------------
// frozen main sequences
// ------------------------------------------------------------------

#[test]
fn frozen_sequence_created_working_idle_waiting_working() {
    let mut obs = Observation::initial(1, 3);
    assert_eq!(public_of(&obs, TaskState::Pending), Lifecycle::Created);
    obs = apply(
        &obs,
        &LifecycleEvent::Ready {
            v: Version::new(1, 1),
        },
    )
    .expect_applied("ready");
    assert_eq!(public_of(&obs, TaskState::Pending), Lifecycle::Idle);
    obs = apply(
        &obs,
        &LifecycleEvent::Complete {
            v: Version::new(1, 2),
        },
    )
    .expect_applied("task delivered");
    assert_eq!(obs.delivery, DeliveryPhase::Pending);
    obs = apply(
        &obs,
        &LifecycleEvent::TurnStarted {
            v: Version::new(1, 3),
        },
    )
    .expect_applied("turn start");
    assert_eq!(public_of(&obs, TaskState::Pending), Lifecycle::Working);
    // turn ends with the completion exit still open -> idle_waiting
    obs = apply(
        &obs,
        &LifecycleEvent::TurnEnded {
            v: Version::new(1, 4),
        },
    )
    .expect_applied("turn end");
    assert_eq!(obs.recovery, RecoveryPhase::IdleWaiting);
    assert_eq!(public_of(&obs, TaskState::Pending), Lifecycle::Working);
    // the reinforcement prompt fires: next turn start returns to working
    obs = apply(
        &obs,
        &LifecycleEvent::TurnStarted {
            v: Version::new(1, 5),
        },
    )
    .expect_applied("re-prompt turn start");
    assert_eq!(obs.recovery, RecoveryPhase::NoRecovery);
    assert_eq!(public_of(&obs, TaskState::Pending), Lifecycle::Working);
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
    assert_eq!(obs.recovery, RecoveryPhase::Draining);
    assert_eq!(public_of(&obs, TaskState::Pending), Lifecycle::Working);
    obs = apply(
        &obs,
        &LifecycleEvent::IntentReceipt {
            v: Version::new(1, 6),
        },
    )
    .expect_applied("completion receipt");
    assert_eq!(obs.delivery, DeliveryPhase::Accepted);
    // The receipt is the session's last word; the exit needs the task's too.
    assert_eq!(
        public_of(&obs, TaskState::Done),
        Lifecycle::Exited,
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
    assert_eq!(obs.agent, AgentPhase::Gone);
    assert_eq!(public_of(&obs, TaskState::Pending), Lifecycle::Exited);
}

#[test]
fn idle_fault_recovers_and_m_n_terminate_on_third_mismatch() {
    let mut obs = Observation::build(
        Version::new(1, 0),
        true,
        1,
        3,
        0,
        AgentPhase::Idle,
        DeliveryPhase::NoIntent,
        ResourcePhase::Attached,
        RecoveryPhase::NoRecovery,
    );
    obs = apply(
        &obs,
        &LifecycleEvent::ReconcileMismatch {
            v: Version::new(1, 1),
        },
    )
    .expect_applied("first mismatch isolates");
    assert_eq!(obs.mismatch_count, 1);
    assert_eq!(obs.recovery, RecoveryPhase::IdleFault);
    obs = apply(
        &obs,
        &LifecycleEvent::ReconcileOk {
            v: Version::new(1, 2),
        },
    )
    .expect_applied("matching evidence recovers");
    assert_eq!(obs.mismatch_count, 0);
    assert_eq!(obs.recovery, RecoveryPhase::NoRecovery);
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
    assert_eq!(obs.agent, AgentPhase::Gone);
    assert!(!obs.generation_live);
    assert_eq!(public_of(&obs, TaskState::Pending), Lifecycle::Exited);
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
        AgentPhase::Idle,
        DeliveryPhase::NoIntent,
        ResourcePhase::Attached,
        RecoveryPhase::NoRecovery,
    );
    let faulted = apply(
        &idle,
        &LifecycleEvent::Fail {
            v: Version::new(1, 1),
        },
    )
    .expect_applied("fault on an idle session");
    assert_eq!(faulted.recovery, RecoveryPhase::IdleFault);
    assert_eq!(public_of(&faulted, TaskState::Failed), Lifecycle::Working);
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
        AgentPhase::Idle,
        DeliveryPhase::Accepted,
        ResourcePhase::Attached,
        RecoveryPhase::NoRecovery,
    );
    let gone = apply(
        &obs,
        &LifecycleEvent::AgentGone {
            v: Version::new(1, 1),
        },
    )
    .expect_applied("gone after the receipt");
    assert_eq!(gone.agent, AgentPhase::Gone);
    assert!(!gone.generation_live);
    assert_eq!(gone.delivery, DeliveryPhase::Accepted);
    assert_eq!(gone.resource, ResourcePhase::Closing);
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
        AgentPhase::Running,
        DeliveryPhase::NoIntent,
        ResourcePhase::Attached,
        RecoveryPhase::NoRecovery,
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
