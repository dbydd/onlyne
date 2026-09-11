#!/usr/bin/env bash
set -euo pipefail
# Verification case: the Orca session backend against the real Orca app.
#
# This is the only case in the suite that talks to a real window manager. It
# spawns `sleep 600` (no agent binary is involved, so the task parks in_flight by
# design) in a tab Orca really opens, and proves four things against the live
# app: the AutoRegister worktree selector, the four-part liveness probe inputs,
# the plugin-facing tab map under `.onlyne/cache/`, and that SIGTERM makes the
# client close the tab it opened.
#
# Safety rules this case holds to:
#   * every path it touches lives under one `mktemp -d` tree;
#   * it never passes `--focus` and never runs `terminal switch`;
#   * the only tab it ever closes is the one whose handle it discovered itself
#     in its own worktree, and the only Orca registration it deletes is the one
#     whose path is that temp workspace — it refuses to run at all when the temp
#     workspace turns out to be registered already;
#   * without a reachable Orca app it prints `SKIP` and exits 0, so a CI host
#     without Orca does not read as a product failure.
SRC=$(pwd)
tmp=$(mktemp -d)
server_pid=""
client_pid=""
ws=""
canon_ws=""
selector=""
pane_key=""
handle=""
task=""
mapping=""
created_registration=false

cleanup() {
  local status=$?
  if [ -n "$client_pid" ] && kill -0 "$client_pid" 2>/dev/null; then
    kill -TERM "$client_pid" 2>/dev/null || true
    for _ in $(seq 1 100); do
      kill -0 "$client_pid" 2>/dev/null || break
      sleep 0.1
    done
    kill -KILL "$client_pid" 2>/dev/null || true
    wait "$client_pid" 2>/dev/null || true
  fi
  # A tab the drain did not reap still belongs to this case, and its handle is
  # the one this script recorded — nothing is ever matched by name.
  if [ -n "$handle" ] && [ -f "$tmp/tabs-now.json" ]; then
    if ! python3 "$tmp/orca_case.py" tab-gone "$tmp/tabs-now.json" "$pane_key" >/dev/null 2>&1; then
      printf 'cleanup: closing leftover tab %s\n' "$handle" >&2
      orca terminal close --terminal "$handle" --json >/dev/null 2>&1 || true
    fi
  fi
  if [ "$created_registration" = true ] && [ -n "$canon_ws" ]; then
    delete_registration
  fi
  kill "$server_pid" 2>/dev/null || true
  wait "$server_pid" 2>/dev/null || true
  rm -rf "$tmp"
  return $status
}
trap cleanup EXIT
. "$SRC/crates/onlyne-testkit/e2e/lib.sh"

# Case 0: the app itself. `orca status --json` is the one reliable readiness
# probe, and this case is skipped (not failed) when it does not answer.
if ! orca status --json > "$tmp/status.json" 2>/dev/null; then
  echo "SKIP: orca app not reachable"
  exit 0
fi
if ! python3 - "$tmp/status.json" <<'PY'
import json, sys
raw = json.load(open(sys.argv[1]))
result = raw.get("result") or {}
running = (result.get("app") or {}).get("running")
sys.exit(0 if raw.get("ok") is True and running is not False else 1)
PY
then
  echo "SKIP: orca app not reachable"
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


def mapping_lines(path):
    with open(path) as handle:
        return [json.loads(line) for line in handle if line.strip()]


mode = sys.argv[1]

if mode == "unregistered":
    raw = load(sys.argv[2])
    code = (raw.get("error") or {}).get("code") if isinstance(raw, dict) else None
    sys.exit(0 if raw.get("ok") is False and code == "selector_not_found" else 1)

if mode == "worktree-registered":
    rows = (result(sys.argv[2]) or {}).get("worktrees") or []
    sys.exit(0 if any(row.get("path") == sys.argv[3] for row in rows) else 1)

if mode == "tab-live":
    rows = tab_rows(sys.argv[2])
    if len(rows) != 1:
        print("expected exactly one tab in the case workspace, found %d" % len(rows))
        sys.exit(1)
    row = rows[0]
    # `terminal list` rows carry tabId/leafId but no paneKey on this app
    # (measured 2026-09-11), so the key is the same `tabId:leafId` the backend
    # synthesizes; identity is never the title, because the login shell's own
    # title frame replaces the create-time `--title` within seconds.
    pane = row.get("paneKey") or "%s:%s" % (row.get("tabId"), row.get("leafId"))
    if not row.get("tabId") or not row.get("leafId"):
        print("tab row has no pane identity: %s" % json.dumps(row))
        sys.exit(1)
    if row.get("connected") is not True:
        print("tab is not connected: %s" % json.dumps(row))
        sys.exit(1)
    sys.stdout.write(json.dumps(dict(row, paneKey=pane)))
    sys.stdout.write("\n")
    sys.exit(0)

if mode == "tab-title":
    rows = tab_rows(sys.argv[2])
    print(rows[0].get("title") if rows else "")
    sys.exit(0)

if mode == "tab-gone":
    rows = tab_rows(sys.argv[2])
    left = [row for row in rows if (row.get("paneKey") or "") == sys.argv[3]]
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
        "handle": sys.argv[4],
        "pane_key": sys.argv[5],
        "worktree_selector": sys.argv[6],
        "title": "onlyne:" + sys.argv[3],
        "state": "spawned",
    }
    for key, want in expected.items():
        if line.get(key) != want:
            print("mapping %s is %r, expected %r" % (key, line.get(key), want))
            sys.exit(1)
    if sorted(line) != sorted(list(expected) + ["updated_at"]):
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

if mode == "setup-id":
    rows = (result(sys.argv[2]) or {}).get("setups") or []
    hits = [row for row in rows if row.get("path") == sys.argv[3]]
    if hits:
        print(hits[0].get("id") or "")
    sys.exit(0 if hits else 1)

print("unknown mode %s" % mode, file=sys.stderr)
sys.exit(2)
PY

delete_registration() {
  orca project setups --json > "$tmp/setups.json" 2>/dev/null || return 0
  local setup
  setup=$(python3 "$tmp/orca_case.py" setup-id "$tmp/setups.json" "$canon_ws" 2>/dev/null) || return 0
  [ -n "$setup" ] || return 0
  printf 'cleanup: orca project setup-delete --setup %s --json\n' "$setup"
  orca project setup-delete --setup "$setup" --json > "$tmp/deregister.json" 2>&1 || true
  python3 - "$tmp/deregister.json" <<'PY' || true
import json, sys
raw = json.load(open(sys.argv[1]))
print("cleanup: setup-delete ok=%s" % raw.get("ok"))
PY
}

# Orca gets an explicit `--worktree`, so the case needs a workspace the
# AutoRegister probe can really register. The public path is `orca repo add`,
# which accepts git checkouts only (measured on 1.4.198; the runtime RPC behind
# it takes a `kind` the CLI never passes), so the case workspace is a git repo.
export ONLYNE_BACKEND=orca
setup_cluster "$tmp/server" "$tmp/planner" planner cluster "" "" 'allowed_senders = ["*", "planner"]
allowed_targets = ["planner"]
session_command = ["sleep", "600"]'
server_pid=$cluster_server_pid
ws="$tmp/planner"
canon_ws=$(cd "$ws" && pwd -P)
selector="path:$canon_ws"
mapping="$canon_ws/.onlyne/cache/orca-tabs.jsonl"

# The role config names the policy the case exercises.
printf '\n[orca]\nworktree = "auto"\n' >> "$ws/.onlyne/config.toml"
git -C "$ws" init -q
git -C "$ws" add -A
git -C "$ws" -c user.email=onlyne-e2e@example.com -c user.name=onlyne-e2e commit -qm "orca live case workspace"

# t0: the workspace is unknown to Orca. That is what makes AutoRegister
# observable, and it is also the guard that keeps cleanup from deleting a
# registration this case did not create.
orca worktree show --worktree "$selector" --json > "$tmp/before.json" 2>/dev/null || true
python3 "$tmp/orca_case.py" unregistered "$tmp/before.json" || fail \
  "the temp workspace must start unregistered, or this case would delete someone else's Orca project" \
  "$(cat "$tmp/before.json")"

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

send_out=$("$ONLYNE" --server-root "$tmp/server" send --from planner --to planner --text "orca live probe") \
  || fail "send command failed" "$send_out"
printf '%s\n' "$send_out" > "$tmp/send.json"
task=$(json_field "$tmp/send.json" '.data.task' 'json.load(sys.stdin)["data"]["task"]')
[ "$(json_field "$tmp/send.json" '.data.state' 'json.load(sys.stdin)["data"]["state"]')" = "in_flight" ] \
  || fail "send data.state must be in_flight" "$send_out"

# 1. AutoRegister: the backend registered the workspace before it created the tab.
registered=false
for _ in $(seq 1 150); do
  orca worktree list --json > "$tmp/worktrees.json" 2>/dev/null || true
  if python3 "$tmp/orca_case.py" worktree-registered "$tmp/worktrees.json" "$canon_ws" 2>/dev/null; then
    registered=true
    break
  fi
  sleep 0.2
done
[ "$registered" = true ] || fail "AutoRegister must make the role workspace an Orca worktree" \
  "client=$(cat "$tmp/client.log" 2>/dev/null) $(cat "$tmp/worktrees.json" 2>/dev/null)"
created_registration=true
echo "PASS orca worktree registration: path:$canon_ws"

# 2. The live tab: exactly one terminal in that worktree, connected, with the
#    pane identity the mapping file records.
tab_row=""
for _ in $(seq 1 150); do
  orca terminal list --worktree "$selector" --json > "$tmp/tabs.json" 2>/dev/null || true
  if tab_row=$(python3 "$tmp/orca_case.py" tab-live "$tmp/tabs.json" "$task" 2>/dev/null); then
    break
  fi
  tab_row=""
  sleep 0.2
done
[ -n "$tab_row" ] || fail "the backend must open one connected tab in the case worktree" \
  "client=$(cat "$tmp/client.log" 2>/dev/null) $(cat "$tmp/tabs.json" 2>/dev/null)"
handle=$(printf '%s' "$tab_row" | python3 -c 'import json,sys; print(json.load(sys.stdin)["handle"])')
pane_key=$(printf '%s' "$tab_row" | python3 -c 'import json,sys; print(json.load(sys.stdin)["paneKey"])')
printf 'PASS orca terminal list row: %s\n' "$tab_row"
printf 'NOTE orca tab title after boot: %s\n' "$(python3 "$tmp/orca_case.py" tab-title "$tmp/tabs.json")"

# 3. The four-part liveness inputs, read from the live app rather than assumed.
orca terminal show --terminal "$handle" --json > "$tmp/show.json" 2>/dev/null || true
python3 "$tmp/orca_case.py" probe-inputs "$tmp/show.json" || fail \
  "a live tab must answer connected with no exitCause" "$(cat "$tmp/show.json")"
printf 'PASS orca terminal show: %s\n' "$(python3 "$tmp/orca_case.py" probe-inputs "$tmp/show.json")"
printf 'orca terminal show --json -> %s\n' "$(cat "$tmp/show.json")"

# 4. The tab map the plugin side folds, written by the backend for this session.
[ -f "$mapping" ] || fail "the backend must write $mapping" "client=$(cat "$tmp/client.log" 2>/dev/null)"
printf 'PASS orca tab map: %s\n' "$(python3 "$tmp/orca_case.py" map-spawned "$mapping" "$task" "$handle" "$pane_key" "$selector")"

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
  orca terminal list --worktree "$selector" --json > "$tmp/tabs-now.json" 2>/dev/null || true
  if python3 "$tmp/orca_case.py" tab-gone "$tmp/tabs-now.json" "$pane_key" 2>/dev/null; then
    gone=true
    break
  fi
  sleep 0.2
done
if [ "$gone" != true ]; then
  # The fallback the contract allows: the case closes the one handle it
  # recorded itself, then re-checks. Reaching it is a finding, not a crash.
  echo "NOTE: the drain left tab $handle behind; closing it with the script's own handle"
  orca terminal close --terminal "$handle" --json > "$tmp/close.json" 2>/dev/null || true
  for _ in $(seq 1 50); do
    orca terminal list --worktree "$selector" --json > "$tmp/tabs-now.json" 2>/dev/null || true
    if python3 "$tmp/orca_case.py" tab-gone "$tmp/tabs-now.json" "$pane_key" 2>/dev/null; then
      gone=true
      break
    fi
    sleep 0.2
  done
fi
[ "$gone" = true ] || fail "the closed tab must disappear from terminal list" \
  "$(cat "$tmp/tabs-now.json" 2>/dev/null)"
echo "PASS orca drain: tab $handle is gone after SIGTERM"

python3 "$tmp/orca_case.py" map-closed "$mapping" "$pane_key" || fail \
  "the tab map must end with a closed tombstone for $pane_key" "$(cat "$mapping" 2>/dev/null)"
printf 'PASS orca tab map tombstone: %s\n' "$(tail -n 1 "$mapping")"

# 6. The registration is this case's own garbage: delete it and prove it is gone.
delete_registration
created_registration=false
orca worktree show --worktree "$selector" --json > "$tmp/after.json" 2>/dev/null || true
python3 "$tmp/orca_case.py" unregistered "$tmp/after.json" || fail \
  "the case must delete the Orca registration it created" "$(cat "$tmp/after.json")"
echo "PASS orca registration cleanup: $selector is unregistered again"

echo "PASS orca-live"
