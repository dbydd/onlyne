#!/usr/bin/env bash
set -euo pipefail
# Verification case: the Orca session backend against the real Orca app.
#
# This is the only case in the suite that talks to a real window manager. It
# spawns the same real mounting agent every other case here uses,
# `onlyne-agent-fake`, in a tab Orca really opens, and proves four things
# against the live app: the tab lands flat in the worktree the supervisor's own
# tab runs in (the `host` worktree policy reading ORCA_WORKTREE_ID), the
# four-part liveness probe inputs, the tab map side-channel carrying that same
# identity, and that SIGTERM makes the client close the tab it opened — all
# without registering anything in Orca, which the case asserts at the end.
#
# The trap this case is built around, named here so it cannot be reintroduced by
# accident: a `sleep` in the tab is not a session. Nothing mounts, so the client
# writes no session row, and every downstream read of that row answers "no
# session owns task X" while the server's own ledger row already records who it
# went to — `owning_role` in `crates/onlyne-cli/src/verbs.rs` refusing an
# operator who passed `--to` correctly is the shape it takes. So the runtime
# command below is a process that mounts, and the case asserts the session row
# that mount produced.
#
# The agent holds its assignment open instead of completing it, which is what
# keeps the task in_flight by design: the drain assertions need a tab that is
# still there to be closed, and a completion would close it before SIGTERM ever
# arrived. The delivery is still driven all the way in — the assign text the
# plugin rendered inside the tab is read back off disk and asserted byte-exact.
#
# Safety rules this case holds to:
#   * it runs only inside an Orca tab. Without ORCA_WORKTREE_ID in the
#     environment it prints `SKIP` and exits 0: that variable is both the policy
#     input and the tab list every assertion reads, so a shell outside Orca has
#     nothing to verify against;
#   * every path it touches lives under one `mktemp -d` tree;
#   * it never passes `--focus` and never runs `terminal switch`;
#   * the host worktree is the operator's own, so the case records the tab count
#     it started with and only ever touches the handle it read out of its own tab
#     map — nothing is matched by name, and every other tab is left alone;
#   * without a reachable Orca app it prints `SKIP` and exits 0, so a CI host
#     without Orca does not read as a product failure.
SRC=$(pwd)
tmp=$(mktemp -d)
server_pid=""
client_pid=""
ws=""
canon_ws=""
host_worktree=""
pane_key=""
handle=""
task=""
mapping=""
host_tabs_before=""

cleanup() {
  local status=$? residue agents
  # The client's own SIGTERM drain is what closes the tabs it opened, so it gets
  # a bounded window to leave before the case reports.
  drain_pid "$client_pid"
  # A tab the drain did not reap still belongs to this case, and its handle is
  # the one this script read out of its own tab map. A close that does not take
  # is reported and fails the run: a leftover tab is real residue, and a
  # swallowed verdict is how one piles up unnoticed.
  #
  # The identity is recovered from the map here rather than reused from the
  # running case, because the tab exists from the moment the client spawns it —
  # a failure between there and the case reading its own map line would
  # otherwise leak the tab on exactly the path that has no handle to close it by.
  if [ -z "$handle" ] && [ -f "$mapping" ]; then
    handle=$(tail -n 1 "$mapping" 2>/dev/null | python3 -c 'import json,sys
try:
    print(json.loads(sys.stdin.readline()).get("handle", ""))
except ValueError:
    print("")' 2>/dev/null || true)
    pane_key=$(tail -n 1 "$mapping" 2>/dev/null | python3 -c 'import json,sys
try:
    print(json.loads(sys.stdin.readline()).get("pane_key", ""))
except ValueError:
    print("")' 2>/dev/null || true)
  fi
  if [ -n "$handle" ]; then
    orca terminal list --worktree "$host_worktree" --json > "$tmp/host-tabs-cleanup.json" 2>/dev/null || true
    if ! python3 "$tmp/orca_case.py" tab-gone "$tmp/host-tabs-cleanup.json" "$pane_key" >/dev/null 2>&1; then
      printf 'cleanup: closing leftover tab %s\n' "$handle" >&2
      if ! residue=$(close_orca_tab "$handle" "$pane_key" 2>&1); then
        printf 'cleanup: leftover tab %s survived its close: %s\n' "$handle" "$residue" >&2
        status=1
      fi
    fi
  fi
  # The agent ran inside Orca's pane, not as a child here, so closing the tab
  # above is what normally ends it. This is the belt to that braces: a failure
  # before the tab existed can still leave the process behind, and the match is
  # on this run's unique scratch workspace, so it can only name this case's agent.
  agents=$(pgrep -f -- "--workspace $tmp/planner" 2>/dev/null || true)
  for agent_pid in $agents; do
    printf 'cleanup: killing leftover agent %s\n' "$agent_pid" >&2
    kill -TERM "$agent_pid" 2>/dev/null || true
  done
  kill "$server_pid" 2>/dev/null || true
  wait "$server_pid" 2>/dev/null || true
  rm -rf "$tmp"
  # `return` from an EXIT trap leaves the script's status alone, so the verdict
  # has to leave through `exit`.
  exit $status
}
trap cleanup EXIT
. "$SRC/crates/onlyne-testkit/e2e/lib.sh"

# Case 0a: the supervisor's own tab. The policy under test reads this variable,
# and every listing below is scoped to the worktree it names, so a shell that is
# not an Orca tab has nothing to assert and skips instead of failing.
if [ -z "${ORCA_WORKTREE_ID:-}" ]; then
  echo "SKIP: ORCA_WORKTREE_ID is unset (this shell is not an Orca tab)"
  exit 0
fi
host_worktree=$ORCA_WORKTREE_ID

# Case 0b: the CLI, then the app. A host with no `orca` on PATH is named as
# such rather than folded into the "app not reachable" line, because the two
# have different remedies: one is a missing install, the other a stopped app.
command -v orca >/dev/null 2>&1 || {
  echo "SKIP: the 'orca' CLI is not on PATH, so there is no Orca to drive"
  exit 0
}

# `orca status --json` is the one reliable readiness probe, and this case is
# skipped (not failed) when it does not answer.
if ! orca status --json > "$tmp/status.json" 2>/dev/null; then
  echo "SKIP: the orca CLI is present but the Orca app did not answer"
  exit 0
fi
if ! python3 - "$tmp/status.json" <<'PY'
import json, sys
raw = json.load(open(sys.argv[1]))
sys.exit(0 if isinstance(raw, dict) and raw.get("ok") is True else 1)
PY
then
  echo "SKIP: the orca CLI answered but the Orca app reported itself unreachable"
  exit 0
fi

# The helper is written before anything can fail: the trap needs it.
cat > "$tmp/orca_case.py" <<'PY'
import json
import sys


def load(path):
    with open(path) as handle:
        return json.load(handle)


def result(path):
    raw = load(path)
    return raw.get("result") if isinstance(raw, dict) else None


def tab_rows(path):
    return (result(path) or {}).get("terminals") or []


def pane_of(row):
    # `terminal list` rows carry tabId/leafId but no paneKey on this app
    # (measured 2026-09-11), so the key is the same `tabId:leafId` the backend
    # synthesizes; identity is never the title, because the login shell's own
    # title frame replaces the create-time `--title` within seconds.
    return row.get("paneKey") or "%s:%s" % (row.get("tabId"), row.get("leafId"))


def mapping_lines(path):
    with open(path) as handle:
        return [json.loads(line) for line in handle if line.strip()]


mode = sys.argv[1]

if mode == "unregistered":
    raw = load(sys.argv[2])
    code = (raw.get("error") or {}).get("code") if isinstance(raw, dict) else None
    sys.exit(0 if raw.get("ok") is False and code == "selector_not_found" else 1)

if mode == "tab-count":
    print(len(tab_rows(sys.argv[2])))
    sys.exit(0)

if mode == "pane-live":
    pane = sys.argv[3]
    hits = [row for row in tab_rows(sys.argv[2]) if pane_of(row) == pane]
    if len(hits) != 1:
        print("expected one row for pane %s, found %d" % (pane, len(hits)))
        sys.exit(1)
    row = hits[0]
    if row.get("connected") is not True:
        print("pane %s is not connected: %s" % (pane, json.dumps(row)))
        sys.exit(1)
    if row.get("handle") != sys.argv[4]:
        print("listing handle %r is not the tab map's %r" % (row.get("handle"), sys.argv[4]))
        sys.exit(1)
    sys.stdout.write(json.dumps(row))
    sys.stdout.write("\n")
    sys.exit(0)

if mode == "tab-gone":
    pane = sys.argv[3]
    left = [row for row in tab_rows(sys.argv[2]) if pane_of(row) == pane]
    sys.exit(0 if not left else 1)

if mode == "probe-inputs":
    row = (result(sys.argv[2]) or {}).get("terminal") or {}
    print(
        "connected=%s writable=%s status=%s exitCause=%s lastOutputAt=%s"
        % (
            row.get("connected"),
            row.get("writable"),
            row.get("status"),
            json.dumps(row.get("exitCause")),
            row.get("lastOutputAt"),
        )
    )
    sys.exit(0 if row.get("connected") is True and not row.get("exitCause") else 1)

if mode == "map-spawned":
    lines = mapping_lines(sys.argv[2])
    if len(lines) != 1:
        print("expected one mapping line, found %d" % len(lines))
        sys.exit(1)
    line = lines[0]
    expected = {
        "task_id": sys.argv[3],
        "session_id": sys.argv[3],
        "role": "planner",
        "worktree_selector": sys.argv[4],
        "title": "onlyne:" + sys.argv[3],
        "state": "spawned",
    }
    for key, want in expected.items():
        if line.get(key) != want:
            print("mapping %s is %r, expected %r" % (key, line.get(key), want))
            sys.exit(1)
    if sorted(line) != sorted(list(expected) + ["handle", "pane_key", "updated_at"]):
        print("mapping keys are %s" % sorted(line))
        sys.exit(1)
    if not str(line.get("updated_at", "")).endswith("Z"):
        print("updated_at is not RFC3339 UTC: %r" % line.get("updated_at"))
        sys.exit(1)
    print(json.dumps(line))
    sys.exit(0)

if mode == "map-closed":
    lines = mapping_lines(sys.argv[2])
    line = lines[-1] if lines else {}
    sys.exit(0 if line.get("state") == "closed" and line.get("pane_key") == sys.argv[3] else 1)

print("unknown mode %s" % mode, file=sys.stderr)
sys.exit(2)
PY

ws="$tmp/planner"

# The runtime command is a real mounting agent, not a `sleep`. `sleep 600` was
# the defect this case is fixed for: it occupies the tab without dialling the
# adapter socket, so no plugin ever mounts, the client writes no session row,
# and every read of that row downstream (`owning_role` above all) refuses while
# the server's ledger already names the owner. `onlyne-agent-fake` is the same
# agent the rest of this suite drives — it connects to the socket the client is
# serving, says `hello`, and mounts the role — so the tab now carries a session
# the client actually recorded.
#
# The command is an absolute argv list rather than a shell string: the Orca
# backend runs it as `cd <cwd> && env <spawn env> <argv>; exit`, so each element
# is quoted once by the backend and reaches the tab intact. The workspace is
# named because the agent resolves the served socket and the role from it, and
# `--role` is passed so the mount does not depend on the config parse the case
# appends to below.
FAKE=$(bin onlyne-agent-fake)

# The transcript this agent runs. Two properties are load-bearing:
#   * it holds the assignment rather than completing it, so the tab is still
#     there when the SIGTERM below arrives — a completion would close it first
#     and the drain assertion would prove nothing;
#   * it writes the assign it received out of the workspace, which is how the
#     delivery is proved to have reached a plugin *inside the Orca tab* rather
#     than merely to have left the server.
#
# `mark_mounted` gives the case a file to wait on instead of a timing guess: the
# client's link to the server answers long before any plugin exists, and a
# delivery sent in that gap is staged rather than delivered.
agent_script="$tmp/agent-script.json"
python3 - "$agent_script" "$E2E_PROSE" <<'PY'
import json
import sys

script = {
    "hello": {"capabilities": ["register", "report", "inject", "recycle"]},
    "steps": [
        {"mark_mounted": "orca-mounted.txt"},
        {"wait_assign": True},
        {"assert_prose_equals": sys.argv[2]},
        {"echo_field_to": {"path": "assign.task_id", "file": "orca-assign-task.txt"}},
        {"echo_field_to": {"path": "assign.text", "file": "orca-assign-text.txt"}},
        {"report": "ready"},
        {"report": "heartbeat"},
        {"sleep_ms": 600000},
    ],
}
with open(sys.argv[1], "w") as handle:
    json.dump(script, handle)
    handle.write("\n")
PY

runtime_command=$(python3 -c 'import json, sys; print(json.dumps(sys.argv[1:]))' \
  "$FAKE" --workspace "$ws" --role planner --script "$agent_script")

# The backend gets an explicit `--worktree` under the `host` policy, which is the
# worktree the supervisor's tab runs in — so the role workspace below is never
# registered, and its role config only has to name the policy and the agent.
export ONLYNE_BACKEND=orca
setup_cluster "$tmp/server" "$ws" planner cluster "" "$E2E_PROSE" 'allowed_senders = ["*", "planner"]
allowed_targets = ["planner"]
[client.runtime]
drive = "plugin"
command = '"$runtime_command"
server_pid=$cluster_server_pid
canon_ws=$(cd "$ws" && pwd -P)
mapping="$canon_ws/.onlyne/cache/orca-tabs.jsonl"

# The role config names the policy the case exercises. `host` is the backend
# default too; spelling it out keeps the case honest if that default ever moves.
printf '\n[orca]\nworktree = "host"\n' >> "$ws/.onlyne/config.toml"

# t0: the role workspace is unknown to Orca, and it must stay that way. Nothing
# in this case registers, opens, renames or deletes an Orca project.
orca worktree show --worktree "path:$canon_ws" --json > "$tmp/before.json" 2>/dev/null || true
python3 "$tmp/orca_case.py" unregistered "$tmp/before.json" || fail \
  "a role workspace must not be an Orca project" "$(cat "$tmp/before.json")"

# t0: the host worktree holds the operator's own tabs. Counting them is what lets
# the drain assertion be "back to exactly what the operator had".
orca terminal list --worktree "$host_worktree" --json > "$tmp/host-tabs-before.json" 2>/dev/null || true
host_tabs_before=$(python3 "$tmp/orca_case.py" tab-count "$tmp/host-tabs-before.json")
printf 'NOTE host worktree %s carries %s tab(s) before the case\n' "$host_worktree" "$host_tabs_before"

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
  sleep 0.2
done
[ "$online" = true ] || fail "planner must register on the server" "client=$(cat "$tmp/client.log" 2>/dev/null)"

# The task text is the operator's own words, and the case asserts them byte-exact
# in the assign the plugin rendered inside the tab — so the sentence is named
# once, here, and read back from the agent's own record further down.
TASK_TEXT='orca live probe: hold this assignment open'
send_out=$("$ONLYNE" --server-root "$tmp/server" send "${SUPERVISOR_FLAGS[@]}" --from planner --to planner --text "$TASK_TEXT") \
  || fail "send command failed" "$send_out"
printf '%s\n' "$send_out" > "$tmp/send.json"
task=$(json_field "$tmp/send.json" '.data.task' 'json.load(sys.stdin)["data"]["task"]')
[ "$(json_field "$tmp/send.json" '.data.state' 'json.load(sys.stdin)["data"]["state"]')" = "in_flight" ] \
  || fail "send data.state must be in_flight" "$send_out"

# 1. The tab map is where this case's identity comes from: one line for the new
#    session, naming the host worktree as the selector the policy resolved.
map_line=""
for _ in $(seq 1 150); do
  if [ -f "$mapping" ] && map_line=$(python3 "$tmp/orca_case.py" map-spawned "$mapping" "$task" "$host_worktree" 2>/dev/null); then
    break
  fi
  map_line=""
  sleep 0.2
done
[ -n "$map_line" ] || fail "the backend must write one tab map line resolving to the host worktree" \
  "selector=$host_worktree client=$(cat "$tmp/client.log" 2>/dev/null) $(cat "$mapping" 2>/dev/null)"
handle=$(printf '%s' "$map_line" | python3 -c 'import json,sys; print(json.load(sys.stdin)["handle"])')
pane_key=$(printf '%s' "$map_line" | python3 -c 'import json,sys; print(json.load(sys.stdin)["pane_key"])')
printf 'PASS orca tab map line: %s\n' "$map_line"

# 2. The agent really mounted. This is the assertion the `sleep 600` fixture
#    could not make: the marker file is written by the agent process itself,
#    from inside the Orca tab, after it dialled the socket the client is
#    serving. A tab that merely exists proves nothing about a session.
mounted=false
for _ in $(seq 1 150); do
  if [ -f "$ws/orca-mounted.txt" ]; then
    mounted=true
    break
  fi
  sleep 0.2
done
[ "$mounted" = true ] || fail "the runtime command in the tab must mount a plugin over the adapter socket" \
  "client=$(cat "$tmp/client.log" 2>/dev/null) agent=$("$ONLYNE" --server-root "$tmp/server" sessions --task "$task" 2>/dev/null)"
echo "PASS orca mount: the runtime command in the tab mounted a plugin"

# 2b. The client's own session row, which is the row `owning_role` reads when an
#     operator leaves `--to` off a control op. The role is named here, so a
#     downstream read resolves instead of refusing: that refusal was the visible
#     symptom of the fixture defect, and asserting the row is what closes it.
sessions_out=""
session_row=""
for _ in $(seq 1 150); do
  sessions_out=$("$ONLYNE" --server-root "$tmp/server" sessions --task "$task" 2>/dev/null) || true
  if session_row=$(printf '%s\n' "$sessions_out" | python3 -c '
import json, sys

rows = json.load(sys.stdin)["data"]["sessions"]
mine = [r for r in rows if r.get("role") == "planner"]
if not mine or mine[0].get("public_lifecycle") != "working":
    sys.exit(1)
print(json.dumps({"role": mine[0]["role"], "public_lifecycle": mine[0]["public_lifecycle"]}))
'); then
    break
  fi
  session_row=""
  sleep 0.2
done
[ -n "$session_row" ] || fail "the mounted agent must leave a client session row naming the role" \
  "sessions=$sessions_out client=$(cat "$tmp/client.log" 2>/dev/null)"
printf 'PASS orca session row: %s\n' "$session_row"

# 2c. The delivery reached a plugin, and the text it rendered is the client's
#     one template: source line and body, no task id or hop. The agent wrote
#     both files out of its own `assign`, so this reads what the process inside
#     the tab received rather than what the server sent.
assign_task=""
for _ in $(seq 1 150); do
  if [ -f "$ws/orca-assign-task.txt" ]; then
    assign_task=$(cat "$ws/orca-assign-task.txt" 2>/dev/null)
    break
  fi
  sleep 0.2
done
[ "$assign_task" = "$task" ] || fail "the agent inside the tab must be assigned the task the server sent" \
  "want=$task got=$assign_task client=$(cat "$tmp/client.log" 2>/dev/null)"
assign_text=$(cat "$ws/orca-assign-text.txt" 2>/dev/null)
case "$assign_text" in
  *"From planner:"*"$TASK_TEXT"*) ;;
  *) fail "the rendered delivery text must reach the plugin inside the tab byte-exact" "text=$assign_text" ;;
esac
printf 'PASS orca delivery: task %s reached the tab, rendered as:\n%s\n' "$task" "$assign_text"

# 3. The live tab: the pane the map names, listed flat among the host worktree's
#    tabs, connected, under the same handle.
tab_row=""
for _ in $(seq 1 150); do
  orca terminal list --worktree "$host_worktree" --json > "$tmp/host-tabs.json" 2>/dev/null || true
  if tab_row=$(python3 "$tmp/orca_case.py" pane-live "$tmp/host-tabs.json" "$pane_key" "$handle" 2>/dev/null); then
    break
  fi
  tab_row=""
  sleep 0.2
done
[ -n "$tab_row" ] || fail "the new tab must be listed in the host worktree, connected, under the handle the map recorded" \
  "client=$(cat "$tmp/client.log" 2>/dev/null) $(cat "$tmp/host-tabs.json" 2>/dev/null)"
printf 'PASS orca terminal list row in the host worktree: %s\n' "$tab_row"

# 4. The four-part liveness inputs, read from the live app rather than assumed.
orca terminal show --terminal "$handle" --json > "$tmp/show.json" 2>/dev/null || true
python3 "$tmp/orca_case.py" probe-inputs "$tmp/show.json" || fail \
  "a live tab must answer connected with no exitCause" "$(cat "$tmp/show.json")"
printf 'PASS orca terminal show: %s\n' "$(python3 "$tmp/orca_case.py" probe-inputs "$tmp/show.json")"
printf 'orca terminal show --json -> %s\n' "$(cat "$tmp/show.json")"

# 5. SIGTERM: the drain closes the tab, and the map gets its tombstone.
kill -TERM "$client_pid"
for _ in $(seq 1 150); do
  kill -0 "$client_pid" 2>/dev/null || break
  sleep 0.1
done
if kill -0 "$client_pid" 2>/dev/null; then
  fail "the client must leave after SIGTERM" "client=$(cat "$tmp/client.log" 2>/dev/null)"
fi
wait "$client_pid" 2>/dev/null || true
client_pid=""

gone=false
for _ in $(seq 1 100); do
  orca terminal list --worktree "$host_worktree" --json > "$tmp/host-tabs-now.json" 2>/dev/null || true
  if python3 "$tmp/orca_case.py" tab-gone "$tmp/host-tabs-now.json" "$pane_key" 2>/dev/null; then
    gone=true
    break
  fi
  sleep 0.2
done
if [ "$gone" != true ]; then
  # The fallback the contract allows: the case closes the one handle it read out
  # of its own map, then re-checks. Reaching it is a finding, not a crash.
  echo "NOTE: the drain left tab $handle behind; closing it with the script's own handle"
  close_orca_tab "$handle" "$pane_key" > "$tmp/close.json" 2>&1 || true
  for _ in $(seq 1 50); do
    orca terminal list --worktree "$host_worktree" --json > "$tmp/host-tabs-now.json" 2>/dev/null || true
    if python3 "$tmp/orca_case.py" tab-gone "$tmp/host-tabs-now.json" "$pane_key" 2>/dev/null; then
      gone=true
      break
    fi
    sleep 0.2
  done
fi
if [ "$gone" != true ]; then
  fail "the closed tab must disappear from the host worktree's terminal list" \
    "close: $(cat "$tmp/close.json" 2>/dev/null)
list: $(cat "$tmp/host-tabs-now.json" 2>/dev/null)"
fi
echo "PASS orca drain: tab $handle is gone after SIGTERM"

# 5b. The agent process went with the tab. It ran inside Orca's pane rather than
#     as a child of this script, so nothing here can `wait` on it: the proof is
#     that no process is left carrying this case's own scratch workspace on its
#     command line. The match is on that unique path, so it can only ever name
#     this run's agent.
agent_left=""
for _ in $(seq 1 100); do
  agent_left=$(pgrep -f -- "--workspace $ws" 2>/dev/null || true)
  [ -n "$agent_left" ] || break
  sleep 0.2
done
[ -z "$agent_left" ] || fail "the agent that mounted in the tab must be gone once the client drained" \
  "pids=$agent_left"
echo "PASS orca residue: no agent process is left from this case's workspace"

python3 "$tmp/orca_case.py" map-closed "$mapping" "$pane_key" || fail \
  "the tab map must end with a closed tombstone for $pane_key" "$(cat "$mapping" 2>/dev/null)"
printf 'PASS orca tab map tombstone: %s\n' "$(tail -n 1 "$mapping")"

# 6. Zero residue: the host worktree is back to the tabs it started with, and the
#    role workspace was never made an Orca project on the way.
orca terminal list --worktree "$host_worktree" --json > "$tmp/host-tabs-after.json" 2>/dev/null || true
host_tabs_after=$(python3 "$tmp/orca_case.py" tab-count "$tmp/host-tabs-after.json")
[ "$host_tabs_after" = "$host_tabs_before" ] || fail \
  "the host worktree must hold the same $host_tabs_before tab(s) it started with" \
  "$(cat "$tmp/host-tabs-after.json" 2>/dev/null)"
echo "PASS orca residue: host worktree still holds $host_tabs_after tab(s)"

orca worktree show --worktree "path:$canon_ws" --json > "$tmp/after.json" 2>/dev/null || true
python3 "$tmp/orca_case.py" unregistered "$tmp/after.json" || fail \
  "the case must never register the role workspace in Orca" "$(cat "$tmp/after.json")"
echo "PASS orca registration: none was created ($canon_ws is still unknown)"

echo "PASS orca-live"
