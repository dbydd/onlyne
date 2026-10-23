#!/usr/bin/env python3
"""ack_authority_race: 投递确认只认事件的首次归属接收者。

模型覆盖 endpoint 已持久接收的两条事件及其投递确认（delivery_ack）：

  1. 两条事件 e1、e2 已按 `(resource_ref, platform_event_id)` 去重并记录首次归属，
     首次归属者都是 A（0006:181-183）；
  2. 确认按 `event_id` 记录，确认方可以是 A、B 或不明的第三方（0012:102）；
  3. 确认幂等；保留策略可以给未确认义务一个明确终态（0006:190、0012:191）。

两种规则：
  current   0012:102 的字面读法：endpoint 校验确认方是该事件的首次归属接收者。
            非首次归属者的确认改变不了任何协议状态，事件留在积压里。
  control   回归探针：去掉确认方校验，任何发送者都能清掉该事件的积压。这是反事实，
            不是 RFC 文本；用来证明 current 的拒绝来自校验本身，而不是模型里没有这条路径。

坏谓词：已确认集合里存在一条事件的确认发送者不是它的首次归属者。
此时该事件的投递义务被错误地记为已清空：endpoint 不再把它推给首次归属者，
而正文可能从未被首次归属者持久保存。协议没有撤销确认的动作，也没有把积压
转给另一个节点的操作（0006:190），所以这条错误记录不可修复。

current 下坏状态不可达（safety_control）；control 下 `ack(e1, B)` 一步到达
（spec_conflict，因为它直接违反 0012:102）。有界搜索只否证性质：absence 只在
本文件的边界内成立。

模型不包含「更正错误确认」或「把义务转回首次归属者」的动作：两条 RFC 都没有
提供这样的操作。要修 control 那条规则，需要新增一条收回确认的协议动作。

依据：
  docs/rfcs/0006-resources-and-handover.md:181-202
  docs/rfcs/0012-im-resource-node.md:100-102、:138、:176、:191-193
"""

from __future__ import annotations

import json
import sys
from collections import deque
from dataclasses import asdict, dataclass, replace

FIRST_OWNER = "A"
SENDERS = ("A", "B", "unknown")
EVENTS = ("e1", "e2")
MAX_DEPTH = 16
MAX_STATES = 4096


@dataclass(frozen=True, slots=True)
class State:
    pending: tuple[str, ...] = EVENTS  # 已持久接收、尚未确认、未被保留策略终结
    acked: tuple[tuple[str, str], ...] = ()  # (event_id, 确认发送者)，按事件 id 排序
    gone: tuple[str, ...] = ()  # 保留策略给出明确终态的事件，正文已删


def ack_decision(mode: str, s: State, event: str, sender: str) -> str:
    """对一次确认请求的判定；只有 accepted 会改变协议状态。"""
    if any(acked_event == event for acked_event, _ in s.acked):
        return "idempotent"  # 0012:102：确认幂等
    if event in s.gone:
        return "rejected_gone"  # 0006:200：内容删除后按 not_found 处理
    if event not in s.pending:
        return "rejected_unknown"
    if mode == "current" and sender != FIRST_OWNER:
        return "rejected_authority"  # 0012:102：确认方必须是首次归属接收者
    return "accepted"


def transitions(mode: str, s: State):
    for event in s.pending:
        for sender in SENDERS:
            if ack_decision(mode, s, event, sender) != "accepted":
                continue
            yield (
                f"ack({event}, {sender}) accepted",
                replace(
                    s,
                    pending=tuple(e for e in s.pending if e != event),
                    acked=tuple(sorted(s.acked + ((event, sender),))),
                ),
            )

    for event in s.pending:
        yield (
            f"retention_expire({event})",
            replace(
                s,
                pending=tuple(e for e in s.pending if e != event),
                gone=tuple(sorted(s.gone + (event,))),
            ),
        )


def resolving_actions(s: State) -> list[str]:
    """把未确认事件推向终态的、规则要求的动作。

    坏状态之后没有可列的动作：错误确认已把该事件移出积压，而协议没有收回确认
    或转交积压的操作。
    """
    return [f"ack({event}, {FIRST_OWNER})" for event in s.pending]


def bad(s: State) -> bool:
    return any(sender != FIRST_OWNER for _, sender in s.acked)


def owner_ack_evidence(s: State) -> bool:
    """首次归属者的确认被接受：积压只由它或被保留策略清空。"""
    return s.acked == ((EVENTS[0], FIRST_OWNER),)


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
            "pending": None,
            "acked": None,
            "gone": None,
            "resolving_actions": None,
            "detail": detail,
        }
    data = asdict(state)
    data["acked"] = [
        {"event_id": event, "ack_sender": sender} for event, sender in state.acked
    ]
    data["resolving_actions"] = resolving_actions(state)
    data["detail"] = detail
    return data


def decision_census(mode: str, exploration: Exploration) -> dict:
    """对每个可达状态、每条事件、每个发送者做一次确认判定。"""
    census = {
        "accepted": 0,
        "rejected_authority": 0,
        "rejected_gone": 0,
        "rejected_unknown": 0,
        "idempotent": 0,
    }
    for state in exploration.order:
        for event in EVENTS:
            for sender in SENDERS:
                census[ack_decision(mode, state, event, sender)] += 1
    return census


def bounds_of(mode: str, exploration: Exploration) -> dict:
    return {
        "events": len(EVENTS),
        "senders": len(SENDERS),
        "first_owner": FIRST_OWNER,
        "max_depth": MAX_DEPTH,
        "max_states": MAX_STATES,
        "exhaustive": not exploration.truncated,
        "wrong_ack_states": sum(1 for s in exploration.order if bad(s)),
        "ack_decisions": decision_census(mode, exploration),
        "scope": (
            "two persisted events first owned by A, three ack senders, "
            "retention expiry as the only non-ack terminal disposition; "
            "no operation retracts an ack or transfers backlog. wrong_ack_states counts "
            "states where an event's obligation is permanently mis-resolved; the rest of "
            "the resource can still progress, so these are loss states, not whole-state stalls"
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
        "scenario": "ack_authority_race",
        "kind": mode,
        "classification": classification,
        "bounds": bounds_of(mode, exploration),
        "states_explored": len(exploration.parent),
        "witness_expected": witness_expected,
        "check_pass": check_pass,
        "trace": trace_of(exploration, trace_state) if trace_state is not None else [],
        "final": final_of(trace_state, detail),
    }


def run() -> list[dict]:
    current = explore("current")
    current_witness = next((s for s in current.order if bad(s)), None)
    current_near = next((s for s in current.order if owner_ack_evidence(s)), None)
    current_census = decision_census("current", current)
    current_ok = (
        current_witness is None
        and current_near is not None
        and not current.truncated
        and current_census["rejected_authority"] > 0
        and current_census["accepted"] > 0
    )
    results = [
        result(
            "current",
            "safety_control",
            current,
            current_near,
            current_ok,
            False,
            "0012:102 要求 endpoint 校验确认方是该事件的首次归属接收者。模型里 B 与 unknown "
            "的确认全部被拒（ack_decisions.rejected_authority），事件留在 pending；"
            "0006:190 规定积压只能由接收者确认或保留策略终态清空，两者都不需要确认方"
            "之外的身份。current 下坏状态不可达。",
        )
    ]

    control = explore("control")
    control_witness = next((s for s in control.order if bad(s)), None)
    control_census = decision_census("control", control)
    control_trace = (
        trace_of(control, control_witness) if control_witness is not None else []
    )
    control_ok = (
        control_witness is not None
        and not control.truncated
        and len(control_trace) == 1
        and control_trace[0].startswith("ack(")
        and not control_trace[0].startswith(f"ack({EVENTS[0]}, {FIRST_OWNER})")
        and control_census["rejected_authority"] == 0
    )
    results.append(
        result(
            "control",
            "spec_conflict",
            control,
            control_witness,
            control_ok,
            True,
            "去掉确认方校验后，B 对 e1 的确认一步被接受，e1 进入已确认集合且发送者不是"
            "首次归属者 A。积压因此被错误清空：endpoint 不再重放 e1，A 可能从未持久保存"
            "正文；0006:190 明确不提供把积压转给另一个节点的操作，也没有动作能撤回确认，"
            "这条错误记录无法修复。该规则与 0012:102 直接冲突，故为 spec_conflict。",
        )
    )
    return results


def main() -> int:
    results = run()
    payload = {
        "scenario": "ack_authority_race",
        "results": results,
        "all_checks_pass": all(r["check_pass"] for r in results),
    }
    print(json.dumps(payload, ensure_ascii=False, indent=2))
    return 0 if payload["all_checks_pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
