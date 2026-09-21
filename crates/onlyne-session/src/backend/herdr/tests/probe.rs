use super::*;

#[test]
fn probe_maps_pane_get_and_process_info() {
    let script = Script::default()
        .reply(
            "pane get",
            0,
            envelope(serde_json::json!({
                "type": "pane_info",
                "pane": {
                    "pane_id": "wF:p2",
                    "agent_status": "idle",
                    "revision": 4,
                    "focused": false
                }
            })),
        )
        .reply(
            "process-info",
            0,
            envelope(serde_json::json!({
                "process_info": {"pane_id": "wF:p2", "shell_pid": 9}
            })),
        );
    let (backend, _) = backend(script);
    let probe = backend.probe(&session_ref("wF:p2")).unwrap();
    assert!(probe.alive);
    assert!(probe.attached);
    assert_eq!(probe.detail.unwrap()["agent_status"], "idle");
}

#[test]
fn probe_missing_pane_is_dead() {
    let script = Script::default().reply("pane get", 1, r#"{"id":"cli:pane:get"}"#);
    let (backend, _) = backend(script);
    let probe = backend.probe(&session_ref("wF:p9")).unwrap();
    assert!(!probe.alive);
    assert!(!probe.attached);
}

#[test]
fn close_sends_pane_close() {
    let script =
        Script::default().reply("pane close", 0, envelope(serde_json::json!({"type": "ok"})));
    let (backend, script) = backend(script);
    backend
        .close(&session_ref("wF:p2"), CloseReason::Completed, true)
        .unwrap();
    assert!(
        script
            .calls()
            .iter()
            .any(|call| call == "herdr pane close wF:p2")
    );
}

#[test]
fn available_reads_injected_env() {
    let (backend, _) = backend(Script::default());
    assert!(backend.available().unwrap());
    let missing = HerdrBackend::with_env(Arc::new(Script::default()), BTreeMap::new());
    assert!(!missing.available().unwrap());
}
