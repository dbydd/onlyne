#!/bin/bash
# Smoke: `control --task` routes from the ledger when no session row exists yet.
#
# The window is real and short — a delivery the server has accepted but no client
# has picked up. Before the ledger fallback the verb refused and told the
# operator to pass --to, naming a role the server's own record already held.
#
# No agent is mounted here, so no session row can ever appear for this task: the
# ledger row is the only record that exists, which is the case in point.
set -u
cd "$(dirname "$0")/../../.." || exit 1
SRC=$(pwd)
tmp=$(mktemp -d)
pids=""
cleanup() {
  for p in $pids; do kill -TERM "$p" 2>/dev/null; done
  sleep 0.5
  for p in $pids; do kill -KILL "$p" 2>/dev/null; done
  rm -rf "$tmp"
}
trap cleanup EXIT

export ONLYNE_BACKEND=fake
ONLYNE="$SRC/target/debug/onlyne"
SERVER="$SRC/target/debug/onlyne-server"
CLIENT="$SRC/target/debug/onlyne-client"
for b in "$ONLYNE" "$SERVER" "$CLIENT"; do
  [ -x "$b" ] || { echo "SKIP: missing $b"; exit 0; }
done

port=$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')
"$SERVER" init --root "$tmp/server" --listen "127.0.0.1:$port" >/dev/null
ws="$tmp/planner"
mkdir -p "$ws"
# The seed entry already allows planner to reach planner, which is what the
# quickstart uses, so nothing here has to restate the ACL.
# init prints the role's `[[client]]` fragment; it reaches the cluster only when
# that fragment is appended to the spec, which is what lib.sh's `client_init` does.
"$CLIENT" init --workspace "$ws" --role planner --server-root "$tmp/server" \
  --prose "You are the planner." >"$tmp/frag.toml"
cat "$tmp/frag.toml" >> "$tmp/server/.onlyne/spec.toml"

"$SERVER" run --root "$tmp/server" >"$tmp/server.log" 2>&1 &
pids="$pids $!"
sleep 1
# The role lands in the spec when init writes its fragment; the server reads it on
# the next reload, and a send before that is refused as an unknown sender.
"$ONLYNE" --server-root "$tmp/server" reload >/dev/null
"$CLIENT" run --workspace "$ws" >"$tmp/client.log" 2>&1 &
pids="$pids $!"
sleep 1.5

receipt=$("$ONLYNE" --server-root "$tmp/server" send --from planner --to planner \
  --text "smoke" --force --yes-i-am-supervisor-not-other-role 2>/dev/null)
task=$(printf '%s' "$receipt" | python3 -c 'import json,sys; print(json.load(sys.stdin)["data"]["task"])' 2>/dev/null)
[ -n "$task" ] || { echo "FAIL: no task id in: $receipt"; exit 1; }

# `cancel` is the op that needs no live session to act, so the op reaching the
# server at all is what the routing bought.
out=$("$ONLYNE" --server-root "$tmp/server" control --task "$task" cancel --reason "smoke" --from planner \
  --force --yes-i-am-supervisor-not-other-role 2>&1)
# Both refusals count: the one this case was written against, and the wording
# before it, which is what a build without the fallback answers with. A guard
# that only knows the new sentence reports a pass for the old defect.
case "$out" in
  *"name an owner"*|*"no session owns task"*)
    echo "FAIL: still refuses when the ledger names one"; echo "$out"; exit 1 ;;
  *"unrecognized subcommand"*)
    echo "FAIL: never reached the server"; echo "$out"; exit 1 ;;
  *)
    echo "PASS control routed from the ledger: $out" ;;
esac
