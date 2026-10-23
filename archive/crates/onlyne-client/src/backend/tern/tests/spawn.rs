//! Spawn: the topology one session builds, and the argv it builds for it.

use super::*;

/// The one call `split_and_start` makes, joined for the record.
fn split_call(backend: &TernBackend, script: &Arc<Script>, spec: &SpawnSpec) -> String {
    let placement = PanePlacement::from_pane_count(0);
    let pane = backend
        .split_and_start("2147483650", spec, placement, "2147483648", "2147483649")
        .unwrap();
    let calls = script.calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(pane, "2147483660", "split returned the block it made");
    calls.into_iter().next().unwrap()
}

#[test]
fn spawn_launches_the_agent_as_a_fresh_sessions_first_block() {
    // Nothing exists: the session is created by launching the agent into it —
    // `new session … -- <launch>` — and the agent's block renames its tab to
    // the role. No anchor shell is made, so there is no split call at all,
    // and the ref's base is the agent's own block.
    let script = Script::default()
        .reply("ls --json", listing(vec![]))
        .reply("new session", created(2147483648, 2147483651, 2147483652))
        .reply("rename 2147483652 planner", "{}");
    let (backend, script) = backend(script);
    let session = backend.spawn(spec()).unwrap();
    let tern = &session.backend_ref["tern"];
    assert_eq!(tern["session_id"], "2147483648");
    assert_eq!(tern["tab_id"], "2147483651");
    assert_eq!(tern["pane_id"], "2147483652");
    assert_eq!(tern["session_label"], "onlyne:lab");
    // The agent is its own base: a first block has nothing else to sit
    // beside, and the direction keeps the word a first split would take.
    assert_eq!(tern["base_pane"], "2147483652");
    assert_eq!(tern["split_direction"], "right");
    let calls = script.calls();
    // Three calls: the listing that finds no session, the launch, the rename
    // of the tab the launch made. No listing re-read and no split: the launch
    // answer names the one block it made.
    assert_eq!(calls.len(), 3, "{calls:?}");
    assert!(calls[0].ends_with("ls --json"), "{calls:?}");
    assert!(
        calls[1].contains(
            "new session onlyne:lab --cwd /w --keep-open --json -- env ONLYNE_CLUSTER=lab \
             ONLYNE_ROLE=planner pi --session-id a b",
        ),
        "{:?}",
        calls[1]
    );
    assert!(calls[2].contains("rename 2147483652 planner"), "{calls:?}");
}

#[test]
fn spawn_creates_the_tab_inside_a_session_it_found() {
    // The session is found by name and the role tab is not there: the agent
    // launches into a new tab of the found session, and its block renames
    // the tab. No `new session` and no split — the agent is the tab's first
    // and only block.
    let existing = listing(vec![session(
        2147483648,
        Some("onlyne:lab"),
        vec![tab(2147483649, Some("other"), &[700], None)],
    )]);
    let script = Script::default()
        .reply("ls --json", existing.clone())
        .reply("ls --json", existing)
        .reply("new tab", created(2147483648, 2147483651, 2147483652))
        .reply("rename 2147483652 planner", "{}");
    let (backend, script) = backend(script);
    let session = backend.spawn(spec()).unwrap();
    let tern = &session.backend_ref["tern"];
    assert_eq!(tern["session_id"], "2147483648");
    assert_eq!(tern["tab_id"], "2147483651");
    assert_eq!(tern["pane_id"], "2147483652");
    assert_eq!(tern["base_pane"], "2147483652");
    let calls = script.calls();
    // Three calls: the listing, the launch into a tab of the found session,
    // the rename of the tab the launch made.
    assert_eq!(calls.len(), 3, "{calls:?}");
    assert!(
        calls.iter().all(|call| !call.contains("new session")),
        "{calls:?}"
    );
    assert!(
        calls[1].contains("new tab 2147483648 --cwd /w --keep-open --json -- env"),
        "{:?}",
        calls[1]
    );
    assert!(calls[2].contains("rename 2147483652 planner"), "{calls:?}");
}

#[test]
fn spawn_reuses_the_session_and_the_role_tab_when_they_exist() {
    // Both found: no create call at all, and the split goes beside the tab's
    // *focused* block rather than its first — a role's panes then accumulate
    // beside the one the operator last looked at.
    let existing = listing(vec![session(
        2147483648,
        Some("onlyne:lab"),
        vec![tab(2147483649, Some("planner"), &[700, 701], Some(1))],
    )]);
    let script = Script::default()
        .reply("ls --json", existing.clone())
        .reply("ls --json", existing)
        .reply("split", created(2147483648, 2147483649, 2147483660));
    let (backend, script) = backend(script);
    let session = backend.spawn(spec()).unwrap();
    let tern = &session.backend_ref["tern"];
    assert_eq!(tern["session_id"], "2147483648");
    assert_eq!(tern["tab_id"], "2147483649");
    assert_eq!(tern["pane_id"], "2147483660");
    assert_eq!(tern["base_pane"], "701");
    let calls = script.calls();
    // Two calls: one listing resolves both the session and its role tab, and
    // the split is the only write.
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(
        !calls.iter().any(|call| call.contains("new ")),
        "a found session and tab must not be re-created: {calls:?}"
    );
    // Two blocks, so the pane count plans a down split at 0.5 — a
    // power-of-two count goes right, every other count goes down.
    assert!(calls[1].contains("split 701 down"), "{:?}", calls[1]);
}

#[test]
fn a_second_spawn_of_a_role_adds_one_pane_to_the_role_tab() {
    // One agent already holds the role tab: the next spawn of the same role
    // adds exactly one block beside it, launching nothing new.
    let existing = listing(vec![session(
        2147483648,
        Some("onlyne:lab"),
        vec![tab(2147483649, Some("planner"), &[700], Some(0))],
    )]);
    let script = Script::default()
        .reply("ls --json", existing.clone())
        .reply("ls --json", existing)
        .reply("split", created(2147483648, 2147483649, 2147483660));
    let (backend, script) = backend(script);
    let session = backend.spawn(spec()).unwrap();
    let tern = &session.backend_ref["tern"];
    assert_eq!(tern["pane_id"], "2147483660");
    assert_eq!(tern["base_pane"], "700");
    let calls = script.calls();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(
        !calls.iter().any(|call| call.contains("new ")),
        "an existing role tab takes only a split: {calls:?}"
    );
    // One existing block means the split brings the count to two — a
    // power of two, so right.
    assert!(calls[1].contains("split 700 right"), "{:?}", calls[1]);
}

#[test]
fn a_failed_rename_closes_the_block_the_spawn_created() {
    // The agent's block is the only thing a fresh spawn creates. When the
    // rename that names its tab is refused, the spawn fails and closes its
    // own block rather than leave a live pane nothing addresses — reporting
    // the rename's error, never the cleanup's.
    let script = Script::default()
        .reply("ls --json", listing(vec![]))
        .reply("new session", created(2147483648, 2147483651, 2147483652))
        .refused(
            "rename 2147483652",
            "tern rename: no block is called `2147483652`",
        )
        .reply("close 2147483652", "{}");
    let (backend, script) = backend(script);
    let error = backend.spawn(spec()).unwrap_err();
    assert!(error.to_string().contains("no block is called"), "{error}");
    let calls = script.calls();
    assert!(
        calls.iter().any(|call| call.contains("close 2147483652")),
        "the spawn's own block is closed on failure: {calls:?}"
    );
    assert!(
        calls.iter().all(|call| !call.contains("kill session")),
        "foreign sessions and tabs are never deleted: {calls:?}"
    );
}

#[test]
fn split_sends_the_command_as_argv_inside_the_pane_shell() {
    // The command travels after `--` as argv, prefixed with `env NAME=VALUE`
    // for the session's own environment — not as a shell line, so a value
    // carrying a space needs no quoting, and nothing else from the client
    // travels with it.
    let script = Script::default().reply("split", created(2147483648, 2147483649, 2147483660));
    let (backend, script) = backend(script);
    let call = split_call(&backend, &script, &spec());
    let tail = call.split(" -- ").nth(1).expect("a -- separator");
    // A value carrying a space stays one argv token — `env` is a real
    // executable, so no shell ever re-splits it and nothing quotes it.
    assert_eq!(
        tail, "env ONLYNE_CLUSTER=lab ONLYNE_ROLE=planner pi --session-id a b",
        "{call}"
    );
}

#[test]
fn split_sends_no_ratio_because_tern_takes_none() {
    let script = Script::default().reply("split", created(2147483648, 2147483649, 2147483660));
    let (backend, script) = backend(script);
    let call = split_call(&backend, &script, &spec());
    assert!(
        !call.contains("--ratio"),
        "tern split takes no ratio: {call}"
    );
}

#[test]
fn split_refuses_a_block_in_another_session() {
    // A `--window` that addresses another window answers with that window's
    // ids. The block would be in a session this client never named and no
    // later call could address, so it is caught here and names the cause.
    let script = Script::default().reply("split", created(99999, 2147483649, 2147483660));
    let (backend, _) = backend(script);
    let error = backend
        .split_and_start(
            "2147483650",
            &spec(),
            PanePlacement::from_pane_count(0),
            "2147483648",
            "2147483649",
        )
        .unwrap_err();
    assert!(
        error.to_string().contains("session 99999, not 2147483648"),
        "{error}"
    );
}

#[test]
fn a_tab_launch_refuses_an_answer_from_another_session() {
    // The same window check the split makes: a `new tab` answered in another
    // window's session would strand the block where no ref could reach it.
    let existing = listing(vec![session(
        2147483648,
        Some("onlyne:lab"),
        vec![tab(2147483649, Some("other"), &[700], None)],
    )]);
    let script = Script::default()
        .reply("ls --json", existing.clone())
        .reply("ls --json", existing)
        .reply("new tab", created(99999, 2147483651, 2147483652));
    let (backend, _) = backend(script);
    let error = backend.spawn(spec()).unwrap_err();
    assert!(
        error.to_string().contains("session 99999, not 2147483648"),
        "{error}"
    );
}

#[test]
fn spawn_refuses_an_empty_command() {
    let (backend, _) = backend(Script::default());
    let mut spec = spec();
    spec.command.clear();
    let error = backend.spawn(spec).unwrap_err();
    assert!(error.to_string().contains("requires a command"), "{error}");
}
