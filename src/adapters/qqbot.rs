use crate::{
    adapters::allowed,
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
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::{
    sync::{Mutex, mpsc},
    task::JoinHandle,
    time::{Duration, sleep},
};
use tokio_tungstenite::{connect_async, tungstenite::Message};
const TOKEN_URL: &str = "https://bots.qq.com/app/getAppAccessToken";
const PROD: &str = "https://api.sgroup.qq.com";
const SANDBOX: &str = "https://sandbox.api.sgroup.qq.com";
const INTENTS: i64 = (1 << 25) | (1 << 26);
pub struct QqBotAdapter {
    app_id: String,
    app_secret: String,
    sandbox: bool,
    rich_text: bool,
    allow_chats: Vec<String>,
    client: Client,
    token: Arc<Mutex<Option<String>>>,
    running: Arc<AtomicBool>,
    task: Option<JoinHandle<()>>,
}
impl QqBotAdapter {
    pub fn new(cfg: &QqBotConfig, env: &Env) -> anyhow::Result<Self> {
        Ok(Self {
            app_id: env.secret(&cfg.app_id_env, &cfg.app_id, "qqbot app_id")?,
            app_secret: env.secret(&cfg.app_secret_env, &cfg.app_secret, "qqbot app_secret")?,
            sandbox: cfg.sandbox,
            rich_text: cfg.rich_text,
            allow_chats: cfg.allow_chats.clone(),
            client: Client::new(),
            token: Arc::new(Mutex::new(None)),
            running: Arc::new(AtomicBool::new(false)),
            task: None,
        })
    }
    fn base(&self) -> &'static str {
        if self.sandbox { SANDBOX } else { PROD }
    }
    async fn access_token(&self) -> anyhow::Result<String> {
        if let Some(t) = self.token.lock().await.clone() {
            return Ok(t);
        }
        let v: Value = self
            .client
            .post(TOKEN_URL)
            .json(&json!({"appId":self.app_id,"clientSecret":self.app_secret}))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let t = v
            .get("access_token")
            .and_then(Value::as_str)
            .context("qqbot access_token")?
            .to_string();
        *self.token.lock().await = Some(t.clone());
        Ok(t)
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
        let allow = self.allow_chats.clone();
        let running = self.running.clone();
        let inbound = ctx.inbound.clone();
        let events = ctx.events.clone();
        self.task = Some(tokio::spawn(async move {
            while running.load(Ordering::SeqCst) {
                if let Err(e) =
                    qq_loop(&app_id, &app_secret, sandbox, &allow, &inbound, &events).await
                {
                    let _ = events
                        .send(Event::AdapterReconnecting {
                            channel_id: ChannelId("qqbot".into()),
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
        let token = self.access_token().await?;
        let mut sent = Vec::new();
        if let Some(text) = msg.text.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            let body = if msg.format == MessageFormat::Markdown {
                if !self.rich_text {
                    return Err(anyhow!("qqbot rich_text disabled"));
                }
                qq_markdown_body(text)
            } else {
                json!({"content":text,"msg_type":0})
            };
            let kind = if msg.format == MessageFormat::Markdown {
                "markdown"
            } else {
                "text"
            };
            let sent_body = self
                .send_group(&token, &msg.conversation_id.0, body)
                .await
                .map_err(|e| {
                    if msg.format == MessageFormat::Markdown {
                        anyhow!("markdown rich send failed: {e}")
                    } else {
                        e
                    }
                })?;
            sent.push((kind, sent_body));
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
fn qq_markdown_body(text: &str) -> Value {
    json!({"msg_type":2,"markdown":{"content":text}})
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

impl QqBotAdapter {
    async fn send_group(
        &self,
        token: &str,
        conv: &str,
        body: Value,
    ) -> anyhow::Result<(MessageId, Value)> {
        match self.send_scoped(token, "groups", conv, body.clone()).await {
            Ok(v) => Ok(v),
            Err(group_err) => self
                .send_scoped(token, "users", conv, body)
                .await
                .with_context(|| format!("qqbot group send failed first: {group_err}")),
        }
    }

    async fn send_scoped(
        &self,
        token: &str,
        scope: &str,
        conv: &str,
        body: Value,
    ) -> anyhow::Result<(MessageId, Value)> {
        let resp = self
            .client
            .post(format!("{}/v2/{scope}/{conv}/messages", self.base()))
            .header("Authorization", format!("QQBot {token}"))
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(anyhow!("qqbot {scope} message {status}: {text}"));
        }
        let v: Value = serde_json::from_str(&text)?;
        let id = v
            .get("id")
            .or_else(|| v.get("msg_id"))
            .and_then(Value::as_str)
            .unwrap_or("qqbot-out")
            .to_string();
        Ok((MessageId(id), v))
    }

    async fn send_attachment(
        &self,
        token: &str,
        conv: &str,
        a: &AttachmentRef,
    ) -> anyhow::Result<(MessageId, Value)> {
        let src = a
            .path
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .or_else(|| a.url.clone())
            .context("attachment needs path or url")?;
        let bytes = media::read_bytes(&src).await?;
        let file_type = match a.kind {
            AttachmentKind::Image => 1,
            AttachmentKind::Video => 2,
            AttachmentKind::File | AttachmentKind::Audio | AttachmentKind::Voice => 4,
        };
        let name = a.file_name.clone().unwrap_or_else(|| "media".into());
        match self
            .send_attachment_scoped(token, "groups", conv, file_type, &name, bytes.clone())
            .await
        {
            Ok(v) => Ok(v),
            Err(group_err) => self
                .send_attachment_scoped(token, "users", conv, file_type, &name, bytes)
                .await
                .with_context(|| format!("qqbot group upload failed first: {group_err}")),
        }
    }

    async fn send_attachment_scoped(
        &self,
        token: &str,
        scope: &str,
        conv: &str,
        file_type: i32,
        name: &str,
        bytes: Vec<u8>,
    ) -> anyhow::Result<(MessageId, Value)> {
        let mut body = json!({
            "file_type": file_type,
            "file_data": BASE64.encode(bytes),
            "srv_send_msg": false,
        });
        if file_type == 4 {
            body["file_name"] = json!(name);
        }
        let resp = self
            .client
            .post(format!("{}/v2/{scope}/{conv}/files", self.base()))
            .header("Authorization", format!("QQBot {token}"))
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(anyhow!("qqbot {scope} upload {status}: {text}"));
        }
        let v: Value = serde_json::from_str(&text)?;
        let info = v
            .get("file_info")
            .and_then(Value::as_str)
            .context("qqbot file_info")?;
        self.send_scoped(
            token,
            scope,
            conv,
            json!({"msg_type":7,"media":{"file_info":info}}),
        )
        .await
    }
}
async fn qq_token(client: &Client, app_id: &str, app_secret: &str) -> anyhow::Result<String> {
    let v: Value = client
        .post(TOKEN_URL)
        .json(&json!({"appId":app_id,"clientSecret":app_secret}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(v.get("access_token")
        .and_then(Value::as_str)
        .context("qqbot token")?
        .to_string())
}
async fn qq_loop(
    app_id: &str,
    app_secret: &str,
    sandbox: bool,
    allow: &[String],
    inbound: &mpsc::Sender<MessageEnvelope>,
    events: &mpsc::Sender<Event>,
) -> anyhow::Result<()> {
    let client = Client::new();
    let token = qq_token(&client, app_id, app_secret).await?;
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
    let mut seq: Option<i64> = None;
    let mut heartbeat = tokio::time::interval(Duration::from_secs(40));
    loop {
        tokio::select! { _=heartbeat.tick()=>{let _=ws.send(Message::Text(json!({"op":1,"d":seq}).to_string())).await;}, item=ws.next()=>{let Some(item)=item else{break};if let Message::Text(t)=item?{let v:Value=serde_json::from_str(&t)?;if let Some(s)=v.get("s").and_then(Value::as_i64){seq=Some(s);}match v.get("op").and_then(Value::as_i64).unwrap_or(-1){10=>{let interval=v.pointer("/d/heartbeat_interval").and_then(Value::as_u64).unwrap_or(41250);heartbeat=tokio::time::interval(Duration::from_millis(interval));ws.send(Message::Text(json!({"op":2,"d":{"token":format!("QQBot {token}"),"intents":INTENTS,"shard":[0,1]}}).to_string())).await?;},0=>{if let Some(env)=parse_dispatch(v,allow){let _=inbound.send(env).await;}},7|9=>return Err(anyhow!("qqbot reconnect requested")),_=>{}}}}}
    }
    let _ = events
        .send(Event::AdapterStopped {
            channel_id: ChannelId("qqbot".into()),
        })
        .await;
    Ok(())
}
fn parse_dispatch(v: Value, allow: &[String]) -> Option<MessageEnvelope> {
    if v.get("op").and_then(Value::as_i64) != Some(0) {
        return None;
    }
    let t = v.get("t").and_then(Value::as_str).unwrap_or("");
    let d = v.get("d")?;
    let (conv, sender) = if t.contains("GROUP") {
        (
            d.get("group_openid")?.as_str()?.to_string(),
            d.get("author")
                .and_then(|a| a.get("member_openid"))
                .and_then(Value::as_str)
                .map(str::to_string),
        )
    } else {
        (
            d.pointer("/author/user_openid")?.as_str()?.to_string(),
            d.pointer("/author/user_openid")
                .and_then(Value::as_str)
                .map(str::to_string),
        )
    };
    if !allowed(allow, &conv) {
        return None;
    }
    Some(MessageEnvelope {
        channel_id: ChannelId("qqbot".into()),
        conversation_id: ConversationId(conv),
        message_id: MessageId(
            d.get("id")
                .or_else(|| d.get("msg_id"))
                .and_then(Value::as_str)
                .unwrap_or("qqbot-in")
                .to_string(),
        ),
        direction: Direction::Inbound,
        sender_id: sender,
        sender_name: None,
        text: d.get("content").and_then(Value::as_str).map(str::to_string),
        format: MessageFormat::Plain,
        attachments: vec![],
        delivery_state: DeliveryState::Delivered,
        timestamp: Utc::now(),
        platform_metadata: v,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn delivery_metadata_keeps_first_id_and_parts() {
        let (id, meta) = delivery_metadata(vec![
            ("markdown", (MessageId("m1".into()), json!({"ok":1}))),
            ("attachment", (MessageId("m2".into()), json!({"ok":2}))),
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
