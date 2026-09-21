//! Settling a task: the close reason a cancelled session reports, the detached tuple
//! that sees no close call, the ack a rejected assign queues, and the cluster a report
//! names.

use crate::common::{ReasonBackend, sample_envelope};
use onlyne_client::{
    runtime::intent::op_for_intent,
    session::dispatch::{DispatchState, dispatch, on_recycled},
};
use onlyne_proto::{AssignAckArgs, ClientOp};
use onlyne_session::SessionLedger;
use onlyne_session::backend::fake::FakeBackend;
use onlyne_store::ClientStore;
use std::sync::Arc;
use tempfile::tempdir;

#[test]
fn assign_ack_rejection_queues_a_rejected_delivery_ack() {
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
    let env = sample_envelope("planner", "reject me");
    let task_id = env.task_id().unwrap().to_string();
    dispatch(&state, &env).unwrap();
    state.attach_msg_id(&task_id, "msg-reject");

    assert!(state.push_assign_ack(AssignAckArgs {
        task_id: task_id.clone(),
        accepted: false,
        reason: Some("already injected conflict".into()),
    }));
    let rows = store.flush_order().unwrap();
    assert_eq!(rows.len(), 1);
    let op = op_for_intent(&rows[0]).unwrap();
    let ClientOp::Ack(ack) = op else {
        panic!("assign rejection must queue a delivery ack, got {op:?}");
    };
    assert_eq!(ack.msg_id, "msg-reject");
    assert!(!ack.accepted);
    assert_eq!(ack.reason.as_deref(), Some("already injected conflict"));

    assert!(!state.push_assign_ack(AssignAckArgs {
        task_id,
        accepted: true,
        reason: None,
    }));
    assert_eq!(
        store.flush_order().unwrap().len(),
        1,
        "accepted assign_ack is still non-terminal"
    );
}

#[test]
fn cancelled_settle_closes_with_the_real_reason() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(ReasonBackend::default());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["echo".into()],
        2,
        backend.clone(),
        store,
    );

    let env = sample_envelope("planner", "task 1");
    let task_id = env.task_id().unwrap().to_string();
    dispatch(&state, &env).unwrap();
    on_recycled(&state, &task_id, onlyne_session::CloseReason::Cancelled).unwrap();

    assert_eq!(
        backend.reasons.lock().as_slice(),
        [onlyne_session::CloseReason::Cancelled]
    );
    assert_eq!(state.session_count(), 0);
}

#[test]
fn detached_tuple_sees_no_close_call() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(ReasonBackend::default());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["echo".into()],
        2,
        backend.clone(),
        store.clone(),
    );

    let env = sample_envelope("planner", "task 1");
    let task_id = env.task_id().unwrap().to_string();
    dispatch(&state, &env).unwrap();

    // Rewind the stored tuple to Detached at a higher watermark, which is the
    // state a probe-confirmed loss leaves behind.
    let row = store.get_session(&task_id).unwrap().unwrap();
    store
        .upsert_session(
            &task_id,
            &onlyne_session::VersionedSession {
                agent_state: row.agent_state.clone(),
                delivery_state: row.delivery_state.clone(),
                resource_state: "detached".to_string(),
                recovery_substate: row.recovery_substate.clone(),
                desired_json: row.desired_json.clone(),
                observed_json: row.observed_json.clone(),
                generation: row.generation,
                seq: row.seq + 1,
                backend_ref: row.backend_ref.clone(),
                mismatch_count: row.mismatch_count,
                updated_at: row.updated_at,
            },
        )
        .unwrap();

    on_recycled(&state, &task_id, onlyne_session::CloseReason::Completed).unwrap();

    assert!(
        backend.reasons.lock().is_empty(),
        "a detached tuple has no resource to close"
    );
    assert_eq!(state.session_count(), 0);
}

#[test]
fn a_supervisor_report_names_its_cluster() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec![],
        1,
        Arc::new(FakeBackend::new()),
        store,
    );
    state.set_cluster_ref("cluster-b");

    let report = onlyne_proto::Report::Ready {
        task_id: "t".into(),
        session_id: "t".into(),
        generation: 1,
        seq: 1,
        cluster_ref: None,
    };
    let value = serde_json::to_value(onlyne_client::session::dispatch::with_cluster(
        &state, report,
    ))
    .unwrap();
    assert_eq!(value["data"]["cluster_ref"], "cluster-b");
}

#[test]
fn a_plain_role_report_omits_the_cluster_key() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec![],
        1,
        Arc::new(FakeBackend::new()),
        store,
    );

    let report = onlyne_proto::Report::Ready {
        task_id: "t".into(),
        session_id: "t".into(),
        generation: 1,
        seq: 1,
        cluster_ref: None,
    };
    let value = serde_json::to_value(onlyne_client::session::dispatch::with_cluster(
        &state, report,
    ))
    .unwrap();
    assert!(
        value["data"].get("cluster_ref").is_none(),
        "a plain role omits the key: {value}"
    );
}
