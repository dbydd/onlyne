//! Tests for the view reducer.
//!
//! The centrepiece is [`golden_fold`](tests::golden_fold): one pinned snapshot
//! and one scripted event sequence, asserted against the whole `View` the fold
//! produces. The reducer is pure — no server, no socket, no terminal — so the
//! fold is pinned exactly rather than sampled.

use super::*;

/// One instant, parsed the one way, so the fixture and the expectation agree.
fn at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .expect("a pinned instant")
        .to_utc()
}

fn principal(role: &str) -> Principal {
    Principal::role(role)
}

/// The pinned `status` answer, verbatim as `onlyne_server::router::status`
/// writes it.
fn status_json() -> Value {
    serde_json::json!({
        "ok": true,
        "cluster": "onlyne-dev",
        "version": "1.4.1",
        "spec_hash": "9f2c",
        "roles": 2,
        "role_count": 2,
        "gateway_count": 1,
        "gateways": [{"id": "gw1", "state": "offline"}],
        "routes": 3,
        "channels": 0,
        "connected_roles": 1,
        "connected_gateways": 0,
        "event_head": 42,
        "uptime_s": 900
    })
}

/// The pinned snapshot: the five admin reads, as their answers carry them.
fn snapshot() -> Snapshot {
    Snapshot {
        status: Some(status_json()),
        roles: serde_json::from_value(serde_json::json!([
            {
                "name": "planner",
                "admin": false,
                "max_sessions": 2,
                "runtime": {"drive": "plugin", "command": ["pi"]},
                "spec_hash": "9f2c",
                "prose": "plan the work",
                "state": "online",
                "sessions": 1,
                "queued": 0,
                "edges": ["builder"],
                "aggregate": null
            },
            {
                "name": "builder",
                "admin": false,
                "max_sessions": 1,
                "runtime": {"drive": "acp", "command": []},
                "spec_hash": "9f2c",
                "state": "offline",
                "sessions": 0,
                "queued": 1,
                "edges": [],
                "aggregate": "workers"
            }
        ]))
        .expect("the pinned roles"),
        sessions: serde_json::from_value(serde_json::json!([
            {
                "session_id": "s-planner-1",
                "task_id": "t1",
                "role": "planner",
                "generation": 1,
                "seq": 7,
                "public_lifecycle": "working",
                "projection": {
                    "lifecycle": "working",
                    "agent": "running",
                    "delivery": "pending",
                    "resource": "attached",
                    "recovery": "none"
                },
                "updated_at": "2026-09-28T10:00:00Z",
                "last_seen": "2026-09-28T10:00:05Z"
            },
            {
                "session_id": "s-builder-1",
                "role": "builder",
                "generation": 1,
                "seq": 3,
                "public_lifecycle": "idle",
                "projection": {
                    "lifecycle": "idle",
                    "agent": "idle",
                    "delivery": "none",
                    "resource": "closed",
                    "recovery": "idle_waiting"
                },
                "updated_at": "2026-09-28T09:59:00Z",
                "last_seen": "2026-09-28T09:59:30Z"
            }
        ]))
        .expect("the pinned sessions"),
        ledger: serde_json::from_value(serde_json::json!([
            {
                "msg_id": "m1",
                "op_id": "o-1",
                "kind": "task",
                "from": {"role": {"role": "_supervisor"}},
                "to": {"role": {"role": "planner"}},
                "task": "t1",
                "hop": 0,
                "family": "t1",
                "hop_budget": 8,
                "origin": "_supervisor",
                "attempt": 1,
                "state": "in_flight",
                "out_head": "plan the work",
                "enqueued_at": "2026-09-28T10:00:00Z"
            },
            {
                "msg_id": "m2",
                "op_id": "o-2",
                "kind": "task",
                "from": {"role": {"role": "planner"}},
                "to": {"role": {"role": "builder"}},
                "task": "t2",
                "parent_task": "t1",
                "hop": 1,
                "family": "t1",
                "hop_budget": 8,
                "origin": "_supervisor",
                "attempt": 1,
                "state": "queued",
                "enqueued_at": "2026-09-28T10:00:10Z"
            }
        ]))
        .expect("the pinned ledger"),
        faults: serde_json::from_value(serde_json::json!([
            {
                "id": 4,
                "task_id": "t2",
                "role": "builder",
                "kind": "intent_exhausted",
                "reason": "retries exhausted",
                "state": "open",
                "created_at": 1790000000
            }
        ]))
        .expect("the pinned faults"),
    }
}

/// The scripted stream: the events one connection sees after that snapshot.
///
/// Read in order: builder's client comes back, its released session takes the
/// queued delivery and starts working it, the delivery is handed over, a hook
/// fails, and planner settles its delivery with the verdict `done`.
fn script() -> Vec<Event> {
    let values = serde_json::json!([
        {
            "type": "role_presence",
            "data": {
                "role": "builder",
                "state": "online",
                "aggregate": "workers",
                "sessions": 1
            }
        },
        {
            "type": "session_state",
            "data": {
                "task_id": "t2",
                "role": "builder",
                "session_id": "s-builder-1",
                "generation": 1,
                "seq": 4,
                "projection": {
                    "lifecycle": "working",
                    "agent": "running",
                    "delivery": "none",
                    "resource": "attached",
                    "recovery": "none"
                }
            }
        },
        {
            "type": "ledger_state",
            "data": {
                "msg_id": "m2",
                "op_id": "o-2",
                "kind": "task",
                "from": {"role": {"role": "planner"}},
                "to": {"role": {"role": "builder"}},
                "task": "t2",
                "state": "in_flight"
            }
        },
        {
            "type": "fault",
            "data": {
                "id": 5,
                "task_id": "t2",
                "role": "builder",
                "kind": "hook_failed",
                "reason": "the hook exited 1",
                "state": "open",
                "created_at": 1790000100
            }
        },
        {
            "type": "session_state",
            "data": {
                "task_id": "t1",
                "role": "planner",
                "session_id": "s-planner-1",
                "generation": 1,
                "seq": 8,
                "projection": {
                    "lifecycle": "exited",
                    "agent": "gone",
                    "delivery": "accepted",
                    "resource": "closing",
                    "recovery": "none",
                    "outcome": "done"
                }
            }
        }
    ]);
    values
        .as_array()
        .expect("an array")
        .iter()
        .map(|value| serde_json::from_value::<Event>(value.clone()).expect("a pinned event"))
        .collect()
}

/// A session row as the fold holds it, for the expected views below.
fn session_at(
    lifecycle: Lifecycle,
    agent: AgentPhase,
    delivery: DeliveryPhase,
    resource: ResourcePhase,
    recovery: RecoveryPhase,
    outcome: Option<Outcome>,
    task_id: Option<&str>,
) -> SessionView {
    SessionView {
        session_id: String::new(),
        role: None,
        task_id: task_id.map(str::to_string),
        generation: 1,
        seq: 0,
        lifecycle,
        agent,
        delivery,
        resource,
        recovery,
        outcome,
        observed: None,
        admin: None,
        updated_at: None,
        last_seen: None,
        heartbeat_stale: false,
    }
}

#[test]
fn golden_fold() {
    let view = snapshot_to_view(&snapshot());
    let script = script();
    let view = script.iter().fold(view, update);

    // builder's release: the snapshot's Idle + Closed resource reads suspended,
    // and the `session_state` event is what moved it back to work.
    let builder = view.sessions.get("s-builder-1").expect("builder's session");
    assert_eq!(builder.session_id, "s-builder-1");
    assert_eq!(builder.role.as_deref(), Some("builder"));
    assert_eq!(builder.task_id.as_deref(), Some("t2"));
    assert_eq!(builder.lifecycle, Lifecycle::Working);
    assert_eq!(builder.agent, AgentPhase::Running);
    assert_eq!(builder.delivery, DeliveryPhase::NoIntent);
    assert_eq!(builder.resource, ResourcePhase::Attached);
    assert_eq!(builder.recovery, RecoveryPhase::NoRecovery);
    assert_eq!(builder.outcome, None);
    assert_eq!(builder.seq, 4, "the event's version is the row's watermark");

    // planner's delivery settled, and the settlement's own session is over.
    let planner = view.sessions.get("s-planner-1").expect("planner's session");
    assert_eq!(planner.lifecycle, Lifecycle::Exited);
    assert_eq!(planner.agent, AgentPhase::Gone);
    assert_eq!(planner.outcome, Some(Outcome::Done));
    assert_eq!(planner.task_id.as_deref(), Some("t1"));
    assert_eq!(planner.seq, 8);

    // The delivery axis moved on the delivery's own row.
    let m2 = view.deliveries.get("m2").expect("the handoff");
    assert_eq!(m2.state, LedgerState::InFlight);
    assert_eq!(m2.axis(), DeliveryAxis::InFlight);
    let m1 = view.deliveries.get("m1").expect("the first delivery");
    assert_eq!(m1.state, LedgerState::InFlight);
    assert_eq!(
        m1.family.as_deref(),
        Some("t1"),
        "causality kept from the read"
    );

    // The faults page gained the hook fault beside the open one.
    assert!(view.faults.contains_key(&4));
    assert!(view.faults.contains_key(&5));
    assert_eq!(view.open_faults().count(), 2);

    // The tail is everything the stream carried, newest first.
    let classes: Vec<&'static str> = view.event_tail.iter().map(Event::type_name).collect();
    assert_eq!(
        classes,
        vec![
            "session_state",
            "fault",
            "ledger_state",
            "session_state",
            "role_presence"
        ]
    );
    assert!(!view.stale, "no notice arrived, so nothing is stale");

    // The whole view, exactly. Every key below is a fact the fold produced:
    // nothing here is sampled or re-derived by the assertion.
    let mut expected = View {
        cluster: ClusterSummary {
            cluster: Some("onlyne-dev".into()),
            version: Some("1.4.1".into()),
            spec_hash: Some("9f2c".into()),
            role_count: Some(2),
            gateway_count: Some(1),
            routes: Some(3),
            channels: Some(0),
            connected_roles: Some(1),
            connected_gateways: Some(0),
            event_head: Some(42),
            uptime_s: Some(900),
        },
        roles: BTreeMap::new(),
        sessions: BTreeMap::new(),
        deliveries: BTreeMap::new(),
        faults: BTreeMap::new(),
        event_tail: Vec::new(),
        stale: false,
    };

    expected.roles.insert(
        "planner".into(),
        serde_json::from_value(serde_json::json!({
            "name": "planner",
            "admin": false,
            "max_sessions": 2,
            "runtime": {"drive": "plugin", "command": ["pi"]},
            "spec_hash": "9f2c",
            "prose": "plan the work",
            "state": "online",
            "sessions": 1,
            "queued": 0,
            "edges": ["builder"]
        }))
        .expect("planner's row"),
    );
    // The presence event is what moved builder: online, one live session, the
    // aggregate and detail the client reported.
    expected.roles.insert(
        "builder".into(),
        serde_json::from_value(serde_json::json!({
            "name": "builder",
            "admin": false,
            "max_sessions": 1,
            "runtime": {"drive": "acp", "command": []},
            "spec_hash": "9f2c",
            "state": "online",
            "sessions": 1,
            "queued": 1,
            "edges": [],
            "aggregate": "workers"
        }))
        .expect("builder's row"),
    );

    let mut planner_row = session_at(
        Lifecycle::Exited,
        AgentPhase::Gone,
        DeliveryPhase::Accepted,
        ResourcePhase::Closing,
        RecoveryPhase::NoRecovery,
        Some(Outcome::Done),
        Some("t1"),
    );
    planner_row.session_id = "s-planner-1".into();
    planner_row.role = Some("planner".into());
    planner_row.seq = 8;
    expected.sessions.insert("s-planner-1".into(), planner_row);

    let mut builder_row = session_at(
        Lifecycle::Working,
        AgentPhase::Running,
        DeliveryPhase::NoIntent,
        ResourcePhase::Attached,
        RecoveryPhase::NoRecovery,
        None,
        Some("t2"),
    );
    builder_row.session_id = "s-builder-1".into();
    builder_row.role = Some("builder".into());
    builder_row.seq = 4;
    expected.sessions.insert("s-builder-1".into(), builder_row);

    let m1_row = DeliveryView {
        msg_id: "m1".into(),
        op_id: Some("o-1".into()),
        kind: MsgKind::Task,
        from: principal("_supervisor"),
        to: principal("planner"),
        task_id: Some("t1".into()),
        family: Some("t1".into()),
        hop: Some(0),
        origin: Some("_supervisor".into()),
        attempt: Some(1),
        state: LedgerState::InFlight,
        outcome: None,
        reason: None,
        out_head: Some("plan the work".into()),
        enqueued_at: Some(at("2026-09-28T10:00:00Z")),
        acked_at: None,
    };
    expected.deliveries.insert("m1".into(), m1_row);

    // The row the stream moved: its event carries no causality and no clock, so
    // the fields the `ledger` read owns — `family`, `hop`, `origin`, `attempt`,
    // `enqueued_at` — keep the read's values while the state takes the event's.
    let m2_row = DeliveryView {
        msg_id: "m2".into(),
        op_id: Some("o-2".into()),
        kind: MsgKind::Task,
        from: principal("planner"),
        to: principal("builder"),
        task_id: Some("t2".into()),
        family: Some("t1".into()),
        hop: Some(1),
        origin: Some("_supervisor".into()),
        attempt: Some(1),
        state: LedgerState::InFlight,
        outcome: None,
        reason: None,
        out_head: None,
        enqueued_at: Some(at("2026-09-28T10:00:10Z")),
        acked_at: None,
    };
    expected.deliveries.insert("m2".into(), m2_row);

    for id in [4, 5] {
        let fault: FaultEvent = view.faults.get(&id).expect("a fault").clone();
        expected.faults.insert(id, fault);
    }
    // The tail is the script reversed, and it is stated from the script rather
    // than copied off the fold: a dropped, duplicated or reordered event fails
    // in the whole-view comparison below, and the classes asserted above say
    // which event is which while the diff stays readable.
    expected.event_tail = script.iter().rev().cloned().collect();

    assert_eq!(view, expected);
}

#[test]
fn a_blocked_delivery_waits_on_a_suspended_session() {
    // The two axes, and the case the slice exists for: a delivery settles while
    // the session that served it is suspended. Both facts stay visible, and
    // neither is resolved away.
    let mut snapshot = snapshot();
    // planner's row: suspended (idle, process released), its delivery acked
    // with a `blocked` verdict — the ending rule's own settlement.
    snapshot.sessions[0].public_lifecycle = Lifecycle::Idle;
    snapshot.sessions[0].projection.lifecycle = Lifecycle::Idle;
    snapshot.sessions[0].projection.agent = AgentPhase::Idle;
    snapshot.sessions[0].projection.resource = ResourcePhase::Closed;
    snapshot.sessions[0].projection.outcome = Some(Outcome::Blocked);
    snapshot.ledger[0].state = LedgerState::Acked;
    snapshot.ledger[0].acked_at = Some(at("2026-09-28T10:01:00Z"));

    // Settle the delivery on the stream too, so the fold and not only the read
    // is what put it there.
    let settled: Event = serde_json::from_value(serde_json::json!({
        "type": "ledger_state",
        "data": {
            "msg_id": "m1",
            "op_id": "o-1",
            "kind": "task",
            "from": {"role": {"role": "_supervisor"}},
            "to": {"role": {"role": "planner"}},
            "task": "t1",
            "state": "acked"
        }
    }))
    .expect("a pinned settlement");
    let view = update(snapshot_to_view(&snapshot), &settled);

    // Axis one: the delivery is settled.
    let delivery = view.deliveries.get("m1").expect("the delivery");
    assert_eq!(delivery.state, LedgerState::Acked);
    assert_eq!(delivery.axis(), DeliveryAxis::Settled);
    // Axis two: its session is suspended, and its verdict is the waiting one.
    let session = view.session_for("t1").expect("the serving session");
    assert_eq!(session.state(), SessionState::Suspended);
    assert!(session.is_blocked());

    // A reducer that merged the axes would have to pick one of the two to keep.
    // Both are asserted above, and the board reading shows the pair without
    // collapsing it: the card waits, and its column says failed-or-blocked.
    let card = view.cards("planner").next().expect("planner's card");
    assert_eq!(card.column(), BoardColumn::FailedOrBlocked);
    assert!(card.waits());
    assert_eq!(
        card.session.expect("the card's session").session_id,
        "s-planner-1"
    );
    assert_eq!(
        view.counts("planner"),
        SessionCounts {
            busy: 0,
            idle: 0,
            suspended: 1
        }
    );
}

#[test]
fn the_resync_notice_marks_stale_and_the_next_snapshot_clears_it() {
    let notice: Event = serde_json::from_value(serde_json::json!({
        "type": "fault",
        "data": {
            "id": 0,
            "kind": "resync_lag",
            "reason": "5 events fell out of the server's broadcast",
            "seq": 5
        }
    }))
    .expect("the lag notice");
    assert!(is_resync_lag(&notice));

    // The same script, with and without the notice. The flag means what it
    // says: it is the gap, not the traffic.
    let with: View = script()
        .iter()
        .fold(update(snapshot_to_view(&snapshot()), &notice), update);
    let without: View = script().iter().fold(snapshot_to_view(&snapshot()), update);

    assert!(with.stale, "the notice marks the view stale");
    assert!(!without.stale, "the same script without it never does");

    // A gap is not news: the notice is not in the tail, and it wrote no fault
    // row over the real one that shares its default id 0.
    assert_eq!(with.event_tail.len(), without.event_tail.len());
    assert!(
        !with.faults.contains_key(&0),
        "the notice is not a fault row"
    );

    // The front end re-reads the snapshot, and that is what clears the flag.
    let rebuilt = snapshot_to_view(&snapshot());
    assert!(!rebuilt.stale);
    assert!(rebuilt.event_tail.is_empty(), "the tail is the stream's");
}

#[test]
fn an_unknown_class_leaves_the_view_unchanged() {
    let view = snapshot_to_view(&snapshot());
    let folded = script().iter().fold(view.clone(), update);

    // A class a newer server could publish: this build cannot decode it, so it
    // is folded as nothing rather than rendered as news.
    let after = update_class(
        folded.clone(),
        "quarantine_released",
        serde_json::json!({"role": "builder"}),
    );
    assert_eq!(after, folded);

    // A class this build does know still folds.
    let known = update_class(
        folded.clone(),
        "role_presence",
        serde_json::json!({"role": "builder", "state": "draining", "sessions": 0}),
    );
    assert_ne!(known, folded);
    assert_eq!(
        known.roles.get("builder").expect("builder").state,
        crate::event::Presence::Draining
    );
}

#[test]
fn the_turn_end_family_is_news_without_a_merged_row() {
    // The three client-owned classes are on disk, so the fold knows them. Their
    // content is two rows' business — the delivery's own `acked` on
    // `ledger_state`, the verdict on the session row's outcome — so folding
    // them here adds news and no merged state.
    let mut snapshot = snapshot();
    snapshot.ledger[0].state = LedgerState::Acked;
    snapshot.ledger[0].acked_at = Some(at("2026-09-28T10:01:00Z"));
    snapshot.sessions[0].projection.outcome = Some(Outcome::Blocked);
    snapshot.sessions[0].public_lifecycle = Lifecycle::Idle;
    snapshot.sessions[0].projection.lifecycle = Lifecycle::Idle;
    snapshot.sessions[0].projection.resource = ResourcePhase::Closed;
    let view = snapshot_to_view(&snapshot);

    let blocked: Event = serde_json::from_value(serde_json::json!({
        "type": "delivery_blocked",
        "data": {"task_id": "t1", "session_id": "s-planner-1", "role": "planner"}
    }))
    .expect("the settlement");
    let ending: Event = serde_json::from_value(serde_json::json!({
        "type": "turn_end_without_complete",
        "data": {"task_id": "t1", "session_id": "s-planner-1", "role": "planner", "nudge": true}
    }))
    .expect("the ending");
    let handoff: Event = serde_json::from_value(serde_json::json!({
        "type": "handoff",
        "data": {
            "task_id": "t2", "session_id": "s-planner-1", "role": "planner",
            "to_role": "builder", "hop": 1, "text": "build it"
        }
    }))
    .expect("the handoff");

    let after = [&blocked, &ending, &handoff]
        .iter()
        .fold(view, |view, event| update(view, event));

    // News in the tail, newest first, all three classes: the family is the
    // client's own account of work it settled, so it is news like any other.
    let classes: Vec<&'static str> = after.event_tail.iter().map(Event::type_name).collect();
    assert_eq!(
        classes,
        vec!["handoff", "turn_end_without_complete", "delivery_blocked"]
    );

    // And no merged state: the delivery row keeps only the delivery axis's
    // words, so a reader cannot find a session fact on it.
    let delivery = after.deliveries.get("m1").expect("the delivery");
    assert_eq!(delivery.state, LedgerState::Acked);
    assert_eq!(delivery.axis(), DeliveryAxis::Settled);
    assert_eq!(delivery.outcome, None, "a delivery row carries no verdict");

    // The waiting reads off the session axis, which owns the verdict.
    let session = after.session_for("t1").expect("the serving session");
    assert!(session.is_blocked());
    assert_eq!(session.state(), SessionState::Suspended);
    assert!(after.cards("planner").next().expect("a card").waits());
}

#[test]
fn a_replayed_row_is_neither_state_nor_news() {
    let view = snapshot_to_view(&snapshot());
    let replayed: Event = serde_json::from_value(serde_json::json!({
        "type": "session_state",
        "data": {
            "task_id": "t1",
            "role": "planner",
            "session_id": "s-planner-1",
            "generation": 1,
            "seq": 7,
            "projection": {
                "lifecycle": "idle",
                "agent": "idle",
                "delivery": "none",
                "resource": "detached",
                "recovery": "none"
            }
        }
    }))
    .expect("a pinned replay");
    let after = update(view.clone(), &replayed);
    assert_eq!(after, view, "the row is at the version this view holds");
}

#[test]
fn the_three_screens_read_off_the_one_view() {
    let view = snapshot_to_view(&snapshot());

    // Cluster: the roles read, their presence, and the header's counts.
    assert_eq!(view.roles.len(), 2);
    assert_eq!(view.cluster.event_head, Some(42));
    assert_eq!(
        view.counts("planner"),
        SessionCounts {
            busy: 1,
            idle: 0,
            suspended: 0
        }
    );
    assert_eq!(
        view.counts("builder"),
        SessionCounts {
            busy: 0,
            idle: 0,
            suspended: 1
        },
        "idle with a released process is a suspended session"
    );

    // Task: one family's path, and the session serving each delivery.
    let family: Vec<&str> = view.family("t1").map(|d| d.msg_id.as_str()).collect();
    assert_eq!(family, vec!["m1", "m2"]);
    assert_eq!(
        view.session_for("t1")
            .expect("the serving session")
            .session_id,
        "s-planner-1"
    );
    assert!(view.session_for("t-none").is_none());

    // Faults: the open rows and nothing else.
    let open: Vec<i64> = view.open_faults().map(|fault| fault.id).collect();
    assert_eq!(open, vec![4]);

    // The cards of one board, indexed by role.
    let planner: Vec<&str> = view
        .cards("planner")
        .map(|card| card.delivery.msg_id.as_str())
        .collect();
    assert_eq!(planner, vec!["m1"]);
    assert_eq!(view.cards("builder").count(), 1);
    assert_eq!(view.cards("reviewer").count(), 0);
    assert!(!view.cards("builder").next().expect("a card").waits());
}

#[test]
fn a_closed_fault_is_not_open() {
    let moved: FaultEvent = serde_json::from_value(serde_json::json!({
        "id": 4,
        "task_id": "t2",
        "role": "builder",
        "kind": "intent_exhausted",
        "reason": "retries exhausted",
        "state": "acked"
    }))
    .expect("the repaired fault");
    assert!(!fault_is_open(&moved));
    assert!(fault_is_open(&FaultEvent {
        id: 9,
        kind: "idle_fault".into(),
        reason: "no heartbeat".into(),
        ..FaultEvent::default()
    }));

    let view = update(snapshot_to_view(&snapshot()), &Event::Fault(moved));
    let open: Vec<i64> = view.open_faults().map(|fault| fault.id).collect();
    assert!(open.is_empty(), "the one fault was closed");
}

#[test]
fn the_event_tail_is_capped() {
    let mut view = snapshot_to_view(&snapshot());
    for id in 100..(100 + EVENT_TAIL_LIMIT as i64 + 20) {
        let fault = Event::Fault(FaultEvent {
            id,
            kind: "idle_fault".into(),
            reason: "no heartbeat".into(),
            ..FaultEvent::default()
        });
        view = update(view, &fault);
    }
    assert_eq!(view.event_tail.len(), EVENT_TAIL_LIMIT);
    assert_eq!(
        view.event_tail.first().expect("newest").type_name(),
        "fault"
    );
    assert_eq!(view.faults.len(), 1 + EVENT_TAIL_LIMIT + 20);
}

#[test]
fn a_role_the_snapshot_does_not_carry_is_not_invented() {
    let view = snapshot_to_view(&snapshot());
    let unknown: Event = serde_json::from_value(serde_json::json!({
        "type": "role_presence",
        "data": {"role": "reviewer", "state": "online", "sessions": 3}
    }))
    .expect("a presence event");
    let after = update(view, &unknown);
    assert_eq!(after.roles.len(), 2, "the registry stays the read's");
    assert_eq!(
        after.event_tail.first().expect("newest").type_name(),
        "role_presence",
        "and the event is still news"
    );
}

#[test]
fn the_summary_reads_the_status_answer_key_by_key() {
    let summary = ClusterSummary::from_status(&status_json());
    assert_eq!(
        summary,
        ClusterSummary {
            cluster: Some("onlyne-dev".into()),
            version: Some("1.4.1".into()),
            spec_hash: Some("9f2c".into()),
            role_count: Some(2),
            gateway_count: Some(1),
            routes: Some(3),
            channels: Some(0),
            connected_roles: Some(1),
            connected_gateways: Some(0),
            event_head: Some(42),
            uptime_s: Some(900),
        }
    );
    // A key this build does not know reads as absent, not as a plausible zero.
    assert_eq!(
        ClusterSummary::from_status(&serde_json::json!({"cluster": "x"})),
        ClusterSummary {
            cluster: Some("x".into()),
            ..ClusterSummary::default()
        }
    );
}

#[test]
fn the_session_axis_table() {
    // The four the contract names, plus the machine's opening state, each on the
    // row the tuple can prove it from.
    let cases = [
        (
            Lifecycle::Idle,
            AgentPhase::Idle,
            ResourcePhase::Closed,
            SessionState::Suspended,
        ),
        (
            Lifecycle::Idle,
            AgentPhase::Idle,
            ResourcePhase::Attached,
            SessionState::Idle,
        ),
        (
            Lifecycle::Working,
            AgentPhase::Running,
            ResourcePhase::Attached,
            SessionState::Busy,
        ),
        (
            Lifecycle::Exited,
            AgentPhase::Gone,
            ResourcePhase::Closed,
            SessionState::Closed,
        ),
        (
            Lifecycle::Created,
            AgentPhase::Booting,
            ResourcePhase::Detached,
            SessionState::Opening,
        ),
    ];
    for (lifecycle, agent, resource, expected) in cases {
        let mut session = session_at(
            lifecycle,
            agent,
            DeliveryPhase::NoIntent,
            resource,
            RecoveryPhase::NoRecovery,
            None,
            None,
        );
        session.session_id = "s".into();
        assert_eq!(
            session.state(),
            expected,
            "{lifecycle:?}/{agent:?}/{resource:?}"
        );
    }
}

#[test]
fn the_delivery_axis_table() {
    let cases = [
        (LedgerState::Queued, DeliveryAxis::Queued),
        (LedgerState::InFlight, DeliveryAxis::InFlight),
        (LedgerState::Acked, DeliveryAxis::Settled),
        (LedgerState::Rejected, DeliveryAxis::Settled),
        (LedgerState::Expired, DeliveryAxis::Settled),
    ];
    for (state, expected) in cases {
        let delivery = DeliveryView {
            msg_id: "m".into(),
            op_id: None,
            kind: MsgKind::Note,
            from: principal("a"),
            to: principal("b"),
            task_id: None,
            family: None,
            hop: None,
            origin: None,
            attempt: None,
            state,
            outcome: None,
            reason: None,
            out_head: None,
            enqueued_at: None,
            acked_at: None,
        };
        assert_eq!(delivery.axis(), expected, "{state:?}");
    }
}

/// The slice's own rule, proven by the crate's dependency table rather than by
/// a reading of its source: `onlyne-proto` carries the vocabulary, the ops and
/// the two pure folds, and takes no async runtime to hold them.
///
/// The folds are the reason. A reducer that can be driven by an executor is a
/// reducer that can be slow, cancelled or interleaved, and both [`update`] and
/// [`snapshot_to_view`] promise a total, single-threaded answer instead.
#[test]
fn the_crate_carries_no_async_runtime() {
    /// The runtime one manifest line names, if any.
    fn runtime_named(line: &str) -> Option<String> {
        line.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            .filter(|word| !word.is_empty())
            .map(str::to_ascii_lowercase)
            .find(|word| {
                word.starts_with("tokio")
                    || matches!(
                        word.as_str(),
                        "async-std" | "smol" | "async-global-executor" | "futures-executor"
                    )
            })
    }

    let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .expect("the crate's own manifest");
    assert!(
        manifest.contains("name = \"onlyne-proto\""),
        "the file read has to be this crate's manifest"
    );
    // The scan is checked before it is trusted, so a green run means a clean
    // manifest rather than a scanner that matches nothing.
    assert_eq!(
        runtime_named("tokio = { workspace = true }").as_deref(),
        Some("tokio")
    );
    assert_eq!(
        runtime_named("[dependencies.tokio]").as_deref(),
        Some("tokio")
    );
    assert_eq!(runtime_named("serde = { workspace = true }"), None);
    assert_eq!(runtime_named("[dev-dependencies]"), None);

    let mut dependency_table = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            // `[dependencies]`, `[dev-dependencies]`, `[build-dependencies]`,
            // and the one-crate tables such as `[dependencies.tokio]`.
            dependency_table = line.contains("dependencies");
            if dependency_table {
                assert!(
                    runtime_named(line).is_none(),
                    "onlyne-proto must keep no async runtime: {line}"
                );
            }
            continue;
        }
        if !dependency_table || line.is_empty() || line.starts_with('#') {
            continue;
        }
        assert!(
            runtime_named(line).is_none(),
            "onlyne-proto must keep no async runtime: {line}"
        );
    }
}
