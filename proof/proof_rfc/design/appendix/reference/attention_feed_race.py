"""Attention feed: page position vs obligation status.

Model: one owner, one member, three obligations with stable ids
(thread_ref, message_ref, recipient) committed atomically with their entries at
seq 101..103 (RFC 0003 L58). The owner pushes one hint per obligation. The member
enumerates the authenticated list in pages of 2, then acks; ack only flips
pending -> acked. The owner and the member stay healthy: no restart, no stream
loss, so the restart-gated reconciliation pass of RFC 0003 L66 is never reached
inside a trace.

Lines under test
  0004 L83-84  list pages in a stable order; an ack changes only the
               obligation's ack status and does not change the meaning of an
               already-issued position; the cursor stays usable after the
               unacked set changes.
  0003 L58-64  obligation id, atomic commit with the entry, survives close,
               authenticated paginated list, ack idempotent per obligation.
  0003 L66     reconcile unacked obligations against every known owner after
               restart or long-connection interruption. No other trigger.
  0002 L34     a closed thread keeps unacked obligations in the member's list.

Two faults
  P1 cursor_passed  the cursor moved past a pending obligation, so no later page
                    can contain it. 0004 L84 forbids this.
  P2 no_trigger     the hint was lost, the member has finished a read cycle, and
                    the only mandated recovery is restart-gated, so nothing
                    reveals the obligation. 0003 L66 has no live trigger.

Runs (one defect isolated per run)
  current           offset over the unacked projection (acks renumber positions),
                    restart-only reconciliation, reliable hints.
  no_reconcile      stable seq cursor, restart-only reconciliation, lossy hints.
                    Positions are fixed, so only the trigger duty is missing.
  hardened          stable seq cursor plus periodic mandatory reconciliation.
  control           offset over the committed list: ack only flips status, so an
                    ack of an earlier obligation cannot hide a later one.

Bounds: 3 obligations, page_size 2, 2 pages, 1 reconciliation pass, depth 12.
Not a whole-RFC claim.
"""

from __future__ import annotations

import json
import sys
from collections import deque
from dataclasses import dataclass, replace
from enum import Enum
from typing import Dict, FrozenSet, List, Optional, Tuple

IDS: Tuple[str, ...] = ("o1", "o2", "o3")
SEQ: Dict[str, int] = {"o1": 101, "o2": 102, "o3": 103}
PAGE_SIZE = 2
MAX_PAGES = 2
MAX_RECONCILES = 1
MAX_DEPTH = 12


class Kind(str, Enum):
    CURRENT = "current"
    HARDENED = "hardened"
    CONTROL = "control"


@dataclass(frozen=True)
class State:
    committed: Tuple[str, ...] = ()
    delivered: FrozenSet[str] = frozenset()
    dropped: FrozenSet[str] = frozenset()
    seen: FrozenSet[str] = frozenset()
    acked: FrozenSet[str] = frozenset()
    off: int = 0
    cur_seq: int = 0
    snap: int = 0
    pages: int = 0
    exhausted: bool = False
    reconciles: int = 0


@dataclass(frozen=True)
class Edge:
    name: str
    state: State
    mandatory: bool


class Model:
    def __init__(
        self,
        kind: Kind,
        cursor: str,
        lossy_hints: bool,
        periodic_reconcile: bool,
        scenario: str,
        expect: bool,
        note: str,
        probe: Optional[List[str]] = None,
    ) -> None:
        self.kind = kind
        self.cursor = cursor
        self.lossy = lossy_hints
        self.periodic = periodic_reconcile
        self.scenario = scenario
        self.expect = expect
        self.note = note
        self.probe = probe

    # -- projections -------------------------------------------------------

    def pending(self, s: State) -> Tuple[str, ...]:
        return tuple(o for o in s.committed if o not in s.acked)

    def unseen(self, s: State) -> Tuple[str, ...]:
        return tuple(o for o in self.pending(s) if o not in s.seen)

    # -- list reads --------------------------------------------------------

    def page(self, s: State) -> Tuple[List[str], State]:
        """One list read under this run's cursor rule."""
        if self.cursor == "unacked_offset":
            remaining = list(self.pending(s))
            window = remaining[s.off : s.off + PAGE_SIZE]
            return window, replace(
                s,
                off=s.off + len(window),
                pages=s.pages + 1,
                seen=s.seen | frozenset(window),
                exhausted=len(window) < PAGE_SIZE,
            )
        if self.cursor == "committed_offset":
            window = list(s.committed[s.off : s.off + PAGE_SIZE])
            return window, replace(
                s,
                off=s.off + len(window),
                pages=s.pages + 1,
                seen=s.seen | frozenset(window),
                exhausted=len(window) < PAGE_SIZE,
            )
        # stable seq cursor: positions are seq values, so ack cannot move them
        snap = s.snap or (SEQ[s.committed[-1]] if s.committed else 0)
        window = [o for o in s.committed if s.cur_seq < SEQ[o] <= snap][:PAGE_SIZE]
        if not window:
            later = [o for o in s.committed if SEQ[o] > snap]
            if later:
                snap = SEQ[later[-1]]
                window = [o for o in later if SEQ[o] <= snap][:PAGE_SIZE]
        cur_seq = SEQ[window[-1]] if window else s.cur_seq
        return window, replace(
            s,
            snap=snap,
            cur_seq=cur_seq,
            pages=s.pages + 1,
            seen=s.seen | frozenset(window),
            exhausted=len(window) < PAGE_SIZE,
        )

    # -- transitions -------------------------------------------------------

    def edges(self, s: State) -> List[Edge]:
        out: List[Edge] = []
        if len(s.committed) < len(IDS):
            nid = IDS[len(s.committed)]
            base = replace(s, committed=s.committed + (nid,))
            out.append(
                Edge(f"commit_{nid}",
                     replace(base, delivered=s.delivered | {nid}), True)
            )
            if self.lossy:
                out.append(
                    Edge(f"commit_{nid}_hint_lost",
                         replace(base, dropped=s.dropped | {nid}), False)
                )
        # the push hint is what makes the member read the list
        if s.pages < MAX_PAGES and any(o in s.delivered and o not in s.seen for o in s.committed):
            _, nxt = self.page(s)
            out.append(Edge("page", nxt, False))
        for o in sorted(s.seen):
            if o not in s.acked:
                out.append(Edge(f"ack_{o}", replace(s, acked=s.acked | {o}), True))
        if self.periodic and s.reconciles < MAX_RECONCILES:
            fresh = self.unseen(s)
            if fresh:
                out.append(
                    Edge("reconcile",
                         replace(s,
                                 seen=s.seen | frozenset(fresh),
                                 reconciles=s.reconciles + 1), True)
                )
        return out

    # -- fault predicates --------------------------------------------------

    def cursor_passed(self, s: State, o: str) -> bool:
        """P1: no later page can contain this pending obligation."""
        if o in s.acked or o in s.seen:
            return False
        if self.cursor == "unacked_offset":
            remaining = self.pending(s)
            return o in remaining and remaining.index(o) < s.off
        if self.cursor == "committed_offset":
            return s.committed.index(o) < s.off
        return SEQ[o] <= s.cur_seq

    def no_trigger(self, s: State, o: str) -> bool:
        """P2: pending, unseen, hint lost, read cycle done, no reconciliation duty."""
        if self.periodic or s.pages < 1:
            return False
        return o in s.committed and o not in s.acked and o not in s.seen and o not in s.delivered

    def bad(self, s: State) -> bool:
        return any(
            self.cursor_passed(s, o) or self.no_trigger(s, o) for o in self.unseen(s)
        )

    def stable(self, s: State) -> bool:
        """No enabled mandatory action clears the fault."""
        if not self.bad(s):
            return False
        for e in self.edges(s):
            if e.mandatory and not self.bad(e.state):
                return False
        return True

    # -- search ------------------------------------------------------------

    def reach(self, s: State) -> Tuple[int, int, int, int, int]:
        return (len(s.committed), s.pages, len(s.seen), len(s.acked), s.reconciles)

    def explore(self):
        start = State()
        parents: Dict[State, Tuple[Optional[State], Optional[str]]] = {start: (None, None)}
        depth: Dict[State, int] = {start: 0}
        queue: deque = deque([start])
        explored = 0
        first_bad: Optional[State] = None
        first_stable: Optional[State] = None
        furthest = start

        while queue:
            s = queue.popleft()
            explored += 1
            if self.reach(s) > self.reach(furthest):
                furthest = s
            if first_bad is None and self.bad(s):
                first_bad = s
            if first_stable is None and self.stable(s):
                first_stable = s
            if first_bad is not None and first_stable is not None:
                break
            if depth[s] >= MAX_DEPTH:
                continue
            for e in self.edges(s):
                if e.state in parents:
                    continue
                parents[e.state] = (s, e.name)
                depth[e.state] = depth[s] + 1
                queue.append(e.state)

        return parents, first_bad, first_stable, furthest, explored

    def trace_of(self, parents, state: State) -> List[str]:
        out: List[str] = []
        while parents[state][0] is not None:
            state, name = parents[state]
            out.append(name)
        return list(reversed(out))

    # -- fixed interleaving ------------------------------------------------

    def follow(self, names: List[str]) -> Optional[State]:
        s = State()
        for name in names:
            nxt = next((e.state for e in self.edges(s) if e.name == name), None)
            if nxt is None:
                return None
            s = nxt
        return s

    def probe_result(self) -> Optional[Dict]:
        if not self.probe:
            return None
        s = self.follow(self.probe)
        if s is None:
            return {"steps": list(self.probe), "valid": False}
        items, _ = self.page(s) if s.committed else ([], s)
        return {
            "steps": list(self.probe),
            "valid": True,
            "pending_unseen": list(self.unseen(s)),
            "lost_by_cursor": [o for o in self.unseen(s) if self.cursor_passed(s, o)],
            "seen": sorted(s.seen),
            "next_page_returns": list(items),
        }

    # -- reporting ---------------------------------------------------------

    def result(self) -> Dict:
        parents, first_bad, witness, furthest, explored = self.explore()
        found = witness is not None
        check_pass = found == self.expect

        if not check_pass:
            classification = "model_error"
        elif witness is None:
            classification = "chosen_implementation"
        elif any(self.cursor_passed(witness, o) for o in self.unseen(witness)):
            classification = "spec_conflict"
        elif any(self.no_trigger(witness, o) for o in self.unseen(witness)):
            classification = "missing_duty"
        else:
            classification = "model_error"

        shown = witness or first_bad or furthest
        page_items, _ = self.page(shown) if shown.committed else ([], shown)
        final = {
            "committed": list(shown.committed),
            "pending": list(self.pending(shown)),
            "pending_unseen": list(self.unseen(shown)),
            "acked": sorted(shown.acked),
            "cursor": {"off": shown.off, "cur_seq": shown.cur_seq, "snapshot_seq": shown.snap},
            "pages_taken": shown.pages,
            "list_exhausted": shown.exhausted,
            "next_page_returns": list(page_items),
            "lost_by_cursor": [o for o in self.unseen(shown) if self.cursor_passed(shown, o)],
            "stranded_by_trigger": [o for o in self.unseen(shown) if self.no_trigger(shown, o)],
            "reconciliation_duty": "periodic" if self.periodic else "restart-or-stream-loss only",
            "canonical_interleaving": self.probe_result(),
            "scope": "bounded obligation/feed model; not a whole-RFC claim",
        }

        return {
            "scenario": self.scenario,
            "kind": {"current": "regression_pre_fix", "hardened": "current"}.get(
                self.kind.value, self.kind.value
            ),
            "classification": classification,
            "bounds": {
                "obligations": len(IDS),
                "page_size": PAGE_SIZE,
                "max_pages": MAX_PAGES,
                "max_reconciliations": MAX_RECONCILES,
                "max_depth": MAX_DEPTH,
            },
            "states_explored": explored,
            "witness_expected": self.expect,
            "check_pass": check_pass,
            "trace": self.trace_of(parents, witness) if witness else [],
            "final": final,
            "note": self.note,
        }


def run() -> List[Dict]:
    return [
        Model(
            Kind.CURRENT, "unacked_offset", lossy_hints=False, periodic_reconcile=False,
            scenario="attention_feed_race", expect=True,
            note=(
                "The cursor is an offset into the unacked projection, so it counts "
                "only obligations that are still unacked at read time. In the "
                "shortest trace a page advances the offset past o1, o2 is committed "
                "and hinted, and acking o1 renumbers o2 to index 0 behind the "
                "offset: no later page can return it. 0004 L84 requires an ack to "
                "change only status and the issued cursor to stay usable after the "
                "unacked set changes. The loss is permanent in-trace because 0003 "
                "L66 reconciles only after a restart or stream loss."
            ),
            probe=["commit_o1", "commit_o2", "commit_o3", "page", "ack_o1", "page"],
        ).result(),
        Model(
            Kind.CURRENT, "stable_seq", lossy_hints=True, periodic_reconcile=False,
            scenario="attention_feed_race_no_reconcile_trigger", expect=True,
            note=(
                "Positions are fixed here, so the only defect is the missing "
                "trigger: the member finished its read cycle, the next obligation's "
                "hint was lost (0004 L87: hints only), and 0003 L66's only live "
                "recovery is restart-gated. The obligation stays pending and "
                "unseen with no duty that would reveal it."
            ),
        ).result(),
        Model(
            Kind.HARDENED, "stable_seq", lossy_hints=True, periodic_reconcile=True,
            scenario="attention_feed_race", expect=False,
            note=(
                "Snapshot seq cursor keeps positions fixed across acks; a lost "
                "hint is covered by the periodic reconciliation duty. No stall in "
                "the bounded search. The 1-pass bound only keeps the search finite: "
                "the duty is periodic, so no bound value creates a stall."
            ),
            probe=["commit_o1", "commit_o2", "commit_o3", "page", "ack_o1", "page"],
        ).result(),
        Model(
            Kind.CONTROL, "committed_offset", lossy_hints=False, periodic_reconcile=False,
            scenario="attention_feed_race", expect=False,
            note=(
                "Same offset envelope, but positions index the committed list and "
                "an ack only flips status, so acking an earlier obligation cannot "
                "move a later one behind the cursor. Acking earlier items does not "
                "hide later ones."
            ),
            probe=["commit_o1", "commit_o2", "commit_o3", "page", "ack_o1", "page"],
        ).result(),
    ]


def main() -> int:
    results = run()
    ok = all(r["check_pass"] for r in results)
    print(json.dumps(
        {"scenario": "attention_feed_race", "results": results, "all_checks_pass": ok},
        ensure_ascii=False, indent=2,
    ))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
