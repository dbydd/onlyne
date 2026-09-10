"""E2E-1 stub agent: single task claim -> out -> exit.

Usage: stub_e2e1.py <workspace_abs> <tree_path> <handle> <marker>

Claims the newest inbound ---swarm task whose body contains <marker>
(so parallel runs on one tree do not steal each other's tasks),
writes the out message (done signal), exits 0 on success, 1 on timeout.
Fire-and-forget: no waiting for anything downstream.
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from stub_common import (header_fields, is_scheduler_delivery, reply_task,
                         send_ready, wait_daemon, wait_for_task)

ws = os.path.realpath(sys.argv[1])
tree_path = sys.argv[2]
handle = sys.argv[3]
marker = sys.argv[4] if len(sys.argv) > 4 else ""

wait_daemon(ws)
send_ready(ws, handle)

text = wait_for_task(
    ws,
    lambda t, m: is_scheduler_delivery(t) and marker in t
    and "stub reply" not in t,
)
if not text:
    print("TIMEOUT waiting for task", flush=True)
    sys.exit(1)
print("TASK:", text[:300].replace("\n", " | "), flush=True)
task_id, transfer = header_fields(text)
if not task_id:
    print("NO task_id in task", flush=True)
    sys.exit(1)
ok = reply_task(ws, tree_path, task_id, transfer or "",
                f"stub reply from {tree_path}")
sys.exit(0 if ok else 1)
