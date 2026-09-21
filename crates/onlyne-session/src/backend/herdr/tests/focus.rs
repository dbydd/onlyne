use super::*;

#[test]
fn focus_walks_workspace_tab_agent_when_elsewhere() {
    let script = Script::default()
        .reply("pane list", 0, focused_pane("w1", "w1:t1", "w1:p1"))
        .reply(
            "workspace focus",
            0,
            envelope(serde_json::json!({"type": "workspace_info"})),
        )
        .reply(
            "tab focus",
            0,
            envelope(serde_json::json!({"type": "tab_info"})),
        )
        .reply(
            "agent focus",
            0,
            envelope(serde_json::json!({"type": "agent_info"})),
        )
        .reply("pane get", 0, pane_info("wF:p2", true));
    let (backend, script) = backend(script);
    backend.focus(&session_ref("wF:p2")).unwrap();
    assert_eq!(
        script.calls(),
        vec![
            "herdr pane list".to_string(),
            "herdr workspace focus wF".to_string(),
            "herdr tab focus wF:t1".to_string(),
            "herdr agent focus wF:p2".to_string(),
            "herdr pane get wF:p2".to_string(),
        ]
    );
}

#[test]
fn focus_skips_workspace_and_tab_when_already_there() {
    let script = Script::default()
        .reply("pane list", 0, focused_pane("wF", "wF:t1", "wF:p1"))
        .reply(
            "agent focus",
            0,
            envelope(serde_json::json!({"type": "agent_info"})),
        )
        .reply("pane get", 0, pane_info("wF:p2", true));
    let (backend, script) = backend(script);
    backend.focus(&session_ref("wF:p2")).unwrap();
    assert_eq!(
        script.calls(),
        vec![
            "herdr pane list".to_string(),
            "herdr agent focus wF:p2".to_string(),
            "herdr pane get wF:p2".to_string(),
        ]
    );
}

#[test]
fn focus_skips_every_hop_when_the_pane_already_holds_focus() {
    let script = Script::default().reply("pane list", 0, focused_pane("wF", "wF:t1", "wF:p2"));
    let (backend, script) = backend(script);
    backend.focus(&session_ref("wF:p2")).unwrap();
    assert_eq!(script.calls(), vec!["herdr pane list".to_string()]);
}

#[test]
fn focus_walks_the_anchor_for_a_shell_pane() {
    let script = Script::default()
        .reply("pane list", 0, focused_pane("w1", "w1:t1", "w1:p1"))
        .reply(
            "workspace focus",
            0,
            envelope(serde_json::json!({"type": "workspace_info"})),
        )
        .reply(
            "tab focus",
            0,
            envelope(serde_json::json!({"type": "tab_info"})),
        )
        .reply("pane focus", 0, envelope(serde_json::json!({"type": "ok"})))
        .reply("pane get", 0, pane_info("wF:p2", true));
    let (backend, script) = backend(script);
    backend.focus(&shell_session_ref("wF:p2")).unwrap();
    assert_eq!(
        script.calls(),
        vec![
            "herdr pane list".to_string(),
            "herdr workspace focus wF".to_string(),
            "herdr tab focus wF:t1".to_string(),
            "herdr pane focus --pane wF:p1 --direction right".to_string(),
            "herdr pane get wF:p2".to_string(),
        ]
    );
}

#[test]
fn focus_walks_the_anchor_in_the_recorded_direction() {
    // A `down` split reaches its pane by walking down from the anchor. The
    // direction comes from the recorded split, so a wrong value would move
    // focus to another session's pane.
    let script = Script::default()
        .reply("pane list", 0, focused_pane("wF", "wF:t1", "wF:p1"))
        .reply("pane focus", 0, envelope(serde_json::json!({"type": "ok"})))
        .reply("pane get", 0, pane_info("wF:p3", true));
    let (backend, script) = backend(script);
    let reference = session_ref_with("wF:p3", "", "wF:p1", "down");
    backend.focus(&reference).unwrap();
    assert_eq!(
        script.calls()[1],
        "herdr pane focus --pane wF:p1 --direction down".to_string()
    );
}

#[test]
fn focus_falls_back_to_the_anchor_when_the_agent_is_gone() {
    let script = Script::default()
            .reply("pane list", 0, focused_pane("wF", "wF:t1", "wF:p1"))
            .reply_err(
                "agent focus",
                1,
                r#"{"error":{"code":"agent_not_found","message":"agent target wF:p2 not found"},"id":"cli:agent:focus"}"#,
            )
            .reply("pane focus", 0, envelope(serde_json::json!({"type": "ok"})))
            .reply("pane get", 0, pane_info("wF:p2", true));
    let (backend, script) = backend(script);
    backend.focus(&session_ref("wF:p2")).unwrap();
    assert_eq!(
        script.calls(),
        vec![
            "herdr pane list".to_string(),
            "herdr agent focus wF:p2".to_string(),
            "herdr pane focus --pane wF:p1 --direction right".to_string(),
            "herdr pane get wF:p2".to_string(),
        ]
    );
}

#[test]
fn focus_reports_a_refused_agent_hop_without_navigation() {
    // Only `agent_not_found` opens the anchor path. Any other refusal is
    // the answer and reaches the caller unchanged.
    let script = Script::default()
            .reply("pane list", 0, focused_pane("wF", "wF:t1", "wF:p1"))
            .reply_err(
                "agent focus",
                1,
                r#"{"error":{"code":"pane_not_found","message":"pane wF:p2 not found"},"id":"cli:agent:focus"}"#,
            );
    let (backend, script) = backend(script);
    let error = backend.focus(&session_ref("wF:p2")).unwrap_err();
    assert!(error.to_string().contains("pane_not_found"), "{error}");
    assert!(
        !script
            .calls()
            .iter()
            .any(|call| call.contains("pane focus"))
    );
}

#[test]
fn focus_reports_a_failure_when_another_pane_holds_focus() {
    let script = Script::default()
        .reply("pane list", 0, focused_pane("wF", "wF:t1", "wF:p1"))
        .reply("pane focus", 0, envelope(serde_json::json!({"type": "ok"})))
        .reply("pane get", 0, pane_info("wF:p2", false))
        .reply("pane list", 0, focused_pane("wF", "wF:t1", "wF:p9"));
    let (backend, _) = backend(script);
    let error = backend.focus(&shell_session_ref("wF:p2")).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("herdr left pane wF:p2 unfocused (focused pane wF:p9)"),
        "{error}"
    );
}

#[test]
fn focus_reports_a_failure_without_a_recorded_anchor() {
    let script = Script::default().reply("pane list", 0, focused_pane("wF", "wF:t1", "wF:p1"));
    let (backend, script) = backend(script);
    let reference = session_ref_with("wF:p2", "", "", "");
    let error = backend.focus(&reference).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("has no recorded split anchor, so focus has no direction to walk"),
        "{error}"
    );
    assert_eq!(script.calls(), vec!["herdr pane list".to_string()]);
}
