#!/usr/bin/env bash
set -euo pipefail
# Verification case 14: heartbeat watch (docs/v1-PLAN.md §12).
#
# The ARIS incident shape, run on real processes: a role that stays connected
# while its session goes quiet. The scripted agent reports `ready`, lands one
# heartbeat, and then sleeps inside the assignment. The client republishes each
# beat, so the row is fresh while beats flow; once they stop, the server's own
# sweep records `heartbeat_missing` inside the grace window and the `sessions`
# answer carries `heartbeat_stale=true`. The lifecycle row itself stays
# `working`: the server flags, it does not flip (docs/operations.md stale note).
SRC=$(pwd)
tmp=$(mktemp -d)
server_pid=""
client_pid=""
fake_pid=""
cleanup() {
  local status=$? pid
  for pid in "$server_pid" "$client_pid" "$fake_pid" "${cluster_server_pid:-}"; do
    drain_pid "$pid"
  done
  if [ "$status" -eq 0 ]; then
    rm -rf "$tmp"
  else
    echo "heartbeat-watch: scratch directory kept at $tmp" >&2
  fi
}
trap cleanup EXIT
. "$SRC/crates/onlyne-testkit/e2e/lib.sh"

SERVER=$(bin onlyne-server)
CLIENT=$(bin onlyne-client)
ONLYNE=$(bin onlyne)
FAKE=$(bin onlyne-agent-fake)

"$SERVER" init --root "$tmp/server" --listen "127.0.0.1:$(free_port)"
# The watch interval is read at startup, so the case rewrites both knobs in the
# init template before the first run: a two-second tick and a four-second grace
# keep the whole case inside a few seconds while exercising the real sweep.
python3 - "$tmp/server/.onlyne/spec.toml" <<'PY'
import re
import sys

path = sys.argv[1]
text = open(path).read()
for key, value in (("stale_watch_secs", "2"), ("heartbeat_grace_secs", "4")):
    pattern = rf"^({key}) = \d+$"
    text, hits = re.subn(pattern, rf"\1 = {value}", text, count=1, flags=re.M)
    assert hits == 1, f"init spec must carry `{key}` once"
open(path, "w").write(text)
PY

"$SERVER" run --root "$tmp/server" >"$tmp/server.log" 2>&1 &
server_pid=$!
for _ in $(seq 1 100); do
  if "$ONLYNE" --server-root "$tmp/server" wait-ready >/dev/null 2>&1; then
    break
  fi
  sleep 0.1
done

client_init "$tmp/planner" planner "$tmp/server" "$tmp/server/.onlyne/spec.toml" \
  "$tmp/spec.frag.toml" "$E2E_PROSE" 'allowed_senders = ["*", "planner"]
allowed_targets = ["planner"]'
"$ONLYNE" --server-root "$tmp/server" reload

"$CLIENT" run --workspace "$tmp/planner" >"$tmp/client.log" 2>&1 &
client_pid=$!
"$FAKE" --workspace "$tmp/planner" --script "$SRC/crates/onlyne-testkit/scripts/heartbeat-quiet.json" >"$tmp/fake.log" 2>&1 &
fake_pid=$!

task=""
for _ in $(seq 1 100); do
  if "$ONLYNE" --server-root "$tmp/server" roles 2>/dev/null | python3 -c 'import json,sys; sys.exit(0 if any(r.get("state")=="online" for r in json.load(sys.stdin)["data"]["roles"]) else 1)'; then
    break
  fi
  sleep 0.1
done
send_out=$("$ONLYNE" --server-root "$tmp/server" send "${SUPERVISOR_FLAGS[@]}" --from planner --to planner --text "beat then go quiet")
printf '%s\n' "$send_out" > "$tmp/send.json"
task=$(json_field "$tmp/send.json" '.data.task' 'json.load(sys.stdin)["data"]["task"]')

# 1. the live phase: one beat landed, the row reads working and not stale.
fresh() {
  python3 -c 'import json,sys
rows=[r for r in json.load(sys.stdin)["data"]["sessions"] if r.get("task_id")==sys.argv[1]]
sys.exit(0 if rows and rows[0]["public_lifecycle"]=="working" and not rows[0].get("heartbeat_stale") else 1)' "$1"
}
sessions_out=""
for _ in $(seq 1 60); do
  sessions_out=$("$ONLYNE" --server-root "$tmp/server" sessions --task "$task" 2>/dev/null) || true
  printf '%s\n' "$sessions_out" | fresh "$task" && break
  sleep 0.5
done
printf '%s\n' "$sessions_out" | fresh "$task" \
  || fail "session must read working and fresh while beats flow" "sessions=$sessions_out fake=$(cat "$tmp/fake.log" 2>/dev/null)"

# 2. the quiet phase: beats stop, the sweep records the fault inside the grace
# window and the flag flips on the same row that stays `working`.
missing() {
  python3 -c 'import json,sys
rows=json.load(sys.stdin)["data"]["faults"]
sys.exit(0 if any(r.get("task_id")==sys.argv[1] and r.get("kind")=="heartbeat_missing" for r in rows) else 1)' "$1"
}
fault_seen=false
faults_out=""
for _ in $(seq 1 60); do
  faults_out=$("$ONLYNE" --server-root "$tmp/server" faults 2>/dev/null) || true
  if printf '%s\n' "$faults_out" | missing "$task"; then
    fault_seen=true
    break
  fi
  sleep 0.5
done
[ "$fault_seen" = "true" ] || fail "server must record heartbeat_missing for the quiet session" "faults=${faults_out:-} server=$(cat "$tmp/server.log" 2>/dev/null)"

stale_working() {
  python3 -c 'import json,sys
rows=[r for r in json.load(sys.stdin)["data"]["sessions"] if r.get("task_id")==sys.argv[1]]
sys.exit(0 if rows and rows[0]["heartbeat_stale"] and rows[0]["public_lifecycle"]=="working" else 1)' "$1"
}
sessions_out=$("$ONLYNE" --server-root "$tmp/server" sessions --task "$task")
printf '%s\n' "$sessions_out" | stale_working "$task" \
  || fail "the stale row must answer heartbeat_stale=true and stay working" "$sessions_out"

echo "PASS heartbeat-watch"
