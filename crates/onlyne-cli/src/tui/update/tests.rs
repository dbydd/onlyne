//! Tests for the fold: keys, forms, the outbox, and the selections.
//!
//! Every case here calls `update` and nothing else. That is the point of the
//! split — `update` is `State -> State`, so a key that asks for an op is
//! assertable as the op it recorded rather than as a socket it opened, and a key
//! that touches only the screen is assertable as the `View` it did not move.

use super::*;
use crate::tui::fixture;
use crate::tui::state::{Level, Page, Prompt, Repair, State};
use crossterm::event as term;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use onlyne_proto::Outcome;

/// One key press, as the terminal reports it.
fn press(code: KeyCode) -> Event {
    Event::Terminal(term::Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

/// One modifier chord, as the terminal reports it.
fn chord(character: char, modifiers: KeyModifiers) -> Event {
    Event::Terminal(term::Event::Key(KeyEvent::new(
        KeyCode::Char(character),
        modifiers,
    )))
}

/// One character typed.
fn typed(text: &str) -> Vec<Event> {
    text.chars()
        .map(|character| press(KeyCode::Char(character)))
        .collect()
}

/// Fold a run of events into a state, in order.
fn fold(state: State, events: impl IntoIterator<Item = Event>) -> State {
    events.into_iter().fold(state, update)
}

#[test]
fn the_page_digits_move_and_tab_walks_the_panes() {
    let state = fixture::state();
    assert_eq!(
        state.ui.page,
        Page::Cluster,
        "the board opens on the cluster"
    );

    let state = update(state, press(KeyCode::Char('3')));
    assert_eq!((state.ui.page, state.ui.pane), (Page::Faults, 0));

    // The faults page has two panes, so `Tab` wraps and `BackTab` unwraps.
    let state = update(state, press(KeyCode::Tab));
    assert_eq!(state.ui.pane, 1);
    let state = update(state, press(KeyCode::Tab));
    assert_eq!(state.ui.pane, 0);
    let state = update(state, press(KeyCode::BackTab));
    assert_eq!(state.ui.pane, 1);

    // A page change puts the keys back on the page's first pane.
    let state = update(state, press(KeyCode::Char('1')));
    assert_eq!((state.ui.page, state.ui.pane), (Page::Cluster, 0));
}

#[test]
fn the_arrow_keys_step_the_selected_row_and_stop_at_the_ends() {
    let state = fixture::state();
    assert_eq!(state.ui.role.as_deref(), Some("builder"), "registry order");

    let state = update(state, press(KeyCode::Down));
    assert_eq!(state.ui.role.as_deref(), Some("planner"));
    let state = update(state, press(KeyCode::Down));
    assert_eq!(
        state.ui.role.as_deref(),
        Some("planner"),
        "the last row is as far as the selection goes"
    );
    let state = update(state, press(KeyCode::Up));
    assert_eq!(state.ui.role.as_deref(), Some("builder"));
    let state = update(state, press(KeyCode::Up));
    assert_eq!(state.ui.role.as_deref(), Some("builder"));
}

#[test]
fn enter_opens_the_selected_cards_family_on_the_task_page() {
    // `tab` walks onto the board, where the selected row is the selected role's
    // first card.
    let state = fold(fixture::state(), [press(KeyCode::Tab)]);
    assert_eq!(state.ui.pane, 1);
    assert_eq!(state.ui.card.as_deref(), Some("m4"), "queued first");

    let state = fold(state, [press(KeyCode::Down)]);
    assert_eq!(state.ui.card.as_deref(), Some("m2"));

    let state = fold(state, [press(KeyCode::Enter)]);
    assert_eq!(state.ui.page, Page::Task);
    assert_eq!(state.ui.family.as_deref(), Some("t1"));
    assert_eq!(
        state.ui.delivery.as_deref(),
        Some("m1"),
        "the task page opens on the family's first hop"
    );
}

#[test]
fn s_sends_a_task_from_the_selected_role_and_never_writes_the_view() {
    let before = fixture::state();
    let state = update(before.clone(), press(KeyCode::Char('s')));
    let Some(Prompt::Send { from, to, .. }) = state.ui.prompt.clone() else {
        panic!("`s` opens the send form");
    };
    assert_eq!(from, "operator", "the sender is the operator's own role");
    assert_eq!(to, "builder", "the selected role is the default target");

    // The caret opens on `to`; `tab` steps to the body.
    let state = fold(state, [press(KeyCode::Tab)]);
    let state = fold(state, typed("build the thing"));
    assert_eq!(
        state.view, before.view,
        "a form is the screen's state, and the view is the reducer's"
    );

    let state = update(state, press(KeyCode::Enter));
    assert!(state.ui.prompt.is_none(), "submitting closes the form");
    assert_eq!(state.view, before.view);
    let mut state = state;
    assert_eq!(
        state.take_actions(),
        vec![Action::Send {
            from: "operator".to_string(),
            to: "builder".to_string(),
            body: "build the thing".to_string(),
        }]
    );
    assert!(
        state.take_actions().is_empty(),
        "the driver takes the outbox, so a second read carries nothing"
    );
}

#[test]
fn f_focuses_a_session_the_operator_addresses_by_hand() {
    // Focus is the one op whose target the plan makes explicit: the operator
    // names it rather than reading it off the selection.
    let state = update(fixture::state(), press(KeyCode::Char('f')));
    let Some(Prompt::Focus { to, task_id, .. }) = state.ui.prompt.clone() else {
        panic!("`f` opens the focus form");
    };
    assert_eq!(to, "builder", "the selection is what it starts from");
    assert_eq!(task_id, "t4", "the selected card's task");

    // Retype the target, keeping the task it is aimed at.
    let state = fold(
        state,
        [
            press(KeyCode::Backspace),
            press(KeyCode::Backspace),
            press(KeyCode::Backspace),
            press(KeyCode::Backspace),
            press(KeyCode::Backspace),
            press(KeyCode::Backspace),
            press(KeyCode::Backspace),
        ],
    );
    let state = fold(state, typed("writer"));
    let state = update(state, press(KeyCode::Enter));
    let mut state = state;
    assert_eq!(
        state.take_actions(),
        vec![Action::Focus {
            from: "operator".to_string(),
            to: "writer".to_string(),
            task_id: "t4".to_string(),
        }]
    );
}

#[test]
fn r_files_a_report_with_the_verdict_and_the_head_line() {
    let state = fold(fixture::state(), [press(KeyCode::Char('2'))]);
    let state = update(state, press(KeyCode::Char('r')));
    let Some(Prompt::Report {
        task_id,
        outcome,
        field,
        ..
    }) = state.ui.prompt.clone()
    else {
        panic!("`r` opens the report form");
    };
    assert_eq!(task_id, "t1", "the selected delivery's task");
    assert_eq!(outcome, "done", "and it opens on a verdict the wire has");
    assert_eq!(field, 2, "with the caret on the verdict");

    // `tab` steps to the head line, and the verdict is left as it opened.
    let state = fold(state, [press(KeyCode::Tab)]);
    let state = fold(state, typed("the plan is done"));
    let state = update(state, press(KeyCode::Enter));
    let mut state = state;
    assert_eq!(
        state.take_actions(),
        vec![Action::Report {
            from: "operator".to_string(),
            task_id: "t1".to_string(),
            outcome: Outcome::Done,
            head: "the plan is done".to_string(),
        }]
    );
}

#[test]
fn a_repair_verb_comes_from_the_fault_and_asks_only_for_what_it_needs() {
    let state = update(fixture::state(), press(KeyCode::Char('3')));
    assert_eq!(state.ui.fault, Some(4), "the open fault is selected");

    // `t` retries the task the fault names, and a retry carries no reason rule.
    let state = update(state, press(KeyCode::Char('t')));
    let Some(Prompt::Repair { verb, .. }) = state.ui.prompt.clone() else {
        panic!("`t` opens the repair form");
    };
    assert_eq!(
        verb,
        Repair::Retry {
            task_id: "t2".to_string(),
            reason: String::new(),
        }
    );
    let state = update(state, press(KeyCode::Enter));
    assert!(state.ui.prompt.is_none());
    let mut state = state;
    assert_eq!(
        state.take_actions(),
        vec![Action::Repair(Repair::Retry {
            task_id: "t2".to_string(),
            reason: String::new(),
        })]
    );

    // `a` needs a reason: empty, the form stays open and says what it wants,
    // and nothing leaves for the IO task.
    let state = update(state, press(KeyCode::Char('a')));
    let state = update(state, press(KeyCode::Enter));
    assert!(state.ui.prompt.is_some(), "the form is still open");
    assert_eq!(
        state.ui.notice.as_ref().map(|notice| notice.text.as_str()),
        Some("repair_ack: reason is required")
    );
    assert!(state.ui.actions.is_empty(), "no op left the screen");

    // Filled in, the same `enter` sends it.
    let state = fold(state, typed("looked at it"));
    let state = update(state, press(KeyCode::Enter));
    let mut state = state;
    assert_eq!(
        state.take_actions(),
        vec![Action::Repair(Repair::Ack {
            fault_id: 4,
            reason: "looked at it".to_string(),
        })]
    );
}

#[test]
fn control_r_reads_the_snapshot_again_and_the_quit_keys_leave() {
    let state = update(fixture::state(), chord('r', KeyModifiers::CONTROL));
    let mut state = state;
    assert_eq!(state.take_actions(), vec![Action::Refresh]);

    for leaving in [
        press(KeyCode::Char('q')),
        press(KeyCode::Esc),
        chord('c', KeyModifiers::CONTROL),
    ] {
        assert!(
            update(fixture::state(), leaving).ui.quit,
            "a key that leaves the board sets quit"
        );
    }
}

#[test]
fn an_op_answer_becomes_the_notice_the_footer_prints() {
    let state = update(
        fixture::state(),
        Event::Answered {
            op: "send",
            result: Ok("accepted".to_string()),
        },
    );
    let notice = state.ui.notice.clone().expect("an answered op is a notice");
    assert_eq!(notice.text, "send: accepted");
    assert_eq!(notice.level, Level::Info);

    let refused = update(
        fixture::state(),
        Event::Answered {
            op: "send",
            result: Err("invalid: to is not a role".to_string()),
        },
    );
    let notice = refused.ui.notice.expect("a refusal is a notice too");
    assert_eq!(notice.text, "send: invalid: to is not a role");
    assert_eq!(notice.level, Level::Error);
}

#[test]
fn the_selection_follows_the_stream_and_keeps_the_operators_place() {
    let state = fold(fixture::state(), [press(KeyCode::Down)]);
    assert_eq!(state.ui.role.as_deref(), Some("planner"));

    // A snapshot that moves nothing leaves the selection where the operator
    // put it: a board that moves under the operator is not an operator's board.
    let kept = update(
        state.clone(),
        Event::Snapshot(Box::new(fixture::snapshot())),
    );
    assert_eq!(kept.ui.role.as_deref(), Some("planner"));

    // A role that leaves the registry takes its selection with it, rather than
    // leaving the highlight on a row nothing answers for.
    let mut without = fixture::snapshot();
    without.roles.retain(|role| role.name != "planner");
    let moved = update(state, Event::Snapshot(Box::new(without)));
    assert_eq!(moved.ui.role.as_deref(), Some("builder"));
}

#[test]
fn the_tail_scrolls_back_and_stops_at_both_ends() {
    let state = fold(
        fixture::state(),
        [
            fixture::presence("builder", "online", 1),
            fixture::presence("planner", "offline", 2),
        ],
    );
    assert_eq!(state.view.event_tail.len(), 2, "the stream's two events");

    // `tab` twice reaches the tail pane, which is the one the scroll keys move.
    let state = fold(state, [press(KeyCode::Tab), press(KeyCode::Tab)]);
    assert_eq!(state.ui.pane, 2);

    let state = update(state, press(KeyCode::Char('[')));
    assert_eq!(state.ui.scroll, 1);
    let state = update(state, press(KeyCode::Char(']')));
    assert_eq!(state.ui.scroll, 0);
    let state = update(state, press(KeyCode::Char(']')));
    assert_eq!(state.ui.scroll, 0, "forward stops at the newest line");

    let state = update(state, press(KeyCode::PageUp));
    assert_eq!(state.ui.scroll, 1, "as far back as the tail goes");
}

#[test]
fn a_key_press_never_writes_the_view() {
    // The whole point of the split: the screen's keys are the screen's, and the
    // reducer's output only ever moves on a snapshot or a stream event.
    let before = fixture::state();
    let presses = [
        press(KeyCode::Char('s')),
        press(KeyCode::Char('f')),
        press(KeyCode::Char('r')),
        press(KeyCode::Char('2')),
        press(KeyCode::Char('3')),
        press(KeyCode::Tab),
        press(KeyCode::Down),
        press(KeyCode::Up),
        press(KeyCode::Enter),
        press(KeyCode::Esc),
        press(KeyCode::Char('[')),
        press(KeyCode::Char(']')),
        chord('r', KeyModifiers::CONTROL),
    ];
    for event in presses {
        let after = update(before.clone(), event.clone());
        assert_eq!(after.view, before.view, "{event:?} moved the view");
    }
}
