use crate::{
    adapters::allowed,
    config::{Env, FeishuConfig},
    core::*,
    markdown, media,
};
use anyhow::{Context, anyhow};
use async_trait::async_trait;
use chrono::Utc;
#[allow(deprecated)]
use open_lark::{
    Config as LarkConfig,
    ws_client::{EventDispatcherHandler, LarkWsClient},
};
use reqwest::{Client, multipart};
use serde::Deserialize;
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

pub struct FeishuAdapter {
    app_id: String,
    app_secret: String,
    domain: String,
    allow_chats: Vec<String>,
    rich_text: bool,
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
            rich_text: cfg.rich_text,
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
                let reason =
                    match feishu_ws_loop(&domain, &app_id, &app_secret, &allow, &inbound).await {
                        Ok(()) => "websocket closed".to_string(),
                        Err(e) => e.to_string(),
                    };
                if running.load(Ordering::SeqCst) {
                    let _ = events
                        .send(Event::AdapterReconnecting {
                            channel_id: ChannelId("feishu".into()),
                            reason,
                        })
                        .await;
                    sleep(Duration::from_secs(5)).await;
                }
            }
            let _ = events
                .send(Event::AdapterStopped {
                    channel_id: ChannelId("feishu".into()),
                })
                .await;
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
        let mut sent = Vec::new();
        if let Some(text) = msg.text.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            let part = if msg.format == MessageFormat::Markdown {
                if !self.rich_text {
                    return Err(anyhow!("feishu rich_text disabled"));
                }
                let check = markdown::check(text);
                if let Some(reason) = check.unsupported_reason {
                    return Err(anyhow!("unsupported markdown for feishu card: {reason}"));
                }
                let card = feishu_markdown_card(text);
                (
                    "markdown_card",
                    self.send_content(&token, &msg.conversation_id.0, "interactive", card)
                        .await
                        .map_err(|e| anyhow!("markdown rich send failed: {e}"))?,
                )
            } else {
                (
                    "text",
                    self.send_content(&token, &msg.conversation_id.0, "text", json!({"text":text}))
                        .await?,
                )
            };
            sent.push(part);
        }
        for a in &msg.attachments {
            sent.push((
                "attachment",
                self.send_attachment(&token, &msg.conversation_id.0, a)
                    .await?,
            ));
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
        self.token().await.map(|_| ())
    }
}
fn feishu_markdown_card(markdown: &str) -> Value {
    json!({
        "config": {"wide_screen_mode": true},
        "elements": [{"tag": "markdown", "content": markdown}]
    })
}

fn delivery_metadata(sent: Vec<(&str, (MessageId, Value))>) -> anyhow::Result<(MessageId, Value)> {
    let Some((_, (first_id, _))) = sent.first() else {
        return Err(anyhow!("feishu send_message needs text or attachments"));
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

impl FeishuAdapter {
    async fn send_content(
        &self,
        token: &str,
        chat: &str,
        msg_type: &str,
        content: Value,
    ) -> anyhow::Result<(MessageId, Value)> {
        let body = json!({"receive_id":chat,"msg_type":msg_type,"content":content.to_string()});
        let receive_id_type = if chat.starts_with("ou_") {
            "open_id"
        } else {
            "chat_id"
        };
        let v: Value = self
            .client
            .post(format!(
                "{}/open-apis/im/v1/messages?receive_id_type={receive_id_type}",
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
) -> anyhow::Result<()> {
    let (payload_tx, mut payload_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let handler = EventDispatcherHandler::builder()
        .payload_sender(payload_tx)
        .build();
    #[allow(deprecated)]
    let cfg = Arc::new(
        LarkConfig::builder()
            .app_id(app_id.to_string())
            .app_secret(app_secret.to_string())
            .base_url(domain.to_string())
            .timeout(Duration::from_secs(30))
            .max_response_size(100 * 1024 * 1024)
            .build()
            .map_err(|e| anyhow!(e.to_string()))?,
    );

    let mut ws = tokio::spawn(async move { LarkWsClient::open(cfg, handler).await });
    loop {
        tokio::select! {
            result = &mut ws => {
                return match result {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(e)) => Err(anyhow!(e.to_string())),
                    Err(e) => Err(anyhow!(e)),
                };
            }
            payload = payload_rx.recv() => {
                let Some(payload) = payload else { return Ok(()); };
                if let Some(env) = parse_feishu_event_payload(&payload, allow) {
                    let _ = inbound.send(env).await;
                }
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct FeishuEnvelope {
    header: FeishuHeader,
    event: FeishuEvent,
}
#[derive(Debug, Deserialize)]
struct FeishuHeader {
    event_id: Option<String>,
    event_type: String,
}
#[derive(Debug, Deserialize)]
struct FeishuEvent {
    sender: Option<FeishuSender>,
    message: FeishuMessage,
    chat: Option<FeishuChat>,
}
#[derive(Debug, Deserialize)]
struct FeishuSender {
    sender_id: FeishuSenderId,
}
#[derive(Debug, Deserialize)]
struct FeishuSenderId {
    open_id: Option<String>,
}
#[derive(Debug, Deserialize)]
struct FeishuMessage {
    message_id: Option<String>,
    content: Option<String>,
    chat_type: Option<String>,
    chat_id: Option<String>,
}
#[derive(Debug, Deserialize)]
struct FeishuChat {
    chat_id: Option<String>,
}

fn parse_feishu_event_payload(payload: &[u8], allow: &[String]) -> Option<MessageEnvelope> {
    let envelope: FeishuEnvelope = serde_json::from_slice(payload).ok()?;
    if envelope.header.event_type != "im.message.receive_v1" {
        return None;
    }
    let chat_type = envelope
        .event
        .message
        .chat_type
        .as_deref()
        .unwrap_or_default();
    let conversation_id = if chat_type == "p2p" {
        envelope
            .event
            .sender
            .as_ref()
            .and_then(|s| s.sender_id.open_id.clone())
            .or_else(|| envelope.event.message.chat_id.clone())?
    } else {
        envelope
            .event
            .chat
            .as_ref()
            .and_then(|c| c.chat_id.clone())
            .or_else(|| envelope.event.message.chat_id.clone())?
    };
    if !allowed(allow, &conversation_id) {
        return None;
    }
    let content = envelope.event.message.content.unwrap_or_default();
    let text = serde_json::from_str::<Value>(&content)
        .ok()
        .and_then(|c| c.get("text").and_then(Value::as_str).map(str::to_string))
        .or_else(|| (!content.is_empty()).then_some(content));
    Some(MessageEnvelope {
        channel_id: ChannelId("feishu".into()),
        conversation_id: ConversationId(conversation_id),
        message_id: MessageId(
            envelope
                .event
                .message
                .message_id
                .or(envelope.header.event_id)
                .unwrap_or_else(|| "feishu-in".into()),
        ),
        direction: Direction::Inbound,
        sender_id: envelope.event.sender.and_then(|s| s.sender_id.open_id),
        sender_name: None,
        text,
        format: MessageFormat::Plain,
        attachments: vec![],
        delivery_state: DeliveryState::Delivered,
        timestamp: Utc::now(),
        platform_metadata: serde_json::from_slice(payload).unwrap_or(Value::Null),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_p2p_event_to_open_id_conversation() {
        let payload = br#"{
            "header":{"event_id":"evt-1","event_type":"im.message.receive_v1"},
            "event":{
                "sender":{"sender_id":{"open_id":"ou_user"}},
                "message":{"message_id":"om_1","chat_type":"p2p","chat_id":"oc_hidden","content":"{\"text\":\"hi\"}"}
            }
        }"#;

        let msg = parse_feishu_event_payload(payload, &[]).unwrap();

        assert_eq!(msg.conversation_id.0, "ou_user");
        assert_eq!(msg.sender_id.as_deref(), Some("ou_user"));
        assert_eq!(msg.text.as_deref(), Some("hi"));
    }

    #[test]
    fn parses_group_event_to_chat_id_conversation() {
        let payload = br#"{
            "header":{"event_id":"evt-2","event_type":"im.message.receive_v1"},
            "event":{
                "sender":{"sender_id":{"open_id":"ou_user"}},
                "chat":{"chat_id":"oc_group"},
                "message":{"message_id":"om_2","chat_type":"group","chat_id":"oc_group","content":"{\"text\":\"hello group\"}"}
            }
        }"#;

        let msg = parse_feishu_event_payload(payload, &[]).unwrap();

        assert_eq!(msg.conversation_id.0, "oc_group");
        assert_eq!(msg.sender_id.as_deref(), Some("ou_user"));
        assert_eq!(msg.text.as_deref(), Some("hello group"));
    }

    #[test]
    fn markdown_card_uses_interactive_markdown_element() {
        let card = feishu_markdown_card("# hi");
        assert_eq!(
            card.pointer("/elements/0/tag").and_then(Value::as_str),
            Some("markdown")
        );
        assert_eq!(
            card.pointer("/elements/0/content").and_then(Value::as_str),
            Some("# hi")
        );
    }

    #[test]
    fn allow_list_filters_resolved_conversation() {
        let payload = br#"{
            "header":{"event_id":"evt-3","event_type":"im.message.receive_v1"},
            "event":{
                "sender":{"sender_id":{"open_id":"ou_user"}},
                "message":{"message_id":"om_3","chat_type":"p2p","chat_id":"oc_hidden","content":"{\"text\":\"hi\"}"}
            }
        }"#;

        assert!(parse_feishu_event_payload(payload, &["other".into()]).is_none());
        assert!(parse_feishu_event_payload(payload, &["ou_user".into()]).is_some());
    }
}
