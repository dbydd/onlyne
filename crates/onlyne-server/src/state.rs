use chrono::{DateTime, Utc};
use onlyne_config::{ClientEntry, Spec};
use onlyne_proto::{Event, Frame};
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
    pub connected_at: DateTime<Utc>,
    pub draining: bool,
}

#[derive(Debug, Clone)]
pub struct GatewayConnection {
    pub gateway: String,
    pub sender: mpsc::Sender<Frame>,
    pub connected_at: DateTime<Utc>,
}

#[derive(Debug, Default)]
pub struct RoleRegistry {
    pub entries: HashMap<String, RoleConnection>,
}

impl RoleRegistry {
    pub fn insert(&mut self, conn: RoleConnection) {
        self.entries.insert(conn.role.clone(), conn);
    }

    pub fn remove(&mut self, role: &str) -> Option<RoleConnection> {
        self.entries.remove(role)
    }

    pub fn get(&self, role: &str) -> Option<&RoleConnection> {
        self.entries.get(role)
    }

    pub fn contains(&self, role: &str) -> bool {
        self.entries.contains_key(role)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Debug, Default)]
pub struct GatewayRegistry {
    pub entries: HashMap<String, GatewayConnection>,
}

impl GatewayRegistry {
    pub fn insert(&mut self, conn: GatewayConnection) {
        self.entries.insert(conn.gateway.clone(), conn);
    }

    pub fn remove(&mut self, gateway: &str) -> Option<GatewayConnection> {
        self.entries.remove(gateway)
    }

    pub fn get(&self, gateway: &str) -> Option<&GatewayConnection> {
        self.entries.get(gateway)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Debug, Default)]
pub struct ListenerHandles {
    pub tcp: Option<tokio::net::TcpListener>,
    pub admin: Option<tokio::net::UnixListener>,
}

#[derive(Debug)]
pub struct Server {
    pub spec: Arc<RwLock<Spec>>,
    pub ledger: ServerLedger,
    pub net: RwLock<ListenerHandles>,
    pub roles: RwLock<RoleRegistry>,
    pub gateways: RwLock<GatewayRegistry>,
    pub events: broadcast::Sender<Arc<Frame>>,
    pub start_at: DateTime<Utc>,
    pub root: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ServerInit {
    pub root: PathBuf,
    pub listen: Option<String>,
}

impl Server {
    pub fn open(init: &ServerInit) -> anyhow::Result<Arc<Self>> {
        let layout = onlyne_layout::ServerRoot::resolve(&init.root);
        layout.bootstrap()?;
        let spec = Spec::load(layout.spec_path())?;
        let ledger = ServerLedger::open(
            layout.state_db_path(),
            spec.server.fault_history_days,
        )?;
        let (events, _) = broadcast::channel(256);
        let server = Arc::new(Self {
            spec: Arc::new(RwLock::new(spec.clone())),
            ledger,
            net: RwLock::new(ListenerHandles::default()),
            roles: RwLock::new(RoleRegistry::default()),
            gateways: RwLock::new(GatewayRegistry::default()),
            events,
            start_at: Utc::now(),
            root: init.root.clone(),
        });
        let hash = spec.semantic_hash();
        for role in &spec.client {
            server.ledger.upsert_role(&RoleRow {
                name: role.role.clone(),
                key: role.key.clone(),
                admin: role.admin,
                max_sessions: i64::from(role.max_sessions),
                spec_hash: hash.clone(),
                updated_at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            })?;
        }
        Ok(server)
    }

    pub fn spec_snapshot(&self) -> Option<Spec> {
        self.spec.read().ok().map(|guard| guard.clone())
    }

    pub fn replace_spec(&self, next: Spec) -> Option<Spec> {
        self.spec.write().ok().map(|mut guard| std::mem::replace(&mut *guard, next))
    }

    pub fn role(&self, name: &str) -> Option<ClientEntry> {
        self.spec
            .read()
            .ok()?
            .client
            .iter()
            .find(|entry| entry.role == name)
            .cloned()
    }

    pub fn is_connected(&self, role: &str) -> bool {
        self.roles.read().map(|table| table.contains(role)).unwrap_or(false)
    }

    pub fn role_sender(&self, role: &str) -> Option<mpsc::Sender<Frame>> {
        self.roles.read().ok()?.get(role).map(|conn| conn.sender.clone())
    }

    pub fn register_role(&self, conn: RoleConnection) {
        if let Ok(mut table) = self.roles.write() {
            table.insert(conn);
        }
    }

    pub fn unregister_role(&self, role: &str) -> Option<RoleConnection> {
        self.roles.write().ok()?.remove(role)
    }

    pub fn register_gateway(&self, conn: GatewayConnection) {
        if let Ok(mut table) = self.gateways.write() {
            table.insert(conn);
        }
    }

    pub fn unregister_gateway(&self, gateway: &str) -> Option<GatewayConnection> {
        self.gateways.write().ok()?.remove(gateway)
    }

    pub fn event_head(&self) -> i64 {
        self.ledger.event_head().unwrap_or(0)
    }

    pub fn emit(&self, event: Event) -> anyhow::Result<u64> {
        let seq = self.ledger.append_event(event.type_name(), &serde_json::to_value(&event)?)? as u64;
        let _ = self.events.send(Arc::new(Frame::event(seq, event)));
        Ok(seq)
    }
}
