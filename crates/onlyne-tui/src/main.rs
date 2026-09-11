use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use onlyne_tui::model::{
    Focus, Page, Snapshot, UiState, cycle_edge, cycle_role, cycle_state, detail, pull,
};
use onlyne_tui::socket::{NO_SOCKET_MESSAGE, SocketArgs, resolve_socket};
use onlyne_tui::ui::{
    apply_page_history, clamp_cursor, graph_len, history_len, history_page_size, move_cursor,
    render, render_once_text, selected_task,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
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
    let mut snapshot = runtime.block_on(pull(&socket, &state.filter, 30));
    if cli.once {
        sync_selection_and_detail(&runtime, &socket, &snapshot, &mut state);
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
    crossterm::execute!(out, crossterm::terminal::EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;
    let result = run_loop(runtime, socket, &mut terminal, snapshot, state);
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
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
    let mut refreshed = Instant::now();
    loop {
        terminal.draw(|frame| render(frame, snapshot, state))?;
        if refreshed.elapsed() >= Duration::from_secs(1) && state.search.is_none() {
            refresh_now(runtime, socket, terminal, snapshot, state, &mut refreshed);
        }
        if !event::poll(Duration::from_millis(100))? {
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
            runtime,
            socket,
            terminal,
            snapshot,
            state,
            &mut refreshed,
        ) {
            continue;
        }
        match key.code {
            KeyCode::Char('q') => return Ok(0),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(0),
            KeyCode::Esc => return Ok(0),
            KeyCode::Char('1') => {
                state.page = Page::RoleMap;
                state.detail_scroll = 0;
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
            KeyCode::Char('J') => state.detail_scroll = state.detail_scroll.saturating_add(1),
            KeyCode::Char('K') => state.detail_scroll = state.detail_scroll.saturating_sub(1),
            KeyCode::Char('/') => state.search = Some(state.filter.text.clone()),
            KeyCode::Char('r') => {
                refresh_now(runtime, socket, terminal, snapshot, state, &mut refreshed)
            }
            KeyCode::Char('f') => {
                cycle_state(&mut state.filter);
                refresh_now(runtime, socket, terminal, snapshot, state, &mut refreshed);
            }
            KeyCode::Char('t') => {
                state.filter.window = state.filter.window.next();
                state.filter.reset_page();
                refresh_now(runtime, socket, terminal, snapshot, state, &mut refreshed);
            }
            KeyCode::Char('o') => {
                cycle_role(&snapshot.roles, &mut state.filter);
                refresh_now(runtime, socket, terminal, snapshot, state, &mut refreshed);
            }
            KeyCode::Char('e') => {
                cycle_edge(snapshot, &mut state.filter);
                refresh_now(runtime, socket, terminal, snapshot, state, &mut refreshed);
            }
            KeyCode::PageDown if state.focus == Focus::History => {
                let page_size = history_page_size(terminal.size()?.height);
                apply_page_history(1, snapshot, state, page_size);
                refresh_now(runtime, socket, terminal, snapshot, state, &mut refreshed);
            }
            KeyCode::PageUp if state.focus == Focus::History => {
                let page_size = history_page_size(terminal.size()?.height);
                apply_page_history(-1, snapshot, state, page_size);
                refresh_now(runtime, socket, terminal, snapshot, state, &mut refreshed);
            }
            KeyCode::PageDown => state.detail_scroll = state.detail_scroll.saturating_add(8),
            KeyCode::PageUp => state.detail_scroll = state.detail_scroll.saturating_sub(8),
            KeyCode::Enter => sync_selection_and_detail(runtime, socket, snapshot, state),
            _ => {}
        }
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
    clamp_cursor(&mut state.graph_cursor, graph_len(snapshot));
    clamp_cursor(&mut state.history_cursor, history_len(snapshot));
    sync_selection_and_detail(runtime, socket, snapshot, state);
    *refreshed = Instant::now();
}

fn sync_selection_and_detail(
    runtime: &tokio::runtime::Runtime,
    socket: &std::path::Path,
    snapshot: &Snapshot,
    state: &mut UiState,
) {
    let selected = selected_task(snapshot, state);
    if selected == state.detail_task_id {
        return;
    }
    state.detail_task_id = selected.clone();
    state.detail_scroll = 0;
    let Some(task_id) = selected else {
        state.detail = None;
        return;
    };
    match runtime.block_on(detail(socket, &task_id)) {
        Ok(detail) => state.detail = Some(detail),
        Err(error) => state.message = format!("detail failed: {error}"),
    }
}

fn focus_len(snapshot: &Snapshot, state: &UiState) -> usize {
    match state.focus {
        Focus::Graph => graph_len(snapshot),
        Focus::History => history_len(snapshot),
    }
}
