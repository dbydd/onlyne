use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{fmt, path::PathBuf};
use tokio::sync::mpsc;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChannelId(pub String);
impl fmt::Display for ChannelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConversationId(pub String);
impl fmt::Display for ConversationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MessageId(pub String);
impl fmt::Display for MessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Inbound,
    Outbound,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    Pending,
    Sent,
    Delivered,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterHealth {
    Stopped,
    Starting,
    Ready,
    Reconnecting,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageFormat {
    Plain,
    #[default]
    Markdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    Image,
    File,
    Audio,
    Voice,
    Video,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachmentRef {
    pub kind: AttachmentKind,
    pub path: Option<PathBuf>,
    pub url: Option<String>,
    pub file_name: Option<String>,
    pub mime_type: Option<String>,
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageEnvelope {
    pub channel_id: ChannelId,
    pub conversation_id: ConversationId,
    pub message_id: MessageId,
    pub direction: Direction,
    pub sender_id: Option<String>,
    pub sender_name: Option<String>,
    pub text: Option<String>,
    #[serde(default)]
    pub format: MessageFormat,
    pub attachments: Vec<AttachmentRef>,
    pub delivery_state: DeliveryState,
    pub timestamp: DateTime<Utc>,
    pub platform_metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboundMessage {
    pub channel_id: ChannelId,
    pub conversation_id: ConversationId,
    pub text: Option<String>,
    #[serde(default)]
    pub format: MessageFormat,
    #[serde(default)]
    pub attachments: Vec<AttachmentRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub channel_id: ChannelId,
    pub conversation_id: ConversationId,
    pub title: Option<String>,
    pub platform_metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum Event {
    InboundMessage(MessageEnvelope),
    OutboundMessage(MessageEnvelope),
    DeliveryUpdate {
        channel_id: ChannelId,
        message_id: MessageId,
        state: DeliveryState,
        error: Option<String>,
    },
    AdapterStarted {
        channel_id: ChannelId,
    },
    AdapterStopped {
        channel_id: ChannelId,
    },
    AdapterReconnecting {
        channel_id: ChannelId,
        reason: String,
    },
    AdapterFailed {
        channel_id: ChannelId,
        error: String,
    },
    HistoryAppended {
        channel_id: ChannelId,
        message_id: MessageId,
    },
    WorkspaceStateChanged {
        message: String,
    },
    Warning {
        channel_id: Option<ChannelId>,
        message: String,
    },
    Error {
        channel_id: Option<ChannelId>,
        message: String,
    },
}

#[derive(Clone)]
pub struct AdapterContext {
    pub events: mpsc::Sender<Event>,
    pub inbound: mpsc::Sender<MessageEnvelope>,
    pub media_dir: PathBuf,
}

#[async_trait]
pub trait Adapter: Send + Sync {
    fn channel_id(&self) -> ChannelId;
    async fn start(&mut self, ctx: AdapterContext) -> anyhow::Result<()>;
    async fn stop(&mut self) -> anyhow::Result<()>;
    fn health(&self) -> AdapterHealth;
    async fn list_conversations(&self) -> anyhow::Result<Vec<Conversation>>;
    async fn send_message(&self, msg: OutboundMessage) -> anyhow::Result<MessageEnvelope>;
    async fn check(&self) -> anyhow::Result<()>;
}

pub fn now_id(prefix: &str) -> MessageId {
    MessageId(format!(
        "{prefix}-{}",
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ))
}
