#!/usr/bin/env bash
set -euo pipefail
# Verification case 9 (docs/v1-PLAN.md lines 508-518, D20): `generate` renders a role
# workspace, the workspace survives a move to another absolute path, and the moved
# copy still reaches the server. No real platform credential or window manager is
# touched: ONLYNE_BACKEND=fake plus the fake gateway only.
SRC=$(pwd)
tmp=$(mktemp -d)
server_pid=""
client_pid=""
fake_pid=""
cleanup() {
  # `setup_cluster` starts the server before it can fail, so its pid must be in
  # the kill list even when the assignment below never ran.
  kill "$server_pid" "$client_pid" "$fake_pid" "${cluster_server_pid:-}" 2>/dev/null || true
  wait "$server_pid" "$client_pid" "$fake_pid" "${cluster_server_pid:-}" 2>/dev/null || true
  rm -rf "$tmp"
}
trap cleanup EXIT
. "$SRC/crates/onlyne-testkit/e2e/lib.sh"

setup_cluster "$tmp/server" "$tmp/planner" planner cluster "" "$E2E_PROSE" 'allowed_senders = ["*", "planner"]
allowed_targets = ["planner", "builder"]'
server_pid=$cluster_server_pid

# Plan §11: `generate` reads templates from `<server-root>/.onlyne/templates`, so
# the shipped example tree is the source the case renders from. The builder seed
# entry exists because `generate --role builder` renders roles the spec names, and
# its key is a placeholder: the fragment the command prints carries the identity
# the role really connects with, and the seed block leaves the file before that
# fragment lands (D13, plan line 395).
cp -R "$SRC/.onlyne.example/templates/." "$tmp/server/.onlyne/templates/"
spec="$tmp/server/.onlyne/spec.toml"
seed_key=$(mint_key "$tmp/builder-seed" "$tmp/server" "$tmp/builder-seed.frag.toml")
seed_bytes=$(wc -c < "$spec" | tr -d ' ')
{
  echo '[[client]]'
  echo 'role = "builder"'
  printf 'key = "%s"\n' "$seed_key"
  printf 'prose = "%s"\n' "$E2E_PROSE"
  echo 'allowed_senders = ["*", "builder"]'
  echo 'allowed_targets = ["planner", "builder"]'
} >> "$spec"
"$ONLYNE" --server-root "$tmp/server" reload >/dev/null

gen_out=$("$ONLYNE" --server-root "$tmp/server" generate --role builder --out "$tmp/gen") || fail "generate failed" "$gen_out"
printf '%s\n' "$gen_out" > "$tmp/frag.toml"
[ "$(sed -n '1p' "$tmp/frag.toml")" = "[[client]]" ] || fail "generate stdout must open with [[client]]" "$(cat "$tmp/frag.toml")"
gen_key=$(python3 -c 'import re, sys
for line in sys.stdin:
    found = re.match(r"key = \"([^\"]+)\"", line)
    if found:
        print(found.group(1))
        break' < "$tmp/frag.toml")

# The generated workspace is the product of this step, so the case reads the tree
# itself: the role's config lands under the topology directory the template tree
# named, and a run directory at generation time would be runtime state the plan
# keeps out of the output (D20, plan line 391).
ws="$tmp/gen/dev/builder"
[ -f "$ws/.onlyne/config.toml" ] || fail "generate must write <out>/dev/builder/.onlyne/config.toml" "$(ls -R "$tmp/gen" 2>/dev/null)"
[ -f "$ws/.onlyne/keys/role.key" ] || fail "generate must write the role key" "$(ls -R "$ws/.onlyne" 2>/dev/null)"
[ ! -e "$ws/.onlyne/run/s" ] || fail "generate must not create run/s" "$(ls -R "$ws/.onlyne" 2>/dev/null)"
[ -f "$tmp/gen/.onlyne-generation.json" ] || fail "generate must write the generation manifest" "$(ls -a "$tmp/gen")"

# The seed entry's placeholder key leaves the spec, and the fragment the command
# printed replaces it, which is the supervisor's half of D13.
truncate -s "$seed_bytes" "$spec"
cat "$tmp/frag.toml" >> "$spec"
"$ONLYNE" --server-root "$tmp/server" reload >/dev/null

# --- the move (plan line 513): another absolute path, no edit inside the tree ---
mkdir -p "$tmp/elsewhere"
mv "$ws" "$tmp/elsewhere/b1"
# Plan line 391 bans two prefixes from the output: the `--out` tree and the
# server root, so the case scans for both after the move.
for prefix in "$tmp/gen" "$tmp/server"; do
  [ -z "$(command grep -rl "$prefix" "$tmp/elsewhere/b1" 2>/dev/null || true)" ] || fail "the moved workspace must carry no generation-time absolute path" "prefix=$prefix $(command grep -rl "$prefix" "$tmp/elsewhere/b1" 2>/dev/null || true)"
done

# The generated key reaches the server, because the fragment the command printed
# is what the spec now holds.
[ -n "$gen_key" ] || fail "the printed fragment must carry a key line" "$(cat "$tmp/frag.toml")"
key_registered=$(KEY="$gen_key" python3 -c 'import os, sys
print(sum(1 for line in sys.stdin if line.strip() == "key = \"" + os.environ["KEY"] + "\""))' < "$spec")
[ "$key_registered" = "1" ] || fail "the generated key must be the one the spec registers" "$(cat "$spec")"

"$CLIENT" run --workspace "$tmp/elsewhere/b1" >"$tmp/client.log" 2>&1 &
client_pid=$!
"$FAKE" --workspace "$tmp/elsewhere/b1" --script "$SRC/crates/onlyne-testkit/scripts/echo-complete.json" >"$tmp/fake.log" 2>&1 &
fake_pid=$!

registered=false
for _ in $(seq 1 100); do
  roles_out=$("$ONLYNE" --server-root "$tmp/server" roles 2>/dev/null) || true
  printf '%s\n' "$roles_out" > "$tmp/roles.json"
  if rows_any "$tmp/roles.json" state online 2>/dev/null; then
    registered=true
    break
  fi
  sleep 0.1
done
[ "$registered" = "true" ] || fail "the relocated role must register" "roles=$roles_out client=$(cat "$tmp/client.log" 2>/dev/null)"

send_out=$("$ONLYNE" --server-root "$tmp/server" send --from planner --to builder --text "relocated") || fail "send failed" "$send_out"
printf '%s\n' "$send_out" > "$tmp/send.json"
task=$(json_field "$tmp/send.json" '.data.task' 'json.load(sys.stdin)["data"]["task"]')
[ -n "$task" ] || fail "the send answer carried no task" "$send_out"

ledger_out=""
for _ in $(seq 1 120); do
  ledger_out=$("$ONLYNE" --server-root "$tmp/server" ledger --task "$task" 2>/dev/null) || true
  printf '%s\n' "$ledger_out" > "$tmp/ledger.json"
  if rows_any "$tmp/ledger.json" state acked 2>/dev/null; then
    break
  fi
  sleep 0.5
done
rows_any "$tmp/ledger.json" state acked || fail "the relocated role must ack its task" "ledger=$ledger_out fake=$(cat "$tmp/fake.log" 2>/dev/null)"

# Plan line 518: `--force` replaces generated files and leaves the runtime paths
# alone, and the reuse rule in `generate` keys on the stored role key, so the
# identity the moved workspace already carries decides the second render.
db="$tmp/elsewhere/b1/.onlyne/client.db"
mkdir -p "$(dirname "$db")"
printf 'sentinel\n' > "$db"
before=$(stat -f '%m' "$db")
sleep 1
"$ONLYNE" --server-root "$tmp/server" generate --role builder --out "$tmp/gen" --force >/dev/null || fail "the forced rerun failed" ""
[ -f "$tmp/elsewhere/b1/.onlyne/client.db" ] || fail "--force must keep the client database" "$(ls -a "$tmp/elsewhere/b1/.onlyne")"
[ "$before" = "$(stat -f '%m' "$db")" ] || fail "--force must not touch the client database" "before=$before after=$(stat -f '%m' "$db")"
[ ! -e "$tmp/gen/dev/builder/.onlyne/run/s" ] || fail "--force must not create run/s" "$(ls -R "$tmp/gen/dev/builder/.onlyne")"
echo "PASS generate-relocate"
