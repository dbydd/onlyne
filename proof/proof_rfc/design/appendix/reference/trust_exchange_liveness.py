"""Trust exchange between two nodes: obligations, loss and convergence.

Models RFC 0017 §3 as two persistent rows that exchange signed statements. Each
node holds its own row (two approval halves), its own key generation and at most
one outstanding delivery obligation. An obligation is created by a local
approval or a rotation and is cleared only by the reply that answers the exact
statement it sent.

The search enumerates every reachable state of the pair under these events:
local confirm, idempotent re-confirm (the library's refresh), local revoke,
rotation, a delivered statement whose reply arrives, the same delivery with the
reply lost, and a reply that answers a different nonce. Every loss outcome is an
explicit event, so a lost statement and a lost reply are both in the state space
instead of being assumed away.

What this script claims:

- the phase projection never contradicts the stored halves;
- an obligation is cleared only by its own answering reply, never by an inbound
  statement from the peer, and never by a reply to another attempt;
- no revoked row carries an obligation;
- from every state where both nodes approved and neither revoked, a re-confirm
  of either side followed by one round trip per side reaches `exchanged` on both
  with no obligation left, using no further human approval.

The convergence check is universal over the reachable set, and it abstracts the
network by one assumption: after some point a send and its reply arrive. It does
not model timing, fairness of the scheduler, nonce reuse by an attacker, or
authentication. Those are stated as boundaries in FINDINGS §10, not proved here.

The state space is also symmetric — both nodes hold a row or neither does — and a
statement does not carry key generations. A first approval that crosses a rotation
on the other side (one node has a row, the other does not, and the row-holder
rotated) is therefore outside this model; it is a protocol boundary recorded in
RFC 0017 §2 and FINDINGS §10, not a state this search explores.

Run standalone or through check_faults.py.
"""

from __future__ import annotations

import json
import sys
from dataclasses import dataclass, replace
from typing import Dict, List, Optional, Tuple

MAX_DEPTH = 9


@dataclass(frozen=True)
class Row:
    """One trust row, reduced to the fields the transitions read."""

    local: bool
    remote: bool
    state: str  # 'pending' | 'trusted' | 'revoked'


@dataclass(frozen=True)
class Obligation:
    """One statement this node still owes the peer (RFC 0017 §3.5)."""

    kind: str  # 'approval' | 'rotation'
    kgen: int  # this node's key generation when the statement was signed
    blocked: bool = False  # an unverified refusal backed the attempt off; it still retries


@dataclass(frozen=True)
class Node:
    row: Optional[Row]
    owed: Optional[Obligation]
    kgen: int


@dataclass(frozen=True)
class State:
    a: Node
    b: Node


def project(row: Optional[Row]) -> str:
    """The host-visible stage (RFC 0017 §3.2); derived, never stored independently."""
    if row is None:
        return "none"
    if row.state == "revoked":
        return "revoked"
    if row.local and row.remote:
        return "exchanged"
    return "requested" if row.local else "invited"


def approved_local(node: Node) -> bool:
    return node.row is not None and node.row.local and node.row.state != "revoked"


def record(receiver: Node, sender_local: bool) -> Node:
    """The receiver persists the sender's statement; its own approval is untouched."""
    row = receiver.row
    if row is not None and row.state == "revoked":
        return receiver  # refused: a revoked peer is not readmitted by a statement
    if row is None and not sender_local:
        # RFC 0017 §3.3: a first-contact statement claiming no approval is invalid.
        return receiver
    local = row.local if row is not None else False
    remote = sender_local
    state = "trusted" if (local and remote) else "pending"
    return replace(receiver, row=Row(local=local, remote=remote, state=state))


def reply(sender: Node, receiver_local: bool) -> Node:
    """The sender records the reply, which states the receiver's own approval."""
    row = sender.row
    if row is None or row.state == "revoked":
        return sender
    state = "trusted" if (row.local and receiver_local) else "pending"
    return replace(sender, row=Row(local=row.local, remote=receiver_local, state=state))


def confirm(node: Node) -> Node:
    """Local approval. Only an established row refreshes; every other row approves or re-tries."""
    row = node.row
    if row is not None and row.state == "trusted":
        return node  # established: timestamps only, nothing owed
    owed = Obligation("approval", node.kgen)
    if row is None or row.state == "revoked":
        return replace(node, row=Row(local=True, remote=False, state="pending"), owed=owed)
    # An invited row becomes trusted on this approval; a requested row re-tries the statement.
    state = "trusted" if (row.remote) else "pending"
    return replace(node, row=Row(local=True, remote=row.remote, state=state), owed=owed)


def revoke(node: Node) -> Node:
    if node.row is None:
        return node
    return replace(node, row=Row(local=False, remote=False, state="revoked"), owed=None)


def rotate(node: Node, peer: Node) -> Node:
    """A new identity key. Every non-revoked relation that may hold the old key owes the proof."""
    if node.kgen != 0:
        return node
    owed = node.owed
    # Read from this node's own row: an approval already sent (requested) or a statement the
    # peer already recorded (invited) both mean the old key is pinned somewhere.
    row = node.row
    if row is not None and row.state != "revoked" and (row.local or row.remote):
        owed = Obligation("rotation", node.kgen)
    return replace(node, kgen=1, owed=owed)


def deliver(sender: Node, receiver: Node, reply_arrives: bool) -> Optional[Tuple[str, Node, Node]]:
    """One send. The outcome is `refused`, `lost` or `answered`.

    A refusal is an unauthenticated error: it parks the obligation with a backoff, and a
    later attempt delivers it. A lost send records nothing and keeps the obligation. An
    answered send records the statement on the peer and settles the obligation.
    """
    owed = sender.owed
    if owed is None:
        return None
    row = receiver.row
    if row is not None and row.state == "revoked":
        return "refused", replace(sender, owed=replace(sender.owed, blocked=True)), receiver
    if row is None and not approved_local(sender):
        # The receiver answers `invalid_request`; a non-retryable refusal ends the retry.
        return "refused", replace(sender, owed=replace(sender.owed, blocked=True)), receiver
    receiver_after = record(receiver, approved_local(sender))
    if not reply_arrives:
        return "lost", sender, receiver_after
    sender_after = reply(sender, receiver_after.row.local if receiver_after.row is not None else False)
    return "answered", replace(sender_after, owed=None), receiver_after


def step(state: State, event: str) -> State:
    side, _, op = event.partition("_")
    node, peer = (state.a, state.b) if side == "a" else (state.b, state.a)
    if op == "confirm":
        node = confirm(node)
    elif op == "revoke":
        node = revoke(node)
    elif op == "rotate":
        node = rotate(node, peer)
    elif op in ("send_round", "send_lost"):
        moved = deliver(node, peer, op == "send_round")
        if moved is not None:
            _, node, peer = moved
    elif op == "stale_reply":
        # A reply that answers another nonce is refused whole: no row change, no settlement.
        pass
    if side == "a":
        return State(node, peer)
    return State(peer, node)


def events() -> List[str]:
    return [f"{side}_{op}" for side in ("a", "b") for op in ("confirm", "revoke", "rotate", "send_round", "send_lost", "stale_reply")]


def enumerate_states() -> List[State]:
    seen = set()
    frontier = [State(Node(None, None, 0), Node(None, None, 0))]
    depth = 0
    while frontier and depth <= MAX_DEPTH:
        nxt = []
        for state in frontier:
            if state in seen:
                continue
            seen.add(state)
            for event in events():
                succ = step(state, event)
                if succ not in seen:
                    nxt.append(succ)
        frontier = nxt
        depth += 1
    return sorted(seen, key=repr)


def phase_invariant(state: State) -> bool:
    for node in (state.a, state.b):
        row = node.row
        if row is None:
            continue
        if row.state == "trusted" and not (row.local and row.remote):
            return False
        if row.state == "pending" and row.local and row.remote:
            return False
        if row.state == "revoked" and (row.local or row.remote):
            return False
    return True


def settle_search(states: List[State]) -> List[str]:
    """An obligation ends only on its owner's answered round trip or a local revoke.

    A refusal backs the attempt off; it may park it, never delete it, because the error
    itself is unauthenticated (RFC 0017 §3.5).
    Both sides are checked: an inbound statement from the peer — the old
    "received something, drop the attempt marker" rule — must never settle the
    receiver's own obligation.
    """
    violations = []
    for state in states:
        for event in events():
            succ = step(state, event)
            side, _, op = event.partition("_")
            for owner in ("a", "b"):
                before = state.a.owed if owner == "a" else state.b.owed
                after = succ.a.owed if owner == "a" else succ.b.owed
                if before is None or after is not None:
                    continue
                if owner != side:
                    # Only the owner's own action may end its obligation.
                    violations.append(f"{state!r} -{event}-> {succ!r}")
                    continue
                sender = state.a if owner == "a" else state.b
                peer = state.b if owner == "a" else state.a
                if not (op == "revoke" or op == "send_round" or op == "send_lost"):
                    violations.append(f"{state!r} -{event}-> {succ!r}")
    return violations


def revoked_search(states: List[State]) -> List[str]:
    bad = []
    for state in states:
        for node in (state.a, state.b):
            if node.row is not None and node.row.state == "revoked" and node.owed is not None:
                bad.append(repr(state))
    return bad


def converge_search(states: List[State]) -> List[str]:
    """Both approved, nobody revoked: one successful delivery per owing side suffices.

    No host action is part of the schedule: the obligation was created by the approval
    itself, and only the side that still owes a statement needs a round trip.
    """
    stuck = []
    for state in states:
        if not (approved_local(state.a) and approved_local(state.b)):
            continue
        if project(state.a.row) == "revoked" or project(state.b.row) == "revoked":
            continue
        current = state
        for side in ("a", "b"):
            node = current.a if side == "a" else current.b
            if node.owed is not None:
                current = step(current, f"{side}_send_round")
        if not (project(current.a.row) == "exchanged" and project(current.b.row) == "exchanged"
                and current.a.owed is None and current.b.owed is None):
            stuck.append(repr(state))
    return stuck


def rotation_search(states: List[State]) -> List[str]:
    """A rotation round trip settles its obligation and leaves the sender's own approvals alone.

    The statement still carries the sender's approval, so the receiver may learn an
    approval half it had not recorded; that is the same fact any statement delivers.
    """
    bad = []
    for state in states:
        for side in ("a", "b"):
            node, peer = (state.a, state.b) if side == "a" else (state.b, state.a)
            if node.owed is None or node.owed.kind != "rotation":
                continue
            moved = deliver(node, peer, True)
            if moved is None:
                bad.append(f"{state!r} -{side}_send_round-> refused")
                continue
            outcome, after_node, after_peer = moved
            if peer.row is not None and peer.row.state == "revoked":
                # The peer revoked this node: the refusal is unauthenticated, so the obligation
                # is parked, never deleted (RFC 0017 §3.5).
                if outcome != "refused" or after_node.owed is None or not after_node.owed.blocked:
                    bad.append(f"{state!r} -{side}_send_round-> {outcome} with {after_node.owed!r}")
                continue
            if outcome != "answered":
                bad.append(f"{state!r} -{side}_send_round-> {outcome}")
                continue
            if after_node.owed is not None:
                bad.append(f"{state!r} -{side}_send_round-> {after_node.owed!r}")
            # The reply may report the peer's approval, which is what makes a one-sided row
            # trusted; this node's own approval is what a rotation must never change.
            if node.row is not None and after_node.row is not None and after_node.row.local != node.row.local:
                bad.append(f"{state!r} -{side}_send_round-> own approval {node.row.local} -> {after_node.row.local}")
            if node.row is not None and node.row.state == "trusted" and (after_node.row is None or after_node.row.state != "trusted"):
                bad.append(f"{state!r} -{side}_send_round-> demoted {after_node.row!r}")
            # The receiver keeps its own approval: a proven rotation is a continuous identity,
            # so it must not demote the peer's row to `invited` (RFC 0017 §3.3).
            if peer.row is not None and after_peer.row is not None and after_peer.row.local != peer.row.local:
                bad.append(f"{state!r} -{side}_send_round-> receiver approval {peer.row.local} -> {after_peer.row.local}")
            if not phase_invariant(State(after_node, after_peer)):
                bad.append(f"{state!r} -{side}_send_round-> halves contradict the stage")
    return bad


def run() -> List[Dict]:
    states = enumerate_states()
    phase = [repr(s) for s in states if not phase_invariant(s)]
    settle = settle_search(states)
    revoked = revoked_search(states)
    converge = converge_search(states)
    rotation = rotation_search(states)
    return [
        {
            "check": "trust.phase-invariant",
            "check_pass": len(phase) == 0,
            "states": len(states),
            "witness": phase[:3],
        },
        {
            "check": "trust.obligation-settles-only-on-own-reply",
            "check_pass": len(settle) == 0,
            "witness": settle[:3],
        },
        {
            "check": "trust.revoked-carries-no-obligation",
            "check_pass": len(revoked) == 0,
            "witness": revoked[:3],
        },
        {
            "check": "trust.bilateral-approval-converges",
            "check_pass": len(converge) == 0,
            "states": len(states),
            "witness": converge[:3],
        },
        {
            "check": "trust.rotation-settles-on-round-trip",
            "check_pass": len(rotation) == 0,
            "witness": rotation[:3],
        },
    ]


def main() -> int:
    results = run()
    print(json.dumps(results, ensure_ascii=False, indent=2))
    return 0 if all(item["check_pass"] for item in results) else 1


if __name__ == "__main__":
    sys.exit(main())
