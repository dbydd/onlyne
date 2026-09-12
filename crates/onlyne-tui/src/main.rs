use clap::Parser;
use crossterm::event::{
    self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use onlyne_tui::model::{
    Detail, Focus, MAX_SPACING, MIN_SPACING, Page, Snapshot, UiState, cycle_edge, cycle_role,
    cycle_state, detail, pull, role_detail, role_edges, selected_role,
};
use onlyne_tui::socket::{NO_SOCKET_MESSAGE, SocketArgs, resolve_socket};
use onlyne_tui::ui::{
    apply_page_history, clamp_cursor, drag_role_view, follow_role_edge, graph_len, history_len,
    history_page_size, map_view_size, move_cursor, move_role_edge, pan_role_view, render,
    render_once_text, role_back, selected_task, sync_map, zoom_role_view,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use std::io::stdout;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Debug, Parser)]
#[command(
    name = "onlyne-tui",
    version,
    about = "Observe an Onlyne v1 admin socket"
)]
struct Cli {
    /// Unix socket path, used verbatim.
    #[arg(long)]
    socket: Option<PathBuf>,
    /// Server root; the admin socket is `<dir>/.onlyne/run/s`.
    #[arg(long)]
    server_root: Option<PathBuf>,
    /// Workspace used only for upward `.onlyne/run/s` discovery.
    #[arg(long)]
    workspace: Option<PathBuf>,
    /// Render one frame as plain text to stdout and exit.
    #[arg(long)]
    once: bool,
    /// Page `--once` renders: 1 is the role network, 2 is the swarm view.
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=2), default_value_t = 1)]
    page: u8,
    /// Role-map spacing/repulsion: 1 is compact, 4 is widest.
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=4), default_value_t = 2)]
    spacing: u8,
}

fn main() {
    std::process::exit(match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("onlyne-tui: {error:#}");
            1
        }
    });
}

fn run() -> anyhow::Result<i32> {
    let cli = Cli::parse();
    let socket = match resolve_socket(&SocketArgs {
        socket: cli.socket,
        server_root: cli.server_root,
        workspace: cli.workspace,
    }) {
        Ok(path) => path,
        Err(_) => {
            eprintln!("{NO_SOCKET_MESSAGE}");
            return Ok(3);
        }
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;
    let mut state = UiState::default();
    if cli.page == 2 {
        state.page = Page::Swarm;
    }
    state.spacing = cli.spacing as usize;
    let mut snapshot = runtime.block_on(pull(&socket, &state.filter, 30));
    if cli.once {
        sync_selection_and_detail(&runtime, &socket, &snapshot, &mut state);
        sync_map(&mut state, &snapshot);
        println!("{}", render_once_text(&snapshot, &state, 120, 36));
        return Ok(0);
    }
    run_interactive(&runtime, &socket, &mut snapshot, &mut state)
}

fn run_interactive(
    runtime: &tokio::runtime::Runtime,
    socket: &std::path::Path,
    snapshot: &mut Snapshot,
    state: &mut UiState,
) -> anyhow::Result<i32> {
    crossterm::terminal::enable_raw_mode()?;
    let mut out = stdout();
    crossterm::execute!(
        out,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    )?;
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;
    let result = run_loop(runtime, socket, &mut terminal, snapshot, state);
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::event::DisableMouseCapture,
        crossterm::terminal::LeaveAlternateScreen
    )?;
    result
}

fn run_loop(
    runtime: &tokio::runtime::Runtime,
    socket: &std::path::Path,
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    snapshot: &mut Snapshot,
    state: &mut UiState,
) -> anyhow::Result<i32> {
    sync_selection_and_detail(runtime, socket, snapshot, state);
    sync_map(state, snapshot);
    let mut refreshed = Instant::now();
    loop {
        terminal.draw(|frame| render(frame, snapshot, state))?;
        if refreshed.elapsed() >= Duration::from_secs(1) && state.search.is_none() {
            refresh_now(runtime, socket, terminal, snapshot, state, &mut refreshed);
        }
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if handle_search_key(
                    key.code,
                    runtime,
                    socket,
                    terminal,
                    snapshot,
                    state,
                    &mut refreshed,
                ) {
                    continue;
                }
                if handle_key(
                    key.code,
                    key.modifiers,
                    runtime,
                    socket,
                    terminal,
                    snapshot,
                    state,
                    &mut refreshed,
                )? {
                    return Ok(0);
                }
            }
            Event::Mouse(mouse) => handle_mouse(mouse, terminal, snapshot, state),
            _ => {}
        }
    }
}

/// One key press. Returns whether the operator asked to leave.
#[allow(clippy::too_many_arguments)]
fn handle_key(
    code: KeyCode,
    modifiers: KeyModifiers,
    runtime: &tokio::runtime::Runtime,
    socket: &std::path::Path,
    terminal: &Terminal<CrosstermBackend<std::io::Stdout>>,
    snapshot: &mut Snapshot,
    state: &mut UiState,
    refreshed: &mut Instant,
) -> anyhow::Result<bool> {
    let view = map_view(terminal);
    match code {
        KeyCode::Char('q') => return Ok(true),
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => return Ok(true),
        KeyCode::Esc => return Ok(true),
        KeyCode::Char('1') => {
            state.page = Page::RoleMap;
            state.detail_scroll = 0;
            sync_selection_and_detail(runtime, socket, snapshot, state);
        }
        KeyCode::Char('2') => {
            state.page = Page::Swarm;
            state.detail_scroll = 0;
            sync_selection_and_detail(runtime, socket, snapshot, state);
        }
        KeyCode::Tab => {
            state.page = state.page.toggle();
            state.detail_scroll = 0;
            sync_selection_and_detail(runtime, socket, snapshot, state);
        }
        KeyCode::Enter => sync_selection_and_detail(runtime, socket, snapshot, state),
        KeyCode::Char('J') => state.detail_scroll = state.detail_scroll.saturating_add(1),
        KeyCode::Char('K') => state.detail_scroll = state.detail_scroll.saturating_sub(1),
        KeyCode::Char('r') => refresh_now(runtime, socket, terminal, snapshot, state, refreshed),
        // Page 1: `hjkl` walks the ring, the arrows and the mouse move the
        // camera, `+`/`-` set the repulsion, `0` recentres, and `e` shows the
        // control-plane spokes the map holds back.
        KeyCode::Char('e') if state.page == Page::RoleMap => {
            state.show_control_edges = !state.show_control_edges;
        }
        KeyCode::Char(c) if state.page == Page::RoleMap && (c == '+' || c == '=') => {
            state.spacing = (state.spacing + 1).min(MAX_SPACING);
            reflow_map(snapshot, state, view);
        }
        KeyCode::Char('-') if state.page == Page::RoleMap => {
            state.spacing = state.spacing.saturating_sub(1).max(MIN_SPACING);
            reflow_map(snapshot, state, view);
        }
        KeyCode::Char('0') if state.page == Page::RoleMap => state.role_cam.reset(),
        KeyCode::Char('j') if state.page == Page::RoleMap => move_role_edge(1, snapshot, state),
        KeyCode::Char('k') if state.page == Page::RoleMap => move_role_edge(-1, snapshot, state),
        KeyCode::Char('l') if state.page == Page::RoleMap => {
            if follow_role_edge(snapshot, state) {
                sync_selection_and_detail(runtime, socket, snapshot, state);
            }
        }
        KeyCode::Char('h') if state.page == Page::RoleMap => {
            if role_back(state) {
                sync_selection_and_detail(runtime, socket, snapshot, state);
            }
        }
        KeyCode::Up if state.page == Page::RoleMap => pan_role_view((0, -1), snapshot, state, view),
        KeyCode::Down if state.page == Page::RoleMap => {
            pan_role_view((0, 1), snapshot, state, view)
        }
        KeyCode::Left if state.page == Page::RoleMap => {
            pan_role_view((-1, 0), snapshot, state, view)
        }
        KeyCode::Right if state.page == Page::RoleMap => {
            pan_role_view((1, 0), snapshot, state, view)
        }
        // Page 2 keeps its own keys.
        KeyCode::Char('g') if state.page == Page::Swarm => state.focus = Focus::Graph,
        KeyCode::Char('h') if state.page == Page::Swarm => state.focus = Focus::History,
        KeyCode::Up | KeyCode::Char('k') => {
            let len = focus_len(snapshot, state);
            match state.focus {
                Focus::Graph => move_cursor(&mut state.graph_cursor, len, -1),
                Focus::History => move_cursor(&mut state.history_cursor, len, -1),
            }
            sync_selection_and_detail(runtime, socket, snapshot, state);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let len = focus_len(snapshot, state);
            match state.focus {
                Focus::Graph => move_cursor(&mut state.graph_cursor, len, 1),
                Focus::History => move_cursor(&mut state.history_cursor, len, 1),
            }
            sync_selection_and_detail(runtime, socket, snapshot, state);
        }
        KeyCode::Char('/') if state.page == Page::Swarm => {
            state.search = Some(state.filter.text.clone())
        }
        KeyCode::Char('f') if state.page == Page::Swarm => {
            cycle_state(&mut state.filter);
            refresh_now(runtime, socket, terminal, snapshot, state, refreshed);
        }
        KeyCode::Char('t') if state.page == Page::Swarm => {
            state.filter.window = state.filter.window.next();
            state.filter.reset_page();
            refresh_now(runtime, socket, terminal, snapshot, state, refreshed);
        }
        KeyCode::Char('o') if state.page == Page::Swarm => {
            cycle_role(&snapshot.roles, &mut state.filter);
            refresh_now(runtime, socket, terminal, snapshot, state, refreshed);
        }
        KeyCode::Char('e') if state.page == Page::Swarm => {
            cycle_edge(snapshot, &mut state.filter);
            refresh_now(runtime, socket, terminal, snapshot, state, refreshed);
        }
        KeyCode::PageDown if state.page == Page::Swarm && state.focus == Focus::History => {
            let page_size = history_page_size(terminal_size(terminal).1);
            apply_page_history(1, snapshot, state, page_size);
            refresh_now(runtime, socket, terminal, snapshot, state, refreshed);
        }
        KeyCode::PageUp if state.page == Page::Swarm && state.focus == Focus::History => {
            let page_size = history_page_size(terminal_size(terminal).1);
            apply_page_history(-1, snapshot, state, page_size);
            refresh_now(runtime, socket, terminal, snapshot, state, refreshed);
        }
        // `a` flips the session views between the rows still holding a slot
        // and everything.
        KeyCode::Char('a') => {
            state.active_only = !state.active_only;
            clamp_cursor(
                &mut state.graph_cursor,
                graph_len(snapshot, state.active_only),
            );
            sync_selection_and_detail(runtime, socket, snapshot, state);
        }
        KeyCode::PageDown => state.detail_scroll = state.detail_scroll.saturating_add(8),
        KeyCode::PageUp => state.detail_scroll = state.detail_scroll.saturating_sub(8),
        _ => {}
    }
    Ok(false)
}

/// Re-settle the page-1 map after the repulsion knob moved, then bring the
/// camera back inside the new extent.
fn reflow_map(snapshot: &Snapshot, state: &mut UiState, view: (usize, usize)) {
    sync_map(state, snapshot);
    pan_role_view((0, 0), snapshot, state, view);
}

/// The page-1 mouse: the wheel zooms, a left drag pans the map.
fn handle_mouse(
    mouse: crossterm::event::MouseEvent,
    terminal: &Terminal<CrosstermBackend<std::io::Stdout>>,
    snapshot: &Snapshot,
    state: &mut UiState,
) {
    if state.page != Page::RoleMap {
        if matches!(mouse.kind, MouseEventKind::Up(MouseButton::Left)) {
            state.drag = None;
        }
        return;
    }
    let view = map_view(terminal);
    match mouse.kind {
        MouseEventKind::ScrollUp => zoom_role_view(-1, snapshot, state, view),
        MouseEventKind::ScrollDown => zoom_role_view(1, snapshot, state, view),
        MouseEventKind::Down(MouseButton::Left) => state.drag = Some((mouse.column, mouse.row)),
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some(from) = state.drag {
                drag_role_view(from, (mouse.column, mouse.row), snapshot, state, view);
                state.drag = Some((mouse.column, mouse.row));
            }
        }
        MouseEventKind::Up(MouseButton::Left) => state.drag = None,
        _ => {}
    }
}

fn handle_search_key(
    key: KeyCode,
    runtime: &tokio::runtime::Runtime,
    socket: &std::path::Path,
    terminal: &Terminal<CrosstermBackend<std::io::Stdout>>,
    snapshot: &mut Snapshot,
    state: &mut UiState,
    refreshed: &mut Instant,
) -> bool {
    let Some(input) = state.search.as_mut() else {
        return false;
    };
    match key {
        KeyCode::Esc => state.search = None,
        KeyCode::Enter => {
            state.filter.text = input.clone();
            state.filter.reset_page();
            state.search = None;
            refresh_now(runtime, socket, terminal, snapshot, state, refreshed);
        }
        KeyCode::Backspace => {
            input.pop();
        }
        KeyCode::Char(ch) => input.push(ch),
        _ => {}
    }
    true
}

fn refresh_now(
    runtime: &tokio::runtime::Runtime,
    socket: &std::path::Path,
    terminal: &Terminal<CrosstermBackend<std::io::Stdout>>,
    snapshot: &mut Snapshot,
    state: &mut UiState,
    refreshed: &mut Instant,
) {
    let previous = snapshot.clone();
    let page_size = history_page_size(terminal.size().map(|size| size.height).unwrap_or(36));
    let next = runtime.block_on(pull(socket, &state.filter, page_size));
    *snapshot = if next.server_online {
        next
    } else {
        Snapshot {
            server_online: false,
            last_error: next.last_error,
            refreshed_at: next.refreshed_at,
            ..previous
        }
    };
    clamp_cursor(
        &mut state.graph_cursor,
        graph_len(snapshot, state.active_only),
    );
    clamp_cursor(&mut state.history_cursor, history_len(snapshot));
    sync_map(state, snapshot);
    sync_selection_and_detail(runtime, socket, snapshot, state);
    *refreshed = Instant::now();
}

fn sync_selection_and_detail(
    runtime: &tokio::runtime::Runtime,
    socket: &std::path::Path,
    snapshot: &Snapshot,
    state: &mut UiState,
) {
    let key = match state.page {
        Page::RoleMap => {
            let role = selected_role(snapshot, state);
            let len = role
                .as_deref()
                .map(|role| role_edges(snapshot, state, role).len())
                .unwrap_or(0);
            state.role_edge = state.role_edge.filter(|index| *index < len);
            role
        }
        Page::Swarm => selected_task(snapshot, state),
    };
    // The subject is tagged with its page, so switching pages always reloads
    // even when a role name and a task id happen to read the same.
    let tagged = key
        .as_ref()
        .map(|key| format!("{}:{key}", state.page.number()));
    if tagged == state.detail_key {
        return;
    }
    state.detail_key = tagged;
    state.detail_scroll = 0;
    let Some(key) = key else {
        state.detail = None;
        return;
    };
    let loaded = match state.page {
        Page::RoleMap => runtime
            .block_on(role_detail(socket, &key))
            .map(Detail::Role),
        Page::Swarm => runtime.block_on(detail(socket, &key)).map(Detail::Task),
    };
    match loaded {
        Ok(detail) => state.detail = Some(detail),
        Err(error) => state.message = format!("detail failed: {error}"),
    }
}

fn focus_len(snapshot: &Snapshot, state: &UiState) -> usize {
    match state.focus {
        Focus::Graph => graph_len(snapshot, state.active_only),
        Focus::History => history_len(snapshot),
    }
}

/// The terminal's size, or the size the maps are laid out for when the
/// terminal will not say.
fn terminal_size(terminal: &Terminal<CrosstermBackend<std::io::Stdout>>) -> (u16, u16) {
    let size = terminal.size().unwrap_or(ratatui::layout::Size {
        width: 120,
        height: 36,
    });
    (size.width, size.height)
}

/// The cells the role map pane draws in, so the camera clamps to the room the
/// pane really has.
fn map_view(terminal: &Terminal<CrosstermBackend<std::io::Stdout>>) -> (usize, usize) {
    let (width, height) = terminal_size(terminal);
    map_view_size(Rect::new(0, 0, width, height))
}
