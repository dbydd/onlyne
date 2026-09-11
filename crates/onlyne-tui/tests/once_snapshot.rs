//! End-to-end: `onlyne-tui --once` renders the role network of a real server.
//!
//! The fixture is the server test suite's own: a real `onlyne-server` state
//! with its admin socket bound, one settled task, one in-flight task, and one
//! running session. The TUI runs as the built binary, so this test also covers
//! socket resolution, the admin frame round trip, and the `--once` exit path.

use chrono::Utc;
use onlyne_proto::{
    AckArgs, Body, Causality, LedgerState, Lifecycle, MsgKind, Principal, SessionProjection,
    SessionSyncArgs,
};
use onlyne_server::state::{RoleConnection, Server, ServerInit};
use onlyne_server::{admin, projection, relay};

const CERT_PIN: &str = "sha256/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

fn key() -> String {
    onlyne_net::KeyPair::from_seed([7_u8; 32]).public_str()
}

fn spec_text() -> String {
    format!(
        r#"[server]
name = "local"
listen = "127.0.0.1:0"
cert_pin = "{CERT_PIN}"
heartbeat_timeout_ms = 60000

[[client]]
role = "planner"
aggregate = "cluster-b"
key = "{key}"
allowed_senders = ["planner", "builder"]
allowed_targets = ["planner", "builder"]

[[client]]
role = "builder"
key = "{key}"
allowed_senders = ["planner", "builder"]
allowed_targets = ["planner"]
"#,
        key = key()
    )
}

fn task(text: &str) -> onlyne_proto::Envelope {
    onlyne_proto::new_envelope(
        MsgKind::Task,
        Principal::role("planner"),
        Principal::role("builder"),
        Body::text(text),
        Some(Causality::root(onlyne_proto::new_task_id())),
    )
    .expect("a valid task")
}

fn accepted(reply: relay::RelayReply) -> relay::SendOutcome {
    match reply {
        relay::RelayReply::Accepted(outcome) => *outcome,
        other => panic!("expected an accepted send, got {other:?}"),
    }
}

fn once(root: &std::path::Path, page: Option<&str>) -> String {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_onlyne-tui"));
    command.args(["--once", "--server-root"]);
    command.arg(root);
    if let Some(page) = page {
        command.args(["--page", page]);
    }
    let output = command.output().expect("run onlyne-tui --once");
    assert!(
        output.status.success(),
        "exit {} with stderr {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn once_prints_the_role_network_and_the_busy_star() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("server");
    std::fs::create_dir_all(root.join(".onlyne")).expect("create the server root");
    std::fs::write(root.join(".onlyne/spec.toml"), spec_text()).expect("write the spec");
    let state = Server::open(&ServerInit {
        root: root.clone(),
        listen: None,
    })
    .expect("open the server");
    let listener = admin::bind(&state).expect("bind the admin socket");
    tokio::spawn(admin::serve_socket(state.clone(), listener));

    // A connected recipient turns an accepted task into an in-flight hop, so
    // the map has an edge the ledger marks as carrying traffic.
    let (sender, _receiver) = tokio::sync::mpsc::channel(8);
    state.register_role(RoleConnection {
        role: "builder".to_string(),
        sender,
        last_seq: 0,
        connected_at: Utc::now(),
        draining: false,
    });
    let settled = accepted(relay::send(&state, &task("first"), false, None).expect("relay"));
    let acked = relay::ack(
        &state,
        &AckArgs {
            msg_id: settled.receipt.msg_id.clone(),
            op_id: None,
            accepted: true,
            reason: None,
        },
    )
    .expect("ack")
    .expect("an accepted ack");
    assert_eq!(acked.state, LedgerState::Acked);

    let in_flight = accepted(relay::send(&state, &task("second"), false, None).expect("relay"));
    assert_eq!(in_flight.receipt.state, LedgerState::InFlight);
    let task_id = in_flight
        .receipt
        .task
        .clone()
        .expect("the receipt names its task");

    // A running session on the sender makes planner a busy node.
    let mut running = SessionProjection::default_working();
    running.lifecycle = Lifecycle::Working;
    running.agent = onlyne_proto::AgentPhase::Running;
    let applied = projection::session_sync(
        &state,
        "planner",
        &SessionSyncArgs {
            task_id,
            session_id: "sess-1".to_string(),
            generation: 1,
            seq: 1,
            projection: running,
        },
    )
    .expect("sync the projection");
    assert!(applied.applied, "the first sync lands");

    let text = once(&root, None);
    assert!(text.matches('╭').count() >= 2, "{text}");
    assert!(
        text.contains("planner*"),
        "the busy role carries a star\n{text}"
    );
    assert!(
        text.contains("⬡planner"),
        "the aggregate role carries its marker\n{text}"
    );
    assert!(text.contains('▶'), "an arrow reaches the target\n{text}");
    assert!(
        text.contains('◐'),
        "the running session shows its glyph\n{text}"
    );
    assert!(text.contains("server online"), "{text}");
    let map_row = text
        .lines()
        .find(|line| line.contains("╭─⬡planner"))
        .unwrap_or_else(|| panic!("no planner box\n{text}"));
    assert!(
        map_row.contains("╭─builder"),
        "boxes sit side by side, not in a list\n{text}"
    );

    let swarm = once(&root, Some("2"));
    assert!(swarm.contains("graph [focus]"), "{swarm}");
    assert!(swarm.contains("history"), "{swarm}");
    assert!(
        swarm.contains("planner→builder"),
        "the graph pane names the in-flight hop\n{swarm}"
    );
}
