//! What the board draws: the frame's chrome, and the words the pages share.
//!
//! [`render`] takes `&State` and a frame, and draws. It reads no clock, opens
//! no socket, and computes no fact about the cluster: every row it prints is a
//! field of the `View` the reducer built, or one of the readings the reducer
//! derives on demand (`Card::column`, `SessionView::state`, `View::counts`).
//! That is what makes a page assertable without a terminal — [`render_text`]
//! draws the same frame into an in-memory buffer, which is the text `--once`
//! prints and the text the tests read.

use crate::tui::cluster;
use crate::tui::faults;
use crate::tui::state::{Level, State};
use crate::tui::task;
use chrono::{DateTime, Utc};
use onlyne_proto::view::SessionState;
use onlyne_proto::{Event, Lifecycle, Principal};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Cell, Clear, Paragraph, Row, Table, Wrap};

/// The header's two lines.
pub const HEADER: u16 = 2;
/// The footer's two lines.
pub const FOOTER: u16 = 2;

/// The narrowest terminal three panes can share.
const MIN_WIDTH: u16 = 56;
/// The shortest terminal that still draws a header, a body and a footer.
const MIN_HEIGHT: u16 = 10;

/// Draw the board.
pub fn render(frame: &mut Frame, state: &State) {
    let area = frame.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        frame.render_widget(
            Paragraph::new(format!(
                "onlyne tui: {} — the terminal is too small; widen it to {MIN_WIDTH}x{MIN_HEIGHT}",
                state.ui.page.title()
            ))
            .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }
    let rows = Layout::vertical([
        Constraint::Length(HEADER),
        Constraint::Min(4),
        Constraint::Length(FOOTER),
    ])
    .split(area);
    header(frame, state, rows[0]);
    match state.ui.page {
        crate::tui::state::Page::Cluster => cluster::render(frame, state, rows[1]),
        crate::tui::state::Page::Task => task::render(frame, state, rows[1]),
        crate::tui::state::Page::Faults => faults::render(frame, state, rows[1]),
    }
    footer(frame, state, rows[2]);
    if let Some(prompt) = &state.ui.prompt {
        overlay(frame, rows[1], prompt);
    }
}

/// Draw one frame into an in-memory backend and flatten it to text.
///
/// `--once` and the render tests share this path, so the text they assert on is
/// the text an operator reads in the alternate screen.
pub fn render_text(state: &State, width: u16, height: u16) -> String {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).expect("in-memory backend");
    terminal
        .draw(|frame| render(frame, state))
        .expect("draw one frame");
    buffer_text(terminal.backend().buffer())
}

/// One line per buffer row, right-trimmed.
pub fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
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

/// The two header lines: what this board is watching, and how the link is.
fn header(frame: &mut Frame, state: &State, area: Rect) {
    let summary = &state.view.cluster;
    let mut line = vec![
        Span::styled("onlyne tui", Style::new().add_modifier(Modifier::BOLD)),
        Span::raw(format!(" · {} page", state.ui.page.title())),
    ];
    if let Some(cluster) = &summary.cluster {
        line.push(Span::raw(format!(" · {cluster}")));
    }
    if let Some(version) = &summary.version {
        line.push(Span::raw(format!(" {version}")));
    }
    if let Some(hash) = &summary.spec_hash {
        line.push(Span::raw(format!(" · spec {}", short(hash))));
    }
    if let Some(head) = summary.event_head {
        line.push(Span::raw(format!(" · head #{head}")));
    }
    line.push(Span::raw(format!(
        " · roles {} · sessions {} · deliveries {} · faults {} open",
        state.view.roles.len(),
        state.view.sessions.len(),
        state.view.deliveries.len(),
        state.view.open_faults().count(),
    )));

    let mut status = vec![Span::styled(
        format!("link: {}", state.ui.link.word()),
        Style::new().fg(match state.ui.link {
            crate::tui::state::Link::Live => Color::Green,
            crate::tui::state::Link::Connecting => Color::Yellow,
            crate::tui::state::Link::Offline(_) => Color::Red,
        }),
    )];
    if state.view.stale {
        status.push(Span::styled(
            " · catching up: the stream lost events; re-reading the snapshot",
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(count) = summary.connected_roles {
        status.push(Span::raw(format!(" · connected roles {count}")));
    }
    if let Some(count) = summary.connected_gateways {
        status.push(Span::raw(format!(" · connected gateways {count}")));
    }

    frame.render_widget(
        Paragraph::new(vec![Line::from(line), Line::from(status)]),
        area,
    );
}

/// The two footer lines: the page's keys, then the last op's answer.
fn footer(frame: &mut Frame, state: &State, area: Rect) {
    let pane = state
        .ui
        .page
        .panes()
        .get(state.ui.pane)
        .copied()
        .unwrap_or("");
    // The link's own word is the header's second line; the footer says where the
    // keys are pointed and which ops this page has.
    let keys = Line::from(format!("pane {pane} · {}", state.ui.page.legend()));
    let notice = match &state.ui.notice {
        Some(notice) => Line::from(Span::styled(
            notice.text.clone(),
            Style::new().fg(match notice.level {
                Level::Info => Color::Cyan,
                Level::Error => Color::Red,
            }),
        )),
        None => Line::from(""),
    };
    frame.render_widget(Paragraph::new(vec![keys, notice]), area);
}

/// The open form, drawn over the body.
fn overlay(frame: &mut Frame, body: Rect, prompt: &crate::tui::state::Prompt) {
    let fields = prompt.fields();
    let height = (fields.len() as u16 + 4).min(body.height);
    if height < 4 || body.width < 24 {
        return;
    }
    let width = body.width.saturating_sub(8).clamp(24, 96);
    let area = Rect {
        x: body.x + (body.width - width) / 2,
        y: body.y + (body.height - height) / 2,
        width,
        height,
    };
    // The caret's field is named in the title as well as highlighted: a
    // highlight is a style, and a reader of the plain buffer a test (or a
    // `--once` run) prints has no styles to see.
    let block = Block::bordered()
        .border_type(BorderType::Double)
        .title(format!(
            "{} · {} (esc cancels)",
            prompt.title(),
            prompt.focus_label()
        ));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    let lines: Vec<Line> = fields
        .iter()
        .enumerate()
        .map(|(index, (label, value))| {
            let style = if index == prompt.caret() {
                Style::new().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::new()
            };
            Line::from(vec![
                Span::styled(format!("{label:>8} "), Style::new().fg(Color::DarkGray)),
                Span::styled((*value).to_string(), style),
            ])
        })
        .chain(std::iter::once(Line::from(Span::styled(
            prompt.help(),
            Style::new().fg(Color::DarkGray),
        ))))
        .collect();
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// A bordered pane: a title, and a body the caller fills.
pub fn pane(title: String, focused: bool, area: Rect) -> (Block<'static>, Rect) {
    let style = if focused {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new().fg(Color::DarkGray)
    };
    let block = Block::bordered().title(title).border_style(style);
    let inner = block.inner(area);
    (block, inner)
}

/// A table of rows, with the selected row's own marker already in its first
/// cell, so the buffer a test reads says which row the keys act on.
pub fn table<'a>(
    header: Vec<&'a str>,
    rows: Vec<Vec<String>>,
    widths: Vec<Constraint>,
    selected: Option<usize>,
) -> Table<'a> {
    let head = Row::new(header).style(Style::new().fg(Color::DarkGray));
    let rows: Vec<Row> = rows
        .into_iter()
        .enumerate()
        .map(|(index, cells)| {
            let row = Row::new(cells.into_iter().map(Cell::from).collect::<Vec<_>>());
            if Some(index) == selected {
                row.style(Style::new().add_modifier(Modifier::REVERSED))
            } else {
                row
            }
        })
        .collect();
    Table::new(rows, widths).header(head).column_spacing(1)
}

/// One pane with nothing in it yet, saying so rather than drawing an empty box.
pub fn empty(frame: &mut Frame, area: Rect, title: String, why: &str, focused: bool) {
    let (block, inner) = pane(title, focused, area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(why.to_string())
            .style(Style::new().fg(Color::DarkGray))
            .wrap(Wrap { trim: true }),
        inner,
    );
}

/// The left gutter every pane's rows share: `▸` on the selected row.
pub fn gutter(selected: bool) -> String {
    if selected {
        "▸".to_string()
    } else {
        " ".to_string()
    }
}

/// The first eight characters of an id, which is what fits in a cell.
pub fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

/// Wall-clock time of day for a stamped row.
pub fn clock(when: Option<DateTime<Utc>>) -> String {
    match when {
        Some(when) => when.format("%H:%M:%S").to_string(),
        None => "—".to_string(),
    }
}

/// Time of day for a row dated in unix seconds.
pub fn clock_secs(when: Option<i64>) -> String {
    when.and_then(|secs| DateTime::from_timestamp(secs, 0))
        .map(|when| when.format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "—".to_string())
}

/// The role a principal names, or the principal's other kind by name.
pub fn principal_word(principal: &Principal) -> String {
    match principal {
        Principal::Role { role, .. } => role.clone(),
        Principal::Gateway { gateway, .. } => format!("gateway:{gateway}"),
        Principal::Cluster { cluster } => format!("cluster:{cluster}"),
    }
}

/// The three session states the board header counts, and the two it does not.
pub fn session_word(state: SessionState) -> &'static str {
    match state {
        SessionState::Opening => "opening",
        SessionState::Busy => "busy",
        SessionState::Idle => "idle",
        SessionState::Suspended => "suspended",
        SessionState::Closed => "closed",
    }
}

/// The public lifecycle word, which the event tail prints off a raw event.
pub fn lifecycle_word(lifecycle: Lifecycle) -> &'static str {
    match lifecycle {
        Lifecycle::Created => "created",
        Lifecycle::Working => "working",
        Lifecycle::Idle => "idle",
        Lifecycle::Exited => "exited",
    }
}

/// One line per event, for the cluster page's tail and a session's log.
pub fn event_line(event: &Event) -> String {
    match event {
        Event::RolePresence(reported) => format!(
            "role {} {} sessions={}{}",
            reported.role,
            reported.state.as_str(),
            reported.sessions,
            reported
                .detail
                .as_deref()
                .map(|detail| format!(" {detail}"))
                .unwrap_or_default()
        ),
        Event::SessionState(reported) => format!(
            "session {} {} lifecycle={} agent={} delivery={} resource={}",
            short(&reported.session_id),
            reported.role,
            lifecycle_word(reported.projection.lifecycle),
            reported.projection.agent,
            reported.projection.delivery,
            reported.projection.resource,
        ),
        Event::LedgerState(reported) => format!(
            "{} {}→{} {}{}",
            short(&reported.msg_id),
            principal_word(&reported.from),
            principal_word(&reported.to),
            reported.state,
            reported
                .outcome
                .map(|outcome| format!(" {}", outcome.as_str()))
                .unwrap_or_default(),
        ),
        Event::Fault(reported) => format!(
            "fault #{} {} {}{}",
            reported.id,
            reported.kind,
            reported.role.as_deref().unwrap_or("—"),
            match reported.reason.is_empty() {
                true => String::new(),
                false => format!(" {}", one_line(&reported.reason, 48)),
            }
        ),
        Event::GatewayPresence { gateway, state, .. } => {
            format!("gateway {gateway} {}", state.as_str())
        }
        Event::SpecReloaded(reloaded) => format!(
            "spec {} roles={} gateways={} routes={}",
            short(&reloaded.spec_hash),
            reloaded.roles,
            reloaded.gateways,
            reloaded.routes
        ),
        Event::TurnEndWithoutComplete(value)
        | Event::DeliveryBlocked(value)
        | Event::Handoff(value) => {
            format!("{} {}", event.type_name(), compact(value))
        }
    }
}

/// One value as a single line, shortened for a table cell.
pub fn compact(value: &serde_json::Value) -> String {
    let text = serde_json::to_string(value).unwrap_or_else(|_| "null".to_string());
    one_line(&text, 64)
}

/// One line of text, shortened to `limit` characters.
pub fn one_line(text: &str, limit: usize) -> String {
    let flattened: String = text
        .chars()
        .map(|character| match character {
            '\n' | '\r' | '\t' => ' ',
            other => other,
        })
        .collect();
    if flattened.chars().count() <= limit {
        return flattened;
    }
    let head: String = flattened.chars().take(limit.saturating_sub(1)).collect();
    format!("{head}…")
}

/// A row that says nothing rather than an empty pane.
pub fn nothing(frame: &mut Frame, area: Rect, line: &str) {
    frame.render_widget(
        Paragraph::new(line.to_string())
            .style(Style::new().fg(Color::DarkGray))
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: true }),
        area,
    );
}

#[cfg(test)]
mod tests;
