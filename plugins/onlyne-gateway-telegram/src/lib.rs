//! Telegram gateway plugin for Onlyne v1.
//!
//! Platform JSON translation is intentionally pure and public.  The host can
//! feed decoded webhook/polling updates through [`inbound_update`] without
//! making the platform payload part of the core envelope.  Outbound requests
//! are represented as JSON by [`outbound_request`] and are then sent by the
//! small `teloxide` adapter below.

pub mod auth;

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::HashMap, time::Instant};
use teloxide::{Bot, payloads::SendPhotoSetters, prelude::Requester, types::{ChatId, InputFile}};
use onlyne_adapter::{AdapterError, AdapterHealth, GatewayHost, GatewayPlugin, OnboardingKind, OnboardingPrompt, Outbound, SendReceipt};
use onlyne_proto::{Capability, ConversationInfo, Causality, Envelope, ErrorCode, HealthArgs, IMAGE_DATA_MAX_BYTES, IMAGE_MIMES, MsgKind, Principal, RegisterChannelArgs, new_envelope, new_task_id};

pub const PLATFORM: &str = "telegram";

/// Opaque local correlation data.  It is never embedded in an Envelope.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GatewayRef {
    pub channel: String,
    pub conversation: String,
    pub external_id: String,
    pub scene: String,
}

impl GatewayRef {
    pub fn new(
        channel: impl Into<String>,
        conversation: impl Into<String>,
        external_id: impl Into<String>,
        scene: impl Into<String>,
    ) -> Self {
        Self {
            channel: channel.into(),
            conversation: conversation.into(),
            external_id: external_id.into(),
            scene: scene.into(),
        }
    }

    /// Encode a ref as an opaque, URL-safe local handle.
    pub fn encode(&self) -> String {
        let raw = serde_json::to_vec(self).expect("GatewayRef is serializable");
        format!("tg1.{}", URL_SAFE_NO_PAD.encode(raw))
    }

    pub fn decode(value: &str) -> Result<Self, AdapterError> {
        let encoded = value.strip_prefix("tg1.").ok_or_else(|| invalid("gateway_ref must start with tg1."))?;
        let raw = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|err| invalid(format!("invalid gateway_ref encoding: {err}")))?;
        serde_json::from_slice(&raw)
            .map_err(|err| invalid(format!("invalid gateway_ref payload: {err}")))
    }
}

/// Stable mapping helper used by the gateway host's local correlation table.
pub fn gateway_ref(
    channel: &str,
    conversation: &str,
    external_id: &str,
    scene: &str,
) -> String {
    GatewayRef::new(channel, conversation, external_id, scene).encode()
}

pub fn parse_gateway_ref(value: &str) -> Result<GatewayRef, AdapterError> {
    GatewayRef::decode(value)
}

/// In-memory local mapping table.  A host may persist the same four columns in
/// its own database; this type is useful for a single-process plugin and pure
/// tests without introducing a store dependency into the plugin crate.
#[derive(Debug, Default, Clone)]
pub struct GatewayRefTable {
    by_ref: HashMap<String, GatewayRef>,
    by_tuple: HashMap<GatewayRef, String>,
}

impl GatewayRefTable {
    pub fn insert(&mut self, value: GatewayRef) -> String {
        let key = value.encode();
        self.by_tuple.insert(value.clone(), key.clone());
        self.by_ref.insert(key.clone(), value);
        key
    }

    pub fn resolve(&self, key: &str) -> Option<&GatewayRef> {
        self.by_ref.get(key)
    }

    pub fn lookup(
        &self,
        channel: &str,
        conversation: &str,
        external_id: &str,
        scene: &str,
    ) -> Option<&str> {
        self.by_tuple
            .get(&GatewayRef::new(channel, conversation, external_id, scene))
            .map(String::as_str)
    }
}

/// Translate a Telegram Update-like JSON object into the Onlyne envelope.
///
/// Telegram media file IDs are platform handles, not bytes.  A caller that
/// has already downloaded an image may add `data_base64` to the selected photo
/// object; otherwise the update is represented as a text note describing the
/// photo and the host can perform its own media fetch.
pub fn inbound_update(
    update: &Value,
    gateway_id: &str,
    target_role: &str,
) -> Result<Envelope, AdapterError> {
    if gateway_id.trim().is_empty() || target_role.trim().is_empty() {
        return Err(invalid("gateway_id and target_role must not be empty"));
    }
    let message = update
        .get("message")
        .or_else(|| update.get("edited_message"))
        .or_else(|| update.get("channel_post"))
        .ok_or_else(|| invalid("Telegram update has no supported message"))?;
    let chat = message
        .get("chat")
        .ok_or_else(|| invalid("Telegram message has no chat"))?;
    let conversation = scalar_string(chat.get("id"))
        .ok_or_else(|| invalid("Telegram chat.id must be a string or integer"))?;
    let _external_id = scalar_string(message.get("message_id"))
        .ok_or_else(|| invalid("Telegram message.message_id must be a string or integer"))?;
    let _scene = chat
        .get("type")
        .and_then(Value::as_str)
        .or_else(|| message.get("scene").and_then(Value::as_str))
        .unwrap_or("chat");

    for unsupported in ["document", "audio", "voice", "video", "animation", "sticker", "location", "contact", "poll"] {
        if message.get(unsupported).is_some() {
            return Err(invalid(format!("unsupported Telegram message type: {unsupported}")));
        }
    }

    let text = message
        .get("text")
        .and_then(Value::as_str)
        .or_else(|| message.get("caption").and_then(Value::as_str))
        .map(str::to_owned);
    let kind = message
        .get("kind")
        .and_then(Value::as_str)
        .or_else(|| message.get("onlyne_kind").and_then(Value::as_str))
        .unwrap_or_else(|| if text.as_deref().is_some_and(|s| s.starts_with("/task ")) { "task" } else { "note" });
    let (kind, text) = match kind {
        "note" => (MsgKind::Note, text.map(|s| s.strip_prefix("/note ").unwrap_or(&s).to_owned())),
        "task" => (MsgKind::Task, text.map(|s| s.strip_prefix("/task ").unwrap_or(&s).to_owned())),
        other => return Err(invalid(format!("unsupported Telegram message kind: {other}"))),
    };

    let image = if let Some(photos) = message.get("photo").and_then(Value::as_array) {
        let selected = photos.last().ok_or_else(|| invalid("Telegram photo array is empty"))?;
        let data = selected
            .get("data_base64")
            .or_else(|| selected.get("data"))
            .and_then(Value::as_str);
        match data {
            Some(encoded) => {
                let bytes = decode_image(encoded)?;
                if bytes.len() > IMAGE_DATA_MAX_BYTES {
                    return Err(invalid(format!("image exceeds {IMAGE_DATA_MAX_BYTES} bytes")));
                }
                Some(onlyne_proto::ImagePart {
                    data_base64: encoded.to_owned(),
                    mime: "image/jpeg".into(),
                    name: Some("telegram-photo.jpg".into()),
                })
            }
            None => None,
        }
    } else {
        None
    };

    if text.is_none() && image.is_none() && message.get("photo").is_none() {
        return Err(invalid("unsupported Telegram message type: message has no text or image"));
    }
    let body = onlyne_proto::Body {
        text: text.or_else(|| message.get("photo").map(|_| "[telegram image]".into())),
        image,
    };
    let from = Principal::gateway(PLATFORM.to_owned() + ":" + gateway_id, PLATFORM, Some(conversation.clone()));
    let to = Principal::role(target_role);
    let causality = (kind == MsgKind::Task).then(|| Causality::root(new_task_id()));
    new_envelope(kind, from, to, body, causality).map_err(|err| invalid(err.to_string()))
}

/// Alias retained for host integrations that call the operation a translation.
pub fn translate_inbound(update: &Value, gateway_id: &str, target_role: &str) -> Result<Envelope, AdapterError> {
    inbound_update(update, gateway_id, target_role)
}

/// Build the JSON shape sent to Telegram's `sendMessage` or `sendPhoto` API.
pub fn outbound_request(msg: &Outbound) -> Result<Value, AdapterError> {
    if !matches!(msg.kind, MsgKind::Note | MsgKind::Task) {
        return Err(invalid(format!("unsupported outbound message kind: {}", msg.kind)));
    }
    if msg.conversation.trim().is_empty() {
        return Err(invalid("Telegram conversation must not be empty"));
    }
    if msg.text.is_empty() && msg.image.is_none() {
        return Err(invalid("Telegram outbound message needs text or image"));
    }
    let chat_id = match msg.conversation.parse::<i64>() {
        Ok(id) => Value::from(id),
        Err(_) => Value::from(msg.conversation.clone()),
    };
    let reply = msg.reply_to.as_deref().and_then(|s| s.parse::<i64>().ok()).map(Value::from);
    if let Some(image) = &msg.image {
        let bytes = decode_image(&image.data_base64)?;
        if bytes.len() > IMAGE_DATA_MAX_BYTES {
            return Err(invalid(format!("image exceeds {IMAGE_DATA_MAX_BYTES} bytes")));
        }
        if !IMAGE_MIMES.contains(&image.mime.as_str()) {
            return Err(invalid(format!("unsupported image mime: {}", image.mime)));
        }
        let mut request = json!({
            "method": "sendPhoto",
            "chat_id": chat_id,
            "photo": {
                "data_base64": image.data_base64,
                "mime": image.mime,
                "name": image.name,
            },
        });
        if !msg.text.is_empty() {
            request["caption"] = Value::String(msg.text.clone());
        }
        if let Some(reply) = reply {
            request["reply_to_message_id"] = reply;
        }
        Ok(request)
    } else {
        let mut request = json!({
            "method": "sendMessage",
            "chat_id": chat_id,
            "text": msg.text,
        });
        if let Some(reply) = reply {
            request["reply_to_message_id"] = reply;
        }
        Ok(request)
    }
}

pub fn translate_outbound(msg: &Outbound) -> Result<Value, AdapterError> {
    outbound_request(msg)
}

/// A Telegram Bot API gateway plugin.  Polling/webhook ownership remains with
/// the host; `inbound_update` is the pure boundary used by either transport.
pub struct TelegramPlugin {
    token: String,
    bot: Bot,
    gateway_id: String,
    bind_conversation: Option<String>,
    running: bool,
    started_at: Instant,
    refs: GatewayRefTable,
}

impl TelegramPlugin {
    pub fn new(token: impl Into<String>) -> Self {
        let token = token.into();
        Self {
            bot: Bot::new(token.clone()),
            token,
            gateway_id: "telegram".into(),
            bind_conversation: None,
            running: false,
            started_at: Instant::now(),
            refs: GatewayRefTable::default(),
        }
    }

    pub fn from_env() -> Result<Self, AdapterError> {
        Ok(Self::new(auth::resolve_token(None)?))
    }

    pub fn with_gateway_id(mut self, gateway_id: impl Into<String>) -> Self {
        self.gateway_id = gateway_id.into();
        self
    }

    pub fn with_bind_conversation(mut self, conversation: impl Into<String>) -> Self {
        self.bind_conversation = Some(conversation.into());
        self
    }

    pub fn gateway_refs(&self) -> &GatewayRefTable {
        &self.refs
    }

    pub fn gateway_refs_mut(&mut self) -> &mut GatewayRefTable {
        &mut self.refs
    }

    pub fn token(&self) -> &str {
        &self.token
    }
}

#[async_trait]
impl GatewayPlugin for TelegramPlugin {
    fn platform(&self) -> &'static str {
        PLATFORM
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::Probe, Capability::Recycle, Capability::Typing]
    }

    async fn start(&mut self, host: &mut dyn GatewayHost) -> Result<(), AdapterError> {
        self.running = true;
        self.started_at = Instant::now();
        host.register_channel(&RegisterChannelArgs {
            platform: PLATFORM.into(),
            channel: self.gateway_id.clone(),
            conversations: self.bind_conversation.as_ref().map(|conversation| {
                vec![ConversationInfo { conversation: conversation.clone(), title: None }]
            }),
        })
        .await?;
        host.report_health(&HealthArgs {
            state: "online".into(),
            detail: None,
            uptime_s: 0,
        })
        .await
    }

    async fn send(&mut self, msg: &Outbound) -> Result<SendReceipt, AdapterError> {
        let request = outbound_request(msg)?;
        let chat = request["chat_id"].as_i64().ok_or_else(|| invalid("Telegram send requires numeric chat id"))?;
        let chat = ChatId(chat);
        let sent = match request["method"].as_str() {
            Some("sendMessage") => self
                .bot
                .send_message(chat, request["text"].as_str().unwrap_or_default().to_owned())
                .await
                .map_err(|err| unexpected(format!("Telegram sendMessage failed: {err}")))?,
            Some("sendPhoto") => {
                let encoded = request["photo"]["data_base64"].as_str().ok_or_else(|| invalid("Telegram photo data is missing"))?;
                let bytes = decode_image(encoded)?;
                self.bot
                    .send_photo(chat, InputFile::memory(bytes))
                    .caption(request["caption"].as_str().unwrap_or_default().to_owned())
                    .await
                    .map_err(|err| unexpected(format!("Telegram sendPhoto failed: {err}")))?
            }
            _ => return Err(invalid("unsupported Telegram request method")),
        };
        Ok(SendReceipt { external_id: sent.id.0.to_string() })
    }

    async fn probe(&mut self) -> Result<AdapterHealth, AdapterError> {
        self.bot
            .get_me()
            .await
            .map_err(|err| unexpected(format!("Telegram probe failed: {err}")))?;
        Ok(AdapterHealth {
            state: "online".into(),
            detail: None,
            uptime_s: self.started_at.elapsed().as_secs(),
        })
    }

    async fn stop(&mut self, _reason: &str) -> Result<(), AdapterError> {
        self.running = false;
        Ok(())
    }

    fn onboarding(&mut self) -> Result<Option<OnboardingPrompt>, AdapterError> {
        Ok(Some(OnboardingPrompt {
            kind: OnboardingKind::ManualCode,
            payload: "Set TELEGRAM_BOT_TOKEN to the token from @BotFather, then restart the gateway.".into(),
            expires_in: None,
        }))
    }
}

fn scalar_string(value: Option<&Value>) -> Option<String> {
    value.and_then(|v| match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    })
}

fn decode_image(encoded: &str) -> Result<Vec<u8>, AdapterError> {
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|err| invalid(format!("invalid image base64: {err}")))
}

fn invalid(message: impl Into<String>) -> AdapterError {
    AdapterError::new(ErrorCode::Invalid, message)
}

fn unexpected(message: impl Into<String>) -> AdapterError {
    AdapterError::Unexpected(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;

    fn outbound(text: &str) -> Outbound {
        Outbound { conversation: "42".into(), text: text.into(), image: None, reply_to: None, kind: MsgKind::Note }
    }

    #[test]
    fn inbound_fixture_becomes_gateway_note() {
        let update = json!({"update_id": 7, "message": {"message_id": 99, "text": "hello", "chat": {"id": 42, "type": "private"}}});
        let env = inbound_update(&update, "g1", "planner").unwrap();
        assert_eq!(env.kind, MsgKind::Note);
        assert_eq!(env.body.text.as_deref(), Some("hello"));
        assert!(matches!(env.from, Principal::Gateway { .. }));
    }

    #[test]
    fn task_command_uses_task_kind() {
        let update = json!({"message": {"message_id": 1, "text": "/task ship it", "chat": {"id": "c1"}}});
        let env = inbound_update(&update, "g1", "planner").unwrap();
        assert_eq!(env.kind, MsgKind::Task);
        assert_eq!(env.body.text.as_deref(), Some("ship it"));
        assert!(env.causality.is_some());
    }

    #[test]
    fn outbound_text_has_telegram_request_shape() {
        let request = outbound_request(&outbound("hello")).unwrap();
        assert_eq!(request["method"], "sendMessage");
        assert_eq!(request["chat_id"], 42);
        assert_eq!(request["text"], "hello");
    }

    #[test]
    fn gateway_ref_round_trips_and_table_indexes() {
        let value = GatewayRef::new("telegram", "42", "99", "private");
        let encoded = gateway_ref(&value.channel, &value.conversation, &value.external_id, &value.scene);
        assert_eq!(parse_gateway_ref(&encoded).unwrap(), value);
        let mut table = GatewayRefTable::default();
        let key = table.insert(value.clone());
        assert_eq!(table.resolve(&key), Some(&value));
        assert_eq!(table.lookup("telegram", "42", "99", "private"), Some(key.as_str()));
    }

    #[test]
    fn unsupported_telegram_media_is_clean_error() {
        let update = json!({"message": {"message_id": 1, "document": {"file_id": "f"}, "chat": {"id": 42}}});
        let err = inbound_update(&update, "g1", "planner").unwrap_err();
        assert!(err.to_string().contains("unsupported Telegram message type"));
    }

    #[test]
    fn image_limit_is_rejected_before_request_upload() {
        let bytes = vec![b'x'; IMAGE_DATA_MAX_BYTES + 1];
        let image = onlyne_proto::ImagePart { data_base64: STANDARD.encode(bytes), mime: "image/png".into(), name: None };
        let mut msg = outbound("caption");
        msg.image = Some(image);
        let err = outbound_request(&msg).unwrap_err();
        assert!(err.to_string().contains("image exceeds 2097152 bytes"));
    }
}
