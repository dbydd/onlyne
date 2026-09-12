use crate::force::Vec2;
use crate::layout::{self, Camera, CellKind, LayoutEdge, LayoutNode, RoleMap};
use crate::model::{
    Alert, AlertKind, Detail, Focus, Page, RoleDetail, Snapshot, TaskDetail, UiState, alerts,
    event_task, layout_edges, layout_nodes, ledger_state_label, page_history, principal_label,
    role_edges, selected_graph_task, selected_history_task, selected_role, visible_edges,
    visible_sessions,
};
use chrono::{DateTime, Local};
use onlyne_proto::{Event, EventRow, FaultEvent, LedgerState, Lifecycle, SessionRow};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, Wrap};
use std::collections::BTreeSet;

pub fn render(frame: &mut Frame, snapshot: &Snapshot, state: &UiState) {
    let (top, middle, bottom) = frame_areas(frame.area());
    render_top(frame, top, snapshot, state);
    match state.page {
        Page::RoleMap => render_role_map(frame, middle, snapshot, state),
        Page::Swarm => render_swarm(frame, middle, snapshot, state),
    }
    render_bottom(frame, bottom, snapshot, state);
}

/// The three stacked bands every page shares: a status bar, the page, and the
/// footer.
fn frame_areas(area: Rect) -> (Rect, Rect, Rect) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(8),
            Constraint::Length(2),
        ])
        .split(area);
    (root[0], root[1], root[2])
}

/// The share of the middle band the role map takes; the role detail takes the
/// rest, the way page 2 splits its graph from its own detail pane.
const MAP_PERCENT: u16 = 62;

/// Page 1: the map beside the role detail, with the map's note line under it.
fn role_map_areas(area: Rect) -> (Rect, Rect, Rect) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(MAP_PERCENT),
            Constraint::Percentage(100 - MAP_PERCENT),
        ])
        .split(area);
    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(4), Constraint::Length(1)])
        .split(columns[0]);
    (left[0], left[1], columns[1])
}

/// The cells the role map pane draws in for a window of `area`, so the camera
/// clamps to the room the pane really has.
pub fn map_view_size(area: Rect) -> (usize, usize) {
    let (_, middle, _) = frame_areas(area);
    let (map, _, _) = role_map_areas(middle);
    let inner = Block::default().borders(Borders::ALL).inner(map);
    (inner.width as usize, inner.height as usize)
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

fn render_role_map(frame: &mut Frame, area: Rect, snapshot: &Snapshot, state: &UiState) {
    let (map_area, note, detail) = role_map_areas(area);
    render_map(frame, map_area, snapshot, state);
    frame.render_widget(
        Paragraph::new(role_note(snapshot, state)).style(Style::default().fg(Color::DarkGray)),
        note,
    );
    render_detail(frame, detail, state);
}

/// The page-1 pane's subject: the layout synced from the snapshot, the camera
/// that centres the cursor's role in the pane, and the point it centres on.
pub struct RoleScene {
    pub nodes: Vec<LayoutNode>,
    pub edges: Vec<LayoutEdge>,
    pub map: RoleMap,
    pub camera: Camera,
    pub anchor: Vec2,
}

/// Build the page-1 scene for a pane of `view` cells. The layout is settled
/// lazily on a topology change, so this is cheap while the graph holds still.
pub fn role_scene(snapshot: &Snapshot, state: &UiState, view: (usize, usize)) -> RoleScene {
    let nodes = layout_nodes(snapshot, state.active_only);
    let edges = visible_edges(snapshot, state.show_control_edges);
    let mut map = state.map.clone();
    map.sync(&nodes, &layout_edges(snapshot), state.spacing);
    // The map is framed on its own middle: the layout already puts the focus
    // at the centre of the rings, and the cursor only marks a role.
    let anchor = map.centre(&nodes);
    let mut camera = state.role_cam;
    if let Some(bounds) = map.extent(&nodes) {
        camera.clamp_pan(bounds, anchor, view);
    }
    RoleScene {
        nodes,
        edges,
        map,
        camera,
        anchor,
    }
}

/// Bring the cached page-1 layout in line with the snapshot. The renderer syncs
/// its own copy too; doing it here keeps the incremental seed warm across
/// refreshes.
pub fn sync_map(state: &mut UiState, snapshot: &Snapshot) {
    let nodes = layout_nodes(snapshot, state.active_only);
    state
        .map
        .sync(&nodes, &layout_edges(snapshot), state.spacing);
}

/// The role map: the egocentric force layout projected into the pane, with the
/// cursor's box and the hop it stands on reversed.
fn render_map(frame: &mut Frame, area: Rect, snapshot: &Snapshot, state: &UiState) {
    let block = Block::default().title("role network").borders(Borders::ALL);
    let view = block.inner(area);
    let (view_w, view_h) = (view.width as usize, view.height as usize);
    let scene = role_scene(snapshot, state, (view_w, view_h));
    let canvas = layout::canvas(
        &scene.map,
        &scene.nodes,
        &scene.edges,
        &scene.camera,
        scene.anchor,
        (view_w, view_h),
    );
    let title = format!(
        "role network · zoom {:.1}x · {} · repel {:.1}x",
        scene.camera.zoom,
        layout::tier_label(scene.map.radius(scene.camera.zoom)),
        scene.map.repulsion(),
    );
    let highlight = highlight_cells(&canvas, snapshot, state);
    let lines = (0..canvas.height)
        .map(|row| {
            Line::from(
                (0..canvas.width)
                    .map(|column| {
                        let cell = canvas.cells[row][column];
                        let style = style_for_cell(cell.kind);
                        let marked = highlight.contains(&(column as isize, row as isize));
                        Span::styled(
                            cell.ch.to_string(),
                            if marked {
                                style.add_modifier(Modifier::REVERSED)
                            } else {
                                style
                            },
                        )
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(Text::from(lines)).block(block.title(title)),
        area,
    );
}

/// The cells the cursor reverses: the selected role's label and, when `j`/`k`
/// stands on one, the hop `l` would walk.
fn highlight_cells(
    canvas: &layout::Canvas,
    snapshot: &Snapshot,
    state: &UiState,
) -> BTreeSet<(isize, isize)> {
    let mut cells = BTreeSet::new();
    if let Some(name) = selected_role(snapshot, state) {
        if let Some(rect) = canvas.node_boxes.iter().find(|rect| rect.name == name) {
            for offset in 0..rect.label.chars().count() {
                cells.insert((rect.label_x + offset as isize, rect.y));
            }
        }
    }
    if let Some(edge) = highlighted_edge(snapshot, state) {
        if let Some(path) = canvas.edge_paths.get(&(edge.from, edge.to)) {
            cells.extend(path.iter().copied());
        }
    }
    cells
}

/// The dim line under the map: which edges the page holds back and which hop
/// `l` would walk.
fn role_note(snapshot: &Snapshot, state: &UiState) -> String {
    let mut parts = Vec::new();
    let control: Vec<String> = snapshot
        .roles
        .iter()
        .filter(|role| role.control())
        .map(|role| role.name.clone())
        .collect();
    if !control.is_empty() {
        parts.push(if state.show_control_edges {
            format!("{} edges shown · e hides them", control.join(" "))
        } else {
            format!("{} edges hidden · e shows them", control.join(" "))
        });
    }
    if let Some(edge) = highlighted_edge(snapshot, state) {
        parts.push(format!("→ {} · l walks it", edge.to));
    }
    parts.join(" · ")
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
    let active = visible_sessions(snapshot, state.active_only);
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
        None => ("detail".to_string(), placeholder(state.page).to_string()),
    };
    frame.render_widget(
        Paragraph::new(body)
            .wrap(Wrap { trim: false })
            .scroll((state.detail_scroll, 0))
            .block(Block::default().title(title).borders(Borders::ALL)),
        area,
    );
}

fn placeholder(page: Page) -> &'static str {
    match page {
        Page::RoleMap => "(no role registered)",
        Page::Swarm => "(select a task to inspect ledger, sessions, and faults)",
    }
}

/// The footer both pages share. The page identity comes first and the legend
/// is derived from the same state, so a reader never takes the key list for
/// the page counter.
pub fn footer_text(state: &UiState, selected: Option<&str>) -> String {
    let mut text = format!(
        "page {}/{} {}",
        state.page.number(),
        Page::COUNT,
        state.page.label()
    );
    if let Some(selected) = selected {
        text.push_str(" · ");
        text.push_str(selected);
    }
    text.push_str(" │ keys: ");
    text.push_str(state.page.keys());
    text
}

fn render_bottom(frame: &mut Frame, area: Rect, snapshot: &Snapshot, state: &UiState) {
    let subject = match state.page {
        Page::RoleMap => selected_role(snapshot, state),
        Page::Swarm => None,
    };
    let hints = if let Some(search) = &state.search {
        format!("search: {search}  [Enter] apply  [Esc] discard")
    } else {
        format!(
            "{}  {}",
            footer_text(state, subject.as_deref()),
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
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(area);
    // The legend owns the first row and the status block the second, so a
    // narrow terminal clips the tail of the key list but never the page the
    // footer is naming first.
    frame.render_widget(
        Paragraph::new(hints).style(Style::default().fg(Color::DarkGray)),
        rows[0],
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
        rows[1],
    );
}

pub fn history_page_size(height: u16) -> usize {
    height
        .saturating_sub(7)
        .saturating_mul(58)
        .saturating_div(100)
        .max(1) as usize
}

pub fn graph_len(snapshot: &Snapshot, active_only: bool) -> usize {
    visible_sessions(snapshot, active_only).len()
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
        Focus::Graph => selected_graph_task(snapshot, state.graph_cursor, state.active_only),
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
/// The hop `l` would walk: the selected role's highlighted out-edge.
pub fn highlighted_edge(snapshot: &Snapshot, state: &UiState) -> Option<LayoutEdge> {
    let role = selected_role(snapshot, state)?;
    let edges = role_edges(snapshot, state, &role);
    state.role_edge.and_then(|index| edges.get(index).cloned())
}

/// The trail keeps at most this many steps, so a long walk cannot grow without
/// bound.
const ROLE_TRAIL_MAX: usize = 32;

/// Pick a role, remembering the cursor's path so `h` can walk it back.
pub fn select_role(snapshot: &Snapshot, state: &mut UiState, role: String) {
    if let Some(current) = selected_role(snapshot, state) {
        if current != role {
            state.role_trail.push(current);
            if state.role_trail.len() > ROLE_TRAIL_MAX {
                state.role_trail.remove(0);
            }
        }
    }
    state.role_selected = Some(role);
    state.role_edge = None;
}

/// `j`/`k`: step the candidate hop, wrapping at both ends. A role without
/// out-edges keeps no candidate, so the keys stay a no-op.
pub fn move_role_edge(delta: isize, snapshot: &Snapshot, state: &mut UiState) {
    let Some(role) = selected_role(snapshot, state) else {
        return;
    };
    let len = role_edges(snapshot, state, &role).len();
    if len == 0 {
        state.role_edge = None;
        return;
    }
    state.role_edge = Some(match state.role_edge {
        None if delta >= 0 => 0,
        None => len - 1,
        Some(index) => (index as isize + delta).rem_euclid(len as isize) as usize,
    });
}

/// `l`: the highlighted out-edge becomes the selection.
pub fn follow_role_edge(snapshot: &Snapshot, state: &mut UiState) -> bool {
    let (Some(edge), Some(role)) = (
        highlighted_edge(snapshot, state),
        selected_role(snapshot, state),
    ) else {
        return false;
    };
    if edge.to == role {
        return false;
    }
    select_role(snapshot, state, edge.to);
    true
}

/// `h`: back to the role the cursor came through.
pub fn role_back(state: &mut UiState) -> bool {
    let Some(previous) = state.role_trail.pop() else {
        return false;
    };
    state.role_selected = Some(previous);
    state.role_edge = None;
    true
}

/// `←→↑↓`: pan the camera over the map, clamped to its extent. The selection
/// stays where it was.
pub fn pan_role_view(
    delta: (isize, isize),
    snapshot: &Snapshot,
    state: &mut UiState,
    view: (usize, usize),
) {
    let scene = role_scene(snapshot, state, view);
    let mut camera = state.role_cam;
    camera.pan = (camera.pan.0 + delta.0, camera.pan.1 + delta.1);
    if let Some(bounds) = scene.map.extent(&scene.nodes) {
        camera.clamp_pan(bounds, scene.anchor, view);
    }
    state.role_cam = camera;
}

/// The mouse wheel: zoom one step, clamped to the camera's own range, and keep
/// the pane over the map.
pub fn zoom_role_view(
    delta: isize,
    snapshot: &Snapshot,
    state: &mut UiState,
    view: (usize, usize),
) {
    let scene = role_scene(snapshot, state, view);
    let mut camera = state.role_cam;
    if delta < 0 {
        camera.zoom_in();
    } else {
        camera.zoom_out();
    }
    if let Some(bounds) = scene.map.extent(&scene.nodes) {
        camera.clamp_pan(bounds, scene.anchor, view);
    }
    state.role_cam = camera;
}

/// The mouse dragging the map: pan by the cells the pointer travelled.
pub fn drag_role_view(
    from: (u16, u16),
    to: (u16, u16),
    snapshot: &Snapshot,
    state: &mut UiState,
    view: (usize, usize),
) {
    pan_role_view(
        (
            to.0 as isize - from.0 as isize,
            to.1 as isize - from.1 as isize,
        ),
        snapshot,
        state,
        view,
    );
}

pub fn detail_text(detail: &Detail) -> (String, String) {
    match detail {
        Detail::Task(task) => task_detail_text(task),
        Detail::Role(role) => role_detail_text(role),
    }
}

fn task_detail_text(detail: &TaskDetail) -> (String, String) {
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
    write_sessions(&mut out, &detail.sessions);
    out.push_str("\nfaults\n");
    write_faults(&mut out, &detail.faults);
    (format!("task {}", short(&detail.task_id)), out)
}

/// The page-1 panel: liveness and capacity, the role's ACL peers, then the
/// sessions the server projects onto it and its faults.
fn role_detail_text(detail: &RoleDetail) -> (String, String) {
    let mut out = String::new();
    out.push_str(&format!("state {}\n", detail.state.as_str()));
    out.push_str(&format!(
        "sessions {}/{}\n",
        detail.session_count, detail.max_sessions
    ));
    out.push_str(&format!(
        "admin {}\n",
        if detail.admin { "yes" } else { "no" }
    ));
    if let Some(aggregate) = &detail.aggregate {
        out.push_str(&format!("aggregate {aggregate}\n"));
    }
    out.push_str("acl peers ");
    if detail.peers.is_empty() {
        out.push_str("(none)");
    } else {
        out.push_str(&detail.peers.join(" "));
    }
    out.push('\n');
    out.push_str("\nsessions\n");
    write_sessions(&mut out, &detail.sessions);
    out.push_str("\nfaults\n");
    write_faults(&mut out, &detail.faults);
    (format!("role {}", detail.role), out)
}

fn write_sessions(out: &mut String, sessions: &[SessionRow]) {
    if sessions.is_empty() {
        out.push_str("  (none)\n");
        return;
    }
    for session in sessions {
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
}

fn write_faults(out: &mut String, faults: &[FaultEvent]) {
    if faults.is_empty() {
        out.push_str("  (none)\n");
        return;
    }
    for fault in faults {
        out.push_str(&format!(
            "  #{} {} role={} state={} {}\n",
            fault.id,
            fault.kind,
            fault.role.as_deref().unwrap_or("?"),
            fault.state.as_deref().unwrap_or("?"),
            fault.reason
        ));
    }
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
        AgentPhase, DeliveryPhase, LedgerEntry, Lifecycle, MsgKind, Presence, Principal,
        RecoveryPhase, ResourcePhase, SessionProjection,
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

    fn session(task: &str, lifecycle: Lifecycle, agent: AgentPhase) -> SessionRow {
        SessionRow {
            task_id: task.into(),
            role: Some("planner".into()),
            session_id: "s1".into(),
            generation: 1,
            seq: 1,
            public_lifecycle: lifecycle,
            projection: projection(lifecycle, agent),
            outcome: None,
            updated_at: Some(Utc::now().timestamp().to_string()),
        }
    }

    fn role(name: &str, edges: &[&str]) -> RoleView {
        RoleView {
            name: name.into(),
            admin: false,
            max_sessions: 1,
            spec_hash: "a".into(),
            prose: None,
            state: Presence::Online,
            session_count: 0,
            detail: None,
            edges: edges.iter().map(|edge| (*edge).to_string()).collect(),
            aggregate: None,
        }
    }

    fn ledger_row(from: &str, to: &str, task: &str, state: LedgerState) -> LedgerEntry {
        LedgerEntry {
            msg_id: "m1".into(),
            op_id: None,
            kind: MsgKind::Task,
            from: Principal::role(from),
            to: Principal::role(to),
            task: Some(task.into()),
            parent_task: None,
            hop: 0,
            attempt: 1,
            state,
            out_head: None,
            body_json: None,
            enqueued_at: Utc::now(),
            acked_at: None,
        }
    }

    /// A ring of three roles: a→b→c→a.
    fn linked_snapshot() -> Snapshot {
        Snapshot {
            status: serde_json::json!({"cluster": "local"}),
            roles: vec![role("a", &["b"]), role("b", &["c"]), role("c", &["a"])],
            server_online: true,
            refreshed_at: Some(SystemTime::now()),
            ..Snapshot::default()
        }
    }

    /// One role with two out-edges, so `j`/`k` have something to cycle.
    fn fan_snapshot() -> Snapshot {
        Snapshot {
            status: serde_json::json!({"cluster": "local"}),
            roles: vec![role("a", &["b", "c"]), role("b", &[]), role("c", &[])],
            server_online: true,
            refreshed_at: Some(SystemTime::now()),
            ..Snapshot::default()
        }
    }

    /// A control role whose spokes are the default-hidden kind.
    fn controlled_snapshot() -> Snapshot {
        Snapshot {
            status: serde_json::json!({"cluster": "local"}),
            roles: vec![
                role("_supervisor", &["a"]),
                role("a", &["b"]),
                role("b", &[]),
            ],
            server_online: true,
            refreshed_at: Some(SystemTime::now()),
            ..Snapshot::default()
        }
    }

    fn five_node_ring_snapshot() -> Snapshot {
        Snapshot {
            status: serde_json::json!({"cluster": "local"}),
            roles: vec![
                role("a", &["b"]),
                role("b", &["c"]),
                role("c", &["d"]),
                role("d", &["e"]),
                role("e", &["a"]),
            ],
            server_online: true,
            refreshed_at: Some(SystemTime::now()),
            ..Snapshot::default()
        }
    }

    fn render_once_buffer(
        snapshot: &Snapshot,
        state: &UiState,
        width: u16,
        height: u16,
    ) -> ratatui::buffer::Buffer {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).expect("test backend");
        terminal
            .draw(|frame| render(frame, snapshot, state))
            .expect("render once");
        terminal.backend().buffer().clone()
    }

    fn row_text(buffer: &ratatui::buffer::Buffer, y: u16) -> String {
        (0..buffer.area().width)
            .map(|x| buffer[(x, y)].symbol())
            .collect()
    }

    /// Whether the row carrying `needle` holds reversed cells: the cursor's
    /// mark, on both the label of a box and the row of a table.
    fn row_has_reversed(buffer: &ratatui::buffer::Buffer, needle: &str) -> bool {
        (0..buffer.area().height).any(|y| {
            row_text(buffer, y).contains(needle)
                && (0..buffer.area().width).any(|x| {
                    buffer[(x, y)]
                        .style()
                        .add_modifier
                        .contains(Modifier::REVERSED)
                })
        })
    }

    /// Whether the row carrying `needle` reverses a cell at or after the
    /// needle's own column: a neighbour further left stays unmarked.
    fn reversed_right_of(buffer: &ratatui::buffer::Buffer, needle: &str) -> bool {
        (0..buffer.area().height).any(|y| {
            let row = row_text(buffer, y);
            let Some(start) = row.find(needle) else {
                return false;
            };
            (start as u16..buffer.area().width).any(|x| {
                buffer[(x, y)]
                    .style()
                    .add_modifier
                    .contains(Modifier::REVERSED)
            })
        })
    }

    fn reversed_count(buffer: &ratatui::buffer::Buffer) -> usize {
        (0..buffer.area().height)
            .map(|y| {
                (0..buffer.area().width)
                    .filter(|x| {
                        buffer[(*x, y)]
                            .style()
                            .add_modifier
                            .contains(Modifier::REVERSED)
                    })
                    .count()
            })
            .sum()
    }

    #[test]
    fn test_backend_renders_network_structure() {
        let snapshot = Snapshot {
            status: serde_json::json!({"cluster":"local"}),
            roles: vec![
                RoleView {
                    name: "planner".into(),
                    aggregate: Some("cluster-x".into()),
                    edges: vec!["builder".into()],
                    ..role("planner", &["builder"])
                },
                role("builder", &["planner"]),
            ],
            sessions: vec![session(
                "abcdef12-3456",
                Lifecycle::Working,
                AgentPhase::Running,
            )],
            ledger: vec![ledger_row(
                "planner",
                "builder",
                "abcdef12-3456",
                LedgerState::InFlight,
            )],
            server_online: true,
            refreshed_at: Some(SystemTime::now()),
            ..Snapshot::default()
        };
        let text = render_once_text(&snapshot, &UiState::default(), 90, 24);
        assert!(text.matches('╭').count() >= 2, "{text}");
        assert!(text.contains("⬡planner*"), "{text}");
        assert!(
            ['▸', '◂', '▴', '▾']
                .iter()
                .any(|arrow| text.contains(*arrow)),
            "the hop carries an arrowhead into its target\n{text}"
        );
        assert!(text.contains('◐'), "{text}");
        assert!(
            text.contains("local + server"),
            "the footer names the cluster\n{text}"
        );
        assert!(
            text.contains("page 1/2 roles"),
            "the footer names the page it prints\n{text}"
        );
        assert!(
            text.contains("planner edges hidden · e shows them"),
            "the map states which spokes it holds back\n{text}"
        );
    }

    #[test]
    fn a_wider_repulsion_spreads_the_ring_further_apart() {
        let snapshot = five_node_ring_snapshot();
        let compact = UiState {
            spacing: 1,
            ..UiState::default()
        };
        let wide = UiState {
            spacing: 4,
            ..UiState::default()
        };
        let spread = |state: &UiState| {
            let scene = role_scene(&snapshot, state, (220, 40));
            let (low, high) = scene.map.extent(&scene.nodes).expect("a map");
            (high.x - low.x) as usize
        };
        assert!(
            spread(&wide) > spread(&compact),
            "compact {} wide {}",
            spread(&compact),
            spread(&wide)
        );

        let text = render_once_text(&snapshot, &wide, 220, 40);
        for name in ["a", "b", "c", "d", "e"] {
            assert!(text.contains(&format!("╭─{name}")), "{text}");
        }
        assert!(
            ['▸', '◂', '▴', '▾']
                .iter()
                .any(|arrow| text.contains(*arrow)),
            "every hop lands on an arrowhead\n{text}"
        );
        assert!(
            text.contains("zoom 1.0x · overview"),
            "the pane names its zoom tier\n{text}"
        );
    }

    #[test]
    fn page_one_highlights_the_cursor_and_follows_a_walk() {
        let snapshot = linked_snapshot();
        let mut state = UiState::default();
        assert_eq!(selected_role(&snapshot, &state).as_deref(), Some("a"));

        let cursor = render_once_buffer(&snapshot, &state, 120, 36);
        assert!(
            row_has_reversed(&cursor, "╭─a"),
            "the cursor's label is reversed\n{}",
            buffer_text(&cursor)
        );
        assert!(
            !reversed_right_of(&cursor, "╭─b"),
            "another role's label is not\n{}",
            buffer_text(&cursor)
        );

        move_role_edge(1, &snapshot, &mut state);
        assert_eq!(
            highlighted_edge(&snapshot, &state).map(|edge| edge.to),
            Some("b".to_string())
        );
        let hop = render_once_buffer(&snapshot, &state, 120, 36);
        assert!(
            reversed_count(&hop) > reversed_count(&cursor),
            "the candidate hop lights up as well"
        );
        assert!(
            row_has_reversed(&hop, "╭─a"),
            "the cursor stays on the role it was on\n{}",
            buffer_text(&hop)
        );

        assert!(follow_role_edge(&snapshot, &mut state));
        assert_eq!(selected_role(&snapshot, &state).as_deref(), Some("b"));
        let walked = render_once_buffer(&snapshot, &state, 120, 36);
        assert!(
            row_has_reversed(&walked, "╭─b"),
            "the highlight follows the walk\n{}",
            buffer_text(&walked)
        );

        assert!(role_back(&mut state), "h walks the trail back");
        assert_eq!(selected_role(&snapshot, &state).as_deref(), Some("a"));
    }

    #[test]
    fn j_k_wrap_the_candidate_hop_without_moving_the_camera() {
        let snapshot = fan_snapshot();
        let mut state = UiState::default();
        state.role_cam.pan = (3, 0);
        let pan = state.role_cam.pan;
        move_role_edge(1, &snapshot, &mut state);
        assert_eq!(
            highlighted_edge(&snapshot, &state).map(|edge| edge.to),
            Some("b".to_string())
        );
        move_role_edge(1, &snapshot, &mut state);
        assert_eq!(
            highlighted_edge(&snapshot, &state).map(|edge| edge.to),
            Some("c".to_string())
        );
        move_role_edge(1, &snapshot, &mut state);
        assert_eq!(
            highlighted_edge(&snapshot, &state).map(|edge| edge.to),
            Some("b".to_string()),
            "the candidates wrap"
        );
        move_role_edge(-1, &snapshot, &mut state);
        assert_eq!(
            highlighted_edge(&snapshot, &state).map(|edge| edge.to),
            Some("c".to_string()),
            "k steps back"
        );
        assert_eq!(state.role_cam.pan, pan, "the keys never move the camera");

        select_role(&snapshot, &mut state, "c".into());
        move_role_edge(1, &snapshot, &mut state);
        assert_eq!(
            state.role_edge, None,
            "a role without out-edges has no candidate"
        );
        assert_eq!(state.role_cam.pan, pan);
    }

    #[test]
    fn the_arrows_and_the_mouse_move_the_camera_not_the_selection() {
        let snapshot = fan_snapshot();
        let mut state = UiState::default();
        let selected = selected_role(&snapshot, &state);
        let view = (60, 20);
        pan_role_view((1, 1), &snapshot, &mut state, view);
        assert_eq!(state.role_cam.pan, (1, 1));
        assert_eq!(
            selected_role(&snapshot, &state),
            selected,
            "the camera leaves the cursor alone"
        );

        pan_role_view((10_000, 10_000), &snapshot, &mut state, view);
        let edge = state.role_cam.pan;
        assert!(
            edge.0 < 10_000 && edge.1 < 10_000,
            "the camera stops at the map's edge: {edge:?}"
        );
        pan_role_view((-10_000, -10_000), &snapshot, &mut state, view);
        assert!(
            state.role_cam.pan.0 < 0 && state.role_cam.pan.1 < 0,
            "the map can be looked around from side to side: {:?}",
            state.role_cam.pan
        );

        // The wheel zooms in and out; a drag pans by the cells the pointer
        // travelled, and `0` puts the camera back.
        zoom_role_view(-1, &snapshot, &mut state, view);
        assert!(state.role_cam.zoom > 1.0, "{:?}", state.role_cam);
        zoom_role_view(1, &snapshot, &mut state, view);
        assert!(
            (state.role_cam.zoom - 1.0).abs() < 1e-6,
            "{:?}",
            state.role_cam
        );
        state.role_cam.pan = (0, 0);
        drag_role_view((10, 5), (13, 4), &snapshot, &mut state, view);
        assert_eq!(state.role_cam.pan, (3, -1), "{:?}", state.role_cam);
        state.role_cam.reset();
        assert_eq!(state.role_cam, Camera::default());
    }

    #[test]
    fn e_reveals_and_hides_the_control_roles_spokes() {
        let snapshot = controlled_snapshot();
        let mut state = UiState::default();
        assert!(
            visible_edges(&snapshot, state.show_control_edges)
                .iter()
                .all(|edge| edge.from != "_supervisor"),
            "the supervisor's spokes stay off the map"
        );
        let hidden = render_once_text(&snapshot, &state, 120, 36);
        assert!(
            hidden.contains("_supervisor edges hidden · e shows them"),
            "{hidden}"
        );

        state.show_control_edges = true;
        assert!(
            visible_edges(&snapshot, state.show_control_edges)
                .iter()
                .any(|edge| edge.from == "_supervisor"),
            "e shows them"
        );
        let shown = render_once_text(&snapshot, &state, 120, 36);
        assert!(
            shown.contains("_supervisor edges shown · e hides them"),
            "{shown}"
        );
    }

    #[test]
    fn the_footer_names_the_current_page_on_both_pages() {
        let snapshot = linked_snapshot();
        let mut state = UiState::default();
        assert_eq!(
            footer_text(&state, Some("a")),
            format!("page 1/2 roles · a │ keys: {}", Page::RoleMap.keys())
        );
        let page_one = render_once_text(&snapshot, &state, 120, 36);
        assert!(page_one.contains("page 1/2 roles"), "{page_one}");

        state.page = Page::Swarm;
        assert!(footer_text(&state, None).starts_with("page 2/2 swarm"));
        let page_two = render_once_text(&snapshot, &state, 120, 30);
        assert!(page_two.contains("page 2/2 swarm"), "{page_two}");
    }

    #[test]
    fn the_role_panel_names_the_state_the_slots_and_the_peers() {
        let detail = Detail::Role(RoleDetail {
            role: "builder".into(),
            state: Presence::Draining,
            session_count: 2,
            max_sessions: 3,
            admin: true,
            aggregate: Some("cluster-b".into()),
            peers: vec!["planner".into()],
            faults: vec![FaultEvent {
                id: 7,
                kind: "intent_exhausted".into(),
                reason: "no accepted intent".into(),
                ..FaultEvent::default()
            }],
            sessions: Vec::new(),
        });
        let (title, body) = detail_text(&detail);
        assert_eq!(title, "role builder");
        assert!(body.contains("state draining"), "{body}");
        assert!(body.contains("sessions 2/3"), "{body}");
        assert!(body.contains("admin yes"), "{body}");
        assert!(body.contains("aggregate cluster-b"), "{body}");
        assert!(body.contains("acl peers planner"), "{body}");
        assert!(body.contains("#7 intent_exhausted"), "{body}");
    }

    #[test]
    fn page_one_renders_the_selected_role_detail() {
        let snapshot = linked_snapshot();
        let mut state = UiState::default();
        state.detail = Some(Detail::Role(RoleDetail {
            role: "a".into(),
            state: Presence::Online,
            session_count: 1,
            max_sessions: 2,
            admin: false,
            aggregate: None,
            peers: vec!["b".into()],
            faults: Vec::new(),
            sessions: vec![session(
                "abcdef12-3456",
                Lifecycle::Working,
                AgentPhase::Running,
            )],
        }));
        let text = render_once_text(&snapshot, &state, 120, 36);
        assert!(text.contains("role a"), "{text}");
        assert!(text.contains("state online"), "{text}");
        assert!(text.contains("acl peers b"), "{text}");
        assert!(text.contains("s1 role=planner life=working"), "{text}");
    }

    #[test]
    fn swarm_page_renders_the_compact_table_and_alert_strip() {
        let snapshot = Snapshot {
            status: serde_json::json!({"cluster": "local"}),
            roles: Vec::new(),
            sessions: vec![session(
                "abcdef12-3456",
                Lifecycle::Working,
                AgentPhase::Running,
            )],
            ledger: vec![ledger_row(
                "planner",
                "builder",
                "abcdef12-3456",
                LedgerState::InFlight,
            )],
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
        let buffer = render_once_buffer(&snapshot, &state, 120, 30);
        let text = buffer_text(&buffer);
        assert!(text.contains("graph [focus]"), "{text}");
        assert!(text.contains("history"), "{text}");
        assert!(text.contains("abcdef12"), "{text}");
        assert!(text.contains("planner→builder"), "{text}");
        assert!(text.contains("! intent_exhausted [builder]"), "{text}");
        assert!(
            row_has_reversed(&buffer, "planner") && reversed_count(&buffer) > 0,
            "the selected graph row keeps its reverse video\n{text}"
        );
    }

    #[test]
    fn a_down_server_keeps_the_snapshot_and_says_so() {
        let snapshot = Snapshot {
            status: serde_json::json!({"cluster": "local"}),
            roles: vec![role("planner", &[])],
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
