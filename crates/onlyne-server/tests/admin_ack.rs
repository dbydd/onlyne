//! Admin `ack`: the supervisor settles a delivery over the server's own socket
//! once the role that received it no longer has one.
//!
//! A dead role client leaves its rows `in_flight` and no socket to settle them
//! on, which is the state `onlyne reject` used to report as `Connection refused`
//! before `AdminOp::Ack` existed. Each case drives `onlyne_server::router::
//! dispatch_admin`, the body the admin socket runs, and reads the ledger back,
//! so a dispatch that answered `ok` without moving the row is a red case.

use chrono::Utc;
use onlyne_config::layout::ServerRoot;
use onlyne_proto::{
    AckArgs, AdminOp, Body, Causality, Envelope, LedgerQuery, MsgKind, Principal, PullArgs,
};
use onlyne_server::relay::{self, pull};
use onlyne_server::router::{self, Session};
use onlyne_server::state::{RoleConnection, State};
use onlyne_server::{Server, ServerInit};
use onlyne_store::LedgerState;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::mpsc;

const MASTER: &str = "master";
const WORKER: &str = "worker";

/// A server over a temporary root: the spec, the key material, and the store
/// live as they do under `onlyne-server run`; only the listeners are absent.
fn server() -> (TempDir, Arc<State>) {
    let dir = tempfile::tempdir().expect("temporary root");
    let root = dir.path().join("server");
    let layout = ServerRoot::resolve(&root);
    layout.bootstrap().expect("bootstrap the server root");
    let key = "ed25519/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    let pin = format!("sha256/{}", "0".repeat(64));
    std::fs::write(
        layout.spec_path(),
        format!(
            "[server]\n\
             name = \"admin-ack\"\n\
             listen = \"127.0.0.1:0\"\n\
             cert_pin = {pin:?}\n\
             requeue_max_attempts = 0\n\
             requeue_ttl_secs = 0\n\
             \n\
             [[client]]\n\
             role = {MASTER:?}\n\
             key = {key:?}\n\
             allowed_targets = [{WORKER:?}]\n\
             allowed_senders = [{WORKER:?}]\n\
             \n\
             [[client]]\n\
             role = {WORKER:?}\n\
             key = {key:?}\n\
             allowed_targets = [{MASTER:?}]\n\
             allowed_senders = [{MASTER:?}]\n"
        ),
    )
    .expect("write spec.toml");
    let state = Server::open(&ServerInit { root, listen: None }).expect("open the server state");
    (dir, state)
}

fn connect(state: &Arc<State>, role: &str) {
    let (sender, _receiver) = mpsc::channel(64);
    state.register_role(RoleConnection {
        role: role.to_string(),
        direct_notes: false,
        sender,
        last_seq: 0,
        connected_at: Utc::now(),
        draining: false,
        generation: 0,
    });
}

fn envelope(from: &str, to: &str, task: &str) -> Envelope {
    onlyne_proto::new_envelope(
        MsgKind::Task,
        Principal::role(from),
        Principal::role(to),
        Body::text("carry this"),
        Some(Causality::root(task)),
    )
    .expect("valid envelope")
}

/// Master dispatches one task to worker, and worker's pull leaves the row
/// `in_flight` — the state a client that then dies leaves behind.
fn in_flight(state: &Arc<State>) -> String {
    let dispatch = envelope(MASTER, WORKER, &onlyne_proto::new_task_id());
    match relay::send(state, &dispatch, false, None).expect("send") {
        relay::RelayReply::Accepted(_) => {}
        relay::RelayReply::Duplicate(_) => panic!("the dispatch was sent twice"),
        relay::RelayReply::Rejected(reject) => {
            panic!("dispatch refused: {} {}", reject.code, reject.message)
        }
    }
    let pulled = pull(state, WORKER, None, &PullArgs::default()).expect("worker pulls");
    assert_eq!(pulled.deliveries[0].msg_id, dispatch.id, "worker takes it");
    dispatch.id
}

fn row_state(state: &Arc<State>, msg_id: &str) -> (LedgerState, Option<String>) {
    let row = state
        .ledger
        .ledger_query(LedgerQuery {
            msg_id: Some(msg_id.to_string()),
            limit: 1,
            ..LedgerQuery::default()
        })
        .expect("ledger query")
        .into_iter()
        .next()
        .expect("the row exists");
    (row.state, row.reason)
}

fn ack(msg_id: &str, accepted: bool, reason: &str) -> AckArgs {
    AckArgs {
        msg_id: msg_id.to_string(),
        op_id: None,
        accepted,
        reason: Some(reason.to_string()),
    }
}

/// The `reject` case: the operator's refusal settles the row the dead worker
/// still held, and the reason reaches the ledger.
#[tokio::test]
async fn admin_reject_settles_a_dead_clients_delivery() {
    let (_root, state) = server();
    connect(&state, MASTER);
    connect(&state, WORKER);
    let msg_id = in_flight(&state);

    // The client is gone: no socket, no link, and the row still in flight.
    state.unregister_role(WORKER);
    assert_eq!(row_state(&state, &msg_id).0, LedgerState::InFlight);

    let mut session = Session::default();
    let body = router::dispatch_admin(
        &state,
        &mut session,
        AdminOp::Ack(ack(&msg_id, false, "role client exited")),
    )
    .await;
    assert!(body.ok, "admin reject answers ok: {body:?}");
    let answered = body.data.as_ref().expect("the settled event");
    assert_eq!(answered["msg_id"], msg_id.as_str());
    assert_eq!(answered["state"], "rejected");

    let (settled, reason) = row_state(&state, &msg_id);
    assert_eq!(settled, LedgerState::Rejected);
    assert_eq!(reason.as_deref(), Some("role client exited"));
    assert!(
        state.delivery_ticket(&msg_id).is_none(),
        "a settled row keeps no ticket"
    );
}

/// The `ack` case: acceptance settles the same way, and the operator's reason
/// rides the row.
#[tokio::test]
async fn admin_ack_settles_a_dead_clients_delivery() {
    let (_root, state) = server();
    connect(&state, MASTER);
    connect(&state, WORKER);
    let msg_id = in_flight(&state);
    state.unregister_role(WORKER);

    let mut session = Session::default();
    let body = router::dispatch_admin(
        &state,
        &mut session,
        AdminOp::Ack(ack(&msg_id, true, "settled by the supervisor")),
    )
    .await;
    assert!(body.ok, "admin ack answers ok: {body:?}");
    assert_eq!(
        body.data.as_ref().expect("the settled event")["state"],
        "acked"
    );

    let (settled, reason) = row_state(&state, &msg_id);
    assert_eq!(settled, LedgerState::Acked);
    assert_eq!(reason.as_deref(), Some("settled by the supervisor"));
}

/// An id no row carries is refused with the same `invalid` a client ack earns,
/// and writes nothing.
#[tokio::test]
async fn admin_ack_refuses_an_unknown_id() {
    let (_root, state) = server();
    let mut session = Session::default();
    let body = router::dispatch_admin(
        &state,
        &mut session,
        AdminOp::Ack(ack("no-such-msg", false, "nothing to settle")),
    )
    .await;
    assert!(!body.ok, "an unknown id is refused: {body:?}");
    let error = body.error.as_ref().expect("the refusal");
    assert_eq!(error.code, onlyne_proto::ErrorCode::Invalid);
}

/// The op spells itself the way the CLI sends it.
#[test]
fn admin_ack_names_itself() {
    let op = AdminOp::Ack(ack("m1", true, "reason"));
    assert_eq!(op.name(), "ack");
    assert!(!op.is_readonly());
}
