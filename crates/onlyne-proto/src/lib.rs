//! Onlyne v1 wire protocol (decisions D3, D4, D8, D9, D10, D11, D16).
//!
//! Pure data: every type here is serde-only, so the protocol travels into the
//! server, the client, the gateway plugins, and the machine-readable schema
//! export from one definition. This crate holds tokio, a database, and a
//! transport in none of its code paths.
//!
//! Layers, bottom up:
//! - [`envelope`]: the unified message (§3). Text plus at most one inline image.
//! - [`frame`]: the multiplexed request/response/event wrapper (§4) and the closed
//!   [`frame::ErrorCode`] set.
//! - [`event`]: the observation plane (§9), at-most-once with cursor resync.
//! - [`ops`]: the client-to-server and admin vocabularies (§6, §8).
//! - [`adapter`]: the one adapter protocol mounted on both sides (§7).
//!
//! Compatibility posture: [`PROTOCOL_VERSION`] is checked at handshake and a
//! mismatch answers [`frame::ErrorCode::ProtocolVersion`]. Legacy layouts and
//! legacy databases are refused at startup by the binaries that load them.
//!
//! ## Frame typing
//!
//! [`Frame<R = ClientOp>`] is the single wrapper for client, admin, and gateway surfaces.
//! [`AdminFrame`] and [`GatewayFrame`] provide typed aliases for the admin and gateway vocabularies.
//! All three surfaces use the same JSON layout with `f`, `id`, `op`, and `args` fields.
//! Cross-vocabulary decoding fails by design and keeps each socket vocabulary closed.

pub mod adapter;
pub mod envelope;
pub mod event;
pub mod frame;
pub mod ops;

pub use adapter::{
    AdapterMsg, AgentMount, AssignAckArgs, AssignArgs, ByeNotice, Capability, ClusterMount,
    ConfigGetArgs, DetachArgs, GatewayBinding, GatewayMount, HELLO_REQUIRED_MESSAGE,
    HELLO_TIMEOUT_MS, HelloAck, HelloArgs, HostOp, Mount, MountKind, MsgDirection, PluginOp,
    RecycleArgs, RenderSendArgs, ServerInfo, SessionRegisterArgs, TypingArgs, WelcomeSlice
};
pub use envelope::{
    BODY_TEXT_MAX_BYTES, Body, Causality, ControlOp, Envelope, Error, IMAGE_DATA_MAX_BYTES,
    IMAGE_MIMES, ImagePart, MsgKind, Outcome, Principal, Result, new_envelope, new_id,
    new_op_id, new_task_id, sha256_hex
};
pub use event::{
    Event, EventRow, EventTier, FaultEvent, GatewayHealth, LedgerState, LedgerStateEvent,
    Lifecycle, Presence, RolePresence, SessionStateEvent, SpecReloaded
};
pub use frame::{
    AdminFrame, ErrorCode, ErrorPayload, Frame, GatewayFrame, MAX_ERROR_MESSAGE_BYTES,
    OP_ID_CONFLICT_MESSAGE, ResBody,
};
pub use ops::{
    AckArgs, AdminControl, AdminOp, AdminSend, AgentPhase, ByeArgs, ClientOp, ControlArgs,
    ConversationInfo, Delivery, DeliveryPhase, GatewayOp, HandshakeArgs, HealthArgs,
    HistoryArgs, LedgerEntry, LedgerQuery, PullArgs, PullReply, QueryFaultsArgs, QueryRolesArgs,
    QuerySessionsArgs, Receipt, RecoveryPhase, RegisterChannelArgs, RepairAck, RepairAdopt,
    RepairFail, RepairRebind, RepairTarget, Report, ResourcePhase, RoleInfo, SessionProjection,
    SessionRow, SessionSyncArgs, ShutdownArgs, Subscribe, Welcome
};

/// Wire protocol revision, carried in every [`envelope::Envelope`] and handshake.
pub const PROTOCOL_VERSION: u16 = 1;

/// Idempotency prefix shared by every generated `op_id`.
pub const OP_ID_PREFIX: &str = "o-";

/// Envelope JSON Schema text baked at build time from `schema/envelope.schema.json`.
pub fn envelope_schema() -> &'static str {
    include_str!(concat!(env!("OUT_DIR"), "/envelope.schema.json"))
}

/// Adapter JSON Schema text baked at build time from `schema/adapter.schema.json`.
pub fn adapter_schema() -> &'static str {
    include_str!(concat!(env!("OUT_DIR"), "/adapter.schema.json"))
}

#[cfg(test)]
mod schema_tests {
    #[test]
    fn schema_texts_parse_and_name_their_roots() {
        let envelope: serde_json::Value =
            serde_json::from_str(super::envelope_schema()).expect("envelope schema parses");
        assert_eq!(
            envelope.get("title").and_then(|title| title.as_str()),
            Some("Envelope")
        );
        let adapter: serde_json::Value =
            serde_json::from_str(super::adapter_schema()).expect("adapter schema parses");
        assert_eq!(
            adapter.get("title").and_then(|title| title.as_str()),
            Some("AdapterMsg")
        );
    }
}
