#!/usr/bin/env python3
"""A scripted ACP v1 agent that reaches its session's tools over MCP (case 20).

The case runs this file as the role's `[client.runtime] command`, with the
spec's `drive = "acp"` giving the pair its meaning, exactly as `acp-agent.py` is
run for case 18:

    python3 -u acp-tools-agent.py --acp --gate <path> --trace <path>

Where that fixture proves the ACP conversation itself, this one proves the
session's *tools mount* — the connection an ACP session has instead of a
plugin's registered functions (`docs/v2-CONTRACT.md` §3b):

* `session/new` hands this agent its `mcpServers` list. The fixture starts the
  entry it finds there itself — its `command`, its `args`, and its `env` added to
  this process's environment — because an ACP stdio server is the agent's child,
  not the client's. The entry, the child's pid, and whether this process's own
  environment carries the token are all traced, so the case can prove who
  started what and that the token rides in the mount's environment alone.
* it then speaks MCP to that child over stdio: `initialize`,
  `notifications/initialized`, `tools/list`, and `tools/call`. Every request and
  every answer is traced as it was parsed, so a case assertion reads a real
  handshake and a real result rather than the presence of bytes on a pipe.
* a `tools/call` for `onlyne_complete` settles the task the session serves.
  Before it, a *second* bridge started with a tampered token makes the same call
  and must settle nothing; after it, the same bridge calls once more and must be
  refused, because a token dies with the delivery it names.
* the gate is held between the refused call and the settling one, so the case can
  read the ledger while the refusal is provably the last thing that happened.
  The wait is bounded, so a case that never releases the gate reports instead of
  hanging.

The token is a capability, so it never reaches this file's trace: the mount is
traced with the token's value replaced by its length, and the child receives the
real value from the entry.

`--acp` is required and carries no meaning of its own: it is the flag the
runtime command spells, and a script that forgot it should fail here rather than
read a pipe it does not own.
"""

import argparse
import json
import os
import subprocess
import sys
import time

# The arguments this agent completes with, and the results it reads back.
# `acp-tools.sh` carries the same literals and checks them against the `start`
# trace line, so a drift between the two files fails at the gate with both sides
# in the message.
SUMMARY = "the tools mount settled this task"
DETAILS = "The fixture settled the task through onlyne mcp. Second line of the full result."
WRONG_SUMMARY = "a tampered token settled this task"
WRONG_DETAILS = "The fixture tried to settle through a tampered token."
RETIRED_SUMMARY = "a retired token settled this task"
RETIRED_DETAILS = "The fixture tried to settle a task after its token had retired."
ANSWER = "The fixture answered through its tools mount."
SESSION_ID = "acp-tools-1"
MOUNT_NAME = "onlyne"
COMPLETE_TOOL = "onlyne_complete"
TOOLS = ["onlyne_send", "onlyne_handoff", "onlyne_complete"]
MCP_PROTOCOL = "2025-06-18"
MCP_CLIENT = "acp-tools-agent"
# The two variables the mount carries. `ONLYNE_MCP_TOKEN` is the capability the
# client never puts in this process's environment; `ONLYNE_SOCKET` is where the
# child dials it.
SOCKET_VAR = "ONLYNE_SOCKET"
TOKEN_VAR = "ONLYNE_MCP_TOKEN"
# Appended to the session's own token for the second bridge: a value that names
# no session, so the client's door — not an agent's mistake — is what refuses it.
TOKEN_TAMPER = "-wrong"
# How long the child is given to leave once its stdin is closed.
BRIDGE_EXIT_SECONDS = 5.0

GATE_POLL_SECONDS = 0.05
# Long enough for a slow machine to complete the gated assertions, short enough
# that a case which never releases the gate still reports.
GATE_WAIT_SECONDS = 120.0

INITIALIZE_RESULT = {
    "protocolVersion": 1,
    "agentInfo": {
        "name": "acp-tools-agent",
        "title": "Onlyne ACP tools fixture",
        "version": "1.0.0",
    },
    "authMethods": [
        {
            "id": "fixture-login",
            "name": "Fixture login",
            "description": "the fixture needs no credential",
        }
    ],
    "agentCapabilities": {
        "loadSession": False,
        "sessionCapabilities": {"close": {}, "cancel": {}},
        "promptCapabilities": {"image": False, "audio": False, "embeddedContext": False},
    },
}

SESSION_NEW_RESULT = {
    "sessionId": SESSION_ID,
    "modes": {
        "currentModeId": "default",
        "availableModes": [
            {"id": "default", "name": "Default", "description": "ask before acting"},
        ],
    },
}


class FixtureError(Exception):
    """A step this fixture cannot continue past, traced before it leaves."""


class Trace:
    """The case's evidence file: one line per event, appended and flushed.

    A write that fails cannot be reported on stdout — that surface is the
    protocol — so it goes to stderr, where the client's drain keeps it, and the
    case fails on the missing line.
    """

    def __init__(self, path):
        self.path = path

    def write(self, line):
        try:
            with open(self.path, "a", encoding="utf-8") as handle:
                handle.write(line + "\n")
                handle.flush()
        except OSError as error:
            sys.stderr.write("acp-tools-agent: trace %s: %s\n" % (self.path, error))
            sys.stderr.flush()


def parse_args(argv):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--acp",
        action="store_true",
        required=True,
        help="speak ACP v1 on stdio: this process is the role's agent",
    )
    parser.add_argument(
        "--gate",
        required=True,
        help="file whose appearance releases the gated call sequence",
    )
    parser.add_argument(
        "--trace",
        required=True,
        help="file this process traces its pid, the mount and every MCP frame to",
    )
    return parser.parse_args(argv)


def send(frame):
    try:
        sys.stdout.write(json.dumps(frame) + "\n")
        sys.stdout.flush()
    except (BrokenPipeError, ValueError):
        # Nobody is left to answer: the client closed the pipe this agent owns.
        sys.exit(0)


def result(rid, value):
    if rid is None:
        # A notification has no id, so it has no answer either.
        return
    send({"jsonrpc": "2.0", "id": rid, "result": value})


def update(session_id, payload):
    """One `session/update` notification, in the shape the client's reader routes."""
    body = {"sessionId": session_id}
    body.update(payload)
    send({"jsonrpc": "2.0", "method": "session/update", "params": body})


def wait_for_gate(path, trace):
    trace.write("gate wait")
    deadline = time.monotonic() + GATE_WAIT_SECONDS
    while not os.path.exists(path):
        if time.monotonic() >= deadline:
            raise FixtureError("gate timeout after %.0fs" % GATE_WAIT_SECONDS)
        time.sleep(GATE_POLL_SECONDS)
    trace.write("gate open")


def child_environment(entry):
    """The environment one stdio mount's child runs with.

    ACP's `env` is a list of pairs the agent adds to its own environment for the
    server it starts, so the merge is this process's `environ` plus the entry's
    pairs, in the entry's order.
    """
    env = dict(os.environ)
    for pair in entry.get("env") or []:
        env[pair["name"]] = pair["value"]
    return env


def redacted(entry):
    """The mount as it is written down: the token's value never reaches a file.

    The token is a capability that speaks for one live session, so the trace
    keeps the pair's name and its length and nothing else. The case still proves
    a token rode there: the child this fixture starts with the real value is what
    the client's door answers.
    """
    copy = json.loads(json.dumps(entry))
    for pair in copy.get("env") or []:
        if pair.get("name") == TOKEN_VAR:
            pair["value"] = "<%d chars>" % len(pair.get("value", ""))
    return copy


def mount_of(servers):
    """The stdio entry `session/new` handed this agent, by the name it answers to."""
    if not isinstance(servers, list):
        raise FixtureError("session/new carried no mcpServers list: %r" % (servers,))
    for entry in servers:
        if (
            isinstance(entry, dict)
            and entry.get("type") == "stdio"
            and entry.get("name") == MOUNT_NAME
        ):
            return entry
    raise FixtureError("session/new carried no stdio entry named %s: %r" % (MOUNT_NAME, servers))


class Bridge:
    """One `onlyne mcp` child, spoken to over stdio as an MCP server."""

    def __init__(self, label, command, args, env, trace, mount_env=()):
        self.label = label
        self.trace = trace
        self.next_id = 0
        try:
            self.proc = subprocess.Popen(
                [command] + list(args),
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                env=env,
                text=True,
                bufsize=1,
            )
        except OSError as error:
            raise FixtureError("cannot start the mount command %s: %s" % (command, error))
        trace.write(
            "spawn label=%s pid=%d command=%s args=%s env=%s"
            % (
                label,
                self.proc.pid,
                command,
                json.dumps(list(args)),
                ",".join(pair["name"] for pair in mount_env),
            )
        )

    def send_frame(self, frame):
        try:
            self.proc.stdin.write(json.dumps(frame) + "\n")
            self.proc.stdin.flush()
        except (BrokenPipeError, ValueError):
            raise FixtureError("bridge %s closed its stdin" % self.label)

    def answer(self, rid):
        while True:
            line = self.proc.stdout.readline()
            if line == "":
                raise FixtureError("bridge %s closed its stdout before answer %s" % (self.label, rid))
            line = line.strip()
            if not line:
                continue
            try:
                message = json.loads(line)
            except ValueError:
                self.trace.write("mcp unparsable label=%s" % self.label)
                continue
            if message.get("id") == rid:
                return message

    def request(self, method, params):
        self.next_id += 1
        self.send_frame(
            {"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params}
        )
        answer = self.answer(self.next_id)
        if "error" in answer:
            error = answer.get("error") or {}
            self.trace.write(
                "mcp error label=%s id=%d method=%s code=%s message=%s"
                % (self.label, self.next_id, method, error.get("code"), error.get("message"))
            )
            raise FixtureError("bridge %s refused %s: %r" % (self.label, method, error))
        return answer

    def notify(self, method, params):
        self.send_frame({"jsonrpc": "2.0", "method": method, "params": params})

    def close(self):
        try:
            self.proc.stdin.close()
        except OSError:
            pass
        try:
            self.proc.wait(timeout=BRIDGE_EXIT_SECONDS)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()
        self.trace.write("exit label=%s status=%s" % (self.label, self.proc.returncode))


def handshake(bridge, trace):
    """The MCP handshake: `initialize`, the notification, then `tools/list`."""
    answer = bridge.request(
        "initialize",
        {
            "protocolVersion": MCP_PROTOCOL,
            "capabilities": {},
            "clientInfo": {"name": MCP_CLIENT, "version": "1.0.0"},
        },
    )
    result_body = answer.get("result") or {}
    server = result_body.get("serverInfo") or {}
    trace.write(
        "mcp initialize label=%s id=%d protocolVersion=%s capabilities=%s server=%s version=%s"
        % (
            bridge.label,
            bridge.next_id,
            result_body.get("protocolVersion"),
            json.dumps(result_body.get("capabilities"), sort_keys=True),
            server.get("name"),
            server.get("version"),
        )
    )
    bridge.notify("notifications/initialized", {})
    trace.write("mcp initialized label=%s" % bridge.label)
    answer = bridge.request("tools/list", {})
    tools = (answer.get("result") or {}).get("tools") or []
    complete = [tool for tool in tools if tool.get("name") == COMPLETE_TOOL]
    required = ((complete[0].get("inputSchema") or {}).get("required") or []) if complete else []
    trace.write(
        "mcp tools/list label=%s id=%d names=%s complete_required=%s"
        % (
            bridge.label,
            bridge.next_id,
            ",".join(tool.get("name", "") for tool in tools),
            ",".join(required),
        )
    )


def call_complete(bridge, trace, outcome, summary, details):
    """One `tools/call` for `onlyne_complete`, traced with the result it answered."""
    answer = bridge.request(
        "tools/call",
        {
            "name": COMPLETE_TOOL,
            "arguments": {"outcome": outcome, "summary": summary, "details": details},
        },
    )
    result_body = answer.get("result") or {}
    text = "".join(
        block.get("text", "")
        for block in result_body.get("content") or []
        if isinstance(block, dict)
    )
    is_error = bool(result_body.get("isError"))
    trace.write(
        "mcp call label=%s id=%d tool=%s outcome=%s isError=%s text=%s"
        % (bridge.label, bridge.next_id, COMPLETE_TOOL, outcome, str(is_error).lower(), text)
    )
    return is_error, text


def run_turn(rid, session_id, prompt, state, trace):
    """One turn: the refused call first, the settling one after the gate."""
    trace.write("session/prompt id=%s text=%s" % (session_id, prompt.replace("\n", " / ")))
    mount = state["mount"]
    # 1. A live session's token is the whole binding, so the same command with the
    #    same args and a tampered token reaches the client's door and is refused
    #    there — before any frame of this call is stamped with the session.
    tampered_env = dict(state["child_env"])
    tampered_env[TOKEN_VAR] = tampered_env.get(TOKEN_VAR, "") + TOKEN_TAMPER
    wrong = Bridge(
        "wrong",
        mount["command"],
        mount.get("args") or [],
        tampered_env,
        trace,
        mount.get("env") or [],
    )
    trace.write("token label=wrong tampered=true")
    handshake(wrong, trace)
    call_complete(wrong, trace, "failed", WRONG_SUMMARY, WRONG_DETAILS)
    wrong.close()
    # 2. The gate: the case reads the ledger here, with a refused call as the last
    #    thing that happened, and releases the settling call.
    wait_for_gate(state["gate"], trace)
    call_complete(state["bridge"], trace, "done", SUMMARY, DETAILS)
    # 3. The token died with the delivery it named, so the same bridge asking again
    #    is refused rather than allowed a second verdict.
    call_complete(state["bridge"], trace, "failed", RETIRED_SUMMARY, RETIRED_DETAILS)
    update(
        session_id,
        {
            "sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": ANSWER},
        },
    )
    trace.write("stop reason=end_turn")
    result(rid, {"stopReason": "end_turn"})


def dispatch(msg, state, trace):
    method = msg.get("method")
    rid = msg.get("id")
    params = msg.get("params") or {}
    if method == "initialize":
        client = params.get("clientInfo") or {}
        trace.write(
            "initialize protocolVersion=%s client=%s"
            % (params.get("protocolVersion"), client.get("name"))
        )
        result(rid, INITIALIZE_RESULT)
    elif method == "session/new":
        trace.write("session/new id=%s cwd=%s" % (SESSION_ID, params.get("cwd")))
        entry = mount_of(params.get("mcpServers"))
        trace.write("mount %s" % json.dumps(redacted(entry), sort_keys=True))
        state["mount"] = entry
        state["child_env"] = child_environment(entry)
        # An ACP stdio server is the agent's child: this process starts it here,
        # at the moment the session is opened, and never the client.
        state["bridge"] = Bridge(
            "session",
            entry["command"],
            entry.get("args") or [],
            state["child_env"],
            trace,
            entry.get("env") or [],
        )
        # The server it just started is initialized before this session answers:
        # the tools an ACP session serves are ready before its first prompt, so a
        # mount that cannot handshake fails the session here, where the case reads
        # it, rather than inside a turn.
        handshake(state["bridge"], trace)
        trace.write("token label=session tampered=false")
        result(rid, SESSION_NEW_RESULT)
    elif method == "session/prompt":
        blocks = params.get("prompt") or []
        prompt = "".join(
            block.get("text", "") for block in blocks if isinstance(block, dict)
        )
        run_turn(rid, params.get("sessionId"), prompt, state, trace)
    elif method == "session/close":
        trace.write("session/close id=%s" % params.get("sessionId"))
        result(rid, {})
    elif method in ("session/cancel", "$/cancel_request"):
        trace.write("%s id=%s" % (method, params.get("sessionId")))
    elif rid is None:
        trace.write("ignored notification %s" % method)
    else:
        trace.write("unknown request %s" % method)
        send(
            {
                "jsonrpc": "2.0",
                "id": rid,
                "error": {"code": -32601, "message": "acp-tools-agent does not implement %s" % method},
            }
        )


def main(argv):
    args = parse_args(argv)
    trace = Trace(args.trace)
    trace.write(
        "start pid=%d constants summary=%s details=%s wrong_summary=%s retired_summary=%s "
        "protocol=%s tool=%s tools=%s answer=%s"
        % (
            os.getpid(),
            SUMMARY,
            DETAILS,
            WRONG_SUMMARY,
            RETIRED_SUMMARY,
            MCP_PROTOCOL,
            COMPLETE_TOOL,
            ",".join(TOOLS),
            ANSWER,
        )
    )
    # The token the mount carries must not be in this process's own environment:
    # the client writes it into the mount's `env` and nowhere else.
    trace.write("start env token_in_env=%s" % str(TOKEN_VAR in os.environ).lower())
    state = {"gate": args.gate, "mount": None, "child_env": None, "bridge": None}
    try:
        while True:
            line = sys.stdin.readline()
            if line == "":
                trace.write("eof")
                return 0
            line = line.strip()
            if not line:
                continue
            try:
                msg = json.loads(line)
            except ValueError:
                trace.write("unparsable frame")
                continue
            if isinstance(msg, dict) and msg.get("method"):
                dispatch(msg, state, trace)
    except FixtureError as error:
        trace.write("error %s" % error)
        sys.stderr.write("acp-tools-agent: %s\n" % error)
        sys.stderr.flush()
        return 2
    finally:
        if state["bridge"] is not None:
            state["bridge"].close()


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
