#!/usr/bin/env bash
# Shared e2e helpers for onlyne-testkit scripts. Sourced, not executed.
# Callers set SRC (repo root) and tmp (scratch dir) before sourcing.
# No real platform credential or window manager is touched: ONLYNE_BACKEND=fake plus the fake gateway only.
export ONLYNE_BACKEND=fake

# Prose the planner entries carry, passed to `onlyne-client init --prose` by the
# registration helpers below. `scripts/echo-complete.json` asserts the fake agent
# receives exactly this string as `assign.prose` (docs/v1-PLAN.md line 498).
E2E_PROSE='v1 smoke prose'

fail() {
  echo "FAIL: $1" >&2
  if [ -n "${2:-}" ]; then echo "---- offending output ----" >&2; printf '%s\n' "$2" >&2; fi
  exit 1
}

# `blocked <reason>` marks a case the tree cannot run yet, with exit 3, so a
# refusal from another crate reads as blocked work rather than a passing test.
blocked() {
  echo "BLOCKED: $1" >&2
  if [ -n "${2:-}" ]; then echo "---- observed ----" >&2; printf '%s\n' "$2" >&2; fi
  exit 3
}

# Build directory holding the workspace binaries. `CARGO_TARGET_DIR` selects the
# private target directory the brief asks for, `ONLYNE_BIN_DIR` overrides the
# whole relative or absolute path, and the default matches a plain
# `cargo build --workspace`.
BIN_DIR=${ONLYNE_BIN_DIR:-${CARGO_TARGET_DIR:-target}/debug}

bin() {
  local name=$1
  local path
  case "$BIN_DIR" in
    /*) path="$BIN_DIR/$name" ;;
    *) path="$SRC/$BIN_DIR/$name" ;;
  esac
  if [[ ! -x "$path" ]]; then
    echo "onlyne: missing binary $path; run cargo build --workspace" >&2
    exit 127
  fi
  printf '%s\n' "$path"
}

# `free_port` prints one currently unbound loopback port. Every case allocates
# its own, so two cases never contend for one address and a leftover daemon from
# an earlier run cannot make a later case read as a product failure.
free_port() {
  python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()'
}

json_field() {
  local file=$1 expr_jq=$2 expr_py=$3
  if command -v python3 >/dev/null 2>&1; then
    python3 -c "import json,sys; print($expr_py)" < "$file"
  elif command -v jq >/dev/null 2>&1; then
    jq -r "$expr_jq" "$file"
  else
    fail "neither python3 nor jq is available"
  fi
}
# `from` and `to` decode the nested principal object and the stored JSON string,
# mirroring `row_principal` in `crates/onlyne-cli/src/ledger.rs`.
ledger_table() {
  local file=$1 out=$2
  python3 - "$file" > "$out" <<'PY'
import json
import sys


def rows_of(raw):
    if isinstance(raw, dict) and isinstance(raw.get("data"), (dict, list)):
        raw = raw["data"]
    if isinstance(raw, list):
        return raw
    if isinstance(raw, dict):
        for key in ("rows", "results", "ledger", "items"):
            if isinstance(raw.get(key), list):
                return raw[key]
        return [raw]
    return []


def role_of(row, key):
    raw = row.get(key)
    if isinstance(raw, str):
        try:
            raw = json.loads(raw)
        except ValueError:
            return ""
    role = raw.get("role") if isinstance(raw, dict) else None
    if isinstance(role, dict):
        role = role.get("role")
    return role if isinstance(role, str) else ""


def text_of(value):
    if value is None:
        return ""
    if isinstance(value, str):
        return value
    return json.dumps(value)


with open(sys.argv[1]) as handle:
    rows = rows_of(json.load(handle))
for row in rows:
    if not isinstance(row, dict):
        continue
    print("\t".join([
        role_of(row, "from"),
        role_of(row, "to"),
        text_of(row.get("state")),
        text_of(row.get("body_json")),
        text_of(row.get("out_head")),
    ]))
PY
}

# `data_rows <answer-file>` prints one JSON object per line for every row in a
# list answer, descending through the `data` envelope and the list keys the CLI
# accepts (`rows`, `results`, `ledger`, `items`). A predicate script reads this
# stream instead of guessing the answer shape again.
data_rows() {
  python3 - "$1" <<'PY'
import json
import sys


def rows_of(raw):
    if isinstance(raw, dict) and isinstance(raw.get("data"), (dict, list)):
        raw = raw["data"]
    if isinstance(raw, list):
        return raw
    if isinstance(raw, dict):
        for key in ("rows", "results", "ledger", "items", "roles", "sessions", "faults"):
            if isinstance(raw.get(key), list):
                return raw[key]
        return [raw]
    return []


with open(sys.argv[1]) as handle:
    for row in rows_of(json.load(handle)):
        if isinstance(row, dict):
            print(json.dumps(row))
PY
}

# `db_count <db-file> <sql>` prints one integer from a SQLite query. sqlite3 is
# the primary reader, python3's stdlib module is the fallback, and a host with
# neither fails loudly, so a missing reader can never turn an assertion silent.
db_count() {
  local db=$1 sql=$2
  if command -v sqlite3 >/dev/null 2>&1; then
    sqlite3 "$db" "$sql"
  elif command -v python3 >/dev/null 2>&1; then
    python3 -c 'import sqlite3,sys; print(sqlite3.connect(sys.argv[1]).execute(sys.argv[2]).fetchone()[0])' "$db" "$sql"
  else
    fail "neither sqlite3 nor python3 is available to read $db"
  fi
}

# `rows_any <answer-file> <field> <value>` exits 0 when any row of a list answer
# carries `field` equal to `value`. `data_rows` normalises the shipped
# `{"ok":true,"data":{"ledger":[...]}}` envelope, so a predicate reads one shape.
rows_any() {
  data_rows "$1" | python3 -c '
import json, sys
field, want = sys.argv[1], sys.argv[2]
try:
    rows = [json.loads(line) for line in sys.stdin]
except ValueError:
    sys.exit(1)
sys.exit(0 if any(row.get(field) == want for row in rows) else 1)
' "$2" "$3"
}

# `row_value <answer-file> <field> [<select-field> <select-value>]` prints `field`
# from the first row whose `select-field` equals `select-value`, and from the first
# row when no selector is given. Prints nothing when no row matches.
row_value() {
  data_rows "$1" | python3 -c '
import json, sys
field = sys.argv[1]
select_field = sys.argv[2] if len(sys.argv) > 2 else ""
select_value = sys.argv[3] if len(sys.argv) > 3 else ""
try:
    rows = [json.loads(line) for line in sys.stdin]
except ValueError:
    rows = []
if select_field:
    rows = [row for row in rows if row.get(select_field) == select_value]
print(rows[0].get(field, "") if rows else "")
' "$2" "${3:-}" "${4:-}"
}

# `client_init <ws> <role> <server_dir> <spec> <fragment> [prose] [acl]` writes
# one role workspace and appends its `[[client]]` fragment to a spec file. The
# plan's acceptance list fixes the fragment shape: first line exactly `[[client]]`
# and one `key = "ed25519/` entry.
#
# `prose` travels through init's `--prose` flag, so the printed entry carries the
# text the fake agent checks against its `assign.prose`. `acl` carries the role's
# two ACL lines as they appear in `spec.toml`, so one grep over the suite shows
# every pair, e.g. 'allowed_senders = ["*", "planner"]
# allowed_targets = ["planner"]'. init prints the rule's default self pair, and a
# caller passing its own lines has the fragment's two ACL lines dropped first.
client_init() {
  local ws=$1 role=$2 server_dir=$3 spec=$4 fragment=$5 prose=${6:-} acl=${7:-}
  if [ -n "$prose" ]; then
    "$CLIENT" init --workspace "$ws" --role "$role" --server-root "$server_dir" --prose "$prose" > "$fragment"
  else
    "$CLIENT" init --workspace "$ws" --role "$role" --server-root "$server_dir" > "$fragment"
  fi
  local first_line
  first_line=$(head -n 1 "$fragment")
  [ "$first_line" = "[[client]]" ] || fail "init fragment first line must be [[client]]" "$first_line"
  grep -q 'key = "ed25519/' "$fragment" || fail "init fragment must contain key = \"ed25519/" "$(cat "$fragment")"
  if [ -n "$acl" ]; then
    # The override restates the lines it carries, so init's own copies of those
    # lines are dropped first and the spec keeps one key each.
    local strip='^(allowed_senders|allowed_targets) = '
    case "$acl" in
      *max_sessions*) strip='^(allowed_senders|allowed_targets|max_sessions) = ' ;;
    esac
    grep -v -E "$strip" "$fragment" >> "$spec"
    printf '%s\n' "$acl" >> "$spec"
  else
    cat "$fragment" >> "$spec"
  fi
}

# `mint_key <ws> <server_dir> <fragment>` mints one local ed25519 key through the
# client init path and prints the `ed25519/` string, so a `[[gateway]]` entry or a
# hand-written role table carries a key without taking init's printed entry.
# Needs CLIENT set.
mint_key() {
  local ws=$1 server_dir=$2 fragment=$3
  "$CLIENT" init --workspace "$ws" --role "$(basename "$ws")" --server-root "$server_dir" > "$fragment"
  grep -o 'ed25519/[A-Za-z0-9+/=]*' "$fragment" | head -n 1
}

# `setup_cluster <server_dir> <ws_dir> [role] [tag] [listen] [prose] [acl]` brings
# up one cluster and registers one role into it through `client_init`, so the
# role's ACL pair and prose arrive in init's printed entry. Sets SERVER, CLIENT,
# ONLYNE, FAKE and cluster_server_pid.
setup_cluster() {
  local server_dir=$1 ws_dir=$2 role=${3:-planner} tag=${4:-cluster} listen=${5:-} prose=${6:-} acl=${7:-}
  [ -n "$listen" ] || listen="127.0.0.1:$(free_port)"
  SERVER=$(bin onlyne-server)
  CLIENT=$(bin onlyne-client)
  ONLYNE=$(bin onlyne)
  FAKE=$(bin onlyne-agent-fake)
  "$SERVER" init --root "$server_dir" --listen "$listen"
  "$SERVER" run --root "$server_dir" >"$tmp/$tag-server.log" 2>&1 &
  cluster_server_pid=$!
  local ready_out=""
  for _ in $(seq 1 100); do
    if ready_out=$("$ONLYNE" --server-root "$server_dir" wait-ready 2>"$tmp/$tag-wait-ready.err"); then
      break
    fi
    sleep 0.1
  done
  if [ -z "$ready_out" ]; then
    # A daemon that never came up still owns its address until it exits, so the
    # failure path reaps it before the case reports.
    kill "$cluster_server_pid" 2>/dev/null || true
    fail "$tag wait-ready never exited 0 inside 10 seconds" "$(cat "$tmp/$tag-wait-ready.err" 2>/dev/null) $(cat "$tmp/$tag-server.log" 2>/dev/null)"
  fi
  client_init "$ws_dir" "$role" "$server_dir" "$server_dir/.onlyne/spec.toml" "$tmp/$tag-spec.frag.toml" "$prose" "$acl"
  "$ONLYNE" --server-root "$server_dir" reload
}

# `drain_pid <pid> [ticks]` asks one process to leave and bounds how long it has:
# SIGTERM, up to `ticks` tenths of a second, then SIGKILL, then reap. An empty or
# already-gone pid is a no-op, so a cleanup can call it unconditionally. The
# default window is the one `orca-live.sh` proved for a client's own drain.
drain_pid() {
  local pid=$1 ticks=${2:-100}
  [ -n "$pid" ] || return 0
  kill -0 "$pid" 2>/dev/null || return 0
  kill -TERM "$pid" 2>/dev/null || true
  for _ in $(seq 1 "$ticks"); do
    kill -0 "$pid" 2>/dev/null || break
    sleep 0.1
  done
  kill -KILL "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
}

# `close_orca_tab <handle> [pane_key]` closes one Orca tab and verifies it is
# gone, returning non-zero while the tab is still listed.
#
# The flag is `--terminal` while the field `terminal list` reports is `handle`,
# so `--handle` reads right and is refused (measured on 1.4.198: "Unknown flag
# --handle for command: terminal close"). A cleanup that swallows that refusal
# leaves the tab in place and says nothing, which is how one probe left sixteen
# tabs behind. So the verdict here is the listing rather than the exit code,
# which also makes a close that raced a drain read as success. Every case that
# closes a tab goes through this, so the flag is spelled in one place only.
close_orca_tab() {
  local handle=$1 pane_key=${2:-} code=0
  orca terminal close --terminal "$handle" --json >/dev/null 2>&1 || true
  orca terminal list --json 2>/dev/null | python3 -c '
import json, sys

handle, pane_key = sys.argv[1], sys.argv[2]
try:
    rows = (json.load(sys.stdin).get("result") or {}).get("terminals") or []
except ValueError:
    sys.exit(2)


def pane_of(row):
    # Rows carry tabId/leafId but no paneKey on 1.4.198, so the key is the same
    # `tabId:leafId` pair the session backend synthesizes.
    return row.get("paneKey") or "%s:%s" % (row.get("tabId"), row.get("leafId"))


for row in rows:
    if row.get("handle") == handle or (pane_key and pane_of(row) == pane_key):
        sys.exit(1)
sys.exit(0)
' "$handle" "$pane_key" || code=$?
  case $code in
    0) return 0 ;;
    1) printf 'close_orca_tab: pane %s is still listed after closing %s\n' "${pane_key:-$handle}" "$handle" >&2 ;;
    *) printf 'close_orca_tab: the tab listing could not be read while closing %s\n' "$handle" >&2 ;;
  esac
  return 1
}
