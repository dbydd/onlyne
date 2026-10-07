//! Fake-runner cases for the tern backend.
//!
//! Every answer in this file is a document the installed ternary actually
//! wrote (ids, `keep_open`, `exited`, the text of a refusal), so a case that
//! passes here is a case whose decoding matches the host's spelling.

use super::TernBackend;
use crate::backend::*;
use parking_lot::Mutex;
use std::sync::Arc;

mod cli;
mod close;
mod focus;
mod probe;
mod spawn;

/// A runner that answers from a script of fragments and records every call.
///
/// Fragments are matched by containment, in script order, so one case can
/// answer the same command twice with different documents — which is what the
/// focus cases need, since focus reads the listing before and after the hop.
#[derive(Default)]
struct Script {
    calls: Mutex<Vec<String>>,
    script: Mutex<Vec<Reply>>,
}

struct Reply {
    fragment: String,
    status: i32,
    stdout: String,
    stderr: String,
}

impl Script {
    fn reply(self, fragment: &str, stdout: impl Into<String>) -> Self {
        self.script.lock().push(Reply {
            fragment: fragment.to_string(),
            status: 0,
            stdout: stdout.into(),
            stderr: String::new(),
        });
        self
    }

    /// A refused call: tern writes its message to stderr, stdout empty, exit 1.
    ///
    /// The message is tern's own wording, because there is no JSON error code
    /// to fake — `no block is called` is what the close and focus cases below
    /// branch on.
    fn refused(self, fragment: &str, stderr: impl Into<String>) -> Self {
        self.script.lock().push(Reply {
            fragment: fragment.to_string(),
            status: 1,
            stdout: String::new(),
            stderr: stderr.into(),
        });
        self
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().clone()
    }

    /// How many calls this script has answered.
    fn call_count(&self) -> usize {
        self.calls.lock().len()
    }
}

impl Runner for Script {
    fn run(
        &self,
        program: &str,
        args: &[String],
        _cwd: Option<&Path>,
        env: &BTreeMap<String, String>,
    ) -> Result<CommandOutput> {
        let call = format!("{program} {}", args.join(" "));
        self.calls.lock().push(call.clone());
        // The window flag is appended after the command, so a scripted answer
        // is matched against the argv without it.
        let mut script = self.script.lock();
        let index = script
            .iter()
            .position(|reply| call.contains(reply.fragment.as_str()))
            .unwrap_or_else(|| panic!("unscripted tern call: {call}"));
        let reply = script.remove(index);
        // The environment the CLI is invoked with is a fact the argv cases
        // check, so it travels to the case through the call record rather
        // than being asserted here.
        let _ = env;
        Ok(CommandOutput {
            status: reply.status,
            stdout: reply.stdout.into_bytes(),
            stderr: reply.stderr.into_bytes(),
        })
    }
}

/// One tab row, with the given blocks. `focused` marks the block at that
/// index as holding focus.
fn tab(id: u64, name: Option<&str>, blocks: &[u64], focused: Option<usize>) -> Value {
    serde_json::json!({
        "id": id,
        "number": 1,
        "name": name,
        "shown": true,
        "zoomed": false,
        "blocks": blocks.iter().enumerate().map(|(at, block)| {
            serde_json::json!({
                "id": block,
                "title": "/w",
                "cwd": "/w",
                "program": "/opt/homebrew/bin/fish",
                "args": ["-l"],
                "command": null,
                "exited": null,
                "keep_open": true,
                "focused": Some(at) == focused,
                "live": true,
            })
        }).collect::<Vec<_>>(),
        "splits": {"Leaf": blocks.first().copied().unwrap_or(0)},
    })
}

/// One session row.
fn session(id: u64, name: Option<&str>, tabs: Vec<Value>) -> Value {
    serde_json::json!({
        "id": id,
        "name": name,
        "shown": true,
        "tabs": tabs,
    })
}

/// A `ls --json` document around the given sessions.
fn listing(sessions: Vec<Value>) -> String {
    serde_json::json!({"sessions": sessions, "detached": []}).to_string()
}

/// The `{"session":N,"tab":N,"block":N}` answer `new session`, `new tab` and
/// `split` all write.
fn created(session: u64, tab_id: u64, block: u64) -> String {
    serde_json::json!({"session": session, "tab": tab_id, "block": block}).to_string()
}

fn session_ref(pane: &str) -> SessionRef {
    SessionRef {
        task_id: "abcd1234-ffff-4000-8000-000000000001".into(),
        backend: "tern".into(),
        backend_ref: serde_json::json!({
            "tern": {
                "session_id": "2147483648",
                "tab_id": "2147483649",
                "pane_id": pane,
                "session_label": "onlyne:lab",
                "base_pane": "2147483650",
                "split_direction": "right",
            }
        }),
        generation: 1,
    }
}

/// A backend over the script, with no window key: a client outside any pane
/// drives the first window, which is what omitting `--window` does.
fn backend(script: Script) -> (TernBackend, Arc<Script>) {
    let script = Arc::new(script);
    (
        TernBackend::with_env(script.clone(), BTreeMap::new()),
        script,
    )
}

/// The `SpawnSpec` one case spawns with: a role, a cluster and a command whose
/// value carries a space, so the argv cases read the quoting question.
fn spec() -> SpawnSpec {
    SpawnSpec {
        cwd: std::path::PathBuf::from("/w"),
        task_id: "abcd1234-ffff-4000-8000-000000000001".into(),
        command: vec!["pi".into(), "--session-id".into(), "a b".into()],
        env: [
            ("ONLYNE_ROLE".to_string(), "planner".to_string()),
            ("ONLYNE_CLUSTER".to_string(), "lab".to_string()),
        ]
        .into_iter()
        .collect(),
        tools_token: String::new(),
        prose: String::new(),
        focus: None,
        placement: None,
        rename: None,
    }
}
