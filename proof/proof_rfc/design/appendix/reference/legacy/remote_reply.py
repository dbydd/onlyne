#!/usr/bin/env python3
"""远端回复的关注通知丢失：有限状态搜索。

场景：thread 的 owner 追加一条回复（本地事务提交到 thread 存盘），
这条回复需要引起远端成员的注意，所以 owner 向远端发一次关注通知；
远端节点收到后写入自己的本地队列。通知是 one-shot，可能丢失；
owner 可以有限次崩溃后恢复，崩溃只丢失内存态，不丢失存盘内容。

三种关系：
  legacy  当前 RFC 读法：回复存盘是本地提交，关注通知没有持久远端义务。
  fixed   提议规则：owner 保留未确认（unacked）的远端关注义务，直到远端
          本地提交并回 ack；通知丢失后仍有强制的重投动作。
  control 安全替代：远端持有 thread 的拉取订阅（显式附加责任），
          通知丢失后可以直接拉取并写入本地队列。

坏谓词：
  回复已存盘 ∧ 目标在线 ∧ 本地队列没有该回复 ∧ 强制关系里没有可执行的投递动作。
  “可执行”以投递权威在线为前提：owner 宕机期间没有动作可执行，那是崩溃窗口，
  不是缺失义务。把崩溃窗口算成见证会让 fixed 和 control 在 owner 宕机时同样误报。

分类为 missing_duty：legacy 缺少“通知未送达”这个持久义务。不判 spec_conflict，
因为本模型没有展开 RFC 0002 §2 “回复落在原 thread（owner 追加）”与
RFC 0003 §3 “回复权威节点”两处措辞的差异；要判 spec_conflict 必须另建一个
把这两处并列建模的搜索。

这个状态在 legacy 的强制关系里是**稳定停滞**：从该状态可达的状态中，
本地队列永远不为真。这里不称 deadlock：成员持有 read 授权，理论上可以主动
拉 thread，但通知丢了之后它没有可恢复的游标，不知道该拉什么，所以拉取
不属于 legacy 的强制关系。它是 control 的显式附加责任，不是 legacy 已经
提供的路径。

状态是 frozen dataclass，转移是显式命名的动作，visited 去重，FIFO 队列
给出最短动作轨迹，全部边界显式：MAX_DEPTH 步、MAX_STATES 个状态。
停滞用有界搜索判定：结果是“在边界内没有恢复路径”，不是永久的 await。
结论只覆盖本模型的两个节点、这些动作和给定边界，不代表整份 RFC 正确。
run() 返回结果列表；作为脚本运行时 stdout 输出共享 JSON；
预期见证缺失，或某个 control 出现坏谓词，退出码非零。
"""

from __future__ import annotations

import json
import sys
from collections import deque
from dataclasses import dataclass, replace
from typing import Callable, Optional

MAX_DEPTH = 12
MAX_STATES = 4096


@dataclass(frozen=True)
class State:
    """抽象状态：只保留影响推进条件的事实，不保留协议表面实体。

    崩溃只清除内存态（notice_pending、notice_inflight）；
    reply_stored、obligation_open、local_queue、subscribed 是持久事实，
    任何动作都不清除它们（回复只能因 append 产生，队列只能因本地提交产生）。
    """

    owner_up: bool = True
    target_up: bool = True
    reply_stored: bool = False  # owner 的 thread 存盘里有这条回复
    notice_pending: bool = False  # legacy：内存里的待发通知意图，崩溃即丢失
    notice_inflight: bool = False  # one-shot 通知已发出、未落地，不是持久态
    obligation_open: bool = False  # fixed：未确认的远端关注义务，持久
    local_queue: bool = False  # 远端节点本地队列里已有该回复
    subscribed: bool = False  # control：远端维护一个可持续推进的拉取游标
    loss_observed: bool = False  # 监控变量：通知已经丢过一次或崩溃打断了发送路径


@dataclass(frozen=True)
class Action:
    name: str
    step: Callable[[State], Optional[State]]
    delivery: bool  # 是否属于“把已存盘的回复交到远端队列”的投递动作

    def enabled(self, state: State) -> bool:
        return self.step(state) is not None


def _append_reply(obligation: bool) -> Callable[[State], Optional[State]]:
    def step(state: State) -> Optional[State]:
        if not state.owner_up or state.reply_stored:
            return None
        if obligation:
            return replace(state, reply_stored=True, obligation_open=True)
        return replace(state, reply_stored=True, notice_pending=True)

    return step


def _send_notice(state: State) -> Optional[State]:
    if not state.owner_up or not state.notice_pending or state.notice_inflight:
        return None
    return replace(state, notice_pending=False, notice_inflight=True)


def _lose_notice(state: State) -> Optional[State]:
    if not state.notice_inflight:
        return None
    return replace(state, notice_inflight=False, loss_observed=True)


def _local_commit(state: State) -> Optional[State]:
    """远端节点把收到的回复写入自己的本地队列。"""
    if not state.notice_inflight or not state.target_up:
        return None
    return replace(state, notice_inflight=False, local_queue=True)


def _deliver_notice(state: State) -> Optional[State]:
    if not state.owner_up or not state.obligation_open:
        return None
    if state.local_queue or state.notice_inflight:
        return None
    return replace(state, notice_inflight=True)


def _ack_notice(state: State) -> Optional[State]:
    if not state.owner_up or not state.target_up:
        return None
    if not state.obligation_open or not state.local_queue:
        return None
    return replace(state, obligation_open=False)


def _pull_thread(state: State) -> Optional[State]:
    if not state.subscribed or not state.owner_up or not state.target_up:
        return None
    if not state.reply_stored or state.local_queue:
        return None
    return replace(state, local_queue=True)


def _crash_owner(state: State) -> Optional[State]:
    if not state.owner_up:
        return None
    # 崩溃丢掉内存里的待发意图和在途通知，等于这次发送路径中断。
    return replace(
        state,
        owner_up=False,
        notice_pending=False,
        notice_inflight=False,
        loss_observed=True,
    )


def _recover_owner(state: State) -> Optional[State]:
    if state.owner_up:
        return None
    return replace(state, owner_up=True)


def _crash_target(state: State) -> Optional[State]:
    if not state.target_up:
        return None
    return replace(state, target_up=False)


def _recover_target(state: State) -> Optional[State]:
    if state.target_up:
        return None
    return replace(state, target_up=True)


def _legacy_actions() -> tuple[Action, ...]:
    return (
        Action("append_reply", _append_reply(obligation=False), delivery=False),
        Action("send_notice", _send_notice, delivery=True),
        Action("lose_notice", _lose_notice, delivery=False),
        Action("local_commit", _local_commit, delivery=True),
        Action("crash_owner", _crash_owner, delivery=False),
        Action("recover_owner", _recover_owner, delivery=False),
        Action("crash_target", _crash_target, delivery=False),
        Action("recover_target", _recover_target, delivery=False),
    )


def _fixed_actions() -> tuple[Action, ...]:
    return (
        Action("append_reply", _append_reply(obligation=True), delivery=False),
        Action("deliver_notice", _deliver_notice, delivery=True),
        Action("lose_notice", _lose_notice, delivery=False),
        Action("local_commit", _local_commit, delivery=True),
        Action("ack_notice", _ack_notice, delivery=False),
        Action("crash_owner", _crash_owner, delivery=False),
        Action("recover_owner", _recover_owner, delivery=False),
        Action("crash_target", _crash_target, delivery=False),
        Action("recover_target", _recover_target, delivery=False),
    )


def _control_actions() -> tuple[Action, ...]:
    return (
        Action("append_reply", _append_reply(obligation=False), delivery=False),
        Action("send_notice", _send_notice, delivery=True),
        Action("lose_notice", _lose_notice, delivery=False),
        Action("local_commit", _local_commit, delivery=True),
        Action("pull_thread", _pull_thread, delivery=True),
        Action("crash_owner", _crash_owner, delivery=False),
        Action("recover_owner", _recover_owner, delivery=False),
        Action("crash_target", _crash_target, delivery=False),
        Action("recover_target", _recover_target, delivery=False),
    )


@dataclass(frozen=True)
class Scenario:
    kind: str
    classification: str
    initial: State
    actions: tuple[Action, ...]
    witness_expected: bool


SCENARIOS: tuple[Scenario, ...] = (
    Scenario("legacy", "missing_duty", State(), _legacy_actions(), True),
    Scenario("fixed", "missing_duty", State(), _fixed_actions(), False),
    # 订阅是加入成员时就建立的附加责任，所以初始状态已订阅。
    Scenario("control", "chosen_implementation", State(subscribed=True), _control_actions(), False),
)


def bad_predicate(state: State, actions: tuple[Action, ...]) -> bool:
    """回复已存盘、目标在线、本地队列没有它，且没有可执行的投递动作。"""
    if not (state.reply_stored and state.owner_up and state.target_up):
        return False
    if state.local_queue:
        return False
    return not any(action.delivery and action.enabled(state) for action in actions)


def notice_lost_pending(state: State, kind: str) -> bool:
    """通知已经丢失（或崩溃丢掉了发送意图）、回复仍在存盘里的状态。

    loss_observed 是监控变量，不参与任何转移的守卫；它只区分
    “还没尝试发送”与“发送过并且丢了”，避免把前者当成停滞见证。
    """
    if not (state.reply_stored and state.owner_up and state.target_up):
        return False
    if not state.loss_observed or state.local_queue or state.notice_inflight:
        return False
    if kind == "fixed":
        return state.obligation_open
    return not state.notice_pending


@dataclass
class Exploration:
    order: list[State]
    parent: dict[State, tuple[Optional[State], str]]
    depth: dict[State, int]
    truncated: bool


def explore(scenario: Scenario) -> Exploration:
    start = scenario.initial
    parent: dict[State, tuple[Optional[State], str]] = {start: (None, "")}
    depth: dict[State, int] = {start: 0}
    order: list[State] = [start]
    queue: deque[State] = deque([start])
    truncated = False

    while queue:
        state = queue.popleft()
        if depth[state] >= MAX_DEPTH:
            # 边界上的状态若还有后继，说明搜索被深度截断。
            if any(action.step(state) is not None for action in scenario.actions):
                truncated = True
            continue
        for action in scenario.actions:
            nxt = action.step(state)
            if nxt is None or nxt in parent:
                continue
            if len(parent) >= MAX_STATES:
                truncated = True
                continue
            parent[nxt] = (state, action.name)
            depth[nxt] = depth[state] + 1
            order.append(nxt)
            queue.append(nxt)

    return Exploration(order, parent, depth, truncated)


def trace_to(state: State, parent: dict[State, tuple[Optional[State], str]]) -> list[str]:
    names: list[str] = []
    while True:
        prev, name = parent[state]
        if prev is None:
            break
        names.append(name)
        state = prev
    names.reverse()
    return names


def closure(start: State, actions: tuple[Action, ...]) -> tuple[set[State], bool]:
    seen: set[State] = {start}
    queue: deque[State] = deque([start])
    truncated = False
    while queue:
        state = queue.popleft()
        for action in actions:
            nxt = action.step(state)
            if nxt is None or nxt in seen:
                continue
            if len(seen) >= MAX_STATES:
                truncated = True
                continue
            seen.add(nxt)
            queue.append(nxt)
    return seen, truncated


def enabled_delivery_names(state: State, actions: tuple[Action, ...]) -> list[str]:
    return [action.name for action in actions if action.delivery and action.enabled(state)]


def shortest_path(
    start: State,
    actions: tuple[Action, ...],
    target: Callable[[State], bool],
) -> Optional[list[str]]:
    """从 start 出发到达首个满足 target 的状态的最短动作轨迹。"""
    if target(start):
        return []
    parent: dict[State, tuple[Optional[State], str]] = {start: (None, "")}
    queue: deque[State] = deque([start])
    while queue:
        state = queue.popleft()
        for action in actions:
            nxt = action.step(state)
            if nxt is None or nxt in parent:
                continue
            if len(parent) >= MAX_STATES:
                return None
            parent[nxt] = (state, action.name)
            if target(nxt):
                return trace_to(nxt, parent)
            queue.append(nxt)
    return None


def recovery_target(kind: str) -> Callable[[State], bool]:
    """恢复成立的条件：fixed 还要看到 ack 把义务收掉。"""
    if kind == "fixed":
        return lambda state: state.local_queue and not state.obligation_open
    return lambda state: state.local_queue


def result_for(scenario: Scenario) -> dict:
    exploration = explore(scenario)
    actions = scenario.actions
    max_depth_reached = max(exploration.depth.values())

    witness = next(
        (state for state in exploration.order if bad_predicate(state, actions)),
        None,
    )
    stalled = next(
        (state for state in exploration.order if notice_lost_pending(state, scenario.kind)),
        None,
    )

    probe = witness if witness is not None else stalled
    reachable, closure_truncated = closure(probe, actions) if probe is not None else (set(), False)
    queue_reachable = any(state.local_queue for state in reachable)
    durable_preserved = all(state.reply_stored for state in reachable) if reachable else False
    recovery = (
        shortest_path(stalled, actions, recovery_target(scenario.kind))
        if stalled is not None and witness is None
        else None
    )

    witness_found = witness is not None
    if scenario.witness_expected:
        check_pass = (
            witness_found
            and not queue_reachable
            and durable_preserved
            and not exploration.truncated
            and not closure_truncated
        )
    else:
        check_pass = (
            not witness_found
            and stalled is not None
            and recovery is not None
            and durable_preserved
            and not exploration.truncated
            and not closure_truncated
        )

    if witness is not None:
        trace = trace_to(witness, exploration.parent)
        final: dict = {
            "owner_up": witness.owner_up,
            "target_up": witness.target_up,
            "reply_stored": witness.reply_stored,
            "local_queue": witness.local_queue,
            "enabled_delivery_actions": enabled_delivery_names(witness, actions),
            "queue_reachable_from_witness": queue_reachable,
            "durable_reply_preserved": durable_preserved,
            "max_depth_reached": max_depth_reached,
            "closure_states": len(reachable),
            "stalled_under_mandatory_relation": True,
            "recovery_requires": "unacked remote attention obligation or a pull subscription",
        }
    elif stalled is not None:
        trace = trace_to(stalled, exploration.parent)
        final = {
            "owner_up": stalled.owner_up,
            "target_up": stalled.target_up,
            "reply_stored": stalled.reply_stored,
            "local_queue": stalled.local_queue,
            "obligation_open": stalled.obligation_open,
            "subscribed": stalled.subscribed,
            "enabled_delivery_actions": enabled_delivery_names(stalled, actions),
            "queue_reachable_from_stall": queue_reachable,
            "recovery_trace": recovery,
            "durable_reply_preserved": durable_preserved,
            "max_depth_reached": max_depth_reached,
            "closure_states": len(reachable),
        }
    else:
        trace = []
        final = {
            "enabled_delivery_actions": [],
            "queue_reachable_from_stall": queue_reachable,
            "durable_reply_preserved": durable_preserved,
            "max_depth_reached": max_depth_reached,
            "closure_states": len(reachable),
        }

    final["truncated"] = exploration.truncated or closure_truncated

    return {
        "scenario": "remote_reply_notice_loss",
        "kind": scenario.kind,
        "classification": scenario.classification,
        "bounds": {"max_depth": MAX_DEPTH, "max_states": MAX_STATES},
        "states_explored": len(exploration.parent),
        "witness_expected": scenario.witness_expected,
        "check_pass": check_pass,
        "trace": trace,
        "final": final,
    }


def run() -> list[dict]:
    return [result_for(scenario) for scenario in SCENARIOS]


def main() -> int:
    results = run()
    payload = {
        "scenario": "remote_reply",
        "results": results,
        "all_checks_pass": all(result["check_pass"] for result in results),
    }
    print(json.dumps(payload, indent=2))
    return 0 if payload["all_checks_pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
