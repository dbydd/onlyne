#!/usr/bin/env bash
set -euo pipefail
# Field case (2026-09-17): a generated role workspace three levels under a 55-byte
# checkout root spells `<ws>/.onlyne/run/s` at 104-109 bytes. macOS `sun_path`
# holds 104 bytes including its NUL, so those clients could not bind the adapter
# socket, retried every 0.5s, and reported `connect EINVAL` to the plugin while
# the server still counted the role connected.
#
# This case builds a workspace whose canonical socket path is past that bound and
# proves the served path is short, published, discoverable from the workspace
# alone, and passes one task end to end.
SRC=$(pwd)
tmp=$(mktemp -d)
server_pid=""
client_pid=""
fake_pid=""
cleanup() {
  kill "$server_pid" "$client_pid" "$fake_pid" "${cluster_server_pid:-}" 2>/dev/null || true
  wait "$server_pid" "$client_pid" "$fake_pid" "${cluster_server_pid:-}" 2>/dev/null || true
  rm -rf "$tmp"
}
trap cleanup EXIT
. "$SRC/crates/onlyne-testkit/e2e/lib.sh"

# The padding is ASCII, so a character count equals the byte count the unix bound
# is stated in. The loop stops once the workspace path alone is long enough to put
# its canonical socket path past 103 bytes.
ws="$tmp/planner"
n=0
while [ "${#ws}" -lt 116 ] && [ "$n" -lt 40 ]; do
  ws="$ws/aaaaaaaa"
  n=$((n + 1))
done
natural="$ws/.onlyne/run/s"
[ "${#natural}" -gt 103 ] || fail "this case needs a canonical socket path past 103 bytes" "$natural (${#natural} bytes)"

setup_cluster "$tmp/server" "$ws" planner cluster "" "$E2E_PROSE" 'allowed_senders = ["*", "planner"]
allowed_targets = ["planner"]'
server_pid=$cluster_server_pid

"$CLIENT" run --workspace "$ws" >"$tmp/client.log" 2>&1 &
client_pid=$!
"$FAKE" --workspace "$ws" --script "$SRC/crates/onlyne-testkit/scripts/echo-complete.json" >"$tmp/fake.log" 2>&1 &
fake_pid=$!

# The client publishes the path it bound in `<run>/socket`, which is how a caller
# that spells the long workspace reaches a socket it cannot name directly.
marker="$ws/.onlyne/run/socket"
for _ in $(seq 1 100); do
  if [ -s "$marker" ]; then break; fi
  sleep 0.1
done
[ -s "$marker" ] || fail "the client must publish the bound socket path in $marker" "$(cat "$tmp/client.log" 2>/dev/null)"
bound=$(tr -d '[:space:]' <"$marker")
[ -n "$bound" ] || fail "the published path must be non-empty" "$(cat "$marker" 2>/dev/null)"
[ "${#bound}" -le 103 ] || fail "the served path must fit the unix bound" "$bound (${#bound} bytes)"
[ "$bound" != "$natural" ] || fail "a canonical path past 103 bytes must bind the short path" "$bound"
[ -S "$bound" ] || fail "the published path must hold a bound socket" "$bound"
[ ! -S "$natural" ] || fail "the canonical path must hold no socket" "$natural"
grep -q -F "$bound" "$tmp/client.log" || fail "the client log must name the served path" "$(cat "$tmp/client.log" 2>/dev/null)"

# `who` names the workspace and never the short path, so this answer is the
# marker doing its job.
who_out=""
who_name=""
for _ in $(seq 1 100); do
  who_out=$("$ONLYNE" --workspace "$ws" who 2>/dev/null) || true
  printf '%s\n' "$who_out" > "$tmp/who.json"
  who_name=$(json_field "$tmp/who.json" '.data.roles[0].name' 'json.load(sys.stdin)["data"]["roles"][0]["name"]' 2>/dev/null || true)
  if [ "$who_name" = "planner" ]; then break; fi
  sleep 0.1
done
[ "$who_name" = "planner" ] || fail "who must reach the client through the workspace" "$who_out $(cat "$tmp/client.log" 2>/dev/null)"

# One task end to end over the short socket: the fake agent resolves the same
# path from the workspace, so a task that settles proves every finder agrees.
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

ledger_out=""
for _ in $(seq 1 120); do
  ledger_out=$("$ONLYNE" --server-root "$tmp/server" ledger --task "$task" 2>/dev/null) || true
  printf '%s\n' "$ledger_out" > "$tmp/ledger.json"
  if rows_any "$tmp/ledger.json" state acked 2>/dev/null; then break; fi
  sleep 0.5
done
rows_any "$tmp/ledger.json" state acked || fail "ledger state must become acked" "ledger=$ledger_out fake=$(cat "$tmp/fake.log" 2>/dev/null)"

sessions_out=""
for _ in $(seq 1 120); do
  sessions_out=$("$ONLYNE" --server-root "$tmp/server" sessions --task "$task" 2>/dev/null) || true
  printf '%s\n' "$sessions_out" > "$tmp/sessions.json"
  if rows_any "$tmp/sessions.json" public_lifecycle exited 2>/dev/null; then break; fi
  sleep 0.5
done
rows_any "$tmp/sessions.json" public_lifecycle exited || fail "sessions public_lifecycle must be exited" "sessions=$sessions_out fake=$(cat "$tmp/fake.log" 2>/dev/null)"
[ "$(row_value "$tmp/sessions.json" outcome)" = "done" ] || fail "sessions outcome must be done" "$sessions_out"
[ "$(cat "$ws/prose.log" 2>/dev/null)" = "$E2E_PROSE" ] || fail "the fake agent must report the spec prose from the deep workspace" "$(cat "$tmp/fake.log" 2>/dev/null)"
echo "PASS socket-path-length"
