#!/usr/bin/env bash
set -euo pipefail
# Verification case 4 (docs/v1-PLAN.md lines 500-501): disconnect and recovery, plus
# the completion an outbox carries across a reconnect.
# No real platform credential or window manager is touched: ONLYNE_BACKEND=fake plus the fake gateway only.
SRC=$(pwd)
tmp=$(mktemp -d)
server_pid=""
client_pid=""
fake_pid=""
cleanup() {
  kill "$server_pid" "$client_pid" "$fake_pid" 2>/dev/null || true
  wait "$server_pid" "$client_pid" "$fake_pid" 2>/dev/null || true
  rm -rf "$tmp"
}
trap cleanup EXIT
. "$SRC/crates/onlyne-testkit/e2e/lib.sh"

# One agent transcript carrying four (wait, complete) groups. The fourth group
# sleeps, so the last task stays in flight long enough for the server to die under
# it and for the completion to land in the client's intent outbox.
agent_script() {
  local groups=$1 sleep_ms=$2 n=1
  printf '{"hello":{"capabilities":["register","report","inject","recycle"]},"steps":['
  while [ "$n" -le "$groups" ]; do
    if [ "$n" -gt 1 ]; then printf ','; fi
    printf '{"wait_assign":true},{"assert_prose_equals":"%s"},{"report":"ready"}' "$E2E_PROSE"
    if [ "$n" = "$groups" ] && [ "$sleep_ms" -gt 0 ]; then
      printf ',{"sleep_ms":%s}' "$sleep_ms"
    fi
    printf ',{"complete":{"outcome":"done","head_from":"assign_body"}}'
    n=$((n + 1))
  done
  printf ']}\n'
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

# max_sessions = 2 rides the entry's own lines through the override block, which is
# what the rule needs here: three tasks arrive while the client accepts two slots.
setup_cluster "$tmp/server" "$tmp/planner" planner cluster "" "$E2E_PROSE" 'allowed_senders = ["*", "planner"]
allowed_targets = ["planner"]
max_sessions = 2'
server_pid=$cluster_server_pid

# Phase one: the client runs alone, so three tasks stay in flight and no completion
# races the kill.
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

# Plan line 500: `kill -9` the client, so no graceful `bye` can requeue for it.
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

# Phase two: the client returns with its agent, and all three tasks are redelivered.
"$CLIENT" run --workspace "$tmp/planner" >"$tmp/client-2.log" 2>&1 &
client_pid=$!
agent_script 4 2500 > "$tmp/planner-script.json"
"$FAKE" --workspace "$tmp/planner" --script "$tmp/planner-script.json" >"$tmp/fake.log" 2>&1 &
fake_pid=$!

for _ in $(seq 1 240); do
  "$ONLYNE" --server-root "$tmp/server" ledger --role planner > "$tmp/ledger-after.json" 2>/dev/null || true
  if [ "$(ledger_count "$tmp/ledger-after.json" acked 2>/dev/null || true)" = "3" ]; then
    break
  fi
  sleep 0.5
done
acked=$(ledger_count "$tmp/ledger-after.json" acked)
[ "$acked" = "3" ] || fail "all three rows must reach acked after the restart" "acked=$acked fake=$(cat "$tmp/fake.log" 2>/dev/null) $(cat "$tmp/ledger-after.json")"

# Redelivery order: the ledger keeps rows in enqueue order, and the ack stamps must
# rise across them, which is the plan's `seq` order seen from the server side.
order=$(data_rows "$tmp/ledger-after.json" | python3 -c '
import json, sys
rows = [json.loads(line) for line in sys.stdin]
acked = sorted((row for row in rows if row.get("state") == "acked" and row.get("kind") == "task"), key=lambda row: row.get("enqueued_at") or "")
stamps = [row.get("acked_at") or "" for row in acked]
print("ok" if len(stamps) == 3 and stamps == sorted(stamps) and len(set(stamps)) == 3 else "no")
')
[ "$order" = "ok" ] || fail "the three tasks must be acked once each in send order" "order=$order $(cat "$tmp/ledger-after.json")"

# Plan line 501: three task rows in `sessions`, one per task, no repeat.
"$ONLYNE" --server-root "$tmp/server" sessions > "$tmp/sessions.json" 2>/dev/null || fail "sessions query failed" "$(cat "$tmp/sessions.json" 2>/dev/null)"
shape=$(data_rows "$tmp/sessions.json" | python3 -c '
import json, sys
rows = [json.loads(line) for line in sys.stdin]
tasks = [row.get("task_id") for row in rows]
print("ok" if len(rows) == 3 and len(set(tasks)) == 3 else "no")
')
[ "$shape" = "ok" ] || fail "sessions must hold exactly three rows, one per task" "shape=$shape $(cat "$tmp/sessions.json")"

# Plan line 501, last sentence: the completion of a task still running when its
# transport dies is carried by the intent outbox and delivered after the reconnect.
n=4
send_out=$("$ONLYNE" --server-root "$tmp/server" send --from planner --to planner --text "inflight $n") || fail "send $n failed" "$send_out"
printf '%s\n' "$send_out" > "$tmp/send-$n.json"
inflight=$(json_field "$tmp/send-$n.json" '.data.task' 'json.load(sys.stdin).get("data",{}).get("task","")')

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
echo "PASS reconnect-requeue"
