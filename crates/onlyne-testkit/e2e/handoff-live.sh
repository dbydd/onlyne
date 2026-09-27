#!/usr/bin/env bash
set -euo pipefail
# Verification case 20: the handoff chain across two real pi sessions.
#
# Case 11 (`pi-live.sh`) proves one pi session completes an assignment through
# its own exit. This case proves the *next* step of the same path: a running
# session hands its task on into a second role, and the child the host mints is
# a real continuation of the family rather than a new run.
#
# The whole point is that the handoff travels the model's own tool. The plugin
# registers `onlyne_handoff`, that tool sends the adapter protocol's `handoff`
# op, and the client mints the child from the causality it holds. Driving the
# operator's `onlyne handoff` verb instead would prove the admin surface works
# and say nothing about the path a session actually takes.
#
# Skip discipline is case 11's: a real model call is needed, so a host without
# pi or without working credentials prints SKIP and exits 0. The probe is a real
# round trip because an env-var sniff cannot tell a live credential from a dead
# one.
SRC=$(pwd)
tmp=$(mktemp -d)
server_pid=""
alpha_pid=""
beta_pid=""
# A live case is diagnosed from its artifacts: a passing run throws the scratch
# directory away, a failing one keeps it and says where it is. `HANDOFF_LIVE_KEEP=1`
# forces it either way.
cleanup() {
  local status=$?
  for pid in "$beta_pid" "$alpha_pid" "$server_pid"; do
    [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
  done
  wait 2>/dev/null || true
  if [ "$status" -ne 0 ] || [ "${HANDOFF_LIVE_KEEP:-}" = "1" ]; then
    echo "handoff-live: scratch kept at $tmp" >&2
  else
    rm -rf "$tmp"
  fi
}
trap cleanup EXIT
. "$SRC/crates/onlyne-testkit/e2e/lib.sh"

# The client must spawn a real agent process and hold its stdin open, which is
# what pi's RPC mode needs; the fake backend spawns nothing.
export ONLYNE_BACKEND=exec

PLUGIN_DIR="$SRC/plugins/onlyne-agent-pi"
MODEL="${HANDOFF_LIVE_MODEL:-axonhub/generic-writer}"

if ! command -v pi >/dev/null 2>&1; then
  echo "SKIP handoff-live: pi is not on PATH"
  exit 0
fi

# The credential probe: one real turn in a throwaway directory, with the model
# this case will run under. A host with pi but a dead credential must not read
# as a pass, and a host with no credential must not read as a product failure.
#
# The question is one a live model answers and a dead credential cannot: the
# writer lane this case runs refuses an instruction to emit a canned marker
# verbatim ("I can't reply with a forced exact token"), so a marker-shaped probe
# would read as a broken credential on a healthy host. The expected answer is
# still a fixed string, and it still comes back only from a real round trip.
probe_dir="$tmp/probe"
mkdir -p "$probe_dir"
probe_ok=false
if (cd "$probe_dir" && timeout 180 pi -ns -nc --no-session --model "$MODEL" -p 'Answer with just the city name: what is the capital of France?' >"$tmp/probe.out" 2>"$tmp/probe.err"); then
  grep -q "Paris" "$tmp/probe.out" && probe_ok=true
fi
if [ "$probe_ok" != true ]; then
  echo "SKIP handoff-live: pi has no working credential for $MODEL"
  sed -n '1,5p' "$tmp/probe.err" >&2 || true
  exit 0
fi

# One role's runtime argv, the `command` half of its `[[client]].runtime` table.
# Two details are load-bearing and both are case 11's: RPC mode treats stdin EOF
# as "the operator left", which is exactly the pipe the `exec` backend holds
# open, and `--session-dir` keeps a session file the case can read back as proof
# the assignment reached pi's context.
runtime_command() {
  local ws=$1
  printf '["pi", "--mode", "rpc", "--model", "%s", "--session-id", "{session}", "--session-dir", "%s/.pi/sessions", "-e", "%s", "-ns", "-nc"]' \
    "$MODEL" "$ws" "$PLUGIN_DIR"
}

ALPHA_PROSE='You are alpha. When a task reaches you, hand it on to beta with the onlyne_handoff tool, then end the task with onlyne_complete and outcome done.'
BETA_PROSE='You are beta. When a task reaches you, end it with onlyne_complete and outcome done.'

setup_cluster "$tmp/server" "$tmp/alpha" alpha alpha "" "$ALPHA_PROSE" \
  'allowed_senders = ["*", "alpha"]
allowed_targets = ["beta"]
[client.runtime]
drive = "plugin"
command = '"$(runtime_command "$tmp/alpha")"
server_pid=$cluster_server_pid
SERVER_ROOT="$tmp/server"

client_init "$tmp/beta" beta "$SERVER_ROOT" "$SERVER_ROOT/.onlyne/spec.toml" "$tmp/beta-spec.frag.toml" "$BETA_PROSE" \
  'allowed_senders = ["*", "beta"]
allowed_targets = ["alpha"]
[client.runtime]
drive = "plugin"
command = '"$(runtime_command "$tmp/beta")"
"$ONLYNE" --server-root "$SERVER_ROOT" reload

# The plugin's own switch file. The generated templates carry it; these
# workspaces come from `client_init`, so the case writes the same two keys.
for ws in "$tmp/alpha" "$tmp/beta"; do
  mkdir -p "$ws/.pi"
  printf '{"enabled":true,"watch":{"autoStart":true}}\n' > "$ws/.pi/onlyne.json"
done

"$CLIENT" run --workspace "$tmp/alpha" >"$tmp/alpha-client.log" 2>&1 &
alpha_pid=$!
"$CLIENT" run --workspace "$tmp/beta" >"$tmp/beta-client.log" 2>&1 &
beta_pid=$!

# Both clients must be registered and connected before any work is dispatched:
# the wait reads the fields `onlyne roles` ships (`name`, and the `Presence` in
# `state`) rather than the v1 spelling, so a role that is listed but not
# connected does not pass for a live one.
roles_out=""
online=false
for _ in $(seq 1 150); do
  roles_out=$("$ONLYNE" --server-root "$SERVER_ROOT" roles 2>/dev/null || true)
  if printf '%s' "$roles_out" | python3 -c '
import json,sys
try:
    data=json.load(sys.stdin)
except ValueError:
    sys.exit(1)
roles={role.get("name"): role.get("state") for role in data.get("data",{}).get("roles",[])}
sys.exit(0 if roles.get("alpha")=="online" and roles.get("beta")=="online" else 1)
'; then
    online=true
    break
  fi
  sleep 0.2
done
[ "$online" = true ] || fail "both roles must register on the server" \
  "roles=$roles_out alpha=$(sed -n '1,20p' "$tmp/alpha-client.log" 2>/dev/null) beta=$(sed -n '1,20p' "$tmp/beta-client.log" 2>/dev/null)"

send_out=$("$ONLYNE" --server-root "$SERVER_ROOT" send "${SUPERVISOR_FLAGS[@]}" --from alpha --to alpha \
  --text "Call the onlyne_handoff tool with to=\"beta\" to hand this task on, then call onlyne_complete with outcome done.") \
  || fail "send command failed" "$send_out"
printf '%s\n' "$send_out" >"$tmp/send.json"
root_task=$(json_field "$tmp/send.json" '.data.task' 'json.load(sys.stdin)["data"]["task"]')

# 1. The family grows a second row: the child the agent's own tool minted.
#
#    The listing is read whole rather than through `ledger --task <root>`: the
#    flag's help says "family", but the filter it reaches the store with is
#    `task=?` on the row's own column, so a child row — whose `task` is its own
#    id — never comes back from the parent's query. The child is the row whose
#    `parent_task` names the root, and step 2 reads both rows off this same
#    answer.
ledger_out=""
child=""
for _ in $(seq 1 360); do
  ledger_out=$("$ONLYNE" --server-root "$SERVER_ROOT" ledger 2>/dev/null || true)
  printf '%s' "$ledger_out" >"$tmp/ledger.json"
  # The two rows are told apart by their parent link: the root has none.
  child=$(data_rows "$tmp/ledger.json" 2>/dev/null | python3 -c '
import json,sys
rows=[json.loads(line) for line in sys.stdin if line.strip()]
for row in rows:
    if row.get("parent_task") == sys.argv[1]:
        print(row.get("task","")); break
' "$root_task" 2>/dev/null || true)
  if [ -n "$child" ]; then
    break
  fi
  sleep 0.5
done
[ -n "$child" ] || fail "the handoff must mint a child task" \
  "ledger=$ledger_out alpha=$(sed -n '1,40p' "$tmp/alpha-client.log" 2>/dev/null)"

# 2. The child continues the family rather than starting a new run: it names the
#    root as its parent, sits one hop below it, and carries the family's own id.
data_rows "$tmp/ledger.json" | python3 -c '
import json,sys
root,child=sys.argv[1],sys.argv[2]
rows=[json.loads(line) for line in sys.stdin if line.strip()]
by_task={r.get("task"):r for r in rows}
child_row=by_task.get(child)
assert child_row is not None, f"no row for child {child}: {rows}"
assert child_row.get("parent_task")==root, f"child parent must be root: {child_row}"
assert child_row.get("hop")==1, f"child hop must be 1: {child_row}"
root_row=by_task.get(root)
assert root_row is not None, f"no row for root {root}: {rows}"
assert child_row.get("family")==root_row.get("family"), "family id must ride along"
assert child_row.get("family"), "family id must be set"
print("PASS handoff-live family: child parent=root hop=1 family carried")
' "$root_task" "$child" || fail "the child must continue the family" "$(cat "$tmp/ledger.json")"

# 3. The recipient role's session really ran the child: its own completion
#    settles the child row, which is what makes the chain a delivered handoff
#    rather than a queued envelope nobody took. Both halves of that are read:
#    the delivery row (alpha -> beta) reaches `acked`, and the completion row
#    that the recipient's own report wrote (beta -> alpha) is acked beside it,
#    so the settlement is the session's and not a supervisor's guess.
settled=false
for _ in $(seq 1 360); do
  ledger_out=$("$ONLYNE" --server-root "$SERVER_ROOT" ledger --task "$child" 2>/dev/null || true)
  printf '%s' "$ledger_out" >"$tmp/child-ledger.json"
  if data_rows "$tmp/child-ledger.json" 2>/dev/null | python3 -c '
import json,sys
rows=[json.loads(line) for line in sys.stdin if line.strip()]
def role(row,key):
    value=row.get(key)
    if isinstance(value,str):
        try: value=json.loads(value)
        except ValueError: return None
    while isinstance(value,dict):
        value=value.get("role")
    return value
delivered=any(row.get("kind")=="task" and row.get("state")=="acked" for row in rows)
completed=any(row.get("kind")=="completion" and row.get("state")=="acked" and role(row,"from")=="beta" for row in rows)
sys.exit(0 if delivered and completed else 1)
'; then
    settled=true
    break
  fi
  sleep 0.5
done
[ "$settled" = true ] || fail "beta must take the child to acked" \
  "ledger=$ledger_out beta=$(sed -n '1,40p' "$tmp/beta-client.log" 2>/dev/null)"
printf 'PASS handoff-live settlement: the child task and beta'"'"'s completion row both read acked\n'

# 4. beta's pi session carries the handoff text, so the child reached a real
#    context rather than only the ledger. Every session file the workspace wrote
#    is read, because a re-run leaves more than one behind and the claim is that
#    the child reached a context, not that it reached the first file `find`
#    lists.
beta_sessions=""
for _ in $(seq 1 100); do
  beta_sessions=$(find "$tmp/beta" -name '*.jsonl' -path '*sessions*' 2>/dev/null)
  [ -n "$beta_sessions" ] && break
  sleep 0.2
done
[ -n "$beta_sessions" ] || fail "beta's pi session file must exist" \
  "$(find "$tmp/beta" -name '*.jsonl' 2>/dev/null | head -n 5)"
# The list is expanded unquoted on purpose: `find` names one file per line and
# each is an argument. No workspace path here holds a space.
#
# The delivery text is the client's, so the file is read for the template's own
# source line: v2 keeps the task id out of the body, and the child id reaches
# this file through the plugin's `onlyne-assign` entry beside the text it
# injected. All three are one claim — this context was handed this child — so
# one file has to carry all three.
beta_file=""
for file in $beta_sessions; do
  if grep -q -F -- "onlyne-assign" "$file" 2>/dev/null \
    && grep -q -F -- "From alpha:" "$file" 2>/dev/null \
    && grep -q -F -- "$child" "$file" 2>/dev/null; then
    beta_file=$file
    break
  fi
done
[ -n "$beta_file" ] || fail "the child assign must reach beta's pi context" \
  "child=$child sessions=$beta_sessions"
printf 'PASS handoff-live delivery: %s carries the child assign\n' "$beta_file"

# 5. alpha's own session records the handoff tool call, which is the evidence
#    that the model used its tool rather than the case driving the admin verb.
#    The check is structural, over the transcript's own `toolCall` blocks: a
#    grep for the tool's name is a false positive with a live model, because the
#    role prose names the tool ("hand it on to beta with the onlyne_handoff
#    tool") and the plugin records that prose in the file as `onlyne-role-prose`.
#    So the transcript must carry an assistant `toolCall` whose name is
#    `onlyne_handoff` and whose `to` argument is beta, and the host's answer to
#    that call is the plugin's own sentence, `handed on to beta`.
#    The child itself is proven from the ledger row it minted, not from that
#    answer: the minted id is host bookkeeping and stays out of the model's
#    context, which is what the plugin's tool result deliberately does not
#    carry. So the two halves are the same act read where each owner writes it —
#    the session records the call and its acceptance, the ledger records the row
#    the host minted (exactly one child of the root, hop 1, addressed to beta,
#    read off the same answer step 2 used).
#    The call and its answer are read the way pi writes them: an assistant
#    message content block of type `toolCall` carries the name and arguments,
#    and a `toolResult` message carrying the same `toolCallId` holds what the
#    host answered. A call that reached the host and a `done` ledger row are the
#    same act only when the row's own columns name the child the case is holding
#    and the answer says the handoff landed, which is why both halves are
#    asserted here rather than the presence of the tool's name.
alpha_sessions=""
for _ in $(seq 1 100); do
  alpha_sessions=$(find "$tmp/alpha" -name '*.jsonl' -path '*sessions*' 2>/dev/null)
  [ -n "$alpha_sessions" ] && break
  sleep 0.2
done
[ -n "$alpha_sessions" ] || fail "alpha's pi session file must exist" \
  "$(find "$tmp/alpha" -name '*.jsonl' 2>/dev/null | head -n 5)"
# One file per line again, and the checker reads all of them: the call the case
# wants may be in any transcript this workspace wrote. The ledger rows arrive on
# stdin, so this one script reads the session's side and the host's side of the
# same handoff and refuses to accept a call the ledger does not back.
data_rows "$tmp/ledger.json" | python3 -c '
import json,sys
root,child=sys.argv[1],sys.argv[2]
paths=sys.argv[3:]

def answer_text(message):
    content=message.get("content")
    if isinstance(content,str):
        return content
    parts=[]
    for block in content or []:
        if isinstance(block,str):
            parts.append(block)
        elif isinstance(block,dict) and isinstance(block.get("text"),str):
            parts.append(block["text"])
    return "\n".join(parts)

def role_of(row):
    """The role the row is addressed to, from the decoded `to` principal."""
    to=row.get("to")
    if isinstance(to,str):
        try:
            to=json.loads(to)
        except ValueError:
            return None
    while isinstance(to,dict):
        to=to.get("role")
    return to if isinstance(to,str) else None

rows=[json.loads(line) for line in sys.stdin if line.strip()]
children=[row for row in rows if row.get("parent_task")==root]
assert len(children)==1, "the root must have exactly one child row: %s" % children
row=children[0]
assert row.get("task")==child, "the child row must be the one the case holds: %s" % row
assert row.get("hop")==1, "the child row must sit at hop 1: %s" % row
assert role_of(row)=="beta", "the child row must be addressed to beta: %s" % row

calls=[]
answers={}
for path in paths:
    for line in open(path):
        line=line.strip()
        if not line:
            continue
        try:
            entry=json.loads(line)
        except ValueError:
            continue
        message=entry.get("message") or {}
        role=message.get("role")
        if role=="assistant":
            for block in message.get("content") or []:
                if isinstance(block,dict) and block.get("type")=="toolCall" and block.get("name")=="onlyne_handoff":
                    calls.append(block)
        elif role=="toolResult" and message.get("toolName")=="onlyne_handoff":
            answers[message.get("toolCallId")]=message
if not calls:
    raise SystemExit("no assistant toolCall named onlyne_handoff in " + " ".join(paths))
paired=[]
for call in calls:
    if (call.get("arguments") or {}).get("to")!="beta":
        continue
    answer=answers.get(call.get("id"))
    if answer is None:
        continue
    paired.append((call,answer))
assert paired, ("no onlyne_handoff call to beta has an answer: arguments=%s answers=%s" % (
    [call.get("arguments") for call in calls],
    [json.dumps(answer.get("content"))[:200] for answer in answers.values()]))
accepted=[answer for _,answer in paired if not answer.get("isError")]
assert accepted, "every onlyne_handoff call to beta was answered with an error: %s" % (
    [answer_text(answer)[:200] for _,answer in paired])
for answer in accepted:
    text=answer_text(answer).strip()
    assert text=="handed on to beta", "the tool result must be the plain sentence: %r" % text
print("PASS handoff-live tool: onlyne_handoff to=beta answered `handed on to beta`, "
      "and the ledger row it minted is child %s (parent=%s hop=1 to=beta)" % (child,root))
' "$root_task" "$child" $alpha_sessions || fail "alpha must have called the handoff tool" \
  "sessions=$alpha_sessions child=$child ledger=$ledger_out"

echo "PASS handoff-live"
