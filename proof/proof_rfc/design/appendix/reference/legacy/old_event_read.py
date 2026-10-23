#!/usr/bin/env python3
"""Bounded state search: does an IM handover strand retained event content?

Scenario
--------
A platform event is first persistently received by the IM endpoint while agent A
is the assignee. The endpoint records the event reference, the binding revision
and the receiving agent, i.e. the event's ``firstOwner`` is frozen at receive
(RFC 0006:178-185). The assignee then moves A -> B through one committed
handover. The retained payload is never expired -- this model has no expire
action -- and no action revokes read rights.

The reading under the current RFCs
----------------------------------
RFC 0012:48 authorizes ``read`` / ``list_events`` "by current assignee", and
RFC 0012:129 narrows that to "当前 assignee，按事件归属过滤" (filtered by event
attribution). One defensible reading of the conjunction is that a caller must be
both the current assignee and the recorded first owner. Under it, after A -> B:

* A is the recorded first owner but no longer the current assignee;
* B is the current assignee but not the recorded first owner;

so neither node passes, and because every fetch action is gated on that same
relation, the payload is never fetched by anyone. RFC 0006:185 promises the
target "可以按授权读取这些事件", but no RFC text grants that authorization, and no
text says the first owner keeps read access over retained content. That gap
between an access promise and the authorization rule is the modeled conflict.

Model scope
-----------
* Kinds: ``legacy`` (intersection reading), ``fixed`` (proposal: the first owner
  keeps read over retained content, the target gets material scope), ``control``
  (legacy reading with handover disabled).
* Bad predicate: content retained AND payload fetched by nobody AND no node in
  {A, B} may read.
* Handover is modeled as its committed transition only; the prepare/accept steps
  of RFC 0006 §5 do not change read authorization. At most one handover is
  modeled, per the A -> B move this scenario is about.
* Search is BFS over an immutable state with a visited set, so a witness trace
  is the shortest one. Counts and limits are reported as ``bounds``.
* A witness state has no enabled resolving action in this transition system.
  That is a stable stall *inside the model*, not a claim of unavoidable
  deadlock: a reverse handover or an explicit historical-read grant would
  recover it, and neither is modeled because neither is an RFC action.
* The search is bounded, so absence of a witness means "none inside these
  bounds", never "the RFC is correct". No result claims whole-RFC correctness.
* ``fixed`` and ``control`` carry ``chosen_implementation``: they report a
  proposed or deliberately restricted reading, not an RFC finding. Only
  ``legacy`` reports a spec finding.

Sources: RFC 0006:178-185, RFC 0012:48, RFC 0012:129.
"""

from __future__ import annotations

import json
import sys
from collections import deque
from dataclasses import dataclass, replace
from typing import Callable, Dict, List, Optional, Tuple

AGENTS: Tuple[str, ...] = ("A", "B")
FIRST_OWNER = "A"
CONTENT_RETAINED = True

BOUNDS: Dict[str, object] = {
    "max_states": 256,
    "max_depth": 4,
    "max_handovers": 1,
    "agents": list(AGENTS),
    "expiry_actions": 0,
    "revoke_actions": 0,
}

SCENARIO_HANDOVER = "im_event_read_after_handover"
SCENARIO_NO_HANDOVER = "im_event_read_without_handover"

SOURCES: List[str] = [
    "docs/rfcs/0006-resources-and-handover.md:178-185",
    "docs/rfcs/0012-im-resource-node.md:48",
    "docs/rfcs/0012-im-resource-node.md:129",
]


@dataclass(frozen=True)
class State:
    """Frozen search state. ``first_owner`` is set at receive and never rewritten."""

    first_owner: str
    current: str
    fetched_a: bool
    fetched_b: bool
    handovers: int


ReadAccess = Callable[[State, str], bool]
StatePredicate = Callable[[State], bool]


def _can_read_legacy(state: State, node: str) -> bool:
    """Intersection reading: caller is current assignee and recorded first owner."""
    return node == state.current and node == state.first_owner


def _can_read_fixed(state: State, node: str) -> bool:
    """Proposal: first owner keeps read over retained content; target gets scope."""
    return node == state.current or node == state.first_owner


def _fetched(state: State, node: str) -> bool:
    return state.fetched_a if node == "A" else state.fetched_b


def _with_fetch(state: State, node: str) -> State:
    if node == "A":
        return replace(state, fetched_a=True)
    return replace(state, fetched_b=True)


def _transitions(
    state: State, can_read: ReadAccess, allow_handover: bool
) -> List[Tuple[str, State]]:
    """Named transitions enabled in ``state``; nothing expires or revokes access."""
    moves: List[Tuple[str, State]] = []
    if (
        allow_handover
        and state.current == state.first_owner
        and state.handovers < int(BOUNDS["max_handovers"])
    ):
        moves.append(
            (
                "handover_A_to_B",
                replace(state, current="B", handovers=state.handovers + 1),
            )
        )
    for node in AGENTS:
        if not _fetched(state, node) and can_read(state, node):
            moves.append((f"fetch_{node}", _with_fetch(state, node)))
    return moves


def _bad_predicate(state: State, can_read: ReadAccess) -> bool:
    if not CONTENT_RETAINED:
        return False
    if state.fetched_a or state.fetched_b:
        return False
    return not any(can_read(state, node) for node in AGENTS)


def _search(
    initial: State,
    goal: StatePredicate,
    can_read: ReadAccess,
    allow_handover: bool,
) -> Tuple[Optional[State], Optional[List[str]], int]:
    visited = {initial}
    queue = deque([(initial, ())])
    explored = 0
    while queue and explored < int(BOUNDS["max_states"]):
        state, trace = queue.popleft()
        explored += 1
        if goal(state):
            return state, list(trace), explored
        if len(trace) >= int(BOUNDS["max_depth"]):
            continue
        for name, successor in _transitions(state, can_read, allow_handover):
            if successor not in visited:
                visited.add(successor)
                queue.append((successor, trace + (name,)))
    return None, None, explored


def _resolving_actions(
    state: State, can_read: ReadAccess, allow_handover: bool
) -> List[str]:
    return [
        name
        for name, successor in _transitions(state, can_read, allow_handover)
        if successor.fetched_a or successor.fetched_b
    ]


def _summary(
    state: State, can_read: ReadAccess, allow_handover: bool, reported: str
) -> Dict[str, object]:
    return {
        "reported_state": reported,
        "first_owner": state.first_owner,
        "current_assignee": state.current,
        "content_retained": CONTENT_RETAINED,
        "payload_fetched": {"A": state.fetched_a, "B": state.fetched_b},
        "can_read": [node for node in AGENTS if can_read(state, node)],
        "enabled_actions": [
            name for name, _ in _transitions(state, can_read, allow_handover)
        ],
        "resolving_actions": _resolving_actions(state, can_read, allow_handover),
    }


def _legacy_loss_probe(state: State) -> bool:
    """The state the legacy reading loses access in: committed handover, nothing fetched."""
    return (
        state.handovers == 1
        and state.current == "B"
        and not state.fetched_a
        and not state.fetched_b
    )


def _result(
    kind: str,
    classification: str,
    scenario: str,
    can_read: ReadAccess,
    allow_handover: bool,
    witness_expected: bool,
    probe_goal: Optional[StatePredicate],
) -> Dict[str, object]:
    initial = State(FIRST_OWNER, FIRST_OWNER, False, False, 0)
    witness, trace, explored = _search(
        initial, lambda s: _bad_predicate(s, can_read), can_read, allow_handover
    )

    if witness is not None:
        # A stall claim needs no enabled action that would produce a fetch.
        check_pass = witness_expected and not _resolving_actions(
            witness, can_read, allow_handover
        )
        reported_state, reported_trace = "witness", trace
        state = witness
    else:
        check_pass = not witness_expected
        probe: Optional[State] = None
        probe_trace: Optional[List[str]] = None
        if probe_goal is not None:
            probe, probe_trace, _ = _search(
                initial, probe_goal, can_read, allow_handover
            )
        if probe is not None:
            state, reported_trace, reported_state = probe, probe_trace, "probe"
        else:
            state, reported_trace, reported_state = initial, [], "initial"

    assert reported_trace is not None
    return {
        "scenario": scenario,
        "kind": kind,
        "classification": classification,
        "bounds": dict(BOUNDS),
        "states_explored": explored,
        "witness_expected": witness_expected,
        "check_pass": check_pass,
        "trace": list(reported_trace),
        "final": _summary(state, can_read, allow_handover, reported_state),
    }


def run() -> List[Dict[str, object]]:
    return [
        _result(
            "legacy",
            "spec_conflict",
            SCENARIO_HANDOVER,
            _can_read_legacy,
            allow_handover=True,
            witness_expected=True,
            probe_goal=None,
        ),
        _result(
            "fixed",
            "chosen_implementation",
            SCENARIO_HANDOVER,
            _can_read_fixed,
            allow_handover=True,
            witness_expected=False,
            probe_goal=_legacy_loss_probe,
        ),
        _result(
            "control",
            "chosen_implementation",
            SCENARIO_NO_HANDOVER,
            _can_read_legacy,
            allow_handover=False,
            witness_expected=False,
            probe_goal=_legacy_loss_probe,
        ),
    ]


def main() -> int:
    results = run()
    all_checks_pass = all(bool(result["check_pass"]) for result in results)
    json.dump(
        {
            "scenario": SCENARIO_HANDOVER,
            "results": results,
            "all_checks_pass": all_checks_pass,
            "sources": SOURCES,
        },
        sys.stdout,
        ensure_ascii=False,
        indent=2,
    )
    sys.stdout.write("\n")
    return 0 if all_checks_pass else 1


if __name__ == "__main__":
    sys.exit(main())
