//! The session persistence bridge over a real client store.
//!
//! Moved out of `onlyne-store`'s own test module when the reducer's bridge became
//! the client's: the store implements the [`SessionLedger`] port and owns the row
//! shape, the driver lives here, and this case is where the two meet — a created
//! and then ready session, read back out of SQLite.

use chrono::{DateTime, Duration, TimeZone, Utc};
use onlyne_client::reconcile::{Bridge, apply_persist, feed_ready};
use onlyne_proto::lifecycle::Version;
use onlyne_proto::{
    Body, Causality, Envelope, Lifecycle, MsgKind, Observation, Principal, new_envelope, project,
};
use onlyne_store::ClientStore;
use onlyne_store::session::{SessionLedger, SessionRecord};
use tempfile::TempDir;

fn temp_db(name: &str) -> (TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    (dir, path)
}

fn fixed_time(offset: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 10, 12, 0, 0).unwrap() + Duration::seconds(offset)
}

fn new_uuid(seed: u8) -> String {
    format!("00000000-0000-4000-8000-{seed:012x}")
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
        &onlyne_proto::lifecycle::LifecycleEvent::Created {
            v: Version::new(1, 1),
        },
    )
    .unwrap();
    let created = store.get_session(&task_id).unwrap().unwrap();
    // The row answers with its tuple, and the public view comes out of
    // `project` beside the task state the caller owns. Nothing here holds a
    // lifecycle to read.
    let created_tuple = project(
        observation(&created).agent,
        observation(&created).delivery,
        observation(&created).resource,
        observation(&created).recovery,
        onlyne_proto::TaskState::Pending,
    );
    assert_eq!(created_tuple, Lifecycle::Created);
    assert_eq!((created.generation, created.seq), (1, 1));
    feed_ready(&bridge, &store, &task_id).unwrap();
    let ready = store.get_session(&task_id).unwrap().unwrap();
    let ready_tuple = observation(&ready);
    assert_eq!(ready_tuple.agent, onlyne_proto::AgentPhase::Ready);
    assert_eq!(
        project(
            ready_tuple.agent,
            ready_tuple.delivery,
            ready_tuple.resource,
            ready_tuple.recovery,
            onlyne_proto::TaskState::Pending,
        ),
        Lifecycle::Idle
    );
    assert_eq!((ready.generation, ready.seq), (1, 2));
    assert!(store.list_faults(&task_id).unwrap().is_empty());
    assert!(store.event_head().unwrap() >= 2);
}

/// Decode the tuple one stored row carries.
fn observation(row: &SessionRecord) -> Observation {
    serde_json::from_str(&row.observed_json).expect("the stored tuple is readable")
}
