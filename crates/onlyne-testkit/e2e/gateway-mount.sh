#!/usr/bin/env bash
set -euo pipefail
# Verification case 6 (docs/v1-PLAN.md line 505): gateway mount consistency.
# No real platform credential or window manager is touched: ONLYNE_BACKEND=fake plus the fake gateway only.
SRC=$(pwd)
tmp=$(mktemp -d)
server_pid=""
client_pid=""
fake_pid=""
GW_FAKE_PID=""
cleanup() {
  kill "$server_pid" "$client_pid" "$fake_pid" "$GW_FAKE_PID" 2>/dev/null || true
  wait "$server_pid" "$client_pid" "$fake_pid" "$GW_FAKE_PID" 2>/dev/null || true
  rm -rf "$tmp"
}
trap cleanup EXIT
. "$SRC/crates/onlyne-testkit/e2e/lib.sh"

# Planner registers through `onlyne-client init --prose "v1 smoke prose"`, and
# `["*", "planner"]` is the self pair: the wildcard covers every other registered
# role, and the role's own name adds the self edge that a bare wildcard would deny.
# Its target list adds reviewer, the note recipient below.
setup_cluster "$tmp/server" "$tmp/planner" planner cluster 127.0.0.1:7899 "$E2E_PROSE" '["*", "planner"]' '["planner", "reviewer"]'
server_pid=$cluster_server_pid

# reviewer is registered without a running client, which is what case 6 means by
# an offline role. An absent spec entry fails the ACL lookup ahead of the
# delivery and answers acl_denied where the case expects recipient_offline. init
# prints the self pair for a role it registers, and this table needs
# `allowed_senders = ["planner", "reviewer"]` with an empty target list, so the
# entry is written here by hand around a key minted through the init path.
reviewer_key=$(mint_key "$tmp/reviewer-key" "$tmp/server" "$tmp/reviewer-key.frag.toml")
{
  echo '[[client]]'
  echo 'role = "reviewer"'
  printf 'key = "ed25519/%s"\n' "$reviewer_key"
  echo 'allowed_senders = ["planner", "reviewer"]'
  echo 'allowed_targets = []'
  printf 'prose = "%s"\n' "$E2E_PROSE"
} >> "$tmp/server/.onlyne/spec.toml"

# One [[gateway]] entry declares the fake platform gateway. Its key is a local
# ed25519 key minted here and names no role.
GW_FAKE=$(bin onlyne-gateway-fake)
gw_key=$(mint_key "$tmp/gw-key" "$tmp/server" "$tmp/gw-key.frag.toml")
{
  echo '[[gateway]]'
  echo 'id = "fg1"'
  echo 'platform = "fake"'
  printf 'key = "%s"\n' "$gw_key"
  echo 'enabled = true'
} >> "$tmp/server/.onlyne/spec.toml"
"$ONLYNE" --server-root "$tmp/server" reload

# onlyne-gateway-fake is a testkit product, so no onlyne-gateway binary is needed.
"$GW_FAKE" --platform fake --gateway-id fg1 --socket "$tmp/server/.onlyne/run/s" >"$tmp/gw-fake.log" 2>&1 &
GW_FAKE_PID=$!

# Start the planner client and fake agent so a Task can be delivered to a registered conversation.
"$CLIENT" run --workspace "$tmp/planner" >"$tmp/client.log" 2>&1 &
client_pid=$!
"$FAKE" --workspace "$tmp/planner" --script "$SRC/crates/onlyne-testkit/scripts/echo-complete.json" >"$tmp/fake.log" 2>&1 &
fake_pid=$!

# Wait for FakeGateway to be registered by polling server status for the gateway id.
status_out=""
for _ in $(seq 1 100); do
  status_out=$("$ONLYNE" --server-root "$tmp/server" status 2>/dev/null) || status_out=''
  printf '%s\n' "$status_out" > "$tmp/status.json"
  if echo "$status_out" | grep -q '"fg1"' 2>/dev/null; then
    break
  fi
  sleep 0.1
done
echo "$status_out" | grep -q '"fg1"' || fail "gateway status must report gateway id fg1" "$status_out"
# capabilities field check: FakeGateway declares report, typing, conversations.
echo "$status_out" | grep -q 'conversations' || fail "gateway status must report capabilities" "$status_out"

# Send a Task and wait for a rendered line from FakeGateway, proving a
# render_send frame arrived.
task_out=$("$ONLYNE" --server-root "$tmp/server" send --from planner --to planner --text "gw deliver me") || fail "gateway send failed" "$task_out"
printf '%s\n' "$task_out" > "$tmp/gw-send.json"
task=$(json_field "$tmp/gw-send.json" '.data.task' 'json.load(sys.stdin)["data"]["task"]')

delivered=""
for _ in $(seq 1 100); do
  if [ -s "$tmp/gw-fake.log" ] && grep -q '"rendered"' "$tmp/gw-fake.log" 2>/dev/null; then
    delivered=$(grep '"rendered"' "$tmp/gw-fake.log" | head -n 1)
    break
  fi
  sleep 0.1
done
[ -n "$delivered" ] || fail "FakeGateway must receive a render_send frame" "$(cat "$tmp/gw-fake.log")"
echo "$delivered" | grep -q "gw deliver me" || fail "rendered text must match sent text" "$delivered"

# Send a note to an offline role (reviewer is not registered).
note_out=$("$ONLYNE" --server-root "$tmp/server" send --from planner --to reviewer --note --text "offline note" 2>"$tmp/note.err") || true
printf '%s\n' "$note_out" > "$tmp/note.json"
[ "$(json_field "$tmp/note.json" '.error.code' 'json.load(sys.stdin)["error"]["code"]')" = "recipient_offline" ] || fail "note to offline role must be recipient_offline" "$note_out$(cat "$tmp/note.err")"

# Send a note with a short ttl_ms and assert the ledger ends as expired.
exp_out=$("$ONLYNE" --server-root "$tmp/server" send --from planner --to planner --note --ttl 100 --text "expire me" 2>"$tmp/exp.err") || true
printf '%s\n' "$exp_out" > "$tmp/exp.json"
ledger_out=""
for _ in $(seq 1 120); do
  ledger_out=$("$ONLYNE" --server-root "$tmp/server" ledger --role planner 2>/dev/null) || ledger_out=''
  printf '%s\n' "$ledger_out" > "$tmp/exp-ledger.json"
  if [ "$(json_field "$tmp/exp-ledger.json" '.rows[-1].state' 'json.load(sys.stdin).get("rows",[{}])[-1].get("state","")')" = "expired" ] 2>/dev/null; then
    break
  fi
  sleep 0.5
done
[ "$(json_field "$tmp/exp-ledger.json" '.rows[-1].state' 'json.load(sys.stdin).get("rows",[{}])[-1].get("state","")')" = "expired" ] || fail "ttl note must end as expired" "ledger=$ledger_out send=$exp_out$(cat "$tmp/exp.err")"
echo "PASS gateway-mount"
