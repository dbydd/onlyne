//! End-to-end tests for `onlyne-acp` against a scripted fake agent: a real
//! `python3` child, real stdio, real JSON-RPC frames. The codec and the
//! connection's own unit tests cannot see a pipe fill up, a process die mid-turn,
//! or an id that must be echoed back exactly, and those are the failures that
//! would otherwise surface in production as a wedged session.
//!
//! The fake agent validates the frames it receives and refuses a badly shaped one
//! with `-32602`, so these tests assert both directions of the protocol: what the
//! client sends must be what ACP says, and what the client does with an agent's
//! stream is what its caller sees.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

use onlyne_acp::{
    Agent, AgentCapabilities, AgentOptions, ClientCapabilities, ClientInfo, ContentBlock,
    EnvVariable, Event, McpServer, PROTOCOL_VERSION, PermissionOption, PermissionOutcome,
    PermissionRequest, PromptOutcome, RequestId, RpcError, SessionStart, Update,
};
use serde_json::{Value, json};
use tempfile::TempDir;

/// How long a test waits for a frame the fake agent is expected to produce. This
/// is the test's own bound, not a protocol timeout: the library has none.
const EVENT_WAIT: Duration = Duration::from_secs(10);

const FAKE_AGENT: &str = r##"#!/usr/bin/env python3
"""A scripted ACP v1 agent.

Reads newline-delimited JSON-RPC 2.0 on stdin; stdout carries protocol frames
only, diagnostics go to stderr. Every frame is flushed as it is written: a piped
stdout is block-buffered, and an unflushed agent looks exactly like a client that
ignores responses. Behavior is driven by markers in the prompt text, so one
script covers a whole turn lifecycle.
"""
import json
import os
import sys
import time

IGNORE_EOF = os.environ.get("FAKE_AGENT_IGNORE_EOF") == "1"

SESSIONS = {}
STATE = {"sessions": 0, "reverse": 0}


def send(frame):
    try:
        sys.stdout.write(json.dumps(frame) + "\n")
        sys.stdout.flush()
    except BrokenPipeError:
        sys.exit(0)


def ok(rid, body):
    send({"jsonrpc": "2.0", "id": rid, "result": body})


def bad(rid, code, message):
    send({"jsonrpc": "2.0", "id": rid, "error": {"code": code, "message": message}})


def notify(method, params):
    send({"jsonrpc": "2.0", "method": method, "params": params})


def update(session_id, payload):
    body = {"sessionId": session_id}
    body.update(payload)
    notify("session/update", body)


def chunk(session_id, kind, text):
    update(session_id, {"sessionUpdate": kind,
                        "content": {"type": "text", "text": text}})


def read_message():
    """The next frame, or None when stdin closes (the client left)."""
    line = sys.stdin.readline()
    if line == "":
        return None
    line = line.strip()
    if not line:
        return {}
    try:
        return json.loads(line)
    except ValueError:
        sys.stderr.write("fake agent: unparseable inbound frame\n")
        return {}


def wait_for_stdin():
    """Ignore end-of-stdin, for the test that checks SIGTERM does."""
    if not IGNORE_EOF:
        sys.exit(0)
    sys.stderr.write("fake agent: end of stdin ignored\n")
    sys.stderr.flush()
    while True:
        time.sleep(0.05)


def await_frame(rid, prompt_id):
    """Wait for the reply to reverse request `rid`, or for the turn to be stopped.

    Returns ("answer", frame), ("cancel_request", None) when this client withdraws
    `prompt_id`, or ("session_cancel", None) when it interrupts the session. The
    two mean different things in ACP: a withdrawn request is a JSON-RPC error, a
    cancelled session is a turn that ends with stopReason cancelled. Other inbound
    frames are dispatched as usual, which is what a real agent has to do while
    parked.
    """
    while True:
        msg = read_message()
        if msg is None:
            wait_for_stdin()
        method = msg.get("method")
        if method == "$/cancel_request" and msg.get("params", {}).get("requestId") == prompt_id:
            return ("cancel_request", None)
        if method == "session/cancel":
            return ("session_cancel", None)
        if method is not None:
            dispatch(msg)
            continue
        if rid is not None and msg.get("id") == rid:
            return ("answer", msg)


def ask_permission(session_id, title):
    STATE["reverse"] += 1
    rid = "perm-%d" % STATE["reverse"]
    send({"jsonrpc": "2.0", "id": rid, "method": "session/request_permission", "params": {
        "sessionId": session_id,
        "toolCall": {"toolCallId": "tc-1", "title": title, "kind": "edit", "status": "pending"},
        "options": [
            {"optionId": "proceed_once", "name": "Allow once", "kind": "allow_once"},
            {"optionId": "always", "name": "Always allow", "kind": "allow_always"},
            {"optionId": "reject_once", "name": "Reject", "kind": "reject_once"}]}})
    kind, frame = await_frame(rid, None)
    if kind != "answer":
        return None
    return ((frame.get("result") or {}).get("outcome")) or {}


def new_session(rid, params):
    cwd = params.get("cwd")
    if not isinstance(cwd, str) or not cwd.startswith("/"):
        return bad(rid, -32602, "session/new needs an absolute cwd, got %r" % (cwd,))
    servers = params.get("mcpServers")
    if not isinstance(servers, list):
        return bad(rid, -32602, "session/new needs an mcpServers array")
    STATE["sessions"] += 1
    session_id = "sess-%d" % STATE["sessions"]
    SESSIONS[session_id] = {"cwd": cwd, "mode": "default", "turns": 0,
                            "mcp": [s.get("name") for s in servers]}
    ok(rid, {
        "sessionId": session_id,
        "modes": {"currentModeId": "default", "availableModes": [
            {"id": "default", "name": "Default", "description": "ask first"},
            {"id": "acceptEdits", "name": "Accept edits", "description": "do not ask"}]},
        "models": {"currentModelId": "fmodel", "availableModels": [
            {"modelId": "fmodel", "name": "Fake model", "description": "the only one"}]},
        "configOptions": [
            {"id": "model", "name": "Model", "category": "model", "type": "select",
             "currentValue": "fmodel",
             "options": [{"value": "fmodel", "name": "Fake model"}]}],
        "_meta": {"fake": {"cwd": cwd, "mcpServers": SESSIONS[session_id]["mcp"]}}})


def prompt(rid, params):
    session_id = params.get("sessionId")
    if session_id not in SESSIONS:
        return bad(rid, -32602, "session %s is not open on this agent" % session_id)
    session = SESSIONS[session_id]
    blocks = params.get("prompt")
    if not isinstance(blocks, list) or not blocks:
        return bad(rid, -32602, "session/prompt needs a non-empty prompt array")
    text = "".join(b.get("text", "") for b in blocks if b.get("type") == "text")
    if not text:
        return bad(rid, -32602, "session/prompt carried no text block")
    session["turns"] += 1

    chunk(session_id, "user_message_chunk", text)
    chunk(session_id, "agent_thought_chunk", "thinking:")

    if "emit-garbage" in text:
        sys.stdout.write("{this is not json\n")
        sys.stdout.write("[1, 2, 3]\n")
        sys.stdout.flush()

    if "die" in text:
        sys.stderr.write("fake agent: about to vanish mid-turn\n")
        sys.stderr.flush()
        chunk(session_id, "agent_message_chunk", "partial answer")
        os._exit(3)

    if "await-cancel" in text:
        update(session_id, {"sessionUpdate": "tool_call", "toolCallId": "tc-9",
                            "title": "long run", "status": "in_progress"})
        kind, _ = await_frame(None, rid)
        if kind == "cancel_request":
            # The client withdrew this JSON-RPC request: the only right answer is
            # the reserved cancellation error, not a result.
            bad(rid, -32800, "Request cancelled")
        else:
            # `session/cancel`: the turn stops, and stops as a result.
            ok(rid, {"stopReason": "cancelled"})
        return

    if "ask-permission" in text:
        outcome = ask_permission(session_id, "write ./notes.md")
        if outcome is None:
            return bad(rid, -32800, "Request cancelled")
        if outcome.get("outcome") != "selected":
            chunk(session_id, "agent_message_chunk", "no decision")
            return ok(rid, {"stopReason": "cancelled"})
        if outcome.get("optionId") == "reject_once":
            chunk(session_id, "agent_message_chunk", "declined")
            return ok(rid, {"stopReason": "refusal",
                            "_meta": {"permission": "denied"}})
        update(session_id, {"sessionUpdate": "tool_call_update", "toolCallId": "tc-1",
                            "status": "completed"})
        chunk(session_id, "agent_message_chunk",
              "written via %s" % outcome.get("optionId"))
        return ok(rid, {"stopReason": "end_turn", "userMessageId": "u-1",
                        "usage": {"totalTokens": 12}})

    if "ask-unknown" in text:
        STATE["reverse"] += 1
        prid = "term-%d" % STATE["reverse"]
        send({"jsonrpc": "2.0", "id": prid, "method": "terminal/wait", "params": {
            "sessionId": session_id, "terminalId": "t-1"}})
        kind, frame = await_frame(prid, rid)
        code = (frame.get("error") or {}).get("code") if kind == "answer" else None
        if code != -32601:
            chunk(session_id, "agent_message_chunk", "client answered %s" % json.dumps(frame))
            return bad(rid, -32000, "client did not answer -32601")
        chunk(session_id, "agent_message_chunk", "fallback")
        return ok(rid, {"stopReason": "end_turn"})

    chunk(session_id, "agent_message_chunk",
          "echo[%s]:%s" % (session["mode"], text))
    update(session_id, {"sessionUpdate": "plan", "entries": [
        {"content": "wrap up", "status": "completed", "priority": "high"}]})
    update(session_id, {"sessionUpdate": "available_commands_update", "availableCommands": []})
    ok(rid, {"stopReason": "end_turn", "userMessageId": "u-%d" % session["turns"],
             "usage": {"totalTokens": len(text)},
             "_meta": {"fake": {"cwd": session["cwd"]}}})


def dispatch(msg):
    method = msg.get("method")
    rid = msg.get("id")
    params = msg.get("params") or {}
    if method == "initialize":
        return ok(rid, {
            "protocolVersion": 1,
            "agentInfo": {"name": "fake-acp-agent", "title": "Fake ACP Agent",
                          "version": "1.0.0"},
            "authMethods": [{"id": "fake-login", "name": "Fake login",
                             "description": "for tests"}],
            "agentCapabilities": {
                "loadSession": False,
                "sessionCapabilities": {"close": {}},
                "promptCapabilities": {"image": True, "embeddedContext": True},
                "mcpCapabilities": {"http": True, "sse": False}},
            # What the process says about itself, so the caller can check the
            # spawn actually put it in its own process group.
            "_meta": {"fake": {
                "pid": os.getpid(),
                "pgid": os.getpgid(0) if hasattr(os, "getpgid") else None,
                "cwd": os.getcwd()}}})
    if method == "session/new":
        return new_session(rid, params)
    if method == "session/prompt":
        return prompt(rid, params)
    if method == "session/set_mode":
        session = SESSIONS.get(params.get("sessionId"))
        if session is None:
            return bad(rid, -32602, "unknown session")
        session["mode"] = params.get("modeId")
        return ok(rid, {})
    if method == "session/set_config_option":
        session = SESSIONS.get(params.get("sessionId"))
        if session is None:
            return bad(rid, -32602, "unknown session")
        if params.get("configId") not in ("model", "mode", "reasoning_effort"):
            return bad(rid, -32602, "unknown config option %s" % params.get("configId"))
        session.setdefault("config", {})[params["configId"]] = params.get("value")
        return ok(rid, {"configOptions": []})
    if method == "session/close":
        if SESSIONS.pop(params.get("sessionId"), None) is None:
            return bad(rid, -32602, "session %s is not open" % params.get("sessionId"))
        return ok(rid, {})
    if method in ("session/cancel", "$/cancel_request"):
        return None
    if rid is not None:
        return bad(rid, -32601, "fake agent does not implement %s" % method)


def main():
    sys.stderr.write("fake agent: started, pid %d\n" % os.getpid())
    sys.stderr.flush()
    while True:
        msg = read_message()
        if msg is None:
            wait_for_stdin()
        if msg:
            dispatch(msg)


main()
"##;

/// One tempdir per agent, holding its script; kept alive by the caller so the
/// child's cwd and script path do not vanish mid-run.
struct FakeAgent {
    root: TempDir,
    script: PathBuf,
}

impl FakeAgent {
    fn new() -> FakeAgent {
        let root = TempDir::new().expect("a temp workspace for the fake agent");
        let script = root.path().join("fake_agent.py");
        std::fs::write(&script, FAKE_AGENT).expect("write the fake agent script");
        FakeAgent { root, script }
    }

    fn dir(&self) -> &Path {
        self.root.path()
    }

    /// argv, not a shell string: this crate spawns a process and hands it stdin,
    /// stdout and stderr pipes. `-u` keeps python from buffering a frame the test
    /// is waiting on.
    fn options(&self) -> AgentOptions {
        AgentOptions {
            command: vec![
                "python3".to_string(),
                "-u".to_string(),
                self.script.display().to_string(),
            ],
            cwd: Some(self.dir().to_path_buf()),
            env: BTreeMap::new(),
        }
    }
}

fn start(fake: &FakeAgent) -> Agent {
    match Agent::start(fake.options()) {
        Ok(agent) => agent,
        Err(error) => panic!("start the fake agent (is python3 on PATH?): {error}"),
    }
}

/// Start with an extra environment variable. Only the unix escalation case needs
/// one, so this stays out of the way elsewhere.
#[cfg(unix)]
fn start_with_env(fake: &FakeAgent, vars: &[(&str, &str)]) -> Agent {
    let mut options = fake.options();
    for (key, value) in vars {
        options.env.insert((*key).to_string(), (*value).to_string());
    }
    match Agent::start(options) {
        Ok(agent) => agent,
        Err(error) => panic!("start the fake agent: {error}"),
    }
}

/// A directory inside the fake agent's own, for a second session's cwd.
fn subdir(root: &Path, name: &str) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).expect("create a session cwd");
    dir
}

fn initialize(agent: &Agent) -> AgentCapabilities {
    agent
        .initialize(
            ClientInfo::new("onlyne-acp-test"),
            ClientCapabilities::default(),
        )
        .expect("the fake answers initialize")
}

/// subscribe() before initialize(): both orders work, and this is the order a
/// caller that must not miss an event uses.
fn ready(agent: &Agent) -> Receiver<Event> {
    let events = agent.subscribe();
    initialize(agent);
    events
}

fn open_session(agent: &Agent, cwd: &Path) -> SessionStart {
    agent
        .new_session(cwd, Vec::new())
        .expect("session/new succeeds on the fake")
}

fn say(agent: &Agent, session_id: &str, text: &str) -> Result<PromptOutcome, anyhow::Error> {
    agent.prompt(session_id, vec![ContentBlock::text(text)])
}

fn next_event(events: &Receiver<Event>) -> Event {
    match events.recv_timeout(EVENT_WAIT) {
        Ok(event) => event,
        Err(error) => panic!("no event within the test's own bound ({error:?}): {EVENT_WAIT:?}"),
    }
}

/// An update stream: `(session id, sessionUpdate, flattened text)` in arrival
/// order. Order is the contract a transcript depends on, so it is asserted as a
/// list rather than as membership.
type Stream = Vec<(String, String, Option<String>)>;

fn kind_of(update: &Update) -> String {
    update
        .raw()
        .get("sessionUpdate")
        .and_then(Value::as_str)
        .unwrap_or("<missing>")
        .to_owned()
}

fn entry(session_id: String, update: Update) -> (String, String, Option<String>) {
    (
        session_id,
        kind_of(&update),
        update.text().map(str::to_owned),
    )
}

/// Everything queued.
fn drained(events: &Receiver<Event>) -> Stream {
    let mut stream = Vec::new();
    while let Ok(event) = events.try_recv() {
        match event {
            Event::Update { session_id, update } => stream.push(entry(session_id, update)),
            other => panic!("a scripted turn emits no permission request and no exit: {other:?}"),
        }
    }
    stream
}

fn kinds(stream: &Stream) -> Vec<&str> {
    stream.iter().map(|(_, kind, _)| kind.as_str()).collect()
}

fn texts(stream: &Stream) -> Vec<&str> {
    stream
        .iter()
        .flat_map(|(_, _, text)| text.as_ref())
        .map(String::as_str)
        .collect()
}

fn session_ids(stream: &Stream) -> Vec<&str> {
    stream
        .iter()
        .map(|(session_id, _, _)| session_id.as_str())
        .collect()
}

/// Read until an update with this discriminator arrives, skipping the ones the
/// test is not interested in.
fn wait_for_update(events: &Receiver<Event>, kind: &str) -> Update {
    loop {
        match next_event(events) {
            Event::Update { update, .. } => {
                if kind_of(&update) == kind {
                    return update;
                }
            }
            Event::Permission(_) => continue,
            Event::Exited { detail } => panic!("the agent left before {kind}: {detail}"),
        }
    }
}

/// Read until the agent asks for permission, returning the request together with
/// the updates that arrived before it: a turn streams and then parks, and which
/// side of the request a chunk fell on is what a caller renders.
fn wait_for_permission(events: &Receiver<Event>) -> (PermissionRequest, Stream) {
    let mut before = Stream::new();
    loop {
        match next_event(events) {
            Event::Permission(request) => return (request, before),
            Event::Update { session_id, update } => before.push(entry(session_id, update)),
            Event::Exited { detail } => {
                panic!("the agent left before asking permission: {detail}")
            }
        }
    }
}

fn wait_for_exit(events: &Receiver<Event>) -> String {
    loop {
        match next_event(events) {
            Event::Exited { detail } => return detail,
            Event::Update { .. } | Event::Permission(_) => continue,
        }
    }
}

/// The handshake order, the subscriber gate, the session-membership rule and the
/// argument checks are protocol correctness, not policy: each must refuse before
/// the wire is touched, rather than leaving an agent confused by a stray request.
#[test]
fn protocol_order_is_enforced_at_the_door() {
    let fake = FakeAgent::new();
    let agent = start(&fake);
    let work = fake.dir();

    let error = agent
        .new_session(work, Vec::new())
        .expect_err("session/new before the handshake is refused");
    assert!(error.to_string().contains("initialize"), "{error}");
    assert!(
        agent
            .prompt("sess-1", vec![ContentBlock::text("hi")])
            .expect_err("nothing can be prompted for before the handshake either")
            .to_string()
            .contains("initialize"),
    );
    // Cancellation is the escape hatch: gating it on a handshake would defeat it.
    agent
        .cancel("sess-never-opened")
        .expect("session/cancel is a notification and stays open");

    let negotiated = initialize(&agent);
    assert_eq!(negotiated.protocol_version, PROTOCOL_VERSION);
    assert_eq!(
        negotiated.agent_info.get("name").and_then(Value::as_str),
        Some("fake-acp-agent")
    );
    assert!(!negotiated.load_session);
    assert!(negotiated.supports_close());
    assert_eq!(negotiated.auth_methods.len(), 1);
    assert_eq!(agent.negotiated().map(|c| c.protocol_version), Some(1));

    let error = agent
        .initialize(ClientInfo::new("again"), ClientCapabilities::default())
        .expect_err("one process has one negotiated capability set");
    assert!(error.to_string().contains("already completed"), "{error}");

    let session = open_session(&agent, work);

    // The order of the local refusals is the order the checks appear in `prompt`:
    // a session this process never opened is refused as itself, and a real session
    // with nobody listening is refused for the reason the caller can act on.
    let error = agent
        .prompt(&session.session_id, vec![ContentBlock::text("hi")])
        .expect_err("a prompt with no subscriber is refused before it is sent");
    assert!(error.to_string().contains("subscribe()"), "{error}");
    let events = agent.subscribe();

    let error = agent
        .prompt("sess-9", vec![ContentBlock::text("hi")])
        .expect_err("an id this process never handed out is refused locally");
    assert!(
        error.to_string().contains("never reported that id"),
        "{error}"
    );
    for refusal in [
        agent.set_mode("sess-9", "default"),
        agent.set_config_option("sess-9", "model", "m"),
        agent.close_session("sess-9"),
    ] {
        let error = refusal.expect_err("the same rule covers a mode change and a close");
        assert!(
            error.to_string().contains("never reported that id"),
            "{error}"
        );
    }

    let error = agent
        .new_session(Path::new("relative/tree"), Vec::new())
        .expect_err("a relative cwd means something else on the far side of the pipe");
    assert!(error.to_string().contains("absolute"), "{error}");

    let error = agent
        .prompt(&session.session_id, Vec::new())
        .expect_err("an empty prompt is a request the agent cannot answer");
    assert!(
        error.to_string().contains("at least one content block"),
        "{error}"
    );
    assert!(
        drained(&events).is_empty(),
        "every refusal above happened without touching the wire"
    );

    agent.shutdown().expect("the fake leaves when stdin closes");
}

/// One process, two sessions. The ids come from the agent, the routing table is
/// keyed by them, and each turn's stream stays attributed to its own session.
#[test]
fn two_sessions_share_one_process_and_keep_their_own_streams() {
    let fake = FakeAgent::new();
    let agent = start(&fake);
    let events = ready(&agent);

    let first_dir = subdir(fake.dir(), "first");
    let second_dir = subdir(fake.dir(), "second");
    let first = agent
        .new_session(
            &first_dir,
            vec![McpServer::Stdio {
                name: "fs".to_string(),
                command: "/bin/echo".to_string(),
                args: vec!["--stdio".to_string()],
                env: vec![EnvVariable {
                    name: "KEY".to_string(),
                    value: "v".to_string(),
                }],
            }],
        )
        .expect("first session/new");
    let second = agent
        .new_session(&second_dir, Vec::new())
        .expect("second session/new");

    assert_eq!(first.session_id, "sess-1");
    assert_eq!(second.session_id, "sess-2");
    assert_eq!(
        first.result["_meta"]["fake"]["cwd"],
        json!(first_dir.display().to_string()),
        "the fake refuses a cwd that is not absolute, so getting this far proves the shape"
    );
    assert_eq!(first.result["_meta"]["fake"]["mcpServers"], json!(["fs"]));
    assert_eq!(
        second.result["_meta"]["fake"]["cwd"],
        json!(second_dir.display().to_string())
    );
    assert_eq!(
        second.result["_meta"]["fake"]["mcpServers"],
        json!([]),
        "an empty server list must serialize as an array, not be omitted"
    );
    // The result reaches the caller whole: modes, models and config options are
    // the caller's to read, not this crate's to model.
    assert_eq!(first.current_mode(), Some("default"));
    assert_eq!(first.current_model(), Some("fmodel"));
    assert_eq!(
        first.result["modes"]["availableModes"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    assert_eq!(first.result["configOptions"][0]["id"], json!("model"));

    say(&agent, &second.session_id, "two").expect("second turn");
    let stream = drained(&events);
    assert_eq!(
        stream,
        vec![
            (
                "sess-2".to_string(),
                "user_message_chunk".to_string(),
                Some("two".to_string())
            ),
            (
                "sess-2".to_string(),
                "agent_thought_chunk".to_string(),
                Some("thinking:".to_string())
            ),
            (
                "sess-2".to_string(),
                "agent_message_chunk".to_string(),
                Some("echo[default]:two".to_string())
            ),
            ("sess-2".to_string(), "plan".to_string(), None),
            (
                "sess-2".to_string(),
                "available_commands_update".to_string(),
                None
            ),
        ],
        "the stream is exactly the frames the fake sent, in order, for one session"
    );

    say(&agent, &first.session_id, "one").expect("first turn");
    let stream = drained(&events);
    assert_eq!(session_ids(&stream), vec!["sess-1"; 5]);
    assert_eq!(
        texts(&stream),
        vec!["one", "thinking:", "echo[default]:one"]
    );

    // Closing one session leaves the other usable, and the closed id stops being
    // ours to route: the refusal is local, so the agent never sees a stray request.
    agent
        .close_session("sess-1")
        .expect("the fake advertises session/close");
    let error = say(&agent, "sess-1", "gone").expect_err("a closed session is not routable");
    assert!(
        error.to_string().contains("never reported that id"),
        "{error}"
    );
    assert!(
        say(&agent, "sess-2", "still here")
            .expect("the survivor turns")
            .is_end_turn()
    );
    assert_eq!(
        texts(&drained(&events)),
        vec!["still here", "thinking:", "echo[default]:still here"]
    );

    agent
        .shutdown()
        .expect("one process served both sessions and left once");
}

/// A whole turn: the streamed chunks in arrival order, then the stop reason and
/// the raw result. This is the pair the caller renders, so neither half may be
/// lost or reordered behind the other.
#[test]
fn a_prompt_turn_streams_updates_in_order_and_returns_the_stop_reason() {
    let fake = FakeAgent::new();
    let agent = start(&fake);
    let events = ready(&agent);
    let session = open_session(&agent, fake.dir());

    let outcome = agent
        .prompt(
            &session.session_id,
            vec![ContentBlock::text("ping"), ContentBlock::text("pong")],
        )
        .expect("the turn ends");

    assert_eq!(outcome.stop_reason, PromptOutcome::END_TURN);
    assert!(outcome.is_end_turn());
    assert!(!outcome.is_cancelled());
    // Both text blocks went out in one prompt array, in order: the fake joins them.
    assert_eq!(outcome.result["usage"]["totalTokens"], json!(8));
    assert_eq!(outcome.result["userMessageId"], json!("u-1"));
    assert_eq!(
        outcome.result["_meta"]["fake"]["cwd"],
        json!(fake.dir().display().to_string())
    );

    let stream = drained(&events);
    assert_eq!(
        stream,
        vec![
            (
                "sess-1".to_string(),
                "user_message_chunk".to_string(),
                Some("pingpong".to_string())
            ),
            (
                "sess-1".to_string(),
                "agent_thought_chunk".to_string(),
                Some("thinking:".to_string())
            ),
            (
                "sess-1".to_string(),
                "agent_message_chunk".to_string(),
                Some("echo[default]:pingpong".to_string())
            ),
            ("sess-1".to_string(), "plan".to_string(), None),
            (
                "sess-1".to_string(),
                "available_commands_update".to_string(),
                None
            ),
        ],
        "chunk text is flattened and unmodelled kinds still arrive"
    );
    assert!(
        matches!(&stream[2], (_, kind, _) if kind == "agent_message_chunk"),
        "the answer chunk is the one a caller appends to the transcript"
    );
    assert_eq!(
        agent.outstanding().len(),
        0,
        "the turn is over, so nothing is parked on the agent"
    );

    // The same session serves the next turn; the fake counts turns per session.
    let next = say(&agent, &session.session_id, "again").expect("second turn");
    assert_eq!(next.result["userMessageId"], json!("u-2"));
    assert_eq!(
        texts(&drained(&events)),
        vec!["again", "thinking:", "echo[default]:again"]
    );

    agent.shutdown().expect("shutdown");
}

/// `session/request_permission` parks the agent's turn until the client answers.
/// This is the one place where a caller can strand a turn, so the round trip has
/// to be proven against a process that really is waiting.
#[test]
fn a_permission_request_parks_the_turn_until_the_caller_answers() {
    let fake = FakeAgent::new();
    let agent = start(&fake);
    let events = ready(&agent);
    let session = open_session(&agent, fake.dir());

    std::thread::scope(|scope| {
        let turn = scope.spawn(|| {
            say(
                &agent,
                &session.session_id,
                "ask-permission write the notes",
            )
        });
        let (request, before) = wait_for_permission(&events);

        // The turn was already streaming before it parked on the question.
        assert_eq!(
            before,
            vec![
                (
                    "sess-1".to_string(),
                    "user_message_chunk".to_string(),
                    Some("ask-permission write the notes".to_string())
                ),
                (
                    "sess-1".to_string(),
                    "agent_thought_chunk".to_string(),
                    Some("thinking:".to_string())
                ),
            ],
            "the chunks before the request are part of the same turn"
        );
        assert_eq!(request.request_id, RequestId::Text("perm-1".to_string()));
        assert_eq!(request.session_id, "sess-1");
        assert_eq!(request.tool_call["toolCallId"], json!("tc-1"));
        assert_eq!(request.tool_call["kind"], json!("edit"));
        assert_eq!(request.options.len(), 3);
        let allow = request
            .option(PermissionOption::ALLOW_ONCE)
            .expect("the fake offers an allow_once option");
        assert_eq!(allow.option_id, "proceed_once");
        assert_eq!(allow.name, "Allow once");
        assert!(!allow.is_reject());
        assert_eq!(
            request
                .option(PermissionOption::REJECT_ONCE)
                .map(|option| option.name.as_str()),
            Some("Reject"),
            "the caller is shown a way out alongside the way in"
        );
        assert_eq!(
            agent.outstanding().len(),
            1,
            "the prompt is parked, and a caller can see it is"
        );

        let option_id = allow.option_id.clone();
        agent
            .answer(
                request.request_id.clone(),
                PermissionOutcome::Selected { option_id },
            )
            .expect("the answer goes out on the wire");

        let outcome = turn
            .join()
            .expect("the turn thread survives")
            .expect("the turn finishes once it is answered");
        assert_eq!(outcome.stop_reason, PromptOutcome::END_TURN);
        assert_eq!(outcome.result["userMessageId"], json!("u-1"));

        // The whole turn in one order: what streamed before the question, and what
        // the answer let through afterwards.
        let stream = [before, drained(&events)].concat();
        assert_eq!(
            kinds(&stream),
            vec![
                "user_message_chunk",
                "agent_thought_chunk",
                // The completion update only exists because the answer was accepted.
                "tool_call_update",
                "agent_message_chunk",
            ]
        );
        assert_eq!(
            texts(&stream),
            vec![
                "ask-permission write the notes",
                "thinking:",
                "written via proceed_once",
            ]
        );
        assert!(
            agent.outstanding().is_empty(),
            "the answer released the parked prompt"
        );
    });

    agent.shutdown().expect("shutdown");
}

/// A rejection is an answer too: the agent must get a valid outcome object and
/// end the turn its own way, not be left parked.
#[test]
fn a_declined_permission_ends_the_turn_as_refusal() {
    let fake = FakeAgent::new();
    let agent = start(&fake);
    let events = ready(&agent);
    let session = open_session(&agent, fake.dir());

    std::thread::scope(|scope| {
        let turn = scope.spawn(|| say(&agent, &session.session_id, "ask-permission"));
        let (request, before) = wait_for_permission(&events);
        let reject = request
            .rejection_option()
            .expect("a permission request always carries a way out");
        assert_eq!(reject.kind, PermissionOption::REJECT_ONCE);
        assert_eq!(reject.option_id, "reject_once");
        assert!(reject.is_reject());

        let request_id = request.request_id.clone();
        let option_id = reject.option_id.clone();
        agent
            .answer(request_id, PermissionOutcome::Selected { option_id })
            .expect("the decline goes out on the wire");

        let outcome = turn
            .join()
            .expect("no panic")
            .expect("a declined turn still comes back");
        assert_eq!(outcome.stop_reason, PromptOutcome::REFUSAL);
        assert_eq!(outcome.result["_meta"]["permission"], json!("denied"));
        assert_eq!(
            texts(&[before, drained(&events)].concat()),
            vec!["ask-permission", "thinking:", "declined"]
        );
    });

    agent.shutdown().expect("shutdown");
}

/// The `-32601` fallback is what keeps a client that does not proxy terminals or
/// files from hanging an agent that asks for one.
#[test]
fn an_unmodelled_agent_request_is_answered_method_not_found() {
    let fake = FakeAgent::new();
    let agent = start(&fake);
    let events = ready(&agent);
    let session = open_session(&agent, fake.dir());

    // The fake only ends this turn if its `terminal/wait` request came back as
    // -32601; any other answer makes it fail the turn with what it received.
    let outcome =
        say(&agent, &session.session_id, "ask-unknown").expect("the fake accepted the refusal");
    assert!(outcome.is_end_turn());
    assert_eq!(
        texts(&drained(&events)),
        vec!["ask-unknown", "thinking:", "fallback"]
    );

    agent.shutdown().expect("shutdown");
}

/// One unparsable line is not a reason to lose a live agent: the frame is skipped
/// and the stream keeps its framing.
#[test]
fn a_malformed_frame_is_skipped_and_the_turn_continues() {
    let fake = FakeAgent::new();
    let agent = start(&fake);
    let events = ready(&agent);
    let session = open_session(&agent, fake.dir());

    let outcome = say(&agent, &session.session_id, "emit-garbage").expect("two junk frames");
    assert!(outcome.is_end_turn());
    assert_eq!(
        kinds(&drained(&events)),
        vec![
            "user_message_chunk",
            "agent_thought_chunk",
            "agent_message_chunk",
            "plan",
            "available_commands_update",
        ],
        "the junk frames are dropped and every real one still gets through"
    );

    agent.shutdown().expect("shutdown");
}

/// A parked caller can be released by name. Without `outstanding` and
/// `cancel_request` a wedged agent is a wedged thread.
#[test]
fn a_parked_prompt_is_released_by_cancel_request() {
    let fake = FakeAgent::new();
    let agent = start(&fake);
    let events = ready(&agent);
    let session = open_session(&agent, fake.dir());

    std::thread::scope(|scope| {
        let turn = scope.spawn(|| say(&agent, &session.session_id, "await-cancel"));
        let tool_call = wait_for_update(&events, "tool_call");
        assert_eq!(tool_call.tool_call_id(), Some("tc-9"));
        assert!(matches!(tool_call, Update::ToolCall(_)));

        let parked = agent.outstanding();
        assert_eq!(
            parked.len(),
            1,
            "the parked prompt is the one outstanding request"
        );
        assert!(matches!(parked[0], RequestId::Number(_)));
        agent
            .cancel_request(parked[0].clone())
            .expect("the notification goes out");

        let error = turn
            .join()
            .expect("no panic")
            .expect_err("the fake answers a cancelled request with -32800");
        let rpc = error
            .downcast_ref::<RpcError>()
            .expect("an agent error stays recoverable as a typed RpcError");
        assert_eq!(rpc.code, RpcError::REQUEST_CANCELLED);
        assert_eq!(rpc.message, "Request cancelled");
    });

    assert!(
        agent.outstanding().is_empty(),
        "the request is forgotten once answered"
    );
    // The pipe survived the cancellation: the session is still usable.
    assert!(
        say(&agent, &session.session_id, "alive?")
            .expect("next turn")
            .is_end_turn()
    );
    agent.shutdown().expect("shutdown");
}

/// Death mid-turn is the failure an operator actually sees. The parked caller must
/// come back with the exit status and the stderr that explains it, and every
/// subscriber must be told once.
#[test]
fn an_agent_that_dies_mid_turn_fails_the_caller_with_its_exit_and_stderr() {
    let fake = FakeAgent::new();
    let agent = start(&fake);
    let events = ready(&agent);
    let session = open_session(&agent, fake.dir());

    let error = say(&agent, &session.session_id, "please die")
        .expect_err("a dead agent cannot finish the turn");
    let detail = error.to_string();
    assert!(detail.contains("exit status: 3"), "{detail}");
    assert!(
        detail.contains("about to vanish mid-turn"),
        "the stderr tail is what makes this diagnosable, and it must be in the \
         error the caller gets: {detail}"
    );

    let reported = wait_for_exit(&events);
    assert!(reported.contains("exit status: 3"), "{reported}");
    assert!(agent.is_gone());
    assert!(
        agent
            .prompt(&session.session_id, vec![ContentBlock::text("again")])
            .expect_err("nothing is left to prompt")
            .to_string()
            .contains("exit status: 3"),
    );
    // Late subscribers get the fact instead of blocking forever on a dead stream.
    let late = agent.subscribe();
    assert!(matches!(next_event(&late), Event::Exited { .. }));
    assert!(
        matches!(events.try_recv(), Err(TryRecvError::Empty)),
        "the exit is reported to each subscriber exactly once"
    );
    assert!(
        agent.shutdown().is_ok(),
        "shutting down an agent that already left is not an error"
    );
}

/// The escalation is the only thing standing between a wedged agent and a leaked
/// process, so the order stdin-close, SIGTERM, and confirmation must hold.
#[test]
#[cfg(unix)]
fn shutdown_escalates_to_sigterm_when_end_of_stdin_is_ignored() {
    let fake = FakeAgent::new();
    let agent = start_with_env(&fake, &[("FAKE_AGENT_IGNORE_EOF", "1")]);
    let events = agent.subscribe();

    let started = Instant::now();
    agent
        .shutdown()
        .expect("the escalation reaches the process");
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(300),
        "the graceful end-of-stdin window was skipped: {elapsed:?}"
    );

    let detail = wait_for_exit(&events);
    assert!(detail.contains("signal: 15"), "{detail}");
}

/// A caller that keeps its own thread alive and wants out uses SIGTERM; a caller
/// that wants the turn to stop without abandoning the session uses `cancel`.
#[test]
fn a_cancelled_session_notification_reaches_the_agent() {
    let fake = FakeAgent::new();
    let agent = start(&fake);
    let events = ready(&agent);
    let session = open_session(&agent, fake.dir());

    std::thread::scope(|scope| {
        let turn = scope.spawn(|| say(&agent, &session.session_id, "await-cancel"));
        wait_for_update(&events, "tool_call");
        agent
            .cancel(&session.session_id)
            .expect("session/cancel is fire and forget");
        let outcome = turn
            .join()
            .expect("no panic")
            .expect("the fake ends the turn cancelled");
        assert_eq!(outcome.stop_reason, PromptOutcome::CANCELLED);
        assert!(outcome.is_cancelled());
    });

    agent.shutdown().expect("shutdown");
}

/// The process group is why the escalation works: a shell wrapper that spawned
/// the real agent would otherwise survive its own death. The fake reports its own
/// group id, so this checks the spawn flag from the far side of the pipe.
#[test]
#[cfg(unix)]
fn the_agent_leads_its_own_process_group() {
    let fake = FakeAgent::new();
    let agent = start(&fake);
    let events = ready(&agent);
    let negotiated = agent.negotiated().expect("the handshake completed");

    let pid = agent.pid();
    assert_ne!(
        pid,
        std::process::id(),
        "the agent is a child, not this process"
    );
    assert_eq!(
        agent.process_group(),
        Some(pid),
        "the client's view: the child is its own group leader"
    );
    assert_eq!(
        negotiated.raw["_meta"]["fake"]["pgid"].as_u64(),
        Some(u64::from(pid)),
        "the child's view: getpgid(0) inside the agent names its own pid"
    );
    assert_eq!(
        negotiated.raw["_meta"]["fake"]["pid"].as_u64(),
        Some(u64::from(pid))
    );
    // macOS hands out `/var/...` for a tempdir and reports `/private/var/...` from
    // `getcwd`, so the comparison is made on resolved paths.
    let work = std::fs::canonicalize(fake.dir()).expect("resolve the session cwd");
    assert_eq!(
        negotiated.raw["_meta"]["fake"]["cwd"],
        json!(work.display().to_string()),
        "the requested cwd took effect"
    );
    assert!(!agent.is_gone());
    assert!(
        drained(&events).is_empty(),
        "the handshake negotiates; it streams nothing"
    );

    agent.shutdown().expect("shutdown");
}

/// A mode or a configuration choice is a request, and the agent's answer is what
/// the next turn reflects.
#[test]
fn mode_and_config_choices_reach_the_agent_and_show_up_in_the_next_turn() {
    let fake = FakeAgent::new();
    let agent = start(&fake);
    let events = ready(&agent);
    let session = open_session(&agent, fake.dir());
    assert_eq!(session.current_mode(), Some("default"));

    agent
        .set_mode(&session.session_id, "acceptEdits")
        .expect("the fake accepts a mode it advertises");
    let outcome = say(&agent, &session.session_id, "now what").expect("turn after the mode change");
    assert!(outcome.is_end_turn());
    assert_eq!(
        texts(&drained(&events)),
        vec!["now what", "thinking:", "echo[acceptEdits]:now what"],
        "the mode change was applied by the agent, not just acknowledged"
    );

    agent
        .set_config_option(&session.session_id, "model", "fmodel")
        .expect("the fake knows the model option from configOptions");
    let error = agent
        .set_config_option(&session.session_id, "temperature", "9")
        .expect_err("an option the fake does not list comes back as invalid params");
    let rpc = error
        .downcast_ref::<RpcError>()
        .expect("an agent's refusal is a typed JSON-RPC error");
    assert_eq!(rpc.code, RpcError::INVALID_PARAMS);
    assert!(
        rpc.message.contains("unknown config option"),
        "{}",
        rpc.message
    );

    agent.shutdown().expect("shutdown");
}

/// A spawn that cannot happen must be reported, not left as a hanging process or
/// a half-built agent.
#[test]
fn a_program_that_cannot_be_spawned_is_an_error_not_a_panic() {
    let error = Agent::start(AgentOptions {
        command: vec!["onlyne-acp-does-not-exist".to_string()],
        cwd: None,
        env: BTreeMap::new(),
    })
    .expect_err("a missing program cannot start an agent");
    assert!(
        error.to_string().contains("onlyne-acp-does-not-exist"),
        "{error}"
    );

    Agent::start(AgentOptions {
        command: Vec::new(),
        cwd: None,
        env: BTreeMap::new(),
    })
    .expect_err("empty argv has nothing to run");
}
