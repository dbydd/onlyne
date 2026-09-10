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

# `ledger_table <answer-file> <out-file>` writes one tab-separated line per ledger
# row: `<from role>\t<to role>\t<state>\t<body_json>\t<out_head>`.
# A `ledger --task` answer arrives as one row and a bare `ledger` answer arrives
# as a list under one of the keys the CLI accepts, so both shapes normalise here.
# `from` and `to` decode the nested principal object and the stored JSON string,
# mirroring `row_principal` in `crates/onlyne-cli/src/ledger.rs`.
ledger_table() {
  local file=$1 out=$2
  python3 - "$file" > "$out" <<'PY'
import json
import sys


def rows_of(raw):
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

# `client_init <ws> <role> <server_dir> <spec> <fragment> [prose] [senders] [targets]`
# writes one role workspace and appends its `[[client]]` fragment to a spec file.
# The plan's acceptance list fixes the fragment shape: first line exactly
# `[[client]]` and one `key = "ed25519/` entry.
#
# `prose` travels through init's `--prose` flag, so the printed entry carries the
# text the fake agent checks against its `assign.prose`. `senders` and `targets`
# are TOML array literals, e.g. '["*", "planner"]'. init prints the rule's default
# self pair, `["*", <role>]` and `[<role>]`, so a caller passing a different pair
# has the fragment's two ACL lines dropped first.
client_init() {
  local ws=$1 role=$2 server_dir=$3 spec=$4 fragment=$5 prose=${6:-} senders=${7:-} targets=${8:-}
  if [ -n "$prose" ]; then
    "$CLIENT" init --workspace "$ws" --role "$role" --server-root "$server_dir" --prose "$prose" > "$fragment"
  else
    "$CLIENT" init --workspace "$ws" --role "$role" --server-root "$server_dir" > "$fragment"
  fi
  local first_line
  first_line=$(head -n 1 "$fragment")
  [ "$first_line" = "[[client]]" ] || fail "init fragment first line must be [[client]]" "$first_line"
  grep -q 'key = "ed25519/' "$fragment" || fail "init fragment must contain key = \"ed25519/" "$(cat "$fragment")"
  if [ -n "$senders" ] && [ -n "$targets" ]; then
    grep -v -E '^(allowed_senders|allowed_targets) = ' "$fragment" >> "$spec"
    printf 'allowed_senders = %s\nallowed_targets = %s\n' "$senders" "$targets" >> "$spec"
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

# `setup_cluster <server_dir> <ws_dir> [role] [tag] [listen] [prose] [senders] [targets]`
# brings up one cluster and registers one role into it through `client_init`, so
# the role's ACL pair and prose arrive in init's printed entry. Sets SERVER,
# CLIENT, ONLYNE, FAKE and cluster_server_pid.
setup_cluster() {
  local server_dir=$1 ws_dir=$2 role=${3:-planner} tag=${4:-cluster} listen=${5:-127.0.0.1:7899} prose=${6:-} senders=${7:-} targets=${8:-}
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
  [ -n "$ready_out" ] || fail "$tag wait-ready never exited 0 inside 10 seconds" "$(cat "$tmp/$tag-wait-ready.err" 2>/dev/null)"
  client_init "$ws_dir" "$role" "$server_dir" "$server_dir/.onlyne/spec.toml" "$tmp/$tag-spec.frag.toml" "$prose" "$senders" "$targets"
  "$ONLYNE" --server-root "$server_dir" reload
}
