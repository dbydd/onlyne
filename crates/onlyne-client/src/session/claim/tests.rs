use super::*;
use crate::backend::fake::FakeBackend;
use crate::session::dispatch::{DispatchState, dispatch, hello_with_live_sessions};
use onlyne_proto::{
    Body, Causality, ClientOp, Envelope, HandshakeArgs, MsgKind, PROTOCOL_VERSION, Principal,
    new_envelope, new_task_id,
};
use onlyne_store::ClientStore;
use onlyne_store::session::{SessionLedger, VersionedSession};
use std::sync::Arc;
use tempfile::tempdir;

fn stored(agent: &str, resource: &str) -> VersionedSession {
    VersionedSession {
        agent_state: agent.into(),
        delivery_state: "none".into(),
        resource_state: resource.into(),
        recovery_substate: "none".into(),
        desired_json: "{}".into(),
        observed_json: "{}".into(),
        generation: 1,
        seq: 1,
        backend_ref: "{}".into(),
        mismatch_count: 0,
        updated_at: 0,
    }
}

fn hello_args(live_sessions: Vec<LiveSession>) -> HandshakeArgs {
    HandshakeArgs {
        protocol: PROTOCOL_VERSION,
        role: "planner".into(),
        key: "ed25519/AAA".into(),
        signature: "sig".into(),
        agent: "onlyne-client".into(),
        version: "1.0.9".into(),
        aggregate: false,
        live_sessions,
    }
}

fn task_envelope() -> Envelope {
    new_envelope(
        MsgKind::Task,
        Principal::role("planner"),
        Principal::role("planner"),
        Body::text("work"),
        Some(Causality::root(new_task_id())),
    )
    .expect("task envelope")
}

#[test]
fn from_sessions_sorts_and_dedups() {
    assert_eq!(
        from_sessions([
            LiveSession {
                session_id: "t-b".into(),
                task_id: Some("t-b".into()),
                suspended: false,
            },
            LiveSession {
                session_id: "t-a".into(),
                task_id: None,
                suspended: true,
            },
            LiveSession {
                session_id: "t-b".into(),
                task_id: None,
                suspended: true,
            },
        ]),
        vec![
            LiveSession {
                session_id: "t-a".into(),
                task_id: None,
                suspended: true,
            },
            LiveSession {
                session_id: "t-b".into(),
                task_id: Some("t-b".into()),
                suspended: false,
            },
        ],
        "one entry per session, the first reading kept, ordered by id"
    );
    assert!(from_sessions(Vec::<LiveSession>::new()).is_empty());
}

#[test]
fn hello_live_sessions_merges_store_rows_with_slots() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    store
        .upsert_session("t-work", &stored("ready", "attached"))
        .unwrap();
    store
        .upsert_session("t-booting", &stored("booting", "attached"))
        .unwrap();
    let dispatch = DispatchState::new(
        "planner",
        dir.path(),
        vec!["echo".into()],
        2,
        Arc::new(FakeBackend::new()),
        store,
    );
    let sessions = dispatch.hello_live_sessions().expect("store answers");
    assert_eq!(
        sessions.len(),
        2,
        "DB rows are merged even when slots are empty: {sessions:?}"
    );
    assert!(
        sessions
            .iter()
            .any(|session| session.session_id == "t-work")
    );
    assert!(
        sessions
            .iter()
            .any(|session| session.session_id == "t-booting")
    );
    let hello = hello_with_live_sessions(&hello_args(Vec::new()), sessions);
    let value = serde_json::to_value(&hello).unwrap();
    let claimed = value["live_sessions"]
        .as_array()
        .expect("live_sessions present");
    assert_eq!(claimed.len(), 2);
}

#[test]
fn hello_live_sessions_only_slot_sessions() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    store
        .upsert_session("t-orphan", &stored("ready", "attached"))
        .unwrap();
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["echo".into()],
        2,
        Arc::new(FakeBackend::new()),
        store,
    );
    let first = task_envelope();
    let first_id = first.task_id().unwrap().to_string();
    dispatch(&state, &first).unwrap();
    let second = task_envelope();
    let second_id = second.task_id().unwrap().to_string();
    dispatch(&state, &second).unwrap();

    let sessions = state.hello_live_sessions().expect("store answers");
    let mut expected = vec![first_id.clone(), second_id.clone(), "t-orphan".to_string()];
    expected.sort();
    let claimed: Vec<String> = sessions
        .iter()
        .map(|session| session.session_id.clone())
        .collect();
    assert_eq!(claimed, expected);
    assert!(
        claimed.contains(&"t-orphan".to_string()),
        "DB row is merged with slots"
    );

    let hello = hello_with_live_sessions(&hello_args(Vec::new()), sessions);
    let frame = serde_json::to_value(ClientOp::Hello(hello)).unwrap();
    let claimed = frame["args"]["live_sessions"]
        .as_array()
        .expect("hello carries live_sessions");
    let names: Vec<&str> = claimed
        .iter()
        .filter_map(|value| value["session_id"].as_str())
        .collect();
    assert_eq!(
        names,
        expected.iter().map(String::as_str).collect::<Vec<_>>()
    );
}

/// The durable half of the claim failing is an answer, not an empty claim.
///
/// A sqlite error used to be swallowed into an empty claim, which told
/// the server this client holds nothing and had every in_flight row requeued.
/// The call must surface the error, and the degraded claim the runloop falls
/// back to must still name the tasks held in memory slots.
#[test]
fn hello_live_sessions_surfaces_store_failure_and_slots_fallback_remembers() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    store
        .upsert_session("t-durable", &stored("ready", "attached"))
        .unwrap();
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["echo".into()],
        2,
        Arc::new(FakeBackend::new()),
        store,
    );
    let task = task_envelope();
    let task_id = task.task_id().unwrap().to_string();
    dispatch(&state, &task).unwrap();

    // Break the durable half the way an io error or a bad migration would:
    // the query's table is gone, so `active_sessions` errors.
    let wreck = rusqlite::Connection::open(dir.path().join("client.db")).unwrap();
    wreck.execute_batch("DROP TABLE sessions").unwrap();
    drop(wreck);

    let error = state
        .hello_live_sessions()
        .expect_err("a broken store must answer with its error, not an empty claim");
    assert!(
        matches!(error, onlyne_store::StoreError::Sqlite(_)),
        "the sqlite failure reaches the caller: {error:?}"
    );
    assert_eq!(
        state.live_claim_from_slots(),
        vec![LiveSession {
            session_id: task_id.clone(),
            task_id: Some(task_id.clone()),
            suspended: false,
        }],
        "the degraded fallback still claims what the memory slots serve"
    );
}
