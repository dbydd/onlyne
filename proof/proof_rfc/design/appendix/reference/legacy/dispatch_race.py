#!/usr/bin/env python3
"""dispatch_race: 交接提交发生在首次推送之前时，事件被推给谁。

模型只覆盖一个资源上的一条平台事件：
  1. receive 记录首次归属（当时 assignee），重复到达不再改写；
  2. 交接准备与提交可以发生在首次推送之前；
  3. 首次推送的接收者按模式读取：
     - legacy：当前 assignee（0012 §4 第 4 步的字面读法）；
     - fixed：事件首次归属（0006 §6、0012 §8 的重放规则）；
     - control：legacy 读法，但存在未推送事件时禁止提交交接（先排空再提交）。

坏谓词：某条已推送事件的接收者不是它的首次归属。
模型不包含积压转移动作；0006 §6 把转移积压列为另一个显式操作。
有界状态搜索只否证性质：absence 只在本文件的边界内成立。

依据：
  docs/rfcs/0006-resources-and-handover.md:181-183、:189
  docs/rfcs/0012-im-resource-node.md:55-58、:119、:174
"""

from __future__ import annotations

import json
import sys
from collections import deque
from dataclasses import dataclass, replace

EVENT = "e1"
NODES = ("A", "B")
MAX_COMMITS = 2  # 最多两次提交：A->B、B->A
MAX_DEPTH = 10
MAX_STATES = 4096


@dataclass(frozen=True, slots=True)
class State:
    assignee: str
    revision: int
    first_owner: tuple[tuple[str, str], ...]
    prepare: tuple[str, int] | None
    pushed: tuple[tuple[str, str, int], ...]


def owner_of(state: State, event: str) -> str | None:
    for known, owner in state.first_owner:
        if known == event:
            return owner
    return None


def pushed_state(state: State, event: str) -> tuple[str, int] | None:
    """已推送事件的 (接收者, 推送时 revision)；未推送返回 None。"""
    for known, recipient, revision in state.pushed:
        if known == event:
            return recipient, revision
    return None


def pending(state: State) -> bool:
    return owner_of(state, EVENT) is not None and pushed_state(state, EVENT) is None


def push_enabled(state: State) -> bool:
    return pending(state)


def transitions(mode: str, state: State):
    if owner_of(state, EVENT) is None:
        owners = tuple(sorted(state.first_owner + ((EVENT, state.assignee),)))
        yield f"receive({EVENT})", replace(state, first_owner=owners)

    if state.prepare is None and state.revision < MAX_COMMITS:
        for target in NODES:
            if target != state.assignee:
                yield (
                    f"prepare_handover({state.assignee}->{target})",
                    replace(state, prepare=(target, state.revision)),
                )

    if state.prepare is not None:
        target, expected = state.prepare
        if expected == state.revision and (mode != "control" or not pending(state)):
            yield (
                "commit_handover",
                replace(state, assignee=target, revision=state.revision + 1, prepare=None),
            )
        yield "abort_handover", replace(state, prepare=None)

    if push_enabled(state):
        owner = owner_of(state, EVENT)
        recipient = state.assignee if mode in ("legacy", "control") else owner
        yield (
            f"push({EVENT})->{recipient}",
            replace(state, pushed=state.pushed + ((EVENT, recipient, state.revision),)),
        )


def bad(state: State) -> bool:
    for event, recipient, _ in state.pushed:
        if recipient != owner_of(state, event):
            return True
    return False


@dataclass(slots=True)
class Exploration:
    parent: dict[State, tuple[State | None, str | None, int]]
    order: list[State]
    truncated: bool
    stalls: int


def explore(mode: str) -> Exploration:
    start = State("A", 0, (), None, ())
    parent: dict[State, tuple[State | None, str | None, int]] = {start: (None, None, 0)}
    order: list[State] = []
    queue = deque([start])
    truncated = False
    stalls = 0
    while queue:
        state = queue.popleft()
        order.append(state)
        if pending(state) and not push_enabled(state):
            stalls += 1
        depth = parent[state][2]
        for name, nxt in transitions(mode, state):
            if nxt in parent:
                continue
            if len(parent) >= MAX_STATES or depth + 1 > MAX_DEPTH:
                truncated = True
                continue
            parent[nxt] = (state, name, depth + 1)
            queue.append(nxt)
    return Exploration(parent, order, truncated, stalls)


def trace_of(exploration: Exploration, state: State) -> list[str]:
    actions: list[str] = []
    current = state
    while True:
        previous, action, _ = exploration.parent[current]
        if previous is None:
            break
        actions.append(action)
        current = previous
    actions.reverse()
    return actions


def final_of(state: State, detail: str) -> dict:
    owner = owner_of(state, EVENT)
    pushed = pushed_state(state, EVENT)
    return {
        "assignee": state.assignee,
        "revision": state.revision,
        "first_owner": owner,
        "dispatched_to": pushed[0] if pushed else None,
        "pushed_at_revision": pushed[1] if pushed else None,
        "recipient_is_first_owner": (pushed[0] == owner) if pushed and owner else None,
        "prepare": [state.prepare[0], state.prepare[1]] if state.prepare else None,
        "detail": detail,
    }


def missing_final(detail: str) -> dict:
    return {
        "assignee": None,
        "revision": None,
        "first_owner": None,
        "dispatched_to": None,
        "pushed_at_revision": None,
        "recipient_is_first_owner": None,
        "prepare": None,
        "detail": detail,
    }


def bounds_of(exploration: Exploration) -> dict:
    return {
        "max_depth": MAX_DEPTH,
        "max_states": MAX_STATES,
        "events": [EVENT],
        "nodes": list(NODES),
        "max_commits": MAX_COMMITS,
        "exhaustive": not exploration.truncated,
        "stall_states": exploration.stalls,
        "scope": "single resource, single platform event, no backlog-transfer action",
    }


def result(
    kind: str,
    classification: str,
    exploration: Exploration,
    trace_state: State | None,
    check_pass: bool,
    witness_expected: bool,
    detail: str,
) -> dict:
    return {
        "scenario": "dispatch_race",
        "kind": kind,
        "classification": classification,
        "bounds": bounds_of(exploration),
        "states_explored": len(exploration.parent),
        "witness_expected": witness_expected,
        "check_pass": check_pass,
        "trace": trace_of(exploration, trace_state) if trace_state is not None else [],
        "final": final_of(trace_state, detail) if trace_state is not None else missing_final(detail),
    }


def near_miss(state: State) -> bool:
    """fixed 下应当到达的近似轨迹：交接后首次推送，接收者仍是首次归属。"""
    pushed = pushed_state(state, EVENT)
    owner = owner_of(state, EVENT)
    return (
        pushed is not None
        and pushed[1] >= 1
        and owner is not None
        and pushed[0] == owner
        and state.assignee != owner
    )


def control_case(state: State) -> bool:
    """control 下应当到达的轨迹：先推送，再交接提交。"""
    pushed = pushed_state(state, EVENT)
    owner = owner_of(state, EVENT)
    return (
        pushed is not None
        and pushed[1] == 0
        and owner is not None
        and pushed[0] == owner
        and state.assignee != owner
        and state.revision >= 1
    )


def run() -> list[dict]:
    legacy = explore("legacy")
    legacy_witness = next((s for s in legacy.order if bad(s)), None)
    legacy_trace = trace_of(legacy, legacy_witness) if legacy_witness is not None else []
    legacy_ok = bool(legacy_trace) and legacy_trace[-1].startswith("push(")
    results = [
        result(
            "legacy",
            "spec_conflict",
            legacy,
            legacy_witness,
            legacy_ok,
            True,
            "0012 §4 第 4 步按当前绑定推送，0006 §6 规定提交前的事件属于原 assignee 且重放沿用首次归属；"
            "交接提交后首次推送落到新 assignee，首次归属仍是原 assignee。",
        )
    ]

    fixed = explore("fixed")
    fixed_witness = next((s for s in fixed.order if bad(s)), None)
    fixed_near = next((s for s in fixed.order if near_miss(s)), None)
    fixed_ok = (
        fixed_witness is None
        and fixed_near is not None
        and not fixed.truncated
        and fixed.stalls == 0
    )
    results.append(
        result(
            "fixed",
            "chosen_implementation",
            fixed,
            fixed_near,
            fixed_ok,
            False,
            "推送与重放都读事件记录的首次归属；同样四步轨迹下接收者保持 A。"
            "这是提案，不是 RFC 文本。",
        )
    )

    control = explore("control")
    control_witness = next((s for s in control.order if bad(s)), None)
    control_hit = next((s for s in control.order if control_case(s)), None)
    control_ok = (
        control_witness is None
        and control_hit is not None
        and not control.truncated
        and control.stalls == 0
    )
    results.append(
        result(
            "control",
            "chosen_implementation",
            control,
            control_hit,
            control_ok,
            False,
            "先排空再提交：存在未推送事件时禁止提交交接。自由交错下坏谓词不可达，"
            "说明缺陷来自推送与交接的交错，而不是推送动作本身。",
        )
    )
    return results


def main() -> int:
    results = run()
    payload = {
        "scenario": "dispatch_race",
        "results": results,
        "all_checks_pass": all(r["check_pass"] for r in results),
    }
    print(json.dumps(payload, ensure_ascii=False, indent=2))
    return 0 if payload["all_checks_pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
