#!/usr/bin/env bash
set -euo pipefail
# Verification case 1 (docs/v1-PLAN.md lines 483-498): one machine, one task, end to end.
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

# Planner registers through `onlyne-client init --prose "v1 smoke prose"`, so the
# printed entry carries the prose the fake agent asserts against its
# `assign.prose` (docs/v1-PLAN.md line 498) and the ACL pair the plan's case-1
# send needs (`--from planner --to planner`, line 496). `["*", "planner"]` is the
# self pair: the wildcard covers every other registered role, and the role's own
# name adds the self edge that a bare wildcard would deny.
setup_cluster "$tmp/server" "$tmp/planner" planner cluster 127.0.0.1:7899 "$E2E_PROSE" '["*", "planner"]' '["planner"]'
server_pid=$cluster_server_pid

"$CLIENT" run --workspace "$tmp/planner" >"$tmp/client.log" 2>&1 &
client_pid=$!
"$FAKE" --workspace "$tmp/planner" --script "$SRC/crates/onlyne-testkit/scripts/echo-complete.json" >"$tmp/fake.log" 2>&1 &
fake_pid=$!

send_out=$("$ONLYNE" --server-root "$tmp/server" send --from planner --to planner --text "hello v1") || fail "send command failed" "$send_out"
printf '%s\n' "$send_out" > "$tmp/send.json"
[ "$(json_field "$tmp/send.json" '.ok' 'json.load(sys.stdin)["ok"]')" = "True" ] || [ "$(json_field "$tmp/send.json" '.ok' 'json.load(sys.stdin)["ok"]')" = "true" ] || fail "send ok must be true" "$send_out"
task=$(json_field "$tmp/send.json" '.data.task' 'json.load(sys.stdin)["data"]["task"]')
case "$task" in
  ????????-????-4???-????-????????????) ;;
  *) fail "data.task must be uuid v4" "$task" ;;
esac
[ "$(json_field "$tmp/send.json" '.data.state' 'json.load(sys.stdin)["data"]["state"]')" = "in_flight" ] || fail "send data.state must be in_flight" "$send_out"

ledger_out=""
for _ in $(seq 1 120); do
  ledger_out=$("$ONLYNE" --server-root "$tmp/server" ledger --task "$task" 2>/dev/null) || true
  printf '%s\n' "$ledger_out" > "$tmp/ledger.json"
  if [ "$(json_field "$tmp/ledger.json" '.state' 'json.load(sys.stdin).get("state","")')" = "acked" ] 2>/dev/null; then
    break
  fi
  sleep 0.5
done
[ "$(json_field "$tmp/ledger.json" '.state' 'json.load(sys.stdin).get("state","")')" = "acked" ] || fail "ledger state must become acked" "ledger=$ledger_out fake=$(cat "$tmp/fake.log" 2>/dev/null)"
json_field "$tmp/ledger.json" '.out_head' 'json.load(sys.stdin).get("out_head","")' | grep -q "hello v1" || fail "ledger out_head must contain hello v1" "$ledger_out"

sessions_out=""
for _ in $(seq 1 120); do
  sessions_out=$("$ONLYNE" --server-root "$tmp/server" sessions --task "$task" 2>/dev/null) || true
  printf '%s\n' "$sessions_out" > "$tmp/sessions.json"
  if [ "$(json_field "$tmp/sessions.json" '.public_lifecycle' 'json.load(sys.stdin)["public_lifecycle"]')" = "exited" ] 2>/dev/null; then
    break
  fi
  sleep 0.5
done
[ "$(json_field "$tmp/sessions.json" '.public_lifecycle' 'json.load(sys.stdin)["public_lifecycle"]')" = "exited" ] || fail "sessions public_lifecycle must be exited" "sessions=$sessions_out fake=$(cat "$tmp/fake.log" 2>/dev/null)"
[ "$(json_field "$tmp/sessions.json" '.outcome' 'json.load(sys.stdin)["outcome"]')" = "done" ] || fail "sessions outcome must be done" "$sessions_out"

# Plan line 498: `echo-complete.json` asserts the agent's received `assign.prose`
# equals the spec `prose`. The dump below is the same claim in file form.
[ -s "$tmp/planner/prose.log" ] || fail "fake agent must dump received assign.prose to prose.log" "$(cat "$tmp/fake.log" 2>/dev/null)"
[ "$(cat "$tmp/planner/prose.log")" = "$E2E_PROSE" ] || fail "prose.log must equal the spec prose" "$(cat "$tmp/planner/prose.log")"
echo "PASS local-task"
