//! The ghost sweep settles a mirror row whose own task ledger row already
//! reached a verdict, and records the write in `ghost_sweeps`.

use onlyne_net::KeyPair;
use onlyne_proto::{
    AckArgs, Body, Causality, Lifecycle, MsgKind, Outcome, Principal, Report, SessionProjection,
};
use onlyne_server::relay::{self, RelayReply};
use onlyne_server::state::{Server, ServerInit};
use onlyne_server::{ghosts, projection};
use std::sync::Arc;
use tempfile::{TempDir, tempdir};

const CERT_PIN: &str = "sha256/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

fn spec_text(ghost_sweep_secs: u64) -> String {
    format!(
        r#"[server]
name = "local"
listen = "127.0.0.1:0"
cert_pin = "{CERT_PIN}"
ghost_sweep_secs = {ghost_sweep_secs}

[[client]]
role = "supervisor"
key = "{key}"
allowed_senders = []
allowed_targets = ["builder"]

[[client]]
role = "builder"
key = "{key}"
allowed_senders = ["supervisor"]
allowed_targets = []
"#,
        key = KeyPair::from_seed([7_u8; 32]).public_str()
    )
}

/// One server root holding a spec that names the sweep's interval, opened.
fn open_server(ghost_sweep_secs: u64) -> (TempDir, Arc<onlyne_server::State>) {
    let dir = tempdir().unwrap();
    let root = dir.path().join("server");
    std::fs::create_dir_all(root.join(".onlyne")).unwrap();
    std::fs::write(root.join(".onlyne/spec.toml"), spec_text(ghost_sweep_secs)).unwrap();
    let state = Server::open(&ServerInit { root, listen: None }).unwrap();
    (dir, state)
}

/// Dispatch one task to `builder` and report its session ready.
///
/// The ledger then holds the task's own row, and the mirror holds a row whose
/// stored projection reads `working`. The answer is the task id and the dispatch
/// row's msg id.
fn working_task(state: &Arc<onlyne_server::State>) -> (String, String) {
    let task_id = onlyne_proto::new_task_id();
    let envelope = onlyne_proto::new_envelope(
        MsgKind::Task,
        Principal::role("supervisor"),
        Principal::role("builder"),
        Body::text("build the thing"),
        Some(Causality::root(task_id.clone())),
    )
    .expect("valid task");
    let msg_id = match relay::send(state, &envelope, true, Some("builder")).expect("the send ran") {
        RelayReply::Accepted(outcome) => outcome.receipt.msg_id,
        other => panic!("the dispatch was refused: {other:?}"),
    };
    projection::report(
        state,
        "builder",
        &Report::Ready {
            task_id: task_id.clone(),
            session_id: "sess-ghost".into(),
            generation: 1,
            seq: 1,
            cluster_ref: None,
        },
    )
    .expect("the ready report lands");
    (task_id, msg_id)
}

/// A mirror row that still reads `working` while its own task ledger row reached
/// `acked` is a fossil, and one pass settles it with the verdict that ledger row
/// carries.
#[test]
fn the_sweep_settles_a_working_row_whose_task_already_acked() {
    let (_dir, state) = open_server(60);
    let (task_id, msg_id) = working_task(&state);
    relay::ack(
        &state,
        &AckArgs {
            msg_id,
            op_id: None,
            accepted: true,
            reason: None,
        },
    )
    .expect("the ack ran")
    .expect("the ack settled the ledger row");

    let swept = ghosts::sweep_once(&state).expect("one pass");
    assert_eq!(swept.len(), 1, "the fossil is the row the pass moves");
    assert_eq!(swept[0].task_id, task_id);
    assert_eq!(swept[0].role, "builder");
    assert_eq!(
        swept[0].outcome,
        Outcome::Done,
        "the verdict is read off the acked ledger row"
    );
    assert_eq!(
        swept[0].evidence, "task_settled:acked",
        "the audit row names the ledger state behind the write"
    );
    assert_eq!(swept[0].seq_after, swept[0].seq_before + 1);

    let row = state
        .ledger
        .get_session_row(&task_id)
        .expect("the row reads")
        .expect("the row is present");
    let projected = projection::row_from_write(&row);
    assert_eq!(projected.public_lifecycle, Lifecycle::Exited);
    assert_eq!(projected.outcome, Some(Outcome::Done));
    assert_eq!(row.seq, swept[0].seq_after, "the mirror carries the bump");

    let audit = state.ledger.list_ghost_sweeps(10).expect("the audit reads");
    assert_eq!(audit.len(), 1, "one settlement records one audit row");
    assert_eq!(audit[0].id, swept[0].id);
    assert_eq!(audit[0].outcome, Outcome::Done);
    assert_eq!(audit[0].session_id, "sess-ghost");
}

/// The client owns the task's verdict, and the mirror carries it: a client
/// publishes that verdict with the lifecycle its own tuple reads, and a settled
/// task beside a live agent projects `working`. The pass moves the row out of
/// `working` and the verdict the client published stands.
#[test]
fn the_sweep_keeps_the_verdict_the_client_published() {
    let (_dir, state) = open_server(60);
    let (task_id, msg_id) = working_task(&state);
    relay::ack(
        &state,
        &AckArgs {
            msg_id,
            op_id: None,
            accepted: true,
            reason: None,
        },
    )
    .expect("the ack ran")
    .expect("the ack settled the ledger row");
    let mut published = SessionProjection::default_working();
    published.lifecycle = Lifecycle::Working;
    published.outcome = Some(Outcome::Cancelled);
    projection::report(
        &state,
        "builder",
        &Report::Heartbeat {
            task_id: task_id.clone(),
            session_id: "sess-ghost".into(),
            generation: 1,
            seq: 2,
            observed: serde_json::Value::Null,
            projection: Some(published),
            cluster_ref: None,
        },
    )
    .expect("the publish lands");

    let swept = ghosts::sweep_once(&state).expect("one pass");
    assert_eq!(
        swept.len(),
        1,
        "the row still reads working, so the pass moves it"
    );
    assert_eq!(
        swept[0].outcome,
        Outcome::Cancelled,
        "the audit row names what the mirror finally reads"
    );
    assert_eq!(
        swept[0].evidence, "task_settled:acked",
        "the evidence still names the ledger state behind the pass"
    );

    let row = state
        .ledger
        .get_session_row(&task_id)
        .expect("the row reads")
        .expect("the row is present");
    let projected = projection::row_from_write(&row);
    assert_eq!(
        projected.public_lifecycle,
        Lifecycle::Exited,
        "the pass moves the row out of working"
    );
    assert_eq!(
        projected.outcome,
        Some(Outcome::Cancelled),
        "the verdict the client published stands"
    );
    assert_eq!(
        state.ledger.list_ghost_sweeps(10).expect("the audit reads")[0].outcome,
        Outcome::Cancelled,
        "the stored audit row carries the same verdict"
    );
    assert!(
        ghosts::sweep_once(&state)
            .expect("one more pass")
            .is_empty(),
        "a row the pass moved reads exited, and the pass reads working rows"
    );
}

/// A `working` row whose own task ledger row is still `in_flight` carries no
/// verdict to read, so one pass leaves the mirror row and the audit table alone.
#[test]
fn the_sweep_leaves_a_working_row_whose_task_is_still_open() {
    let (_dir, state) = open_server(60);
    let (task_id, msg_id) = working_task(&state);
    state
        .ledger
        .mark_in_flight(&msg_id)
        .expect("the dispatch row moves to in_flight");

    let swept = ghosts::sweep_once(&state).expect("one pass");
    assert!(
        swept.is_empty(),
        "an open task gives the pass no verdict to write"
    );

    let row = state
        .ledger
        .get_session_row(&task_id)
        .expect("the row reads")
        .expect("the row is present");
    assert_eq!(
        projection::row_from_write(&row).public_lifecycle,
        Lifecycle::Working,
        "the mirror row keeps the projection it stored"
    );
    assert!(
        state
            .ledger
            .list_ghost_sweeps(10)
            .expect("the audit reads")
            .is_empty(),
        "a row the pass did not move records no audit row"
    );
}

/// `[server].ghost_sweep_secs` decides whether the pass runs at all, and `0`
/// leaves the server without it.
#[tokio::test]
async fn a_zero_interval_spawns_no_ghost_sweep() {
    let (_dir, off) = open_server(0);
    assert!(
        onlyne_server::spawn_ghost_sweep(off).is_none(),
        "0 disables the sweep"
    );

    let (_dir, on) = open_server(60);
    let spawned = onlyne_server::spawn_ghost_sweep(on).expect("a nonzero interval spawns the pass");
    spawned.abort();
}

/// `query_ghost_sweeps` answers the rows one pass recorded, under the key the
/// verb names, with each field as the wire carries it.
#[tokio::test]
async fn the_admin_verb_answers_the_rows_one_pass_recorded() {
    let (_dir, state) = open_server(60);
    let (task_id, msg_id) = working_task(&state);
    relay::ack(
        &state,
        &AckArgs {
            msg_id,
            op_id: None,
            accepted: true,
            reason: None,
        },
    )
    .expect("the ack ran")
    .expect("the ack settled the ledger row");
    ghosts::sweep_once(&state).expect("one pass");

    let mut session = onlyne_server::router::Session::default();
    let body = onlyne_server::router::dispatch_admin(
        &state,
        &mut session,
        onlyne_proto::AdminOp::QueryGhostSweeps(10),
    )
    .await;
    assert!(body.ok, "the arm answered ok");
    let data = body.data.expect("the answer carries its rows");
    let rows = data["ghost_sweeps"]
        .as_array()
        .expect("the rows are a list");
    assert_eq!(rows.len(), 1, "the verb names its answer key");
    assert_eq!(rows[0]["task_id"], task_id.as_str());
    assert_eq!(rows[0]["role"], "builder");
    assert_eq!(rows[0]["session_id"], "sess-ghost");
    assert_eq!(rows[0]["outcome"], "done");
    assert_eq!(rows[0]["evidence"], "task_settled:acked");
    assert_eq!(
        rows[0]["seq_after"].as_u64(),
        rows[0]["seq_before"].as_u64().map(|seq| seq + 1),
        "the two versions name the exact write"
    );
}
