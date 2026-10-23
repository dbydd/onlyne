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
# fake-backend clients, and one scripted agent process per session — twelve over
# the run, because the token travels the ring twice and one process serves one
# session — all on the loopback socket.
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
TUI=$(bin onlyne)
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

# One agent process serves one session: the client binds a mounting plugin to the
# one session it hands it — the process parks naming no session, takes this
# role's next staged session, and that connection serves nothing after it. The
# token walks the ring twice, so every role carries two hops over the run and
# needs one agent process for each of them. A second unnamed mount replaces the
# one parked slot a client holds, so a role's next agent may start only after the
# one before it took the session it came for: the role's hop reading `acked` in
# the ledger is that proof.
mount_light() {
  local i=$1
  # `{next_role}` is the one value the script cannot name itself, so it arrives
  # in the spawn environment: each agent knows the ring only through it.
  ONLYNE_NEXT_ROLE="light$(ring_next "$i")" \
    "$FAKE" --workspace "$tmp/light$i" --script "$SCRIPT" >>"$tmp/light$i-fake.log" 2>&1 &
  track $!
}

# `hop_acked <hop>` prints 1 while the ledger's task row for that hop reads
# `acked`, and 0 otherwise, leaving the ledger it read in `$tmp/hop.json`. The
# ring delivers one hop at a time, so this is how the second lap's agents keep
# step with it rather than with a clock.
hop_acked() {
  "$ONLYNE" --server-root "$tmp/server" ledger > "$tmp/hop.json" 2>/dev/null || true
  json_field "$tmp/hop.json" x \
    "int(any(r.get('kind') == 'task' and r.get('hop') == $1 and r.get('state') == 'acked' for r in json.load(sys.stdin)['data']['ledger']))" \
    2>/dev/null || true
}

for i in 1 2 3 4 5 6; do
  mount_light "$i"
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
denied=$(ONLYNE_ROLE=light1 "$ONLYNE" --workspace "$tmp/light1" send "${SUPERVISOR_FLAGS[@]}" --to light4 --text "shortcut" 2>"$tmp/denied.err") || true
printf '%s\n' "$denied" > "$tmp/denied.json"
detail="$denied$(cat "$tmp/denied.err")"
[ "$(json_field "$tmp/denied.json" '.ok' 'str(json.load(sys.stdin).get("ok")).lower()' 2>/dev/null || true)" = "false" ] || fail "a chord send must be refused" "$detail"
[ "$(json_field "$tmp/denied.json" '.error.code' 'json.load(sys.stdin).get("error",{}).get("code","")' 2>/dev/null || true)" = "acl_denied" ] || fail "a chord send must answer acl_denied" "$detail"

# --- start the ring ---------------------------------------------------------
# `light6` is both the ring's last hop and the token's origin: the send that
# starts the lights is the edge the wrap-around already allows, so the example
# needs no seventh role, no extra client, and no extra ACL pair.
send_out=$(ONLYNE_ROLE=light6 "$ONLYNE" --workspace "$tmp/light6" send "${SUPERVISOR_FLAGS[@]}" --to light1 --text "running-lights token, hop 0") \
  || fail "the ring's first send failed" "$send_out"
printf '%s\n' "$send_out" > "$tmp/send.json"
[ "$(json_field "$tmp/send.json" '.data.state' 'json.load(sys.stdin)["data"]["state"]')" = "in_flight" ] || fail "the first task must start in_flight" "$send_out"
first_task=$(json_field "$tmp/send.json" '.data.task' 'json.load(sys.stdin)["data"]["task"]')

# The token's second lap — hops 6..11 — is served by a second set of agents: the
# one each role's first hop claimed has finished with its session, so the role's
# client can hand its next session to a new one. The mount waits for the role's
# first hop to read `acked`, the moment that client's parked slot is free again;
# starting every second agent on a timer instead would risk mounting into an
# occupied slot, and a session left without its agent is refused `session_dead`
# once the reconnect grace expires rather than waited for.
for hop in 0 1 2 3 4 5; do
  settled=false
  # The wait outlasts `[client] reconnect_grace_secs` (60 s by default), so a
  # session the sweep refused is reported as the refusal it earned rather than
  # as a silence: eight hundred tenths of a second.
  for _ in $(seq 1 800); do
    if [ "$(hop_acked "$hop")" = "1" ]; then
      settled=true
      break
    fi
    if rows_any "$tmp/hop.json" reason session_dead; then
      fail "hop $hop's session was refused session_dead instead of being served" \
        "$(cat "$tmp/hop.json" 2>/dev/null)"
    fi
    sleep 0.1
  done
  [ "$settled" = true ] || fail "hop $hop never settled, so light$((hop + 1)) has no agent for its second hop" \
    "$(cat "$tmp/hop.json" 2>/dev/null)"
  mount_light $((hop + 1))
done

# --- two frames of the moving light -----------------------------------------
# The frame the case reads is the cluster page's role registry: one row per role,
# `role · presence · sess · busy · idle · susp · queue`, so a row's `busy` count
# names the role holding the token — it is the only role in the ring with a
# session being worked. (The page this case used to read, a per-session graph
# table, went with the map in `fa2fbe7`; the registry is where the same fact
# lives now, and `onlyne tui` takes no `--page` flag at all.)
#
# `working_rows <frame>` prints `role busy` for every registry row. `sighting
# <frame> <role>` is the narrow question: that role's row counts a busy session.
# `inflight_edges <answer>` and `edge_seen <answer> <edge>` read the same edge
# from the ledger, so the case takes the picture and the record as one fact
# rather than two. The chain's earlier rows stay in flight until they are acked,
# so the ledger is asked whether the edge is present among them.
working_rows() {
  python3 - "$1" <<'PY'
import re
import sys

# The registry row is `role presence sess busy idle susp queue`, and the
# selected row carries a `▸` gutter in front of the name. A row is matched whole
# rather than split into cells, because the frame puts the roles pane and the
# board pane side by side and a cell split would read across the seam.
ROW = re.compile(
    r"(?:▸\s+|\s\s+)(light[1-6])\s+(online|offline)\s+(\d+)/(\d+)"
    r"\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s"
)

for line in open(sys.argv[1]).read().splitlines():
    match = ROW.search(line)
    if match:
        print("%s %s" % (match.group(1), match.group(5)))
PY
}

working_roles() {
  working_rows "$1" | awk '$2 >= 1 { print $1 }' | sort -u | paste -sd, -
}

sighting() {
  working_rows "$1" | awk -v want="$2" \
    '$1 == want && $2 >= 1 { found = 1 } END { print found + 0 }'
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

edge_seen() {
  inflight_edges "$1" | awk -F, -v want="$2" \
    '{ for (i = 1; i <= NF; i++) if ($i == want) found = 1 } END { print found + 0 }'
}

capture_any_light() {
  local exclude=$1 frame=$2 ledger=$3 attempt seen edge role roles_seen= caught=none
  for attempt in $(seq 1 300); do
    "$ONLYNE" --server-root "$tmp/server" ledger --state in_flight > "$ledger.cand" 2>/dev/null || true
    "$TUI" tui --server-root "$tmp/server" --once > "$frame.cand" 2>/dev/null || true
    while read -r role; do
      [ -n "$role" ] || continue
      [ "$role" != "$exclude" ] || continue
      roles_seen="${roles_seen:+$roles_seen,}$role"
      caught=$role
      seen=$(sighting "$frame.cand" "$role")
      edge=$(edge_seen "$ledger.cand" "$(light_edge "$role")")
      if [ "$seen" = "1" ] && [ "$edge" = "1" ]; then
        mv "$ledger.cand" "$ledger"
        mv "$frame.cand" "$frame"
        printf '%s\n' "$role"
        return 0
      fi
    done < <(working_rows "$frame.cand" | awk '$2 >= 1 { print $1 }' | sort -u)
    sleep 0.05
  done
  fail "no TUI frame caught a working light (caught role: $caught; roles saw: ${roles_seen:-none})" \
    "$(cat "$frame.cand" 2>/dev/null)$(cat "$ledger.cand" 2>/dev/null)"
}

first_role=$(capture_any_light "" "$tmp/frame-a.txt" "$tmp/frame-a-ledger.json")
second_role=$(capture_any_light "$first_role" "$tmp/frame-b.txt" "$tmp/frame-b-ledger.json")
[ "$first_role" != "$second_role" ] \
  || fail "the two captures must catch the light on different roles" "$first_role"

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
    # count that disagrees with the payload it was written from. Hop 0 is the
    # operator's own send, so it carries the token bare; every later hop is a
    # relayed child, and a task row's `out_head` is the body preview the server
    # keeps for the operator, so the relay builder's own `handoff: ` prefix
    # (RELAY_BODY_PREFIX in crates/onlyne-client/src/session/handoff.rs)
    # shows on it. That builder is the contract, not a stray.
    prefix = "" if hop == 0 else "handoff: "
    if row.get("out_head") != prefix + "running-lights token, hop %d" % hop:
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
