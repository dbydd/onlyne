"""Backlog expiry: permitted terminal disposition vs a duty to produce one.

Scenario
  One resource endpoint owns the delivery obligations of one IM conversation.
  Two platform events are persisted; the assignee is permanently unreachable, so
  neither delivery nor `delivery_ack` ever happens (0012 L102). The binding is
  never changed by the failure, and the owner and endpoint stay healthy. Only
  retention can end the obligations.

Lines under test
  0006 L188  handover commit requires zero backlog; otherwise the submit returns
             `conflict` and the caller must clear the backlog first.
  0006 L190  backlog clears only by recipient ack or by an explicit terminal
             disposition of the obligation under the retention policy. There is
             no transfer operation.
  0006 L200  after retention deletes content the event answers `not_found`.
             That is content retention; it is not a terminal disposition of the
             delivery obligation.
  0012 L176  queue depth and retention period are declared by the endpoint.
             Handover and reassignment stay refused "until these events are
             acknowledged or given a clear terminal state".
  0012 L124  the same clearing rule restated for the IM node.

Fault
  P1 expiry_not_required: an unacked obligation has passed its retention
  deadline and no terminal disposition exists. In current a disposition is
  permitted but not mandatory: no transition is required to produce one, so an
  endpoint that implements only content retention (0006 L200) keeps the
  obligation forever. The backlog stays non-empty, every handover submission
  conflicts (0006 L188), and nothing else forbids that state. The stall is stable
  under the contract's fairness: only mandatory actions carry a progress duty.

Runs
  current   expiry enabled at the retention deadline, not mandatory.
  control   expiry mandatory at the retention deadline; every bad state has a
            mandatory action that strictly shrinks the backlog.

Both runs treat the retention clock and event arrival as compulsory: the
declared retention period ends (0012 L176). Only the terminal disposition is
optional in current, which is the gap under test.

Bounds: 2 events, depth 8. The model assumes a declared retention period
(0012 L176), not that the period produces a terminal disposition. Ack delivery,
endpoint restart and reassignment are outside it. Not a whole-RFC claim.
"""

from __future__ import annotations

import json
import sys
from collections import deque
from dataclasses import dataclass, replace
from enum import Enum
from typing import Dict, FrozenSet, List, Optional, Tuple

EVENTS: Tuple[str, ...] = ("e1", "e2")
MAX_DEPTH = 8
ASSIGNEE = "urn:onlyne:agent:lazuli"
RESOURCE = {"owner": "urn:onlyne:gateway:im", "id": "chat-01"}


class Kind(str, Enum):
    CURRENT = "current"
    CONTROL = "control"


@dataclass(frozen=True)
class State:
    received: int = 0
    terminal: FrozenSet[str] = frozenset()
    elapsed: FrozenSet[str] = frozenset()
    handover: str = "open"
    handover_rejected: bool = False
    revision: int = 0
    endpoint_online: bool = True
    owner_online: bool = True
    recipient_reachable: bool = False


@dataclass(frozen=True)
class Edge:
    name: str
    state: State
    mandatory: bool


class Model:
    def __init__(
        self,
        kind: Kind,
        expiry_mandatory: bool,
        scenario: str,
        expect: bool,
        note: str,
        probe: Optional[List[str]] = None,
    ) -> None:
        self.kind = kind
        self.expiry_mandatory = expiry_mandatory
        self.scenario = scenario
        self.expect = expect
        self.note = note
        self.probe = probe

    # -- projections -------------------------------------------------------

    def live(self, s: State) -> Tuple[str, ...]:
        return EVENTS[: s.received]

    def backlog(self, s: State) -> Tuple[str, ...]:
        """Delivery obligations without ack or terminal disposition."""
        return tuple(e for e in self.live(s) if e not in s.terminal)

    def bad(self, s: State) -> bool:
        """An unacked obligation outlived its retention deadline.

        Scoped to the unchanged binding: after a committed handover the events
        belong to the new assignee, which this model does not represent.
        """
        if s.handover != "open":
            return False
        return any(e in s.elapsed for e in self.backlog(s))

    def progress(self, s: State, t: State) -> bool:
        """A resolving step: the backlog strictly shrinks."""
        return self.bad(s) and len(self.backlog(t)) < len(self.backlog(s))

    # -- transitions -------------------------------------------------------

    def edges(self, s: State) -> List[Edge]:
        out: List[Edge] = []
        if s.received < len(EVENTS):
            out.append(
                Edge(
                    f"receive_{EVENTS[s.received]}",
                    replace(s, received=s.received + 1),
                    True,
                )
            )
        for e in self.live(s):
            if e not in s.elapsed:
                out.append(
                    Edge(
                        f"retention_deadline_{e}",
                        replace(s, elapsed=s.elapsed | {e}),
                        True,
                    )
                )
        for e in self.backlog(s):
            if e in s.elapsed:
                out.append(
                    Edge(
                        f"retention_expire_{e}",
                        replace(s, terminal=s.terminal | {e}),
                        self.expiry_mandatory,
                    )
                )
        if s.handover == "open":
            if self.backlog(s):
                out.append(Edge("handover_submit", replace(s, handover_rejected=True), True))
            else:
                out.append(
                    Edge(
                        "handover_submit",
                        replace(s, handover="committed", revision=s.revision + 1),
                        True,
                    )
                )
        return out

    # -- stall predicate ---------------------------------------------------

    def stable(self, s: State) -> bool:
        """Bad and no enabled mandatory action that shrinks the backlog."""
        if not self.bad(s):
            return False
        return not any(
            e.mandatory and self.progress(s, e.state) for e in self.edges(s)
        )

    # -- search ------------------------------------------------------------

    def reach(self, s: State) -> Tuple[int, int, int, int, int]:
        return (
            s.received,
            len(s.terminal),
            len(s.elapsed),
            s.revision,
            int(s.handover != "open"),
        )

    def explore(self):
        start = State()
        parents: Dict[State, Tuple[Optional[State], Optional[str]]] = {
            start: (None, None)
        }
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
        return {
            "steps": list(self.probe),
            "valid": True,
            "backlog": list(self.backlog(s)),
            "terminal_dispositions": sorted(s.terminal),
            "binding": {
                "resource_ref": RESOURCE,
                "assignee": ASSIGNEE,
                "revision": s.revision,
                "handover": s.handover,
            },
            "handover_rejected": s.handover_rejected,
            "stable": self.stable(s),
        }

    # -- reporting ---------------------------------------------------------

    def result(self) -> Dict:
        parents, first_bad, witness, furthest, explored = self.explore()
        found = witness is not None
        probe = self.probe_result()
        check_pass = found == self.expect
        if probe is not None:
            check_pass = check_pass and probe["valid"] and probe["stable"] == self.expect

        if not check_pass:
            classification = "model_error"
        elif witness is None:
            classification = "chosen_implementation"
        else:
            classification = "missing_duty"

        shown = witness or first_bad or furthest
        final = {
            "received": list(self.live(shown)),
            "backlog": list(self.backlog(shown)),
            "retention_elapsed": sorted(shown.elapsed),
            "terminal_dispositions": sorted(shown.terminal),
            "recipient_reachable": shown.recipient_reachable,
            "owner_online": shown.owner_online,
            "endpoint_online": shown.endpoint_online,
            "binding": {
                "resource_ref": RESOURCE,
                "assignee": ASSIGNEE,
                "revision": shown.revision,
                "handover": shown.handover,
            },
            "delivery_attempts": 0,
            "acks": 0,
            "handover_action": (
                "receive conflict (0006 L188)"
                if self.backlog(shown)
                else "commit allowed"
            ),
            "handover_rejected": shown.handover_rejected,
            "unresolved_after_deadline": [
                e for e in self.backlog(shown) if e in shown.elapsed
            ],
            "stable_stall": witness is not None,
            "enabled_actions": [
                f"{e.name}{' (mandatory)' if e.mandatory else ' (optional)'}"
                for e in self.edges(shown)
            ],
            "mandatory_progress_actions": [
                e.name
                for e in self.edges(shown)
                if e.mandatory and self.progress(shown, e.state)
            ],
            "expiry_duty": (
                "mandatory at retention deadline"
                if self.expiry_mandatory
                else "permitted at retention deadline, not mandatory"
            ),
            "scenario_probe": probe,
            "scope": "bounded two-obligation backlog model; not a whole-RFC claim",
        }

        return {
            "scenario": self.scenario,
            "kind": {
                "current": "regression_pre_fix",
                "hardened": "current",
                "control": "current",
            }.get(self.kind.value, self.kind.value),
            "classification": classification,
            "bounds": {
                "events": len(EVENTS),
                "max_depth": MAX_DEPTH,
                "recipient": "permanently offline: no delivery, no ack",
                "endpoint_restarts": 0,
                "handover_commits": 1,
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
            Kind.CURRENT,
            expiry_mandatory=False,
            scenario="backlog_expiry_liveness",
            expect=True,
            note=(
                "0006 L188 refuses handover while any delivery obligation is "
                "unconfirmed. L190 allows only an ack or a terminal disposition "
                "under retention, and L200 only defines what content retention "
                "does to a lookup. No clause makes the terminal disposition "
                "mandatory once the declared retention period ends (0012 L176). "
                "So after retention_deadline_e1 the backlog remains, every "
                "handover submit conflicts, and the only clearing edge is "
                "optional: a compliant endpoint that never records a disposition "
                "stalls permanently with owner and endpoint healthy. The shortest "
                "trace is two steps; the probe shows the same stall with both "
                "events received."
            ),
            probe=[
                "receive_e1",
                "receive_e2",
                "retention_deadline_e1",
                "retention_deadline_e2",
                "handover_submit",
            ],
        ).result(),
        Model(
            Kind.CONTROL,
            expiry_mandatory=True,
            scenario="backlog_expiry_liveness",
            expect=False,
            note=(
                "Same obligations and the same handover fence, but the terminal "
                "disposition becomes mandatory when the retention deadline is "
                "reached. No stable stall exists: at every bad state the "
                "retention_expire edge is mandatory and strictly shrinks the "
                "backlog. The probe clears both obligations and commits the "
                "handover at revision 1, which is what the fence permits once the "
                "backlog is empty (0006 L188-192)."
            ),
            probe=[
                "receive_e1",
                "receive_e2",
                "retention_deadline_e1",
                "retention_deadline_e2",
                "retention_expire_e1",
                "retention_expire_e2",
                "handover_submit",
            ],
        ).result(),
    ]


def main() -> int:
    results = run()
    ok = all(r["check_pass"] for r in results)
    print(
        json.dumps(
            {
                "scenario": "backlog_expiry_liveness",
                "results": results,
                "all_checks_pass": ok,
            },
            ensure_ascii=False,
            indent=2,
        )
    )
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
