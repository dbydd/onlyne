//! `onlyne-view`: one ACP session journal, full screen.
//!
//! An ACP session owns no terminal — the client holds the agent's process and
//! journals every record to
//! `<workspace>/.onlyne/logs/session-<task>.events.jsonl`. This binary is the
//! other half of that: point it at a workspace and a task, and it renders the
//! conversation and keeps up with the file as the turn runs. The client runs one
//! per pane.
//!
//! Everything here is chrome. Reading, grouping, the two verbosity modes, and
//! the text the `--once` path prints all come from [`onlyne_tui::content`], the
//! same module any other page draws from, and the text path is the board's
//! [`onlyne_tui::ui::render_text`], so a journal reads identically whichever
//! surface shows it.

use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use onlyne_tui::content;
use onlyne_tui::content::{ContentLine, ContentMode, Doc, JournalSource, LineKind, plain_text};
use onlyne_tui::ui;
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use std::io::{Stdout, stdout};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// The narrowest `--once` frame: the same width the board prints at. A frame
/// only grows past it to keep the footer's journal path whole.
const ONCE_WIDTH: u16 = 120;
/// The rows around the journal in a text frame: the top bar, the pane's two
/// border rows, and the footer.
const CHROME: usize = 4;
/// How often the view checks the journal for growth.
const POLL: Duration = Duration::from_millis(250);

#[derive(Debug, Parser)]
#[command(
    name = "onlyne-view",
    version,
    about = "Read one Onlyne ACP session journal full screen"
)]
struct Cli {
    /// Workspace whose `.onlyne/logs` holds the session journal.
    #[arg(long)]
    workspace: PathBuf,
    /// Task id naming the journal `session-<task>.events.jsonl`.
    #[arg(long)]
    task: String,
    /// Render the journal as plain text to stdout and exit.
    #[arg(long)]
    once: bool,
    /// With `--once`: stay attached and print the lines the journal grows by.
    #[arg(long)]
    follow: bool,
    /// How much of each block to show: `full` adds reasoning and one tool
    /// summary line per call, `compact` keeps the names.
    #[arg(long, value_enum, default_value_t = ModeFlag::Full)]
    mode: ModeFlag,
    /// Accepted and ignored for now: a later slice points this view at the
    /// client socket instead of the journal file.
    #[arg(long)]
    socket: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum ModeFlag {
    Full,
    Compact,
}

impl From<ModeFlag> for ContentMode {
    fn from(flag: ModeFlag) -> Self {
        match flag {
            ModeFlag::Full => ContentMode::Full,
            ModeFlag::Compact => ContentMode::Compact,
        }
    }
}

fn main() {
    std::process::exit(match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("onlyne-view: {error:#}");
            1
        }
    });
}

fn run() -> anyhow::Result<i32> {
    let cli = Cli::parse();
    if cli.socket.is_some() {
        eprintln!("onlyne-view: --socket is not a feed yet; reading the journal file");
    }
    let mut view = Viewer::open(
        JournalSource::new(cli.workspace),
        &cli.task,
        cli.mode.into(),
    );
    if cli.once {
        println!("{}", view.render_text());
        if cli.follow {
            return follow_appended(&mut view);
        }
        return Ok(0);
    }
    run_interactive(&mut view)
}

fn run_interactive(view: &mut Viewer) -> anyhow::Result<i32> {
    crossterm::terminal::enable_raw_mode()?;
    let mut out = stdout();
    crossterm::execute!(out, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;
    let result = run_loop(view, &mut terminal);
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    result?;
    Ok(0)
}

fn run_loop(
    view: &mut Viewer,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
) -> anyhow::Result<()> {
    let size = terminal.size()?;
    view.set_pane(bands(Rect::new(0, 0, size.width, size.height)).1);
    let mut checked = Instant::now();
    loop {
        terminal.draw(|frame| draw(frame, view))?;
        view.set_pane(bands(terminal.get_frame().area()).1);
        if checked.elapsed() >= POLL {
            checked = Instant::now();
            if view.changed() {
                view.reload();
            }
        }
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match classify_key(key.code, key.modifiers) {
                ViewKey::Quit => return Ok(()),
                ViewKey::ToggleMode => view.set_mode(view.mode.toggle()),
                ViewKey::Scroll(delta) => view.scroll_by(delta),
                ViewKey::Tail(follow) => view.set_follow(follow),
                ViewKey::Reload => view.reload(),
                ViewKey::Ignore => {}
            }
        }
    }
}

/// `--once --follow`: print the journal, then print what it grows by.
///
/// The first frame is the framed render; what follows is bare appended text, so
/// the output stays readable through `less` and greppable by an operator who
/// wants the log rather than the picture.
fn follow_appended(view: &mut Viewer) -> anyhow::Result<i32> {
    let mut shown = view.lines.clone();
    loop {
        std::thread::sleep(POLL);
        if !view.changed() {
            continue;
        }
        view.reload();
        for line in content::appended(&shown, &view.lines) {
            println!("{}", line.text);
        }
        shown = view.lines.clone();
    }
}

/// One key press, classified. The journal has no lists to walk, so every key is
/// about this one page.
#[derive(Debug, PartialEq, Eq)]
enum ViewKey {
    Quit,
    ToggleMode,
    Scroll(i16),
    Tail(bool),
    Reload,
    Ignore,
}

fn classify_key(code: KeyCode, modifiers: KeyModifiers) -> ViewKey {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    match code {
        KeyCode::Char('q') => ViewKey::Quit,
        KeyCode::Char('c') if ctrl => ViewKey::Quit,
        KeyCode::Esc => ViewKey::Quit,
        KeyCode::Char('m') => ViewKey::ToggleMode,
        KeyCode::Char('J') | KeyCode::Down | KeyCode::Char('j') => ViewKey::Scroll(1),
        KeyCode::Char('K') | KeyCode::Up | KeyCode::Char('k') => ViewKey::Scroll(-1),
        KeyCode::PageDown => ViewKey::Scroll(8),
        KeyCode::PageUp => ViewKey::Scroll(-8),
        KeyCode::End => ViewKey::Tail(true),
        KeyCode::Home => ViewKey::Tail(false),
        KeyCode::Char('r') => ViewKey::Reload,
        _ => ViewKey::Ignore,
    }
}

/// The page: a journal read through the content seam, the verbosity mode it is
/// showing, and where in it the operator stands.
struct Viewer {
    source: JournalSource,
    task_id: String,
    mode: ContentMode,
    doc: Doc,
    lines: Vec<ContentLine>,
    body: String,
    scroll: u16,
    /// Whether the view tracks the tail of the journal. `End` resumes it,
    /// scrolling back leaves it, and a reload while set lands on the last line.
    following: bool,
    pane: Rect,
    stamp: Option<FileStamp>,
}

impl Viewer {
    fn open(source: JournalSource, task_id: &str, mode: ContentMode) -> Self {
        let mut view = Self {
            source,
            task_id: task_id.to_string(),
            mode,
            doc: Doc::default(),
            lines: Vec::new(),
            body: String::new(),
            scroll: 0,
            following: true,
            pane: Rect::new(0, 0, ONCE_WIDTH, 36),
            stamp: None,
        };
        view.reload();
        view
    }

    /// Re-read the journal. A file that grew while the operator was reading shows
    /// up here, so following the session needs no restart.
    fn reload(&mut self) {
        self.doc = content::load(&self.source, &self.task_id);
        self.stamp = FileStamp::of(&self.doc.path);
        self.recompose();
    }

    /// Re-derive the lines and the scroll from the document and the mode.
    fn recompose(&mut self) {
        self.lines = self.compose();
        self.body = plain_text(&self.lines);
        if self.following {
            self.scroll_to_end();
        } else {
            self.clamp();
        }
    }

    /// The journal's lines, or what to say when there is nothing to read yet.
    fn compose(&self) -> Vec<ContentLine> {
        let mut lines = self.doc.lines(self.mode);
        if lines.is_empty() {
            lines.push(ContentLine {
                kind: LineKind::Notice,
                text: if self.doc.journal_exists {
                    format!("waiting for the first record from {}", self.task_id)
                } else {
                    format!("no journal at {}", self.doc.path.display())
                },
            });
        }
        lines
    }

    fn set_mode(&mut self, mode: ContentMode) {
        self.mode = mode;
        self.recompose();
    }

    fn set_pane(&mut self, pane: Rect) {
        if pane == self.pane {
            return;
        }
        self.pane = pane;
        if self.following {
            self.scroll_to_end();
        } else {
            self.clamp();
        }
    }

    fn changed(&self) -> bool {
        FileStamp::of(&self.doc.path) != self.stamp
    }

    fn max_scroll(&self) -> usize {
        ui::detail_extent(&self.body, self.pane).max
    }

    fn clamp(&mut self) {
        let max = self.max_scroll();
        self.scroll = self.scroll.min(u16::try_from(max).unwrap_or(u16::MAX));
    }

    fn scroll_to_end(&mut self) {
        self.scroll = u16::try_from(self.max_scroll()).unwrap_or(u16::MAX);
    }

    fn scroll_by(&mut self, delta: i16) {
        self.scroll = if delta >= 0 {
            self.scroll
                .saturating_add(u16::try_from(delta).unwrap_or(u16::MAX))
        } else {
            self.scroll.saturating_sub(delta.unsigned_abs())
        };
        self.clamp();
        // Reaching the last line the page can show is re-joining the tail.
        self.following = usize::from(self.scroll) >= self.max_scroll();
    }

    fn set_follow(&mut self, follow: bool) {
        self.following = follow;
        if follow {
            self.scroll_to_end();
        } else {
            self.scroll = 0;
        }
    }

    /// The whole journal as text, the same render path the board's `--once` uses.
    ///
    /// A text render has no screen to fit: the frame is as tall as the journal
    /// needs, and at least as wide as the one-line footer, because the footer
    /// names the journal path and an operator pipes the output rather than
    /// watching it clip.
    fn render_text(&mut self) -> String {
        let wanted = u16::try_from(footer(self).chars().count() + 1).unwrap_or(u16::MAX);
        let width = ONCE_WIDTH.max(wanted).min(240);
        // Probing a 2-row band at that width counts the wrapped rows; the frame
        // then gets a band exactly as tall as the journal.
        let rows = ui::detail_extent(&self.body, Rect::new(0, 0, width, 2)).lines;
        let height = u16::try_from(rows + CHROME).unwrap_or(u16::MAX);
        self.pane = bands(Rect::new(0, 0, width, height)).1;
        // Everything fits, so this lands on 0; only a frame that had to be
        // capped scrolls, and then it shows the tail rather than the head.
        self.scroll_to_end();
        ui::render_text(width, height, |frame| draw(frame, self))
    }
}

/// What changed in a journal, without reading it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
}

impl FileStamp {
    fn of(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        Some(Self {
            len: meta.len(),
            modified: meta.modified().ok(),
        })
    }
}

/// The three bands the page owns: a bar naming the session, the journal, and the
/// footer naming the mode and the file.
fn bands(area: Rect) -> (Rect, Rect, Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(area);
    (rows[0], rows[1], rows[2])
}

fn draw(frame: &mut Frame, view: &Viewer) {
    let (top, middle, bottom) = bands(frame.area());
    let mut bar = vec![
        Span::styled(
            " onlyne ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("session {}", view.task_id),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if view.doc.journal_exists {
        bar.push(Span::styled(
            format!("  {}  ", view.doc.turn_note()),
            Style::default().fg(Color::DarkGray),
        ));
        bar.push(Span::styled(
            format!("{} blocks", view.doc.blocks.len()),
            Style::default().fg(Color::DarkGray),
        ));
    } else {
        bar.push(Span::styled(
            "  no journal",
            Style::default().fg(Color::Red),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(bar)), top);

    frame.render_widget(
        Paragraph::new(Text::from(styled(&view.lines)))
            .wrap(Wrap { trim: false })
            .scroll((view.scroll, 0))
            .block(Block::default().borders(Borders::ALL)),
        middle,
    );

    frame.render_widget(
        Paragraph::new(footer(view).as_str()).style(Style::default().fg(Color::DarkGray)),
        bottom,
    );
}

fn styled(lines: &[ContentLine]) -> Vec<Line<'static>> {
    lines
        .iter()
        .map(|line| Line::from(Span::styled(line.text.clone(), style_for(line.kind))))
        .collect()
}

fn style_for(kind: LineKind) -> Style {
    match kind {
        LineKind::User => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        LineKind::Assistant => Style::default().fg(Color::White),
        // Reasoning reads as reasoning: dim, italic, and its own marker.
        LineKind::Reasoning => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
        LineKind::Tool => Style::default().fg(Color::Magenta),
        LineKind::Blank => Style::default(),
        LineKind::Notice => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::ITALIC),
    }
}

/// The footer, with the two things an operator checks first: how much is being
/// shown, and which file it came from.
fn footer(view: &Viewer) -> String {
    format!(
        "mode {} · {} · {} · keys: m mode  j/k·↑↓ line  PgUp/PgDn page  End tail  Home top  r reload  q quit",
        view.mode.label(),
        view.doc.path.display(),
        if view.following { "live" } else { "back" },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A journal long enough to run past one pane, in the shape the client writes.
    fn write_journal(dir: &Path, task_id: &str, turns: usize) {
        let logs = dir.join(content::LOGS_RELATIVE);
        std::fs::create_dir_all(&logs).expect("logs dir");
        let mut body = String::from(
            r#"{"onlyne":{"kind":"dispatch","task_id":"t-1","prose":"Do the work.","at":"2026-09-18T10:00:00Z"}}
{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"Let me look."}}
"#,
        );
        for index in 0..turns {
            body.push_str(&format!(
                r#"{{"sessionUpdate":"tool_call","toolCallId":"call_{index}","status":"in_progress","title":"Edit file{index}.py","kind":"edit"}}
{{"sessionUpdate":"tool_call_update","toolCallId":"call_{index}","status":"completed"}}
{{"sessionUpdate":"agent_message_chunk","content":{{"type":"text","text":"Step {index} done. "}}}}
"#
            ));
        }
        body.push_str(r#"{"onlyne":{"kind":"turn","task_id":"t-1","stop_reason":"end_turn","head":"Step done.","at":"2026-09-18T10:01:00Z"}}"#);
        std::fs::write(logs.join(format!("session-{task_id}.events.jsonl")), body)
            .expect("journal");
    }

    #[test]
    fn m_toggles_the_mode_and_the_arrows_scroll() {
        assert_eq!(
            classify_key(KeyCode::Char('m'), KeyModifiers::NONE),
            ViewKey::ToggleMode
        );
        assert_eq!(
            classify_key(KeyCode::Down, KeyModifiers::NONE),
            ViewKey::Scroll(1)
        );
        assert_eq!(
            classify_key(KeyCode::Char('K'), KeyModifiers::NONE),
            ViewKey::Scroll(-1)
        );
        assert_eq!(
            classify_key(KeyCode::Char('q'), KeyModifiers::NONE),
            ViewKey::Quit
        );
        assert_eq!(
            classify_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
            ViewKey::Quit
        );
        assert_eq!(
            classify_key(KeyCode::Char('z'), KeyModifiers::NONE),
            ViewKey::Ignore
        );
    }

    /// Scrolling back off the tail stops the view from following; reaching the
    /// last line the pane can show resumes it.
    #[test]
    fn scrolling_back_pauses_following_and_end_resumes_it() {
        let dir = tempfile::tempdir().expect("temp dir");
        write_journal(dir.path(), "t-1", 30);
        let mut view = Viewer::open(JournalSource::new(dir.path()), "t-1", ContentMode::Full);
        view.set_pane(Rect::new(0, 0, 40, 8));
        view.reload();
        assert!(view.following, "the page opens on the tail");
        assert!(
            view.scroll > 0,
            "the tail is below the fold in an 8-row pane"
        );
        let full = view.body.clone();
        view.set_mode(ContentMode::Compact);
        assert!(!view.body.contains("Let me look."), "{full}");
        view.scroll_by(-1);
        assert!(
            !view.following,
            "a scroll up is the operator taking the wheel"
        );
        view.set_follow(true);
        assert!(view.following, "End hands the page back to the journal");
    }

    #[test]
    fn a_journal_that_is_not_there_is_said_on_the_page() {
        let dir = tempfile::tempdir().expect("temp dir");
        let view = Viewer::open(JournalSource::new(dir.path()), "t-9", ContentMode::Full);
        assert_eq!(view.lines.len(), 1);
        assert_eq!(view.lines[0].kind, LineKind::Notice);
        assert!(
            view.body.contains(".onlyne/logs/session-t-9.events.jsonl"),
            "{}",
            view.body
        );
        let footer = footer(&view);
        assert!(footer.contains("mode full"), "{footer}");
        assert!(footer.contains("m mode"), "{footer}");
    }

    /// The text frame is as tall as the journal, so `--once` loses nothing.
    #[test]
    fn the_text_frame_holds_the_whole_journal() {
        let dir = tempfile::tempdir().expect("temp dir");
        write_journal(dir.path(), "t-1", 6);
        let mut view = Viewer::open(JournalSource::new(dir.path()), "t-1", ContentMode::Full);
        let text = view.render_text();
        assert_eq!(view.scroll, 0, "nothing is scrolled away in a tall frame");
        assert!(
            text.starts_with(" onlyne session t-1"),
            "the bar names the session first\n{text}"
        );
        assert!(text.contains("> Do the work."), "{text}");
        assert!(text.contains("~ Let me look."), "{text}");
        assert!(
            text.contains("  [edit] Edit file5.py (completed)"),
            "{text}"
        );
        assert!(text.contains("turn end_turn"), "{text}");
    }
}
