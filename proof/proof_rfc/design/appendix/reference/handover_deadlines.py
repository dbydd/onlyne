"""Finite handover search; deadline flags abstract time, not a wall clock.

RFC 0006:164-173 separates admission retry window from decision deadline.
RFC 0006:195-201 fences sends by current caller and non-reused revision.
The explicit search is a concrete instance, not an automatically extracted VDM
refinement. Progress requires recovery and weak fairness of enabled settlement.
"""
from __future__ import annotations

import json
import sys
from collections import deque
from dataclasses import asdict, dataclass, replace


@dataclass(frozen=True)
class State:
    phase: str = "new"
    accepted: bool = False
    retry_elapsed: bool = False
    decision_elapsed: bool = False
    online: bool = True
    crashed: bool = False
    commit_valid: bool = True
    committed_after_retry: bool = False
    notice: str = "none"


def transitions(s):
    if not s.retry_elapsed:
        yield "retry_window_elapses", replace(s, retry_elapsed=True)
    if not s.decision_elapsed:
        yield "decision_deadline_elapses", replace(s, decision_elapsed=True)
    if s.online and not s.crashed:
        yield "crash", replace(s, online=False, crashed=True)
    if not s.online:
        yield "recover", replace(s, online=True)
    if not s.online:
        return
    if s.phase == "new" and not s.retry_elapsed and not s.decision_elapsed:
        yield "admit_prepare", replace(s, phase="pending")
    if s.phase == "pending":
        if s.decision_elapsed:
            yield "abort_overdue", replace(s, phase="aborted")
        else:
            if not s.accepted:
                yield "target_accepts", replace(s, accepted=True)
            if s.accepted:
                yield "commit", replace(
                    s, phase="committed", notice="pending",
                    commit_valid=s.online and s.accepted and not s.decision_elapsed,
                    committed_after_retry=s.retry_elapsed,
                )
    if s.phase == "committed" and s.notice == "pending":
        yield "lose_result_notice", replace(s, notice="lost")


def explore(start, edges):
    parents = {start: (None, "")}
    queue = deque([start])
    while queue:
        state = queue.popleft()
        for action, nxt in edges(state):
            if nxt not in parents:
                parents[nxt] = (state, action)
                queue.append(nxt)
    return parents


def trace(parents, state):
    actions = []
    while parents[state][0] is not None:
        state, action = parents[state]
        actions.append(action)
    return list(reversed(actions))


def result(name, parents, predicate, expected, bounds):
    witness = next((s for s in parents if predicate(s)), None)
    return {
        "scenario": name, "kind": "control", "classification": "safety_control",
        "bounds": bounds, "states_explored": len(parents),
        "witness_expected": expected, "check_pass": (witness is not None) == expected,
        "trace": trace(parents, witness) if witness is not None else [],
        "final": asdict(witness) if witness is not None else {},
    }


@dataclass(frozen=True)
class Fence:
    revision: int = 0
    observed: bool = False
    old_send_accepted: bool = False


def fence_transitions(s):
    if s.revision < 2:
        actor = "A" if s.revision == 0 else "B"
        target = "B" if s.revision == 0 else "A"
        yield f"commit_{actor}_to_{target}", replace(s, revision=s.revision + 1)
    if s.revision == 2 and not s.observed:
        actor = "A"
        current = "A" if s.revision % 2 == 0 else "B"
        expected_revision = 0
        accepted = actor == current and expected_revision == s.revision
        yield "check_first_term_send", replace(s, observed=True, old_send_accepted=accepted)


@dataclass(frozen=True)
class Backlog:
    received: int = 0
    acked: int = 0
    committed: bool = False
    violated: bool = False


def backlog_checked(s):
    if s.received < 2:
        yield "receive", replace(s, received=s.received + 1)
    if s.acked < s.received:
        yield "ack", replace(s, acked=s.acked + 1)
    if not s.committed:
        if s.acked == s.received:
            yield "commit", replace(s, committed=True, violated=False)
        else:
            yield "reject_commit", replace(s)


def backlog_unchecked(s):
    if s.received < 2:
        yield "receive", replace(s, received=s.received + 1)
    if s.acked < s.received:
        yield "ack", replace(s, acked=s.acked + 1)
    if not s.committed:
        yield "commit_unchecked", replace(
            s, committed=True, violated=s.acked < s.received
        )


def run():
    parents = explore(State(), transitions)
    bounds = {"prepares": 1, "crashes": 1, "monotone_deadline_flags": 2,
              "frontier_exhausted": True}
    results = [
        result("no_commit_after_deadline", parents, lambda s: not s.commit_valid,
               False, bounds),
        result("retry_expiry_does_not_block_commit", parents,
               lambda s: s.phase == "committed" and s.committed_after_retry,
               True, bounds),
        result("recovery_aborts_overdue", parents,
               lambda s: s.phase == "aborted" and s.crashed and s.online,
               True, bounds),
        result("no_stable_overdue_pending", parents,
               lambda s: s.online and s.phase == "pending" and s.decision_elapsed
               and not any(action == "abort_overdue" for action, _ in transitions(s)),
               False, bounds),
        result("notice_loss_preserves_commit", parents,
               lambda s: s.notice == "lost" and s.phase != "committed",
               False, bounds),
        result("lost_notice_is_reachable", parents,
               lambda s: s.notice == "lost" and s.phase == "committed",
               True, bounds),
    ]
    fenced = explore(Fence(), fence_transitions)
    fence_bounds = {"commits": 2, "actors": ["A", "B"], "frontier_exhausted": True}
    results.extend([
        result("aba_stale_send_not_accepted", fenced, lambda s: s.old_send_accepted,
               False, fence_bounds),
        result("aba_stale_send_rejected", fenced,
               lambda s: s.revision == 2 and s.observed and not s.old_send_accepted,
               True, fence_bounds),
    ])
    checked = explore(Backlog(), backlog_checked)
    unchecked = explore(Backlog(), backlog_unchecked)
    backlog_bounds = {"events": 2, "acked": "monotone", "frontier_exhausted": True}
    results.extend([
        result("commit_rejected_with_backlog", checked,
               lambda s: s.violated, False, backlog_bounds),
        result("unchecked_commit_witness", unchecked,
               lambda s: s.violated, True, backlog_bounds),
    ])
    return results


if __name__ == "__main__":
    results = run()
    ok = all(r["check_pass"] for r in results)
    print(json.dumps({"scenario": "handover_deadlines", "results": results,
                      "all_checks_pass": ok}, ensure_ascii=False, indent=2))
    sys.exit(0 if ok else 1)
