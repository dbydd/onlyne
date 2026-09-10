"""Reclaim-probe stub: claims a task, waits for the recycle signal, acks it.

Usage: stub_reclaim.py <workspace_abs> <tree_path> <handle> <marker>

Amendment-3 path: after claiming, the stub blocks on the downlink
`---swarm-ctl recycle` wire (not on model work), sends the uplink
`swarm_recycled` ack, then exits 0 WITHOUT writing an out. The scheduler
must observe the ack and close the tab; the task must NOT be requeued
(no out was ever written, so no done row may appear for it).
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from stub_common import (header_fields, is_scheduler_delivery, send_ready,
                         send_recycled, wait_daemon, wait_for_ctl, wait_for_task)

ws = os.path.realpath(sys.argv[1])
tree_path = sys.argv[2]
handle = sys.argv[3]
marker = sys.argv[4] if len(sys.argv) > 4 else ""

wait_daemon(ws)
send_ready(ws, handle)

text = wait_for_task(
    ws,
    lambda t, m: is_scheduler_delivery(t) and marker in t,
)
if not text:
    print("TIMEOUT waiting for task", flush=True)
    sys.exit(1)
task_id, _ = header_fields(text)
print("CLAIMED:", task_id, flush=True)

ctl = wait_for_ctl(ws, timeout=60)
if not ctl:
    print("TIMEOUT waiting for recycle signal", flush=True)
    sys.exit(1)
print("CTL:", ctl.replace("\n", " | ")[:160], flush=True)
if task_id not in ctl:
    print("CTL names a different task; ignoring", flush=True)
    sys.exit(1)

ok = send_recycled(ws, task_id, handle, "quit:test-recycle")
sys.exit(0 if ok else 1)
