#!/usr/bin/env bash
set -euo pipefail
# Headless (exec) session backend: one server, one client, fake agent as
# session_command. Workspace config carries `backend = "headless"` (the parse
# alias); lib.sh pins ONLYNE_BACKEND=fake, so this case covers that export
# with `exec` after the source. Projections still name the backend `exec`.
SRC=$(pwd)
tmp=$(mktemp -d)
server_pid=""
client_pid=""
cleanup() {
  local status=$?
  drain_pid "$client_pid" 2>/dev/null || true
  drain_pid "$server_pid" 2>/dev/null || true
  if [ "$status" -eq 0 ] && [ "${E2E_KEEP:-0}" != 1 ]; then
    rm -rf "$tmp"
  else
    echo "exec-headless: scratch directory kept at $tmp" >&2
  fi
  exit "$status"
}
trap cleanup EXIT
. "$SRC/crates/onlyne-testkit/e2e/lib.sh"

# lib.sh pins ONLYNE_BACKEND=fake. This case talks to the exec backend, so it
# covers that export after the source.
export ONLYNE_BACKEND=exec

case "$BIN_DIR" in
  /*) FAKE_CANDIDATE="$BIN_DIR/onlyne-agent-fake" ;;
  *) FAKE_CANDIDATE="$SRC/$BIN_DIR/onlyne-agent-fake" ;;
esac
if [ ! -x "$FAKE_CANDIDATE" ]; then
  echo "SKIP exec-headless: missing onlyne-agent-fake"
  exit 0
fi

SERVER=$(bin onlyne-server)
CLIENT=$(bin onlyne-client)
ONLYNE=$(bin onlyne)
FAKE=$FAKE_CANDIDATE
SCRIPT="$SRC/crates/onlyne-testkit/scripts/echo-complete.json"
ws="$tmp/planner"

"$SERVER" init --root "$tmp/server" --listen "127.0.0.1:$(free_port)"
"$SERVER" run --root "$tmp/server" >"$tmp/server.log" 2>&1 &
server_pid=$!
for _ in $(seq 1 100); do
  "$ONLYNE" --server-root "$tmp/server" wait-ready >/dev/null 2>&1 && break
  sleep 0.1
done

# The fake agent speaks the adapter socket and writes nothing to stdio, so a
# one-line banner then `exec` is what makes session-<task>.log observably
# nonempty while still running ONLYNE_BIN_DIR/onlyne-agent-fake as the agent.
run_agent="$tmp/run-agent.sh"
printf '#!/bin/sh\nprintf '"'"'exec-headless\n'"'"'\nexec "%s" --workspace "%s" --script "%s" --once\n' \
  "$FAKE" "$ws" "$SCRIPT" > "$run_agent"
chmod +x "$run_agent"
SESSION_COMMAND='["'"$run_agent"'"]'
client_init "$ws" planner "$tmp/server" "$tmp/server/.onlyne/spec.toml" \
  "$tmp/spec.frag.toml" "$E2E_PROSE" 'allowed_senders = ["*", "planner"]
allowed_targets = ["planner"]
session_command = '"$SESSION_COMMAND"
"$ONLYNE" --server-root "$tmp/server" reload

# Config field + alias: the workspace document names `headless`; env `exec`
# still wins when set. Inserted above `[server]` so the key stays top-level
# (a trailing append would land inside that table).
config="$ws/.onlyne/config.toml"
{
  printf 'backend = "headless"\n'
  cat "$config"
} > "$config.new"
mv "$config.new" "$config"

"$CLIENT" run --workspace "$ws" >"$tmp/client.log" 2>&1 &
client_pid=$!

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
[ "$registered" = "true" ] || fail "planner must register before the send" \
  "roles=$(cat "$tmp/roles.json" 2>/dev/null) client=$(cat "$tmp/client.log" 2>/dev/null)"

send_out=$("$ONLYNE" --server-root "$tmp/server" send --from planner --to planner --text "hello v1") \
  || fail "send command failed" "$send_out"
printf '%s\n' "$send_out" > "$tmp/send.json"
task=$(json_field "$tmp/send.json" '.data.task' 'json.load(sys.stdin)["data"]["task"]')

ledger_out=""
for _ in $(seq 1 120); do
  ledger_out=$("$ONLYNE" --server-root "$tmp/server" ledger --task "$task" 2>/dev/null) || true
  printf '%s\n' "$ledger_out" > "$tmp/ledger.json"
  if rows_any "$tmp/ledger.json" state acked 2>/dev/null; then
    break
  fi
  sleep 0.5
done
rows_any "$tmp/ledger.json" state acked || fail "ledger state must become acked" \
  "ledger=$ledger_out client=$(cat "$tmp/client.log" 2>/dev/null)"

sessions_out=""
for _ in $(seq 1 120); do
  sessions_out=$("$ONLYNE" --server-root "$tmp/server" sessions --task "$task" 2>/dev/null) || true
  printf '%s\n' "$sessions_out" > "$tmp/sessions.json"
  if rows_any "$tmp/sessions.json" public_lifecycle exited 2>/dev/null; then
    break
  fi
  sleep 0.5
done
rows_any "$tmp/sessions.json" public_lifecycle exited || fail "sessions public_lifecycle must be exited" \
  "sessions=$sessions_out client=$(cat "$tmp/client.log" 2>/dev/null)"
[ "$(row_value "$tmp/sessions.json" outcome)" = "done" ] || fail "sessions outcome must be done" "$sessions_out"

session_log="$ws/.onlyne/logs/session-$task.log"
[ -s "$session_log" ] || fail "the exec backend must capture the child's stdio at $session_log" \
  "logs=$(find "$ws/.onlyne/logs" -type f 2>/dev/null) client=$(cat "$tmp/client.log" 2>/dev/null)"
grep -q "exec-headless" "$session_log" || fail "session log must contain the child banner" \
  "$(cat "$session_log" 2>/dev/null)"

db="$ws/.onlyne/client.db"
backend=$(db_count "$db" "SELECT backend FROM sessions WHERE task_id='$task'")
[ "$backend" = "exec" ] || fail "client.db sessions.backend must be exec (not headless)" \
  "backend=$backend ref=$(db_count "$db" "SELECT backend_ref FROM sessions WHERE task_id='$task'")"

ref=$(db_count "$db" "SELECT backend_ref FROM sessions WHERE task_id='$task'")
printf '%s\n' "$ref" | python3 -c '
import json, sys
raw = sys.stdin.read()
ref = json.loads(raw)
assert ref.get("backend") == "exec", raw
inner = ref.get("backend_ref") or {}
assert inner.get("pid") is not None, raw
assert inner.get("log"), raw
'
[ -s "$ws/prose.log" ] || fail "fake agent must dump received assign.prose to prose.log" \
  "$(cat "$tmp/client.log" 2>/dev/null)"
[ "$(cat "$ws/prose.log")" = "$E2E_PROSE" ] || fail "prose.log must equal the spec prose" \
  "$(cat "$ws/prose.log")"

echo "PASS exec-headless"
