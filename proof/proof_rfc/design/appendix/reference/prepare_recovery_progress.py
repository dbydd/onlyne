#!/usr/bin/env python3
"""Restart during an accepted handover prepare: can the prepare still commit?

RFC 0006:166 puts the commit-before-deadline step inside the prepare flow: the
endpoint rechecks both parties, compares the expected revision and writes the
new assignee atomically before ``decision_deadline``. RFC 0006:169 keeps the old
revision across a crash before that commit, and RFC 0006:172 aborts an overdue
prepare and forbids a late commit. RFC 0006:175 requires a restarted endpoint to
take over every unfinished prepare and to abort the overdue ones without a new
request; the same bullet never says the restarted endpoint resumes the commit
step.

``current`` models the revised text as written: recovery restores the durable
prepare record, the deadline sweep still aborts an overdue prepare with no
client request, and the decision step is gone with the process that held it, so
an accepted prepare commits only if the client sends the request again.
``hardened`` adds the missing rule: recovery may itself commit an accepted
prepare before the deadline, and abort after it. ``control`` keeps the current
rule and removes the crash, which localizes the gap to recovery rather than to
the prepare flow.

The search drives one prepare through one bounded crash at most. Client retry is
an explicit action; in the modelled scenario no client request arrives. No
transition invents a timer: after the restart the only non-client actions are
the deadline flag, another crash and the restart.
"""

from __future__ import annotations

import json
import sys
from collections import deque
from dataclasses import asdict, dataclass, replace

PENDING = ("prepared", "accepted")
RESOLVING = frozenset({"commit_before_deadline", "abort_overdue"})
MAX_STATES = 2048


@dataclass(frozen=True)
class State:
    phase: str = "prepared"          # prepared | accepted | committed | aborted
    deadline_elapsed: bool = False
    online: bool = True
    has_crashed: bool = False
    restarted: bool = False
    new_request: bool = False        # the client retries the same operation id
    late_commit_rejected: bool = False


def resolution(rule: str, state: State) -> list[tuple[str, State]]:
    if not state.online or state.phase not in PENDING:
        return []
    if state.deadline_elapsed:
        # RFC 0006:172 — the deadline outcome is abort, and 175 attaches it to
        # recovery, so no client request is needed.
        return [("abort_overdue", replace(state, phase="aborted"))]
    if state.phase != "accepted":
        return []
    if rule == "hardened" or not state.restarted or state.new_request:
        # RFC 0006:166 — commit is allowed until the deadline. In `current`, a
        # restarted endpoint only reaches this branch through a fresh request.
        return [("commit_before_deadline", replace(state, phase="committed"))]
    return []


def transitions(rule: str):
    crashes = rule != "control"

    def step(state: State) -> list[tuple[str, State]]:
        out: list[tuple[str, State]] = []
        if state.phase in PENDING:
            if not state.deadline_elapsed:
                # Wall time passes whether or not the endpoint is online.
                out.append(
                    ("decision_deadline_elapses",
                     replace(state, deadline_elapsed=True))
                )
            if crashes and state.online and not state.has_crashed:
                out.append(
                    ("endpoint_crashes",
                     replace(state, online=False, has_crashed=True))
                )
            if not state.online:
                out.append(
                    ("endpoint_restarts",
                     replace(state, online=True, restarted=True))
                )
            if state.online and not state.has_crashed and state.phase == "prepared":
                # The acceptance fact the recovered record is supposed to keep
                # (RFC 0006:175) is recorded while the original process lives.
                out.append(("target_accepts", replace(state, phase="accepted")))
            if state.online and not state.new_request:
                out.append(("new_request_arrives", replace(state, new_request=True)))
            if state.online and state.phase == "accepted" and state.deadline_elapsed:
                # The fence is exercised: a commit attempt after the deadline is
                # rejected instead of being unreachable.
                out.append(
                    ("attempt_commit_after_deadline",
                     replace(state, late_commit_rejected=True))
                )
        out.extend(resolution(rule, state))
        return out

    return step


def explore(rule: str) -> tuple[dict[State, tuple[str, ...]], bool]:
    step = transitions(rule)
    start = State()
    traces: dict[State, tuple[str, ...]] = {start: ()}
    queue: deque[State] = deque([start])
    truncated = False
    while queue:
        state = queue.popleft()
        for action, nxt in step(state):
            if nxt in traces:
                continue
            if len(traces) >= MAX_STATES:
                truncated = True
                break
            traces[nxt] = traces[state] + (action,)
            queue.append(nxt)
    return traces, truncated


def resolving_actions(state: State, step) -> list[str]:
    return sorted({action for action, _ in step(state)} & RESOLVING)


def is_stall(state: State, step) -> bool:
    """Accepted, deadline open, online, and nothing can resolve the prepare."""
    return (
        state.online
        and state.phase == "accepted"
        and not state.deadline_elapsed
        and not resolving_actions(state, step)
    )


def stable_without_client_request(state: State, step) -> bool:
    """No non-client action in the internal closure can enable a commit."""
    seen = {state}
    queue: deque[State] = deque([state])
    while queue:
        current = queue.popleft()
        for action, nxt in step(current):
            if action == "new_request_arrives" or nxt in seen:
                continue
            seen.add(nxt)
            if "commit_before_deadline" in {a for a, _ in step(nxt)}:
                return False
            queue.append(nxt)
    return True


def view(state: State, step) -> dict:
    return {
        "state": asdict(state),
        "enabled_actions": sorted(action for action, _ in step(state)),
        "resolving_actions": resolving_actions(state, step),
        "stall": is_stall(state, step),
    }


def bounds(rule: str, truncated: bool) -> dict:
    crashes = 0 if rule == "control" else 1
    return {
        "prepares": 1,
        "operation_ids": 1,
        "targets": 1,
        "accepts": 1,
        "new_client_requests": 1,
        "retry_window": "not modelled; the client request is undated",
        "crashes": crashes,
        "restarts": crashes,
        "deadline_flags": ["open", "elapsed"],
        "max_states": MAX_STATES,
        "frontier_exhausted": not truncated,
    }


def scenario_current() -> dict:
    step = transitions("current")
    traces, truncated = explore("current")
    witness = next((s for s in traces if is_stall(s, step)), None)
    final = view(witness, step) if witness else None
    checks = {
        "stall_witness_found": witness is not None,
        "witness_accepted_with_open_deadline": bool(
            witness and witness.phase == "accepted" and not witness.deadline_elapsed
        ),
        "witness_online_after_restart": bool(
            witness and witness.online and witness.restarted
        ),
        "witness_no_new_request": bool(witness and not witness.new_request),
        "no_enabled_resolving_action": bool(
            witness and not final["resolving_actions"]
        ),
        "witness_stable_without_client_request": bool(
            witness and stable_without_client_request(witness, step)
        ),
        # The client sends nothing in this scenario, so a commit after the
        # restart must be impossible — that impossibility is the finding.
        "commit_unreachable_after_restart_without_request": not any(
            s.phase == "committed" and s.restarted and not s.new_request
            for s in traces
        ),
        "no_commit_after_deadline": not any(
            s.phase == "committed" and s.deadline_elapsed for s in traces
        ),
        "late_commit_attempt_rejected": any(s.late_commit_rejected for s in traces),
        "not_truncated": not truncated,
    }
    return {
        "scenario": "accepted-prepare-after-restart",
        "kind": "regression_pre_fix",
        "classification": "missing_duty",
        "bounds": bounds("current", truncated),
        "states_explored": len(traces),
        "witness_expected": True,
        "check_pass": all(checks.values()),
        "checks": checks,
        "trace": list(traces[witness]) if witness else [],
        "final": final,
    }


def scenario_hardened() -> dict:
    step = transitions("hardened")
    traces, truncated = explore("hardened")
    recovery_commit = next(
        (s for s in traces if s.phase == "committed" and s.restarted), None
    )
    checks = {
        "stall_absent": not any(is_stall(s, step) for s in traces),
        "commit_after_restart_without_request": any(
            s.phase == "committed" and s.restarted and not s.new_request
            for s in traces
        ),
        "overdue_abort_after_restart": any(
            s.phase == "aborted" and s.restarted for s in traces
        ),
        "no_commit_after_deadline": not any(
            s.phase == "committed" and s.deadline_elapsed for s in traces
        ),
        "late_commit_attempt_rejected": any(s.late_commit_rejected for s in traces),
        "not_truncated": not truncated,
    }
    return {
        "scenario": "recovered-prepare-commits-before-deadline",
        "kind": "current",
        "classification": "missing_duty",
        "bounds": bounds("hardened", truncated),
        "states_explored": len(traces),
        "witness_expected": False,
        "check_pass": all(checks.values()),
        "checks": checks,
        "trace": list(traces[recovery_commit]) if recovery_commit else [],
        "final": view(recovery_commit, step) if recovery_commit else None,
    }


def scenario_control() -> dict:
    step = transitions("control")
    traces, truncated = explore("control")
    first_commit = next((s for s in traces if s.phase == "committed"), None)
    checks = {
        "stall_absent": not any(is_stall(s, step) for s in traces),
        "commit_reached": first_commit is not None,
        "no_crash_or_restart": all(
            not s.has_crashed and not s.restarted for s in traces
        ),
        "no_commit_after_deadline": not any(
            s.phase == "committed" and s.deadline_elapsed for s in traces
        ),
        "not_truncated": not truncated,
    }
    return {
        "scenario": "accepted-prepare-without-restart",
        "kind": "control",
        "classification": "missing_duty",
        "bounds": bounds("control", truncated),
        "states_explored": len(traces),
        "witness_expected": False,
        "check_pass": all(checks.values()),
        "checks": checks,
        "trace": list(traces[first_commit]) if first_commit else [],
        "final": view(first_commit, step) if first_commit else None,
    }


def run() -> list[dict]:
    return [scenario_current(), scenario_hardened(), scenario_control()]


def main() -> int:
    results = run()
    payload = {
        "scenario": "prepare_recovery_progress",
        "results": results,
        "all_checks_pass": all(result["check_pass"] for result in results),
        "claim": "bounded search over the modelled transitions; no claim about paths outside them",
    }
    print(json.dumps(payload, ensure_ascii=False, indent=2))
    return 0 if payload["all_checks_pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
