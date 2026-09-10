//! QQ Open Platform gateway plugin for Onlyne v1.
//!
//! Platform payloads are translated at this boundary. The core only receives
//! `Envelope`s with a `Principal::Gateway`; QQ's original event JSON is never
//! put into the envelope.

mod auth;

pub use auth::{APP_ID_ENV, APP_SECRET_ENV, QqBotCredentials, onboarding_prompt};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use onlyne_adapter::{
    AdapterError, AdapterHealth, GatewayHost, GatewayPlugin, OnboardingPrompt, Outbound,
    SendReceipt,
};
use onlyne_proto::{
    Body, Capability, Causality, ConversationInfo, Envelope, HealthArgs, IMAGE_DATA_MAX_BYTES,
    MsgKind, Principal, RegisterChannelArgs, new_envelope, new_task_id,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::HashMap, time::{Duration, Instant}};

const CHANNEL: &str = "qqbot";
const TOKEN_URL: &str = "https://bots.qq.com/app/getAppAccessToken";
const API_URL: &str = "https://api.sgroup.qq.com";
const SANDBOX_API_URL: &str = "https://sandbox.api.sgroup.qq.com";

/// QQ's four addressable conversation scenes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QqScene {
    Group,
    #[serde(rename = "c2c")]
    C2c,
    Channel,
    Direct,
}

impl QqScene {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Group => "group",
            Self::C2c => "c2c",
            Self::Channel => "channel",
            Self::Direct => "direct",
        }
    }

    fn endpoint(self, conversation: &str) -> String {
        match self {
            Self::Group => format!("/v2/groups/{conversation}/messages"),
            Self::C2c => format!("/v2/users/{conversation}/messages"),
            Self::Channel => format!("/channels/{conversation}/messages"),
            Self::Direct => format!("/dms/{conversation}/messages"),
        }
    }

    fn upload_endpoint(self, conversation: &str) -> Option<String> {
        match self {
            Self::Group => Some(format!("/v2/groups/{conversation}/files")),
            Self::C2c => Some(format!("/v2/users/{conversation}/files")),
            Self::Channel | Self::Direct => None,
        }
    }
}

impl std::fmt::Display for QqScene {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Opaque local correlation stored by the gateway, not sent through the core.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GatewayRef {
    pub channel: String,
    pub conversation: String,
    pub external_id: String,
    pub scene: QqScene,
}

impl GatewayRef {
    pub fn new(
        channel: impl Into<String>,
        conversation: impl Into<String>,
        external_id: impl Into<String>,
        scene: QqScene,
    ) -> Self {
        Self {
            channel: channel.into(),
            conversation: conversation.into(),
            external_id: external_id.into(),
            scene,
        }
    }

    pub fn encode(&self) -> String {
        let bytes = serde_json::to_vec(self).expect("GatewayRef contains only serializable strings");
        format!("qqbot-ref-v1:{}", URL_SAFE_NO_PAD.encode(bytes))
    }

    pub fn decode(value: &str) -> Result<Self, AdapterError> {
        parse_gateway_ref(value)
    }
}

/// Make an opaque handle for the gateway's local `gateway_ref` table.
///
/// The encoded form is versioned and URL-safe so it may safely travel in the
/// adapter JSON protocol while retaining every field losslessly.
pub fn gateway_ref(
    channel: impl Into<String>,
    conversation: impl Into<String>,
    external_id: impl Into<String>,
    scene: QqScene,
) -> String {
    GatewayRef::new(channel, conversation, external_id, scene).encode()
}

pub fn parse_gateway_ref(value: &str) -> Result<GatewayRef, AdapterError> {
    let encoded = value.strip_prefix("qqbot-ref-v1:").ok_or_else(|| {
        AdapterError::Unexpected("qqbot gateway_ref has an unknown version".into())
    })?;
    let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|error| {
        AdapterError::Unexpected(format!("qqbot gateway_ref is not valid base64: {error}"))
    })?;
    let reference = serde_json::from_slice::<GatewayRef>(&bytes).map_err(|error| {
        AdapterError::Unexpected(format!("qqbot gateway_ref payload is invalid: {error}"))
    })?;
    if reference.channel.trim().is_empty()
        || reference.conversation.trim().is_empty()
        || reference.external_id.trim().is_empty()
    {
        return Err(AdapterError::Unexpected(
            "qqbot gateway_ref fields must not be empty".into(),
        ));
    }
    Ok(reference)
}

/// In-memory representation of QQ's local gateway_ref table.
#[derive(Debug, Default, Clone)]
pub struct GatewayRefTable {
    entries: HashMap<String, GatewayRef>,
}

impl GatewayRefTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn remember(&mut self, reference: GatewayRef) -> String {
        let key = reference.encode();
        self.entries.insert(key.clone(), reference);
        key
    }

    pub fn remember_message(
        &mut self,
        channel: impl Into<String>,
        conversation: impl Into<String>,
        external_id: impl Into<String>,
        scene: QqScene,
    ) -> String {
        self.remember(GatewayRef::new(channel, conversation, external_id, scene))
    }

    pub fn get(&self, key: &str) -> Option<&GatewayRef> {
        self.entries.get(key)
    }

    pub fn resolve(&self, key: &str) -> Result<&GatewayRef, AdapterError> {
        self.get(key).ok_or_else(|| {
            AdapterError::Unexpected("qqbot gateway_ref is not present in local table".into())
        })
    }

    pub fn remove(&mut self, key: &str) -> Option<GatewayRef> {
        self.entries.remove(key)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Debug, Clone)]
struct ParsedInbound {
    scene: QqScene,
    conversation: String,
    external_id: String,
    text: Option<String>,
    kind: MsgKind,
}

/// Translate a QQ gateway dispatch into the Onlyne envelope format.
///
/// `AT_MESSAGE_CREATE` is a task because it is an explicit bot-directed
/// message; all other supported message events are notes. Call
/// [`translate_inbound_event_as`] when a host has a different routing policy.
pub fn translate_inbound_event(event: &Value) -> Result<Envelope, AdapterError> {
    let parsed = parse_inbound_event(event)?;
    build_inbound_envelope(parsed)
}

/// String convenience wrapper around [`translate_inbound_event`].
pub fn translate_inbound_json(json_text: &str) -> Result<Envelope, AdapterError> {
    let value: Value = serde_json::from_str(json_text)?;
    translate_inbound_event(&value)
}

/// Translate a supported event while explicitly selecting Note or Task.
pub fn translate_inbound_event_as(
    event: &Value,
    kind: MsgKind,
) -> Result<Envelope, AdapterError> {
    if !matches!(kind, MsgKind::Note | MsgKind::Task) {
        return Err(AdapterError::Unexpected(format!(
            "qqbot inbound kind must be note or task, got {kind}"
        )));
    }
    let mut parsed = parse_inbound_event(event)?;
    parsed.kind = kind;
    build_inbound_envelope(parsed)
}

/// Translate and remember the external message correlation in one operation.
pub fn translate_inbound_with_ref(
    event: &Value,
    refs: &mut GatewayRefTable,
) -> Result<(Envelope, String), AdapterError> {
    let parsed = parse_inbound_event(event)?;
    let reference = refs.remember_message(
        CHANNEL,
        parsed.conversation.clone(),
        parsed.external_id.clone(),
        parsed.scene,
    );
    let envelope = build_inbound_envelope(parsed)?;
    Ok((envelope, reference))
}

fn parse_inbound_event(event: &Value) -> Result<ParsedInbound, AdapterError> {
    if let Some(op) = event.get("op").and_then(Value::as_i64)
        && op != 0
    {
        return Err(AdapterError::Unexpected(format!(
            "qqbot event op {op} is not a message dispatch"
        )));
    }
    let event_type = event
        .get("t")
        .or_else(|| event.get("event"))
        .and_then(Value::as_str)
        .ok_or_else(|| AdapterError::Unexpected("qqbot event type is missing".into()))?;
    let data = event
        .get("d")
        .or_else(|| event.get("data"))
        .unwrap_or(event);
    let external_id = data
        .get("id")
        .or_else(|| data.get("msg_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("qqbot-inbound")
        .to_string();

    let parsed = match event_type {
        "GROUP_AT_MESSAGE_CREATE" | "GROUP_MESSAGE_CREATE" => ParsedInbound {
            scene: QqScene::Group,
            conversation: required_string(data, "group_openid")?,
            external_id,
            text: clean_content(data.get("content").and_then(Value::as_str)),
            kind: if event_type == "GROUP_AT_MESSAGE_CREATE" {
                MsgKind::Task
            } else {
                MsgKind::Note
            },
        },
        "C2C_MESSAGE_CREATE" => {
            let user = nested_required_string(data, &["author", "user_openid"])?;
            ParsedInbound {
                scene: QqScene::C2c,
                conversation: user,
                external_id,
                text: clean_content(data.get("content").and_then(Value::as_str)),
                kind: MsgKind::Note,
            }
        }
        "AT_MESSAGE_CREATE" => ParsedInbound {
            scene: QqScene::Channel,
            conversation: required_string(data, "channel_id")?,
            external_id,
            text: clean_content(data.get("content").and_then(Value::as_str)),
            kind: MsgKind::Task,
        },
        "DIRECT_MESSAGE_CREATE" => ParsedInbound {
            scene: QqScene::Direct,
            conversation: required_string(data, "guild_id")?,
            external_id,
            text: clean_content(data.get("content").and_then(Value::as_str)),
            kind: MsgKind::Note,
        },
        other if other.contains("GROUP") => ParsedInbound {
            scene: QqScene::Group,
            conversation: required_string(data, "group_openid")?,
            external_id,
            text: clean_content(data.get("content").and_then(Value::as_str)),
            kind: MsgKind::Note,
        },
        other => {
            return Err(AdapterError::Unexpected(format!(
                "unsupported qqbot message type: {other}"
            )))
        }
    };
    if parsed.text.is_none() {
        return Err(AdapterError::Unexpected(
            "qqbot message has no supported text content".into(),
        ));
    }
    Ok(parsed)
}

fn build_inbound_envelope(parsed: ParsedInbound) -> Result<Envelope, AdapterError> {
    let from = Principal::gateway(CHANNEL, CHANNEL, Some(parsed.conversation));
    let causality = (parsed.kind == MsgKind::Task).then(|| Causality::root(new_task_id()));
    new_envelope(
        parsed.kind,
        from,
        Principal::role("gateway"),
        Body::text(parsed.text.unwrap_or_default()),
        causality,
    )
    .map_err(|error| AdapterError::Unexpected(format!("qqbot inbound envelope invalid: {error}")))
}

fn required_string(value: &Value, field: &str) -> Result<String, AdapterError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| AdapterError::Unexpected(format!("qqbot inbound field {field} is missing")))
}

fn nested_required_string(value: &Value, fields: &[&str]) -> Result<String, AdapterError> {
    nested_string(value, fields).ok_or_else(|| {
        AdapterError::Unexpected(format!("qqbot inbound field {} is missing", fields.join(".")))
    })
}

fn nested_string(value: &Value, fields: &[&str]) -> Option<String> {
    let mut cursor = value;
    for field in fields {
        cursor = cursor.get(*field)?;
    }
    cursor
        .as_str()
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned)
}

fn clean_content(content: Option<&str>) -> Option<String> {
    let mut text = content?.trim().to_owned();
    while let Some(start) = text.find("<@") {
        let Some(end) = text[start..].find('>').map(|offset| start + offset) else {
            break;
        };
        text.replace_range(start..=end, "");
    }
    let text = text.trim().to_owned();
    (!text.is_empty()).then_some(text)
}

/// Validate the common outbound contract before any token request or upload.
pub fn validate_outbound(msg: &Outbound) -> Result<(), AdapterError> {
    if !matches!(msg.kind, MsgKind::Note | MsgKind::Task) {
        return Err(AdapterError::Unexpected(format!(
            "unsupported qqbot outbound message kind: {}",
            msg.kind
        )));
    }
    if msg.conversation.trim().is_empty() {
        return Err(AdapterError::Unexpected(
            "qqbot outbound conversation is required".into(),
        ));
    }
    if msg.text.trim().is_empty() && msg.image.is_none() {
        return Err(AdapterError::Unexpected(
            "qqbot outbound needs text or image".into(),
        ));
    }
    if let Some(image) = &msg.image {
        let bytes = image.decode().map_err(|error| {
            AdapterError::Unexpected(format!("qqbot image data is invalid: {error}"))
        })?;
        if bytes.len() > IMAGE_DATA_MAX_BYTES {
            return Err(AdapterError::Unexpected(format!(
                "qqbot image exceeds {IMAGE_DATA_MAX_BYTES} bytes"
            )));
        }
    }
    Ok(())
}

/// Build a QQ message request without making a network call.
///
/// For images, `file_info` is the value returned by QQ's upload endpoint. If
/// omitted, the image's base64 is retained as a deterministic fixture value;
/// [`outbound_upload_request_json`] is the actual upload request shape used by
/// [`QqBotPlugin::send`].
pub fn outbound_message_request_json(
    msg: &Outbound,
    scene: QqScene,
    msg_seq: u64,
    file_info: Option<&str>,
) -> Result<Value, AdapterError> {
    validate_outbound(msg)?;
    if msg.image.is_some() && !matches!(scene, QqScene::Group | QqScene::C2c) {
        return Err(AdapterError::Unexpected(
            "qqbot image uploads are only supported for group and c2c scenes".into(),
        ));
    }
    let mut body = if let Some(image) = &msg.image {
        json!({
            "msg_type": 7,
            "media": {"file_info": file_info.unwrap_or(&image.data_base64)}
        })
    } else {
        json!({"msg_type": 0, "content": msg.text.trim()})
    };
    if matches!(scene, QqScene::Channel | QqScene::Direct) {
        body.as_object_mut()
            .expect("message body is an object")
            .remove("msg_type");
    } else {
        body["msg_seq"] = json!(msg_seq.max(1));
    }
    if let Some(reply_to) = msg.reply_to.as_deref().filter(|value| !value.trim().is_empty()) {
        body["msg_id"] = json!(reply_to);
    }
    Ok(body)
}

/// Build the QQ group/C2C media upload request. It performs the size check
/// before the request body is returned, and therefore before any upload call.
pub fn outbound_upload_request_json(
    msg: &Outbound,
    scene: QqScene,
) -> Result<Value, AdapterError> {
    validate_outbound(msg)?;
    let image = msg.image.as_ref().ok_or_else(|| {
        AdapterError::Unexpected("qqbot upload request requires an image".into())
    })?;
    let file_type = if image.mime == "image/gif" { 2 } else { 1 };
    let mut body = json!({
        "file_type": file_type,
        "srv_send_msg": false,
        "file_data": image.data_base64,
    });
    match scene {
        QqScene::Group => body["group_openid"] = json!(msg.conversation),
        QqScene::C2c => body["openid"] = json!(msg.conversation),
        QqScene::Channel | QqScene::Direct => {
            return Err(AdapterError::Unexpected(
                "qqbot image uploads are only supported for group and c2c scenes".into(),
            ))
        }
    }
    Ok(body)
}

/// Translate an outbound message using the scene encoded in its conversation.
pub fn translate_outbound(msg: &Outbound) -> Result<Value, AdapterError> {
    let (_, scene, _) = parse_target(&msg.conversation)?;
    outbound_message_request_json(msg, scene, 1, None)
}

fn parse_target(conversation: &str) -> Result<(String, QqScene, Option<GatewayRef>), AdapterError> {
    if conversation.starts_with("qqbot-ref-v1:") {
        let reference = parse_gateway_ref(conversation)?;
        if reference.channel != CHANNEL {
            return Err(AdapterError::Unexpected(format!(
                "qqbot gateway_ref belongs to channel {}, not {CHANNEL}",
                reference.channel
            )));
        }
        return Ok((reference.conversation.clone(), reference.scene, Some(reference)));
    }
    for (prefix, scene) in [
        ("group:", QqScene::Group),
        ("c2c:", QqScene::C2c),
        ("user:", QqScene::C2c),
        ("private:", QqScene::C2c),
        ("channel:", QqScene::Channel),
        ("direct:", QqScene::Direct),
    ] {
        if let Some(value) = conversation.strip_prefix(prefix) {
            if value.trim().is_empty() {
                return Err(AdapterError::Unexpected(
                    "qqbot outbound conversation id is empty".into(),
                ));
            }
            return Ok((value.to_owned(), scene, None));
        }
    }
    // A bare target is treated as a channel id, matching QQ's most common
    // gateway address while still allowing explicit scene prefixes.
    Ok((conversation.to_owned(), QqScene::Channel, None))
}

#[derive(Debug, Clone)]
struct AccessToken {
    value: String,
    expires_at: Instant,
}

pub struct QqBotPlugin {
    credentials: Option<QqBotCredentials>,
    sandbox: bool,
    client: Client,
    token: Option<AccessToken>,
    started_at: Instant,
    running: bool,
    sequence: u64,
    refs: GatewayRefTable,
}

impl QqBotPlugin {
    pub fn new(credentials: QqBotCredentials, sandbox: bool) -> Self {
        Self::with_optional_credentials(Some(credentials), sandbox)
    }

    pub fn from_env(sandbox: bool) -> Result<Self, AdapterError> {
        Ok(Self::new(QqBotCredentials::from_env()?, sandbox))
    }

    /// Construct an instance that can expose onboarding before credentials are
    /// provisioned. Network operations return an error naming the env var.
    pub fn unconfigured(sandbox: bool) -> Self {
        Self::with_optional_credentials(None, sandbox)
    }

    fn with_optional_credentials(credentials: Option<QqBotCredentials>, sandbox: bool) -> Self {
        Self {
            credentials,
            sandbox,
            client: Client::new(),
            token: None,
            started_at: Instant::now(),
            running: false,
            sequence: 0,
            refs: GatewayRefTable::new(),
        }
    }

    pub fn references(&self) -> &GatewayRefTable {
        &self.refs
    }

    pub fn references_mut(&mut self) -> &mut GatewayRefTable {
        &mut self.refs
    }

    fn api_base(&self) -> &'static str {
        if self.sandbox { SANDBOX_API_URL } else { API_URL }
    }

    fn next_sequence(&mut self) -> u64 {
        self.sequence = (self.sequence % 10_000) + 1;
        self.sequence
    }

    async fn access_token(&mut self) -> Result<String, AdapterError> {
        let credentials = self.credentials.as_ref().ok_or_else(|| {
            AdapterError::Unexpected(format!(
                "qqbot credentials missing: set {APP_ID_ENV} and {APP_SECRET_ENV}"
            ))
        })?;
        if let Some(token) = &self.token
            && Instant::now() < token.expires_at
        {
            return Ok(token.value.clone());
        }
        let response = self
            .client
            .post(TOKEN_URL)
            .json(&json!({
                "appId": credentials.app_id,
                "clientSecret": credentials.app_secret,
            }))
            .send()
            .await
            .map_err(|error| AdapterError::Unexpected(format!("qqbot token request failed: {error}")))?;
        let status = response.status();
        let text = response.text().await.map_err(|error| {
            AdapterError::Unexpected(format!("qqbot token response failed: {error}"))
        })?;
        if !status.is_success() {
            return Err(AdapterError::Unexpected(format!(
                "qqbot token request {status}: {text}"
            )));
        }
        let value: Value = serde_json::from_str(&text)?;
        let token = value
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|token| !token.trim().is_empty())
            .ok_or_else(|| AdapterError::Unexpected("qqbot token response omitted access_token".into()))?
            .to_owned();
        let expires_in = value
            .get("expires_in")
            .or_else(|| value.get("expiresIn"))
            .and_then(Value::as_u64)
            .unwrap_or(7_200);
        self.token = Some(AccessToken {
            value: token.clone(),
            expires_at: Instant::now() + Duration::from_secs(expires_in.saturating_sub(60)),
        });
        Ok(token)
    }

    async fn post_api(&self, token: &str, path: &str, body: Value) -> Result<Value, AdapterError> {
        let response = self
            .client
            .post(format!("{}{}", self.api_base(), path))
            .bearer_auth(token)
            .header("Authorization", format!("QQBot {token}"))
            .json(&body)
            .send()
            .await
            .map_err(|error| AdapterError::Unexpected(format!("qqbot request failed: {error}")))?;
        let status = response.status();
        let text = response.text().await.map_err(|error| {
            AdapterError::Unexpected(format!("qqbot response failed: {error}"))
        })?;
        if !status.is_success() {
            return Err(AdapterError::Unexpected(format!(
                "qqbot request {path} {status}: {text}"
            )));
        }
        if text.trim().is_empty() {
            return Ok(Value::Object(Default::default()));
        }
        Ok(serde_json::from_str(&text)?)
    }

    async fn upload_image(
        &self,
        token: &str,
        scene: QqScene,
        conversation: &str,
        body: Value,
    ) -> Result<String, AdapterError> {
        let path = scene.upload_endpoint(conversation).ok_or_else(|| {
            AdapterError::Unexpected("qqbot image uploads are only supported for group and c2c scenes".into())
        })?;
        let result = self.post_api(token, &path, body).await?;
        result
            .get("file_info")
            .or_else(|| result.pointer("/media/file_info"))
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
            .ok_or_else(|| AdapterError::Unexpected("qqbot upload response omitted file_info".into()))
    }
}

#[async_trait]
impl GatewayPlugin for QqBotPlugin {
    fn platform(&self) -> &'static str {
        CHANNEL
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::Probe]
    }

    async fn start(&mut self, host: &mut dyn GatewayHost) -> Result<(), AdapterError> {
        // Resolve before touching the host so an unconfigured gateway reports a
        // useful credential error instead of registering a dead channel.
        let _ = self.credentials.as_ref().ok_or_else(|| {
            AdapterError::Unexpected(format!(
                "qqbot credentials missing: set {APP_ID_ENV} and {APP_SECRET_ENV}"
            ))
        })?;
        host.register_channel(&RegisterChannelArgs {
            platform: CHANNEL.into(),
            channel: CHANNEL.into(),
            conversations: None,
        })
        .await?;
        self.running = true;
        self.started_at = Instant::now();
        host.report_health(&HealthArgs {
            state: "online".into(),
            detail: None,
            uptime_s: 0,
        })
        .await?;
        Ok(())
    }

    async fn send(&mut self, msg: &Outbound) -> Result<SendReceipt, AdapterError> {
        // This is intentionally first: oversized images must fail before token
        // acquisition and before an upload request can be attempted.
        validate_outbound(msg)?;
        let (conversation, scene, reference) = parse_target(&msg.conversation)?;
        let sequence = self.next_sequence();
        let token = self.access_token().await?;
        let file_info = if msg.image.is_some() {
            let upload = outbound_upload_request_json(msg, scene)?;
            Some(self.upload_image(&token, scene, &conversation, upload).await?)
        } else {
            None
        };
        let body = outbound_message_request_json(msg, scene, sequence, file_info.as_deref())?;
        let response = self
            .post_api(&token, &scene.endpoint(&conversation), body)
            .await?;
        let external_id = response
            .get("id")
            .or_else(|| response.get("msg_id"))
            .or_else(|| response.get("message_id"))
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("qqbot-out-{sequence}"));
        self.refs.remember_message(CHANNEL, conversation, external_id.clone(), scene);
        if let Some(reference) = reference {
            self.refs.remember(reference);
        }
        Ok(SendReceipt { external_id })
    }

    async fn probe(&mut self) -> Result<AdapterHealth, AdapterError> {
        if self.credentials.is_none() {
            return Err(AdapterError::Unexpected(format!(
                "qqbot credentials missing: set {APP_ID_ENV} and {APP_SECRET_ENV}"
            )));
        }
        Ok(AdapterHealth {
            state: if self.running { "online" } else { "stopped" }.into(),
            detail: None,
            uptime_s: self.started_at.elapsed().as_secs(),
        })
    }

    async fn stop(&mut self, _reason: &str) -> Result<(), AdapterError> {
        self.running = false;
        self.token = None;
        Ok(())
    }

    fn onboarding(&mut self) -> Result<Option<OnboardingPrompt>, AdapterError> {
        if self.credentials.is_some() {
            Ok(None)
        } else {
            Ok(Some(onboarding_prompt()))
        }
    }

    async fn list_conversations(&mut self) -> Result<Vec<ConversationInfo>, AdapterError> {
        // QQ does not provide a stable, permission-independent enumeration API.
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group_event() -> Value {
        json!({
            "op": 0,
            "t": "GROUP_MESSAGE_CREATE",
            "d": {
                "id": "m1",
                "content": "<@!bot> hello",
                "group_openid": "group-1",
                "author": {"member_openid": "member-1", "username": "alice"}
            }
        })
    }

    fn text_outbound() -> Outbound {
        Outbound {
            conversation: "group:g1".into(),
            text: "hello".into(),
            image: None,
            reply_to: Some("m0".into()),
            kind: MsgKind::Note,
        }
    }

    #[test]
    fn inbound_group_fixture_becomes_gateway_note() {
        let envelope = translate_inbound_event(&group_event()).unwrap();
        assert_eq!(envelope.kind, MsgKind::Note);
        assert_eq!(envelope.body.text.as_deref(), Some("hello"));
        assert_eq!(
            envelope.from,
            Principal::gateway("qqbot", "qqbot", Some("group-1".into()))
        );
        assert_eq!(envelope.to, Principal::role("gateway"));
    }

    #[test]
    fn inbound_at_fixture_becomes_gateway_task() {
        let event = json!({
            "t": "AT_MESSAGE_CREATE",
            "d": {
                "id": "m2",
                "content": "@bot do this",
                "channel_id": "channel-1",
                "author": {"id": "user-1"}
            }
        });
        let envelope = translate_inbound_event(&event).unwrap();
        assert_eq!(envelope.kind, MsgKind::Task);
        assert!(envelope.causality.is_some());
    }

    #[test]
    fn outbound_text_has_qq_shape_and_reply() {
        let request = translate_outbound(&text_outbound()).unwrap();
        assert_eq!(request["msg_type"], 0);
        assert_eq!(request["content"], "hello");
        assert_eq!(request["msg_id"], "m0");
        assert_eq!(request["msg_seq"], 1);
    }

    #[test]
    fn gateway_ref_round_trips_through_opaque_table_key() {
        let key = gateway_ref("qqbot", "conv/with:punct", "external id", QqScene::C2c);
        let decoded = parse_gateway_ref(&key).unwrap();
        assert_eq!(decoded.channel, "qqbot");
        assert_eq!(decoded.conversation, "conv/with:punct");
        assert_eq!(decoded.external_id, "external id");
        assert_eq!(decoded.scene, QqScene::C2c);

        let mut table = GatewayRefTable::new();
        let stored = table.remember(decoded.clone());
        assert_eq!(table.resolve(&stored).unwrap(), &decoded);
    }

    #[test]
    fn unsupported_type_is_a_clean_error() {
        let error = translate_inbound_event(&json!({
            "t": "MESSAGE_DELETE",
            "d": {"id": "m1"}
        }))
        .unwrap_err();
        assert!(error.to_string().contains("unsupported qqbot message type"));
    }

    #[test]
    fn image_limit_is_rejected_before_upload_request_is_built() {
        let bytes = vec![7u8; IMAGE_DATA_MAX_BYTES + 1];
        let msg = Outbound {
            conversation: "group:g1".into(),
            text: String::new(),
            image: Some(onlyne_proto::ImagePart {
                data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
                mime: "image/png".into(),
                name: None,
            }),
            reply_to: None,
            kind: MsgKind::Note,
        };
        let error = outbound_upload_request_json(&msg, QqScene::Group).unwrap_err();
        assert!(error.to_string().contains("exceeds 2097152 bytes"));
    }

    #[test]
    fn unsupported_outbound_kind_is_rejected() {
        let mut msg = text_outbound();
        msg.kind = MsgKind::Completion;
        let error = translate_outbound(&msg).unwrap_err();
        assert!(error.to_string().contains("unsupported qqbot outbound message kind"));
    }
}
