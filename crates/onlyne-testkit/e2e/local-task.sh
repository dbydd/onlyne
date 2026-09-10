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
  # `setup_cluster` starts the server before it can fail, so its pid must be in
  # the kill list even when the assignment below never ran.
  kill "$server_pid" "$client_pid" "$fake_pid" "${cluster_server_pid:-}" 2>/dev/null || true
  wait "$server_pid" "$client_pid" "$fake_pid" "${cluster_server_pid:-}" 2>/dev/null || true
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
setup_cluster "$tmp/server" "$tmp/planner" planner cluster "" "$E2E_PROSE" 'allowed_senders = ["*", "planner"]
allowed_targets = ["planner"]'
server_pid=$cluster_server_pid

"$CLIENT" run --workspace "$tmp/planner" >"$tmp/client.log" 2>&1 &
client_pid=$!
"$FAKE" --workspace "$tmp/planner" --script "$SRC/crates/onlyne-testkit/scripts/echo-complete.json" >"$tmp/fake.log" 2>&1 &
fake_pid=$!

# Plan line 432: a task aimed at a role that is not connected stays `queued`, so
# the `in_flight` assertion below is an assertion about the connected case and the
# script has to create that case. The poll watches the same admin `roles` answer
# `Presence::Online` fills, bounded at 10s, so the case cannot pass by timing.
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
[ "$registered" = "true" ] || fail "planner must register before the send" "roles=$roles_out client=$(cat "$tmp/client.log" 2>/dev/null)"

send_out=$("$ONLYNE" --server-root "$tmp/server" send --from planner --to planner --text "hello v1") || fail "send command failed" "$send_out"
printf '%s\n' "$send_out" > "$tmp/send.json"
[ "$(json_field "$tmp/send.json" '.ok' 'json.load(sys.stdin)["ok"]')" = "True" ] || [ "$(json_field "$tmp/send.json" '.ok' 'json.load(sys.stdin)["ok"]')" = "true" ] || fail "send ok must be true" "$send_out"
task=$(json_field "$tmp/send.json" '.data.task' 'json.load(sys.stdin)["data"]["task"]')
case "$task" in
  ????????-????-4???-????-????????????) ;;
  *) fail "data.task must be uuid v4" "$task" ;;
esac
[ "$(json_field "$tmp/send.json" '.data.state' 'json.load(sys.stdin)["data"]["state"]')" = "in_flight" ] || fail "send data.state must be in_flight" "$send_out"

# The shipped answers wrap rows in `data`: `{"ledger":[...]}` and
# `{"sessions":[...]}` (crates/onlyne-server/src/router.rs:158-215), so the
# assertions read rows through `rows_any` and `row_value`.
ledger_out=""
for _ in $(seq 1 120); do
  ledger_out=$("$ONLYNE" --server-root "$tmp/server" ledger --task "$task" 2>/dev/null) || true
  printf '%s\n' "$ledger_out" > "$tmp/ledger.json"
  if rows_any "$tmp/ledger.json" state acked 2>/dev/null; then
    break
  fi
  sleep 0.5
done
rows_any "$tmp/ledger.json" state acked || fail "ledger state must become acked" "ledger=$ledger_out fake=$(cat "$tmp/fake.log" 2>/dev/null)"
row_value "$tmp/ledger.json" out_head state acked | grep -q "hello v1" || fail "ledger out_head must contain hello v1" "$ledger_out"

sessions_out=""
for _ in $(seq 1 120); do
  sessions_out=$("$ONLYNE" --server-root "$tmp/server" sessions --task "$task" 2>/dev/null) || true
  printf '%s\n' "$sessions_out" > "$tmp/sessions.json"
  if rows_any "$tmp/sessions.json" public_lifecycle exited 2>/dev/null; then
    break
  fi
  sleep 0.5
done
rows_any "$tmp/sessions.json" public_lifecycle exited || fail "sessions public_lifecycle must be exited" "sessions=$sessions_out fake=$(cat "$tmp/fake.log" 2>/dev/null)"
[ "$(row_value "$tmp/sessions.json" outcome)" = "done" ] || fail "sessions outcome must be done" "$sessions_out"

# Plan line 498: `echo-complete.json` asserts the agent's received `assign.prose`
# equals the spec `prose`. The dump below is the same claim in file form.
[ -s "$tmp/planner/prose.log" ] || fail "fake agent must dump received assign.prose to prose.log" "$(cat "$tmp/fake.log" 2>/dev/null)"
[ "$(cat "$tmp/planner/prose.log")" = "$E2E_PROSE" ] || fail "prose.log must equal the spec prose" "$(cat "$tmp/planner/prose.log")"
echo "PASS local-task"
