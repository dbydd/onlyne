use crate::{
    adapters,
    config::{self, Config, Env},
    core::*,
    events::EventBus,
    ipc::Request,
    markdown, media,
    store::Store,
    workspace::Workspace,
};
use anyhow::{Context, anyhow};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{Mutex, mpsc};

pub struct App {
    pub workspace: Workspace,
    pub config: Config,
    pub events: EventBus,
    pub store: Store,
    adapters: Mutex<HashMap<String, Box<dyn Adapter>>>,
    debug_reply: bool,
}
impl App {
    pub async fn load(workspace: Workspace) -> anyhow::Result<Arc<Self>> {
        Self::load_with_debug(workspace, false).await
    }

    pub async fn load_with_debug(
        workspace: Workspace,
        debug_reply: bool,
    ) -> anyhow::Result<Arc<Self>> {
        workspace.bootstrap()?;
        let cfg = config::load_config(&workspace.config_path())?;
        let env = Env::load(&workspace.dotenv_path(), &workspace.root().join(".env"));
        let store = Store::open(workspace.db_path())?;
        let mut map = HashMap::new();
        for a in adapters::build_enabled(&cfg, &env, &workspace).await? {
            let id = a.channel_id().0;
            store
                .upsert_channel(&ChannelId(id.clone()), AdapterHealth::Stopped)
                .await?;
            map.insert(id, a);
        }
        Ok(Arc::new(Self {
            workspace,
            config: cfg,
            events: EventBus::new(1024),
            store,
            adapters: Mutex::new(map),
            debug_reply,
        }))
    }
    pub async fn start_all(self: &Arc<Self>) -> anyhow::Result<()> {
        let (tx, mut rx) = mpsc::channel::<MessageEnvelope>(1024);
        let app = self.clone();
        tokio::spawn(async move {
            while let Some(m) = rx.recv().await {
                let _ = app.store.append_message(&m).await;
                app.publish_history_appended(&m);
                app.events.publish(Event::InboundMessage(m.clone()));
                if app.debug_reply {
                    let app = app.clone();
                    tokio::spawn(async move {
                        app.debug_reply_to(&m).await;
                    });
                }
            }
        });
        let mut guard = self.adapters.lock().await;
        for (id, a) in guard.iter_mut() {
            let ctx = AdapterContext {
                events: adapter_event_sender(self.clone()),
                inbound: tx.clone(),
                media_dir: self.workspace.media_dir(),
            };
            match a.start(ctx).await {
                Ok(()) => {
                    self.store
                        .upsert_channel(&ChannelId(id.clone()), AdapterHealth::Ready)
                        .await?;
                    self.events.publish(Event::AdapterStarted {
                        channel_id: ChannelId(id.clone()),
                    });
                }
                Err(e) => {
                    self.store
                        .upsert_channel(&ChannelId(id.clone()), AdapterHealth::Failed)
                        .await?;
                    self.events.publish(Event::AdapterFailed {
                        channel_id: ChannelId(id.clone()),
                        error: e.to_string(),
                    });
                }
            }
        }
        Ok(())
    }
    pub async fn handle(&self, req: Request) -> anyhow::Result<Value> {
        match req.op.as_str() {
            "ping" => Ok(json!({"pong":true})),
            "status" => Ok(
                json!({"workspace":self.workspace.root(),"socket":self.workspace.socket_path(),"channels":self.store.list_channels().await?}),
            ),
            "list_channels" => Ok(json!(self.store.list_channels().await?)),
            "list_conversations" => {
                let c = req.channel_id.map(ChannelId);
                Ok(json!(self.store.list_conversations(c.as_ref()).await?))
            }
            "send_message" | "reply_message" => self.send(req).await,
            "fetch_history" | "fetch_all_history" => Ok(json!(
                self.store
                    .fetch_history(None, None, req.limit.unwrap_or(100))
                    .await?
            )),
            "fetch_channel_history" => {
                let c = req
                    .channel_id
                    .map(ChannelId)
                    .context("channel_id required")?;
                let v = req.conversation_id.map(ConversationId);
                Ok(json!(
                    self.store
                        .fetch_history(Some(&c), v.as_ref(), req.limit.unwrap_or(100))
                        .await?
                ))
            }
            "start_adapter" => {
                let id = req.channel_id.context("channel_id required")?;
                self.events.publish(Event::Warning {
                    channel_id: Some(ChannelId(id.clone())),
                    message: "adapter lifecycle is controlled by run startup in this build".into(),
                });
                Ok(json!({"started":false,"reason":"use onlyne run"}))
            }
            "stop_adapter" | "restart_adapter" => {
                Ok(json!({"ok":false,"reason":"daemon lifecycle is external; restart onlyne run"}))
            }
            _ => Err(anyhow!("unknown op {}", req.op)),
        }
    }
    async fn debug_reply_to(&self, inbound: &MessageEnvelope) {
        let msg = OutboundMessage {
            channel_id: inbound.channel_id.clone(),
            conversation_id: inbound.conversation_id.clone(),
            text: Some(debug_reply_text(inbound)),
            format: MessageFormat::Plain,
            attachments: vec![],
        };
        let guard = self.adapters.lock().await;
        let Some(a) = guard.get(&inbound.channel_id.0) else {
            return;
        };
        match a.send_message(msg).await {
            Ok(out) => {
                let _ = self.store.append_message(&out).await;
                self.publish_history_appended(&out);
                self.events.publish(Event::OutboundMessage(out));
            }
            Err(e) => self.events.publish(Event::Warning {
                channel_id: Some(inbound.channel_id.clone()),
                message: format!("debug reply failed: {e}"),
            }),
        }
    }

    async fn send(&self, req: Request) -> anyhow::Result<Value> {
        let channel = req.channel_id.context("channel_id required")?;
        let conversation = req.conversation_id.context("conversation_id required")?;
        let msg = OutboundMessage {
            channel_id: ChannelId(channel.clone()),
            conversation_id: ConversationId(conversation),
            text: req.text,
            format: req.format,
            attachments: req.attachments,
        };
        self.validate_attachments(&msg.attachments).await?;
        let guard = self.adapters.lock().await;
        let a = guard
            .get(&channel)
            .ok_or_else(|| anyhow!("adapter {channel} not enabled"))?;
        let out = if msg.format == MessageFormat::Markdown
            && self.should_segment_tables(&msg, &channel)
        {
            self.send_segmented_markdown(a.as_ref(), msg.clone())
                .await?
        } else {
            a.send_message(msg.clone()).await?
        };
        self.store.append_message(&out).await?;
        self.publish_history_appended(&out);
        self.events.publish(Event::OutboundMessage(out.clone()));
        Ok(json!(out))
    }

    fn should_segment_tables(&self, msg: &OutboundMessage, channel: &str) -> bool {
        channel != "qqbot"
            && channel != "feishu"
            && msg.text.as_deref().is_some_and(|text| {
                markdown::split_tables(text)
                    .iter()
                    .any(|s| matches!(s, markdown::MarkdownSegment::Table(_)))
            })
    }

    async fn send_segmented_markdown(
        &self,
        adapter: &dyn Adapter,
        msg: OutboundMessage,
    ) -> anyhow::Result<MessageEnvelope> {
        let text = msg.text.clone().unwrap_or_default();
        let mut sent = Vec::new();
        for segment in markdown::split_tables(&text) {
            match segment {
                markdown::MarkdownSegment::Text(text) => sent.push(
                    adapter
                        .send_message(OutboundMessage {
                            channel_id: msg.channel_id.clone(),
                            conversation_id: msg.conversation_id.clone(),
                            text: Some(text),
                            format: MessageFormat::Markdown,
                            attachments: vec![],
                        })
                        .await?,
                ),
                markdown::MarkdownSegment::Table(table) => {
                    let path =
                        media::render_markdown_table_png(&self.workspace.rendered_dir(), &table)
                            .await?;
                    sent.push(
                        adapter
                            .send_message(OutboundMessage {
                                channel_id: msg.channel_id.clone(),
                                conversation_id: msg.conversation_id.clone(),
                                text: None,
                                format: MessageFormat::Plain,
                                attachments: vec![AttachmentRef {
                                    kind: AttachmentKind::Image,
                                    path: Some(path),
                                    url: None,
                                    file_name: Some("markdown-table.png".into()),
                                    mime_type: Some("image/png".into()),
                                    size: None,
                                }],
                            })
                            .await?,
                    );
                }
            }
        }
        if !msg.attachments.is_empty() {
            sent.push(
                adapter
                    .send_message(OutboundMessage {
                        channel_id: msg.channel_id.clone(),
                        conversation_id: msg.conversation_id.clone(),
                        text: None,
                        format: MessageFormat::Plain,
                        attachments: msg.attachments.clone(),
                    })
                    .await?,
            );
        }
        merge_segmented_envelopes(msg, sent)
    }

    async fn validate_attachments(&self, attachments: &[AttachmentRef]) -> anyhow::Result<()> {
        for a in attachments {
            if let Some(path) = &a.path {
                let len = tokio::fs::metadata(path)
                    .await
                    .with_context(|| format!("read attachment metadata {}", path.display()))?
                    .len();
                if len > self.config.rich_text.max_attachment_bytes {
                    return Err(anyhow!(
                        "attachment too large: {} > {} bytes",
                        len,
                        self.config.rich_text.max_attachment_bytes
                    ));
                }
            }
        }
        Ok(())
    }

    async fn publish_adapter_event(&self, ev: Event) {
        let health = match &ev {
            Event::AdapterStarted { channel_id } => Some((channel_id, AdapterHealth::Ready)),
            Event::AdapterStopped { channel_id } => Some((channel_id, AdapterHealth::Stopped)),
            Event::AdapterReconnecting { channel_id, .. } => {
                Some((channel_id, AdapterHealth::Reconnecting))
            }
            Event::AdapterFailed { channel_id, .. } => Some((channel_id, AdapterHealth::Failed)),
            _ => None,
        };
        if let Some((channel_id, health)) = health {
            let _ = self.store.upsert_channel(channel_id, health).await;
        }
        self.events.publish(ev);
    }

    fn publish_history_appended(&self, m: &MessageEnvelope) {
        self.events.publish(Event::HistoryAppended {
            channel_id: m.channel_id.clone(),
            message_id: m.message_id.clone(),
        });
    }

    pub async fn check(&self) -> anyhow::Result<Vec<(String, Result<(), String>)>> {
        let guard = self.adapters.lock().await;
        let mut out = vec![];
        for (id, a) in guard.iter() {
            out.push((id.clone(), a.check().await.map_err(|e| e.to_string())));
        }
        Ok(out)
    }
}
fn merge_segmented_envelopes(
    msg: OutboundMessage,
    sent: Vec<MessageEnvelope>,
) -> anyhow::Result<MessageEnvelope> {
    let Some(first) = sent.first() else {
        return Err(anyhow!("markdown send produced no segments"));
    };
    let parts: Vec<Value> = sent
        .iter()
        .map(|env| {
            json!({
                "kind": if env.attachments.iter().any(|a| matches!(a.kind, AttachmentKind::Image)) { "rendered_table" } else { "markdown_text" },
                "message_id": env.message_id.0,
                "metadata": env.platform_metadata,
            })
        })
        .collect();
    Ok(MessageEnvelope {
        channel_id: msg.channel_id,
        conversation_id: msg.conversation_id,
        message_id: first.message_id.clone(),
        direction: Direction::Outbound,
        sender_id: None,
        sender_name: None,
        text: msg.text,
        format: MessageFormat::Markdown,
        attachments: msg.attachments,
        delivery_state: DeliveryState::Sent,
        timestamp: chrono::Utc::now(),
        platform_metadata: json!({"segmented": true, "delivery_parts": parts}),
    })
}

fn debug_reply_text(m: &MessageEnvelope) -> String {
    let mut lines = vec![
        "onlyne debug auth/context".to_string(),
        format!("channel_id={}", m.channel_id.0),
        format!("conversation_id={}", m.conversation_id.0),
        format!("message_id={}", m.message_id.0),
    ];
    if let Some(v) = &m.sender_id {
        lines.push(format!("sender_id={v}"));
    }
    if let Some(v) = &m.sender_name {
        lines.push(format!("sender_name={v}"));
    }
    for key in [
        "thread_id",
        "parent_id",
        "root_id",
        "group_id",
        "session_id",
        "context_token",
        "chat_id",
        "message_thread_id",
    ] {
        if let Some(v) = m.platform_metadata.get(key) {
            let value = if secretish(key) {
                "<redacted>".to_string()
            } else if let Some(s) = v.as_str() {
                s.to_string()
            } else {
                v.to_string()
            };
            lines.push(format!("{key}={value}"));
        }
    }
    lines.join("\n")
}

fn secretish(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.contains("token") || key.contains("secret") || key.contains("password")
}

fn adapter_event_sender(app: Arc<App>) -> mpsc::Sender<Event> {
    let (tx, mut rx) = mpsc::channel(1024);
    tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            app.publish_adapter_event(ev).await;
        }
    });
    tx
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[tokio::test]
    async fn adapter_events_update_channel_health() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::resolve(dir.path());
        let app = App::load(ws).await.unwrap();

        app.store
            .upsert_channel(&ChannelId("feishu".into()), AdapterHealth::Ready)
            .await
            .unwrap();
        app.publish_adapter_event(Event::AdapterReconnecting {
            channel_id: ChannelId("feishu".into()),
            reason: "lost websocket".into(),
        })
        .await;

        let channels = app.store.list_channels().await.unwrap();
        assert!(matches!(channels[0].1, AdapterHealth::Reconnecting));
    }

    #[tokio::test]
    async fn feishu_tables_are_not_split_into_images() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::resolve(dir.path());
        let app = App::load(ws).await.unwrap();
        let msg = OutboundMessage {
            channel_id: ChannelId("feishu".into()),
            conversation_id: ConversationId("oc_1".into()),
            text: Some("| A | B |\n| --- | --- |\n| 1 | 2 |".into()),
            format: MessageFormat::Markdown,
            attachments: vec![],
        };

        assert!(!app.should_segment_tables(&msg, "feishu"));
        assert!(app.should_segment_tables(&msg, "telegram"));
    }

    #[test]
    fn debug_reply_text_includes_threadish_metadata() {
        let msg = MessageEnvelope {
            channel_id: ChannelId("weixin".into()),
            conversation_id: ConversationId("peer@im.wechat".into()),
            message_id: MessageId("m1".into()),
            direction: Direction::Inbound,
            sender_id: Some("sender".into()),
            sender_name: Some("Alice".into()),
            text: Some("hi".into()),
            format: MessageFormat::Plain,
            attachments: vec![],
            delivery_state: DeliveryState::Delivered,
            timestamp: Utc::now(),
            platform_metadata: serde_json::json!({
                "context_token":"secret-context",
                "thread_id":"thread-1",
                "parent_id": "parent-1"
            }),
        };
        let out = debug_reply_text(&msg);
        assert!(out.contains("channel_id=weixin"));
        assert!(out.contains("conversation_id=peer@im.wechat"));
        assert!(out.contains("sender_id=sender"));
        assert!(out.contains("message_id=m1"));
        assert!(out.contains("thread_id=thread-1"));
        assert!(out.contains("parent_id=parent-1"));
        assert!(out.contains("context_token=<redacted>"));
        assert!(!out.contains("secret-context"));
    }
}
