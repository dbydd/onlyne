//! The durable outbound intent queue: the key a row is stored under, and what settles
//! a row against the server's answer.

use crate::common::sample_envelope;
use onlyne_client::{
    ops::local_cli::LocalCli,
    runtime::intent::{IntentMachine, IntentResult, op_for_intent, permanent_error},
    session::dispatch::DispatchState,
};
use onlyne_proto::{ClientOp, Envelope, ErrorCode, MsgKind, Receipt, ResBody, new_envelope};
use onlyne_session::backend::fake::FakeBackend;
use onlyne_store::ClientStore;
use std::sync::Arc;
use tempfile::tempdir;

#[test]
fn intent_acl_denial_drops_row_without_retry() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("client.db");
    let store = ClientStore::open(&db).unwrap();
    let machine = IntentMachine::new(store.clone(), 3, vec![1000, 2000]);

    let envelope = sample_envelope("planner", "hello");
    machine.enqueue(&envelope).unwrap();
    assert_eq!(store.pending_intent_count().unwrap(), 1);

    let rows = store.flush_order().unwrap();
    assert_eq!(rows.len(), 1);

    let res = machine
        .attempt(
            &rows[0],
            Some(&ResBody::err(ErrorCode::AclDenied, "acl denied", None)),
        )
        .unwrap();
    assert!(matches!(
        res,
        IntentResult::Dropped(ErrorCode::AclDenied, _)
    ));
    assert_eq!(store.pending_intent_count().unwrap(), 0);
}

#[test]
fn intent_idempotent_duplicate_settles_accepted() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("client.db");
    let store = ClientStore::open(&db).unwrap();
    let machine = IntentMachine::new(store.clone(), 3, vec![1000, 2000]);

    let envelope = sample_envelope("planner", "hello");
    machine.enqueue(&envelope).unwrap();
    let rows = store.flush_order().unwrap();

    let receipt = Receipt {
        msg_id: "msg-1".into(),
        op_id: envelope.op_id.clone(),
        kind: MsgKind::Task,
        task: Some("task-1".into()),
        state: onlyne_proto::LedgerState::Queued,
        enqueued_at: chrono::Utc::now(),
    };
    let body = ResBody::err_with_data(
        ErrorCode::Duplicate,
        "duplicate",
        None,
        serde_json::to_value(&receipt).unwrap(),
    );

    let res = machine.attempt(&rows[0], Some(&body)).unwrap();
    assert!(matches!(res, IntentResult::Accepted(Some(r)) if r.msg_id == "msg-1"));
    assert_eq!(store.pending_intent_count().unwrap(), 0);
}

#[test]
fn intent_exhaustion_drives_fault_after_ceiling() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("client.db");
    let store = ClientStore::open(&db).unwrap();
    let machine = IntentMachine::new(store.clone(), 3, vec![100, 200]);

    let envelope = sample_envelope("planner", "hello");
    machine.enqueue(&envelope).unwrap();
    let rows = store.flush_order().unwrap();
    let row = &rows[0];

    // Attempt 1: internal -> Retryable
    let res1 = machine
        .attempt(
            row,
            Some(&ResBody::err(ErrorCode::Internal, "internal error", None)),
        )
        .unwrap();
    assert!(matches!(
        res1,
        IntentResult::Retryable(ErrorCode::Internal, _)
    ));

    // Update row attempt to 2
    let rows = store.flush_order().unwrap();
    let res2 = machine
        .attempt(
            &rows[0],
            Some(&ResBody::err(ErrorCode::Internal, "internal error", None)),
        )
        .unwrap();
    assert!(matches!(
        res2,
        IntentResult::Retryable(ErrorCode::Internal, _)
    ));

    // Attempt 3 reaches attempts ceiling (3) -> Exhausted
    let rows = store.flush_order().unwrap();
    let res3 = machine
        .attempt(
            &rows[0],
            Some(&ResBody::err(ErrorCode::Internal, "internal error", None)),
        )
        .unwrap();
    assert!(matches!(res3, IntentResult::Exhausted));
    assert_eq!(store.pending_intent_count().unwrap(), 0);

    let task_id = envelope.task_id().unwrap();
    let faults = onlyne_session::SessionLedger::list_faults(&store, task_id).unwrap();
    assert_eq!(faults.len(), 1);
    assert_eq!(faults[0].kind, "intent_exhausted");
}

/// Build a note the way the pi plugin's `protocol.mjs` does: the conformance
/// vector `adapter_plugin_send_note.json` pins an `op_id`-less note, so the
/// envelope leaves the field unset rather than the envelope layer minting one.
fn note_envelope(role: &str, text: &str) -> Envelope {
    let mut envelope = new_envelope(
        MsgKind::Note,
        onlyne_proto::Principal::role("planner"),
        onlyne_proto::Principal::role(role),
        onlyne_proto::Body::text(text),
        None,
    )
    .unwrap();
    envelope.op_id = None;
    envelope
}

/// Whether one id is the shape `new_op_id` mints: the proto prefix and a bare
/// uuid v4, which is the `/^o-[0-9a-f-]{36}$/` the field report asked for.
fn is_minted_op_id(op_id: &str) -> bool {
    let Some(rest) = op_id.strip_prefix("o-") else {
        return false;
    };
    rest.len() == 36
        && rest
            .chars()
            .all(|ch| ch.is_ascii_digit() || ('a'..='f').contains(&ch) || ch == '-')
}

#[test]
fn a_note_without_a_key_gets_one_the_row_stores_and_replays() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["agent".into()],
        1,
        Arc::new(FakeBackend::new()),
        store.clone(),
    );

    let note = note_envelope("writer", "batch 12 finished");
    let op_id = state.enqueue_outbound(&note).unwrap();
    assert!(
        is_minted_op_id(&op_id),
        "a note without a key gets a minted one: {op_id}"
    );
    assert!(
        note.op_id.is_none(),
        "the plugin's own envelope is untouched"
    );

    let rows = store.flush_order().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].op_id, op_id);
    assert_eq!(rows[0].env_json["op_id"].as_str(), Some(op_id.as_str()));

    let ClientOp::Send(replayed) = op_for_intent(&rows[0]).unwrap() else {
        panic!("a stored note replays as a send");
    };
    assert_eq!(replayed.op_id.as_deref(), Some(op_id.as_str()));
    assert!(
        replayed.validate().is_ok(),
        "a note carrying a stamped key is a legal frame"
    );
}

#[test]
fn a_task_envelope_keeps_the_key_it_brought() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["agent".into()],
        1,
        Arc::new(FakeBackend::new()),
        store.clone(),
    );

    let task = sample_envelope("writer", "do the thing");
    let brought = task.op_id.clone().expect("a task mints its own key");
    assert_eq!(state.enqueue_outbound(&task).unwrap(), brought);
    assert_eq!(store.flush_order().unwrap()[0].op_id, brought);
}

#[test]
fn two_notes_queue_under_two_distinct_keys() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["agent".into()],
        1,
        Arc::new(FakeBackend::new()),
        store.clone(),
    );

    let first = state
        .enqueue_outbound(&note_envelope("writer", "note one"))
        .unwrap();
    let second = state
        .enqueue_outbound(&note_envelope("writer", "note two"))
        .unwrap();
    assert_ne!(first, second, "notes are never deduped");

    let rows = store.flush_order().unwrap();
    assert_eq!(rows.len(), 2);
    for row in &rows {
        assert_eq!(
            row.env_json["op_id"].as_str(),
            Some(row.op_id.as_str()),
            "every row is keyed by the id inside its stored envelope"
        );
    }
    assert!(rows.iter().any(|row| row.op_id == first));
    assert!(rows.iter().any(|row| row.op_id == second));
}

#[test]
fn the_offline_reply_reports_the_key_the_row_stores() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let cli = LocalCli::with_role(IntentMachine::new(store.clone(), 3, vec![100]), "planner");

    let note = note_envelope("writer", "note over the plugin socket");
    let body = cli.offline_send(&note).unwrap();
    let reported = body.data.unwrap()["op_id"]
        .as_str()
        .expect("the reply names the stored key")
        .to_string();
    assert!(is_minted_op_id(&reported), "reported: {reported}");
    assert_eq!(store.flush_order().unwrap()[0].op_id, reported);
    assert!(
        note.op_id.is_none(),
        "the plugin's own envelope is untouched"
    );
}

#[test]
fn permanent_versus_retryable_error_split() {
    assert!(permanent_error(ErrorCode::AclDenied));
    assert!(permanent_error(ErrorCode::Conflict));
    assert!(permanent_error(ErrorCode::Forbidden));
    assert!(permanent_error(ErrorCode::UnknownRole));
    assert!(permanent_error(ErrorCode::NotAdmin));
    assert!(permanent_error(ErrorCode::BadFrame));
    assert!(permanent_error(ErrorCode::FrameTooLarge));
    assert!(permanent_error(ErrorCode::ProtocolVersion));

    assert!(!permanent_error(ErrorCode::Internal));
    assert!(!permanent_error(ErrorCode::Duplicate));
    assert!(!permanent_error(ErrorCode::RecipientOffline));
    // The server answers `invalid` for conditions a retry clears, such as a frame
    // that arrives before the routed `hello`, so the row ends as an observable
    // fault rather than a silent deletion.
    assert!(!permanent_error(ErrorCode::Invalid));
}

#[test]
fn intent_survives_a_frame_refused_before_the_routed_hello() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("client.db");
    let store = ClientStore::open(&db).unwrap();
    let machine = IntentMachine::new(store.clone(), 3, vec![1000, 2000]);

    let envelope = sample_envelope("planner", "hello");
    machine.enqueue(&envelope).unwrap();
    let rows = store.flush_order().unwrap();

    // The link layer redials on its own, so the first frame of a fresh
    // connection can reach a session that has not read `hello` yet.
    let res = machine
        .attempt(
            &rows[0],
            Some(&ResBody::err(
                ErrorCode::Invalid,
                onlyne_proto::HELLO_REQUIRED_MESSAGE,
                Some("op".to_string()),
            )),
        )
        .unwrap();
    assert!(matches!(
        res,
        IntentResult::Retryable(ErrorCode::Internal, _)
    ));
    assert_eq!(store.pending_intent_count().unwrap(), 1);
    let kept = store.flush_order().unwrap();
    assert_eq!(kept[0].attempt, rows[0].attempt);
}
