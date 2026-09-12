#[cfg(test)]
mod ledger_gates {
    use chrono::{Duration, TimeZone, Utc};
    use onlyne_proto::{
        Body, Causality, Envelope, LedgerQuery, LedgerState, MsgKind, Principal, new_envelope,
    };
    use onlyne_session::lifecycle::Version;
    use onlyne_session::reconcile::{Bridge, feed_ready};
    use onlyne_session::{SessionLedger, VersionedSession, apply_persist};
    use rusqlite::Connection;
    use serde_json::json;
    use tempfile::TempDir;

    use crate::{
        Append, ClientStore, FaultQuery, LedgerRow, ServerFaultRow, ServerLedger, StoreError,
        transition_allowed,
    };

    fn temp_db(name: &str) -> (TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        (dir, path)
    }

    fn fixed_time(offset: i64) -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 10, 12, 0, 0).unwrap() + Duration::seconds(offset)
    }

    fn envelope(kind: MsgKind, text: &str, op_id: Option<&str>) -> Envelope {
        let causality = (kind != MsgKind::Note).then(|| Causality::root(new_uuid(10)));
        let mut env = new_envelope(
            kind,
            Principal::role("alice"),
            Principal::role("worker"),
            Body::text(text),
            causality,
        )
        .unwrap();
        env.id = new_uuid(11);
        env.op_id = op_id.map(str::to_string);
        env.ts = fixed_time(0);
        env
    }

    fn ledger(kind: MsgKind, text: &str, op_id: Option<&str>, fingerprint: &str) -> LedgerRow {
        LedgerRow::from_envelope(&envelope(kind, text, op_id), fingerprint).unwrap()
    }

    fn new_uuid(seed: u8) -> String {
        format!("00000000-0000-4000-8000-{seed:012x}")
    }

    fn versioned(generation: i64, seq: i64, value: &str) -> VersionedSession {
        VersionedSession {
            agent_state: format!("agent-{value}"),
            delivery_state: format!("delivery-{value}"),
            resource_state: format!("resource-{value}"),
            public_lifecycle: format!("life-{value}"),
            recovery_substate: format!("recovery-{value}"),
            desired_json: format!("{{\"desired\":\"{value}\"}}"),
            observed_json: format!("{{\"observed\":\"{value}\"}}"),
            generation,
            seq,
            backend_ref: format!(
                "{{\"backend\":\"fake\",\"task_id\":\"task-1\",\"value\":\"{value}\"}}"
            ),
            mismatch_count: seq,
            updated_at: 1_789_000_000 + seq,
        }
    }

    #[test]
    fn fresh_file_creates_schema_marker_and_reopen_is_noop() {
        let (_dir, server_path) = temp_db("server.db");
        let server = ServerLedger::open(&server_path, 14).unwrap();
        let reopened = ServerLedger::open(&server_path, 14).unwrap();
        assert_eq!(server.path(), reopened.path());
        let conn = Connection::open(&server_path).unwrap();
        let marker: (String, i64, i64) = conn
            .query_row(
                "SELECT name,version,protocol_version FROM schema_marker",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(marker, ("onlyne-server".to_string(), 2, 1));

        let (_dir, client_path) = temp_db("client.db");
        ClientStore::open(&client_path).unwrap();
        ClientStore::open(&client_path).unwrap();
        let conn = Connection::open(&client_path).unwrap();
        let marker: (String, i64, i64) = conn
            .query_row(
                "SELECT name,version,protocol_version FROM schema_marker",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(marker, ("onlyne-client".to_string(), 1, 1));
    }

    #[test]
    fn server_refuses_legacy_and_wrong_marker() {
        let (_dir, legacy_path) = temp_db("server-legacy.db");
        let conn = Connection::open(&legacy_path).unwrap();
        conn.execute("CREATE TABLE io_cursors(id TEXT PRIMARY KEY)", [])
            .unwrap();
        let err = ServerLedger::open(&legacy_path, 14).unwrap_err();
        assert_eq!(err.to_string(), crate::UNSUPPORTED_SCHEMA);

        let (_dir, marker_path) = temp_db("server-marker.db");
        let conn = Connection::open(&marker_path).unwrap();
        conn.execute("CREATE TABLE schema_marker(name TEXT PRIMARY KEY, version INTEGER NOT NULL, protocol_version INTEGER NOT NULL)", []).unwrap();
        conn.execute(
            "INSERT INTO schema_marker(name,version,protocol_version) VALUES('onlyne-server',3,1)",
            [],
        )
        .unwrap();
        let err = ServerLedger::open(&marker_path, 14).unwrap_err();
        assert_eq!(err.to_string(), crate::UNSUPPORTED_SCHEMA);
    }

    #[test]
    fn client_refuses_legacy_and_wrong_marker() {
        let (_dir, legacy_path) = temp_db("client-legacy.db");
        let conn = Connection::open(&legacy_path).unwrap();
        conn.execute("CREATE TABLE io_cursors(id TEXT PRIMARY KEY)", [])
            .unwrap();
        let err = ClientStore::open(&legacy_path).unwrap_err();
        assert_eq!(err.to_string(), crate::UNSUPPORTED_SCHEMA);

        let (_dir, marker_path) = temp_db("client-marker.db");
        let conn = Connection::open(&marker_path).unwrap();
        conn.execute("CREATE TABLE schema_marker(name TEXT PRIMARY KEY, version INTEGER NOT NULL, protocol_version INTEGER NOT NULL)", []).unwrap();
        conn.execute(
            "INSERT INTO schema_marker(name,version,protocol_version) VALUES('onlyne-client',2,1)",
            [],
        )
        .unwrap();
        let err = ClientStore::open(&marker_path).unwrap_err();
        assert_eq!(err.to_string(), crate::UNSUPPORTED_SCHEMA);
    }

    #[test]
    fn append_ledger_idempotency_and_null_op_id() {
        let (_dir, path) = temp_db("server.db");
        let store = ServerLedger::open(&path, 14).unwrap();
        let first = ledger(
            MsgKind::Task,
            "first",
            Some("o-11111111-1111-4111-8111-111111111111"),
            "fp-a",
        );
        let accepted = store.append_ledger(&first).unwrap();
        assert!(matches!(accepted, Append::Accepted(_)));

        let mut duplicate_same = first.clone();
        duplicate_same.msg_id = new_uuid(12);
        let result = store.append_ledger(&duplicate_same).unwrap();
        match result {
            Append::Duplicate {
                existing,
                fingerprint_matches,
            } => {
                assert!(fingerprint_matches);
                assert_eq!(existing.msg_id, first.msg_id);
            }
            other => panic!("unexpected append result {other:?}"),
        }

        let mut duplicate_different = first.clone();
        duplicate_different.msg_id = new_uuid(13);
        duplicate_different.fingerprint = Some("fp-b".to_string());
        let result = store.append_ledger(&duplicate_different).unwrap();
        match result {
            Append::Duplicate {
                existing,
                fingerprint_matches,
            } => {
                assert!(!fingerprint_matches);
                assert_eq!(existing.fingerprint.as_deref(), Some("fp-a"));
            }
            other => panic!("unexpected append result {other:?}"),
        }

        let mut note_a = ledger(MsgKind::Note, "a", None, "note-a");
        note_a.msg_id = new_uuid(14);
        let mut note_b = ledger(MsgKind::Note, "b", None, "note-b");
        note_b.msg_id = new_uuid(15);
        assert!(matches!(
            store.append_ledger(&note_a).unwrap(),
            Append::Accepted(_)
        ));
        assert!(matches!(
            store.append_ledger(&note_b).unwrap(),
            Append::Accepted(_)
        ));
    }

    #[test]
    fn monotonic_session_gate_accepts_only_newer_watermarks() {
        let (_dir, path) = temp_db("client.db");
        let store = ClientStore::open(&path).unwrap();
        assert!(
            store
                .upsert_session("task-1", &versioned(1, 5, "a"))
                .unwrap()
        );
        assert!(
            !store
                .upsert_session("task-1", &versioned(1, 4, "b"))
                .unwrap()
        );
        assert_eq!(store.get_session("task-1").unwrap().unwrap().seq, 5);
        assert!(
            store
                .upsert_session("task-1", &versioned(2, 0, "c"))
                .unwrap()
        );
        assert!(
            !store
                .upsert_session("task-1", &versioned(1, 99, "d"))
                .unwrap()
        );
        let row = store.get_session("task-1").unwrap().unwrap();
        assert_eq!((row.generation, row.seq), (2, 0));
        assert!(row.desired_json.contains("c"));
    }

    #[test]
    fn transition_matrix_and_mutation_guard() {
        let states = [
            LedgerState::Queued,
            LedgerState::InFlight,
            LedgerState::Acked,
            LedgerState::Rejected,
            LedgerState::Expired,
        ];
        for from in states {
            for to in states {
                let expected = matches!(
                    (from, to),
                    (LedgerState::Queued, LedgerState::InFlight)
                        | (LedgerState::Queued, LedgerState::Rejected)
                        | (LedgerState::Queued, LedgerState::Expired)
                        | (LedgerState::InFlight, LedgerState::Acked)
                        | (LedgerState::InFlight, LedgerState::Queued)
                        | (LedgerState::InFlight, LedgerState::Rejected)
                );
                assert_eq!(transition_allowed(from, to), expected, "{from:?}->{to:?}");
            }
        }

        let (_dir, path) = temp_db("server.db");
        let store = ServerLedger::open(&path, 14).unwrap();
        let mut row = ledger(
            MsgKind::Task,
            "first",
            Some("o-22222222-2222-4222-8222-222222222222"),
            "fp-a",
        );
        row.msg_id = new_uuid(20);
        store.append_ledger(&row).unwrap();
        store.mark_in_flight(&row.msg_id).unwrap();
        store.mark_acked(&row.msg_id, fixed_time(10)).unwrap();
        let err = store.mark_in_flight(&row.msg_id).unwrap_err();
        assert_eq!(
            err,
            StoreError::InvalidState {
                from: "acked".to_string(),
                to: "in_flight".to_string()
            }
        );
    }

    #[test]
    fn queued_for_fifo_and_requeue_in_flight_preserves_rows() {
        let (_dir, path) = temp_db("server.db");
        let store = ServerLedger::open(&path, 14).unwrap();
        let mut second = ledger(
            MsgKind::Task,
            "second",
            Some("o-33333333-3333-4333-8333-333333333333"),
            "fp-2",
        );
        second.msg_id = new_uuid(30);
        second.enqueued_at = crate::rfc3339(fixed_time(20));
        let mut first = ledger(
            MsgKind::Task,
            "first",
            Some("o-44444444-4444-4444-8444-444444444444"),
            "fp-1",
        );
        first.msg_id = new_uuid(31);
        first.enqueued_at = crate::rfc3339(fixed_time(10));
        store.append_ledger(&second).unwrap();
        store.append_ledger(&first).unwrap();
        let queued = store.queued_for("worker", 10).unwrap();
        assert_eq!(
            queued
                .iter()
                .map(|row| row.msg_id.as_str())
                .collect::<Vec<_>>(),
            vec![first.msg_id.as_str(), second.msg_id.as_str()]
        );

        store.mark_in_flight(&first.msg_id).unwrap();
        assert_eq!(store.in_flight_for("worker").unwrap().len(), 1);
        assert_eq!(store.requeue_in_flight("worker").unwrap(), 1);
        assert_eq!(store.queued_for("worker", 10).unwrap().len(), 2);
        assert_eq!(store.in_flight_for("worker").unwrap().len(), 0);
    }

    #[test]
    fn expire_queued_before_settles_only_old_rows() {
        let (_dir, path) = temp_db("server.db");
        let store = ServerLedger::open(&path, 14).unwrap();
        let mut old = ledger(MsgKind::Note, "old", None, "fp-old");
        old.msg_id = new_uuid(40);
        old.enqueued_at = crate::rfc3339(fixed_time(0));
        let mut fresh = ledger(MsgKind::Note, "fresh", None, "fp-fresh");
        fresh.msg_id = new_uuid(41);
        fresh.enqueued_at = crate::rfc3339(fixed_time(30));
        store.append_ledger(&old).unwrap();
        store.append_ledger(&fresh).unwrap();
        assert_eq!(store.expire_queued_before(fixed_time(10)).unwrap(), 1);
        let rows = store.ledger_query(Default::default()).unwrap();
        let old_state = rows
            .iter()
            .find(|row| row.msg_id == old.msg_id)
            .unwrap()
            .state;
        let fresh_state = rows
            .iter()
            .find(|row| row.msg_id == fresh.msg_id)
            .unwrap()
            .state;
        assert_eq!(old_state, LedgerState::Expired);
        assert_eq!(fresh_state, LedgerState::Queued);
    }

    #[test]
    fn prune_nulls_old_acked_body_and_keeps_out_head_and_fresh_body() {
        let (_dir, path) = temp_db("server.db");
        let store = ServerLedger::open(&path, 14).unwrap();
        let mut old = ledger(
            MsgKind::Task,
            "old body",
            Some("o-55555555-5555-4555-8555-555555555555"),
            "fp-old",
        );
        old.msg_id = new_uuid(50);
        let mut fresh = ledger(
            MsgKind::Task,
            "fresh body",
            Some("o-66666666-6666-4666-8666-666666666666"),
            "fp-fresh",
        );
        fresh.msg_id = new_uuid(51);
        store.append_ledger(&old).unwrap();
        store.append_ledger(&fresh).unwrap();
        store.mark_in_flight(&old.msg_id).unwrap();
        store.mark_acked(&old.msg_id, fixed_time(0)).unwrap();
        store.mark_in_flight(&fresh.msg_id).unwrap();
        store.mark_acked(&fresh.msg_id, fixed_time(30)).unwrap();
        assert_eq!(store.prune(fixed_time(10)).unwrap(), 1);
        let rows = store.ledger_query(Default::default()).unwrap();
        let old_row = rows.iter().find(|row| row.msg_id == old.msg_id).unwrap();
        assert_eq!(old_row.body_json, None);
        assert_eq!(old_row.out_head.as_deref(), Some("old body"));
        let fresh_row = rows.iter().find(|row| row.msg_id == fresh.msg_id).unwrap();
        assert!(
            fresh_row
                .body_json
                .as_deref()
                .unwrap()
                .contains("fresh body")
        );
    }

    #[test]
    fn intent_queue_lifecycle_and_flush_order() {
        let (_dir, path) = temp_db("client.db");
        let store = ClientStore::open(&path).unwrap();
        let env = envelope(
            MsgKind::Task,
            "intent",
            Some("o-77777777-7777-4777-8777-777777777777"),
        );
        let env_json = serde_json::to_value(&env).unwrap();
        assert!(
            store
                .enqueue_intent(env.op_id.as_deref().unwrap(), &env_json)
                .unwrap()
        );
        let now = Utc::now();
        assert_eq!(
            store
                .due_intents(now - Duration::seconds(1), 10)
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            store
                .due_intents(now + Duration::seconds(1), 10)
                .unwrap()
                .len(),
            1
        );
        assert!(
            store
                .bump_intent(
                    env.op_id.as_deref().unwrap(),
                    now + Duration::seconds(20),
                    "retry"
                )
                .unwrap()
        );
        assert_eq!(
            store
                .due_intents(now + Duration::seconds(10), 10)
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            store.due_intents(now + Duration::seconds(21), 10).unwrap()[0].state,
            "retrying"
        );
        assert!(
            store
                .accept_intent(env.op_id.as_deref().unwrap(), &json!({"ok": true}))
                .unwrap()
        );
        assert_eq!(
            store
                .due_intents(now + Duration::seconds(100), 10)
                .unwrap()
                .len(),
            0
        );

        let env2 = envelope(
            MsgKind::Task,
            "intent2",
            Some("o-88888888-8888-4888-8888-888888888888"),
        );
        let env2_json = serde_json::to_value(&env2).unwrap();
        assert!(
            store
                .enqueue_intent(env2.op_id.as_deref().unwrap(), &env2_json)
                .unwrap()
        );
        assert!(
            store
                .bump_intent(env2.op_id.as_deref().unwrap(), fixed_time(1), "one")
                .unwrap()
        );
        assert!(
            store
                .bump_intent(env2.op_id.as_deref().unwrap(), fixed_time(2), "two")
                .unwrap()
        );
        assert!(
            store
                .bump_intent(env2.op_id.as_deref().unwrap(), fixed_time(3), "three")
                .unwrap()
        );
        assert!(
            store
                .exhaust_intent(env2.op_id.as_deref().unwrap(), "exhausted")
                .unwrap()
        );
        assert_eq!(store.due_intents(fixed_time(4), 10).unwrap().len(), 0);

        let env3 = envelope(
            MsgKind::Task,
            "intent3",
            Some("o-99999999-9999-4999-8999-999999999999"),
        );
        let env4 = envelope(
            MsgKind::Task,
            "intent4",
            Some("o-aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"),
        );
        store
            .enqueue_intent(
                env3.op_id.as_deref().unwrap(),
                &serde_json::to_value(&env3).unwrap(),
            )
            .unwrap();
        store
            .enqueue_intent(
                env4.op_id.as_deref().unwrap(),
                &serde_json::to_value(&env4).unwrap(),
            )
            .unwrap();
        let order = store.flush_order().unwrap();
        assert_eq!(
            order
                .iter()
                .map(|row| row.op_id.as_str())
                .collect::<Vec<_>>(),
            vec![
                env3.op_id.as_deref().unwrap(),
                env4.op_id.as_deref().unwrap(),
            ]
        );
    }

    #[test]
    fn events_since_and_head_survive_restart() {
        let (_dir, path) = temp_db("server.db");
        let store = ServerLedger::open(&path, 14).unwrap();
        assert_eq!(store.append_event("alpha", &json!({"n": 1})).unwrap(), 1);
        assert_eq!(store.append_event("beta", &json!({"n": 2})).unwrap(), 2);
        assert_eq!(store.event_head().unwrap(), 2);
        let reopened = ServerLedger::open(&path, 14).unwrap();
        assert_eq!(reopened.event_head().unwrap(), 2);
        let events = reopened.events_since(1, 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "beta");
        assert_eq!(events[0].data, json!({"n": 2}));
    }

    #[test]
    fn client_store_drives_apply_persist_created_and_ready() {
        let (_dir, path) = temp_db("client.db");
        let store = ClientStore::open(&path).unwrap();
        let bridge = Bridge::new();
        let task_id = new_uuid(10);
        let env = envelope(
            MsgKind::Task,
            "tracked",
            Some("o-bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"),
        );
        store
            .enqueue_intent(
                env.op_id.as_deref().unwrap(),
                &serde_json::to_value(&env).unwrap(),
            )
            .unwrap();
        apply_persist(
            &bridge,
            &store,
            &task_id,
            &onlyne_session::lifecycle::LifecycleEvent::Created {
                v: Version::new(1, 1),
            },
        )
        .unwrap();
        let created = store.get_session(&task_id).unwrap().unwrap();
        assert_eq!(created.public_lifecycle, "created");
        assert_eq!((created.generation, created.seq), (1, 1));
        feed_ready(&bridge, &store, &task_id).unwrap();
        let ready = store.get_session(&task_id).unwrap().unwrap();
        assert_eq!(ready.public_lifecycle, "idle");
        assert_eq!((ready.generation, ready.seq), (1, 2));
        assert!(store.list_faults(&task_id).unwrap().is_empty());
        assert!(store.event_head().unwrap() >= 2);
    }

    #[test]
    fn insert_fault_round_trips_task_columns() {
        let (_dir, path) = temp_db("client-fault.db");
        let store = ClientStore::open(&path).unwrap();
        let fault = onlyne_session::FaultRecord {
            id: 999,
            task_id: "task-fault-1".to_string(),
            session_id: "task-fault-1".to_string(),
            generation: 2,
            seq: 7,
            desired_json: "{\"desired\":true}".to_string(),
            observed_json: "{\"observed\":true}".to_string(),
            intent: "reconcile:probe_dead".to_string(),
            attempt: 3,
            backend_ref: "{\"backend\":\"fake\"}".to_string(),
            kind: "probe_dead".to_string(),
            reason: "backend resource gone".to_string(),
            state: "open".to_string(),
            created_at: 1_789_000_000,
        };
        let assigned = store.insert_fault(&fault).unwrap();
        assert!(assigned > 0);
        let rows = store.list_faults("task-fault-1").unwrap();
        assert_eq!(rows.len(), 1);
        let stored = &rows[0];
        assert_eq!(stored.id, assigned);
        assert_eq!(stored.task_id, fault.task_id);
        assert_eq!(stored.session_id, fault.session_id);
        assert_eq!(stored.generation, fault.generation);
        assert_eq!(stored.seq, fault.seq);
        assert_eq!(stored.kind, fault.kind);
        assert_eq!(stored.state, fault.state);
        assert_eq!(stored.created_at, fault.created_at);
    }

    fn block_event_type(path: &std::path::Path, kind: &str) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(&format!(
            "CREATE TRIGGER block_events BEFORE INSERT ON events WHEN NEW.type='{kind}' BEGIN SELECT RAISE(ABORT,'forced event failure'); END;"
        ))
        .unwrap();
    }

    fn unblock_event_type(path: &std::path::Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch("DROP TRIGGER block_events;").unwrap();
    }

    fn fault_draft(task_id: &str, kind: &str) -> ServerFaultRow {
        ServerFaultRow {
            id: 0,
            task_id: Some(task_id.to_string()),
            role: Some("worker".to_string()),
            session_id: Some(task_id.to_string()),
            generation: Some(1),
            seq: Some(2),
            desired_json: Some("{\"desired\":true}".to_string()),
            observed_json: Some("{\"observed\":true}".to_string()),
            intent: Some("reconcile:probe_dead".to_string()),
            attempt: Some(1),
            backend_ref: Some("{\"backend\":\"fake\"}".to_string()),
            kind: kind.to_string(),
            reason: "backend resource gone".to_string(),
            state: "open".to_string(),
            created_at: 1_789_000_000,
        }
    }

    #[test]
    fn late_ack_on_expired_row_is_refused_and_keeps_one_event() {
        let (_dir, path) = temp_db("server.db");
        let store = ServerLedger::open(&path, 14).unwrap();
        let mut row = ledger(MsgKind::Note, "late ack", None, "fp-late");
        row.msg_id = new_uuid(60);
        store.append_ledger(&row).unwrap();
        store.expire_one(&row.msg_id, "ttl elapsed").unwrap();
        let err = store.mark_acked(&row.msg_id, fixed_time(30)).unwrap_err();
        assert_eq!(
            err,
            StoreError::InvalidState {
                from: "expired".to_string(),
                to: "acked".to_string()
            }
        );
        let stored = store
            .ledger_query(LedgerQuery {
                msg_id: Some(row.msg_id.clone()),
                limit: 1,
                ..LedgerQuery::default()
            })
            .unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].state, LedgerState::Expired);
        assert_eq!(store.event_head().unwrap(), 1);
    }

    #[test]
    fn requeue_one_moves_in_flight_row_and_publishes_one_event() {
        let (_dir, path) = temp_db("server.db");
        let store = ServerLedger::open(&path, 14).unwrap();
        let mut row = ledger(
            MsgKind::Task,
            "requeue",
            Some("o-cccccccc-cccc-4ccc-8ccc-cccccccccccc"),
            "fp-r",
        );
        row.msg_id = new_uuid(61);
        store.append_ledger(&row).unwrap();
        store.mark_in_flight(&row.msg_id).unwrap();
        let updated = store.requeue_one(&row.msg_id).unwrap();
        assert_eq!(updated.state, LedgerState::Queued);
        assert_eq!(store.event_head().unwrap(), 1);
        let events = store.events_since(0, 10).unwrap();
        assert_eq!(events[0].kind, "ledger_state");
        assert_eq!(events[0].data["type"], "ledger_state");
        assert_eq!(events[0].data["data"]["msg_id"], row.msg_id);
        assert_eq!(events[0].data["data"]["state"], "queued");
    }

    #[test]
    fn requeue_one_is_noop_on_queued_row() {
        let (_dir, path) = temp_db("server.db");
        let store = ServerLedger::open(&path, 14).unwrap();
        let mut row = ledger(
            MsgKind::Task,
            "double requeue",
            Some("o-dddddddd-dddd-4ddd-8ddd-dddddddddddd"),
            "fp-d",
        );
        row.msg_id = new_uuid(62);
        store.append_ledger(&row).unwrap();
        let first = store.requeue_one(&row.msg_id).unwrap();
        let second = store.requeue_one(&row.msg_id).unwrap();
        assert_eq!(first.state, LedgerState::Queued);
        assert_eq!(second.state, LedgerState::Queued);
        assert_eq!(first.msg_id, second.msg_id);
        assert_eq!(store.event_head().unwrap(), 0);
    }

    #[test]
    fn expire_one_settles_queued_row_and_publishes_one_event() {
        let (_dir, path) = temp_db("server.db");
        let store = ServerLedger::open(&path, 14).unwrap();
        let mut row = ledger(MsgKind::Note, "sweep me", None, "fp-sweep");
        row.msg_id = new_uuid(63);
        store.append_ledger(&row).unwrap();
        let updated = store.expire_one(&row.msg_id, "ttl elapsed").unwrap();
        assert_eq!(updated.state, LedgerState::Expired);
        assert_eq!(updated.reason.as_deref(), Some("ttl elapsed"));
        let events = store.events_since(0, 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "ledger_state");
        assert_eq!(events[0].data["data"]["state"], "expired");
        assert_eq!(events[0].data["data"]["reason"], "ttl elapsed");
    }

    #[test]
    fn requeue_one_rolls_back_when_the_event_insert_fails() {
        let (_dir, path) = temp_db("server.db");
        let store = ServerLedger::open(&path, 14).unwrap();
        let mut row = ledger(
            MsgKind::Task,
            "rollback",
            Some("o-eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee"),
            "fp-e",
        );
        row.msg_id = new_uuid(64);
        store.append_ledger(&row).unwrap();
        store.mark_in_flight(&row.msg_id).unwrap();
        block_event_type(&path, "ledger_state");
        let result = store.requeue_one(&row.msg_id);
        unblock_event_type(&path);
        assert!(result.is_err(), "blocked event insert must fail the call");
        let stored = store
            .ledger_query(LedgerQuery {
                msg_id: Some(row.msg_id.clone()),
                limit: 1,
                ..LedgerQuery::default()
            })
            .unwrap();
        assert_eq!(stored[0].state, LedgerState::InFlight);
        assert_eq!(store.event_head().unwrap(), 0);
    }

    #[test]
    fn expire_one_rolls_back_when_the_event_insert_fails() {
        let (_dir, path) = temp_db("server.db");
        let store = ServerLedger::open(&path, 14).unwrap();
        let mut row = ledger(MsgKind::Note, "rollback expiry", None, "fp-x");
        row.msg_id = new_uuid(65);
        store.append_ledger(&row).unwrap();
        block_event_type(&path, "ledger_state");
        let result = store.expire_one(&row.msg_id, "ttl elapsed");
        unblock_event_type(&path);
        assert!(result.is_err(), "blocked event insert must fail the call");
        let stored = store
            .ledger_query(LedgerQuery {
                msg_id: Some(row.msg_id.clone()),
                limit: 1,
                ..LedgerQuery::default()
            })
            .unwrap();
        assert_eq!(stored[0].state, LedgerState::Queued);
        assert_eq!(store.event_head().unwrap(), 0);
    }

    #[test]
    fn update_fault_state_moves_open_faults_and_publishes_events() {
        let (_dir, path) = temp_db("server.db");
        let store = ServerLedger::open(&path, 14).unwrap();
        let open_id = store
            .record_fault(&fault_draft("task-f-1", "probe_dead"))
            .unwrap();
        let settled_id = store
            .record_fault(&fault_draft("task-f-1", "older"))
            .unwrap();
        store.ack_fault(settled_id).unwrap();
        let moved = store
            .update_fault_state("task-f-1", "acked", "operator ack")
            .unwrap();
        assert_eq!(moved.len(), 1);
        assert_eq!(moved[0].id, open_id);
        assert_eq!(moved[0].state, "acked");
        assert_eq!(moved[0].reason, "operator ack");
        let events = store.events_since(0, 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "fault");
        assert_eq!(events[0].data["data"]["id"], open_id);
        assert_eq!(events[0].data["data"]["state"], "acked");
        let rows = store
            .faults_query(FaultQuery {
                task_id: Some("task-f-1".to_string()),
                limit: 10,
                ..FaultQuery::default()
            })
            .unwrap();
        assert_eq!(rows.iter().filter(|row| row.state == "acked").count(), 2);
        assert!(store.open_faults().unwrap().is_empty());
    }

    #[test]
    fn update_fault_state_rolls_back_when_the_event_insert_fails() {
        let (_dir, path) = temp_db("server.db");
        let store = ServerLedger::open(&path, 14).unwrap();
        store
            .record_fault(&fault_draft("task-f-2", "probe_dead"))
            .unwrap();
        block_event_type(&path, "fault");
        let result = store.update_fault_state("task-f-2", "acked", "operator ack");
        unblock_event_type(&path);
        assert!(result.is_err(), "blocked event insert must fail the call");
        let rows = store
            .faults_query(FaultQuery {
                task_id: Some("task-f-2".to_string()),
                limit: 10,
                ..FaultQuery::default()
            })
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].state, "open");
        assert_eq!(store.event_head().unwrap(), 0);
    }

    /// The preview is grapheme-safe: a multi-byte scalar, a combining sequence,
    /// and a ZWJ emoji each survive whole, and the empty and exact-ceiling
    /// bodies stay untouched.
    #[test]
    fn out_head_never_splits_a_multibyte_scalar() {
        use unicode_segmentation::UnicodeSegmentation;

        use crate::server::{OUT_HEAD_CLUSTERS, head_preview};

        assert_eq!(head_preview(""), "");
        assert_eq!(head_preview("héllo — v1"), "héllo — v1");

        let exact: String = "é".repeat(OUT_HEAD_CLUSTERS);
        assert_eq!(head_preview(&exact), exact);

        let over: String = "é".repeat(OUT_HEAD_CLUSTERS + 1);
        let head = head_preview(&over);
        assert_eq!(head.graphemes(true).count(), OUT_HEAD_CLUSTERS);
        assert_eq!(head, "é".repeat(OUT_HEAD_CLUSTERS));
        assert!(head.is_char_boundary(head.len()));
        assert!(head.len() < onlyne_frame::MAX_FRAME_BYTES);
    }

    #[test]
    fn out_head_keeps_combining_and_zwj_clusters_intact() {
        use crate::server::{OUT_HEAD_CLUSTERS, head_preview};
        use unicode_segmentation::UnicodeSegmentation;

        let combining = "e\u{0301}".repeat(OUT_HEAD_CLUSTERS + 40);
        let head = head_preview(&combining);
        assert_eq!(head.graphemes(true).count(), OUT_HEAD_CLUSTERS);
        assert!(head.ends_with("e\u{0301}"), "combining mark was orphaned");
        assert!(
            head.graphemes(true).all(|cluster| cluster == "e\u{0301}"),
            "every kept cluster is the whole sequence"
        );

        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
        let zwj = family.repeat(OUT_HEAD_CLUSTERS + 7);
        let head = head_preview(&zwj);
        assert_eq!(head.graphemes(true).count(), OUT_HEAD_CLUSTERS);
        assert!(head.ends_with(family), "a ZWJ sequence was cut mid-cluster");
        assert_eq!(head.matches('\u{200D}').count(), OUT_HEAD_CLUSTERS * 3);
        assert!(head.len() < onlyne_frame::MAX_FRAME_BYTES);

        let flag = "\u{1F1EF}\u{1F1F5}".repeat(OUT_HEAD_CLUSTERS + 3);
        let head = head_preview(&flag);
        assert_eq!(head.graphemes(true).count(), OUT_HEAD_CLUSTERS);
        assert!(head.ends_with("\u{1F1EF}\u{1F1F5}"), "a flag was split");
    }

    /// The plan reads one task's ledger rows back in write order. Three rows
    /// written inside one `enqueued_at` second still come back in insertion
    /// order, and the general view keeps its newest-first presentation.
    #[test]
    fn task_keyed_ledger_read_is_insertion_ordered() {
        let (_dir, path) = temp_db("server.db");
        let store = ServerLedger::open(&path, 14).unwrap();
        let task = new_uuid(10);
        let mut ids = Vec::new();
        for (index, op) in [
            "o-66666666-6666-4666-8666-666666666666",
            "o-77777777-7777-4777-8777-777777777777",
            "o-88888888-8888-4888-8888-888888888888",
        ]
        .into_iter()
        .enumerate()
        {
            let mut row = ledger(MsgKind::Task, "payload", Some(op), "fp-order");
            row.msg_id = new_uuid(70 + index as u8);
            row.enqueued_at = crate::rfc3339(fixed_time(0));
            store.append_ledger(&row).unwrap();
            ids.push(row.msg_id);
        }

        let ordered = store.ledger_task(&task, 10).unwrap();
        assert_eq!(
            ordered
                .iter()
                .map(|row| row.msg_id.as_str())
                .collect::<Vec<_>>(),
            ids.iter().map(String::as_str).collect::<Vec<_>>(),
            "task-keyed read follows the write order"
        );

        let view = store
            .ledger_query(LedgerQuery {
                task: Some(task.clone()),
                limit: 10,
                ..LedgerQuery::default()
            })
            .unwrap();
        assert_eq!(
            view.iter()
                .map(|row| row.msg_id.as_str())
                .collect::<Vec<_>>(),
            ids.iter().rev().map(String::as_str).collect::<Vec<_>>(),
            "the general view stays newest first"
        );
    }
}
