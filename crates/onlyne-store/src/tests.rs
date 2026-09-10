#[cfg(test)]
mod ledger_gates {
    use chrono::{Duration, TimeZone, Utc};
    use onlyne_proto::{Body, Causality, Envelope, LedgerState, MsgKind, Principal, new_envelope};
    use onlyne_session::lifecycle::Version;
    use onlyne_session::reconcile::{Bridge, feed_ready};
    use onlyne_session::{SessionLedger, VersionedSession, apply_persist};
    use rusqlite::Connection;
    use serde_json::json;
    use tempfile::TempDir;

    use crate::{
        Append, ClientStore, LedgerRow, ServerLedger, StoreError, transition_allowed,
    };

    fn temp_db(name: &str) -> (TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        (dir, path)
    }

    fn fixed_time(offset: i64) -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 10, 12, 0, 0)
            .unwrap()
            + Duration::seconds(offset)
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
            backend_ref: format!("{{\"backend\":\"fake\",\"task_id\":\"task-1\",\"value\":\"{value}\"}}"),
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
        assert_eq!(marker, ("onlyne-server".to_string(), 1, 1));

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
            "INSERT INTO schema_marker(name,version,protocol_version) VALUES('onlyne-server',2,1)",
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
        let first = ledger(MsgKind::Task, "first", Some("o-11111111-1111-4111-8111-111111111111"), "fp-a");
        let accepted = store.append_ledger(&first).unwrap();
        assert!(matches!(accepted, Append::Accepted(_)));

        let mut duplicate_same = first.clone();
        duplicate_same.msg_id = new_uuid(12);
        let result = store.append_ledger(&duplicate_same).unwrap();
        match result {
            Append::Duplicate { existing, fingerprint_matches } => {
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
            Append::Duplicate { existing, fingerprint_matches } => {
                assert!(!fingerprint_matches);
                assert_eq!(existing.fingerprint.as_deref(), Some("fp-a"));
            }
            other => panic!("unexpected append result {other:?}"),
        }

        let mut note_a = ledger(MsgKind::Note, "a", None, "note-a");
        note_a.msg_id = new_uuid(14);
        let mut note_b = ledger(MsgKind::Note, "b", None, "note-b");
        note_b.msg_id = new_uuid(15);
        assert!(matches!(store.append_ledger(&note_a).unwrap(), Append::Accepted(_)));
        assert!(matches!(store.append_ledger(&note_b).unwrap(), Append::Accepted(_)));
    }

    #[test]
    fn monotonic_session_gate_accepts_only_newer_watermarks() {
        let (_dir, path) = temp_db("client.db");
        let store = ClientStore::open(&path).unwrap();
        assert!(store.upsert_session("task-1", &versioned(1, 5, "a")).unwrap());
        assert!(!store.upsert_session("task-1", &versioned(1, 4, "b")).unwrap());
        assert_eq!(store.get_session("task-1").unwrap().unwrap().seq, 5);
        assert!(store.upsert_session("task-1", &versioned(2, 0, "c")).unwrap());
        assert!(!store.upsert_session("task-1", &versioned(1, 99, "d")).unwrap());
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
        let mut row = ledger(MsgKind::Task, "first", Some("o-22222222-2222-4222-8222-222222222222"), "fp-a");
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
        let mut second = ledger(MsgKind::Task, "second", Some("o-33333333-3333-4333-8333-333333333333"), "fp-2");
        second.msg_id = new_uuid(30);
        second.enqueued_at = crate::rfc3339(fixed_time(20));
        let mut first = ledger(MsgKind::Task, "first", Some("o-44444444-4444-4444-8444-444444444444"), "fp-1");
        first.msg_id = new_uuid(31);
        first.enqueued_at = crate::rfc3339(fixed_time(10));
        store.append_ledger(&second).unwrap();
        store.append_ledger(&first).unwrap();
        let queued = store.queued_for("worker", 10).unwrap();
        assert_eq!(queued.iter().map(|row| row.msg_id.as_str()).collect::<Vec<_>>(), vec![first.msg_id.as_str(), second.msg_id.as_str()]);

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
        let old_state = rows.iter().find(|row| row.msg_id == old.msg_id).unwrap().state;
        let fresh_state = rows.iter().find(|row| row.msg_id == fresh.msg_id).unwrap().state;
        assert_eq!(old_state, LedgerState::Expired);
        assert_eq!(fresh_state, LedgerState::Queued);
    }

    #[test]
    fn prune_nulls_old_acked_body_and_keeps_out_head_and_fresh_body() {
        let (_dir, path) = temp_db("server.db");
        let store = ServerLedger::open(&path, 14).unwrap();
        let mut old = ledger(MsgKind::Task, "old body", Some("o-55555555-5555-4555-8555-555555555555"), "fp-old");
        old.msg_id = new_uuid(50);
        let mut fresh = ledger(MsgKind::Task, "fresh body", Some("o-66666666-6666-4666-8666-666666666666"), "fp-fresh");
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
        assert!(fresh_row.body_json.as_deref().unwrap().contains("fresh body"));
    }

    #[test]
    fn intent_queue_lifecycle_and_flush_order() {
        let (_dir, path) = temp_db("client.db");
        let store = ClientStore::open(&path).unwrap();
        let env = envelope(MsgKind::Task, "intent", Some("o-77777777-7777-4777-8777-777777777777"));
        let env_json = serde_json::to_value(&env).unwrap();
        assert!(store.enqueue_intent(env.op_id.as_deref().unwrap(), &env_json).unwrap());
        let now = Utc::now();
        assert_eq!(store.due_intents(now - Duration::seconds(1), 10).unwrap().len(), 0);
        assert_eq!(store.due_intents(now + Duration::seconds(1), 10).unwrap().len(), 1);
        assert!(store
            .bump_intent(env.op_id.as_deref().unwrap(), now + Duration::seconds(20), "retry")
            .unwrap());
        assert_eq!(store.due_intents(now + Duration::seconds(10), 10).unwrap().len(), 0);
        assert_eq!(store.due_intents(now + Duration::seconds(21), 10).unwrap()[0].state, "retrying");
        assert!(store.accept_intent(env.op_id.as_deref().unwrap(), &json!({"ok": true})).unwrap());
        assert_eq!(store.due_intents(now + Duration::seconds(100), 10).unwrap().len(), 0);

        let env2 = envelope(MsgKind::Task, "intent2", Some("o-88888888-8888-4888-8888-888888888888"));
        let env2_json = serde_json::to_value(&env2).unwrap();
        assert!(store.enqueue_intent(env2.op_id.as_deref().unwrap(), &env2_json).unwrap());
        assert!(store.bump_intent(env2.op_id.as_deref().unwrap(), fixed_time(1), "one").unwrap());
        assert!(store.bump_intent(env2.op_id.as_deref().unwrap(), fixed_time(2), "two").unwrap());
        assert!(store.bump_intent(env2.op_id.as_deref().unwrap(), fixed_time(3), "three").unwrap());
        assert!(store.exhaust_intent(env2.op_id.as_deref().unwrap(), "exhausted").unwrap());
        assert_eq!(store.due_intents(fixed_time(4), 10).unwrap().len(), 0);

        let env3 = envelope(MsgKind::Task, "intent3", Some("o-99999999-9999-4999-8999-999999999999"));
        let env4 = envelope(MsgKind::Task, "intent4", Some("o-aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"));
        store.enqueue_intent(env3.op_id.as_deref().unwrap(), &serde_json::to_value(&env3).unwrap()).unwrap();
        store.enqueue_intent(env4.op_id.as_deref().unwrap(), &serde_json::to_value(&env4).unwrap()).unwrap();
        let order = store.flush_order().unwrap();
        assert_eq!(
            order.iter().map(|row| row.op_id.as_str()).collect::<Vec<_>>(),
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
        let env = envelope(MsgKind::Task, "tracked", Some("o-bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"));
        store.enqueue_intent(env.op_id.as_deref().unwrap(), &serde_json::to_value(&env).unwrap()).unwrap();
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
}
