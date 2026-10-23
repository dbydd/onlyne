#!/usr/bin/env python3
"""Finite-state regression probe: dispatch, effect, lost response, timeout.

The revised RFC 0006 §1 operations bullet puts every resource operation result
in four outcomes and states that a timeout or a broken connection is
``unknown``. RFC 0015 §3 says the same for an MCP-backed resource: transport
failure, timeout and disconnect record ``unknown``, and ``aborted`` only means
the call was explicitly rejected or stopped locally — it never means the remote
had no effect.

The model separates three coordinates that the old three-state reading folded
together: whether the remote performed the effect, whether the caller ever saw
a response, and what outcome the caller records. No transition derives one from
another, so a lost response can leave ``effect=once`` with ``evidence=timeout``.

A bad state is a timeout state that either records ``aborted`` or is governed
by a rule that requires ``aborted`` at timeout. The current rule set must have
none. The control deliberately restores the old three-state reading as a
regression probe: if the probe stops finding the bad state, the search no
longer exercises the regression it exists for.

Bounds: at most two wait ticks, effect happens at most once, finite BFS.
"""

from __future__ import annotations

import json
import sys
from collections import deque
from dataclasses import dataclass, replace

TIME_LIMIT = 2
MAX_STATES = 512

# Outcomes each reading accepts at a transport timeout. Key is the RFC
# location that constrains the timeout result.
CURRENT_REQUIRED = {
    "0006 §1 operations bullet": frozenset({"unknown"}),
    "0015 §3 result table": frozenset({"unknown"}),
    "0015 §3 aborted caveat": frozenset({"unknown"}),
}
# Regression probe: the pre-revision three-state reading folded a timeout into
# aborted.
CONTROL_REQUIRED = {
    "0006 §1 operations bullet (old three-state reading)": frozenset({"aborted"}),
    "0015 §3 result table (old three-state reading)": frozenset({"aborted"}),
}

REQUIRED = {
    "current": CURRENT_REQUIRED,
    "control": CONTROL_REQUIRED,
}

# The outcome each reading writes once the local maximum timeout is reached.
OUTCOME_AT_TIMEOUT = {
    "current": "unknown",
    "control": "aborted",
}


@dataclass(frozen=True)
class State:
    policy: str            # current | control
    phase: str             # idle | dispatched | settled
    elapsed: int           # ticks waited, bounded by TIME_LIMIT
    effect: str            # none | once — the remote world
    response_lost: bool    # the caller will never see the response
    evidence: str          # none | timeout — what the caller observed
    outcome: str | None    # the recorded operation result, only after settling


def transitions(state: State) -> list[tuple[str, State]]:
    out: list[tuple[str, State]] = []

    if state.phase == "idle":
        out.append(("dispatch", replace(state, phase="dispatched")))
        return out

    # The effect belongs to the remote. RFC 0015 §3/§5: cancellation only stops
    # the local wait, so the effect may land after the caller has settled.
    if state.effect == "none":
        out.append(("remote_effect", replace(state, effect="once")))

    if state.phase == "dispatched":
        if not state.response_lost:
            out.append(("response_lost", replace(state, response_lost=True)))
        if state.elapsed < TIME_LIMIT:
            out.append(("tick", replace(state, elapsed=state.elapsed + 1)))
        elif state.response_lost and state.evidence == "none":
            out.append(("max_timeout", replace(state, evidence="timeout")))
        if state.evidence == "timeout":
            out.append(
                (
                    "settle",
                    replace(
                        state,
                        phase="settled",
                        outcome=OUTCOME_AT_TIMEOUT[state.policy],
                    ),
                )
            )
    return out


def rule_requires_aborted(policy: str) -> bool:
    """True when any rule governing this reading demands aborted at timeout."""
    return any("aborted" in required for required in REQUIRED[policy].values())


def rule_requires_unknown(policy: str) -> bool:
    return all("unknown" in required for required in REQUIRED[policy].values())


def bad_predicate(state: State) -> bool:
    """A timeout state recording aborted, or governed by a rule requiring it."""
    if state.evidence != "timeout":
        return False
    return state.outcome == "aborted" or rule_requires_aborted(state.policy)


def explore(policy: str) -> tuple[dict[State, tuple[str, ...]], list[tuple[State, tuple[str, ...]]], bool]:
    start = State(
        policy=policy,
        phase="idle",
        elapsed=0,
        effect="none",
        response_lost=False,
        evidence="none",
        outcome=None,
    )
    traces: dict[State, tuple[str, ...]] = {start: ()}
    queue: deque[State] = deque([start])
    hits: list[tuple[State, tuple[str, ...]]] = []
    truncated = False

    while queue:
        state = queue.popleft()
        if bad_predicate(state):
            hits.append((state, traces[state]))
        for action, nxt in transitions(state):
            if nxt in traces:
                continue
            if len(traces) >= MAX_STATES:
                truncated = True
                break
            traces[nxt] = traces[state] + (action,)
            queue.append(nxt)
    return traces, hits, truncated


def settle_view(state: State, trace: tuple[str, ...]) -> dict:
    required = REQUIRED[state.policy]
    return {
        "phase": state.phase,
        "outcome": state.outcome,
        "effect": state.effect,
        "response_lost": state.response_lost,
        "evidence": state.evidence,
        "elapsed": state.elapsed,
        "required_outcomes": {rule: sorted(s) for rule, s in required.items()},
        "rule_requires_aborted_at_timeout": rule_requires_aborted(state.policy),
        "rule_requires_unknown_at_timeout": rule_requires_unknown(state.policy),
        "trace_length": len(trace),
    }


def bounds(policy: str) -> dict:
    return {
        "max_ticks": TIME_LIMIT,
        "max_states": MAX_STATES,
        "effects": ["none", "once"],
        "response_paths": ["lost"],
        "evidence": ["none", "timeout"],
        "outcomes_recorded": sorted({OUTCOME_AT_TIMEOUT[policy]}),
        "rules": sorted(REQUIRED[policy]),
    }


def _settled_timeouts(
    traces: dict[State, tuple[str, ...]],
    *,
    effect: str | None = None,
    outcome: str | None = None,
) -> list[tuple[State, tuple[str, ...]]]:
    """Settled timeout states, shortest first, optionally filtered."""
    picked = [
        (state, trace)
        for state, trace in traces.items()
        if state.phase == "settled"
        and state.evidence == "timeout"
        and (effect is None or state.effect == effect)
        and (outcome is None or state.outcome == outcome)
    ]
    picked.sort(key=lambda item: (not item[0].response_lost, len(item[1]), item[0].outcome or ""))
    return picked


def scenario_current() -> dict:
    traces, hits, truncated = explore("current")
    handled = _settled_timeouts(traces, effect="once", outcome="unknown")
    witness = handled[0] if handled else None
    view = settle_view(*witness) if witness else None

    timeout_states = [
        state for state in traces if state.phase == "settled" and state.evidence == "timeout"
    ]
    checks = {
        "bad_state_absent": not hits,
        "rule_requires_unknown": rule_requires_unknown("current"),
        "rule_never_requires_aborted": not rule_requires_aborted("current"),
        "every_timeout_records_unknown": bool(timeout_states)
        and all(state.outcome == "unknown" for state in timeout_states),
        "effect_survives_lost_response": bool(view)
        and view["outcome"] == "unknown"
        and view["effect"] == "once",
        "witness_shortest": bool(handled)
        and len(witness[1]) == min(len(trace) for _, trace in handled),
        "not_truncated": not truncated,
    }
    return {
        "scenario": "timeout-outcome-under-revised-0006",
        "kind": "current",
        "classification": "chosen_implementation",
        "bounds": bounds("current"),
        "states_explored": len(traces),
        "predicate_hits": len(hits),
        "witness_expected": False,
        "check_pass": all(checks.values()),
        "checks": checks,
        "trace": list(witness[1]) if witness else [],
        "final": view,
    }


def scenario_control() -> dict:
    traces, hits, truncated = explore("control")
    witnesses = [
        (state, trace)
        for state, trace in hits
        if state.phase == "settled"
        and state.outcome == "aborted"
        and state.effect == "once"
    ]
    witnesses.sort(key=lambda item: (not item[0].response_lost, len(item[1])))
    witness = witnesses[0] if witnesses else None
    view = settle_view(*witness) if witness else None

    checks = {
        "bad_state_present": bool(hits),
        "rule_requires_aborted": rule_requires_aborted("control"),
        "witness_expected": witness is not None,
        "witness_records_aborted_at_timeout": bool(view)
        and view["outcome"] == "aborted"
        and view["evidence"] == "timeout",
        "witness_effect_happened": bool(view) and view["effect"] == "once",
        "witness_shortest": bool(witnesses)
        and len(witness[1]) == min(len(trace) for _, trace in witnesses),
        "not_truncated": not truncated,
    }
    return {
        "scenario": "regression-probe-old-three-state-reading",
        "kind": "control",
        "classification": "chosen_implementation",
        "bounds": bounds("control"),
        "states_explored": len(traces),
        "predicate_hits": len(hits),
        "witness_expected": True,
        "check_pass": all(checks.values()),
        "checks": checks,
        "trace": list(witness[1]) if witness else [],
        "final": view,
    }


def run() -> list[dict]:
    return [scenario_current(), scenario_control()]


def main() -> int:
    results = run()
    payload = {
        "scenario": "timeout_regression",
        "results": results,
        "all_checks_pass": all(result["check_pass"] for result in results),
        "claim": "bounded search over the modeled transitions; no claim about paths outside them",
    }
    print(json.dumps(payload, ensure_ascii=False, indent=2))
    return 0 if payload["all_checks_pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
