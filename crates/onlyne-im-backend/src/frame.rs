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
    /// Report what authenticating this backend needs, so an operator can be
    /// told before the first send fails.
    Onboarding(OnboardingPrompt),
    /// The answer to a [`CoreOp::Login`].
    Login(LoginResult),
    /// The answer to a [`CoreOp::ListConversations`].
    Conversations(ImConversations),
    /// Report one platform event (an inbound message or a button action).
    Event(ImEvent),
    /// Report the backend's own health.
    Health(HealthArgs),
}

/// A core-to-backend frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "op", content = "args")]
pub enum CoreOp {
    /// Hand the backend a credential and ask it to authenticate.
    Login(LoginArgs),
    /// Render and send one standard payload into a conversation.
    Send(SendArgs),
    /// Ask the backend which conversations it can see on the platform.
    ListConversations(ListConversationsArgs),
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
    /// Whether this backend holds a credential it can authenticate with.
    /// Absent reads as `false`, so a backend that predates this field is
    /// unauthenticated and the gateway says so rather than discovering it on
    /// the first refused send.
    #[serde(default)]
    pub authenticated: bool,
    /// What authenticating needs, when it is not already done. The gateway
    /// shows this to an operator; the backend never prints a secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub onboarding: Option<OnboardingPrompt>,
}

/// One credential, handed to a backend that asked for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct LoginArgs {
    /// The gateway id this login is for, so the credential lands with the
    /// right backend when a gateway root runs several.
    pub gateway: String,
    /// The credential itself. It crosses stdio once and is never logged.
    pub credential: String,
}

/// What one [`CoreOp::Login`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct LoginResult {
    /// Whether the credential was accepted.
    pub ok: bool,
    /// Why it was not, in the words an operator reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Whom the platform says this credential belongs to — a bot username, an
    /// app name — so an operator can see they logged in as the right identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
}

/// The conversations a backend can see, one row each.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct ListConversationsArgs {
    /// The gateway id whose conversations are wanted.
    pub gateway: String,
}

/// One platform conversation a backend reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct ImConversation {
    /// The id the platform addresses it by, e.g. a Telegram chat id.
    pub id: String,
    /// What the platform calls it, when it names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// `private`, `group`, `channel`, or the platform's own word.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

/// The answer to a [`CoreOp::ListConversations`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct ImConversations {
    pub conversations: Vec<ImConversation>,
}

/// How a backend wants an operator to authenticate it.
///
/// The two shapes are the ones v1's platform plugins used
/// (`frozen/gateway-v1`'s `auth.rs` modules): a code an operator pastes, or a
/// QR an operator scans. A backend that needs neither authenticates from its
/// own environment and never sends one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OnboardingKind {
    /// A token, app id and secret, or similar: the operator pastes it.
    #[default]
    ManualCode,
    /// A QR code the operator scans with the platform's app.
    Qr,
}

/// What authenticating one backend needs, in the words an operator reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct OnboardingPrompt {
    pub kind: OnboardingKind,
    /// The sentence naming what to provide and where it comes from.
    pub payload: String,
    /// Seconds the prompt stays valid, when the platform gives it one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<u64>,
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
