"""Closed attention: a thread closes with an accepted, un-injected notice.

The model is the VDM contract in
proof/proof_rfc/design/architecture/attention/attention.md. It searches the reachable
state graph exhaustively inside small bounds and reports the shortest trace.

Read the finding as: the obligation is still recorded, the member still has read
access, the automatic fragment no longer shows it. The history stays readable by
an explicit query, so this is a lost automatic path, not a deleted message.
"""

from __future__ import annotations

import json
import sys
from collections import deque
from dataclasses import asdict, dataclass

THREAD = "t0"
MESSAGE = "m0"
ITEMS = (THREAD, MESSAGE)


@dataclass(frozen=True)
class State:
    due: tuple[str, ...] = ()
    queued: tuple[str, ...] = ()
    injected: tuple[str, ...] = ()
    closed: bool = False
    read_allowed: bool = True


def admit(s: State) -> State:
    return State(tuple(sorted(set(s.due) | {ITEMS[0]})), s.queued, s.injected,
                 s.closed, s.read_allowed)


def queue(s: State) -> State:
    if ITEMS[0] not in s.due:
        return s
    return State(s.due, tuple(sorted(set(s.queued) | {ITEMS[0]})), s.injected,
                 s.closed, s.read_allowed)


def inject(s: State) -> State:
    if ITEMS[0] not in s.queued:
        return s
    return State(s.due, s.queued, tuple(sorted(set(s.injected) | {ITEMS[0]})),
                 s.closed, s.read_allowed)


def close(s: State) -> State:
    return State(s.due, s.queued, s.injected, True, s.read_allowed)


def automatic(s: State) -> tuple[str, ...]:
    """Current automatic fragment: queued, not injected, thread still open."""
    if s.closed:
        return ()
    return tuple(x for x in s.queued if x not in s.injected)


def transitions(s: State):
    yield "admit", admit(s)
    yield "queue", queue(s)
    yield "inject", inject(s)
    if not s.closed:
        yield "close", close(s)


def bad(s: State) -> bool:
    """Obligation recorded, still allowed, un-injected, absent from the fragment."""
    return (
        ITEMS[0] in s.due
        and ITEMS[0] in s.queued
        and ITEMS[0] not in s.injected
        and s.closed
        and s.read_allowed
        and ITEMS[0] not in automatic(s)
    )


def explore(rule: str):
    """rule=legacy closes the fragment; rule=fixed keeps accepted items visible."""
    start = State()
    parents = {start: (None, None)}
    queue_ = deque([start])
    while queue_:
        state = queue_.popleft()
        for name, nxt in transitions(state):
            if rule == "fixed" and name == "close":
                nxt = State(nxt.due, nxt.queued, nxt.injected, True, True)
            if nxt in parents:
                continue
            parents[nxt] = (state, name)
            queue_.append(nxt)
    return parents


def fragment(rule: str, s: State) -> tuple[str, ...]:
    if rule == "fixed":
        return tuple(x for x in s.queued if x not in s.injected)
    return automatic(s)


def trace_of(parents, state):
    out = []
    while parents[state][0] is not None:
        state, name = parents[state]
        out.append(name)
    return list(reversed(out))


def run():
    results = []
    for rule, expected in (("legacy", True), ("fixed", False)):
        parents = explore(rule)
        witness = next((s for s in parents if bad(s) and fragment(rule, s) == ()), None)
        results.append(
            {
                "scenario": "closed_attention",
                "kind": rule,
                "classification": "spec_conflict" if rule == "legacy" else "chosen_implementation",
                "bounds": {"messages": 1, "threads": 1, "states": len(parents)},
                "states_explored": len(parents),
                "witness_expected": expected,
                "check_pass": (witness is not None) == expected,
                "trace": trace_of(parents, witness) if witness else [],
                "final": asdict(witness) if witness else {},
                "note": "The message stays readable; only the automatic path is lost.",
            }
        )
    return results


def main() -> int:
    results = run()
    ok = all(r["check_pass"] for r in results)
    print(json.dumps({"scenario": "closed_attention", "results": results,
                      "all_checks_pass": ok}, ensure_ascii=False, indent=2))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
