use crate::layout::{self, CellKind};
use crate::model::{
    Alert, AlertKind, Focus, Page, Snapshot, TaskDetail, UiState, active_sessions, alerts,
    event_task, layout_edges, layout_nodes, ledger_state_label, page_history, principal_label,
    selected_graph_task, selected_history_task,
};
use chrono::{DateTime, Local};
use onlyne_proto::{Event, EventRow, LedgerState, Lifecycle, SessionRow};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, Wrap};

pub fn render(frame: &mut Frame, snapshot: &Snapshot, state: &UiState) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(8),
            Constraint::Length(2),
        ])
        .split(frame.area());
    render_top(frame, root[0], snapshot, state);
    match state.page {
        Page::RoleMap => render_role_map(frame, root[1], snapshot),
        Page::Swarm => render_swarm(frame, root[1], snapshot, state),
    }
    render_bottom(frame, root[2], snapshot, state);
}

/// Render one frame into an in-memory backend and flatten it to plain text.
///
/// `--once` and the render tests share this path, so the text they assert on is
/// the same text an operator sees in the alternate screen.
pub fn render_once_text(snapshot: &Snapshot, state: &UiState, width: u16, height: u16) -> String {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).expect("test backend");
    terminal
        .draw(|frame| render(frame, snapshot, state))
        .expect("render once");
    buffer_text(terminal.backend().buffer())
}

/// One line per buffer row, right-trimmed, with wide glyphs collapsed to their
/// first symbol so a box border never doubles.
fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
    let area = *buffer.area();
    (0..area.height)
        .map(|y| {
            let mut line = String::new();
            for x in 0..area.width {
                line.push_str(buffer[(x, y)].symbol());
            }
            line.trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_top(frame: &mut Frame, area: Rect, snapshot: &Snapshot, state: &UiState) {
    let status = if snapshot.server_online {
        Span::styled(
            "server online",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(
            "server down",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )
    };
    let title = Line::from(vec![
        Span::styled(
            " onlyne ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("page "),
        Span::styled(state.page.label(), Style::default().fg(Color::Yellow)),
        Span::raw("  "),
        status,
        Span::raw("  "),
        Span::raw(snapshot.last_error.as_deref().unwrap_or("")),
    ]);
    frame.render_widget(Paragraph::new(title), area);
}

fn render_role_map(frame: &mut Frame, area: Rect, snapshot: &Snapshot) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(6), Constraint::Length(1)])
        .split(area);
    let nodes = layout_nodes(snapshot);
    let edges = layout_edges(snapshot);
    let canvas = layout::layout(&nodes, &edges, chunks[0].width, chunks[0].height);
    let lines = canvas
        .cells
        .iter()
        .map(|row| {
            Line::from(
                row.iter()
                    .map(|cell| Span::styled(cell.ch.to_string(), style_for_cell(cell.kind)))
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .block(Block::default().title("role network").borders(Borders::ALL)),
        chunks[0],
    );
    frame.render_widget(
        Paragraph::new("1/2 or Tab page  ↑↓ select  Enter detail  r refresh  / search  f state  t window  o role  q quit")
            .style(Style::default().fg(Color::DarkGray)),
        chunks[1],
    );
}

fn style_for_cell(kind: CellKind) -> Style {
    match kind {
        CellKind::Plain => Style::default(),
        CellKind::Online => Style::default().fg(Color::Green),
        CellKind::Offline => Style::default().fg(Color::DarkGray),
        CellKind::Draining => Style::default().fg(Color::Yellow),
        CellKind::Busy => Style::default()
            .fg(Color::LightYellow)
            .add_modifier(Modifier::BOLD),
        CellKind::Edge => Style::default().fg(Color::Blue),
        CellKind::ActiveEdge => Style::default()
            .fg(Color::LightYellow)
            .add_modifier(Modifier::BOLD),
        CellKind::Aggregate => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    }
}

fn render_swarm(frame: &mut Frame, area: Rect, snapshot: &Snapshot, state: &UiState) {
    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(main[1]);
    let alerts = alerts(snapshot);
    let shown = alerts.len().min(2) as u16;
    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(4), Constraint::Length(shown)])
        .split(main[0]);
    render_swarm_graph(frame, left[0], snapshot, state);
    if shown > 0 {
        render_alerts(frame, left[1], &alerts[..shown as usize]);
    }
    render_history(frame, right[0], snapshot, state);
    render_detail(frame, right[1], state);
}

/// The alert strip the old swarm pane kept under its graph: open faults first,
/// then any notice the status answer carries.
fn render_alerts(frame: &mut Frame, area: Rect, alerts: &[Alert]) {
    let lines = alerts
        .iter()
        .map(|alert| {
            let style = match alert.kind {
                AlertKind::Alert => Style::default().fg(Color::Red),
                AlertKind::Notice => Style::default().fg(Color::Yellow),
            };
            Line::styled(format!("! {}", alert.text), style)
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(Text::from(lines)), area);
}

fn render_swarm_graph(frame: &mut Frame, area: Rect, snapshot: &Snapshot, state: &UiState) {
    let active = active_sessions(snapshot);
    let selected = if state.page == Page::Swarm && state.focus == Focus::Graph {
        state.graph_cursor
    } else {
        usize::MAX
    };
    let mut rows = Vec::new();
    for (idx, session) in active.iter().enumerate() {
        let style = if idx == selected {
            Style::default().add_modifier(Modifier::REVERSED)
        } else if session.outcome.is_some() {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default()
        };
        rows.push(
            Row::new(vec![
                Cell::from(role_of(session)),
                Cell::from(short(&session.task_id)),
                Cell::from(lifecycle_label(session.public_lifecycle)),
                Cell::from(agent_label(session)),
                Cell::from(inflight_route(snapshot, &session.task_id)),
            ])
            .style(style),
        );
    }
    if rows.is_empty() {
        rows.push(Row::new(vec![Cell::from("idle")]).style(Style::default().fg(Color::DarkGray)));
    }
    let title = if state.page == Page::Swarm && state.focus == Focus::Graph {
        "graph [focus]"
    } else {
        "graph"
    };
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(10),
                Constraint::Length(10),
                Constraint::Length(7),
                Constraint::Length(8),
                Constraint::Min(14),
            ],
        )
        .header(
            Row::new(["role", "task", "life", "agent", "in-flight"])
                .style(Style::default().add_modifier(Modifier::BOLD)),
        )
        .block(Block::default().title(title).borders(Borders::ALL)),
        area,
    );
}

/// The `from→to` hop an in-flight ledger row carries for one task.
fn inflight_route(snapshot: &Snapshot, task_id: &str) -> String {
    snapshot
        .ledger
        .iter()
        .find(|entry| {
            entry.task.as_deref() == Some(task_id) && entry.state == LedgerState::InFlight
        })
        .map(|entry| {
            format!(
                "{}→{}",
                principal_label(&entry.from),
                principal_label(&entry.to)
            )
        })
        .unwrap_or_default()
}

fn render_history(frame: &mut Frame, area: Rect, snapshot: &Snapshot, state: &UiState) {
    let title = format!(
        "history {}   {}/{}{}",
        state.filter.label(),
        snapshot.history.len(),
        snapshot.history_total,
        if state.page == Page::Swarm && state.focus == Focus::History {
            " [focus]"
        } else {
            ""
        }
    );
    let rows = snapshot
        .history
        .iter()
        .enumerate()
        .map(|(idx, row)| {
            let style = history_style(row, idx, state);
            Row::new(vec![
                Cell::from(row.created_at.format("%H:%M:%S").to_string()),
                Cell::from(event_kind(row)),
                Cell::from(event_route(row)),
                Cell::from(event_state(row)),
                Cell::from(event_task(row).map(|id| short(&id)).unwrap_or_default()),
            ])
            .style(style)
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(10),
                Constraint::Length(8),
                Constraint::Min(17),
                Constraint::Length(10),
                Constraint::Length(9),
            ],
        )
        .header(
            Row::new(["created", "kind", "from→to", "state", "task"])
                .style(Style::default().add_modifier(Modifier::BOLD)),
        )
        .block(Block::default().title(title).borders(Borders::ALL)),
        area,
    );
}

fn history_style(row: &EventRow, idx: usize, state: &UiState) -> Style {
    if state.page == Page::Swarm && state.focus == Focus::History && idx == state.history_cursor {
        return Style::default().add_modifier(Modifier::REVERSED);
    }
    match &row.event {
        Event::Fault(_) => Style::default().fg(Color::Red),
        Event::LedgerState(event)
            if matches!(event.state, LedgerState::Rejected | LedgerState::Expired) =>
        {
            Style::default().fg(Color::Yellow)
        }
        Event::LedgerState(event) if event.state == LedgerState::InFlight => {
            Style::default().fg(Color::Green)
        }
        _ => Style::default(),
    }
}

fn render_detail(frame: &mut Frame, area: Rect, state: &UiState) {
    let (title, body) = match &state.detail {
        Some(detail) => detail_text(detail),
        None => (
            "detail".to_string(),
            "(select a task to inspect ledger, sessions, and faults)".to_string(),
        ),
    };
    frame.render_widget(
        Paragraph::new(body)
            .wrap(Wrap { trim: false })
            .scroll((state.detail_scroll, 0))
            .block(Block::default().title(title).borders(Borders::ALL)),
        area,
    );
}

fn render_bottom(frame: &mut Frame, area: Rect, snapshot: &Snapshot, state: &UiState) {
    let hints = if let Some(search) = &state.search {
        format!("search: {search}  [Enter] apply  [Esc] discard")
    } else {
        format!(
            "[1/2 Tab] page  [g/h] focus  [↑↓/jk] move  [Enter] detail  [/] search  [f] state  [t] win  [o] role  [e] edge  [PgUp/PgDn] page  [q] quit  {}",
            state.message
        )
    };
    let cluster = snapshot
        .status
        .get("cluster")
        .and_then(|value| value.as_str())
        .unwrap_or("cluster?");
    let when = snapshot
        .refreshed_at
        .map(|time| DateTime::<Local>::from(time).format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "--:--:--".to_string());
    let (online, online_style) = if snapshot.server_online {
        ("online", Style::default().fg(Color::Green))
    } else {
        ("down", Style::default().fg(Color::Red))
    };
    let right = format!("{cluster} + server {online} + refresh {when}");
    // The status block keeps its own column so a narrow terminal clips the
    // hints, never the cluster name, the server state, or the refresh clock.
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(0),
            Constraint::Length((right.chars().count() as u16).min(area.width)),
        ])
        .split(area);
    frame.render_widget(
        Paragraph::new(hints).style(Style::default().fg(Color::DarkGray)),
        columns[0],
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(cluster.to_string(), Style::default().fg(Color::Cyan)),
            Span::styled(" + server ", Style::default().fg(Color::DarkGray)),
            Span::styled(online, online_style),
            Span::styled(
                format!(" + refresh {when}"),
                Style::default().fg(Color::DarkGray),
            ),
        ]))
        .alignment(Alignment::Right),
        columns[1],
    );
}

pub fn history_page_size(height: u16) -> usize {
    height
        .saturating_sub(7)
        .saturating_mul(58)
        .saturating_div(100)
        .max(1) as usize
}

pub fn graph_len(snapshot: &Snapshot) -> usize {
    active_sessions(snapshot).len()
}

pub fn history_len(snapshot: &Snapshot) -> usize {
    snapshot.history.len()
}

pub fn clamp_cursor(cursor: &mut usize, len: usize) {
    if len == 0 {
        *cursor = 0;
    } else if *cursor >= len {
        *cursor = len - 1;
    }
}

pub fn move_cursor(cursor: &mut usize, len: usize, delta: isize) {
    if len == 0 {
        *cursor = 0;
        return;
    }
    *cursor = (*cursor as isize + delta).clamp(0, len.saturating_sub(1) as isize) as usize;
}

pub fn selected_task(snapshot: &Snapshot, state: &UiState) -> Option<String> {
    match state.focus {
        Focus::Graph => selected_graph_task(snapshot, state.graph_cursor),
        Focus::History => selected_history_task(snapshot, state.history_cursor),
    }
}

pub fn apply_page_history(
    delta: isize,
    snapshot: &Snapshot,
    state: &mut UiState,
    page_size: usize,
) {
    page_history(delta, &mut state.filter, snapshot.history_total, page_size);
    state.history_cursor = 0;
}

pub fn detail_text(detail: &TaskDetail) -> (String, String) {
    let mut out = String::new();
    out.push_str("ledger\n");
    if detail.ledger.is_empty() {
        out.push_str("  (none)\n");
    }
    for entry in &detail.ledger {
        out.push_str(&format!(
            "  {} {}→{} {} att={} {}\n",
            short(&entry.msg_id),
            principal_label(&entry.from),
            principal_label(&entry.to),
            ledger_state_label(entry),
            entry.attempt,
            entry.out_head.as_deref().unwrap_or("")
        ));
    }
    out.push_str("\nsessions\n");
    if detail.sessions.is_empty() {
        out.push_str("  (none)\n");
    }
    for session in &detail.sessions {
        out.push_str(&format!(
            "  {} role={} life={} agent={:?} gen={} seq={} outcome={} updated={}\n",
            session.session_id,
            session.role.as_deref().unwrap_or("?"),
            lifecycle_label(session.public_lifecycle),
            session.projection.agent,
            session.generation,
            session.seq,
            session
                .outcome
                .map(|outcome| outcome.to_string())
                .unwrap_or_else(|| "-".into()),
            session.updated_at.as_deref().unwrap_or("-")
        ));
    }
    out.push_str("\nfaults\n");
    if detail.faults.is_empty() {
        out.push_str("  (none)\n");
    }
    for fault in &detail.faults {
        out.push_str(&format!(
            "  #{} {} role={} state={} {}\n",
            fault.id,
            fault.kind,
            fault.role.as_deref().unwrap_or("?"),
            fault.state.as_deref().unwrap_or("?"),
            fault.reason
        ));
    }
    (format!("task {}", short(&detail.task_id)), out)
}

fn role_of(session: &SessionRow) -> String {
    session.role.clone().unwrap_or_else(|| "?".to_string())
}

fn lifecycle_label(lifecycle: Lifecycle) -> &'static str {
    match lifecycle {
        Lifecycle::Created => "created",
        Lifecycle::Working => "working",
        Lifecycle::Idle => "idle",
        Lifecycle::Exited => "exited",
    }
}

fn agent_label(session: &SessionRow) -> String {
    format!("{:?}", session.projection.agent).to_lowercase()
}

/// The short event name the history table shows; the full wire type name lives
/// on the event itself.
fn event_kind(row: &EventRow) -> String {
    match &row.event {
        Event::RolePresence(_) => "role".into(),
        Event::SessionState(_) => "session".into(),
        Event::LedgerState(_) => "ledger".into(),
        Event::Fault(_) => "fault".into(),
        Event::GatewayPresence { .. } => "gateway".into(),
        Event::SpecReloaded(_) => "spec".into(),
    }
}

fn event_route(row: &EventRow) -> String {
    match &row.event {
        Event::LedgerState(event) => format!(
            "{}→{}",
            principal_label(&event.from),
            principal_label(&event.to)
        ),
        Event::SessionState(event) => event.role.clone(),
        Event::RolePresence(event) => event.role.clone(),
        Event::Fault(event) => event.role.clone().unwrap_or_default(),
        Event::GatewayPresence { gateway, .. } => gateway.clone(),
        Event::SpecReloaded(event) => format!("roles={}", event.roles),
    }
}

fn event_state(row: &EventRow) -> String {
    match &row.event {
        Event::LedgerState(event) => event.state.as_str().into(),
        Event::SessionState(event) => lifecycle_label(event.projection.lifecycle).into(),
        Event::RolePresence(event) => event.state.as_str().into(),
        Event::Fault(event) => event.state.clone().unwrap_or_else(|| "open".into()),
        Event::GatewayPresence { state, .. } => format!("{:?}", state).to_lowercase(),
        Event::SpecReloaded(_) => "ok".into(),
    }
}

fn short(value: &str) -> String {
    value.chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::RoleView;
    use chrono::Utc;
    use onlyne_proto::{
        AgentPhase, DeliveryPhase, FaultEvent, LedgerEntry, Lifecycle, MsgKind, Presence,
        Principal, RecoveryPhase, ResourcePhase, SessionProjection,
    };
    use std::time::SystemTime;

    fn projection(lifecycle: Lifecycle, agent: AgentPhase) -> SessionProjection {
        SessionProjection {
            lifecycle,
            agent,
            delivery: DeliveryPhase::Pending,
            resource: ResourcePhase::Attached,
            recovery: RecoveryPhase::NoRecovery,
            outcome: None,
            observed: None,
        }
    }

    #[test]
    fn test_backend_renders_network_structure() {
        let snapshot = Snapshot {
            status: serde_json::json!({"cluster":"local"}),
            roles: vec![
                RoleView {
                    name: "planner".into(),
                    admin: true,
                    max_sessions: 1,
                    spec_hash: "a".into(),
                    prose: None,
                    state: Presence::Online,
                    session_count: 1,
                    detail: None,
                    edges: vec!["builder".into()],
                    aggregate: Some("cluster-x".into()),
                },
                RoleView {
                    name: "builder".into(),
                    admin: false,
                    max_sessions: 1,
                    spec_hash: "b".into(),
                    prose: None,
                    state: Presence::Draining,
                    session_count: 0,
                    detail: None,
                    edges: Vec::new(),
                    aggregate: None,
                },
            ],
            sessions: vec![SessionRow {
                task_id: "abcdef12-3456".into(),
                role: Some("planner".into()),
                session_id: "s1".into(),
                generation: 1,
                seq: 1,
                public_lifecycle: Lifecycle::Working,
                projection: projection(Lifecycle::Working, AgentPhase::Running),
                outcome: None,
                updated_at: Some(Utc::now().timestamp().to_string()),
            }],
            ledger: vec![LedgerEntry {
                msg_id: "m1".into(),
                op_id: None,
                kind: MsgKind::Task,
                from: Principal::role("planner"),
                to: Principal::role("builder"),
                task: Some("abcdef12-3456".into()),
                parent_task: None,
                attempt: 1,
                state: LedgerState::InFlight,
                out_head: None,
                body_json: None,
                enqueued_at: Utc::now(),
                acked_at: None,
            }],
            server_online: true,
            refreshed_at: Some(SystemTime::now()),
            ..Snapshot::default()
        };
        let text = render_once_text(&snapshot, &UiState::default(), 90, 24);
        assert!(text.matches('╭').count() >= 2, "{text}");
        assert!(text.contains("⬡planner*"), "{text}");
        assert!(text.contains('▶'), "{text}");
        assert!(text.contains('◐'), "{text}");
        assert!(
            text.contains("local + server"),
            "the footer names the cluster\n{text}"
        );
    }

    #[test]
    fn swarm_page_renders_the_compact_table_and_alert_strip() {
        let snapshot = Snapshot {
            status: serde_json::json!({"cluster": "local"}),
            roles: Vec::new(),
            sessions: vec![SessionRow {
                task_id: "abcdef12-3456".into(),
                role: Some("builder".into()),
                session_id: "s1".into(),
                generation: 1,
                seq: 1,
                public_lifecycle: Lifecycle::Working,
                projection: projection(Lifecycle::Working, AgentPhase::Running),
                outcome: None,
                updated_at: Some(Utc::now().timestamp().to_string()),
            }],
            ledger: vec![LedgerEntry {
                msg_id: "m1".into(),
                op_id: None,
                kind: MsgKind::Task,
                from: Principal::role("planner"),
                to: Principal::role("builder"),
                task: Some("abcdef12-3456".into()),
                parent_task: None,
                attempt: 1,
                state: LedgerState::InFlight,
                out_head: None,
                body_json: None,
                enqueued_at: Utc::now(),
                acked_at: None,
            }],
            faults: vec![FaultEvent {
                id: 4,
                kind: "intent_exhausted".into(),
                reason: "no accepted intent".into(),
                role: Some("builder".into()),
                ..FaultEvent::default()
            }],
            server_online: true,
            refreshed_at: Some(SystemTime::now()),
            ..Snapshot::default()
        };
        let state = UiState {
            page: Page::Swarm,
            ..UiState::default()
        };
        let text = render_once_text(&snapshot, &state, 120, 30);
        assert!(text.contains("graph [focus]"), "{text}");
        assert!(text.contains("history"), "{text}");
        assert!(text.contains("abcdef12"), "{text}");
        assert!(text.contains("planner→builder"), "{text}");
        assert!(text.contains("! intent_exhausted [builder]"), "{text}");
    }

    #[test]
    fn a_down_server_keeps_the_snapshot_and_says_so() {
        let snapshot = Snapshot {
            status: serde_json::json!({"cluster": "local"}),
            roles: vec![RoleView {
                name: "planner".into(),
                admin: false,
                max_sessions: 1,
                spec_hash: "a".into(),
                prose: None,
                state: Presence::Online,
                session_count: 0,
                detail: None,
                edges: Vec::new(),
                aggregate: None,
            }],
            server_online: false,
            last_error: Some("connection refused".into()),
            refreshed_at: Some(SystemTime::now()),
            ..Snapshot::default()
        };
        let text = render_once_text(&snapshot, &UiState::default(), 100, 16);
        assert!(text.contains("server down"), "{text}");
        assert!(
            text.contains("╭─planner"),
            "the last snapshot stays on screen\n{text}"
        );
        assert!(text.contains("connection refused"), "{text}");
        assert!(text.contains("+ server down +"), "{text}");
    }
}
