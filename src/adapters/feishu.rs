use crate::{
    adapters::allowed,
    config::{Env, FeishuConfig},
    core::*,
    media,
};
use anyhow::{Context, anyhow};
use async_trait::async_trait;
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use reqwest::{Client, multipart};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::{
    sync::mpsc,
    task::JoinHandle,
    time::{Duration, sleep},
};
use tokio_tungstenite::{connect_async, tungstenite::Message};

pub struct FeishuAdapter {
    app_id: String,
    app_secret: String,
    domain: String,
    allow_chats: Vec<String>,
    client: Client,
    running: Arc<AtomicBool>,
    task: Option<JoinHandle<()>>,
}
impl FeishuAdapter {
    pub fn new(cfg: &FeishuConfig, env: &Env) -> anyhow::Result<Self> {
        Ok(Self {
            app_id: env.secret(&cfg.app_id_env, &cfg.app_id, "feishu app_id")?,
            app_secret: env.secret(&cfg.app_secret_env, &cfg.app_secret, "feishu app_secret")?,
            domain: cfg
                .domain
                .clone()
                .unwrap_or_else(|| "https://open.feishu.cn".into())
                .trim_end_matches('/')
                .into(),
            allow_chats: cfg.allow_chats.clone(),
            client: Client::new(),
            running: Arc::new(AtomicBool::new(false)),
            task: None,
        })
    }
    async fn token(&self) -> anyhow::Result<String> {
        let v: Value = self
            .client
            .post(format!(
                "{}/open-apis/auth/v3/tenant_access_token/internal",
                self.domain
            ))
            .json(&json!({"app_id": self.app_id, "app_secret": self.app_secret}))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        v.get("tenant_access_token")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("feishu token failed: {v}"))
    }
}
#[async_trait]
impl Adapter for FeishuAdapter {
    fn channel_id(&self) -> ChannelId {
        ChannelId("feishu".into())
    }
    async fn start(&mut self, ctx: AdapterContext) -> anyhow::Result<()> {
        self.check().await?;
        self.running.store(true, Ordering::SeqCst);
        let domain = self.domain.clone();
        let app_id = self.app_id.clone();
        let app_secret = self.app_secret.clone();
        let allow = self.allow_chats.clone();
        let running = self.running.clone();
        let inbound = ctx.inbound.clone();
        let events = ctx.events.clone();
        self.task = Some(tokio::spawn(async move {
            while running.load(Ordering::SeqCst) {
                if let Err(e) =
                    feishu_ws_loop(&domain, &app_id, &app_secret, &allow, &inbound, &events).await
                {
                    let _ = events
                        .send(Event::AdapterReconnecting {
                            channel_id: ChannelId("feishu".into()),
                            reason: e.to_string(),
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
        let token = self.token().await?;
        let mut sent = None;
        for a in &msg.attachments {
            sent = Some(
                self.send_attachment(&token, &msg.conversation_id.0, a)
                    .await?,
            );
        }
        if let Some(text) = msg.text.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            sent = Some(
                self.send_content(&token, &msg.conversation_id.0, "text", json!({"text":text}))
                    .await?,
            );
        }
        let (message_id, platform_metadata) =
            sent.ok_or_else(|| anyhow!("feishu send_message needs text or attachments"))?;
        Ok(MessageEnvelope {
            channel_id: self.channel_id(),
            conversation_id: msg.conversation_id,
            message_id,
            direction: Direction::Outbound,
            sender_id: None,
            sender_name: None,
            text: msg.text,
            attachments: msg.attachments,
            delivery_state: DeliveryState::Sent,
            timestamp: Utc::now(),
            platform_metadata,
        })
    }
    async fn check(&self) -> anyhow::Result<()> {
        self.token().await.map(|_| ())
    }
}
impl FeishuAdapter {
    async fn send_content(
        &self,
        token: &str,
        chat: &str,
        msg_type: &str,
        content: Value,
    ) -> anyhow::Result<(MessageId, Value)> {
        let body = json!({"receive_id":chat,"msg_type":msg_type,"content":content.to_string()});
        let v: Value = self
            .client
            .post(format!(
                "{}/open-apis/im/v1/messages?receive_id_type=chat_id",
                self.domain
            ))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if v.get("code").and_then(Value::as_i64).unwrap_or(0) != 0 {
            return Err(anyhow!("feishu send failed: {v}"));
        }
        let id = v
            .pointer("/data/message_id")
            .and_then(Value::as_str)
            .unwrap_or("feishu-out")
            .to_string();
        Ok((MessageId(id), v))
    }

    async fn send_attachment(
        &self,
        token: &str,
        chat: &str,
        a: &AttachmentRef,
    ) -> anyhow::Result<(MessageId, Value)> {
        let src = a
            .path
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .or_else(|| a.url.clone())
            .context("attachment needs path or url")?;
        let bytes = media::read_bytes(&src).await?;
        let name = a.file_name.clone().unwrap_or_else(|| "media".into());
        let (upload, msg_type, content_key) = match a.kind {
            AttachmentKind::Image => ("image", "image", "image_key"),
            AttachmentKind::Audio | AttachmentKind::Voice => ("file", "audio", "file_key"),
            AttachmentKind::Video => ("file", "media", "file_key"),
            AttachmentKind::File => ("file", "file", "file_key"),
        };
        let url = if upload == "image" {
            format!("{}/open-apis/im/v1/images", self.domain)
        } else {
            format!("{}/open-apis/im/v1/files", self.domain)
        };
        let form = multipart::Form::new()
            .text("image_type", "message".to_string())
            .text("file_type", msg_type.to_string())
            .part(
                "image",
                multipart::Part::bytes(bytes.clone()).file_name(name.clone()),
            )
            .part("file", multipart::Part::bytes(bytes).file_name(name));
        let v: Value = self
            .client
            .post(url)
            .bearer_auth(token)
            .multipart(form)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let key = v
            .pointer(&format!("/data/{content_key}"))
            .and_then(Value::as_str)
            .context("feishu upload key")?;
        self.send_content(token, chat, msg_type, json!({content_key:key}))
            .await
    }
}
async fn feishu_ws_loop(
    domain: &str,
    app_id: &str,
    app_secret: &str,
    allow: &[String],
    inbound: &mpsc::Sender<MessageEnvelope>,
    events: &mpsc::Sender<Event>,
) -> anyhow::Result<()> {
    let ws_base = domain
        .replace("https://", "wss://")
        .replace("http://", "ws://");
    let url = format!("{ws_base}/open-apis/ws/v1?app_id={app_id}&app_secret={app_secret}");
    let (mut ws, _) = connect_async(url)
        .await
        .context("feishu websocket connect")?;
    while let Some(msg) = ws.next().await {
        match msg? {
            Message::Text(t) => {
                if let Ok(v) = serde_json::from_str::<Value>(&t) {
                    if v.get("type").and_then(Value::as_str) == Some("event_callback")
                        || v.pointer("/header/event_type").is_some()
                    {
                        if let Some(env) = parse_feishu_event(v, allow) {
                            let _ = inbound.send(env).await;
                        }
                    } else if v.get("type").and_then(Value::as_str) == Some("url_verification") {
                        let _ = ws
                            .send(Message::Text(
                                json!({"challenge":v.get("challenge")}).to_string(),
                            ))
                            .await;
                    }
                }
            }
            Message::Ping(p) => {
                let _ = ws.send(Message::Pong(p)).await;
            }
            _ => {}
        }
    }
    let _ = events
        .send(Event::AdapterStopped {
            channel_id: ChannelId("feishu".into()),
        })
        .await;
    Ok(())
}
fn parse_feishu_event(v: Value, allow: &[String]) -> Option<MessageEnvelope> {
    let ev = v.get("event").or_else(|| v.pointer("/schema/event"))?;
    let msg = ev.get("message").unwrap_or(ev);
    let chat = msg.get("chat_id").and_then(Value::as_str)?.to_string();
    if !allowed(allow, &chat) {
        return None;
    }
    let mid = msg
        .get("message_id")
        .and_then(Value::as_str)
        .unwrap_or("feishu-in")
        .to_string();
    let content = msg.get("content").and_then(Value::as_str).unwrap_or("");
    let text = serde_json::from_str::<Value>(content)
        .ok()
        .and_then(|c| c.get("text").and_then(Value::as_str).map(str::to_string))
        .or_else(|| Some(content.to_string()));
    Some(MessageEnvelope {
        channel_id: ChannelId("feishu".into()),
        conversation_id: ConversationId(chat),
        message_id: MessageId(mid),
        direction: Direction::Inbound,
        sender_id: ev
            .pointer("/sender/sender_id/open_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        sender_name: None,
        text,
        attachments: vec![],
        delivery_state: DeliveryState::Delivered,
        timestamp: Utc::now(),
        platform_metadata: v,
    })
}
