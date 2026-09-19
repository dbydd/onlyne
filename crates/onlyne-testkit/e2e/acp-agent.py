#!/usr/bin/env python3
"""A scripted ACP v1 agent for the acp-session end-to-end case (case 18).

The case runs this file as the role's `session_command`:

    python3 -u acp-agent.py --acp --gate <path> --trace <path>

Protocol discipline, which is what makes the case observable:

* stdout carries ACP v1 JSON-RPC 2.0 frames only, one per line, flushed as they
  are written. Diagnostics go to stderr, and the trace of the process and of
  every method it receives goes to the `--trace` file the case reads.
* a request id is echoed back exactly as it arrived, including a string id.
* `session/prompt` is where the case's gate lives: the prompt is traced, then
  the agent waits for the `--gate` file to appear before streaming a single
  deterministic turn — one reasoning chunk, one tool call, its completion, one
  answer chunk — and answering `stopReason: end_turn`. The wait is bounded, so a
  case that never releases the gate fails its own assertions instead of hanging.
* end of stdin is the client leaving: the agent exits 0.
"""

import argparse
import json
import os
import sys
import time

# The sentences this agent speaks and the case asserts on. `acp-session.sh`
# carries the same literals and checks them against the `start` trace line, so a
# drift between the two files fails at the gate with both sides in the message.
REASONING = "The fixture reasons about the acp session."
ANSWER = "The fixture answered through the acp session."
TOOL_TITLE = "Edit session.rs"
TOOL_CALL_ID = "call-session-1"
TOOL_KIND = "edit"
SESSION_ID = "acp-session-1"
ACCEPT_MODE = "acceptEdits"
DEFAULT_MODE = "default"

GATE_POLL_SECONDS = 0.05
# Long enough for a slow machine to complete the gated assertions,
# short enough that a case which never releases the gate still reports.
GATE_WAIT_SECONDS = 120.0

INITIALIZE_RESULT = {
    "protocolVersion": 1,
    "agentInfo": {
        "name": "acp-agent",
        "title": "Onlyne ACP session fixture",
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
        "currentModeId": DEFAULT_MODE,
        "availableModes": [
            {"id": DEFAULT_MODE, "name": "Default", "description": "ask before acting"},
            {"id": ACCEPT_MODE, "name": "Accept edits", "description": "apply edits without asking"},
        ],
    },
    "models": {
        "currentModelId": "fixture-model",
        "availableModels": [
            {"modelId": "fixture-model", "name": "Fixture model", "description": "the case's model"}
        ],
    },
    "configOptions": [
        {
            "id": "model",
            "name": "Model",
            "category": "model",
            "type": "select",
            "currentValue": "fixture-model",
            "options": [{"value": "fixture-model", "name": "Fixture model"}],
        },
        {
            "id": "reasoning_effort",
            "name": "Reasoning effort",
            "category": "thought_level",
            "type": "select",
            "currentValue": "low",
            "options": [
                {"value": "low", "name": "Low"},
                {"value": "high", "name": "High"},
            ],
        },
    ],
}


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
            sys.stderr.write("acp-agent: trace %s: %s\n" % (self.path, error))
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
        help="file whose appearance releases the gated turn",
    )
    parser.add_argument(
        "--trace",
        required=True,
        help="file this process traces its pid and every received method to",
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


def failure(rid, code, message):
    send({"jsonrpc": "2.0", "id": rid, "error": {"code": code, "message": message}})


def update(session_id, payload):
    """One `session/update` notification, in the shape the client's reader routes.

    `sessionId` and the update payload share one params object: the reader
    classifies by the `sessionUpdate` discriminator on the params it receives.
    """
    body = {"sessionId": session_id}
    body.update(payload)
    send({"jsonrpc": "2.0", "method": "session/update", "params": body})


def wait_for_gate(path, trace):
    deadline = time.monotonic() + GATE_WAIT_SECONDS
    while not os.path.exists(path):
        if time.monotonic() >= deadline:
            trace.write("gate timeout after %.0fs" % GATE_WAIT_SECONDS)
            return
        time.sleep(GATE_POLL_SECONDS)
    trace.write("gate open")


def run_turn(rid, session_id, prompt, gate, trace):
    """One deterministic turn: traced, gated, then streamed in a fixed order."""
    trace.write("session/prompt id=%s text=%s" % (session_id, prompt.replace("\n", " / ")))
    wait_for_gate(gate, trace)
    update(
        session_id,
        {
            "sessionUpdate": "agent_thought_chunk",
            "content": {"type": "text", "text": REASONING},
        },
    )
    update(
        session_id,
        {
            "sessionUpdate": "tool_call",
            "toolCallId": TOOL_CALL_ID,
            "title": TOOL_TITLE,
            "kind": TOOL_KIND,
            "status": "pending",
        },
    )
    update(
        session_id,
        {
            "sessionUpdate": "tool_call_update",
            "toolCallId": TOOL_CALL_ID,
            "status": "completed",
        },
    )
    update(
        session_id,
        {
            "sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": ANSWER},
        },
    )
    result(rid, {"stopReason": "end_turn"})


def dispatch(msg, gate, trace):
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
        result(rid, SESSION_NEW_RESULT)
    elif method == "session/set_mode":
        trace.write(
            "session/set_mode id=%s modeId=%s" % (params.get("sessionId"), params.get("modeId"))
        )
        result(rid, {})
    elif method == "session/set_config_option":
        trace.write(
            "session/set_config_option id=%s configId=%s value=%s"
            % (params.get("sessionId"), params.get("configId"), params.get("value"))
        )
        result(rid, {})
    elif method == "session/prompt":
        blocks = params.get("prompt") or []
        prompt = "".join(
            block.get("text", "") for block in blocks if isinstance(block, dict)
        )
        run_turn(rid, params.get("sessionId"), prompt, gate, trace)
    elif method == "session/close":
        trace.write("session/close id=%s" % params.get("sessionId"))
        result(rid, {})
    elif method in ("session/cancel", "$/cancel_request"):
        # Both arrive as notifications: the client is telling this agent to stop,
        # and a notification has no id to answer.
        trace.write("%s id=%s" % (method, params.get("sessionId")))
    elif rid is None:
        trace.write("ignored notification %s" % method)
    else:
        trace.write("unknown request %s" % method)
        failure(rid, -32601, "acp-agent does not implement %s" % method)


def main(argv):
    args = parse_args(argv)
    trace = Trace(args.trace)
    trace.write(
        "start pid=%d constants reasoning=%s answer=%s tool=%s call=%s kind=%s"
        % (os.getpid(), REASONING, ANSWER, TOOL_TITLE, TOOL_CALL_ID, TOOL_KIND)
    )
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
            dispatch(msg, args.gate, trace)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
