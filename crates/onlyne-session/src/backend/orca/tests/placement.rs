use super::*;

#[test]
fn the_config_value_selects_the_policy() {
    assert_eq!(WorktreePolicy::from_config(""), WorktreePolicy::Host);
    assert_eq!(WorktreePolicy::from_config(" host "), WorktreePolicy::Host);
    // The spelling `auto` used to mean the default, so it still does.
    assert_eq!(WorktreePolicy::from_config("auto"), WorktreePolicy::Host);
    assert_eq!(
        WorktreePolicy::from_config("inherit"),
        WorktreePolicy::Inherit
    );
    assert_eq!(
        WorktreePolicy::from_config("id:folder:abc"),
        WorktreePolicy::Selector("id:folder:abc".into())
    );
}

#[test]
fn spawn_lands_in_the_host_worktree_as_a_flat_tab() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let cli = Arc::new(OrcaCli::default().reply("terminal create", 0, envelope(created_row())));
    let backend = OrcaBackend::with_host_worktree(
        cli.clone(),
        WorktreePolicy::Host,
        Some(HOST_WORKTREE.into()),
    );
    let spawned = backend.spawn(spawn_spec(&root)).unwrap();

    assert_eq!(spawned.backend_ref["selector"], HOST_WORKTREE);
    assert_eq!(spawned.backend_ref["handle"], "term_one");
    assert_eq!(spawned.backend_ref["pane_key"], "tab-1:leaf-2");
    assert_eq!(spawned.backend_ref["worktree_id"], "inst::/tmp/ws");
    assert_eq!(spawned.generation, 1);
    // One call, nothing else: no registration and no selector probe, even
    // though the role workspace is a directory Orca has never seen.
    let shell = spawn_command(&spawn_spec(&root)).unwrap();
    assert_eq!(
        cli.calls(),
        [format!(
            "orca terminal create --worktree {HOST_WORKTREE} --title onlyne:task-1 \
                 --command {shell} --json"
        )]
    );
    assert_eq!(mapping_lines(&root)[0]["worktree_selector"], HOST_WORKTREE);
}

#[test]
fn spawn_outside_an_orca_tab_lets_orca_pick_the_worktree() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let cli = Arc::new(OrcaCli::default().reply("terminal create", 0, envelope(created_row())));
    let backend = OrcaBackend::with_host_worktree(cli.clone(), WorktreePolicy::Host, None);
    let spawned = backend.spawn(spawn_spec(&root)).unwrap();

    assert!(!cli.calls()[0].contains("--worktree"));
    assert!(spawned.backend_ref.get("selector").is_none());
    assert_eq!(mapping_lines(&root)[0]["worktree_selector"], "");
}

#[test]
fn an_explicit_selector_overrides_the_host() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let cli = Arc::new(OrcaCli::default().reply("terminal create", 0, envelope(created_row())));
    let backend = OrcaBackend::with_host_worktree(
        cli.clone(),
        WorktreePolicy::Selector("id:folder:abc".into()),
        Some(HOST_WORKTREE.into()),
    );
    let spawned = backend.spawn(spawn_spec(&root)).unwrap();

    assert_eq!(spawned.backend_ref["selector"], "id:folder:abc");
    assert_eq!(
        cli.called("terminal create --worktree id:folder:abc"),
        1,
        "{:?}",
        cli.calls()
    );
}

#[test]
fn inherit_lets_orca_pick_the_worktree() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let cli = Arc::new(OrcaCli::default().reply("terminal create", 0, envelope(created_row())));
    let backend = OrcaBackend::with_host_worktree(
        cli.clone(),
        WorktreePolicy::Inherit,
        Some(HOST_WORKTREE.into()),
    );
    let spawned = backend.spawn(spawn_spec(&root)).unwrap();

    assert!(!cli.calls()[0].contains("--worktree"));
    assert!(spawned.backend_ref.get("selector").is_none());
    assert_eq!(mapping_lines(&root)[0]["worktree_selector"], "");
}

#[test]
fn a_handleless_create_closes_the_tab_it_made() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    // `create` made a tab but named no handle. The pane key it did carry is
    // the hook the rollback addresses it by, through `terminal list`.
    let cli = Arc::new(
        OrcaCli::default()
            .reply(
                "terminal create",
                0,
                envelope(serde_json::json!({
                    "terminal": {"tabId": "tab-1", "leafId": "leaf-2"}
                })),
            )
            .reply(
                "terminal list",
                0,
                envelope(serde_json::json!({"terminals": [relisted_row()]})),
            )
            .reply(
                "terminal close",
                0,
                envelope(serde_json::json!({"closed": true})),
            ),
    );
    let backend = OrcaBackend::with_host_worktree(
        cli.clone(),
        WorktreePolicy::Host,
        Some(HOST_WORKTREE.into()),
    );

    let error = backend.spawn(spawn_spec(&root)).unwrap_err();

    assert_eq!(cli.called("terminal close --terminal term_two"), 1);
    assert!(error.to_string().contains("term_two"), "{error}");
    assert!(error.to_string().contains("tab-1:leaf-2"), "{error}");
    // The spawn never handed a session out, so the tab map has no line and
    // the rollback is the only record of the tab.
    assert!(!root.join(".onlyne/cache/orca-tabs.jsonl").exists());
}

#[test]
fn a_handleless_create_falls_back_to_its_title_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    // No coordinates at all, so the create-time title is the only hook left.
    // Two rows carry it, and the newest one is the tab `create` just made.
    let row = |handle: &str, last: i64| {
        serde_json::json!({
            "handle": handle,
            "paneKey": format!("tab-{handle}:leaf-1"),
            "title": "onlyne:task-1",
            "lastOutputAt": last
        })
    };
    let cli = Arc::new(
        OrcaCli::default()
            .reply(
                "terminal create",
                0,
                envelope(serde_json::json!({"terminal": {}})),
            )
            .reply(
                "terminal list",
                0,
                envelope(serde_json::json!({
                    "terminals": [row("term_old", 5), row("term_new", 9)]
                })),
            )
            .reply(
                "terminal close",
                0,
                envelope(serde_json::json!({"closed": true})),
            ),
    );
    let backend = OrcaBackend::with_host_worktree(
        cli.clone(),
        WorktreePolicy::Host,
        Some(HOST_WORKTREE.into()),
    );

    let error = backend.spawn(spawn_spec(&root)).unwrap_err();

    assert_eq!(cli.called("terminal close --terminal term_new"), 1);
    assert_eq!(cli.called("terminal close --terminal term_old"), 0);
    assert!(error.to_string().contains("term_new"), "{error}");
}

#[test]
fn a_create_with_no_coordinates_is_reported_never_guessed() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    // The response named neither a handle nor a pane, and the listing holds
    // only the operator's own tab: nothing identifies the tab `create` made,
    // so the rollback closes nothing and says so. No close call is scripted
    // and the double panics on an unscripted call, so a rollback that
    // guessed would fail this test by touching a tab it does not own.
    let cli = Arc::new(
        OrcaCli::default()
            .reply(
                "terminal create",
                0,
                envelope(serde_json::json!({"terminal": {}})),
            )
            .reply(
                "terminal list",
                0,
                envelope(serde_json::json!({
                    "terminals": [{
                        "handle": "term_operator",
                        "paneKey": "tab-9:leaf-9",
                        "tabId": "tab-9",
                        "leafId": "leaf-9",
                        "title": "someone@workstation: ~/work",
                        "lastOutputAt": 11
                    }]
                })),
            ),
    );
    let backend = OrcaBackend::with_host_worktree(
        cli.clone(),
        WorktreePolicy::Host,
        Some(HOST_WORKTREE.into()),
    );

    let error = backend.spawn(spawn_spec(&root)).unwrap_err();

    assert_eq!(cli.called("terminal close"), 0);
    assert!(error.to_string().contains("close it by hand"), "{error}");
    // The coordinates that let an operator finish the job are in the error.
    assert!(error.to_string().contains("onlyne:task-1"), "{error}");
}
