#!/usr/bin/env bash
set -euo pipefail
# Case 19: payload-v2 closure and handoff routing, end to end.
#
# One ACP role authors its report through the shipped `onlyne report` verbs and
# one fake role receives what the client routes. The case proves both sides of
# the contract: a valid `hop-done:` report settles the turn and creates child
# tasks, while a denied, blocked, or malformed report changes only the routing
# evidence and never invents a verdict the client cannot read.
#
# The ACP fixture is gated so the case can write the report while the turn is
# provably still open. The report file is the only settlement input; the agent
# never calls an `onlyne` command to close its own task.
#
# One `onlyne-agent-fake` process serves one session: the client hands a
# mounting plugin the next session it stages, and that connection serves nothing
# after it. The valid report routes two child tasks to the worker, so the
# worker's client stages two sessions and needs one agent process for each — the
# second mounts once the first child has settled and the agent that served it has
# taken its session off the client's one parked slot.
SRC=$(pwd)
tmp=$(mktemp -d /tmp/onlyne-acp-payload-v2.XXXXXX)
pids=""
track() { pids="$pids $1"; }

# `alive <pid>` is true while the process exists and is not an unreaped zombie.
alive() {
  local pid=$1 state
  [ -n "$pid" ] || return 1
  state=$(ps -p "$pid" -o state= 2>/dev/null | tr -d '[:space:]')
  [ -n "$state" ] && [ "${state#Z}" = "$state" ]
}

cleanup() {
  local status=$? survived="" pid
  for pid in $pids "${cluster_server_pid:-}"; do
    drain_pid "$pid"
  done
  for pid in $pids "${cluster_server_pid:-}"; do
    if alive "$pid"; then
      survived="$survived $pid"
    fi
  done
  if [ -n "$survived" ]; then
    echo "acp-payload-v2: processes survived cleanup:$survived" >&2
    status=1
  fi
  if [ "$status" -eq 0 ] && [ "${E2E_KEEP:-0}" != 1 ]; then
    rm -rf "$tmp"
  else
    echo "acp-payload-v2: scratch directory kept at $tmp" >&2
  fi
  exit "$status"
}
trap cleanup EXIT
. "$SRC/crates/onlyne-testkit/e2e/lib.sh"

SERVER=$(bin onlyne-server)
CLIENT=$(bin onlyne-client)
ONLYNE=$(bin onlyne)
FAKE=$(bin onlyne-agent-fake)
AGENT="$SRC/crates/onlyne-testkit/e2e/acp-agent.py"
SCRIPT="$SRC/crates/onlyne-testkit/scripts/echo-complete-generic.json"
[ -f "$AGENT" ] || fail "the ACP fixture must sit beside this case" "$AGENT"
[ -f "$SCRIPT" ] || fail "the generic completion script must ship with the case" "$SCRIPT"

PLANNER=planner
RECEIVER=worker
DENIED_ROLE=ghost
CALLER_MARKER='PAYLOAD-V2-CALLER-REPORT'
HANDOFF_PREFIX='handoff: '

HEAD='The report handed two briefs to the worker.'
CUSTOM='Inspect the first handoff payload.'
DENIED_HEAD='The report named a role this ACL does not reach.'
BLOCK_REASON='The task waits on a caller that never arrives.'
INVALID_BODY='The invalid report stays where its author can fix it.'

planner_ws="$tmp/planner"
worker_ws="$tmp/worker"
gate="$tmp/gate"
trace="$tmp/agent.trace"
db="$planner_ws/.onlyne/client.db"

# The fixture receives the report path from the client directive but yields file
# ownership to this case. Without the argv marker it preserves payload-v1 and
# `acp-session.sh` byte for byte.
session_command=$(python3 -c 'import json, sys; print(json.dumps(sys.argv[1:]))' \
  python3 -u "$AGENT" --acp --gate "$gate" --trace "$trace" \
  --caller-report-marker "$CALLER_MARKER")
planner_acl=$(printf 'allowed_senders = ["*", "%s"]\nallowed_targets = ["%s", "%s"]\nsession_command = %s\n' \
  "$PLANNER" "$PLANNER" "$RECEIVER" "$session_command")
receiver_acl=$(printf 'allowed_senders = ["*", "%s"]\nallowed_targets = ["%s", "%s"]\n' \
  "$RECEIVER" "$RECEIVER" "$PLANNER")

setup_cluster "$tmp/server" "$planner_ws" "$PLANNER" payload "" "$E2E_PROSE" "$planner_acl"
server_pid=$cluster_server_pid
track "$server_pid"
client_init "$worker_ws" "$RECEIVER" "$tmp/server" "$tmp/server/.onlyne/spec.toml" \
  "$tmp/worker.frag.toml" "$E2E_PROSE" "$receiver_acl"
"$ONLYNE" --server-root "$tmp/server" reload

# The planner workspace ships the ACP backend; the lib's fake default remains
# the worker's host. Each client's backend therefore comes from one explicit
# environment value rather than one global override shared by both roles.
config="$planner_ws/.onlyne/config.toml"
{
  printf 'backend = "acp"\n'
  cat "$config"
  printf '\n[acp]\npermission = "deny"\n'
} > "$config.new"
mv "$config.new" "$config"
if ! python3 - "$config" <<'PY'
import sys
import tomllib

with open(sys.argv[1], "rb") as handle:
    doc = tomllib.load(handle)
assert doc.get("backend") == "acp", doc
assert doc.get("acp", {}).get("permission") == "deny", doc
PY
then
  fail "the planner workspace must configure the ACP backend" "$(cat "$config")"
fi

env ONLYNE_BACKEND=acp "$CLIENT" run --workspace "$planner_ws" >"$tmp/planner-client.log" 2>&1 &
planner_client_pid=$!
track "$planner_client_pid"
# `mount_worker` starts one agent process for one of the worker's sessions. A
# second unnamed mount replaces the one parked slot a client holds, so a later
# call may run only after the agent before it has taken the session it came for.
mount_worker() {
  "$FAKE" --workspace "$worker_ws" --script "$SCRIPT" >>"$tmp/worker-fake.log" 2>&1 &
  worker_fake_pid=$!
  track "$worker_fake_pid"
}

env ONLYNE_BACKEND=fake "$CLIENT" run --workspace "$worker_ws" >"$tmp/worker-client.log" 2>&1 &
worker_client_pid=$!
track "$worker_client_pid"
mount_worker

online=0
for _ in $(seq 1 200); do
  "$ONLYNE" --server-root "$tmp/server" roles >"$tmp/roles.json" 2>/dev/null || true
  online=$(json_field "$tmp/roles.json" x \
    'sum(1 for r in json.load(sys.stdin)["data"]["roles"] if r["state"] == "online")' 2>/dev/null || echo 0)
  if [ "$online" = 2 ]; then
    break
  fi
  sleep 0.1
done
[ "$online" = 2 ] || fail "both payload roles must come online" "$(cat "$tmp/roles.json" 2>/dev/null)"

prompt_count() {
  local count
  count=$(grep -c 'session/prompt id=' "$trace" 2>/dev/null || true)
  printf '%s\n' "${count:-0}"
}

wait_for_prompt() {
  local before=$1 attempt
  for attempt in $(seq 1 300); do
    if [ "$(prompt_count)" -gt "$before" ]; then
      return 0
    fi
    sleep 0.1
  done
  fail "the ACP fixture never received the gated prompt" \
    "trace=$(cat "$trace" 2>/dev/null) planner=$(cat "$tmp/planner-client.log" 2>/dev/null)"
}

capture_acp_pid() {
  local task=$1 attempt pid=""
  for attempt in $(seq 1 300); do
    pid=$(python3 - "$db" "$task" <<'PY' 2>/dev/null || true
import json
import sqlite3
import sys

try:
    row = sqlite3.connect(sys.argv[1]).execute(
        "SELECT backend_ref FROM sessions WHERE task_id=?", (sys.argv[2],)
    ).fetchone()
    if row is None or row[0] is None:
        raise SystemExit(0)
    stored = json.loads(row[0])
    inner = stored.get("backend_ref", stored)
    if isinstance(inner, str):
        inner = json.loads(inner)
    print(inner.get("pid", "") if isinstance(inner, dict) else "")
except Exception:
    pass
PY
)
    if [ -n "$pid" ]; then
      case "$pid" in
        ''|*[!0-9]*) fail "the ACP session reference must carry a numeric pid" "pid=$pid task=$task" ;;
      esac
      track "$pid"
      return 0
    fi
    sleep 0.1
  done
  fail "the ACP child must store its session reference before the report lands" \
    "task=$task trace=$(cat "$trace" 2>/dev/null) planner=$(cat "$tmp/planner-client.log" 2>/dev/null)"
}

report_path_for() {
  local task=$1 output
  output=$("$ONLYNE" --workspace "$planner_ws" report path --task "$task") \
    || fail "report path must resolve the task's report file" "$output"
  printf '%s\n' "$output" > "$tmp/report-path.$task.txt"
  REPORT_PATH=$(python3 - "$tmp/report-path.$task.txt" <<'PY'
import sys

for line in open(sys.argv[1], encoding="utf-8"):
    if line.startswith("report: "):
        print(line[len("report: "):].strip())
        break
PY
)
  [ -n "$REPORT_PATH" ] || fail "report path output must name one report file" "$(cat "$tmp/report-path.$task.txt")"
}

send_and_gate() {
  local prose=$1 out task before
  rm -f "$gate"
  before=$(prompt_count)
  out=$("$ONLYNE" --server-root "$tmp/server" send "${SUPERVISOR_FLAGS[@]}" --from "$PLANNER" --to "$PLANNER" --text "$prose") \
    || fail "the report task send failed" "$out"
  printf '%s\n' "$out" > "$tmp/send.json"
  task=$(json_field "$tmp/send.json" '.data.task' 'json.load(sys.stdin)["data"]["task"]')
  [ "$(json_field "$tmp/send.json" '.data.state' 'json.load(sys.stdin)["data"]["state"]')" = "in_flight" ] \
    || fail "the ACP task must start in_flight" "$out"
  wait_for_prompt "$before"
  capture_acp_pid "$task"
  report_path_for "$task"
  TASK="$task"
}

assert_valid_check() {
  local path=$1 expected_file=$2 output
  output=$("$ONLYNE" --workspace "$planner_ws" report check --path "$path") \
    || fail "a valid report must pass report check" "$output"
  if [ "$output" != "$(cat "$expected_file")" ]; then
    fail "report check must echo every verdict and handoff line" \
      "actual=$output expected=$(cat "$expected_file")"
  fi
}

ledger_field() {
  local file=$1 field=$2
  shift 2
  data_rows "$file" | python3 -c '
import json
import sys

field = sys.argv[1]
selects = sys.argv[2:]
try:
    rows = [json.loads(line) for line in sys.stdin]
except ValueError:
    rows = []
for row in rows:
    if all(row.get(selects[index]) == selects[index + 1] for index in range(0, len(selects), 2)):
        value = row.get(field, "")
        print("" if value is None else value)
        break
' "$field" "$@"
}

wait_root_acked() {
  local task=$1 head=$2 ledger_out="" root_head=""
  for _ in $(seq 1 120); do
    ledger_out=$("$ONLYNE" --server-root "$tmp/server" ledger --task "$task" 2>/dev/null) || true
    printf '%s\n' "$ledger_out" > "$tmp/root-ledger.$task.json"
    root_head=$(ledger_field "$tmp/root-ledger.$task.json" out_head kind completion state acked)
    if [ "$root_head" = "$head" ]; then
      return 0
    fi
    sleep 0.5
  done
  fail "the root task must settle its verdict in out_head" \
    "head=$root_head expected=$head ledger=$ledger_out trace=$(cat "$trace" 2>/dev/null) planner=$(cat "$tmp/planner-client.log" 2>/dev/null)"
}

wait_session_outcome() {
  local task=$1 want=$2 sessions_out="" outcome=""
  for _ in $(seq 1 120); do
    sessions_out=$("$ONLYNE" --server-root "$tmp/server" sessions --task "$task" 2>/dev/null) || true
    printf '%s\n' "$sessions_out" > "$tmp/sessions.$task.json"
    outcome=$(row_value "$tmp/sessions.$task.json" outcome)
    if [ "$outcome" = "$want" ]; then
      return 0
    fi
    sleep 0.5
  done
  fail "the task session must report outcome $want" \
    "outcome=$outcome sessions=$sessions_out trace=$(cat "$trace" 2>/dev/null) planner=$(cat "$tmp/planner-client.log" 2>/dev/null)"
}

validate_positive_children() {
  local file=$1 parent=$2
  python3 - "$file" "$parent" "$RECEIVER" "$HEAD" "$CUSTOM" "$HANDOFF_PREFIX" <<'PY'
import json
import sys


def principal_role(value):
    if isinstance(value, str):
        value = json.loads(value)
    role = value.get("role") if isinstance(value, dict) else None
    if isinstance(role, dict):
        role = role.get("role")
    return role if isinstance(role, str) else ""


path, parent, receiver, head, custom, prefix = sys.argv[1:7]
rows = json.load(open(path))["data"]["ledger"]
parents = [row for row in rows if row.get("kind") == "task" and row.get("task") == parent]
if len(parents) != 1:
    raise SystemExit("the root task row is missing or duplicated")
parent_hop = parents[0]["hop"]

children = [row for row in rows if row.get("parent_task") == parent and row.get("kind") == "task"]
if len(children) != 2:
    raise SystemExit("the report produced %d child tasks, not 2" % len(children))

expected = sorted([prefix + custom, prefix + head])
actual = []
for row in children:
    if row.get("state") != "acked":
        raise SystemExit("child %s is %s, not acked" % (row.get("task"), row.get("state")))
    if principal_role(row.get("from")) != "planner":
        raise SystemExit("child %s came from %s, not planner" % (row.get("task"), principal_role(row.get("from"))))
    if principal_role(row.get("to")) != receiver:
        raise SystemExit("child %s went to %s, not %s" % (row.get("task"), principal_role(row.get("to")), receiver))
    if row.get("hop") != parent_hop + 1:
        raise SystemExit("child %s is hop %s, not %d" % (row.get("task"), row.get("hop"), parent_hop + 1))
    body = json.loads(row["body_json"])
    text = body.get("text", "")
    if not text.startswith(prefix):
        raise SystemExit("child %s body lacks the literal handoff prefix" % row.get("task"))
    actual.append(text)
    if row.get("out_head") != text:
        raise SystemExit("child %s completed with %r, not %r" % (row.get("task"), row.get("out_head"), text))
    completions = [row2 for row2 in rows if row2.get("kind") == "completion" and row2.get("task") == row.get("task")]
    if not completions:
        raise SystemExit("child %s never filed a completion receipt" % row.get("task"))
if sorted(actual) != expected:
    raise SystemExit("child bodies are %s, not %s" % (actual, expected))
PY
}

child_task_ids() {
  local file=$1 parent=$2
  python3 - "$file" "$parent" <<'PY'
import json
import sys

path, parent = sys.argv[1:3]
rows = json.load(open(path))["data"]["ledger"]
for row in rows:
    if row.get("kind") == "task" and row.get("parent_task") == parent:
        print(row["task"])
PY
}

# `wait_first_child_acked <parent>` waits for one child of the report to reach
# `acked`. Both children are delivered together, so the first one to settle is
# the proof that the worker's first agent took the session it mounted for and the
# client's parked slot is free for the second agent.
wait_first_child_acked() {
  local parent=$1 attempt
  for attempt in $(seq 1 240); do
    "$ONLYNE" --server-root "$tmp/server" ledger > "$tmp/first-child.json" 2>/dev/null || true
    if data_rows "$tmp/first-child.json" | python3 -c '
import json, sys
rows = [json.loads(line) for line in sys.stdin]
sys.exit(0 if any(row.get("kind") == "task" and row.get("parent_task") == sys.argv[1] and row.get("state") == "acked" for row in rows) else 1)
' "$parent"; then
      return 0
    fi
    if rows_any "$tmp/first-child.json" reason session_dead; then
      fail "a child session was refused session_dead instead of being served" \
        "$(cat "$tmp/first-child.json" 2>/dev/null) worker=$(cat "$tmp/worker-client.log" 2>/dev/null)"
    fi
    sleep 0.5
  done
  fail "the report's first child never settled, so the worker's second agent has no session to take" \
    "children=$(cat "$tmp/first-child.json" 2>/dev/null) worker=$(cat "$tmp/worker-client.log" 2>/dev/null)"
}

validate_no_children() {
  local file=$1 parent=$2
  python3 - "$file" "$parent" <<'PY'
import json
import sys

path, parent = sys.argv[1:3]
rows = json.load(open(path))["data"]["ledger"]
found = [row.get("task") for row in rows if row.get("kind") == "task" and row.get("parent_task") == parent]
if found:
    raise SystemExit("task %s produced %s, not zero children" % (parent, found))
PY
}

wait_handoff_denied() {
  local task=$1 role=$2 attempt event=""
  for attempt in $(seq 1 120); do
    event=$(python3 - "$db" "$task" "$role" <<'PY' 2>/dev/null || true
import json
import sqlite3
import sys

try:
    rows = sqlite3.connect(sys.argv[1]).execute(
        "SELECT data_json FROM events WHERE type='handoff_denied'"
    ).fetchall()
    for raw, in rows:
        event = json.loads(raw)
        if event.get("task_id") == sys.argv[2] and event.get("to_role") == sys.argv[3]:
            print(json.dumps(event, sort_keys=True))
            break
except Exception:
    pass
PY
)
    if [ -n "$event" ]; then
      HANDOFF_DENIED_EVENT="$event"
      return 0
    fi
    sleep 0.5
  done
  fail "the client ledger must record handoff_denied for the refused relay" \
    "task=$task role=$role planner=$(cat "$tmp/planner-client.log" 2>/dev/null)"
}

# --- a valid done report routes two relays ----------------------------------
POSITIVE_PROSE="payload-v2 routes two relays $CALLER_MARKER"
send_and_gate "$POSITIVE_PROSE"
write_out=$("$ONLYNE" --workspace "$planner_ws" report write --path "$REPORT_PATH" \
  --verdict done --head "$HEAD" --handoff "$RECEIVER|$CUSTOM" --handoff "$RECEIVER") \
  || fail "report write must atomically construct a two-handoff report" "$write_out"
[ "$write_out" = "$REPORT_PATH" ] || fail "report write must print the file it renamed into place" \
  "actual=$write_out expected=$REPORT_PATH"
printf 'kind: done\nhead: %s\nhandoff: %s | %s\nhandoff: %s\n' \
  "$HEAD" "$RECEIVER" "$CUSTOM" "$RECEIVER" > "$tmp/check-positive.expected"
assert_valid_check "$REPORT_PATH" "$tmp/check-positive.expected"
touch "$gate"
wait_root_acked "$TASK" "$HEAD"
grep -Fq 'payload skipped: caller owns the report file' "$trace" \
  || fail "the ACP fixture must preserve a caller-owned report" "$(cat "$trace" 2>/dev/null)"
wait_session_outcome "$TASK" done

# The second child's session is the worker's second one, and it needs an agent
# process of its own: the one parked slot a client holds is the first agent's
# until that agent has taken its session, which is what the first child settling
# shows.
wait_first_child_acked "$TASK"
mount_worker

children_ok=false
children_report=""
for _ in $(seq 1 120); do
  "$ONLYNE" --server-root "$tmp/server" ledger >"$tmp/children-ledger.json" 2>/dev/null || true
  if children_report=$(validate_positive_children "$tmp/children-ledger.json" "$TASK" 2>&1); then
    children_ok=true
    break
  fi
  sleep 0.5
done
[ "$children_ok" = "true" ] || fail "the report must create two completed child tasks" \
  "children=$children_report trace=$(cat "$trace" 2>/dev/null) planner=$(cat "$tmp/planner-client.log" 2>/dev/null) worker=$(cat "$tmp/worker-fake.log" 2>/dev/null)"
child_tasks=$(child_task_ids "$tmp/children-ledger.json" "$TASK")
[ "$(printf '%s\n' "$child_tasks" | wc -l | tr -d ' ')" = "2" ] \
  || fail "the validated report must name two child tasks" "child_tasks=$child_tasks"
while IFS= read -r child; do
  [ -n "$child" ] || continue
  wait_session_outcome "$child" done
done <<EOF
$child_tasks
EOF

# --- an unauthorized relay is recorded, never silently dropped --------------
DENIED_PROSE="payload-v2 names an unreached role $CALLER_MARKER"
send_and_gate "$DENIED_PROSE"
"$ONLYNE" --workspace "$planner_ws" report write --path "$REPORT_PATH" \
  --verdict done --head "$DENIED_HEAD" --handoff "$DENIED_ROLE|This brief must not route." >/dev/null \
  || fail "report write must accept a syntactically valid handoff to any role token"
printf 'kind: done\nhead: %s\nhandoff: %s | This brief must not route.\n' \
  "$DENIED_HEAD" "$DENIED_ROLE" > "$tmp/check-denied.expected"
assert_valid_check "$REPORT_PATH" "$tmp/check-denied.expected"
touch "$gate"
wait_root_acked "$TASK" "$DENIED_HEAD"
wait_session_outcome "$TASK" done
wait_handoff_denied "$TASK" "$DENIED_ROLE"
if ! printf '%s\n' "$HANDOFF_DENIED_EVENT" | python3 -c '
import json, sys
event = json.load(sys.stdin)
assert event["task_id"] == sys.argv[1] and event["to_role"] == sys.argv[2], event
assert event["text"].startswith("This brief"), event
' "$TASK" "$DENIED_ROLE"; then
  fail "the handoff_denied event must name the settled task and refused role" "$HANDOFF_DENIED_EVENT"
fi
"$ONLYNE" --server-root "$tmp/server" ledger >"$tmp/denied-ledger.json" 2>/dev/null || true
validate_no_children "$tmp/denied-ledger.json" "$TASK" \
  || fail "a refused handoff must create no task row" "$(cat "$tmp/denied-ledger.json")"

# --- a blocked report lowers the turn and routes nothing --------------------
BLOCKED_PROSE="payload-v2 reports a blocker $CALLER_MARKER"
send_and_gate "$BLOCKED_PROSE"
"$ONLYNE" --workspace "$planner_ws" report write --path "$REPORT_PATH" \
  --verdict blocked --head "$BLOCK_REASON" --handoff "$RECEIVER|The blocker must skip this line." >/dev/null \
  || fail "report write must accept a blocked verdict with grammar-valid handoffs"
printf 'kind: blocked\nreason: %s\nhandoff: %s | The blocker must skip this line.\n' \
  "$BLOCK_REASON" "$RECEIVER" > "$tmp/check-blocked.expected"
assert_valid_check "$REPORT_PATH" "$tmp/check-blocked.expected"
touch "$gate"
wait_root_acked "$TASK" "$BLOCK_REASON"
wait_session_outcome "$TASK" failed
"$ONLYNE" --server-root "$tmp/server" ledger >"$tmp/blocked-ledger.json" 2>/dev/null || true
validate_no_children "$tmp/blocked-ledger.json" "$TASK" \
  || fail "a blocked report must create no child task" "$(cat "$tmp/blocked-ledger.json")"

# --- an invalid report stays fixable and cancels its turn -------------------
INVALID_PROSE="payload-v2 author rejected the grammar $CALLER_MARKER"
send_and_gate "$INVALID_PROSE"
mkdir -p "$(dirname "$REPORT_PATH")"
printf '%s\n' "$INVALID_BODY" > "$REPORT_PATH"
check_code=0
check_out=$("$ONLYNE" --workspace "$planner_ws" report check --path "$REPORT_PATH" 2>&1) || check_code=$?
[ "$check_code" = "2" ] || fail "report check must exit 2 for a non-report" "code=$check_code output=$check_out"
printf '%s\n' "$check_out" > "$tmp/check-invalid.txt"
grep -Fq 'line 1: report line carries no prefix' "$tmp/check-invalid.txt" \
  || fail "report check must name the invalid first line" "$check_out"
touch "$gate"
wait_session_outcome "$TASK" cancelled
[ -e "$REPORT_PATH" ] || fail "an invalid report must remain on disk for its author" "$REPORT_PATH"
"$ONLYNE" --workspace "$planner_ws" report write --path "$REPORT_PATH" \
  --verdict done --head "$INVALID_BODY" >/dev/null \
  || fail "the retained report path must accept a corrected report"
printf 'kind: done\nhead: %s\n' "$INVALID_BODY" > "$tmp/check-rewrite.expected"
assert_valid_check "$REPORT_PATH" "$tmp/check-rewrite.expected"

echo "PASS acp-payload-v2"
