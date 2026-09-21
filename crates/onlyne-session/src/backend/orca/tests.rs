use super::OrcaBackend;
use super::cli::{spawn_command, spawn_command_cmd, spawn_command_posix};
use crate::backend::*;
use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::path::Path;

mod command_decoding;
mod lifecycle;
mod placement;
mod tab_map;

use super::*;

/// Scripted Orca CLI: every entry answers the first call whose argv
/// contains its fragment and is then consumed, so a retry needs its own
/// entry and an unscripted call panics.
#[derive(Default)]
struct OrcaCli {
    calls: Mutex<Vec<String>>,
    script: Mutex<Vec<(String, i32, String)>>,
}

impl OrcaCli {
    fn reply(self, fragment: &str, status: i32, body: String) -> Self {
        self.script
            .lock()
            .push((fragment.to_string(), status, body));
        self
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().clone()
    }
    fn called(&self, fragment: &str) -> usize {
        self.calls()
            .iter()
            .filter(|call| call.contains(fragment))
            .count()
    }
}

impl Runner for OrcaCli {
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
            .unwrap_or_else(|| panic!("unscripted orca call: {call}"));
        let (_, status, body) = script.remove(index);
        Ok(CommandOutput {
            status,
            stdout: body.into_bytes(),
            stderr: Vec::new(),
        })
    }
}

/// A successful response envelope; `run_json` unwraps `result`.
fn envelope(result: Value) -> String {
    serde_json::json!({"ok": true, "result": result}).to_string()
}

/// The failure shape 1.4.198 uses: exit 1, empty stderr, error body on
/// stdout.
fn refusal(code: &str) -> String {
    serde_json::json!({
        "ok": false,
        "error": {"code": code, "message": format!("{code} refused")}
    })
    .to_string()
}

/// A `terminal create` row, as the CLI nests it under `terminal`.
fn created_row() -> Value {
    serde_json::json!({
        "terminal": {
            "handle": "term_one",
            "paneKey": "tab-1:leaf-2",
            "tabId": "tab-1",
            "leafId": "leaf-2",
            "ptyId": "inst::/tmp/ws@@ab",
            "worktreeId": "inst::/tmp/ws"
        }
    })
}

/// A `terminal list` row for the same pane on a newer PTY incarnation.
fn relisted_row() -> Value {
    serde_json::json!({
        "handle": "term_two",
        "paneKey": "tab-1:leaf-2",
        "tabId": "tab-1",
        "leafId": "leaf-2",
        "ptyId": "inst2::/tmp/ws@@cd",
        "worktreeId": "inst::/tmp/ws",
        "connected": true,
        "writable": true,
        "lastOutputAt": 9
    })
}

fn session(reference: Value) -> SessionRef {
    SessionRef {
        task_id: "task-1".into(),
        backend: "orca".into(),
        backend_ref: reference,
        generation: 1,
    }
}

fn spawn_spec(cwd: &Path) -> SpawnSpec {
    let mut env = BTreeMap::new();
    env.insert("ONLYNE_SESSION_ID".into(), "session-1".into());
    env.insert("ONLYNE_TASK_ID".into(), "task-1".into());
    env.insert("ONLYNE_ROLE".into(), "planner".into());
    SpawnSpec {
        cwd: cwd.to_path_buf(),
        task_id: "task-1".into(),
        command: vec!["pi".into()],
        env,
        focus: None,
        placement: None,
        rename: None,
    }
}

fn mapping_lines(root: &Path) -> Vec<Value> {
    std::fs::read_to_string(root.join(".onlyne/cache/orca-tabs.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// The worktree id Orca exports to a tab, in its measured 1.4.198 shape:
/// `<worktree-id>::<abs workspace path>`.
const HOST_WORKTREE: &str = "2ea2fe23-829c-4a8f-bcac-4129eb78a164::/tmp/host-ws";
