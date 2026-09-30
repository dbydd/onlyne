#!/usr/bin/env bash
set -euo pipefail
# Verification case 11: the pi agent plugin against a real role client.
#
# The plugin lives outside this repository's Rust tree (plugins/onlyne-agent-pi)
# and speaks the adapter protocol from `crates/onlyne-adapter/PROTOCOL.md`. This
# case is the one end-to-end proof that a real pi process, spawned as a role
# session by `onlyne-client`, reaches `acked` through its own completion exit:
# the role's `[[client]].runtime` command points pi at the plugin directory, pi
# runs one turn, and the ledger, the session projection and the injected
# delivery text are all asserted from the supervisor side.
#
# Skip discipline (same as orca-live.sh): this case needs a real model call, so
# it prints SKIP and exits 0 unless pi is on PATH *and* answers a credential
# check. A host without credentials must not read as a product failure; a host
# with pi but no working model must not read as a pass either, which is why the
# probe is a real one-turn round trip rather than an env-var sniff.
SRC=$(pwd)
tmp=$(mktemp -d)
server_pid=""
client_pid=""
# A live case is diagnosed from its artifacts: a passing run throws the scratch
# directory away, a failing one keeps it and says where it is. `PI_LIVE_KEEP=1`
# forces it either way.
cleanup() {
  status=$?
  kill "$server_pid" "$client_pid" "${client_pid:-}" 2>/dev/null || true
  wait "$server_pid" "$client_pid" 2>/dev/null || true
  if [ "$status" -eq 0 ] && [ "${PI_LIVE_KEEP:-0}" != 1 ]; then
    rm -rf "$tmp"
  else
    echo "pi-live: scratch directory kept at $tmp" >&2
  fi
}
trap cleanup EXIT
. "$SRC/crates/onlyne-testkit/e2e/lib.sh"

# lib.sh pins the fake backend, which models sessions in memory and spawns
# nothing; this case is about a real agent process, so it switches to the
# headless `exec` backend. `exec` runs the command as a child of the client with
# stdin held open (the client owns the pipe for as long as the session lives)
# and appends the child's stdout and stderr to `.onlyne/logs/`.
export ONLYNE_BACKEND=exec

PLUGIN_DIR="$SRC/plugins/onlyne-agent-pi"

if ! command -v pi >/dev/null 2>&1; then
  echo "SKIP pi-live: pi is not on PATH"
  exit 0
fi

# The credential probe: one real turn in a throwaway directory with no session
# file, no context files and no extensions. It doubles as the model-readiness
# check, and it is the only probe that cannot go green on a broken credential.
probe_dir="$tmp/probe"
mkdir -p "$probe_dir"
#
# The model is named, not inherited: the acceptance criterion names one, and a
# case that silently ran on whatever pi's own default happened to be would prove
# only that some model answered. `axonhub/generic-writer` is the first entry in
# the `axonhub` provider's model list that `pi --list-models` reads out of
# `~/.pi/agent/autoprovider.json`, so the case names the id it means.
PI_MODEL=${PI_LIVE_MODEL:-axonhub/generic-writer}
probe_ok=false
if (cd "$probe_dir" && timeout 180 pi -ns -nc --no-session --model "$PI_MODEL" -p 'reply with exactly: PI_ONLYNE_PROBE_OK' >"$tmp/probe.out" 2>"$tmp/probe.err"); then
  grep -q "PI_ONLYNE_PROBE_OK" "$tmp/probe.out" && probe_ok=true
fi
# The probe output goes to a file rather than a pipe: a `grep -q` that matches
# early closes the pipe under a still-writing pi, which surfaces as EPIPE noise
# instead of the answer being read.
if [ "$probe_ok" != true ]; then
  echo "SKIP pi-live: pi has no usable model credentials (see below)"
  sed -n '1,5p' "$tmp/probe.err" 2>/dev/null || true
  exit 0
fi

# The drive and its argv live in the role's `[[client]].runtime` table (the
# server sends the slice in `welcome`, and the client spawns the command per
# task with `{session}`/`{task}` substituted), so they travel in the
# `client_init` fragment the helper appends.
#
# Two details are load-bearing:
#   * RPC mode needs a stdin that never closes: pi reads its next command from
#     stdin and treats EOF as "the operator left". That is exactly what the
#     `exec` backend provides — it spawns the command with a pipe the client
#     holds open — so the command is a plain argv list with no shell and no
#     `tail -f /dev/null |` keep-alive wrapper. `-p` would instead run one
#     prompt and exit before the plugin could inject anything.
#   * `--session-dir` keeps a session file the case can grep, which is how the
#     injected `assign` is proven to have reached pi's context.
mkdir -p "$tmp/planner/.pi/sessions"
SESSION_COMMAND='["pi", "--mode", "rpc", "--session-id", "{session}", "--session-dir", "'"$tmp"'/planner/.pi/sessions", "-e", "'"$PLUGIN_DIR"'", "--model", "'"$PI_MODEL"'", "-ns", "-nc"]'

setup_cluster "$tmp/server" "$tmp/planner" planner cluster "" "$E2E_PROSE" 'allowed_senders = ["*", "planner"]
allowed_targets = ["planner"]
[client.runtime]
drive = "plugin"
command = '"$SESSION_COMMAND"
server_pid=$cluster_server_pid
ws="$tmp/planner"

# The plugin's own switch file. The generated templates carry it; this workspace
# comes from `client_init`, so the case writes the same two keys itself.
mkdir -p "$ws/.pi"
printf '{"enabled":true,"watch":{"autoStart":true}}\n' > "$ws/.pi/onlyne.json"

# The case stands in for Orca here: a real pane exports the ids of the pane it
# started the command in (measured 2026-09-11 on Orca 1.4.198), the client
# passes its environment to the session command, and the plugin reads them from
# there. Step 2b asserts the pane that comes back out of the session axis.
# `hostBinding` prefers ambient `ORCA_TAB_ID` and `ORCA_LEAF_ID` over the two
# halves of the pinned pane key, so an interactive Orca shell would otherwise
# win with its own ids. The terminal handle below remains the pinned value.
unset ORCA_TAB_ID ORCA_LEAF_ID
ORCA_PANE_KEY="45e603f7-0772-48aa-bcf6-832272747713:b6d067b6-9255-4f5c-a13f-24f194ea0560"
ORCA_TERMINAL_HANDLE="term_e2e_pi_live"
export ORCA_PANE_KEY ORCA_TERMINAL_HANDLE

"$CLIENT" run --workspace "$ws" >"$tmp/client.log" 2>&1 &
client_pid=$!

online=false
for _ in $(seq 1 150); do
  roles_out=$("$ONLYNE" --server-root "$tmp/server" roles 2>/dev/null) || true
  printf '%s\n' "$roles_out" > "$tmp/roles.json"
  if rows_any "$tmp/roles.json" state online 2>/dev/null; then
    online=true
    break
  fi
  sleep 0.1
done
[ "$online" = true ] || fail "planner must register on the server" "client=$(cat "$tmp/client.log" 2>/dev/null)"

# The task text is the operator's own words, and it names the obligation the
# case asserts. Under v1 the delivery and the plugin's reminder ladder pushed
# the model toward the completion; v2's delivery is deliberately neutral and
# the tool descriptions state effect and precondition only (§3a, §3b), so a
# fixture that left the completion to the model's inclination would assert
# something it never asked for. Naming a tool is not protocol vocabulary: the
# body is an operator's sentence, and the case's claim is that the client's
# rendered delivery reaches the model byte-exact with that sentence inside it.
TASK_TEXT='reply with exactly: OK, then report this task finished with onlyne_complete and summary OK'
send_out=$("$ONLYNE" --server-root "$tmp/server" send "${SUPERVISOR_FLAGS[@]}" --from planner --to planner --text "$TASK_TEXT") \
  || fail "send command failed" "$send_out"
printf '%s\n' "$send_out" > "$tmp/send.json"
task=$(json_field "$tmp/send.json" '.data.task' 'json.load(sys.stdin)["data"]["task"]')
[ "$(json_field "$tmp/send.json" '.data.state' 'json.load(sys.stdin)["data"]["state"]')" = "in_flight" ] \
  || fail "send data.state must be in_flight" "$send_out"

# 1. The ledger reaches `acked` and its `out_head` carries the model's own
#    summary, which the task text asked it for beside the completion.
#
#    The row read is the completion the plugin's report wrote, selected by its
#    own `kind` rather than by whichever row happens to be listed first: the
#    assign row is acked as soon as the session mounts, so a first-acked-row
#    read would print the operator's body preview back and assert nothing about
#    the model.
ledger_out=""
out_head=""
for _ in $(seq 1 240); do
  ledger_out=$("$ONLYNE" --server-root "$tmp/server" ledger --task "$task" 2>/dev/null) || true
  printf '%s\n' "$ledger_out" > "$tmp/ledger.json"
  out_head=$(data_rows "$tmp/ledger.json" 2>/dev/null | python3 -c '
import json,sys
rows=[json.loads(line) for line in sys.stdin if line.strip()]
settled=[row for row in rows if row.get("kind")=="completion" and row.get("state")=="acked"]
print(settled[0].get("out_head","") if settled else "")
' 2>/dev/null || true)
  if [ -n "$out_head" ]; then
    break
  fi
  sleep 0.5
done
[ -n "$out_head" ] || fail "the ledger must carry the plugin's acked completion row" \
  "ledger=$ledger_out client=$(cat "$tmp/client.log" 2>/dev/null)"
case "$out_head" in
  *OK*) ;;
  *) fail "ledger out_head must contain the model's OK" "out_head=$out_head ledger=$ledger_out" ;;
esac
printf 'PASS pi-live ledger: acked on %s, out_head=%s\n' "$PI_MODEL" "$out_head"

# 1b. The agent ran as a child of the client, not as something the case started
#     itself: the exec backend appends the child's stdio to the workspace log,
#     and pi's RPC banner is what lands there.
session_log="$ws/.onlyne/logs/session-$task.log"
[ -s "$session_log" ] || fail "the exec backend must capture pi's stdio at $session_log" \
  "logs=$(find "$ws/.onlyne/logs" -type f 2>/dev/null | head -n 5) client=$(cat "$tmp/client.log" 2>/dev/null)"
printf 'PASS pi-live child stdio: %s (%s bytes)\n' "$session_log" "$(wc -c <"$session_log" | tr -d ' ')"

# 2. The session projection ends `exited` with outcome `done`, which is the
#    plugin's completion report, not a supervisor guess.
sessions_out=""
for _ in $(seq 1 240); do
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
printf 'PASS pi-live session projection: exited/done\n'

# 2b. The session row states where the process ran. The pane travels from the
#     environment the process inherited, through the heartbeat, to the session
#     axis a supervisor reads — no file in the workspace is consulted, which is
#     what lets the board scope its tab list to real panes.
pane=$(json_field "$tmp/sessions.json" '.data.sessions[0].projection.observed.host.orca.pane_key' \
  'json.load(sys.stdin)["data"]["sessions"][0]["projection"]["observed"]["host"]["orca"]["pane_key"]' 2>/dev/null) \
  || fail "the session row must carry the pane its process reported" "$sessions_out"
[ "$pane" = "$ORCA_PANE_KEY" ] || fail "the reported pane must be the inherited ORCA_PANE_KEY (got '$pane')" "$sessions_out"
printf 'PASS pi-live host binding: %s\n' "$pane"

# 3. The assign really reached pi's context, and the plugin's own completion
#    entry was recorded. pi's session file records both, so the two claims are
#    read back from the file the plugin was told to use: the injected message is
#    the delivery text the client rendered — the source line and the task text,
#    with no task id, hop, or budget in it — the plugin's own entry names the
#    task it injected, and the completion entry names the outcome the ledger
#    already shows.
session_file=""
for _ in $(seq 1 100); do
  session_file=$(find "$ws/.pi/sessions" -name "*${task}*" -type f 2>/dev/null | head -n 1)
  [ -n "$session_file" ] && break
  sleep 0.1
done
[ -n "$session_file" ] || fail "pi must have written a session file for $task" \
  "sessions=$(find "$ws" -name '*.jsonl' 2>/dev/null | head -n 5) client=$(cat "$tmp/client.log" 2>/dev/null)"
grep -q "From planner:" "$session_file" || fail "the rendered delivery text must reach pi's context" "$session_file"
grep -q -F -- "$TASK_TEXT" "$session_file" || fail "the injected task text must reach pi's context" "$session_file"
grep -q "onlyne-assign" "$session_file" || fail "the plugin must record the assign it injected" "$session_file"
grep -q "$task" "$session_file" || fail "the assigned task id must reach pi's session file" "$session_file"
grep -q "onlyne-complete" "$session_file" || fail "the plugin must record its completion entry" "$session_file"
# The role prose arrives with `welcome`, as context rather than as a turn, and
# the plugin records it as its own entry. Both halves are asserted: the entry
# type proves the welcome-time channel was used, and the spec's own text
# proves what reached pi's context. A plugin that missed the welcome and
# folded the prose into the assignment fails both.
grep -q "onlyne-role-prose" "$session_file" || fail "the role prose must arrive as context at welcome time" "$session_file"
grep -q "$E2E_PROSE" "$session_file" || fail "the spec's prose text must reach pi's context" "$session_file"
printf 'PASS pi-live injection: %s carries the assign, the prose and the completion entry\n' "$session_file"

# 4. Detach on the way out: SIGTERM stops the client, and the client must leave
#    with exit 0 rather than hanging on the session it owns.
kill -TERM "$client_pid"
for _ in $(seq 1 100); do
  kill -0 "$client_pid" 2>/dev/null || break
  sleep 0.1
done
if kill -0 "$client_pid" 2>/dev/null; then
  fail "the client must leave after SIGTERM" "client=$(cat "$tmp/client.log" 2>/dev/null)"
fi
wait "$client_pid" 2>/dev/null || true
client_pid=""
grep -q "SIGTERM" "$tmp/client.log" || fail "the client must log its SIGTERM drain" "$(cat "$tmp/client.log" 2>/dev/null)"
printf 'PASS pi-live detach: client drained on SIGTERM\n'

echo "PASS pi-live"
