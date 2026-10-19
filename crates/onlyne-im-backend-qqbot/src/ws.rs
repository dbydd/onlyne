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
use std::collections::VecDeque;
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

    /// Process one frame received while waiting for the `IDENTIFY`/`RESUME`
    /// handshake to complete.
    ///
    /// - `Ok(Some(true))`: handshake succeeded (`READY` with a `session_id`, or
    ///   `RESUMED` with an existing `session_id`).
    /// - `Ok(Some(false))`: non-terminal frame handled (event dispatch buffered
    ///   into `buffered` so no replayed message is dropped, or `HeartbeatAck`).
    /// - `Ok(None)`: server requested an immediate `Heartbeat` (`op 1`).
    /// - `Err(_)`: handshake rejected (`op 9` invalidates the session when
    ///   `d != true`, or `op 7` asks for reconnect).
    pub fn accept_handshake_frame(
        &mut self,
        frame: Value,
        buffered: &mut VecDeque<Value>,
    ) -> Result<Option<bool>> {
        match classify(&frame) {
            GatewayOp::Dispatch => {
                self.note_dispatch(&frame);
                match frame.get("t").and_then(Value::as_str) {
                    Some("READY") => {
                        if self.session_id.is_none() {
                            bail!("the gateway answered IDENTIFY with no READY session id: {frame}");
                        }
                        Ok(Some(true))
                    }
                    Some("RESUMED") => {
                        if self.session_id.is_none() {
                            bail!("the gateway answered RESUME with RESUMED but no prior session id was held: {frame}");
                        }
                        Ok(Some(true))
                    }
                    _ => {
                        buffered.push_back(frame);
                        if self.session_id.is_some() {
                            // During a RESUME or active session, an event frame arriving
                            // means the stream is live; keep it buffered and finish handshake.
                            Ok(Some(true))
                        } else {
                            Ok(Some(false))
                        }
                    }
                }
            }
            GatewayOp::Heartbeat => Ok(None),
            GatewayOp::HeartbeatAck | GatewayOp::Hello | GatewayOp::Other => Ok(Some(false)),
            GatewayOp::InvalidSession => {
                let resumable = frame.get("d").and_then(Value::as_bool).unwrap_or(false);
                if !resumable {
                    self.invalidate();
                }
                bail!("the gateway rejected the session during handshake (op 9, resumable={resumable}): {frame}");
            }
            GatewayOp::Reconnect => {
                bail!("the gateway asked to reconnect during handshake (op 7): {frame}");
            }
        }
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
    buffered: VecDeque<Value>,
}

impl Gateway {
    /// Connect, answer `HELLO` with `IDENTIFY` or `RESUME`, and wait for the
    /// handshake to complete (`READY` or `RESUMED`), buffering any missed
    /// dispatch frames that arrive along the way.
    pub async fn connect(
        url: &str,
        token: &str,
        identify: Value,
        prior: Option<Session>,
    ) -> Result<Self> {
        let (socket, _response) = tokio_tungstenite::connect_async(url)
            .await
            .with_context(|| format!("connecting to {url}"))?;
        let mut gateway = Self {
            sink: socket,
            session: prior.unwrap_or_default(),
            heartbeat_ms: 30_000,
            buffered: VecDeque::new(),
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
        loop {
            let frame = gateway
                .next_json()
                .await
                .context("waiting for READY or RESUMED after IDENTIFY/RESUME")?;
            if let Some(done) = gateway.session.accept_handshake_frame(frame, &mut gateway.buffered)? {
                if done {
                    break;
                }
            } else {
                // Heartbeat request during handshake.
                gateway.send_json(&gateway.session.heartbeat()).await?;
            }
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
        if let Some(frame) = self.buffered.pop_front() {
            return Ok(Some(frame));
        }
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

/// How long to wait before dialing QQ's event gateway again.
///
/// Doubling from a second and capped at a minute: long enough that a platform
/// outage is not hammered, short enough that a bot is back while the operator
/// is still looking at the log. The jitter comes from the clock rather than a
/// random source — the same cheap trick `render::starting_sequence` uses — so
/// bots that lost the gateway together do not all return in the same second.
pub fn reconnect_delay(attempt: u32) -> Duration {
    let base_ms = 1_000u64.saturating_mul(1u64 << attempt.min(6));
    let capped = base_ms.min(60_000);
    let jitter = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_millis() as u64)
        .unwrap_or(0))
        % 1_000;
    Duration::from_millis(capped + jitter)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_reconnect_delay_doubles_up_to_a_minute_and_is_jittered() {
        // The ladder, not the exact millisecond: the jitter is the clock's.
        assert!(reconnect_delay(0).as_millis() < 2_000);
        assert!(reconnect_delay(1).as_millis() >= 2_000);
        assert!(reconnect_delay(2).as_millis() >= 4_000);
        // Past the cap it holds, so a platform that stays down is not hammered
        // and a bot that comes back is back within a minute.
        for attempt in [6, 7, 20, u32::MAX] {
            assert!(reconnect_delay(attempt).as_millis() <= 61_000);
        }
    }

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
    fn resumed_and_replayed_events_complete_handshake_without_dropping_messages() {
        let mut session = Session {
            session_id: Some("sess-live-1".to_string()),
            last_seq: Some(1),
        };
        let mut buffered = VecDeque::new();

        // 1. A RESUMED frame with empty `d` succeeds and preserves `session_id`.
        let done = session
            .accept_handshake_frame(
                json!({"op": 0, "s": 2, "t": "RESUMED", "d": ""}),
                &mut buffered,
            )
            .expect("RESUMED is valid");
        assert_eq!(done, Some(true));
        assert_eq!(session.session_id.as_deref(), Some("sess-live-1"));
        assert_eq!(session.last_seq, Some(2));
        assert!(buffered.is_empty());

        // 2. A C2C_MESSAGE_CREATE arriving before READY/RESUMED is buffered and never dropped.
        let mut fresh = Session::default();
        let msg = json!({
            "op": 0,
            "s": 9,
            "t": "C2C_MESSAGE_CREATE",
            "d": {"id": "m-9", "content": "来看看qq，能收到吗"}
        });
        let step1 = fresh
            .accept_handshake_frame(msg.clone(), &mut buffered)
            .expect("event buffered while waiting for READY");
        assert_eq!(step1, Some(false));
        assert_eq!(buffered.len(), 1);
        let step2 = fresh
            .accept_handshake_frame(
                json!({"op": 0, "s": 10, "t": "READY", "d": {"session_id": "sess-new"}}),
                &mut buffered,
            )
            .expect("READY completes handshake");
        assert_eq!(step2, Some(true));
        assert_eq!(fresh.session_id.as_deref(), Some("sess-new"));
        assert_eq!(fresh.last_seq, Some(10));
        assert_eq!(buffered.pop_front(), Some(msg));

        // 3. op 9, d=false during handshake invalidates the prior session so the next attempt sends IDENTIFY.
        let mut dead = Session {
            session_id: Some("sess-stale".to_string()),
            last_seq: Some(5),
        };
        let err = dead
            .accept_handshake_frame(json!({"op": 9, "d": false}), &mut buffered)
            .unwrap_err();
        assert!(err.to_string().contains("op 9"));
        assert_eq!(dead.session_id, None);
        assert_eq!(dead.last_seq, None);
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
