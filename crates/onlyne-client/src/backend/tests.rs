use super::command::command_failure;
use super::*;
use anyhow::Result;
use parking_lot::Mutex;
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
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
fn placement_detection_uses_explicit_declared_probe_then_fallback() {
    let cases = [
        (
            env(&[("ONLYNE_BACKEND", "herdr")]),
            None,
            SessionPlacement::Named(onlyne_config::Placement::Herdr),
            SelectionSource::Explicit,
        ),
        (
            env(&[("ONLYNE_BACKEND", "exec")]),
            None,
            SessionPlacement::Named(onlyne_config::Placement::Headless),
            SelectionSource::Explicit,
        ),
        (
            env(&[("ONLYNE_BACKEND", "fake")]),
            None,
            SessionPlacement::Fake,
            SelectionSource::Explicit,
        ),
        (
            env(&[]),
            Some(SessionPlacement::Named(onlyne_config::Placement::Orca)),
            SessionPlacement::Named(onlyne_config::Placement::Orca),
            SelectionSource::Declared,
        ),
        // The declaration is the run's own answer, so it outranks the probe:
        // a machine that could host a pane still runs whatever this run was
        // told to use. The scenario suite relies on it to name the in-process
        // runtime on a host that has a terminal host of its own.
        (
            env(&[("HERDR_ENV", "1"), ("HERDR_SESSION", "s")]),
            Some(SessionPlacement::Fake),
            SessionPlacement::Fake,
            SelectionSource::Declared,
        ),
        // And an explicit `ONLYNE_BACKEND` outranks both.
        (
            env(&[("ONLYNE_BACKEND", "orca")]),
            Some(SessionPlacement::Fake),
            SessionPlacement::Named(onlyne_config::Placement::Orca),
            SelectionSource::Explicit,
        ),
        (
            env(&[("HERDR_ENV", "1"), ("HERDR_SESSION", "s")]),
            None,
            SessionPlacement::Named(onlyne_config::Placement::Herdr),
            SelectionSource::Probe,
        ),
        (
            env(&[("ORCA_PANE_KEY", "tab:leaf")]),
            None,
            SessionPlacement::Named(onlyne_config::Placement::Orca),
            SelectionSource::Probe,
        ),
        (
            env(&[("ZELLIJ", "1")]),
            None,
            SessionPlacement::Named(onlyne_config::Placement::Zellij),
            SelectionSource::Probe,
        ),
        (
            env(&[]),
            None,
            SessionPlacement::Named(onlyne_config::Placement::Headless),
            SelectionSource::Fallback,
        ),
    ];
    for (input, declared, placement, source) in cases {
        let detected = detect_placement(&input, declared).unwrap();
        assert_eq!(detected.placement, placement, "{input:?}");
        assert_eq!(detected.source, source, "{input:?}");
    }
}

#[test]
fn placement_parse_and_unknown_explicit_name_are_precise() {
    for name in [
        "herdr", "orca", "zellij", "headless", "external", "exec", "fake",
    ] {
        assert!(SessionPlacement::parse(name).is_some(), "{name}");
    }
    for name in ["acp", "auto", "garbage"] {
        assert!(SessionPlacement::parse(name).is_none(), "{name}");
    }
    let error = detect_placement(&env(&[("ONLYNE_BACKEND", "garbage")]), None).unwrap_err();
    assert!(error.downcast_ref::<UnknownPlacement>().is_some());
    assert!(error.to_string().contains(onlyne_config::PLACEMENT_NAMES));
}

#[test]
fn backend_matrix_validates_drive_and_placement_pairs() {
    use onlyne_config::{Drive, Placement};
    let runner = Arc::new(ProbeRunner::default());
    let acp = AcpOptions::default();
    let cells = [
        (Drive::Plugin, Placement::Herdr, "herdr"),
        (Drive::Plugin, Placement::Orca, "orca"),
        (Drive::Plugin, Placement::Zellij, "zellij"),
        (Drive::Plugin, Placement::Headless, "exec"),
        (Drive::Plugin, Placement::External, "external"),
        (Drive::Exec, Placement::Herdr, "herdr"),
        (Drive::Exec, Placement::Orca, "orca"),
        (Drive::Exec, Placement::Zellij, "zellij"),
        (Drive::Exec, Placement::Headless, "exec"),
        (Drive::Exec, Placement::External, "exec"),
        (Drive::Acp, Placement::Headless, "acp"),
    ];
    for (drive, placement, name) in cells {
        let backend = backend_for(
            drive,
            SessionPlacement::Named(placement),
            runner.clone(),
            WorktreePolicy::Host,
            &acp,
        )
        .unwrap();
        assert_eq!(backend.name(), name, "{drive:?} x {placement:?}");
    }
    let error = backend_for(
        Drive::Acp,
        SessionPlacement::Named(Placement::Orca),
        runner,
        WorktreePolicy::Host,
        &acp,
    )
    .err()
    .expect("ACP pane pairing must fail");
    assert!(
        error
            .to_string()
            .contains("drive = \"acp\" pairs only with placement = \"headless\"")
    );
}

/// The external placement is the row where the operator starts the runtime, so
/// this backend answers with a session name and runs nothing: the argv belongs
/// to the resident runtime, and a spawn that ran it would fail here on a binary
/// that does not exist. The reference it hands back names the placement the
/// runtime's plugin matches on when it dials the client (`docs/v2-PLAN.md`
/// line 289).
#[test]
fn external_placement_names_a_session_and_starts_no_process() {
    let backend = external::ExternalBackend::new();
    let spec = SpawnSpec {
        cwd: PathBuf::from("/nonexistent/workspace"),
        task_id: "T-external".to_string(),
        command: vec!["/nonexistent/onlyne-no-such-binary".to_string()],
        env: BTreeMap::new(),
        tools_token: String::new(),
        prose: String::new(),
        focus: None,
        placement: None,
        rename: None,
    };
    let session = backend
        .spawn(spec)
        .expect("a resident runtime's session opens with no process of this client's");
    assert_eq!(session.backend, "external");
    assert_eq!(session.backend_ref["id"], "T-external");
    assert_eq!(session.backend_ref["placement"], "external");
    assert!(
        backend.probe(&session).expect("probe").alive,
        "the connection the runtime opened is the resource, and this client holds it"
    );
    assert!(
        backend
            .close(&session, CloseReason::Completed, false)
            .is_ok(),
        "closing stops a process this client never had"
    );
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
        outcome: Some(onlyne_proto::lifecycle::TaskState::Done),
        head: Some("done".into()),
        note: None,
        refusals: None,
    });
    sink.push(SessionOutcome {
        task_id: "t2".into(),
        outcome: Some(onlyne_proto::lifecycle::TaskState::Failed),
        head: None,
        note: Some("agent exited".into()),
        refusals: Some("2 refused".into()),
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
