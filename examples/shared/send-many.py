#!/usr/bin/env python3
import json, os, pathlib, socket, sys, time

start = pathlib.Path.cwd()
for d in (start, *start.parents):
    sock = d / '.onlyne/run/onlyne.sock'
    if sock.exists():
        break
else:
    sock = start / '.onlyne/run/onlyne.sock'
text = os.environ.get('ONLYNE_TEXT', 'zig')
fmt = os.environ.get('ONLYNE_FORMAT', 'plain')
attachments_raw = os.environ.get('ONLYNE_ATTACHMENTS', '[]')
try:
    attachments = json.loads(attachments_raw)
except json.JSONDecodeError as e:
    raise SystemExit(f'bad ONLYNE_ATTACHMENTS JSON: {e}')
raw = os.environ.get('ONLYNE_TARGETS', '')
if not raw.strip():
    raise SystemExit('set ONLYNE_TARGETS as channel:conversation[,channel:conversation...]')
items = []
for part in raw.split(','):
    channel, sep, conv = part.partition(':')
    if not sep or not channel or not conv:
        raise SystemExit(f'bad target {part!r}; want channel:conversation')
    items.append((channel, conv))
with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
    s.connect(str(sock))
    for n, (channel, conv) in enumerate(items, 1):
        req = {"id": f"send-{n}", "op":"send_message", "channel_id":channel, "conversation_id":conv, "text":text, "format":fmt, "attachments":attachments}
        s.sendall((json.dumps(req, ensure_ascii=False) + '\n').encode())
        line = b''
        while not line.endswith(b'\n'):
            chunk = s.recv(4096)
            if not chunk:
                raise SystemExit('socket closed')
            line += chunk
        print(line.decode().strip())
