//! Page one: the cluster — roles, the selected role's board, the event tail.
//!
//! The plan's three facts for this page are the registry row (presence and the
//! sessions it holds), how many of those sessions are busy, idle or suspended,
//! and how deep its queue is; the board beside it is the selected role's own
//! column of work, read as the joint projection of the two axes [`Card`]
//! carries. Nothing here is counted twice: the header's three numbers are
//! `View::counts`, the queue depth is the registry row's own `queued`, and a
//! card's column is `Card::column`.

use crate::tui::render::{self, gutter, pane, principal_word, session_word, short, table};
use crate::tui::rows;
use crate::tui::state::State;
use onlyne_proto::view::{BoardColumn, Card};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::Paragraph;

/// The five columns the board arranges its cards into, in the plan's order.
const COLUMNS: [BoardColumn; 5] = [
    BoardColumn::Queued,
    BoardColumn::Running,
    BoardColumn::Waiting,
    BoardColumn::Done,
    BoardColumn::FailedOrBlocked,
];

/// Draw the cluster page into `area`.
pub fn render(frame: &mut Frame, state: &State, area: Rect) {
    let rows =
        Layout::vertical([Constraint::Min(6), Constraint::Length(tail_height(area))]).split(area);
    let columns = Layout::horizontal([Constraint::Length(role_width(area)), Constraint::Min(30)])
        .split(rows[0]);
    roles(frame, state, columns[0]);
    board(frame, state, columns[1]);
    tail(frame, state, rows[1]);
}

/// The role registry: presence, the sessions it holds, and its queue.
fn roles(frame: &mut Frame, state: &State, area: Rect) {
    let focused = state.ui.pane == 0;
    let roles = rows::roles(&state.view);
    if roles.is_empty() {
        render::empty(
            frame,
            area,
            "roles".to_string(),
            "no roles: the snapshot has not answered yet",
            focused,
        );
        return;
    }
    let selected = roles
        .iter()
        .position(|role| state.ui.role.as_ref() == Some(&role.name));
    let body: Vec<Vec<String>> = roles
        .iter()
        .enumerate()
        .map(|(index, role)| {
            let counts = state.view.counts(&role.name);
            vec![
                format!("{} {}", gutter(Some(index) == selected), role.name),
                role.state.as_str().to_string(),
                format!("{}/{}", role.sessions, role.max_sessions),
                counts.busy.to_string(),
                counts.idle.to_string(),
                counts.suspended.to_string(),
                role.queued.to_string(),
            ]
        })
        .collect();
    let (block, inner) = pane(format!("roles ({})", roles.len()), focused, area);
    let table = table(
        vec!["role", "presence", "sess", "busy", "idle", "susp", "queue"],
        body,
        vec![
            Constraint::Min(10),
            Constraint::Length(9),
            Constraint::Length(6),
            Constraint::Length(4),
            Constraint::Length(4),
            Constraint::Length(4),
            Constraint::Length(5),
        ],
        selected,
    );
    frame.render_widget(block, area);
    frame.render_widget(table, inner);
}

/// The selected role's board: every delivery addressed to it, in its column.
fn board(frame: &mut Frame, state: &State, area: Rect) {
    let focused = state.ui.pane == 1;
    let Some(role) = state.ui.role.clone() else {
        render::empty(
            frame,
            area,
            "board".to_string(),
            "no role selected",
            focused,
        );
        return;
    };
    let cards = rows::cards(&state.view, &role);
    let counts: Vec<String> = COLUMNS
        .iter()
        .map(|column| {
            format!(
                "{} {}",
                column.as_str(),
                cards.iter().filter(|card| card.column() == *column).count()
            )
        })
        .collect();
    let title = format!("board: {role} · {}", counts.join(" · "));
    if cards.is_empty() {
        render::empty(
            frame,
            area,
            title,
            "nothing addressed to this role",
            focused,
        );
        return;
    }
    let selected = cards
        .iter()
        .position(|card| state.ui.card.as_ref() == Some(&card.delivery.msg_id));
    let body: Vec<Vec<String>> = cards
        .iter()
        .enumerate()
        .map(|(index, card)| {
            vec![
                format!(
                    "{} {}",
                    gutter(Some(index) == selected),
                    card.column().as_str()
                ),
                card.delivery.state.to_string(),
                card.delivery
                    .hop
                    .map(|hop| hop.to_string())
                    .unwrap_or_else(|| "—".to_string()),
                short(&card.delivery.msg_id),
                card.delivery.kind.as_str().to_string(),
                format!(
                    "{}→{}",
                    principal_word(&card.delivery.from),
                    principal_word(&card.delivery.to)
                ),
                session_cell(card),
            ]
        })
        .collect();
    let (block, inner) = pane(title, focused, area);
    let table = table(
        vec!["column", "ledger", "hop", "msg", "kind", "route", "session"],
        body,
        vec![
            Constraint::Length(17),
            Constraint::Length(9),
            Constraint::Length(3),
            Constraint::Length(8),
            Constraint::Length(10),
            Constraint::Min(12),
            Constraint::Min(8),
        ],
        selected,
    );
    frame.render_widget(block, area);
    frame.render_widget(table, inner);
}

/// The session half of a card: the state it is in, and the verdict that has it
/// waiting rather than failed.
fn session_cell(card: &Card<'_>) -> String {
    match card.session {
        None => "—".to_string(),
        Some(session) if session.is_blocked() => {
            format!("{} blocked", session_word(session.state()))
        }
        Some(session) => session_word(session.state()).to_string(),
    }
}

/// The event tail: what the subscription just delivered, newest first.
fn tail(frame: &mut Frame, state: &State, area: Rect) {
    let focused = state.ui.pane == 2;
    let scroll = state.ui.scroll;
    let title = match scroll {
        0 => format!("events (tail {})", state.view.event_tail.len()),
        back => format!(
            "events (tail {} · scrolled back {back})",
            state.view.event_tail.len()
        ),
    };
    let (block, inner) = pane(title, focused, area);
    let lines = rows::window(&state.view.event_tail, scroll, inner.height as usize);
    frame.render_widget(block, area);
    if lines.is_empty() {
        render::nothing(frame, inner, "nothing has arrived on the stream yet");
        return;
    }
    let text: Vec<String> = lines
        .iter()
        .map(|event| format!("{} {}", event.type_name(), render::event_line(event)))
        .collect();
    frame.render_widget(Paragraph::new(text.join("\n")), inner);
}

/// How tall the tail pane is: a third of the page, but never the whole of it.
fn tail_height(area: Rect) -> u16 {
    (area.height / 3).clamp(4, 12)
}

/// How wide the role column is: the rest of the page is the board.
fn role_width(area: Rect) -> u16 {
    (area.width * 45 / 100).clamp(34, 60)
}
