use super::*;
use std::collections::BTreeMap;
use std::path::Path;

fn spec(cwd: &Path, task: &str, command: Vec<&str>) -> SpawnSpec {
    SpawnSpec {
        cwd: cwd.to_path_buf(),
        task_id: task.into(),
        command: command.into_iter().map(str::to_string).collect(),
        env: BTreeMap::new(),
        focus: None,
        placement: None,
        rename: None,
    }
}

#[cfg(unix)]
fn log_text(spec: &SpawnSpec) -> String {
    let path = spec
        .cwd
        .join(".onlyne")
        .join("logs")
        .join(format!("session-{}.log", spec.task_id));
    std::fs::read_to_string(path).unwrap_or_default()
}

/// The probe blocks in `read` and writes nothing until a line arrives, so a
/// child still alive with no `got=` line afterwards is a child whose stdin
/// was not at EOF: `Stdio::null()` or a dropped write end would make `read`
/// return at once, the script would print `got=` and exit.
#[cfg(unix)]
#[test]
fn a_spawned_child_does_not_see_stdin_eof() {
    let dir = tempfile::tempdir().unwrap();
    let backend = ExecBackend::new();
    let spawned = spec(
        dir.path(),
        "stdin-open",
        vec!["sh", "-c", "read line; printf 'got=%s\\n' \"$line\""],
    );
    let session = backend.spawn(spawned.clone()).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let probe = backend.probe(&session).unwrap();
    assert!(
        probe.alive,
        "the child must still be parked in read: {probe:?}"
    );
    assert!(probe.attached);
    assert_eq!(
        log_text(&spawned),
        "",
        "no line arrived, so read cannot have returned"
    );
    backend
        .close(&session, CloseReason::Completed, false)
        .unwrap();
    assert!(!backend.probe(&session).unwrap().alive);
}

/// The child's own output is what an operator reads, so both streams land in
/// the workspace log rather than in the client's stdio.
#[cfg(unix)]
#[test]
fn child_stdio_lands_in_the_workspace_log() {
    let dir = tempfile::tempdir().unwrap();
    let backend = ExecBackend::new();
    let spawned = spec(
        dir.path(),
        "logs",
        vec!["sh", "-c", "echo out; echo err 1>&2"],
    );
    let session = backend.spawn(spawned.clone()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let text = log_text(&spawned);
        if text.contains("out") && text.contains("err") {
            break;
        }
        std::thread::sleep(REAP_POLL);
    }
    let text = log_text(&spawned);
    assert!(text.contains("out"), "stdout must reach the log: {text:?}");
    assert!(text.contains("err"), "stderr must reach the log: {text:?}");
    backend
        .close(&session, CloseReason::Completed, true)
        .unwrap();
}

#[cfg(unix)]
#[test]
fn probe_follows_the_child_until_close_reaps_it() {
    let dir = tempfile::tempdir().unwrap();
    let backend = ExecBackend::new();
    let session = backend
        .spawn(spec(dir.path(), "long", vec!["sleep", "30"]))
        .unwrap();
    let pid = session.backend_ref["pid"].as_u64().unwrap() as u32;
    assert!(backend.probe(&session).unwrap().alive);
    assert!(backend.attach(&session).is_ok());
    backend
        .close(&session, CloseReason::Cancelled, false)
        .unwrap();
    assert!(!backend.probe(&session).unwrap().alive);
    assert!(
        !ExecBackend::pid_alive(pid),
        "the session must leave no process behind"
    );
    assert!(backend.attach(&session).is_err());
    // Close is idempotent: a second pass has nothing left to signal.
    backend
        .close(&session, CloseReason::Cancelled, false)
        .unwrap();
}

#[cfg(unix)]
#[test]
fn force_close_kills_a_child_that_ignores_term() {
    let dir = tempfile::tempdir().unwrap();
    let backend = ExecBackend::new();
    let session = backend
        .spawn(spec(
            dir.path(),
            "stubborn",
            vec!["sh", "-c", "trap '' TERM; sleep 30"],
        ))
        .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    backend
        .close(&session, CloseReason::Shutdown, true)
        .unwrap();
    assert!(!backend.probe(&session).unwrap().alive);
}

/// The field shape this backend has to survive: an agent starts work of its
/// own, and closing the session stops that work too. Signalling the leader
/// pid alone leaves the `sleep` running under no owner — the reported case was
/// a driver script that kept rewriting the measured surface minutes after its
/// session was closed.
#[cfg(unix)]
#[test]
fn close_stops_the_children_the_session_started() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("grandchild");
    let backend = ExecBackend::new();
    let script = format!(
        "sleep 120 & printf '%s' \"$!\" > {}; wait",
        marker.display()
    );
    let session = backend
        .spawn(spec(dir.path(), "group", vec!["sh", "-c", &script]))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let grandchild = loop {
        let reported = std::fs::read_to_string(&marker).unwrap_or_default();
        if !reported.is_empty() {
            break reported.trim().parse::<u32>().expect("a pid");
        }
        assert!(Instant::now() < deadline, "the child never reported");
        std::thread::sleep(REAP_POLL);
    };
    backend
        .close(&session, CloseReason::Operator, false)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while ExecBackend::pid_alive(grandchild) && Instant::now() < deadline {
        std::thread::sleep(REAP_POLL);
    }
    assert!(
        !ExecBackend::pid_alive(grandchild),
        "pid {grandchild} outlived the session that started it"
    );
}

#[test]
fn a_role_without_a_session_command_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let backend = ExecBackend::new();
    let error = backend
        .spawn(spec(dir.path(), "empty", vec![]))
        .unwrap_err();
    assert!(error.to_string().contains("no session_command"), "{error}");
}

#[test]
fn an_unspawnable_command_is_an_error_rather_than_an_empty_session() {
    let dir = tempfile::tempdir().unwrap();
    let backend = ExecBackend::new();
    let error = backend
        .spawn(spec(
            dir.path(),
            "missing",
            vec!["onlyne-there-is-no-such-binary"],
        ))
        .unwrap_err();
    assert!(error.to_string().contains("spawn"), "{error}");
}

fn spec_cmd(cwd: &Path, task: &str, command: Vec<String>) -> SpawnSpec {
    SpawnSpec {
        cwd: cwd.to_path_buf(),
        task_id: task.into(),
        command,
        env: BTreeMap::new(),
        focus: None,
        placement: None,
        rename: None,
    }
}

fn echo_and_exit(message: &str, code: i32) -> Vec<String> {
    #[cfg(unix)]
    {
        vec![
            "sh".into(),
            "-c".into(),
            format!("printf '%s\\n' '{message}'; exit {code}"),
        ]
    }
    #[cfg(windows)]
    {
        vec![
            "cmd".into(),
            "/C".into(),
            format!("echo {message}& exit {code}"),
        ]
    }
}

#[cfg(windows)]
fn sleep_cmd() -> Vec<String> {
    vec!["ping".into(), "-n".into(), "31".into(), "127.0.0.1".into()]
}

fn wait_until_exit(backend: &ExecBackend, session: &SessionRef) -> ResourceProbe {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let probe = backend.probe(session).unwrap();
        if !probe.alive {
            return probe;
        }
        assert!(
            Instant::now() < deadline,
            "the child never exited: {probe:?}"
        );
        std::thread::sleep(REAP_POLL);
    }
}

#[test]
fn a_finished_child_reports_its_exit_code_and_log_tail() {
    let dir = tempfile::tempdir().unwrap();
    let backend = ExecBackend::new();
    let session = backend
        .spawn(spec_cmd(
            dir.path(),
            "exit-code",
            echo_and_exit("onlyne-exec-tail", 7),
        ))
        .unwrap();
    assert_eq!(session.backend, "exec");
    assert_eq!(session.backend_ref["id"], "exit-code");
    assert!(
        session
            .backend_ref
            .get("pid")
            .and_then(Value::as_u64)
            .is_some()
    );
    let log = session.backend_ref["log"].as_str().expect("log path");
    assert!(
        log.ends_with("session-exit-code.log"),
        "log path must name the session file: {log}"
    );

    let probe = wait_until_exit(&backend, &session);
    let detail = probe.detail.expect("exit detail");
    assert_eq!(detail["exit"], 7, "{detail}");
    let tail = detail["output_tail"].as_str().unwrap_or("");
    assert!(
        tail.contains("onlyne-exec-tail"),
        "output_tail must carry the child's line: {tail:?}"
    );
    let meta = std::fs::metadata(log).expect("log file");
    assert!(meta.len() > 0, "the session log must grow");
    let again = backend.probe(&session).unwrap();
    assert_eq!(again.detail.unwrap()["exit"], 7);
}

#[cfg(windows)]
#[test]
fn windows_close_reaps_when_ctrl_break_has_no_console() {
    let dir = tempfile::tempdir().unwrap();
    let backend = ExecBackend::new();
    let session = backend
        .spawn(spec_cmd(dir.path(), "win-kill", sleep_cmd()))
        .unwrap();
    let pid = session.backend_ref["pid"].as_u64().unwrap() as u32;
    backend
        .close(&session, CloseReason::Shutdown, false)
        .unwrap();
    assert!(!backend.probe(&session).unwrap().alive);
    assert!(
        !ExecBackend::pid_alive(pid),
        "GenerateConsoleCtrlEvent failure must fall through to child.kill"
    );
}

/// A sibling whose argv contains `onlyne` must outlive session close.
/// `kill(2)` on pid 0/`-1` or a cmdline glob would take it — and a GHA
/// runner whose argv contains `/home/runner/work/onlyne/onlyne`.
#[cfg(unix)]
#[test]
fn close_does_not_signal_an_unrelated_onlyne_named_process() {
    let mut canary = Command::new("bash")
        .args(["-c", "exec -a onlyne-canary sleep 120"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("canary");
    let canary_pid = canary.id();
    let dir = tempfile::tempdir().unwrap();
    let backend = ExecBackend::new();
    let session = backend
        .spawn(spec(dir.path(), "canary-session", vec!["sleep", "30"]))
        .unwrap();
    assert_eq!(
        session.backend_ref["pgid"].as_u64().unwrap() as u32,
        session.backend_ref["pid"].as_u64().unwrap() as u32
    );
    backend
        .close(&session, CloseReason::Cancelled, false)
        .unwrap();
    assert!(
        ExecBackend::pid_alive(canary_pid),
        "pid {canary_pid} (argv onlyne-canary) must not die with the session"
    );
    let _ = canary.kill();
    let _ = canary.wait();
}
