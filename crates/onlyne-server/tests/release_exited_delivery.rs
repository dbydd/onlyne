//! Released task completion: a session's `exited` write releases its own
//! delivery and never a completion bound for somebody else.
//!
//! Each case seeds a real `Server` over a temporary root and drives the path
//! production runs: `relay::send` writes the rows, `relay::pull` arms the
//! delivery tickets, the settled task's verdict is published through
//! `projection::report`, and the `exited` heartbeat that follows goes through
//! `projection::report` too — which is what calls
//! `relay::release_exited_delivery`. Assertions read the ledger and the tickets
//! back, so a release that requeues a completion, or that refuses a row it does
//! not hold, is a red case rather than a passing no-op.

use chrono::Utc;
use onlyne_config::layout::ServerRoot;
use onlyne_proto::{
    AckArgs, AgentPhase, Body, Causality, DeliveryPhase, Envelope, Lifecycle, MsgKind, Outcome,
    Principal, PullArgs, RecoveryPhase, Report, ResourcePhase, SessionProjection,
};
use onlyne_server::relay::{RELEASE_SETTLED_REASON, pull};
use onlyne_server::state::{RoleConnection, State};
use onlyne_server::{Server, ServerInit, projection, relay};
use onlyne_store::LedgerState;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::mpsc;

const MASTER: &str = "master";
const WORKER: &str = "worker";
/// The pooled session a role's client holds while it serves deliveries; it is
/// a different value from any task id.
const POOL: &str = "pool-1";
/// A second session of the same role, which holds a delivery of its own.
const OTHER: &str = "pool-2";

/// A server over a temporary root, with two roles that may address each other.
///
/// The root is `tempfile`'s, so the spec, the key material, and the store live
/// exactly as they do under `onlyne-server run`; only the listeners are absent.
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
             name = \"release-regression\"\n\
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

/// The role's connection channel, so `send` counts the role online.
fn connect(state: &Arc<State>, role: &str) {
    let (sender, _receiver) = mpsc::channel(64);
    state.register_role(RoleConnection {
        role: role.to_string(),
        sender,
        last_seq: 0,
        connected_at: Utc::now(),
        draining: false,
        generation: 0,
    });
}

fn envelope(kind: MsgKind, from: &str, to: &str, task: &str, text: &str) -> Envelope {
    let causality =
        matches!(kind, MsgKind::Task | MsgKind::Completion).then(|| Causality::root(task));
    onlyne_proto::new_envelope(
        kind,
        Principal::role(from),
        Principal::role(to),
        Body::text(text),
        causality,
    )
    .expect("valid envelope")
}

/// One send through the production path; a refusal fails the case by its word.
fn send(state: &Arc<State>, envelope: &Envelope) {
    match relay::send(state, envelope, false, None).expect("send") {
        relay::RelayReply::Accepted(_) => {}
        relay::RelayReply::Duplicate(_) => panic!("{} was sent twice", envelope.kind),
        relay::RelayReply::Rejected(reject) => {
            panic!(
                "{} send refused: {} {}",
                envelope.kind, reject.code, reject.message
            )
        }
    }
}

fn row_state(state: &Arc<State>, msg_id: &str) -> (Option<LedgerState>, Option<String>) {
    let row = state
        .ledger
        .ledger_query(onlyne_proto::LedgerQuery {
            msg_id: Some(msg_id.to_string()),
            limit: 1,
            ..onlyne_proto::LedgerQuery::default()
        })
        .expect("ledger query")
        .into_iter()
        .next()
        .expect("the row exists");
    (Some(row.state), row.reason)
}

fn ticket(state: &Arc<State>, msg_id: &str) -> Option<(String, Option<String>)> {
    state
        .delivery_ticket(msg_id)
        .map(|ticket| (ticket.role, ticket.session_id))
}

/// The `exited` write a session publishes when its pane dies without a verdict.
///
/// `(generation, seq)` is the write's version on the session row, so a second
/// report about one session names the next `seq` and is not refused as stale.
fn exited(task_id: &str, session_id: &str, seq: u64, outcome: Option<Outcome>) -> Report {
    Report::Heartbeat {
        task_id: task_id.to_string(),
        session_id: session_id.to_string(),
        generation: 1,
        seq,
        observed: serde_json::json!({}),
        projection: Some(SessionProjection {
            lifecycle: Lifecycle::Exited,
            agent: AgentPhase::Gone,
            delivery: DeliveryPhase::NoIntent,
            resource: ResourcePhase::Closed,
            recovery: RecoveryPhase::NoRecovery,
            outcome,
            observed: None,
        }),
        cluster_ref: None,
    }
}

struct Seeded {
    dispatch: String,
    completion: String,
}

/// Master dispatches one task to worker; worker holds it in flight and answers
/// with a completion; master pulls that completion into a ticket of its own.
fn seed(state: &Arc<State>, task: &str) -> Seeded {
    let dispatch = envelope(MsgKind::Task, MASTER, WORKER, task, "carry this");
    send(state, &dispatch);

    let pulled = pull(state, WORKER, None, &PullArgs::default()).expect("worker pulls the task");
    assert_eq!(pulled.deliveries.len(), 1, "worker is handed the dispatch");
    assert_eq!(pulled.deliveries[0].msg_id, dispatch.id);
    assert_eq!(
        ticket(state, &dispatch.id),
        Some((WORKER.to_string(), None)),
        "a role-level pull arms a ticket with no session"
    );

    let completion = envelope(MsgKind::Completion, WORKER, MASTER, task, "done");
    send(state, &completion);
    let pulled =
        pull(state, MASTER, None, &PullArgs::default()).expect("master pulls the completion");
    assert_eq!(
        pulled.deliveries.len(),
        1,
        "master is handed the completion"
    );
    assert_eq!(pulled.deliveries[0].msg_id, completion.id);
    assert_eq!(
        ticket(state, &completion.id),
        Some((MASTER.to_string(), None)),
        "a completion is handed to its recipient with a role-level ticket"
    );

    assert_eq!(
        row_state(state, &dispatch.id).0,
        Some(LedgerState::InFlight)
    );
    assert_eq!(
        row_state(state, &completion.id).0,
        Some(LedgerState::InFlight)
    );

    Seeded {
        dispatch: dispatch.id,
        completion: completion.id,
    }
}

/// A verdict on the applied `exited` write settles the delivery it was earned
/// by: the task row reads `acked`, nothing goes back on the queue, and the
/// completion keeps its recipient's ticket.
///
/// The seed is the race this covers: master dispatches one task to worker,
/// worker answers with a completion master holds, and the pooled session that
/// served the dispatch publishes `exited` after its task already settled. The
/// pooled session id is not the task id.
#[test]
fn settled_task_acks_its_in_flight_dispatch_row() {
    let (_root, state) = server();
    connect(&state, MASTER);
    connect(&state, WORKER);
    let task = onlyne_proto::new_task_id();
    let seeded = seed(&state, &task);

    let completed = projection::report(
        &state,
        WORKER,
        &Report::Complete {
            task_id: task.clone(),
            outcome: Outcome::Done,
            head: Some("done".to_string()),
            details: None,
            files: Vec::new(),
            reply_to: Some(MASTER.to_string()),
            cluster_ref: None,
        },
    )
    .expect("worker completes its task");
    assert!(completed.applied);
    assert_eq!(
        completed.row.as_ref().and_then(|row| row.outcome),
        Some(Outcome::Done)
    );

    // The applied write's own verdict is what settles the row: the client's
    // mirror already carries the outcome its completion published.
    let released = projection::report(&state, WORKER, &exited(&task, POOL, 2, Some(Outcome::Done)))
        .expect("exited write");
    assert!(released.applied, "the exited write lands on the pooled row");

    let (row, reason) = row_state(&state, &seeded.dispatch);
    assert_eq!(
        row,
        Some(LedgerState::Acked),
        "the delivery that produced the verdict is acknowledged, not refused"
    );
    assert_eq!(reason.as_deref(), Some(RELEASE_SETTLED_REASON));
    assert_eq!(
        ticket(&state, &seeded.dispatch),
        None,
        "the settled row keeps no ticket"
    );
    assert_eq!(
        state
            .ledger
            .queued_count_for(WORKER)
            .expect("queued rows of worker"),
        0,
        "a settled row is never offered again"
    );

    // The completion is the recipient's, not the dying session's: it keeps its
    // state and its ticket, and the recipient still ack it.
    assert_eq!(
        row_state(&state, &seeded.completion).0,
        Some(LedgerState::InFlight)
    );
    assert_eq!(
        ticket(&state, &seeded.completion),
        Some((MASTER.to_string(), None))
    );
    let pulled = pull(&state, MASTER, None, &PullArgs::default()).expect("master pulls again");
    assert!(
        pulled.deliveries.is_empty(),
        "master already holds the completion: {:?}",
        pulled.deliveries
    );
    relay::ack(
        &state,
        &AckArgs {
            msg_id: seeded.completion.clone(),
            op_id: None,
            accepted: true,
            reason: None,
        },
    )
    .expect("ack call")
    .expect("master accepts the completion");
    assert_eq!(
        row_state(&state, &seeded.completion).0,
        Some(LedgerState::Acked)
    );
}

/// A crash (no verdict) returns the session's own in-flight row to the queue
/// and leaves the completion its recipient holds exactly where it is — the
/// completion keeps its ticket and is still acked by the role that holds it.
#[test]
fn crash_requeues_its_delivery_and_keeps_the_completion() {
    let (_root, state) = server();
    connect(&state, MASTER);
    connect(&state, WORKER);
    let task = onlyne_proto::new_task_id();
    let seeded = seed(&state, &task);

    let released =
        projection::report(&state, WORKER, &exited(&task, POOL, 1, None)).expect("exited write");
    assert!(released.applied);

    // The completion belongs to master: same state, same ticket.
    assert_eq!(
        row_state(&state, &seeded.completion).0,
        Some(LedgerState::InFlight),
        "the task's completion is never released by the worker's exit"
    );
    assert_eq!(
        ticket(&state, &seeded.completion),
        Some((MASTER.to_string(), None)),
        "the completion keeps the ticket it was handed with"
    );

    // The worker's own dispatch row goes back on its queue.
    assert_eq!(
        row_state(&state, &seeded.dispatch).0,
        Some(LedgerState::Queued),
        "the crashed session's delivery is requeued"
    );
    assert_eq!(
        ticket(&state, &seeded.dispatch),
        None,
        "the released row keeps no ticket"
    );

    // Master still holds the completion: it pulls nothing new for it, and the
    // delivery it already holds acks as accepted.
    let pulled = pull(&state, MASTER, None, &PullArgs::default()).expect("master pulls again");
    assert!(
        pulled.deliveries.is_empty(),
        "master already holds its delivery: {:?}",
        pulled.deliveries
    );
    relay::ack(
        &state,
        &AckArgs {
            msg_id: seeded.completion.clone(),
            op_id: None,
            accepted: true,
            reason: None,
        },
    )
    .expect("ack call")
    .expect("master accepts the completion");
    assert_eq!(
        row_state(&state, &seeded.completion).0,
        Some(LedgerState::Acked)
    );
}

/// A row another session holds stays handed to that session, and a row another
/// role holds is not released by this role's exit even on the same task.
#[test]
fn another_holder_keeps_its_row() {
    let (_root, state) = server();
    connect(&state, MASTER);
    connect(&state, WORKER);
    let task = onlyne_proto::new_task_id();

    let dispatch = envelope(MsgKind::Task, MASTER, WORKER, &task, "held elsewhere");
    send(&state, &dispatch);
    let pulled = pull(&state, WORKER, Some(OTHER), &PullArgs::default()).expect("named pull");
    assert_eq!(pulled.deliveries[0].msg_id, dispatch.id);
    assert_eq!(
        ticket(&state, &dispatch.id),
        Some((WORKER.to_string(), Some(OTHER.to_string()))),
        "a named pull arms the ticket against the session, not the task"
    );

    let released =
        projection::report(&state, WORKER, &exited(&task, POOL, 1, None)).expect("exited write");
    assert!(released.applied);
    assert_eq!(
        row_state(&state, &dispatch.id).0,
        Some(LedgerState::InFlight),
        "a row held by another session stays handed to it"
    );
    assert_eq!(
        ticket(&state, &dispatch.id),
        Some((WORKER.to_string(), Some(OTHER.to_string()))),
        "the other session's ticket is preserved"
    );

    let other = envelope(MsgKind::Task, WORKER, MASTER, &task, "master's own row");
    send(&state, &other);
    let pulled = pull(&state, MASTER, None, &PullArgs::default()).expect("master pull");
    assert_eq!(pulled.deliveries[0].msg_id, other.id);
    let released =
        projection::report(&state, WORKER, &exited(&task, POOL, 2, None)).expect("exited write");
    assert!(released.applied);
    assert_eq!(
        row_state(&state, &other.id).0,
        Some(LedgerState::InFlight),
        "another role's row is not released by this role's exit"
    );
    assert_eq!(
        ticket(&state, &other.id),
        Some((MASTER.to_string(), None)),
        "the master's own ticket is preserved"
    );
}

/// A session whose own id is the task id — the client-held spelling — releases
/// only the delivery it served. The completion of that task belongs to its
/// recipient and is not swept up with it.
#[test]
fn a_session_named_by_its_task_keeps_the_completion() {
    let (_root, state) = server();
    connect(&state, MASTER);
    connect(&state, WORKER);
    let task = onlyne_proto::new_task_id();
    let seeded = seed(&state, &task);

    // A client-held session's id *is* its task id, which is the shape the
    // release used to key its fallback on.
    let released =
        projection::report(&state, WORKER, &exited(&task, &task, 1, None)).expect("exited write");
    assert!(released.applied);

    assert_eq!(
        row_state(&state, &seeded.dispatch).0,
        Some(LedgerState::Queued),
        "the served delivery is requeued"
    );
    assert_eq!(
        row_state(&state, &seeded.completion).0,
        Some(LedgerState::InFlight),
        "the completion of that task is the recipient's, not the session's"
    );
    assert_eq!(
        ticket(&state, &seeded.completion),
        Some((MASTER.to_string(), None)),
        "the recipient keeps its ticket"
    );
}
