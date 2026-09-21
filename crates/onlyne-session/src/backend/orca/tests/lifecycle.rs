use super::*;

#[test]
fn probe_reads_liveness_from_status_and_exit_cause() {
    let cases = [
        (
            "a tab with no status and a live pty",
            serde_json::json!({"connected": true, "writable": true, "lastOutputAt": 7}),
            true,
        ),
        (
            "a tab the operator closed",
            serde_json::json!({
                "connected": true,
                "writable": true,
                "exitCause": {"kind": "operator_close"}
            }),
            false,
        ),
        (
            "an exited status",
            serde_json::json!({"status": "exited", "connected": false, "writable": false}),
            false,
        ),
        (
            "a pty that is neither connected nor writable",
            serde_json::json!({"connected": false, "writable": false}),
            false,
        ),
    ];
    for (name, row, alive) in cases {
        let cli = Arc::new(OrcaCli::default().reply(
            "terminal show",
            0,
            envelope(serde_json::json!({"terminal": row})),
        ));
        let backend = OrcaBackend::with_policy(cli, WorktreePolicy::Inherit);
        let probe = backend
            .probe(&session(
                serde_json::json!({"handle": "term_live", "pane_key": "tab-1:leaf-2"}),
            ))
            .unwrap();
        assert_eq!(probe.alive, alive, "{name}");
    }
}

#[test]
fn availability_follows_the_listing_answer() {
    let live = Arc::new(OrcaCli::default().reply(
        "terminal list",
        0,
        envelope(serde_json::json!({"terminals": []})),
    ));
    assert!(
        OrcaBackend::with_policy(live, WorktreePolicy::Inherit)
            .available()
            .unwrap()
    );

    // A CLI that answers on stdout with a refusal is not usable, even
    // though the exit code alone used to read as ready.
    let refusing =
        Arc::new(OrcaCli::default().reply("terminal list", 1, refusal("runtime_unavailable")));
    assert!(
        !OrcaBackend::with_policy(refusing, WorktreePolicy::Inherit)
            .available()
            .unwrap()
    );
}

#[test]
fn probe_detail_carries_the_fields_the_reaper_reads() {
    let cli = Arc::new(OrcaCli::default().reply(
        "terminal show",
        0,
        envelope(serde_json::json!({"terminal": {
            "status": "running",
            "connected": true,
            "writable": true,
            "lastOutputAt": 7
        }})),
    ));
    let backend = OrcaBackend::with_policy(cli, WorktreePolicy::Inherit);
    let probe = backend
        .probe(&session(serde_json::json!({"handle": "term_live"})))
        .unwrap();
    assert!(probe.alive);
    let detail = probe.detail.unwrap();
    assert_eq!(detail["handle"], "term_live");
    assert_eq!(detail["status"], "running");
    assert!(detail["exit_cause"].is_null());
    assert_eq!(detail["last_output_at"], 7);
}

#[test]
fn a_stale_handle_is_reminted_through_the_listing() {
    let cli = Arc::new(
        OrcaCli::default()
            .reply(
                "terminal show --terminal term_old",
                1,
                refusal("terminal_handle_stale"),
            )
            .reply(
                "terminal list",
                0,
                envelope(serde_json::json!({"terminals": [relisted_row()]})),
            )
            .reply(
                "terminal show --terminal term_two",
                0,
                envelope(serde_json::json!({"terminal": {
                    "handle": "term_two",
                    "connected": true,
                    "writable": true,
                    "lastOutputAt": 9
                }})),
            ),
    );
    let backend = OrcaBackend::with_policy(cli.clone(), WorktreePolicy::Inherit);
    let stale = session(serde_json::json!({
        "handle": "term_old",
        "pane_key": "tab-1:leaf-2",
        "pty_id": "inst::/tmp/ws@@ab",
        "selector": "path:/tmp"
    }));
    let probe = backend.probe(&stale).unwrap();

    assert!(probe.alive);
    assert_eq!(probe.detail.unwrap()["handle"], "term_two");
    assert_eq!(
        cli.calls(),
        vec![
            "orca terminal show --terminal term_old --json".to_string(),
            "orca terminal list --worktree path:/tmp --json".to_string(),
            "orca terminal show --terminal term_two --json".to_string(),
        ]
    );
}

#[test]
fn attach_rewrites_a_stale_ref_and_leaves_a_live_one_alone() {
    let cli = Arc::new(
        OrcaCli::default()
            .reply(
                "terminal show --terminal term_old",
                1,
                refusal("terminal_handle_stale"),
            )
            .reply(
                "terminal list",
                0,
                envelope(serde_json::json!({"terminals": [relisted_row()]})),
            )
            .reply(
                "terminal show --terminal term_two",
                0,
                envelope(serde_json::json!({"terminal": {"handle": "term_two"}})),
            )
            .reply(
                "terminal show --terminal term_live",
                0,
                envelope(serde_json::json!({"terminal": {"handle": "term_live"}})),
            ),
    );
    let backend = OrcaBackend::with_policy(cli.clone(), WorktreePolicy::Inherit);
    let stale = session(serde_json::json!({
        "handle": "term_old",
        "pane_key": "tab-1:leaf-2",
        "pty_id": "inst::/tmp/ws@@ab",
        "selector": "path:/tmp"
    }));
    let refreshed = backend.attach(&stale).unwrap();

    assert_eq!(refreshed.backend_ref["handle"], "term_two");
    assert_eq!(refreshed.backend_ref["pty_id"], "inst2::/tmp/ws@@cd");
    assert_eq!(refreshed.backend_ref["selector"], "path:/tmp");
    assert_eq!(refreshed.task_id, "task-1");
    assert_eq!(refreshed.generation, 1);

    let live = session(serde_json::json!({"handle": "term_live", "pane_key": "tab-1:leaf-2"}));
    assert_eq!(backend.attach(&live).unwrap(), live);
    assert_eq!(cli.calls().len(), 4);
}

#[test]
fn a_pane_missing_from_every_listing_is_dead() {
    let cli = Arc::new(
        OrcaCli::default()
            .reply("terminal show", 1, refusal("terminal_handle_stale"))
            .reply(
                "terminal list",
                0,
                envelope(serde_json::json!({"terminals": []})),
            ),
    );
    let backend = OrcaBackend::with_policy(cli.clone(), WorktreePolicy::Inherit);
    let probe = backend
        .probe(&session(serde_json::json!({
            "handle": "term_old",
            "pane_key": "tab-1:leaf-2"
        })))
        .unwrap();

    assert!(!probe.alive);
    assert!(!probe.attached);
    assert!(
        probe.detail.unwrap()["error"]
            .as_str()
            .unwrap()
            .contains("terminal_not_found")
    );
    // The unfiltered listing is the fallback when no selector is known.
    assert_eq!(cli.calls()[1], "orca terminal list --json".to_string(),);
}

#[test]
fn a_stale_ref_without_a_pane_key_cannot_be_probed() {
    let cli =
        Arc::new(OrcaCli::default().reply("terminal show", 1, refusal("terminal_handle_stale")));
    let backend = OrcaBackend::with_policy(cli, WorktreePolicy::Inherit);
    let error = backend
        .probe(&session(serde_json::json!({"handle": "term_old"})))
        .unwrap_err();
    assert!(error.to_string().contains("pane_key"));
}

#[test]
fn close_remints_a_stale_handle_and_records_the_tombstone() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let selector = HOST_WORKTREE;
    let cli = Arc::new(
        OrcaCli::default()
            .reply("terminal create", 0, envelope(created_row()))
            .reply(
                "terminal show --terminal term_one",
                1,
                refusal("terminal_handle_stale"),
            )
            .reply(
                "terminal list",
                0,
                envelope(serde_json::json!({"terminals": [relisted_row()]})),
            )
            .reply(
                "terminal show --terminal term_two",
                0,
                envelope(serde_json::json!({"terminal": {"handle": "term_two"}})),
            )
            .reply(
                "terminal close --terminal term_two",
                0,
                envelope(serde_json::json!({"closed": true})),
            ),
    );
    let backend = OrcaBackend::with_host_worktree(
        cli.clone(),
        WorktreePolicy::Host,
        Some(HOST_WORKTREE.into()),
    );
    let spawned = backend.spawn(spawn_spec(&root)).unwrap();
    backend
        .close(&spawned, CloseReason::Completed, false)
        .unwrap();

    assert_eq!(cli.called("terminal close --terminal term_two"), 1);
    assert_eq!(cli.called("terminal close --terminal term_one"), 0);
    // The remint records the new handle, then the close records the end.
    let lines = mapping_lines(&root);
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert_eq!(lines[0]["handle"], "term_one");
    assert_eq!(lines[0]["state"], "spawned");
    assert_eq!(lines[1]["handle"], "term_two");
    assert_eq!(lines[1]["state"], "spawned");
    let tombstone = lines.last().unwrap();
    assert_eq!(tombstone["state"], "closed");
    assert_eq!(tombstone["handle"], "term_two");
    assert_eq!(tombstone["pane_key"], "tab-1:leaf-2");
    assert_eq!(tombstone["worktree_selector"], selector);
    assert_eq!(tombstone["role"], "planner");
    assert_eq!(tombstone["session_id"], "session-1");
    assert_eq!(tombstone["title"], "onlyne:task-1");
}

#[test]
fn closing_a_pane_that_is_already_gone_is_a_no_op() {
    let cli = Arc::new(
        OrcaCli::default()
            .reply("terminal show", 1, refusal("terminal_handle_stale"))
            .reply(
                "terminal list",
                0,
                envelope(serde_json::json!({"terminals": []})),
            ),
    );
    let backend = OrcaBackend::with_policy(cli.clone(), WorktreePolicy::Inherit);
    backend
        .close(
            &session(serde_json::json!({"handle": "term_old", "pane_key": "tab-1:leaf-2"})),
            CloseReason::Operator,
            true,
        )
        .unwrap();

    assert_eq!(cli.called("terminal close"), 0);
}
