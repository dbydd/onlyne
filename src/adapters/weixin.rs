use crate::{
    adapters::allowed,
    config::{Env, WeixinConfig},
    core::*,
};
use anyhow::{Context, anyhow};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::{
    sync::Mutex,
    task::JoinHandle,
    time::{Duration, sleep},
};
use wechat_ilink::{
    Credentials, IncomingMessage, SendContent, WechatContext, WechatEvent, WechatIlinkClient,
};

const DEFAULT_BASE: &str = "https://ilinkai.weixin.qq.com";
const VERSION: &str = "onlyne-weixin/1.0";

pub struct WeixinAdapter {
    token: String,
    base: String,
    allow_chats: Vec<String>,
    client: Arc<WechatIlinkClient>,
    running: Arc<AtomicBool>,
    contexts: Arc<Mutex<std::collections::HashMap<String, WechatContext>>>,
    task: Option<JoinHandle<()>>,
}

impl WeixinAdapter {
    pub fn new(
        cfg: &WeixinConfig,
        env: &Env,
        _ws: &crate::workspace::Workspace,
    ) -> anyhow::Result<Self> {
        let token = env.secret(&cfg.token_env, &cfg.token, "weixin token")?;
        let base = cfg
            .base_url
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_BASE.into())
            .trim_end_matches('/')
            .to_string();
        let client = Arc::new(
            WechatIlinkClient::builder()
                .base_url(base.clone())
                .bot_agent(VERSION)
                .credentials(Credentials {
                    token: token.clone(),
                    base_url: base.clone(),
                    account_id: String::new(),
                    user_id: String::new(),
                    saved_at: None,
                })
                .build(),
        );
        Ok(Self {
            token,
            base,
            allow_chats: cfg.allow_chats.clone(),
            client,
            running: Arc::new(AtomicBool::new(false)),
            contexts: Arc::new(Mutex::new(std::collections::HashMap::new())),
            task: None,
        })
    }
}

#[async_trait]
impl Adapter for WeixinAdapter {
    fn channel_id(&self) -> ChannelId {
        ChannelId("weixin".into())
    }

    async fn start(&mut self, ctx: AdapterContext) -> anyhow::Result<()> {
        self.check().await?;
        self.running.store(true, Ordering::SeqCst);
        let client = self.client.clone();
        let running = self.running.clone();
        let allow = self.allow_chats.clone();
        let inbound = ctx.inbound.clone();
        let events = ctx.events.clone();
        let media_dir = ctx.media_dir.clone();
        let contexts = self.contexts.clone();
        self.task = Some(tokio::spawn(async move {
            while running.load(Ordering::SeqCst) {
                let mut stream = client.clone().events_from_cursor(None);
                loop {
                    if !running.load(Ordering::SeqCst) {
                        client.stop().await;
                        let _ = events
                            .send(Event::AdapterStopped {
                                channel_id: ChannelId("weixin".into()),
                            })
                            .await;
                        return;
                    }
                    match stream.next().await {
                        Some(Ok(WechatEvent::ContextObserved(c))) => {
                            contexts.lock().await.insert(c.user_id.clone(), c);
                        }
                        Some(Ok(WechatEvent::Message(msg))) => {
                            if let Some(env) =
                                weixin_msg_to_envelope(&client, &allow, &media_dir, &contexts, msg)
                                    .await
                            {
                                let _ = inbound.send(env).await;
                            }
                        }
                        Some(Ok(WechatEvent::AuthSessionExpired { account_key })) => {
                            let _ = events
                                .send(Event::AdapterFailed {
                                    channel_id: ChannelId("weixin".into()),
                                    error: format!("weixin auth expired: {account_key}"),
                                })
                                .await;
                            return;
                        }
                        Some(Ok(WechatEvent::CursorAdvanced { .. })) => {}
                        Some(Ok(WechatEvent::UserInteractionRequested { reason, .. })) => {
                            let _ = events
                                .send(Event::Warning {
                                    channel_id: Some(ChannelId("weixin".into())),
                                    message: format!(
                                        "weixin user interaction suggested: {reason:?}"
                                    ),
                                })
                                .await;
                        }
                        Some(Err(e)) => {
                            let _ = events
                                .send(Event::AdapterReconnecting {
                                    channel_id: ChannelId("weixin".into()),
                                    reason: e.to_string(),
                                })
                                .await;
                            break;
                        }
                        None => break,
                    }
                }
                sleep(Duration::from_secs(5)).await;
            }
        }));
        Ok(())
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        self.running.store(false, Ordering::SeqCst);
        self.client.stop().await;
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
        let context = self
            .contexts
            .lock()
            .await
            .get(&msg.conversation_id.0)
            .cloned()
            .ok_or_else(|| {
                anyhow!(
                    "weixin context_token missing for {}; receive a message from this peer first",
                    msg.conversation_id.0
                )
            })?;
        let mut receipt_ids = Vec::new();
        for attachment in &msg.attachments {
            let content = send_content_from_attachment(attachment).await?;
            let receipt = self
                .client
                .send_media_with_context(&context, content)
                .await?;
            receipt_ids.extend(receipt.message_ids);
        }
        if let Some(text) = msg.text.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            let receipt = self.client.send_text_with_context(&context, text).await?;
            receipt_ids.extend(receipt.message_ids);
        }
        if receipt_ids.is_empty() {
            return Err(anyhow!("weixin send_message needs text or attachments"));
        }
        Ok(MessageEnvelope {
            channel_id: self.channel_id(),
            conversation_id: msg.conversation_id,
            message_id: MessageId(
                receipt_ids
                    .last()
                    .cloned()
                    .unwrap_or_else(|| now_id("weixin").0),
            ),
            direction: Direction::Outbound,
            sender_id: None,
            sender_name: None,
            text: msg.text,
            attachments: msg.attachments,
            delivery_state: DeliveryState::Sent,
            timestamp: Utc::now(),
            platform_metadata: json!({"message_ids": receipt_ids}),
        })
    }

    async fn check(&self) -> anyhow::Result<()> {
        self.client
            .set_credentials(Credentials {
                token: self.token.clone(),
                base_url: self.base.clone(),
                account_id: String::new(),
                user_id: String::new(),
                saved_at: None,
            })
            .await;
        let v: Value = reqwest::Client::new()
            .post(format!("{}/ilink/bot/getupdates", self.base))
            .bearer_auth(&self.token)
            .header("AuthorizationType", "ilink_bot_token")
            .header("X-WECHAT-UIN", "MDAwMA==")
            .json(&json!({"get_updates_buf":"","base_info":{"channel_version":VERSION}}))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if v.get("ret").and_then(Value::as_i64).unwrap_or(0) == 0 {
            Ok(())
        } else {
            Err(anyhow!("weixin check failed: {v}"))
        }
    }
}

async fn send_content_from_attachment(a: &AttachmentRef) -> anyhow::Result<SendContent> {
    let src = a
        .path
        .as_ref()
        .map(|p| p.to_string_lossy().to_string())
        .or_else(|| a.url.clone())
        .context("attachment needs path or url")?;
    let data = crate::media::read_bytes(&src).await?;
    let file_name = a.file_name.clone().unwrap_or_else(|| "file.bin".into());
    Ok(match a.kind {
        AttachmentKind::Image => SendContent::Image {
            data,
            caption: None,
        },
        AttachmentKind::Video => SendContent::Video {
            data,
            caption: None,
        },
        AttachmentKind::File | AttachmentKind::Audio | AttachmentKind::Voice => SendContent::File {
            data,
            file_name,
            caption: None,
        },
    })
}

async fn weixin_msg_to_envelope(
    client: &Arc<WechatIlinkClient>,
    allow: &[String],
    media_root: &std::path::Path,
    contexts: &Arc<Mutex<std::collections::HashMap<String, WechatContext>>>,
    msg: IncomingMessage,
) -> Option<MessageEnvelope> {
    if !allowed(allow, &msg.user_id) {
        return None;
    }
    if let Some(c) = msg.context.clone() {
        contexts.lock().await.insert(c.user_id.clone(), c);
    }
    let mut attachments = Vec::new();
    if let Ok(Some(download)) = client.download(&msg).await
        && let Ok(path) = crate::media::cache_bytes(
            media_root,
            "weixin",
            download.file_name.as_deref().unwrap_or("weixin-media"),
            &download.data,
        )
        .await
    {
        attachments.push(AttachmentRef {
            kind: match download.media_type.as_str() {
                "image" => AttachmentKind::Image,
                "video" => AttachmentKind::Video,
                "voice" => AttachmentKind::Voice,
                _ => AttachmentKind::File,
            },
            path: Some(path),
            url: None,
            file_name: download.file_name,
            mime_type: None,
            size: Some(download.data.len() as u64),
        });
    }
    let timestamp: DateTime<Utc> = msg.timestamp.into();
    Some(MessageEnvelope {
        channel_id: ChannelId("weixin".into()),
        conversation_id: ConversationId(msg.user_id.clone()),
        message_id: MessageId(msg.message_id.clone().unwrap_or_else(|| now_id("weixin").0)),
        direction: Direction::Inbound,
        sender_id: Some(msg.user_id),
        sender_name: None,
        text: (!msg.text.is_empty()).then_some(msg.text),
        attachments,
        delivery_state: DeliveryState::Delivered,
        timestamp,
        platform_metadata: serde_json::to_value(&msg.raw).unwrap_or(Value::Null),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weixin_attachment_kind_maps_to_sdk_content() {
        assert!(matches!(AttachmentKind::Image, AttachmentKind::Image));
    }
}
