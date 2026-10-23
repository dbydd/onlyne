#!/usr/bin/env python3
"""reply_exposure_overlap: 一条回复同时走 await 工具结果和默认片段时，正文是否只被消费一次。

链条：
  回复条目与对等待方的关注义务在同一事务提交（0003:180）；
  网络接收路径匹配 reply 后解除等待（0005:73-76）；
  宿主把 await 工具结果交给上下文，调用本身返回 void（0008:248）；
  上下文消费边界确认这条工具结果已被消费（0008:264）；
  注入游标只按 (context, thread, seq) 记录片段注入，不因工具结果推进（0004:153）；
  默认片段仍收录这条关注条目（0008:107、:122），于是同一正文再次进入上下文。

计数口径：重复只按已确认的消费计。submit 返回 void 不算确认（0008:248、:264）；
未确认的投送不计入重复。等待结局未知时片段照常送达，那是重试，不是重复；
本模型不声称 exactly-once。

两种送达顺序：
  await_first     等待中先由工具结果送达，确认后自动片段再送一次。
  fragment_first  自动片段先送达并确认；之后建立等待者，按 0005:76 立即用本地证据解除。

规则：
  current    现行规则，没有跨路径共享的消费记录：两种顺序都得到两次已确认消费。
             0004:156 只说重复注入“可以折叠”，是许可不是 MUST，所以这是策略/契约
             缺口，不是已确立的违规。
  candidate  共享 (context, thread, seq) 消费记录 + 协同调度：同一正文只被确认消费
             一次；等待结局未知时仍放行片段，边界不被过度收紧。

依据：
  docs/rfcs/0003-messages.md:56-64、:153-180
  docs/rfcs/0004-events-and-views.md:140-158
  docs/rfcs/0005-delivery-and-runtime.md:71-84
  docs/rfcs/0008-omp-surface.md:99-132、:164-190、:221-232、:238-266

有界搜索只否证性质：absence 只在本文件的边界内成立。
"""

from __future__ import annotations

import json
import sys
from collections import deque
from dataclasses import asdict, dataclass, replace
from typing import Callable, Dict, Iterable, List, Optional


def reach(start: "Reply", edges: Callable[["Reply"], Iterable]) -> Dict["Reply", tuple]:
    """有界 BFS；返回状态到 (前驱, 动作) 的映射。"""
    parents = {start: (None, "")}
    queue = deque([start])
    while queue:
        state = queue.popleft()
        for action, nxt in edges(state):
            if nxt in parents:
                continue
            parents[nxt] = (state, action)
            queue.append(nxt)
    return parents


def trace(parents, state) -> List[str]:
    actions = []
    while parents[state][0] is not None:
        state, action = parents[state]
        actions.append(action)
    return list(reversed(actions))


@dataclass(frozen=True)
class Reply:
    # 回复条目（seq=1）与等待方关注义务已同事务提交（0003:180）
    persisted: bool = False
    # 上下文已注册等待者；reply 先到、等待者后建时按 0005:76 立即解除
    registered: bool = False
    # 网络接收路径匹配 reply，解除本地等待（0005:73-76）
    released: bool = False
    # await 工具结果已交给宿主；void 返回，不证明消费（0008:248）
    returned: bool = False
    # 该工具结果是否携带答复正文（0008:186）；candidate 在正文已消费时改带引用
    body_in_result: bool = False
    # 上下文消费边界确认工具结果已消费（0008:264）
    consumed: bool = False
    # 宿主从未回报消费；模型边界里的终态
    unknown: bool = False
    # 注入游标 (context, thread, seq)：0 未推进，1 已推进（0004:153）
    cursor: int = 0
    # 默认片段对这条回复的投送次数（上限 1）
    auto: int = 0
    # 片段投送已被确认消费的次数；确认才推进游标（0008:264）
    auto_confirmed: int = 0


def confirmed(state: Reply) -> int:
    """已确认的正文消费次数：工具结果一次，片段至多一次。"""
    return (1 if state.consumed else 0) + state.auto_confirmed


def duplicate(state: Reply) -> bool:
    return confirmed(state) >= 2


def edges(kind: str) -> Callable[[Reply], Iterable]:
    def step(s: Reply):
        if not s.persisted:
            yield "persist_reply_and_attention", replace(s, persisted=True)

        if not s.registered:
            # 建立等待者时先查本地已持久化的合法 reply 证据（0005:76）
            yield "register_await", replace(
                s, registered=True, released=s.released or s.persisted
            )

        if s.registered and s.persisted and not s.released:
            yield "release_waiter", replace(s, released=True)

        if s.released and not s.returned:
            # 现行规则总把答复正文放进工具结果（0008:186）；
            # 共享记录命中时只给原消息引用与到达事实。
            body = kind == "current" or confirmed(s) == 0
            yield "submit_await_result", replace(s, returned=True, body_in_result=body)

        if s.returned and s.body_in_result and not s.consumed and not s.unknown:
            yield "confirm_await_consumed", replace(s, consumed=True)

        if s.returned and not s.consumed and not s.unknown:
            yield "await_outcome_unknown", replace(s, unknown=True)

        if s.persisted and s.auto == 0 and s.cursor == 0 and fragment_allowed(kind, s):
            yield "inject_fragment", replace(s, auto=1)

        if s.auto == 1 and s.auto_confirmed == 0:
            yield "confirm_fragment_consumed", replace(
                s, auto_confirmed=1, cursor=1
            )

    return step


def fragment_allowed(kind: str, s: Reply) -> bool:
    # 上下文停在 await 工具调用里时不注入，来信继续排队（0008:230）
    if s.registered and not s.returned:
        return False
    if kind == "current":
        return True
    # candidate：已确认消费的条目不进自动投送；等待结果未决时也不抢先投送，
    # 否则迟到的消费确认会与片段确认叠加成第二次。
    if confirmed(s) > 0:
        return False
    if s.registered and not (s.consumed or s.unknown):
        return False
    return True


def result(
    name: str,
    kind: str,
    classification: str,
    parents,
    predicate: Callable[[Reply], bool],
    expected: bool,
    bounds: Dict,
    note: str,
) -> Dict:
    witness: Optional[Reply] = next((s for s in parents if predicate(s)), None)
    return {
        "scenario": name,
        "kind": kind,
        "classification": classification,
        "bounds": bounds,
        "states_explored": len(parents),
        "witness_expected": expected,
        "check_pass": (witness is not None) == expected,
        "trace": trace(parents, witness) if witness is not None else [],
        "final": {
            "witness_state": asdict(witness) if witness is not None else None,
            "duplicate_reachable": any(duplicate(s) for s in parents),
            "max_confirmed_presentations": max(confirmed(s) for s in parents),
            "fragment_delivered": any(s.auto_confirmed > 0 for s in parents),
            "cursor_reached": max(s.cursor for s in parents),
        },
        "note": note,
    }


AWAIT_FIRST_BOUNDS = {
    "contexts": 1,
    "threads": 1,
    "replies": 1,
    "seq": 1,
    "waiters": 1,
    "fragment_deliveries": 1,
    "host_outcomes": 2,
    "initial_state": "waiter_registered",
    "frontier_exhausted": True,
}

FRAGMENT_FIRST_BOUNDS = {
    "contexts": 1,
    "threads": 1,
    "replies": 1,
    "seq": 1,
    "waiters": 1,
    "fragment_deliveries": 1,
    "initial_state": "fragment_confirmed",
    "frontier_exhausted": True,
}


def await_first():
    start = Reply(registered=True)
    bounds = AWAIT_FIRST_BOUNDS
    return [
        result(
            "reply_await_then_fragment_current",
            "current",
            "policy_contract_gap",
            reach(start, edges("current")),
            duplicate,
            True,
            bounds,
            "等待解除后工具结果直接把答复正文交给上下文（0008:186、:190）；"
            "确认该消费（0008:264）不推进注入游标，游标只按片段注入记录（0004:153）。"
            "默认片段仍收录这条关注条目（0008:107、:122），同一 "
            "(context, thread, seq) 的正文因此被确认消费两次。0004:156 只写"
            "“可以折叠”，是许可不是 MUST，所以这是策略/契约缺口，不是已确立的违规。",
        ),
        result(
            "reply_await_then_fragment_candidate",
            "candidate",
            "candidate_repair",
            reach(start, edges("candidate")),
            duplicate,
            False,
            bounds,
            "共享 (context, thread, seq) 消费记录 + 协同调度：片段只在没有已确认消费、"
            "且没有待决等待结果时投送；等待确认后该条目对片段已是已消费，正文只出现一次。"
            "等待语义不变，工具结果仍报告到达与原消息引用（0005:76、0008:186）。",
        ),
        result(
            "reply_unknown_outcome_retry_candidate",
            "candidate",
            "candidate_repair_boundary",
            reach(start, edges("candidate")),
            lambda s: s.unknown and s.auto_confirmed == 1,
            True,
            bounds,
            "宿主从不回报消费时（0008:248 的 void 返回、:264 只认消费边界确认），"
            "片段必须照常送达，这是重试不是重复：未确认的提交不计入重复，"
            "已确认消费仍为 1。模型把未知结局当终态；迟到的观测不在本边界内，"
            "不声称 exactly-once。",
        ),
    ]


def fragment_first():
    start = Reply(persisted=True, auto=1, auto_confirmed=1, cursor=1)
    bounds = FRAGMENT_FIRST_BOUNDS
    return [
        result(
            "reply_fragment_then_await_current",
            "current",
            "policy_contract_gap",
            reach(start, edges("current")),
            duplicate,
            True,
            bounds,
            "片段先送达并确认，游标已到该 seq；随后建立等待者时按 0005:76 立即用"
            "本地证据解除等待，工具结果再次给出答复正文（0008:186）。现行规则没有"
            "跨路径共享的消费记录，第二条路径不知道第一条已经送达。",
        ),
        result(
            "reply_fragment_then_await_candidate",
            "candidate",
            "candidate_repair",
            reach(start, edges("candidate")),
            duplicate,
            False,
            bounds,
            "同一记录反向起作用：该条目已由片段确认送达时，await 工具结果只带原消息"
            "引用与到达事实，不再重复正文。要成立需把 0004:156 的“可以折叠”收紧为"
            "MUST，并在 0008:164-190 明确正文已在同一上下文时工具结果以引用表示。",
        ),
    ]


def run() -> List[Dict]:
    results: List[Dict] = []
    results.extend(await_first())
    results.extend(fragment_first())
    return results


def main() -> int:
    results = run()
    ok = all(item["check_pass"] for item in results)
    print(
        json.dumps(
            {
                "scenario": "reply_exposure_overlap",
                "results": results,
                "all_checks_pass": ok,
                "claim": "bounded search; a green run reproduces the current-rule gaps and finds no confirmed duplicate under the candidate, which still delivers on unknown host outcomes",
            },
            ensure_ascii=False,
            indent=2,
        )
    )
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
