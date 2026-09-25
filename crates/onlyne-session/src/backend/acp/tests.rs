//! End-to-end tests for the ACP backend: a scripted python agent speaks the
//! protocol on a pipe, and every assertion is about what this client left in its
//! journals or handed to its outcome feed.
//!
//! The shared rig — the fake agent, its trace file, and the helpers that run one
//! turn to its reported ending — lives here; the subjects live in submodules.

use crate::backend::{
    CloseReason, OutcomeFeed, SessionBackend, SessionOutcome, SessionRef, SpawnSpec,
};
use crate::lifecycle::TaskState;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tempfile::TempDir;

use super::turn::{payload_dir, payload_path};
use super::{AcpBackend, AcpOptions};

mod payload;
mod process;
mod session;
mod turn;

/// A scripted ACP v1 agent, used as the role's `session_command`. It is a
/// python child because the thing under test is a protocol on a pipe: a mock
/// in this crate could not show that an id must be echoed back exactly, that a
/// frame the agent never answers has to fail its caller, or that a process can
/// die in the middle of a turn.
///
/// Markers in the prompt text choose what the turn does, so one script covers
/// every ending the backend has to map, and the agent's chosen answer is echoed
/// back as its closing message — which is how a test reads what this client
/// decided without a second channel.
const FAKE_AGENT: &str = r##"#!/usr/bin/env python3
"""A scripted ACP v1 agent for the onlyne-session acp backend tests."""
import json, os, sys

TRACE = os.environ.get("ACP_TRACE", "")
SESSIONS = [0]


def trace(line):
    if TRACE:
        with open(TRACE, "a") as fh:
            fh.write(line + "\n")
            fh.flush()


def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def result(rid, value):
    send({"jsonrpc": "2.0", "id": rid, "result": value})


def failure(rid, code, message):
    send({"jsonrpc": "2.0", "id": rid, "error": {"code": code, "message": message}})


def update(sid, body):
    send({"jsonrpc": "2.0", "method": "session/update",
          "params": dict({"sessionId": sid}, **body)})


def chunk(sid, text):
    update(sid, {"sessionUpdate": "agent_message_chunk",
                 "content": {"type": "text", "text": text}})


def read_line():
    line = sys.stdin.readline()
    if not line:
        return None
    try:
        return json.loads(line)
    except ValueError:
        return {}


def await_reply(rid):
    """Read until the client answers our request. A single-threaded agent can
    afford to block on it, which is exactly what makes a refused ask observable."""
    while True:
        msg = read_line()
        if msg is None:
            sys.exit(0)
        if msg.get("id") == rid and ("result" in msg or "error" in msg):
            return msg


def turn(rid, sid, prompt):
    if "MARK:die" in prompt:
        # Say something on the way out: the journal of a turn the process did not
        # finish is still the operator's only record of it.
        update(sid, {"sessionUpdate": "agent_thought_chunk",
                     "content": {"type": "text", "text": "Reading the file.\n"}})
        sys.stderr.write("boom: the fake agent gives up\n")
        sys.stderr.flush()
        os._exit(7)
    closing = "I edited hello.py."
    if "MARK:ask" in prompt or "MARK:askalways" in prompt:
        if "MARK:askalways" in prompt:
            options = [{"optionId": "always", "kind": "allow_always", "name": "Always allow"}]
        else:
            options = [{"optionId": "once", "kind": "allow_once", "name": "Allow once"},
                       {"optionId": "always", "kind": "allow_always", "name": "Always"},
                       {"optionId": "no", "kind": "reject_once", "name": "Reject once"}]
        send({"jsonrpc": "2.0", "id": "perm-1", "method": "session/request_permission",
              "params": {"sessionId": sid,
                         "toolCall": {"toolCallId": "call_9", "title": "Edit hello.py",
                                      "kind": "edit", "status": "pending"},
                         "options": options}})
        reply = await_reply("perm-1")
        outcome = (reply.get("result") or {}).get("outcome") or {}
        chosen = outcome.get("optionId") or outcome.get("outcome") or "nothing"
        trace("answered %s" % chosen)
        closing = "answer=%s" % chosen
    update(sid, {"sessionUpdate": "agent_thought_chunk",
                 "content": {"type": "text", "text": "Let me "}})
    update(sid, {"sessionUpdate": "agent_thought_chunk",
                 "content": {"type": "text", "text": "try.\n"}})
    update(sid, {"sessionUpdate": "available_commands_update",
                 "availableCommands": [{"name": "/cmd%d" % i} for i in range(120)]})
    update(sid, {"sessionUpdate": "tool_call", "toolCallId": "call_9", "status": "pending",
                 "title": "Edit hello.py", "kind": "edit"})
    update(sid, {"sessionUpdate": "tool_call_update", "toolCallId": "call_9",
                 "status": "completed"})
    if "MARK:noise" in prompt:
        # Another session sharing this process: its text belongs to its journal.
        chunk("sess-other", "not this turn's answer")
    if "MARK:noanswer" not in prompt:
        chunk(sid, closing + "\n")
        chunk(sid, "<status>done")
    if "MARK:nostop" in prompt:
        return result(rid, {})
    stop = "end_turn"
    for marker, reason in (("MARK:cancel", "cancelled"), ("MARK:maxtokens", "max_tokens"),
                           ("MARK:refusal", "refusal"), ("MARK:weird", "stopped_by_hook")):
        if marker in prompt:
            stop = reason
    result(rid, {"stopReason": stop})


def dispatch(msg):
    method, rid = msg.get("method"), msg.get("id")
    params = msg.get("params") or {}
    if method == "initialize":
        caps = {"protocolVersion": 1, "agentInfo": {"name": "fake-acp", "version": "0"},
                "authMethods": []}
        if not os.environ.get("ACP_NO_CLOSE"):
            caps["agentCapabilities"] = {"sessionCapabilities": {"close": {}}}
        result(rid, caps)
    elif method == "session/new":
        SESSIONS[0] += 1
        sid = "sess-%d" % SESSIONS[0]
        trace("new %s" % sid)
        result(rid, {"sessionId": sid, "modes": {"currentModeId": "default"},
                     "models": {"currentModelId": "fast"}, "configOptions": []})
    elif method == "session/set_mode":
        trace("set_mode %s" % params.get("modeId"))
        if os.environ.get("ACP_REJECT_CONFIG"):
            failure(rid, -32602, "no such mode")
        else:
            result(rid, {})
    elif method == "session/set_config_option":
        trace("set_config_option %s=%s" % (params.get("configId"), params.get("value")))
        if os.environ.get("ACP_REJECT_CONFIG"):
            failure(rid, -32602, "no such option")
        else:
            result(rid, {})
    elif method == "session/close":
        trace("close %s" % params.get("sessionId"))
        result(rid, {})
    elif method == "session/prompt":
        prompt = "".join(block.get("text", "") for block in params.get("prompt") or [])
        trace("prompt %s" % prompt.replace("\n", " / "))
        turn(rid, params.get("sessionId"), prompt)
    elif method == "session/cancel":
        trace("cancel")
    elif rid is not None:
        failure(rid, -32601, "fake agent does not implement %s" % method)


def main():
    trace("start pid %d" % os.getpid())
    while True:
        msg = read_line()
        if msg is None:
            trace("eof")
            return
        if msg:
            dispatch(msg)


main()
"##;

/// One tempdir per fake agent: the script, the trace, and the workspace whose
/// `.onlyne/logs` the backend journals into.
struct Fake {
    root: TempDir,
    script: PathBuf,
    trace: PathBuf,
}

impl Fake {
    fn new() -> Self {
        let root = TempDir::new().expect("a temp workspace for the fake agent");
        let script = root.path().join("fake_agent.py");
        fs::write(&script, FAKE_AGENT).expect("write the fake agent script");
        Fake {
            trace: root.path().join("agent.trace"),
            root,
            script,
        }
    }

    fn command(&self) -> Vec<String> {
        // argv, not a shell string. `-u` keeps python from buffering a frame
        // the test is waiting on.
        vec![
            "python3".to_string(),
            "-u".to_string(),
            self.script.to_string_lossy().to_string(),
        ]
    }

    fn spec(&self, task: &str) -> SpawnSpec {
        SpawnSpec {
            cwd: self.root.path().to_path_buf(),
            task_id: task.to_string(),
            command: self.command(),
            env: BTreeMap::from([("ACP_TRACE".to_string(), self.trace_display())]),
            focus: None,
            placement: None,
            rename: None,
        }
    }

    fn trace_display(&self) -> String {
        self.trace.to_string_lossy().to_string()
    }

    fn traced(&self) -> String {
        fs::read_to_string(&self.trace).unwrap_or_default()
    }
}

fn journal(session: &SessionRef, key: &str) -> PathBuf {
    PathBuf::from(
        session
            .backend_ref
            .get(key)
            .and_then(Value::as_str)
            .expect("the ref carries its journal paths"),
    )
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

/// A turn that sent nothing renderable leaves no log at all, which is not a
/// failure: the journal is created by its first write.
fn read_or_empty(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

fn jsonl(path: &Path) -> Vec<Value> {
    read(path)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("bad JSONL line {line}: {error}"))
        })
        .collect()
}

fn onlyne_records(lines: &[Value], kind: &str) -> Vec<Value> {
    lines
        .iter()
        .filter_map(|line| line.get("onlyne"))
        .filter(|record| record.get("kind").and_then(Value::as_str) == Some(kind))
        .cloned()
        .collect()
}

fn await_outcome(feed: &OutcomeFeed, task: &str) -> SessionOutcome {
    // One turn is a spawned agent and its round trips. The suite runs every crate's
    // tests over the runner's few cores at once, so this bound belongs to the
    // machine's schedule: a turn that arrives late is still the turn it was owed,
    // and a turn that never arrives fails here.
    let deadline = Instant::now() + Duration::from_secs(90);
    while Instant::now() < deadline {
        if let Some(outcome) = feed.recv_timeout(Duration::from_millis(500)) {
            assert_eq!(outcome.task_id, task, "a fact arrived for another task");
            return outcome;
        }
    }
    panic!("no outcome for {task} within 90s");
}

/// Run one turn to its reported ending and hand back the outcome with both
/// journal files as written for it.
fn run_turn(
    backend: &AcpBackend,
    session: &SessionRef,
    task: &str,
    prose: &str,
) -> (SessionOutcome, Vec<Value>, String) {
    let feed = backend
        .outcomes()
        .expect("an acp backend reports its own endings");
    let events = journal(session, "events");
    let log = journal(session, "log");
    backend
        .deliver(session, task, prose)
        .unwrap_or_else(|error| panic!("deliver {task}: {error}"));
    let outcome = await_outcome(&feed, task);
    // The journal is written before the fact is handed over, so a complete
    // file is observable in the same moment the outcome is. `run_turn` ends
    // there, so the whole file is settled by the time the report is read.
    (outcome, jsonl(&events), read_or_empty(&log))
}

/// Close every session of one test and wait for its agents to go, so a fake
/// agent process never outlives the test that started it.
fn finish(backend: &AcpBackend, fake: &Fake, sessions: &[&SessionRef]) {
    for session in sessions {
        backend
            .close(session, CloseReason::Completed, false)
            .expect("close succeeds");
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if backend.state.agents.lock().is_empty() && backend.state.sessions.lock().is_empty() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "agent processes outlived the test: {:?}\ntrace: {}",
        backend.state.agents.lock().keys().collect::<Vec<_>>(),
        fake.traced()
    );
}
