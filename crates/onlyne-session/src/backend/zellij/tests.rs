use super::*;
use parking_lot::Mutex;
use std::collections::VecDeque;

/// A uuid v4 task id, the shape the server mints.
const TASK: &str = "550e8400-e29b-41d4-a716-446655440000";

/// Answers one scripted `(status, stdout)` per call and records every argv,
/// so a test reads back the session name the CLI was handed.
#[derive(Default)]
struct ScriptRunner {
    calls: Mutex<Vec<Vec<String>>>,
    script: Mutex<VecDeque<(i32, String)>>,
}

impl ScriptRunner {
    fn reply(self, status: i32, stdout: &str) -> Self {
        self.script.lock().push_back((status, stdout.to_string()));
        self
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().clone()
    }
}

impl Runner for ScriptRunner {
    fn run(
        &self,
        _program: &str,
        args: &[String],
        _cwd: Option<&Path>,
        _env: &BTreeMap<String, String>,
    ) -> Result<CommandOutput> {
        self.calls.lock().push(args.to_vec());
        let (status, stdout) = self
            .script
            .lock()
            .pop_front()
            .unwrap_or_else(|| (0, String::new()));
        Ok(CommandOutput {
            status,
            stdout: stdout.into_bytes(),
            stderr: Vec::new(),
        })
    }
}

fn spec() -> SpawnSpec {
    SpawnSpec {
        cwd: PathBuf::from("/tmp/ws"),
        task_id: TASK.into(),
        command: vec!["pi".into()],
        env: BTreeMap::new(),
        focus: None,
        placement: None,
        rename: None,
    }
}

#[test]
fn the_name_is_short_enough_for_the_socket_budget() {
    let name = short_session_name(TASK);
    assert_eq!(name, "onlyne-550e8400e29b");
    assert_eq!(name.len(), SESSION_PREFIX.len() + SESSION_ID_CHARS);
    // The full task id used to be the name. At 43 bytes it cannot fit
    // beside a socket directory this machine already spends 79 bytes on,
    // which is what made every zellij spawn fail.
    let full = format!("onlyne-{TASK}");
    assert_eq!(full.len(), 43);
    assert_ne!(name, full);
    assert!(!name.contains(TASK));
}

#[test]
fn the_name_is_a_pure_function_of_the_task_id() {
    assert_ne!(
        short_session_name(TASK),
        short_session_name("6ba7b810-9dad-11d1-80b4-00c04fd430c8")
    );
    // A task id that is not a uuid still maps to a name, and no id length
    // carries into it.
    assert_eq!(short_session_name("task-1"), "onlyne-task1");
    assert_eq!(
        short_session_name(&"a".repeat(400)).len(),
        SESSION_PREFIX.len() + SESSION_ID_CHARS
    );
}

#[test]
fn the_budget_stops_one_byte_short_of_the_zellij_limit() {
    let name = short_session_name(TASK);
    let dir = |len: usize| PathBuf::from(format!("/{}", "d".repeat(len - 1)));
    // `dir` + "/" + `name` one byte under the limit is accepted; one byte
    // more is refused, naming the override that fixes it.
    let fits = SOCK_PATH_LIMIT - 1 - name.len() - 1;
    assert!(check_socket_budget(&dir(fits), &name).is_ok());
    let error = check_socket_budget(&dir(fits + 1), &name)
        .unwrap_err()
        .to_string();
    assert!(error.contains("ZELLIJ_SOCKET_DIR"), "{error}");
    assert!(error.contains(&name), "{error}");
}

/// One call's argv, as a comparable list.
fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_string()).collect()
}

/// The name every assertion here expects for [`TASK`].
const NAME: &str = "onlyne-550e8400e29b";

/// The `run` argv: the command becomes a pane of the named session.
fn run_argv() -> Vec<String> {
    argv(&[
        "--session",
        NAME,
        "run",
        "--cwd",
        "/tmp/ws",
        "--no-focus",
        "--",
        "pi",
    ])
}

/// A spawn whose session does not exist brings it up first, because `zellij
/// run` is an action addressed to a live session.
#[test]
fn spawn_creates_the_session_before_running_in_it() {
    let runner = Arc::new(
        ScriptRunner::default()
            .reply(0, "other\n")
            .reply(0, "")
            .reply(0, "terminal_3\n"),
    );
    let backend = ZellijBackend::new(runner.clone());
    let session = backend.spawn(spec()).unwrap();
    let calls = runner.calls();
    assert_eq!(calls[0], argv(&["list-sessions", "--short"]));
    assert_eq!(calls[1], argv(&["attach", "--create-background", NAME]));
    assert_eq!(calls[2], run_argv());
    assert_eq!(session.backend_ref["session"], NAME);
    assert_eq!(session.backend_ref["pane"], "terminal_3");
}

/// A session that is already listed is reused rather than created again.
#[test]
fn spawn_reuses_a_session_that_is_already_listed() {
    let runner = Arc::new(
        ScriptRunner::default()
            .reply(0, "other\nonlyne-550e8400e29b\n")
            .reply(0, "terminal_4\n"),
    );
    let backend = ZellijBackend::new(runner.clone());
    backend.spawn(spec()).unwrap();
    let calls = runner.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0], argv(&["list-sessions", "--short"]));
    assert_eq!(calls[1], run_argv());
}

/// A create that fails stops the spawn: no run is attempted against a
/// session that is not there.
#[test]
fn a_failed_create_short_circuits_before_the_run() {
    let runner = Arc::new(ScriptRunner::default().reply(0, "other\n").reply(1, ""));
    let backend = ZellijBackend::new(runner.clone());
    let error = backend.spawn(spec()).unwrap_err().to_string();
    assert!(error.contains("attach --create-background"), "{error}");
    assert_eq!(runner.calls().len(), 2);
}

/// A run that fails after this call created the session takes that session
/// with it: nothing outlives a spawn that produced no session ref.
#[test]
fn a_failed_run_reclaims_the_session_it_created() {
    let runner = Arc::new(
        ScriptRunner::default()
            .reply(0, "other\n")
            .reply(0, "")
            .reply(1, "")
            .reply(0, ""),
    );
    let backend = ZellijBackend::new(runner.clone());
    let error = backend.spawn(spec()).unwrap_err().to_string();
    assert!(error.contains("status 1"), "{error}");
    let calls = runner.calls();
    assert_eq!(calls.len(), 4);
    assert_eq!(calls[3], argv(&["kill-session", NAME]));
}

/// A run that fails against a session this call did not create leaves it
/// alone: that session may still back a live session ref.
#[test]
fn a_failed_run_keeps_a_session_it_did_not_create() {
    let runner = Arc::new(
        ScriptRunner::default()
            .reply(0, "onlyne-550e8400e29b\n")
            .reply(1, ""),
    );
    let backend = ZellijBackend::new(runner.clone());
    assert!(backend.spawn(spec()).is_err());
    assert_eq!(runner.calls().len(), 2);
}

/// `attach` matches the derived name in `list-sessions` and `close` kills
/// it, even when the ref was written under the old 43-byte naming.
#[test]
fn attach_and_close_derive_the_name_instead_of_reading_the_ref() {
    let runner = Arc::new(
        ScriptRunner::default()
            .reply(0, "other\nonlyne-550e8400e29b\n")
            .reply(0, ""),
    );
    let backend = ZellijBackend::new(runner.clone());
    let stored = SessionRef {
        task_id: TASK.into(),
        backend: "zellij".into(),
        backend_ref: serde_json::json!({
            "session": format!("onlyne-{TASK}"),
            "pane": "terminal_3"
        }),
        generation: 1,
    };
    backend.attach(&stored).unwrap();
    backend
        .close(&stored, CloseReason::Completed, false)
        .unwrap();
    let calls = runner.calls();
    assert_eq!(calls[0], argv(&["list-sessions", "--short"]));
    assert_eq!(calls[1], argv(&["kill-session", NAME]));
}

fn stored(pane: &str) -> SessionRef {
    SessionRef {
        task_id: TASK.into(),
        backend: "zellij".into(),
        backend_ref: serde_json::json!({"session": NAME, "pane": pane}),
        generation: 1,
    }
}

fn panes_argv() -> Vec<String> {
    argv(&[
        "--session",
        NAME,
        "action",
        "list-panes",
        "--json",
        "--state",
        "--command",
    ])
}

#[test]
fn probe_maps_exited_pane_exit_status() {
    let runner = Arc::new(
        ScriptRunner::default()
            .reply(0, "onlyne-550e8400e29b [Created 1s ago]\n")
            .reply(
                0,
                r#"[{"id":3,"is_plugin":false,"exited":true,"exit_status":7,"is_held":false}]"#,
            ),
    );
    let backend = ZellijBackend::new(runner.clone());
    let probe = backend.probe(&stored("terminal_3")).unwrap();
    assert!(!probe.alive);
    assert!(!probe.attached);
    let detail = probe.detail.unwrap();
    assert_eq!(detail["exit"], 7);
    assert_eq!(detail["exited"], true);
    let calls = runner.calls();
    assert_eq!(calls[0], argv(&["list-sessions", "--no-formatting"]));
    assert_eq!(calls[1], panes_argv());
    assert!(
        calls
            .iter()
            .all(|call| call.get(1).map(String::as_str) != Some("--short"))
    );
}

#[test]
fn probe_maps_a_held_pane_without_inventing_an_exit_code() {
    let runner = Arc::new(
        ScriptRunner::default()
            .reply(0, "onlyne-550e8400e29b [Created 1s ago]\n")
            .reply(
                0,
                r#"[{"id":3,"is_plugin":false,"exited":false,"exit_status":null,"is_held":true}]"#,
            ),
    );
    let backend = ZellijBackend::new(runner);
    let probe = backend.probe(&stored("terminal_3")).unwrap();
    assert!(!probe.alive);
    let detail = probe.detail.unwrap();
    assert!(detail["exit"].is_null(), "{detail}");
    assert_eq!(detail["exited"], true);
}

#[test]
fn probe_reports_a_missing_pane_row() {
    let runner = Arc::new(
        ScriptRunner::default()
            .reply(0, "onlyne-550e8400e29b [Created 1s ago]\n")
            .reply(0, r#"[{"id":1,"is_plugin":false,"exited":false}]"#),
    );
    let backend = ZellijBackend::new(runner);
    let probe = backend.probe(&stored("terminal_3")).unwrap();
    assert!(!probe.alive);
    assert_eq!(probe.detail.unwrap()["reason"], "pane_missing");
}

#[test]
fn probe_does_not_attach_an_exited_session() {
    let runner =
        Arc::new(ScriptRunner::default().reply(0, "onlyne-550e8400e29b [Created 2h ago] EXITED\n"));
    let backend = ZellijBackend::new(runner.clone());
    let probe = backend.probe(&stored("terminal_3")).unwrap();
    assert!(!probe.alive);
    assert_eq!(probe.detail.unwrap()["reason"], "session_exited");
    assert_eq!(
        runner.calls(),
        vec![argv(&["list-sessions", "--no-formatting"])]
    );
}

#[test]
fn probe_reports_a_missing_session_name() {
    let runner = Arc::new(ScriptRunner::default().reply(0, "other\n"));
    let backend = ZellijBackend::new(runner.clone());
    let probe = backend.probe(&stored("terminal_3")).unwrap();
    assert!(!probe.alive);
    assert_eq!(probe.detail.unwrap()["reason"], "session_missing");
    assert_eq!(
        runner.calls(),
        vec![argv(&["list-sessions", "--no-formatting"])]
    );
}

#[test]
fn probe_accepts_a_bare_pane_id_and_a_plugin_token() {
    assert_eq!(parse_pane_token("terminal_3"), Some((3, false)));
    assert_eq!(parse_pane_token("3"), Some((3, false)));
    assert_eq!(parse_pane_token("plugin_2"), Some((2, true)));
    let rows = serde_json::json!([{"id":2,"is_plugin":true,"exited":true,"exit_status":9}]);
    let probe = probe_pane(&rows, "plugin_2");
    assert!(!probe.alive);
    assert_eq!(probe.detail.unwrap()["exit"], 9);
}
