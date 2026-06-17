#!/usr/bin/env bash
set -euo pipefail
CHANNEL=${ONLYNE_CHANNEL:?set ONLYNE_CHANNEL}
ENV_PREFIX=${ONLYNE_ENV_PREFIX:-${CHANNEL^^}}
SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
WORKSPACE=${ONLYNE_WORKSPACE:-$PWD}
ONLYNE_BIN=${ONLYNE_BIN:-$SCRIPT_DIR/../../target/debug/onlyne}
TEXT=${ONLYNE_TEXT:-zig}
TIMEOUT=${ONLYNE_TIMEOUT:-180}
EVENT_LOG=${ONLYNE_EVENT_LOG:-$WORKSPACE/${CHANNEL}-events.ndjson}
DAEMON_LOG=${ONLYNE_DAEMON_LOG:-$WORKSPACE/${CHANNEL}-daemon.log}
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
mkdir -p "$WORKSPACE"
cd "$WORKSPACE"
"$ONLYNE_BIN" init >/dev/null
python3 "$SCRIPT_DIR/enable_adapter.py" "$CHANNEL"
rm -f "$EVENT_LOG" "$DAEMON_LOG" .onlyne/run/onlyne.sock
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
for _ in $(seq 1 900); do
  if [[ -S .onlyne/run/onlyne.sock ]] && "$ONLYNE_BIN" client '{"id":"probe","op":"ping"}' >/dev/null 2>&1; then break; fi
  sleep 0.1
done
[[ -S .onlyne/run/onlyne.sock ]] || { echo "daemon socket did not appear; see $DAEMON_LOG" >&2; exit 1; }

python3 - "$WORKSPACE/.onlyne/run/onlyne.sock" "$EVENT_LOG" <<'PY' &
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

if [[ -z "$CONV" ]]; then
  echo "Send any text to $CHANNEL now; Onlyne will reply with: $TEXT"
  CONV=$(python3 - "$EVENT_LOG" "$TIMEOUT" "$CHANNEL" <<'PY'
import json, pathlib, sys, time
path=pathlib.Path(sys.argv[1]); end=time.time()+int(sys.argv[2]); channel=sys.argv[3]; seen=0
while time.time() < end:
    if path.exists():
        lines=path.read_text(errors='replace').splitlines()
        for line in lines[seen:]:
            try: obj=json.loads(line)
            except Exception: continue
            if obj.get('type') == 'inbound_message':
                data=obj.get('data',{}).get('data',{})
                if data.get('channel_id') == channel and data.get('conversation_id'):
                    print(data['conversation_id']); raise SystemExit(0)
        seen=len(lines)
    time.sleep(0.5)
raise SystemExit(1)
PY
) || { echo "no inbound $CHANNEL message captured; see $EVENT_LOG and $DAEMON_LOG" >&2; exit 3; }
fi

echo "sending '$TEXT' to $CHANNEL conversation: $CONV"
REQ=$(python3 - "$CHANNEL" "$CONV" "$TEXT" <<'PY'
import json, sys
print(json.dumps({"id":"send","op":"send_message","channel_id":sys.argv[1],"conversation_id":sys.argv[2],"text":sys.argv[3]}, ensure_ascii=False))
PY
)
"$ONLYNE_BIN" client "$REQ"
sleep 1
"$ONLYNE_BIN" client "{\"id\":\"hist-channel\",\"op\":\"fetch_channel_history\",\"channel_id\":\"$CHANNEL\",\"limit\":20}"
"$ONLYNE_BIN" client '{"id":"hist-all","op":"fetch_all_history","limit":20}'
tail -n 20 "$EVENT_LOG" || true
