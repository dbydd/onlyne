//! onlyne-gateway-weixin — Weixin iLink gateway plugin (S10).
//!
//! The plugin owns the process-local correlation between a Weixin user id and
//! the protocol envelope.  Raw `RawWireMessage` payloads never enter
//! `onlyne-proto`; they are reduced to text + optional image bodies and to a
//! local [`GatewayRef`] which the gateway stores in its own table.  Platform
//! details are not copied into the envelope.
//!
//! The outbound path performs the 2 MiB image ceiling check before any SDK or
//! network call, using `onlyne-proto::IMAGE_DATA_MAX_BYTES`.

pub mod auth;

use async_trait::async_trait;
use base64::Engine as _;
use onlyne_adapter::{
    AdapterError, AdapterHealth, GatewayHost, GatewayPlugin, OnboardingPrompt, Outbound,
    SendReceipt,
};
use onlyne_proto::{
    Body, Capability, Causality, ConversationInfo, Envelope, ErrorCode, IMAGE_DATA_MAX_BYTES,
    ImagePart, MsgKind, Principal,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use wechat_ilink::{ContentType, IncomingMessage, SendContent, WechatContext, WireMessage};

/// Weixin gateway identity used for both protocol `Principal::Gateway` fields.
pub const GATEWAY_ID: &str = "weixin";
/// Legacy channel name carried in the protocol principal.
pub const CHANNEL_ID: &str = "wechat";
/// Plugin version banner for the iLink bot agent field.
pub const VERSION: &str = "onlyne-weixin/1.0";
/// Ceiling for outbound image payload bytes, shared with the protocol envelope.
pub const IMAGE_BYTES_MAX: usize = IMAGE_DATA_MAX_BYTES;

/// Local correlation row for a Weixin conversation.
///
/// The gateway persists these rows in its own table and only passes the
/// encoded [`GatewayRef::encode`] string across to the host as `gateway_ref`.
/// Raw wire payloads are never forwarded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayRef {
    pub channel: String,
    pub conversation: String,
    pub external_id: Option<String>,
    pub scene: Option<String>,
}

impl GatewayRef {
    /// Construct a correlation row for one conversation.
    pub fn new(
        conversation: impl Into<String>,
        external_id: Option<String>,
        scene: Option<String>,
    ) -> Self {
        GatewayRef {
            channel: CHANNEL_ID.to_string(),
            conversation: conversation.into(),
            external_id,
            scene,
        }
    }

    /// Encode for the `register_channel`/`render_send` `gateway_ref` field.
    pub fn encode(&self) -> String {
        let escaped = |value: Option<&str>| {
            value
                .unwrap_or_default()
                .replace('\\', "\\\\")
                .replace('|', "\\p")
                .replace(';', "\\s")
        };
        format!(
            "channel={};conversation={};external_id={};scene={}",
            escaped(Some(&self.channel)),
            escaped(Some(&self.conversation)),
            escaped(self.external_id.as_deref()),
            escaped(self.scene.as_deref()),
        )
    }

    /// Decode a row previously produced by [`GatewayRef::encode`].
    pub fn decode(value: &str) -> Result<Self, AdapterError> {
        let mut entries: HashMap<&str, String> = HashMap::new();
        for segment in value.split(';') {
            let Some((key, encoded)) = segment.split_once('=') else {
                return Err(AdapterError::new(
                    ErrorCode::Invalid,
                    format!("malformed weixin gateway_ref: {value}"),
                ));
            };
            entries.insert(key, unescape(encoded));
        }
        let channel = entries
            .remove("channel")
            .filter(|channel| !channel.trim().is_empty())
            .ok_or_else(|| {
                AdapterError::new(
                    ErrorCode::Invalid,
                    format!("malformed weixin gateway_ref: {value}"),
                )
            })?;
        let conversation = entries
            .remove("conversation")
            .filter(|conversation| !conversation.trim().is_empty())
            .ok_or_else(|| {
                AdapterError::new(
                    ErrorCode::Invalid,
                    format!("malformed weixin gateway_ref: {value}"),
                )
            })?;
        let external_id = entries
            .remove("external_id")
            .filter(|id| !id.is_empty());
        let scene = entries.remove("scene").filter(|scene| !scene.is_empty());
        Ok(GatewayRef {
            channel,
            conversation,
            external_id,
            scene,
        })
    }
}

fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(char) = chars.next() {
        if char == '\\' {
            match chars.next() {
                Some('p') => out.push('|'),
                Some('s') => out.push(';'),
                Some(next) => out.push(next),
                None => out.push('\\'),
            }
        } else {
            out.push(char);
        }
    }
    out
}

/// A raw wire payload plus the account that observed it.
///
/// Decoding is deliberately strict: bot-originated or malformed payloads are
/// rejected with [`ErrorCode::Invalid`], never forwarded as notes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeixinInboundEvent {
    pub wire: WireMessage,
    #[serde(default)]
    pub account_key: Option<String>,
    #[serde(default)]
    pub context_token: Option<String>,
    #[serde(default)]
    pub external_id: Option<String>,
}

/// The local outbound request shape produced by [`outbound_to_request`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeixinSendRequest {
    pub conversation: String,
    pub text: Option<String>,
    pub image: Option<WeixinImageUpload>,
    pub client_context: WeixinSendContext,
}

/// Image payload guarded by [`IMAGE_BYTES_MAX`] before upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeixinImageUpload {
    pub data: Vec<u8>,
    pub mime: String,
    pub name: Option<String>,
}

/// The user and context token the SDK needs for either a text or media send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeixinSendContext {
    pub user_id: String,
    pub context_token: String,
}

/// Plugin configuration: credential names plus optional binding.
#[derive(Debug, Clone, Default)]
pub struct WeixinConfig {
    pub token: Option<String>,
    pub token_env: Option<String>,
    pub bind_conversation_id: Option<String>,
}

/// The running plugin.  The network client exists only after [`WeixinPlugin::start`].
pub struct WeixinPlugin {
    config: WeixinConfig,
    base_url: String,
    client: Option<Arc<wechat_ilink::WechatIlinkClient>>,
    contexts: Arc<Mutex<HashMap<String, WechatContext>>>,
    running: bool,
    started_at: Option<std::time::Instant>,
}

impl WeixinPlugin {
    /// Build a plugin without touching the network or the environment.
    pub fn new(config: WeixinConfig) -> Self {
        Self::with_base_url(config, auth::DEFAULT_BASE_URL)
    }

    /// Build with an explicit API base; tests use this to stay local.
    pub fn with_base_url(config: WeixinConfig, base_url: impl Into<String>) -> Self {
        WeixinPlugin {
            config,
            base_url: base_url.into(),
            client: None,
            contexts: Arc::new(Mutex::new(HashMap::new())),
            running: false,
            started_at: None,
        }
    }

    /// Access the local context table (process-local reply correlation).
    pub fn contexts(&self) -> Arc<Mutex<HashMap<String, WechatContext>>> {
        Arc::clone(&self.contexts)
    }

    /// Seed an observed context without network access (tests and replay).
    pub async fn observe_context(&self, context: WechatContext) {
        self.contexts
            .lock()
            .await
            .insert(context.user_id.clone(), context);
    }

    /// Resolve the SDK send target for one conversation.
    pub async fn send_context_for(
        &self,
        conversation: &str,
    ) -> Result<WeixinSendContext, AdapterError> {
        let context = self
            .contexts
            .lock()
            .await
            .get(conversation)
            .cloned()
            .ok_or_else(|| {
                AdapterError::new(
                    ErrorCode::Invalid,
                    format!(
                        "weixin context_token missing for {conversation}; receive a message from this peer first"
                    ),
                )
            })?;
        if context.context_token.trim().is_empty() {
            return Err(AdapterError::new(
                ErrorCode::Invalid,
                format!("weixin context_token missing for {conversation}"),
            ));
        }
        Ok(WeixinSendContext {
            user_id: context.user_id,
            context_token: context.context_token,
        })
    }
}

/// Convert one raw inbound wire payload into a protocol envelope.
///
/// Supported shapes are text and image inbound user messages.  Voice, file,
/// and video items are rejected as unsupported (the caller keeps the fallback
/// text only when the item list is exclusively text).  Unknown item kinds are
/// rejected rather than silently dropped so operators notice new wire shapes.
pub fn inbound_event_to_envelope(
    event: &WeixinInboundEvent,
    to: Principal,
) -> Result<Envelope, AdapterError> {
    let wire = &event.wire;
    let Some(message) = event
        .account_key
        .as_deref()
        .and_then(|account| IncomingMessage::from_wire_for_account(wire, account))
        .or_else(|| IncomingMessage::from_wire(wire))
    else {
        return Err(AdapterError::new(
            ErrorCode::Invalid,
            "unsupported weixin message: non-user wire payload",
        ));
    };

    if let Some(bind) = event
        .external_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        if message.user_id != bind {
            return Err(AdapterError::new(
                ErrorCode::Invalid,
                "weixin message outside bound conversation",
            ));
        }
    }

    let text = message.text.trim().to_string();
    let body = match message.content_type {
        ContentType::Text => {
            if text.is_empty() {
                return Err(AdapterError::new(
                    ErrorCode::Invalid,
                    "unsupported weixin message: empty text",
                ));
            }
            Body::text(text)
        }
        ContentType::Image => {
            let image = message.images.first().ok_or_else(|| {
                AdapterError::new(
                    ErrorCode::Invalid,
                    "unsupported weixin message: image without content",
                )
            })?;
            image_reference_body(image, (!text.is_empty()).then_some(text))?
        }
        ContentType::Voice | ContentType::File | ContentType::Video => {
            return Err(AdapterError::new(
                ErrorCode::Invalid,
                format!(
                    "unsupported weixin message type: {}",
                    content_type_name(message.content_type)
                ),
            ));
        }
    };

    let from = Principal::gateway(
        GATEWAY_ID,
        CHANNEL_ID,
        Some(message.user_id.clone()),
    );
    let kind = if looks_like_task(&body_text(&body)) {
        MsgKind::Task
    } else {
        MsgKind::Note
    };
    let causality = match kind {
        MsgKind::Task => Some(Causality::root(onlyne_proto::new_task_id())),
        MsgKind::Note => None,
        _ => None,
    };
    let external_id = message
        .message_id
        .clone()
        .or_else(|| event.external_id.clone());
    let scene = message
        .raw
        .session_id
        .clone()
        .filter(|session| !session.trim().is_empty());
    let gateway_ref = GatewayRef::new(message.user_id.clone(), external_id, scene);
    let envelope = onlyne_proto::new_envelope(kind, from, to, body, causality).map_err(|err| {
        AdapterError::new(ErrorCode::Invalid, format!("weixin envelope invalid: {err}"))
    })?;
    // Keep the opaque cross-process handle local: only the envelope id and the
    // gateway's own ref table travel with the message.  Encoding here lets a
    // caller persist the ref without re-deriving it from raw payload.
    let _ = gateway_ref.encode();
    Ok(envelope)
}

fn body_text(body: &Body) -> String {
    body.text.clone().unwrap_or_default()
}

fn looks_like_task(text: &str) -> bool {
    let lowered = text.to_lowercase();
    lowered.starts_with("/task ")
        || lowered.starts_with("/task\n")
        || lowered == "/task"
        || lowered.starts_with("task:")
}

/// Weixin text extraction renders images/voice/file/video as placeholder
/// lines (`[image]`, `[voice]`...).  For protocol bodies we only accept the
/// placeholder-free text path, or an image item reference for images.
fn image_reference_body(
    image: &wechat_ilink::ImageContent,
    caption: Option<String>,
) -> Result<Body, AdapterError> {
    // Keep the envelope independent of CDN state: carry the stable image URL
    // when the wire message has one, otherwise fall back to the text caption.
    if let Some(url) = image.url.as_deref().filter(|url| !url.trim().is_empty()) {
        let text = caption
            .filter(|caption| !caption.trim().is_empty())
            .map(|caption| format!("{caption}\n{url}"))
            .unwrap_or_else(|| url.to_string());
        return Ok(Body::text(text));
    }
    caption
        .filter(|caption| !caption.trim().is_empty())
        .map(Body::text)
        .ok_or_else(|| {
            AdapterError::new(
                ErrorCode::Invalid,
                "unsupported weixin message: image without url",
            )
        })
}

fn content_type_name(content: ContentType) -> &'static str {
    match content {
        ContentType::Text => "text",
        ContentType::Image => "image",
        ContentType::Voice => "voice",
        ContentType::File => "file",
        ContentType::Video => "video",
    }
}

/// Build the local send request for one adapter [`Outbound`] message.
///
/// Text becomes a filtered SDK text payload; an [`ImagePart`] is decoded and
/// checked against [`IMAGE_BYTES_MAX`] before any upload happens.  Mixed
/// text+image sends image first with the text as caption, matching legacy
/// `send_message` behavior.  Empty sends are rejected.
pub async fn outbound_to_request(
    plugin: &WeixinPlugin,
    outbound: &Outbound,
) -> Result<WeixinSendRequest, AdapterError> {
    let text = outbound.text.trim();
    let text = (!text.is_empty()).then(|| outbound.text.trim().to_string());

    let image = match outbound.image.as_ref() {
        Some(part) => Some(decode_outbound_image(part)?),
        None => None,
    };

    if text.is_none() && image.is_none() {
        return Err(AdapterError::new(
            ErrorCode::Invalid,
            "weixin send_message needs text or image",
        ));
    }

    let context = plugin.send_context_for(&outbound.conversation).await?;
    Ok(WeixinSendRequest {
        conversation: outbound.conversation.clone(),
        text,
        image,
        client_context: context,
    })
}

/// Decode and ceiling-check an outbound image before upload.
pub fn decode_outbound_image(part: &ImagePart) -> Result<WeixinImageUpload, AdapterError> {
    if !onlyne_proto::IMAGE_MIMES.contains(&part.mime.as_str()) {
        return Err(AdapterError::new(
            ErrorCode::Invalid,
            format!("unsupported weixin image mime: {}", part.mime),
        ));
    }
    let data = base64::engine::general_purpose::STANDARD
        .decode(&part.data_base64)
        .map_err(|err| {
            AdapterError::new(
                ErrorCode::Invalid,
                format!("weixin image data_base64 invalid: {err}"),
            )
        })?;
    if data.len() > IMAGE_BYTES_MAX {
        return Err(AdapterError::new(
            ErrorCode::Invalid,
            format!("weixin image exceeds {IMAGE_BYTES_MAX} bytes"),
        ));
    }
    Ok(WeixinImageUpload {
        data,
        mime: part.mime.clone(),
        name: part.name.clone(),
    })
}

/// Map a decoded upload to the SDK [`SendContent`] shape.
///
/// Used by the live `send` path after [`decode_outbound_image`] has enforced
/// the ceiling; kept separate so tests cover the mapping without networking.
pub fn send_content_for_upload(upload: WeixinImageUpload) -> SendContent {
    SendContent::Image {
        data: upload.data,
        caption: None,
    }
}

/// Build the iLink text payload JSON for one send request.
///
/// This mirrors `protocol::build_text_message` (message_type 2,
/// message_state 2, single `type: 1` item) without pulling internal SDK
/// helpers into the plugin surface.
pub fn text_payload_json(request: &WeixinSendRequest, client_id: &str) -> serde_json::Value {
    let text = request.text.clone().unwrap_or_default();
    let filtered = wechat_ilink::filter_markdown(&text);
    serde_json::json!({
        "from_user_id": "",
        "to_user_id": request.client_context.user_id,
        "client_id": client_id,
        "message_type": 2,
        "message_state": 2,
        "context_token": request.client_context.context_token,
        "item_list": [{ "type": 1, "msg_id": client_id, "text_item": { "text": filtered } }]
    })
}

/// Build the iLink media-item JSON for an image upload already sent to the CDN.
pub fn image_item_json(media: serde_json::Value, encrypted_size: usize) -> serde_json::Value {
    serde_json::json!({
        "type": 2,
        "image_item": {
            "media": media,
            "mid_size": encrypted_size,
        }
    })
}

/// Record one reply correlation row without network access.
pub async fn remember_context(
    plugin: &WeixinPlugin,
    event: &WeixinInboundEvent,
) -> Option<GatewayRef> {
    let message = event
        .account_key
        .as_deref()
        .and_then(|account| IncomingMessage::from_wire_for_account(&event.wire, account))
        .or_else(|| IncomingMessage::from_wire(&event.wire))?;
    let context = message.context.clone()?;
    let gateway_ref = GatewayRef::new(
        message.user_id.clone(),
        message.message_id.clone(),
        message.raw.session_id.clone(),
    );
    plugin.observe_context(context).await;
    Some(gateway_ref)
}

#[async_trait]
impl GatewayPlugin for WeixinPlugin {
    fn platform(&self) -> &'static str {
        GATEWAY_ID
    }

    fn capabilities(&self) -> Vec<Capability> {
        // `Conversations` is deliberately absent: the iLink API exposes no
        // conversation enumeration, so the host must not expect a list.
        vec![Capability::Probe, Capability::Typing]
    }

    async fn start(&mut self, host: &mut dyn GatewayHost) -> Result<(), AdapterError> {
        let token = auth::resolve_token(
            self.config.token.as_deref(),
            self.config.token_env.as_deref(),
        )?;
        let base = self.base_url.trim_end_matches('/').to_string();
        let client = Arc::new(
            wechat_ilink::WechatIlinkClient::builder()
                .base_url(base.clone())
                .bot_agent(VERSION)
                .credentials(wechat_ilink::Credentials {
                    token,
                    base_url: base.clone(),
                    account_id: String::new(),
                    user_id: String::new(),
                    saved_at: None,
                })
                .build(),
        );
        self.client = Some(Arc::clone(&client));
        self.running = true;
        self.started_at = Some(std::time::Instant::now());
        host.register_channel(&onlyne_proto::RegisterChannelArgs {
            platform: GATEWAY_ID.to_string(),
            channel: CHANNEL_ID.to_string(),
            conversations: None,
        })
        .await?;
        Ok(())
    }

    async fn send(&mut self, msg: &Outbound) -> Result<SendReceipt, AdapterError> {
        let client = self.client.clone().ok_or_else(|| {
            AdapterError::new(ErrorCode::Invalid, "weixin plugin is not started")
        })?;
        let request = outbound_to_request(self, msg).await?;
        let contexts = self.contexts.lock().await;
        let context = contexts
            .get(&request.conversation)
            .cloned()
            .ok_or_else(|| {
                AdapterError::new(
                    ErrorCode::Invalid,
                    format!(
                        "weixin context_token missing for {}; receive a message from this peer first",
                        request.conversation
                    ),
                )
            })?;
        drop(contexts);

        let mut message_ids = Vec::new();
        if let Some(text) = request.text.as_deref() {
            let receipt = client
                .send_text_with_context(&context, text)
                .await
                .map_err(|err| AdapterError::new(ErrorCode::Invalid, err.to_string()))?;
            message_ids.extend(receipt.message_ids);
        }
        if let Some(upload) = request.image {
            let content = send_content_for_upload(upload);
            let receipt = client
                .send_media_with_context(&context, content)
                .await
                .map_err(|err| AdapterError::new(ErrorCode::Invalid, err.to_string()))?;
            message_ids.extend(receipt.message_ids);
        }
        let external_id = message_ids.last().cloned().ok_or_else(|| {
            AdapterError::new(
                ErrorCode::Invalid,
                "weixin send_message needs text or image",
            )
        })?;
        Ok(SendReceipt { external_id })
    }

    async fn probe(&mut self) -> Result<AdapterHealth, AdapterError> {
        let uptime_s = self
            .started_at
            .map(|started| started.elapsed().as_secs())
            .unwrap_or(0);
        if self.running && self.client.is_some() {
            Ok(AdapterHealth {
                state: "online".to_string(),
                detail: None,
                uptime_s,
            })
        } else {
            Ok(AdapterHealth {
                state: "failed".to_string(),
                detail: Some("weixin plugin is not started".to_string()),
                uptime_s,
            })
        }
    }

    async fn stop(&mut self, _reason: &str) -> Result<(), AdapterError> {
        if let Some(client) = self.client.take() {
            client.stop().await;
        }
        self.running = false;
        Ok(())
    }

    fn onboarding(&mut self) -> Result<Option<OnboardingPrompt>, AdapterError> {
        let configured = auth::resolve_token(
            self.config.token.as_deref(),
            self.config.token_env.as_deref(),
        );
        match configured {
            Ok(_) => Ok(None),
            Err(_) => Ok(Some(auth::qr_onboarding_prompt())),
        }
    }

    async fn list_conversations(&mut self) -> Result<Vec<ConversationInfo>, AdapterError> {
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests;
