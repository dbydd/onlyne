use super::*;

#[test]
fn spawn_creates_workspace_tab_and_splits() {
    let script = Script::default()
        .reply(
            "workspace list",
            0,
            envelope(serde_json::json!({"workspaces": []})),
        )
        .reply("workspace create", 0, create_workspace())
        .reply("tab list", 0, envelope(serde_json::json!({"tabs": []})))
        .reply("tab create", 0, create_tab())
        .reply("pane split", 0, split_pane())
        .reply("agent start", 0, agent_started());
    let (backend, script) = backend(script);
    let session = backend.spawn(spec(session_command())).unwrap();
    assert_eq!(session.backend, "herdr");
    assert_eq!(session.backend_ref["herdr"]["pane_id"], "wF:p2");
    assert_eq!(
        session.backend_ref["herdr"]["agent"],
        "onlyne-planner-abcd1234"
    );
    assert_eq!(
        session.backend_ref["herdr"]["workspace_label"],
        "onlyne:lab"
    );
    assert_eq!(session.backend_ref["herdr"]["workspace_id"], "wF");
    let cwd = absolute_cwd(Path::new("/tmp/ws"));
    let calls = script.calls();
    assert!(calls.iter().any(|call| call.contains("workspace list")));
    assert!(calls.iter().any(|call| {
        call.contains("workspace create")
            && call.contains("--label")
            && call.contains("onlyne:lab")
            && call.contains(&format!("--cwd {cwd}"))
            && call.contains("--no-focus")
    }));
    assert!(calls.iter().any(|call| {
        call.contains("tab create")
            && call.contains("--workspace")
            && call.contains("wF")
            && call.contains("--label")
            && call.contains("planner")
            && call.contains(&format!("--cwd {cwd}"))
            && call.contains("--no-focus")
    }));
    let split = calls
        .iter()
        .find(|call| call.contains("pane split"))
        .unwrap();
    assert!(split.contains("--pane wF:p1"));
    assert!(split.contains("--direction right"));
    assert!(split.contains("--ratio 0.5"));
    assert!(split.contains(&format!("--cwd {cwd}")));
    assert!(split.contains("--env ONLYNE_CLUSTER=lab"));
    assert!(split.contains("--env ONLYNE_ROLE=planner"));
    assert!(split.contains("--no-focus"));
    // The record ends at the last agent argument, so the tail reaches the
    // pane whole.
    let started = calls
        .iter()
        .find(|call| call.contains("agent start"))
        .unwrap();
    assert!(
        started.ends_with(
            "--kind pi --pane wF:p2 --timeout 25000 -- --session-id s-1 --session-dir .pi/sessions"
        ),
        "{started}"
    );
}

#[test]
fn spawn_sends_no_separator_for_a_bare_agent_command() {
    // Token 0 is the whole command, so there is nothing for herdr to forward
    // and an empty trailing argument list would reach the agent as noise.
    let script = Script::default()
        .reply(
            "workspace list",
            0,
            envelope(serde_json::json!({"workspaces": []})),
        )
        .reply("workspace create", 0, create_workspace())
        .reply("tab list", 0, envelope(serde_json::json!({"tabs": []})))
        .reply("tab create", 0, create_tab())
        .reply("pane split", 0, split_pane())
        .reply("agent start", 0, agent_started());
    let (backend, script) = backend(script);
    backend.spawn(spec(vec!["pi"])).unwrap();
    let started = script
        .calls()
        .into_iter()
        .find(|call| call.contains("agent start"))
        .unwrap();
    assert_eq!(
        started,
        "herdr agent start onlyne-planner-abcd1234 --kind pi --pane wF:p2 --timeout 25000"
    );
}

#[test]
fn spawn_sends_an_absolute_cwd_for_a_relative_workspace() {
    // herdr resolves a relative `--cwd` against its own working directory,
    // which is how session panes landed in `$HOME` during the first real
    // run of a formal-research tree.
    let script = Script::default()
        .reply(
            "workspace list",
            0,
            envelope(serde_json::json!({"workspaces": []})),
        )
        .reply("workspace create", 0, create_workspace())
        .reply("tab list", 0, envelope(serde_json::json!({"tabs": []})))
        .reply("tab create", 0, create_tab())
        .reply("pane split", 0, split_pane())
        .reply("agent start", 0, agent_started());
    let (backend, script) = backend(script);
    backend
        .spawn(spec_at(session_command(), "ws/formal/research/planner"))
        .unwrap();
    let cwd = absolute_cwd(Path::new("ws/formal/research/planner"));
    assert!(Path::new(&cwd).is_absolute(), "{cwd}");
    let calls = script.calls();
    for fragment in ["workspace create", "tab create", "pane split"] {
        let call = calls.iter().find(|call| call.contains(fragment)).unwrap();
        assert!(call.contains(&format!("--cwd {cwd}")), "{call}");
    }
}

#[test]
fn spawn_reuses_workspace_and_tab() {
    let script = Script::default()
        .reply(
            "workspace list",
            0,
            envelope(serde_json::json!({
                "workspaces": [{
                    "workspace_id": "wF",
                    "label": "onlyne:lab",
                    "active_tab_id": "wF:t1",
                    "pane_count": 2,
                    "focused": true
                }]
            })),
        )
        .reply(
            "tab list",
            0,
            envelope(serde_json::json!({
                "tabs": [{
                    "tab_id": "wF:t1",
                    "label": "planner",
                    "pane_count": 2,
                    "workspace_id": "wF"
                }]
            })),
        )
        .reply(
            "pane list",
            0,
            envelope(serde_json::json!({
                "panes": [{
                    "pane_id": "wF:p1",
                    "tab_id": "wF:t1",
                    "workspace_id": "wF",
                    "focused": true
                }]
            })),
        )
        .reply("pane split", 0, split_pane())
        .reply("agent start", 0, agent_started());
    let (backend, script) = backend(script);
    backend.spawn(spec(vec!["pi"])).unwrap();
    let calls = script.calls().join("\n");
    assert!(!calls.contains("workspace create"));
    assert!(!calls.contains("tab create"));
    assert!(calls.contains("pane split"));
}

#[test]
fn pane_run_line_quotes_for_posix_and_cmd() {
    let cases: &[(&[&str], &str, &str)] = &[
        (
            &["echo", "hello world"],
            "'echo' 'hello world'",
            "\"echo\" \"hello world\"",
        ),
        (&["a'b"], "'a'\\''b'", "\"a'b\""),
        (&["say", r#"x"y"#], "'say' 'x\"y'", "\"say\" \"x\"\"y\""),
    ];
    for (argv, posix, cmd) in cases {
        let tokens: Vec<String> = argv.iter().map(|s| (*s).to_string()).collect();
        let posix_line = tokens
            .iter()
            .map(|arg| posix_shell_quote(arg))
            .collect::<Vec<_>>()
            .join(" ");
        let cmd_line = tokens
            .iter()
            .map(|arg| cmd_quote(arg))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(posix_line, *posix, "posix {argv:?}");
        assert_eq!(cmd_line, *cmd, "cmd {argv:?}");
    }
    let live = pane_run_line(&["echo".into(), "hello world".into()]);
    #[cfg(unix)]
    assert_eq!(live, "'echo' 'hello world'");
    #[cfg(windows)]
    assert_eq!(live, "\"echo\" \"hello world\"");
}

#[test]
fn spawn_falls_back_to_pane_run_for_unknown_kind() {
    let script = Script::default()
        .reply(
            "workspace list",
            0,
            envelope(serde_json::json!({"workspaces": []})),
        )
        .reply("workspace create", 0, create_workspace())
        .reply("tab list", 0, envelope(serde_json::json!({"tabs": []})))
        .reply("tab create", 0, create_tab())
        .reply("pane split", 0, split_pane())
        .reply("pane run", 0, String::new());
    let (backend, script) = backend(script);
    // A tail that would qualify for `agent start` still travels as shell
    // arguments, since herdr has no kind for the leading executable.
    let command = vec!["echo", "hello world", "--session-id", "s-1"];
    backend.spawn(spec(command.clone())).unwrap();
    let calls = script.calls();
    assert!(calls.iter().all(|call| !call.contains("agent start")));
    let tokens: Vec<String> = command.into_iter().map(str::to_string).collect();
    let line = pane_run_line(&tokens);
    assert!(
        calls
            .iter()
            .any(|call| { call.contains("pane run wF:p2") && call.contains(&line) })
    );
}

#[test]
fn spawn_falls_back_to_pane_run_when_agent_start_fails() {
    let script = Script::default()
        .reply(
            "workspace list",
            0,
            envelope(serde_json::json!({"workspaces": []})),
        )
        .reply("workspace create", 0, create_workspace())
        .reply("tab list", 0, envelope(serde_json::json!({"tabs": []})))
        .reply("tab create", 0, create_tab())
        .reply("pane split", 0, split_pane())
        .reply(
            "agent start",
            1,
            r#"{"id":"cli:agent:start","error":{"message":"not ready"}}"#,
        )
        .reply("pane run", 0, String::new());
    let (backend, script) = backend(script);
    backend.spawn(spec(session_command())).unwrap();
    let joined = script.calls().join("\n");
    assert!(joined.contains("--timeout 25000 -- --session-id s-1"));
    assert!(joined.contains("pane run wF:p2"));
}

#[test]
fn agent_name_follows_herdr_charset_and_length() {
    assert_eq!(
        agent_name("planner", "ABCD1234-ffff-4000-8000-000000000001"),
        "onlyne-planner-abcd1234"
    );
    assert!(agent_name("planner", "abcd1234ffff").len() <= 32);
    assert!(is_agent_name(&agent_name("planner", "abcd1234ffff")));
}
