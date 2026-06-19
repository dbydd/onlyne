use crate::{
    adapters::allowed,
    config::{Env, TelegramConfig},
    core::*,
    media,
};
use anyhow::{Context, anyhow};
use async_trait::async_trait;
use chrono::Utc;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use teloxide::{
    Bot,
    prelude::Requester,
    types::{ChatId, InputFile},
};
use tokio::{
    sync::mpsc,
    task::JoinHandle,
    time::{Duration, sleep},
};

pub struct TelegramAdapter {
    token: String,
    allow_chats: Vec<String>,
    client: Client,
    bot: Bot,
    running: Arc<AtomicBool>,
    task: Option<JoinHandle<()>>,
}
impl TelegramAdapter {
    pub fn new(cfg: &TelegramConfig, env: &Env) -> anyhow::Result<Self> {
        let token = env.secret(&cfg.token_env, &cfg.token, "telegram token")?;
        Ok(Self {
            bot: Bot::new(token.clone()),
            token,
            allow_chats: cfg.allow_chats.clone(),
            client: Client::new(),
            running: Arc::new(AtomicBool::new(false)),
            task: None,
        })
    }
}

#[derive(Deserialize)]
struct TgResp<T> {
    ok: bool,
    result: T,
    description: Option<String>,
}
#[derive(Deserialize, Clone)]
struct TgUpdate {
    update_id: i64,
    message: Option<TgMessage>,
}
#[derive(Deserialize, Clone)]
struct TgMessage {
    message_id: i64,
    text: Option<String>,
    chat: TgChat,
    from: Option<TgUser>,
    photo: Option<Vec<TgPhoto>>,
    document: Option<TgFile>,
    audio: Option<TgFile>,
    voice: Option<TgFile>,
    video: Option<TgFile>,
}
#[derive(Deserialize, Clone)]
struct TgChat {
    id: i64,
    title: Option<String>,
    username: Option<String>,
}
#[derive(Deserialize, Clone)]
struct TgUser {
    id: i64,
    username: Option<String>,
    first_name: Option<String>,
}
#[derive(Deserialize, Clone)]
struct TgPhoto {
    file_id: String,
    file_size: Option<u64>,
}
#[derive(Deserialize, Clone)]
struct TgFile {
    file_id: String,
    file_name: Option<String>,
    mime_type: Option<String>,
    file_size: Option<u64>,
}

#[async_trait]
impl Adapter for TelegramAdapter {
    fn channel_id(&self) -> ChannelId {
        ChannelId("telegram".into())
    }
    async fn start(&mut self, ctx: AdapterContext) -> anyhow::Result<()> {
        self.check().await?;
        self.running.store(true, Ordering::SeqCst);
        let token = self.token.clone();
        let client = self.client.clone();
        let running = self.running.clone();
        let allow = self.allow_chats.clone();
        let inbound = ctx.inbound.clone();
        let events = ctx.events.clone();
        let media_dir = ctx.media_dir.clone();
        self.task = Some(tokio::spawn(async move {
            let mut offset = 0_i64;
            while running.load(Ordering::SeqCst) {
                let url = format!("https://api.telegram.org/bot{token}/getUpdates");
                let res = client
                    .get(url)
                    .query(&[("timeout", "30"), ("offset", &offset.to_string())])
                    .send()
                    .await;
                match res {
                    Ok(r) => match r.json::<TgResp<Vec<TgUpdate>>>().await {
                        Ok(body) if body.ok => {
                            for upd in body.result {
                                offset = upd.update_id + 1;
                                if let Some(msg) = upd.message {
                                    handle_msg(
                                        &client, &token, &allow, &media_dir, &inbound, &events, msg,
                                    )
                                    .await;
                                }
                            }
                        }
                        Ok(body) => {
                            let _ = events
                                .send(Event::AdapterFailed {
                                    channel_id: ChannelId("telegram".into()),
                                    error: body
                                        .description
                                        .unwrap_or_else(|| "telegram getUpdates failed".into()),
                                })
                                .await;
                            sleep(Duration::from_secs(5)).await;
                        }
                        Err(e) => {
                            let _ = events
                                .send(Event::AdapterFailed {
                                    channel_id: ChannelId("telegram".into()),
                                    error: e.to_string(),
                                })
                                .await;
                            sleep(Duration::from_secs(5)).await;
                        }
                    },
                    Err(e) => {
                        let _ = events
                            .send(Event::AdapterReconnecting {
                                channel_id: ChannelId("telegram".into()),
                                reason: e.to_string(),
                            })
                            .await;
                        sleep(Duration::from_secs(5)).await;
                    }
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
        let chat = telegram_chat_id(&msg.conversation_id.0)?;
        let mut sent = None;
        if let Some(text) = msg.text.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            let m = self.bot.send_message(chat, text.to_string()).await?;
            sent = Some((
                MessageId(m.id.0.to_string()),
                serde_json::to_value(&m).unwrap_or(Value::Null),
            ));
        }
        for a in &msg.attachments {
            sent = Some(self.send_attachment(chat, a).await?);
        }
        let (message_id, platform_metadata) =
            sent.ok_or_else(|| anyhow!("telegram send_message needs text or attachments"))?;
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
        self.bot.get_me().await.map(|_| ()).map_err(Into::into)
    }
}
impl TelegramAdapter {
    async fn send_attachment(
        &self,
        chat_id: ChatId,
        a: &AttachmentRef,
    ) -> anyhow::Result<(MessageId, Value)> {
        let src = a
            .path
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .or_else(|| a.url.clone())
            .ok_or_else(|| anyhow!("attachment needs path or url"))?;
        let bytes = media::read_bytes(&src).await?;
        let name = a.file_name.clone().unwrap_or_else(|| "media".into());
        let file = InputFile::memory(bytes).file_name(name);
        let m = match a.kind {
            AttachmentKind::Image => self.bot.send_photo(chat_id, file).await?,
            AttachmentKind::Audio => self.bot.send_audio(chat_id, file).await?,
            AttachmentKind::Voice => self.bot.send_voice(chat_id, file).await?,
            AttachmentKind::Video => self.bot.send_video(chat_id, file).await?,
            AttachmentKind::File => self.bot.send_document(chat_id, file).await?,
        };
        Ok((
            MessageId(m.id.0.to_string()),
            serde_json::to_value(&m).unwrap_or(Value::Null),
        ))
    }
}
fn telegram_chat_id(s: &str) -> anyhow::Result<ChatId> {
    s.parse::<i64>()
        .map(ChatId)
        .with_context(|| format!("telegram chat id must be an integer: {s}"))
}

async fn handle_msg(
    client: &Client,
    token: &str,
    allow: &[String],
    media_root: &std::path::Path,
    inbound: &mpsc::Sender<MessageEnvelope>,
    events: &mpsc::Sender<Event>,
    msg: TgMessage,
) {
    let chat = msg.chat.id.to_string();
    if !allowed(allow, &chat) {
        let _ = events
            .send(Event::Warning {
                channel_id: Some(ChannelId("telegram".into())),
                message: format!("rejected telegram chat {chat}; add to allow_chats"),
            })
            .await;
        return;
    }
    let mut attachments = vec![];
    for (kind, f) in tg_files(&msg) {
        if let Ok(path) = download_file(
            client,
            token,
            media_root,
            &f.file_id,
            f.file_name.as_deref().unwrap_or("telegram-media"),
        )
        .await
        {
            attachments.push(AttachmentRef {
                kind,
                path: Some(path),
                url: None,
                file_name: f.file_name,
                mime_type: f.mime_type,
                size: f.file_size,
            });
        }
    }
    let env = MessageEnvelope {
        channel_id: ChannelId("telegram".into()),
        conversation_id: ConversationId(chat),
        message_id: MessageId(msg.message_id.to_string()),
        direction: Direction::Inbound,
        sender_id: msg.from.as_ref().map(|u| u.id.to_string()),
        sender_name: msg
            .from
            .as_ref()
            .and_then(|u| u.username.clone().or(u.first_name.clone())),
        text: msg.text,
        format: MessageFormat::Plain,
        attachments,
        delivery_state: DeliveryState::Delivered,
        timestamp: Utc::now(),
        platform_metadata: json!({"chat_title": msg.chat.title, "chat_username": msg.chat.username}),
    };
    let _ = inbound.send(env).await;
}
fn tg_files(msg: &TgMessage) -> Vec<(AttachmentKind, TgFile)> {
    let mut out = vec![];
    if let Some(p) = msg.photo.as_ref().and_then(|v| v.last()) {
        out.push((
            AttachmentKind::Image,
            TgFile {
                file_id: p.file_id.clone(),
                file_name: Some("photo.jpg".into()),
                mime_type: Some("image/jpeg".into()),
                file_size: p.file_size,
            },
        ));
    }
    if let Some(f) = &msg.document {
        out.push((AttachmentKind::File, f.clone()));
    }
    if let Some(f) = &msg.audio {
        out.push((AttachmentKind::Audio, f.clone()));
    }
    if let Some(f) = &msg.voice {
        out.push((AttachmentKind::Voice, f.clone()));
    }
    if let Some(f) = &msg.video {
        out.push((AttachmentKind::Video, f.clone()));
    }
    out
}
async fn download_file(
    client: &Client,
    token: &str,
    root: &std::path::Path,
    file_id: &str,
    name: &str,
) -> anyhow::Result<std::path::PathBuf> {
    let meta: Value = client
        .get(format!("https://api.telegram.org/bot{token}/getFile"))
        .query(&[("file_id", file_id)])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let file_path = meta
        .pointer("/result/file_path")
        .and_then(Value::as_str)
        .context("telegram file_path")?;
    let bytes = client
        .get(format!(
            "https://api.telegram.org/file/bot{token}/{file_path}"
        ))
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    media::cache_bytes(root, "telegram", name, &bytes).await
}
