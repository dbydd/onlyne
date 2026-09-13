//! Opt-in live herdr probe. Touches only the `onlyne-test` session.

use onlyne_session::{
    CloseReason, CommandOutput, HerdrBackend, ProcessRunner, Runner, SessionBackend, SessionRef,
    SpawnSpec, process_env,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const RESERVED_WORKSPACE_IDS: &[&str] = &["w1", "w2"];
const RESERVED_WORKSPACE_LABELS: &[&str] = &["onlyne", "onlyne:probe"];

struct LoggingRunner {
    inner: ProcessRunner,
    calls: Mutex<Vec<(String, Vec<String>)>>,
}

impl LoggingRunner {
    fn new() -> Self {
        Self {
            inner: ProcessRunner,
            calls: Mutex::new(Vec::new()),
        }
    }

    fn snapshot(&self) -> Vec<(String, Vec<String>)> {
        self.calls
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }
}

impl Runner for LoggingRunner {
    fn run(
        &self,
        program: &str,
        args: &[String],
        cwd: Option<&Path>,
        env: &BTreeMap<String, String>,
    ) -> anyhow::Result<CommandOutput> {
        println!("herdr_live argv: program={program} args={args:?}");
        self.calls
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push((program.to_string(), args.to_vec()));
        self.inner.run(program, args, cwd, env)
    }
}

struct Cleanup<'a> {
    backend: &'a HerdrBackend,
    runner: &'a LoggingRunner,
    cli_env: &'a BTreeMap<String, String>,
    session: Option<SessionRef>,
    workspace_label: String,
    done: bool,
}

impl Cleanup<'_> {
    fn close_pane(&mut self) {
        if let Some(session) = self.session.take() {
            match self.backend.close(&session, CloseReason::Operator, false) {
                Ok(()) => println!("cleanup: pane close ok task={}", session.task_id),
                Err(error) => println!("cleanup: pane close error: {error:#}"),
            }
        }
    }

    fn close_workspace(&mut self) {
        if self.done {
            return;
        }
        self.done = true;
        close_workspace_by_label(self.runner, self.cli_env, &self.workspace_label);
    }

    fn close_now(&mut self) {
        self.close_pane();
        self.close_workspace();
    }
}

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        self.close_now();
    }
}

#[test]
#[ignore = "needs a live herdr session: HERDR_LIVE_PROBE=1"]
fn herdr_live_probe() {
    if std::env::var("HERDR_LIVE_PROBE").as_deref() != Ok("1") {
        println!("herdr_live_probe: skip, HERDR_LIVE_PROBE is not 1");
        return;
    }
    match std::env::var("HERDR_SESSION") {
        Ok(session) if session == "onlyne-test" => {}
        other => {
            println!(
                "herdr_live_probe: skip, HERDR_SESSION={other:?} (only onlyne-test is allowed)"
            );
            return;
        }
    }

    let mut host_env = process_env();
    host_env.insert("HERDR_ENV".into(), "1".into());
    let cli_env: BTreeMap<String, String> = host_env
        .iter()
        .filter(|(key, _)| key.starts_with("HERDR_"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    println!(
        "herdr_live_probe: HERDR_BIN_PATH={:?} HERDR_SESSION={:?} HERDR_ENV={:?}",
        host_env.get("HERDR_BIN_PATH"),
        host_env.get("HERDR_SESSION"),
        host_env.get("HERDR_ENV")
    );

    let runner = Arc::new(LoggingRunner::new());
    let backend = HerdrBackend::with_env(runner.clone(), host_env);
    let available = backend.available().expect("available()");
    println!("assert available() == true -> {available}");
    assert!(
        available,
        "available() must be true with HERDR_ENV=1 and HERDR_SESSION=onlyne-test"
    );

    let tmp = tempfile::tempdir().expect("tempdir");
    let cwd = tmp.path().to_path_buf();
    println!("probe cwd={}", cwd.display());

    let sleep_res = sleep_track(&backend, runner.as_ref(), &cli_env, &cwd);
    let pi_res = pi_track(&backend, runner.as_ref(), &cli_env, &cwd);

    println!("--- final pane list ---");
    let pane_list = herdr_json(runner.as_ref(), &cli_env, &["pane", "list"]);
    println!("{}", summarize_panes(&pane_list));
    println!("--- final workspace list ---");
    let workspace_list = herdr_json(runner.as_ref(), &cli_env, &["workspace", "list"]);
    println!("{}", summarize_workspaces(&workspace_list));

    println!("sleep track result: {sleep_res:?}");
    println!("pi track result: {pi_res:?}");
    sleep_res.expect("sleep track");
    pi_res.expect("pi track");
}

fn sleep_track(
    backend: &HerdrBackend,
    runner: &LoggingRunner,
    cli_env: &BTreeMap<String, String>,
    cwd: &Path,
) -> anyhow::Result<()> {
    let stamp = fresh_id();
    let cluster = format!("ls{stamp}");
    let label = format!("onlyne:{cluster}");
    let mut guard = Cleanup {
        backend,
        runner,
        cli_env,
        session: None,
        workspace_label: label.clone(),
        done: false,
    };
    let before = runner.snapshot().len();
    println!("sleep track: cluster={cluster} label={label}");
    let spec = spawn_spec(
        cwd,
        &fresh_uuid(&stamp),
        &["sleep", "120"],
        &cluster,
        "livesleep",
    );
    let session = backend.spawn(spec)?;
    guard.session = Some(session.clone());
    println!(
        "sleep spawn backend={} backend_ref={}",
        session.backend, session.backend_ref
    );
    anyhow::ensure!(
        session.backend == "herdr",
        "backend name {}",
        session.backend
    );
    let ids = herdr_ids(&session)?;
    anyhow::ensure!(!ids.workspace_id.is_empty(), "workspace_id empty");
    anyhow::ensure!(!ids.tab_id.is_empty(), "tab_id empty");
    anyhow::ensure!(!ids.pane_id.is_empty(), "pane_id empty");
    println!(
        "sleep ids workspace={} tab={} pane={}",
        ids.workspace_id, ids.tab_id, ids.pane_id
    );

    let calls = runner.snapshot();
    let track_calls = &calls[before..];
    print_named_argv(track_calls, "pane", "split");
    print_named_argv(track_calls, "pane", "run");
    print_named_argv(track_calls, "agent", "start");
    anyhow::ensure!(
        has_cmd(track_calls, "pane", "split"),
        "sleep track missing pane split"
    );
    anyhow::ensure!(
        has_cmd(track_calls, "pane", "run"),
        "sleep track missing pane run"
    );
    anyhow::ensure!(
        !has_cmd(track_calls, "agent", "start"),
        "sleep track must not call agent start"
    );

    let probe = backend.probe(&session)?;
    println!(
        "sleep probe alive={} attached={}",
        probe.alive, probe.attached
    );
    anyhow::ensure!(probe.alive, "sleep probe alive");

    // The pane-run track records no managed agent, so focus() takes the anchor
    // hop: the workspace and tab hops, then `pane focus --pane <base_pane>
    // --direction <split_direction>`, verified against `pane get`. This is what
    // the backend_ref anchor fields exist for, so the live case asserts the
    // outcome rather than recording a refusal.
    backend
        .focus(&session)
        .map_err(|error| anyhow::anyhow!("sleep focus failed: {error}"))?;
    println!("sleep focus Ok");
    observe_focus(runner, cli_env, &ids, "sleep")?;

    observe_pane_cwd_or_agent_status(runner, cli_env, cwd, &ids.pane_id, "sleep")?;

    guard.close_pane();
    let after_close = herdr_raw(runner, cli_env, &["pane", "get", &ids.pane_id]);
    println!(
        "sleep pane get after close status={} stdout={} stderr={}",
        after_close.status,
        String::from_utf8_lossy(&after_close.stdout),
        String::from_utf8_lossy(&after_close.stderr)
    );
    anyhow::ensure!(
        after_close.status != 0,
        "sleep pane get after close must fail"
    );
    let code = error_code(&after_close);
    println!("sleep pane get after close error.code={code}");
    anyhow::ensure!(
        code == "pane_not_found",
        "sleep pane get after close error.code={code}"
    );
    guard.close_workspace();
    Ok(())
}

fn pi_track(
    backend: &HerdrBackend,
    runner: &LoggingRunner,
    cli_env: &BTreeMap<String, String>,
    cwd: &Path,
) -> anyhow::Result<()> {
    let stamp = fresh_id();
    let cluster = format!("lp{stamp}");
    let label = format!("onlyne:{cluster}");
    let mut guard = Cleanup {
        backend,
        runner,
        cli_env,
        session: None,
        workspace_label: label.clone(),
        done: false,
    };
    let before = runner.snapshot().len();
    let task_id = fresh_uuid(&stamp);
    println!("pi track: cluster={cluster} label={label} task_id={task_id}");
    let spec = spawn_spec(cwd, &task_id, &["pi"], &cluster, "livepi");
    let session = backend.spawn(spec)?;
    guard.session = Some(session.clone());
    println!(
        "pi spawn backend={} backend_ref={}",
        session.backend, session.backend_ref
    );
    anyhow::ensure!(
        session.backend == "herdr",
        "backend name {}",
        session.backend
    );
    let ids = herdr_ids(&session)?;
    anyhow::ensure!(!ids.workspace_id.is_empty(), "workspace_id empty");
    anyhow::ensure!(!ids.tab_id.is_empty(), "tab_id empty");
    anyhow::ensure!(!ids.pane_id.is_empty(), "pane_id empty");
    anyhow::ensure!(!ids.agent.is_empty(), "agent field empty");
    println!(
        "pi ids workspace={} tab={} pane={} agent={}",
        ids.workspace_id, ids.tab_id, ids.pane_id, ids.agent
    );

    let calls = runner.snapshot();
    let track_calls = &calls[before..];
    print_named_argv(track_calls, "pane", "split");
    print_named_argv(track_calls, "pane", "run");
    print_named_argv(track_calls, "agent", "start");
    anyhow::ensure!(
        has_cmd(track_calls, "pane", "split"),
        "pi track missing pane split"
    );
    anyhow::ensure!(
        has_agent_start_kind(track_calls, "pi"),
        "pi track missing agent start --kind pi"
    );

    let probe = backend.probe(&session)?;
    println!("pi probe alive={} attached={}", probe.alive, probe.attached);
    anyhow::ensure!(probe.alive, "pi probe alive");

    backend.focus(&session)?;
    println!("pi focus Ok");
    observe_focus(runner, cli_env, &ids, "pi")?;

    let pane = herdr_json(runner, cli_env, &["pane", "get", &ids.pane_id]);
    let agent_status = pane
        .pointer("/pane/agent_status")
        .cloned()
        .or_else(|| pane.get("agent_status").cloned());
    println!("pi pane get agent_status={agent_status:?} body={pane}");
    anyhow::ensure!(
        agent_status.as_ref().is_some_and(|value| !value.is_null()),
        "pi pane get missing agent_status"
    );

    guard.close_now();

    let pane_list = herdr_json(runner, cli_env, &["pane", "list"]);
    println!("pi after close panes={}", summarize_panes(&pane_list));
    anyhow::ensure!(
        !pane_id_present(&pane_list, &ids.pane_id),
        "pi pane {} still listed after close",
        ids.pane_id
    );

    let agents = herdr_json(runner, cli_env, &["agent", "list"]);
    println!("pi after close agents={agents}");
    anyhow::ensure!(
        !agent_name_present(&agents, &ids.agent),
        "pi managed agent {} still listed after close",
        ids.agent
    );
    Ok(())
}

fn spawn_spec(cwd: &Path, task_id: &str, command: &[&str], cluster: &str, role: &str) -> SpawnSpec {
    let mut env = BTreeMap::new();
    env.insert("ONLYNE_CLUSTER".into(), cluster.into());
    env.insert("ONLYNE_ROLE".into(), role.into());
    env.insert("ONLYNE_TASK_ID".into(), task_id.into());
    SpawnSpec {
        cwd: cwd.to_path_buf(),
        task_id: task_id.to_string(),
        command: command.iter().map(|part| (*part).to_string()).collect(),
        env,
        focus: None,
        placement: None,
        rename: None,
    }
}

struct HerdrIds {
    workspace_id: String,
    tab_id: String,
    pane_id: String,
    agent: String,
}

fn herdr_ids(session: &SessionRef) -> anyhow::Result<HerdrIds> {
    let herdr = session
        .backend_ref
        .get("herdr")
        .ok_or_else(|| anyhow::anyhow!("backend_ref missing herdr"))?;
    let field = |key: &str| -> String {
        herdr
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    Ok(HerdrIds {
        workspace_id: field("workspace_id"),
        tab_id: field("tab_id"),
        pane_id: field("pane_id"),
        agent: field("agent"),
    })
}

fn report_focus_fields(
    runner: &LoggingRunner,
    cli_env: &BTreeMap<String, String>,
    ids: &HerdrIds,
    tag: &str,
) -> (Option<bool>, Option<bool>, Option<bool>) {
    let pane = herdr_json(runner, cli_env, &["pane", "get", &ids.pane_id]);
    let tab = herdr_json(runner, cli_env, &["tab", "get", &ids.tab_id]);
    let workspace = herdr_json(runner, cli_env, &["workspace", "get", &ids.workspace_id]);
    let pane_focused = pane.pointer("/pane/focused").and_then(Value::as_bool);
    let tab_focused = tab.pointer("/tab/focused").and_then(Value::as_bool);
    let workspace_focused = workspace
        .pointer("/workspace/focused")
        .and_then(Value::as_bool);
    println!(
        "{tag} focus fields pane.focused={pane_focused:?} tab.focused={tab_focused:?} workspace.focused={workspace_focused:?}"
    );
    println!("{tag} pane get after focus {pane}");
    println!("{tag} tab get after focus {tab}");
    println!("{tag} workspace get after focus {workspace}");
    (pane_focused, tab_focused, workspace_focused)
}

fn observe_focus(
    runner: &LoggingRunner,
    cli_env: &BTreeMap<String, String>,
    ids: &HerdrIds,
    tag: &str,
) -> anyhow::Result<()> {
    let (pane_focused, _, _) = report_focus_fields(runner, cli_env, ids, tag);
    anyhow::ensure!(
        pane_focused == Some(true),
        "{tag} pane.focused is not true after focus()"
    );
    Ok(())
}

fn observe_pane_cwd_or_agent_status(
    runner: &LoggingRunner,
    cli_env: &BTreeMap<String, String>,
    expected_cwd: &Path,
    pane_id: &str,
    tag: &str,
) -> anyhow::Result<()> {
    let pane = herdr_json(runner, cli_env, &["pane", "get", pane_id]);
    let reported = pane
        .pointer("/pane/cwd")
        .and_then(Value::as_str)
        .unwrap_or("");
    let agent_status = pane.pointer("/pane/agent_status").cloned();
    println!(
        "{tag} pane cwd={reported:?} expected={} agent_status={agent_status:?}",
        expected_cwd.display()
    );
    let expected = canonicalize_lossy(expected_cwd);
    let got = canonicalize_lossy(&PathBuf::from(reported));
    if expected == got {
        println!("{tag} cwd matched after canonicalize {got}");
        return Ok(());
    }
    println!("{tag} cwd mismatch expected={expected} got={got}; asserting agent_status exists");
    anyhow::ensure!(
        agent_status.as_ref().is_some_and(|value| !value.is_null()),
        "{tag} pane get missing agent_status after cwd mismatch"
    );
    Ok(())
}

fn close_workspace_by_label(
    runner: &LoggingRunner,
    cli_env: &BTreeMap<String, String>,
    want_label: &str,
) {
    let listed = herdr_json(runner, cli_env, &["workspace", "list"]);
    let rows = listed
        .get("workspaces")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for row in rows {
        let id = row
            .get("workspace_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        let label = row.get("label").and_then(Value::as_str).unwrap_or("");
        if label != want_label {
            continue;
        }
        if reserved_workspace(id, label) {
            println!("cleanup: refusing reserved workspace id={id} label={label}");
            continue;
        }
        println!("cleanup: workspace close id={id} label={label}");
        let output = herdr_raw(runner, cli_env, &["workspace", "close", id]);
        println!(
            "cleanup: workspace close status={} stdout={} stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn reserved_workspace(id: &str, label: &str) -> bool {
    RESERVED_WORKSPACE_IDS.contains(&id) || RESERVED_WORKSPACE_LABELS.contains(&label)
}

fn herdr_raw(
    runner: &LoggingRunner,
    cli_env: &BTreeMap<String, String>,
    args: &[&str],
) -> CommandOutput {
    let args: Vec<String> = args.iter().map(|part| (*part).to_string()).collect();
    runner
        .run("herdr", &args, None, cli_env)
        .unwrap_or_else(|error| CommandOutput {
            status: -1,
            stdout: Vec::new(),
            stderr: error.to_string().into_bytes(),
        })
}

fn herdr_json(runner: &LoggingRunner, cli_env: &BTreeMap<String, String>, args: &[&str]) -> Value {
    let output = herdr_raw(runner, cli_env, args);
    let stdout = String::from_utf8_lossy(&output.stdout);
    println!(
        "herdr {} status={} stdout={} stderr={}",
        args.join(" "),
        output.status,
        stdout,
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed: Value = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
    parsed.get("result").cloned().unwrap_or(parsed)
}

fn error_code(output: &CommandOutput) -> String {
    for bytes in [&output.stdout, &output.stderr] {
        let Ok(body) = serde_json::from_slice::<Value>(bytes) else {
            continue;
        };
        if let Some(code) = body.pointer("/error/code").and_then(Value::as_str) {
            return code.to_string();
        }
    }
    String::new()
}

fn has_cmd(calls: &[(String, Vec<String>)], group: &str, verb: &str) -> bool {
    calls.iter().any(|(_, args)| {
        args.first().map(String::as_str) == Some(group)
            && args.get(1).map(String::as_str) == Some(verb)
    })
}

fn has_agent_start_kind(calls: &[(String, Vec<String>)], kind: &str) -> bool {
    calls.iter().any(|(_, args)| {
        args.first().map(String::as_str) == Some("agent")
            && args.get(1).map(String::as_str) == Some("start")
            && args
                .windows(2)
                .any(|pair| pair[0] == "--kind" && pair[1] == kind)
    })
}

fn print_named_argv(calls: &[(String, Vec<String>)], group: &str, verb: &str) {
    for (program, args) in calls {
        if args.first().map(String::as_str) == Some(group)
            && args.get(1).map(String::as_str) == Some(verb)
        {
            println!("actual argv {group} {verb}: program={program} args={args:?}");
        }
    }
}

fn pane_id_present(listed: &Value, pane_id: &str) -> bool {
    listed
        .get("panes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|row| row.get("pane_id").and_then(Value::as_str) == Some(pane_id))
}

fn agent_name_present(listed: &Value, name: &str) -> bool {
    listed
        .get("agents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|row| {
            row.get("name").and_then(Value::as_str) == Some(name)
                || row.get("agent").and_then(Value::as_str) == Some(name)
        })
}

fn summarize_panes(listed: &Value) -> String {
    let rows = listed
        .get("panes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let brief: Vec<Value> = rows
        .iter()
        .map(|row| {
            serde_json::json!({
                "pane_id": row.get("pane_id"),
                "workspace_id": row.get("workspace_id"),
                "tab_id": row.get("tab_id"),
                "cwd": row.get("cwd"),
                "focused": row.get("focused"),
                "agent_status": row.get("agent_status"),
            })
        })
        .collect();
    serde_json::json!({"panes": brief}).to_string()
}

fn summarize_workspaces(listed: &Value) -> String {
    let rows = listed
        .get("workspaces")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let brief: Vec<Value> = rows
        .iter()
        .map(|row| {
            serde_json::json!({
                "workspace_id": row.get("workspace_id"),
                "label": row.get("label"),
                "focused": row.get("focused"),
                "pane_count": row.get("pane_count"),
                "tab_count": row.get("tab_count"),
            })
        })
        .collect();
    serde_json::json!({"workspaces": brief}).to_string()
}

fn canonicalize_lossy(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

fn fresh_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let pid = u128::from(std::process::id());
    format!("{pid:x}{nanos:x}")
}

fn fresh_uuid(stamp: &str) -> String {
    let mut hex: String = stamp.chars().filter(|ch| ch.is_ascii_hexdigit()).collect();
    hex = hex.to_ascii_lowercase();
    while hex.len() < 32 {
        hex.push('0');
    }
    format!(
        "{}-{}-4{}-a{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[13..16],
        &hex[17..20],
        &hex[20..32]
    )
}
