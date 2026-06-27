#!/usr/bin/env bash
set -euo pipefail
CHANNEL=${ONLYNE_CHANNEL:?set ONLYNE_CHANNEL}
ENV_PREFIX=${ONLYNE_ENV_PREFIX:-${CHANNEL^^}}
SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
WORKSPACE=${ONLYNE_WORKSPACE:-}
RUN_DIR=${WORKSPACE:-$PWD}
ONLYNE_BIN=${ONLYNE_BIN:-$SCRIPT_DIR/../../target/debug/onlyne}
TEXT=${ONLYNE_TEXT:-zig}
TIMEOUT=${ONLYNE_TIMEOUT:-180}
EVENT_LOG=${ONLYNE_EVENT_LOG:-$RUN_DIR/${CHANNEL}-events.ndjson}
DAEMON_LOG=${ONLYNE_DAEMON_LOG:-$RUN_DIR/${CHANNEL}-daemon.log}
CONV_VAR="ONLYNE_${ENV_PREFIX}_CONVERSATION_ID"
CONV=${!CONV_VAR:-}

if [[ "${1:-}" == "--local-check" ]]; then
  command -v python3 >/dev/null
  bash -n "$0"
  echo "local-check ok"
  exit 0
fi

[[ -x "$ONLYNE_BIN" ]] || { echo "onlyne binary not found: $ONLYNE_BIN" >&2; exit 1; }
command -v python3 >/dev/null || { echo "python3 required" >&2; exit 1; }
if [[ -n "$WORKSPACE" ]]; then mkdir -p "$WORKSPACE"; cd "$WORKSPACE"; fi
"$ONLYNE_BIN" init >/dev/null
python3 "$SCRIPT_DIR/enable_adapter.py" "$CHANNEL"
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

echo "waiting for $CHANNEL daemon..."
SOCKET=""
for _ in $(seq 1 900); do
  if STATUS=$("$ONLYNE_BIN" client '{"id":"probe","op":"status"}' 2>/dev/null); then
    SOCKET=$(printf '%s' "$STATUS" | python3 -c 'import json,sys; print(json.load(sys.stdin)["data"]["socket"])')
    [[ -S "$SOCKET" ]] && break
  fi
  sleep 0.1
done
[[ -n "$SOCKET" && -S "$SOCKET" ]] || { echo "daemon socket did not appear; see $DAEMON_LOG" >&2; exit 1; }

python3 - "$SOCKET" "$EVENT_LOG" <<'PY' &
import socket, sys
sock_path, log_path = sys.argv[1], sys.argv[2]
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM); s.connect(sock_path)
s.sendall(b'{"id":"sub","op":"subscribe_events"}\n')
with open(log_path, 'a', buffering=1) as f:
    buf=b''
    while True:
        chunk=s.recv(4096)
        if not chunk: break
        buf += chunk
        while b'\n' in buf:
            line, buf = buf.split(b'\n', 1)
            if line: f.write(line.decode('utf-8','replace')+'\n')
PY
SUB_PID=$!
sleep 0.5
"$ONLYNE_BIN" client '{"id":"status","op":"status"}'
"$ONLYNE_BIN" client '{"id":"channels","op":"list_channels"}'

[[ -n "$CONV" ]] || { echo "set $CONV_VAR to the single configured conversation id" >&2; exit 3; }
python3 - "$CHANNEL" "$CONV" <<'PY'
import pathlib, sys
channel, conv = sys.argv[1], sys.argv[2]
path = pathlib.Path('.onlyne/config.toml')
text = path.read_text()
needle = f'[adapters.{channel}]\n'
if needle not in text:
    raise SystemExit(f'missing {needle.strip()} in {path}')
lines = text.splitlines()
out = []
in_section = False
wrote = False
for line in lines:
    if line.startswith('[adapters.'):
        if in_section and not wrote:
            out.append(f'bind_conversation_id = "{conv}"')
            wrote = True
        in_section = line == needle.strip()
    if in_section and line.startswith('bind_conversation_id ='):
        if not wrote:
            out.append(f'bind_conversation_id = "{conv}"')
            wrote = True
        continue
    out.append(line)
if in_section and not wrote:
    out.append(f'bind_conversation_id = "{conv}"')
path.write_text('\n'.join(out) + '\n')
PY

echo "sending '$TEXT' to bound $CHANNEL conversation: $CONV"
REQ=$(python3 - "$CHANNEL" "$TEXT" <<'PY'
import json, sys
print(json.dumps({"id":"send","op":"send_message","channel_id":sys.argv[1],"text":sys.argv[2]}, ensure_ascii=False))
PY
)
"$ONLYNE_BIN" client "$REQ"
sleep 1
"$ONLYNE_BIN" client "{\"id\":\"hist-channel\",\"op\":\"fetch_channel_history\",\"channel_id\":\"$CHANNEL\",\"limit\":20}"
"$ONLYNE_BIN" client '{"id":"hist-all","op":"fetch_all_history","limit":20}'
tail -n 20 "$EVENT_LOG" || true
