use super::HerdrBackend;
use super::cli::{absolute_cwd, cmd_quote, pane_run_line, posix_shell_quote};
use super::policy::{agent_name, is_agent_name};
use crate::backend::*;
use parking_lot::Mutex;
use std::sync::Arc;

mod focus;
mod probe;
mod spawn;

#[derive(Default)]
struct Script {
    calls: Mutex<Vec<String>>,
    script: Mutex<Vec<(String, i32, String, String)>>,
}

impl Script {
    fn reply(self, fragment: &str, status: i32, body: impl Into<String>) -> Self {
        self.script
            .lock()
            .push((fragment.to_string(), status, body.into(), String::new()));
        self
    }

    /// A refused call: herdr puts its error document on stderr with stdout empty.
    fn reply_err(self, fragment: &str, status: i32, stderr: impl Into<String>) -> Self {
        self.script
            .lock()
            .push((fragment.to_string(), status, String::new(), stderr.into()));
        self
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().clone()
    }
}

impl Runner for Script {
    fn run(
        &self,
        program: &str,
        args: &[String],
        _cwd: Option<&Path>,
        _env: &BTreeMap<String, String>,
    ) -> Result<CommandOutput> {
        let call = format!("{program} {}", args.join(" "));
        self.calls.lock().push(call.clone());
        let mut script = self.script.lock();
        let index = script
            .iter()
            .position(|(fragment, ..)| call.contains(fragment.as_str()))
            .unwrap_or_else(|| panic!("unscripted herdr call: {call}"));
        let (_, status, stdout, stderr) = script.remove(index);
        Ok(CommandOutput {
            status,
            stdout: stdout.into_bytes(),
            stderr: stderr.into_bytes(),
        })
    }
}

fn envelope(result: Value) -> String {
    serde_json::json!({"id": "cli:test", "result": result}).to_string()
}

/// The `session_command` shape a generated role workspace carries: the
/// agent binary plus the flags that tie the pane to its client's session.
fn session_command() -> Vec<&'static str> {
    vec!["pi", "--session-id", "s-1", "--session-dir", ".pi/sessions"]
}

fn spec(command: Vec<&str>) -> SpawnSpec {
    spec_at(command, "/tmp/ws")
}

fn spec_at(command: Vec<&str>, cwd: &str) -> SpawnSpec {
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
        placement: Some(PanePlacement {
            direction: SplitDirection::Right,
            ratio: 0.5,
        }),
        rename: None,
    }
}

fn session_ref(pane: &str) -> SessionRef {
    session_ref_with(pane, "onlyne-planner-abcd1234", "wF:p1", "right")
}

/// The ref a `pane run` session gets: a pane, an anchor, no managed agent.
fn shell_session_ref(pane: &str) -> SessionRef {
    session_ref_with(pane, "", "wF:p1", "right")
}

fn session_ref_with(pane: &str, agent: &str, base_pane: &str, direction: &str) -> SessionRef {
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
                "split_direction": direction,
            }
        }),
        generation: 1,
    }
}

/// `pane get` answer for one pane, with the focus flag the caller wants.
fn pane_info(pane: &str, focused: bool) -> String {
    envelope(serde_json::json!({
        "type": "pane_info",
        "pane": {"pane_id": pane, "focused": focused, "agent_status": "unknown"},
    }))
}

/// `pane list` answer naming which workspace/tab/pane holds focus.
fn focused_pane(workspace: &str, tab: &str, pane: &str) -> String {
    envelope(serde_json::json!({
        "panes": [{
            "pane_id": pane,
            "tab_id": tab,
            "workspace_id": workspace,
            "focused": true
        }]
    }))
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
            "number": 1,
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
            "focused": false,
            "revision": 1
        }
    }))
}

fn agent_started() -> String {
    envelope(serde_json::json!({
        "type": "agent_started",
        "agent": {
            "name": "onlyne-planner-abcd1234",
            "agent": "pi",
            "agent_status": "idle",
            "interactive_ready": true,
            "pane_id": "wF:p2"
        },
        "argv": ["pi"]
    }))
}
