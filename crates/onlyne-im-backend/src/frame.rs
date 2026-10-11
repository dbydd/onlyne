//! The frame vocabulary between the IM core and one platform backend.
//!
//! Two directions, two enums. The backend never sees Onlyne's role or session
//! vocabulary: it speaks platform events one way and standard payloads the
//! other, and routing belongs to the core (`docs/v2.2-IM-GATEWAY-PLAN.md` §6).

use crate::payload::{ImEvent, ImPayload};
use serde::{Deserialize, Serialize};

/// A backend-to-core frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "op", content = "args")]
pub enum BackendOp {
    /// Announce the platform this backend speaks and what it can do.
    Hello(HelloArgs),
    /// Report one platform event (an inbound message or a button action).
    Event(ImEvent),
    /// Report the backend's own health.
    Health(HealthArgs),
}

/// A core-to-backend frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "op", content = "args")]
pub enum CoreOp {
    /// Render and send one standard payload into a conversation.
    Send(SendArgs),
    /// Stop the backend; the process exits after this.
    Stop(StopArgs),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct HelloArgs {
    /// The platform this backend bridges, e.g. `telegram`, `feishu`.
    pub platform: String,
    /// The backend crate's version.
    pub version: String,
    /// What this backend can do: `message`, `action`, `typing`.
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct SendArgs {
    /// The platform conversation the payload goes to.
    pub conversation: String,
    /// The standard, platform-independent payload.
    pub payload: ImPayload,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct StopArgs {
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", default)]
pub struct HealthArgs {
    /// `online`, `reconnecting`, `failed`.
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub uptime_s: u64,
}

impl Default for HealthArgs {
    fn default() -> Self {
        Self {
            state: "online".to_string(),
            detail: None,
            uptime_s: 0,
        }
    }
}
