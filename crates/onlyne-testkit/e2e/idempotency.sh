#!/usr/bin/env bash
set -euo pipefail
# Verification case 3 (docs/v1-PLAN.md line 502): pinned op_id, duplicate then conflict.
# No real platform credential or window manager is touched: ONLYNE_BACKEND=fake plus the fake gateway only.
SRC=$(pwd)
tmp=$(mktemp -d)
server_pid=""
cleanup() {
  kill "$server_pid" 2>/dev/null || true
  wait "$server_pid" 2>/dev/null || true
  rm -rf "$tmp"
}
trap cleanup EXIT
. "$SRC/crates/onlyne-testkit/e2e/lib.sh"

# The recipient stays offline: the control-plane proof needs one row per op_id
# and no completion traffic behind it. Planner registers through
# `onlyne-client init --prose "v1 smoke prose"`, and `["*", "planner"]` is the
# self pair: the wildcard covers every other registered role, and the role's own
# name adds the self edge that a bare wildcard would deny.
setup_cluster "$tmp/server" "$tmp/planner" planner cluster 127.0.0.1:7899 "$E2E_PROSE" '["*", "planner"]' '["planner"]'
server_pid=$cluster_server_pid

# `--request` replaces the whole admin `send` body, so one hand-built envelope
# pins the `op_id` the plan asks for. The envelope shape is
# `crates/onlyne-proto/src/envelope.rs` Envelope inside AdminSend.
OP_ID="o-aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"
MSG_ID="11111111-2222-4333-8444-555555555555"
send_req() {
  local text=$1 op_id=$2 body
  body=$(printf '{"from":"planner","envelope":{"protocol":1,"id":"%s","op_id":"%s","kind":"task","from":{"role":{"role":"planner"}},"to":{"role":{"role":"planner"}},"control":null,"causality":{"task":"t1","parent_task":null,"reply_to":null,"hop":0,"attempt":0},"body":{"text":"%s","image":null},"ts":"2026-01-01T00:00:00Z","ttl_ms":null,"admin":false}}' "$MSG_ID" "$op_id" "$text")
  "$ONLYNE" --server-root "$tmp/server" send --from planner --to planner --text "$text" --request "$body" 2>&1
}

first=$(send_req "hello idem" "$OP_ID") || fail "first send failed" "$first"
printf '%s\n' "$first" > "$tmp/first.json"
[ "$(json_field "$tmp/first.json" '.ok' 'json.load(sys.stdin)["ok"]')" = "True" ] || [ "$(json_field "$tmp/first.json" '.ok' 'json.load(sys.stdin)["ok"]')" = "true" ] || fail "first send ok must be true" "$first"

second=$(send_req "hello idem" "$OP_ID") || true
printf '%s\n' "$second" > "$tmp/second.json"
[ "$(json_field "$tmp/second.json" '.ok' 'json.load(sys.stdin)["ok"]')" = "False" ] || [ "$(json_field "$tmp/second.json" '.ok' 'json.load(sys.stdin)["ok"]')" = "false" ] || fail "duplicate send ok must be false" "$second"
[ "$(json_field "$tmp/second.json" '.error.code' 'json.load(sys.stdin)["error"]["code"]')" = "duplicate" ] || fail "duplicate error.code must be duplicate" "$second"
# Plan line 502: the duplicate `data` equals the first receipt byte for byte.
[ "$(json_field "$tmp/first.json" '.data' 'json.dumps(json.load(sys.stdin)["data"], sort_keys=True)')" = "$(json_field "$tmp/second.json" '.data' 'json.dumps(json.load(sys.stdin)["data"], sort_keys=True)')" ] || fail "duplicate data must equal the first receipt" "first=$first second=$second"
[ "$(json_field "$tmp/second.json" '.data.msg_id' 'json.load(sys.stdin)["data"]["msg_id"]')" = "$MSG_ID" ] || fail "duplicate data.msg_id must be the pinned envelope id" "$second"

conflict=$(send_req "hello changed" "$OP_ID") || true
printf '%s\n' "$conflict" > "$tmp/conflict.json"
[ "$(json_field "$tmp/conflict.json" '.error.code' 'json.load(sys.stdin)["error"]["code"]')" = "conflict" ] || fail "conflict error.code must be conflict" "$conflict"
[ "$(json_field "$tmp/conflict.json" '.error.message' 'json.load(sys.stdin)["error"]["message"]')" = "op_id conflict: request differs from durable receipt" ] || fail "conflict message byte-equal" "$conflict"

rows=$("$ONLYNE" --server-root "$tmp/server" ledger --role planner 2>/dev/null) || rows='{"rows":[]}'
printf '%s\n' "$rows" > "$tmp/rows.json"
count=$(json_field "$tmp/rows.json" '.rows | length' 'len(json.load(sys.stdin).get("rows",[]))')
[ "$count" = "1" ] || fail "ledger must hold exactly one row across every attempt" "count=$count rows=$rows"
echo "PASS idempotency"
