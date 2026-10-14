#!/usr/bin/env bash
set -euo pipefail
# v2.3 live acceptance: one real cluster, one real IM gateway, one real platform
# backend, one real human conversation.
#
# This is the case that proves the chat addressing protocol end to end on a
# machine that talks to a real platform. Unlike the scenario suite it needs
# credentials, and unlike the e2e scripts it is not hermetic: a Telegram bot
# token and a chat id are read from the environment and every message goes to
# that chat.
#
# What it proves (docs/v2.3-CHAT-PROTOCOL.md):
#   1. `onlyne-im init` + `generate` lay a gateway root down with one role and
#      one conversation, and the cluster half lands in the live spec.
#   2. A human message on the platform reaches the agent role as a delivery
#      whose first line is that conversation's chat key.
#   3. The agent's reply, addressed to the gateway role with the key echoed,
#      renders back into that platform conversation — not another one.
#   4. A reply with no key, or a key this gateway does not carry, is refused
#      and recorded rather than delivered.
#
# Credentials come from the environment, never from a file in the repo:
#   ONLYNE_TELEGRAM_TOKEN   the bot token (also `onlyne-im login --gateway tg1`)
#   ONLYNE_IM_CHAT_ID       the chat id this case talks to, e.g. -1001234567890
#   ONLYNE_IM_PEER          the agent role that answers; default _supervisor
#
# Everything else is built in a scratch directory and torn down on exit.

SRC=$(pwd)
tmp=$(mktemp -d)
server_pid=""
client_pid=""
fake_pid=""
gateway_pid=""
cleanup() {
  drain_pid "$gateway_pid" 2>/dev/null || true
  drain_pid "$fake_pid" 2>/dev/null || true
  drain_pid "$client_pid" 2>/dev/null || true
  drain_pid "$server_pid" 2>/dev/null || true
  rm -rf "$tmp"
}
trap cleanup EXIT
. "$SRC/crates/onlyne-testkit/e2e/lib.sh"

CHAT_ID=${ONLYNE_IM_CHAT_ID:-}
TOKEN=${ONLYNE_TELEGRAM_TOKEN:-}
PEER=${ONLYNE_IM_PEER:-_supervisor}
GATEWAY_ID=tg1
GW_ROLE=im

[ -n "$TOKEN" ] || fail "ONLYNE_TELEGRAM_TOKEN is not set" \
  "export a Telegram bot token; this case talks to a real chat"
[ -n "$CHAT_ID" ] || fail "ONLYNE_IM_CHAT_ID is not set" \
  "export the chat id to talk to, e.g. -1001234567890 for a group"

IM=$(bin onlyne-im)
BACKEND=$(bin onlyne-im-backend-telegram)

# ---------------------------------------------------------------- cluster ----
# One server, one agent role. The agent role is `planner` so the fake agent's
# echo-complete script works unchanged, and its ACL pair names the gateway role
# in both directions.
setup_cluster "$tmp/server" "$tmp/planner" planner cluster "" "$E2E_PROSE" "allowed_senders = [\"*\", \"planner\", \"$GW_ROLE\"]
allowed_targets = [\"planner\", \"$GW_ROLE\"]"
server_pid=$cluster_server_pid

# ------------------------------------------------------------ gateway root ---
# `init` writes the root document with the gateway role, then `generate` mints
# the one conversation and declares the role in the live spec through the admin
# socket. The backend argv names this machine's build, so no PATH lookup can
# pick up an installed one.
python3 - "$tmp/gw" "$BACKEND" "$GATEWAY_ID" <<'PY'
import pathlib, sys
root, backend, gateway = sys.argv[1], sys.argv[2], sys.argv[3]
onlyne = pathlib.Path(root) / ".onlyne"
onlyne.mkdir(parents=True, exist_ok=True)
(onlyne / "config.toml").write_text(f"""
[gateway]
role = "im"

[server]
endpoint = "PLACEHOLDER"
cert_pin = "PLACEHOLDER"

[[im.backend]]
gateway = "{gateway}"
run = ["{backend}", "--gateway", "{gateway}"]
""")
PY

# The endpoint and pin come from the server root's own spec file, which is the
# one place both halves agree on: the server daemon was started from it and the
# gateway dials what it says.
read -r ENDPOINT PIN <<<"$(python3 - "$tmp/server/.onlyne/spec.toml" <<'PY'
import re, sys
text = open(sys.argv[1]).read()
listen = re.search(r'^\s*listen\s*=\s*"([^"]+)"', text, re.M)
pin = re.search(r'^\s*cert_pin\s*=\s*"([^"]+)"', text, re.M)
print(listen.group(1) if listen else "", pin.group(1) if pin else "")
PY
)"
[ -n "$ENDPOINT" ] || fail "could not read the server endpoint from its spec" \
  "$(cat "$tmp/server/.onlyne/spec.toml")"
[ -n "$PIN" ] || fail "could not read the cert pin from the server spec" \
  "$(cat "$tmp/server/.onlyne/spec.toml")"
python3 - "$tmp/gw/.onlyne/config.toml" "$ENDPOINT" "$PIN" <<'PY'
import pathlib, sys
path, endpoint, pin = sys.argv[1], sys.argv[2], sys.argv[3]
text = pathlib.Path(path).read_text()
pathlib.Path(path).write_text(
    text.replace('endpoint = "PLACEHOLDER"', f'endpoint = "{endpoint}"')
        .replace('cert_pin = "PLACEHOLDER"', f'cert_pin = "{pin}"')
)
PY

# The credential goes where the backend process reads it: one file per gateway
# id, 0600, never printed.
"$IM" login --root "$tmp/gw" --gateway "$GATEWAY_ID" --token "$TOKEN"

# The trigger script is the inbound half's routing decision: this one sends
# every message to the peer role with the conversation's chat key as the
# delivery's first line, which is the shape §5.1 fixes.
cat >"$tmp/im-in.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
# stdin is the event JSON; $1 is the mode word. The chat key is the token this
# conversation answers to, so it rides the delivery's first line.
input=$(cat)
chat=$(printf '%s' "$input" | python3 -c 'import json,sys; print(json.load(sys.stdin)["chat"])')
text=$(printf '%s' "$input" | python3 -c 'import json,sys
e = json.load(sys.stdin).get("event") or {}
print(e.get("text") or "(no text)")')
printf '%s\n\n%s\n' "<chat:$chat>" "$text" > "$tmp/reply-body.txt"
# `--as admin`: a bare socket path carries no hint of which surface it serves,
# and `--from` is an admin-surface flag. Discovery by `--server-root` would work
# too, but the gateway hands a script the socket and nothing else.
"$ONLYNE" send --socket "$ONLYNE_SOCKET" --as admin --from im --to "$PEER" \
  --text "$(cat "$tmp/reply-body.txt")" --force --yes-i-am-supervisor-not-other-role
SH
chmod +x "$tmp/im-in.sh"
python3 - "$tmp/gw/.onlyne/config.toml" "$tmp/im-in.sh" <<'PY'
import pathlib, sys
path, script = sys.argv[1], sys.argv[2]
text = pathlib.Path(path).read_text()
text += f'\n[[im.push]]\nmode = "trigger"\nrun = ["{script}"]\ntimeout = "15s"\n'
pathlib.Path(path).write_text(text)
PY

# `generate` writes the conversation and the cluster half. The peer is the
# planner role, so the ACL pair `generate` writes is the pair this case uses.
gen_out=$("$IM" generate --root "$tmp/gw" --gateway "$GATEWAY_ID" --conversation "$CHAT_ID" \
  --role live-chat --peer planner 2>&1) || fail "onlyne-im generate failed" "$gen_out"
printf '%s\n' "$gen_out" > "$tmp/generate.out"
CHAT_KEY=$(sed -n 's/^chat key //p' "$tmp/generate.out")
[ -n "$CHAT_KEY" ] || fail "generate printed no chat key" "$(cat "$tmp/generate.out")"

# The gateway role's row is now in the live spec; reload so both halves agree.
"$ONLYNE" --server-root "$tmp/server" reload >/dev/null

# ------------------------------------------------------------- agent half ----
"$CLIENT" run --workspace "$tmp/planner" >"$tmp/client.log" 2>&1 &
client_pid=$!
"$FAKE" --workspace "$tmp/planner" --script "$SRC/crates/onlyne-testkit/scripts/echo-complete.json" >"$tmp/fake.log" 2>&1 &
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
[ "$registered" = "true" ] || fail "planner must be online before the gateway dials" \
  "roles=$roles_out client=$(cat "$tmp/client.log" 2>/dev/null)"

# ---------------------------------------------------------------- gateway ----
# The gateway dials the cluster as the one role `generate` declared and pulls
# deliveries for it. Its log is the evidence for every step below.
"$IM" run --root "$tmp/gw" --socket "$(runtime_socket "$tmp/server")" >"$tmp/gateway.log" 2>&1 &
gateway_pid=$!

# ------------------------------------------------------------- the message ---
# A human message on the platform. The case sends it through the bot so the
# inbound path is the real one: gateway backend -> getUpdates -> trigger script
# -> send -> agent -> complete -> gateway render -> sendMessage.
say_out=$(curl -sS -X POST "https://api.telegram.org/bot$TOKEN/sendMessage" \
  -H 'Content-Type: application/json' \
  -d "$(python3 -c 'import json,sys; print(json.dumps({"chat_id": sys.argv[1], "text": sys.argv[2]}))' "$CHAT_ID" "onlyne live check $(date +%H:%M:%S)")") \
  || fail "could not post the probe message to the chat" "$say_out"
printf '%s\n' "$say_out" > "$tmp/say.json"
python3 - "$tmp/say.json" <<'PY' || fail "Telegram refused the probe message" "$(cat "$tmp/say.json")"
import json, sys
body = json.load(open(sys.argv[1]))
sys.exit(0 if body.get("ok") else 1)
PY
echo "probe message sent to chat $CHAT_ID"

# The inbound half: the gateway pulls the update, the trigger script sends it to
# planner, and planner's fake agent completes. The ledger row is the evidence.
task_seen=""
for _ in $(seq 1 120); do
  ledger_out=$("$ONLYNE" --server-root "$tmp/server" ledger 2>/dev/null) || ledger_out=''
  printf '%s\n' "$ledger_out" > "$tmp/ledger.json"
  if rows_any "$tmp/ledger.json" state acked 2>/dev/null; then
    task_seen=$(row_value "$tmp/ledger.json" task state acked)
    break
  fi
  sleep 0.5
done
[ -n "$task_seen" ] || fail "the inbound message never produced a settled ledger row" \
  "ledger=$ledger_out gateway=$(cat "$tmp/gateway.log" 2>/dev/null)"

# The delivery's first line was the chat key, which is what makes the reply
# addressable: the row's body carries it.
row_value "$tmp/ledger.json" body state acked | grep -q "<chat:$CHAT_KEY>" \
  || fail "the delivery's body did not carry the chat key on its first line" \
  "body=$(row_value "$tmp/ledger.json" body state acked)"

# The outbound half: the agent's completion is addressed to the gateway role,
# and the gateway renders it into the platform conversation. The chat log is
# the gateway's own record of what it sent.
log_seen=""
for _ in $(seq 1 60); do
  log_seen=$(cat "$tmp/gw/conversations/live-chat/chat.jsonl" 2>/dev/null || true)
  if printf '%s' "$log_seen" | grep -q '"direction":"out"'; then
    break
  fi
  sleep 0.5
done
printf '%s' "$log_seen" | grep -q '"direction":"out"' \
  || fail "the gateway never recorded an outbound payload" \
  "chatlog=$log_seen gateway=$(cat "$tmp/gateway.log" 2>/dev/null)"

# The outbound line names this conversation's key: that is the proof the reply
# was addressed to this chat and not another one.
printf '%s' "$log_seen" | grep -q "$CHAT_KEY" \
  || fail "the outbound chat log line does not name this conversation's key" "$log_seen"

# ---------------------------------------------------------- the refusals -----
# Two deliveries the gateway must refuse, each recorded as a fault rather than
# delivered into a chat. They are sent through the admin socket as the gateway
# role, which is the same path an agent role takes.
refuse_case() {
  local label=$1 body=$2 expect=$3
  local out task
  out=$("$ONLYNE" --server-root "$tmp/server" send "${SUPERVISOR_FLAGS[@]}" \
    --from "$GW_ROLE" --to "$GW_ROLE" --text "$body" 2>"$tmp/refuse.err") || true
  printf '%s\n' "$out" > "$tmp/refuse.json"
  task=$(json_field "$tmp/refuse.json" '.data.task' 'json.load(sys.stdin)["data"]["task"]' 2>/dev/null || true)
  [ -n "$task" ] || fail "$label: the send was refused before it reached the gateway" \
    "out=$out err=$(cat "$tmp/refuse.err")"
  local found="" faults_out=""
  for _ in $(seq 1 60); do
    faults_out=$("$ONLYNE" --server-root "$tmp/server" faults 2>/dev/null) || faults_out=''
    printf '%s\n' "$faults_out" > "$tmp/faults.json"
    if data_rows "$tmp/faults.json" | python3 -c '
import json, sys
want = sys.argv[1]
for line in sys.stdin:
    try:
        row = json.loads(line)
    except ValueError:
        continue
    if row.get("kind") == "im_unplaced_delivery" and want in (row.get("reason") or ""):
        sys.exit(0)
sys.exit(1)' "$expect"; then
      found=$faults_out
      break
    fi
    sleep 0.5
  done
  [ -n "$found" ] || fail "$label: no im_unplaced_delivery fault names $expect" \
    "faults=$faults_out gateway=$(cat "$tmp/gateway.log" 2>/dev/null)"
  echo "  refused as designed: $label"
}

# A key no conversation in this registry carries.
refuse_case "unknown key" "$(printf '<chat:%s>\n\nno such conversation' "$(python3 -c 'print("f"*32)')")" \
  "no conversation in this gateway carries chat key"
# A key that is not the body's first line.
refuse_case "misplaced key" "$(printf 'hello there\n\n<chat:%s>' "$CHAT_KEY")" \
  "not the body's first line"
# No key at all.
refuse_case "missing key" "a delivery with no chat key at all" \
  "names no chat key"

echo "PASS im-live (v2.3 chat addressing on a real platform)"
