//! The whole inbound path, against a local Socket Mode counterpart.
//!
//! The SDK's listener mints a connection URL with `apps.connections.open` and
//! dials whatever it answers; nothing in the SDK takes an address from its
//! caller. So the test stands in for Slack's API on the Web API root and
//! answers that call with the local websocket's URL — which is what a real
//! workspace would answer, minus the ticket. From there the path is the one a
//! deployment runs: the SDK dials, the listener reads, the callback queues, and
//! `next_event` returns what the callback queued.
//!
//! This is also where `ONLYNE_SLACK_WS_URL` is honoured: a test names the
//! address, its fake API hands that address out, and the listener dials it.

use futures_util::{SinkExt, StreamExt};
use onlyne_im_backend::backend::Backend;
use onlyne_im_backend_slack::SlackBackend;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

mod support;

use support::{fake_api, recorded, stream_gate, wait_for};

/// A local Socket Mode counterpart: greets, delivers one events_api envelope,
/// records every ack, then keeps the connection open.
async fn fake_slack(listener: tokio::net::TcpListener) -> Arc<Mutex<Vec<Value>>> {
    let acks = Arc::new(Mutex::new(Vec::new()));
    let log = acks.clone();
    tokio::spawn(async move {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await else {
            return;
        };
        let _ = socket
            .send(Message::text(
                json!({"type": "hello", "num_connections": 1, "connection_info": {"app_id": "A1"},
                       "debug_info": {"host": "local"}})
                .to_string(),
            ))
            .await;
        let _ = socket
            .send(Message::text(
                json!({
                    "type": "events_api",
                    "envelope_id": "env-1",
                    "accepts_response_payload": false,
                    "payload": {
                        "team_id": "T1",
                        "api_app_id": "A1",
                        "event": {"type": "message", "channel": "C1", "user": "U1",
                                  "text": "hello", "ts": "1.1"},
                        "type": "event_callback",
                        "event_id": "Ev1",
                        "event_time": 1
                    }
                })
                .to_string(),
            ))
            .await;
        while let Some(Ok(message)) = socket.next().await {
            if let Message::Text(text) = message {
                if let Ok(value) = serde_json::from_str::<Value>(&text) {
                    log.lock().expect("ack log").push(value);
                }
            }
        }
    });
    acks
}

#[tokio::test]
async fn an_event_is_queued_then_acknowledged_then_returned() {
    let _gate = stream_gate().await;
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the socket");
    let socket_url = format!("ws://{}", socket.local_addr().expect("addr"));
    let acks = fake_slack(socket).await;

    // The API answers `apps.connections.open` with that socket's address.
    let (base, requests) = fake_api(vec![(
        200,
        json!({"ok": true, "url": socket_url}).to_string(),
    )]);
    let mut backend = SlackBackend::from_env(Some("xapp-app:xoxb-bot".into()), "gw", Some(base));

    let event = tokio::time::timeout(Duration::from_secs(10), backend.next_event())
        .await
        .expect("the stream delivers within the deadline")
        .expect("the stream is open")
        .expect("one event");
    assert_eq!(event.id, "1.1");
    assert_eq!(event.conversation, "C1");
    assert_eq!(event.text.as_deref(), Some("hello"));

    wait_for(&acks, 1).await;
    assert_eq!(
        acks.lock().unwrap().as_slice(),
        &[json!({"envelope_id": "env-1"})],
        "the listener acknowledges the envelope after the callback queued it"
    );
    // The listener mints its own connection URL, with the app-level token.
    // Asserted on the recorded set: the fake records one `read()` per
    // connection, which under load can be a partial request line.
    let minted = recorded(&requests);
    assert!(
        minted
            .iter()
            .any(|request| request.starts_with("POST /apps.connections.open")),
        "the listener asks the API for its socket: {minted:?}"
    );
    assert!(
        minted.iter().any(|request| request
            .to_lowercase()
            .contains("authorization: bearer xapp-app")),
        "the socket is minted with the app-level token: {minted:?}"
    );

    backend.stop().await.expect("a clean stop");
}

#[tokio::test]
async fn an_unanswered_stream_keeps_the_backend_polling() {
    let _gate = stream_gate().await;
    // A local socket that accepts and then greets nothing: the connection is
    // open, and a stream with nothing to deliver waits rather than ending.
    // (A refused dial is the SDK's own reconnect loop, which parks outside
    // `next_event`, so it is not what this measures.)
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the socket");
    let address = socket.local_addr().expect("addr");
    tokio::spawn(async move {
        let Ok((stream, _)) = socket.accept().await else {
            return;
        };
        let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await else {
            return;
        };
        // Hold the connection open, greeting nothing.
        while socket.next().await.is_some() {}
    });
    let (base, _requests) = fake_api(vec![(
        200,
        json!({"ok": true, "url": format!("ws://{address}")}).to_string(),
    )]);
    let mut backend = SlackBackend::from_env(Some("xapp-app:xoxb-bot".into()), "gw", Some(base));
    let pending = tokio::time::timeout(Duration::from_secs(2), backend.next_event()).await;
    assert!(
        pending.is_err(),
        "a stream with no delivery parks instead of ending: {pending:?}"
    );
}
