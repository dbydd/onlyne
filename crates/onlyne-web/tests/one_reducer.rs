// The one-reducer proof: the state the web's own code path serves is the
// state `onlyne_proto::view` folds from the same inputs, so a second fold
// cannot hide (`docs/v2-CONTRACT.md` §"Slice 10" acceptance).

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use onlyne_proto::view::{snapshot_to_view, update, Snapshot, View};
use onlyne_proto::{
    AdminOp, Event, Frame, LedgerState, LedgerStateEvent, MsgKind, Principal, ResBody,
};
use onlyne_web::{mint_token, serve_request, App};
use onlyne_wire::{write_frame, FrameReader};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::PathBuf;
use tempfile::tempdir;
use tokio::net::UnixListener;

/// The scripted cluster the stub serves: three roles, one delivery.
fn scripted_snapshot() -> Snapshot {
    Snapshot {
        status: Some(json!({"event_head": 0, "cluster": "test", "role_count": 3})),
        roles: vec![
            role_info("planner", vec!["builder"]),
            role_info("builder", vec!["planner"]),
            role_info("reviewer", vec![]),
        ],
        sessions: vec![],
        ledger: vec![ledger_entry(
            "m1",
            "_supervisor",
            "planner",
            "t1",
            LedgerState::Queued,
        )],
        faults: vec![],
    }
}

fn role_info(name: &str, edges: Vec<&str>) -> onlyne_proto::RoleInfo {
    use onlyne_proto::{Presence, RoleInfo, RoleRuntime};
    RoleInfo {
        name: name.to_string(),
        admin: false,
        max_sessions: 3,
        runtime: RoleRuntime::default(),
        spec_hash: "h".to_string(),
        prose: None,
        state: Presence::Offline,
        sessions: 0,
        queued: 0,
        edges: edges.into_iter().map(String::from).collect(),
        detail: None,
        aggregate: None,
    }
}

fn ledger_entry(
    msg_id: &str,
    from: &str,
    to: &str,
    task: &str,
    state: LedgerState,
) -> onlyne_proto::LedgerEntry {
    onlyne_proto::LedgerEntry {
        msg_id: msg_id.to_string(),
        op_id: None,
        kind: MsgKind::Task,
        from: Principal::role(from),
        to: Principal::role(to),
        task: Some(task.to_string()),
        parent_task: None,
        hop: 0,
        family: Some(task.to_string()),
        hop_budget: Some(8),
        origin: Some(from.to_string()),
        deadline: None,
        labels: None,
        attempt: 0,
        state,
        reason: None,
        out_head: Some(format!("work for {to}")),
        body_json: None,
        // A pinned timestamp, so the stub's row and the expected fold carry
        // the same instant; the test's own `Utc::now()` would differ by
        // milliseconds and the comparison is byte-for-byte.
        enqueued_at: fixed_time(),
        acked_at: None,
    }
}

fn fixed_time() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339("2026-09-29T00:00:00Z")
        .expect("a fixed timestamp")
        .with_timezone(&chrono::Utc)
}

/// The event the stub pushes after the first page: the queued delivery moves
/// to in-flight.
fn in_flight_event() -> Event {
    Event::LedgerState(LedgerStateEvent {
        msg_id: "m1".into(),
        op_id: None,
        kind: MsgKind::Task,
        from: Principal::role("_supervisor"),
        to: Principal::role("planner"),
        task: Some("t1".into()),
        state: LedgerState::InFlight,
        outcome: None,
        reason: None,
    })
}

/// A stub admin server that serves the scripted snapshot, then pushes one
/// event on the subscribe stream when told to.
mod stub {
    use super::*;
    use std::time::Duration;

    pub async fn run(socket: PathBuf) {
        let listener = UnixListener::bind(&socket).expect("bind");
        let snapshot = scripted_snapshot();
        let mut pushed = false;
        loop {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut reader = FrameReader::new();
            loop {
                let frame = match reader.next::<_, Frame<AdminOp>>(&mut stream).await {
                    Ok(Some(frame)) => frame,
                    Ok(None) | Err(_) => break,
                };
                if let Frame::Req { id, op } = frame {
                    let body = answer(&op, &snapshot);
                    write_frame(&mut stream, &Frame::res(id, body)).await.ok();
                    if matches!(op, AdminOp::Subscribe(_)) {
                        if !pushed {
                            // Give the link task time to process the
                            // page before the event moves the card.
                            tokio::time::sleep(Duration::from_millis(500)).await;
                            pushed = true;
                        }
                        // Push the event.
                        let ev = Frame::event(1, in_flight_event());
                        write_frame(&mut stream, &ev).await.ok();
                    }
                }
            }
        }
    }

    fn answer(op: &AdminOp, snapshot: &Snapshot) -> ResBody {
        match op {
            AdminOp::Status(_) => ResBody::ok(snapshot.status.clone().unwrap_or(Value::Null)),
            AdminOp::Roles(_) => ResBody::ok(json!({"roles": snapshot.roles})),
            AdminOp::Sessions(_) => ResBody::ok(json!({"sessions": snapshot.sessions})),
            AdminOp::Ledger(_) => ResBody::ok(json!({"ledger": snapshot.ledger})),
            AdminOp::Faults(_) => ResBody::ok(json!({"faults": snapshot.faults})),
            AdminOp::Subscribe(_) => ResBody::ok(json!({"events": []})),
            _ => ResBody::ok(Value::Null),
        }
    }
}

/// The web's served view, after the stream moved the one card, equals the
/// reducer's own fold of the same snapshot plus the same event — so there is
/// no second fold anywhere in the web's path.
#[tokio::test]
async fn served_view_is_the_reducer_fold() {
    let dir = tempdir().expect("temp dir");
    let socket = dir.path().join("reducer.sock");
    let _server = tokio::spawn(stub::run(socket.clone()));

    let bind = "127.0.0.1:0".parse::<SocketAddr>().unwrap();
    let token = mint_token();
    let app = App::new(socket, 2000, token.clone(), bind);

    // Wait for the link to publish its first state.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Before the push: the card is queued.
    let req = Request::builder()
        .uri(format!("/api/view?token={token}"))
        .header(header::HOST, "127.0.0.1")
        .body(Body::empty())
        .unwrap();
    let resp = serve_request(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let payload: Value = serde_json::from_slice(&body).expect("the view payload parses");

    // The reducer's own fold of the same snapshot.
    let snapshot = scripted_snapshot();
    let expected: View = snapshot_to_view(&snapshot);
    let expected_json = serde_json::to_value(&expected).expect("encode the reducer's view");
    assert_eq!(
        payload["view"], expected_json,
        "the served view is the reducer's fold, not a second one"
    );

    // The board's card is in the queued column.
    let planner = payload["boards"]
        .as_array()
        .expect("boards")
        .iter()
        .find(|board| board["role"] == "planner")
        .expect("the planner board");
    assert_eq!(planner["cards"][0]["column"], "queued");

    // The stub pushes the event 500ms after the subscribe page; wait for the
    // fold to land by polling until the delivery moves.
    let mut moved = false;
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let state = app
            .link
            .current()
            .view
            .deliveries
            .get("m1")
            .map(|d| d.state);
        if state == Some(onlyne_proto::LedgerState::InFlight) {
            moved = true;
            break;
        }
    }
    assert!(moved, "the stream moved the card to in_flight");

    // After the push: the card moved because the stream moved it.
    let req = Request::builder()
        .uri(format!("/api/view?token={token}"))
        .header(header::HOST, "127.0.0.1")
        .body(Body::empty())
        .unwrap();
    let resp = serve_request(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let payload: Value = serde_json::from_slice(&body).expect("the view payload parses");

    // The reducer's own fold of the same snapshot plus the same event.
    let after: View = update(snapshot_to_view(&snapshot), &in_flight_event());
    let after_json = serde_json::to_value(&after).expect("encode");
    assert_eq!(
        payload["view"], after_json,
        "the served view after the push is the reducer's fold of it"
    );

    // The card is now in the waiting column (in-flight with no busy session).
    let planner = payload["boards"]
        .as_array()
        .expect("boards")
        .iter()
        .find(|board| board["role"] == "planner")
        .expect("the planner board");
    assert_eq!(
        planner["cards"][0]["column"], "waiting",
        "the card moved because the stream moved it"
    );
}
