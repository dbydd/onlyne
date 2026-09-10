use chrono::Utc;
use onlyne_config::{ClientEntry, Spec};
use onlyne_proto::{Event, Frame, LedgerState, RolePresence};
use onlyne_store::{RoleRow, ServerLedger};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::sync::{broadcast, mpsc};

#[derive(Debug, Clone)]
pub struct RoleConnection {
    pub role: String,
    pub sender: mpsc::Sender<Frame>,
    pub last_seq: u64,
    pub connected_at: chrono::DateTime<Utc>,
    pub draining: bool,
}
#[derive(Debug, Clone)]
pub struct GatewayConnection {
    pub gateway: String,
    pub sender: mpsc::Sender<Frame>,
    pub connected_at: chrono::DateTime<Utc>,
}
#[derive(Debug, Default)]
pub struct RoleRegistry { pub entries: HashMap<String, RoleConnection> }
#[derive(Debug, Default)]
pub struct GatewayRegistry { pub entries: HashMap<String, GatewayConnection> }
#[derive(Debug, Default)]
pub struct ListenerHandles { pub tcp: Option<tokio::net::TcpListener>, pub admin: Option<tokio::net::UnixListener> }

pub struct Server {
    pub spec: Arc<RwLock<Spec>>,
    pub ledger: ServerLedger,
    pub net: ListenerHandles,
    pub roles: RwLock<RoleRegistry>,
    pub gateways: RwLock<GatewayRegistry>,
    pub events: broadcast::Sender<Arc<Frame>>,
    pub start_at: chrono::DateTime<Utc>,
    pub root: PathBuf,
}
#[derive(Debug, Clone)]
pub struct ServerInit { pub root: PathBuf, pub listen: Option<String> }

impl Server {
    pub fn open(init: &ServerInit) -> anyhow::Result<Arc<Self>> {
        let layout = onlyne_layout::ServerRoot::resolve(&init.root);
        let spec = Spec::load(layout.spec_path())?;
        layout.bootstrap()?;
        let ledger = ServerLedger::open(layout.state_db_path(), spec.server.fault_history_days)?;
        let (events, _) = broadcast::channel(256);
        let server = Arc::new(Self { spec: Arc::new(RwLock::new(spec.clone())), ledger, net: ListenerHandles::default(), roles: RwLock::new(RoleRegistry::default()), gateways: RwLock::new(GatewayRegistry::default()), events, start_at: Utc::now(), root: init.root.clone() });
        for role in &spec.client { server.ledger.upsert_role(&RoleRow { name: role.role.clone(), key: role.key.clone(), admin: role.admin, max_sessions: i64::from(role.max_sessions), spec_hash: spec.semantic_hash(), updated_at: Utc::now().to_rfc3339() })?; }
        Ok(server)
    }
    pub fn role(&self, name: &str) -> Option<ClientEntry> { self.spec.read().ok()?.client.iter().find(|r| r.role == name).cloned() }
    pub fn emit(&self, event: Event) -> anyhow::Result<u64> {
        let seq = self.ledger.append_event(event.type_name(), &serde_json::to_value(&event)?)? as u64;
        let _ = self.events.send(Arc::new(Frame::event(seq, event)));
        Ok(seq)
    }
}
