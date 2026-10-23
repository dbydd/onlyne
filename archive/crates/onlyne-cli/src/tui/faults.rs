//! Page three: the open faults, and the repair verb each one offers.
//!
//! A fault is the server's own record that something needs a person; the page
//! shows the open ones (`View::open_faults`, which is the reducer's reading of
//! the `state` word every repair verb moves) and, under the selected one, the
//! verbs that apply. A verb the fault cannot offer is not drawn at all, so the
//! page never invites a repair that has nothing to move.

use crate::tui::render::{self, clock_secs, compact, gutter, one_line, pane, short, table};
use crate::tui::rows;
use crate::tui::state::{Repair, State};
use onlyne_proto::FaultEvent;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::Paragraph;

/// Draw the faults page into `area`.
pub fn render(frame: &mut Frame, state: &State, area: Rect) {
    let rows = Layout::vertical([
        Constraint::Min(4),
        Constraint::Length(detail_height(area)),
        Constraint::Length(REPAIR_HEIGHT),
    ])
    .split(area);
    faults(frame, state, rows[0]);
    detail(frame, state, rows[1]);
    repair(frame, state, rows[2]);
}

/// The open faults, one row each.
fn faults(frame: &mut Frame, state: &State, area: Rect) {
    let focused = state.ui.pane == 0;
    let faults = rows::open_faults(&state.view);
    if faults.is_empty() {
        render::empty(
            frame,
            area,
            "open faults".to_string(),
            "no open fault: every record has been closed by a repair verb",
            focused,
        );
        return;
    }
    let selected = faults
        .iter()
        .position(|fault| state.ui.fault == Some(fault.id));
    let body: Vec<Vec<String>> = faults
        .iter()
        .enumerate()
        .map(|(index, fault)| {
            vec![
                format!("{} {}", gutter(Some(index) == selected), fault.id),
                fault.kind.clone(),
                fault.role.clone().unwrap_or_else(|| "—".to_string()),
                fault
                    .task_id
                    .as_deref()
                    .map(short)
                    .unwrap_or_else(|| "—".to_string()),
                fault
                    .session_id
                    .as_deref()
                    .map(short)
                    .unwrap_or_else(|| "—".to_string()),
                clock_secs(fault.created_at),
                one_line(&fault.reason, 40),
            ]
        })
        .collect();
    let (block, inner) = pane(format!("open faults ({})", faults.len()), focused, area);
    let table = table(
        vec!["id", "kind", "role", "task", "session", "created", "reason"],
        body,
        vec![
            Constraint::Length(5),
            Constraint::Min(14),
            Constraint::Length(10),
            Constraint::Length(8),
            Constraint::Length(8),
            Constraint::Length(8),
            Constraint::Min(20),
        ],
        selected,
    );
    frame.render_widget(block, area);
    frame.render_widget(table, inner);
}

/// The selected fault's own fields, including what it desired and observed.
fn detail(frame: &mut Frame, state: &State, area: Rect) {
    let focused = state.ui.pane == 1;
    let Some(fault) = selected(state) else {
        render::empty(
            frame,
            area,
            "fault detail".to_string(),
            "no fault selected",
            focused,
        );
        return;
    };
    let title = format!("fault #{} · {}", fault.id, fault.kind);
    let (block, inner) = pane(title, focused, area);
    frame.render_widget(block, area);
    let lines = detail_lines(fault);
    let shown: Vec<String> = lines
        .into_iter()
        .skip(state.ui.scroll)
        .take(inner.height as usize)
        .collect();
    frame.render_widget(Paragraph::new(shown.join("\n")), inner);
}

/// One fault's fields as labelled lines.
fn detail_lines(fault: &FaultEvent) -> Vec<String> {
    let field = |label: &str, value: String| format!("{label:<10} {value}");
    let mut lines = vec![
        field(
            "state",
            fault.state.clone().unwrap_or_else(|| "—".to_string()),
        ),
        field("reason", one_line(&fault.reason, 96)),
        field(
            "role",
            fault.role.clone().unwrap_or_else(|| "—".to_string()),
        ),
        field(
            "task",
            fault.task_id.clone().unwrap_or_else(|| "—".to_string()),
        ),
        field(
            "session",
            fault.session_id.clone().unwrap_or_else(|| "—".to_string()),
        ),
        field(
            "generation",
            fault
                .generation
                .map(|generation| generation.to_string())
                .unwrap_or_else(|| "—".to_string()),
        ),
        field(
            "intent",
            fault.intent.clone().unwrap_or_else(|| "—".to_string()),
        ),
        field(
            "attempt",
            fault
                .attempt
                .map(|attempt| attempt.to_string())
                .unwrap_or_else(|| "—".to_string()),
        ),
    ];
    if let Some(desired) = &fault.desired {
        lines.push(field("desired", compact(desired)));
    }
    if let Some(observed) = &fault.observed {
        lines.push(field("observed", compact(observed)));
    }
    if let Some(reference) = &fault.backend_ref {
        lines.push(field("backend", compact(reference)));
    }
    lines
}

/// The repair entry points the selected fault offers.
fn repair(frame: &mut Frame, state: &State, area: Rect) {
    let title = "repair".to_string();
    let (block, inner) = pane(title, false, area);
    frame.render_widget(block, area);
    let Some(fault) = selected(state) else {
        render::nothing(frame, inner, "select a fault to see the verbs it offers");
        return;
    };
    let lines: Vec<String> = Repair::offered(fault)
        .into_iter()
        .map(|verb| format!("[{}] {:<14} {}", verb.key(), verb.word(), verb.describe()))
        .collect();
    frame.render_widget(Paragraph::new(lines.join("\n")), inner);
}

/// The fault the page's keys act on.
fn selected(state: &State) -> Option<&FaultEvent> {
    let id = state.ui.fault?;
    rows::open_faults(&state.view)
        .into_iter()
        .find(|fault| fault.id == id)
}

/// How tall the detail pane is: enough for the labelled fields, no more.
fn detail_height(area: Rect) -> u16 {
    (area.height / 2).clamp(6, 14)
}

/// The repair pane's fixed height: five verbs and a border.
const REPAIR_HEIGHT: u16 = 7;
