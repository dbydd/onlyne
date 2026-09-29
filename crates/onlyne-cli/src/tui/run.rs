//! The `tui` verb: the observation board, drawn in this process.
//!
//! This module is the board's front end and its only driver. It owns the
//! `State`, folds every event through `update`, and draws; it opens no socket.
//! The one task that talks to the server is [`crate::tui::io`], started here and
//! handed the ops this loop takes out of the state's outbox.
//!
//! The loop waits on two channels and nothing else: the terminal's input, and
//! what the IO task learned. v1 polled the server for the same five reads every
//! second (`docs/v2-PLAN.md` line 395), and there is no timer here to do that
//! with — a page moves when the stream says the cluster did, and a resize
//! redraws because a new state was drawn anyway.

use crate::flags::{AsArg, DEFAULT_TIMEOUT_MS};
use crate::runtime::{EXIT_ANSWER_FAILED, EXIT_NO_SOCKET, EXIT_OK, EXIT_VALIDATION};
use crate::tui::io;
use crate::tui::render::render;
use crate::tui::render::render_text;
use crate::tui::socket::{NEEDS_ADMIN, NO_SOCKET_MESSAGE, SocketArgs, resolve_socket};
use crate::tui::state::State;
use crate::tui::update::{self, Event};
use clap::Parser;
use crossterm::event as term;
use crossterm::execute;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io::stdout;
use std::path::{Path, PathBuf};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

#[derive(Debug, Parser)]
#[command(
    name = "onlyne tui",
    version,
    about = "Observe an Onlyne admin socket: cluster, task, and faults"
)]
struct Cli {
    /// Unix socket path, used verbatim with `--as` for its surface.
    #[arg(long)]
    socket: Option<PathBuf>,
    /// Server root; its admin socket is derived from the root and lives in the
    /// machine-level runtime directory as `<runtime>/<digest>.sock`.
    #[arg(long)]
    server_root: Option<PathBuf>,
    /// Workspace used only for upward discovery of the tree that owns a socket
    /// in the runtime directory.
    #[arg(long)]
    workspace: Option<PathBuf>,
    /// Surface of a `--socket` path that carries no other hint.
    #[arg(long = "as", value_enum, default_value = "auto")]
    surface_hint: AsArg,
    /// Bound for one socket read, in milliseconds.
    #[arg(long, default_value_t = DEFAULT_TIMEOUT_MS)]
    timeout: u64,
    /// Read one snapshot, print one frame as plain text, and exit.
    #[arg(long)]
    once: bool,
}

/// The `tui` verb, parsed from the arguments `onlyne` forwarded to it.
pub fn run(args: &[String]) -> i32 {
    let argv = std::iter::once("onlyne tui").chain(args.iter().map(String::as_str));
    match board(Cli::parse_from(argv)) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("onlyne tui: {error:#}");
            EXIT_ANSWER_FAILED
        }
    }
}

/// The board, from the resolved socket to the exit code.
fn board(cli: Cli) -> anyhow::Result<i32> {
    let args = SocketArgs {
        socket: cli.socket.clone(),
        server_root: cli.server_root.clone(),
        workspace: cli.workspace.clone(),
        surface_hint: cli.surface_hint,
    };
    let target = match resolve_socket(&args) {
        Ok(target) => target,
        Err(_) => {
            eprintln!("{NO_SOCKET_MESSAGE}");
            return Ok(EXIT_NO_SOCKET);
        }
    };
    if target.surface != crate::socket::Surface::Admin {
        eprintln!("{NEEDS_ADMIN}");
        return Ok(EXIT_VALIDATION);
    }
    // The role a form opens as the sender. A verb on the client surface sends
    // as the role its workspace runs; the board has no workspace of its own, so
    // it starts from the same word `ONLYNE_ROLE` carries and lets the operator
    // change it in the form.
    let sender = crate::flags::GlobalFlags::addressing(None, None, None).local_role();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;
    Ok(runtime.block_on(async move {
        if cli.once {
            return once(&target.path, cli.timeout, sender).await;
        }
        interactive(target.path, cli.timeout, sender).await
    }))
}

/// Read one snapshot, draw one frame as text, and stop.
///
/// The frame goes through the same `update` and the same `render` the alternate
/// screen does, so what this prints is what an operator reads there.
async fn once(path: &Path, timeout_ms: u64, sender: String) -> i32 {
    let snapshot = match io::read_snapshot(path, timeout_ms).await {
        Ok(snapshot) => snapshot,
        Err(reason) => {
            eprintln!("onlyne tui: {reason}");
            return EXIT_ANSWER_FAILED;
        }
    };
    let state = update::update(State::new(sender), Event::Snapshot(Box::new(snapshot)));
    println!("{}", render_text(&state, 120, 36));
    EXIT_OK
}

/// The interactive board: draw, wait for one event, fold it, draw again.
async fn interactive(path: PathBuf, timeout_ms: u64, sender: String) -> i32 {
    let mut screen = match Screen::enter() {
        Ok(screen) => screen,
        Err(error) => {
            eprintln!("onlyne tui: {error}");
            return EXIT_ANSWER_FAILED;
        }
    };
    let mut input = match Input::spawn() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("onlyne tui: {error}");
            return EXIT_ANSWER_FAILED;
        }
    };
    let mut task = io::spawn(path, timeout_ms);
    let mut state = State::new(sender);
    let result = loop {
        if let Err(error) = screen.draw(&state) {
            break Err(error);
        }
        tokio::select! {
            // A channel that closed is a terminal that is gone: leave rather
            // than draw a screen nobody is reading.
            Some(event) = input.events.recv() => state = update::update(state, Event::Terminal(event)),
            Some(event) = task.events.recv() => state = update::update(state, event),
            else => break Ok(()),
        }
        for action in state.take_actions() {
            let _ = task.actions.send(action);
        }
        if state.ui.quit {
            break Ok(());
        }
    };
    // Restore the terminal before anything is printed over it.
    drop(screen);
    match result {
        Ok(()) => EXIT_OK,
        Err(error) => {
            eprintln!("onlyne tui: {error}");
            EXIT_ANSWER_FAILED
        }
    }
}

/// The alternate screen, restored however this function is left.
struct Screen {
    terminal: Terminal<CrosstermBackend<std::io::Stdout>>,
}

impl Screen {
    /// Enter the alternate screen and raw mode.
    fn enter() -> std::io::Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        let mut out = stdout();
        execute!(out, crossterm::terminal::EnterAlternateScreen)?;
        let terminal = Terminal::new(CrosstermBackend::new(out))?;
        Ok(Screen { terminal })
    }

    /// Draw one frame of one state.
    fn draw(&mut self, state: &State) -> std::io::Result<()> {
        self.terminal.draw(|frame| render(frame, state))?;
        Ok(())
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let _ = crossterm::execute!(
            self.terminal.backend_mut(),
            crossterm::terminal::LeaveAlternateScreen
        );
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// Terminal input, read on its own thread.
///
/// `crossterm::event::read` blocks, and the IO task shares this runtime: a
/// blocking read inside the select would starve the stream. The thread ends
/// with the process, and it stops sending the moment the board is gone.
struct Input {
    events: UnboundedReceiver<term::Event>,
}

impl Input {
    fn spawn() -> std::io::Result<Self> {
        let (events, receiver) = unbounded_channel();
        let sender: UnboundedSender<term::Event> = events;
        std::thread::Builder::new()
            .name("onlyne-tui-input".to_string())
            .spawn(move || {
                while let Ok(event) = term::read() {
                    if sender.send(event).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Input { events: receiver })
    }
}
