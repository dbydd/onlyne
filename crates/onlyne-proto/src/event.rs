//! The observation plane (§9, decision D11).
//!
//! Events are at-most-once pushes with a monotonic per-server `seq`. A
//! subscriber that falls behind re-subscribes with `since_seq` and rebuilds from
//! the ledger queries; nothing here replays by itself.

use crate::envelope::{MsgKind, Outcome, Principal};
use crate::ops::SessionProjection;
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A turn ended with its task still open. Published by the client that
/// witnessed the ending (`docs/v2-CONTRACT.md` §"Slice 7").
pub const TURN_END_WITHOUT_COMPLETE: &str = "turn_end_without_complete";

/// A delivery settled blocked. Published by the client that witnessed the
/// settlement (`docs/v2-CONTRACT.md` §"Slice 7").
pub const DELIVERY_BLOCKED: &str = "delivery_blocked";

/// One turn handed work on instead of finishing. Published by the client
/// that witnessed the handoff (`docs/v2-CONTRACT.md` §"Slice 7").
pub const HANDOFF: &str = "handoff";

/// The closed set of classes a client may publish (`ClientOp::PublishEvent`):
/// the turn-end family and nothing else. A client owns these facts, so the
/// server carries a client's word for them and never invents one. A class
/// outside this set is refused by name, so a peer cannot invent an event
/// name and a hook cannot bind to a spelling nobody publishes
/// (`docs/v2-CONTRACT.md` §"Slice 7").
pub const CLIENT_EVENT_CLASSES: [&str; 3] = [TURN_END_WITHOUT_COMPLETE, DELIVERY_BLOCKED, HANDOFF];

/// Build the settlement event a client published, when `class` is in the
/// closed set. `None` for a class the client may not publish, so the server's
/// one check refuses by name rather than decode a free string into a variant.
pub fn client_event(class: &str, payload: Value) -> Option<Event> {
    Some(match class {
        TURN_END_WITHOUT_COMPLETE => Event::TurnEndWithoutComplete(payload),
        DELIVERY_BLOCKED => Event::DeliveryBlocked(payload),
        HANDOFF => Event::Handoff(payload),
        _ => return None,
    })
}

/// Liveness of a registered role's client connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Presence {
    Online,
    Offline,
    /// Connected and refusing new deliveries while it drains running sessions
    /// (decision D3).
    Draining,
}

impl Presence {
    pub fn as_str(self) -> &'static str {
        match self {
            Presence::Online => "online",
            Presence::Offline => "offline",
            Presence::Draining => "draining",
        }
    }
}

/// Health of a connected gateway process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GatewayHealth {
    Online,
    Reconnecting,
    Failed,
}

impl GatewayHealth {
    pub fn as_str(self) -> &'static str {
        match self {
            GatewayHealth::Online => "online",
            GatewayHealth::Reconnecting => "reconnecting",
            GatewayHealth::Failed => "failed",
        }
    }
}

/// Public lifecycle projection of a session, as published by the client that
/// owns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    #[default]
    Created,
    Working,
    Idle,
    Exited,
}

/// Ledger row state for one envelope (decision D10, D11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LedgerState {
    /// Accepted for a recipient that is not connected yet.
    Queued,
    /// Handed to the recipient's client, awaiting `ack`.
    InFlight,
    /// The recipient settled it.
    Acked,
    /// Refused at the gate; the sender got an error frame.
    Rejected,
    /// A note whose `ttl_ms` elapsed before delivery.
    Expired,
}

impl LedgerState {
    pub fn as_str(self) -> &'static str {
        match self {
            LedgerState::Queued => "queued",
            LedgerState::InFlight => "in_flight",
            LedgerState::Acked => "acked",
            LedgerState::Rejected => "rejected",
            LedgerState::Expired => "expired",
        }
    }
}

impl std::fmt::Display for LedgerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct RolePresence {
    pub role: String,
    pub state: Presence,
    /// Aggregate cluster this role represents, when the spec declares one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregate: Option<String>,
    /// Live session count reported by the client.
    pub sessions: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct SessionStateEvent {
    /// The delivery this session was serving when the write landed, read off
    /// the row's binding. Absent for a session no delivery is bound to, and the
    /// field keeps its place so a write that names a delivery still encodes to
    /// the same bytes it always did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub role: String,
    pub session_id: String,
    pub generation: u64,
    pub seq: u64,
    pub projection: SessionProjection,
    /// The operator who filed this write on the session's behalf over the
    /// admin surface. Absent when the session reported on itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub admin: Option<Principal>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct LedgerStateEvent {
    pub msg_id: String,
    pub op_id: Option<String>,
    pub kind: MsgKind,
    pub from: Principal,
    pub to: Principal,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    pub state: LedgerState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct FaultEvent {
    /// `faults.id` in the ledger.
    pub id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    /// Stable fault taxonomy, e.g. `intent_exhausted`, `idle_fault`,
    /// `gateway_unconfigured`, `spec_reload_failed`.
    pub kind: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desired: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_ref: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct SpecReloaded {
    pub spec_hash: String,
    pub roles: u32,
    pub gateways: u32,
    pub routes: u32,
}

/// Observation-plane event. `seq` lives on the enclosing frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "type", content = "data")]
pub enum Event {
    /// A role's client connected, drained, or dropped.
    RolePresence(RolePresence),
    /// A session projection moved.
    SessionState(SessionStateEvent),
    /// A ledger row changed state.
    LedgerState(LedgerStateEvent),
    /// The server recorded a fault that needs a supervisor decision.
    Fault(FaultEvent),
    /// A gateway process changed health.
    GatewayPresence {
        gateway: String,
        platform: String,
        state: GatewayHealth,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// `spec.toml` was reloaded successfully.
    SpecReloaded(SpecReloaded),
    /// §3c's turn-end family, published by the client that witnessed it. The
    /// server carries these on its stream so a hook bound to the class can
    /// serve the plan's own example, and the client keeps no second copy
    /// (`docs/v2-CONTRACT.md` §"Slice 7"). `type_name` is the class a hook
    /// binds to, so the row's `type` column is the class and a hook's `on`
    /// list filters by it exactly as a subscriber does.
    TurnEndWithoutComplete(Value),
    /// A delivery settled blocked: the work waits on something outside the
    /// delivery, and a board reads it as waiting rather than as failed.
    DeliveryBlocked(Value),
    /// One turn handed work on instead of finishing.
    Handoff(Value),
}

impl Event {
    pub fn type_name(&self) -> &'static str {
        match self {
            Event::RolePresence(_) => "role_presence",
            Event::SessionState(_) => "session_state",
            Event::LedgerState(_) => "ledger_state",
            Event::Fault(_) => "fault",
            Event::GatewayPresence { .. } => "gateway_presence",
            Event::SpecReloaded(_) => "spec_reloaded",
            Event::TurnEndWithoutComplete(_) => TURN_END_WITHOUT_COMPLETE,
            Event::DeliveryBlocked(_) => DELIVERY_BLOCKED,
            Event::Handoff(_) => HANDOFF,
        }
    }

    /// Tiers drive subscription filtering: control-plane events are durable and
    /// always replayable, observation events are ring-buffered.
    pub fn tier(&self) -> EventTier {
        match self {
            Event::LedgerState(_) | Event::SessionState(_) => EventTier::Durable,
            Event::RolePresence(_)
            | Event::Fault(_)
            | Event::GatewayPresence { .. }
            | Event::SpecReloaded(_) => EventTier::Advisory,
            // The settlement family is persisted in `events` and replayable
            // from a cursor exactly as the durable class is, so a subscriber
            // that reconnects with its last `seq` resumes with no gap.
            Event::TurnEndWithoutComplete(_) | Event::DeliveryBlocked(_) | Event::Handoff(_) => {
                EventTier::Durable
            }
        }
    }
}

/// Subscription filter classes (§4 `subscribe.tiers`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventTier {
    /// Persisted in `events`, replayable from a cursor.
    Durable,
    /// Best effort; a lagging subscriber resyncs by querying.
    Advisory,
}

/// A timestamped event row, as returned by `watch` and `history`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct EventRow {
    pub seq: u64,
    pub created_at: DateTime<Utc>,
    pub event: Event,
}
