#!/usr/bin/env python3
"""Finite-state search: a resource operation whose reply is lost at max timeout.

RFC 0006 §1 (line 71) puts every resource operation result in exactly three
states and folds timeout into ``aborted``. RFC 0015 §3 and §5 (lines 50, 93,
119) require ``unknown`` for a transport timeout and never mention ``aborted``.
An MCP-backed resource is a resource operation, so both rules apply to the same
timeout, and they require disjoint outcomes.

The model keeps the external effect and the local evidence in separate
coordinates. The remote may perform the effect exactly once while the local
node only ever witnesses a timeout. No transition derives one coordinate from
the other, so no state lets ``aborted`` stand for "no side effect happened".
"""

from __future__ import annotations

import json
import sys
from collections import deque
from dataclasses import dataclass, replace

TIME_LIMIT = 2
MAX_STATES = 512

# Rule set per reading. Key is the RFC location, value is the set of operation
# outcomes that location accepts at a transport timeout.
LEGACY_REQUIRED = {
    "0006:71": frozenset({"aborted"}),
    "0015:50,93,119": frozenset({"unknown"}),
}
FIXED_REQUIRED = {
    "0006:71 (proposed)": frozenset({"unknown"}),
    "0015:50,93,119": frozenset({"unknown"}),
}
# The control keeps the proposed rule text and differs only in how the
# implementation keeps effect and evidence apart.
CONTROL_REQUIRED = FIXED_REQUIRED

REQUIRED = {
    "legacy": LEGACY_REQUIRED,
    "fixed": FIXED_REQUIRED,
    "control": CONTROL_REQUIRED,
}

# The outcome each reading writes when the local maximum timeout is reached.
OUTCOME_AT_TIMEOUT = {
    "legacy": "aborted",
    "fixed": "unknown",
    "control": "unknown",
}


@dataclass(frozen=True)
class State:
    policy: str            # legacy | fixed | control
    phase: str             # idle | dispatched | settled
    elapsed: int           # ticks waited, bounded by TIME_LIMIT
    effect: str            # none | once — the external world
    evidence: str          # none | rejected | timeout — what the local node saw
    outcome: str | None    # the recorded operation result, only after settling
    response_lost: bool


def transitions(state: State) -> list[tuple[str, State]]:
    out: list[tuple[str, State]] = []

    if state.phase == "idle":
        out.append(("dispatch", replace(state, phase="dispatched")))
        if state.policy == "control":
            # The safe use of `aborted`: the caller rejected the operation
            # locally, so no effect was possible. No request left the node.
            out.append(
                (
                    "local_reject",
                    replace(
                        state,
                        phase="settled",
                        evidence="rejected",
                        outcome="aborted",
                    ),
                )
            )
        return out

    # The effect is the remote's, not the local node's. It may land after the
    # local node has already stopped waiting (RFC 0015 §5: cancellation does not
    # stop the peer), so the transition stays enabled past settle.
    if state.effect == "none" and state.evidence != "rejected":
        out.append(("remote_effect", replace(state, effect="once")))

    if state.phase == "dispatched":
        if state.evidence == "none":
            if not state.response_lost:
                out.append(("response_lost", replace(state, response_lost=True)))
            if state.elapsed < TIME_LIMIT:
                out.append(("tick", replace(state, elapsed=state.elapsed + 1)))
            else:
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


def requirements_are_disjoint(policy: str) -> bool:
    sets = list(REQUIRED[policy].values())
    common = set(sets[0])
    for required in sets[1:]:
        common &= required
    return not common


def bad_predicate(state: State) -> bool:
    """Timeout reached while the applicable rules require disjoint outcomes."""
    return state.evidence == "timeout" and requirements_are_disjoint(state.policy)


def explore(policy: str) -> tuple[dict[State, tuple[str, ...]], list[tuple[State, tuple[str, ...]]], bool]:
    start = State(
        policy=policy,
        phase="idle",
        elapsed=0,
        effect="none",
        evidence="none",
        outcome=None,
        response_lost=False,
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
        "evidence": state.evidence,
        "elapsed": state.elapsed,
        "response_lost": state.response_lost,
        "required_outcomes": {rule: sorted(s) for rule, s in required.items()},
        "requirements_disjoint": requirements_are_disjoint(state.policy),
        "trace_length": len(trace),
    }


def bounds(policy: str) -> dict:
    evidence = ["none", "timeout"]
    if policy == "control":
        evidence = ["none", "rejected", "timeout"]
    return {
        "max_ticks": TIME_LIMIT,
        "max_states": MAX_STATES,
        "effects": ["none", "once"],
        "evidence": evidence,
        "outcomes_reached": ["aborted", "unknown"],
        "response_paths": ["lost"],
        "rules": sorted(REQUIRED[policy]),
    }


def _settled_timeouts(traces):
    picked = [
        (state, trace)
        for state, trace in traces.items()
        if state.phase == "settled" and state.evidence == "timeout"
    ]
    # Prefer the fault the search is about: a reply that was sent and lost,
    # after an effect that really happened; then the shortest trace.
    picked.sort(
        key=lambda item: (
            item[0].effect != "once",
            not item[0].response_lost,
            len(item[1]),
            item[0].outcome or "",
        )
    )
    return picked


def scenario_legacy() -> dict:
    traces, hits, truncated = explore("legacy")
    witness = None
    candidates = [
        (state, trace)
        for state, trace in hits
        if state.phase == "settled" and state.outcome == "aborted" and state.effect == "once"
    ]
    candidates.sort(key=lambda item: (not item[0].response_lost, len(item[1])))
    if candidates:
        witness = candidates[0]

    final = settle_view(*witness) if witness else None
    checks = {
        "witness_found": witness is not None,
        "outcome_is_aborted": bool(final and final["outcome"] == "aborted"),
        "effect_happened_anyway": bool(final and final["effect"] == "once"),
        "evidence_is_timeout": bool(final and final["evidence"] == "timeout"),
        "effect_and_evidence_separate": bool(
            final and {"effect", "evidence"} <= set(final) and final["effect"] != final["evidence"]
        ),
        "rules_disjoint": bool(final and final["requirements_disjoint"]),
        "not_truncated": not truncated,
    }
    return {
        "scenario": "mcp-timeout-under-rfc-0006-71",
        "kind": "legacy",
        "classification": "spec_conflict",
        "bounds": bounds("legacy"),
        "states_explored": len(traces),
        "predicate_hits": len(hits),
        "witness_expected": True,
        "check_pass": all(checks.values()),
        "checks": checks,
        "trace": list(witness[1]) if witness else [],
        "final": final,
    }


def scenario_fixed() -> dict:
    traces, hits, truncated = explore("fixed")
    representative = _settled_timeouts(traces)
    view = settle_view(*representative[0]) if representative else None
    checks = {
        "predicate_absent": not hits,
        "timeout_maps_to_unknown": bool(view and view["outcome"] == "unknown"),
        "effect_preserved_after_unknown": bool(view and view["effect"] == "once"),
        "not_truncated": not truncated,
    }
    return {
        "scenario": "mcp-timeout-under-proposed-0006",
        "kind": "fixed",
        "classification": "spec_conflict",
        "bounds": bounds("fixed"),
        "states_explored": len(traces),
        "predicate_hits": len(hits),
        "witness_expected": False,
        "check_pass": all(checks.values()),
        "checks": checks,
        "trace": list(representative[0][1]) if representative else [],
        "final": view,
    }


def scenario_control() -> dict:
    traces, hits, truncated = explore("control")
    safe = [
        (state, trace)
        for state, trace in traces.items()
        if state.phase == "settled"
        and state.evidence == "timeout"
        and state.outcome == "unknown"
        and state.effect == "once"
    ]
    safe.sort(key=lambda item: (not item[0].response_lost, len(item[1])))
    rejections = [
        (state, trace)
        for state, trace in traces.items()
        if state.outcome == "aborted" and state.evidence == "rejected" and state.effect == "none"
    ]
    aborted_with_effect = [
        state for state in traces if state.outcome == "aborted" and state.effect == "once"
    ]
    view = settle_view(*safe[0]) if safe else None
    checks = {
        "predicate_absent": not hits,
        "safe_unknown_after_effect": bool(safe),
        "effect_and_evidence_separate": bool(
            view and {"effect", "evidence"} <= set(view) and view["effect"] == "once" and view["evidence"] == "timeout"
        ),
        "aborted_only_after_local_reject": not aborted_with_effect,
        "aborted_requires_no_effect": all(
            state.effect == "none" for state in traces if state.outcome == "aborted"
        ),
        "local_reject_is_reachable": bool(rejections),
        "not_truncated": not truncated,
    }
    return {
        "scenario": "safe-unknown-after-lost-response",
        "kind": "control",
        "classification": "chosen_implementation",
        "bounds": bounds("control"),
        "states_explored": len(traces),
        "predicate_hits": len(hits),
        "witness_expected": False,
        "check_pass": all(checks.values()),
        "checks": checks,
        "trace": list(safe[0][1]) if safe else [],
        "final": view,
    }


def run() -> list[dict]:
    return [scenario_legacy(), scenario_fixed(), scenario_control()]


def main() -> int:
    results = run()
    payload = {
        "scenario": "timeout_outcome",
        "results": results,
        "all_checks_pass": all(result["check_pass"] for result in results),
        "claim": "bounded search over the modeled transitions; no claim about paths outside them",
    }
    print(json.dumps(payload, ensure_ascii=False, indent=2))
    return 0 if payload["all_checks_pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
