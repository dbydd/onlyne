#!/usr/bin/env bash
set -euo pipefail
# Verification case 5 (docs/v1-PLAN.md line 504): two-cluster federation, the recursion proof.
# A parent cluster hosts `planner`, a child cluster hosts `builder`, and the child
# supervisor's client joins the parent as the aggregate role `cluster-b` with the
# key the parent spec registers (S11 line 460). The parent ledger must settle
# `acked` under that aggregate name and must never carry a child role name or the
# child prose.
# No real platform credential or window manager is touched: ONLYNE_BACKEND=fake plus the fake gateway only.
SRC=$(pwd)
tmp=$(mktemp -d)
pids=""
track() { pids="$pids $1"; }
cleanup() {
  local pid
  # Intentional word splitting: `pids` is a space-separated pid list.
  for pid in $pids; do
    kill "$pid" 2>/dev/null || true
  done
  for pid in $pids; do
    wait "$pid" 2>/dev/null || true
  done
  rm -rf "$tmp"
}
trap cleanup EXIT
. "$SRC/crates/onlyne-testkit/e2e/lib.sh"

PARENT_PROSE="$E2E_PROSE"
AGG_PROSE='aggregate prose: cluster-b forwards one parent round trip'
CHILD_PROSE='child prose: builder-only marker 7f3a'

# One fake-agent transcript per role, so each agent asserts the prose its own spec
# entry carries.
agent_script() {
  printf '{"hello":{"capabilities":["register","report","inject","recycle"]},"steps":[{"wait_assign":true},{"assert_prose_equals":"%s"},{"report":"ready"},{"complete":{"outcome":"done","head_from":"assign_body"}},{"echo_prose_to":"prose.log"}]}\n' "$1"
}

# `wait_acked <server_root> <task> <answer_file>` polls one task's ledger rows for
# an `acked` row, bounded to 60 seconds, and leaves the last answer in place.
wait_acked() {
  local root=$1 task=$2 out=$3 attempt
  for attempt in $(seq 1 120); do
    if "$ONLYNE" --server-root "$root" ledger --task "$task" > "$out" 2>/dev/null; then
      ledger_table "$out" "$out.tsv"
      if cut -f3 "$out.tsv" | grep -q '^acked$'; then
        return 0
      fi
    fi
    sleep 0.5
  done
  return 1
}

# --- parent cluster: planner on 127.0.0.1:7899 -------------------------------
# Planner registers through `onlyne-client init --prose "v1 smoke prose"`. Its
# target list stays the bare wildcard, the one deliberate wildcard pin in the
# suite: `["*"]` reaches cluster-b and excludes planner, the role that carries it,
# so the wildcard's self-exclusion is proven by the target side alone while the
# senders list names planner for the traffic the parent accepts from itself.
setup_cluster "$tmp/parent" "$tmp/planner" planner parent "" "$E2E_PROSE" 'allowed_senders = ["*", "planner"]
allowed_targets = ["*"]'
track "$cluster_server_pid"

# --- child cluster: builder on 127.0.0.1:7898 --------------------------------
# Builder's pair is the rule's self pair: `["*", "builder"]` reaches every other
# registered role in the child cluster, and the self name delivers the child
# round trip back to builder.
setup_cluster "$tmp/child" "$tmp/builder" builder child "" "$CHILD_PROSE" 'allowed_senders = ["*", "builder"]
allowed_targets = ["builder"]'
track "$cluster_server_pid"

# --- the child supervisor joins the parent as aggregate role cluster-b -------
# cluster-b names itself in both lists, because the supervisor posts its own
# completions through it, and names planner as a target for that completion.
client_init "$tmp/cluster-b" cluster-b "$tmp/parent" "$tmp/parent/.onlyne/spec.toml" "$tmp/cluster-b.frag.toml" "$AGG_PROSE" 'allowed_senders = ["planner", "cluster-b"]
allowed_targets = ["planner", "cluster-b"]'
printf 'aggregate = "cluster-b"\n' >> "$tmp/parent/.onlyne/spec.toml"

# --- fake gateway on the parent, no credential -------------------------------
GW_FAKE=$(bin onlyne-gateway-fake)
gw_key=$(mint_key "$tmp/gw-key" "$tmp/parent" "$tmp/gw-key.frag.toml")
{
  echo '[[gateway]]'
  echo 'id = "fg1"'
  echo 'platform = "fake"'
  printf 'key = "%s"\n' "$gw_key"
  echo 'enabled = true'
} >> "$tmp/parent/.onlyne/spec.toml"

"$ONLYNE" --server-root "$tmp/parent" reload
"$ONLYNE" --server-root "$tmp/child" reload

# A FIFO on stdin holds the gateway open: a pipeline would leave a `tail -f`
# sibling that no pid list can reach, and the case would hang on its reap.
mkfifo "$tmp/gw-in"
"$GW_FAKE" --platform fake --gateway-id fg1 --socket "$tmp/parent/.onlyne/run/s" <"$tmp/gw-in" >"$tmp/gw-fake.log" 2>&1 &
gw_pid=$!
track "$gw_pid"
# A read-write open on the FIFO never blocks, so the shell holds a writer before
# the gateway process starts reading.
exec 3<>"$tmp/gw-in"

agent_script "$AGG_PROSE" > "$tmp/cluster-b-script.json"
agent_script "$CHILD_PROSE" > "$tmp/builder-script.json"
"$CLIENT" run --workspace "$tmp/cluster-b" >"$tmp/cluster-b-client.log" 2>&1 &
track $!
"$FAKE" --workspace "$tmp/cluster-b" --script "$tmp/cluster-b-script.json" >"$tmp/cluster-b-fake.log" 2>&1 &
track $!
"$CLIENT" run --workspace "$tmp/builder" >"$tmp/builder-client.log" 2>&1 &
track $!
"$FAKE" --workspace "$tmp/builder" --script "$tmp/builder-script.json" >"$tmp/builder-fake.log" 2>&1 &
track $!
# The parent round trip closes on the planner side: cluster-b answers the parent
# task with its completion, and that receipt settles only once the role it names
# is online to read it. The planner needs no agent for this leg, because a
# completion is a receipt rather than work (plan §3 line 152).
"$CLIENT" run --workspace "$tmp/planner" >"$tmp/planner-client.log" 2>&1 &
track $!

# The parent still serves the gateway surface while an aggregate role is attached.
gw_out=""
for _ in $(seq 1 100); do
  gw_out=$("$ONLYNE" --server-root "$tmp/parent" status 2>/dev/null) || gw_out=''
  if echo "$gw_out" | grep -q '"fg1"'; then
    break
  fi
  sleep 0.1
done
echo "$gw_out" | grep -q '"fg1"' || fail "parent status must report gateway id fg1" "$gw_out"

# --- child cluster round trip: builder works inside its own cluster ----------
child_send=$("$ONLYNE" --server-root "$tmp/child" send --from builder --to builder --text "B1 builder round trip") || fail "child send failed" "$child_send"
printf '%s\n' "$child_send" > "$tmp/child-send.json"
child_task=$(json_field "$tmp/child-send.json" '.data.task' 'json.load(sys.stdin)["data"]["task"]')
wait_acked "$tmp/child" "$child_task" "$tmp/child-task.json" || fail "child ledger never reached acked" "$(cat "$tmp/child-task.json" 2>/dev/null) fake=$(cat "$tmp/builder-fake.log" 2>/dev/null)"
cut -f4,5 "$tmp/child-task.json.tsv" > "$tmp/child-text.txt"
# The child leg is the positive control for the negative greps below: the child
# ledger does carry its own role name and its own prose-free text.
grep -q 'builder' "$tmp/child-text.txt" || fail "child ledger must name its own builder role" "$(cat "$tmp/child-text.txt")"

# --- parent round trip through the aggregate role ----------------------------
parent_send=$("$ONLYNE" --server-root "$tmp/parent" send --from planner --to cluster-b --text "P1 round trip") || fail "parent send failed" "$parent_send"
printf '%s\n' "$parent_send" > "$tmp/parent-send.json"
[ "$(json_field "$tmp/parent-send.json" '.ok' 'json.load(sys.stdin)["ok"]')" = "True" ] || [ "$(json_field "$tmp/parent-send.json" '.ok' 'json.load(sys.stdin)["ok"]')" = "true" ] || fail "parent send ok must be true" "$parent_send"
parent_task=$(json_field "$tmp/parent-send.json" '.data.task' 'json.load(sys.stdin)["data"]["task"]')
wait_acked "$tmp/parent" "$parent_task" "$tmp/parent-task.json" || fail "parent ledger never reached acked" "$(cat "$tmp/parent-task.json" 2>/dev/null) fake=$(cat "$tmp/cluster-b-fake.log" 2>/dev/null)"
# The round trip settles in two rows: the task planner sent and the completion
# cluster-b answers with. `wait_acked` returns on the first of them, so this case
# polls for the aggregate's own ack before reading the table.
for _ in $(seq 1 120); do
  "$ONLYNE" --server-root "$tmp/parent" ledger --task "$parent_task" > "$tmp/parent-task.json" 2>/dev/null || true
  ledger_table "$tmp/parent-task.json" "$tmp/parent-task.json.tsv"
  if cut -f1,3 "$tmp/parent-task.json.tsv" | grep -F -x -q -e "$(printf 'cluster-b\tacked')"; then
    break
  fi
  sleep 0.25
done
cut -f1,3 "$tmp/parent-task.json.tsv" | grep -F -x -q -e "$(printf 'cluster-b\tacked')" || fail "parent ledger must settle acked with from.role = cluster-b" "$(cat "$tmp/parent-task.json.tsv")"

# --- parent ledger holds parent-visible roles only ---------------------------
"$ONLYNE" --server-root "$tmp/parent" ledger > "$tmp/parent-all.json" 2>/dev/null || fail "parent ledger query failed" "$(cat "$tmp/parent-all.json" 2>/dev/null)"
ledger_table "$tmp/parent-all.json" "$tmp/parent-all.tsv"
cut -f1 "$tmp/parent-all.tsv" | sort -u > "$tmp/parent-senders.txt"
cut -f2 "$tmp/parent-all.tsv" | sort -u > "$tmp/parent-targets.txt"
if grep -qv -E '^(planner|cluster-b)$' "$tmp/parent-senders.txt"; then
  fail "parent ledger sender must be a parent-visible role" "$(cat "$tmp/parent-senders.txt")"
fi
if grep -qv -E '^(planner|cluster-b)$' "$tmp/parent-targets.txt"; then
  fail "parent ledger target must be a parent-visible role" "$(cat "$tmp/parent-targets.txt")"
fi

# --- the two negative conditions, with the parent text as the control --------
cut -f4 "$tmp/parent-all.tsv" > "$tmp/parent-body.txt"
cut -f5 "$tmp/parent-all.tsv" > "$tmp/parent-out-head.txt"
grep -q 'P1 round trip' "$tmp/parent-body.txt" || fail "parent ledger body_json must carry the parent round trip" "$(cat "$tmp/parent-body.txt")"
grep -q 'P1 round trip' "$tmp/parent-out-head.txt" || fail "parent ledger out_head must carry the parent round trip" "$(cat "$tmp/parent-out-head.txt")"
if grep -q 'builder' "$tmp/parent-body.txt"; then
  fail "no parent ledger body_json may name a child role" "$(cat "$tmp/parent-body.txt")"
fi
if grep -q 'builder' "$tmp/parent-out-head.txt"; then
  fail "no parent ledger out_head may name a child role" "$(cat "$tmp/parent-out-head.txt")"
fi
if grep -q -F -e "$CHILD_PROSE" "$tmp/parent-body.txt" "$tmp/parent-out-head.txt"; then
  fail "parent ledger must not carry child prose" "$(cat "$tmp/parent-body.txt" "$tmp/parent-out-head.txt")"
fi
echo "PASS two-cluster"
