//! Page two: one task family's path across roles.
//!
//! The plan asks for every delivery of one family, every receipt, and the tail
//! of the serving session's log. A delivery and its receipt are one ledger row —
//! the receipt is the row's own settling facts (`Outcome`, `reason`,
//! `out_head`, `acked_at`) — while the session log is the stream's account of
//! the row that took it, which is why the two are drawn in two panes rather
//! than fused into a third shape.

use crate::tui::render::{self, clock, gutter, one_line, pane, principal_word, short, table};
use crate::tui::rows;
use crate::tui::state::State;
use onlyne_proto::Event;
use onlyne_proto::view::{DeliveryAxis, DeliveryView};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::Paragraph;

/// Draw the task page into `area`.
pub fn render(frame: &mut Frame, state: &State, area: Rect) {
    let rows =
        Layout::vertical([Constraint::Min(6), Constraint::Length(log_height(area))]).split(area);
    let columns = Layout::horizontal([Constraint::Length(family_width(area)), Constraint::Min(34)])
        .split(rows[0]);
    families(frame, state, columns[0]);
    deliveries(frame, state, columns[1]);
    log(frame, state, rows[1]);
}

/// Every family the view knows, so the page is reachable without a card.
fn families(frame: &mut Frame, state: &State, area: Rect) {
    let focused = state.ui.pane == 0;
    let families = rows::families(&state.view);
    if families.is_empty() {
        render::empty(
            frame,
            area,
            "families".to_string(),
            "no task family yet: the ledger has no delivery carrying one",
            focused,
        );
        return;
    }
    let selected = families
        .iter()
        .position(|family| state.ui.family.as_ref() == Some(family));
    let body: Vec<Vec<String>> = families
        .iter()
        .enumerate()
        .map(|(index, family)| {
            let deliveries = rows::family_deliveries(&state.view, family);
            let root = deliveries
                .first()
                .map(|delivery| principal_word(&delivery.from))
                .unwrap_or_else(|| "—".to_string());
            let hop = deliveries
                .iter()
                .filter_map(|delivery| delivery.hop)
                .max()
                .unwrap_or_default();
            vec![
                format!("{} {}", gutter(Some(index) == selected), short(family)),
                root,
                format!("{} del", deliveries.len()),
                format!("h{hop}"),
            ]
        })
        .collect();
    let (block, inner) = pane(format!("families ({})", families.len()), focused, area);
    let table = table(
        vec!["family", "root", "size", "hop"],
        body,
        vec![
            Constraint::Min(10),
            Constraint::Min(8),
            Constraint::Length(6),
            Constraint::Length(3),
        ],
        selected,
    );
    frame.render_widget(block, area);
    frame.render_widget(table, inner);
}

/// One family's deliveries, each with the receipt it settled with.
fn deliveries(frame: &mut Frame, state: &State, area: Rect) {
    let focused = state.ui.pane == 1;
    let Some(family) = state.ui.family.clone() else {
        render::empty(
            frame,
            area,
            "deliveries".to_string(),
            "no family selected",
            focused,
        );
        return;
    };
    let deliveries = rows::family_deliveries(&state.view, &family);
    let origin = deliveries
        .iter()
        .find_map(|delivery| delivery.origin.clone())
        .unwrap_or_else(|| "—".to_string());
    let title = format!(
        "family {} · origin {origin} · {} deliveries",
        short(&family),
        deliveries.len()
    );
    if deliveries.is_empty() {
        render::empty(
            frame,
            area,
            title,
            "no delivery carries this family",
            focused,
        );
        return;
    }
    let selected = deliveries
        .iter()
        .position(|delivery| state.ui.delivery.as_ref() == Some(&delivery.msg_id));
    let body: Vec<Vec<String>> = deliveries
        .iter()
        .enumerate()
        .map(|(index, delivery)| {
            vec![
                format!("{} {}", gutter(Some(index) == selected), hop_of(delivery)),
                short(&delivery.msg_id),
                format!(
                    "{}→{}",
                    principal_word(&delivery.from),
                    principal_word(&delivery.to)
                ),
                delivery.kind.as_str().to_string(),
                delivery.state.to_string(),
                verdict(state, delivery),
                receipt(delivery),
            ]
        })
        .collect();
    let (block, inner) = pane(title, focused, area);
    let table = table(
        vec!["hop", "msg", "route", "kind", "state", "verdict", "receipt"],
        body,
        vec![
            Constraint::Length(3),
            Constraint::Length(8),
            Constraint::Length(26),
            Constraint::Length(10),
            Constraint::Length(9),
            Constraint::Length(7),
            // The receipt is the widest fact on this page and the one the plan
            // sends a reader here for: it gets what is left, not a minimum.
            Constraint::Min(40),
        ],
        selected,
    );
    frame.render_widget(block, area);
    frame.render_widget(table, inner);
}

/// The tail of the log of the session serving the selected delivery.
fn log(frame: &mut Frame, state: &State, area: Rect) {
    let focused = state.ui.pane == 2;
    let session = selected_delivery(state)
        .and_then(|delivery| delivery.task_id.as_deref())
        .and_then(|task_id| state.view.session_for(task_id));
    let Some(session) = session else {
        render::empty(
            frame,
            area,
            "session log".to_string(),
            "no session is serving the selected delivery",
            focused,
        );
        return;
    };
    let title = match state.ui.scroll {
        0 => format!(
            "session log: {} · {}",
            short(&session.session_id),
            state_role(state, session)
        ),
        back => format!(
            "session log: {} · {} · scrolled back {back}",
            short(&session.session_id),
            state_role(state, session)
        ),
    };
    let (block, inner) = pane(title, focused, area);
    let lines: Vec<Event> = state
        .view
        .session_log(&session.session_id)
        .cloned()
        .collect();
    frame.render_widget(block, area);
    let window = rows::window(&lines, state.ui.scroll, inner.height as usize);
    if window.is_empty() {
        render::nothing(
            frame,
            inner,
            "the stream has carried nothing for this session yet",
        );
        return;
    }
    let text: Vec<String> = window
        .iter()
        .map(|event| format!("{} {}", event.type_name(), render::event_line(event)))
        .collect();
    frame.render_widget(Paragraph::new(text.join("\n")), inner);
}

/// The session's role and state, as its pane's title prints them.
fn state_role(state: &State, session: &onlyne_proto::view::SessionView) -> String {
    let role = session.role.clone().unwrap_or_else(|| "—".to_string());
    let counts = state.view.counts(&role);
    format!(
        "role {role} · busy {} idle {} suspended {}",
        counts.busy, counts.idle, counts.suspended
    )
}

/// A delivery's hop, or a dash when the row first arrived on the stream.
fn hop_of(delivery: &DeliveryView) -> String {
    delivery
        .hop
        .map(|hop| hop.to_string())
        .unwrap_or_else(|| "—".to_string())
}

/// The task's verdict: the delivery's own when the settling event carried one,
/// else the serving session's.
///
/// The `ledger` read has no outcome column — a settled row's verdict lives on
/// the session that took it ([`onlyne_proto::view::SessionView::outcome`], the
/// durable carrier) — so a page that read the delivery alone would print a dash
/// for every row of a snapshot, a blocked one included, and the plan wants that
/// one read as waiting rather than as failed.
fn verdict(state: &State, delivery: &DeliveryView) -> String {
    let outcome = delivery.outcome.or_else(|| {
        delivery
            .task_id
            .as_deref()
            .and_then(|task_id| state.view.session_for(task_id))
            .and_then(|session| session.outcome)
    });
    outcome
        .map(|outcome| outcome.as_str().to_string())
        .unwrap_or_else(|| "—".to_string())
}

/// What a delivery settled with: when it settled, the result's head line, and
/// the reason it carries when a rejection or an expiry ended it.
fn receipt(delivery: &DeliveryView) -> String {
    if delivery.axis() != DeliveryAxis::Settled {
        return "—".to_string();
    }
    let mut parts: Vec<String> = Vec::new();
    if let Some(when) = delivery.acked_at {
        parts.push(clock(Some(when)));
    }
    if let Some(head) = &delivery.out_head {
        parts.push(format!("\"{}\"", one_line(head, 28)));
    }
    if let Some(reason) = &delivery.reason {
        parts.push(one_line(reason, 28));
    }
    if parts.is_empty() {
        "settled".to_string()
    } else {
        parts.join(" ")
    }
}

/// The delivery the task page's keys act on.
fn selected_delivery(state: &State) -> Option<&DeliveryView> {
    let id = state.ui.delivery.as_ref()?;
    let family = state.ui.family.as_deref()?;
    rows::family_deliveries(&state.view, family)
        .into_iter()
        .find(|delivery| &delivery.msg_id == id)
}

/// How tall the log pane is.
fn log_height(area: Rect) -> u16 {
    (area.height / 3).clamp(4, 12)
}

/// How wide the family column is.
fn family_width(area: Rect) -> u16 {
    (area.width * 28 / 100).clamp(30, 44)
}
