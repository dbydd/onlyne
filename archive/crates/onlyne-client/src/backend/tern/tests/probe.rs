//! Probe: what makes a block alive, and what makes it gone.

use super::*;

#[test]
fn probe_reports_a_live_block_with_its_process() {
    let live = listing(vec![session(
        2147483648,
        Some("onlyne:lab"),
        vec![tab(2147483649, Some("planner"), &[2147483660], Some(0))],
    )]);
    let script = Script::default().reply("ls --json", live).reply(
        "process 2147483660",
        serde_json::json!({
            "pane": 2147483660u64,
            "child": {"pid": 94787, "name": "pi", "argv": ["pi"], "cwd": "/w"},
            "group": 94787,
        })
        .to_string(),
    );
    let (backend, _) = backend(script);
    let probe = backend.probe(&session_ref("2147483660")).unwrap();
    assert!(probe.alive, "{probe:?}");
    assert!(probe.attached, "{probe:?}");
    let detail = probe.detail.unwrap();
    assert_eq!(detail["pane"]["id"], "2147483660", "{detail}");
    assert_eq!(detail["tab_id"], "2147483649", "{detail}");
    assert_eq!(detail["session_id"], "2147483648", "{detail}");
    assert_eq!(detail["process"]["child"]["pid"], 94787, "{detail}");
}

#[test]
fn probe_reports_a_block_whose_program_exited_as_not_alive() {
    // `--keep-open` holds a block after its command returns, so the block is
    // still addressable while `exited` carries the status. The session that
    // owned it is over: alive follows `exited`, not the block's presence.
    let exited = listing(vec![session(
        2147483648,
        Some("onlyne:lab"),
        vec![serde_json::json!({
            "id": 2147483649u64,
            "name": "planner",
            "blocks": [serde_json::json!({
                "id": 2147483660u64,
                "title": "/w",
                "cwd": "/w",
                "program": "/bin/sh",
                "args": [],
                "command": "pi",
                "exited": 0,
                "keep_open": true,
                "focused": true,
                "live": true,
            })],
        })],
    )]);
    let script = Script::default()
        .reply("ls --json", exited)
        // A block whose program has returned holds no process to report, and
        // the probe still answers from the listing.
        .refused(
            "process 2147483660",
            "tern process: no block is called `2147483660`",
        );
    let (backend, script) = backend(script);
    let probe = backend.probe(&session_ref("2147483660")).unwrap();
    assert!(!probe.alive, "{probe:?}");
    // Still listed and still live, so the daemon holds the pty — addressable,
    // though nothing runs in it.
    assert!(probe.attached, "{probe:?}");
    // The listing already answered, so a process that cannot be asked about
    // does not make the probe fail.
    assert_eq!(script.call_count(), 2, "{:?}", script.calls());
}

#[test]
fn probe_reports_a_block_in_no_tab_as_gone() {
    // The listing holds no such block, which is how a closed one reads.
    let script = Script::default().reply(
        "ls --json",
        listing(vec![session(
            2147483648,
            Some("onlyne:lab"),
            vec![tab(2147483649, Some("planner"), &[700], None)],
        )]),
    );
    let (backend, _) = backend(script);
    let probe = backend.probe(&session_ref("2147483660")).unwrap();
    assert!(!probe.alive, "{probe:?}");
    assert!(!probe.attached, "{probe:?}");
    assert_eq!(probe.detail.unwrap()["error"], "block is in no tab",);
}

#[test]
fn attach_refuses_a_block_that_is_gone() {
    let script = Script::default().reply("ls --json", listing(vec![]));
    let (backend, _) = backend(script);
    let error = backend.attach(&session_ref("2147483660")).unwrap_err();
    assert!(error.to_string().contains("block is gone"), "{error}");
}

#[test]
fn available_is_the_listing_itself() {
    // The honest test: run the read every other call begins with. A window key
    // in the environment proves nothing about the binary answering.
    let script = Script::default().reply("ls --json", listing(vec![]));
    let (host, script) = backend(script);
    assert!(host.available().unwrap());
    assert_eq!(script.call_count(), 1);

    let refused = Script::default().refused("ls --json", "tern ls: no session daemon");
    let (host, _) = backend(refused);
    assert!(!host.available().unwrap());
}

#[test]
fn capabilities_answer_rename_unsupported_because_tern_renames_the_tab() {
    // `tern rename <BLOCK_ID> NAME` renames the block's tab, and
    // `tern rename <TAB_ID> NAME` is refused with `no block is called`. One
    // session's title would land on every pane in the role's tab.
    let (host, _) = backend(Script::default());
    let capabilities = host.capabilities();
    assert!(capabilities.spawn, "split launches a command");
    assert!(capabilities.attach, "the listing names a live block");
    assert!(capabilities.probe, "the listing names a block's state");
    assert!(capabilities.close, "tern close takes a block id");
    assert!(capabilities.focus, "tern focus takes a block id");
    assert!(
        !capabilities.rename,
        "renaming a block renames its tab: the capability must say so"
    );
    let error = host
        .rename(&session_ref("2147483660"), "onlyne-planner")
        .unwrap_err();
    assert!(
        error.to_string().contains("renames a block's tab"),
        "{error}"
    );
}
