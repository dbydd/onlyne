//! The standard, platform-independent payloads the two sides exchange.
//!
//! Neither side names a platform UI here. The core carries `text`,
//! `attachments`, and `actions`; a backend renders them into whatever the
//! platform shows — Telegram inline keyboards, Feishu cards, QQ rich text.
//! Moving a card shape between platforms is the backend's job, never the
//! core's (`docs/v2.2-IM-GATEWAY-PLAN.md` §5).

use serde::{Deserialize, Serialize};

/// One platform instance's id, from the spec's `[[gateway]]`.
pub type ImGatewayId = String;
/// A platform name, e.g. `telegram`.
pub type ImPlatform = String;
/// One conversation on a platform, as the backend addresses it.
pub type ImConversationId = String;

/// How a backend reads `text`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImFormat {
    /// Send the bytes as-is.
    #[default]
    Plain,
    /// A standard markdown subset; a backend degrades what it cannot render.
    Markdown,
}

/// One attachment on an inbound or outbound payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct ImAttachment {
    /// `image`, `file`, `audio`, ...
    pub kind: String,
    /// A local absolute path (outbound) or a written-out path (inbound).
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alt: Option<String>,
}

/// One button the platform should show for a payload.
///
/// A press comes back as an inbound [`ImEvent`] whose `action` carries [`ImAction::id`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct ImAction {
    pub id: String,
    pub label: String,
    /// `primary`, `danger`, `default`, as a hint the backend may honor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
}

/// A payload the core asks a backend to render and send.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", default)]
pub struct ImPayload {
    pub text: String,
    pub format: ImFormat,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<ImAttachment>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<ImAction>,
    /// The platform message id this payload answers, when it is a reply.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
}

/// What kind of platform event a backend reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImEventKind {
    /// A conversation message.
    Message,
    /// A button press.
    Action,
}

/// One platform event, as the core hands it to a trigger script.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", default)]
pub struct ImEvent {
    /// The `[[gateway]]` id this backend serves.
    pub gateway: ImGatewayId,
    pub platform: ImPlatform,
    /// The platform's channel name for this conversation set.
    pub channel: String,
    pub conversation: ImConversationId,
    pub kind: ImEventKind,
    /// The platform message id, the dedupe key a script keys on.
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<ImSender>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<ImAttachment>,
    /// The pressed button's id, for `kind = action`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// RFC 3339 UTC.
    pub at: String,
}

impl Default for ImEvent {
    fn default() -> Self {
        Self {
            gateway: String::new(),
            platform: String::new(),
            channel: String::new(),
            conversation: String::new(),
            kind: ImEventKind::Message,
            id: String::new(),
            from: None,
            text: None,
            attachments: Vec::new(),
            action: None,
            at: String::new(),
        }
    }
}

/// Who sent an inbound event.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", default)]
pub struct ImSender {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}
