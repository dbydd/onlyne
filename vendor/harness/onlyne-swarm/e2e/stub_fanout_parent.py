"""E2E-2 fanout parent stub: claim task -> spawn 2 children -> out -> exit.

Usage: stub_fanout_parent.py <workspace_abs> <tree_path> <handle> <marker> <e2e_root>
       [delay_between_children_secs]

Children are written directly to each target's loopback/in FIFO. Each child
carries transfer_send_to=<parent> as lineage only. The parent does NOT wait:
it writes its own out and exits immediately. Fan-in happens in files, not in
the session. Exits 0 iff both children were spawned and the out succeeds.
"""
import os
import sys
import time
import uuid

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from stub_common import (header_fields, is_scheduler_delivery, reply_task,
                         send_ready, wait_daemon, wait_for_task)

ws = os.path.realpath(sys.argv[1])
tree_path = sys.argv[2]
handle = sys.argv[3]
marker = sys.argv[4]
e2e_root = os.path.realpath(sys.argv[5])
gap = float(sys.argv[6]) if len(sys.argv) > 6 else 0.0

wait_daemon(ws)
send_ready(ws, handle)

text = wait_for_task(
    ws,
    lambda t, m: is_scheduler_delivery(t) and marker in t,
)
if not text:
    print("TIMEOUT waiting for parent task", flush=True)
    sys.exit(1)
parent, _ = header_fields(text)
print("PARENT:", parent, flush=True)

children = []
for i, target in enumerate(["b", "c"]):
    cid = str(uuid.uuid4())
    children.append(cid)
    body = (f"---swarm\ntask_id: {cid}\nfrom: {tree_path}\n"
            f"transfer_send_to: {parent}\nattempt: 1\n---\n"
            f"swarm-child-{marker} for {target}\n")
    fifo = os.path.join(e2e_root, ".ws", target,
                        ".onlyne/channels/loopback/in")
    fd = os.open(fifo, os.O_WRONLY)
    os.write(fd, body.encode())
    os.close(fd)
    print("child sent:", cid, "->", target, flush=True)
    if gap and i == 0:
        time.sleep(gap)

print("children:", len(children), children, flush=True)
ok = reply_task(ws, tree_path, parent, "",
                f"fanout spawned: {len(children)}/2")
sys.exit(0 if ok else 1)
