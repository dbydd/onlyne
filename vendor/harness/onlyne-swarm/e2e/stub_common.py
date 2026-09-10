"""Shared helpers for the onlyne-swarm headless stub agents (E2E tests).

Each stub speaks the workspace daemon's Unix-socket JSON protocol directly:
ping -> swarm_ready handshake -> poll loopback history -> reply via
send_message to the loopback channel. No pi / model / orca needed.

Amendment-1: fire-and-forget. `transfer_send_to` is lineage only
(which task spawned this one). There are no callbacks, no waiting, no
parent bookkeeping. Each hop completes (out) and exits immediately.
"""
import json
import os
import socket
import sys
import time


def daemon_sock(ws):
    return os.path.join(ws, ".onlyne/run/s")


def rpc(ws, req, recv_bytes=65536):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(daemon_sock(ws))
    s.sendall((json.dumps(req) + "\n").encode())
    buf = b""
    while b"\n" not in buf:
        buf += s.recv(recv_bytes)
    s.close()
    return json.loads(buf.decode())


def wait_daemon(ws, tries=100):
    for _ in range(tries):
        try:
            if rpc(ws, {"id": "p", "op": "ping"}).get("ok"):
                return
        except Exception:
            time.sleep(0.2)
    print("no daemon", ws, flush=True)
    sys.exit(1)


def send_ready(ws, handle):
    r = rpc(ws, {
        "id": "r",
        "op": "swarm_ready",
        "text": json.dumps({"workspace": ws, "terminal_handle": handle}),
    })
    print("ready:", r.get("ok"), flush=True)
    return r.get("ok")


def fetch_loopback(ws, limit=20):
    r = rpc(ws, {
        "id": "h",
        "op": "fetch_channel_history",
        "channel_id": "loopback",
        "limit": limit,
    })
    return r.get("data") or []


def is_scheduler_delivery(text):
    """True for the scheduler's second delivery, never raw relay or out."""
    if not text.startswith("---swarm\n"):
        return False
    head, sep, _ = text.partition("\n---")
    return bool(sep) and any(
        line.strip() == "delivery: scheduler" for line in head.split("\n")
    )


def header_fields(text):
    """Parse a ---swarm header. Returns (task_id, transfer_send_to) or (None, None)."""
    task_id = None
    transfer = None
    for line in text.split("\n"):
        if line.startswith("task_id:"):
            task_id = line.split(":", 1)[1].strip()
        elif line.startswith("transfer_send_to:"):
            transfer = line.split(":", 1)[1].strip()
    return task_id, transfer


def reply_task(ws, tree_path, task_id, transfer_send_to, body_text):
    """Write the out message for one hop: done signal, then exit. No waiting."""
    body = (
        f"---swarm\ntask_id: {task_id}\nfrom: {tree_path}\n"
        f"transfer_send_to: {transfer_send_to}\nattempt: 1\n---\n{body_text}\n"
    )
    r = rpc(ws, {
        "id": "s",
        "op": "send_message",
        "channel_id": "loopback",
        "text": body,
    })
    print("reply:", r.get("ok"), task_id, flush=True)
    return r.get("ok")


def wait_for_task(ws, predicate, timeout=120, limit=10, poll=1.0):
    """Poll loopback history until predicate(text, msg) matches. Returns text."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        for m in fetch_loopback(ws, limit):
            t = m.get("text") or ""
            if m.get("direction") == "inbound" and predicate(t, m):
                return t
        time.sleep(poll)
    return ""


def send_recycled(ws, task_id, handle, reason):
    """Uplink ack for the reclaim protocol (mirrors pi-onlyne swarmRecycled)."""
    r = rpc(ws, {
        "id": "recycled",
        "op": "swarm_recycled",
        "text": __import__("json").dumps({"task_id": task_id, "terminal_handle": handle, "reason": reason}),
    })
    print("recycled:", r.get("ok"), task_id, flush=True)
    return r.get("ok")


def wait_for_ctl(ws, timeout=30, poll=0.5):
    """Poll loopback history for a ---swarm-ctl recycle wire. Returns text."""
    import time as _t
    deadline = _t.time() + timeout
    while _t.time() < deadline:
        for m in fetch_loopback(ws, 10):
            t = m.get("text") or ""
            if m.get("direction") == "inbound" and t.startswith("---swarm-ctl"):
                return t
        _t.sleep(poll)
    return ""
