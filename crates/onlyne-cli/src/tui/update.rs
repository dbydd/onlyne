//! `update(State, Event) -> State`: the board's only way to move.
//!
//! Every input the board folds arrives here — a key, a terminal resize, a
//! snapshot, one event off the subscription, the link's health, an op's answer
//! — and every one of them leaves as a new [`State`]. Nothing else writes the
//! state, which is what makes the acceptance case for a page inkable from a
//! pinned `View`: the fold is a pure function over data a test already holds.
//!
//! The two facts a fold may not invent are the two the reducer owns. A
//! snapshot becomes a `View` through `snapshot_to_view` and a stream event
//! through `update`, and this module reaches neither map directly: a front end
//! that could write the view would be a second reader of the cluster with its
//! own opinion about what a delivery's state is.

use crate::tui::rows;
use crate::tui::state::{Action, Link, Notice, Page, Prompt, Repair, State};
use crossterm::event::{self as term, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use onlyne_proto::Event as ProtoEvent;
use onlyne_proto::view::{self, Snapshot};

/// One input the board folds.
#[derive(Debug, Clone)]
pub enum Event {
    /// The five admin reads, as the one snapshot the reducer takes.
    Snapshot(Box<Snapshot>),
    /// One event off the subscription.
    Stream(Box<ProtoEvent>),
    /// What the IO task's link is doing.
    Link(Link),
    /// What an op the operator asked for answered.
    Answered {
        /// The op's word, as the notice prints it.
        op: &'static str,
        /// The answer's one line, or why it refused.
        result: Result<String, String>,
    },
    /// One terminal input.
    Terminal(term::Event),
}

/// Fold one event into the state. Pure: no clock, no socket, no disk.
pub fn update(state: State, event: Event) -> State {
    let mut state = state;
    match event {
        Event::Snapshot(snapshot) => state.view = view::snapshot_to_view(&snapshot),
        Event::Stream(event) => state.view = view::update(state.view, &event),
        Event::Link(link) => state.ui.link = link,
        Event::Answered { op, result } => {
            state.ui.notice = Some(match result {
                Ok(detail) => Notice::ok(op, &detail),
                Err(why) => Notice::error(op, &why),
            });
        }
        Event::Terminal(event) => match event {
            // A terminal that reports releases reports each key twice.
            term::Event::Key(key) if key.kind != KeyEventKind::Release => {
                key_press(&mut state, key)
            }
            // A resize, a paste, and a focus change move nothing this screen
            // holds: the frame's area is the canvas the terminal hands the
            // renderer, and every new event redraws anyway.
            _ => {}
        },
    }
    settle(&mut state);
    state
}

/// Fold one key press, in whichever vocabulary the open form or the page has.
fn key_press(state: &mut State, key: KeyEvent) {
    if state.ui.prompt.is_some() {
        prompt_key(state, key);
        return;
    }
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    match (key.code, control) {
        (KeyCode::Char('q'), false) | (KeyCode::Esc, _) => state.ui.quit = true,
        (KeyCode::Char('c'), true) => state.ui.quit = true,
        (KeyCode::Char('r'), true) => state.ui.actions.push(Action::Refresh),
        (KeyCode::Char(digit), false) if Page::from_number(digit).is_some() => {
            let page = Page::from_number(digit).expect("checked above");
            if state.ui.page != page {
                state.ui.page = page;
                state.ui.pane = 0;
                state.ui.scroll = 0;
            }
        }
        // The pane walk wraps, so `Tab` alone reaches every pane of a page.
        (KeyCode::Tab, _) => walk_pane(state, 1),
        (KeyCode::BackTab, _) => walk_pane(state, -1),
        (KeyCode::Up, _) => row(state, -1),
        (KeyCode::Down, _) => row(state, 1),
        (KeyCode::PageUp, _) => scroll(state, SCROLL_PAGE),
        (KeyCode::PageDown, _) => scroll(state, -SCROLL_PAGE),
        (KeyCode::Char('['), false) => scroll(state, SCROLL_STEP),
        (KeyCode::Char(']'), false) => scroll(state, -SCROLL_STEP),
        (KeyCode::Enter, _) => open_family(state),
        (KeyCode::Char('s'), false) => send_prompt(state),
        _ => match state.ui.page {
            Page::Cluster => cluster_key(state, key),
            Page::Task => task_key(state, key),
            Page::Faults => faults_key(state, key),
        },
    }
}

/// Cluster-page keys: `f` focuses the selected role's session, and `enter`
/// opens the selected card's family on the task page.
fn cluster_key(state: &mut State, key: KeyEvent) {
    if key.code == KeyCode::Char('f') {
        let task = selected_card(state).and_then(|card| card.delivery.task_id.clone());
        let to = state.ui.role.clone();
        state.ui.prompt = Some(Prompt::focus(
            &state.ui.sender,
            to.as_deref(),
            task.as_deref(),
        ));
    }
}

/// Task-page keys: `f` and `r` act on the selected delivery.
fn task_key(state: &mut State, key: KeyEvent) {
    let task = selected_delivery(state)
        .and_then(|delivery| delivery.task_id.clone())
        .or_else(|| state.ui.family.clone());
    match key.code {
        KeyCode::Char('f') => {
            let to = selected_delivery(state)
                .and_then(|delivery| role_of(&delivery.to))
                .or_else(|| state.ui.role.clone());
            state.ui.prompt = Some(Prompt::focus(
                &state.ui.sender,
                to.as_deref(),
                task.as_deref(),
            ));
        }
        KeyCode::Char('r') => {
            state.ui.prompt = Some(Prompt::report(&state.ui.sender, task.as_deref()));
        }
        _ => {}
    }
}

/// Faults-page keys: the repair verbs the selected fault offers.
fn faults_key(state: &mut State, key: KeyEvent) {
    let KeyCode::Char(letter) = key.code else {
        return;
    };
    let Some(fault) = selected_fault(state) else {
        return;
    };
    if let Some(verb) = Repair::offered(fault)
        .into_iter()
        .find(|verb| verb.key() == letter)
    {
        state.ui.prompt = Some(Prompt::repair(verb));
    }
}

/// Open the selected card's family on the task page.
fn open_family(state: &mut State) {
    if state.ui.page != Page::Cluster {
        return;
    }
    let Some(family) = selected_card(state).and_then(|card| {
        card.delivery
            .family
            .clone()
            .or_else(|| card.delivery.task_id.clone())
    }) else {
        return;
    };
    state.ui.page = Page::Task;
    state.ui.pane = 0;
    state.ui.family = Some(family);
    state.ui.scroll = 0;
}

/// `s`: send a task, from the selected role when the page has one selected.
fn send_prompt(state: &mut State) {
    let to = state.ui.role.clone();
    state.ui.prompt = Some(Prompt::send(&state.ui.sender, to.as_deref()));
}

/// One key inside an open form.
fn prompt_key(state: &mut State, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => state.ui.prompt = None,
        KeyCode::Enter => submit(state),
        KeyCode::Tab => focus_field(state, 1),
        KeyCode::BackTab => focus_field(state, -1),
        KeyCode::Backspace => {
            if let Some(prompt) = state.ui.prompt.as_mut() {
                prompt.backspace();
            }
        }
        KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(prompt) = state.ui.prompt.as_mut() {
                prompt.insert(character);
            }
        }
        _ => {}
    }
}

/// Move the caret inside the open form.
fn focus_field(state: &mut State, delta: isize) {
    if let Some(prompt) = state.ui.prompt.as_mut() {
        prompt.next_field(delta);
    }
}

/// Submit the open form, or leave it open and say what it is missing.
fn submit(state: &mut State) {
    let Some(prompt) = state.ui.prompt.clone() else {
        return;
    };
    let mut sender = state.ui.sender.clone();
    match prompt.submit(&mut sender) {
        Ok(action) => {
            state.ui.sender = sender;
            state.ui.prompt = None;
            state.ui.actions.push(action);
        }
        Err(why) => state.ui.notice = Some(Notice::error(prompt.title(), &why)),
    }
}

/// Move the pane focus, wrapping.
fn walk_pane(state: &mut State, delta: isize) {
    let panes = state.ui.page.panes().len() as isize;
    state.ui.pane = (state.ui.pane as isize + delta).rem_euclid(panes.max(1)) as usize;
}

/// Step the selected row of the focused pane, or scroll it when it is a pane
/// of lines rather than of rows.
fn row(state: &mut State, delta: isize) {
    let moved = match state.ui.page {
        Page::Cluster => match state.ui.pane {
            0 => {
                let names: Vec<String> = rows::roles(&state.view)
                    .into_iter()
                    .map(|role| role.name.clone())
                    .collect();
                step(&mut state.ui.role, &names, delta)
            }
            1 => {
                let ids: Vec<String> = rows::cards(&state.view, role_of_selected(state))
                    .into_iter()
                    .map(|card| card.delivery.msg_id.clone())
                    .collect();
                step(&mut state.ui.card, &ids, delta)
            }
            _ => {
                scroll(state, -delta);
                false
            }
        },
        Page::Task => match state.ui.pane {
            0 => {
                let families = rows::families(&state.view);
                step(&mut state.ui.family, &families, delta)
            }
            1 => {
                let ids: Vec<String> = rows::family_deliveries(&state.view, family_of(state))
                    .into_iter()
                    .map(|delivery| delivery.msg_id.clone())
                    .collect();
                step(&mut state.ui.delivery, &ids, delta)
            }
            _ => {
                scroll(state, -delta);
                false
            }
        },
        Page::Faults => match state.ui.pane {
            0 => {
                let ids: Vec<i64> = rows::open_faults(&state.view)
                    .into_iter()
                    .map(|fault| fault.id)
                    .collect();
                step(&mut state.ui.fault, &ids, delta)
            }
            _ => {
                scroll(state, -delta);
                false
            }
        },
    };
    if moved {
        // A new subject starts at its newest line: the tail is what just
        // happened, and leaving it scrolled back would hide it.
        state.ui.scroll = 0;
    }
}

/// Scroll the focused pane's lines: positive goes back in time.
fn scroll(state: &mut State, delta: isize) {
    let limit = scroll_limit(state);
    let next = (state.ui.scroll as isize + delta).clamp(0, limit as isize);
    state.ui.scroll = next as usize;
}

/// How far the focused pane's lines can be scrolled back.
fn scroll_limit(state: &State) -> usize {
    match state.ui.page {
        Page::Cluster => state.view.event_tail.len().saturating_sub(1),
        Page::Task => {
            let session = rows::family_deliveries(&state.view, family_of(state))
                .into_iter()
                .find(|delivery| delivery.msg_id == selected_id(state))
                .and_then(|delivery| delivery.task_id.clone())
                .and_then(|task_id| state.view.session_for(&task_id))
                .map(|session| session.session_id.clone());
            match session {
                Some(session_id) => state
                    .view
                    .session_log(&session_id)
                    .count()
                    .saturating_sub(1),
                None => 0,
            }
        }
        // A fault's detail is a handful of fields: the observed value can run
        // long, and this is how far a reader can walk into it.
        Page::Faults => FAULT_DETAIL_LINES,
    }
}

/// Move one selection to the neighbouring candidate, clamped at both ends.
fn step<T: PartialEq + Clone>(slot: &mut Option<T>, candidates: &[T], delta: isize) -> bool {
    if candidates.is_empty() {
        let had = slot.is_some();
        *slot = None;
        return had;
    }
    let index = slot
        .as_ref()
        .and_then(|held| candidates.iter().position(|candidate| candidate == held))
        .unwrap_or(0) as isize;
    let next = (index + delta).clamp(0, candidates.len() as isize - 1) as usize;
    let moved = slot.as_ref() != Some(&candidates[next]);
    *slot = Some(candidates[next].clone());
    moved
}

/// Keep every selection pointing at a row the view still holds.
///
/// The stream removes as well as adds: a session that closed drops out of the
/// three counts, a handled fault leaves the open set, and a selection left on a
/// row that is gone is a highlighted line nothing answers for. A slot that
/// still names a live row keeps it, so a board that moves under the operator
/// does not move the operator's place in it.
fn settle(state: &mut State) {
    state.ui.pane = state
        .ui
        .pane
        .min(state.ui.page.panes().len().saturating_sub(1));

    let roles: Vec<String> = rows::roles(&state.view)
        .into_iter()
        .map(|role| role.name.clone())
        .collect();
    keep(&mut state.ui.role, &roles);

    let cards: Vec<String> = rows::cards(&state.view, role_of_selected(state))
        .into_iter()
        .map(|card| card.delivery.msg_id.clone())
        .collect();
    keep(&mut state.ui.card, &cards);

    keep(&mut state.ui.family, &rows::families(&state.view));

    let deliveries: Vec<String> = rows::family_deliveries(&state.view, family_of(state))
        .into_iter()
        .map(|delivery| delivery.msg_id.clone())
        .collect();
    keep(&mut state.ui.delivery, &deliveries);

    let faults: Vec<i64> = rows::open_faults(&state.view)
        .into_iter()
        .map(|fault| fault.id)
        .collect();
    keep(&mut state.ui.fault, &faults);

    let limit = scroll_limit(state);
    state.ui.scroll = state.ui.scroll.min(limit);
}

/// Point one selection at a live row, keeping it when it already names one.
fn keep<T: PartialEq + Clone>(slot: &mut Option<T>, candidates: &[T]) {
    if let Some(held) = slot.as_ref() {
        if candidates.iter().any(|candidate| candidate == held) {
            return;
        }
    }
    *slot = candidates.first().cloned();
}

/// The selected role's name, or the empty string when nothing is selected.
fn role_of_selected(state: &State) -> &str {
    state.ui.role.as_deref().unwrap_or_default()
}

/// The selected family, or the empty string when nothing is selected.
fn family_of(state: &State) -> &str {
    state.ui.family.as_deref().unwrap_or_default()
}

/// The selected delivery's own key, or the empty string when nothing is
/// selected.
fn selected_id(state: &State) -> String {
    state.ui.delivery.clone().unwrap_or_default()
}

/// One page's selected row, by the key the page's selection holds.
fn selected_card(state: &State) -> Option<onlyne_proto::view::Card<'_>> {
    let id = state.ui.card.as_ref()?;
    rows::cards(&state.view, role_of_selected(state))
        .into_iter()
        .find(|card| &card.delivery.msg_id == id)
}

fn selected_delivery(state: &State) -> Option<&onlyne_proto::DeliveryView> {
    let id = state.ui.delivery.as_ref()?;
    rows::family_deliveries(&state.view, family_of(state))
        .into_iter()
        .find(|delivery| &delivery.msg_id == id)
}

fn selected_fault(state: &State) -> Option<&onlyne_proto::FaultEvent> {
    let id = state.ui.fault?;
    rows::open_faults(&state.view)
        .into_iter()
        .find(|fault| fault.id == id)
}

/// The role a principal addresses, for the ops a card's target names.
fn role_of(principal: &onlyne_proto::Principal) -> Option<String> {
    match principal {
        onlyne_proto::Principal::Role { role, .. } => Some(role.clone()),
        _ => None,
    }
}

/// Lines one `[` or `]` scrolls.
const SCROLL_STEP: isize = 1;

/// Lines `PgUp` or `PgDn` scrolls.
const SCROLL_PAGE: isize = 10;

/// How far into a fault's detail the scroll keys walk.
const FAULT_DETAIL_LINES: usize = 32;

#[cfg(test)]
mod tests;
