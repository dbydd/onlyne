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
fn spawn_creates_the_cluster_session_the_role_tab_and_the_block() {
    // Nothing exists: the session, the tab and the block are all created, and
    // the ref names the three ids the answers gave.
    let script = Script::default()
        .reply("ls --json", listing(vec![]))
        .reply("new session", created(2147483648, 2147483649, 2147483650))
        .reply("ls --json", listing(vec![]))
        .reply("new tab", created(2147483648, 2147483651, 2147483652))
        .reply("rename 2147483652 planner", "{}")
        .reply("split", created(2147483648, 2147483651, 2147483660));
    let (backend, script) = backend(script);
    let session = backend.spawn(spec()).unwrap();
    let tern = &session.backend_ref["tern"];
    assert_eq!(tern["session_id"], "2147483648");
    assert_eq!(tern["tab_id"], "2147483651");
    assert_eq!(tern["pane_id"], "2147483660");
    assert_eq!(tern["session_label"], "onlyne:lab");
    assert_eq!(tern["base_pane"], "2147483652");
    assert_eq!(tern["split_direction"], "right");
    let calls = script.calls();
    // Six calls: the listing that finds no session, the create, the listing
    // that finds no tab in the session it just made, the create, the tab
    // rename, and the split.
    // The tab answer's own block is the one the split goes beside.
    assert!(calls[0].ends_with("ls --json"), "{calls:?}");
    assert!(
        calls[1].contains("new session onlyne:lab --cwd /w --keep-open --json"),
        "{:?}",
        calls[1]
    );
    assert!(calls[2].ends_with("ls --json"), "{calls:?}");
    assert!(
        calls[3].contains("new tab 2147483648 --cwd /w --keep-open --json"),
        "{:?}",
        calls[3]
    );
    // A count of 0 blocks plans a right split at 0.5, and tern takes the
    // direction with no ratio.
    assert!(
        calls[5].contains("split 2147483652 right --cwd /w --keep-open --json"),
        "{:?}",
        calls[5]
    );
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
    assert_eq!(tern["base_pane"], "701");
    let calls = script.calls();
    assert_eq!(calls.len(), 3, "{calls:?}");
    assert!(
        !calls.iter().any(|call| call.contains("new ")),
        "a found session and tab must not be re-created: {calls:?}"
    );
    // Two blocks, so the pane count plans a down split at 0.5 — a
    // power-of-two count goes right, every other count goes down.
    assert!(calls[2].contains("split 701 down"), "{:?}", calls[2]);
}

#[test]
fn spawn_creates_the_tab_inside_a_session_it_found() {
    // The session is found by name and the role tab is not there: one create
    // call, naming the session it belongs to.
    let existing = listing(vec![session(
        2147483648,
        Some("onlyne:lab"),
        vec![tab(2147483649, Some("other"), &[700], None)],
    )]);
    let script = Script::default()
        .reply("ls --json", existing.clone())
        .reply("ls --json", existing)
        .reply("new tab", created(2147483648, 2147483651, 2147483652))
        .reply("rename 2147483652 planner", "{}")
        .reply("split", created(2147483648, 2147483651, 2147483660));
    let (backend, script) = backend(script);
    backend.spawn(spec()).unwrap();
    let calls = script.calls();
    assert!(
        calls.iter().all(|call| !call.contains("new session")),
        "{calls:?}"
    );
    assert!(calls[2].contains("new tab 2147483648"), "{:?}", calls[2]);
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
fn spawn_refuses_an_empty_command() {
    let (backend, _) = backend(Script::default());
    let mut spec = spec();
    spec.command.clear();
    let error = backend.spawn(spec).unwrap_err();
    assert!(error.to_string().contains("requires a command"), "{error}");
}
