use super::command::command_failure;
use super::*;
use anyhow::Result;
use parking_lot::Mutex;
use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// Answers one scripted `(status, stdout)` per call; a call with no answer
/// fails the way an absent CLI does.
#[derive(Default)]
struct ProbeRunner {
    calls: Mutex<Vec<(String, Vec<String>)>>,
    script: Mutex<VecDeque<(i32, String)>>,
}

impl ProbeRunner {
    fn reply(self, status: i32, body: &str) -> Self {
        self.script.lock().push_back((status, body.to_string()));
        self
    }

    fn calls(&self) -> Vec<(String, Vec<String>)> {
        self.calls.lock().clone()
    }
}

impl Runner for ProbeRunner {
    fn run(
        &self,
        program: &str,
        args: &[String],
        _: Option<&Path>,
        _: &BTreeMap<String, String>,
    ) -> Result<CommandOutput> {
        self.calls.lock().push((program.to_owned(), args.to_vec()));
        let (status, stdout) = self
            .script
            .lock()
            .pop_front()
            .unwrap_or_else(|| (1, String::new()));
        Ok(CommandOutput {
            status,
            stdout: stdout.into_bytes(),
            stderr: Vec::new(),
        })
    }
}

fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

#[test]
fn detect_host_table_covers_explicit_env_and_none() {
    let cases = [
        (
            env(&[("ONLYNE_BACKEND", "herdr")]),
            Some(BackendName::Herdr),
            SelectionSource::Explicit,
        ),
        (
            env(&[("ONLYNE_BACKEND", "fake")]),
            Some(BackendName::Fake),
            SelectionSource::Explicit,
        ),
        (
            env(&[("ONLYNE_BACKEND", "ACP")]),
            Some(BackendName::Acp),
            SelectionSource::Explicit,
        ),
        (
            env(&[
                ("HERDR_ENV", "1"),
                ("HERDR_SESSION", "onlyne-test"),
                ("ORCA_PANE_KEY", "tab:leaf"),
            ]),
            Some(BackendName::Herdr),
            SelectionSource::Env,
        ),
        (
            env(&[("ORCA_WORKTREE_ID", "wt-1")]),
            Some(BackendName::Orca),
            SelectionSource::Env,
        ),
        (
            env(&[("ZELLIJ", "0")]),
            Some(BackendName::Zellij),
            SelectionSource::Env,
        ),
        (env(&[]), None, SelectionSource::None),
        (env(&[("HERDR_ENV", "1")]), None, SelectionSource::None),
        (
            env(&[("ONLYNE_BACKEND", "auto"), ("ZELLIJ", "1")]),
            Some(BackendName::Zellij),
            SelectionSource::Env,
        ),
    ];
    for (input, backend, source) in cases {
        let detected = detect_host(&input);
        assert_eq!(detected.backend, backend, "{input:?}");
        assert_eq!(detected.source, source, "{input:?}");
    }
}

#[test]
fn empty_env_refuses_with_no_supported_host() {
    let runner = Arc::new(ProbeRunner::default());
    let error = select_backend_from_env(
        &BTreeMap::new(),
        runner,
        WorktreePolicy::Host,
        &AcpOptions::default(),
    )
    .err()
    .expect("empty env must refuse");
    assert!(error.downcast_ref::<NoSupportedHost>().is_some());
    assert_eq!(error.to_string(), NO_SUPPORTED_HOST);
}

#[test]
fn named_backends_stay_exact_and_unknown_names_error() {
    let runner = Arc::new(ProbeRunner::default());
    assert_eq!(
        backend_by_name(
            "herdr",
            runner.clone(),
            WorktreePolicy::Host,
            &AcpOptions::default()
        )
        .unwrap()
        .name(),
        "herdr"
    );
    assert_eq!(
        backend_for_env(
            "zellij",
            &BTreeMap::new(),
            runner.clone(),
            WorktreePolicy::Host,
            &AcpOptions::default()
        )
        .unwrap()
        .name(),
        "zellij"
    );
    assert_eq!(
        backend_for_env(
            "fake",
            &BTreeMap::new(),
            runner.clone(),
            WorktreePolicy::Host,
            &AcpOptions::default()
        )
        .unwrap()
        .name(),
        "fake"
    );
    assert_eq!(
        backend_for_env(
            "exec",
            &BTreeMap::new(),
            runner.clone(),
            WorktreePolicy::Host,
            &AcpOptions::default()
        )
        .unwrap()
        .name(),
        "exec"
    );
    assert_eq!(runner.calls().len(), 0);
    let error = backend_for_env(
        "nope",
        &BTreeMap::new(),
        runner,
        WorktreePolicy::Host,
        &AcpOptions::default(),
    )
    .err()
    .expect("unknown name must error");
    assert_eq!(
        error.to_string(),
        format!("unknown session backend: nope; accepted: {BACKEND_NAMES}")
    );
}

#[test]
fn headless_is_the_exec_alias() {
    assert_eq!(BackendName::parse("headless"), Some(BackendName::Exec));
    assert_eq!(BackendName::parse("HEADLESS"), Some(BackendName::Exec));
    assert_eq!(BackendName::parse("exec"), Some(BackendName::Exec));
    assert_eq!(BackendName::Exec.as_str(), "exec");
    assert_eq!(BackendName::parse("nope"), None);

    let detected = detect_host(&env(&[("ONLYNE_BACKEND", "headless")]));
    assert_eq!(detected.backend, Some(BackendName::Exec));
    assert_eq!(detected.source, SelectionSource::Explicit);
    assert_eq!(detected.backend.unwrap().as_str(), "exec");

    let runner = Arc::new(ProbeRunner::default());
    assert_eq!(
        backend_by_name(
            "headless",
            runner.clone(),
            WorktreePolicy::Host,
            &AcpOptions::default()
        )
        .unwrap()
        .name(),
        "exec"
    );
}

#[test]
fn env_probe_picks_herdr_before_orca() {
    let runner = Arc::new(ProbeRunner::default());
    let backend = select_backend_from_env(
        &env(&[
            ("HERDR_ENV", "1"),
            ("HERDR_SOCKET_PATH", "/tmp/herdr.sock"),
            ("ORCA_PANE_KEY", "tab:leaf"),
            ("ZELLIJ", "0"),
        ]),
        runner,
        WorktreePolicy::Host,
        &AcpOptions::default(),
    )
    .unwrap();
    assert_eq!(backend.name(), "herdr");
}

/// `acp` is a name like any other: selectable by name and by
/// `ONLYNE_BACKEND`, and never a host that auto-discovery can stumble into.
#[test]
fn acp_is_named_but_never_discovered() {
    let runner = Arc::new(ProbeRunner::default());
    assert_eq!(
        backend_by_name(
            "acp",
            runner.clone(),
            WorktreePolicy::Host,
            &AcpOptions::default()
        )
        .unwrap()
        .name(),
        "acp"
    );
    let explicit = select_backend_from_env(
        &env(&[("ONLYNE_BACKEND", "acp"), ("ZELLIJ", "1")]),
        runner.clone(),
        WorktreePolicy::Host,
        &AcpOptions::default(),
    )
    .unwrap();
    assert_eq!(explicit.name(), "acp");
    // An auto probe in a zellij terminal must still pick zellij, not acp.
    let auto = select_backend_from_env(
        &env(&[("ONLYNE_BACKEND", "auto"), ("ZELLIJ", "1")]),
        runner,
        WorktreePolicy::Host,
        &AcpOptions::default(),
    )
    .unwrap();
    assert_eq!(auto.name(), "zellij");
}

/// A backend outlives its first drain, and any number of views may pull from
/// one stream: each fact is taken once and only once.
#[test]
fn an_outcome_stream_hands_each_fact_to_one_consumer() {
    let (sink, feed) = OutcomeFeed::channel();
    let second = feed.clone();
    assert!(feed.try_recv().is_none());
    sink.push(SessionOutcome {
        task_id: "t1".into(),
        outcome: crate::lifecycle::TaskState::Done,
        head: Some("done".into()),
        head_kind: Some("done".into()),
        note: None,
        refusals: None,
        handoffs: Vec::new(),
    });
    sink.push(SessionOutcome {
        task_id: "t2".into(),
        outcome: crate::lifecycle::TaskState::Failed,
        head: None,
        head_kind: None,
        note: Some("agent exited".into()),
        refusals: Some("2 refused".into()),
        handoffs: Vec::new(),
    });
    assert_eq!(
        feed.recv_timeout(Duration::from_millis(10))
            .unwrap()
            .task_id,
        "t1"
    );
    assert_eq!(second.try_recv().unwrap().task_id, "t2");
    assert!(feed.try_recv().is_none());
    assert!(feed.recv_timeout(Duration::from_millis(5)).is_none());
}

#[test]
fn placement_from_pane_count_matches_required_inputs() {
    let expected = [
        (0, SplitDirection::Right),
        (1, SplitDirection::Right),
        (2, SplitDirection::Down),
        (3, SplitDirection::Right),
        (4, SplitDirection::Down),
        (5, SplitDirection::Down),
    ];
    for (count, direction) in expected {
        let placement = PanePlacement::from_pane_count(count);
        assert_eq!(placement.direction, direction, "count {count}");
        assert_eq!(placement.ratio, 0.5);
    }
}

/// Orca is the only backend that reads the policy, and the JSON error body
/// is where its CLI puts the reason for a refusal.
#[test]
fn run_json_unwraps_result_and_names_a_refusal() {
    let cli = Arc::new(
        ProbeRunner::default()
            .reply(0, r#"{"ok":true,"result":{"terminal":{"handle":"term_1"}}}"#)
            .reply(
                1,
                r#"{"ok":false,"error":{"code":"terminal_handle_stale","message":"handle is stale"}}"#,
            )
            .reply(1, r#"{"ok":false,"error":{"code":"selector_not_found"}}"#),
    );
    let args = ["terminal".to_string(), "show".to_string()];
    let value = run_json(cli.as_ref(), "orca", &args, None, &BTreeMap::new()).unwrap();
    assert_eq!(value.pointer("/terminal/handle").unwrap(), "term_1");

    let error = run_json(cli.as_ref(), "orca", &args, None, &BTreeMap::new()).unwrap_err();
    let failure = error.downcast_ref::<CommandFailure>().unwrap();
    assert_eq!(failure.code(), Some("terminal_handle_stale"));
    assert_eq!(
        error.to_string(),
        "runtime command failed: orca terminal show (status 1, terminal_handle_stale): handle is stale"
    );

    // A body without a message still names the code it failed on.
    let error = run_json(cli.as_ref(), "orca", &args, None, &BTreeMap::new()).unwrap_err();
    assert_eq!(
        error.downcast_ref::<CommandFailure>().unwrap().code(),
        Some("selector_not_found")
    );
}

/// Herdr answers a refusal with a JSON document on stderr and an empty
/// stdout, so the code the backends branch on has to come from there.
#[test]
fn command_failure_reads_a_code_from_the_stderr_document() {
    let output = CommandOutput {
        status: 1,
        stdout: Vec::new(),
        stderr: br#"{"error":{"code":"agent_not_found","message":"agent target wF:p2 not found"},"id":"cli:agent:focus"}"#.to_vec(),
    };
    let failure = command_failure("herdr agent focus wF:p2", &output, None);
    assert_eq!(failure.code(), Some("agent_not_found"));
    assert_eq!(
        failure.to_string(),
        "runtime command failed: herdr agent focus wF:p2 (status 1, agent_not_found): agent target wF:p2 not found"
    );
}

/// A backend that answers in plain text keeps that text as the message, and
/// carries no code.
#[test]
fn command_failure_keeps_plain_stderr_text() {
    let output = CommandOutput {
        status: 2,
        stdout: Vec::new(),
        stderr: b"unknown option: --bogus".to_vec(),
    };
    let failure = command_failure("herdr pane split", &output, None);
    assert_eq!(failure.code(), None);
    assert_eq!(
        failure.to_string(),
        "runtime command failed: herdr pane split (status 2): unknown option: --bogus"
    );
}
