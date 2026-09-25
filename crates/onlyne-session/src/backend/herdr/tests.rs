use super::HerdrBackend;
use super::cli::{cmd_quote, pane_run_line, posix_shell_quote};
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

fn session_ref(pane: &str) -> SessionRef {
    session_ref_with(pane, "onlyne-planner-abcd1234", "wF:p1", "right")
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
