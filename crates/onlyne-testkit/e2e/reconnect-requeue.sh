#!/usr/bin/env bash
set -euo pipefail
# Verification case 4 (docs/v1-PLAN.md line 503): disconnect and recovery, plus
# the completion an outbox carries across a reconnect.
#
# What this case proves, on real processes:
#   * `kill -9` on the client leaves the three task rows it was holding in the
#     ledger as `queued` or `in_flight` -- no graceful `bye` requeues for a role
#     that is gone -- and the server reports that role `offline`. The client's
#     two `max_sessions` slots are what hold the third row back while it runs,
#     and all three are still there after it is killed;
#   * the returning client is handed the three rows again in send order, each of
#     them reaches `acked` exactly once, and `sessions` holds one row per task
#     with no repeat. No row may be refused `session_dead` instead: a session
#     staged for a redelivered task is served by the agent mounted for it, not
#     left for the grace sweep;
#   * the completion of a task still running when its transport dies waits in the
#     client's intent outbox while the server is down, and the reconnect delivers
#     that same envelope: the `op_id` pinned before the restart is the id the
#     restarted server receipts, so the task settles off the completion and no
#     second completion row exists;
#   * `assign.prose` reaches every agent as the role entry's prose
#     (`assert_prose_equals` in both scripts below).
#
# One agent process serves one session: the client binds a mounting plugin to the
# one session it hands it, and that connection serves nothing else, so every
# session this case stages needs its own `onlyne-agent-fake`. Handing all four
# assignments to a single process -- what this case used to do -- left the
# sessions whose transport never mounted in place, and past
# `[client] reconnect_grace_secs` the grace sweep refused their rows
# `session_dead` with `acked` still at one.
# No real platform credential or window manager is touched: ONLYNE_BACKEND=fake plus the fake gateway only.
SRC=$(pwd)
tmp=$(mktemp -d)
server_pid=""
client_pid=""
# One entry per `onlyne-agent-fake` process, because each of them serves exactly
# one session; `mount_agent` appends to it and cleanup reaps the whole list.
agent_pids=""
cleanup() {
  # shellcheck disable=SC2086
  kill $server_pid $client_pid $agent_pids 2>/dev/null || true
  wait 2>/dev/null || true
  rm -rf "$tmp"
}
trap cleanup EXIT
# `lib.sh` derives `BIN_DIR` from `ONLYNE_BIN_DIR`/`CARGO_TARGET_DIR` and drops a
# caller's own `BIN_DIR` on the way, which is the spelling the acceptance line
# asks for the release build with
# (`BIN_DIR=target/release crates/onlyne-testkit/e2e/reconnect-requeue.sh`).
# Handing that choice to the override the helper does read keeps it; with neither
# set the helper's own default stands.
ONLYNE_BIN_DIR=${ONLYNE_BIN_DIR:-${BIN_DIR:-}}
. "$SRC/crates/onlyne-testkit/e2e/lib.sh"

# One (wait, complete) group: the transcript a single agent process runs for the
# one session it serves. A group that sleeps keeps its task in flight long enough
# for the server to die under the session, so that task's completion is produced
# with no link to send it on and lands in the client's intent outbox.
agent_script() {
  local sleep_ms=$1
  printf '{"hello":{"capabilities":["register","report","inject","recycle"]},"steps":[{"wait_assign":true},{"assert_prose_equals":"%s"},{"report":"ready"}' "$E2E_PROSE"
  if [ "$sleep_ms" -gt 0 ]; then
    printf ',{"sleep_ms":%s}' "$sleep_ms"
  fi
  printf ',{"complete":{"outcome":"done","head_from":"assign_body"}}]}\n'
}

# `ledger_count <answer-file> <state>...` prints how many task rows carry one of
# the named states. The ledger also records the completion each settled task
# answers with (plan §3 line 152), so a count over every row would double.
ledger_count() {
  local file=$1
  shift
  local states
  states=$(printf '"%s",' "$@")
  json_field "$file" '.data.ledger | map(select(.state)) | length' "sum(1 for row in json.load(sys.stdin).get('data',{}).get('ledger',[]) if row.get('state') in (${states%,}) and row.get('kind') == 'task')"
}

# How many task rows this role has had settled, read afresh from the server.
acked=0
refresh_acked() {
  "$ONLYNE" --server-root "$tmp/server" ledger --role planner > "$tmp/ledger-mount.json" 2>/dev/null || true
  acked=$(ledger_count "$tmp/ledger-mount.json" acked 2>/dev/null || true)
}

# `mount_agent <script>` starts one agent process and waits for the settle of the
# session the client hands it: one more task row reads `acked`, and the row count
# moves.
#
# The client gives a plugin that mounts naming no session the next session it
# stages, once, and the connection serves that session and nothing after it. So
# exactly one process may be waiting for a hand-over at a time, and the next one
# starts only after this one's task settled. A row the sweep refused with
# `session_dead` -- `onlyne_client::session::dispatch::SESSION_DEAD`, the refusal
# a session whose transport never mounted earns once the grace window expires --
# ends the wait with that reason instead of a timeout. The wait outlasts that
# window (the default `[client] reconnect_grace_secs` of 60 s), so a session left
# without a transport is reported as the refusal it earned rather than as a
# silence: eight hundred tenths of a second.
mount_agent() {
  local script=$1
  local before=$acked
  "$FAKE" --workspace "$tmp/planner" --script "$script" >>"$tmp/fake.log" 2>&1 &
  agent_pids="$agent_pids $!"
  for _ in $(seq 1 800); do
    refresh_acked
    if [ -n "$acked" ] && [ "$acked" != "$before" ]; then return 0; fi
    if rows_any "$tmp/ledger-mount.json" reason session_dead 2>/dev/null; then return 1; fi
    sleep 0.1
  done
  return 1
}

# max_sessions = 2 rides the entry's own lines through the override block, which is
# what the rule needs here: three tasks arrive while the client accepts two slots.
setup_cluster "$tmp/server" "$tmp/planner" planner cluster "" "$E2E_PROSE" 'allowed_senders = ["*", "planner"]
allowed_targets = ["planner"]
max_sessions = 2'
server_pid=$cluster_server_pid

# Phase one: the client runs alone, no agent is mounted, so three tasks stay in
# flight and no completion races the kill.
"$CLIENT" run --workspace "$tmp/planner" >"$tmp/client-1.log" 2>&1 &
client_pid=$!
socket=""
for _ in $(seq 1 100); do
  if [ -S "$tmp/planner/.onlyne/run/s" ]; then socket="$tmp/planner/.onlyne/run/s"; break; fi
  sleep 0.1
done
[ -n "$socket" ] || blocked "planner client socket never appeared" "client=$(cat "$tmp/client-1.log" 2>/dev/null)"

task_ids=""
n=1
while [ "$n" -le 3 ]; do
  send_out=$("$ONLYNE" --server-root "$tmp/server" send --from planner --to planner --text "requeue $n") || fail "send $n failed" "$send_out"
  printf '%s\n' "$send_out" > "$tmp/send-$n.json"
  id=$(json_field "$tmp/send-$n.json" '.data.task' 'json.load(sys.stdin).get("data",{}).get("task","")' 2>/dev/null || true)
  [ -n "$id" ] || fail "send $n must answer data.task" "$send_out"
  task_ids="$task_ids $id"
  n=$((n + 1))
done

for _ in $(seq 1 120); do
  "$ONLYNE" --server-root "$tmp/server" ledger --role planner > "$tmp/ledger-before.json" 2>/dev/null || true
  if [ "$(json_field "$tmp/ledger-before.json" '.data.ledger | length' 'len(json.load(sys.stdin).get("data",{}).get("ledger",[]))' 2>/dev/null || true)" = "3" ]; then
    break
  fi
  sleep 0.5
done
[ "$(json_field "$tmp/ledger-before.json" '.data.ledger | length' 'len(json.load(sys.stdin).get("data",{}).get("ledger",[]))' 2>/dev/null || true)" = "3" ] || fail "three ledger rows must exist before the kill" "$(cat "$tmp/ledger-before.json")"

# Plan line 503: `kill -9` the client, so no graceful `bye` can requeue for it.
kill -9 "$client_pid" 2>/dev/null || true
wait "$client_pid" 2>/dev/null || true
client_pid=""

for _ in $(seq 1 100); do
  "$ONLYNE" --server-root "$tmp/server" roles > "$tmp/roles.json" 2>/dev/null || true
  if rows_any "$tmp/roles.json" state offline 2>/dev/null; then
    break
  fi
  sleep 0.1
done
rows_any "$tmp/roles.json" state offline || blocked "the server never reported the killed client offline" "roles=$(cat "$tmp/roles.json") client=$(cat "$tmp/client-1.log" 2>/dev/null)"

"$ONLYNE" --server-root "$tmp/server" ledger --role planner > "$tmp/ledger-killed.json" 2>/dev/null || true
kept=$(ledger_count "$tmp/ledger-killed.json" queued in_flight)
[ "$kept" = "3" ] || fail "the three rows must survive the kill as queued or in_flight" "kept=$kept $(cat "$tmp/ledger-killed.json")"

# The cursor for the ordering check below: every event past it belongs to the
# reconnect, so the first `in_flight` each row reaches past it is the moment the
# restarted client was handed that row.
"$ONLYNE" --server-root "$tmp/server" history --limit 500 > "$tmp/history-mark.json" 2>/dev/null || fail "history query failed" "$(cat "$tmp/history-mark.json" 2>/dev/null)"
mark=$(json_field "$tmp/history-mark.json" '.data.events | map(.seq) | max // 0' 'max([row.get("seq", 0) for row in json.load(sys.stdin).get("data", {}).get("events", [])] or [0])')

# Phase two: the client returns, and each redelivered task gets an agent process
# of its own. The server re-offers the three rows the dropped link left behind,
# and a session with no transport waits for the plugin its own mount will bring,
# so one process per session is what settles all three.
"$CLIENT" run --workspace "$tmp/planner" >"$tmp/client-2.log" 2>&1 &
client_pid=$!
agent_script 0 > "$tmp/planner-script.json"

n=1
while [ "$n" -le 3 ]; do
  mount_agent "$tmp/planner-script.json" || fail "the agent mounted for task $n never settled a redelivered task" "acked=$acked $(cat "$tmp/ledger-mount.json" 2>/dev/null) fake=$(cat "$tmp/fake.log" 2>/dev/null)"
  n=$((n + 1))
done
[ "$acked" = "3" ] || fail "all three rows must reach acked after the restart" "acked=$acked fake=$(cat "$tmp/fake.log" 2>/dev/null) $(cat "$tmp/ledger-mount.json")"

# No row may carry the sweep's refusal: a row reading `session_dead` is a session
# the client staged and no transport ever mounted, which is the shape this case
# exists to catch and not to tolerate.
if rows_any "$tmp/ledger-mount.json" reason session_dead; then
  fail "a redelivered session was refused session_dead instead of being served" "$(cat "$tmp/ledger-mount.json")"
fi

# Each of the three rows is acked once: three acked task rows, the three task ids
# that were sent, and three distinct stamps. The ack stamps cannot carry the
# send order -- each redelivered task is served by its own process, so the acks
# land in the order those mounts took their sessions -- which is why the order
# claim is read off the server's delivery events below.
once=$(data_rows "$tmp/ledger-mount.json" | python3 -c '
import json, sys
want = set(sys.argv[1].split())
rows = [json.loads(line) for line in sys.stdin]
acked = [row for row in rows if row.get("kind") == "task" and row.get("state") == "acked"]
stamps = [row.get("acked_at") or "" for row in acked]
print("ok" if len(acked) == 3 and len(set(stamps)) == 3 and {row.get("task") for row in acked} == want else "no")
' "$task_ids")
[ "$once" = "ok" ] || fail "each of the three redelivered tasks must be acked exactly once" "once=$once $(cat "$tmp/ledger-mount.json")"

# Redelivery order: plan line 503 has the three rows re-offered in `seq` order.
# The server's own `ledger_state` events are the witness -- `pull` publishes the
# `in_flight` event of every row it hands out, in the order it hands them out --
# so the first delivery each row reached past the mark is that order, seen from
# the server side.
"$ONLYNE" --server-root "$tmp/server" history --since "$mark" --kind ledger_state > "$tmp/history-after.json" 2>/dev/null || fail "history query failed" "$(cat "$tmp/history-after.json" 2>/dev/null)"
order=$(python3 - "$tmp/history-after.json" "$task_ids" <<'PY'
import json, sys
want = sys.argv[2].split()
payload = json.load(open(sys.argv[1]))
seen = []
for row in payload.get("data", {}).get("events", []):
    event = row.get("event", row)
    if event.get("type") != "ledger_state":
        continue
    body = event.get("data") or {}
    if isinstance(body, str):
        body = json.loads(body)
    task = body.get("task")
    if body.get("kind") != "task" or body.get("state") != "in_flight":
        continue
    if task in want and task not in seen:
        seen.append(task)
print("ok" if seen == want else "no")
PY
)
[ "$order" = "ok" ] || fail "the three rows must be redelivered in send order" "order=$order mark=$mark $(cat "$tmp/history-after.json")"

# Plan line 503: three task rows in `sessions`, one per task, no repeat.
"$ONLYNE" --server-root "$tmp/server" sessions > "$tmp/sessions.json" 2>/dev/null || fail "sessions query failed" "$(cat "$tmp/sessions.json" 2>/dev/null)"
shape=$(data_rows "$tmp/sessions.json" | python3 -c '
import json, sys
rows = [json.loads(line) for line in sys.stdin]
tasks = [row.get("task_id") for row in rows]
print("ok" if len(rows) == 3 and len(set(tasks)) == 3 else "no")
')
[ "$shape" = "ok" ] || fail "sessions must hold exactly three rows, one per task" "shape=$shape $(cat "$tmp/sessions.json")"

# Plan line 503, last sentence: the completion of a task still running when its
# transport dies is carried by the intent outbox and delivered after the reconnect.
n=4
send_out=$("$ONLYNE" --server-root "$tmp/server" send --from planner --to planner --text "inflight $n") || fail "send $n failed" "$send_out"
printf '%s\n' "$send_out" > "$tmp/send-$n.json"
inflight=$(json_field "$tmp/send-$n.json" '.data.task' 'json.load(sys.stdin).get("data",{}).get("task","")')

# One more session, so one more process: it parks, the client hands it the
# session it stages for task $n, and it sleeps 2500 ms inside the assignment.
agent_script 2500 > "$tmp/inflight-script.json"
"$FAKE" --workspace "$tmp/planner" --script "$tmp/inflight-script.json" >>"$tmp/fake.log" 2>&1 &
agent_pids="$agent_pids $!"

# The row reaches `in_flight` when the server hands it to the online role, so the
# client-side session is what proves the agent holds the task: the kill has to
# land while that session runs, which is the state case 4 describes.
inflight_sessions() {
  data_rows "$1" 2>/dev/null | python3 -c '
import json, os, sys
want = os.environ.get("INFLIGHT", "")
print(sum(1 for line in sys.stdin if (json.loads(line).get("task_id") or "") == want))
'
}
for _ in $(seq 1 120); do
  "$ONLYNE" --server-root "$tmp/server" ledger --task "$inflight" > "$tmp/ledger-inflight.json" 2>/dev/null || true
  "$ONLYNE" --server-root "$tmp/server" sessions > "$tmp/sessions-inflight.json" 2>/dev/null || true
  if rows_any "$tmp/ledger-inflight.json" state in_flight 2>/dev/null \
    && [ "$(INFLIGHT="$inflight" inflight_sessions "$tmp/sessions-inflight.json" 2>/dev/null || echo 0)" != "0" ]; then
    break
  fi
  sleep 0.1
done
rows_any "$tmp/ledger-inflight.json" state in_flight || blocked "task $n never reached in_flight, so no completion could be caught in the outbox" "$(cat "$tmp/ledger-inflight.json")"
[ "$(INFLIGHT="$inflight" inflight_sessions "$tmp/sessions-inflight.json" 2>/dev/null || echo 0)" != "0" ] || blocked "the client never held task $n in a session" "$(cat "$tmp/sessions-inflight.json" 2>/dev/null)"
# The hand-over reaches the agent a moment after the session row lands.
sleep 0.5

# The agent sleeps 2500 ms before completing, so the server dies while the task runs.
kill -9 "$server_pid" 2>/dev/null || true
wait "$server_pid" 2>/dev/null || true
server_pid=""

client_db="$tmp/planner/.onlyne/client.db"
pending=""
for _ in $(seq 1 120); do
  [ -f "$client_db" ] || sleep 0.25
  pending=$(db_count "$client_db" "SELECT COUNT(*) FROM intents WHERE env_json LIKE '%$inflight%' AND state IN ('pending','retrying')" 2>/dev/null || true)
  if [ "$pending" != "0" ] && [ -n "$pending" ]; then break; fi
  sleep 0.25
done
[ -n "$pending" ] && [ "$pending" != "0" ] || blocked "the completion never landed in the intent outbox while the server was down" "pending=$pending fake=$(cat "$tmp/fake.log" 2>/dev/null)"

# The `op_id` the outbox pinned while the link was down: the retry after the
# reconnect is this same op, not a new one, which is what makes the at-least-once
# delivery of an intent safe.
pinned=$(db_count "$client_db" "SELECT op_id FROM intents WHERE env_json LIKE '%$inflight%' AND state IN ('pending','retrying') ORDER BY created_at LIMIT 1")

"$SERVER" run --root "$tmp/server" >"$tmp/server-2.log" 2>&1 &
server_pid=$!
for _ in $(seq 1 100); do
  if "$ONLYNE" --server-root "$tmp/server" wait-ready >/dev/null 2>&1; then break; fi
  sleep 0.1
done
"$ONLYNE" --server-root "$tmp/server" wait-ready >/dev/null 2>&1 || blocked "the restarted server never answered wait-ready" "$(cat "$tmp/server-2.log" 2>/dev/null)"

# The completion envelope rides the queue, and the delivery ack that follows it
# names the same task, so the count reaches one or more rather than exactly one.
accepted=""
for _ in $(seq 1 240); do
  accepted=$(db_count "$client_db" "SELECT COUNT(*) FROM intents WHERE env_json LIKE '%$inflight%' AND state='accepted'" 2>/dev/null || true)
  if [ -n "$accepted" ] && [ "$accepted" != "0" ]; then break; fi
  sleep 0.5
done
[ -n "$accepted" ] && [ "$accepted" != "0" ] || fail "the outbox row for task $n must reach accepted after the reconnect" "accepted=$accepted rows=$(db_count "$client_db" "SELECT op_id,state FROM intents")"

"$ONLYNE" --server-root "$tmp/server" ledger --task "$inflight" > "$tmp/ledger-inflight-after.json" 2>/dev/null || true
rows_any "$tmp/ledger-inflight-after.json" state acked || fail "the delivered completion must ack task $n in the ledger" "$(cat "$tmp/ledger-inflight-after.json")"

# The pinned op travelled: the completion row the restarted server receipts
# carries the id the outbox minted before the link dropped, and the task holds
# one completion row and no second op. A re-stamped envelope would land as a
# second completion, and a re-stamped id whose bytes differ would earn the
# server's `op_id conflict` refusal and settle nothing.
receipted=$(row_value "$tmp/ledger-inflight-after.json" op_id kind completion)
completions=$(data_rows "$tmp/ledger-inflight-after.json" | python3 -c '
import json, sys
print(sum(1 for line in sys.stdin if json.loads(line).get("kind") == "completion"))
')
[ -n "$pinned" ] || fail "the outbox row must name the op_id it pinned" "pinned=$pinned rows=$(db_count "$client_db" "SELECT op_id,state FROM intents")"
[ "$receipted" = "$pinned" ] || fail "the reconnect must deliver the pinned op_id, not a fresh one" "pinned=$pinned receipted=$receipted $(cat "$tmp/ledger-inflight-after.json")"
[ "$completions" = "1" ] || fail "task $n must hold exactly one completion row" "completions=$completions $(cat "$tmp/ledger-inflight-after.json")"

echo "PASS reconnect-requeue"
