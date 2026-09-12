#!/usr/bin/env bash
set -euo pipefail
# Verification case 7 (docs/v1-PLAN.md line 506): the pre-v1 workspace layout is refused.
# Legacy shape built by hand from crates/onlyne-layout/src/lib.rs detect_legacy_with_probe:
# a channels/ directory or a state.db carrying io_cursors / loopback_idempotency marks a pre-v1 workspace.
SRC=$(pwd)
tmp=$(mktemp -d)
cleanup() { rm -rf "$tmp"; }
trap cleanup EXIT
. "$SRC/crates/onlyne-testkit/e2e/lib.sh"

client=$(bin onlyne-client)
dir="$tmp/legacy"
mkdir -p "$dir/.onlyne/channels"
printf 'io_cursors' > "$dir/.onlyne/state.db"
before=$(cd "$tmp" && find legacy | sort)
# `--role` and `--server-root` satisfy the clap contract; the legacy check runs
# before the server spec is read, so the dummy root is never touched.
set +e
out=$("$client" init --workspace "$dir" --role planner --server-root "$tmp/no-server" 2>"$tmp/stderr.txt")
code=$?
set -e
[ "$code" = "2" ] || fail "init must exit 2 on legacy layout, got $code" "$out$(cat "$tmp/stderr.txt")"
[ "$(cat "$tmp/stderr.txt")" = "onlyne: legacy workspace layout; v1.0.0 does not migrate" ] || fail "stderr must be byte-exact legacy refusal" "$(cat "$tmp/stderr.txt")"
after=$(cd "$tmp" && find legacy | sort)
[ "$before" = "$after" ] || fail "no file may be created on legacy refusal" "$(diff <(printf '%s\n' "$before") <(printf '%s\n' "$after"))"
echo "PASS legacy-layout"
