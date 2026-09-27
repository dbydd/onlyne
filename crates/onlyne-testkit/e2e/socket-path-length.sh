#!/usr/bin/env bash
set -euo pipefail
# Verification case 17, rewritten for the v2 socket move. The rule it used to
# pin is gone: v1 bound `<workspace>/.onlyne/run/s` while that spelling fit
# `sun_path` and moved a deeper tree to a short derived path recorded in
# `run/socket`, and a generated role nests deep enough to pass the bound. v2
# binds every root at `<runtime_dir>/<digest>.sock` whatever its length, so the
# length of the workspace is no longer an input to where the socket lives.
#
# What is left to pin is the invariant that replaced it, and it is a real one:
# a workspace whose canonical spelling is far past the old bound still serves one
# short runtime path, its `<digest>.json` registration names that same path and
# the same root, and nothing at all is created under `<workspace>/.onlyne/run/`.
# The case keeps the deep workspace and the end-to-end task, because a deep root
# is exactly the input that used to split one tree across two directories.
#
# No real platform credential or window manager is touched: ONLYNE_BACKEND=fake plus the fake gateway only.
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

# The padding is ASCII, so a character count equals the byte count the old unix
# bound was stated in. The loop stops once the workspace path alone is long
# enough that its v1 canonical spelling would have been over that bound.
ws="$tmp/planner"
n=0
while [ "${#ws}" -lt 116 ] && [ "$n" -lt 40 ]; do
  ws="$ws/aaaaaaaa"
  n=$((n + 1))
done
natural="$ws/.onlyne/run/s"
[ "${#natural}" -gt 103 ] \
  || fail "this case needs a workspace past the old 103-byte bound" "$natural (${#natural} bytes)"

setup_cluster "$tmp/server" "$ws" planner cluster "" "$E2E_PROSE" 'allowed_senders = ["*", "planner"]
allowed_targets = ["planner"]'
server_pid=$cluster_server_pid

"$CLIENT" run --workspace "$ws" >"$tmp/client.log" 2>&1 &
client_pid=$!
"$FAKE" --workspace "$ws" --script "$SRC/crates/onlyne-testkit/scripts/echo-complete.json" >"$tmp/fake.log" 2>&1 &
fake_pid=$!

# The served path is the one the client bound, derived from this root alone, so
# the same helper every other case calls answers it without a marker to read.
bound=$(wait_for_socket "$ws" 100) \
  || fail "the client must bind a runtime socket for this workspace" "$(cat "$tmp/client.log" 2>/dev/null)"
[ "${#bound}" -le 103 ] || fail "a runtime path must fit the unix bound" "$bound (${#bound} bytes)"
case "$bound" in
  "$(runtime_dir)"/*.sock) ;;
  *) fail "the served path must sit in the runtime directory" "$bound" ;;
esac
grep -q -F "$bound" "$tmp/client.log" \
  || fail "the client log must name the served path" "$(cat "$tmp/client.log" 2>/dev/null)"

# The registration beside it names the same root and the same socket, so a reader
# needs no tree walk to learn who serves what.
registration=$(runtime_registration "$ws")
[ -f "$registration" ] \
  || fail "the client must publish a registration at $registration" "$(cat "$tmp/client.log" 2>/dev/null)"
reg_root=$(json_field "$registration" '.root' 'json.load(sys.stdin).get("root","")')
reg_runtime=$(json_field "$registration" '.runtime' 'json.load(sys.stdin).get("runtime","")')
reg_kind=$(json_field "$registration" '.kind' 'json.load(sys.stdin).get("kind","")')
reg_role=$(json_field "$registration" '.role' 'json.load(sys.stdin).get("role","")')
[ "$(basename "$registration")" = "$(workspace_digest "$ws").json" ] \
  || fail "the registration must be named for this root's digest" "$registration"
[ "$reg_root" = "$(cd "$ws" && pwd -P)" ] \
  || fail "the registration must name the canonical workspace root" "root=$reg_root expected=$(cd "$ws" && pwd -P)"
[ "$reg_kind" = "client" ] || fail "a role workspace must register as a client" "kind=$reg_kind"
[ "$reg_role" = "planner" ] || fail "the registration must name the serving role" "role=$reg_role"
[ "$reg_runtime" = "fake" ] || fail "the registration must name the session runtime" "runtime=$reg_runtime"

# Nothing binds in the tree any more: the canonical spelling and the v1 marker
# are both absent, and `run/` holds no socket at all.
[ ! -e "$natural" ] || fail "nothing may be created at the canonical spelling" "$natural"
[ ! -e "$ws/.onlyne/run/socket" ] || fail "the v1 marker must not be published" "$ws/.onlyne/run/socket"
[ ! -S "$ws/.onlyne/run/s" ] || fail "no socket may live under the workspace run directory" "$ws/.onlyne/run/s"

# `who` names the workspace and never the path, so this answer is the resolver
# doing its job.
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

# One task end to end over the runtime socket: the fake agent resolves the same
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

send_out=$("$ONLYNE" --server-root "$tmp/server" send "${SUPERVISOR_FLAGS[@]}" --from planner --to planner --text "hello v1") || fail "send command failed" "$send_out"
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
