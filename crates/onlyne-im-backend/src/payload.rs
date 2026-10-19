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
    /// The id a platform needs to *display* this payload as a quote, when that
    /// is a different id from `reply_to` (QQ: `msg_id` takes `ROBOT1.0_…`,
    /// `message_reference` takes `REFIDX_…`). Absent when the platform has one
    /// id or when the message being answered has no handle recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_reference: Option<String>,
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
    /// What this event answers and which discussion it sits in, when the
    /// platform says. Absent for a message that opens its own discussion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply: Option<ImReply>,
    /// The platform's original event type name (e.g. `im.message.receive_v1`,
    /// `card.action.trigger`, `calendar.calendar.event.changed_v4`), when the
    /// backend records one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_type: Option<String>,
    /// Optional raw platform event JSON, preserved for downstream trigger
    /// scripts and MCP tools that inspect platform-specific resource IDs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<serde_json::Value>,
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
            reply: None,
            event_type: None,
            raw: None,
            at: String::new(),
        }
    }
}

/// The platform's reply coordinate: what this event answers, and the
/// discussion it sits in.
///
/// Three ids, because the platforms name three different things and collapsing
/// them loses what a thread platform needs:
///
/// * `parent` — the message this one replies to (Feishu `parent_id`, QQ's
///   quoted ref, Telegram's `reply_to_message`).
/// * `root` — the anchor of the whole discussion (Feishu `root_id`, a thread's
///   start). This is what names the list on a platform whose replies have no
///   list of their own.
/// * `thread` — a platform thread distinct from a reply (Feishu `thread_id`).
///
/// A platform that reports none of them leaves the field absent, and the
/// gateway then has nothing but the message itself — which is the honest
/// reading, and the shape every v2.3 backend already had.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case", default)]
pub struct ImReply {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
    /// The text of the message this one answers, when the platform deliveries
    /// it inside the event — Telegram's `reply_to_message` does. A backend that
    /// cannot get it leaves this absent and a reader falls back to the history
    /// API under the operator's grant.
    ///
    /// It rides *here* rather than as the message's own text, because what the
    /// message says is what the sender typed and what it quotes is material —
    /// the distinction the delivery template already draws for upstream work.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quoted_text: Option<String>,
    /// The platform's own handle for quoting **this** message back.
    ///
    /// A platform can hand out two ids for one message and mean different
    /// things by them: QQ accepts a passive reply against the message id it
    /// sent (`ROBOT1.0_…`, `msg_id`) and renders a *displayed* quote only
    /// against the message's quote handle (`REFIDX_…`, `message_reference`).
    /// The handle arrives in the event that carries the message
    /// (`message_scene.ext`'s `msg_idx`), so it rides here for the record to
    /// keep — nothing outbound needs it until a later reply quotes this one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub self_handle: Option<String>,
}

impl ImReply {
    /// Whether any coordinate is present.
    pub fn is_empty(&self) -> bool {
        self.parent.is_none()
            && self.root.is_none()
            && self.thread.is_none()
            && self.quoted_text.is_none()
            && self.self_handle.is_none()
    }

    /// The message this event answers, from whichever id names it.
    pub fn answers(&self) -> Option<&str> {
        self.parent.as_deref().or(self.root.as_deref())
    }

    /// The anchor of the discussion this event belongs to.
    ///
    /// The root when the platform names one, otherwise the parent — a reply
    /// whose platform does not distinguish them anchors on what it answers,
    /// which is the same reading `thread::assign_list` makes.
    pub fn anchor(&self) -> Option<&str> {
        self.root.as_deref().or(self.parent.as_deref())
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
