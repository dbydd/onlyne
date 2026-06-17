use crate::{
    adapters,
    config::{self, Config, Env},
    core::*,
    events::EventBus,
    ipc::Request,
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
}
impl App {
    pub async fn load(workspace: Workspace) -> anyhow::Result<Arc<Self>> {
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
        }))
    }
    pub async fn start_all(self: &Arc<Self>) -> anyhow::Result<()> {
        let (tx, mut rx) = mpsc::channel::<MessageEnvelope>(1024);
        let app = self.clone();
        tokio::spawn(async move {
            while let Some(m) = rx.recv().await {
                let _ = app.store.append_message(&m).await;
                app.events.publish(Event::HistoryAppended {
                    channel_id: m.channel_id.clone(),
                    message_id: m.message_id.clone(),
                });
                app.events.publish(Event::InboundMessage(m));
            }
        });
        let mut guard = self.adapters.lock().await;
        for (id, a) in guard.iter_mut() {
            let ctx = AdapterContext {
                events: event_sender(self.events.clone()),
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
    async fn send(&self, req: Request) -> anyhow::Result<Value> {
        let channel = req.channel_id.context("channel_id required")?;
        let conversation = req.conversation_id.context("conversation_id required")?;
        let msg = OutboundMessage {
            channel_id: ChannelId(channel.clone()),
            conversation_id: ConversationId(conversation),
            text: req.text,
            attachments: req.attachments,
        };
        let guard = self.adapters.lock().await;
        let a = guard
            .get(&channel)
            .ok_or_else(|| anyhow!("adapter {channel} not enabled"))?;
        let out = a.send_message(msg).await?;
        self.store.append_message(&out).await?;
        self.events.publish(Event::OutboundMessage(out.clone()));
        Ok(json!(out))
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
fn event_sender(bus: EventBus) -> mpsc::Sender<Event> {
    let (tx, mut rx) = mpsc::channel(1024);
    tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            bus.publish(ev)
        }
    });
    tx
}
