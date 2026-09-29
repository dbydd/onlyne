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
[ "$code" = "6" ] || fail "init must exit 6 on a workspace from an older layout, got $code" "$out$(cat "$tmp/stderr.txt")"
# The refusal has to name the marker that decided it and the way forward; the
# exact wording is not the contract, so it is checked for those two facts rather
# than frozen. A v1 sentence naming a product version told the reader nothing
# they could act on.
grep -q "channels directory" "$tmp/stderr.txt" || fail "the refusal must name the marker" "$(cat "$tmp/stderr.txt")"
grep -q "onlyne-client init" "$tmp/stderr.txt" || fail "the refusal must name the remedy" "$(cat "$tmp/stderr.txt")"
after=$(cd "$tmp" && find legacy | sort)
[ "$before" = "$after" ] || fail "no file may be created on legacy refusal" "$(diff <(printf '%s\n' "$before") <(printf '%s\n' "$after"))"
echo "PASS legacy-layout"
