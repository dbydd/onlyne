use chrono::{DateTime, Utc};
use onlyne_adapter::AdapterIo;
use onlyne_config::{ClientEntry, MsgKindClass, Spec};
use onlyne_net::{AclTable, MsgClass};
use onlyne_proto::{Capability, ConversationInfo, Event, Frame, GatewayHealth};
use onlyne_store::{RoleRow, ServerLedger};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::sync::{Notify, broadcast, mpsc};

/// The shared runtime handle published by [`crate::serve`].
pub type State = Server;

/// One delivery handed to a connected client, awaiting `ack`.
#[derive(Debug, Clone)]
pub struct DeliveryTicket {
    pub msg_id: String,
    pub role: String,
    pub session_id: Option<String>,
    pub generation: u64,
    pub seq: u64,
    pub delivered_at: DateTime<Utc>,
}

/// Adapter transport of one gateway connection, filled once `hello` lands.
#[derive(Default)]
pub struct AdapterLink {
    pub io: tokio::sync::Mutex<Option<AdapterIo>>,
}

impl std::fmt::Debug for AdapterLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AdapterLink")
    }
}

/// One registered platform channel with the conversations it announced.
#[derive(Debug, Clone)]
pub struct ChannelBinding {
    pub gateway: String,
    pub platform: String,
    pub channel: String,
    pub conversations: Vec<ConversationInfo>,
    pub registered_at: DateTime<Utc>,
}

/// Map one spec message class onto the ACL class the table evaluates.
pub fn msg_class(kind: MsgKindClass) -> MsgClass {
    match kind {
        MsgKindClass::Any => MsgClass::Any,
        MsgKindClass::Note => MsgClass::Note,
        MsgKindClass::Control => MsgClass::Control,
    }
}

/// Build the ACL table for a spec from its concrete edges.
///
/// `Spec::acl_edges` is the only place a wildcard has meaning: it expands
/// `"*"` against `Spec::role_names` and returns concrete pairs. The raw
/// `allowed_senders` and `allowed_targets` lists stay out of this path.
pub fn acl_from_spec(spec: &Spec) -> anyhow::Result<AclTable> {
    let roles = spec
        .client
        .iter()
        .map(|entry| (entry.role.clone(), entry.key.clone(), entry.admin));
    let edges = spec.acl_edges().into_iter().map(|edge| onlyne_net::AclEdge {
        from: edge.from,
        to: edge.to,
        class: msg_class(edge.kind),
        admin: edge.admin,
    });
    Ok(AclTable::new(roles, edges)?)
}

#[derive(Debug, Clone)]
pub struct RoleConnection {
    pub role: String,
    pub sender: mpsc::Sender<Frame>,
    pub last_seq: u64,
    pub connected_at: DateTime<Utc>,
    pub draining: bool,
}

/// One live gateway process and the transport it speaks.
#[derive(Debug, Clone)]
pub struct GatewayConnection {
    pub gateway: String,
    pub platform: String,
    pub sender: mpsc::Sender<Frame>,
    pub connected_at: DateTime<Utc>,
    pub health: GatewayHealth,
    pub last_health_at: Option<DateTime<Utc>>,
    pub channels: Vec<ChannelBinding>,
    pub adapter: Option<Arc<AdapterLink>>,
    /// Capabilities the gateway declared at `hello`.
    pub capabilities: Vec<Capability>,
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
    pub acl: RwLock<Arc<AclTable>>,
    pub deliveries: RwLock<HashMap<String, DeliveryTicket>>,
    pub expiries: RwLock<HashMap<String, DateTime<Utc>>>,
    pub channels: RwLock<HashMap<String, ChannelBinding>>,
    pub shutdown: Arc<Notify>,
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
            acl: RwLock::new(Arc::new(acl_from_spec(&spec)?)),
            spec: Arc::new(RwLock::new(spec.clone())),
            ledger,
            net: RwLock::new(ListenerHandles::default()),
            roles: RwLock::new(RoleRegistry::default()),
            gateways: RwLock::new(GatewayRegistry::default()),
            events,
            start_at: Utc::now(),
            root: init.root.clone(),
            deliveries: RwLock::new(HashMap::new()),
            expiries: RwLock::new(HashMap::new()),
            channels: RwLock::new(HashMap::new()),
            shutdown: Arc::new(Notify::new()),
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

    /// The current ACL table, rebuilt on every successful reload.
    pub fn acl_table(&self) -> Arc<AclTable> {
        self.acl
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_else(|_| Arc::new(AclTable::default()))
    }

    /// Swap in a freshly built ACL table.
    pub fn replace_acl(&self, table: AclTable) {
        if let Ok(mut guard) = self.acl.write() {
            *guard = Arc::new(table);
        }
    }

    /// Remember one handed-out delivery until it is acknowledged.
    pub fn record_delivery(&self, ticket: DeliveryTicket) {
        if let Ok(mut table) = self.deliveries.write() {
            table.insert(ticket.msg_id.clone(), ticket);
        }
    }

    /// Remove the ticket for one message.
    pub fn take_delivery(&self, msg_id: &str) -> Option<DeliveryTicket> {
        self.deliveries.write().ok()?.remove(msg_id)
    }

    /// The unacknowledged delivery of one session, when it holds one.
    pub fn open_delivery(&self, role: &str, session_id: Option<&str>) -> Option<DeliveryTicket> {
        let table = self.deliveries.read().ok()?;
        table
            .values()
            .find(|ticket| ticket.role == role && ticket.session_id.as_deref() == session_id)
            .cloned()
    }

    /// Drop every unacknowledged delivery of one role after its link closed.
    pub fn clear_deliveries(&self, role: &str) -> usize {
        let Ok(mut table) = self.deliveries.write() else {
            return 0;
        };
        let before = table.len();
        table.retain(|_, ticket| ticket.role != role);
        before - table.len()
    }

    /// Arm the expiry deadline of one queued note.
    pub fn note_expiry(&self, msg_id: &str, deadline: DateTime<Utc>) {
        if let Ok(mut table) = self.expiries.write() {
            table.insert(msg_id.to_string(), deadline);
        }
    }

    /// Every armed deadline that has come due, oldest first.
    pub fn due_expiries(&self, now: DateTime<Utc>) -> Vec<String> {
        let Ok(table) = self.expiries.read() else {
            return Vec::new();
        };
        let mut due: Vec<(String, DateTime<Utc>)> = table
            .iter()
            .filter(|(_, deadline)| **deadline <= now)
            .map(|(msg_id, deadline)| (msg_id.clone(), *deadline))
            .collect();
        due.sort_by_key(|(_, deadline)| *deadline);
        due.into_iter().map(|(msg_id, _)| msg_id).collect()
    }

    /// Forget one expiry deadline.
    pub fn forget_expiry(&self, msg_id: &str) {
        if let Ok(mut table) = self.expiries.write() {
            table.remove(msg_id);
        }
    }

    /// Record a channel declaration from a gateway.
    pub fn register_channel(&self, binding: ChannelBinding) {
        if let Ok(mut table) = self.channels.write() {
            table.insert(
                format!("{}:{}", binding.gateway, binding.channel),
                binding,
            );
        }
    }

    /// Every channel a gateway declared.
    pub fn channels_for(&self, gateway: &str) -> Vec<ChannelBinding> {
        let Ok(table) = self.channels.read() else {
            return Vec::new();
        };
        let mut rows: Vec<ChannelBinding> = table
            .values()
            .filter(|binding| binding.gateway == gateway)
            .cloned()
            .collect();
        rows.sort_by(|left, right| left.channel.cmp(&right.channel));
        rows
    }

    /// Count of every declared channel.
    pub fn channel_count(&self) -> usize {
        self.channels.read().map(|table| table.len()).unwrap_or(0)
    }

    /// Drop every channel a gateway declared after its link closed.
    pub fn clear_channels(&self, gateway: &str) -> usize {
        let Ok(mut table) = self.channels.write() else {
            return 0;
        };
        let before = table.len();
        table.retain(|_, binding| binding.gateway != gateway);
        before - table.len()
    }

    /// Health of one connected gateway.
    pub fn gateway_health(&self, gateway: &str) -> Option<GatewayHealth> {
        self.gateways.read().ok()?.get(gateway).map(|link| link.health)
    }

    /// Note a health observation and report whether it changed the state.
    pub fn touch_health(
        &self,
        gateway: &str,
        health: GatewayHealth,
        at: DateTime<Utc>,
    ) -> Option<GatewayHealth> {
        let mut table = self.gateways.write().ok()?;
        let link = table.entries.get_mut(gateway)?;
        let previous = link.health;
        link.health = health;
        link.last_health_at = Some(at);
        (previous != health).then_some(previous)
    }

    /// Every gateway id whose last health observation is older than `limit_ms`.
    pub fn stale_gateways(&self, now: DateTime<Utc>, limit_ms: u64) -> Vec<String> {
        let Ok(table) = self.gateways.read() else {
            return Vec::new();
        };
        let limit = chrono::Duration::milliseconds(limit_ms as i64);
        let mut stale: Vec<String> = table
            .entries
            .values()
            .filter(|link| match link.last_health_at {
                Some(at) => now - at > limit,
                None => true,
            })
            .map(|link| link.gateway.clone())
            .collect();
        stale.sort();
        stale
    }

    /// Subscribe to the bounded broadcast of durable and advisory events.
    pub fn subscribe_frames(&self) -> broadcast::Receiver<Arc<Frame>> {
        self.events.subscribe()
    }

    /// Ask every listener loop to stop.
    pub fn request_shutdown(&self) {
        self.shutdown.notify_waiters();
    }

    /// Wait for the shutdown request.
    pub async fn await_shutdown(&self) {
        self.shutdown.notified().await;
    }
}
