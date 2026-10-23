#!/usr/bin/env bash
set -euo pipefail
# Case 20: the ACP session's tools mount, end to end.
#
# An ACP session has no plugin connection — this client spawned the agent
# itself, so nothing on the adapter socket carries the session's obligations.
# `session/new`'s `mcpServers` list is where they are handed over instead:
# the client resolves `onlyne` on its own side of the pipe, names it `onlyne`,
# runs it with `mcp`, and puts the session's token in that entry's environment
# and nowhere else (`docs/v2-CONTRACT.md` §3b). The agent starts that child
# itself, because an ACP stdio server is the agent's child, and speaks MCP to
# it.
#
# This case is the whole arc of that path, and it is the agent's own process
# that drives it: `acp-tools-agent.py` is the role's `[client.runtime] command`
# with the spec's `drive = "acp"`, exactly as case 18's `acp-agent.py` is. Where
# case 18 proves the ACP conversation and the turn's settlement, this one proves
# the tools mount:
#
#   1. the mount the client handed `session/new` is one stdio entry named
#      `onlyne`, its command is the CLI beside this client's own binary, and the
#      *agent* started it — the fixture spawns the `command`, with the `args` and
#      the `env` of that entry, so the child's parent is the agent and the
#      agent's parent is the client. The token is a capability, so the fixture
#      traces the entry with that value redacted and the case proves the real one
#      rode there by the only door that can: the client answering the child.
#   2. the bridge completes a real MCP handshake on stdio — `initialize`
#      answered by `onlyne` itself, `notifications/initialized` taken, and
#      `tools/list` answering the three obligation tools with
#      `onlyne_complete`'s required arguments.
#   3. a `tools/call` for `onlyne_complete` settles the task the session serves:
#      the ledger's task row acks, exactly one completion receipt exists, its
#      `out_head` is the summary the call carried, its body carries the details
#      the call carried, and the session's projection reads exited/done.
#   4. a call carrying a token that names no live session settles nothing, in
#      both spellings: a tampered token while the session is live (refused at the
#      client's door, with the ledger still holding no verdict — read while the
#      fixture holds its gate, so the refusal is provably the last thing that
#      happened), and the session's own retired token after the settlement
#      (refused rather than allowed a second verdict).
#   5. every one of those claims is read off a parsed protocol answer — the mount
#      the agent was handed, initialize's result, tools/list's schema, each
#      tools/call's text — so a missing handshake, a missing tool result or a
#      missing ledger row fails this case loudly instead of leaving a green line
#      behind.
#
# Nothing here duplicates case 18: that case asserts the turn's journal, its
# content index and its rendered log. This one asserts the tools mount, and it
# never reads the journal at all.
SRC=$(pwd)
# A short scratch root, like case 18's: the client publishes the adapter socket
# path inside this tree, and the mount entry names the same path, so the case
# compares two spellings of one file without leaving the bare form.
tmp=$(mktemp -d /tmp/onlyne-acp-tools.XXXXXX)
server_pid=""
client_pid=""
agent_pid=""

# `alive <pid>` is true while the process exists and is not a zombie. `kill -0`
# alone answers true for a child this shell has not reaped yet, which is exactly
# the state an already-exited agent sits in while its client is alive.
alive() {
  local pid=$1 state
  [ -n "$pid" ] || return 1
  state=$(ps -p "$pid" -o state= 2>/dev/null | tr -d '[:space:]')
  [ -n "$state" ] && [ "${state#Z}" = "$state" ]
}

cleanup() {
  local status=$? survived="" pid
  # The client holds the session, and the ACP agent is the client's child;
  # draining the client closes the stdin that ends the agent, and the agent's
  # own exit closes the two bridges it started. Each drain is unconditional, so
  # a case that failed halfway leaves nothing behind.
  drain_pid "$client_pid" 2>/dev/null || true
  drain_pid "$agent_pid" 2>/dev/null || true
  drain_pid "$server_pid" 2>/dev/null || true
  for pid in "$client_pid" "$agent_pid" "$server_pid"; do
    if alive "$pid"; then
      survived="$survived $pid"
    fi
  done
  if [ -n "$survived" ]; then
    echo "acp-tools: processes survived cleanup:$survived" >&2
  fi
  if [ "$status" -eq 0 ] && [ "${E2E_KEEP:-0}" != 1 ]; then
    rm -rf "$tmp"
  else
    echo "acp-tools: scratch directory kept at $tmp" >&2
  fi
  exit "$status"
}
trap cleanup EXIT
. "$SRC/crates/onlyne-testkit/e2e/lib.sh"

# lib.sh pins ONLYNE_BACKEND=fake. This case talks to a real child of the
# client, so it covers that export after the source: `headless` is the placement
# the workspace config names too, and it is the only placement `acp` pairs with,
# because stdio carries the channel.
export ONLYNE_BACKEND=headless

SERVER=$(bin onlyne-server)
CLIENT=$(bin onlyne-client)
ONLYNE=$(bin onlyne)
AGENT="$SRC/crates/onlyne-testkit/e2e/acp-tools-agent.py"
[ -f "$AGENT" ] || fail "the ACP tools fixture must sit beside this case" "$AGENT"

# The four arguments the fixture completes with, the MCP revision it asks for
# and the tool it calls. `acp-tools-agent.py` carries the same literals and
# traces them in its `start` line, which the checks below read back, so a drift
# between the two files fails here with both sides in the message.
SUMMARY='the tools mount settled this task'
DETAILS='The fixture settled the task through onlyne mcp. Second line of the full result.'
WRONG_SUMMARY='a tampered token settled this task'
RETIRED_SUMMARY='a retired token settled this task'
MCP_PROTOCOL='2025-06-18'
COMPLETE_TOOL='onlyne_complete'
MCP_CLIENT='acp-tools-agent'
# The client's own words for a token that names no live session: it is the
# answer the handshake gives a mount nobody holds, and the answer the per-frame
# gate gives one whose session has retired (`DispatchState::TOOLS_GONE_MESSAGE`).
REFUSED='unauthorized: token names no live session'
MCP_SERVER='onlyne'
TASK_PROSE='acp tools task: settle this through the mounted tools'

ws="$tmp/planner"
gate="$tmp/gate"
trace="$tmp/agent.trace"

# One `[[client]]` row — ACL self-send included, so the role can be sent to —
# whose drive is `acp` and whose command is the absolute fixture path. The JSON
# is generated rather than quoted by hand so a path a shell would have to escape
# still reaches the spec as one argv element. The `[client.runtime]` table is
# stated last: a key written after a table header belongs to that table, not to
# the entry.
runtime_command=$(python3 -c 'import json, sys; print(json.dumps(sys.argv[1:]))' \
  python3 -u "$AGENT" --acp --gate "$gate" --trace "$trace")
acl=$(printf 'allowed_senders = ["*", "planner"]\nallowed_targets = ["planner"]\n[client.runtime]\ndrive = "acp"\ncommand = %s\n' \
  "$runtime_command")
setup_cluster "$tmp/server" "$ws" planner cluster "" "$E2E_PROSE" "$acl"
server_pid=$cluster_server_pid

# `placement` is a top-level config key and `[acp]` a table of its own. The guard
# the agent asks for is `deny`, which is the shipped default: this case never
# asks for permission, so the table is here for the parse rather than for a
# behaviour of its own.
config="$ws/.onlyne/config.toml"
{
  printf 'placement = "headless"\n'
  cat "$config"
  printf '\n[acp]\npermission = "deny"\n'
} > "$config.new"
mv "$config.new" "$config"
python3 - "$config" <<'PY' || fail "the workspace must carry a top-level placement plus the [acp] table" "$(cat "$config")"
import sys
import tomllib

with open(sys.argv[1], "rb") as handle:
    doc = tomllib.load(handle)
# The key has to be read off the document root: a `placement` inside `[server]`
# would leave this None, which is the mistake this assertion exists for.
assert doc.get("placement") == "headless", doc
assert doc["acp"].get("permission") == "deny", doc
PY

"$CLIENT" run --workspace "$ws" >"$tmp/client.log" 2>&1 &
client_pid=$!

registered=false
for _ in $(seq 1 100); do
  roles_out=$("$ONLYNE" --server-root "$tmp/server" roles 2>/dev/null) || true
  printf '%s\n' "$roles_out" > "$tmp/roles.json"
  if rows_any "$tmp/roles.json" state online 2>/dev/null; then
    registered=true
    break
  fi
  sleep 0.1
done
[ "$registered" = "true" ] || fail "planner must register before the send" \
  "roles=$(cat "$tmp/roles.json" 2>/dev/null) client=$(cat "$tmp/client.log" 2>/dev/null)"

socket=$(wait_for_socket "$ws" 100) \
  || fail "the client must bind the runtime socket for $ws" "$(cat "$tmp/client.log" 2>/dev/null)"

send_out=$("$ONLYNE" --server-root "$tmp/server" send "${SUPERVISOR_FLAGS[@]}" --from planner --to planner --text "$TASK_PROSE") \
  || fail "send command failed" "$send_out"
printf '%s\n' "$send_out" > "$tmp/send.json"
[ "$(json_field "$tmp/send.json" '.ok' 'str(json.load(sys.stdin).get("ok")).lower()')" = "true" ] \
  || fail "send ok must be true" "$send_out"
[ "$(json_field "$tmp/send.json" '.data.state' 'json.load(sys.stdin).get("data",{}).get("state","")')" = "in_flight" ] \
  || fail "send data.state must be in_flight" "$send_out"
task=$(json_field "$tmp/send.json" '.data.task' 'json.load(sys.stdin)["data"]["task"]')

# `wait_trace <text>` waits for one exact line the fixture writes, so every
# assertion below reads a step that really happened rather than a sleep long
# enough to hope. The trace holds no secret: the fixture redacts the token's
# value on the way in, which is why a failure may quote the file.
wait_trace() {
  local want=$1 tries=${2:-300} n=0
  while [ "$n" -lt "$tries" ]; do
    if grep -q -F -- "$want" "$trace" 2>/dev/null; then
      return 0
    fi
    n=$((n + 1))
    sleep 0.1
  done
  return 1
}

# The mount arrives with `session/new`, and the agent opens the bridge on it at
# that moment: the exchange this case reads starts there, not at the first tool
# call.
wait_trace "mount {" || fail "the client must hand the agent an mcpServers entry at session/new" \
  "trace=$(cat "$trace" 2>/dev/null) client=$(cat "$tmp/client.log" 2>/dev/null)"
wait_trace "mcp initialize label=session" || fail "the agent's bridge must answer initialize" \
  "trace=$(cat "$trace" 2>/dev/null) client=$(cat "$tmp/client.log" 2>/dev/null)"

# The CLI the client commands the mount with is the one beside its own binary,
# resolved on the client's side so an agent that resolved `onlyne` again on its
# own PATH could not land on a different build (`crates/onlyne-client/src/backend/acp/mcp.rs`).
# The case compares against that rule rather than against a hardcoded
# `target/debug/onlyne`, and the fixture's own spawn is what proves the path
# runs.
cli_beside_client=$(python3 -c 'import os, sys; print(os.path.realpath(os.path.join(os.path.dirname(sys.argv[1]), "onlyne")))' "$CLIENT")
[ "$cli_beside_client" = "$ONLYNE" ] || fail "the case's own CLI path must be the one beside its client" \
  "beside=$cli_beside_client bin=$ONLYNE"

trace_report=$(cat "$trace" 2>/dev/null || true)
if ! python3 - "$trace" "$cli_beside_client" "$socket" "$client_pid" "$MCP_PROTOCOL" "$COMPLETE_TOOL" \
  "$MCP_CLIENT" "$MCP_SERVER" "$SUMMARY" "$DETAILS" "$WRONG_SUMMARY" "$RETIRED_SUMMARY" <<'PY'
import json
import subprocess
import sys

trace_path, cli, socket, client_pid_text = sys.argv[1:5]
(
    protocol,
    complete_tool,
    mcp_client,
    mcp_server,
    summary,
    details,
    wrong_summary,
    retired_summary,
) = sys.argv[5:13]

lines = [line for line in open(trace_path, encoding="utf-8").read().splitlines() if line.strip()]
start = [line for line in lines if line.startswith("start pid=")]
assert len(start) == 1, start
# The constants both files carry, checked against the fixture's own record: the
# whole tail of its `start` line, pid aside, is compared word for word, so a
# drift between `acp-tools.sh` and `acp-tools-agent.py` fails here with both
# sides in the message rather than at some later step that reads a stale literal.
pid_text, _, carried = start[0][len("start pid=") :].partition(" ")
assert pid_text.isdigit(), start[0]
expected_carried = (
    "constants summary=%s details=%s wrong_summary=%s retired_summary=%s "
    "protocol=%s tool=%s tools=%s answer=%s"
    % (
        summary,
        details,
        wrong_summary,
        retired_summary,
        protocol,
        complete_tool,
        "onlyne_send,onlyne_handoff,onlyne_complete",
        "The fixture answered through its tools mount.",
    )
)
assert carried == expected_carried, (start[0], expected_carried)

# The mount itself: one stdio entry, named for the CLI, running the one verb.
mounts = [line[len("mount "):] for line in lines if line.startswith("mount ")]
assert len(mounts) == 1, mounts
entry = json.loads(mounts[0])
assert entry.get("type") == "stdio", entry
assert entry.get("name") == mcp_server, entry
assert entry.get("command") == cli, entry
assert entry.get("args") == ["mcp"], entry
pairs = {pair["name"]: pair["value"] for pair in entry.get("env") or []}
assert list(pairs) == ["ONLYNE_SOCKET", "ONLYNE_MCP_TOKEN"], entry
assert pairs["ONLYNE_SOCKET"] == socket, entry
# The token is a capability, so the trace carries its length and never its value.
assert pairs["ONLYNE_MCP_TOKEN"].startswith("<") and pairs["ONLYNE_MCP_TOKEN"].endswith(
    " chars>"
), entry
assert int(pairs["ONLYNE_MCP_TOKEN"].split()[0].lstrip("<")) > 0, entry

# The agent's own environment: the token rides in the mount's env and nowhere
# else, which is what makes the mount the session's only carrier.
assert "start env token_in_env=false" in lines, [line for line in lines if "token_in_env" in line]
assert "token label=session tampered=false" in lines, [line for line in lines if "token " in line]

# Who started it: the fixture spawned the entry's command, the fixtures's parent
# is the client, and the child's parent is the fixture. The client is the pid
# this case started, so nothing here is inferred from a name.
spawn = [
    line
    for line in lines
    if line.startswith("spawn label=session pid=")
]
assert len(spawn) == 1, spawn
spawn_fields = dict(item.split("=", 1) for item in spawn[0].split(" ") if "=" in item)
assert spawn_fields["command"] == cli, spawn
assert json.loads(spawn_fields["args"]) == ["mcp"], spawn
assert spawn_fields["env"] == "ONLYNE_SOCKET,ONLYNE_MCP_TOKEN", spawn
bridge_pid = int(spawn_fields["pid"])
fixture_pid = int(start[0].split("start pid=")[1].split(" ")[0])


def ppid_of(pid):
    return int(subprocess.check_output(["ps", "-o", "ppid=", "-p", str(pid)]).strip())


assert ppid_of(bridge_pid) == fixture_pid, (bridge_pid, fixture_pid)
# The client's pid is the one this case started, so the chain is read from live
# processes rather than from a name any of them could share.
client_pid = int(client_pid_text)
assert ppid_of(fixture_pid) == client_pid, (fixture_pid, client_pid_text)
print(
    "PASS acp-tools mount: the agent started %s mcp (pid %d, child of the fixture pid %d, "
    "itself the client's child) from the mcpServers entry the client handed session/new; "
    "the token rides in that entry's env alone" % (cli, bridge_pid, fixture_pid)
)
PY
then
  fail "the tools mount must be the CLI beside the client, started by the agent from the entry" "$trace_report"
fi

# The handshake, read off the answers rather than off the presence of bytes:
# `initialize` is answered by the bridge itself, `notifications/initialized` is
# taken (a notification is answered by nothing), and `tools/list` names the three
# obligations with `onlyne_complete`'s required arguments.
if ! python3 - "$trace" "$MCP_PROTOCOL" "$MCP_CLIENT" "$MCP_SERVER" "$COMPLETE_TOOL" <<'PY'
import sys

trace_path, protocol, mcp_client, mcp_server, complete_tool = sys.argv[1:6]
lines = [line for line in open(trace_path, encoding="utf-8").read().splitlines() if line.strip()]
assert "initialize protocolVersion=1 client=onlyne-client" in lines, lines[:6]
initialize = [
    line for line in lines if line.startswith("mcp initialize label=session id=1 ")
]
assert len(initialize) == 1, initialize
assert "protocolVersion=%s" % protocol in initialize[0], initialize
assert 'capabilities={"tools": {}}' in initialize[0], initialize
assert " server=%s version=" % mcp_server in initialize[0], initialize
assert not initialize[0].rstrip().endswith(" version="), initialize
assert "mcp initialized label=session" in lines, lines
tools = [line for line in lines if line.startswith("mcp tools/list label=session id=2 ")]
assert len(tools) == 1, tools
assert "names=onlyne_send,onlyne_handoff,%s" % complete_tool in tools[0], tools
assert "complete_required=outcome,summary" in tools[0], tools
print(
    "PASS acp-tools handshake: the bridge answered initialize with protocol %s as %s "
    "(capabilities tools), took notifications/initialized, and answered tools/list with "
    "onlyne_send,onlyne_handoff,%s — complete requiring outcome,summary"
    % (protocol, mcp_server, complete_tool)
)
PY
then
  fail "the bridge must complete a real MCP handshake on stdio" "$trace_report"
fi

# The gate: the fixture holds here, after the tampered token's call and before the
# settling one, so the ledger read below is the state the refusal left behind.
wait_trace "gate wait" || fail "the fixture must hold its gate before the settling call" \
  "trace=$(cat "$trace" 2>/dev/null) client=$(cat "$tmp/client.log" 2>/dev/null)"

# What the tampered token earned, and what the ledger holds while the refused
# call is the last thing that happened. The wrong bridge speaks the whole MCP
# handshake — the bridge answers that by itself — and only its tool call reaches
# the client, which is where a token nobody holds is refused.
"$ONLYNE" --server-root "$tmp/server" ledger --task "$task" > "$tmp/ledger-held.json" 2>/dev/null \
  || fail "the ledger must answer for a live task" "$(cat "$tmp/ledger-held.json")"
hold_report=$(cat "$trace" 2>/dev/null || true)
if ! python3 - "$trace" "$tmp/ledger-held.json" "$REFUSED" "$COMPLETE_TOOL" "$WRONG_SUMMARY" <<'PY'
import json
import sys

trace_path, ledger_path, refused, complete_tool, wrong_summary = sys.argv[1:6]
lines = [line for line in open(trace_path, encoding="utf-8").read().splitlines() if line.strip()]
initialize = [line for line in lines if line.startswith("mcp initialize label=wrong ")]
assert len(initialize) == 1, initialize
tools = [line for line in lines if line.startswith("mcp tools/list label=wrong id=2 ")]
assert len(tools) == 1, tools
calls = [line for line in lines if line.startswith("mcp call label=wrong ")]
assert len(calls) == 1, calls
assert "tool=%s outcome=failed isError=true text=%s" % (complete_tool, refused) in calls[0], calls

raw = json.load(open(ledger_path))
rows = raw.get("data", raw)
rows = rows.get("ledger") or rows.get("rows") or rows.get("results") or rows or []
rows = [row for row in rows if isinstance(row, dict)]
completions = [row for row in rows if row.get("kind") == "completion"]
assert completions == [], completions
assert all(row.get("state") != "acked" for row in rows), rows
assert not any(wrong_summary in json.dumps(row) for row in rows), rows
print(
    "PASS acp-tools refusal (live): a tools/call for %s carrying a tampered token was answered "
    "`%s` — the client's own sentence, since the handshake never reached it over a live session — "
    "and the ledger still holds no verdict at all" % (complete_tool, refused)
)
PY
then
  fail "a tampered token must settle nothing and leave the ledger unchanged" "$hold_report"
fi

# Release the settling call. Nothing between here and the assertions below changes
# what the fixture sends: the gate is the only switch it has.
touch "$gate"

ledger_out=""
for _ in $(seq 1 120); do
  ledger_out=$("$ONLYNE" --server-root "$tmp/server" ledger --task "$task" 2>/dev/null) || true
  printf '%s\n' "$ledger_out" > "$tmp/ledger.json"
  if rows_any "$tmp/ledger.json" state acked 2>/dev/null; then
    break
  fi
  sleep 0.5
done
rows_any "$tmp/ledger.json" state acked || fail "the tools mount's completion must settle the task" \
  "ledger=$ledger_out trace=$(cat "$trace" 2>/dev/null) client=$(cat "$tmp/client.log" 2>/dev/null)"

# The tool result, the ledger and the projection are one act read three ways, so
# they are asserted together and the case fails if any of them disagrees.
wait_trace "mcp call label=session id=3 " || fail "the settling call must be answered" \
  "trace=$(cat "$trace" 2>/dev/null) client=$(cat "$tmp/client.log" 2>/dev/null)"
sessions_out=""
for _ in $(seq 1 120); do
  sessions_out=$("$ONLYNE" --server-root "$tmp/server" sessions --task "$task" 2>/dev/null) || true
  printf '%s\n' "$sessions_out" > "$tmp/sessions.json"
  if rows_any "$tmp/sessions.json" public_lifecycle exited 2>/dev/null; then
    break
  fi
  sleep 0.5
done
rows_any "$tmp/sessions.json" public_lifecycle exited || fail "the session's projection must read exited" \
  "sessions=$sessions_out ledger=$ledger_out client=$(cat "$tmp/client.log" 2>/dev/null)"

settle_report=$(cat "$trace" 2>/dev/null || true)
if ! python3 - "$trace" "$tmp/ledger.json" "$tmp/sessions.json" "$COMPLETE_TOOL" "$SUMMARY" "$DETAILS" <<'PY'
import json
import sys

trace_path, ledger_path, sessions_path, complete_tool, summary, details = sys.argv[1:7]
lines = [line for line in open(trace_path, encoding="utf-8").read().splitlines() if line.strip()]
calls = [line for line in lines if line.startswith("mcp call label=session ")]
assert len(calls) == 2, calls
assert "tool=%s outcome=done isError=false text=reported done" % complete_tool in calls[0], calls
# The fixture ends its turn after the second call, which is the agent's own
# closing fact: the settlement was the tool call's, not the turn's.
assert "stop reason=end_turn" in lines, lines[-4:]


def rows_of(raw):
    # The list keys `lib.sh:data_rows` reads, so a `ledger --task` answer and a
    # `sessions --task` answer are both read as rows rather than as one blob: the
    # sessions answer is keyed `sessions`, which the ledger's key list misses.
    if isinstance(raw, dict) and isinstance(raw.get("data"), (dict, list)):
        raw = raw["data"]
    if isinstance(raw, list):
        return [row for row in raw if isinstance(row, dict)]
    if isinstance(raw, dict):
        for key in ("rows", "results", "ledger", "items", "roles", "sessions", "faults"):
            if isinstance(raw.get(key), list):
                return [row for row in raw[key] if isinstance(row, dict)]
        return [raw]
    return []


rows = rows_of(json.load(open(ledger_path)))
tasks = [row for row in rows if row.get("kind") == "task"]
completions = [row for row in rows if row.get("kind") == "completion"]
# Both literals are read below as substrings, and an empty one would make those
# checks pass on nothing at all.
assert summary and details, (summary, details)
assert len(tasks) == 1, rows
assert len(completions) == 1, rows
assert tasks[0].get("state") == "acked", rows
receipt = completions[0]
assert receipt.get("state") == "acked", rows
assert receipt.get("task") == tasks[0].get("task"), (receipt, tasks[0])
# The completion's body, as the originator reads it, is where the full result
# belongs (`docs/v2-CONTRACT.md` §3c: "`details` … delivered verbatim to the next
# hop and the originator").
body = json.loads(receipt["body_json"]) if isinstance(receipt.get("body_json"), str) else receipt.get("body")
assert body is not None, receipt
assert body.get("text") == details, (details, body)
# The head is the `summary` the call carried, not a preview of the result: §3c
# calls it "one display line" and the head a display field, so a body carrying a
# full result and a head naming its own line must show the line. The store
# derives `out_head` from the body's text only for an envelope that carries no
# head of its own (`crates/onlyne-store/src/server.rs::body_head`), which is the
# CLI's own completion and not this receipt.
assert receipt.get("out_head") == summary, (summary, receipt.get("out_head"), receipt)
assert body.get("head") == summary, (summary, body.get("head"), body)

sessions = rows_of(json.load(open(sessions_path)))
assert len(sessions) == 1, sessions
session = sessions[0]
assert session.get("public_lifecycle") == "exited", session
assert session.get("outcome") == "done", session
assert session.get("task_id") == tasks[0].get("task"), session
print(
    "PASS acp-tools settlement: tools/call %s outcome=done answered `reported done`; the task "
    "row and its one completion receipt both ack, the receipt's out_head is the call's "
    "summary and its body is the call's details, and the session's projection reads "
    "exited/done"
    % complete_tool
)
PY
then
  fail "the completion's verdict, its body and the session's projection must agree" \
    "trace=$settle_report ledger=$ledger_out sessions=$sessions_out"
fi

# The negative half of the same door: the session's own token after the
# settlement. `.onlyne/client.db` is not read for it and the fixture does not
# mint one — the bridge that settled the task is still alive and asks again, and
# the call is refused because a token dies with the delivery it names.
wait_trace "mcp call label=session id=4 " || fail "the retired token's call must be answered" \
  "trace=$(cat "$trace" 2>/dev/null) client=$(cat "$tmp/client.log" 2>/dev/null)"
"$ONLYNE" --server-root "$tmp/server" ledger --task "$task" > "$tmp/ledger-after.json" 2>/dev/null \
  || fail "the ledger must still answer after the settlement" "$(cat "$tmp/ledger-after.json")"
retired_report=$(cat "$trace" 2>/dev/null || true)
if ! python3 - "$trace" "$tmp/ledger-after.json" "$tmp/sessions.json" "$COMPLETE_TOOL" "$REFUSED" \
  "$SUMMARY" "$DETAILS" "$WRONG_SUMMARY" "$RETIRED_SUMMARY" <<'PY'
import json
import sys

(
    trace_path,
    ledger_path,
    sessions_path,
    complete_tool,
    refused,
    summary,
    details,
    wrong_summary,
    retired_summary,
) = sys.argv[1:10]
lines = [line for line in open(trace_path, encoding="utf-8").read().splitlines() if line.strip()]
calls = [line for line in lines if line.startswith("mcp call label=session ")]
assert len(calls) == 2, calls
retired = calls[1]
assert "tool=%s outcome=failed isError=true text=%s" % (complete_tool, refused) in retired, retired


def rows_of(raw):
    # Same reader as the block above: the sessions answer is keyed `sessions`.
    if isinstance(raw, dict) and isinstance(raw.get("data"), (dict, list)):
        raw = raw["data"]
    if isinstance(raw, list):
        return [row for row in raw if isinstance(row, dict)]
    if isinstance(raw, dict):
        for key in ("rows", "results", "ledger", "items", "roles", "sessions", "faults"):
            if isinstance(raw.get(key), list):
                return [row for row in raw[key] if isinstance(row, dict)]
        return [raw]
    return []


rows = rows_of(json.load(open(ledger_path)))
completions = [row for row in rows if row.get("kind") == "completion"]
assert summary and details, (summary, details)
assert len(completions) == 1, rows
assert completions[0].get("out_head"), completions
body = json.loads(completions[0]["body_json"])
assert details in (body.get("text") or ""), body
# Nothing the refused calls carried reached a row: the verdict the first call
# wrote is the only one that stands, in the ledger and in the projection.
blob = json.dumps(rows)
assert retired_summary not in blob and wrong_summary not in blob, rows
sessions = rows_of(json.load(open(sessions_path)))
assert sessions and sessions[0].get("outcome") == "done", sessions
print(
    "PASS acp-tools refusal (retired): the same bridge's second tools/call, carrying the token of "
    "the delivery that had just settled, was answered `%s`, and the ledger still holds exactly the "
    "first verdict — one completion receipt, its own head and body, no row for either refused call"
    % refused
)
PY
then
  fail "a retired token must settle nothing and leave the settled rows alone" \
    "trace=$retired_report ledger=$(cat "$tmp/ledger-after.json") sessions=$(cat "$tmp/sessions.json")"
fi

# The property every check above had to satisfy to be worth anything: the case
# reads parsed protocol answers — the entry the agent was handed, the bridge's
# own initialize and tools/list results, each tools/call's result text — and it
# stops at the first step that is missing. A log with the right bytes in it would
# not pass any of them.
echo "PASS acp-tools evidence: every claim above was read off a parsed protocol answer (the mount entry, initialize's result, tools/list's schema, each tools/call's text), so a missing handshake or a missing tool result fails this case instead of passing on the presence of bytes"

# The turns are over and every assertion has passed, so the client is drained
# exactly as an operator would stop it: it closes its session on the way out,
# which ends the agent this case read its pid from, and the agent's own exit
# closes the two bridges it started.
agent_pid=$(sed -n 's/^start pid=\([0-9]*\) .*/\1/p' "$trace" | sed -n '1p')
case "$agent_pid" in
  ''|*[!0-9]*) fail "the fixture trace must name its own pid" "$(sed -n '1,4p' "$trace")" ;;
esac
bridge_pids=$(sed -n 's/^spawn label=[a-z]* pid=\([0-9]*\) .*/\1/p' "$trace")
drain_pid "$client_pid"
client_pid=""
agent_gone=false
for _ in $(seq 1 100); do
  if ! alive "$agent_pid"; then
    agent_gone=true
    break
  fi
  sleep 0.1
done
[ "$agent_gone" = "true" ] || fail "the ACP agent must be gone once its client is drained" \
  "pid=$agent_pid trace=$(cat "$trace" 2>/dev/null) client=$(cat "$tmp/client.log" 2>/dev/null)"
agent_pid=""
for bridge in $bridge_pids; do
  bridge_gone=false
  for _ in $(seq 1 100); do
    if ! alive "$bridge"; then
      bridge_gone=true
      break
    fi
    sleep 0.1
  done
  [ "$bridge_gone" = "true" ] || fail "a tools mount must be gone once its agent is" \
    "pid=$bridge trace=$(cat "$trace" 2>/dev/null) client=$(cat "$tmp/client.log" 2>/dev/null)"
done

echo "PASS acp-tools"
