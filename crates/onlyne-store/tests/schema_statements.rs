//! Schema and statement agreement.
//!
//! SQLite resolves column names when a statement runs, so a table declaration
//! and the statements written against it drift apart silently until a cold path
//! executes. Each test below opens a fresh database through the public
//! constructor, runs every statement the crate publishes, and names the failing
//! statement in the panic message.

use chrono::{TimeZone, Utc};
use onlyne_proto::{Body, Causality, LedgerQuery, MsgKind, Principal, new_envelope};
use onlyne_session::{FaultRecord, SessionLedger, VersionedSession};
use onlyne_store::{
    Append, ClientStore, FaultQuery, LedgerRow, ServerFaultRow, ServerLedger, SessionWrite,
};

/// Fail with the failing statement in the message: a column named by a query
/// and absent from the declaration surfaces here.
trait OrDescribe<T> {
    fn describe(self, statement: &'static str) -> T;
}

impl<T> OrDescribe<T> for Result<T, onlyne_store::StoreError> {
    fn describe(self, statement: &'static str) -> T {
        self.unwrap_or_else(|error| panic!("no such column while running: {statement}: {error}"))
    }
}

impl<T> OrDescribe<T> for anyhow::Result<T> {
    fn describe(self, statement: &'static str) -> T {
        self.unwrap_or_else(|error| panic!("no such column while running: {statement}: {error}"))
    }
}

fn at(offset: i64) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 10, 12, 0, 0).unwrap() + chrono::Duration::seconds(offset)
}

fn envelope(kind: MsgKind, text: &str, op_id: &str) -> onlyne_proto::Envelope {
    let causality = (kind != MsgKind::Note)
        .then(|| Causality::root("00000000-0000-4000-8000-000000000001".to_string()));
    let mut env = new_envelope(
        kind,
        Principal::role("alice"),
        Principal::role("worker"),
        Body::text(text),
        causality,
    )
    .unwrap();
    env.op_id = Some(op_id.to_string());
    env.ts = at(0);
    env
}

#[test]
fn every_client_statement_runs_against_the_client_schema() {
    let dir = tempfile::tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let task_id = "00000000-0000-4000-8000-0000000000aa";
    let env = envelope(
        MsgKind::Task,
        "hello",
        "o-11111111-1111-4111-8111-111111111111",
    );
    let env_json = serde_json::to_value(&env).unwrap();
    let op_id = env.op_id.clone().unwrap();

    store
        .enqueue_intent(&op_id, &env_json)
        .describe("INSERT INTO intents(...)");
    store
        .due_intents(at(1), 10)
        .describe("SELECT ... FROM intents WHERE state IN (...)");
    store
        .bump_intent(&op_id, at(30), "retry")
        .describe("UPDATE intents SET state='retrying' ...");
    store
        .pending_intent_count()
        .describe("SELECT COUNT(*) FROM intents");
    store
        .flush_order()
        .describe("SELECT ... FROM intents ORDER BY created_at,rowid");
    store
        .accept_intent(&op_id, &serde_json::json!({"ok": true}))
        .describe("UPDATE intents SET state='accepted' ...");
    store
        .exhaust_intent(&op_id, "ceiling")
        .describe("UPDATE intents SET state='exhausted' ...");
    store
        .put_out_head(task_id, "head text")
        .describe("INSERT INTO out_head_cache ... ON CONFLICT");
    assert_eq!(
        store
            .out_head(task_id)
            .describe("SELECT head FROM out_head_cache"),
        Some("head text".to_string())
    );
    store
        .put_prose("planner", "prose", "hash")
        .describe("INSERT INTO prose_cache ... ON CONFLICT");
    assert!(
        store
            .prose("planner")
            .describe("SELECT prose,spec_hash FROM prose_cache")
            .is_some()
    );
    store
        .put_config("k", "v")
        .describe("INSERT INTO config_cache ... ON CONFLICT");
    assert!(
        store
            .config("k")
            .describe("SELECT value FROM config_cache")
            .is_some()
    );
    store
        .append_event("lifecycle", &serde_json::json!({"n": 1}))
        .describe("INSERT INTO events(type,data_json,created_at)");
    store
        .events_since(0, 10)
        .describe("SELECT seq,type,data_json,created_at FROM events");
    store
        .event_head()
        .describe("SELECT COALESCE(MAX(seq),0) FROM events");

    let version = VersionedSession {
        agent_state: "idle".to_string(),
        delivery_state: "none".to_string(),
        resource_state: "detached".to_string(),
        public_lifecycle: "created".to_string(),
        recovery_substate: "idle_waiting".to_string(),
        desired_json: "{\"desired\":true}".to_string(),
        observed_json: "{\"observed\":true}".to_string(),
        generation: 1,
        seq: 1,
        backend_ref: "{\"backend\":\"fake\",\"task_id\":\"sess-1\"}".to_string(),
        mismatch_count: 0,
        updated_at: 1_789_000_000,
    };
    assert!(
        store
            .upsert_session(task_id, &version)
            .describe("INSERT INTO sessions(...) ... ON CONFLICT(task_id)")
    );
    store
        .get_session(task_id)
        .describe("SELECT ... FROM sessions WHERE task_id=?");
    store
        .task_is_known(task_id)
        .describe("SELECT COUNT(*) FROM sessions WHERE task_id=?");
    store
        .task_attempt(task_id)
        .describe("SELECT env_json,attempt FROM intents");
    let fault = FaultRecord {
        id: 0,
        task_id: task_id.to_string(),
        session_id: "sess-1".to_string(),
        generation: 1,
        seq: 1,
        desired_json: "{}".to_string(),
        observed_json: "{}".to_string(),
        intent: "intent:exhausted".to_string(),
        attempt: 2,
        backend_ref: "{}".to_string(),
        kind: "intent_exhausted".to_string(),
        reason: "ceiling".to_string(),
        state: "open".to_string(),
        created_at: 1_789_000_000,
    };
    store
        .insert_fault(&fault)
        .describe("INSERT INTO faults(...) with attempt");
    let faults = store
        .list_faults(task_id)
        .describe("SELECT ... FROM faults WHERE task_id=? with attempt");
    assert!(
        !faults.is_empty(),
        "the insert above must be visible to the reader"
    );
    store.emit("lifecycle", serde_json::json!({"n": 2}));
    store.note_alert("alert line".to_string());
}

#[test]
fn every_server_statement_runs_against_the_server_schema() {
    let dir = tempfile::tempdir().unwrap();
    let store = ServerLedger::open(dir.path().join("server.db"), 14).unwrap();
    let task_id = "00000000-0000-4000-8000-0000000000bb";
    let session_id = "sess-1";
    let msg_id = "00000000-0000-4000-8000-0000000000cc";

    store
        .upsert_role(&onlyne_store::RoleRow {
            name: "worker".to_string(),
            key: "ed25519/abc".to_string(),
            admin: false,
            max_sessions: 4,
            spec_hash: "hash".to_string(),
            updated_at: "2026-09-10T12:00:00Z".to_string(),
        })
        .describe("INSERT INTO roles(...) ON CONFLICT(name)");
    store
        .list_roles()
        .describe("SELECT name,key,admin,... FROM roles");
    store
        .remove_role_missing_from(&["worker".to_string()])
        .describe("DELETE FROM roles WHERE name NOT IN (...)");
    store
        .project_session(&SessionWrite {
            task_id: task_id.to_string(),
            role: "worker".to_string(),
            session_id: session_id.to_string(),
            generation: 1,
            seq: 1,
            public_lifecycle: "created".to_string(),
            agent_state: "booting".to_string(),
            delivery_state: "none".to_string(),
            resource_state: "detached".to_string(),
            recovery_substate: "none".to_string(),
            desired_json: "{}".to_string(),
            observed_json: "{}".to_string(),
            mismatch_count: 0,
            updated_at: 1_789_000_000,
        })
        .describe("INSERT INTO sessions(...) ON CONFLICT(task_id)");
    store
        .get_session_row(task_id)
        .describe("SELECT ... FROM sessions WHERE task_id=?");
    store
        .list_sessions(Default::default())
        .describe("SELECT ... FROM sessions ORDER BY updated_at");

    let mut env = envelope(
        MsgKind::Task,
        "payload",
        "o-22222222-2222-4222-8222-222222222222",
    );
    env.id = msg_id.to_string();
    let row = LedgerRow::from_envelope(&env, "fp-1").unwrap();
    let appended = store
        .append_ledger(&row)
        .describe("INSERT INTO ledger(...)");
    assert!(matches!(appended, Append::Accepted(_)));
    store
        .ledger_query(LedgerQuery {
            msg_id: Some(msg_id.to_string()),
            limit: 1,
            ..LedgerQuery::default()
        })
        .describe("SELECT ... FROM ledger WHERE msg_id=?");
    store
        .mark_in_flight(msg_id)
        .describe("UPDATE ledger SET state='in_flight' WHERE msg_id=?");
    store
        .in_flight_for("worker")
        .describe("SELECT ... FROM ledger WHERE state='in_flight' AND json_extract");
    store
        .requeue_one(msg_id)
        .describe("UPDATE ledger SET state='queued' WHERE msg_id=?");
    store
        .queued_for("worker", 10)
        .describe("SELECT ... FROM ledger WHERE state='queued' AND json_extract");
    store
        .requeue_in_flight("worker")
        .describe("UPDATE ledger SET state='queued' WHERE state='in_flight'");
    store
        .mark_in_flight(msg_id)
        .describe("UPDATE ledger SET state='in_flight' WHERE msg_id=?");
    store
        .mark_acked(msg_id, at(10))
        .describe("UPDATE ledger SET state=?,acked_at=... WHERE msg_id=?");
    store
        .prune(at(20))
        .describe("UPDATE ledger SET body_json=NULL WHERE state='acked'");

    let mut expirable = LedgerRow::from_envelope(
        &envelope(
            MsgKind::Note,
            "note body",
            "o-44444444-4444-4444-8444-444444444444",
        ),
        "fp-3",
    )
    .unwrap();
    expirable.msg_id = "00000000-0000-4000-8000-0000000000ee".to_string();
    store.append_ledger(&expirable).unwrap();
    store
        .expire_one(&expirable.msg_id, "ttl elapsed")
        .describe("UPDATE ledger SET state='expired',reason=? WHERE msg_id=?");
    let mut sweepable = LedgerRow::from_envelope(
        &envelope(
            MsgKind::Note,
            "second note",
            "o-55555555-5555-4555-8555-555555555555",
        ),
        "fp-4",
    )
    .unwrap();
    sweepable.msg_id = "00000000-0000-4000-8000-0000000000ff".to_string();
    store.append_ledger(&sweepable).unwrap();
    store
        .expire_queued_before(at(30))
        .describe("UPDATE ledger SET state='expired' WHERE state='queued'");

    let mut rejectable = LedgerRow::from_envelope(
        &envelope(
            MsgKind::Task,
            "second",
            "o-33333333-3333-4333-8333-333333333333",
        ),
        "fp-2",
    )
    .unwrap();
    rejectable.msg_id = "00000000-0000-4000-8000-0000000000dd".to_string();
    store.append_ledger(&rejectable).unwrap();
    store
        .mark_rejected(&rejectable.msg_id, "gate")
        .describe("UPDATE ledger SET state='rejected' WHERE msg_id=?");

    store
        .append_event("ledger_state", &serde_json::json!({"n": 1}))
        .describe("INSERT INTO events(type,data_json,created_at)");
    store
        .events_since(0, 10)
        .describe("SELECT seq,type,data_json,created_at FROM events");
    store
        .event_head()
        .describe("SELECT COALESCE(MAX(seq),0) FROM events");

    let fault_id = store
        .record_fault(&ServerFaultRow {
            id: 0,
            task_id: Some(task_id.to_string()),
            role: Some("worker".to_string()),
            session_id: Some(session_id.to_string()),
            generation: Some(1),
            seq: Some(1),
            desired_json: Some("{}".to_string()),
            observed_json: Some("{}".to_string()),
            intent: Some("reconcile:probe_dead".to_string()),
            attempt: Some(1),
            backend_ref: Some("{}".to_string()),
            kind: "probe_dead".to_string(),
            reason: "gone".to_string(),
            state: "open".to_string(),
            created_at: 1_789_000_000,
        })
        .describe("INSERT INTO faults(...)");
    store
        .open_faults()
        .describe("SELECT ... FROM faults WHERE state='open'");
    store
        .faults_query(FaultQuery {
            task_id: Some(task_id.to_string()),
            limit: 10,
            ..FaultQuery::default()
        })
        .describe("SELECT ... FROM faults WHERE task_id=?");
    store
        .faults_query_proto(Default::default())
        .describe("SELECT ... FROM faults ORDER BY id");
    store
        .update_fault_state(task_id, "acked", "operator ack")
        .describe("UPDATE faults SET state=?,reason=? WHERE id=?");
    store
        .ack_fault(fault_id)
        .describe("UPDATE faults SET state='acked' WHERE id=?");
    store
        .cursor_for("worker")
        .describe("SELECT role,last_msg_id,last_seq,updated_at FROM inbox_cursors");
    store
        .set_cursor("worker", Some(msg_id), 1)
        .describe("INSERT INTO inbox_cursors(...) ON CONFLICT(role)");

    assert!(store.event_head().unwrap() >= 1);
}
