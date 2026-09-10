//! The multiplexed frame wrapper (§4).
//!
//! One connection carries requests, responses, events, heartbeats, and the
//! shutdown notice. The discriminant is the short `f` key so the hot path stays
//! cheap on the wire.

use crate::event::Event;
use crate::ops::{AdminOp, ClientOp, GatewayOp};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Ceiling for one human-readable error message on the wire.
pub const MAX_ERROR_MESSAGE_BYTES: usize = 4096;

/// The exact wording returned when a reused `op_id` carries different content.
pub const OP_ID_CONFLICT_MESSAGE: &str = "op_id conflict: request differs from durable receipt";

/// The response body carried by [`Frame::Res`].
///
/// `ok = true` pairs with `data`; `ok = false` pairs with `error`. A duplicate
/// `op_id` answers with both: the rejection plus the original receipt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub struct ResBody {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorPayload>,
}

impl ResBody {
    pub fn ok(data: serde_json::Value) -> Self {
        ResBody {
            ok: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn err(code: ErrorCode, message: impl Into<String>, field: Option<String>) -> Self {
        ResBody {
            ok: false,
            data: None,
            error: Some(ErrorPayload {
                code,
                message: message.into(),
                field,
            }),
        }
    }

    /// A rejection that also hands back the earlier successful answer, so the
    /// sender can settle its intent from the replayed receipt.
    pub fn err_with_data(
        code: ErrorCode,
        message: impl Into<String>,
        field: Option<String>,
        data: serde_json::Value,
    ) -> Self {
        ResBody {
            ok: false,
            data: Some(data),
            error: Some(ErrorPayload {
                code,
                message: message.into(),
                field,
            }),
        }
    }

    pub fn data(&self) -> Option<&serde_json::Value> {
        self.data.as_ref()
    }
}

/// A wire error: closed code, human message, optional offending field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct ErrorPayload {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

impl ErrorPayload {
    /// Shorten the message to [`MAX_ERROR_MESSAGE_BYTES`] at a char boundary.
    pub fn trimmed(mut self) -> Self {
        if self.message.len() > MAX_ERROR_MESSAGE_BYTES {
            let mut cut = MAX_ERROR_MESSAGE_BYTES;
            while cut > 0 && !self.message.is_char_boundary(cut) {
                cut -= 1;
            }
            self.message.truncate(cut);
        }
        self
    }
}

/// The complete set of wire error codes, all lowercase with underscores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// A field or argument failed a protocol rule.
    Invalid,
    /// The `op` name is unknown to this connection's vocabulary.
    UnknownOp,
    /// The spec forbids this sender for this target and kind.
    AclDenied,
    /// No role by that name exists in the spec.
    UnknownRole,
    /// The recipient's client is not connected and the kind cannot queue.
    RecipientOffline,
    /// This `op_id` was already accepted with an identical fingerprint.
    Duplicate,
    /// This `op_id` was already accepted with a different fingerprint.
    Conflict,
    /// Handshake identity failed: unregistered key or bad signature.
    Unauthorized,
    /// Connected and registered, yet forbidden from this action.
    Forbidden,
    /// The action needs `admin = true` in the spec.
    NotAdmin,
    /// A frame exceeded the negotiated byte ceiling.
    FrameTooLarge,
    /// A frame failed to decode.
    BadFrame,
    /// Protocol revision outside the accepted range.
    ProtocolVersion,
    /// Ledger or runtime failure inside the daemon.
    Internal,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::Invalid => "invalid",
            ErrorCode::UnknownOp => "unknown_op",
            ErrorCode::AclDenied => "acl_denied",
            ErrorCode::UnknownRole => "unknown_role",
            ErrorCode::RecipientOffline => "recipient_offline",
            ErrorCode::Duplicate => "duplicate",
            ErrorCode::Conflict => "conflict",
            ErrorCode::Unauthorized => "unauthorized",
            ErrorCode::Forbidden => "forbidden",
            ErrorCode::NotAdmin => "not_admin",
            ErrorCode::FrameTooLarge => "frame_too_large",
            ErrorCode::BadFrame => "bad_frame",
            ErrorCode::ProtocolVersion => "protocol_version",
            ErrorCode::Internal => "internal",
        }
    }

    pub const ALL: [ErrorCode; 14] = [
        ErrorCode::Invalid,
        ErrorCode::UnknownOp,
        ErrorCode::AclDenied,
        ErrorCode::UnknownRole,
        ErrorCode::RecipientOffline,
        ErrorCode::Duplicate,
        ErrorCode::Conflict,
        ErrorCode::Unauthorized,
        ErrorCode::Forbidden,
        ErrorCode::NotAdmin,
        ErrorCode::FrameTooLarge,
        ErrorCode::BadFrame,
        ErrorCode::ProtocolVersion,
        ErrorCode::Internal,
    ];

    /// Codes where retrying the same request can never succeed.
    pub fn is_permanent(self) -> bool {
        !matches!(
            self,
            ErrorCode::RecipientOffline
                | ErrorCode::Duplicate
                | ErrorCode::Unauthorized
                | ErrorCode::Internal
        )
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::error::Error for ErrorCode {}

/// One multiplexed frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "f")]
pub enum Frame<R = ClientOp> {
    /// A request from a client, gateway, or admin connection.
    Req {
        id: String,
        #[serde(flatten)]
        op: R,
    },
    /// The reply to a [`Frame::Req`], matched by `id`.
    Res {
        id: String,
        #[serde(flatten)]
        body: ResBody,
    },
    /// An observation-plane push; `seq` is monotonic per server.
    Ev {
        seq: u64,
        #[serde(flatten)]
        event: Event,
    },
    /// Confirmation that the peer settled a delivery or event.
    Ack { seq: u64 },
    /// Liveness probe carrying the sender's clock.
    Ping { t: i64 },
    /// Liveness answer echoing `t` and reporting the server's event cursor.
    Pong { t: i64, server_seq: u64 },
    /// Ordered shutdown with a reason.
    Bye { reason: String },
}

/// Admin request frames share the same wrapper and admin op vocabulary.
pub type AdminFrame = Frame<AdminOp>;

/// Gateway request frames share the same wrapper and gateway op vocabulary.
pub type GatewayFrame = Frame<GatewayOp>;

impl<R> Frame<R> {
    pub fn req(id: impl Into<String>, op: R) -> Self {
        Frame::Req {
            id: id.into(),
            op,
        }
    }

    /// The request id, when this frame opens or answers one.
    pub fn id(&self) -> Option<&str> {
        match self {
            Frame::Req { id, .. } | Frame::Res { id, .. } => Some(id),
            Frame::Ev { .. }
            | Frame::Ack { .. }
            | Frame::Ping { .. }
            | Frame::Pong { .. }
            | Frame::Bye { .. } => None,
        }
    }

    pub fn is_response(&self) -> bool {
        matches!(self, Frame::Res { .. })
    }
}

impl Frame {
    pub fn res(id: impl Into<String>, body: ResBody) -> Self {
        Frame::Res {
            id: id.into(),
            body,
        }
    }

    pub fn ok(id: impl Into<String>, data: serde_json::Value) -> Self {
        Frame::res(id, ResBody::ok(data))
    }

    pub fn error(
        id: impl Into<String>,
        code: ErrorCode,
        message: impl Into<String>,
        field: Option<String>,
    ) -> Self {
        Frame::res(id, ResBody::err(code, message, field))
    }

    pub fn event(seq: u64, event: Event) -> Self {
        Frame::Ev { seq, event }
    }

    pub fn bye(reason: impl Into<String>) -> Self {
        Frame::Bye {
            reason: reason.into(),
        }
    }
}



#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{
        Body, Causality, Envelope, MsgKind, Principal, new_envelope, new_task_id,
    };
    use crate::event::{EventTier, Presence, RolePresence, SessionStateEvent};
    use crate::ops::{
        AckArgs, AdminOp, ByeArgs, ControlArgs, GatewayOp, HandshakeArgs, LedgerQuery, PullArgs,
        QueryFaultsArgs, QueryRolesArgs, QuerySessionsArgs, RegisterChannelArgs, Report,
        SessionProjection, SessionSyncArgs, Subscribe,
    };
    use crate::{PROTOCOL_VERSION, envelope::ControlOp};

    fn task() -> Envelope {
        new_envelope(
            MsgKind::Task,
            Principal::role("planner"),
            Principal::role("builder"),
            Body::text("build it"),
            Some(Causality::root(new_task_id())),
        )
        .expect("valid task")
    }

    #[test]
    fn request_frame_places_op_and_args_beside_id() {
        let frame = Frame::req("r1", ClientOp::Send(Box::new(task())));
        let json = serde_json::to_string(&frame).expect("encode");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_eq!(value["f"], "req");
        assert_eq!(value["id"], "r1");
        assert_eq!(value["op"], "send");
        assert_eq!(value["args"]["kind"], "task");
        let back: Frame = serde_json::from_str(&json).expect("decode");
        assert_eq!(back, frame);
    }

    #[test]
    fn response_frame_matches_the_documented_shape() {
        let frame = Frame::ok("r1", serde_json::json!({"state": "in_flight"}));
        let value = serde_json::to_value(&frame).expect("encode");
        assert_eq!(
            value,
            serde_json::json!({
                "f": "res",
                "id": "r1",
                "ok": true,
                "data": {"state": "in_flight"}
            })
        );
    }

    #[test]
    fn error_response_names_code_and_field() {
        let frame = Frame::error(
            "r1",
            ErrorCode::AclDenied,
            "planner may not send to reviewer",
            Some("to.role".to_string()),
        );
        let value = serde_json::to_value(&frame).expect("encode");
        assert_eq!(value["ok"], false);
        assert_eq!(value["error"]["code"], "acl_denied");
        assert_eq!(value["error"]["field"], "to.role");
        let back: Frame = serde_json::from_value(value).expect("decode");
        assert_eq!(back, frame);
    }

    #[test]
    fn a_duplicate_rejection_still_carries_the_original_data() {
        let body = ResBody::err_with_data(
            ErrorCode::Duplicate,
            "already accepted",
            None,
            serde_json::json!({"msg_id": "m1"}),
        );
        let value = serde_json::to_value(&body).expect("encode");
        assert_eq!(value["ok"], false);
        assert_eq!(value["data"]["msg_id"], "m1");
        assert_eq!(value["error"]["code"], "duplicate");
    }

    #[test]
    fn every_error_code_round_trips_through_its_wire_name() {
        assert_eq!(ErrorCode::ALL.len(), 14);
        for code in ErrorCode::ALL {
            let value = serde_json::to_value(code).expect("encode");
            assert_eq!(value, serde_json::Value::String(code.as_str().to_string()));
            let back: ErrorCode = serde_json::from_value(value).expect("decode");
            assert_eq!(back, code);
        }
    }

    #[test]
    fn transient_codes_stay_retryable() {
        for code in [
            ErrorCode::RecipientOffline,
            ErrorCode::Duplicate,
            ErrorCode::Unauthorized,
            ErrorCode::Internal,
        ] {
            assert!(!code.is_permanent(), "{code} must stay retryable");
        }
        for code in [
            ErrorCode::Invalid,
            ErrorCode::AclDenied,
            ErrorCode::UnknownRole,
            ErrorCode::Conflict,
            ErrorCode::NotAdmin,
            ErrorCode::ProtocolVersion,
        ] {
            assert!(code.is_permanent(), "{code} can never succeed on retry");
        }
    }

    #[test]
    fn event_frame_keeps_seq_beside_the_typed_event() {
        let event = Event::SessionState(SessionStateEvent {
            task_id: new_task_id(),
            role: "builder".into(),
            session_id: "s1".into(),
            generation: 1,
            seq: 14,
            projection: SessionProjection::default_working(),
        });
        let value = serde_json::to_value(Frame::event(41, event.clone())).expect("encode");
        assert_eq!(value["f"], "ev");
        assert_eq!(value["seq"], 41);
        assert_eq!(value["type"], "session_state");
        assert!(value["data"].is_object());
        let back: Frame = serde_json::from_value(value).expect("decode");
        assert_eq!(back, Frame::event(41, event));
    }

    #[test]
    fn presence_events_carry_the_draining_state() {
        let event = Event::RolePresence(RolePresence {
            role: "builder".into(),
            state: Presence::Draining,
            aggregate: None,
            sessions: 2,
            detail: None,
        });
        let value = serde_json::to_value(&event).expect("encode");
        assert_eq!(value["type"], "role_presence");
        assert_eq!(value["data"]["state"], "draining");
    }

    #[test]
    fn heartbeat_frames_carry_the_server_cursor() {
        let value = serde_json::to_value(Frame::<ClientOp>::Pong { t: 7, server_seq: 42 }).expect("encode");
        assert_eq!(
            value,
            serde_json::json!({"f": "pong", "t": 7, "server_seq": 42})
        );
    }

    #[test]
    fn bye_and_ack_frames_open_no_request() {
        let bye = Frame::bye("shutdown");
        assert_eq!(bye.id(), None);
        assert!(!bye.is_response());
        let value = serde_json::to_value(&bye).expect("encode");
        assert_eq!(value, serde_json::json!({"f": "bye", "reason": "shutdown"}));
        let ack: Frame = Frame::Ack { seq: 41 };
        assert_eq!(
            serde_json::to_value(ack).expect("encode"),
            serde_json::json!({"f": "ack", "seq": 41})
        );
    }

    #[test]
    fn pull_requests_survive_the_frame_wrapper() {
        let frame = Frame::req(
            "r9",
            ClientOp::Pull(PullArgs {
                role: None,
                limit: 16,
                hold_ms: Some(500),
            }),
        );
        let back: Frame = serde_json::from_value(serde_json::to_value(&frame).expect("e"))
            .expect("decode");
        assert_eq!(back, frame);
        assert_eq!(back.id(), Some("r9"));
    }

    #[test]
    fn error_messages_are_capped_at_a_char_boundary() {
        let payload = ErrorPayload {
            code: ErrorCode::Internal,
            message: "é".repeat(MAX_ERROR_MESSAGE_BYTES),
            field: None,
        };
        let trimmed = payload.trimmed();
        assert!(trimmed.message.len() <= MAX_ERROR_MESSAGE_BYTES);
        assert!(trimmed.message.is_char_boundary(trimmed.message.len()));
    }

    #[test]
    fn conflict_wording_is_pinned_to_the_ledger_text() {
        assert_eq!(
            OP_ID_CONFLICT_MESSAGE,
            "op_id conflict: request differs from durable receipt"
        );
    }

    #[test]
    fn every_client_op_survives_the_frame_wrapper() {
        let ops = vec![
            ClientOp::Hello(HandshakeArgs {
                protocol: PROTOCOL_VERSION,
                role: "planner".into(),
                key: "ed25519/AAA".into(),
                signature: "sig".into(),
                agent: "onlyne-client".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                aggregate: false,
            }),
            ClientOp::Send(Box::new(task())),
            ClientOp::Pull(PullArgs {
                role: None,
                limit: 32,
                hold_ms: Some(250),
            }),
            ClientOp::Ack(AckArgs {
                msg_id: "m1".into(),
                op_id: None,
                accepted: true,
                reason: None,
            }),
            ClientOp::Report(Report::Ready {
                task_id: new_task_id(),
                session_id: "s".into(),
                generation: 1,
                seq: 3,
            }),
            ClientOp::SessionSync(SessionSyncArgs {
                task_id: new_task_id(),
                session_id: "s".into(),
                generation: 1,
                seq: 4,
                projection: SessionProjection::default_working(),
            }),
            ClientOp::Subscribe(Subscribe {
                since_seq: 7,
                tiers: vec![EventTier::Durable],
                kinds: vec![],
                roles: vec![],
            }),
            ClientOp::QueryLedger(LedgerQuery::default()),
            ClientOp::QuerySessions(QuerySessionsArgs::default()),
            ClientOp::QueryRoles(QueryRolesArgs::default()),
            ClientOp::QueryFaults(QueryFaultsArgs::default()),
            ClientOp::Control(ControlArgs {
                to: Some("builder".into()),
                op: ControlOp::Probe {
                    task_id: new_task_id(),
                },
            }),
            ClientOp::Bye(ByeArgs {
                reason: "shutdown".into(),
                drain_ms: None,
            }),
        ];
        assert_eq!(ops.len(), 13);
        for (index, op) in ops.into_iter().enumerate() {
            let id = format!("r{index}");
            let frame = Frame::req(&id, op);
            let value = serde_json::to_value(&frame).expect("encode");
            assert_eq!(value["f"], "req");
            assert_eq!(value["id"], id);
            assert!(value.get("op").is_some(), "op sits beside id");
            assert!(value.get("args").is_some(), "args sits beside id");
            let back: Frame = serde_json::from_value(value).expect("decode");
            assert_eq!(back, frame);
        }
    }

    #[test]
    fn admin_and_gateway_frames_share_the_request_layout() {
        let admin: AdminFrame = Frame::req("a1", AdminOp::Roles(QueryRolesArgs::default()));
        let admin_value = serde_json::to_value(&admin).expect("admin frame encodes");
        assert_eq!(admin_value["f"], "req");
        assert_eq!(admin_value["id"], "a1");
        assert_eq!(admin_value["op"], "roles");
        assert!(admin_value.get("args").is_some());
        let admin_back: AdminFrame = serde_json::from_value(admin_value).expect("admin decodes");
        assert_eq!(admin_back, admin);

        let gateway: GatewayFrame = Frame::req(
            "g1",
            GatewayOp::RegisterChannel(RegisterChannelArgs {
                platform: "telegram".into(),
                channel: "telegram".into(),
                conversations: None,
            }),
        );
        let gateway_value = serde_json::to_value(&gateway).expect("gateway frame encodes");
        assert_eq!(gateway_value["f"], "req");
        assert_eq!(gateway_value["id"], "g1");
        assert_eq!(gateway_value["op"], "register_channel");
        assert!(gateway_value.get("args").is_some());
        let gateway_back: GatewayFrame =
            serde_json::from_value(gateway_value).expect("gateway decodes");
        assert_eq!(gateway_back, gateway);
    }

    #[test]
    fn client_frame_rejects_admin_request_vocab() {
        let admin: AdminFrame = Frame::req("a2", AdminOp::Roles(QueryRolesArgs::default()));
        let value = serde_json::to_value(admin).expect("admin frame encodes");
        let err = serde_json::from_value::<Frame>(value).expect_err("client vocab rejects admin op");
        assert!(err.to_string().contains("roles"), "err = {err}");
    }
}
