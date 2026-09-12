#!/usr/bin/env bash
set -euo pipefail
# Verification case 2 (docs/v1-PLAN.md lines 499-501): the ACL hard refusal, taken
# from a real client connection rather than from a hand-built frame.
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

# Builder carries the self pair alone: `["*", "builder"]` admits every other role
# into builder, and `["builder"]` reaches builder only, so a send to reviewer fails
# the sender half of the rule and names `to.role`.
setup_cluster "$tmp/server" "$tmp/builder" builder cluster "" "$E2E_PROSE" 'allowed_senders = ["*", "builder"]
allowed_targets = ["builder"]'
server_pid=$cluster_server_pid

# reviewer registers with its own wildcard pair, which leaves the builder's target
# list as the binding side of the pair.
client_init "$tmp/reviewer" reviewer "$tmp/server" "$tmp/server/.onlyne/spec.toml" "$tmp/reviewer.frag.toml" "$E2E_PROSE" 'allowed_senders = ["*", "reviewer"]
allowed_targets = ["reviewer"]'
"$ONLYNE" --server-root "$tmp/server" reload

"$CLIENT" run --workspace "$tmp/builder" >"$tmp/builder-client.log" 2>&1 &
client_pid=$!
"$FAKE" --workspace "$tmp/builder" --script "$SRC/crates/onlyne-testkit/scripts/echo-complete.json" >"$tmp/builder-fake.log" 2>&1 &
fake_pid=$!

# The message verbs connect to the workspace socket, which the client binds.
socket=""
for _ in $(seq 1 100); do
  if [ -S "$tmp/builder/.onlyne/run/s" ]; then socket="$tmp/builder/.onlyne/run/s"; break; fi
  sleep 0.1
done
[ -n "$socket" ] || blocked "builder client socket never appeared" "client=$(cat "$tmp/builder-client.log" 2>/dev/null)"
[ -n "$(pgrep -f "onlyne-client run --workspace $tmp/builder" || true)" ] || blocked "builder client exited before the send" "client=$(cat "$tmp/builder-client.log" 2>/dev/null)"

# Plan line 500: `onlyne --workspace "$tmp/builder" send --to reviewer --text x`.
# `--from` stays absent, so the client surface takes the sender from ONLYNE_ROLE.
denied=$(ONLYNE_ROLE=builder "$ONLYNE" --workspace "$tmp/builder" send --to reviewer --text x 2>"$tmp/denied.err") || true
printf '%s\n' "$denied" > "$tmp/denied.json"
detail="$denied$(cat "$tmp/denied.err")"
[ "$(json_field "$tmp/denied.json" '.ok' 'str(json.load(sys.stdin).get("ok")).lower()' 2>/dev/null || true)" = "false" ] || fail "denied send ok must be false" "$detail"
[ "$(json_field "$tmp/denied.json" '.error.code' 'json.load(sys.stdin).get("error",{}).get("code","")' 2>/dev/null || true)" = "acl_denied" ] || fail "denied send error.code must be acl_denied" "$detail"
[ "$(json_field "$tmp/denied.json" '.error.field' 'json.load(sys.stdin).get("error",{}).get("field","")' 2>/dev/null || true)" = "to.role" ] || fail "denied send error.field must be to.role" "$detail"

# Plan line 501: the server ledger gains no row for a refusal.
"$ONLYNE" --server-root "$tmp/server" ledger > "$tmp/ledger.json" 2>/dev/null || fail "ledger query failed" "$(cat "$tmp/ledger.json" 2>/dev/null)"
count=$(json_field "$tmp/ledger.json" '.data.ledger | length' 'len(json.load(sys.stdin).get("data",{}).get("ledger",[]))')
[ "$count" = "0" ] || fail "a denied send must leave the ledger empty" "count=$count ledger=$(cat "$tmp/ledger.json")"
reviewer_rows=$("$ONLYNE" --server-root "$tmp/server" ledger --role reviewer 2>/dev/null) || reviewer_rows='{"ok":false,"data":{"ledger":[]}}'
printf '%s\n' "$reviewer_rows" > "$tmp/ledger-reviewer.json"
reviewer_count=$(json_field "$tmp/ledger-reviewer.json" '.data.ledger | length' 'len(json.load(sys.stdin).get("data",{}).get("ledger",[]))')
[ "$reviewer_count" = "0" ] || fail "reviewer must hold zero ledger rows" "count=$reviewer_count ledger=$reviewer_rows"

# Plan line 501: the sender keeps no intent after a refusal. The count comes from
# `db_count`, which reads through sqlite3 and falls back to python3's stdlib
# module, and fails loudly on a host with neither.
client_db="$tmp/builder/.onlyne/client.db"
[ -f "$client_db" ] || fail "builder client database must exist after the client ran" "$(ls -la "$tmp/builder/.onlyne" 2>&1)"
intents=$(db_count "$client_db" "SELECT COUNT(*) FROM intents")
[ "$intents" = "0" ] || fail "a refusal must leave no intent row" "intents=$intents rows=$(db_count "$client_db" "SELECT op_id,state FROM intents")"

# Positive control: the same client reaching its own role is accepted, which is the
# shape plan line 496 depends on. A denial-only run cannot tell wiring from policy.
control=$(ONLYNE_ROLE=builder "$ONLYNE" --workspace "$tmp/builder" send --to builder --text "self edge" 2>"$tmp/control.err") || fail "self send must be accepted" "$control$(cat "$tmp/control.err")"
printf '%s\n' "$control" > "$tmp/control.json"
[ "$(json_field "$tmp/control.json" '.ok' 'str(json.load(sys.stdin).get("ok")).lower()' 2>/dev/null || true)" = "true" ] || fail "self send ok must be true" "$control"
[ "$(json_field "$tmp/control.json" '.data.state' 'json.load(sys.stdin).get("data",{}).get("state","")' 2>/dev/null || true)" = "in_flight" ] || fail "self send data.state must be in_flight" "$control"

# The allowed send is the counterpart of the denial: one row lands, one row only.
"$ONLYNE" --server-root "$tmp/server" ledger > "$tmp/ledger-after.json" 2>/dev/null || fail "ledger query failed after the control" "$(cat "$tmp/ledger-after.json" 2>/dev/null)"
after=$(json_field "$tmp/ledger-after.json" '.data.ledger | length' 'len(json.load(sys.stdin).get("data",{}).get("ledger",[]))')
[ "$after" = "1" ] || fail "the accepted send must add exactly one ledger row" "count=$after ledger=$(cat "$tmp/ledger-after.json")"
echo "PASS acl-reject"
