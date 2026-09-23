#!/usr/bin/env bash
set -euo pipefail
# Verification case 15: the hello claim keeps a live session's row in flight
# across a server restart (docs/operations.md requeue section).
#
# The ARIS flap shape on real processes: the task is in flight, the client and
# its session survive, and the server dies under the link. A 1.0.8 server took
# the row back to `queued` at the first hello on the new link and handed it out
# again; the delivery flip even rode the ledger without an event. Now the
# reconnecting client declares its live tasks at `hello`, adoption leaves the
# row `in_flight` with its ticket rehung on the new link, and the one session
# completes as if the link had never moved.
SRC=$(pwd)
tmp=$(mktemp -d)
server_pid=""
client_pid=""
fake_pid=""
cleanup() {
  for pid in "$fake_pid" "$client_pid" "$server_pid"; do
    [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
  done
  wait 2>/dev/null || true
  if [ "${E2E_KEEP:-0}" = "1" ]; then
    echo "requeue-claim: scratch directory kept at $tmp" >&2
  else
    rm -rf "$tmp"
  fi
}
trap cleanup EXIT
. "$SRC/crates/onlyne-testkit/e2e/lib.sh"

SERVER=$(bin onlyne-server)
CLIENT=$(bin onlyne-client)
ONLYNE=$(bin onlyne)
FAKE=$(bin onlyne-agent-fake)

"$SERVER" init --root "$tmp/server" --listen "127.0.0.1:$(free_port)"
"$SERVER" run --root "$tmp/server" >"$tmp/server.log" 2>&1 &
server_pid=$!
for _ in $(seq 1 100); do
  "$ONLYNE" --server-root "$tmp/server" wait-ready >/dev/null 2>&1 && break
  sleep 0.1
done

client_init "$tmp/planner" planner "$tmp/server" "$tmp/server/.onlyne/spec.toml" \
  "$tmp/spec.frag.toml" "$E2E_PROSE" 'allowed_senders = ["*", "planner"]
allowed_targets = ["planner"]'
"$ONLYNE" --server-root "$tmp/server" reload

# The agent sleeps 9000 ms inside the assignment. That window carries the
# restart, the reconnect, and every ledger check below while the session is
# genuinely live in the client's memory.
printf '{"hello":{"capabilities":["register","report","inject","recycle"]},"steps":[{"wait_assign":true},{"assert_prose_equals":"%s"},{"report":"ready"},{"report":"heartbeat"},{"sleep_ms":9000},{"complete":{"outcome":"done","head_from":"assign_body"}}]}\n' \
  "$E2E_PROSE" > "$tmp/planner-script.json"

"$CLIENT" run --workspace "$tmp/planner" >"$tmp/client.log" 2>&1 &
client_pid=$!
"$FAKE" --workspace "$tmp/planner" --script "$tmp/planner-script.json" >"$tmp/fake.log" 2>&1 &
fake_pid=$!

send_out=""
for _ in $(seq 1 100); do
  send_out=$("$ONLYNE" --server-root "$tmp/server" send --from planner --to planner --text "survive the restart" 2>/dev/null) && break
  sleep 0.1
done
[ -n "$send_out" ] || blocked "no send answer before the fake plugin registered" "client=$(cat "$tmp/client.log" 2>/dev/null)"
printf '%s\n' "$send_out" > "$tmp/send.json"
task=$(json_field "$tmp/send.json" '.data.task' 'json.load(sys.stdin)["data"]["task"]')

# The in-flight window: the row reads `in_flight` and the session reads
# `working`, which is the state the restart must leave untouched.
row_state() {
  "$ONLYNE" --server-root "$tmp/server" ledger --task "$task" 2>/dev/null |
    python3 -c '
import json, sys
rows = [r for r in json.load(sys.stdin).get("data", {}).get("ledger", []) if r.get("kind") == "task"]
print(rows[0]["state"] if rows else "none")
'
}

# `ledger_counts <history-file>` prints how many `in_flight` transitions and how
# many `queued` transitions followed the first delivery. Enqueue writes its own
# leading queued event, so a claimed row's arc reads `1 0`: one delivery, no
# requeue. The history envelope nests each row under `event`.
ledger_counts() {
  python3 -c '
import json, sys
payload = json.load(open(sys.argv[1]))
states = []
for e in payload.get("data", {}).get("events", []):
    ev = e.get("event", e)
    if ev.get("type") != "ledger_state":
        continue
    body = ev.get("data")
    body = json.loads(body) if isinstance(body, str) else body
    states.append((body or {}).get("state") if (body or {}).get("kind") == "task" else None)
first = states.index("in_flight") if "in_flight" in states else len(states)
print(states.count("in_flight"), sum(1 for s in states[first:] if s == "queued"))
' "$1"
}
state=""
for _ in $(seq 1 120); do
  state=$(row_state || true)
  [ "$state" = "in_flight" ] && break
  sleep 0.25
done
[ "$state" = "in_flight" ] || blocked "the task never reached in_flight before the restart" "state=$state fake=$(cat "$tmp/fake.log" 2>/dev/null) client=$(cat "$tmp/client.log" 2>/dev/null)"

# Plan line for case 15: the server dies while the link lives. `kill -9` keeps
# any graceful requeue from running; the rows ride the restart as `in_flight`
# because nothing writes state at open.
kill -9 "$server_pid" 2>/dev/null || true
wait "$server_pid" 2>/dev/null || true
server_pid=""
"$SERVER" run --root "$tmp/server" >"$tmp/server-2.log" 2>&1 &
server_pid=$!
for _ in $(seq 1 100); do
  "$ONLYNE" --server-root "$tmp/server" wait-ready >/dev/null 2>&1 && break
  sleep 0.1
done
"$ONLYNE" --server-root "$tmp/server" wait-ready >/dev/null 2>&1 ||
  blocked "the restarted server never answered wait-ready" "$(cat "$tmp/server-2.log" 2>/dev/null)"

# The reconnect: the client's hello carries the claim, and the presence row
# going online means adoption requeue has already run with it.
for _ in $(seq 1 200); do
  "$ONLYNE" --server-root "$tmp/server" roles > "$tmp/roles.json" 2>/dev/null || true
  rows_any "$tmp/roles.json" state online && break
  sleep 0.25
done
rows_any "$tmp/roles.json" state online ||
  blocked "the client never reconnected to the restarted server" "roles=$(cat "$tmp/roles.json") client=$(cat "$tmp/client.log" 2>/dev/null)"

# The claim held: the row is still `in_flight`, the event log holds no `queued`
# transition for this task, and the session axis still shows one working row.
state=$(row_state || true)
[ "$state" = "in_flight" ] ||
  fail "the hello claim must keep the row in_flight across the restart" "state=$state roles=$(cat "$tmp/roles.json") server=$(cat "$tmp/server-2.log" 2>/dev/null)"
"$ONLYNE" --server-root "$tmp/server" history --task "$task" > "$tmp/history.json" 2>/dev/null ||
  fail "history query failed" "$(cat "$tmp/history.json" 2>/dev/null)"
counts=$(ledger_counts "$tmp/history.json")
[ "$counts" = "1 0" ] ||
  fail "the claimed row must show one delivery and no requeue" "counts=$counts history=$(cat "$tmp/history.json")"
"$ONLYNE" --server-root "$tmp/server" sessions --task "$task" > "$tmp/sessions.json" 2>/dev/null ||
  fail "sessions query failed" "$(cat "$tmp/sessions.json" 2>/dev/null)"
shape=$(data_rows "$tmp/sessions.json" | python3 -c '
import json, sys
rows = [json.loads(line) for line in sys.stdin]
tasks = {row.get("task_id") for row in rows}
print("ok" if len(rows) == 1 and len(tasks) == 1 and rows[0].get("public_lifecycle") == "working" else "no")
')
[ "$shape" = "ok" ] ||
  fail "exactly one working session row must survive the restart" "shape=$shape sessions=$(cat "$tmp/sessions.json") fake=$(cat "$tmp/fake.log" 2>/dev/null)"

# The completion rides the kept row: acked with a single delivery behind it.
acked=""
for _ in $(seq 1 240); do
  state=$(row_state || true)
  [ "$state" = "acked" ] && acked=yes && break
  sleep 0.25
done
[ -n "$acked" ] ||
  fail "the claimed in_flight row must ack the natural completion" "state=$state fake=$(cat "$tmp/fake.log" 2>/dev/null) server=$(cat "$tmp/server-2.log" 2>/dev/null) client=$(cat "$tmp/client.log" 2>/dev/null)"

# The full arc on one row: exactly one in_flight delivery event and no queued
# transition, read again after settling.
"$ONLYNE" --server-root "$tmp/server" history --task "$task" > "$tmp/history-after.json" 2>/dev/null || true
inflight=$(ledger_counts "$tmp/history-after.json")
[ "$inflight" = "1 0" ] ||
  fail "one in_flight delivery event and zero queued events must describe the arc" "counts=$inflight history=$(cat "$tmp/history-after.json")"

echo "PASS requeue-claim"
