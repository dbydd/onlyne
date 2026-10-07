//! Focus: one call, where herdr walked a workspace, a tab and a pane.

use super::*;

#[test]
fn focus_skips_the_call_when_the_block_already_holds_it() {
    // The ref's block, tab and session must all match: tern reports `focused`
    // per block per tab, and a window with several sessions shows a focused
    // block in each. Matching on the block id alone would call a different
    // session's focused block this one.
    let here = listing(vec![session(
        2147483648,
        Some("onlyne:lab"),
        vec![tab(2147483649, Some("planner"), &[2147483660], Some(0))],
    )]);
    let script = Script::default().reply("ls --json", here);
    let (backend, script) = backend(script);
    backend.focus(&session_ref("2147483660")).unwrap();
    assert_eq!(
        script.calls(),
        vec!["/Applications/Tern.app/Contents/MacOS/tern ls --json".to_string()]
    );
}

#[test]
fn focus_moves_to_the_block_and_confirms_it_took_focus() {
    // `tern focus BLOCK` is one call — every window shows it — so there is no
    // tab hop to skip. The listing is read again afterwards: a focus that
    // lands on another block sends the operator's keyboard to another
    // session, and that is reported rather than passed off as success.
    let elsewhere = listing(vec![session(
        2147483648,
        Some("onlyne:lab"),
        vec![tab(
            2147483649,
            Some("planner"),
            &[700, 2147483660],
            Some(0),
        )],
    )]);
    let landed = listing(vec![session(
        2147483648,
        Some("onlyne:lab"),
        vec![tab(
            2147483649,
            Some("planner"),
            &[700, 2147483660],
            Some(1),
        )],
    )]);
    let script = Script::default()
        .reply("ls --json", elsewhere)
        .reply(
            "focus 2147483660",
            serde_json::json!({"block": 2147483660u64}).to_string(),
        )
        .reply("ls --json", landed);
    let (backend, script) = backend(script);
    backend.focus(&session_ref("2147483660")).unwrap();
    let calls = script.calls();
    assert_eq!(
        calls[1],
        "/Applications/Tern.app/Contents/MacOS/tern focus 2147483660 --json"
    );
    assert_eq!(calls.len(), 3, "confirm by re-reading: {calls:?}");
}

#[test]
fn focus_reports_a_focus_that_landed_on_another_block() {
    let elsewhere = listing(vec![session(
        2147483648,
        Some("onlyne:lab"),
        vec![tab(
            2147483649,
            Some("planner"),
            &[700, 2147483660],
            Some(0),
        )],
    )]);
    let script = Script::default()
        .reply("ls --json", elsewhere.clone())
        .reply(
            "focus 2147483660",
            serde_json::json!({"block": 2147483660u64}).to_string(),
        )
        .reply("ls --json", elsewhere);
    let (backend, _) = backend(script);
    let error = backend.focus(&session_ref("2147483660")).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("left block 2147483660 unfocused (focused block 700)"),
        "{error}"
    );
}

#[test]
fn focus_reports_a_window_with_no_focused_block() {
    let elsewhere = listing(vec![session(
        2147483648,
        Some("onlyne:lab"),
        vec![tab(
            2147483649,
            Some("planner"),
            &[700, 2147483660],
            Some(0),
        )],
    )]);
    let empty = listing(vec![]);
    let script = Script::default()
        .reply("ls --json", elsewhere)
        .reply(
            "focus 2147483660",
            serde_json::json!({"block": 2147483660u64}).to_string(),
        )
        .reply("ls --json", empty);
    let (backend, _) = backend(script);
    let error = backend.focus(&session_ref("2147483660")).unwrap_err();
    assert!(
        error.to_string().contains("no block holds focus"),
        "{error}"
    );
}
