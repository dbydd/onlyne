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
    pub task_id: String,
    pub role: String,
    pub session_id: String,
    pub generation: u64,
    pub seq: u64,
    pub projection: SessionProjection,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{Causality, new_task_id};
    use crate::ops::SessionProjection;

    #[test]
    fn events_carry_a_wire_type_name_and_payload() {
        let event = Event::RolePresence(RolePresence {
            role: "builder".into(),
            state: Presence::Offline,
            aggregate: None,
            sessions: 0,
            detail: None,
        });
        let value = serde_json::to_value(&event).expect("encode");
        assert_eq!(value["type"], "role_presence");
        assert_eq!(value["data"]["state"], "offline");
        let back: Event = serde_json::from_value(value).expect("decode");
        assert_eq!(back, event);
    }

    #[test]
    fn ledger_state_names_are_the_documented_strings() {
        assert_eq!(LedgerState::InFlight.as_str(), "in_flight");
        assert_eq!(LedgerState::Queued.as_str(), "queued");
        assert_eq!(LedgerState::Expired.as_str(), "expired");
        assert_eq!(Presence::Draining.as_str(), "draining");
        assert_eq!(GatewayHealth::Reconnecting.as_str(), "reconnecting");
    }

    #[test]
    fn durable_tiers_cover_ledger_and_session_only() {
        let session = Event::SessionState(SessionStateEvent {
            task_id: new_task_id(),
            role: "builder".into(),
            session_id: "s".into(),
            generation: 1,
            seq: 2,
            projection: SessionProjection::default_working(),
        });
        assert_eq!(session.tier(), EventTier::Durable);
        let fault = Event::Fault(FaultEvent {
            id: 1,
            task_id: Some(Causality::root(new_task_id()).task),
            role: None,
            session_id: None,
            generation: None,
            seq: None,
            kind: "idle_fault".into(),
            reason: "no heartbeat".into(),
            desired: None,
            observed: None,
            intent: None,
            attempt: None,
            backend_ref: None,
            state: None,
            created_at: None,
        });
        assert_eq!(fault.tier(), EventTier::Advisory);
        let value = serde_json::to_value(&fault).expect("encode");
        assert_eq!(value["type"], "fault");
        assert_eq!(value["data"]["kind"], "idle_fault");
    }

    #[test]
    fn lifecycle_and_outcome_serialise_snake_case() {
        assert_eq!(
            serde_json::to_value(Lifecycle::Exited).expect("encode"),
            Value::String("exited".into())
        );
        assert_eq!(
            serde_json::to_value(Outcome::Cancelled).expect("encode"),
            Value::String("cancelled".into())
        );
    }
}
