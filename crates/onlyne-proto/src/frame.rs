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
/// `ok = true` carries `data` and `ok = false` carries `error`, and neither may
/// omit its own half. The one pair is the duplicate `op_id` replay: `ok = false`
/// with an `error` naming `duplicate` and the replayed receipt in `data`, which
/// is the shape `docs/v1-PLAN.md` case 3 at line 502 compares byte for byte.
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

    /// What a caller should do about this code.
    ///
    /// One vocabulary, shared with the network layer's own failures, so a
    /// refusal read on the wire and a failure read at the socket are answered
    /// by the same rule. Two functions with the same name and opposite
    /// verdicts — which is what this and `onlyne_net::is_permanent` were —
    /// left a reader to work out which one applied before they could act.
    pub fn retry(self) -> Retry {
        match self {
            // The recipient may come back, and the server may be busy.
            ErrorCode::RecipientOffline | ErrorCode::Internal => Retry::UnderBackoff,
            // The request landed; asking again returns the same receipt.
            ErrorCode::Duplicate => Retry::Never,
            // A key that is not this role's, and a protocol the peer does not
            // speak, are both fixed by a person: re-key, or upgrade one side.
            ErrorCode::Unauthorized | ErrorCode::ProtocolVersion => Retry::AfterHuman,
            _ => Retry::Never,
        }
    }
}

/// What a caller should do about a failure.
///
/// The three answers are different actions, not degrees of one. `Never` means
/// the same call cannot succeed as it stands. `AfterHuman` means it cannot
/// succeed until someone re-keys a role or upgrades one side, and retrying
/// through that is a loop that never ends. `UnderBackoff` is the only one a
/// retry loop acts on by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retry {
    /// Do not retry. The call cannot succeed as it stands.
    Never,
    /// Do not retry yet: a person has to re-key a role or upgrade a side first.
    AfterHuman,
    /// Retry under the backoff schedule.
    UnderBackoff,
}

impl Retry {
    /// Whether a retry loop should keep going on its own.
    pub fn is_transient(self) -> bool {
        matches!(self, Retry::UnderBackoff)
    }

    /// Whether the run should end here, and whether a person is why.
    pub fn ends_the_run(self) -> bool {
        !self.is_transient()
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
    /// An observation-plane push; `seq` is monotonic per server. The event is
    /// boxed so an ack, ping, or bye frame does not carry its 336 bytes.
    Ev {
        seq: u64,
        #[serde(flatten)]
        event: Box<Event>,
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
        Frame::Req { id: id.into(), op }
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
        Frame::Ev {
            seq,
            event: Box::new(event),
        }
    }

    pub fn bye(reason: impl Into<String>) -> Self {
        Frame::Bye {
            reason: reason.into(),
        }
    }
}
