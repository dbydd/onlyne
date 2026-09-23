use super::*;
use crate::backend::fake::FakeBackend;
use crate::backend::{Capabilities, CloseReason, SessionBackend, SessionRef, SpawnSpec};
use crate::lifecycle::{
    self, AgentState, IgnoredReason, LifecycleEvent, Observation, PublicLifecycle, TaskState,
    Verdict, Version, project,
};
use std::collections::BTreeMap;

/// The public view of one stored row, with the task state a caller holds
/// alongside it. The row keeps no projection of its own: `project` is what
/// turns these columns into `created` / `idle` / `working` / `exited`.
fn projection(row: &SessionRecord, task_state: TaskState) -> PublicLifecycle {
    let obs: Observation =
        serde_json::from_str(&row.observed_json).expect("a stored session tuple");
    project(
        obs.agent,
        obs.delivery,
        obs.resource,
        obs.recovery,
        task_state,
    )
}

fn tracked(ledger: &MemoryLedger, task: &str) -> (Bridge, Version) {
    ledger.track_task(task, 1);
    let bridge = Bridge::new();
    let verdict = feed_created(&bridge, ledger, task).unwrap();
    assert!(matches!(verdict, Verdict::Applied(_)), "{verdict:?}");
    let row = ledger.get_session(task).unwrap().unwrap();
    let obs: Observation = serde_json::from_str(&row.observed_json).unwrap();
    (bridge, obs.version)
}

#[test]
fn created_ready_working_chain_applies_and_persists() {
    let ledger = MemoryLedger::new();
    let (bridge, _) = tracked(&ledger, "chain-1");
    let first = ledger.get_session("chain-1").unwrap().unwrap();
    assert_eq!(
        projection(&first, TaskState::Pending),
        PublicLifecycle::Created
    );
    assert_eq!((first.generation, first.seq), (1, 1));
    assert_eq!(first.backend_ref, "{}");

    bridge.track_live(SessionRef {
        task_id: "chain-1".into(),
        backend: "fake".into(),
        backend_ref: serde_json::json!({"handle": "term-chain"}),
        generation: 1,
    });
    feed_ready(&bridge, &ledger, "chain-1").unwrap();
    let second = ledger.get_session("chain-1").unwrap().unwrap();
    assert_eq!(
        projection(&second, TaskState::Pending),
        PublicLifecycle::Idle
    );
    assert_eq!(second.seq, 2);
    assert!(
        second.backend_ref.contains("term-chain"),
        "{}",
        second.backend_ref
    );
    feed_resource_attached(&bridge, &ledger, "chain-1").unwrap();
    feed_turn_started(&bridge, &ledger, "chain-1").unwrap();
    let third = ledger.get_session("chain-1").unwrap().unwrap();
    assert_eq!(third.agent_state, "running");
    assert_eq!(third.resource_state, "attached");
    assert_eq!(
        projection(&third, TaskState::Pending),
        PublicLifecycle::Working
    );
    assert_eq!(third.seq, 4);
    let obs: Observation = serde_json::from_str(&third.observed_json).unwrap();
    assert!(lifecycle::is_legal(&obs));
    assert!(
        third.desired_json.contains("turn_started"),
        "{}",
        third.desired_json
    );

    feed_turn_ended(&bridge, &ledger, "chain-1").unwrap();
    let events = ledger.events();
    let last = events.iter().rev().find(|(k, _)| k == "lifecycle").unwrap();
    assert_eq!(last.1["task_id"], "chain-1");
    // The bus carries the tuple the reducer settled on, not a public view: the
    // projection needs the task state, and the session row does not own one.
    assert_eq!(last.1["agent"], "idle");
    assert_eq!(last.1["recovery"], "none");
    assert_eq!(last.1["event"], "turn_ended");
    assert!(last.1.get("public").is_none(), "{}", last.1);
    assert!(last.1.get("outcome").is_none(), "{}", last.1);
}

#[test]
fn duplicate_and_stale_events_never_write() {
    let ledger = MemoryLedger::new();
    let (bridge, _) = tracked(&ledger, "dup-1");
    assert!(matches!(
        feed_created(&bridge, &ledger, "dup-1").unwrap(),
        Verdict::Ignored(IgnoredReason::NoOp)
    ));
    let before = ledger.get_session("dup-1").unwrap().unwrap();
    assert_eq!(before.seq, 1);
    let verdict = apply_persist(
        &bridge,
        &ledger,
        "dup-1",
        &LifecycleEvent::Ready {
            v: Version::new(1, 1),
        },
    )
    .unwrap();
    assert!(matches!(
        verdict,
        Verdict::Ignored(lifecycle::IgnoredReason::StaleOrDuplicateSeq)
    ));
    let after = ledger.get_session("dup-1").unwrap().unwrap();
    assert_eq!(after.observed_json, before.observed_json);
    assert_eq!(after.updated_at, before.updated_at);
    let stale = VersionedSession {
        seq: 0,
        generation: 0,
        ..to_versioned(
            &serde_json::from_str(&after.observed_json).unwrap(),
            &after.backend_ref,
            "{}",
        )
        .unwrap()
    };
    assert!(!ledger.upsert_session("dup-1", &stale).unwrap());
    assert_eq!(
        ledger.get_session("dup-1").unwrap().unwrap().observed_json,
        before.observed_json
    );
}
#[test]
fn older_seq_after_newer_seq_is_ignored_without_writing() {
    let ledger = MemoryLedger::new();
    let (bridge, _) = tracked(&ledger, "mono-1");
    feed_ready(&bridge, &ledger, "mono-1").unwrap();
    feed_resource_attached(&bridge, &ledger, "mono-1").unwrap();
    feed_turn_started(&bridge, &ledger, "mono-1").unwrap();
    feed_turn_ended(&bridge, &ledger, "mono-1").unwrap();
    let before = ledger.get_session("mono-1").unwrap().unwrap();
    assert_eq!((before.generation, before.seq), (1, 5));
    let event_count = ledger.events().len();
    let verdict = apply_persist(
        &bridge,
        &ledger,
        "mono-1",
        &LifecycleEvent::TurnStarted {
            v: Version::new(1, 4),
        },
    )
    .unwrap();
    assert!(matches!(
        verdict,
        Verdict::Ignored(lifecycle::IgnoredReason::StaleOrDuplicateSeq)
    ));
    let after = ledger.get_session("mono-1").unwrap().unwrap();
    assert_eq!(after.observed_json, before.observed_json);
    assert_eq!((after.generation, after.seq), (1, 5));
    assert_eq!(ledger.events().len(), event_count);
}

fn pane_host(pane_key: &str) -> crate::host::HostRef {
    crate::host::HostRef {
        orca: Some(crate::host::OrcaPane {
            pane_key: pane_key.to_string(),
            tab_id: None,
            leaf_id: None,
            handle: Some("term_1".to_string()),
        }),
    }
}

#[test]
fn a_heartbeats_reported_host_is_persisted_and_survives_a_settle() {
    let ledger = MemoryLedger::new();
    let (bridge, _) = tracked(&ledger, "host-1");
    // A settlement is the arrival of a receipt, and a process that never passed
    // Ready has delivered nothing, so the session is driven that far first.
    feed_ready(&bridge, &ledger, "host-1").unwrap();
    let row = ledger.get_session("host-1").unwrap().unwrap();
    let ready: Observation = serde_json::from_str(&row.observed_json).unwrap();
    let body = ready.clone().with_host(Some(pane_host("tab-1:leaf-1")));
    let verdict = apply_persist(
        &bridge,
        &ledger,
        "host-1",
        &LifecycleEvent::Heartbeat {
            v: Version::new(ready.version.generation, ready.version.seq + 1),
            body,
        },
    )
    .unwrap();
    assert!(matches!(verdict, Verdict::Applied(_)), "{verdict:?}");

    // The row is what a client republishes and what the server mirrors, so
    // the binding has to be readable back out of it.
    let bound = ledger.get_session("host-1").unwrap().unwrap();
    assert!(
        bound.observed_json.contains(r#""pane_key":"tab-1:leaf-1""#),
        "{}",
        bound.observed_json
    );
    assert_eq!(
        stored_observation(&ledger, Some(&bound)).host,
        Some(pane_host("tab-1:leaf-1"))
    );

    // A completed turn still runs in the pane it was reported from.
    let verdict = settle(&bridge, &ledger, "host-1").unwrap();
    assert!(matches!(verdict, Verdict::Applied(_)), "{verdict:?}");
    let settled = ledger.get_session("host-1").unwrap().unwrap();
    assert_eq!(settled.delivery_state, "accepted");
    assert_eq!(
        stored_observation(&ledger, Some(&settled)).host,
        Some(pane_host("tab-1:leaf-1"))
    );
}

#[test]
fn unadopted_generation_is_rejected_without_writing() {
    let ledger = MemoryLedger::new();
    ledger.track_task("gen-1", 1);
    let bridge = Bridge::new();
    bridge.track_live(SessionRef {
        task_id: "gen-1".into(),
        backend: "fake".into(),
        backend_ref: serde_json::json!({"id": "gen-1"}),
        generation: 1,
    });
    feed_created(&bridge, &ledger, "gen-1").unwrap();
    feed_ready(&bridge, &ledger, "gen-1").unwrap();
    feed_turn_started(&bridge, &ledger, "gen-1").unwrap();
    let before = ledger.get_session("gen-1").unwrap().unwrap();
    let verdict = apply_persist(
        &bridge,
        &ledger,
        "gen-1",
        &LifecycleEvent::Ready {
            v: Version::new(2, 1),
        },
    )
    .unwrap();
    assert!(matches!(
        verdict,
        Verdict::Rejected(lifecycle::RejectReason::UnadoptedGeneration)
    ));
    let after = ledger.get_session("gen-1").unwrap().unwrap();
    assert_eq!(after.generation, before.generation);
    assert_eq!(after.seq, before.seq);
    assert_eq!(after.observed_json, before.observed_json);
}

#[test]
fn corrupt_row_keeps_its_watermark_and_reports_itself() {
    let ledger = MemoryLedger::new();
    ledger.track_task("cr-1", 1);
    let bridge = Bridge::new();
    let mut broken = to_versioned(&Observation::initial(1, 3), "{}", "{}").unwrap();
    broken.generation = 1;
    broken.seq = 5;
    broken.observed_json = "{ not json".into();
    assert!(ledger.upsert_session("cr-1", &broken).unwrap());
    let verdict = feed_turn_ended(&bridge, &ledger, "cr-1").unwrap();
    assert!(matches!(verdict, Verdict::Applied(_)), "{verdict:?}");
    let after = ledger.get_session("cr-1").unwrap().unwrap();
    assert_eq!(after.generation, 1);
    assert_eq!(after.seq, 6);
    assert!(
        ledger
            .alerts()
            .iter()
            .any(|line| line.contains("cr-1") && line.contains("corrupt"))
    );
    assert!(
        ledger
            .events()
            .iter()
            .any(|(k, _)| k == "lifecycle_corrupt")
    );
}

#[test]
fn probe_dead_fails_the_session_and_records_one_fault() {
    let ledger = MemoryLedger::new();
    ledger.track_task("dead-1", 1);
    let bridge = Bridge::new();
    let backend = FakeBackend::new();
    let session = backend
        .spawn(SpawnSpec {
            cwd: ".".into(),
            task_id: "dead-1".into(),
            command: vec!["agent".into()],
            env: BTreeMap::new(),
            focus: None,
            placement: None,
            rename: None,
        })
        .unwrap();
    bridge.track_live(session.clone());
    feed_created(&bridge, &ledger, "dead-1").unwrap();
    feed_resource_attached(&bridge, &ledger, "dead-1").unwrap();
    feed_ready(&bridge, &ledger, "dead-1").unwrap();
    feed_turn_started(&bridge, &ledger, "dead-1").unwrap();
    backend.close(&session, CloseReason::Fault, true).unwrap();
    bridge.untrack_live("dead-1");
    // The stored row keeps the live resource reference, so the probe
    // resolves its target from the live map.
    let row = ledger.get_session("dead-1").unwrap().unwrap();
    assert!(row.backend_ref.contains("dead-1"));
    bridge.track_live(session.clone());
    let verdict = reconcile_probe(&bridge, &ledger, &backend, "dead-1").unwrap();
    assert_eq!(verdict, ProbeVerdict::DeadFaulted);
    let session_row = ledger.get_session("dead-1").unwrap().unwrap();
    assert_eq!(session_row.agent_state, "gone");
    // A gone agent is an exit whatever the task it served ended as.
    assert_eq!(
        projection(&session_row, TaskState::Pending),
        PublicLifecycle::Exited
    );
    let faults = ledger.list_faults("dead-1").unwrap();
    assert_eq!(faults.len(), 1, "{faults:?}");
    assert_eq!(faults[0].kind, "probe_dead");
    assert_eq!(faults[0].state, "open");
    // A repeated pass dedupes on (task_id, kind, generation).
    bridge.track_live(session.clone());
    let _ = reconcile_probe(&bridge, &ledger, &backend, "dead-1").unwrap();
    assert_eq!(ledger.list_faults("dead-1").unwrap().len(), 1);
}
#[test]
fn forced_probe_failure_drives_the_dead_branch() {
    let ledger = MemoryLedger::new();
    ledger.track_task("forced-1", 1);
    let bridge = Bridge::new();
    let backend = FakeBackend::new();
    let session = backend
        .spawn(SpawnSpec {
            cwd: ".".into(),
            task_id: "forced-1".into(),
            command: vec!["agent".into()],
            env: BTreeMap::new(),
            focus: None,
            placement: None,
            rename: None,
        })
        .unwrap();
    bridge.track_live(session.clone());
    feed_created(&bridge, &ledger, "forced-1").unwrap();
    feed_resource_attached(&bridge, &ledger, "forced-1").unwrap();
    feed_ready(&bridge, &ledger, "forced-1").unwrap();
    feed_turn_started(&bridge, &ledger, "forced-1").unwrap();
    backend.fail_probe("forced-1");
    let probe = backend.probe(&session).unwrap();
    assert!(!probe.alive);
    assert!(!probe.attached);
    let verdict = reconcile_probe(&bridge, &ledger, &backend, "forced-1").unwrap();
    assert_eq!(verdict, ProbeVerdict::DeadFaulted);
    let faults = ledger.list_faults("forced-1").unwrap();
    assert_eq!(faults.len(), 1, "{faults:?}");
    assert_eq!(faults[0].kind, "probe_dead");
    backend.clear_probe_failure("forced-1");
    assert!(backend.probe(&session).unwrap().alive);
}

#[test]
fn inconclusive_probe_never_judges_a_generation_dead() {
    let ledger = MemoryLedger::new();
    ledger.track_task("unk-1", 1);
    let bridge = Bridge::new();
    let seed = to_versioned(&Observation::initial(1, 3), "{}", "{}").unwrap();
    assert!(ledger.upsert_session("unk-1", &seed).unwrap());
    let backend = FakeBackend::new();
    let verdict = reconcile_probe(&bridge, &ledger, &backend, "unk-1").unwrap();
    assert_eq!(verdict, ProbeVerdict::Unknown);
    assert!(ledger.list_faults("unk-1").unwrap().is_empty());
}

#[test]
fn a_delivered_hop_closes_the_drain_before_the_resource_closes() {
    let ledger = MemoryLedger::new();
    ledger.track_task("out-1", 1);
    let bridge = Bridge::new();
    bridge.track_live(SessionRef {
        task_id: "out-1".into(),
        backend: "fake".into(),
        backend_ref: serde_json::json!({"id": "out-1"}),
        generation: 1,
    });
    feed_created(&bridge, &ledger, "out-1").unwrap();
    feed_resource_attached(&bridge, &ledger, "out-1").unwrap();
    feed_ready(&bridge, &ledger, "out-1").unwrap();
    feed_turn_started(&bridge, &ledger, "out-1").unwrap();
    let verdict = feed_delivered(&bridge, &ledger, "out-1").unwrap();
    assert!(matches!(verdict, Verdict::Applied(_)), "{verdict:?}");
    let session = ledger.get_session("out-1").unwrap().unwrap();
    assert_eq!(session.delivery_state, "accepted");
    // The row says the receipt landed and nothing about how the task ended:
    // the exit is the pair of those facts, read together by whoever holds the
    // task.
    assert_eq!(
        projection(&session, TaskState::Done),
        PublicLifecycle::Exited
    );
    assert_eq!(
        projection(&session, TaskState::Pending),
        PublicLifecycle::Working
    );
    let verdict = feed_resource_closed(&bridge, &ledger, "out-1").unwrap();
    assert!(matches!(verdict, Verdict::Applied(_)), "{verdict:?}");
    let session = ledger.get_session("out-1").unwrap().unwrap();
    assert_eq!(session.resource_state, "closed");
    assert_eq!(session.agent_state, "gone");
}

#[test]
fn fake_backend_spawn_probe_close_drives_the_reducer() {
    let ledger = MemoryLedger::new();
    ledger.track_task("fake-1", 1);
    let bridge = Bridge::new();
    let backend = FakeBackend::new();
    assert!(
        backend.capabilities()
            == Capabilities {
                spawn: true,
                attach: true,
                probe: true,
                close: true,
                focus: true,
                rename: true,
            }
    );
    let session = backend
        .spawn(SpawnSpec {
            cwd: ".".into(),
            task_id: "fake-1".into(),
            command: vec!["agent".into()],
            env: BTreeMap::new(),
            focus: None,
            placement: None,
            rename: None,
        })
        .unwrap();
    bridge.track_live(session.clone());
    let public = |task: &str, task_state| -> PublicLifecycle {
        let row = ledger.get_session(task).unwrap().unwrap();
        projection(&row, task_state)
    };
    feed_created(&bridge, &ledger, "fake-1").unwrap();
    assert_eq!(
        public("fake-1", TaskState::Pending),
        PublicLifecycle::Created
    );
    let probe = backend.probe(&session).unwrap();
    assert!(probe.alive);
    feed_resource_attached(&bridge, &ledger, "fake-1").unwrap();
    feed_ready(&bridge, &ledger, "fake-1").unwrap();
    assert_eq!(public("fake-1", TaskState::Pending), PublicLifecycle::Idle);
    feed_turn_started(&bridge, &ledger, "fake-1").unwrap();
    assert_eq!(
        public("fake-1", TaskState::Pending),
        PublicLifecycle::Working
    );
    feed_delivered(&bridge, &ledger, "fake-1").unwrap();
    // Delivered, and the task ledger agrees it is over: that pair is the exit,
    // not the row alone.
    assert_eq!(public("fake-1", TaskState::Done), PublicLifecycle::Exited);
    backend
        .close(&session, CloseReason::Completed, false)
        .unwrap();
    assert!(!backend.probe(&session).unwrap().alive);
    feed_resource_closed(&bridge, &ledger, "fake-1").unwrap();
    let closed = ledger.get_session("fake-1").unwrap().unwrap();
    assert_eq!(
        projection(&closed, TaskState::Pending),
        PublicLifecycle::Exited
    );
    assert_eq!(closed.resource_state, "closed");
    assert_eq!(closed.agent_state, "gone");
}

/// A competing writer landing between one local event's version read and its
/// write. The wrapper only interposes on the first `upsert_session`: it writes a
/// beat at a higher sequence first and then refuses the caller's own write, which
/// is exactly the race the retry exists for. Every other method is the memory
/// ledger's own answer.
struct RacingLedger {
    inner: MemoryLedger,
    armed: std::sync::Mutex<bool>,
    rival: VersionedSession,
}

impl SessionLedger for RacingLedger {
    fn get_session(&self, task_id: &str) -> anyhow::Result<Option<SessionRecord>> {
        self.inner.get_session(task_id)
    }
    fn upsert_session(&self, task_id: &str, row: &VersionedSession) -> anyhow::Result<bool> {
        let mut armed = self.armed.lock().unwrap();
        if *armed {
            *armed = false;
            assert!(
                self.inner.upsert_session(task_id, &self.rival).unwrap(),
                "the rival beat must land"
            );
            return Ok(false);
        }
        self.inner.upsert_session(task_id, row)
    }
    fn emit(&self, kind: &str, payload: serde_json::Value) {
        self.inner.emit(kind, payload);
    }
    fn task_is_known(&self, task_id: &str) -> anyhow::Result<bool> {
        self.inner.task_is_known(task_id)
    }
    fn task_attempt(&self, task_id: &str) -> anyhow::Result<i64> {
        self.inner.task_attempt(task_id)
    }
    fn list_faults(&self, task_id: &str) -> anyhow::Result<Vec<FaultRecord>> {
        self.inner.list_faults(task_id)
    }
    fn insert_fault(&self, fault: &FaultRecord) -> anyhow::Result<i64> {
        self.inner.insert_fault(fault)
    }
    fn note_alert(&self, line: String) {
        self.inner.note_alert(line)
    }
}

/// A local transition still lands when a plugin beat takes the watermark between
/// its version read and its write. The race left the transition behind before the
/// retry: the delivery stayed `pending` beside a task whose ledger row said
/// `acked`, and the session's public view read `working` for the rest of its life.
#[test]
fn a_lost_watermark_race_retries_on_the_fresh_row() {
    let ledger = MemoryLedger::new();
    let (bridge, _) = tracked(&ledger, "race-1");
    feed_ready(&bridge, &ledger, "race-1").unwrap();
    let before: Observation =
        serde_json::from_str(&ledger.get_session("race-1").unwrap().unwrap().observed_json)
            .unwrap();

    // The rival beat: the session's own generation, a sequence past every version
    // this test has written so far.
    let mut rival = before.clone();
    rival.agent = AgentState::Running;
    rival.version.seq = before.version.seq + 10;
    let rival_row = to_versioned(&rival, "{}", "{}").unwrap();

    let racing = RacingLedger {
        inner: ledger,
        armed: std::sync::Mutex::new(true),
        rival: rival_row,
    };
    let verdict = apply_at_next(&bridge, &racing, "race-1", |v| LifecycleEvent::TurnEnded {
        v,
    })
    .unwrap();
    assert!(matches!(verdict, Verdict::Applied(_)), "{verdict:?}");

    let after: Observation = serde_json::from_str(
        &racing
            .inner
            .get_session("race-1")
            .unwrap()
            .unwrap()
            .observed_json,
    )
    .unwrap();
    assert_eq!(
        after.delivery, before.delivery,
        "the transition carried the delivery it started from"
    );
    assert!(
        after.version.seq > rival.version.seq,
        "the retry wrote past the beat that took the watermark"
    );
}

/// The ledger's columns are the one authority on a session's watermark: the write
/// gate compares them, and a heartbeat that lands in the no-op bump advances them
/// without touching the tuple's bytes. Reading the tuple's older embedded sequence
/// instead made `next_version` propose a number the gate had already refused, so
/// every later local write for that session was lost — the shape that left a dead
/// agent's session projecting `working` beside a task row refused `session_dead`.
#[test]
fn a_column_watermark_ahead_of_the_tuple_governs_local_writes() {
    let ledger = MemoryLedger::new();
    let (bridge, _) = tracked(&ledger, "wm-1");
    feed_ready(&bridge, &ledger, "wm-1").unwrap();
    let stored = ledger.get_session("wm-1").unwrap().unwrap();
    let tuple: Observation = serde_json::from_str(&stored.observed_json).expect("stored tuple");

    // The bump's shape: columns forward, the tuple's bytes untouched.
    let mut advanced = to_versioned(&tuple, "{}", "{}").unwrap();
    advanced.seq = tuple.version.seq as i64 + 10;
    assert!(
        ledger.upsert_session("wm-1", &advanced).unwrap(),
        "the bumped columns are strictly newer than the stored ones"
    );

    let next = next_version(&ledger, "wm-1").expect("the session's watermark");
    assert_eq!(
        next.seq,
        tuple.version.seq + 11,
        "a local write is allocated past the column watermark, not the tuple's"
    );
    let verdict = feed_turn_ended(&bridge, &ledger, "wm-1").expect("the feed runs");
    assert!(
        matches!(verdict, Verdict::Applied(_)),
        "the gate refuses a write whose reader never saw the bumped column: {verdict:?}"
    );
}
