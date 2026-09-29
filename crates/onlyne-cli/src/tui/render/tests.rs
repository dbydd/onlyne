//! Tests for what the board draws.
//!
//! One case per acceptance line, and each of them reads a real buffer: a pinned
//! `View`, one frame drawn into an in-memory backend, and the words a reader
//! sees asserted in the order the line carries them. Nothing here has a server,
//! a socket, a timer, or a terminal — [`render_text`] takes a `&State` and
//! nothing else — which is also how the no-server case is pinned rather than
//! argued.

use super::*;
use crate::tui::fixture;
use crate::tui::state::{Page, State};
use crate::tui::update::{self, Event};
use crossterm::event as term;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// The size the page cases draw at: wide enough for the header's whole first
/// line, tall enough for every row the fixture holds.
const WIDTH: u16 = 160;
const HEIGHT: u16 = 40;

/// Draw one state and assert every fact appears on some line, as whole words in
/// order. The whole frame goes into the failure message: a case that fails says
/// what the screen said instead.
fn assert_shows(state: &State, facts: &[&str]) -> String {
    let text = render_text(state, WIDTH, HEIGHT);
    for fact in facts {
        assert!(
            shows(&text, fact),
            "the frame does not show {fact:?}\n{text}\n---"
        );
    }
    text
}

/// True when one line of `text` carries the words of `expected`, in order.
///
/// Words rather than columns: the layout's padding is not a fact about the
/// cluster, and a case that pinned it would break on every width the board
/// learns to draw at. The frame's own glyphs — the borders every pane is drawn
/// with — are separators, so a fact that sits against a border still reads as
/// the words it is.
fn shows(text: &str, expected: &str) -> bool {
    let expected: Vec<&str> = expected.split_whitespace().collect();
    if expected.is_empty() {
        return true;
    }
    text.lines().any(|line| {
        words(line)
            .windows(expected.len())
            .any(|window| window == expected.as_slice())
    })
}

/// One line's words, with the frame's own glyphs read as separators.
fn words(line: &str) -> Vec<&str> {
    line.split(|character: char| character.is_whitespace() || is_frame(character))
        .filter(|word| !word.is_empty())
        .collect()
}

/// True for the box-drawing characters a pane's border is drawn with.
fn is_frame(character: char) -> bool {
    matches!(
        character,
        '│' | '┌'
            | '┐'
            | '└'
            | '┘'
            | '─'
            | '├'
            | '┤'
            | '┬'
            | '┴'
            | '┼'
            | '║'
            | '╔'
            | '╗'
            | '╚'
            | '╝'
            | '═'
            | '╠'
            | '╣'
            | '╦'
            | '╩'
            | '╬'
    )
}

/// One key press, as the terminal reports it.
fn key(character: char) -> Event {
    press(KeyCode::Char(character))
}

/// One key press, from its own code.
fn press(code: KeyCode) -> Event {
    Event::Terminal(term::Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

#[test]
fn the_cluster_page_shows_presence_the_three_session_counts_and_queue_depth() {
    let state = fixture::state();
    assert_shows(
        &state,
        &[
            // What this board is watching, and what the snapshot carried.
            "onlyne tui · cluster page · onlyne-dev 1.4.1 · spec 9f2c · head #42",
            "roles 2 · sessions 3 · deliveries 4 · faults 1 open",
            // The registry: presence, the sessions it holds, and the three
            // counts a session can still be worked from, then the queue depth.
            "role presence sess busy idle susp queue",
            "▸ builder offline 1/1 0 1 0 2",
            "planner online 2/2 1 0 1 0",
        ],
    );
}

#[test]
fn the_cluster_page_shows_the_selected_roles_column() {
    let state = fixture::state();
    let text = assert_shows(
        &state,
        &[
            // The board is the selected role's own work, in the plan's five
            // columns, each read as the joint projection of the two axes.
            "board: builder · queued 1 · running 0 · waiting 1 · done 0 · failed_or_blocked 0",
            "column ledger hop msg kind route session",
            "▸ queued queued 3 m4 note planner→builder —",
            "waiting in_flight 1 m2 task planner→builder idle blocked",
            // The event tail is its own pane and says it has nothing yet.
            "events (tail 0)",
            "nothing has arrived on the stream yet",
        ],
    );
    // Every row of the board belongs to the selected role: planner's delivery
    // is not in builder's column, and a page that mixed them would be a second
    // answer to "whose work is this".
    assert!(
        !shows(&text, "m1"),
        "planner's delivery is not in builder's board\n{text}"
    );

    // Walk the registry onto the planner, and the board is that role's work:
    // the delivery it is working, and the one it has settled.
    let state = update::update(state, press(KeyCode::Down));
    assert_eq!(state.ui.role.as_deref(), Some("planner"));
    let text = assert_shows(
        &state,
        &[
            "board: planner · queued 0 · running 1 · waiting 0 · done 1 · failed_or_blocked 0",
            "▸ running in_flight 0 m1 task _supervisor→planner busy",
            "done acked 2 m3 completion builder→planner —",
        ],
    );
    assert!(
        !shows(&text, "m2"),
        "builder's deliveries are not in planner's board\n{text}"
    );
}

#[test]
fn a_stream_event_moves_the_page_with_no_poll() {
    let before = fixture::state();
    let text = render_text(&before, WIDTH, HEIGHT);
    assert!(shows(&text, "builder offline 1/1 0 1 0 2"), "{text}");

    // One event off the subscription, folded by the board's own `update`: no
    // timer, no second read, no poll.
    let after = update::update(before.clone(), fixture::presence("builder", "online", 1));
    let text = render_text(&after, WIDTH, HEIGHT);
    assert!(shows(&text, "builder online 1/1 0 1 0 2"), "{text}");
    assert!(
        shows(&text, "role_presence role builder online sessions=1"),
        "the tail carries what arrived\n{text}"
    );
    assert!(shows(&text, "events (tail 1)"), "{text}");
    assert!(
        after.ui.actions.is_empty(),
        "an event is not an op: nothing left the screen for the IO task"
    );
}

#[test]
fn the_task_page_shows_every_delivery_its_receipt_and_the_log() {
    let mut state = update::update(fixture::state(), key('2'));
    assert_eq!(state.ui.page, Page::Task);
    // A session event, so the log pane has the tail it exists for.
    state = update::update(
        state,
        fixture::stream(serde_json::json!({
            "type": "session_state",
            "data": {
                "task_id": "t1",
                "role": "planner",
                "session_id": "s-planner-1",
                "generation": 1,
                "seq": 8,
                "projection": {
                    "lifecycle": "working",
                    "agent": "running",
                    "delivery": "pending",
                    "resource": "attached",
                    "recovery": "none"
                }
            }
        })),
    );
    assert_shows(
        &state,
        &[
            "onlyne tui · task page",
            // The family, named by the deliveries that carry it.
            "families (1)",
            "▸ t1",
            "family t1 · origin _supervisor · 4 deliveries",
            // Every delivery of the family, by hop, with the verdict and the
            // receipt each one settled with.
            "hop msg route kind state verdict receipt",
            "▸ 0 m1 _supervisor→planner task in_flight — —",
            "1 m2 planner→builder task in_flight blocked —",
            "2 m3 builder→planner completion acked — 10:00:30 \"the build is done\" delivered",
            "3 m4 planner→builder note queued — —",
            // The session serving the selected delivery, and the tail of its
            // log, which is the stream's own account.
            "session log: s-planne · role planner · busy 1 idle 0 suspended 1",
            "session_state session s-planne planner lifecycle=working agent=running",
        ],
    );
}

#[test]
fn the_faults_page_shows_the_open_fault_and_the_verbs_it_offers() {
    let state = update::update(fixture::state(), key('3'));
    let text = assert_shows(
        &state,
        &[
            "onlyne tui · faults page",
            "open faults (1)",
            "id kind role task session created reason",
            // The fault itself: id, class, the role it happened in, the task it
            // names, and why it was recorded.
            "▸ 4 intent_exhausted builder t2 —",
            "retries exhausted",
            "fault #4 · intent_exhausted",
            "state open",
            "reason retries exhausted",
            "task t2",
            // The repair entry points, each with the key that opens it.
            "repair",
            "[a] repair_ack close the record as handled",
            "[t] repair_retry re-queue the task once",
            "[c] repair_close close the session and settle its task",
            "[F] repair_fail settle the task failed",
            "[i] repair_inspect read the session's reducer state",
        ],
    );
    // A fault no verb has moved is the page's row; one already handled is not.
    assert!(
        !shows(&text, "idle_fault"),
        "a handled fault is not an open fault\n{text}"
    );
}

#[test]
fn a_lost_stream_says_the_screen_is_catching_up() {
    let lagging = update::update(fixture::state(), fixture::resync_lag());
    assert!(
        lagging.view.stale,
        "the synthetic fault marks what the view holds incomplete"
    );
    let text = render_text(&lagging, WIDTH, HEIGHT);
    assert!(
        shows(
            &text,
            "catching up: the stream lost events; re-reading the snapshot"
        ),
        "{text}"
    );
    // The gap is not news about the cluster: it is in no pane, and the board
    // still draws everything it has.
    assert!(
        !shows(&text, "resync_lag"),
        "a gap is drawn as a state, not as an event\n{text}"
    );
    assert!(shows(&text, "▸ builder offline 1/1 0 1 0 2"), "{text}");

    // The snapshot the reconnect re-reads is what clears it.
    let healed = update::update(lagging, Event::Snapshot(Box::new(fixture::snapshot())));
    assert!(!healed.view.stale);
    assert!(!shows(&render_text(&healed, WIDTH, HEIGHT), "catching up"));
}

#[test]
fn a_page_inks_from_a_view_alone() {
    // A board that has read nothing draws rather than waiting for IO, and says
    // what it is waiting for.
    let empty = State::new("operator");
    let text = render_text(&empty, WIDTH, HEIGHT);
    assert!(
        shows(&text, "no roles: the snapshot has not answered yet"),
        "{text}"
    );
    assert!(shows(&text, "link: connecting"), "{text}");

    // The pinned view draws every page with no socket anywhere in the picture:
    // `render_text` is handed a `&State` and nothing else.
    for page in Page::ALL {
        let state = update::update(fixture::state(), key(page.number()));
        let text = render_text(&state, WIDTH, HEIGHT);
        assert!(
            shows(&text, page.title()),
            "the {page:?} page did not draw\n{text}"
        );
    }

    // Too small to hold three panes: the board says so instead of panicking on
    // arithmetic that went negative. The sentence wraps at 40 columns, so the
    // two halves are the two lines a reader gets.
    let text = render_text(&fixture::state(), 40, 8);
    assert!(
        shows(&text, "the terminal is") && shows(&text, "too small; widen it to 56x10"),
        "{text}"
    );
}

#[test]
fn every_page_names_the_ops_the_plan_gives_it() {
    // The operations are on the screen, not only in the key map: an operator
    // who has not read the docs finds them in the footer.
    for (page, ops) in [
        (Page::Cluster, "enter task · s send · f focus"),
        (Page::Task, "s send · f focus · r report"),
        (Page::Faults, "repair below · ^r refresh"),
    ] {
        let state = update::update(fixture::state(), key(page.number()));
        let text = render_text(&state, WIDTH, HEIGHT);
        assert!(shows(&text, ops), "the {page:?} footer:\n{text}");
    }
}

#[test]
fn the_open_form_names_the_field_the_caret_is_in() {
    // `s` on the cluster page opens the send form with the target already
    // named, over the body it will be drawn on top of.
    let state = update::update(fixture::state(), key('s'));
    assert_shows(
        &state,
        &[
            "send a task · to (esc cancels)",
            "from",
            "operator",
            "to",
            "builder",
            "body",
            "enter send · tab field · esc cancel",
        ],
    );
}

#[test]
fn an_ops_answer_is_printed_on_the_footer() {
    // What the IO task answered reaches the footer: an op the operator asked
    // for is not silent, whether it was accepted or refused.
    let accepted = update::update(
        fixture::state(),
        Event::Answered {
            op: "send",
            result: Ok("accepted as m5".to_string()),
        },
    );
    assert_shows(&accepted, &["send: accepted as m5"]);

    let refused = update::update(
        fixture::state(),
        Event::Answered {
            op: "repair_retry",
            result: Err("fault 4 is not open".to_string()),
        },
    );
    assert_shows(&refused, &["repair_retry: fault 4 is not open"]);
}
