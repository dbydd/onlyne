// The end-to-end cases the contract names: a send from a board is a
// `_supervisor` send whose receipt appears on the operator's board, and a
// dragged edge becomes a typed `SpecApply` edit that a second client sees
// after the reload, with the spec's comments surviving it.

use axum::body::Body;
use axum::http::{header, Request};
use onlyne_proto::{AdminOp, SetTargets, SpecEdit};
use onlyne_testkit::harness::Cluster;
use onlyne_web::admin::exchange;
use onlyne_web::{mint_token, ops::WebOp, serve_request, App};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::time::Duration;

/// The spec a real cluster runs for these cases: three roles, two routes.
fn spec() -> String {
    r#"[server]
name = "web-e2e"
listen = "127.0.0.1:0"
"#
    .to_string()
}

/// Start a real cluster, register one role with a fake agent that completes,
/// start the web app against the admin socket, and return both.
async fn cluster_with_web() -> (Cluster, std::sync::Arc<App>, String) {
    let cluster = Cluster::start(&spec()).await.expect("start the cluster");
    // The supervisor role is registered through the same `onlyne-client
    // init` a real operator runs, which mints a real key and appends the
    // entry to the spec.
    let _supervisor_ws = cluster
        .register_role(
            "_supervisor",
            "the operator",
            // The operator addresses every board, so its target list is the
            // wildcard. A role with no `allowed_targets` may address nobody,
            // and a send from it is refused `acl_denied` by the server — which
            // is the ACL working, not a web-layer fault.
            Some("allowed_senders = [\"*\"]\nallowed_targets = [\"*\"]\nadmin = true"),
        )
        .await
        .expect("register supervisor");
    let planner_ws = cluster
        .register_role(
            "planner",
            "plan the work",
            // `allowed_targets` is the role's permission *and* its obligation:
            // every downstream role on the list must have received a delivery
            // before this session may report a terminal outcome. This task's
            // agent completes without handing anything on, so the list names
            // the role it reports back to instead — the task's originator,
            // which is never owed a handoff because the completion is itself
            // the delivery to it. A list naming `builder` here would be a
            // promise this task never keeps, and its completion would be
            // refused for it.
            Some("allowed_senders = [\"*\"]\nallowed_targets = [\"_supervisor\"]"),
        )
        .await
        .expect("register planner");
    cluster
        .start_client(&planner_ws)
        .await
        .expect("start planner client");

    let script = onlyne_testkit::AgentScript {
        hello: onlyne_testkit::ScriptHello {
            capabilities: vec![
                onlyne_proto::Capability::Register,
                onlyne_proto::Capability::Report,
                onlyne_proto::Capability::Inject,
                onlyne_proto::Capability::Recycle,
            ],
        },
        steps: vec![
            json!({"wait_assign": true}),
            json!({"report": "ready"}),
            json!({"report": "heartbeat"}),
            json!({"complete": {"outcome": "done", "head_from": "assign_body"}}),
        ],
        repeat: false,
    };
    cluster
        .start_fake_agent(&planner_ws, &script)
        .await
        .expect("start the fake agent");
    cluster
        .wait_role_online("planner")
        .await
        .expect("planner online");

    let socket = cluster.admin_socket().expect("the admin socket");
    let token = mint_token();
    let bind = "127.0.0.1:0".parse::<SocketAddr>().unwrap();
    let app = App::new(socket, 5000, token.clone(), bind);
    (cluster, app, token)
}

/// Send one op through the web's own `/api/op` path.
async fn op_via_web(app: &std::sync::Arc<App>, token: &str, op: WebOp) -> Value {
    let body = serde_json::to_string(&op).expect("encode the web op");
    let request = Request::builder()
        .uri(format!("/api/op?token={token}"))
        .header(header::HOST, "127.0.0.1")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    let response = serve_request(app, request).await;
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read the op answer");
    serde_json::from_slice(&bytes).expect("the op answer parses")
}

/// Read the current view through the web's own `/api/view` path.
async fn view_via_web(app: &std::sync::Arc<App>, token: &str) -> Value {
    let request = Request::builder()
        .uri(format!("/api/view?token={token}"))
        .header(header::HOST, "127.0.0.1")
        .body(Body::empty())
        .unwrap();
    let response = serve_request(app, request).await;
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read the view");
    serde_json::from_slice(&bytes).expect("the view parses")
}

/// Poll the view until a predicate holds, bounded by a timeout.
///
/// Returns the last view it read alongside whether the predicate held, so a
/// timeout names the state that was actually on screen instead of failing on a
/// bare `None`. A poll that cannot say what it saw is a poll whose failure costs
/// the reader the whole diagnosis.
async fn poll_view(
    app: &std::sync::Arc<App>,
    token: &str,
    predicate: impl Fn(&Value) -> bool,
    timeout: Duration,
) -> (bool, Value) {
    let start = std::time::Instant::now();
    let mut last = Value::Null;
    while start.elapsed() < timeout {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let view = view_via_web(app, token).await;
        last = view.clone();
        if predicate(&view) {
            return (true, view);
        }
    }
    (false, last)
}

/// A send from a board is a `_supervisor` send, and its receipt appears on
/// the operator's board.
#[tokio::test]
async fn a_send_from_a_board_is_a_supervisor_send_with_receipt() {
    let (cluster, app, token) = cluster_with_web().await;

    // Send a task to the planner board through the web's op path.
    let answer = op_via_web(
        &app,
        &token,
        WebOp::Send {
            to: "planner".to_string(),
            text: "plan the migration".to_string(),
        },
    )
    .await;
    // The send must be *accepted*, not merely answered. A refusal carries a
    // `code` and no `task`, so the task id is the assertion that matters: the
    // loose one this replaces let an `acl_denied` send through and the case
    // then failed 60 seconds later on a symptom three steps from its cause.
    assert!(
        answer.get("code").is_none(),
        "the send was not refused: {answer}"
    );
    let task = answer["task"]
        .as_str()
        .unwrap_or_else(|| panic!("the send minted a task id: {answer}"));
    assert!(!task.is_empty(), "the task id is not empty: {answer}");

    // The planner's board eventually shows the card as done.
    let (settled, view) = poll_view(
        &app,
        &token,
        |view| {
            view["boards"]
                .as_array()
                .and_then(|boards| boards.iter().find(|b| b["role"] == "planner"))
                .and_then(|b| b["cards"].as_array())
                .map(|cards| cards.iter().any(|c| c["column"] == "done"))
                .unwrap_or(false)
        },
        Duration::from_secs(60),
    )
    .await;
    assert!(
        settled,
        "the planner's card settled as done: the last view read was {view}"
    );
    let done = view;

    // The operator's board carries the receipt: a delivery to _supervisor.
    let operator = done["boards"]
        .as_array()
        .expect("boards")
        .iter()
        .find(|b| b["role"] == "_supervisor")
        .expect("the operator board exists");
    assert!(operator["operator"].as_bool().unwrap_or(false));
    let receipts = operator["cards"].as_array().cloned().unwrap_or_default();
    assert!(
        !receipts.is_empty(),
        "the operator's board carries the completion receipt"
    );
    assert!(
        receipts.iter().all(|card| !card["from"].is_null()),
        "every receipt names its sender"
    );

    drop(cluster);
}

/// Dragging an edge (a `set_targets` edit through `/api/op`) reaches the
/// server, reloads the spec, and a second client sees the new route — with
/// the spec's own comments surviving the edit.
#[tokio::test]
async fn dragging_an_edge_applies_the_edit_a_second_client_sees() {
    let (cluster, app, token) = cluster_with_web().await;
    let socket = cluster.admin_socket().expect("the admin socket");

    // The operator's own comment in the spec file, which must survive.
    let spec_path = cluster.spec_path();
    let original = tokio::fs::read_to_string(&spec_path)
        .await
        .expect("read spec");
    assert!(original.contains("[[client]]"), "the spec declares roles");
    // Write a comment above the planner's entry.
    let with_comment = original.replace(
        "[[client]]",
        "# the planner's routes, edited from the web\n[[client]]",
    );
    tokio::fs::write(&spec_path, &with_comment)
        .await
        .expect("write comment");

    // Read the spec through the web's op path.
    let spec_view = op_via_web(&app, &token, WebOp::SpecGet).await;
    let base_hash = spec_view["source_hash"]
        .as_str()
        .expect("the hash")
        .to_string();
    assert!(!base_hash.is_empty());

    // Apply the dragged edge: planner gains builder as an allowed target.
    let edit = SpecEdit::SetTargets(SetTargets {
        role: "planner".to_string(),
        targets: vec!["builder".to_string()],
    });
    let answer = op_via_web(
        &app,
        &token,
        WebOp::SpecApply {
            base_hash,
            edits: vec![edit],
        },
    )
    .await;
    assert!(!answer.is_null(), "the spec apply answered: {answer}");

    // The spec file survived with its comment.
    let after = tokio::fs::read_to_string(&spec_path)
        .await
        .expect("re-read spec");
    assert!(
        after.contains("# the planner's routes, edited from the web"),
        "the operator's comment survived the edit"
    );

    // The server answers the new route to a second client (a direct admin
    // exchange, which is what a second browser's view would carry).
    let second = exchange(&socket, AdminOp::Roles(Default::default()), 5000)
        .await
        .expect("the second client's roles read");
    let roles = second["roles"].as_array().expect("the roles");
    let planner = roles
        .iter()
        .find(|r| r["name"] == "planner")
        .expect("planner");
    let edges: Vec<&str> = planner["edges"]
        .as_array()
        .and_then(|edges| edges.iter().map(|e| e.as_str()).collect())
        .unwrap_or_default();
    assert!(
        edges.contains(&"builder"),
        "the second client sees the dragged route: {edges:?}"
    );

    // The web's own view (the second browser's stream) also carries it.
    //
    // The predicate names `builder`, not merely a non-empty edge list: the
    // planner's board already carries its own route to `_supervisor`, so a
    // weaker predicate was satisfied by the *initial* spec and the case passed
    // without the dragged edge ever reaching the view. A case that cannot fail
    // is not a case.
    let (reloaded, view) = poll_view(
        &app,
        &token,
        |view| {
            view["boards"]
                .as_array()
                .and_then(|boards| boards.iter().find(|b| b["role"] == "planner"))
                .map(|b| {
                    b["edges"]
                        .as_array()
                        .is_some_and(|edges| edges.iter().any(|e| e == "builder"))
                })
                .unwrap_or(false)
        },
        Duration::from_secs(10),
    )
    .await;
    assert!(
        reloaded,
        "the web's view carries the reloaded spec: the last view read was {view}"
    );

    drop(cluster);
}
