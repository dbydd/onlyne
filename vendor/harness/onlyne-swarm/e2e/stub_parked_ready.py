"""E2E parked-ready stub: a pre-existing online session on a workspace.

Usage: stub_parked_ready.py <workspace_abs> <tree_path> <handle>

Sends swarm_ready once, then sits idle forever (like a human-owned pi
session that stays open in the workspace). It never claims tasks and never
writes out. Regression cover for the 5f459ff5 incident: a live session's
stale ready signal must not steal the delivery of a later dispatch, and the
dispatched task must still open its own terminal and close via out.
"""
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from stub_common import send_ready, wait_daemon

ws = os.path.realpath(sys.argv[1])
handle = sys.argv[3] if len(sys.argv) > 3 else "parked"

wait_daemon(ws)
send_ready(ws, handle)
print("parked ready, idling", flush=True)
while True:
    time.sleep(60)
