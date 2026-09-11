use crate::layout::{
    LayoutEdge, LayoutNode, Presence as LayoutPresence, SessionLine, SessionState,
};
use chrono::{DateTime, Utc};
use onlyne_proto::{
    AdminFrame, AdminOp, AgentPhase, EventRow, FaultEvent, Frame, LedgerEntry, LedgerQuery,
    LedgerState, Lifecycle, MsgKind, Presence, Principal, QueryFaultsArgs, QueryRolesArgs,
    QuerySessionsArgs, ResBody, RoleInfo, SessionRow, new_id,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
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

/// Decode one `roles` row. A row from a server that predates `edges` or
/// `aggregate` still lands: both default, and the page then draws no arrow.
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
    pub detail_task_id: Option<String>,
    pub detail: Option<TaskDetail>,
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
            detail_task_id: None,
            detail: None,
            detail_scroll: 0,
            search: None,
            message: String::new(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct TaskDetail {
    pub task_id: String,
    pub ledger: Vec<LedgerEntry>,
    pub sessions: Vec<SessionRow>,
    pub faults: Vec<FaultEvent>,
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
            onlyne_proto::Event::SessionState(event) => {
                !matches!(event.projection.lifecycle, Lifecycle::Exited)
                    || matches!(
                        event.projection.agent,
                        AgentPhase::Ready | AgentPhase::Running
                    )
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

pub fn layout_nodes(snapshot: &Snapshot) -> Vec<LayoutNode> {
    let mut sessions_by_role: BTreeMap<String, Vec<&SessionRow>> = BTreeMap::new();
    for session in &snapshot.sessions {
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

pub fn session_busy(session: &SessionRow) -> bool {
    !matches!(session.public_lifecycle, Lifecycle::Exited)
        || matches!(
            session.projection.agent,
            AgentPhase::Ready | AgentPhase::Running
        )
}

pub fn active_sessions(snapshot: &Snapshot) -> Vec<&SessionRow> {
    snapshot
        .sessions
        .iter()
        .filter(|session| session_busy(session))
        .collect()
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

pub fn selected_graph_task(snapshot: &Snapshot, index: usize) -> Option<String> {
    active_sessions(snapshot)
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
}
