"""E2E-2 fanout child stub: claim one swarm-child task -> out -> exit.

Usage: stub_fanout_child.py <workspace_abs> <tree_path> <handle> <marker>

Matches inbound ---swarm tasks containing `swarm-child-<marker>`.
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from stub_common import (header_fields, is_scheduler_delivery, reply_task,
                         send_ready, wait_daemon, wait_for_task)

ws = os.path.realpath(sys.argv[1])
tree_path = sys.argv[2]
handle = sys.argv[3]
marker = sys.argv[4]

wait_daemon(ws)
send_ready(ws, handle)

text = wait_for_task(
    ws,
    lambda t, m: is_scheduler_delivery(t) and f"swarm-child-{marker}" in t,
)
if not text:
    print("TIMEOUT waiting for child task", flush=True)
    sys.exit(1)
print("CHILD TASK:", text[:200].replace("\n", " | "), flush=True)
task_id, transfer = header_fields(text)
if not task_id:
    print("NO task_id in child task", flush=True)
    sys.exit(1)
ok = reply_task(ws, tree_path, task_id, transfer or "",
                f"stub reply from {tree_path}")
sys.exit(0 if ok else 1)
