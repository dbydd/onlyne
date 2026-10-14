//! The QQ Open Platform event gateway: the websocket the platform pushes
//! dispatches over.
//!
//! QQ's protocol (the one the Open Platform documents for its bot event
//! stream, the same one v1 declared `tokio-tungstenite` for but never wired):
//!
//! 1. Connect to the gateway URL.
//! 2. The server sends `HELLO` (`op 10`, heartbeat interval inside).
//! 3. The client answers with `IDENTIFY` (`op 2`, token + intents).
//! 4. The server answers `READY` (`op 0`, `t = "READY"`) carrying a
//!    `session_id` and the last `s` sequence number.
//! 5. Dispatches arrive as `op 0` frames with `t` naming the event type and
//!    `s` the sequence. The client must heartbeat (`op 1`) on the interval
//!    `HELLO` named, and on reconnect sends `RESUME` (`op 6`) with the saved
//!    session id and last sequence instead of re-identifying.
//!
//! What is verified without a live bot: the frame classification, the
//! `s`-tracking and session save, and the RESUME payload shape — all pure
//! functions over `Value`/`Message` below. The connect, the TLS handshake,
//! and the server's actual interval behavior are not.

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

/// One server op on the QQ gateway websocket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayOp {
    /// A server dispatch (`op 0`), the only op that carries an event.
    Dispatch,
    /// The server's heartbeat request (`op 1`); answer `op 1` with the last `d`.
    Heartbeat,
    /// The server's heartbeat acknowledgment (`op 11`).
    HeartbeatAck,
    /// The server reopening an old session (`op 7`); reconnect and resume.
    Reconnect,
    /// The server rejected the session (`op 9`). The `d` boolean says whether
    /// resume is still possible; when it is false the caller must identify a
    /// fresh session instead of sending the rejected session back in `op 6`.
    InvalidSession,
    /// The initial hello (`op 10`), carrying the heartbeat interval in `d`.
    Hello,
    /// Anything else; logged and ignored.
    Other,
}

/// Classify one text frame by its `op`.
pub fn classify(frame: &Value) -> GatewayOp {
    match frame.get("op").and_then(Value::as_i64) {
        Some(0) => GatewayOp::Dispatch,
        Some(1) => GatewayOp::Heartbeat,
        Some(7) => GatewayOp::Reconnect,
        Some(10) => GatewayOp::Hello,
        Some(11) => GatewayOp::HeartbeatAck,
        Some(9) => GatewayOp::InvalidSession,
        _ => GatewayOp::Other,
    }
}

/// The heartbeat interval a `HELLO` names, in milliseconds.
pub fn heartbeat_interval_ms(frame: &Value) -> Option<u64> {
    frame
        .get("d")
        .and_then(|d| d.get("heartbeat_interval"))
        .and_then(Value::as_u64)
}

/// The connection state the dispatch stream carries.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Session {
    /// The session id `READY` named, kept for a `RESUME` reconnect.
    pub session_id: Option<String>,
    /// The last dispatch sequence seen; the resume point.
    pub last_seq: Option<u64>,
}

impl Session {
    /// Forget the session after QQ says it cannot be resumed (`op 9, d=false`).
    pub fn invalidate(&mut self) {
        self.session_id = None;
        self.last_seq = None;
    }

    /// Record one dispatch's sequence.
    pub fn note_dispatch(&mut self, frame: &Value) {
        if let Some(seq) = frame.get("s").and_then(Value::as_u64) {
            self.last_seq = Some(self.last_seq.map_or(seq, |last| last.max(seq)));
        }
        if frame.get("t").and_then(Value::as_str) == Some("READY") {
            self.session_id = frame
                .pointer("/d/session_id")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
    }

    /// The heartbeat answer to send now.
    pub fn heartbeat(&self) -> Value {
        json!({ "op": 1, "d": self.last_seq })
    }

    /// The `RESUME` payload a reconnect sends instead of `IDENTIFY`.
    pub fn resume(&self, token: &str) -> Value {
        json!({
            "op": 6,
            "d": {
                "token": format!("QQBot {token}"),
                "session_id": self.session_id,
                "seq": self.last_seq,
            }
        })
    }
}

/// A live gateway connection: the socket, the session state, and the
/// interval the server asked the client to heartbeat on.
pub struct Gateway {
    sink: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    pub session: Session,
    pub heartbeat_ms: u64,
}

impl Gateway {
    /// Connect, answer `HELLO` with `IDENTIFY`, and wait for `READY`.
    pub async fn connect(url: &str, token: &str, identify: Value) -> Result<Self> {
        let (socket, _response) = tokio_tungstenite::connect_async(url)
            .await
            .with_context(|| format!("connecting to {url}"))?;
        let mut gateway = Self {
            sink: socket,
            session: Session::default(),
            heartbeat_ms: 30_000,
        };
        // HELLO names the heartbeat interval and must arrive first.
        let hello = gateway.next_json().await.with_context(
            || "the gateway closed before its HELLO arrived; the credential may be wrong",
        )?;
        if classify(&hello) != GatewayOp::Hello {
            bail!("the gateway's first frame was not a HELLO: {hello}");
        }
        gateway.heartbeat_ms = heartbeat_interval_ms(&hello).unwrap_or(30_000);
        gateway.send_json(&identify).await?;
        let ready = gateway
            .next_json()
            .await
            .context("waiting for READY after IDENTIFY")?;
        gateway.session.note_dispatch(&ready);
        if gateway.session.session_id.is_none() {
            bail!("the gateway answered IDENTIFY with no READY session id: {ready}");
        }
        let _ = token;
        Ok(gateway)
    }

    /// One text frame as JSON, skipping pings/pongs and binary frames.
    async fn next_json(&mut self) -> Result<Value> {
        loop {
            let message = self
                .sink
                .next()
                .await
                .transpose()?
                .context("the gateway websocket closed")?;
            match message {
                Message::Text(text) => {
                    return serde_json::from_str(&text)
                        .with_context(|| format!("a gateway frame is not JSON: {text}"));
                }
                Message::Ping(_) | Message::Pong(_) | Message::Binary(_) => continue,
                Message::Close(frame) => {
                    bail!("the gateway closed the connection: {frame:?}");
                }
                Message::Frame(_) => continue,
            }
        }
    }

    /// Send one JSON frame.
    pub async fn send_json(&mut self, value: &Value) -> Result<()> {
        self.sink
            .send(Message::text(value.to_string()))
            .await
            .context("writing to the gateway websocket")
    }

    /// The next server dispatch, or `None` when the gateway closed.
    ///
    /// The heartbeat is the client's job: QQ (like the protocol it inherits
    /// from) has the client send `op 1` every interval `HELLO` named, and never
    /// asks for one itself. A socket that says nothing is reset without a close
    /// handshake — measured on a live bot at about a minute, logged as
    /// `the qqbot gateway stream failed: WebSocket protocol error: Connection
    /// reset without closing handshake`. So the read carries a deadline: when it
    /// expires the heartbeat goes out and the read resumes. A server `op 1` is
    /// answered the same way, in case one ever arrives.
    ///
    /// `next_json` is cancellation-safe — frames already read stay in the
    /// stream's buffer — so the timeout loses nothing.
    pub async fn next_dispatch(&mut self) -> Result<Option<Value>> {
        let interval = Duration::from_millis(self.heartbeat_ms.max(1_000));
        loop {
            let frame = match tokio::time::timeout(interval, self.next_json()).await {
                Ok(Ok(frame)) => frame,
                Ok(Err(error)) => {
                    // A dead websocket is the caller's reconnect problem, and
                    // the session state survives it for the RESUME.
                    return if self.session.session_id.is_some() {
                        Err(error)
                    } else {
                        Ok(None)
                    };
                }
                Err(_elapsed) => {
                    self.send_json(&self.session.heartbeat()).await?;
                    continue;
                }
            };
            match classify(&frame) {
                GatewayOp::Dispatch => {
                    self.session.note_dispatch(&frame);
                    return Ok(Some(frame));
                }
                GatewayOp::Heartbeat => {
                    self.send_json(&self.session.heartbeat()).await?;
                }
                GatewayOp::HeartbeatAck | GatewayOp::Other => {}
                GatewayOp::Reconnect => {
                    // The server is retiring this connection; a clean `None`
                    // ends this stream and the caller reconnects with RESUME.
                    return Ok(None);
                }
                GatewayOp::InvalidSession => {
                    // QQ's `d=false` means the old session is dead. Do not
                    // feed its id back into RESUME: the next connection must
                    // send IDENTIFY, or it will loop on op 9 forever.
                    self.session.invalidate();
                    return Ok(None);
                }
                GatewayOp::Hello => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ops_classify_by_their_number() {
        for (op, expected) in [
            (0, GatewayOp::Dispatch),
            (1, GatewayOp::Heartbeat),
            (7, GatewayOp::Reconnect),
            (9, GatewayOp::InvalidSession),
            (10, GatewayOp::Hello),
            (11, GatewayOp::HeartbeatAck),
            (2, GatewayOp::Other),
            (13, GatewayOp::Other),
        ] {
            assert_eq!(classify(&json!({"op": op})), expected, "op {op}");
        }
        assert_eq!(classify(&json!({})), GatewayOp::Other);
    }

    #[test]
    fn the_hello_interval_is_read_from_d() {
        assert_eq!(
            heartbeat_interval_ms(&json!({"op": 10, "d": {"heartbeat_interval": 45000}})),
            Some(45_000)
        );
        assert_eq!(heartbeat_interval_ms(&json!({"op": 10})), None);
    }

    #[test]
    fn dispatch_sequences_advance_and_survive_out_of_order() {
        let mut session = Session::default();
        session.note_dispatch(&json!({"op": 0, "s": 4, "t": "GROUP_MESSAGE_CREATE"}));
        assert_eq!(session.last_seq, Some(4));
        // A RESUME reply may carry an older `s`; the max is what survives.
        session.note_dispatch(&json!({"op": 0, "s": 2}));
        assert_eq!(session.last_seq, Some(4));
    }

    #[test]
    fn ready_names_the_session() {
        let mut session = Session::default();
        session.note_dispatch(&json!({
            "op": 0, "s": 1, "t": "READY",
            "d": {"session_id": "sess-9", "user": {"id": "bot"}}
        }));
        assert_eq!(session.session_id.as_deref(), Some("sess-9"));
        assert_eq!(session.last_seq, Some(1));
    }

    #[test]
    fn op9_invalidates_the_session_before_reconnect() {
        let mut session = Session::default();
        session.note_dispatch(&json!({
            "op": 0, "s": 4, "t": "READY",
            "d": {"session_id": "rejected-session"}
        }));
        assert_eq!(
            classify(&json!({"op": 9, "d": false})),
            GatewayOp::InvalidSession
        );

        // op 9, d=false must lead to IDENTIFY, not a RESUME carrying the dead id.
        session.invalidate();
        assert_eq!(session.session_id, None);
        assert_eq!(session.last_seq, None);
    }

    #[test]
    fn the_heartbeat_carries_the_last_seq() {
        let mut session = Session::default();
        session.note_dispatch(&json!({"op": 0, "s": 7}));
        assert_eq!(session.heartbeat(), json!({"op": 1, "d": 7}));
        // No dispatch yet: `d` is null, which the protocol reads as "nothing".
        assert_eq!(Session::default().heartbeat(), json!({"op": 1, "d": null}));
    }

    #[test]
    fn the_resume_carries_token_session_and_seq() {
        let mut session = Session::default();
        session.note_dispatch(&json!({
            "op": 0, "s": 3, "t": "READY", "d": {"session_id": "sess-1"}
        }));
        let resume = session.resume("abc");
        assert_eq!(resume["op"], 6);
        assert_eq!(resume["d"]["token"], "QQBot abc");
        assert_eq!(resume["d"]["session_id"], "sess-1");
        assert_eq!(resume["d"]["seq"], 3);
    }

    #[test]
    fn the_identify_carries_token_and_intents() {
        let api = crate::api::QqApi::new(
            crate::credential::QqCredentials::parse("id:secret").unwrap(),
            crate::api::Endpoint::Production,
        );
        let identify = api.identify_payload("tok", None, None);
        assert_eq!(identify["op"], 2);
        assert_eq!(identify["d"]["token"], "QQBot tok");
        assert!(identify["d"]["intents"].as_u64().unwrap() > 0);
    }
}
