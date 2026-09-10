#!/usr/bin/env bash
set -euo pipefail
SRC=$(pwd)
tmp=$(mktemp -d)
cleanup() { rm -rf "$tmp"; }
trap cleanup EXIT

bin() {
  local name=$1
  local path="$SRC/target/debug/$name"
  if [[ ! -x "$path" ]]; then
    echo "onlyne: missing binary $path; run cargo build --workspace" >&2
    exit 127
  fi
  printf '%s\n' "$path"
}
SERVER=$(bin onlyne-server)
CLIENT=$(bin onlyne-client)
ONLYNE=$(bin onlyne)
FAKE=$(bin onlyne-agent-fake)

workspace="$tmp/workspace"
mkdir -p "$workspace/.onlyne/run"
"$SERVER" --workspace "$workspace" >"$tmp/server.log" 2>&1 &
server_pid=$!
for _ in $(seq 1 100); do
  if [[ -S "$workspace/.onlyne/run/s" ]]; then
    break
  fi
  sleep 0.1
done
if [[ ! -S "$workspace/.onlyne/run/s" ]]; then
  echo "onlyne: server did not become ready within 10 seconds" >&2
  exit 1
fi
"$CLIENT" --workspace "$workspace" --role planner >"$tmp/client.log" 2>&1 &
client_pid=$!
"$FAKE" --workspace "$workspace" --role planner --script "$SRC/crates/onlyne-testkit/scripts/echo-complete.json" --once
"$ONLYNE" --workspace "$workspace" status
kill "$client_pid" "$server_pid" 2>/dev/null || true
wait "$client_pid" "$server_pid" 2>/dev/null || true
