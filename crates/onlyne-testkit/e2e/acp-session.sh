#!/usr/bin/env bash
set -euo pipefail
# Case 18: the ACP session backend, end to end.
#
# The role runs `python3 -u acp-agent.py ... --acp` as its `session_command`: a
# real ACP v1 peer on stdio, not a fake backend. The workspace config names a
# top-level `backend = "acp"` plus an `[acp]` table, so the client opens the
# session with the mode, model and reasoning effort this case chose, and the
# agent's trace is the proof it received them.
#
# The agent holds its one turn at a gate. That is the whole trick: while the
# gate is held, the journal provably carries the client's dispatch record
# alone. Releasing the gate then makes the turn's updates arrive in the journal,
# the content index, and the rendered log — one conversation read three ways,
# all of them durable files under the role workspace.
#
# Every prompt travels with the backend's payload-v1 report directive, and the
# fixture obeys it: the first task leaves a `hop-done:` line that becomes the
# ledger head in place of the streamed answer; a second task whose prose
# carries `HOPFAIL` reports `hop-failed:` and must settle Failed with the
# reason as its head and one acp fault on the record.
#
# No product id is assumed. The task comes from `send`, the socket from the
# client's own marker, and the ACP session id, agent pid, journal and log paths
# from `client.db`'s stored reference for that task.
SRC=$(pwd)
# A short scratch root is part of this case: the client publishes the adapter
# socket path it bound, and this case reads that marker back and probes the
# path after the client exits. `/tmp` is unique under `mktemp -d` like any
# other root and keeps the served socket path inside the bare spelling.
tmp=$(mktemp -d /tmp/onlyne-acp-session.XXXXXX)
server_pid=""
client_pid=""
acp_pid=""

# `alive <pid>` is true while the process exists and is not a zombie. `kill -0`
# alone answers true for a child this shell has not reaped yet, which is exactly
# the state an already-exited ACP child sits in while its client is alive.
alive() {
  local pid=$1 state
  [ -n "$pid" ] || return 1
  state=$(ps -p "$pid" -o state= 2>/dev/null | tr -d '[:space:]')
  [ -n "$state" ] && [ "${state#Z}" = "$state" ]
}

cleanup() {
  local status=$? survived="" pid
  # The client holds the session, and the ACP agent is the client's child;
  # draining the client closes the stdin that ends it. Each drain is
  # unconditional, so a case that failed halfway still leaves nothing behind.
  drain_pid "$client_pid" 2>/dev/null || true
  drain_pid "$acp_pid" 2>/dev/null || true
  drain_pid "$server_pid" 2>/dev/null || true
  for pid in "$client_pid" "$acp_pid" "$server_pid"; do
    if alive "$pid"; then
      survived="$survived $pid"
    fi
  done
  if [ -n "$survived" ]; then
    echo "acp-session: processes survived cleanup:$survived" >&2
  fi
  if [ "$status" -eq 0 ] && [ "${E2E_KEEP:-0}" != 1 ]; then
    rm -rf "$tmp"
  else
    echo "acp-session: scratch directory kept at $tmp" >&2
  fi
  exit "$status"
}
trap cleanup EXIT
. "$SRC/crates/onlyne-testkit/e2e/lib.sh"

# lib.sh pins ONLYNE_BACKEND=fake. This case talks to the ACP backend, so it
# covers that export after the source; the workspace config names the same
# backend, which is what an operator would ship.
export ONLYNE_BACKEND=acp

SERVER=$(bin onlyne-server)
CLIENT=$(bin onlyne-client)
ONLYNE=$(bin onlyne)
AGENT="$SRC/crates/onlyne-testkit/e2e/acp-agent.py"
[ -f "$AGENT" ] || fail "the ACP fixture must sit beside this case" "$AGENT"

# The turn the fixture performs. `acp-agent.py` carries the same strings
# and traces them in its `start` line, which the trace assertions check against
# these values, so a drift between the two files fails at the gate with both
# sides in the message.
TASK_PROSE='acp session task: prove both halves of the turn'
REASONING='The fixture reasons about the acp session.'
ANSWER='The fixture answered through the acp session.'
TOOL_TITLE='Edit session.rs'
TOOL_CALL_ID='call-session-1'
TOOL_KIND='edit'
ACCEPT_MODE='acceptEdits'
FIXTURE_MODEL='fixture-model'
FIXTURE_EFFORT='high'
# The payload-v1 report literals: the fixture writes these lines to the report
# path its prompt names, and the ledger rows below must carry the text after
# the prefix — not the streamed answer.
PAYLOAD_HEAD='The fixture reported through the payload file.'
PAYLOAD_FAIL_NOTE='The fixture failed on purpose for the payload case.'

ws="$tmp/planner"
gate="$tmp/gate"
trace="$tmp/agent.trace"

# One `[[client]]` row — ACL self-send included, so the role can be sent to —
# whose `session_command` is the absolute fixture path. The JSON is generated
# rather than quoted by hand so a path a shell would have to escape still
# reaches the spec as one argv element.
session_command=$(python3 -c 'import json, sys; print(json.dumps(sys.argv[1:]))' \
  python3 -u "$AGENT" --acp --gate "$gate" --trace "$trace")
acl=$(printf 'allowed_senders = ["*", "planner"]\nallowed_targets = ["planner"]\nsession_command = %s\n' \
  "$session_command")
setup_cluster "$tmp/server" "$ws" planner cluster "" "$E2E_PROSE" "$acl"
server_pid=$cluster_server_pid

# `backend` is a top-level config key and `[acp]` a table of its own. The key is
# prepended above `[server]` because a trailing append would land inside that
# table, and the table is appended after the document it belongs to. The client
# would refuse both mistakes — the "must register" poll below is their real
# proof — but the parse here names the mistake instead of leaving it inferred.
config="$ws/.onlyne/config.toml"
{
  printf 'backend = "acp"\n'
  cat "$config"
  printf '\n[acp]\nmode = "%s"\nmodel = "%s"\nreasoning_effort = "%s"\npermission = "deny"\n' \
    "$ACCEPT_MODE" "$FIXTURE_MODEL" "$FIXTURE_EFFORT"
} > "$config.new"
mv "$config.new" "$config"
if ! python3 - "$config" "$ACCEPT_MODE" "$FIXTURE_MODEL" "$FIXTURE_EFFORT" <<'PY'
import sys
import tomllib

with open(sys.argv[1], "rb") as handle:
    doc = tomllib.load(handle)
mode, model, effort = sys.argv[2:5]
# The key has to be read off the document root: a `backend` inside `[server]`
# would leave this None, which is the mistake this assertion exists for.
assert doc.get("backend") == "acp", doc
acp = doc["acp"]
assert acp.get("mode") == mode, acp
assert acp.get("model") == model, acp
assert acp.get("reasoning_effort") == effort, acp
assert acp.get("permission") == "deny", acp
PY
then
  fail "the workspace must carry a top-level backend plus the [acp] table" "$(cat "$config")"
fi

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

# The client publishes the path it bound in `<run>/socket`, and the case reads
# that instead of spelling the canonical path: the socket whose lifecycle this
# case then follows is the one the client is serving, whatever the path cost.
marker="$ws/.onlyne/run/socket"
for _ in $(seq 1 100); do
  if [ -s "$marker" ]; then
    break
  fi
  sleep 0.1
done
[ -s "$marker" ] || fail "the client must publish the bound socket path in $marker" \
  "$(cat "$tmp/client.log" 2>/dev/null)"
socket=$(tr -d '[:space:]' <"$marker")
[ -n "$socket" ] || fail "the published socket path must be non-empty" "$(cat "$marker" 2>/dev/null)"
[ -S "$socket" ] || fail "the published path must hold a bound socket" "$socket"

send_out=$("$ONLYNE" --server-root "$tmp/server" send --from planner --to planner --text "$TASK_PROSE") \
  || fail "send command failed" "$send_out"
printf '%s\n' "$send_out" > "$tmp/send.json"
[ "$(json_field "$tmp/send.json" '.ok' 'str(json.load(sys.stdin).get("ok")).lower()')" = "true" ] \
  || fail "send ok must be true" "$send_out"
[ "$(json_field "$tmp/send.json" '.data.state' 'json.load(sys.stdin).get("data",{}).get("state","")')" = "in_flight" ] \
  || fail "send data.state must be in_flight" "$send_out"
task=$(json_field "$tmp/send.json" '.data.task' 'json.load(sys.stdin)["data"]["task"]')

# The reference the client stored for this task is where every product id below
# comes from, so nothing is guessed. The fixture traces the prompt before it
# waits on the gate, which is the moment the turn is provably held and the
# journal's first half is complete.
db="$ws/.onlyne/client.db"
ref=""
gated="false"
for _ in $(seq 1 300); do
  ref=$(db_count "$db" "SELECT backend_ref FROM sessions WHERE task_id='$task'" 2>/dev/null) || true
  if [ -n "$ref" ] && grep -q -F "session/prompt id=" "$trace" 2>/dev/null; then
    gated="true"
    break
  fi
  sleep 0.1
done
[ "$gated" = "true" ] || fail "the fixture must report the prompt it is holding at the gate" \
  "trace=$(cat "$trace" 2>/dev/null) ref=$ref client=$(cat "$tmp/client.log" 2>/dev/null)"

printf '%s\n' "$ref" > "$tmp/acp-ref.json"
if ! python3 - "$tmp/acp-ref.json" > "$tmp/acp-ref.txt" <<'PY'
import json
import sys

with open(sys.argv[1]) as handle:
    ref = json.load(handle)
assert ref.get("backend") == "acp", ref
inner = ref.get("backend_ref") or {}
for key in ("id", "pid", "events", "log"):
    assert inner.get(key) not in (None, "", 0), (key, ref)
for key in ("id", "pid", "events", "log"):
    print(inner[key])
PY
then
  fail "client.db must store one ACP session reference for the task" \
    "ref=$ref client=$(cat "$tmp/client.log" 2>/dev/null)"
fi
acp_session=$(sed -n '1p' "$tmp/acp-ref.txt")
acp_pid=$(sed -n '2p' "$tmp/acp-ref.txt")
events=$(sed -n '3p' "$tmp/acp-ref.txt")
log=$(sed -n '4p' "$tmp/acp-ref.txt")
# The backend derives both journal paths and the report path from the same
# workspace, so the two `dirname`s off the stored journal spelling reach
# `<workdir>/.onlyne/out` whatever way the path was written to disk.
report_path="$(dirname "$(dirname "$events")")/out/$task.md"
case "$acp_pid" in
  ''|*[!0-9]*) fail "the ACP session reference must carry a numeric pid" "pid=$acp_pid ref=$ref" ;;
esac
if ! alive "$acp_pid"; then
  fail "the ACP child must be running while its client holds the gated turn" \
    "pid=$acp_pid ref=$ref trace=$(cat "$trace" 2>/dev/null) client=$(cat "$tmp/client.log" 2>/dev/null)"
fi

# The gated journal: exactly the dispatch record and nothing else. A missing
# update is what proves the gate was held, so this is asserted while it is.
[ -s "$events" ] || fail "the ACP backend must journal the dispatch record before the prompt" \
  "logs=$(ls -1 "$ws/.onlyne/logs" 2>/dev/null) client=$(cat "$tmp/client.log" 2>/dev/null)"
journal_report=$(cat "$events" 2>/dev/null || true)
if ! python3 - "$events" "$task" "$TASK_PROSE" "$report_path" <<'PY'
import json
import sys

path, task, prose, report_path = sys.argv[1:5]
lines = [line for line in open(path, encoding="utf-8").read().splitlines() if line.strip()]
assert len(lines) == 1, lines
record = json.loads(lines[0])["onlyne"]
assert record["kind"] == "dispatch", record
assert record["task_id"] == task, record
# The dispatch record is the whole prompt: the task's prose with the report
# directive appended, naming the very path the fixture's agent will write.
assert record["prose"].startswith(prose), record
assert f"Result report (write before you stop): {report_path}" in record["prose"], record
PY
then
  fail "a gated turn must leave exactly the one dispatch record in the journal" "$journal_report"
fi

# Release the turn. Nothing between here and the assertions below changes what
# the agent sends: the gate is the only switch it has.
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
rows_any "$tmp/ledger.json" state acked || fail "ledger state must become acked" \
  "ledger=$ledger_out trace=$(cat "$trace" 2>/dev/null) client=$(cat "$tmp/client.log" 2>/dev/null)"
# Every settled task files its completion receipt, and this one's head is the
# payload report the fixture wrote — the streamed answer no longer settles.
rows_any "$tmp/ledger.json" kind completion || fail "the settled task must file a completion row" \
  "ledger=$ledger_out trace=$(cat "$trace" 2>/dev/null) client=$(cat "$tmp/client.log" 2>/dev/null)"
out_head=$(row_value "$tmp/ledger.json" out_head state acked)
[ "$out_head" = "$PAYLOAD_HEAD" ] \
  || fail "the acked row must carry the payload report in out_head" "out_head=$out_head ledger=$ledger_out"
case "$out_head" in
  *"$ANSWER"*) fail "out_head must not fall back to the streamed answer" "out_head=$out_head" ;;
esac

sessions_out=""
for _ in $(seq 1 120); do
  sessions_out=$("$ONLYNE" --server-root "$tmp/server" sessions --task "$task" 2>/dev/null) || true
  printf '%s\n' "$sessions_out" > "$tmp/sessions.json"
  if rows_any "$tmp/sessions.json" public_lifecycle exited 2>/dev/null; then
    break
  fi
  sleep 0.5
done
rows_any "$tmp/sessions.json" public_lifecycle exited || fail "sessions public_lifecycle must be exited" \
  "sessions=$sessions_out client=$(cat "$tmp/client.log" 2>/dev/null)"
[ "$(row_value "$tmp/sessions.json" outcome)" = "done" ] || fail "sessions outcome must be done" "$sessions_out"

# The journal's two halves, semantically: the client's dispatch first, the
# agent's four updates in the order it sent them, then the client's own read of
# the payload report and the turn record closing the file.
journal_report=$(cat "$events" 2>/dev/null || true)
if ! python3 - "$events" "$task" "$acp_session" "$TASK_PROSE" "$REASONING" "$ANSWER" "$TOOL_TITLE" \
  "$report_path" "$PAYLOAD_HEAD" <<'PY'
import json
import sys

path, task, session, prose, reasoning, answer, tool_title, report_path, payload_head = sys.argv[1:10]
lines = [line for line in open(path, encoding="utf-8").read().splitlines() if line.strip()]
records = [json.loads(line) for line in lines]

ours = [record for record in records if "onlyne" in record]
assert len(ours) == 3, ours
assert records[0] is ours[0], "the dispatch record opens the journal"
assert records[-1] is ours[2], "the turn record closes the journal"
dispatch, payload, turn = (record["onlyne"] for record in ours)
assert dispatch["kind"] == "dispatch" and dispatch["task_id"] == task, dispatch
assert dispatch["prose"].startswith(prose), dispatch
assert f"Result report (write before you stop): {report_path}" in dispatch["prose"], dispatch
assert payload["kind"] == "payload" and payload["task_id"] == task, payload
assert payload["path"] == report_path, payload
assert payload["payload_kind"] == "done", payload
assert payload["head"] == payload_head, payload
assert turn["kind"] == "turn" and turn["task_id"] == task, turn
assert turn["stop_reason"] == "end_turn", turn
# The report stands in for the closing message: the head of record is the
# payload line, and the streamed answer survives only in the agent's update.
assert turn["head"] == payload_head, turn

updates = [record for record in records if "onlyne" not in record]
assert [update.get("sessionUpdate") for update in updates] == [
    "agent_thought_chunk",
    "tool_call",
    "tool_call_update",
    "agent_message_chunk",
], updates
assert all(update.get("sessionId") == session for update in updates), updates
thought, call, done, message = updates
assert thought["content"]["text"] == reasoning, thought
assert message["content"]["text"] == answer, message
assert call["toolCallId"] == done["toolCallId"], (call, done)
assert call["title"] == tool_title, call
assert call["kind"] == "edit" and call["status"] == "pending", call
assert done["status"] == "completed", done
PY
then
  fail "the journal must hold dispatch, updates, payload and turn in order" "$journal_report"
fi

# The index is the role-wide cursor over those same records, so it has one entry
# per journal line, in order, naming the journal file and the exact bytes it
# indexed — read back here the way `read_content_records` does.
index="$ws/.onlyne/logs/content.index.jsonl"
index_report=$(cat "$index" 2>/dev/null || true)
if ! python3 - "$index" "$events" "$task" "$acp_session" <<'PY'
import json
import os
import sys

index_path, events_path, task, session = sys.argv[1:5]
journal = [line for line in open(events_path, encoding="utf-8").read().splitlines() if line.strip()]
entries = [json.loads(line) for line in open(index_path, encoding="utf-8").read().splitlines() if line.strip()]
assert len(entries) == len(journal), (len(entries), len(journal))
assert [entry["task_id"] for entry in entries] == [task] * len(entries), entries
assert [entry["session_id"] for entry in entries] == [session] * len(entries), entries
seqs = [entry["seq"] for entry in entries]
assert all(later > earlier for earlier, later in zip(seqs, seqs[1:])), seqs
assert all(entry["journal"] == os.path.basename(events_path) for entry in entries), entries
blob = open(events_path, "rb").read()
for entry, line in zip(entries, journal):
    body = blob[entry["offset"]:entry["offset"] + entry["len"]]
    assert json.loads(body) == json.loads(line), (entry, body, line)
PY
then
  fail "content.index.jsonl must index every journal line for this task and session" \
    "index=$index_report journal=$journal_report"
fi

# The operator-facing render: the same turn as prose, reasoning prefixed and
# each tool call one line per half of its life.
[ -s "$log" ] || fail "the ACP backend must render the turn into $log" \
  "logs=$(ls -1 "$ws/.onlyne/logs" 2>/dev/null) client=$(cat "$tmp/client.log" 2>/dev/null)"
log_report=$(cat "$log" 2>/dev/null || true)
for want in "> $REASONING" "tool $TOOL_CALL_ID $TOOL_TITLE kind=$TOOL_KIND" \
  "tool $TOOL_CALL_ID status=completed" "$ANSWER"; do
  if ! grep -q -F -- "$want" "$log"; then
    fail "the rendered log must carry: $want" "$log_report"
  fi
done

# The fixture's own record: the pid the reference names, the constants both
# files share, and the settings the client applied on this session.
trace_report=$(cat "$trace" 2>/dev/null || true)
for want in \
  "start pid=$acp_pid constants reasoning=$REASONING answer=$ANSWER tool=$TOOL_TITLE call=$TOOL_CALL_ID kind=$TOOL_KIND" \
  "initialize protocolVersion=1 client=onlyne-client" \
  "session/new id=$acp_session" \
  "session/set_mode id=$acp_session modeId=$ACCEPT_MODE" \
  "session/set_config_option id=$acp_session configId=model value=$FIXTURE_MODEL" \
  "session/set_config_option id=$acp_session configId=reasoning_effort value=$FIXTURE_EFFORT" \
  "session/prompt id=$acp_session text=$TASK_PROSE" \
  "Result report (write before you stop): $report_path" \
  "gate open" \
  "payload line=hop-done: $PAYLOAD_HEAD"; do
  if ! grep -q -F -- "$want" "$trace"; then
    fail "the fixture trace must carry: $want" "$trace_report"
  fi
done

# The contract's downgrading half: a fresh task whose prose carries `HOPFAIL`
# makes the fixture report `hop-failed:`, and that one line must settle the
# task Failed — reason as head and fault note, receipt still filed, report
# consumed. The second turn runs on the same client path as the first.
fail_prose='acp session task: HOPFAIL settle this one from the report'
send2_out=$("$ONLYNE" --server-root "$tmp/server" send --from planner --to planner --text "$fail_prose") \
  || fail "second send command failed" "$send2_out"
printf '%s\n' "$send2_out" > "$tmp/send2.json"
[ "$(json_field "$tmp/send2.json" '.ok' 'str(json.load(sys.stdin).get("ok")).lower()')" = "true" ] \
  || fail "second send ok must be true" "$send2_out"
task2=$(json_field "$tmp/send2.json" '.data.task' 'json.load(sys.stdin)["data"]["task"]')
ledger2_out=""
out_head2=""
for _ in $(seq 1 120); do
  ledger2_out=$("$ONLYNE" --server-root "$tmp/server" ledger --task "$task2" 2>/dev/null) || true
  printf '%s\n' "$ledger2_out" > "$tmp/ledger2.json"
  out_head2=$(row_value "$tmp/ledger2.json" out_head state acked)
  if [ "$out_head2" = "$PAYLOAD_FAIL_NOTE" ]; then
    break
  fi
  sleep 0.5
done
[ "$out_head2" = "$PAYLOAD_FAIL_NOTE" ] \
  || fail "the failed task must carry the report's reason in out_head" \
     "out_head=$out_head2 ledger=$ledger2_out trace=$(cat "$trace" 2>/dev/null) client=$(cat "$tmp/client.log" 2>/dev/null)"
rows_any "$tmp/ledger2.json" kind completion || fail "a failed ACP task must still file its completion" \
  "ledger=$ledger2_out client=$(cat "$tmp/client.log" 2>/dev/null)"
sessions2_out=""
for _ in $(seq 1 120); do
  sessions2_out=$("$ONLYNE" --server-root "$tmp/server" sessions --task "$task2" 2>/dev/null) || true
  printf '%s\n' "$sessions2_out" > "$tmp/sessions2.json"
  if rows_any "$tmp/sessions2.json" public_lifecycle exited 2>/dev/null; then
    break
  fi
  sleep 0.5
done
rows_any "$tmp/sessions2.json" public_lifecycle exited || fail "the failed session must reach exited" \
  "sessions=$sessions2_out client=$(cat "$tmp/client.log" 2>/dev/null)"
[ "$(row_value "$tmp/sessions2.json" outcome)" = "failed" ] \
  || fail "the failed report must settle the session as failed" "$sessions2_out"
if ! grep -q -F -- "payload line=hop-failed: $PAYLOAD_FAIL_NOTE" "$trace"; then
  fail "the fixture trace must carry the failed report line" "$(cat "$trace" 2>/dev/null)"
fi
[ ! -e "$ws/.onlyne/out/$task2.md" ] || fail "the client must consume the report file" \
  "reports=$(ls -1 "$ws/.onlyne/out" 2>/dev/null)"
faults=$(db_count "$db" "SELECT COUNT(*) FROM faults WHERE task_id='$task2' AND kind='acp'") || faults=0
[ "${faults:-0}" -ge 1 ] || fail "a failed ACP turn must record one acp fault" \
  "faults=$faults ledger=$ledger2_out client=$(cat "$tmp/client.log" 2>/dev/null)"

# The turns are over and every assertion has passed, so the client is drained
# exactly as an operator would stop it: it closes its session on the way out,
# which is what ends the ACP child this case read its pid from.
drain_pid "$client_pid"
client_pid=""
child_gone="false"
for _ in $(seq 1 100); do
  if ! alive "$acp_pid"; then
    child_gone="true"
    break
  fi
  sleep 0.1
done
[ "$child_gone" = "true" ] || fail "the ACP child must be gone once its client is drained" \
  "pid=$acp_pid trace=$(cat "$trace" 2>/dev/null) client=$(cat "$tmp/client.log" 2>/dev/null)"
acp_pid=""
if ! python3 - "$socket" <<'PY'
import socket
import sys

probe = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
try:
    probe.connect(sys.argv[1])
except OSError:
    raise SystemExit(0)
raise SystemExit("the published socket still answers")
PY
then
  fail "the published socket must stop listening with its client" "$socket"
fi

echo "PASS acp-session"
