//! A `pull` that asks the server to hold (`hold_ms`) is answered off the
//! connection's read loop.
//!
//! Each case drives `role_connection_with_io` over a duplex stream, so the
//! frames on the wire are the ones production writes. The held pull's answer
//! arrives through the session's channel, which means the loop stays free:
//! a report sent during the window is answered first, and a send aimed at the
//! role wakes the pull before its window closes.
//!
//! The same wait must end, without claiming a row, when the connection it would
//! answer on is gone: a successor link that registered the role, or a server
//! shutdown request.

use onlyne_config::layout::ServerRoot;
use onlyne_net::NetError;
use onlyne_proto::{
    Body, Causality, ClientOp, Envelope, ErrorCode, Frame, HandshakeArgs, MsgKind,
    PROTOCOL_VERSION, Principal, PullArgs, Report, ResBody,
};
use onlyne_server::state::State;
use onlyne_server::{RoleIo, Server, ServerInit, role_connection_with_io};
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tokio::io::DuplexStream;

const MASTER: &str = "master";
const WORKER: &str = "worker";
/// The held pull's window in the ordering case: long enough that an answer
/// which never left the read loop would be visibly late, short enough that a
/// real-time test stays quick.
const WINDOW_MS: u64 = 1000;

/// A server over a temporary root with two roles that may address each other.
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
             name = \"pull-hold\"\n\
             listen = \"127.0.0.1:0\"\n\
             cert_pin = {pin:?}\n\
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

/// One framed end of a duplex stream.
struct Wire {
    read: DuplexStream,
    write: DuplexStream,
    frames: onlyne_wire::FrameReader,
}

impl Wire {
    fn new(read: DuplexStream, write: DuplexStream) -> Self {
        Wire {
            read,
            write,
            frames: onlyne_wire::FrameReader::new(),
        }
    }
}

impl RoleIo for Wire {
    async fn send_frame(&mut self, frame: &Frame) -> Result<(), NetError> {
        onlyne_wire::write_frame(&mut self.write, frame)
            .await
            .map_err(|error| NetError::Io(error.to_string()))
    }

    async fn recv_frame(&mut self) -> Result<Option<Frame<ClientOp>>, NetError> {
        self.frames
            .next(&mut self.read)
            .await
            .map_err(|error| NetError::Io(error.to_string()))
    }
}

/// The client side of one served connection.
struct Client {
    read: DuplexStream,
    write: DuplexStream,
    frames: onlyne_wire::FrameReader,
}

impl Client {
    async fn send(&mut self, id: &str, op: ClientOp) {
        let frame = Frame::Req {
            id: id.to_string(),
            op,
        };
        onlyne_wire::write_frame(&mut self.write, &frame)
            .await
            .expect("write one request");
    }

    async fn recv(&mut self) -> Frame {
        tokio::time::timeout(Duration::from_secs(5), self.frames.next(&mut self.read))
            .await
            .expect("a frame inside the deadline")
            .expect("read one frame")
            .expect("the server closed the link")
    }

    /// The next answer on this connection, skipping the events a planned push
    /// may interleave with it.
    async fn answer(&mut self) -> (String, ResBody) {
        loop {
            if let Frame::Res { id, body } = self.recv().await {
                return (id, body);
            }
        }
    }
}

/// Serve one role over a duplex pair and complete its `hello`.
async fn connect(state: &Arc<State>, role: &'static str) -> Client {
    let (server_read, client_write) = tokio::io::duplex(64 * 1024);
    let (client_read, server_write) = tokio::io::duplex(64 * 1024);
    tokio::spawn(role_connection_with_io(
        Arc::clone(state),
        Wire::new(server_read, server_write),
        role,
    ));
    let mut client = Client {
        read: client_read,
        write: client_write,
        frames: onlyne_wire::FrameReader::new(),
    };
    client
        .send(
            "hello",
            ClientOp::Hello(HandshakeArgs {
                protocol: PROTOCOL_VERSION,
                role: role.to_string(),
                agent: "pull-hold".to_string(),
                ..HandshakeArgs::default()
            }),
        )
        .await;
    let (id, body) = client.answer().await;
    assert_eq!(id, "hello");
    assert!(body.ok, "hello was refused: {body:?}");
    client
}

/// A bare liveness beat for a session of the role that carries it.
fn heartbeat() -> Report {
    Report::Heartbeat {
        task_id: "task-beat".to_string(),
        session_id: "session-beat".to_string(),
        generation: 1,
        seq: 1,
        observed: json!({}),
        projection: None,
        cluster_ref: None,
    }
}

/// The one task master dispatches to worker in the wake case.
fn dispatch() -> Envelope {
    onlyne_proto::new_envelope(
        MsgKind::Task,
        Principal::role(MASTER),
        Principal::role(WORKER),
        Body::text("carry this"),
        Some(Causality::root("11111111-1111-4111-8111-111111111111")),
    )
    .expect("valid envelope")
}

fn hold(ms: u64) -> ClientOp {
    ClientOp::Pull(PullArgs {
        hold_ms: Some(ms),
        ..PullArgs::default()
    })
}

/// An empty queue keeps the pull for its window, and the connection keeps
/// serving meanwhile: the report sent during the hold is answered first, and
/// the pull answers empty at roughly the window.
#[tokio::test]
async fn a_held_pull_answers_empty_at_its_window_and_leaves_the_link_serving() {
    let (_root, state) = server();
    let mut worker = connect(&state, WORKER).await;

    let started = Instant::now();
    worker.send("hold", hold(WINDOW_MS)).await;
    worker.send("beat", ClientOp::Report(heartbeat())).await;

    let (first, body) = worker.answer().await;
    assert_eq!(first, "beat", "the report is answered while the pull waits");
    assert!(body.ok, "the heartbeat report was refused: {body:?}");
    let beaten = started.elapsed();
    assert!(
        beaten < Duration::from_millis(WINDOW_MS / 2),
        "the report waited {beaten:?} behind the held pull"
    );

    let (second, body) = worker.answer().await;
    assert_eq!(second, "hold");
    let held = started.elapsed();
    assert!(
        held >= Duration::from_millis(WINDOW_MS - 100),
        "the held pull answered after only {held:?}"
    );
    assert!(
        held < Duration::from_millis(WINDOW_MS * 3),
        "the held pull overran its window: {held:?}"
    );
    assert!(body.ok, "the held pull failed: {body:?}");
    let data = body.data.expect("a pull reply body");
    assert_eq!(
        data["deliveries"],
        json!([]),
        "an empty queue answers no deliveries"
    );
}

/// A send aimed at the role wakes the held pull: the delivery arrives without
/// waiting out the window.
#[tokio::test]
async fn a_send_to_the_role_wakes_the_held_pull() {
    let (_root, state) = server();
    let mut worker = connect(&state, WORKER).await;
    let mut master = connect(&state, MASTER).await;

    let started = Instant::now();
    worker.send("hold", hold(10_000)).await;
    // The pull is held: nothing answers it while the queue is empty.
    assert!(
        tokio::time::timeout(Duration::from_millis(300), worker.answer())
            .await
            .is_err(),
        "the pull answered before any delivery or its window"
    );

    master
        .send("dispatch", ClientOp::Send(Box::new(dispatch())))
        .await;
    let (id, body) = master.answer().await;
    assert_eq!(id, "dispatch");
    assert!(body.ok, "the dispatch was refused: {body:?}");
    let msg_id = body.data.expect("a receipt body")["msg_id"]
        .as_str()
        .expect("the receipt names its row")
        .to_string();

    let (id, body) = worker.answer().await;
    assert_eq!(id, "hold");
    let woke = started.elapsed();
    assert!(
        woke < Duration::from_secs(2),
        "the held pull waited out its window instead of waking: {woke:?}"
    );
    assert!(body.ok, "the woken pull failed: {body:?}");
    let data = body.data.expect("a pull reply body");
    let deliveries = data["deliveries"].as_array().expect("a delivery list");
    assert_eq!(deliveries.len(), 1, "the wake hands over the one row");
    assert_eq!(deliveries[0]["msg_id"], json!(msg_id));
}

/// One connection reads its queue once per wait: a second pull while the first
/// is held is refused by name, and the refusal reads no queue — it hands over
/// no row and arms no ticket.
#[tokio::test]
async fn a_second_pull_while_one_waits_is_refused() {
    let (_root, state) = server();
    let mut worker = connect(&state, WORKER).await;

    let started = Instant::now();
    worker.send("hold", hold(WINDOW_MS)).await;
    // No `hold_ms`: the immediate read is refused by the same rule, because it
    // is the same read of the same queue.
    worker
        .send(
            "again",
            ClientOp::Pull(PullArgs {
                limit: 1,
                ..PullArgs::default()
            }),
        )
        .await;

    let (id, body) = worker.answer().await;
    assert_eq!(id, "again", "the refusal is answered at once");
    assert!(!body.ok, "the second pull was served: {body:?}");
    assert_eq!(
        body.error.expect("a refusal").code,
        ErrorCode::Conflict,
        "the second pull is refused by name"
    );
    let refused = started.elapsed();
    assert!(
        refused < Duration::from_millis(WINDOW_MS / 2),
        "the refusal waited {refused:?}"
    );

    let (id, body) = worker.answer().await;
    assert_eq!(id, "hold", "the first pull still owns the wait");
    assert!(
        started.elapsed() >= Duration::from_millis(WINDOW_MS - 100),
        "the first pull answered before its window"
    );
    assert!(body.ok, "the first pull failed: {body:?}");
    assert_eq!(
        body.data.expect("a pull reply body")["deliveries"],
        json!([])
    );
}

/// A successor link takes the role's registry entry, and the link it replaced
/// must not claim a row when its window closes: the row is the successor's.
#[tokio::test]
async fn a_replaced_link_ends_its_held_pull_without_claiming_a_row() {
    let (_root, state) = server();
    let mut departed = connect(&state, WORKER).await;
    let mut master = connect(&state, MASTER).await;

    let started = Instant::now();
    departed.send("hold", hold(WINDOW_MS)).await;
    let mut successor = connect(&state, WORKER).await;

    master
        .send("dispatch", ClientOp::Send(Box::new(dispatch())))
        .await;
    let (id, body) = master.answer().await;
    assert_eq!(id, "dispatch");
    assert!(body.ok, "the dispatch was refused: {body:?}");
    let msg_id = body.data.expect("a receipt body")["msg_id"]
        .as_str()
        .expect("the receipt names its row")
        .to_string();

    // Wait past the departed link's window: a wait that went on reading the
    // queue would have claimed this row by now, and the ticket it armed would
    // keep the successor from seeing it.
    let past_window = Duration::from_millis(WINDOW_MS + 200);
    tokio::time::sleep(past_window.saturating_sub(started.elapsed())).await;

    // A pull the superseded link sends after the handover is refused by name,
    // and it arms no ticket: the row below still reaches the successor.
    departed
        .send(
            "late",
            ClientOp::Pull(PullArgs {
                limit: 1,
                ..PullArgs::default()
            }),
        )
        .await;
    let (id, body) = departed.answer().await;
    assert_eq!(id, "late");
    assert!(!body.ok, "a superseded link was served a pull: {body:?}");
    assert_eq!(
        body.error.expect("a refusal").code,
        ErrorCode::Conflict,
        "a superseded link is refused by name"
    );

    successor
        .send(
            "deliver",
            ClientOp::Pull(PullArgs {
                limit: 1,
                ..PullArgs::default()
            }),
        )
        .await;
    let (id, body) = successor.answer().await;
    assert_eq!(id, "deliver");
    assert!(body.ok, "the successor's pull failed: {body:?}");
    let data = body.data.expect("a pull reply body");
    let deliveries = data["deliveries"].as_array().expect("a delivery list");
    assert_eq!(
        deliveries.len(),
        1,
        "the row belongs to the link that holds the role"
    );
    assert_eq!(deliveries[0]["msg_id"], json!(msg_id));
    assert!(
        tokio::time::timeout(Duration::from_millis(300), departed.answer())
            .await
            .is_err(),
        "the departed link answered a pull it no longer owned"
    );
}

/// A shutdown request ends every held wait, and the wait it ends claims no
/// row: nothing is left armed for a process that is leaving.
#[tokio::test]
async fn a_shutdown_request_ends_the_held_pull_without_claiming_a_row() {
    let (_root, state) = server();
    let mut worker = connect(&state, WORKER).await;
    let mut master = connect(&state, MASTER).await;

    worker.send("hold", hold(10_000)).await;
    // The wait is registered on the shutdown notification by now.
    tokio::time::sleep(Duration::from_millis(200)).await;
    state.request_shutdown();
    tokio::time::sleep(Duration::from_millis(100)).await;

    master
        .send("dispatch", ClientOp::Send(Box::new(dispatch())))
        .await;
    let (id, body) = master.answer().await;
    assert_eq!(id, "dispatch");
    assert!(body.ok, "the dispatch was refused: {body:?}");
    assert!(
        tokio::time::timeout(Duration::from_millis(400), worker.answer())
            .await
            .is_err(),
        "a shutdown request must end the held pull without claiming its row"
    );
}
