#!/usr/bin/env bash
set -euo pipefail
# Verification case 12: the running-lights example.
#
# Six roles in one closed ring, each admitting only its two neighbours, and one
# token that travels the ring twice: `light1` gets it, passes it to `light2` with
# `onlyne handoff`, and settles its own task, so the light moves and the ledger
# records a chain of twelve tasks with hops 0..11. The last hop spends the chain
# budget (`max_hop`), keeps the task, and answers `done`.
#
# Nothing here is a model, a network peer, or the Orca app: one server, six
# fake-backend clients, and six scripted agents, all on the loopback socket.
SRC=$(pwd)
tmp=$(mktemp -d)
pids=""
track() { pids="$pids $1"; }
cleanup() {
  local status=$? pid
  # `drain_pid` is the suite's verified reap: SIGTERM, bounded wait, SIGKILL.
  # A pid that is already gone is a no-op, so the server is listed twice when
  # `setup_cluster` did not get far enough to return.
  for pid in $pids "${cluster_server_pid:-}"; do
    drain_pid "$pid"
  done
  # A failing case is diagnosed from its artifacts: a passing run throws the
  # scratch directory away, a failing one keeps it and says where it is.
  # `RUNNING_LIGHTS_KEEP=1` forces it either way.
  if [ "$status" -eq 0 ] && [ "${RUNNING_LIGHTS_KEEP:-0}" != 1 ]; then
    rm -rf "$tmp"
  else
    echo "running-lights: scratch directory kept at $tmp" >&2
  fi
}
trap cleanup EXIT
. "$SRC/crates/onlyne-testkit/e2e/lib.sh"

LIGHTS_PROSE='running lights'
SCRIPT="$SRC/crates/onlyne-testkit/scripts/running-light.json"
TUI=$(bin onlyne-tui)
# The chain's length is the script's own hop budget: the agent that meets a task
# at hop `max_hop` keeps it instead of passing it on, so a budget of `max_hop`
# leaves `max_hop + 1` tasks behind. Reading it here keeps one number, in the
# script, as the only place the ring's length is written down.
MAX_HOP=$(python3 - "$SCRIPT" <<'PY'
import json
import sys

steps = json.load(open(sys.argv[1]))["steps"]
print(next(step["handoff"]["max_hop"] for step in steps if "handoff" in step))
PY
)

ring_prev() { if [ "$1" = 1 ]; then echo 6; else echo $(( $1 - 1 )); fi; }
ring_next() { if [ "$1" = 6 ]; then echo 1; else echo $(( $1 + 1 )); fi; }

# `light_edge <role>` prints the one in-flight edge that ends on `role`, the
# edge the TUI paints as active while that role holds the token.
light_edge() { printf '%s>%s\n' "light$(ring_prev "${1#light}")" "$1"; }

# --- the ring's ACL ---------------------------------------------------------
# Every role admits its two neighbours. `allowed_targets` carries the forward
# edge the token takes and the backward edge its completion returns on; the
# matching `allowed_senders` makes the pair mutual, which is what `Spec::acl_edges`
# requires. No role names a third, so the ring has no chord to shortcut.
acl_for() {
  printf 'allowed_senders = ["light%s", "light%s"]\nallowed_targets = ["light%s", "light%s"]\n' \
    "$(ring_prev "$1")" "$(ring_next "$1")" "$(ring_next "$1")" "$(ring_prev "$1")"
}

setup_cluster "$tmp/server" "$tmp/light1" light1 lights "" "$LIGHTS_PROSE" "$(acl_for 1)"
track "$cluster_server_pid"
for i in 2 3 4 5 6; do
  client_init "$tmp/light$i" "light$i" "$tmp/server" "$tmp/server/.onlyne/spec.toml" \
    "$tmp/light$i.frag.toml" "$LIGHTS_PROSE" "$(acl_for "$i")"
done
"$ONLYNE" --server-root "$tmp/server" reload

for i in 1 2 3 4 5 6; do
  "$CLIENT" run --workspace "$tmp/light$i" >"$tmp/light$i-client.log" 2>&1 &
  track $!
done
for i in 1 2 3 4 5 6; do
  # `{next_role}` is the one value the script cannot name itself, so it arrives
  # in the spawn environment: each agent knows the ring only through it.
  ONLYNE_NEXT_ROLE="light$(ring_next "$i")" \
    "$FAKE" --workspace "$tmp/light$i" --script "$SCRIPT" >"$tmp/light$i-fake.log" 2>&1 &
  track $!
done

online=0
for _ in $(seq 1 200); do
  "$ONLYNE" --server-root "$tmp/server" roles > "$tmp/roles.json" 2>/dev/null || true
  online=$(json_field "$tmp/roles.json" x \
    'sum(1 for r in json.load(sys.stdin)["data"]["roles"] if r["state"] == "online")' 2>/dev/null || echo 0)
  if [ "$online" = 6 ]; then break; fi
  sleep 0.1
done
[ "$online" = 6 ] || fail "all six light roles must come online" "$(cat "$tmp/roles.json" 2>/dev/null)"

# The ring is closed: each role reaches its two neighbours and nothing else.
python3 - "$tmp/roles.json" <<'PY' || fail "the spec's ring edges are not the two neighbours"
import json, sys

roles = {row["name"]: sorted(row["edges"]) for row in json.load(open(sys.argv[1]))["data"]["roles"]}
for index in range(1, 7):
    name = "light%d" % index
    want = sorted(["light%d" % (index % 6 + 1), "light%d" % ((index + 4) % 6 + 1)])
    assert roles[name] == want, "%s reaches %s, not %s" % (name, roles[name], want)
PY

# A chord inside the ring is refused before the token ever moves: `light1` may
# not address `light4`, and the refusal names the sender half of the pair.
denied=$(ONLYNE_ROLE=light1 "$ONLYNE" --workspace "$tmp/light1" send --to light4 --text "shortcut" 2>"$tmp/denied.err") || true
printf '%s\n' "$denied" > "$tmp/denied.json"
detail="$denied$(cat "$tmp/denied.err")"
[ "$(json_field "$tmp/denied.json" '.ok' 'str(json.load(sys.stdin).get("ok")).lower()' 2>/dev/null || true)" = "false" ] || fail "a chord send must be refused" "$detail"
[ "$(json_field "$tmp/denied.json" '.error.code' 'json.load(sys.stdin).get("error",{}).get("code","")' 2>/dev/null || true)" = "acl_denied" ] || fail "a chord send must answer acl_denied" "$detail"

# --- start the ring ---------------------------------------------------------
# `light6` is both the ring's last hop and the token's origin: the send that
# starts the lights is the edge the wrap-around already allows, so the example
# needs no seventh role, no extra client, and no extra ACL pair.
send_out=$(ONLYNE_ROLE=light6 "$ONLYNE" --workspace "$tmp/light6" send --to light1 --text "running-lights token, hop 0") \
  || fail "the ring's first send failed" "$send_out"
printf '%s\n' "$send_out" > "$tmp/send.json"
[ "$(json_field "$tmp/send.json" '.data.state' 'json.load(sys.stdin)["data"]["state"]')" = "in_flight" ] || fail "the first task must start in_flight" "$send_out"
first_task=$(json_field "$tmp/send.json" '.data.task' 'json.load(sys.stdin)["data"]["task"]')

# --- two frames of the moving light -----------------------------------------
# `working_roles <frame>` names the roles whose node holds a session in the
# working state — that glyph is the light. The star beside a role name cannot
# carry it: a settled session stays warm for reuse, so every role that has
# served a task keeps its star for the rest of the run.
#
# `inflight_edges <answer>` names the from>to pair of every in-flight row, the
# row `onlyne-tui` colours as the active edge. A frame counts as a sighting only
# when both agree on the role, so the case reads the picture and the ledger as
# one fact rather than two.
working_roles() {
  python3 - "$1" <<'PY'
import re
import sys

lines = open(sys.argv[1]).read().splitlines()
# Only a node's title row names roles, and its session rows start in the same
# column, so the box a glyph sits in is the last title starting at or before it.
titles = {}
for line in lines:
    for match in re.finditer(r"light[1-6]", line):
        titles.setdefault(match.start(), match.group(0))
working = set()
for line in lines:
    for match in re.finditer("\u25d0", line):
        owner = None
        for start in sorted(titles):
            if start <= match.start():
                owner = titles[start]
        if owner:
            working.add(owner)
print(",".join(sorted(working)))
PY
}
inflight_edges() {
  python3 - "$1" <<'PY'
import json, sys

def role(principal):
    return principal["role"]["role"]

rows = json.load(open(sys.argv[1]))["data"]["ledger"]
print(",".join(sorted("%s>%s" % (role(r["from"]), role(r["to"])) for r in rows if r["state"] == "in_flight")))
PY
}

capture_light() {
  local want=$1 frame=$2 ledger=$3 attempt found edge
  for attempt in $(seq 1 300); do
    "$ONLYNE" --server-root "$tmp/server" ledger --state in_flight > "$ledger.cand" 2>/dev/null || true
    "$TUI" --server-root "$tmp/server" --once > "$frame.cand" 2>/dev/null || true
    found=$(working_roles "$frame.cand")
    edge=$(inflight_edges "$ledger.cand")
    if [ "$found" = "$want" ] && [ "$edge" = "$(light_edge "$want")" ]; then
      mv "$ledger.cand" "$ledger"
      mv "$frame.cand" "$frame"
      return 0
    fi
    sleep 0.05
  done
  fail "no TUI frame caught the light on $want" "$(cat "$frame.cand" 2>/dev/null)$(cat "$ledger.cand" 2>/dev/null)"
}

capture_light light2 "$tmp/frame-a.txt" "$tmp/frame-a-ledger.json"
capture_light light5 "$tmp/frame-b.txt" "$tmp/frame-b-ledger.json"

# The two sightings are of different lights: another role holds the working
# session, the active edge is another role pair, and the frames differ.
[ "$(working_roles "$tmp/frame-a.txt")" != "$(working_roles "$tmp/frame-b.txt")" ] \
  || fail "the two frames must catch the light on different roles" "$(working_roles "$tmp/frame-a.txt")"
[ "$(inflight_edges "$tmp/frame-a-ledger.json")" != "$(inflight_edges "$tmp/frame-b-ledger.json")" ] \
  || fail "the active edge must move between the two frames" "$(inflight_edges "$tmp/frame-a-ledger.json")"
if cmp -s "$tmp/frame-a.txt" "$tmp/frame-b.txt"; then
  fail "the two TUI frames must differ" "$(cat "$tmp/frame-a.txt")"
fi

# --- the ledger the ring leaves behind --------------------------------------
for _ in $(seq 1 200); do
  "$ONLYNE" --server-root "$tmp/server" ledger > "$tmp/ledger.json" 2>/dev/null || true
  settled=$(json_field "$tmp/ledger.json" x \
    'sum(1 for r in json.load(sys.stdin)["data"]["ledger"] if r["kind"] == "task" and r["state"] == "acked")' 2>/dev/null || echo 0)
  if [ "$settled" = "$((MAX_HOP + 1))" ]; then break; fi
  sleep 0.5
done
[ "$settled" = "$((MAX_HOP + 1))" ] || fail "every task of the chain must settle acked" "acked=$settled $(cat "$tmp/ledger.json" 2>/dev/null)"

python3 - "$tmp/ledger.json" "$first_task" "$MAX_HOP" <<'PY' > "$tmp/chain.txt" || fail "the ledger chain of the ring" "$(cat "$tmp/chain.txt")"
import json, sys

ledger, first_task, max_hop = sys.argv[1], sys.argv[2], int(sys.argv[3])


def role(principal):
    return principal["role"]["role"]


ledger = json.load(open(ledger))["data"]["ledger"]
tasks = sorted((row for row in ledger if row["kind"] == "task"), key=lambda row: row["hop"])

if [row["hop"] for row in tasks] != list(range(max_hop + 1)):
    raise SystemExit("hops are %s, not 0..%d" % ([row["hop"] for row in tasks], max_hop))
if tasks[0]["task"] != first_task:
    raise SystemExit("hop 0 is %s, not the task the ring started with (%s)" % (tasks[0]["task"], first_task))

print("hop  from     to       state  task      parent    head")
for hop, row in enumerate(tasks):
    if row["state"] != "acked":
        raise SystemExit("hop %d is %s, not acked" % (hop, row["state"]))
    # The token walks the ring one role at a time: hop n leaves light(n+5) and
    # lands on light(n), counting the ring's six roles modulo six.
    sender, receiver = "light%d" % ((hop + 5) % 6 + 1), "light%d" % (hop % 6 + 1)
    if role(row["from"]) != sender or role(row["to"]) != receiver:
        raise SystemExit("hop %d runs %s->%s, not %s->%s" % (hop, role(row["from"]), role(row["to"]), sender, receiver))
    # The text each hop carries names its own hop, so a mismatch here is a hop
    # count that disagrees with the payload it was written from.
    if row.get("out_head") != "running-lights token, hop %d" % hop:
        raise SystemExit("hop %d carries %r" % (hop, row.get("out_head")))
    parent = None if hop == 0 else tasks[hop - 1]["task"]
    if row.get("parent_task") != parent:
        raise SystemExit("hop %d names parent %s, not %s" % (hop, row.get("parent_task"), parent))
    print("%-4d %-8s %-8s %-6s %-9s %-9s %s" % (
        hop, role(row["from"]), role(row["to"]), row["state"], row["task"][:8],
        (row.get("parent_task") or "-")[:8], row["out_head"]))
PY

# The agents' own record of the hop they were handed: every assign the ring
# delivered carried one, and together they are the same 0..11 the ledger holds.
# `seq -s,` is not portable enough to compare against — BSD seq terminates the
# last number with the separator too — so the list is built once, in python.
cat "$tmp"/light*/hops.log > "$tmp/hops.txt"
python3 - "$tmp/hops.txt" "$MAX_HOP" <<'PY' || fail "the agents must be handed hops 0..$MAX_HOP" "$(cat "$tmp/hops.txt")"
import sys

handed = sorted(int(line) for line in open(sys.argv[1]) if line.strip())
want = list(range(int(sys.argv[2]) + 1))
if handed != want:
    raise SystemExit("the ring handed out %s, not %s" % (handed, want))
PY

cat "$tmp/chain.txt"
echo "PASS running-lights"
