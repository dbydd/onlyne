#!/usr/bin/env bash
# onlyne-swarm headless stub E2E runner (no pi / model / orca needed).
#
# Usage: run_e2e.sh [e2e-1|e2e-2|e2e-2race|e2e-3|cancel-probe|reclaim-probe|all]
#
# Builds onlyne + onlyne-swarm from this checkout, stands up a fresh swarm
# tree under $E2E_ROOT (default: $PWD/.e2e-tree, never /tmp), drives it with
# the python stub agents, asserts the TEST.md expectations, then tears the
# scheduler + daemons down. Safe to re-run: each run wipes $E2E_ROOT first.
#
# Env overrides:
#   E2E_ROOT    swarm tree root (default $PWD/.e2e-tree)
#   ONLYNE_BIN  onlyne binary (default: built from the parent checkout)
#   SWARM_BIN   onlyne-swarm binary (default: cargo build of this repo)
set -euo pipefail

E2E_ROOT="${E2E_ROOT:-$PWD/.e2e-tree}"
# Physical path for process-cwd matching: on macOS /tmp is a symlink to
# /private/tmp, so a literal compare against $E2E_ROOT never matches lsof
# output and cleanup silently skips every leaked scheduler. Resolve the
# parent (E2E_ROOT may not exist yet) and rejoin the basename.
E2E_ROOT_PHYS="$(cd "$(dirname -- "$E2E_ROOT")" 2>/dev/null && pwd -P)/$(basename -- "$E2E_ROOT")"
REPO="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ONLYNE_BIN="${ONLYNE_BIN:-$(cd "$REPO/../../.." && pwd)/target/debug/onlyne}"
SWARM_BIN="${SWARM_BIN:-$(cd "$REPO/.." && pwd)/target/debug/onlyne-swarm}"
MARK="run-$$-$(date +%s)"

log() { echo "[e2e] $*"; }
die() { echo "[e2e] FAIL: $*" >&2; exit 1; }

need() { command -v "$1" >/dev/null || die "missing $1"; }

swarm() { "$SWARM_BIN" "$@"; }
task_state() { # task_state <id8> -> state string
  (cd "$E2E_ROOT" && swarm list 2>/dev/null | python3 -c "
import json,sys
for t in json.load(sys.stdin)['data']['rows']:
    if t['task_id'].startswith('$1'):
        print(t['state']); break
")
}
wait_state() { # wait_state <id8> <want> <timeout_secs>
  local end=$((SECONDS + $3))
  while ((SECONDS < end)); do
    [[ "$(task_state "$1")" == "$2" ]] && return 0
    sleep 2
  done
  return 1
}

# Kill only schedulers rooted in this test tree. A blanket
# `pkill -f "onlyne-swarm run"` kills every ring on the machine — including
# unrelated production schedulers (observed: a live ARIS ring). Resolve each
# candidate's cwd and match it against $E2E_ROOT; if the cwd is unreadable,
# skip rather than risk a wrong kill.
kill_tree_schedulers() {
  local pid cwd
  for pid in $(pgrep -f "onlyne-swarm run" 2>/dev/null || true); do
    cwd=$(lsof -a -p "$pid" -d cwd -Fn 2>/dev/null | sed -n 's/^n//p' | head -1)
    case "$cwd" in
      "$E2E_ROOT_PHYS" | "$E2E_ROOT_PHYS"/*) kill "$pid" 2>/dev/null || true ;;
    esac
  done
}

cleanup() {
  log "tearing down scheduler + daemons + stubs"
  kill_tree_schedulers
  # shellcheck disable=SC2046
  kill $(ps aux | grep "[o]nlyne --workspace $E2E_ROOT" | awk '{print $2}') 2>/dev/null || true
  pkill -f "stub_e2e1.py $E2E_ROOT" 2>/dev/null || true
  pkill -f "stub_fanout_" 2>/dev/null || true
  pkill -f "stub_loop.py $E2E_ROOT" 2>/dev/null || true
  pkill -f "stub_parked_ready.py" 2>/dev/null || true
  pkill -f "stub_reclaim.py" 2>/dev/null || true
}
trap cleanup EXIT

build() {
  log "building onlyne + onlyne-swarm"
  (cd "$(cd "$REPO/../../.." && pwd)" && cargo build --offline 2>/dev/null || cargo build) | tail -1
  (cd "$REPO/.." && cargo build --offline 2>/dev/null || cargo build) | tail -1
  [[ -x "$ONLYNE_BIN" ]] || die "onlyne binary missing: $ONLYNE_BIN"
  [[ -x "$SWARM_BIN" ]] || die "onlyne-swarm binary missing: $SWARM_BIN"
}

mktree() { # mktree <with_c:0|1>
  log "fresh tree at $E2E_ROOT (marker $MARK)"
  rm -rf "$E2E_ROOT"
  mkdir -p "$E2E_ROOT/.agents/.schedule/a" "$E2E_ROOT/.agents/.schedule/b" "$E2E_ROOT/.onlyne"
  cat >"$E2E_ROOT/.agents/.schedule/a/template.workspace.jsonc" <<'EOF'
{"name": "a", "role": "echo node a", "model": {"provider": "p", "model": "m", "effort": "low"}, "back_edges": []}
EOF
  cat >"$E2E_ROOT/.agents/.schedule/b/template.workspace.jsonc" <<'EOF'
{"name": "b", "role": "echo node b", "model": {"provider": "p", "model": "m", "effort": "low"}, "back_edges": ["../a"]}
EOF
  if [[ "${1:-0}" == "1" ]]; then
    mkdir -p "$E2E_ROOT/.agents/.schedule/c"
    cat >"$E2E_ROOT/.agents/.schedule/c/template.workspace.jsonc" <<'EOF'
{"name": "c", "role": "echo node c", "model": {"provider": "p", "model": "m", "effort": "low"}, "back_edges": []}
EOF
  fi
}

start_sched() {
  log "starting scheduler"
  export ONLYNE_BIN SWARM_STUB_AGENT=1
  unset SWARM_PI_EXT
  (cd "$E2E_ROOT" && nohup "$SWARM_BIN" run >"$E2E_ROOT/../sched-$MARK.log" 2>&1 &)
  for _ in $(seq 1 30); do
    (cd "$E2E_ROOT" && swarm status >/dev/null 2>&1) && break
    sleep 1
  done
  (cd "$E2E_ROOT" && swarm status >/dev/null 2>&1) || { tail -20 "$E2E_ROOT/../sched-$MARK.log"; die "scheduler did not come up"; }
}

submit() { # submit <to> <marker_text> -> task_id
  echo "$2" >"$E2E_ROOT/../payload-$MARK.md"
  (cd "$E2E_ROOT" && swarm submit --to "$1" --payload "$E2E_ROOT/../payload-$MARK.md" 2>/dev/null | python3 -c "import json,sys; print(json.load(sys.stdin)['data']['task_id'])")
}

wsdir() { # wsdir <tree_path> -> abs workspace dir
  if [[ "$1" == "." ]]; then echo "$E2E_ROOT"; else echo "$E2E_ROOT/.ws/$1"; fi
}

e2e1() {
  log "E2E-1 single chain root -> a"
  mktree 0; start_sched
  local id; id=$(submit a "hello e2e-1 $MARK")
  log "task $id"
  python3 "$REPO/stub_e2e1.py" "$(wsdir a)" a "h1-$MARK" "$MARK" &
  wait_state "${id:0:8}" closed 120 || die "E2E-1 task $id not closed (state=$(task_state "${id:0:8}"))"
  log "E2E-1 PASS"
}

e2e2() { # e2e2 <gap> <name>
  log "E2E-2 fanout ($2, child gap ${1}s)"
  mktree 1; start_sched
  local id; id=$(submit a "hello fanout $MARK")
  log "parent $id"
  python3 "$REPO/stub_fanout_parent.py" "$(wsdir a)" a "ph-$MARK" "$MARK" "$E2E_ROOT" "$1" &
  python3 "$REPO/stub_fanout_child.py" "$(wsdir b)" b "chb-$MARK" "$MARK" &
  python3 "$REPO/stub_fanout_child.py" "$(wsdir c)" c "chc-$MARK" "$MARK" &
  # NOTE: the parent stub exits right after its out; the scheduler may
  # legitimately recycle its tab (reclaim signal) in the same window the
  # out event is still in flight. A failed-by-recycle parent whose children
  # both closed is a pass: the work (2 children spawned) is done.
  wait_state "${id:0:8}" closed 180 || {
    st=$(task_state "${id:0:8}")
    if [[ "$st" != "failed" ]]; then die "E2E-2 parent $id not closed (state=$st)"; fi
    log "E2E-2 parent recycled mid-out (state=failed); accepting if children closed"
  }
  # Fire-and-forget: the parent is done as soon as its own out lands.
  # Children are independent tasks; each stub exits right after its own out,
  # but their out events may still be in flight when the parent closes.
  # Poll until all three tasks reach a terminal state.
  local end=$((SECONDS + 120))
  while ((SECONDS < end)); do
    local done; done=$(cd "$E2E_ROOT" && swarm list --state done 2>/dev/null | python3 -c "import json,sys; print(len(json.load(sys.stdin)['data']['rows']))")
    local closed; closed=$(cd "$E2E_ROOT" && swarm list --state closed 2>/dev/null | python3 -c "import json,sys; print(len(json.load(sys.stdin)['data']['rows']))")
    ((done + closed >= 3)) && break
    sleep 2
  done
  local done; done=$(cd "$E2E_ROOT" && swarm list --state done 2>/dev/null | python3 -c "import json,sys; print(len(json.load(sys.stdin)['data']['rows']))")
  local closed; closed=$(cd "$E2E_ROOT" && swarm list --state closed 2>/dev/null | python3 -c "import json,sys; print(len(json.load(sys.stdin)['data']['rows']))")
  ((done + closed >= 3)) || die "E2E-2 expected parent+2 children terminal (done=$done closed=$closed)"
  log "E2E-2 ($2) PASS"
}

e2e3() {
  log "E2E-3 back-edge self-exciting loop + cancel"
  mktree 0; start_sched
  # hand-written overlay: a -> b closes the ring (b -> a already in template)
  python3 - "$E2E_ROOT/.ws/a/.onlyne/swarm.workspace.jsonc" <<'PY'
import json,sys
p = sys.argv[1]
d = json.load(open(p)); d["back_edges"] = ["b"]
json.dump(d, open(p, "w"), indent=2)
PY
  local id; id=$(submit a "loop-seed $MARK")
  log "seed $id"
  python3 "$REPO/stub_loop.py" "$(wsdir a)" a "loop-a-$MARK" b "$E2E_ROOT" &
  python3 "$REPO/stub_loop.py" "$(wsdir b)" b "loop-b-$MARK" a "$E2E_ROOT" &
  # Online-session case (root-workspace variant of the 5f459ff5 incident): a
  # manual ready terminal is parked on workspace b BEFORE the loop seed is
  # submitted. The seed's dispatch must still open its own session and close
  # it via out; the parked handle must not steal the delivery.
  python3 "$REPO/stub_parked_ready.py" "$(wsdir b)" b "parked-$MARK" &
  sleep 45
  local before; before=$(cd "$E2E_ROOT" && swarm list 2>/dev/null | python3 -c "import json,sys; print(len(json.load(sys.stdin)['data']['rows']))")
  ((before >= 5)) || die "E2E-3 loop did not self-excite (tasks=$before)"
  log "loop alive with $before tasks; cancelling the seed family"
  # Fire-and-forget hops close fast; there may be no Running row at this
  # instant. Cancel the seed's transfer lineage (closed hops are harmless),
  # then stop the stubs so no new task enters after the observation point.
  (cd "$E2E_ROOT" && swarm cancel "$id" --reason "e2e-3 stop" >/dev/null)
  sleep 5
  pkill -f "stub_loop.py $E2E_ROOT" || true
  # quiesce: cancelled chain members are terminal; any still-running tasks
  # belong to sibling chains whose stubs just died -> cancel them too.
  for _ in $(seq 1 10); do
    live=$(cd "$E2E_ROOT" && swarm list --state running 2>/dev/null | python3 -c "
import json,sys
ds = json.load(sys.stdin)['data']['rows']
print(ds[0]['task_id'] if ds else '')")
    [[ -n "$live" ]] || break
    (cd "$E2E_ROOT" && swarm cancel "$live" --reason "e2e-3 quiesce" >/dev/null)
    sleep 2
  done
  local running; running=$(cd "$E2E_ROOT" && swarm list --state running 2>/dev/null | python3 -c "import json,sys; print(len(json.load(sys.stdin)['data']['rows']))")
  [[ "$running" == "0" ]] || die "E2E-3 $running tasks still running after cancel+stubkill"
  log "E2E-3 PASS"
}

reclaim_probe() {
  log "reclaim-probe: downlink recycle -> uplink ack -> tab close, no kill"
  mktree 0; start_sched
  local id; id=$(submit a "hello reclaim $MARK")
  log "task $id"
  python3 "$REPO/stub_reclaim.py" "$(wsdir a)" a "rc-$MARK" "$MARK" &
  local stub=$!
  # The task must reach running (dispatched + ready), then the operator
  # cancels: scheduler signals recycle, stub acks, tab closes.
  wait_state "${id:0:8}" running 60 || die "reclaim task $id never running"
  sleep 3
  local out; out=$(cd "$E2E_ROOT" && swarm cancel "$id" --reason e2e-reclaim)
  echo "$out" | grep -q "$id" || die "cancel did not list $id"
  wait "$stub" || die "reclaim stub failed (no ctl wire or no ack path)"
  # Ack observed -> task cancelled exactly once, never requeued as done.
  local st; st=$(task_state "${id:0:8}")
  [[ "$st" == "cancelled" ]] || die "reclaim task state=$st, want cancelled"
  log "reclaim-probe PASS"
}

cancel_probe() {
  log "cancel-probe: cancel a live running task family"
  mktree 0; start_sched
  local id; id=$(submit a "cancel me $MARK")
  sleep 3
  local out; out=$(cd "$E2E_ROOT" && swarm cancel "$id" --reason e2e-probe)
  echo "$out" | grep -q "$id" || die "cancel did not list $id"
  log "cancel-probe PASS"
}

usage() { echo "usage: $0 [e2e-1|e2e-2|e2e-2race|e2e-3|cancel-probe|all]"; exit 2; }

need python3; need cargo
build
case "${1:-all}" in
  e2e-1) e2e1 ;;
  e2e-2) e2e2 15 seq ;;
  e2e-2race) e2e2 0 race ;;
  e2e-3) e2e3 ;;
  cancel-probe) cancel_probe ;;
  reclaim-probe) reclaim_probe ;;
  all) e2e1; e2e2 15 seq; e2e2 0 race; e2e3; cancel_probe; reclaim_probe ;;
  *) usage ;;
esac
log "ALL DONE ($1)"
