use crate::layout::{
    LayoutEdge, LayoutNode, Presence as LayoutPresence, SessionLine, SessionState,
};
use chrono::{DateTime, Utc};
use onlyne_proto::{
    AdminFrame, AdminOp, EventRow, FaultEvent, Frame, LedgerEntry, LedgerQuery, LedgerState,
    Lifecycle, MsgKind, Presence, Principal, QueryFaultsArgs, QueryRolesArgs, QuerySessionsArgs,
    ResBody, RoleInfo, SessionRow, new_id,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;
use std::time::SystemTime;
use tokio::net::UnixStream;
use tokio::time::{Duration, timeout};

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub status: Value,
    pub roles: Vec<RoleView>,
    pub sessions: Vec<SessionRow>,
    pub ledger: Vec<LedgerEntry>,
    pub faults: Vec<FaultEvent>,
    pub history: Vec<EventRow>,
    pub history_total: usize,
    pub server_online: bool,
    pub last_error: Option<String>,
    pub refreshed_at: Option<SystemTime>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleView {
    pub name: String,
    pub admin: bool,
    pub max_sessions: u32,
    pub spec_hash: String,
    pub prose: Option<String>,
    pub state: Presence,
    pub session_count: u32,
    pub detail: Option<String>,
    pub edges: Vec<String>,
    pub aggregate: Option<String>,
}

impl From<RoleInfo> for RoleView {
    fn from(info: RoleInfo) -> Self {
        Self {
            name: info.name,
            admin: info.admin,
            max_sessions: info.max_sessions,
            spec_hash: info.spec_hash,
            prose: info.prose,
            state: info.state,
            session_count: info.sessions,
            detail: info.detail,
            edges: info.edges,
            aggregate: info.aggregate,
        }
    }
}

/// Decode one `roles` row. A row from a server that predates `edges`,
/// `aggregate`, `reuse`, or `session_command` still lands: `edges` and
/// `aggregate` default and the page then draws no arrow, `reuse` defaults to
/// `true`, and `session_command` defaults to an empty token list.
impl TryFrom<Value> for RoleView {
    type Error = serde_json::Error;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        serde_json::from_value::<RoleInfo>(value).map(RoleView::from)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlertKind {
    /// Something wants an operator decision now.
    Alert,
    /// Routine bookkeeping worth showing.
    Notice,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alert {
    pub text: String,
    pub kind: AlertKind,
}

/// The alert strip page 2 keeps under its graph: every fault that still wants a
/// decision, then any notice the status answer carries.
pub fn alerts(snapshot: &Snapshot) -> Vec<Alert> {
    let mut out = Vec::new();
    for fault in &snapshot.faults {
        if fault.state.as_deref() == Some("acked") {
            continue;
        }
        out.push(Alert {
            text: format!(
                "{} [{}] {}",
                fault.kind,
                fault.role.as_deref().unwrap_or("-"),
                fault.reason
            ),
            kind: AlertKind::Alert,
        });
    }
    if let Some(notices) = snapshot.status.get("alerts").and_then(Value::as_array) {
        for notice in notices.iter().filter_map(Value::as_str) {
            out.push(Alert {
                text: notice.to_string(),
                kind: AlertKind::Notice,
            });
        }
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    RoleMap,
    Swarm,
}

impl Page {
    /// How many pages the footer counts.
    pub const COUNT: u8 = 2;

    pub fn toggle(self) -> Self {
        match self {
            Page::RoleMap => Page::Swarm,
            Page::Swarm => Page::RoleMap,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Page::RoleMap => "roles",
            Page::Swarm => "swarm",
        }
    }

    /// The page's position in the footer's count.
    pub fn number(self) -> u8 {
        match self {
            Page::RoleMap => 1,
            Page::Swarm => 2,
        }
    }

    /// The keys this page answers to. Each page lists only its own keys, so
    /// the legend never advertises a key that does nothing here.
    pub fn keys(self) -> &'static str {
        match self {
            Page::RoleMap => {
                "1/2·Tab switch  hjkl navigate  ←→↑↓ pan  +/- repel  Enter detail  e edges  a all/active  r refresh  q quit"
            }
            Page::Swarm => {
                "1/2·Tab switch  g/h focus  ↑↓ select  Enter detail  / search  f state  t window  o role  e edge  a all/active  PgUp/PgDn page  r refresh  q quit"
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Graph,
    History,
}

impl Focus {
    pub fn toggle(self) -> Self {
        match self {
            Focus::Graph => Focus::History,
            Focus::History => Focus::Graph,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Focus::Graph => "graph",
            Focus::History => "history",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeWindow {
    Any,
    Hour,
    Day,
    Week,
}

impl TimeWindow {
    pub fn next(self) -> Self {
        match self {
            TimeWindow::Any => TimeWindow::Hour,
            TimeWindow::Hour => TimeWindow::Day,
            TimeWindow::Day => TimeWindow::Week,
            TimeWindow::Week => TimeWindow::Any,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TimeWindow::Any => "any",
            TimeWindow::Hour => "1h",
            TimeWindow::Day => "24h",
            TimeWindow::Week => "7d",
        }
    }

    pub fn cutoff(self) -> Option<DateTime<Utc>> {
        let now = Utc::now();
        match self {
            TimeWindow::Any => None,
            TimeWindow::Hour => Some(now - chrono::Duration::hours(1)),
            TimeWindow::Day => Some(now - chrono::Duration::days(1)),
            TimeWindow::Week => Some(now - chrono::Duration::weeks(1)),
        }
    }
}

pub const STATES: [&str; 8] = [
    "active",
    "all",
    "queued",
    "in_flight",
    "acked",
    "rejected",
    "expired",
    "fault",
];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EdgeFilter {
    pub from: String,
    pub to: String,
}

#[derive(Clone, Debug)]
pub struct HistoryFilter {
    pub state: String,
    pub role: Option<String>,
    pub edge: Option<EdgeFilter>,
    pub text: String,
    pub window: TimeWindow,
    pub offset: usize,
}

impl Default for HistoryFilter {
    fn default() -> Self {
        Self {
            state: "active".to_string(),
            role: None,
            edge: None,
            text: String::new(),
            window: TimeWindow::Any,
            offset: 0,
        }
    }
}

impl HistoryFilter {
    pub fn label(&self) -> String {
        let role = self.role.as_deref().unwrap_or("any");
        let edge = self
            .edge
            .as_ref()
            .map(|edge| format!("{}→{}", edge.from, edge.to))
            .unwrap_or_else(|| "any".to_string());
        format!(
            "state={} role={} edge={} text=\"{}\" win={}",
            self.state,
            role,
            edge,
            self.text,
            self.window.label()
        )
    }

    pub fn reset_page(&mut self) {
        self.offset = 0;
    }
}

#[derive(Clone, Debug)]
pub struct UiState {
    pub page: Page,
    pub focus: Focus,
    pub graph_cursor: usize,
    pub history_cursor: usize,
    pub filter: HistoryFilter,
    /// The page-1 pick, held by name so a refresh cannot move the cursor under
    /// the operator's feet.
    pub role_selected: Option<String>,
    /// Which of the selected role's out-edges `j`/`k` stands on.
    pub role_edge: Option<usize>,
    /// The roles the cursor walked through, oldest first: `h` pops one.
    pub role_trail: Vec<String>,
    /// The page-1 camera, in world cells. Clamped to the map extent.
    pub role_pan: (usize, usize),
    /// Role-map repulsion. Higher values widen gutters and row channels.
    pub spacing: usize,
    /// Whether the views list only the sessions still holding a slot. `a`
    /// flips it, and the history views keep their own state filter.
    pub active_only: bool,
    /// Whether control-plane out-edges are drawn. They crowd the chain, so
    /// the page hides them until `e`.
    pub show_control_edges: bool,
    /// The pane's subject: page 1 holds a role, page 2 a task.
    pub detail: Option<Detail>,
    /// The subject the loaded detail belongs to.
    pub detail_key: Option<String>,
    pub detail_scroll: u16,
    pub search: Option<String>,
    pub message: String,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            page: Page::RoleMap,
            focus: Focus::Graph,
            graph_cursor: 0,
            history_cursor: 0,
            filter: HistoryFilter::default(),
            role_selected: None,
            role_edge: None,
            role_trail: Vec::new(),
            role_pan: (0, 0),
            spacing: DEFAULT_SPACING,
            active_only: true,
            show_control_edges: false,
            detail: None,
            detail_key: None,
            detail_scroll: 0,
            search: None,
            message: String::new(),
        }
    }
}

/// The detail pane's subject.
#[derive(Clone, Debug)]
pub enum Detail {
    Task(TaskDetail),
    Role(RoleDetail),
}

#[derive(Clone, Debug, Default)]
pub struct TaskDetail {
    pub task_id: String,
    pub ledger: Vec<LedgerEntry>,
    pub sessions: Vec<SessionRow>,
    pub faults: Vec<FaultEvent>,
}

/// Everything the page-1 panel says about one role: its registry row, the
/// sessions the server projects onto it, and its faults.
#[derive(Clone, Debug)]
pub struct RoleDetail {
    pub role: String,
    pub state: Presence,
    pub session_count: u32,
    pub max_sessions: u32,
    pub admin: bool,
    pub aggregate: Option<String>,
    /// The role's `allowed_targets` verbatim.
    pub peers: Vec<String>,
    pub faults: Vec<FaultEvent>,
    pub sessions: Vec<SessionRow>,
}

/// What the page-1 map knows about one role's place in the topology.
impl RoleView {
    pub fn control(&self) -> bool {
        control_role(&self.name, self.aggregate.as_deref())
    }
}

pub async fn pull(socket: &Path, filter: &HistoryFilter, page_size: usize) -> Snapshot {
    let mut snapshot = Snapshot::default();
    let status = match request_data(socket, AdminOp::Status(json!({}))).await {
        Ok(value) => value,
        Err(error) => {
            snapshot.server_online = false;
            snapshot.last_error = Some(error.to_string());
            snapshot.refreshed_at = Some(SystemTime::now());
            return snapshot;
        }
    };
    snapshot.status = status;
    snapshot.server_online = true;

    match request_data(socket, AdminOp::Roles(QueryRolesArgs::default())).await {
        Ok(value) => {
            snapshot.roles = value
                .get("roles")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|row| RoleView::try_from(row.clone()).ok())
                .collect();
        }
        Err(error) => snapshot.last_error = Some(error.to_string()),
    }
    match request_data(
        socket,
        AdminOp::Sessions(QuerySessionsArgs {
            limit: 500,
            ..QuerySessionsArgs::default()
        }),
    )
    .await
    {
        Ok(value) => {
            snapshot.sessions =
                serde_json::from_value(value.get("sessions").cloned().unwrap_or_else(|| json!([])))
                    .unwrap_or_default();
        }
        Err(error) => snapshot.last_error = Some(error.to_string()),
    }
    match request_data(
        socket,
        AdminOp::Ledger(LedgerQuery {
            limit: 500,
            ..LedgerQuery::default()
        }),
    )
    .await
    {
        Ok(value) => {
            snapshot.ledger =
                serde_json::from_value(value.get("ledger").cloned().unwrap_or_else(|| json!([])))
                    .unwrap_or_default();
        }
        Err(error) => snapshot.last_error = Some(error.to_string()),
    }
    match request_data(
        socket,
        AdminOp::Faults(QueryFaultsArgs {
            limit: 200,
            ..QueryFaultsArgs::default()
        }),
    )
    .await
    {
        Ok(value) => {
            snapshot.faults =
                serde_json::from_value(value.get("faults").cloned().unwrap_or_else(|| json!([])))
                    .unwrap_or_default();
        }
        Err(error) => snapshot.last_error = Some(error.to_string()),
    }
    match request_data(
        socket,
        AdminOp::History(onlyne_proto::HistoryArgs {
            since_seq: 0,
            limit: 500,
            kind: None,
            task_id: None,
        }),
    )
    .await
    {
        Ok(value) => {
            let mut history: Vec<EventRow> =
                serde_json::from_value(value.get("events").cloned().unwrap_or_else(|| json!([])))
                    .unwrap_or_default();
            apply_history_filter(&mut history, filter);
            snapshot.history_total = history.len();
            let end = (filter.offset + page_size).min(history.len());
            snapshot.history = if filter.offset < history.len() {
                history[filter.offset..end].to_vec()
            } else {
                Vec::new()
            };
        }
        Err(error) => snapshot.last_error = Some(error.to_string()),
    }
    snapshot.refreshed_at = Some(SystemTime::now());
    snapshot
}

pub async fn detail(socket: &Path, task_id: &str) -> anyhow::Result<TaskDetail> {
    let ledger = request_data(
        socket,
        AdminOp::Ledger(LedgerQuery {
            task: Some(task_id.to_string()),
            limit: 200,
            ..LedgerQuery::default()
        }),
    )
    .await?;
    let sessions = request_data(
        socket,
        AdminOp::Sessions(QuerySessionsArgs {
            task_id: Some(task_id.to_string()),
            limit: 50,
            ..QuerySessionsArgs::default()
        }),
    )
    .await?;
    let faults = request_data(
        socket,
        AdminOp::Faults(QueryFaultsArgs {
            task_id: Some(task_id.to_string()),
            limit: 50,
            ..QueryFaultsArgs::default()
        }),
    )
    .await?;
    Ok(TaskDetail {
        task_id: task_id.to_string(),
        ledger: serde_json::from_value(ledger.get("ledger").cloned().unwrap_or_else(|| json!([])))
            .unwrap_or_default(),
        sessions: serde_json::from_value(
            sessions
                .get("sessions")
                .cloned()
                .unwrap_or_else(|| json!([])),
        )
        .unwrap_or_default(),
        faults: serde_json::from_value(faults.get("faults").cloned().unwrap_or_else(|| json!([])))
            .unwrap_or_default(),
    })
}
/// Everything the page-1 panel shows for one role.
pub async fn role_detail(socket: &Path, role: &str) -> anyhow::Result<RoleDetail> {
    let roles = request_data(
        socket,
        AdminOp::Roles(QueryRolesArgs {
            role: Some(role.to_string()),
        }),
    )
    .await?;
    let row = roles
        .get("roles")
        .and_then(Value::as_array)
        .and_then(|rows| {
            rows.iter()
                .find(|row| row.get("name").and_then(Value::as_str) == Some(role))
        })
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("the server does not register role {role}"))?;
    let view = RoleView::try_from(row)?;
    let sessions = request_data(
        socket,
        AdminOp::Sessions(QuerySessionsArgs {
            role: Some(role.to_string()),
            limit: 50,
            ..QuerySessionsArgs::default()
        }),
    )
    .await?;
    let faults = request_data(
        socket,
        AdminOp::Faults(QueryFaultsArgs {
            role: Some(role.to_string()),
            limit: 50,
            ..QueryFaultsArgs::default()
        }),
    )
    .await?;
    Ok(RoleDetail {
        role: view.name,
        state: view.state,
        session_count: view.session_count,
        max_sessions: view.max_sessions,
        admin: view.admin,
        aggregate: view.aggregate,
        peers: view.edges,
        faults: serde_json::from_value(faults.get("faults").cloned().unwrap_or_else(|| json!([])))
            .unwrap_or_default(),
        sessions: serde_json::from_value(
            sessions
                .get("sessions")
                .cloned()
                .unwrap_or_else(|| json!([])),
        )
        .unwrap_or_default(),
    })
}

async fn request_data(socket: &Path, op: AdminOp) -> anyhow::Result<Value> {
    let mut stream = timeout(Duration::from_millis(1500), UnixStream::connect(socket)).await??;
    let request: AdminFrame = Frame::Req { id: new_id(), op };
    timeout(
        Duration::from_millis(1500),
        onlyne_frame::write_frame(&mut stream, &request),
    )
    .await??;
    let frame = timeout(
        Duration::from_millis(1500),
        onlyne_frame::read_frame::<_, AdminFrame>(&mut stream),
    )
    .await??
    .ok_or_else(|| anyhow::anyhow!("server closed the connection"))?;
    match frame {
        Frame::Res { body, .. } => data_or_error(body),
        other => anyhow::bail!("expected res frame, got {:?}", other),
    }
}

fn data_or_error(body: ResBody) -> anyhow::Result<Value> {
    if body.ok {
        Ok(body.data.unwrap_or(Value::Null))
    } else {
        let message = body
            .error
            .map(|error| error.message)
            .unwrap_or_else(|| "admin request failed".to_string());
        anyhow::bail!(message)
    }
}

fn apply_history_filter(rows: &mut Vec<EventRow>, filter: &HistoryFilter) {
    if let Some(cutoff) = filter.window.cutoff() {
        rows.retain(|row| row.created_at >= cutoff);
    }
    if filter.state != "all" {
        rows.retain(|row| event_matches_state(row, &filter.state));
    }
    if let Some(role) = &filter.role {
        rows.retain(|row| event_roles(row).iter().any(|candidate| candidate == role));
    }
    if let Some(edge) = &filter.edge {
        rows.retain(|row| {
            event_edge(row)
                .map(|(from, to)| from == edge.from && to == edge.to)
                .unwrap_or(false)
        });
    }
    if !filter.text.is_empty() {
        let needle = filter.text.to_lowercase();
        rows.retain(|row| {
            serde_json::to_string(row)
                .unwrap_or_default()
                .to_lowercase()
                .contains(&needle)
        });
    }
    rows.sort_by_key(|row| std::cmp::Reverse(row.seq));
}

fn event_matches_state(row: &EventRow, state: &str) -> bool {
    match state {
        "active" => match &row.event {
            onlyne_proto::Event::LedgerState(event) => {
                matches!(event.state, LedgerState::Queued | LedgerState::InFlight)
            }
            // Exit ends the session, whatever the projection's agent phase
            // last reported.
            onlyne_proto::Event::SessionState(event) => {
                !matches!(event.projection.lifecycle, Lifecycle::Exited)
            }
            onlyne_proto::Event::Fault(event) => event.state.as_deref() != Some("acked"),
            _ => false,
        },
        "queued" | "in_flight" | "acked" | "rejected" | "expired" => match &row.event {
            onlyne_proto::Event::LedgerState(event) => event.state.as_str() == state,
            _ => false,
        },
        "fault" => matches!(row.event, onlyne_proto::Event::Fault(_)),
        _ => true,
    }
}

fn event_roles(row: &EventRow) -> Vec<String> {
    match &row.event {
        onlyne_proto::Event::RolePresence(event) => vec![event.role.clone()],
        onlyne_proto::Event::SessionState(event) => vec![event.role.clone()],
        onlyne_proto::Event::LedgerState(event) => {
            let mut roles = Vec::new();
            if let Some(role) = role_name(&event.from) {
                roles.push(role.to_string());
            }
            if let Some(role) = role_name(&event.to) {
                roles.push(role.to_string());
            }
            roles
        }
        onlyne_proto::Event::Fault(event) => event.role.clone().into_iter().collect(),
        onlyne_proto::Event::GatewayPresence { gateway, .. } => vec![gateway.clone()],
        onlyne_proto::Event::SpecReloaded(_) => Vec::new(),
    }
}

pub fn event_edge(row: &EventRow) -> Option<(String, String)> {
    match &row.event {
        onlyne_proto::Event::LedgerState(event) => Some((
            role_name(&event.from)?.to_string(),
            role_name(&event.to)?.to_string(),
        )),
        _ => None,
    }
}

pub fn role_name(principal: &Principal) -> Option<&str> {
    match principal {
        Principal::Role { role, .. } => Some(role.as_str()),
        Principal::Gateway { gateway, .. } => Some(gateway.as_str()),
        Principal::Cluster { cluster } => Some(cluster.as_str()),
    }
}

pub fn layout_nodes(snapshot: &Snapshot, active_only: bool) -> Vec<LayoutNode> {
    let mut sessions_by_role: BTreeMap<String, Vec<&SessionRow>> = BTreeMap::new();
    for session in visible_sessions(snapshot, active_only) {
        if let Some(role) = &session.role {
            sessions_by_role
                .entry(role.clone())
                .or_default()
                .push(session);
        }
    }
    let mut nodes = Vec::new();
    for role in &snapshot.roles {
        let sessions = sessions_by_role.remove(&role.name).unwrap_or_default();
        let busy = sessions.iter().any(|session| session_busy(session));
        nodes.push(LayoutNode {
            name: role.name.clone(),
            title: role.name.clone(),
            presence: match role.state {
                Presence::Online => LayoutPresence::Online,
                Presence::Offline => LayoutPresence::Offline,
                Presence::Draining => LayoutPresence::Draining,
            },
            sessions: sessions
                .into_iter()
                .map(|session| SessionLine {
                    task: session.task_id.clone(),
                    state: match session.public_lifecycle {
                        Lifecycle::Created => SessionState::Created,
                        Lifecycle::Working => SessionState::Working,
                        Lifecycle::Idle => SessionState::Idle,
                        Lifecycle::Exited => SessionState::Exited,
                    },
                    age: age_from(session.updated_at.as_deref()),
                })
                .collect(),
            aggregate: role.aggregate.clone(),
            busy,
        });
    }
    nodes
}

pub fn layout_edges(snapshot: &Snapshot) -> Vec<LayoutEdge> {
    let roles = snapshot
        .roles
        .iter()
        .map(|role| role.name.clone())
        .collect::<BTreeSet<_>>();
    let active = snapshot
        .ledger
        .iter()
        .filter(|entry| entry.state == LedgerState::InFlight)
        .filter_map(|entry| {
            Some((
                role_name(&entry.from)?.to_string(),
                role_name(&entry.to)?.to_string(),
            ))
        })
        .collect::<BTreeSet<_>>();
    let mut edges = BTreeMap::<(String, String), bool>::new();
    for role in &snapshot.roles {
        for target in &role.edges {
            if role.name != *target && roles.contains(target) {
                let key = (role.name.clone(), target.clone());
                edges.insert(key.clone(), active.contains(&key));
            }
        }
    }
    for key in active {
        if key.0 != key.1 && roles.contains(&key.0) && roles.contains(&key.1) {
            edges.insert(key, true);
        }
    }
    edges
        .into_iter()
        .map(|((from, to), in_flight)| LayoutEdge {
            from,
            to,
            in_flight,
        })
        .collect()
}

/// Whether the session still holds its role's slot. Lifecycle owns this: an
/// exited session is done even when the row's last projection still says the
/// agent was running, because the projection's agent phase stops at the last
/// heartbeat and the exit lands after it. The agent phase stays a display
/// column in the swarm table and the detail panes.
pub fn session_busy(session: &SessionRow) -> bool {
    !matches!(session.public_lifecycle, Lifecycle::Exited)
}

pub fn active_sessions(snapshot: &Snapshot) -> Vec<&SessionRow> {
    visible_sessions(snapshot, true)
}

/// The sessions a view lists: every row, or only the ones still holding a
/// slot while `active_only`.
pub fn visible_sessions(snapshot: &Snapshot, active_only: bool) -> Vec<&SessionRow> {
    snapshot
        .sessions
        .iter()
        .filter(|session| !active_only || session_busy(session))
        .collect()
}
/// A control-plane role: the supervisor and every aggregate role. Its box
/// takes a row of its own below the chain, and its spoke edges stay off the
/// map until `e`.
pub fn control_role(name: &str, aggregate: Option<&str>) -> bool {
    name.starts_with('_') || aggregate.is_some()
}

/// One role waiting for a place in the page-1 map.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleSlot {
    pub name: String,
    pub control: bool,
}

/// The top-left corner a role's box takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RolePlace {
    pub x: usize,
    pub y: usize,
}

/// Whether any two of these box corners touch, given a `w`x`h` box.
pub fn boxes_overlap(places: &[RolePlace], w: usize, h: usize) -> bool {
    places.iter().enumerate().any(|(index, a)| {
        places[index + 1..]
            .iter()
            .any(|b| a.x < b.x + w && b.x < a.x + w && a.y < b.y + h && b.y < a.y + h)
    })
}

/// Fixed margin around the layered role map.
pub const PLACE_MARGIN: usize = 1;
pub const MIN_SPACING: usize = 1;
pub const MAX_SPACING: usize = 4;
pub const DEFAULT_SPACING: usize = 2;

pub fn clamp_spacing(spacing: usize) -> usize {
    spacing.clamp(MIN_SPACING, MAX_SPACING)
}

/// The gap between role columns and between row bands.
pub fn spacing_gap(spacing: usize) -> usize {
    2 * clamp_spacing(spacing)
}

/// Where each slot's box goes in a left-to-right layered role graph.
///
/// Visible edges define ranks; control roles stay in rank 0, and isolated
/// non-control roles stack in the column after the rightmost connected rank.
pub fn role_positions(
    slots: &[RoleSlot],
    edges: &[LayoutEdge],
    _w: usize,
    node_w: usize,
    node_h: usize,
    spacing: usize,
) -> Vec<(String, RolePlace)> {
    if slots.is_empty() || node_w == 0 || node_h == 0 {
        return Vec::new();
    }
    let LayerPlan {
        ranks,
        back_edges: _back_edges,
    } = layer_plan(slots, edges);
    let mut by_rank = BTreeMap::<usize, Vec<String>>::new();
    for slot in slots {
        let rank = ranks.get(&slot.name).copied().unwrap_or(0);
        by_rank.entry(rank).or_default().push(slot.name.clone());
    }
    for names in by_rank.values_mut() {
        names.sort();
    }
    let mut by_name = BTreeMap::new();
    for (rank, names) in by_rank {
        for (row, name) in names.into_iter().enumerate() {
            by_name.insert(name, cell(row, rank, node_w, node_h, spacing));
        }
    }
    slots
        .iter()
        .map(|slot| {
            (
                slot.name.clone(),
                by_name
                    .get(&slot.name)
                    .copied()
                    .unwrap_or(RolePlace { x: 0, y: 0 }),
            )
        })
        .collect()
}

#[derive(Clone, Debug, Default)]
struct LayerPlan {
    ranks: BTreeMap<String, usize>,
    back_edges: BTreeSet<(String, String)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Visit {
    Visiting,
    Done,
}

fn layer_plan(slots: &[RoleSlot], edges: &[LayoutEdge]) -> LayerPlan {
    let slot_names = slots
        .iter()
        .map(|slot| slot.name.clone())
        .collect::<BTreeSet<_>>();
    let controls = slots
        .iter()
        .filter(|slot| slot.control)
        .map(|slot| slot.name.clone())
        .collect::<BTreeSet<_>>();
    let mut adjacency = slot_names
        .iter()
        .map(|name| (name.clone(), BTreeSet::<String>::new()))
        .collect::<BTreeMap<_, _>>();
    let mut incoming = adjacency.clone();
    let mut weak = adjacency.clone();
    let mut incident = BTreeSet::new();
    for edge in edges {
        if edge.from == edge.to
            || !slot_names.contains(&edge.from)
            || !slot_names.contains(&edge.to)
        {
            continue;
        }
        adjacency
            .entry(edge.from.clone())
            .or_default()
            .insert(edge.to.clone());
        incoming
            .entry(edge.to.clone())
            .or_default()
            .insert(edge.from.clone());
        weak.entry(edge.from.clone())
            .or_default()
            .insert(edge.to.clone());
        weak.entry(edge.to.clone())
            .or_default()
            .insert(edge.from.clone());
        incident.insert(edge.from.clone());
        incident.insert(edge.to.clone());
    }

    let graph_nodes = slot_names
        .iter()
        .filter(|name| controls.contains(*name) || incident.contains(*name))
        .cloned()
        .collect::<BTreeSet<_>>();
    let isolated = slot_names
        .iter()
        .filter(|name| !controls.contains(*name) && !incident.contains(*name))
        .cloned()
        .collect::<Vec<_>>();

    let components = weak_components(&graph_nodes, &weak);
    let mut ranks = BTreeMap::new();
    let mut back_edges = BTreeSet::new();
    for component in components {
        let roots = component_roots(&component, &incoming, &controls);
        mark_component_back_edges(&component, &roots, &adjacency, &controls, &mut back_edges);
        rank_component(&component, &adjacency, &controls, &back_edges, &mut ranks);
    }

    let isolated_rank = ranks.values().max().map(|rank| rank + 1).unwrap_or(0);
    for name in isolated {
        ranks.insert(name, isolated_rank);
    }

    LayerPlan { ranks, back_edges }
}

fn weak_components(
    nodes: &BTreeSet<String>,
    weak: &BTreeMap<String, BTreeSet<String>>,
) -> Vec<BTreeSet<String>> {
    let mut unseen = nodes.clone();
    let mut components = Vec::new();
    while let Some(root) = unseen.iter().next().cloned() {
        unseen.remove(&root);
        let mut component = BTreeSet::new();
        let mut queue = VecDeque::from([root]);
        while let Some(name) = queue.pop_front() {
            if !component.insert(name.clone()) {
                continue;
            }
            if let Some(neighbours) = weak.get(&name) {
                for neighbour in neighbours {
                    if unseen.remove(neighbour) {
                        queue.push_back(neighbour.clone());
                    }
                }
            }
        }
        components.push(component);
    }
    components
}

fn component_roots(
    component: &BTreeSet<String>,
    incoming: &BTreeMap<String, BTreeSet<String>>,
    controls: &BTreeSet<String>,
) -> Vec<String> {
    let mut roots = component
        .iter()
        .filter(|name| {
            controls.contains(*name)
                || incoming
                    .get(*name)
                    .map(|from| from.iter().all(|source| !component.contains(source)))
                    .unwrap_or(true)
        })
        .cloned()
        .collect::<Vec<_>>();
    if roots.is_empty() {
        if let Some(name) = component.iter().next() {
            roots.push(name.clone());
        }
    }
    roots.sort();
    roots
}

fn mark_component_back_edges(
    component: &BTreeSet<String>,
    roots: &[String],
    adjacency: &BTreeMap<String, BTreeSet<String>>,
    controls: &BTreeSet<String>,
    back_edges: &mut BTreeSet<(String, String)>,
) {
    let mut state = BTreeMap::new();
    for root in roots {
        dfs_back_edges(root, component, adjacency, controls, &mut state, back_edges);
    }
    for name in component {
        if !state.contains_key(name) {
            dfs_back_edges(name, component, adjacency, controls, &mut state, back_edges);
        }
    }
}

fn dfs_back_edges(
    name: &str,
    component: &BTreeSet<String>,
    adjacency: &BTreeMap<String, BTreeSet<String>>,
    controls: &BTreeSet<String>,
    state: &mut BTreeMap<String, Visit>,
    back_edges: &mut BTreeSet<(String, String)>,
) {
    if state.get(name) == Some(&Visit::Done) {
        return;
    }
    state.insert(name.to_string(), Visit::Visiting);
    if let Some(targets) = adjacency.get(name) {
        for target in targets {
            if !component.contains(target) {
                continue;
            }
            if controls.contains(target) {
                back_edges.insert((name.to_string(), target.clone()));
                continue;
            }
            match state.get(target) {
                Some(Visit::Visiting) => {
                    back_edges.insert((name.to_string(), target.clone()));
                }
                Some(Visit::Done) => {}
                None => dfs_back_edges(target, component, adjacency, controls, state, back_edges),
            }
        }
    }
    state.insert(name.to_string(), Visit::Done);
}

fn rank_component(
    component: &BTreeSet<String>,
    adjacency: &BTreeMap<String, BTreeSet<String>>,
    controls: &BTreeSet<String>,
    back_edges: &BTreeSet<(String, String)>,
    ranks: &mut BTreeMap<String, usize>,
) {
    let mut indegree = component
        .iter()
        .map(|name| (name.clone(), 0usize))
        .collect::<BTreeMap<_, _>>();
    for from in component {
        for target in adjacency.get(from).into_iter().flatten() {
            if component.contains(target)
                && !controls.contains(target)
                && !back_edges.contains(&(from.clone(), target.clone()))
            {
                *indegree.entry(target.clone()).or_default() += 1;
            }
        }
    }
    let mut ready = indegree
        .iter()
        .filter(|(_, count)| **count == 0)
        .map(|(name, _)| name.clone())
        .collect::<BTreeSet<_>>();
    for control in controls.iter().filter(|name| component.contains(*name)) {
        ranks.insert(control.clone(), 0);
        ready.insert(control.clone());
    }

    while let Some(name) = ready.pop_first() {
        let rank = *ranks.entry(name.clone()).or_insert(0);
        for target in adjacency.get(&name).into_iter().flatten() {
            if !component.contains(target)
                || controls.contains(target)
                || back_edges.contains(&(name.clone(), target.clone()))
            {
                continue;
            }
            let candidate = rank + 1;
            ranks
                .entry(target.clone())
                .and_modify(|rank| *rank = (*rank).max(candidate))
                .or_insert(candidate);
            if let Some(count) = indegree.get_mut(target) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    ready.insert(target.clone());
                }
            }
        }
    }

    for name in component {
        ranks.entry(name.clone()).or_insert(0);
    }
}

/// The top-left corner of the box at a rank and vertical slot.
fn cell(row: usize, rank: usize, node_w: usize, node_h: usize, spacing: usize) -> RolePlace {
    let gap = spacing_gap(spacing);
    RolePlace {
        x: PLACE_MARGIN + rank * (node_w + gap),
        y: row * (node_h + gap),
    }
}

/// The edges the map draws: every ACL target, minus the control-plane spokes
/// while they are hidden.
pub fn visible_edges(snapshot: &Snapshot, show_control_edges: bool) -> Vec<LayoutEdge> {
    layout_edges(snapshot)
        .into_iter()
        .filter(|edge| show_control_edges || !source_is_control(snapshot, &edge.from))
        .collect()
}

fn source_is_control(snapshot: &Snapshot, name: &str) -> bool {
    snapshot
        .roles
        .iter()
        .any(|role| role.name == name && role.control())
}

/// The role the page-1 cursor sits on: the operator's pick while it is still
/// registered, else the first role with an out-edge to walk, else the first
/// role.
pub fn selected_role(snapshot: &Snapshot, state: &UiState) -> Option<String> {
    if let Some(name) = &state.role_selected {
        if snapshot.roles.iter().any(|role| &role.name == name) {
            return Some(name.clone());
        }
    }
    let edges = visible_edges(snapshot, state.show_control_edges);
    snapshot
        .roles
        .iter()
        .find(|role| edges.iter().any(|edge| edge.from == role.name))
        .or_else(|| snapshot.roles.first())
        .map(|role| role.name.clone())
}

/// The out-edges `j`/`k` walks for one role, in map order.
pub fn role_edges(snapshot: &Snapshot, state: &UiState, role: &str) -> Vec<LayoutEdge> {
    let mut edges: Vec<LayoutEdge> = visible_edges(snapshot, state.show_control_edges)
        .into_iter()
        .filter(|edge| edge.from == role)
        .collect();
    edges.sort_by(|a, b| a.to.cmp(&b.to));
    edges
}

pub fn cycle_state(filter: &mut HistoryFilter) {
    let current = STATES
        .iter()
        .position(|state| *state == filter.state)
        .unwrap_or(0);
    filter.state = STATES[(current + 1) % STATES.len()].to_string();
    filter.reset_page();
}

pub fn cycle_role(roles: &[RoleView], filter: &mut HistoryFilter) {
    let choices = roles
        .iter()
        .map(|role| role.name.clone())
        .collect::<Vec<_>>();
    filter.role = cycle_option(filter.role.take(), &choices);
    filter.reset_page();
}

pub fn cycle_edge(snapshot: &Snapshot, filter: &mut HistoryFilter) {
    let choices = layout_edges(snapshot)
        .into_iter()
        .map(|edge| EdgeFilter {
            from: edge.from,
            to: edge.to,
        })
        .collect::<Vec<_>>();
    let current = filter.edge.take();
    filter.edge = match current {
        None => choices.first().cloned(),
        Some(value) => choices
            .iter()
            .position(|edge| edge == &value)
            .and_then(|index| choices.get(index + 1).cloned()),
    };
    filter.reset_page();
}

fn cycle_option(current: Option<String>, choices: &[String]) -> Option<String> {
    match current {
        None => choices.first().cloned(),
        Some(value) => choices
            .iter()
            .position(|choice| choice == &value)
            .and_then(|index| choices.get(index + 1).cloned()),
    }
}

pub fn page_history(delta: isize, filter: &mut HistoryFilter, total: usize, page_size: usize) {
    if delta > 0 {
        if filter.offset + page_size < total {
            filter.offset += page_size;
        }
    } else {
        filter.offset = filter.offset.saturating_sub(page_size);
    }
}

pub fn selected_graph_task(snapshot: &Snapshot, index: usize, active_only: bool) -> Option<String> {
    visible_sessions(snapshot, active_only)
        .get(index)
        .map(|session| session.task_id.clone())
}

pub fn selected_history_task(snapshot: &Snapshot, index: usize) -> Option<String> {
    snapshot.history.get(index).and_then(event_task)
}

pub fn event_task(row: &EventRow) -> Option<String> {
    match &row.event {
        onlyne_proto::Event::SessionState(event) => Some(event.task_id.clone()),
        onlyne_proto::Event::LedgerState(event) => event.task.clone(),
        onlyne_proto::Event::Fault(event) => event.task_id.clone(),
        onlyne_proto::Event::RolePresence(_)
        | onlyne_proto::Event::GatewayPresence { .. }
        | onlyne_proto::Event::SpecReloaded(_) => None,
    }
}

pub fn age_from(updated_at: Option<&str>) -> String {
    let Some(raw) = updated_at else {
        return "--".to_string();
    };
    let then = raw
        .parse::<i64>()
        .ok()
        .and_then(|epoch| DateTime::<Utc>::from_timestamp(epoch, 0))
        .or_else(|| {
            DateTime::parse_from_rfc3339(raw)
                .ok()
                .map(|dt| dt.with_timezone(&Utc))
        });
    let Some(then) = then else {
        return "--".to_string();
    };
    let secs = (Utc::now() - then).num_seconds().max(0) as u64;
    format_age(secs)
}

pub fn format_age(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

pub fn principal_label(principal: &Principal) -> String {
    match principal {
        Principal::Role { role, session } => session
            .as_ref()
            .map(|session| format!("{role}/{session}"))
            .unwrap_or_else(|| role.clone()),
        Principal::Gateway {
            gateway,
            channel,
            conversation,
        } => conversation
            .as_ref()
            .map(|conversation| format!("{gateway}:{channel}/{conversation}"))
            .unwrap_or_else(|| format!("{gateway}:{channel}")),
        Principal::Cluster { cluster } => format!("⬡{cluster}"),
    }
}

pub fn ledger_state_label(entry: &LedgerEntry) -> String {
    match entry.kind {
        MsgKind::Task => match entry.state {
            LedgerState::Queued => "pending".to_string(),
            LedgerState::InFlight => "running".to_string(),
            LedgerState::Acked => "done".to_string(),
            LedgerState::Rejected => "failed".to_string(),
            LedgerState::Expired => "closed".to_string(),
        },
        _ => entry.state.as_str().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use onlyne_proto::{AgentPhase, SessionProjection};

    #[test]
    fn role_view_tolerates_missing_future_fields() {
        let value = json!({
            "name": "builder",
            "admin": false,
            "max_sessions": 2,
            "spec_hash": "abc",
            "state": "online",
            "sessions": 1
        });
        let role = RoleView::try_from(value).unwrap();
        assert_eq!(role.name, "builder");
        assert!(role.edges.is_empty());
        assert_eq!(role.aggregate, None);
    }

    #[test]
    fn role_view_reads_edges_and_aggregate_when_present() {
        let value = json!({
            "name": "router",
            "admin": true,
            "max_sessions": 1,
            "spec_hash": "abc",
            "state": "draining",
            "sessions": 0,
            "edges": ["builder", "reviewer"],
            "aggregate": "cluster-x"
        });
        let role = RoleView::try_from(value).unwrap();
        assert_eq!(role.edges, vec!["builder", "reviewer"]);
        assert_eq!(role.aggregate.as_deref(), Some("cluster-x"));
    }

    #[test]
    fn alerts_list_open_faults_then_status_notices() {
        let snapshot = Snapshot {
            status: json!({"alerts": ["spec reloaded"]}),
            faults: vec![
                FaultEvent {
                    id: 1,
                    kind: "intent_exhausted".into(),
                    reason: "no accepted intent".into(),
                    role: Some("builder".into()),
                    ..FaultEvent::default()
                },
                FaultEvent {
                    id: 2,
                    kind: "gateway_unconfigured".into(),
                    reason: "old".into(),
                    state: Some("acked".into()),
                    ..FaultEvent::default()
                },
            ],
            ..Snapshot::default()
        };
        let rows = alerts(&snapshot);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(rows[0].kind, AlertKind::Alert);
        assert!(rows[0].text.contains("intent_exhausted") && rows[0].text.contains("builder"));
        assert_eq!(rows[1].kind, AlertKind::Notice);
        assert_eq!(rows[1].text, "spec reloaded");
    }

    fn state_session(lifecycle: Lifecycle, agent: AgentPhase) -> SessionRow {
        SessionRow {
            task_id: "t1".into(),
            role: Some("builder".into()),
            session_id: "s1".into(),
            generation: 1,
            seq: 1,
            public_lifecycle: lifecycle,
            projection: SessionProjection {
                lifecycle,
                agent,
                ..SessionProjection::default()
            },
            outcome: None,
            updated_at: None,
        }
    }

    #[test]
    fn an_exited_session_is_not_busy_though_its_projection_says_running() {
        let exited = state_session(Lifecycle::Exited, AgentPhase::Running);
        assert!(!session_busy(&exited));
        let working = state_session(Lifecycle::Working, AgentPhase::Running);
        assert!(session_busy(&working));
    }

    #[test]
    fn the_active_only_view_keeps_the_exited_rows_out() {
        let snapshot = Snapshot {
            sessions: vec![
                state_session(Lifecycle::Exited, AgentPhase::Running),
                state_session(Lifecycle::Working, AgentPhase::Running),
            ],
            roles: vec![role_view("builder")],
            ..Snapshot::default()
        };
        assert_eq!(visible_sessions(&snapshot, true).len(), 1);
        assert_eq!(visible_sessions(&snapshot, false).len(), 2);
        assert_eq!(layout_nodes(&snapshot, true)[0].sessions.len(), 1);
        assert_eq!(layout_nodes(&snapshot, false)[0].sessions.len(), 2);
    }

    #[test]
    fn active_sessions_leaves_the_exited_row_out() {
        let snapshot = Snapshot {
            sessions: vec![
                state_session(Lifecycle::Exited, AgentPhase::Running),
                state_session(Lifecycle::Working, AgentPhase::Running),
            ],
            ..Snapshot::default()
        };
        let active = active_sessions(&snapshot);
        assert_eq!(active.len(), 1, "{active:?}");
        assert_eq!(active[0].public_lifecycle, Lifecycle::Working);
    }

    fn slots(names: &[(&str, bool)]) -> Vec<RoleSlot> {
        names
            .iter()
            .map(|(name, control)| RoleSlot {
                name: (*name).to_string(),
                control: *control,
            })
            .collect()
    }

    fn edge(from: &str, to: &str) -> LayoutEdge {
        LayoutEdge {
            from: from.into(),
            to: to.into(),
            in_flight: false,
        }
    }

    fn placed_at(placed: &[(String, RolePlace)], name: &str) -> RolePlace {
        placed
            .iter()
            .find(|(slot, _)| slot == name)
            .map(|(_, place)| *place)
            .unwrap_or_else(|| panic!("no {name}"))
    }

    #[test]
    fn chain_roles_rank_left_to_right() {
        let chain = slots(&[
            ("a", false),
            ("b", false),
            ("c", false),
            ("d", false),
            ("e", false),
        ]);
        let edges = vec![
            edge("a", "b"),
            edge("b", "c"),
            edge("c", "d"),
            edge("d", "e"),
        ];
        let placed = role_positions(&chain, &edges, 120, 14, 7, DEFAULT_SPACING);
        let points: Vec<RolePlace> = placed.iter().map(|(_, place)| *place).collect();
        assert!(!boxes_overlap(&points, 14, 7), "{points:?}");
        let xs = ["a", "b", "c", "d", "e"].map(|name| placed_at(&placed, name).x);
        assert!(xs.windows(2).all(|pair| pair[0] < pair[1]), "{xs:?}");
        let step = 14 + spacing_gap(DEFAULT_SPACING);
        assert_eq!(
            xs,
            [
                PLACE_MARGIN,
                PLACE_MARGIN + step,
                PLACE_MARGIN + 2 * step,
                PLACE_MARGIN + 3 * step,
                PLACE_MARGIN + 4 * step
            ]
        );
    }

    #[test]
    fn cycle_keeps_one_back_edge_and_forward_ranks_monotonic() {
        let ring = slots(&[("a", false), ("b", false), ("c", false)]);
        let edges = vec![edge("a", "b"), edge("b", "c"), edge("c", "a")];
        let plan = layer_plan(&ring, &edges);
        assert_eq!(plan.back_edges.len(), 1, "{:?}", plan.back_edges);
        assert!(
            plan.back_edges
                .contains(&("c".to_string(), "a".to_string())),
            "{:?}",
            plan.back_edges
        );

        let placed = role_positions(&ring, &edges, 120, 14, 7, DEFAULT_SPACING);
        let ax = placed_at(&placed, "a").x;
        let bx = placed_at(&placed, "b").x;
        let cx = placed_at(&placed, "c").x;
        assert!(ax < bx && bx < cx, "{placed:?}");
    }

    #[test]
    fn diamond_branches_share_the_middle_rank() {
        let diamond = slots(&[("a", false), ("b", false), ("c", false), ("d", false)]);
        let edges = vec![
            edge("a", "b"),
            edge("a", "c"),
            edge("b", "d"),
            edge("c", "d"),
        ];
        let placed = role_positions(&diamond, &edges, 120, 14, 7, DEFAULT_SPACING);
        let b = placed_at(&placed, "b");
        let c = placed_at(&placed, "c");
        assert_eq!(b.x, c.x, "{placed:?}");
        assert_ne!(b.y, c.y, "branches stack instead of colliding\n{placed:?}");
        assert!(placed_at(&placed, "a").x < b.x, "{placed:?}");
        assert!(b.x < placed_at(&placed, "d").x, "{placed:?}");
    }

    #[test]
    fn spacing_expands_columns_and_rows_without_overlap() {
        let graph = slots(&[("a", false), ("b", false), ("c", false)]);
        let edges = vec![edge("a", "c"), edge("b", "c")];
        let compact = role_positions(&graph, &edges, 120, 14, 7, 1);
        let wide = role_positions(&graph, &edges, 120, 14, 7, 4);
        let compact_points: Vec<RolePlace> = compact.iter().map(|(_, place)| *place).collect();
        let wide_points: Vec<RolePlace> = wide.iter().map(|(_, place)| *place).collect();
        assert!(!boxes_overlap(&compact_points, 14, 7), "{compact_points:?}");
        assert!(!boxes_overlap(&wide_points, 14, 7), "{wide_points:?}");
        assert_eq!(
            placed_at(&compact, "c").x - placed_at(&compact, "a").x,
            14 + spacing_gap(1)
        );
        assert_eq!(
            placed_at(&wide, "c").x - placed_at(&wide, "a").x,
            14 + spacing_gap(4)
        );
        assert_eq!(
            placed_at(&compact, "b").y - placed_at(&compact, "a").y,
            7 + spacing_gap(1)
        );
        assert_eq!(
            placed_at(&wide, "b").y - placed_at(&wide, "a").y,
            7 + spacing_gap(4)
        );
    }

    #[test]
    fn isolated_roles_stack_in_the_far_right_column() {
        let graph = slots(&[("a", false), ("b", false), ("z", false)]);
        let edges = vec![edge("a", "b")];
        let placed = role_positions(&graph, &edges, 120, 14, 7, DEFAULT_SPACING);
        assert!(
            placed_at(&placed, "z").x > placed_at(&placed, "b").x,
            "{placed:?}"
        );
        assert_eq!(placed_at(&placed, "z").y, 0, "{placed:?}");
    }

    #[test]
    fn control_roles_hold_rank_zero_even_with_incoming_edges() {
        let graph = slots(&[("a", false), ("b", false), ("_supervisor", true)]);
        let edges = vec![edge("a", "b"), edge("b", "_supervisor")];
        let placed = role_positions(&graph, &edges, 120, 14, 7, DEFAULT_SPACING);
        assert_eq!(placed_at(&placed, "a").x, PLACE_MARGIN, "{placed:?}");
        assert_eq!(
            placed_at(&placed, "_supervisor").x,
            PLACE_MARGIN,
            "{placed:?}"
        );
        assert!(placed_at(&placed, "b").x > PLACE_MARGIN, "{placed:?}");
    }

    #[test]
    fn placements_are_deterministic() {
        let all = slots(&[
            ("a", false),
            ("b", false),
            ("c", false),
            ("_supervisor", true),
        ]);
        let edges = vec![edge("a", "b"), edge("b", "c"), edge("c", "a")];
        assert_eq!(
            role_positions(&all, &edges, 120, 14, 7, DEFAULT_SPACING),
            role_positions(&all, &edges, 120, 14, 7, DEFAULT_SPACING)
        );
    }

    #[test]
    fn control_edges_stay_off_the_map_until_told() {
        let snapshot = Snapshot {
            roles: vec![
                RoleView {
                    name: "_supervisor".into(),
                    aggregate: None,
                    edges: vec!["builder".into()],
                    ..role_view("_supervisor")
                },
                RoleView {
                    edges: vec!["_supervisor".into()],
                    ..role_view("builder")
                },
            ],
            ..Snapshot::default()
        };
        let hidden = visible_edges(&snapshot, false);
        assert_eq!(hidden.len(), 1, "{hidden:?}");
        assert_eq!(hidden[0].from, "builder");
        let shown = visible_edges(&snapshot, true);
        assert_eq!(shown.len(), 2, "{shown:?}");
    }

    fn role_view(name: &str) -> RoleView {
        RoleView {
            name: name.into(),
            admin: false,
            max_sessions: 1,
            spec_hash: "a".into(),
            prose: None,
            state: Presence::Online,
            session_count: 0,
            detail: None,
            edges: Vec::new(),
            aggregate: None,
        }
    }
}
