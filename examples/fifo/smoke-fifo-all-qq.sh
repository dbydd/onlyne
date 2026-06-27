#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
ROOT_DIR=$(cd -- "$SCRIPT_DIR/../.." && pwd)
ONLYNE_BIN=${ONLYNE_BIN:-$ROOT_DIR/target/debug/onlyne}
WORKSPACE=${ONLYNE_WORKSPACE:-$ROOT_DIR/examples}
WORKSPACE=$(python3 - "$WORKSPACE" <<'PY'
import pathlib, sys
print(pathlib.Path(sys.argv[1]).resolve())
PY
)
TEXT=${ONLYNE_TEXT:-"Onlyne FIFO all-channel push smoke $(date -u +%Y-%m-%dT%H:%M:%SZ)"}
TIMEOUT=${ONLYNE_TIMEOUT:-180}
CHANNELS=${ONLYNE_CHANNELS:-telegram,feishu,qqbot,wechat}
DAEMON_LOG=${ONLYNE_DAEMON_LOG:-$WORKSPACE/fifo-daemon.log}
EVENT_LOG=${ONLYNE_EVENT_LOG:-$WORKSPACE/fifo-events.ndjson}
QQ_OUT_LOG=${ONLYNE_QQ_OUT_LOG:-$WORKSPACE/fifo-qqbot-out.log}

if [[ "${1:-}" == "--local-check" ]]; then
  command -v python3 >/dev/null
  bash -n "$0"
  echo "local-check ok"
  exit 0
fi

[[ -x "$ONLYNE_BIN" ]] || { echo "onlyne binary not found: $ONLYNE_BIN" >&2; exit 1; }
command -v python3 >/dev/null || { echo "python3 required" >&2; exit 1; }
mkdir -p "$WORKSPACE"
cd "$WORKSPACE"
"$ONLYNE_BIN" init >/dev/null

python3 - "$CHANNELS" <<'PY'
import os, pathlib, re, sqlite3, sys
channels = [c.strip() for c in sys.argv[1].split(',') if c.strip()]
config = pathlib.Path('.onlyne/config.toml')
text = config.read_text()
state = pathlib.Path('.onlyne/state.db')

def env_name(channel):
    return 'ONLYNE_WEIXIN_CONVERSATION_ID' if channel == 'wechat' else f'ONLYNE_{channel.upper()}_CONVERSATION_ID'

def db_conversation(channel):
    if not state.exists():
        return ''
    names = [channel]
    if channel == 'wechat': names.append('weixin')
    con = sqlite3.connect(state)
    for name in names:
        row = con.execute('select conversation_id from conversations where channel_id=? order by updated_at desc limit 1', (name,)).fetchone()
        if row and row[0]: return row[0]
        row = con.execute('select conversation_id from messages where channel_id=? order by timestamp desc limit 1', (name,)).fetchone()
        if row and row[0]: return row[0]
    return ''

def ensure_section(text, channel):
    candidates = [f'[adapters.{channel}]']
    if channel == 'wechat': candidates.append('[adapters.weixin]')
    for marker in candidates:
        if marker in text:
            return text, marker
    block = f'\n[adapters.{channel}]\nenabled = false\nbind_conversation_id = ""\n'
    return text.rstrip() + block + '\n', f'[adapters.{channel}]'

def set_key(block, key, value):
    line = f'{key} = {value}'
    if re.search(rf'^{re.escape(key)}\s*=', block, re.M):
        return re.sub(rf'^{re.escape(key)}\s*=.*$', line, block, flags=re.M)
    return block.rstrip() + '\n' + line + '\n'

for channel in channels:
    conv = os.environ.get(env_name(channel), '').strip() or db_conversation(channel)
    if not conv:
        raise SystemExit(f'missing {env_name(channel)} and no stored {channel} conversation in .onlyne/state.db')
    text, marker = ensure_section(text, channel)
    start = text.index(marker)
    end = text.find('\n[', start + 1)
    if end == -1: end = len(text)
    block = text[start:end]
    block = set_key(block, 'enabled', 'true')
    block = set_key(block, 'bind_conversation_id', f'"{conv}"')
    text = text[:start] + block + text[end:]
    print(f'{channel}: bound conversation <set> via {marker}')
config.write_text(text)
PY

rm -f "$DAEMON_LOG" "$EVENT_LOG" "$QQ_OUT_LOG"
"$ONLYNE_BIN" run --debug >"$DAEMON_LOG" 2>&1 &
DAEMON_PID=$!
SUB_PID=""
cleanup() {
  [[ -n "$SUB_PID" ]] && kill "$SUB_PID" 2>/dev/null || true
  kill "$DAEMON_PID" 2>/dev/null || true
  [[ -n "$SUB_PID" ]] && wait "$SUB_PID" 2>/dev/null || true
  wait "$DAEMON_PID" 2>/dev/null || true
}
trap cleanup EXIT

SOCKET=""
for _ in $(seq 1 900); do
  if STATUS=$("$ONLYNE_BIN" client '{"id":"probe","op":"status"}' 2>/dev/null); then
    SOCKET=$(printf '%s' "$STATUS" | python3 -c 'import json,sys; print(json.load(sys.stdin)["data"]["socket"])')
    [[ -S "$SOCKET" && -p .onlyne/channels/qqbot/out ]] && break
  fi
  sleep 0.1
done
[[ -S "$SOCKET" ]] || { echo "daemon socket did not appear; see $DAEMON_LOG" >&2; exit 1; }
[[ -p .onlyne/channels/qqbot/out ]] || { echo "qqbot out FIFO did not appear; see $DAEMON_LOG" >&2; exit 1; }

python3 - "$SOCKET" "$EVENT_LOG" <<'PY' &
import socket, sys
sock_path, log_path = sys.argv[1], sys.argv[2]
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM); s.connect(sock_path)
s.sendall(b'{"id":"sub","op":"subscribe_events"}\n')
with open(log_path, 'a', buffering=1) as f:
    buf = b''
    while True:
        chunk = s.recv(4096)
        if not chunk: break
        buf += chunk
        while b'\n' in buf:
            line, buf = buf.split(b'\n', 1)
            if line: f.write(line.decode('utf-8', 'replace') + '\n')
PY
SUB_PID=$!

LATEST_QQ_IN=$(python3 - <<'PY'
import sqlite3, pathlib
p = pathlib.Path('.onlyne/state.db')
if not p.exists():
    raise SystemExit(0)
con = sqlite3.connect(p)
row = con.execute("select message_id from messages where channel_id='qqbot' and direction like '%inbound%' order by timestamp desc limit 1").fetchone()
print(row[0] if row else '')
PY
)
if [[ -n "$LATEST_QQ_IN" ]]; then
  REQ=$(python3 - "$LATEST_QQ_IN" <<'PY'
import json, sys
print(json.dumps({'id':'consume-current-qq','op':'mark_io_consumed','message_id':sys.argv[1]}))
PY
)
  "$ONLYNE_BIN" client "$REQ" >/dev/null || true
fi

cat .onlyne/channels/qqbot/out >"$QQ_OUT_LOG" &
CAT_PID=$!
sleep 0.5

IFS=',' read -ra channels <<< "$CHANNELS"
for channel in "${channels[@]}"; do
  channel=${channel// /}
  [[ -n "$channel" ]] || continue
  fifo=".onlyne/channels/$channel/in"
  [[ -p "$fifo" ]] || { echo "missing FIFO: $fifo" >&2; exit 1; }
  printf '%s [%s]\n' "$TEXT" "$channel" > "$fifo"
  echo "wrote $fifo"
done

sleep 2
"$ONLYNE_BIN" client '{"id":"hist-after-fifo-write","op":"fetch_all_history","limit":20}' >/dev/null || true
echo "waiting for qqbot inbound via .onlyne/channels/qqbot/out ..."
deadline=$((SECONDS + TIMEOUT))
while kill -0 "$CAT_PID" 2>/dev/null; do
  [[ -s "$QQ_OUT_LOG" ]] && break
  if (( SECONDS >= deadline )); then
    kill "$CAT_PID" 2>/dev/null || true
    wait "$CAT_PID" 2>/dev/null || true
    echo "timed out waiting for qqbot out; ask the QQ target to send any reply to the bot/account" >&2
    echo "daemon log: $DAEMON_LOG" >&2
    echo "event log: $EVENT_LOG" >&2
    tail -20 "$EVENT_LOG" 2>/dev/null || true
    exit 2
  fi
  sleep 1
done
wait "$CAT_PID" 2>/dev/null || true
[[ -s "$QQ_OUT_LOG" ]] || { echo "qqbot out was empty" >&2; exit 2; }

printf 'qqbot out: '
cat "$QQ_OUT_LOG"
