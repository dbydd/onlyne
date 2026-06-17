#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd -- "$SCRIPT_DIR/../.." && pwd)
WORKSPACE=${ONLYNE_WORKSPACE:-$SCRIPT_DIR}
ONLYNE_BIN=${ONLYNE_BIN:-$REPO_ROOT/target/debug/onlyne}
TEXT=${ONLYNE_TEXT:-zig}
TIMEOUT=${ONLYNE_TIMEOUT:-180}
EVENT_LOG=${ONLYNE_EVENT_LOG:-$WORKSPACE/wechat-events.ndjson}
DAEMON_LOG=${ONLYNE_DAEMON_LOG:-$WORKSPACE/wechat-daemon.log}

usage() {
  cat <<'USAGE'
Usage:
  ./smoke-wechat.sh [--local-check]

One-shot WeChat CLI smoke. Default outbound text is: zig

Environment:
  ONLYNE_BIN=/path/to/onlyne              default: ../../target/debug/onlyne
  ONLYNE_WORKSPACE=/path/to/workspace     default: examples/wechat
  ONLYNE_WECHAT_CONVERSATION_ID=<peer>    optional; if absent, wait for inbound WeChat text
  ONLYNE_TEXT=zig                         outbound text
  ONLYNE_TIMEOUT=180                      seconds to wait for inbound event
  ONLYNE_EVENT_LOG=/path/events.ndjson    event capture file

Flow:
  1. init workspace
  2. require WeChat auth in cwd/.onlyne
  3. start daemon
  4. subscribe to events
  5. wait for inbound conversation if needed
  6. send "zig" (or ONLYNE_TEXT) once
  7. fetch WeChat and all-channel history
  8. stop daemon and exit
USAGE
}

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  usage
  exit 0
fi

if [[ "${1:-}" == "--local-check" ]]; then
  command -v python3 >/dev/null
  [[ -f "$SCRIPT_DIR/smoke-wechat.sh" ]]
  bash -n "$SCRIPT_DIR/smoke-wechat.sh"
  echo "local-check ok"
  exit 0
fi

if [[ ! -x "$ONLYNE_BIN" ]]; then
  echo "onlyne binary not found: $ONLYNE_BIN" >&2
  echo "Build first: cargo build" >&2
  exit 1
fi
command -v python3 >/dev/null || { echo "python3 required" >&2; exit 1; }
mkdir -p "$WORKSPACE"
cd "$WORKSPACE"

"$ONLYNE_BIN" init >/dev/null

if ! grep -Eq '^WEIXIN_ILINK_TOKEN=.+' .onlyne/.env 2>/dev/null; then
  cat >&2 <<EOF2
No WEIXIN_ILINK_TOKEN found in $WORKSPACE/.onlyne/.env
Run from this directory first:
  "$ONLYNE_BIN" auth weixin
  # or
  "$ONLYNE_BIN" auth weixin --token '<token>'
EOF2
  exit 2
fi

python3 - <<'PY'
from pathlib import Path
p = Path('.onlyne/config.toml')
s = p.read_text()
marker = '[adapters.weixin]'
i = s.index(marker)
j = s.find('\n[', i + 1)
if j == -1:
    j = len(s)
block = s[i:j]
if 'enabled = true' not in block:
    lines = block.splitlines()
    for n, line in enumerate(lines):
        if line.startswith('enabled = '):
            lines[n] = 'enabled = true'
            break
    else:
        lines.insert(1, 'enabled = true')
    s = s[:i] + '\n'.join(lines) + s[j:]
    p.write_text(s)
PY

rm -f "$EVENT_LOG" "$DAEMON_LOG"
"$ONLYNE_BIN" run --debug >"$DAEMON_LOG" 2>&1 &
DAEMON_PID=$!
SUB_PID=""
cleanup() {
  [[ -n "$SUB_PID" ]] && kill "$SUB_PID" 2>/dev/null || true
  kill "$DAEMON_PID" 2>/dev/null || true
  wait "$SUB_PID" 2>/dev/null || true
  wait "$DAEMON_PID" 2>/dev/null || true
}
trap cleanup EXIT

rm -f .onlyne/run/onlyne.sock
echo "waiting for daemon socket (WeChat startup may long-poll)..."
for _ in $(seq 1 900); do
  if [[ -S .onlyne/run/onlyne.sock ]] && "$ONLYNE_BIN" client '{"id":"probe","op":"ping"}' >/dev/null 2>&1; then
    break
  fi
  sleep 0.1
done
if ! [[ -S .onlyne/run/onlyne.sock ]] || ! "$ONLYNE_BIN" client '{"id":"probe","op":"ping"}' >/dev/null 2>&1; then
  echo "daemon socket did not become ready; see $DAEMON_LOG" >&2
  exit 1
fi

python3 - "$WORKSPACE/.onlyne/run/onlyne.sock" "$EVENT_LOG" <<'PY' &
import socket, sys
sock_path, log_path = sys.argv[1], sys.argv[2]
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect(sock_path)
s.sendall(b'{"id":"sub","op":"subscribe_events"}\n')
with open(log_path, 'a', buffering=1) as f:
    buf = b''
    while True:
        chunk = s.recv(4096)
        if not chunk:
            break
        buf += chunk
        while b'\n' in buf:
            line, buf = buf.split(b'\n', 1)
            if line:
                f.write(line.decode('utf-8', 'replace') + '\n')
PY
SUB_PID=$!
sleep 0.5

echo '--- status ---'
"$ONLYNE_BIN" client '{"id":"status","op":"status"}'
echo '--- channels ---'
"$ONLYNE_BIN" client '{"id":"channels","op":"list_channels"}'

CONV=${ONLYNE_WECHAT_CONVERSATION_ID:-}
if [[ -z "$CONV" ]]; then
  echo "Send any text to the WeChat bot now. After inbound is captured, Onlyne will send: $TEXT"
  CONV=$(python3 - "$EVENT_LOG" "$TIMEOUT" <<'PY'
import json, pathlib, sys, time
path = pathlib.Path(sys.argv[1]); timeout = int(sys.argv[2]); end = time.time() + timeout; seen = 0
while time.time() < end:
    if path.exists():
        lines = path.read_text(errors='replace').splitlines()
        for line in lines[seen:]:
            try:
                obj = json.loads(line)
            except Exception:
                continue
            if obj.get('type') == 'inbound_message':
                data = obj.get('data', {}).get('data', {})
                if data.get('channel_id') == 'weixin' and data.get('conversation_id'):
                    print(data['conversation_id'])
                    raise SystemExit(0)
        seen = len(lines)
    time.sleep(0.5)
raise SystemExit(1)
PY
) || { echo "no inbound WeChat message captured; see $EVENT_LOG and $DAEMON_LOG" >&2; exit 3; }
fi

echo "--- sending '$TEXT' to weixin conversation: $CONV ---"
REQ=$(python3 - "$CONV" "$TEXT" <<'PY'
import json, sys
print(json.dumps({"id":"send","op":"send_message","channel_id":"weixin","conversation_id":sys.argv[1],"text":sys.argv[2]}, ensure_ascii=False))
PY
)
SEND_OUT=$("$ONLYNE_BIN" client "$REQ")
echo "$SEND_OUT"
python3 - "$SEND_OUT" <<'PY'
import json, sys
obj = json.loads(sys.argv[1])
if not obj.get('ok'):
    raise SystemExit(obj.get('error', {}).get('message', 'send failed'))
PY

sleep 1

echo '--- weixin history ---'
"$ONLYNE_BIN" client '{"id":"hist-channel","op":"fetch_channel_history","channel_id":"weixin","limit":20}'
echo '--- all history ---'
"$ONLYNE_BIN" client '{"id":"hist-all","op":"fetch_all_history","limit":20}'
echo "--- event log tail: $EVENT_LOG ---"
tail -n 20 "$EVENT_LOG" || true
