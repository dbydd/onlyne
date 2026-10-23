"""Proof: owner result committed → pending notice optional/lossy.

Scope of this model (deliberately small, not whole-RFC correctness):

  owner result committed, notice optional/lossy, connection healthy,
  local known in {pending, committed, unknown, expired}, owner evidence retained.

  Legacy mandatory relation: idle only. No required query back to the owner,
  no bounded local wait. Notices are fire-and-forget projections.

Bad predicate:
  owner result == committed
  AND receiver's local knowledge of that notice == pending
  AND no enabled *mandatory* resolving action exists.

A state satisfying it with every mandatory successor still satisfying it is a
stable stall: nothing in the model's mandatory relation can move the notice out
of pending. Optional querying *would* recover (it is enabled and would reach
committed), so the finding is a missing duty, not an impossibility.

Fixed variant: adds a mandatory refresh on pending and a mandatory query on
unknown/expired. This is a proposal, not RFC text.

Control variant: notices are projections of owner evidence; reading commits
directly, so pending never exists. It must NOT exhibit the bad predicate.

Finite bounds are modeling bounds, not findings: max_notices caps how many
distinct notices the owner may produce, max_queue caps the unread backlog,
max_depth caps BFS distance.
"""

from __future__ import annotations

import json
import sys
from collections import deque
from dataclasses import dataclass, replace
from enum import Enum
from typing import Dict, List, Optional, Set, Tuple


class OwnerState(Enum):
    UNCOMPUTED = "uncomputed"
    COMMITTED = "committed"


class LocalState(Enum):
    UNKNOWN = "unknown"
    PENDING = "pending"
    COMMITTED = "committed"
    EXPIRED = "expired"


class Kind(str, Enum):
    LEGACY = "legacy"
    FIXED = "fixed"
    CONTROL = "control"


@dataclass(frozen=True)
class State:
    owner: OwnerState
    queue: frozenset  # notice ids delivered but not read
    local: Tuple[Tuple[str, LocalState], ...]  # notice id -> local knowledge
    acked: frozenset  # notice ids optionally acknowledged
    seq: int  # next notice id counter

    def __hash__(self) -> int:
        return hash((self.owner, self.queue, self.local, self.acked, self.seq))

    def local_state(self, nid: str) -> Optional[LocalState]:
        for k, v in self.local:
            if k == nid:
                return v
        return None

    def with_local(self, nid: str, st: LocalState) -> "State":
        entries = {k: v for k, v in self.local}
        entries[nid] = st
        return replace(self, local=tuple(sorted(entries.items())))

    def pending_ids(self) -> List[str]:
        return [k for k, v in self.local if v == LocalState.PENDING]

    def unknown_or_expired_ids(self) -> List[str]:
        return [k for k, v in self.local if v in (LocalState.UNKNOWN, LocalState.EXPIRED)]


@dataclass(frozen=True)
class Edge:
    name: str
    state: State
    mandatory: bool


class Model:
    def __init__(self, kind: Kind, max_notices: int = 2, max_queue: int = 2):
        self.kind = kind
        self.max_notices = max_notices
        self.max_queue = max_queue

    # -- transitions -------------------------------------------------------

    def initial(self) -> State:
        return State(
            owner=OwnerState.UNCOMPUTED,
            queue=frozenset(),
            local=(),
            acked=frozenset(),
            seq=0,
        )

    def _new_notice_id(self, s: State) -> str:
        return f"n{s.seq}"

    def edges(self, s: State) -> List[Edge]:
        out: List[Edge] = []

        # Owner computes and retains evidence. Mandatory.
        if s.owner == OwnerState.UNCOMPUTED:
            out.append(Edge("compute_result", replace(s, owner=OwnerState.COMMITTED), True))

        # Owner pushes a notice. Delivered is the healthy path; lost models the
        # "optional/lossy" property (receiver ends in unknown, never saw it).
        if s.owner == OwnerState.COMMITTED and s.seq < self.max_notices:
            nid = self._new_notice_id(s)
            nxt = replace(s, seq=s.seq + 1)
            if len(s.queue) < self.max_queue:
                out.append(Edge("push_notice_delivered", replace(nxt, queue=s.queue | {nid}), True))
            if self.kind != Kind.CONTROL:
                out.append(Edge("push_notice_lost", nxt.with_local(nid, LocalState.UNKNOWN), True))

        # Receiver reads a delivered notice.
        for nid in sorted(s.queue):
            if self.kind == Kind.CONTROL:
                # Projection semantics: reading commits against owner evidence.
                out.append(Edge("read_notice", replace(s, queue=s.queue - {nid}).with_local(nid, LocalState.COMMITTED), True))
            else:
                out.append(Edge("read_notice", replace(s, queue=s.queue - {nid}).with_local(nid, LocalState.PENDING), True))

        if self.kind != Kind.CONTROL:
            # Optional acknowledgment. Never resolves; not mandatory.
            for nid in s.pending_ids():
                if nid not in s.acked:
                    out.append(Edge("acknowledge_optional", replace(s, acked=s.acked | {nid}), False))

            # Optional query of owner evidence. Would recover pending/unknown.
            for nid in s.pending_ids() + s.unknown_or_expired_ids():
                if s.owner == OwnerState.COMMITTED:
                    out.append(Edge("query_owner_optional", s.with_local(nid, LocalState.COMMITTED), False))

            # Lossy notice channel ages an unread pending notice out. This is the
            # channel dropping its copy, not the agent forgetting durable state.
            for nid in s.pending_ids():
                out.append(Edge("expire_notice_ttl", s.with_local(nid, LocalState.EXPIRED), False))

        if self.kind == Kind.FIXED:
            # Proposed rule: mandatory refresh of pending notices.
            for nid in s.pending_ids():
                if s.owner == OwnerState.COMMITTED:
                    out.append(Edge("mandatory_refresh_pending", s.with_local(nid, LocalState.COMMITTED), True))
            # Proposed rule: mandatory query of unknown/expired notices.
            for nid in s.unknown_or_expired_ids():
                if s.owner == OwnerState.COMMITTED:
                    out.append(Edge("mandatory_query_unknown_expired", s.with_local(nid, LocalState.COMMITTED), True))

        return out

    # -- predicate ---------------------------------------------------------

    def mandatory_resolving_actions(self, s: State, nid: str) -> Set[str]:
        """Enabled mandatory action names that move nid out of pending."""
        if s.local_state(nid) != LocalState.PENDING:
            return set()
        names = set()
        for e in self.edges(s):
            if not e.mandatory:
                continue
            if e.state.local_state(nid) != LocalState.PENDING:
                names.add(e.name)
        return names

    def bad_predicate(self, s: State) -> bool:
        if s.owner != OwnerState.COMMITTED:
            return False
        for nid in s.pending_ids():
            if not self.mandatory_resolving_actions(s, nid):
                return True
        return False

    def stable_stall(self, s: State) -> bool:
        """Bad predicate holds and every mandatory successor still holds it."""
        if not self.bad_predicate(s):
            return False
        for e in self.edges(s):
            if e.mandatory and not self.bad_predicate(e.state):
                return False
        return True

    # -- search ------------------------------------------------------------

    def explore(self, max_depth: int) -> Tuple[List[str], int, Optional[State], Optional[State], State]:
        """FIFO BFS. Returns (shortest bad trace, explored count, bad state, stable state, furthest state)."""
        start = self.initial()
        queue: deque = deque([(start, ())])
        visited: Set[State] = {start}
        explored = 0
        first_bad: Optional[Tuple[State, Tuple[str, ...]]] = None
        first_stable: Optional[Tuple[State, Tuple[str, ...]]] = None
        furthest = start

        def reach(s: State) -> Tuple[int, int, int]:
            return (1 if s.owner == OwnerState.COMMITTED else 0, s.seq, len(s.local))

        while queue:
            state, trace = queue.popleft()
            explored += 1
            if reach(state) > reach(furthest):
                furthest = state

            if self.bad_predicate(state) and first_bad is None:
                first_bad = (state, trace)
            if self.stable_stall(state) and first_stable is None:
                first_stable = (state, trace)
            if first_bad is not None and first_stable is not None:
                break
            if len(trace) >= max_depth:
                continue

            for e in self.edges(state):
                if e.state in visited:
                    continue
                visited.add(e.state)
                queue.append((e.state, trace + (e.name,)))

        if first_bad is None:
            return [], explored, None, None, furthest
        return (
            list(first_bad[1]),
            explored,
            first_bad[0],
            (first_stable[0] if first_stable else None),
            furthest,
        )

    # -- reporting ---------------------------------------------------------

    def result(self, max_depth: int) -> Dict:
        trace, explored, bad, stable, furthest = self.explore(max_depth)
        witness = stable is not None
        expected = self.kind == Kind.LEGACY
        check_pass = witness == expected

        shown = stable or bad or furthest

        if not check_pass:
            classification = "model_error"  # the variant contradicted its expectation
        elif self.kind == Kind.LEGACY:
            classification = "missing_duty"
        else:
            classification = "chosen_implementation"

        final: Dict = {
            "owner": shown.owner.value,
            "pending_notices": len(shown.pending_ids()),
            "unknown_notices": len(shown.unknown_or_expired_ids()),
            "queued_notices": len(shown.queue),
            "optional_query_available": bool(
                any(
                    e.name == "query_owner_optional" and e.state.local_state(nid) == LocalState.COMMITTED
                    for nid in shown.pending_ids()
                    for e in self.edges(shown)
                )
            ),
            "finding": (
                "stable stall in mandatory relation; optional query would recover, so missing duty"
                if witness
                else "no stable stall in this model"
            ),
            "scope": "bounded notice model only; not a whole-RFC claim",
        }

        return {
            "scenario": "owner_committed_pending_notice",
            "kind": self.kind.value,
            "classification": classification,
            "bounds": {
                "max_notices": self.max_notices,
                "max_queue": self.max_queue,
                "max_depth": max_depth,
            },
            "states_explored": explored,
            "witness_expected": expected,
            "check_pass": check_pass,
            "trace": trace,
            "final": final,
        }


def run() -> List[Dict]:
    max_depth = 12
    results = [
        Model(Kind.LEGACY).result(max_depth),
        Model(Kind.FIXED).result(max_depth),
        Model(Kind.CONTROL).result(max_depth),
    ]
    return results


def main() -> int:
    results = run()
    print(json.dumps({"scenario": "owner_committed_pending_notice", "results": results,
                      "all_checks_pass": all(r["check_pass"] for r in results)}, indent=2))
    return 0 if all(r["check_pass"] for r in results) else 1


if __name__ == "__main__":
    sys.exit(main())
