use super::*;

#[cfg(unix)]
#[test]
fn spawn_writes_the_tab_map_under_the_canonical_workspace() {
    // The map is a workspace file and the workspace may be reached through
    // a symlink (`/tmp` on macOS), so the line has to land under the
    // canonical root the supervisor script reads.
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let link = std::env::temp_dir().join(format!("onlyne-orca-link-{}", std::process::id()));
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(&root, &link).unwrap();
    let cli = Arc::new(OrcaCli::default().reply("terminal create", 0, envelope(created_row())));
    let backend = OrcaBackend::with_host_worktree(
        cli.clone(),
        WorktreePolicy::Host,
        Some(HOST_WORKTREE.into()),
    );
    backend.spawn(spawn_spec(&link)).unwrap();
    std::fs::remove_file(&link).unwrap();

    assert!(root.join(".onlyne/cache/orca-tabs.jsonl").exists());
}

#[test]
fn spawn_records_the_plugin_mapping_line() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let cli = Arc::new(OrcaCli::default().reply("terminal create", 0, envelope(created_row())));
    let backend =
        OrcaBackend::with_host_worktree(cli, WorktreePolicy::Host, Some(HOST_WORKTREE.into()));
    backend.spawn(spawn_spec(&root)).unwrap();

    let text = std::fs::read_to_string(root.join(".onlyne/cache/orca-tabs.jsonl")).unwrap();
    let lines = text.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 1, "{text}");
    // The field order is the contract the supervisor script folds on.
    assert!(lines[0].starts_with(r#"{"pane_key":"tab-1:leaf-2","handle":"term_one""#));
    let line = &mapping_lines(&root)[0];
    assert_eq!(line.as_object().unwrap().len(), 9);
    assert_eq!(line["task_id"], "task-1");
    assert_eq!(line["session_id"], "session-1");
    assert_eq!(line["role"], "planner");
    assert_eq!(line["worktree_selector"], HOST_WORKTREE);
    assert_eq!(line["title"], "onlyne:task-1");
    assert_eq!(line["state"], "spawned");
    assert!(line["updated_at"].as_str().unwrap().ends_with('Z'));
}

#[test]
fn an_unwritable_mapping_cache_does_not_fail_the_spawn() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    // A file where the `.onlyne` directory belongs makes the append fail.
    std::fs::write(root.join(".onlyne"), b"not a directory").unwrap();
    let cli = Arc::new(OrcaCli::default().reply("terminal create", 0, envelope(created_row())));
    let backend =
        OrcaBackend::with_host_worktree(cli, WorktreePolicy::Host, Some(HOST_WORKTREE.into()));

    let spawned = backend.spawn(spawn_spec(&root)).unwrap();
    assert_eq!(spawned.backend_ref["handle"], "term_one");
}
