use onlyne_session::{
    BackendName, CloseReason, CommandOutput, HerdrBackend, PanePlacement, Runner, SelectionSource,
    SessionBackend, SessionRef, SpawnSpec, SplitDirection, detect_host,
};
use parking_lot::Mutex;
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::thread::ThreadId;

#[derive(Clone)]
struct Reply {
    status: i32,
    stdout: String,
    stderr: String,
}

#[derive(Default)]
struct Script {
    calls: Mutex<Vec<(String, Vec<String>)>>,
    replies: Mutex<VecDeque<Reply>>,
}

impl Script {
    fn reply(self, status: i32, stdout: impl Into<String>) -> Self {
        self.replies.lock().push_back(Reply {
            status,
            stdout: stdout.into(),
            stderr: String::new(),
        });
        self
    }

    fn reply_err(self, status: i32, stderr: impl Into<String>) -> Self {
        self.replies.lock().push_back(Reply {
            status,
            stdout: String::new(),
            stderr: stderr.into(),
        });
        self
    }

    fn calls(&self) -> Vec<(String, Vec<String>)> {
        self.calls.lock().clone()
    }

    fn argv(&self) -> Vec<Vec<String>> {
        self.calls().into_iter().map(|(_, args)| args).collect()
    }
}

impl Runner for Script {
    fn run(
        &self,
        program: &str,
        args: &[String],
        _cwd: Option<&Path>,
        _env: &BTreeMap<String, String>,
    ) -> anyhow::Result<CommandOutput> {
        self.calls.lock().push((program.to_string(), args.to_vec()));
        let reply = self.replies.lock().pop_front().unwrap_or_else(|| Reply {
            status: 1,
            stdout: String::new(),
            stderr: format!("unscripted: {program} {}", args.join(" ")),
        });
        Ok(CommandOutput {
            status: reply.status,
            stdout: reply.stdout.into_bytes(),
            stderr: reply.stderr.into_bytes(),
        })
    }
}

fn envelope(result: Value) -> String {
    serde_json::json!({"id": "cli:test", "result": result}).to_string()
}

fn spec(command: Vec<&str>, placement: Option<PanePlacement>) -> SpawnSpec {
    spec_at(command, "/tmp/ws", placement)
}

fn spec_at(command: Vec<&str>, cwd: &str, placement: Option<PanePlacement>) -> SpawnSpec {
    let mut env = BTreeMap::new();
    env.insert("ONLYNE_ROLE".into(), "planner".into());
    env.insert("ONLYNE_CLUSTER".into(), "lab".into());
    env.insert("ONLYNE_TASK_ID".into(), "abcd1234ffff".into());
    SpawnSpec {
        cwd: PathBuf::from(cwd),
        task_id: "abcd1234-ffff-4000-8000-000000000001".into(),
        command: command.into_iter().map(str::to_string).collect(),
        env,
        focus: None,
        placement,
        rename: None,
    }
}

/// The `session_command` shape a generated role workspace carries: the agent
/// binary plus the flags that tie the pane to its client's session.
fn session_command() -> Vec<&'static str> {
    vec!["pi", "--session-id", "s-1", "--session-dir", ".pi/sessions"]
}

/// The spelling of `cwd` the backend owes herdr: absolute, since herdr resolves
/// a relative `--cwd` against its own working directory.
fn absolute_cwd(cwd: &str) -> String {
    let path = Path::new(cwd);
    std::path::absolute(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// The `--cwd <value>` pair, for an argv window check.
fn cwd_arg(cwd: &str) -> Vec<String> {
    vec!["--cwd".into(), absolute_cwd(cwd)]
}

fn session_ref(pane: &str) -> SessionRef {
    session_ref_at(pane, "onlyne-planner-abcd1234", "wF:p1", "right")
}

/// A ref as the backend would have recorded it, with the split anchor and the
/// agent name a real spawn would have written.
fn session_ref_at(pane: &str, agent: &str, base_pane: &str, split_direction: &str) -> SessionRef {
    SessionRef {
        task_id: "abcd1234-ffff-4000-8000-000000000001".into(),
        backend: "herdr".into(),
        backend_ref: serde_json::json!({
            "herdr": {
                "workspace_id": "wF",
                "tab_id": "wF:t1",
                "pane_id": pane,
                "agent": agent,
                "workspace_label": "onlyne:lab",
                "base_pane": base_pane,
                "split_direction": split_direction,
            }
        }),
        generation: 1,
    }
}

fn backend(script: Script) -> (HerdrBackend, Arc<Script>) {
    let script = Arc::new(script);
    let mut env = BTreeMap::new();
    env.insert("HERDR_ENV".into(), "1".into());
    env.insert("HERDR_SESSION".into(), "onlyne-test".into());
    (HerdrBackend::with_env(script.clone(), env), script)
}

fn create_workspace() -> String {
    envelope(serde_json::json!({
        "type": "workspace_created",
        "workspace": {"workspace_id": "wF"},
        "tab": {"tab_id": "wF:t0"},
        "root_pane": {"pane_id": "wF:p0"},
    }))
}

fn create_tab() -> String {
    envelope(serde_json::json!({
        "type": "tab_created",
        "tab": {
            "tab_id": "wF:t1",
            "label": "planner",
            "pane_count": 1,
            "workspace_id": "wF"
        },
        "root_pane": {"pane_id": "wF:p1"},
    }))
}

fn split_pane() -> String {
    envelope(serde_json::json!({
        "type": "pane_info",
        "pane": {
            "pane_id": "wF:p2",
            "tab_id": "wF:t1",
            "workspace_id": "wF",
            "agent_status": "unknown",
            "focused": false
        }
    }))
}

fn agent_started() -> String {
    envelope(serde_json::json!({
        "type": "agent_started",
        "agent": {
            "name": "onlyne-planner-abcd1234",
            "agent_status": "idle",
            "interactive_ready": true,
            "pane_id": "wF:p2"
        }
    }))
}

fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

#[test]
fn spawn_creates_workspace_and_tab_then_splits() {
    let script = Script::default()
        .reply(0, envelope(serde_json::json!({"workspaces": []})))
        .reply(0, create_workspace())
        .reply(0, envelope(serde_json::json!({"tabs": []})))
        .reply(0, create_tab())
        .reply(0, split_pane())
        .reply(0, agent_started());
    let (backend, script) = backend(script);
    let session = backend
        .spawn(spec(
            session_command(),
            Some(PanePlacement {
                direction: SplitDirection::Right,
                ratio: 0.5,
            }),
        ))
        .unwrap();
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
    let argv = script.argv();
    assert_eq!(argv[0], vec!["workspace", "list"]);
    assert_eq!(
        argv[1],
        vec![
            "workspace",
            "create",
            "--label",
            "onlyne:lab",
            "--cwd",
            absolute_cwd("/tmp/ws").as_str(),
            "--no-focus",
        ]
    );
    assert_eq!(argv[2], vec!["tab", "list", "--workspace", "wF"]);
    assert_eq!(
        argv[3],
        vec![
            "tab",
            "create",
            "--workspace",
            "wF",
            "--label",
            "planner",
            "--cwd",
            absolute_cwd("/tmp/ws").as_str(),
            "--no-focus",
        ]
    );
    let split = &argv[4];
    assert_eq!(split[0], "pane");
    assert_eq!(split[1], "split");
    assert!(split.windows(2).any(|w| w == ["--pane", "wF:p1"]));
    assert!(split.windows(2).any(|w| w == ["--direction", "right"]));
    assert!(split.windows(2).any(|w| w == ["--ratio", "0.5"]));
    assert!(split.windows(2).any(|w| w == cwd_arg("/tmp/ws")));
    assert!(split.iter().any(|arg| arg == "--no-focus"));
    assert!(
        split
            .windows(2)
            .any(|w| w[0] == "--env" && w[1] == "ONLYNE_CLUSTER=lab")
    );
    assert!(
        split
            .windows(2)
            .any(|w| w[0] == "--env" && w[1] == "ONLYNE_ROLE=planner")
    );
    // herdr's usage is `agent start <NAME> --kind <KIND> --pane <ID> [OPTIONS]
    // [-- [AGENT_ARG]...]`, and the agent arguments belong to the pane: the
    // session id and session directory are how the agent finds its client.
    assert_eq!(
        argv[5],
        vec![
            "agent",
            "start",
            "onlyne-planner-abcd1234",
            "--kind",
            "pi",
            "--pane",
            "wF:p2",
            "--timeout",
            "25000",
            "--",
            "--session-id",
            "s-1",
            "--session-dir",
            ".pi/sessions",
        ]
    );
}

#[test]
fn spawn_sends_no_separator_for_a_bare_agent_command() {
    let script = Script::default()
        .reply(0, envelope(serde_json::json!({"workspaces": []})))
        .reply(0, create_workspace())
        .reply(0, envelope(serde_json::json!({"tabs": []})))
        .reply(0, create_tab())
        .reply(0, split_pane())
        .reply(0, agent_started());
    let (backend, script) = backend(script);
    backend
        .spawn(spec(
            vec!["pi"],
            Some(PanePlacement {
                direction: SplitDirection::Right,
                ratio: 0.5,
            }),
        ))
        .unwrap();
    let started = script
        .argv()
        .into_iter()
        .find(|args| args.first().map(String::as_str) == Some("agent"))
        .unwrap();
    assert_eq!(
        started,
        vec![
            "agent",
            "start",
            "onlyne-planner-abcd1234",
            "--kind",
            "pi",
            "--pane",
            "wF:p2",
            "--timeout",
            "25000",
        ]
    );
}

#[test]
fn spawn_sends_an_absolute_cwd_for_a_relative_workspace() {
    // The first real run of a formal-research tree had `onlyne-client run
    // --workspace ws/formal/...`, and herdr resolved that against its own
    // working directory, so every session pane opened in `$HOME`.
    let script = Script::default()
        .reply(0, envelope(serde_json::json!({"workspaces": []})))
        .reply(0, create_workspace())
        .reply(0, envelope(serde_json::json!({"tabs": []})))
        .reply(0, create_tab())
        .reply(0, split_pane())
        .reply(0, agent_started());
    let (backend, script) = backend(script);
    backend
        .spawn(spec_at(
            session_command(),
            "ws/formal/research/planner",
            None,
        ))
        .unwrap();
    let cwd = absolute_cwd("ws/formal/research/planner");
    assert!(Path::new(&cwd).is_absolute(), "{cwd}");
    let argv = script.argv();
    let pair = cwd_arg("ws/formal/research/planner");
    for (group, verb) in [
        ("workspace", "create"),
        ("tab", "create"),
        ("pane", "split"),
    ] {
        let args = argv
            .iter()
            .find(|args| {
                args.first().map(String::as_str) == Some(group)
                    && args.get(1).map(String::as_str) == Some(verb)
            })
            .unwrap_or_else(|| panic!("no `herdr {group} {verb}` in {argv:?}"));
        assert!(args.windows(2).any(|w| w == pair), "{args:?}");
    }
}

#[test]
fn spawn_reuses_workspace_and_tab_and_plans_from_pane_count() {
    let script = Script::default()
        .reply(
            0,
            envelope(serde_json::json!({
                "workspaces": [{
                    "workspace_id": "wF",
                    "label": "onlyne:lab"
                }]
            })),
        )
        .reply(
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
        .reply(0, split_pane())
        .reply(0, agent_started());
    let (backend, script) = backend(script);
    backend.spawn(spec(vec!["pi"], None)).unwrap();
    let argv = script.argv();
    assert_eq!(argv[0], vec!["workspace", "list"]);
    assert_eq!(argv[1], vec!["tab", "list", "--workspace", "wF"]);
    assert_eq!(argv[2], vec!["pane", "list", "--workspace", "wF"]);
    let split = &argv[3];
    assert!(
        split.windows(2).any(|w| w == ["--direction", "down"]),
        "{split:?}"
    );
    assert!(split.windows(2).any(|w| w == ["--ratio", "0.5"]));
    assert!(argv.iter().all(|args| {
        args.first().map(String::as_str) != Some("workspace")
            || args.get(1).map(String::as_str) != Some("create")
    }));
    assert!(argv.iter().all(|args| {
        args.first().map(String::as_str) != Some("tab")
            || args.get(1).map(String::as_str) != Some("create")
    }));
}

#[test]
fn spawn_falls_back_to_pane_run_for_unknown_kind() {
    let script = Script::default()
        .reply(0, envelope(serde_json::json!({"workspaces": []})))
        .reply(0, create_workspace())
        .reply(0, envelope(serde_json::json!({"tabs": []})))
        .reply(0, create_tab())
        .reply(0, split_pane())
        .reply(0, String::new());
    let (backend, script) = backend(script);
    let session = backend
        .spawn(spec(
            vec!["echo", "hello world", "--session-id", "s-1"],
            Some(PanePlacement {
                direction: SplitDirection::Right,
                ratio: 0.5,
            }),
        ))
        .unwrap();
    let argv = script.argv();
    // The ref has to carry the anchor and record no agent: focus reads both to
    // choose the anchor hop over `agent focus`, which answers `agent_not_found`
    // for a shell pane.
    assert_eq!(session.backend_ref["herdr"]["agent"], "");
    assert_eq!(session.backend_ref["herdr"]["base_pane"], "wF:p1");
    assert_eq!(session.backend_ref["herdr"]["split_direction"], "right");
    assert!(
        argv.iter()
            .all(|args| args.first().map(String::as_str) != Some("agent"))
    );
    let run = argv
        .iter()
        .find(|args| args.get(1).map(String::as_str) == Some("run"))
        .unwrap();
    assert_eq!(run[0], "pane");
    assert_eq!(run[2], "wF:p2");
    #[cfg(unix)]
    assert_eq!(run[3], "'echo' 'hello world' '--session-id' 's-1'");
    #[cfg(windows)]
    assert_eq!(run[3], "\"echo\" \"hello world\" \"--session-id\" \"s-1\"");
    assert_eq!(run.len(), 4);
}

#[test]
fn spawn_falls_back_to_pane_run_when_agent_start_fails() {
    let script = Script::default()
        .reply(0, envelope(serde_json::json!({"workspaces": []})))
        .reply(0, create_workspace())
        .reply(0, envelope(serde_json::json!({"tabs": []})))
        .reply(0, create_tab())
        .reply(0, split_pane())
        .reply(
            1,
            r#"{"id":"cli:agent:start","error":{"message":"not ready"}}"#,
        )
        .reply(0, String::new());
    let (backend, script) = backend(script);
    backend
        .spawn(spec(
            session_command(),
            Some(PanePlacement {
                direction: SplitDirection::Right,
                ratio: 0.5,
            }),
        ))
        .unwrap();
    let argv = script.argv();
    let started = argv
        .iter()
        .find(|args| args.get(1).map(String::as_str) == Some("start"))
        .unwrap();
    assert!(
        started.ends_with(&[
            "--".to_string(),
            "--session-id".into(),
            "s-1".into(),
            "--session-dir".into(),
            ".pi/sessions".into(),
        ]),
        "{started:?}"
    );
    let run = argv
        .iter()
        .find(|args| args.get(1).map(String::as_str) == Some("run"))
        .unwrap();
    assert_eq!(run[0], "pane");
    assert_eq!(run[2], "wF:p2");
    // The fallback shell line carries the same arguments, so a pane herdr
    // refuses to manage still reaches its client.
    #[cfg(unix)]
    assert_eq!(
        run[3],
        "'pi' '--session-id' 's-1' '--session-dir' '.pi/sessions'"
    );
    #[cfg(windows)]
    assert_eq!(
        run[3],
        "\"pi\" \"--session-id\" \"s-1\" \"--session-dir\" \".pi/sessions\""
    );
}

#[test]
fn workspace_create_warns_with_the_rename_remedy() {
    // The lookup keys on the label alone, so an operator working in a workspace
    // under another label gets a fresh sibling. The warning is the record that
    // explains the extra workspace, and it carries the rename that ends it.
    let warns = warn_capture();
    let script = Script::default()
        .reply(0, envelope(serde_json::json!({"workspaces": []})))
        .reply(0, create_workspace())
        .reply(0, envelope(serde_json::json!({"tabs": []})))
        .reply(0, create_tab())
        .reply(0, split_pane())
        .reply(0, agent_started());
    let (backend, script) = backend(script);
    let session = backend
        .spawn(spec(
            session_command(),
            Some(PanePlacement {
                direction: SplitDirection::Right,
                ratio: 0.5,
            }),
        ))
        .unwrap();
    assert_eq!(session.backend_ref["herdr"]["workspace_id"], "wF");
    assert_eq!(
        session.backend_ref["herdr"]["workspace_label"],
        "onlyne:lab"
    );
    let create = script
        .argv()
        .into_iter()
        .find(|args| {
            args.first().map(String::as_str) == Some("workspace")
                && args.get(1).map(String::as_str) == Some("create")
        })
        .unwrap();
    assert!(create.windows(2).any(|w| w == ["--label", "onlyne:lab"]));
    let line = warns
        .lines()
        .into_iter()
        .find(|line| line.contains("workspace rename"))
        .unwrap_or_else(|| panic!("no rename remedy in the warnings: {warns:?}"));
    assert!(line.contains(r#"label = "onlyne:lab""#), "{line}");
    assert!(line.contains(r#"workspace_id = "wF""#), "{line}");
    assert!(
        line.contains("herdr workspace rename <WORKSPACE_ID> onlyne:lab"),
        "{line}"
    );
}

#[test]
fn a_found_workspace_emits_no_rename_remedy() {
    // The labelled workspace is the ordinary case after the first spawn, and a
    // warning on every one of them would bury the signal.
    let warns = warn_capture();
    let script = Script::default()
        .reply(
            0,
            envelope(serde_json::json!({
                "workspaces": [{
                    "workspace_id": "wF",
                    "label": "onlyne:lab"
                }]
            })),
        )
        .reply(
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
        .reply(0, split_pane())
        .reply(0, agent_started());
    let (backend, script) = backend(script);
    backend.spawn(spec(session_command(), None)).unwrap();
    assert!(
        warns
            .lines()
            .iter()
            .all(|line| !line.contains("workspace rename")),
        "{:?}",
        warns.lines()
    );
    assert!(
        script
            .argv()
            .iter()
            .all(|args| !(args[0] == "workspace" && args[1] == "create")),
        "{:?}",
        script.argv()
    );
}

/// Collects the warn-level messages one call emits, so a test can read an
/// operator-facing remedy the way the log does.
#[derive(Clone, Debug, Default)]
struct Warns(Arc<Mutex<Vec<(ThreadId, String)>>>);

impl Warns {
    fn lines(&self) -> Vec<String> {
        let here = std::thread::current().id();
        self.0
            .lock()
            .iter()
            .filter(|(thread, _)| *thread == here)
            .map(|(_, line)| line.clone())
            .collect()
    }
}

/// Installs the collector as this binary's single global subscriber, once.
fn warn_capture() -> &'static Warns {
    static INSTALLED: LazyLock<Warns> = LazyLock::new(|| {
        let warns = Warns::default();
        tracing::subscriber::set_global_default(warns.clone())
            .expect("no other global subscriber takes the warn calls in this binary");
        warns
    });
    &INSTALLED
}

struct WarnRecord(Vec<String>);

impl tracing::field::Visit for WarnRecord {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.push(format!("{} = {:?}", field.name(), value));
    }
}

/// Spans are outside what these tests read, so the span half of the trait
/// stays inert and only `enabled` plus `event` carry meaning.
impl tracing::Subscriber for Warns {
    fn enabled(&self, meta: &tracing::Metadata<'_>) -> bool {
        meta.level() <= &tracing::Level::WARN
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let mut record = WarnRecord(Vec::new());
        event.record(&mut record);
        self.0
            .lock()
            .push((std::thread::current().id(), record.0.join(", ")));
    }

    fn enter(&self, _span: &tracing::span::Id) {}

    fn exit(&self, _span: &tracing::span::Id) {}
}

#[test]
fn probe_maps_pane_get_json_and_alive() {
    let script = Script::default()
        .reply(
            0,
            envelope(serde_json::json!({
                "type": "pane_info",
                "pane": {
                    "pane_id": "wF:p2",
                    "agent_status": "idle",
                    "focused": false
                }
            })),
        )
        .reply(
            0,
            envelope(serde_json::json!({
                "process_info": {"pane_id": "wF:p2", "shell_pid": 9}
            })),
        );
    let (backend, script) = backend(script);
    let probe = backend.probe(&session_ref("wF:p2")).unwrap();
    assert!(probe.alive);
    assert!(probe.attached);
    assert_eq!(probe.detail.unwrap()["agent_status"], "idle");
    assert_eq!(
        script.argv(),
        vec![
            vec!["pane", "get", "wF:p2"],
            vec!["pane", "process-info", "--pane", "wF:p2"],
        ]
    );
}

#[test]
fn probe_missing_pane_is_dead() {
    let script = Script::default().reply(1, r#"{"id":"cli:pane:get"}"#);
    let (backend, _) = backend(script);
    let probe = backend.probe(&session_ref("wF:p9")).unwrap();
    assert!(!probe.alive);
    assert!(!probe.attached);
}

#[test]
fn close_sends_pane_close() {
    let script = Script::default().reply(0, envelope(serde_json::json!({"type": "ok"})));
    let (backend, script) = backend(script);
    backend
        .close(&session_ref("wF:p2"), CloseReason::Completed, true)
        .unwrap();
    assert_eq!(script.argv(), vec![vec!["pane", "close", "wF:p2"]]);
}

#[test]
fn herdr_pane_close_is_idempotent_only_for_a_missing_pane() {
    let missing = Script::default().reply_err(
        1,
        r#"{"error":{"code":"pane_not_found","message":"pane wF:p2 not found"},"id":"cli:pane:close"}"#,
    );
    let (missing_backend, _) = backend(missing);
    missing_backend
        .close(&session_ref("wF:p2"), CloseReason::Completed, false)
        .expect("a missing pane already satisfies close");

    let refused = Script::default().reply_err(
        1,
        r#"{"error":{"code":"session_unavailable","message":"session is unavailable"},"id":"cli:pane:close"}"#,
    );
    let (backend, _) = backend(refused);
    let error = backend
        .close(&session_ref("wF:p2"), CloseReason::Completed, false)
        .expect_err("a different close failure remains visible");
    assert!(error.to_string().contains("session_unavailable"), "{error}");
}

#[test]
fn focus_walks_workspace_tab_then_agent() {
    let script = Script::default()
        .reply(
            0,
            envelope(serde_json::json!({
                "panes": [{
                    "pane_id": "w1:p1",
                    "tab_id": "w1:t1",
                    "workspace_id": "w1",
                    "focused": true
                }]
            })),
        )
        .reply(0, envelope(serde_json::json!({"type": "workspace_info"})))
        .reply(0, envelope(serde_json::json!({"type": "tab_info"})))
        .reply(0, envelope(serde_json::json!({"type": "agent_info"})))
        .reply(
            0,
            envelope(serde_json::json!({
                "type": "pane_info",
                "pane": {"pane_id": "wF:p2", "focused": true}
            })),
        );
    let (backend, script) = backend(script);
    backend.focus(&session_ref("wF:p2")).unwrap();
    assert_eq!(
        script.argv(),
        vec![
            vec!["pane", "list"],
            vec!["workspace", "focus", "wF"],
            vec!["tab", "focus", "wF:t1"],
            vec!["agent", "focus", "wF:p2"],
            vec!["pane", "get", "wF:p2"],
        ]
    );
}

#[test]
fn focus_navigates_the_anchor_for_a_shell_pane() {
    // A `pane run` session has no managed agent, so `agent focus` answers
    // `agent_not_found` there. The recorded anchor and direction reach the pane
    // through the neighbour hop herdr does accept.
    let script = Script::default()
        .reply(
            0,
            envelope(serde_json::json!({
                "panes": [{
                    "pane_id": "w1:p1",
                    "tab_id": "w1:t1",
                    "workspace_id": "w1",
                    "focused": true
                }]
            })),
        )
        .reply(0, envelope(serde_json::json!({"type": "workspace_info"})))
        .reply(0, envelope(serde_json::json!({"type": "tab_info"})))
        .reply(0, envelope(serde_json::json!({"type": "ok"})))
        .reply(
            0,
            envelope(serde_json::json!({
                "type": "pane_info",
                "pane": {"pane_id": "wF:p2", "focused": true}
            })),
        );
    let (backend, script) = backend(script);
    let reference = session_ref_at("wF:p2", "", "wF:p1", "right");
    backend.focus(&reference).unwrap();
    assert_eq!(
        script.argv(),
        vec![
            vec!["pane", "list"],
            vec!["workspace", "focus", "wF"],
            vec!["tab", "focus", "wF:t1"],
            vec!["pane", "focus", "--pane", "wF:p1", "--direction", "right"],
            vec!["pane", "get", "wF:p2"],
        ]
    );
}

#[test]
fn focus_falls_back_to_the_anchor_when_the_agent_is_gone() {
    let script = Script::default()
        .reply(
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
        .reply_err(
            1,
            r#"{"error":{"code":"agent_not_found","message":"agent target wF:p2 not found"},"id":"cli:agent:focus"}"#,
        )
        .reply(0, envelope(serde_json::json!({"type": "ok"})))
        .reply(
            0,
            envelope(serde_json::json!({
                "type": "pane_info",
                "pane": {"pane_id": "wF:p2", "focused": true}
            })),
        );
    let (backend, script) = backend(script);
    backend.focus(&session_ref("wF:p2")).unwrap();
    assert_eq!(
        script.argv(),
        vec![
            vec!["pane", "list"],
            vec!["agent", "focus", "wF:p2"],
            vec!["pane", "focus", "--pane", "wF:p1", "--direction", "right"],
            vec!["pane", "get", "wF:p2"],
        ]
    );
}

#[test]
fn focus_reports_a_failure_when_the_hop_lands_elsewhere() {
    // An operator can close or move panes, and then the neighbour hop lands on a
    // different pane. Reporting that beats leaving focus on another session.
    let script = Script::default()
        .reply(
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
        .reply(0, envelope(serde_json::json!({"type": "ok"})))
        .reply(
            0,
            envelope(serde_json::json!({
                "type": "pane_info",
                "pane": {"pane_id": "wF:p2", "focused": false}
            })),
        )
        .reply(
            0,
            envelope(serde_json::json!({
                "panes": [{
                    "pane_id": "wF:p8",
                    "tab_id": "wF:t1",
                    "workspace_id": "wF",
                    "focused": true
                }]
            })),
        );
    let (backend, _) = backend(script);
    let error = backend
        .focus(&session_ref_at("wF:p2", "", "wF:p1", "right"))
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("herdr left pane wF:p2 unfocused (focused pane wF:p8)"),
        "{error}"
    );
}

#[test]
fn unknown_option_stderr_is_surfaced() {
    let script = Script::default().reply_err(1, "unknown option: --bogus");
    let (backend, _) = backend(script);
    let error = backend
        .spawn(spec(
            vec!["pi"],
            Some(PanePlacement {
                direction: SplitDirection::Right,
                ratio: 0.5,
            }),
        ))
        .unwrap_err();
    assert!(
        error.to_string().contains("unknown option: --bogus"),
        "{error}"
    );
}

#[test]
fn detect_host_covers_herdr_orca_zellij_none_and_explicit() {
    let herdr = detect_host(&env(&[
        ("HERDR_ENV", "1"),
        ("HERDR_SOCKET_PATH", "/tmp/herdr.sock"),
    ]));
    assert_eq!(herdr.backend, Some(BackendName::Herdr));
    assert_eq!(herdr.source, SelectionSource::Env);

    let orca = detect_host(&env(&[("ORCA_PANE_KEY", "tab:leaf")]));
    assert_eq!(orca.backend, Some(BackendName::Orca));
    assert_eq!(orca.source, SelectionSource::Env);

    let explicit = detect_host(&env(&[("ONLYNE_BACKEND", "fake")]));
    assert_eq!(explicit.backend, Some(BackendName::Fake));
    assert_eq!(explicit.source, SelectionSource::Explicit);
    assert_eq!(explicit.explicit.as_deref(), Some("fake"));
}
