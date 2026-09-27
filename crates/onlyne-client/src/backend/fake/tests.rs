use super::*;
#[test]
fn reducer_like_spawn_probe_close() {
    let backend = FakeBackend::new();
    let spec = SpawnSpec {
        cwd: ".".into(),
        task_id: "task".into(),
        command: vec!["pi".into()],
        env: BTreeMap::new(),
        focus: None,
        placement: None,
        rename: None,
    };
    let session = backend.spawn(spec).unwrap();
    assert!(backend.probe(&session).unwrap().alive);
    backend
        .close(&session, CloseReason::Completed, false)
        .unwrap();
    assert!(!backend.probe(&session).unwrap().alive);
}

#[test]
fn forced_probe_failure_reports_dead_without_closing() {
    let backend = FakeBackend::new();
    let session = backend
        .spawn(SpawnSpec {
            cwd: ".".into(),
            task_id: "ghost-task".into(),
            command: vec!["agent".into()],
            env: BTreeMap::new(),
            focus: None,
            placement: None,
            rename: None,
        })
        .unwrap();
    assert!(backend.probe(&session).unwrap().alive);
    backend.fail_probe("ghost-task");
    let probe = backend.probe(&session).unwrap();
    assert!(!probe.alive);
    assert!(!probe.attached);
    assert!(probe.detail.is_some());
    backend.clear_probe_failure("ghost-task");
    assert!(backend.probe(&session).unwrap().alive);
}
