use crate::force::Vec2;
use crate::layout::{self, Camera, CellKind, LayoutEdge, LayoutNode, RoleMap};
use crate::model::{
    Alert, AlertKind, Detail, Focus, Page, RoleDetail, Snapshot, TaskDetail, UiState, alerts,
    event_task, layout_edges, layout_nodes, ledger_state_label, live_sessions, page_history,
    principal_label, role_edges, selected_graph_task, selected_history_task, selected_role,
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
    let edges = layout_edges(snapshot);
    let mut map = state.map.clone();
    map.sync(&nodes, &edges, state.spacing);
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

/// The dim line under the map: the hop `l` would walk.
fn role_note(snapshot: &Snapshot, state: &UiState) -> String {
    match highlighted_edge(snapshot, state) {
        Some(edge) => format!("→ {} · l walks it", edge.to),
        None => String::new(),
    }
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

/// Page 2's panes: the graph and its alert strip on the left, the history and
/// the detail panel on the right.
fn swarm_areas(area: Rect) -> (Rect, Rect, Rect) {
    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(main[1]);
    (main[0], right[0], right[1])
}

/// The page-2 detail panel for a terminal of `size`, so the key handler can
/// clamp the scroll against the room the panel really has.
pub fn detail_pane_size(size: (u16, u16)) -> Rect {
    let (_, middle, _) = frame_areas(Rect::new(0, 0, size.0, size.1));
    swarm_areas(middle).2
}

fn render_swarm(frame: &mut Frame, area: Rect, snapshot: &Snapshot, state: &UiState) {
    let (left, history, detail) = swarm_areas(area);
    let alerts = alerts(snapshot);
    let shown = alerts.len().min(2) as u16;
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(4), Constraint::Length(shown)])
        .split(left);
    render_swarm_graph(frame, rows[0], snapshot, state);
    if shown > 0 {
        render_alerts(frame, rows[1], &alerts[..shown as usize]);
    }
    render_history(frame, history, snapshot, state);
    render_detail(frame, detail, state);
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
        } else if session.heartbeat_stale {
            Style::default().fg(Color::Yellow)
        } else if session.outcome.is_some() {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default()
        };
        rows.push(
            Row::new(vec![
                Cell::from(role_of(session)),
                Cell::from(short(&session.task_id)),
                Cell::from(state_label(session)),
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
                Cell::from(hop_cell(row, area.width)),
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
                Constraint::Length(HISTORY_CREATED_CELLS),
                Constraint::Length(HISTORY_KIND_CELLS),
                Constraint::Min(HISTORY_HOP_FLOOR),
                Constraint::Length(HISTORY_STATE_CELLS),
                Constraint::Length(HISTORY_TASK_CELLS),
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

/// The cells the history table's `created` column holds its `%H:%M:%S` in.
const HISTORY_CREATED_CELLS: u16 = 10;
/// The cells the `kind` column holds `gateway` in.
const HISTORY_KIND_CELLS: u16 = 8;
/// The cells the `from→to` column floors at. It is the table's one flexible column,
/// so this floor is where the pane stops having anything to spare.
const HISTORY_HOP_FLOOR: u16 = 17;
/// The cells the `state` column holds `in_flight` in.
const HISTORY_STATE_CELLS: u16 = 10;
/// The cells the `task` column holds a `short` task id in.
const HISTORY_TASK_CELLS: u16 = 9;
/// What the four fixed columns and the gaps between the five cost the pane: the
/// arithmetic a row's slack is read out of, spelled from the same constants the table
/// hands the widget. The flexible column's own [`HISTORY_HOP_FLOOR`] sits on top of
/// this, and whatever the pane has beyond the two is a row's slack.
const HISTORY_PINNED_CELLS: usize = HISTORY_CREATED_CELLS as usize
    + HISTORY_KIND_CELLS as usize
    + HISTORY_STATE_CELLS as usize
    + HISTORY_TASK_CELLS as usize
    + 4;
/// The label the board puts on a settlement reason, the one the task panel prints and
/// the `onlyne ledger` help describes.
const REASON_LABEL: &str = "reason=";
/// The cells a reason tail asks for: the joining space, the label, and three
/// characters, the shortest read that tells `session_dead` from `requeue_exh…`. Below
/// that the row prints its hop alone and the task panel, which wraps, keeps the word.
const REASON_TAIL_CELLS: usize = 1 + REASON_LABEL.len() + 3;

/// The `from→to` cell: the hop, and where the ledger stored a reason for the state the
/// row reports, that reason on the cell's tail under the label the task panel uses.
///
/// The reason rides this cell because the cell is the row's only slack: the four pinned
/// columns spend [`HISTORY_PINNED_CELLS`] and `state` fits `in_flight` inside its ten,
/// so a row has nowhere else to put a field whose text an operator writes. The tail
/// spends cells the hop left over and cuts itself on a character boundary with the
/// ellipsis the map's boxes give an overlong line, which keeps the hop whole and leaves
/// every column of every row exactly where it stands. A row that stored nothing prints
/// the hop it printed before the field reached the board, byte for byte, at every width.
///
/// The board's two promises about this field sit on different surfaces, and moving it
/// to another column breaks one of them. Here it is the layout promise: a row with no
/// reason holds its cells. The wire promise — that an empty reason adds no `reason` key
/// to an answer — is `onlyne ledger`'s, and its help text speaks for itself.
fn hop_cell(row: &EventRow, width: u16) -> String {
    let hop = event_route(row);
    let (Some(reason), Some(column)) = (stored_reason(row), hop_column_cells(width)) else {
        return hop;
    };
    let spare = column.saturating_sub(hop.chars().count());
    if spare < REASON_TAIL_CELLS {
        return hop;
    }
    let word = spare - (1 + REASON_LABEL.len());
    format!("{hop} {REASON_LABEL}{}", layout::truncate(reason, word))
}

/// The word a ledger row stored for the state it reports. A fault row carries its own
/// text and reaches the board through the alert strip and the panel below, so the tail
/// stays the ledger's field. A row whose stored word is empty or blank prints the state
/// word alone, the row it printed with no key at all.
fn stored_reason(row: &EventRow) -> Option<&str> {
    match &row.event {
        Event::LedgerState(event) => event
            .reason
            .as_deref()
            .filter(|reason| !reason.trim().is_empty()),
        _ => None,
    }
}

/// The cells the `from→to` column gets in a pane `width` wide, and `None` where the
/// pane cannot pay for the four pinned columns and the column's own floor together.
/// There the solver squeezes every column at once and a row has no slack of its own, so
/// the row keeps to its hop.
fn hop_column_cells(width: u16) -> Option<usize> {
    let inner = usize::from(width).saturating_sub(2);
    let column = inner.checked_sub(HISTORY_PINNED_CELLS)?;
    (usize::from(HISTORY_HOP_FLOOR) <= column).then_some(column)
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
    let title = detail_title(state);
    let body = detail_body(state);
    let extent = detail_extent(&body, area);
    let scroll = (state.detail_scroll as usize).min(extent.max);
    let title = if extent.max == 0 {
        title
    } else {
        // Where the panel stands in its text: an operator can see there is
        // more above or below.
        format!(
            "{title}  {}-{}/{} ▲▼",
            scroll + 1,
            (scroll + extent.view).min(extent.lines),
            extent.lines
        )
    };
    frame.render_widget(
        Paragraph::new(body)
            .wrap(Wrap { trim: false })
            .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0))
            .block(Block::default().title(title).borders(Borders::ALL)),
        area,
    );
}

/// The detail panel's heading: its subject, or the pane's name when nothing is
/// selected.
fn detail_title(state: &UiState) -> String {
    match &state.detail {
        Some(Detail::Role(detail)) => format!("role {}", detail.role),
        Some(Detail::Task(detail)) => format!("task {}", short(&detail.task_id)),
        None => "detail".to_string(),
    }
}

/// The detail panel's body: the subject's text, or what the page expects you
/// to pick. A pending operator notice (`state.message`) sits on the first line.
pub fn detail_body(state: &UiState) -> String {
    let body = match &state.detail {
        Some(detail) => detail_text(detail).1,
        None => placeholder(state.page).to_string(),
    };
    if state.message.is_empty() {
        body
    } else {
        format!("{}\n{body}", state.message)
    }
}

/// How far the detail panel's text runs, how much of it shows, and the last
/// scroll offset that still has text on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DetailExtent {
    pub lines: usize,
    pub view: usize,
    pub max: usize,
}

/// The wrapped height of `body` in the panel `area`, and the scroll it allows.
pub fn detail_extent(body: &str, area: Rect) -> DetailExtent {
    let view = area.height.saturating_sub(2) as usize;
    let width = area.width.saturating_sub(2).max(1) as usize;
    let lines = body.split('\n').map(|line| wrapped_rows(line, width)).sum();
    DetailExtent {
        lines,
        view,
        max: lines.saturating_sub(view),
    }
}

/// Stop a detail scroll where the text does, so `J` at the end of the pane does
/// nothing rather than scrolling into blankness.
pub fn clamp_detail_scroll(state: &mut UiState, pane: Rect) {
    let body = detail_body(state);
    let max = detail_extent(&body, pane).max;
    state.detail_scroll = state
        .detail_scroll
        .min(u16::try_from(max).unwrap_or(u16::MAX));
}

/// The rows one line of the panel needs once wrapped to `width` columns, the
/// way the panel wraps it: on spaces where it can, mid-word where it must.
fn wrapped_rows(line: &str, width: usize) -> usize {
    if line.is_empty() {
        return 1;
    }
    let mut rows = 1;
    let mut used = 0usize;
    for word in line.split(' ') {
        let len = word.chars().count();
        if used == 0 {
            used = len;
            while used > width {
                rows += 1;
                used -= width;
            }
        } else if used + 1 + len <= width {
            used += 1 + len;
        } else {
            rows += 1;
            used = len;
            while used > width {
                rows += 1;
                used -= width;
            }
        }
    }
    rows
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
    let edges = role_edges(snapshot, &role);
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
    let len = role_edges(snapshot, &role).len();
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
        // The reason joins the row's tail: an acked row prints exactly what it
        // printed before the column reached the board.
        let mut tail = entry.out_head.clone().unwrap_or_default();
        if let Some(reason) = &entry.reason {
            if !tail.is_empty() {
                tail.push(' ');
            }
            tail.push_str("reason=");
            tail.push_str(reason);
        }
        out.push_str(&format!(
            "  {} {}→{} {} att={} {}\n",
            short(&entry.msg_id),
            principal_label(&entry.from),
            principal_label(&entry.to),
            ledger_state_label(entry),
            entry.attempt,
            tail
        ));
    }
    out.push_str("\nsessions\n");
    write_sessions(&mut out, detail.sessions.iter());
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
        "sessions {}/{} · queued {}\n",
        detail.session_count, detail.max_sessions, detail.queued
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
    // Page 1 lists the sessions still holding a slot: the same "live" test the
    // snapshot views apply, so an exited session does not read as a running one
    // here.
    write_sessions(&mut out, live_sessions(&detail.sessions));
    out.push_str("\nfaults\n");
    write_faults(&mut out, &detail.faults);
    (format!("role {}", detail.role), out)
}

/// The session rows one panel lists, one per line; an empty list still says
/// `(none)`.
fn write_sessions<'a>(out: &mut String, sessions: impl IntoIterator<Item = &'a SessionRow>) {
    let mut any = false;
    for session in sessions {
        any = true;
        out.push_str(&format!(
            "  {} role={} life={} agent={:?} gen={} seq={} outcome={} updated={}\n",
            session.session_id,
            session.role.as_deref().unwrap_or("?"),
            state_label(session),
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
    if !any {
        out.push_str("  (none)\n");
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

/// The lifecycle word, with `+stale` when the server timed the heartbeats out.
/// The flag rides the answer, so the TUI renders the server's own verdict.
fn state_label(session: &SessionRow) -> String {
    let base = lifecycle_label(session.public_lifecycle);
    if session.heartbeat_stale {
        format!("{base}+stale")
    } else {
        base.to_string()
    }
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
    use crate::model::{RoleView, SUPERVISOR_ROLE, cycle_role, hidden_role};
    use chrono::Utc;
    use onlyne_proto::{
        AgentPhase, DeliveryPhase, LedgerEntry, LedgerStateEvent, Lifecycle, MsgKind, Presence,
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
            heartbeat_stale: false,
            fresh: None,
        }
    }

    /// A session whose last projection is `age_secs` old. A render test fixes
    /// the age rather than reading the clock, so two renders of one snapshot
    /// cannot straddle a format boundary (`59s` becoming `1m`).
    fn session_aged(task: &str, age_secs: i64) -> SessionRow {
        let mut row = session(task, Lifecycle::Working, AgentPhase::Running);
        row.updated_at = Some(
            (Utc::now() - chrono::Duration::seconds(age_secs))
                .timestamp()
                .to_string(),
        );
        row
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
            queued: 0,
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
            reason: None,
            out_head: None,
            body_json: None,
            enqueued_at: Utc::now(),
            acked_at: None,
        }
    }

    /// One row of the page-2 feed: a `planner→builder` hop that settled in `state`,
    /// carrying the `reason` the ledger stored where there is one. The clock is fixed
    /// and the graph stays idle, so a render test reads the feed and nothing else.
    fn history_row(state: LedgerState, reason: Option<&str>) -> EventRow {
        EventRow {
            seq: 1,
            created_at: chrono::DateTime::parse_from_rfc3339("2026-09-23T12:34:56Z")
                .expect("the fixture clock")
                .with_timezone(&Utc),
            event: Event::LedgerState(LedgerStateEvent {
                msg_id: "m1".into(),
                op_id: None,
                kind: MsgKind::Task,
                from: Principal::role("planner"),
                to: Principal::role("builder"),
                task: Some("abcdef12-3456".into()),
                state,
                outcome: None,
                reason: reason.map(str::to_string),
            }),
        }
    }

    /// A page-2 snapshot whose feed is `rows`.
    fn history_snapshot(rows: Vec<EventRow>) -> Snapshot {
        let total = rows.len();
        Snapshot {
            status: serde_json::json!({"cluster": "local"}),
            history: rows,
            history_total: total,
            server_online: true,
            ..Snapshot::default()
        }
    }

    fn swarm_page() -> UiState {
        UiState {
            page: Page::Swarm,
            ..UiState::default()
        }
    }

    /// The feed's rows as the pane draws them, one string per row: the pane sits behind
    /// the graph's right border, and its own border comes off the end.
    fn history_rows(text: &str) -> Vec<String> {
        text.lines()
            .filter(|line| line.contains("12:34:56"))
            .filter_map(|line| line.split("││").nth(1))
            .map(|pane| pane.trim_end_matches('│').to_string())
            .collect()
    }

    /// The column `needle` starts at in a rendered row, counted in the cells the pane
    /// counts: the ellipsis a cut tail ends with is one cell.
    fn cell_column(row: &str, needle: &str) -> Option<usize> {
        row.find(needle).map(|at| row[..at].chars().count())
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

    /// A cluster whose registry carries the operator's own agent
    /// ([`SUPERVISOR_ROLE`]) beside a two-role ring. Every surface it could
    /// reach is loaded: it is offline and holding queued deliveries, a hop runs
    /// to it and one runs back, and the server projects a live session and a
    /// finished one onto it.
    fn supervised_snapshot() -> Snapshot {
        let mut supervisor = role(SUPERVISOR_ROLE, &["a"]);
        supervisor.state = Presence::Offline;
        supervisor.queued = 3;
        let mut live = session_aged("aaaa1111-live", 12);
        live.role = Some(SUPERVISOR_ROLE.into());
        let mut finished = session_aged("bbbb2222-done", 300);
        finished.role = Some(SUPERVISOR_ROLE.into());
        finished.public_lifecycle = Lifecycle::Exited;
        let mut ring = session_aged("cccc3333-ring", 40);
        ring.role = Some("a".into());
        Snapshot {
            status: serde_json::json!({"cluster": "local"}),
            roles: vec![
                supervisor,
                role("a", &[SUPERVISOR_ROLE, "b"]),
                role("b", &["a"]),
            ],
            sessions: vec![live, finished, ring],
            ledger: vec![ledger_row("a", "b", "cccc3333-ring", LedgerState::InFlight)],
            server_online: true,
            // A fixed clock: the footer prints the refresh time, and a live one
            // would part two renders of one snapshot on its own.
            refreshed_at: Some(
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000),
            ),
            ..Snapshot::default()
        }
    }

    /// The same snapshot as the board would read it with the operator's agent
    /// never registered: the registry row goes, the sessions the server projects
    /// onto it go, because a role that is not in the registry can have neither,
    /// and so does every `allowed_targets` name pointing at it, because a name
    /// with no row behind it is no hop the map draws. The ledger and the faults
    /// stay: they are records of messages that name principals, not rows of the
    /// registry.
    fn without_supervisor(snapshot: &Snapshot) -> Snapshot {
        Snapshot {
            roles: snapshot
                .roles
                .iter()
                .filter(|role| !hidden_role(&role.name))
                .cloned()
                .map(|mut role| {
                    role.edges.retain(|edge| !hidden_role(edge));
                    role
                })
                .collect(),
            sessions: snapshot
                .sessions
                .iter()
                .filter(|session| session.role.as_deref() != Some(SUPERVISOR_ROLE))
                .cloned()
                .collect(),
            ..snapshot.clone()
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
        assert!(text.matches('╭').count() >= 1, "{text}");
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
    }

    /// The server answers `sessions` `ORDER BY updated_at DESC`, so a heartbeat
    /// or a fresh session permutes that slice on every refresh. One logical
    /// snapshot, permuted, must render byte for byte the same: the role boxes,
    /// the page-2 table and the row its cursor highlights all read the same
    /// canonical order, and the box width reads neither the order nor the clock.
    ///
    /// The four sessions are built so a width taken from "the first two rows"
    /// would differ between the two orders: two carry a long task id with a
    /// two-cell age, two a short task id with a three-cell age.
    #[test]
    fn a_permuted_session_order_renders_the_same_picture() {
        let snapshot = Snapshot {
            status: serde_json::json!({"cluster": "local"}),
            roles: vec![role("planner", &["builder"]), role("builder", &[])],
            sessions: vec![
                session_aged("aaaaaaaa-1", 5),
                session_aged("bbbbbbbb-2", 300),
                session_aged("c-3", 12 * 3600),
                session_aged("d-4", 20 * 86_400),
            ],
            server_online: true,
            // A fixed clock: the footer prints the refresh time, and a live one
            // would part two renders of one snapshot on its own.
            refreshed_at: Some(
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000),
            ),
            ..Snapshot::default()
        };
        let mut permuted = snapshot.clone();
        permuted.sessions.reverse();

        // The width the map lays its boxes out at is a function of the session
        // set alone, so permuting the slice a node carries cannot move it: a
        // width that followed the front of the list would resize every box and
        // re-route every hop on the refresh that reshuffled it.
        let nodes = layout_nodes(&snapshot, false);
        let mut permuted_nodes = nodes.clone();
        for node in &mut permuted_nodes {
            node.sessions.reverse();
        }
        assert_eq!(
            layout::box_width(&nodes),
            layout::box_width(&permuted_nodes),
            "the box width turned on the session order"
        );

        for (label, state) in [
            ("page 1", UiState::default()),
            (
                "page 2",
                UiState {
                    page: Page::Swarm,
                    ..UiState::default()
                },
            ),
        ] {
            assert_eq!(
                render_once_text(&snapshot, &state, 120, 36),
                render_once_text(&permuted, &state, 120, 36),
                "{label} moved when only the session order changed"
            );
        }
    }

    /// The board draws the cluster, and `_supervisor` is the operator's own
    /// seat on it: a registry entry whose key registers the operator identity,
    /// with no client behind it (decision D15). Nothing of it reaches the
    /// screen, so a snapshot that carries it draws the very bytes the same
    /// snapshot draws without it — the same map, the same hops both ways, the
    /// same rows in the role lists, the same sessions, and the same pane the
    /// cursor would open on it.
    #[test]
    fn a_registered_supervisor_draws_nothing() {
        let snapshot = supervised_snapshot();
        let stripped = without_supervisor(&snapshot);
        // The subject is really here, so the equality below cannot hold for
        // want of a fixture.
        assert!(
            snapshot.roles.iter().any(|role| hidden_role(&role.name)),
            "the fixture registers no {}",
            SUPERVISOR_ROLE
        );
        assert!(
            snapshot
                .sessions
                .iter()
                .any(|session| session.role.as_deref() == Some(SUPERVISOR_ROLE)),
            "the fixture projects no session onto {}",
            SUPERVISOR_ROLE
        );
        // A fixed clock and fixed session ages keep two renders of one snapshot
        // together, so the only difference either render can see is the two
        // snapshots.
        assert_eq!(
            render_once_text(&snapshot, &UiState::default(), 120, 36),
            render_once_text(&snapshot, &UiState::default(), 120, 36),
            "the fixture does not render the same way twice"
        );

        let states = [
            ("page 1", UiState::default()),
            (
                "page 1 with the cursor left on it",
                UiState {
                    role_selected: Some(SUPERVISOR_ROLE.into()),
                    ..UiState::default()
                },
            ),
            (
                "page 2 over every session",
                UiState {
                    page: Page::Swarm,
                    active_only: false,
                    ..UiState::default()
                },
            ),
        ];
        for (label, state) in states {
            for (width, height) in [(120, 36), (90, 24)] {
                assert_eq!(
                    render_once_text(&snapshot, &state, width, height),
                    render_once_text(&stripped, &state, width, height),
                    "{label} at {width}x{height} drew the {SUPERVISOR_ROLE}"
                );
            }
        }
        assert!(
            !render_once_text(&snapshot, &UiState::default(), 120, 36).contains(SUPERVISOR_ROLE),
            "page 1 printed the hidden role"
        );

        // Its row in the role lists: the page-2 `o` filter stops on the same
        // names either way, and never on the hidden one.
        let (mut with, mut without) = (UiState::default(), UiState::default());
        for _ in 0..4 {
            cycle_role(&snapshot, &mut with.filter);
            cycle_role(&stripped, &mut without.filter);
            assert_eq!(
                with.filter.role, without.filter.role,
                "the role list moved apart"
            );
            assert!(
                !with.filter.role.as_deref().is_some_and(hidden_role),
                "the role list stops on {:?}",
                with.filter.role
            );
        }
    }

    /// One aggregate role's registry row whose single out-edge under test points
    /// at `target`; `None` leaves that hop out of the edge list. The operator's
    /// own seat is registered beside it, the way a real registry carries it.
    fn aggregate_edge_snapshot(target: Option<&str>) -> Snapshot {
        let mut planner = role("planner", &[]);
        planner.aggregate = Some("cluster-x".into());
        planner.edges = target
            .map(|target| vec![target.to_string()])
            .unwrap_or_default();
        Snapshot {
            status: serde_json::json!({"cluster": "local"}),
            roles: vec![role(SUPERVISOR_ROLE, &[]), planner, role("builder", &[])],
            server_online: true,
            refreshed_at: Some(
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000),
            ),
            ..Snapshot::default()
        }
    }

    /// Whether the map landed a hop's arrowhead somewhere on this page. Only a
    /// drawn hop has one; the pan and page keys spell other arrows.
    fn hop_tip(text: &str) -> bool {
        ['▸', '◂', '▴', '▾'].iter().any(|tip| text.contains(*tip))
    }

    /// A spoke is drawn only when the map draws both of its ends. An aggregate
    /// role's edge list is the registry's own, verbatim, so it can name the
    /// operator's seat ([`SUPERVISOR_ROLE`]) — a role no view draws a box for.
    /// That hop is no hop at all: the page renders the bytes it renders with the
    /// hop left out. The same edge, pointed at a role that is on the board,
    /// draws the spoke it always did.
    #[test]
    fn a_spoke_draws_only_when_the_map_draws_both_its_ends() {
        let pointed = aggregate_edge_snapshot(Some(SUPERVISOR_ROLE));
        let hidden = aggregate_edge_snapshot(None);
        let drawn = aggregate_edge_snapshot(Some("builder"));
        // The subject is really here, so the equality below cannot hold for want
        // of an edge, nor for want of the seat it points at.
        assert!(
            pointed.roles.iter().any(|role| role
                .edges
                .iter()
                .any(|edge| edge.as_str() == SUPERVISOR_ROLE)),
            "the fixture carries no edge toward {SUPERVISOR_ROLE}"
        );
        assert!(
            pointed
                .roles
                .iter()
                .any(|role| role.name == SUPERVISOR_ROLE),
            "the fixture registers no {}",
            SUPERVISOR_ROLE
        );

        for (width, height) in [(120, 36), (90, 24)] {
            let toward_hidden = render_once_text(&pointed, &UiState::default(), width, height);
            let without_hop = render_once_text(&hidden, &UiState::default(), width, height);
            let toward_drawn = render_once_text(&drawn, &UiState::default(), width, height);
            assert_eq!(
                toward_hidden, without_hop,
                "the hop to {SUPERVISOR_ROLE} drew at {width}x{height}"
            );
            assert!(
                !hop_tip(&without_hop),
                "the fixture drew a hop of its own\n{without_hop}"
            );
            assert!(
                hop_tip(&toward_drawn),
                "the hop to a drawn role is gone\n{toward_drawn}"
            );
            // Both pages carry the same two boxes, so what the repointed hop
            // changes is the hop and nothing else.
            for text in [&toward_hidden, &toward_drawn] {
                assert!(text.contains("╭─⬡planner"), "{text}");
                assert!(text.contains("╭─builder"), "{text}");
            }
        }
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

    /// A role whose panel is far taller than any pane, so the scroll has
    /// somewhere to go.
    fn long_role_detail() -> RoleDetail {
        RoleDetail {
            role: "builder".into(),
            state: Presence::Online,
            session_count: 40,
            max_sessions: 4,
            queued: 0,
            admin: false,
            aggregate: None,
            peers: vec!["planner".into()],
            faults: Vec::new(),
            sessions: (0..40)
                .map(|i| session(&format!("s{i:02}"), Lifecycle::Working, AgentPhase::Running))
                .collect(),
        }
    }

    #[test]
    fn the_detail_panel_wraps_the_way_its_extent_says() {
        assert_eq!(wrapped_rows("", 20), 1);
        assert_eq!(wrapped_rows("short", 20), 1);
        assert_eq!(wrapped_rows("abcdefghijklm", 10), 2);
        assert_eq!(wrapped_rows("aaaa bbbb", 9), 1);
        assert_eq!(wrapped_rows("aaaa bbbb", 8), 2);
    }

    #[test]
    fn page_two_scrolls_the_detail_panel_only_as_far_as_its_text() {
        let pane = Rect::new(0, 0, 60, 12);
        let mut state = UiState {
            page: Page::Swarm,
            detail: Some(Detail::Role(long_role_detail())),
            ..UiState::default()
        };
        let extent = detail_extent(&detail_body(&state), pane);
        assert!(extent.lines > extent.view, "{extent:?}");

        state.detail_scroll = u16::MAX;
        clamp_detail_scroll(&mut state, pane);
        assert_eq!(
            state.detail_scroll as usize, extent.max,
            "the scroll stops on the last page of text"
        );

        // A panel with nothing more to show does not scroll at all.
        let mut short = UiState {
            page: Page::Swarm,
            ..UiState::default()
        };
        short.detail_scroll = 5;
        clamp_detail_scroll(&mut short, pane);
        assert_eq!(short.detail_scroll, 0);
    }

    #[test]
    fn a_scrollable_detail_panel_reports_where_it_stands() {
        let state = UiState {
            page: Page::Swarm,
            detail: Some(Detail::Role(long_role_detail())),
            ..UiState::default()
        };
        let text = render_once_text(&linked_snapshot(), &state, 120, 30);
        assert!(
            text.contains("▲▼"),
            "the panel says there is more text below\n{text}"
        );
        assert!(text.contains("role builder  1-"), "{text}");
    }

    #[test]
    fn the_role_panel_leaves_the_exited_sessions_out() {
        let mut detail = long_role_detail();
        detail.sessions = vec![
            session("live", Lifecycle::Working, AgentPhase::Running),
            session("gone", Lifecycle::Exited, AgentPhase::Running),
        ];
        let (title, body) = detail_text(&Detail::Role(detail));
        assert_eq!(title, "role builder");
        assert!(body.contains("life=working"), "{body}");
        assert!(
            !body.contains("life=exited"),
            "an exited session is history, not a live slot\n{body}"
        );
    }

    #[test]
    fn a_stale_working_session_shows_the_stale_mark() {
        let mut quiet = session("gone", Lifecycle::Working, AgentPhase::Idle);
        quiet.heartbeat_stale = true;
        let mut detail = long_role_detail();
        detail.sessions = vec![quiet];
        let (_, body) = detail_text(&Detail::Role(detail));
        assert!(
            body.contains("life=working+stale"),
            "a row the server timed out carries the mark the on-call reads\n{body}"
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
        let page_two = render_once_text(&snapshot, &state, 160, 30);
        assert!(page_two.contains("page 2/2 swarm"), "{page_two}");
        assert!(
            page_two.contains("F session"),
            "page 2 advertises the session-focus key\n{page_two}"
        );
        assert!(
            page_two.contains("f state"),
            "page 2 keeps the state filter on lowercase f\n{page_two}"
        );
    }

    #[test]
    fn page_one_footer_advertises_session_focus_key() {
        let snapshot = linked_snapshot();
        let text = render_once_text(&snapshot, &UiState::default(), 160, 36);
        assert!(
            text.contains("F session"),
            "page 1 advertises the session-focus key\n{text}"
        );
    }

    #[test]
    fn footer_shows_focus_no_socket() {
        let snapshot = linked_snapshot();
        let state = UiState {
            page: Page::Swarm,
            message: crate::model::focus_message(&crate::model::FocusOutcome::NoSocket),
            ..UiState::default()
        };
        let text = render_once_text(&snapshot, &state, 160, 30);
        assert!(text.contains("focus: no socket"), "{text}");
    }

    #[test]
    fn footer_shows_focus_acl_denied() {
        let snapshot = linked_snapshot();
        let state = UiState {
            page: Page::Swarm,
            message: crate::model::focus_message(&crate::model::FocusOutcome::Denied {
                code: "acl_denied".into(),
                message: "owner only".into(),
            }),
            ..UiState::default()
        };
        let text = render_once_text(&snapshot, &state, 160, 30);
        assert!(text.contains("focus acl_denied owner only"), "{text}");
    }

    #[test]
    fn footer_shows_focus_forbidden() {
        let snapshot = linked_snapshot();
        let state = UiState {
            page: Page::Swarm,
            message: crate::model::focus_message(&crate::model::FocusOutcome::Denied {
                code: "forbidden".into(),
                message: "not admin".into(),
            }),
            ..UiState::default()
        };
        let text = render_once_text(&snapshot, &state, 160, 30);
        assert!(text.contains("focus forbidden not admin"), "{text}");
    }

    #[test]
    fn footer_shows_focus_settled_for_an_ok_reply() {
        let snapshot = linked_snapshot();
        let state = UiState {
            page: Page::Swarm,
            message: crate::model::focus_message(&crate::model::FocusOutcome::Settled {
                task_id: "task-9".into(),
            }),
            ..UiState::default()
        };
        let text = render_once_text(&snapshot, &state, 160, 30);
        assert!(text.contains("focus settled task-9"), "{text}");
    }

    #[test]
    fn the_role_panel_names_the_state_the_slots_and_the_peers() {
        let detail = Detail::Role(RoleDetail {
            role: "builder".into(),
            state: Presence::Draining,
            session_count: 2,
            max_sessions: 3,
            queued: 0,
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

    /// A task panel row names its settlement reason when the row carries one;
    /// a settled row prints exactly as it did before the column reached the
    /// board, trailing space and all.
    #[test]
    fn the_task_panel_prints_the_settlement_reason_only_when_a_row_has_one() {
        let settled = ledger_row("planner", "builder", "abcdef12-3456", LedgerState::Acked);
        let mut vetoed = ledger_row("planner", "builder", "abcdef12-3456", LedgerState::Rejected);
        vetoed.reason = Some("not my scope".into());
        let detail = Detail::Task(TaskDetail {
            task_id: "abcdef12-3456".into(),
            ledger: vec![settled, vetoed],
            sessions: Vec::new(),
            faults: Vec::new(),
        });
        let body = detail_text(&detail).1;
        assert!(
            body.contains("  m1 planner→builder done att=1 \n"),
            "an acked row carries no reason tail\n{body}"
        );
        assert!(
            body.contains("  m1 planner→builder failed att=1 reason=not my scope\n"),
            "a rejected row names the refusal the receiver stored\n{body}"
        );
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
            queued: 0,
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

    /// One settled hop's row: the state it reached, and the row's slack spent on the
    /// reason the ledger stored. The row's cells hold their places — the tail reaches
    /// the feed through the cells the hop left behind and through no other cell.
    #[test]
    fn the_history_row_carries_the_reason_the_ledger_stored() {
        let sentence = "the pane backend refused the session command on its stdio";
        let text = render_once_text(
            &history_snapshot(vec![
                history_row(LedgerState::Rejected, Some("session_dead")),
                history_row(LedgerState::Rejected, Some(sentence)),
                history_row(LedgerState::Rejected, Some("   ")),
                history_row(LedgerState::Acked, None),
            ]),
            &swarm_page(),
            160,
            30,
        );
        let rows = history_rows(&text);
        assert_eq!(rows.len(), 4, "{text}");
        assert!(
            rows[0].contains("planner→builder reason=session_dead"),
            "the reason reaches the row\n{text}"
        );
        assert!(
            rows[1].contains("planner→builder reason=the pane back…"),
            "a sentence the column cannot hold is cut on a character boundary with the \
             ellipsis, and the hop keeps every cell it had\n{text}"
        );
        assert!(
            !text.contains("on its stdio"),
            "the tail stops at the column's edge\n{text}"
        );
        assert_eq!(
            text.matches("reason=").count(),
            2,
            "a row whose reason is blank, and a row with no reason at all, print the \
             hop alone\n{text}"
        );
        // The tail spends the hop column's own slack, so no other cell of the row
        // moves: the state and task cells hold the places a row without a reason has
        // for them.
        let state_cell = cell_column(&rows[2], "rejected").expect("the state cell");
        let task_cell = cell_column(&rows[3], "abcdef12").expect("the task cell");
        assert!(
            rows[0].starts_with("12:34:56   ledger   planner→builder reason=session_dead"),
            "the hop keeps its own cells ahead of the tail\n{text}"
        );
        for row in &rows {
            assert_eq!(
                cell_column(row, "abcdef12"),
                Some(task_cell),
                "{row}\n{text}"
            );
            assert_eq!(
                row.chars().count(),
                rows[3].chars().count(),
                "{row}\n{text}"
            );
        }
        for row in &rows[..3] {
            assert_eq!(
                cell_column(row, "rejected"),
                Some(state_cell),
                "{row}\n{text}"
            );
        }
    }

    /// The hop floor is where a row's slack runs out. In a pane that has only the floor
    /// to give, a reason reaches no row and the feed prints the bytes it printed before
    /// the field arrived — the layout promise the board keeps, which is a different
    /// promise from the one `onlyne ledger`'s help makes about the answer's key.
    #[test]
    fn a_history_row_without_slack_keeps_the_row_it_printed() {
        let text = render_once_text(
            &history_snapshot(vec![
                history_row(LedgerState::Rejected, Some("session_dead")),
                history_row(LedgerState::Acked, None),
            ]),
            &swarm_page(),
            120,
            30,
        );
        assert!(
            !text.contains("reason="),
            "a pane at the hop's floor has no slack to spend\n{text}"
        );
        assert_eq!(
            history_rows(&text),
            vec![
                "12:34:56   ledger   planner→builder   rejected   abcdef12 ".to_string(),
                "12:34:56   ledger   planner→builder   acked      abcdef12 ".to_string(),
            ],
            "the feed moved a row it had no room to change\n{text}"
        );
    }
}
