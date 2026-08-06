use crate::{
    adapters::bound_matches,
    config::{Env, QqBotConfig},
    core::*,
    media,
};
use anyhow::{Context, anyhow};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tokio::{
    sync::{Mutex, mpsc},
    task::JoinHandle,
    time::{Duration, Instant, sleep},
};
use tokio_tungstenite::{connect_async, tungstenite::Message};

const TOKEN_URL: &str = "https://bots.qq.com/app/getAppAccessToken";
const PROD: &str = "https://api.sgroup.qq.com";
const SANDBOX: &str = "https://sandbox.api.sgroup.qq.com";
const INTENTS: i64 = (1 << 25) | (1 << 30) | (1 << 12);
const TOKEN_REFRESH_SKEW: Duration = Duration::from_secs(300);
static MSG_SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
struct QqAccessToken {
    value: String,
    expires_at: Instant,
}

impl QqAccessToken {
    fn is_valid(&self) -> bool {
        Instant::now() + TOKEN_REFRESH_SKEW < self.expires_at
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QqScene {
    Group,
    C2c,
    Channel,
    Direct,
}

impl QqScene {
    fn as_str(self) -> &'static str {
        match self {
            QqScene::Group => "group",
            QqScene::C2c => "c2c",
            QqScene::Channel => "channel",
            QqScene::Direct => "direct",
        }
    }
}

#[derive(Clone)]
struct QqSessionCache {
    scenes: Arc<Mutex<HashMap<String, QqScene>>>,
    last_message_ids: Arc<Mutex<HashMap<String, MessageId>>>,
}

impl QqSessionCache {
    fn new() -> Self {
        Self {
            scenes: Arc::new(Mutex::new(HashMap::new())),
            last_message_ids: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn remember(&self, conv: &str, scene: QqScene, message_id: &MessageId) {
        self.scenes.lock().await.insert(conv.to_string(), scene);
        self.last_message_ids
            .lock()
            .await
            .insert(conv.to_string(), message_id.clone());
    }

    async fn scene(&self, conv: &str) -> Option<QqScene> {
        self.scenes.lock().await.get(conv).copied()
    }

    async fn last_message_id(&self, conv: &str) -> Option<MessageId> {
        self.last_message_ids.lock().await.get(conv).cloned()
    }

    async fn remember_outbound(&self, conv: &str, message_id: &MessageId) {
        self.last_message_ids
            .lock()
            .await
            .insert(conv.to_string(), message_id.clone());
    }
}

pub struct QqBotAdapter {
    app_id: String,
    app_secret: String,
    sandbox: bool,
    rich_text: bool,
    bind_conversation_id: Option<String>,
    client: Client,
    token: Arc<Mutex<Option<QqAccessToken>>>,
    running: Arc<AtomicBool>,
    cache: QqSessionCache,
    task: Option<JoinHandle<()>>,
}

impl QqBotAdapter {
    pub fn new(cfg: &QqBotConfig, env: &Env) -> anyhow::Result<Self> {
        Ok(Self {
            app_id: env.secret(&cfg.app_id, &cfg.app_id_env, "qqbot app_id")?,
            app_secret: env.secret(&cfg.app_secret, &cfg.app_secret_env, "qqbot app_secret")?,
            sandbox: cfg.sandbox,
            rich_text: cfg.rich_text,
            bind_conversation_id: env.value(&cfg.bind_conversation_id),
            client: Client::new(),
            token: Arc::new(Mutex::new(None)),
            running: Arc::new(AtomicBool::new(false)),
            cache: QqSessionCache::new(),
            task: None,
        })
    }

    fn base(&self) -> &'static str {
        if self.sandbox { SANDBOX } else { PROD }
    }

    async fn access_token(&self) -> anyhow::Result<String> {
        if let Some(t) = self
            .token
            .lock()
            .await
            .clone()
            .filter(QqAccessToken::is_valid)
        {
            return Ok(t.value);
        }
        let t = qq_token(&self.client, &self.app_id, &self.app_secret).await?;
        *self.token.lock().await = Some(t.clone());
        Ok(t.value)
    }

    async fn refresh_access_token(&self) -> anyhow::Result<String> {
        let t = qq_token(&self.client, &self.app_id, &self.app_secret).await?;
        *self.token.lock().await = Some(t.clone());
        Ok(t.value)
    }
}

#[async_trait]
impl Adapter for QqBotAdapter {
    fn channel_id(&self) -> ChannelId {
        ChannelId("qqbot".into())
    }

    async fn start(&mut self, ctx: AdapterContext) -> anyhow::Result<()> {
        self.check().await?;
        self.running.store(true, Ordering::SeqCst);
        let app_id = self.app_id.clone();
        let app_secret = self.app_secret.clone();
        let sandbox = self.sandbox;
        let bind = self.bind_conversation_id.clone();
        let running = self.running.clone();
        let inbound = ctx.inbound.clone();
        let events = ctx.events.clone();
        let cache = self.cache.clone();
        self.task = Some(tokio::spawn(async move {
            while running.load(Ordering::SeqCst) {
                if let Err(e) = qq_loop(
                    &app_id,
                    &app_secret,
                    sandbox,
                    &bind,
                    &cache,
                    &inbound,
                    &events,
                )
                .await
                {
                    let reason = e.to_string();
                    tracing::warn!(error = %reason, "qqbot reconnecting");
                    let _ = events
                        .send(Event::AdapterReconnecting {
                            channel_id: ChannelId("qqbot".into()),
                            reason,
                        })
                        .await;
                    sleep(Duration::from_secs(5)).await;
                }
            }
        }));
        Ok(())
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        self.running.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn health(&self) -> AdapterHealth {
        if self.running.load(Ordering::SeqCst) {
            AdapterHealth::Ready
        } else {
            AdapterHealth::Stopped
        }
    }

    async fn list_conversations(&self) -> anyhow::Result<Vec<Conversation>> {
        Ok(vec![])
    }

    async fn send_message(&self, msg: OutboundMessage) -> anyhow::Result<MessageEnvelope> {
        let target = self.resolve_target(&msg.conversation_id.0).await;
        let mut token = self.access_token().await?;
        let mut sent = Vec::new();

        if let Some(text) = msg.text.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            let mut body = if msg.format == MessageFormat::Markdown {
                if !self.rich_text {
                    return Err(anyhow!("qqbot rich_text disabled"));
                }
                qq_markdown_body(text)
            } else {
                qq_text_body(text)
            };
            if let Some(reply_to) = &msg.reply_to_message_id {
                body["msg_id"] = json!(reply_to.0);
            }
            let kind = if msg.format == MessageFormat::Markdown {
                "markdown"
            } else {
                "text"
            };
            let sent_body = match self
                .send_to_target(
                    &token,
                    &target,
                    body.clone(),
                    msg.format == MessageFormat::Markdown,
                )
                .await
            {
                Ok(v) => v,
                Err(e) if is_qq_auth_error(&e) => {
                    token = self.refresh_access_token().await?;
                    self.send_to_target(
                        &token,
                        &target,
                        body,
                        msg.format == MessageFormat::Markdown,
                    )
                    .await?
                }
                Err(e) => return Err(e),
            };
            self.cache
                .remember_outbound(&msg.conversation_id.0, &sent_body.0)
                .await;
            sent.push((kind, sent_body));
        }

        for a in &msg.attachments {
            let attachment = match self
                .send_attachment(&token, &target, a, msg.reply_to_message_id.as_ref())
                .await
            {
                Ok(v) => v,
                Err(e) if is_qq_auth_error(&e) => {
                    token = self.refresh_access_token().await?;
                    self.send_attachment(&token, &target, a, msg.reply_to_message_id.as_ref())
                        .await?
                }
                Err(e) => return Err(e),
            };
            self.cache
                .remember_outbound(&msg.conversation_id.0, &attachment.0)
                .await;
            sent.push(("attachment", attachment));
        }

        let (message_id, platform_metadata) = delivery_metadata(sent)?;
        Ok(MessageEnvelope {
            channel_id: self.channel_id(),
            conversation_id: msg.conversation_id,
            message_id,
            direction: Direction::Outbound,
            sender_id: None,
            sender_name: None,
            text: msg.text,
            format: msg.format,
            attachments: msg.attachments,
            delivery_state: DeliveryState::Sent,
            timestamp: Utc::now(),
            platform_metadata,
        })
    }

    async fn check(&self) -> anyhow::Result<()> {
        let token = self.access_token().await?;
        let v: Value = self
            .client
            .get(format!("{}/gateway/bot", self.base()))
            .header("Authorization", format!("QQBot {token}"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if v.get("url").and_then(Value::as_str).is_some() {
            Ok(())
        } else {
            Err(anyhow!("qqbot gateway failed: {v}"))
        }
    }
}

fn qq_text_body(text: &str) -> Value {
    json!({"content": text, "msg_type": 0})
}

fn qq_markdown_body(text: &str) -> Value {
    json!({"msg_type": 2, "markdown": {"content": text}})
}

fn is_qq_auth_error(e: &anyhow::Error) -> bool {
    let s = e.to_string().to_ascii_lowercase();
    s.contains("401")
        || s.contains("access_token")
        || s.contains("invalid token")
        || s.contains("token expired")
        || s.contains("qqbot token")
}

fn is_markdown_not_allowed(e: &anyhow::Error) -> bool {
    let s = e.to_string();
    s.contains("不允许发送原生 markdown")
        || s.to_ascii_lowercase().contains("markdown") && s.contains("400")
}

fn delivery_metadata(sent: Vec<(&str, (MessageId, Value))>) -> anyhow::Result<(MessageId, Value)> {
    let Some((_, (first_id, _))) = sent.first() else {
        return Err(anyhow!("qqbot send_message needs text or attachments"));
    };
    let parts: Vec<Value> = sent
        .iter()
        .map(|(kind, (id, meta))| json!({"kind": kind, "message_id": id.0, "metadata": meta}))
        .collect();
    let last = sent
        .last()
        .map(|(_, (_, meta))| meta.clone())
        .unwrap_or(Value::Null);
    Ok((
        first_id.clone(),
        json!({"delivery_parts": parts, "last_response": last}),
    ))
}

#[derive(Debug, Clone)]
enum QqTarget {
    Known { scene: QqScene, id: String },
    Unknown(String),
}

impl QqBotAdapter {
    async fn resolve_target(&self, conv: &str) -> QqTarget {
        if let Some(id) = conv.strip_prefix("group:") {
            return QqTarget::Known {
                scene: QqScene::Group,
                id: id.to_string(),
            };
        }
        if let Some(id) = conv
            .strip_prefix("c2c:")
            .or_else(|| conv.strip_prefix("user:"))
            .or_else(|| conv.strip_prefix("private:"))
        {
            return QqTarget::Known {
                scene: QqScene::C2c,
                id: id.to_string(),
            };
        }
        if let Some(id) = conv.strip_prefix("channel:") {
            return QqTarget::Known {
                scene: QqScene::Channel,
                id: id.to_string(),
            };
        }
        if let Some(id) = conv.strip_prefix("direct:") {
            return QqTarget::Known {
                scene: QqScene::Direct,
                id: id.to_string(),
            };
        }
        if let Some(scene) = self.cache.scene(conv).await {
            QqTarget::Known {
                scene,
                id: conv.to_string(),
            }
        } else {
            QqTarget::Unknown(conv.to_string())
        }
    }

    async fn send_to_target(
        &self,
        token: &str,
        target: &QqTarget,
        body: Value,
        markdown: bool,
    ) -> anyhow::Result<(MessageId, Value)> {
        match target {
            QqTarget::Known { scene, id } => {
                self.send_known(token, *scene, id, body, markdown).await
            }
            QqTarget::Unknown(id) => self.send_unknown(token, id, body, markdown).await,
        }
    }

    async fn send_unknown(
        &self,
        token: &str,
        id: &str,
        body: Value,
        markdown: bool,
    ) -> anyhow::Result<(MessageId, Value)> {
        let mut last = None;
        for scene in [QqScene::Group, QqScene::C2c, QqScene::Channel] {
            match self
                .send_known(token, scene, id, body.clone(), markdown)
                .await
            {
                Ok(v) => {
                    self.cache.scenes.lock().await.insert(id.to_string(), scene);
                    return Ok(v);
                }
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| anyhow!("qqbot send failed")))
    }

    async fn send_known(
        &self,
        token: &str,
        scene: QqScene,
        id: &str,
        mut body: Value,
        markdown: bool,
    ) -> anyhow::Result<(MessageId, Value)> {
        if matches!(scene, QqScene::Channel | QqScene::Direct)
            && body.get("msg_id").is_none()
            && let Some(message_id) = self.cache.last_message_id(id).await
        {
            body["msg_id"] = json!(message_id.0);
        }
        add_scene_send_fields(&mut body, scene);
        match self.send_scoped(token, scene, id, body.clone()).await {
            Ok(v) => Ok(v),
            Err(e) if markdown && is_markdown_not_allowed(&e) => {
                let text = body
                    .pointer("/markdown/content")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let mut fallback = qq_text_body(text);
                add_scene_send_fields(&mut fallback, scene);
                self.send_scoped(token, scene, id, fallback).await
            }
            Err(e) => Err(e),
        }
    }

    async fn send_scoped(
        &self,
        token: &str,
        scene: QqScene,
        conv: &str,
        body: Value,
    ) -> anyhow::Result<(MessageId, Value)> {
        let path = match scene {
            QqScene::Group => format!("/v2/groups/{conv}/messages"),
            QqScene::C2c => format!("/v2/users/{conv}/messages"),
            QqScene::Channel => format!("/channels/{conv}/messages"),
            QqScene::Direct => format!("/dms/{conv}/messages"),
        };
        let resp = self
            .client
            .post(format!("{}{}", self.base(), path))
            .header("Authorization", format!("QQBot {token}"))
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(anyhow!(
                "qqbot {} message {}: {}",
                scene.as_str(),
                status,
                text
            ));
        }
        let v: Value = serde_json::from_str(&text)?;
        let id = message_id_from_response(&v, "qqbot-out");
        Ok((MessageId(id), v))
    }

    async fn send_attachment(
        &self,
        token: &str,
        target: &QqTarget,
        a: &AttachmentRef,
        reply_to: Option<&MessageId>,
    ) -> anyhow::Result<(MessageId, Value)> {
        match target {
            QqTarget::Known { scene, id } => {
                self.send_attachment_known(token, *scene, id, a, reply_to)
                    .await
            }
            QqTarget::Unknown(id) => {
                let mut last = None;
                for scene in [QqScene::Group, QqScene::C2c] {
                    match self
                        .send_attachment_known(token, scene, id, a, reply_to)
                        .await
                    {
                        Ok(v) => {
                            self.cache.scenes.lock().await.insert(id.to_string(), scene);
                            return Ok(v);
                        }
                        Err(e) => last = Some(e),
                    }
                }
                Err(last.unwrap_or_else(|| anyhow!("qqbot attachment send failed")))
            }
        }
    }

    async fn send_attachment_known(
        &self,
        token: &str,
        scene: QqScene,
        conv: &str,
        a: &AttachmentRef,
        reply_to: Option<&MessageId>,
    ) -> anyhow::Result<(MessageId, Value)> {
        match scene {
            QqScene::Group | QqScene::C2c => {}
            _ => {
                return Err(anyhow!(
                    "qqbot attachments are only implemented for group/c2c"
                ));
            }
        }
        let file_type = match a.kind {
            AttachmentKind::Image => 1,
            AttachmentKind::Video => 2,
            AttachmentKind::Audio | AttachmentKind::Voice => 3,
            AttachmentKind::File => 4,
        };
        let name = a.file_name.clone().unwrap_or_else(|| "media".into());
        let media = self
            .upload_attachment(token, scene, conv, file_type, &name, a)
            .await?;
        let mut body = json!({"msg_type": 7, "media": {"file_info": media.get("file_info").cloned().unwrap_or(Value::Null)}});
        if let Some(reply_to) = reply_to {
            body["msg_id"] = json!(reply_to.0);
        }
        add_scene_send_fields(&mut body, scene);
        self.send_scoped(token, scene, conv, body).await
    }

    async fn upload_attachment(
        &self,
        token: &str,
        scene: QqScene,
        conv: &str,
        file_type: i32,
        name: &str,
        a: &AttachmentRef,
    ) -> anyhow::Result<Value> {
        let mut body = json!({"file_type": file_type, "srv_send_msg": false});
        if file_type == 4 {
            body["file_name"] = json!(name);
        }
        if let Some(url) = a.url.as_deref().filter(|u| !u.trim().is_empty()) {
            body["url"] = json!(url);
        } else {
            let src = a
                .path
                .as_ref()
                .map(|p| p.to_string_lossy().to_string())
                .context("attachment needs path or url")?;
            body["file_data"] = json!(BASE64.encode(media::read_bytes(&src).await?));
        }
        let path = match scene {
            QqScene::Group => {
                body["group_openid"] = json!(conv);
                format!("/v2/groups/{conv}/files")
            }
            QqScene::C2c => {
                body["openid"] = json!(conv);
                format!("/v2/users/{conv}/files")
            }
            _ => return Err(anyhow!("qqbot upload is only implemented for group/c2c")),
        };
        let resp = self
            .client
            .post(format!("{}{}", self.base(), path))
            .header("Authorization", format!("QQBot {token}"))
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(anyhow!(
                "qqbot {} upload {}: {}",
                scene.as_str(),
                status,
                text
            ));
        }
        Ok(serde_json::from_str(&text)?)
    }
}

fn add_scene_send_fields(body: &mut Value, scene: QqScene) {
    if matches!(scene, QqScene::Group | QqScene::C2c) {
        body["msg_seq"] = json!(next_msg_seq());
    }
    if matches!(scene, QqScene::Channel | QqScene::Direct) {
        body.as_object_mut().map(|o| o.remove("msg_type"));
    }
}

fn next_msg_seq() -> u64 {
    (MSG_SEQ.fetch_add(1, Ordering::SeqCst) % 10_000) + 1
}

fn message_id_from_response(v: &Value, fallback_prefix: &str) -> String {
    v.get("id")
        .or_else(|| v.get("msg_id"))
        .or_else(|| v.get("message_id"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| now_id(fallback_prefix).0)
}

async fn qq_token(
    client: &Client,
    app_id: &str,
    app_secret: &str,
) -> anyhow::Result<QqAccessToken> {
    let v: Value = client
        .post(TOKEN_URL)
        .json(&json!({"appId": app_id, "clientSecret": app_secret}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let value = v
        .get("access_token")
        .and_then(Value::as_str)
        .context("qqbot token")?
        .to_string();
    let expires_in = v
        .get("expires_in")
        .or_else(|| v.get("expiresIn"))
        .and_then(Value::as_u64)
        .unwrap_or(7200);
    Ok(QqAccessToken {
        value,
        expires_at: Instant::now() + Duration::from_secs(expires_in),
    })
}

async fn qq_loop(
    app_id: &str,
    app_secret: &str,
    sandbox: bool,
    bind: &Option<String>,
    cache: &QqSessionCache,
    inbound: &mpsc::Sender<MessageEnvelope>,
    events: &mpsc::Sender<Event>,
) -> anyhow::Result<()> {
    let client = Client::new();
    let token = qq_token(&client, app_id, app_secret).await?.value;
    let base = if sandbox { SANDBOX } else { PROD };
    let gw: Value = client
        .get(format!("{base}/gateway/bot"))
        .header("Authorization", format!("QQBot {token}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let url = gw
        .get("url")
        .and_then(Value::as_str)
        .context("gateway url")?;
    let (mut ws, _) = connect_async(url).await?;
    let _ = events
        .send(Event::AdapterStarted {
            channel_id: ChannelId("qqbot".into()),
        })
        .await;
    let mut seq: Option<i64> = None;
    let mut heartbeat_period = Duration::from_secs(40);
    let mut heartbeat =
        tokio::time::interval_at(Instant::now() + heartbeat_period, heartbeat_period);
    let mut last_rx = Instant::now();
    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                if last_rx.elapsed() > heartbeat_period * 3 {
                    return Err(anyhow!("qqbot heartbeat timeout"));
                }
                ws.send(Message::Text(json!({"op": 1, "d": seq}).to_string().into())).await?;
            }
            item = ws.next() => {
                let Some(item) = item else {
                    return Err(anyhow!("qqbot websocket closed"));
                };
                if let Message::Text(t) = item? {
                    last_rx = Instant::now();
                    let v: Value = serde_json::from_str(&t)?;
                    if let Some(s) = v.get("s").and_then(Value::as_i64) {
                        seq = Some(s);
                    }
                    match v.get("op").and_then(Value::as_i64).unwrap_or(-1) {
                        10 => {
                            heartbeat_period = Duration::from_millis(v.pointer("/d/heartbeat_interval").and_then(Value::as_u64).unwrap_or(41250));
                            heartbeat = tokio::time::interval_at(Instant::now() + heartbeat_period, heartbeat_period);
                            ws.send(Message::Text(json!({"op": 2, "d": {"token": format!("QQBot {token}"), "intents": INTENTS, "shard": [0, 1]}}).to_string().into())).await?;
                        }
                        0 => {
                            if let Some(env) = parse_dispatch(v, bind) {
                                if let Some(scene) = env.platform_metadata.pointer("/onlyne/qq_scene").and_then(Value::as_str).and_then(scene_from_str) {
                                    cache.remember(&env.conversation_id.0, scene, &env.message_id).await;
                                }
                                let _ = inbound.send(env).await;
                            }
                        }
                        7 | 9 => return Err(anyhow!("qqbot reconnect requested")),
                        _ => {}
                    }
                }
            }
        }
    }
}

fn scene_from_str(s: &str) -> Option<QqScene> {
    match s {
        "group" => Some(QqScene::Group),
        "c2c" => Some(QqScene::C2c),
        "channel" => Some(QqScene::Channel),
        "direct" => Some(QqScene::Direct),
        _ => None,
    }
}

fn parse_dispatch(v: Value, bind: &Option<String>) -> Option<MessageEnvelope> {
    if v.get("op").and_then(Value::as_i64) != Some(0) {
        return None;
    }
    let t = v.get("t").and_then(Value::as_str).unwrap_or("");
    let d = v.get("d")?;
    let parsed = parse_conversation(t, d)?;
    if !bound_matches(bind, &parsed.conversation_id) {
        return None;
    }
    let message_id = MessageId(
        d.get("id")
            .or_else(|| d.get("msg_id"))
            .and_then(Value::as_str)
            .unwrap_or("qqbot-in")
            .to_string(),
    );
    let mut metadata = v;
    metadata["onlyne"] = json!({"qq_scene": parsed.scene.as_str()});
    Some(MessageEnvelope {
        channel_id: ChannelId("qqbot".into()),
        conversation_id: ConversationId(parsed.conversation_id),
        message_id,
        direction: Direction::Inbound,
        sender_id: parsed.sender_id,
        sender_name: parsed.sender_name,
        text: parsed.text,
        format: MessageFormat::Plain,
        attachments: vec![],
        delivery_state: DeliveryState::Delivered,
        timestamp: Utc::now(),
        platform_metadata: metadata,
    })
}

struct ParsedQqInbound {
    scene: QqScene,
    conversation_id: String,
    sender_id: Option<String>,
    sender_name: Option<String>,
    text: Option<String>,
}

fn parse_conversation(t: &str, d: &Value) -> Option<ParsedQqInbound> {
    match t {
        "GROUP_AT_MESSAGE_CREATE" | "GROUP_MESSAGE_CREATE" => Some(ParsedQqInbound {
            scene: QqScene::Group,
            conversation_id: d.get("group_openid")?.as_str()?.to_string(),
            sender_id: d
                .get("author")
                .and_then(|a| a.get("member_openid"))
                .and_then(Value::as_str)
                .map(str::to_string),
            sender_name: d
                .get("author")
                .and_then(|a| a.get("username"))
                .and_then(Value::as_str)
                .map(str::to_string),
            text: clean_qq_content(d.get("content").and_then(Value::as_str)),
        }),
        "C2C_MESSAGE_CREATE" => {
            let author = d.get("author")?;
            let user = author.get("user_openid")?.as_str()?.to_string();
            Some(ParsedQqInbound {
                scene: QqScene::C2c,
                conversation_id: user.clone(),
                sender_id: Some(user),
                sender_name: author
                    .get("username")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                text: clean_qq_content(d.get("content").and_then(Value::as_str)),
            })
        }
        "AT_MESSAGE_CREATE" => Some(ParsedQqInbound {
            scene: QqScene::Channel,
            conversation_id: d.get("channel_id")?.as_str()?.to_string(),
            sender_id: d
                .get("author")
                .and_then(|a| a.get("id"))
                .and_then(Value::as_str)
                .map(str::to_string),
            sender_name: d
                .get("author")
                .and_then(|a| a.get("username"))
                .and_then(Value::as_str)
                .map(str::to_string),
            text: clean_qq_content(d.get("content").and_then(Value::as_str)),
        }),
        "DIRECT_MESSAGE_CREATE" => Some(ParsedQqInbound {
            scene: QqScene::Direct,
            conversation_id: d.get("guild_id")?.as_str()?.to_string(),
            sender_id: d
                .get("author")
                .and_then(|a| a.get("id"))
                .and_then(Value::as_str)
                .map(str::to_string),
            sender_name: d
                .get("author")
                .and_then(|a| a.get("username"))
                .and_then(Value::as_str)
                .map(str::to_string),
            text: clean_qq_content(d.get("content").and_then(Value::as_str)),
        }),
        other if other.contains("GROUP") => Some(ParsedQqInbound {
            scene: QqScene::Group,
            conversation_id: d.get("group_openid")?.as_str()?.to_string(),
            sender_id: d
                .get("author")
                .and_then(|a| a.get("member_openid"))
                .and_then(Value::as_str)
                .map(str::to_string),
            sender_name: None,
            text: clean_qq_content(d.get("content").and_then(Value::as_str)),
        }),
        _ => None,
    }
}

fn clean_qq_content(content: Option<&str>) -> Option<String> {
    let mut text = content?.trim().to_string();
    while let Some(start) = text.find("<@") {
        let Some(end) = text[start..].find('>').map(|i| start + i) else {
            break;
        };
        text.replace_range(start..=end, "");
    }
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_token_validity_uses_refresh_skew() {
        let valid = QqAccessToken {
            value: "t".into(),
            expires_at: Instant::now() + TOKEN_REFRESH_SKEW + Duration::from_secs(1),
        };
        assert!(valid.is_valid());

        let expiring = QqAccessToken {
            value: "t".into(),
            expires_at: Instant::now() + TOKEN_REFRESH_SKEW,
        };
        assert!(!expiring.is_valid());
    }

    #[test]
    fn detects_qq_auth_errors() {
        assert!(is_qq_auth_error(&anyhow!(
            "qqbot groups message 401: token expired"
        )));
        assert!(is_qq_auth_error(&anyhow!("access_token invalid")));
        assert!(!is_qq_auth_error(&anyhow!(
            "qqbot groups message 400: bad request"
        )));
    }

    #[test]
    fn bind_conversation_id_resolves_env() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join(".env");
        std::fs::write(&env_path, "CHAT=group-1\n").unwrap();
        let env = Env::load(&env_path, &dir.path().join("missing"));
        let cfg = QqBotConfig {
            enabled: true,
            app_id: Some("app".into()),
            app_id_env: None,
            app_secret: Some("secret".into()),
            app_secret_env: None,
            sandbox: false,
            rich_text: true,
            bind_conversation_id: Some("$CHAT".into()),
            io: None,
        };
        let adapter = QqBotAdapter::new(&cfg, &env).unwrap();
        assert_eq!(adapter.bind_conversation_id.as_deref(), Some("group-1"));
    }

    #[test]
    fn markdown_body_uses_qq_markdown_content() {
        let body = qq_markdown_body("# hi");
        assert_eq!(body.get("msg_type").and_then(Value::as_i64), Some(2));
        assert_eq!(
            body.pointer("/markdown/content").and_then(Value::as_str),
            Some("# hi")
        );
    }

    #[test]
    fn text_body_uses_plain_content() {
        let body = qq_text_body("hi");
        assert_eq!(body.get("msg_type").and_then(Value::as_i64), Some(0));
        assert_eq!(body.get("content").and_then(Value::as_str), Some("hi"));
    }

    #[test]
    fn parse_group_message_create_preserves_raw_conversation_and_scene() {
        let env = parse_dispatch(
            json!({
                "op": 0,
                "t": "GROUP_MESSAGE_CREATE",
                "d": {
                    "id": "m1",
                    "content": "<@!bot> hello",
                    "group_openid": "group-1",
                    "author": {"member_openid": "member-1", "username": "alice"}
                }
            }),
            &None,
        )
        .unwrap();
        assert_eq!(env.conversation_id.0, "group-1");
        assert_eq!(env.sender_id.as_deref(), Some("member-1"));
        assert_eq!(env.sender_name.as_deref(), Some("alice"));
        assert_eq!(env.text.as_deref(), Some("hello"));
        assert_eq!(
            env.platform_metadata
                .pointer("/onlyne/qq_scene")
                .and_then(Value::as_str),
            Some("group")
        );
    }

    #[test]
    fn parse_c2c_message_create_uses_user_openid() {
        let env = parse_dispatch(
            json!({
                "op": 0,
                "t": "C2C_MESSAGE_CREATE",
                "d": {
                    "id": "m1",
                    "content": "hello",
                    "author": {"user_openid": "user-1", "username": "bob"}
                }
            }),
            &None,
        )
        .unwrap();
        assert_eq!(env.conversation_id.0, "user-1");
        assert_eq!(env.sender_id.as_deref(), Some("user-1"));
        assert_eq!(env.sender_name.as_deref(), Some("bob"));
        assert_eq!(
            env.platform_metadata
                .pointer("/onlyne/qq_scene")
                .and_then(Value::as_str),
            Some("c2c")
        );
    }

    #[tokio::test]
    async fn prefixed_targets_are_explicit() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::load(&dir.path().join("missing"), &dir.path().join("missing2"));
        let cfg = QqBotConfig {
            enabled: true,
            app_id: Some("app".into()),
            app_id_env: None,
            app_secret: Some("secret".into()),
            app_secret_env: None,
            sandbox: false,
            rich_text: true,
            bind_conversation_id: None,
            io: None,
        };
        let group = QqBotAdapter::new(&cfg, &env)
            .unwrap()
            .resolve_target("group:g1")
            .await;
        match group {
            QqTarget::Known { scene, id } => {
                assert_eq!(scene, QqScene::Group);
                assert_eq!(id, "g1");
            }
            _ => panic!("expected known group"),
        }
    }

    #[test]
    fn delivery_metadata_keeps_first_id_and_parts() {
        let (id, meta) = delivery_metadata(vec![
            ("markdown", (MessageId("m1".into()), json!({"ok": 1}))),
            ("attachment", (MessageId("m2".into()), json!({"ok": 2}))),
        ])
        .unwrap();
        assert_eq!(id.0, "m1");
        assert_eq!(
            meta.pointer("/delivery_parts/1/message_id")
                .and_then(Value::as_str),
            Some("m2")
        );
    }
}
