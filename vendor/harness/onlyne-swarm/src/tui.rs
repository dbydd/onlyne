use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use chrono::{Local, TimeZone};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, Wrap},
    Terminal,
};

const STATES: [&str; 8] = [
    "active",
    "all",
    "pending",
    "running",
    "done",
    "failed",
    "cancelled",
    "closed",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Graph,
    History,
}

impl Focus {
    fn toggle(self) -> Self {
        match self {
            Self::Graph => Self::History,
            Self::History => Self::Graph,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Graph => "graph",
            Self::History => "history",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TimeWindow {
    Any,
    Hour,
    Day,
    Week,
}

impl TimeWindow {
    fn next(self) -> Self {
        match self {
            Self::Any => Self::Hour,
            Self::Hour => Self::Day,
            Self::Day => Self::Week,
            Self::Week => Self::Any,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::Hour => "1h",
            Self::Day => "24h",
            Self::Week => "7d",
        }
    }

    fn since(self) -> Option<i64> {
        let now = chrono::Utc::now().timestamp();
        match self {
            Self::Any => None,
            Self::Hour => Some(now - 60 * 60),
            Self::Day => Some(now - 24 * 60 * 60),
            Self::Week => Some(now - 7 * 24 * 60 * 60),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct EdgeFilter {
    from: String,
    to: String,
}

#[derive(Clone, Debug)]
struct HistoryFilter {
    state: String,
    role: Option<String>,
    edge: Option<EdgeFilter>,
    text: String,
    window: TimeWindow,
    retry_only: bool,
    offset: usize,
}

impl Default for HistoryFilter {
    fn default() -> Self {
        Self {
            state: "active".into(),
            role: None,
            edge: None,
            text: String::new(),
            window: TimeWindow::Any,
            retry_only: false,
            offset: 0,
        }
    }
}

impl HistoryFilter {
    fn label(&self) -> String {
        let role = self.role.as_deref().unwrap_or("any");
        let edge = self
            .edge
            .as_ref()
            .map(|edge| format!("{}→{}", edge.from, edge.to))
            .unwrap_or_else(|| "any".into());
        let retry = if self.retry_only { "retry" } else { "any" };
        format!(
            "state={} role={} edge={} text=\"{}\" win={} retry={}",
            self.state,
            role,
            edge,
            self.text,
            self.window.label(),
            retry
        )
    }

    fn reset_page(&mut self) {
        self.offset = 0;
    }
}

#[derive(Clone, Debug, Default)]
struct Snapshot {
    status: serde_json::Value,
    workspaces: Vec<serde_json::Value>,
    graph: serde_json::Value,
    active_tasks: Vec<serde_json::Value>,
    history: Vec<serde_json::Value>,
    history_total: usize,
}

#[derive(Clone, Debug)]
struct UiState {
    focus: Focus,
    graph_cursor: usize,
    history_cursor: usize,
    filter: HistoryFilter,
    detail_task_id: Option<String>,
    detail: Option<serde_json::Value>,
    detail_scroll: u16,
    search: Option<String>,
    message: String,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphLineKind {
    Normal,
    Selected,
    Failed,
    Alert,
    /// R4: lifecycle notices (adopted) — bold, default color. Distinct from
    /// Alert (red) so routine adoption never reads as a fault.
    Notice,
    /// R4 (CR4): hop-active rail edge. Weight carried by Modifier::BOLD
    /// (stroke alone only encodes semantics), so a busy edge stays
    /// distinguishable on de-colored terminals.
    HopActive,
    Legend,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphLine {
    pub text: String,
    pub kind: GraphLineKind,
    pub task_id: Option<String>,
    /// R4: per-character style overrides (e.g. HopActive rail cells) for
    /// cells on this line. `None` entries inherit `kind`. Same length as
    /// `text` after trimming.
    pub overrides: Vec<Option<GraphLineKind>>,
}

#[derive(Clone, Debug, Default)]
pub struct GraphLayout {
    pub lines: Vec<GraphLine>,
    pub task_ids: Vec<String>,
}

/// R4: alert text stays a stable wire-like string. `alert_kind` classifies
/// routine adoption versus red dropped-out entries at render time.
pub type AlertLine = String;

#[derive(Clone, Debug)]
struct RoleNode {
    path: String,
    name: String,
    daemon: bool,
    declared: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EdgeKind {
    Declared,
    Communicated,
    /// R4: edge currently hosting a busy hop.
    HopActive,
}

#[derive(Clone, Debug)]
struct EdgeSpec {
    from: String,
    to: String,
    kind: EdgeKind,
}

/// Open the revision-2 graph/history/detail monitor.
pub fn run_tui(sock: &Path) -> anyhow::Result<()> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut term = Terminal::new(backend)?;
    let result = run_loop(&mut term, sock);
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        term.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen
    )?;
    result
}

fn run_loop(
    term: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    sock: &Path,
) -> anyhow::Result<()> {
    let mut state = UiState::default();
    let mut snapshot = pull(sock, &state.filter, history_page_size(term.size()?.height));
    sync_selection_and_detail(term, sock, &snapshot, &mut state);
    let mut refreshed = Instant::now();

    loop {
        term.draw(|frame| render(frame, &snapshot, &state))?;
        if refreshed.elapsed() >= Duration::from_secs(2) && state.search.is_none() {
            snapshot = pull(sock, &state.filter, history_page_size(term.size()?.height));
            sync_selection_and_detail(term, sock, &snapshot, &mut state);
            refreshed = Instant::now();
        }
        if !event::poll(Duration::from_millis(150))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if handle_search_key(
            key.code,
            sock,
            term,
            &mut snapshot,
            &mut state,
            &mut refreshed,
        )? {
            continue;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Tab => {
                state.focus = state.focus.toggle();
                state.detail_scroll = 0;
                sync_selection_and_detail(term, sock, &snapshot, &mut state);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                move_focus_cursor(term, &snapshot, &mut state, -1);
                sync_selection_and_detail(term, sock, &snapshot, &mut state);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                move_focus_cursor(term, &snapshot, &mut state, 1);
                sync_selection_and_detail(term, sock, &snapshot, &mut state);
            }
            KeyCode::Char('J') => state.detail_scroll = state.detail_scroll.saturating_add(1),
            KeyCode::Char('K') => state.detail_scroll = state.detail_scroll.saturating_sub(1),
            KeyCode::Char('/') => {
                state.search = Some(state.filter.text.clone());
            }
            KeyCode::Char('s') => {
                cycle_state(&mut state.filter);
                refresh_now(term, sock, &mut snapshot, &mut state, &mut refreshed);
            }
            KeyCode::Char('r') => {
                cycle_role(&snapshot.workspaces, &mut state.filter);
                refresh_now(term, sock, &mut snapshot, &mut state, &mut refreshed);
            }
            KeyCode::Char('e') => {
                cycle_edge(&snapshot.graph, &mut state.filter);
                refresh_now(term, sock, &mut snapshot, &mut state, &mut refreshed);
            }
            KeyCode::Char('w') => {
                state.filter.window = state.filter.window.next();
                state.filter.reset_page();
                refresh_now(term, sock, &mut snapshot, &mut state, &mut refreshed);
            }
            KeyCode::Char('x') => {
                state.filter.retry_only = !state.filter.retry_only;
                state.filter.reset_page();
                refresh_now(term, sock, &mut snapshot, &mut state, &mut refreshed);
            }
            KeyCode::PageDown if state.focus == Focus::History => {
                page_history(1, term, sock, &mut snapshot, &mut state, &mut refreshed);
            }
            KeyCode::PageUp if state.focus == Focus::History => {
                page_history(-1, term, sock, &mut snapshot, &mut state, &mut refreshed);
            }
            KeyCode::PageDown => state.detail_scroll = state.detail_scroll.saturating_add(8),
            KeyCode::PageUp => state.detail_scroll = state.detail_scroll.saturating_sub(8),
            KeyCode::Char('c') => {
                if let Some(task_id) = selected_task_id(term, &snapshot, &state) {
                    match req(
                        sock,
                        serde_json::json!({"id":"tui","op":"cancel","task_id":task_id,"reason":"tui cancel"}),
                    ) {
                        Ok(_) => state.message = format!("cancelled {task_id}"),
                        Err(error) => state.message = format!("cancel failed: {error}"),
                    }
                    refresh_now(term, sock, &mut snapshot, &mut state, &mut refreshed);
                }
            }
            KeyCode::Char('f') | KeyCode::Enter => {
                focus_selected(term, &snapshot, &mut state);
            }
            KeyCode::Char('t') => {
                let enabled = snapshot
                    .status
                    .get("swarm_enabled")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(true);
                match req(
                    sock,
                    serde_json::json!({"id":"tui","op":"toggle_swarm","workspace":".","enabled":!enabled}),
                ) {
                    Ok(_) => state.message = format!("swarm -> {}", !enabled),
                    Err(error) => state.message = format!("toggle failed: {error}"),
                }
                refresh_now(term, sock, &mut snapshot, &mut state, &mut refreshed);
            }
            _ => {}
        }
    }
}

fn handle_search_key(
    key: KeyCode,
    sock: &Path,
    term: &Terminal<CrosstermBackend<std::io::Stdout>>,
    snapshot: &mut Snapshot,
    state: &mut UiState,
    refreshed: &mut Instant,
) -> anyhow::Result<bool> {
    let Some(input) = state.search.as_mut() else {
        return Ok(false);
    };
    match key {
        KeyCode::Esc => state.search = None,
        KeyCode::Enter => {
            state.filter.text = input.clone();
            state.filter.reset_page();
            state.search = None;
            refresh_now(term, sock, snapshot, state, refreshed);
        }
        KeyCode::Backspace => {
            input.pop();
        }
        KeyCode::Char(character) => input.push(character),
        _ => {}
    }
    Ok(true)
}

fn refresh_now(
    term: &Terminal<CrosstermBackend<std::io::Stdout>>,
    sock: &Path,
    snapshot: &mut Snapshot,
    state: &mut UiState,
    refreshed: &mut Instant,
) {
    *snapshot = pull(
        sock,
        &state.filter,
        history_page_size(term.size().map(|size| size.height).unwrap_or(30)),
    );
    sync_selection_and_detail(term, sock, snapshot, state);
    *refreshed = Instant::now();
}

fn pull(sock: &Path, filter: &HistoryFilter, page_size: usize) -> Snapshot {
    let status = request_data(sock, serde_json::json!({"id":"tui","op":"status"}));
    let workspaces = request_array(sock, serde_json::json!({"id":"tui","op":"list_workspaces"}));
    let graph = request_data(sock, serde_json::json!({"id":"tui","op":"graph"}));
    let active_tasks = request_page(
        sock,
        serde_json::json!({"id":"tui","op":"list_tasks","state":"active","limit":500,"offset":0}),
    )
    .0;
    let history_request = history_request(filter, page_size);
    let (history, history_total) = request_page(sock, history_request);
    // R4: merge the live hop clock into each active row so the graph colors
    // an overlong busy hop red (design §4) without threading a side map
    // through every layout helper.
    let hops = status
        .get("hops")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let active_tasks: Vec<serde_json::Value> = active_tasks
        .into_iter()
        .map(|mut task| {
            let id = task_id(&task).map(str::to_owned);
            if let (Some(id), Some(obj)) = (id, task.as_object_mut()) {
                if let Some(hop) = hops.get(id) {
                    obj.insert("hop".into(), hop.clone());
                }
            }
            task
        })
        .collect();
    Snapshot {
        status,
        workspaces,
        graph,
        active_tasks,
        history,
        history_total,
    }
}

fn history_request(filter: &HistoryFilter, page_size: usize) -> serde_json::Value {
    let mut request = serde_json::json!({
        "id": "tui",
        "op": "list_tasks",
        "state": filter.state,
        "text": filter.text,
        "retry_only": filter.retry_only,
        "limit": page_size,
        "offset": filter.offset,
    });
    if let Some(role) = &filter.role {
        request["to_ws"] = serde_json::json!(role);
    }
    if let Some(edge) = &filter.edge {
        request["from_ws"] = serde_json::json!(edge.from);
        request["to_ws"] = serde_json::json!(edge.to);
    }
    if let Some(since) = filter.window.since() {
        request["since"] = serde_json::json!(since);
    }
    request
}

fn request_data(sock: &Path, request: serde_json::Value) -> serde_json::Value {
    req(sock, request)
        .ok()
        .and_then(|value| value.get("data").cloned())
        .unwrap_or(serde_json::Value::Null)
}

fn request_array(sock: &Path, request: serde_json::Value) -> Vec<serde_json::Value> {
    serde_json::from_value(request_data(sock, request)).unwrap_or_default()
}

fn request_page(sock: &Path, request: serde_json::Value) -> (Vec<serde_json::Value>, usize) {
    let data = request_data(sock, request);
    let rows = data
        .get("rows")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();
    let total = data
        .get("total")
        .and_then(|value| value.as_u64())
        .unwrap_or(0) as usize;
    (rows, total)
}

fn sync_selection_and_detail(
    term: &Terminal<CrosstermBackend<std::io::Stdout>>,
    sock: &Path,
    snapshot: &Snapshot,
    state: &mut UiState,
) {
    let size = term.size().unwrap_or(ratatui::layout::Size {
        width: 120,
        height: 30,
    });
    let ids = graph_task_ids(snapshot, size.width, size.height);
    clamp_cursor(&mut state.graph_cursor, ids.len());
    clamp_cursor(&mut state.history_cursor, snapshot.history.len());
    let selected = selected_task_id(term, snapshot, state);
    if selected == state.detail_task_id {
        return;
    }
    state.detail_task_id = selected.clone();
    state.detail_scroll = 0;
    let Some(task_id) = selected else {
        state.detail = None;
        return;
    };
    match req(
        sock,
        serde_json::json!({"id":"tui","op":"task_detail","task_id":task_id}),
    ) {
        Ok(value) => {
            if let Some(data) = value.get("data") {
                state.detail = Some(data.clone());
            }
        }
        Err(error) => state.message = format!("detail failed: {error}"),
    }
}

fn move_focus_cursor(
    term: &Terminal<CrosstermBackend<std::io::Stdout>>,
    snapshot: &Snapshot,
    state: &mut UiState,
    delta: isize,
) {
    let len = match state.focus {
        Focus::Graph => {
            let size = term.size().unwrap_or(ratatui::layout::Size {
                width: 120,
                height: 30,
            });
            graph_task_ids(snapshot, size.width, size.height).len()
        }
        Focus::History => snapshot.history.len(),
    };
    let cursor = match state.focus {
        Focus::Graph => &mut state.graph_cursor,
        Focus::History => &mut state.history_cursor,
    };
    move_cursor(cursor, len, delta);
}

fn page_history(
    delta: isize,
    term: &Terminal<CrosstermBackend<std::io::Stdout>>,
    sock: &Path,
    snapshot: &mut Snapshot,
    state: &mut UiState,
    refreshed: &mut Instant,
) {
    let page_size = history_page_size(term.size().map(|size| size.height).unwrap_or(30));
    if delta > 0 {
        if state.filter.offset + page_size < snapshot.history_total {
            state.filter.offset += page_size;
        }
    } else {
        state.filter.offset = state.filter.offset.saturating_sub(page_size);
    }
    state.history_cursor = 0;
    refresh_now(term, sock, snapshot, state, refreshed);
}

fn cycle_state(filter: &mut HistoryFilter) {
    let current = STATES
        .iter()
        .position(|state| *state == filter.state)
        .unwrap_or(0);
    filter.state = STATES[(current + 1) % STATES.len()].into();
    filter.reset_page();
}

fn cycle_role(workspaces: &[serde_json::Value], filter: &mut HistoryFilter) {
    let roles: Vec<String> = workspaces
        .iter()
        .filter_map(|workspace| {
            workspace
                .get("path")
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        })
        .collect();
    filter.role = cycle_option(filter.role.take(), &roles);
    filter.reset_page();
}

fn cycle_edge(graph: &serde_json::Value, filter: &mut HistoryFilter) {
    let edges: Vec<EdgeFilter> = graph
        .get("edges")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|edge| {
            Some(EdgeFilter {
                from: edge.get("from")?.as_str()?.into(),
                to: edge.get("to")?.as_str()?.into(),
            })
        })
        .collect();
    let current = filter.edge.take();
    filter.edge = match current {
        None => edges.first().cloned(),
        Some(value) => edges
            .iter()
            .position(|edge| edge == &value)
            .and_then(|index| edges.get(index + 1).cloned()),
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

fn focus_selected(
    term: &Terminal<CrosstermBackend<std::io::Stdout>>,
    snapshot: &Snapshot,
    state: &mut UiState,
) {
    let Some(task_id) = selected_task_id(term, snapshot, state) else {
        return;
    };
    let task = find_task(snapshot, &task_id);
    let handle = task
        .and_then(|task| task.get("terminal"))
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let id8 = short_id(&task_id);
    if handle.is_empty() || handle.starts_with("stub-") {
        state.message = format!("no live tab for {id8}");
        return;
    }
    match crate::orca_term::focus(handle) {
        Ok(()) => state.message = format!("focused {id8}"),
        Err(error) => state.message = format!("focus failed: {error}"),
    }
}

fn selected_task_id(
    term: &Terminal<CrosstermBackend<std::io::Stdout>>,
    snapshot: &Snapshot,
    state: &UiState,
) -> Option<String> {
    match state.focus {
        Focus::Graph => {
            let size = term.size().unwrap_or(ratatui::layout::Size {
                width: 120,
                height: 30,
            });
            graph_task_ids(snapshot, size.width, size.height)
                .get(state.graph_cursor)
                .cloned()
        }
        Focus::History => snapshot
            .history
            .get(state.history_cursor)
            .and_then(task_id)
            .map(str::to_owned),
    }
}

fn find_task<'a>(snapshot: &'a Snapshot, id: &str) -> Option<&'a serde_json::Value> {
    snapshot
        .history
        .iter()
        .chain(snapshot.active_tasks.iter())
        .find(|task| task_id(task) == Some(id))
}

fn graph_task_ids(snapshot: &Snapshot, terminal_width: u16, terminal_height: u16) -> Vec<String> {
    build_graph_layout(
        &snapshot.workspaces,
        &snapshot.active_tasks,
        &snapshot.graph,
        graph_width(terminal_width).saturating_sub(2),
        graph_height(terminal_height),
        None,
        &alerts(snapshot),
    )
    .task_ids
}

fn graph_width(terminal_width: u16) -> u16 {
    terminal_width
        .saturating_mul(48)
        .saturating_div(100)
        .max(42)
}

fn graph_height(terminal_height: u16) -> u16 {
    terminal_height.saturating_sub(5).max(8)
}

fn history_page_size(terminal_height: u16) -> usize {
    terminal_height
        .saturating_sub(5)
        .saturating_mul(58)
        .saturating_div(100)
        .saturating_sub(4)
        .max(1) as usize
}

fn clamp_cursor(cursor: &mut usize, len: usize) {
    if len == 0 {
        *cursor = 0;
    } else if *cursor >= len {
        *cursor = len - 1;
    }
}

fn move_cursor(cursor: &mut usize, len: usize, delta: isize) {
    if len == 0 {
        *cursor = 0;
        return;
    }
    let next = (*cursor as isize + delta).clamp(0, len.saturating_sub(1) as isize);
    *cursor = next as usize;
}

fn render(frame: &mut ratatui::Frame, snapshot: &Snapshot, state: &UiState) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(8), Constraint::Length(3)])
        .split(frame.area());
    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(48), Constraint::Percentage(52)])
        .split(root[0]);
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(main[1]);

    render_graph(frame, main[0], snapshot, state);
    render_history(frame, right[0], snapshot, state);
    render_detail(frame, right[1], state);
    render_keys(frame, root[1], state);
}

fn render_graph(frame: &mut ratatui::Frame, area: Rect, snapshot: &Snapshot, state: &UiState) {
    let unselected = build_graph_layout(
        &snapshot.workspaces,
        &snapshot.active_tasks,
        &snapshot.graph,
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
        None,
        &alerts(snapshot),
    );
    let selected = if state.focus == Focus::Graph {
        unselected.task_ids.get(state.graph_cursor).cloned()
    } else {
        None
    };
    let layout = build_graph_layout(
        &snapshot.workspaces,
        &snapshot.active_tasks,
        &snapshot.graph,
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
        selected.as_deref(),
        &alerts(snapshot),
    );
    let lines: Vec<Line> = layout
        .lines
        .iter()
        .map(|line| {
            let spans = line
                .text
                .chars()
                .enumerate()
                .map(|(index, ch)| {
                    let kind = line
                        .overrides
                        .get(index)
                        .and_then(|kind| *kind)
                        .unwrap_or(line.kind);
                    Span::styled(ch.to_string(), graph_style(kind))
                })
                .collect::<Vec<_>>();
            Line::from(spans)
        })
        .collect();
    frame.render_widget(
        Paragraph::new(Text::from(lines)).block(
            Block::default()
                .title(if state.focus == Focus::Graph {
                    "graph [focus]"
                } else {
                    "graph"
                })
                .borders(Borders::ALL),
        ),
        area,
    );
}

fn render_history(frame: &mut ratatui::Frame, area: Rect, snapshot: &Snapshot, state: &UiState) {
    let title = format!(
        "history {}   {}/{}",
        state.filter.label(),
        snapshot.history.len(),
        snapshot.history_total
    );
    let rows: Vec<Row> = snapshot
        .history
        .iter()
        .enumerate()
        .map(|(index, task)| {
            let state_name = task_string(task, "state");
            let style = if state.focus == Focus::History && index == state.history_cursor {
                Style::default().add_modifier(Modifier::REVERSED)
            } else if state_name == "failed" {
                Style::default().fg(Color::Red)
            } else if state_name == "cancelled" {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default()
            };
            let handle = task_string(task, "terminal");
            let live = if !handle.is_empty() && !handle.starts_with("stub-") {
                "◉"
            } else {
                ""
            };
            Row::new(vec![
                Cell::from(format_created(
                    task.get("created_at")
                        .and_then(|value| value.as_i64())
                        .unwrap_or(0),
                )),
                Cell::from(short_id(task_id(task).unwrap_or("?"))),
                Cell::from(format!(
                    "{}→{}",
                    task_string(task, "from_ws"),
                    task_string(task, "to_ws")
                )),
                Cell::from(
                    task.get("attempt")
                        .and_then(|value| value.as_u64())
                        .unwrap_or(0)
                        .to_string(),
                ),
                Cell::from(state_name),
                Cell::from(live),
            ])
            .style(style)
        })
        .collect();
    let widths = [
        Constraint::Length(9),
        Constraint::Length(9),
        Constraint::Min(12),
        Constraint::Length(4),
        Constraint::Length(10),
        Constraint::Length(2),
    ];
    frame.render_widget(
        Table::new(rows, widths)
            .header(
                Row::new(["created", "id8", "from→to", "att", "state", ""])
                    .style(Style::default().add_modifier(Modifier::BOLD)),
            )
            .block(
                Block::default()
                    .title(if state.focus == Focus::History {
                        format!("{title} [focus]")
                    } else {
                        title
                    })
                    .borders(Borders::ALL),
            ),
        area,
    );
}

fn render_detail(frame: &mut ratatui::Frame, area: Rect, state: &UiState) {
    let (title, body) = match &state.detail {
        Some(detail) => detail_text(detail),
        None => (
            "session".into(),
            "(select a session to inspect payload and lineage)".into(),
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

fn render_keys(frame: &mut ratatui::Frame, area: Rect, state: &UiState) {
    // R4: compact selected-hop dwell in the status bar, so the operator
    // sees how long the current hop has sat in its substate without
    // opening the detail pane. Adoption shows as `*` (clock from restart).
    let hop_tag = state
        .detail
        .as_ref()
        .and_then(|detail| detail.get("hop"))
        .and_then(|hop| {
            let state = hop.get("state")?.as_str()?;
            let secs = hop.get("secs")?.as_u64()?;
            let mark = if hop
                .get("adopted")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                "*"
            } else {
                ""
            };
            Some(format!("   hop {state} {}{mark}", format_secs(secs)))
        })
        .unwrap_or_default();
    let status = if let Some(search) = &state.search {
        format!("search: {search}  [Enter] apply  [Esc] discard")
    } else {
        format!(
            "[Tab] graph↔history  [↑↓/jk] move  [/] text  [s][r][e][w][x] filter  [J/K] detail  [f]ocus  [c]ancel  [t]oggle  [q]uit   {message}{hop_tag}",
            message = state.message
        )
    };
    frame.render_widget(
        Paragraph::new(status).block(
            Block::default()
                .title(format!("focus: {}", state.focus.label()))
                .borders(Borders::ALL),
        ),
        area,
    );
}

pub(crate) fn build_graph_layout(
    workspaces: &[serde_json::Value],
    active_tasks: &[serde_json::Value],
    graph: &serde_json::Value,
    width: u16,
    height: u16,
    selected_id: Option<&str>,
    alerts: &[AlertLine],
) -> GraphLayout {
    let roles = role_nodes(workspaces);
    if roles.is_empty() || width < 12 || height < 4 {
        return GraphLayout {
            lines: vec![GraphLine {
                text: "(no roles)".into(),
                kind: GraphLineKind::Normal,
                task_id: None,
                overrides: vec![],
            }],
            task_ids: vec![],
        };
    }
    let width = width as usize;
    let legend_rows = 1usize;
    let alert_rows = alerts.len().min(2);
    let usable_height = (height as usize).saturating_sub(legend_rows + alert_rows);
    let role_count = roles.len();
    let body_cap = ((usable_height.saturating_sub(role_count * 3)) / role_count).max(1);
    let rail_width = width.saturating_sub(30).clamp(4, 12);
    let frame_width = width.saturating_sub(rail_width).max(12);
    let planned_height = role_count * (body_cap + 2) + role_count.saturating_sub(1);
    let canvas_height = planned_height.max(1);
    let mut canvas = vec![vec![' '; width]; canvas_height];
    let counts = graph_counts(graph);
    let active_by_role = active_by_role(active_tasks);
    let mut anchors = BTreeMap::new();
    let mut task_ids = vec![];
    let mut line_styles = BTreeMap::new();
    let mut task_for_line = BTreeMap::new();
    let mut rail_cells = vec![];
    let busy_pairs = active_tasks
        .iter()
        .filter(|task| task_string(task, "hop_state") == "busy")
        .map(|task| (task_string(task, "from_ws"), task_string(task, "to_ws")))
        .collect::<BTreeSet<_>>();
    let mut row_y = 0usize;

    for role in &roles {
        anchors.insert(role.path.clone(), row_y);
        draw_text(
            &mut canvas,
            row_y,
            0,
            &box_title(&role.name, role.daemon, frame_width),
        );
        let sessions = active_by_role.get(&role.path).cloned().unwrap_or_default();
        let terminal_summary = terminal_summary(&counts, &role.path);
        let body = role_body(&sessions, &terminal_summary, body_cap, selected_id);
        for (offset, entry) in body.iter().enumerate() {
            let y = row_y + 1 + offset;
            draw_text(&mut canvas, y, 0, &box_body(&entry.text, frame_width));
            if let Some(task_id) = &entry.task_id {
                task_ids.push(task_id.clone());
                task_for_line.insert(y, task_id.clone());
                if selected_id == Some(task_id.as_str()) {
                    line_styles.insert(y, GraphLineKind::Selected);
                }
            }
            if entry.failed {
                line_styles.insert(y, GraphLineKind::Failed);
            }
        }
        draw_text(
            &mut canvas,
            row_y + body_cap + 1,
            0,
            &box_bottom(frame_width),
        );
        row_y += body_cap + 3;
    }

    route_edges(
        &mut canvas,
        frame_width,
        &anchors,
        combined_edges(&roles, graph, &busy_pairs),
        &mut rail_cells,
    );

    let mut lines = vec![];
    for (row, chars) in canvas.into_iter().enumerate() {
        let text = chars.into_iter().collect::<String>().trim_end().to_string();
        let task_id = task_for_line.get(&row).cloned();
        let kind = line_styles
            .get(&row)
            .copied()
            .unwrap_or(GraphLineKind::Normal);
        let mut overrides = vec![None; text.chars().count()];
        for (cell_row, cell_col, style) in rail_cells.iter().copied() {
            if cell_row == row {
                if let Some(slot) = overrides.get_mut(cell_col) {
                    *slot = style;
                }
            }
        }
        lines.push(GraphLine {
            text,
            kind,
            task_id,
            overrides,
        });
    }
    lines.push(GraphLine {
        text: "┄ declared edge   ━ communication observed   ━ hop active   ◉ live tab".into(),
        kind: GraphLineKind::Legend,
        task_id: None,
        overrides: vec![],
    });
    for alert in alerts.iter().take(alert_rows) {
        lines.push(GraphLine {
            text: format!("! {alert}"),
            kind: alert_kind(alert),
            task_id: None,
            overrides: vec![],
        });
    }
    GraphLayout { lines, task_ids }
}

#[derive(Clone, Debug)]
struct RoleBodyLine {
    text: String,
    task_id: Option<String>,
    failed: bool,
}

fn role_body(
    sessions: &[serde_json::Value],
    terminal_summary: &str,
    cap: usize,
    selected_id: Option<&str>,
) -> Vec<RoleBodyLine> {
    if sessions.is_empty() {
        if terminal_summary.is_empty() {
            return vec![RoleBodyLine {
                text: "(idle)".into(),
                task_id: None,
                failed: false,
            }];
        }
        return vec![RoleBodyLine {
            text: terminal_summary.into(),
            task_id: None,
            failed: terminal_summary.contains("failed"),
        }];
    }
    let selected =
        selected_id.and_then(|id| sessions.iter().find(|task| task_id(task) == Some(id)));
    let first = selected.unwrap_or(&sessions[0]);
    let shown = if sessions.len() > cap {
        1
    } else {
        sessions.len().min(cap)
    };
    let mut visible = vec![first];
    for task in sessions {
        if visible.len() >= shown {
            break;
        }
        if task_id(task) != task_id(first) {
            visible.push(task);
        }
    }
    let mut lines: Vec<RoleBodyLine> = visible
        .iter()
        .map(|task| RoleBodyLine {
            text: graph_task_text(task, selected_id == task_id(task)),
            task_id: task_id(task).map(str::to_owned),
            // R4: a failed row is red; so is a busy hop past its role cap
            // (hop_overlong), which stays alive — the color is a signal for
            // the operator, never a kill (design §4, CR6).
            failed: task_string(task, "state") == "failed"
                || task
                    .get("hop")
                    .and_then(|h| h.get("overlong"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
        })
        .collect();
    let hidden = sessions.len().saturating_sub(lines.len());
    if hidden > 0 {
        if lines.len() < cap {
            lines.push(RoleBodyLine {
                text: format!("+{hidden} active"),
                task_id: None,
                failed: false,
            });
        } else if let Some(first) = lines.first_mut() {
            first.text.push_str(&format!(" +{hidden} active"));
        }
    }
    if !terminal_summary.is_empty() && lines.len() < cap {
        lines.push(RoleBodyLine {
            text: terminal_summary.into(),
            task_id: None,
            failed: terminal_summary.contains("failed"),
        });
    }
    while lines.len() < cap {
        lines.push(RoleBodyLine {
            text: String::new(),
            task_id: None,
            failed: false,
        });
    }
    lines
}

fn role_nodes(workspaces: &[serde_json::Value]) -> Vec<RoleNode> {
    workspaces
        .iter()
        .map(|workspace| RoleNode {
            path: workspace_string(workspace, "path"),
            name: workspace_string(workspace, "name"),
            daemon: workspace_string(workspace, "daemon") == "ready",
            declared: workspace
                .get("back_edges")
                .and_then(|value| value.as_array())
                .into_iter()
                .flatten()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect(),
        })
        .collect()
}

fn active_by_role(tasks: &[serde_json::Value]) -> BTreeMap<String, Vec<serde_json::Value>> {
    let mut grouped = BTreeMap::new();
    for task in tasks {
        let state = task_string(task, "state");
        if state != "pending" && state != "running" {
            continue;
        }
        grouped
            .entry(task_string(task, "to_ws"))
            .or_insert_with(Vec::new)
            .push(task.clone());
    }
    grouped
}

fn graph_counts(graph: &serde_json::Value) -> BTreeMap<(String, String), i64> {
    let mut counts = BTreeMap::new();
    for row in graph
        .get("by_ws")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
    {
        let Some(ws) = row.get("ws").and_then(|value| value.as_str()) else {
            continue;
        };
        let Some(state) = row.get("state").and_then(|value| value.as_str()) else {
            continue;
        };
        let n = row.get("n").and_then(|value| value.as_i64()).unwrap_or(0);
        counts.insert((ws.into(), state.into()), n);
    }
    counts
}

fn terminal_summary(counts: &BTreeMap<(String, String), i64>, workspace: &str) -> String {
    let mut parts = vec![];
    let done = counts
        .get(&(workspace.into(), "done".into()))
        .copied()
        .unwrap_or(0)
        + counts
            .get(&(workspace.into(), "closed".into()))
            .copied()
            .unwrap_or(0);
    if done > 0 {
        parts.push(format!("+{done} done"));
    }
    for state in ["failed", "cancelled"] {
        let n = counts
            .get(&(workspace.into(), state.into()))
            .copied()
            .unwrap_or(0);
        if n > 0 {
            parts.push(format!("+{n} {state}"));
        }
    }
    parts.join(" ")
}

/// R4: promote an edge to HopActive when a busy hop currently runs on it.
/// `busy_pairs` is (from_ws, to_ws) per busy active task. Declared-only
/// edges stay thin; communicated history stays default weight; only live
/// work takes the bold rail (CR4).
fn combined_edges(
    roles: &[RoleNode],
    graph: &serde_json::Value,
    busy_pairs: &BTreeSet<(String, String)>,
) -> Vec<EdgeSpec> {
    let known: BTreeSet<&str> = roles.iter().map(|role| role.path.as_str()).collect();
    let mut combined: BTreeMap<(String, String), EdgeKind> = BTreeMap::new();
    for role in roles {
        for target in &role.declared {
            if role.path != *target && known.contains(target.as_str()) {
                combined.insert((role.path.clone(), target.clone()), EdgeKind::Declared);
            }
        }
    }
    for edge in graph
        .get("edges")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
    {
        let Some(from) = edge.get("from").and_then(|value| value.as_str()) else {
            continue;
        };
        let Some(to) = edge.get("to").and_then(|value| value.as_str()) else {
            continue;
        };
        if from != to && known.contains(from) && known.contains(to) {
            combined.insert((from.into(), to.into()), EdgeKind::Communicated);
        }
    }
    combined
        .into_iter()
        .map(|((from, to), kind)| {
            let kind = if kind == EdgeKind::Communicated
                && busy_pairs.contains(&(from.clone(), to.clone()))
            {
                EdgeKind::HopActive
            } else {
                kind
            };
            EdgeSpec { from, to, kind }
        })
        .collect()
}

fn route_edges(
    canvas: &mut [Vec<char>],
    frame_width: usize,
    anchors: &BTreeMap<String, usize>,
    mut edges: Vec<EdgeSpec>,
    rail_cells: &mut Vec<(usize, usize, Option<GraphLineKind>)>,
) {
    if frame_width >= canvas.first().map(Vec::len).unwrap_or(0) {
        return;
    }
    edges.sort_by_key(|edge| {
        let source = anchors.get(&edge.from).copied().unwrap_or(0);
        let target = anchors.get(&edge.to).copied().unwrap_or(0);
        std::cmp::Reverse(source.abs_diff(target))
    });
    let rail_width = canvas[0].len().saturating_sub(frame_width);
    let mut occupied: Vec<Vec<(usize, usize)>> = vec![vec![]; rail_width];
    for edge in edges {
        let Some(source) = anchors.get(&edge.from).copied() else {
            continue;
        };
        let Some(target) = anchors.get(&edge.to).copied() else {
            continue;
        };
        let low = source.min(target);
        let high = source.max(target);
        let channel = occupied.iter().position(|intervals| {
            intervals
                .iter()
                .all(|(start, end)| high < *start || low > *end)
        });
        let Some(channel) = channel else {
            continue;
        };
        occupied[channel].push((low, high));
        let x = frame_width + channel;
        let stroke = match edge.kind {
            EdgeKind::Declared => '┄',
            EdgeKind::Communicated | EdgeKind::HopActive => '━',
        };
        let rail_kind = match edge.kind {
            EdgeKind::HopActive => Some(GraphLineKind::HopActive),
            _ => None,
        };
        for row in low.saturating_add(1)..high {
            put_cell(canvas, row, x, stroke);
            rail_cells.push((row, x, rail_kind));
        }
        put_cell(canvas, source, x, '╴');
        put_cell(canvas, target, x, '◀');
    }
}

fn box_title(name: &str, daemon: bool, width: usize) -> String {
    let dot = if daemon { "●" } else { "○" };
    let prefix = format!("╭ {name} {dot} ");
    let fill = width.saturating_sub(char_len(&prefix) + 1);
    format!("{prefix}{}╮", "─".repeat(fill))
}

fn box_body(body: &str, width: usize) -> String {
    let available = width.saturating_sub(3);
    let body = truncate(body, available);
    let pad = available.saturating_sub(char_len(&body));
    format!("│ {body}{}│", " ".repeat(pad))
}

fn box_bottom(width: usize) -> String {
    format!("╰{}╯", "─".repeat(width.saturating_sub(2)))
}

fn graph_task_text(task: &serde_json::Value, selected: bool) -> String {
    let marker = if selected { "▶" } else { "·" };
    let state = task_string(task, "state");
    // R4: the hop substate refines the running symbol: busy works, idle
    // waits, dispatched/ready are handshake intermediates.
    let hop = task_string(task, "hop_state");
    let symbol = match hop.as_str() {
        "busy" => "■",
        "idle" => "◌",
        "dispatched" | "ready" => "…",
        _ if state == "running" => "■",
        _ => "·",
    };
    let label = if hop.is_empty() {
        state.as_str()
    } else {
        hop.as_str()
    };
    let handle = task_string(task, "terminal");
    let live = if !handle.is_empty() && !handle.starts_with("stub-") {
        " ◉"
    } else {
        ""
    };
    format!(
        "{marker} {symbol} {} {label}{live}",
        short_id(task_id(task).unwrap_or("?"))
    )
}

fn draw_text(canvas: &mut [Vec<char>], row: usize, column: usize, text: &str) {
    for (offset, character) in text.chars().enumerate() {
        put_cell(canvas, row, column + offset, character);
    }
}

fn put_cell(canvas: &mut [Vec<char>], row: usize, column: usize, character: char) {
    if let Some(line) = canvas.get_mut(row) {
        if let Some(cell) = line.get_mut(column) {
            *cell = character;
        }
    }
}

fn graph_style(kind: GraphLineKind) -> Style {
    match kind {
        GraphLineKind::Normal => Style::default(),
        GraphLineKind::Selected => Style::default().add_modifier(Modifier::REVERSED),
        GraphLineKind::Failed => Style::default().fg(Color::Red),
        GraphLineKind::Alert => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        GraphLineKind::Notice => Style::default().add_modifier(Modifier::BOLD),
        GraphLineKind::HopActive => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        GraphLineKind::Legend => Style::default().fg(Color::DarkGray),
    }
}

/// R4: classify a status alert by content. Adoption is routine (Notice,
/// bold default); dropped-out rows are real loss (Alert, red).
fn alert_kind(text: &str) -> GraphLineKind {
    if text.starts_with("dropped-out:") {
        GraphLineKind::Alert
    } else if text.starts_with("adopted:") {
        GraphLineKind::Notice
    } else {
        GraphLineKind::Alert
    }
}

fn alerts(snapshot: &Snapshot) -> Vec<AlertLine> {
    let mut alerts = vec![];
    for orphan in snapshot
        .status
        .get("orphans")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
    {
        alerts.push(format!(
            "orphan-instance: {}",
            orphan.as_str().unwrap_or("?")
        ));
    }
    for dangling in snapshot
        .status
        .get("dangling")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
    {
        alerts.push(format!(
            "dangling-link: {}",
            dangling.as_str().unwrap_or("?")
        ));
    }
    for legacy in snapshot
        .status
        .get("legacy_views")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
    {
        alerts.push(format!("legacy-view: {}", legacy.as_str().unwrap_or("?")));
    }
    for recent in snapshot
        .status
        .get("alerts")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
    {
        let text = recent.as_str().unwrap_or("?").to_string();
        alerts.push(text);
    }
    alerts
}

pub(crate) fn detail_text(detail: &serde_json::Value) -> (String, String) {
    let task = detail
        .get("task")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let id = task_id(&task).unwrap_or("?");
    let state = task_string(&task, "state");
    let terminal = task_string(&task, "terminal");
    let terminal_state: String = if terminal.is_empty() {
        "no tab".into()
    } else if terminal.starts_with("stub-") {
        "stub".into()
    } else {
        "◉ live".into()
    };
    let parent = detail
        .get("parent")
        .and_then(|value| value.as_object())
        .map(|parent| {
            format!(
                "{} {}",
                short_id(
                    parent
                        .get("task_id")
                        .and_then(|value| value.as_str())
                        .unwrap_or("?")
                ),
                parent
                    .get("state")
                    .and_then(|value| value.as_str())
                    .unwrap_or("?")
            )
        })
        .unwrap_or_else(|| "(root)".into());
    let children = detail
        .get("children")
        .and_then(|value| value.as_array())
        .map(|children| {
            if children.is_empty() {
                "(none)".into()
            } else {
                children
                    .iter()
                    .map(|child| {
                        format!(
                            "{} {}",
                            short_id(task_id(child).unwrap_or("?")),
                            task_string(child, "state")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        })
        .unwrap_or_else(|| "(none)".into());
    let payload = task_string(&task, "payload");
    let out_head = task_string(&task, "out_head");
    let reason = task_string(&task, "reason");
    let created = task
        .get("created_at")
        .and_then(|value| value.as_i64())
        .unwrap_or(0);
    // R4: live hop clock from task_detail. The adopted marker labels the
    // dwell as "since this scheduler restart", never as the session age.
    let hop_line = match detail.get("hop") {
        Some(hop) if hop.is_object() => {
            let state = hop.get("state").and_then(|v| v.as_str()).unwrap_or("?");
            let secs = hop.get("secs").and_then(|v| v.as_u64()).unwrap_or(0);
            let adopted = hop
                .get("adopted")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let mark = if adopted { " (adopted)" } else { "" };
            format!("hop {state} for {}{mark}", format_secs(secs))
        }
        _ => "hop (none)".into(),
    };
    let body = format!(
        "{} → {}   {}   attempt {}   {}\n{}\n\nterminal: {}  {}\nparent: {}\nchildren: {}\n\npayload\n{}\n\nout\n{}\n\nreason\n{}",
        task_string(&task, "from_ws"),
        task_string(&task, "to_ws"),
        state,
        task.get("attempt").and_then(|value| value.as_u64()).unwrap_or(0),
        format_created(created),
        hop_line,
        if terminal.is_empty() { "(none)" } else { &terminal },
        terminal_state,
        parent,
        children,
        if payload.is_empty() { "(empty)" } else { &payload },
        if out_head.is_empty() { "(not written)" } else { &out_head },
        if reason.is_empty() { "(empty)" } else { &reason },
    );
    (format!("session {}", short_id(id)), body)
}

fn format_secs(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m{:02}s", secs / 60, secs % 60)
    }
}

fn format_created(epoch: i64) -> String {
    Local
        .timestamp_opt(epoch, 0)
        .single()
        .map(|time| time.format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "--:--:--".into())
}

fn task_id(task: &serde_json::Value) -> Option<&str> {
    task.get("task_id").and_then(|value| value.as_str())
}

fn task_string(task: &serde_json::Value, key: &str) -> String {
    task.get(key)
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_owned()
}

fn workspace_string(workspace: &serde_json::Value, key: &str) -> String {
    workspace
        .get(key)
        .and_then(|value| value.as_str())
        .unwrap_or("?")
        .to_owned()
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

fn truncate(value: &str, max: usize) -> String {
    if char_len(value) <= max {
        return value.into();
    }
    if max <= 1 {
        return value.chars().take(max).collect();
    }
    format!("{}…", value.chars().take(max - 1).collect::<String>())
}

fn char_len(value: &str) -> usize {
    value.chars().count()
}

fn req(sock: &Path, request: serde_json::Value) -> anyhow::Result<serde_json::Value> {
    let mut stream = UnixStream::connect(sock)?;
    let mut line = serde_json::to_string(&request)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    let mut reader = BufReader::new(&stream);
    let mut response = String::new();
    reader.read_line(&mut response)?;
    let value: serde_json::Value = serde_json::from_str(&response)?;
    if value.get("ok").and_then(|ok| ok.as_bool()) == Some(false) {
        let message = value
            .pointer("/error/message")
            .and_then(|message| message.as_str())
            .unwrap_or("swarm request failed");
        anyhow::bail!(message.to_owned());
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn role(path: &str, name: &str, daemon: &str, back_edges: &[&str]) -> serde_json::Value {
        serde_json::json!({
            "path": path,
            "name": name,
            "daemon": daemon,
            "back_edges": back_edges,
        })
    }

    fn task(id: &str, from: &str, to: &str, state: &str, terminal: &str) -> serde_json::Value {
        serde_json::json!({
            "task_id": id,
            "from_ws": from,
            "to_ws": to,
            "state": state,
            "terminal": terminal,
            "attempt": 1,
            "created_at": 100,
        })
    }

    #[test]
    fn graph_layout_groups_roles_routes_edges_and_folds_sessions() {
        let workspaces = vec![
            role(".", "root", "ready", &["model"]),
            role("scout", "scout", "offline", &[]),
            role("model", "model", "ready", &["."]),
        ];
        let active = vec![
            task("11111111-a", ".", "scout", "running", "orca-a"),
            task("22222222-b", ".", "scout", "pending", ""),
            task("33333333-c", "scout", "model", "running", "stub-c"),
        ];
        let graph = serde_json::json!({
            "by_ws": [
                {"ws":"scout","state":"done","n":12},
                {"ws":"model","state":"failed","n":2}
            ],
            "edges": [
                {"from":".","to":"scout","n":3},
                {"from":"scout","to":"model","n":1}
            ]
        });
        let layout = build_graph_layout(
            &workspaces,
            &active,
            &graph,
            52,
            20,
            Some("11111111-a"),
            &[],
        );
        let text = layout
            .lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("root ●"));
        assert!(text.contains("scout ○"));
        assert!(text.contains("+12 done"));
        assert!(text.contains("◉"));
        assert!(text.contains('━'));
        assert!(text.contains('┄'));
        assert_eq!(
            layout.task_ids,
            vec!["11111111-a", "22222222-b", "33333333-c"]
        );
        assert!(layout
            .lines
            .iter()
            .any(|line| line.kind == GraphLineKind::Selected));
    }

    #[test]
    fn graph_layout_keeps_idle_role_and_alerts_visible() {
        let workspaces = vec![role(".", "root", "ready", &[])];
        let layout = build_graph_layout(
            &workspaces,
            &[],
            &serde_json::json!({"by_ws":[],"edges":[]}),
            42,
            8,
            None,
            &["orphan-instance: ghost".into()],
        );
        let text = layout
            .lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("(idle)"));
        assert!(text.contains("orphan-instance: ghost"));
        assert!(layout
            .lines
            .iter()
            .any(|line| line.kind == GraphLineKind::Alert));
    }

    #[test]
    fn hop_symbols_and_overlong_red_and_adopted_detail() {
        // R4 §4: busy ■, idle ◌, dispatched … ; overlong busy colors the
        // task line red; an adopted hop shows the (adopted) dwell marker.
        let busy = serde_json::json!({"task_id":"bbbbbbbb-1","state":"running","hop_state":"busy","from_ws":".","to_ws":"a","terminal":"term-1"});
        assert!(graph_task_text(&busy, false).contains('■'));
        assert!(graph_task_text(&busy, false).contains("busy"));
        let idle = serde_json::json!({"task_id":"iiiiiiii-1","state":"running","hop_state":"idle","from_ws":".","to_ws":"a"});
        assert!(graph_task_text(&idle, false).contains('◌'));
        let disp = serde_json::json!({"task_id":"dddddddd-1","state":"running","hop_state":"dispatched","from_ws":".","to_ws":"a"});
        assert!(graph_task_text(&disp, false).contains('…'));
        // overlong red: a hop object with overlong=true renders Failed.
        let overlong = serde_json::json!({"task_id":"oooooooo-1","state":"running","hop_state":"busy","from_ws":".","to_ws":"a","hop":{"state":"busy","secs":9999,"overlong":true}});
        let workspaces = vec![
            role(".", "root", "ready", &[]),
            role("a", "worker", "ready", &[]),
        ];
        let layout = build_graph_layout(
            &workspaces,
            &[overlong],
            &serde_json::json!({"by_ws":[],"edges":[]}),
            60,
            12,
            None,
            &[],
        );
        assert!(layout
            .lines
            .iter()
            .any(|line| line.task_id.as_deref() == Some("oooooooo-1")
                && line.kind == GraphLineKind::Failed));
        // adopted detail marker
        let detail = serde_json::json!({
            "task": {"task_id":"aaaaaaaa-1234","from_ws":".","to_ws":"a","attempt":1,"state":"running","terminal":"","created_at":0,"payload":"p","out_head":"","reason":"","ledger_state":"adopted"},
            "hop": {"state":"busy","secs":95,"adopted":true},
            "parent": null, "children": []
        });
        let (_, body) = detail_text(&detail);
        assert!(body.contains("hop busy for 1m35s (adopted)"), "{body}");
    }

    #[test]
    fn history_filter_cycles_and_builds_predicates() {
        let mut filter = HistoryFilter::default();
        cycle_state(&mut filter);
        assert_eq!(filter.state, "all");
        cycle_role(
            &vec![
                role(".", "root", "ready", &[]),
                role("scout", "scout", "ready", &[]),
            ],
            &mut filter,
        );
        assert_eq!(filter.role.as_deref(), Some("."));
        let graph = serde_json::json!({"edges":[{"from":".","to":"scout","n":1}]});
        cycle_edge(&graph, &mut filter);
        assert_eq!(
            filter.edge,
            Some(EdgeFilter {
                from: ".".into(),
                to: "scout".into()
            })
        );
        filter.window = TimeWindow::Day;
        filter.retry_only = true;
        filter.text = "needle".into();
        let request = history_request(&filter, 25);
        assert_eq!(
            request.get("state").and_then(|value| value.as_str()),
            Some("all")
        );
        assert_eq!(
            request.get("from_ws").and_then(|value| value.as_str()),
            Some(".")
        );
        assert_eq!(
            request.get("to_ws").and_then(|value| value.as_str()),
            Some("scout")
        );
        assert_eq!(
            request.get("retry_only").and_then(|value| value.as_bool()),
            Some(true)
        );
        assert!(request
            .get("since")
            .and_then(|value| value.as_i64())
            .is_some());
    }

    #[test]
    fn detail_includes_payload_and_lineage() {
        let detail = serde_json::json!({
            "task": {
                "task_id":"12345678-a",
                "from_ws":".", "to_ws":"scout", "state":"running", "attempt":2,
                "terminal":"orca-a", "created_at":100, "payload":"full payload", "out_head":"", "reason":""
            },
            "parent": {"task_id":"parent-a","state":"done"},
            "children": [{"task_id":"child-a","state":"pending"}]
        });
        let (title, text) = detail_text(&detail);
        assert!(title.contains("12345678"));
        assert!(text.contains("full payload"));
        assert!(text.contains("parent-a"));
        assert!(text.contains("child-a"));
    }
}
