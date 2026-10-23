#!/usr/bin/env python3
"""grant_transaction_race: 「添加成员」与「持久授权通知」之间的崩溃窗口。

模型只覆盖 owner 侧一次主动添加离线成员：
  1. owner 写入成员记录，并为该成员创建一条持久授权通知；
  2. 通知投递失败可重试；成员上线后接收通知并确认；
  3. owner 可以在两个窗口崩溃，恢复时只重读存盘；
  4. 成员只从授权通知得知这个 owner（0003:66 的已知 owner 集合）。

三种关系：
  current   0002:118 的字面读法：成员记录与授权通知是两次写，「同时」没有给出事务
            边界；崩溃落在两次写之间时成员记录已 active，通知的内存意图随进程消失。
  hardened  提议规则：两次写在同一个事务提交，恢复重读后通知与成员记录要么都在、
            要么都不在。
  control   安全替代：仍然先写授权邀请，但成员记录在成员确认通知之前不进入 active，
            所以 active 蕴含成员已经记录了这个 owner。

坏谓词：成员记录 active ∧ 成员没有记录该 owner ∧ 不存在未投递的持久通知。
此时成员永远发现不了这条成员资格，也没有任何规则要求的补齐动作。
有界状态搜索只否证性质：absence 只在本文件的边界内成立。

同一份 RFC 在需要原子性时会明写：0002:180 要求与关闭、移除共用「同一原子顺序」，
0003:58 要求与消息条目、回执「同一事务」。0002:118 只写「同时」，两次写是文本
允许的读法。这是把 current 读成 spec_conflict 的依据。

模型不包含「为已有 active 成员补写通知」的动作：0002、0003 没有任何规则要求 owner
这样做，恢复规则只重读存盘。要修 current，必须先加一条这样的要求。

依据：
  docs/rfcs/0002-threads.md:116-118、:180、:213
  docs/rfcs/0003-messages.md:58、:62-66、:236
"""

from __future__ import annotations

import json
import sys
from collections import deque
from dataclasses import asdict, dataclass, replace

MAX_ATTEMPTS = 2  # 授权通知的投递尝试上限；用尽后存盘里的通知仍在，等待成员上线
MAX_DEPTH = 16
MAX_STATES = 4096


@dataclass(frozen=True, slots=True)
class State:
    owner_state: str = "up"  # up | down
    membership: str = "absent"  # absent | active
    grant: str = "absent"  # absent | pending | persisted | delivered | confirmed
    recorded_owner: bool = False  # 成员已持久记录这个 owner
    delivery_attempts: int = 0
    member_online: bool = False
    crash_window: int = 0  # 0 未崩溃；1 首次写之前；2 首次写之后


def transitions(mode: str, s: State):
    if s.owner_state == "down":
        # 恢复只重读存盘：崩溃动作已经丢掉内存里的 pending 意图，恢复不会把它变回来，
        # 也不会丢掉 persisted / delivered 的通知。
        yield "recover(reread durable store)", replace(s, owner_state="up")
        return

    if not s.member_online:
        yield "member_online", replace(s, member_online=True)

    if s.crash_window == 0 and s.membership == "absent" and s.grant == "absent":
        yield "crash_before_first_write", replace(s, owner_state="down", crash_window=1)

    if s.crash_window == 0 and s.grant in ("pending", "persisted"):
        lost = "absent" if s.grant == "pending" else s.grant
        yield "crash_after_first_write", replace(
            s, owner_state="down", crash_window=2, grant=lost
        )

    if s.membership == "absent" and s.grant == "absent":
        if mode == "current":
            # 第一次写：成员记录先落盘，通知还没写。
            yield "add_member(write membership)", replace(
                s, membership="active", grant="pending"
            )
        elif mode == "hardened":
            # 两次写在同一事务里提交。
            yield "add_member(atomic commit)", replace(
                s, membership="active", grant="persisted"
            )
        else:
            # 邀请先持久化，成员记录等到确认再写。
            yield "invite_member(persist grant)", replace(s, grant="persisted")

    if s.grant == "pending":
        yield "persist_grant", replace(s, grant="persisted")

    if s.grant == "persisted" and s.delivery_attempts < MAX_ATTEMPTS:
        if s.member_online:
            yield "deliver_grant(ok)", replace(
                s,
                grant="delivered",
                recorded_owner=True,
                delivery_attempts=s.delivery_attempts + 1,
            )
        else:
            yield "deliver_grant(member offline)", replace(
                s, delivery_attempts=s.delivery_attempts + 1
            )

    if s.grant == "delivered" and s.member_online:
        if mode == "control":
            yield "confirm_grant(activate membership)", replace(
                s, grant="confirmed", membership="active"
            )
        else:
            yield "confirm_grant", replace(s, grant="confirmed")


def resolving_actions(s: State) -> list[str]:
    """规则要求的、能把这名成员从当前状态推向可发现的动作。"""
    if s.owner_state == "down":
        return ["recover"]
    out = []
    if s.grant == "pending":
        out.append("persist_grant")
    if s.grant == "persisted" and s.delivery_attempts < MAX_ATTEMPTS:
        out.append("deliver_grant")
    if s.grant == "delivered":
        out.append("confirm_grant")
    return out


def bad(s: State) -> bool:
    return (
        s.owner_state == "up"
        and s.membership == "active"
        and not s.recorded_owner
        and s.grant == "absent"
        and s.member_online
        and not resolving_actions(s)
    )


def hardened_evidence(s: State) -> bool:
    """提交后崩溃：成员记录 active，通知仍在存盘里等重投。"""
    return (
        s.owner_state == "up"
        and s.crash_window == 2
        and s.membership == "active"
        and s.grant == "persisted"
    )


def control_evidence(s: State) -> bool:
    """邀请写入后崩溃，成员上线确认，成员记录才进入 active。"""
    return (
        s.owner_state == "up"
        and s.crash_window == 2
        and s.membership == "active"
        and s.recorded_owner
        and s.grant == "confirmed"
    )


@dataclass(slots=True)
class Exploration:
    parent: dict[State, tuple[State | None, str | None, int]]
    order: list[State]
    truncated: bool


def explore(mode: str) -> Exploration:
    start = State()
    parent: dict[State, tuple[State | None, str | None, int]] = {start: (None, None, 0)}
    order = [start]
    queue = deque([start])
    truncated = False

    while queue:
        state = queue.popleft()
        depth = parent[state][2]
        for name, nxt in transitions(mode, state):
            if nxt in parent:
                continue
            if depth >= MAX_DEPTH or len(parent) >= MAX_STATES:
                truncated = True
                continue
            parent[nxt] = (state, name, depth + 1)
            order.append(nxt)
            queue.append(nxt)

    return Exploration(parent, order, truncated)


def trace_of(exploration: Exploration, state: State) -> list[str]:
    actions: list[str] = []
    cursor: State | None = state
    while cursor is not None:
        prev, action, _ = exploration.parent[cursor]
        if action is not None:
            actions.append(action)
        cursor = prev
    return list(reversed(actions))


def final_of(state: State | None, detail: str) -> dict:
    if state is None:
        return {
            "owner_state": None,
            "membership": None,
            "grant": None,
            "recorded_owner": None,
            "delivery_attempts": None,
            "member_online": None,
            "crash_window": None,
            "resolving_actions": None,
            "detail": detail,
        }
    data = asdict(state)
    data["resolving_actions"] = resolving_actions(state)
    data["detail"] = detail
    return data


def bounds_of(exploration: Exploration) -> dict:
    reachable_bad = sum(1 for s in exploration.order if bad(s))
    retry_bound = sum(
        1
        for s in exploration.order
        if s.grant == "persisted"
        and s.delivery_attempts >= MAX_ATTEMPTS
        and not s.recorded_owner
    )
    return {
        "members": 1,
        "memberships": 1,
        "crash_windows": 2,
        "delivery_attempts": MAX_ATTEMPTS,
        "max_depth": MAX_DEPTH,
        "max_states": MAX_STATES,
        "exhaustive": not exploration.truncated,
        "stall_states": reachable_bad,
        "retry_bound_states": retry_bound,
        "scope": (
            "one owner, one offline member, one owner-initiated addition; "
            "no compensating rewrite of a missing grant"
        ),
    }


def result(
    mode: str,
    classification: str,
    exploration: Exploration,
    trace_state: State | None,
    check_pass: bool,
    witness_expected: bool,
    detail: str,
) -> dict:
    return {
        "scenario": "grant_transaction_race",
        "kind": {"current": "regression_pre_fix", "hardened": "current"}.get(mode, mode),
        "classification": classification,
        "bounds": bounds_of(exploration),
        "states_explored": len(exploration.parent),
        "witness_expected": witness_expected,
        "check_pass": check_pass,
        "trace": trace_of(exploration, trace_state) if trace_state is not None else [],
        "final": final_of(trace_state, detail),
    }


def run() -> list[dict]:
    current = explore("current")
    current_witness = next((s for s in current.order if bad(s)), None)
    current_trace = trace_of(current, current_witness) if current_witness is not None else []
    current_ok = (
        current_witness is not None
        and "crash_after_first_write" in current_trace
        and "recover(reread durable store)" in current_trace
        and "member_online" in current_trace
        and not current.truncated
    )
    results = [
        result(
            "current",
            "spec_conflict",
            current,
            current_witness,
            current_ok,
            True,
            "0002:118 只要求「同时」创建通知，没有规定两次写的事务边界；同一文档在 0002:180"
            "与 0003:58 需要原子性时都明写「同一原子顺序」「同一事务」，所以两次写是文本允许"
            "的读法。崩溃落在两次写之间时成员记录已 active，通知从未落盘，恢复无事可重试。"
            "0002:213 却承诺「owner 添加离线成员后，成员上线能凭授权通知找到 owner」。"
            "此后任何 @all 接纳都会为这名 active 成员提交关注义务，而它永远不会去查这个"
            "owner 的关注清单。缺的职责是：没有规则要求恢复时修复已 active 却缺通知的成员，"
            "也没有规则授权 owner 补写通知。",
        )
    ]

    hardened = explore("hardened")
    hardened_witness = next((s for s in hardened.order if bad(s)), None)
    hardened_near = next((s for s in hardened.order if hardened_evidence(s)), None)
    hardened_ok = (
        hardened_witness is None
        and hardened_near is not None
        and not hardened.truncated
    )
    results.append(
        result(
            "hardened",
            "chosen_implementation",
            hardened,
            hardened_near,
            hardened_ok,
            False,
            "成员记录与授权通知同事务提交：任一窗口崩溃后，恢复重读到的要么两者都有，"
            "要么两者都无；通知保留到成员确认。这是提案，不是 RFC 文本。",
        )
    )

    control = explore("control")
    control_witness = next((s for s in control.order if bad(s)), None)
    control_near = next((s for s in control.order if control_evidence(s)), None)
    control_ok = (
        control_witness is None
        and control_near is not None
        and not control.truncated
    )
    results.append(
        result(
            "control",
            "chosen_implementation",
            control,
            control_near,
            control_ok,
            False,
            "邀请先持久化，成员确认通知之后成员记录才进入 active。同一个崩溃窗口下坏谓词"
            "不可达，说明缺陷来自「先激活、后通知」的顺序，而不是崩溃本身。代价是成员"
            "在被添加后要等到上线确认才获得投递权。",
        )
    )
    return results


def main() -> int:
    results = run()
    payload = {
        "scenario": "grant_transaction_race",
        "results": results,
        "all_checks_pass": all(r["check_pass"] for r in results),
    }
    print(json.dumps(payload, ensure_ascii=False, indent=2))
    return 0 if payload["all_checks_pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
