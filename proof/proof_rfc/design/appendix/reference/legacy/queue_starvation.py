#!/usr/bin/env python3
"""关注队列饥饿与资源隔离：有界状态搜索，JSON 输出。

场景 queue_starvation：agent 的单条关注队列里，队头条目永久没有可用的解决动作，
按队头消费的实现让后面可消费的条目永远轮不到。
场景 resource_isolation：两个独立资源各自积压，一个资源的积压不阻塞另一个（RFC 0006 §6）。
"""

from __future__ import annotations

import json
import sys
from collections import deque
from dataclasses import dataclass, replace

MAX_STATES = 512
MAX_DEPTH = 12


@dataclass(frozen=True)
class State:
    """不可变状态。queues 保持顺序；其余字段是集合的有序投影。"""

    queues: tuple[tuple[str, tuple[str, ...]], ...]
    consumed: tuple[str, ...] = ()
    parked: tuple[str, ...] = ()
    blocked: tuple[str, ...] = ()
    closed: tuple[str, ...] = ()


@dataclass(frozen=True)
class Scenario:
    name: str
    watched: str
    queues: tuple[tuple[str, tuple[str, ...]], ...]
    blocked: tuple[str, ...] = ()
    closable: tuple[tuple[str, tuple[str, ...]], ...] = ()

    def initial(self) -> State:
        return State(queues=self.queues, blocked=tuple(sorted(self.blocked)))


def _replace_queue(state: State, qid: str, entries: tuple[str, ...]) -> State:
    return replace(
        state,
        queues=tuple((q, entries if q == qid else cur) for q, cur in state.queues),
    )


def _queued(state: State) -> set[str]:
    return {name for _, entries in state.queues for name in entries}


def _close(scenario: Scenario, state: State):
    queued = _queued(state)
    for obj, owned in scenario.closable:
        if obj in state.closed:
            continue
        blocked = set(state.blocked) | {name for name in owned if name in queued}
        yield (
            f"close:{obj}",
            replace(
                state,
                closed=tuple(sorted(state.closed + (obj,))),
                blocked=tuple(sorted(blocked)),
            ),
        )


def _consume(state: State, qid: str, index: int) -> tuple[str, State]:
    entries = next(cur for q, cur in state.queues if q == qid)
    name = entries[index]
    remaining = entries[:index] + entries[index + 1:]
    return name, replace(
        state,
        queues=tuple(
            (q, remaining if q == qid else cur) for q, cur in state.queues
        ),
        consumed=tuple(sorted(state.consumed + (name,))),
    )


def _consume_head(state: State):
    for qid, entries in state.queues:
        if entries and entries[0] not in state.blocked:
            _, nxt = _consume(state, qid, 0)
            yield f"consume_head:{qid}", nxt


def _consume_any(state: State):
    for qid, entries in state.queues:
        for index, name in enumerate(entries):
            if name not in state.blocked:
                _, nxt = _consume(state, qid, index)
                yield f"consume_any_eligible:{qid}", nxt
                break


def _park_head(state: State):
    for qid, entries in state.queues:
        if entries and entries[0] in state.blocked:
            name = entries[0]
            yield (
                f"park_stalled_head:{qid}",
                replace(
                    _replace_queue(state, qid, entries[1:]),
                    parked=tuple(sorted(state.parked + (name,))),
                ),
            )


def transitions(kind: str, scenario: Scenario, state: State):
    actions = list(_close(scenario, state))
    if kind == "legacy":
        actions.extend(_consume_head(state))
    elif kind == "fixed":
        actions.extend(_park_head(state))
        actions.extend(_consume_head(state))
    elif kind == "control":
        actions.extend(_consume_any(state))
    else:
        raise ValueError(f"unknown kind: {kind}")
    return actions


def _still_consumable(kind: str, scenario: Scenario, state: State):
    """从 state 可达的状态里 watched 是否可能被消费；越过边界时返回 None。"""

    watched = scenario.watched
    if watched in state.consumed:
        return True
    seen = {state}
    queue = deque([state])
    while queue:
        cur = queue.popleft()
        for _, nxt in transitions(kind, scenario, cur):
            if watched in nxt.consumed:
                return True
            if nxt in seen:
                continue
            if len(seen) >= MAX_STATES:
                return None
            seen.add(nxt)
            queue.append(nxt)
    return False


def starved(kind: str, scenario: Scenario, state: State):
    """坏谓词：watched 可消费、未消费，且模型里再也没有能消费它的动作。"""

    watched = scenario.watched
    if watched in state.consumed or watched in state.blocked:
        return False
    reachable = _still_consumable(kind, scenario, state)
    if reachable is None:
        return None
    return not reachable


def explore(kind: str, scenario: Scenario):
    init = scenario.initial()
    seen = {init}
    queue = deque([(init, 0, ())])
    witness = None
    terminal = None
    exhausted = True
    unknown = False
    while queue:
        state, depth, trace = queue.popleft()
        succ = transitions(kind, scenario, state)
        if terminal is None and not succ:
            terminal = (state, trace)
        if witness is None:
            verdict = starved(kind, scenario, state)
            if verdict is None:
                unknown = True
            elif verdict:
                witness = (state, trace)
        for label, nxt in succ:
            if nxt in seen:
                continue
            if depth + 1 > MAX_DEPTH or len(seen) >= MAX_STATES:
                exhausted = False
                continue
            seen.add(nxt)
            queue.append((nxt, depth + 1, trace + (label,)))
    return seen, witness, terminal, exhausted, unknown


def _final(state: State) -> dict:
    return {
        "queues": {qid: list(entries) for qid, entries in state.queues},
        "consumed": list(state.consumed),
        "parked": list(state.parked),
        "blocked": list(state.blocked),
        "closed": list(state.closed),
    }


def _result(
    kind: str,
    scenario: Scenario,
    witness_expected: bool,
    detail: str,
) -> dict:
    seen, witness, terminal, exhausted, unknown = explore(kind, scenario)
    shown_state, trace = (
        witness
        if witness is not None
        else terminal
        if terminal is not None
        else (scenario.initial(), ())
    )
    ok = (witness is not None) == witness_expected and not (witness is None and unknown)
    return {
        "scenario": scenario.name,
        "kind": kind,
        "classification": "chosen_implementation",
        "bounds": {
            "max_states": MAX_STATES,
            "max_depth": MAX_DEPTH,
            "queues": len(scenario.queues),
            "entries": sum(len(entries) for _, entries in scenario.queues),
            "frontier_exhausted": exhausted,
        },
        "states_explored": len(seen),
        "witness_expected": witness_expected,
        "check_pass": ok,
        "trace": list(trace),
        "final": _final(shown_state),
        "detail": detail,
    }


STARVATION = Scenario(
    name="queue_starvation",
    watched="t2",
    queues=(("inbox", ("t1", "t2")),),
    closable=(("thread:T1", ("t1",)),),
)

ISOLATION = Scenario(
    name="resource_isolation",
    watched="e3",
    queues=(("R1", ("e1", "e2")), ("R2", ("e3",))),
    blocked=("e1",),
)


def run() -> list[dict]:
    return [
        _result(
            "legacy",
            STARVATION,
            True,
            "legacy=按 agent 单一关注队列的顺序消费（0004 §1 的 inbox 是每个主体一条列表）。"
            "t1 入队后主持人关闭它所在的 thread，之后对该条目的解决动作（回复）只能得到 "
            "thread_closed（docs/rfcs/0002-threads.md:174、:180），t1 永远排不掉，队头不动，"
            "t2 永远轮不到。RFC 从未要求全局 FIFO：0003 只规定 thread 内 seq 递增"
            "（0003:38），inbox 按 thread 聚合而不规定跨条目消费顺序（0004:64），SSoT 的关注面"
            "把 queued 建模为集合、没有队头（proof/proof_rfc/design/architecture/attention/"
            "attention.md:32），Inject 也不以 thread 仍打开为前提（attention.md:62-67）。"
            "因此这是实现选择（chosen_implementation），不是规范冲突；Automatic 本来就排除"
            "已关闭 thread 的条目（attention.md:69-74），legacy 不是唯一读法。停滞只发生在该"
            "实现的强制关系里：legacy 只有队头消费，直接读 t2 所在的 thread（0004 §2）是可选"
            "查询，所以它不是不可恢复的死锁，也不是信息丢失（关闭不删历史，0002:34）。"
            "consumed 对应运行时注入游标（0004 §5），不是消息状态（0003 §1）。"
            "有界搜索只否证性质，不证明性质。",
        ),
        _result(
            "fixed",
            STARVATION,
            False,
            "fixed=拟定规则（提案，不是 RFC 正文）：队头条目在该实现的强制关系里没有可用的"
            "解决动作时，移入 parked 并保留，消费继续处理后面的条目。parked 不删除、不算"
            "已消费，也不改动消息与历史；口径与 Automatic 排除已关闭 thread 的条目一致"
            "（proof/proof_rfc/design/architecture/attention/attention.md:69-74）。没有建模解除阻塞"
            "后把条目放回队列的动作，因为本场景的阻塞是终态事实。t2 在每条轨迹上都被消费。",
        ),
        _result(
            "control",
            STARVATION,
            False,
            "control=安全替代：每步在队列里选第一条可解决条目消费，不可解决的条目留在队列里。"
            "关闭与消费的先后都不影响 t2 被消费，坏谓词不出现。控制组只说明这条替代规则不会"
            "饥饿，不评价 legacy 的取舍。",
        ),
        _result(
            "legacy",
            ISOLATION,
            False,
            "两个独立资源各有自己的积压（0006 §3、§4 的绑定表与积压都是 owner 的局部状态）。"
            "R1 队头 e1 的接收者不可达，endpoint 保留事件并暴露未送达记录（0006:191）；"
            "R2 的 e3 照常投送。0006:191 要求该资源的积压不阻塞其他资源，这条读法满足它；"
            "R1 自身队头的阻塞与场景一属于同一个实现选择，不在这里判成协议缺陷。",
        ),
        _result(
            "fixed",
            ISOLATION,
            False,
            "fixed=拟定规则（提案）：R1 队头没有可用解决动作时移入 parked 后继续投送 e2，"
            "R2 的 e3 不受影响，两个队列的推进互不依赖。0006:191 的隔离要求在该规则下仍成立。",
        ),
    ]


def main() -> int:
    results = run()
    payload = {
        "scenario": "queue_starvation",
        "results": results,
        "all_checks_pass": all(item["check_pass"] for item in results),
    }
    json.dump(payload, sys.stdout, ensure_ascii=False, indent=2)
    sys.stdout.write("\n")
    return 0 if payload["all_checks_pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
