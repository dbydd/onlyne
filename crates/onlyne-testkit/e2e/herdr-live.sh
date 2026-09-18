#!/usr/bin/env bash
set -euo pipefail
# Verification case 13: the herdr backend against a live herdr session.
#
# Needs a reachable herdr session. HERDR_SESSION defaults to onlyne-test.
# The binary defaults to HERDR_BIN_PATH or /opt/homebrew/bin/herdr.
#
# session_command is sleep 600 so spawn takes the pane-run track. Starting a
# real agent is covered by pi-live.sh and crates/onlyne-session/tests/herdr_live.rs.
#
# Skip discipline: a missing binary or an unreachable HERDR_SESSION prints
# `SKIP herdr-live: no reachable herdr session $HERDR_SESSION` and exits 0.
# A host without herdr must not read as a product failure.
#
# This case changes herdr layout. Confirm HERDR_SESSION points at a sacrificial
# test session before running it. Whatever workspaces that session already had
# are snapshotted first and must be there when the case ends.
SRC=$(pwd)
tmp=$(mktemp -d)
server_pid=""
client_pid=""
workspace_id=""
tab_id=""
ref_pane=""
root_pane=""
pane_pid=""
task=""
workspace_closed=false

HERDR_BIN=${HERDR_BIN_PATH:-/opt/homebrew/bin/herdr}
HERDR_SESSION=${HERDR_SESSION:-onlyne-test}

herdr_q() {
  env -u HERDR_ENV -u HERDR_SOCKET_PATH HERDR_SESSION="$HERDR_SESSION" "$HERDR_BIN" "$@"
}

cleanup() {
  local status=$?
  drain_pid "$client_pid"
  if [ -n "$workspace_id" ] && [ "$workspace_closed" != true ]; then
    herdr_q workspace close "$workspace_id" >/dev/null 2>&1 || true
  fi
  kill "$server_pid" 2>/dev/null || true
  wait "$server_pid" 2>/dev/null || true
  # An assertion that fails before the case learns its workspace id leaves the
  # tree behind with no name to close it by. With the daemons down, sweep every
  # workspace that appeared after the pre-run snapshot, so a failed live case
  # still restores the session it touched.
  if [ -f "$tmp/workspaces-before.json" ] && [ -f "$tmp/herdr_case.py" ]; then
    herdr_q workspace list >"$tmp/workspaces-cleanup.json" 2>/dev/null || true
    python3 "$tmp/herdr_case.py" stray-workspaces "$tmp/workspaces-before.json" \
      "$tmp/workspaces-cleanup.json" 2>/dev/null | while read -r stray; do
      [ -n "$stray" ] && herdr_q workspace close "$stray" >/dev/null 2>&1 || true
    done
  fi
  if [ "$status" -eq 0 ] && [ "${HERDR_LIVE_KEEP:-0}" != 1 ]; then
    rm -rf "$tmp"
  else
    echo "herdr-live: scratch directory kept at $tmp" >&2
  fi
  exit "$status"
}
trap cleanup EXIT
. "$SRC/crates/onlyne-testkit/e2e/lib.sh"

# lib.sh pins ONLYNE_BACKEND=fake. This case talks to herdr, so it covers that
# export after the source.
export ONLYNE_BACKEND=herdr

if [ ! -x "$HERDR_BIN" ]; then
  echo "SKIP herdr-live: no reachable herdr session $HERDR_SESSION"
  exit 0
fi

if ! herdr_q workspace list >"$tmp/herdr-probe.json" 2>"$tmp/herdr-probe.err"; then
  echo "SKIP herdr-live: no reachable herdr session $HERDR_SESSION"
  exit 0
fi

cat >"$tmp/herdr_case.py" <<'PY'
import json
import sqlite3
import sys


def load(path):
    with open(path) as handle:
        raw = json.load(handle)
    if isinstance(raw, dict) and isinstance(raw.get("result"), dict):
        return raw["result"]
    return raw if isinstance(raw, dict) else {}


def workspaces(path):
    listed = load(path).get("workspaces")
    return listed if isinstance(listed, list) else []


def tabs(path):
    listed = load(path).get("tabs")
    return listed if isinstance(listed, list) else []


def panes(path):
    listed = load(path).get("panes")
    return listed if isinstance(listed, list) else []


def pane_of(path):
    result = load(path)
    pane = result.get("pane")
    return pane if isinstance(pane, dict) else result


def tab_row(path, label):
    for row in tabs(path):
        if row.get("label") == label:
            return row
    return None


def panes_for(path, workspace_id, tab_id):
    return [
        row
        for row in panes(path)
        if row.get("workspace_id") == workspace_id and row.get("tab_id") == tab_id
    ]


def herdr_object(raw):
    if not isinstance(raw, dict):
        return None
    if isinstance(raw.get("herdr"), dict):
        return raw["herdr"]
    inner = raw.get("backend_ref")
    if isinstance(inner, dict) and isinstance(inner.get("herdr"), dict):
        return inner["herdr"]
    return None


cmd = sys.argv[1]
if cmd == "workspace-id":
    label = sys.argv[3]
    for row in workspaces(sys.argv[2]):
        if row.get("label") == label and row.get("workspace_id"):
            print(row["workspace_id"])
            sys.exit(0)
    sys.exit(1)
if cmd == "tab-id":
    row = tab_row(sys.argv[2], sys.argv[3])
    if row and row.get("tab_id"):
        print(row["tab_id"])
        sys.exit(0)
    sys.exit(1)
if cmd == "tab-pane-count":
    row = tab_row(sys.argv[2], sys.argv[3])
    want = int(sys.argv[4])
    if row and int(row.get("pane_count") or 0) == want:
        sys.exit(0)
    sys.exit(1)
if cmd == "tab-panes":
    # Every pane id in one tab. The case uses this instead of a "root pane"
    # query because herdr exposes no such flag: the tab's own pane is whatever
    # is left when the session's pane (named by the client's `backend_ref`) is
    # taken away.
    found = False
    for row in panes_for(sys.argv[2], sys.argv[3], sys.argv[4]):
        pane_id = row.get("pane_id")
        if pane_id:
            found = True
            print(pane_id)
    sys.exit(0 if found else 1)
if cmd == "pane-absent":
    pane_id = sys.argv[3]
    for row in panes(sys.argv[2]):
        if row.get("pane_id") == pane_id:
            sys.exit(1)
    sys.exit(0)
if cmd == "focused":
    pane = pane_of(sys.argv[2])
    sys.exit(0 if pane.get("focused") is True else 1)
if cmd == "process-pid":
    info = load(sys.argv[2])
    nested = info.get("process_info")
    if isinstance(nested, dict):
        info = nested
    pid = info.get("shell_pid")
    if isinstance(pid, int) and pid > 1:
        print(pid)
        sys.exit(0)
    sys.exit(1)
if cmd == "snapshot":
    workspaces_path, panes_path = sys.argv[2], sys.argv[3]
    json.dump(
        {"workspaces": workspaces(workspaces_path), "panes": panes(panes_path)},
        sys.stdout,
    )
    sys.exit(0)
if cmd == "snapshot-restored":
    before = json.load(open(sys.argv[2]))
    after = workspaces(sys.argv[3])
    closed = sys.argv[4]
    before_ids = {row.get("workspace_id") for row in before.get("workspaces") or [] if row.get("workspace_id")}
    after_ids = {row.get("workspace_id") for row in after if row.get("workspace_id")}
    if closed in after_ids:
        sys.exit(1)
    if not after_ids.issubset(before_ids):
        sys.exit(1)
    # Every workspace the session had before the case started is still there.
    # Which ones that is belongs to the host, not to this case.
    if not before_ids.issubset(after_ids):
        sys.exit(1)
    sys.exit(0)
if cmd == "stray-workspaces":
    # Ids that appeared after the pre-run snapshot: the tree this case made,
    # whatever label it carried and whether the case learned its id yet.
    before = json.load(open(sys.argv[2]))
    after = workspaces(sys.argv[3])
    before_ids = {row.get("workspace_id") for row in before.get("workspaces") or [] if row.get("workspace_id")}
    for row in after:
        workspace_id = row.get("workspace_id")
        if workspace_id and workspace_id not in before_ids:
            print(workspace_id)
    sys.exit(0)
if cmd == "backend-ref":
    db, task = sys.argv[2], sys.argv[3]
    row = sqlite3.connect(db).execute(
        "SELECT backend_ref FROM sessions WHERE task_id=?", (task,)
    ).fetchone()
    if not row or not row[0]:
        sys.exit(1)
    try:
        parsed = json.loads(row[0])
    except ValueError:
        sys.exit(1)
    herdr = herdr_object(parsed)
    if herdr is None:
        sys.exit(1)
    workspace_id = herdr.get("workspace_id") or ""
    tab_id = herdr.get("tab_id") or ""
    pane_id = herdr.get("pane_id") or ""
    if not workspace_id or not tab_id or not pane_id:
        sys.exit(1)
    json.dump(
        {"workspace_id": workspace_id, "tab_id": tab_id, "pane_id": pane_id, "raw": parsed},
        sys.stdout,
    )
    sys.exit(0)
sys.exit(2)
PY

herdr_q workspace list >"$tmp/workspaces-before.json" 2>"$tmp/workspaces-before.err" \
  || fail "herdr workspace list failed while recording the pre-run snapshot" "$(cat "$tmp/workspaces-before.err" 2>/dev/null)"
herdr_q pane list >"$tmp/panes-before.json" 2>"$tmp/panes-before.err" \
  || fail "herdr pane list failed while recording the pre-run snapshot" "$(cat "$tmp/panes-before.err" 2>/dev/null)"
python3 "$tmp/herdr_case.py" snapshot "$tmp/workspaces-before.json" "$tmp/panes-before.json" >"$tmp/herdr-before.json"

# The client availability gate is HERDR_ENV=1 plus one of HERDR_SESSION /
# HERDR_SOCKET_PATH / HERDR_WORKSPACE_ID. Session name is enough; the client
# resolves the socket itself.
export HERDR_ENV=1
export HERDR_SESSION
export HERDR_BIN_PATH="$HERDR_BIN"

setup_cluster "$tmp/server" "$tmp/planner" planner cluster "" "" 'allowed_senders = ["*", "planner"]
allowed_targets = ["planner"]
session_command = ["sleep", "600"]'
server_pid=$cluster_server_pid
ws="$tmp/planner"

# The workspace label is the server's own topology name (`onlyne-server init`
# writes `[server] name` from the server root directory, and the client reads it
# back as `welcome.cluster`), so the case reads it from the spec it generated and
# never pins a name the product does not promise.
server_name=$(sed -n 's/^name = "\(.*\)"$/\1/p' "$tmp/server/.onlyne/spec.toml" | head -1)
[ -n "$server_name" ] || fail "spec.toml must name the server topology" "$(cat "$tmp/server/.onlyne/spec.toml")"
want_ws_label="onlyne:${server_name}"

"$CLIENT" run --workspace "$ws" >"$tmp/client.log" 2>&1 &
client_pid=$!

online=false
for _ in $(seq 1 150); do
  roles_out=$("$ONLYNE" --server-root "$tmp/server" roles 2>/dev/null) || true
  printf '%s\n' "$roles_out" >"$tmp/roles.json"
  if rows_any "$tmp/roles.json" state online 2>/dev/null; then
    online=true
    break
  fi
  sleep 0.2
done
[ "$online" = true ] || fail "planner must register on the server" "client=$(cat "$tmp/client.log" 2>/dev/null)"

send_out=$("$ONLYNE" --server-root "$tmp/server" send --from planner --to planner --text "herdr live task") \
  || fail "send command failed" "$send_out"
printf '%s\n' "$send_out" >"$tmp/send.json"
task=$(json_field "$tmp/send.json" '.data.task' 'json.load(sys.stdin)["data"]["task"]')
[ "$(json_field "$tmp/send.json" '.data.state' 'json.load(sys.stdin)["data"]["state"]')" = "in_flight" ] \
  || fail "send data.state must be in_flight" "$send_out"

# a. the tree: a workspace labelled `onlyne:<server name>` holding a tab labelled
# with the role. Any pane count — the split lands within a few hundred ms of the
# tab's creation, so the 1-pane moment is not a state a poller may rely on.
found_ws=false
for _ in $(seq 1 150); do
  herdr_q workspace list >"$tmp/workspaces.json" 2>"$tmp/workspaces.err" || true
  if workspace_id=$(python3 "$tmp/herdr_case.py" workspace-id "$tmp/workspaces.json" "$want_ws_label" 2>/dev/null); then
    found_ws=true
    break
  fi
  sleep 0.2
done
[ "$found_ws" = true ] || fail "workspace list must grow a row labelled $want_ws_label" \
  "workspaces=$(cat "$tmp/workspaces.json" 2>/dev/null) err=$(cat "$tmp/workspaces.err" 2>/dev/null) client=$(cat "$tmp/client.log" 2>/dev/null)"

found_tab=false
for _ in $(seq 1 150); do
  herdr_q tab list --workspace "$workspace_id" >"$tmp/tabs.json" 2>"$tmp/tabs.err" || true
  if tab_id=$(python3 "$tmp/herdr_case.py" tab-id "$tmp/tabs.json" planner 2>/dev/null); then
    found_tab=true
    break
  fi
  sleep 0.2
done
[ "$found_tab" = true ] || fail "tab list must show a tab labelled planner in $want_ws_label" \
  "tabs=$(cat "$tmp/tabs.json" 2>/dev/null) err=$(cat "$tmp/tabs.err" 2>/dev/null) client=$(cat "$tmp/client.log" 2>/dev/null)"
printf 'PASS herdr-live tree: workspace=%s tab=%s\n' "$workspace_id" "$tab_id"

# b. the split: the role tab carries two panes once the session is placed.
found_split=false
for _ in $(seq 1 150); do
  herdr_q tab list --workspace "$workspace_id" >"$tmp/tabs.json" 2>"$tmp/tabs.err" || true
  if python3 "$tmp/herdr_case.py" tab-pane-count "$tmp/tabs.json" "planner" 2 2>/dev/null; then
    herdr_q pane list >"$tmp/panes.json" 2>"$tmp/panes.err" || true
    if python3 "$tmp/herdr_case.py" tab-panes "$tmp/panes.json" "$workspace_id" "$tab_id" \
      >"$tmp/tab-panes.txt" 2>/dev/null && [ "$(wc -l <"$tmp/tab-panes.txt" | tr -d ' ')" = 2 ]; then
      found_split=true
      break
    fi
  fi
  sleep 0.2
done
[ "$found_split" = true ] || fail "tab planner pane_count must become 2 with two panes listed" \
  "tabs=$(cat "$tmp/tabs.json" 2>/dev/null) panes=$(cat "$tmp/panes.json" 2>/dev/null) client=$(cat "$tmp/client.log" 2>/dev/null)"
printf 'PASS herdr-live pane_count=2 panes=%s\n' "$(tr '\n' ' ' <"$tmp/tab-panes.txt")"

# c. ledger stays in_flight (sleep never completes). The wire SessionRow from
# `onlyne sessions` has no backend_ref column (crates/onlyne-proto SessionRow,
# crates/onlyne-server row_from_write). The herdr object lives in client.db
# sessions.backend_ref, which stores the SessionRef JSON
# `{backend, backend_ref: {herdr: {workspace_id, tab_id, pane_id, ...}}}`.
ledger_out=""
ledger_inflight=false
for _ in $(seq 1 150); do
  ledger_out=$("$ONLYNE" --server-root "$tmp/server" ledger --task "$task" 2>/dev/null) || true
  printf '%s\n' "$ledger_out" >"$tmp/ledger.json"
  if rows_any "$tmp/ledger.json" state in_flight 2>/dev/null; then
    ledger_inflight=true
    break
  fi
  sleep 0.2
done
[ "$ledger_inflight" = true ] || fail "ledger state must be in_flight" \
  "ledger=$ledger_out client=$(cat "$tmp/client.log" 2>/dev/null)"

ref_ok=false
sessions_out=""
for _ in $(seq 1 150); do
  sessions_out=$("$ONLYNE" --server-root "$tmp/server" sessions --task "$task" 2>/dev/null) || true
  printf '%s\n' "$sessions_out" >"$tmp/sessions.json"
  if python3 "$tmp/herdr_case.py" backend-ref "$ws/.onlyne/client.db" "$task" >"$tmp/backend-ref.json" 2>/dev/null; then
    ref_ok=true
    break
  fi
  sleep 0.2
done
[ "$ref_ok" = true ] || fail "client.db sessions.backend_ref must carry a herdr object with workspace_id/tab_id/pane_id" \
  "sessions=$sessions_out client=$(cat "$tmp/client.log" 2>/dev/null)"
ref_ws=$(json_field "$tmp/backend-ref.json" '.workspace_id' 'json.load(sys.stdin)["workspace_id"]')
ref_tab=$(json_field "$tmp/backend-ref.json" '.tab_id' 'json.load(sys.stdin)["tab_id"]')
ref_pane=$(json_field "$tmp/backend-ref.json" '.pane_id' 'json.load(sys.stdin)["pane_id"]')
[ "$ref_ws" = "$workspace_id" ] && [ "$ref_tab" = "$tab_id" ] \
  || fail "backend_ref.herdr must name the live workspace and tab" \
    "ref=$(cat "$tmp/backend-ref.json") workspace=$workspace_id tab=$tab_id sessions=$sessions_out"
# The session's pane is one of the two in that tab; the other is the tab's own
# root pane, which the client keeps.
grep -qx "$ref_pane" "$tmp/tab-panes.txt" \
  || fail "backend_ref.herdr.pane_id must be a pane in the role tab" \
    "ref_pane=$ref_pane tab_panes=$(tr '\n' ' ' <"$tmp/tab-panes.txt")"
root_pane=$(grep -vx "$ref_pane" "$tmp/tab-panes.txt" | head -1)
[ -n "$root_pane" ] && [ "$root_pane" != "$ref_pane" ] \
  || fail "the role tab must keep a root pane beside the session pane" \
    "ref_pane=$ref_pane tab_panes=$(tr '\n' ' ' <"$tmp/tab-panes.txt")"
printf 'PASS herdr-live ledger in_flight backend_ref.herdr=%s/%s/%s root=%s\n' \
  "$ref_ws" "$ref_tab" "$ref_pane" "$root_pane"

# d. control focus, then pane get result.pane.focused becomes true.
# `--from` is global on the `control` subcommand, so it follows the verb.
focus_out=$("$ONLYNE" --server-root "$tmp/server" control --from planner focus --task "$task") \
  || fail "control focus --task failed" "out=$focus_out client=$(cat "$tmp/client.log" 2>/dev/null)"
focused=false
for _ in $(seq 1 150); do
  herdr_q pane get "$ref_pane" >"$tmp/pane-get.json" 2>"$tmp/pane-get.err" || true
  if python3 "$tmp/herdr_case.py" focused "$tmp/pane-get.json" 2>/dev/null; then
    focused=true
    break
  fi
  sleep 0.2
done
[ "$focused" = true ] || fail "pane get result.pane.focused must become true after control focus" \
  "pane=$(cat "$tmp/pane-get.json" 2>/dev/null) err=$(cat "$tmp/pane-get.err" 2>/dev/null) focus=$focus_out client=$(cat "$tmp/client.log" 2>/dev/null)"
printf 'PASS herdr-live focus: pane %s focused=true\n' "$ref_pane"

# e. capture the process herdr reports for the session pane, then close the
# session through the control surface.
herdr_q pane process-info --pane "$ref_pane" >"$tmp/process-info.json" 2>"$tmp/process-info.err" \
  || fail "pane process-info must describe the live session process" \
    "pane=$ref_pane err=$(cat "$tmp/process-info.err" 2>/dev/null)"
pane_pid=$(python3 "$tmp/herdr_case.py" process-pid "$tmp/process-info.json" 2>/dev/null) \
  || fail "pane process-info must carry a shell_pid" \
    "pane=$ref_pane info=$(cat "$tmp/process-info.json" 2>/dev/null)"
kill -0 "$pane_pid" 2>/dev/null \
  || fail "the process reported for the live pane must exist" "pane=$ref_pane pid=$pane_pid"

recycle_out=$("$ONLYNE" --server-root "$tmp/server" control --from planner recycle \
  --task "$task" --reason "herdr live close") \
  || fail "control recycle --task failed" \
    "out=$recycle_out client=$(cat "$tmp/client.log" 2>/dev/null)"

pane_gone=false
process_gone=false
for _ in $(seq 1 150); do
  herdr_q pane list >"$tmp/panes.json" 2>"$tmp/panes.err" || true
  if python3 "$tmp/herdr_case.py" pane-absent "$tmp/panes.json" "$ref_pane" 2>/dev/null; then
    pane_gone=true
  fi
  if ! kill -0 "$pane_pid" 2>/dev/null; then
    process_gone=true
  fi
  if [ "$pane_gone" = true ] && [ "$process_gone" = true ]; then
    break
  fi
  sleep 0.2
done
[ "$pane_gone" = true ] || fail "pane list must drop the control-closed pane id" \
  "panes=$(cat "$tmp/panes.json" 2>/dev/null) recycle=$recycle_out client=$(cat "$tmp/client.log" 2>/dev/null)"
[ "$process_gone" = true ] || fail "the process inside the control-closed pane must exit" \
  "pane=$ref_pane pid=$pane_pid recycle=$recycle_out client=$(cat "$tmp/client.log" 2>/dev/null)"

set +e
herdr_q pane get "$ref_pane" >"$tmp/pane-get-gone.out" 2>"$tmp/pane-get-gone.err"
gone_rc=$?
set -e
gone_body=$(printf '%s\n%s\n' "$(cat "$tmp/pane-get-gone.out" 2>/dev/null)" "$(cat "$tmp/pane-get-gone.err" 2>/dev/null)")
[ "$gone_rc" -eq 1 ] || fail "pane get on a closed pane must exit 1" "rc=$gone_rc body=$gone_body"
printf '%s' "$gone_body" | grep -q 'pane_not_found' \
  || fail "pane get on a closed pane must name pane_not_found" "rc=$gone_rc body=$gone_body"

count_back=false
for _ in $(seq 1 150); do
  herdr_q tab list --workspace "$workspace_id" >"$tmp/tabs.json" 2>"$tmp/tabs.err" || true
  if python3 "$tmp/herdr_case.py" tab-pane-count "$tmp/tabs.json" "planner" 1 2>/dev/null; then
    herdr_q pane list >"$tmp/panes-survivor.json" 2>/dev/null || true
    survivor=$(python3 "$tmp/herdr_case.py" tab-panes "$tmp/panes-survivor.json" "$workspace_id" "$tab_id" 2>/dev/null | head -1)
    count_back=true
    break
  fi
  sleep 0.2
done
[ "$count_back" = true ] || fail "role tab pane_count must return to 1 after control close" \
  "tabs=$(cat "$tmp/tabs.json" 2>/dev/null) client=$(cat "$tmp/client.log" 2>/dev/null)"
[ "$survivor" = "$root_pane" ] \
  || fail "the tab root pane must survive the session pane" \
    "survivor=$survivor root=$root_pane"
printf 'PASS herdr-live control close: pane %s gone, process %s gone, pane_count=1, root %s kept\n' \
  "$ref_pane" "$pane_pid" "$root_pane"

# f. drain the client after the closed session has released its host resource.
kill -TERM "$client_pid" 2>/dev/null || true
drain_pid "$client_pid"
client_pid=""

herdr_q workspace close "$workspace_id" >"$tmp/workspace-close.json" 2>"$tmp/workspace-close.err" \
  || fail "workspace close of the case workspace must succeed" \
    "id=$workspace_id out=$(cat "$tmp/workspace-close.json" 2>/dev/null) err=$(cat "$tmp/workspace-close.err" 2>/dev/null)"
workspace_closed=true

closed_gone=false
for _ in $(seq 1 150); do
  herdr_q workspace list >"$tmp/workspaces-after.json" 2>"$tmp/workspaces-after.err" || true
  if python3 "$tmp/herdr_case.py" snapshot-restored "$tmp/herdr-before.json" "$tmp/workspaces-after.json" "$workspace_id" 2>/dev/null; then
    closed_gone=true
    break
  fi
  sleep 0.2
done
[ "$closed_gone" = true ] || fail "workspace list must drop $want_ws_label and keep the pre-run workspaces" \
  "before=$(cat "$tmp/herdr-before.json") after=$(cat "$tmp/workspaces-after.json" 2>/dev/null)"
printf 'PASS herdr-live workspace close: %s gone, pre-run snapshot restored\n' "$want_ws_label"

echo "PASS herdr-live"
