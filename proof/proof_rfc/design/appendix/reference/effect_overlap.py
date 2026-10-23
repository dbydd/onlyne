#!/usr/bin/env python3
"""effect_overlap: 同一事实被两条路径分别推进时，效果是否只发生一次。

效果重合指同一身份经不同路径产生重复提交。本文件分别输出受守卫保护的效果、
反事实探针、模型集合性质、未定歧义与外部重试边界。

带守卫的场景各跑两条规则，两条规则共用同一个动作图，只差那条决定性的守卫：
  current   现行规则带去重或单一提交点；重合必须不可达。
  control   反事实探针：去掉那条守卫；重合必须可达。探针失败说明这个检查已经
            不再检查它声称的东西。探针复现的是被守卫挡住的反事实路径，不是现行
            规范的冲突。

另有两类单条结果，不套现规则：
  invariant 模型事实，例如积压是集合，第二次移除是空操作；不声称规范如此。
  ambiguity 规范未定之处，例如 ack 事实与 expired 终态同时命中时的优先级。

场景：
  1. 接纳：同一 MessageRef 重复到达只产生一条条目与一个 seq。
  2. 关注：同一条消息因 mention 与 reply 同时命中，只产生一条义务。两种命中顺序
     都走，义务标识固定为 (thread_ref, message_ref, recipient)。
  3. 事件终态：积压是待处理义务的集合，ack 与保留期过期移除同一事件只有一次有效
     移除；ack 事实与 expired 终态可以并存，谁优先在 RFC 里没写，单独报告为歧义。
  4. 交接提交：decide 与 recover 都能推进已接受的准备，只提交一次。两种顺序都走，
     每条提交路径各尝试一次。
  5. 资源操作：已提交记录的同 ID 重投与查询不再执行。外部效果后的记录丢失可能
     导致重试再次执行；分别观察不重执行策略与重执行策略，不宣称恰好一次。
  6. 域归组：并发建组与加入不同域时最多一条 active 记录；退出后携带旧版本的迟到
     加入被拒，退出墓碑不被覆盖。
  7. 域重投：同一幂等键重投原加入请求返回历史结果，但当前成员状态不变，关系不复活。

依据：
  docs/rfcs/0001-core.md:101-119
  docs/rfcs/0003-messages.md:31-42、:56-64、:217-224
  docs/rfcs/0006-resources-and-handover.md:51-75、:168-177、:185-192
  docs/rfcs/0011-discovery-and-domains.md:9-13、35、52-56
  docs/rfcs/0012-im-resource-node.md:176
  docs/rfcs/0015-mcp-resource-node.md:90-98

有界搜索只否证性质：absence 只在本文件的边界内成立。
"""

from __future__ import annotations

import json
import sys
from collections import deque
from dataclasses import dataclass, fields, replace

CLASSIFICATION = {
    "current": "guarded_invariant",
    "control": "counterfactual_probe",
    "invariant": "model_invariant",
    "ambiguity": "open_ambiguity",
    "policy": "selected_retry_policy",
    "boundary": "acknowledged_external_boundary",
}


def reach(start, edges):
    """有界 BFS；返回状态集合与最短轨迹。"""
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


def trace(parents, state):
    actions = []
    while parents[state][0] is not None:
        state, action = parents[state]
        actions.append(action)
    return list(reversed(actions))


def snapshot(state):
    """见证状态的字段快照；集合排序后输出，保证 JSON 稳定。"""
    out = {}
    for field in fields(state):
        value = getattr(state, field.name)
        out[field.name] = sorted(value) if isinstance(value, frozenset) else value
    return out


def result(name, kind, parents, hit, bounds, note, expected):
    witness = next((s for s in parents if hit(s)), None)
    return {
        "scenario": name,
        "kind": kind,
        "classification": CLASSIFICATION[kind],
        "bounds": bounds,
        "states_explored": len(parents),
        "witness_expected": expected,
        "check_pass": (witness is not None) == expected,
        "trace": trace(parents, witness) if witness is not None else [],
        "witness_state": snapshot(witness) if witness is not None else None,
        "final": {"overlap": witness is not None},
        "note": note,
    }


# --- 1. 接纳：重复到达只产生一份条目 ------------------------------------------


@dataclass(frozen=True)
class Admission:
    admitted: bool = False
    entries: int = 0
    seq_used: int = 0
    attempts: int = 0


def admission_edges(guard):
    def edges(s):
        if s.attempts >= 2:
            return
        if not s.admitted:
            yield "arrive", replace(s, admitted=True, entries=1, seq_used=1, attempts=1)
        else:
            yield "arrive_again", replace(
                s,
                entries=s.entries + (0 if guard else 1),
                seq_used=s.seq_used + (0 if guard else 1),
                attempts=s.attempts + 1,
            )

    return edges


def admission():
    bounds = {"arrivals": 2, "message_refs": 1, "frontier_exhausted": True}
    return [
        result(
            "admission_duplicate_arrival", "current",
            reach(Admission(), admission_edges(True)),
            lambda s: s.entries > 1 or s.seq_used > 1,
            bounds,
            "同一 MessageRef 重复到达只产生一条条目和一个 seq；去重记录保留到 retry_until"
            "（0001:101-119、0003:217-224）。",
            False,
        ),
        result(
            "admission_duplicate_arrival_probe", "control",
            reach(Admission(), admission_edges(False)),
            lambda s: s.entries > 1 or s.seq_used > 1,
            bounds,
            "反事实：去掉 MessageRef 去重后，第二次到达又分配了一个序号。",
            True,
        ),
    ]


# --- 2. 关注：两个理由同时命中只产生一条义务 ----------------------------------


@dataclass(frozen=True)
class Obligation:
    mention: bool = False
    reply: bool = False
    obligations: int = 0
    identity: str = "(thr-1, msg-1, recipient)"


def obligation_edges(guard):
    def edges(s):
        for reason, field in (("mention_hit", "mention"), ("reply_hit", "reply")):
            if getattr(s, field):
                continue
            yield reason, replace(
                s,
                **{field: True},
                obligations=s.obligations + (0 if guard and s.obligations else 1),
            )

    return edges


def obligation():
    bounds = {
        "reasons": 2,
        "messages": 1,
        "recipients": 1,
        "obligation_identity": "(thread_ref, message_ref, recipient)",
        "frontier_exhausted": True,
    }
    return [
        result(
            "attention_reason_overlap", "current",
            reach(Obligation(), obligation_edges(True)),
            lambda s: s.obligations > 1,
            bounds,
            "同一消息因 mention 与 reply 同时命中时只提交一条义务；两种命中顺序都覆盖，"
            "标识相同即同一份义务（0003:58、:64）。",
            False,
        ),
        result(
            "attention_reason_overlap_probe", "control",
            reach(Obligation(), obligation_edges(False)),
            lambda s: s.obligations > 1,
            bounds,
            "反事实：去掉按义务标识合并后，mention 与 reply 各产生一条义务。",
            True,
        ),
    ]


# --- 3. 事件终态：积压是集合，终态标签的优先级未定 ----------------------------


EVENT = "ev-1"


@dataclass(frozen=True)
class Backlog:
    pending: frozenset = frozenset({EVENT})
    ack_evidence: bool = False
    terminals: frozenset = frozenset()
    effective_removals: int = 0


def backlog_edges():
    def edges(s):
        removed = 1 if EVENT in s.pending else 0
        if not s.ack_evidence:
            yield "ack", replace(
                s,
                pending=s.pending - {EVENT},
                ack_evidence=True,
                effective_removals=s.effective_removals + removed,
            )
        if "expired" not in s.terminals:
            yield "retention_expire", replace(
                s,
                pending=s.pending - {EVENT},
                terminals=s.terminals | {"expired"},
                effective_removals=s.effective_removals + removed,
            )

    return edges


def terminal():
    bounds = {
        "events": 1,
        "ack_attempts": 1,
        "expiry_writes": 1,
        "obligation_identity": "(resource_ref, event_id)",
        "frontier_exhausted": True,
    }
    parents = reach(Backlog(), backlog_edges())
    return [
        result(
            "event_terminal_backlog_removal", "invariant", parents,
            lambda s: s.effective_removals > 1,
            bounds,
            "积压是待处理义务的集合：ack 与 retention_expire 移除同一事件，第二次移除不减少任何条目"
            "（0006:185、:190、0012:176）。这是集合语义的模型事实，不是规范对终态唯一性的声明。",
            False,
        ),
        result(
            "event_terminal_label_precedence", "ambiguity", parents,
            lambda s: s.ack_evidence and "expired" in s.terminals,
            bounds,
            "同一事件可以在 ack 之后到达保留期，ack 事实与 expired 终态并存。0003:64 只规定 ack 按"
            "义务标识幂等，0006:188-192 只规定保留期到达写 expired 并清积压，没有任何一条规定两者"
            "同时命中时哪个终态优先。此处报告未定歧义，不报告缺陷。",
            True,
        ),
    ]


# --- 4. 交接提交：decide 与 recover 只提交一次 --------------------------------


@dataclass(frozen=True)
class Prepare:
    accepted: bool = False
    commits: int = 0
    revision: int = 0
    decide_done: bool = False
    recover_done: bool = False


def prepare_edges(guard):
    def edges(s):
        if not s.accepted:
            yield "target_accepts", replace(s, accepted=True)
            return
        for path, flag in (("decide_commit", "decide_done"), ("recover_commit", "recover_done")):
            if getattr(s, flag):
                continue
            advance = 0 if guard and s.commits else 1
            yield path, replace(
                s,
                commits=s.commits + advance,
                revision=s.revision + advance,
                **{flag: True},
            )

    return edges


def prepare():
    bounds = {
        "prepares": 1,
        "commit_paths": ["decide", "recover"],
        "attempts_per_path": 1,
        "frontier_exhausted": True,
    }
    return [
        result(
            "handover_commit_overlap", "current",
            reach(Prepare(), prepare_edges(True)),
            lambda s: s.commits > 1 or s.revision > 1,
            bounds,
            "已接受的准备只提交一次：decide 与 recover 任一顺序共用同一提交点，恢复不重推 revision"
            "（0006:168-177）。",
            False,
        ),
        result(
            "handover_commit_overlap_probe", "control",
            reach(Prepare(), prepare_edges(False)),
            lambda s: s.commits > 1 or s.revision > 1,
            bounds,
            "反事实：去掉单一提交点后，两条路径各提交一次并推进两个 revision。",
            True,
        ),
    ]


# --- 5. 资源操作：记录已落盘时的重投，与记录未落盘的崩溃 ----------------------


@dataclass(frozen=True)
class Operation:
    record: bool = False
    effects: int = 0
    attempts: int = 0


def operation_edges(guard):
    def edges(s):
        if s.attempts >= 3:
            return
        if not s.record:
            yield "claim_and_execute", replace(s, record=True, effects=1, attempts=1)
            return
        for path in ("retry_same_id", "resume_same_id"):
            yield path, replace(
                s,
                effects=s.effects + (0 if guard else 1),
                attempts=s.attempts + 1,
            )

    return edges


def operation():
    bounds = {"operation_ids": 1, "paths": 3, "attempts": 3, "frontier_exhausted": True}
    return [
        result(
            "operation_effect_overlap", "current",
            reach(Operation(), operation_edges(True)),
            lambda s: s.effects > 1,
            bounds,
            "去重记录已落盘的同一 operation_id，重投与查回都不再产生外部效果"
            "（0001:101-119、0006:56-75）。只覆盖记录已提交的一段；效果先于记录落盘的一段"
            "见下一场景。",
            False,
        ),
        result(
            "operation_effect_overlap_probe", "control",
            reach(Operation(), operation_edges(False)),
            lambda s: s.effects > 1,
            bounds,
            "反事实：去掉 operation_id 查重后，重投与 resume 各执行一次。",
            True,
        ),
    ]


@dataclass(frozen=True)
class CrashOperation:
    effects: int = 0
    record: bool = False
    outcome: str = "none"
    attempts: int = 0


def crash_edges(guard):
    def edges(s):
        if s.attempts >= 3:
            return
        if s.outcome == "none":
            yield "execute_and_record", replace(
                s, effects=s.effects + 1, record=True, outcome="committed", attempts=1)
            yield "execute_then_crash_before_record", replace(
                s, effects=s.effects + 1, record=False, outcome="unknown", attempts=1)
            return
        if s.outcome != "unknown":
            return
        yield "query_by_operation_id", replace(s, attempts=s.attempts + 1)
        yield "retry_same_id", replace(
            s,
            effects=s.effects + (0 if guard else 1),
            record=s.record or not guard,
            outcome=s.outcome if guard else "committed",
            attempts=s.attempts + 1,
        )

    return edges


def operation_crash():
    bounds = {
        "operation_ids": 1,
        "attempts": 3,
        "effects_behind_one_committed_record": 1,
        "frontier_exhausted": True,
    }
    return [
        result(
            "operation_crash_before_record", "policy",
            reach(CrashOperation(), crash_edges(True)),
            lambda s: s.outcome == "committed" and s.effects > 1,
            bounds,
            "选定不重执行的策略：效果后、结果落盘前崩溃时保留 unknown。0015:92-98 允许本地策略"
            "决定是否重发；本行只检查不重发策略，不能据此断言 RFC 排除了第二次外部效果。",
            False,
        ),
        result(
            "operation_crash_before_record_retry", "boundary",
            reach(CrashOperation(), crash_edges(False)),
            lambda s: s.outcome == "committed" and s.effects > 1,
            bounds,
            "选择重执行策略时，崩溃后的同一调用可产生第二次效果。0015:92-98 明确认可该环境边界；"
            "它不是 operation_id 本地去重失效的证明。",
            True,
        ),
    ]


# --- 6. 域归组：一个节点最多一条 active 记录 ----------------------------------

ACTIVE = "active"
LEFT = "left"
NONE = "none"


@dataclass(frozen=True)
class Grouping:
    a: str = NONE
    b: str = NONE
    revision: int = 0
    conflicts: int = 0
    mutations: int = 0


def grouping_edges(guard):
    def edges(s):
        if s.mutations >= 2:
            return
        for field in ("a", "b"):
            if getattr(s, field) == ACTIVE:
                continue
            other = s.b if field == "a" else s.a
            for verb in ("domains.create", "domains.join"):
                if guard and other == ACTIVE:
                    yield f"{verb}_{field}:conflict", replace(
                        s, conflicts=s.conflicts + 1, mutations=s.mutations + 1)
                    continue
                yield f"{verb}_{field}", replace(
                    s,
                    **{field: ACTIVE},
                    revision=s.revision + 1,
                    mutations=s.mutations + 1,
                )

    return edges


def grouping():
    bounds = {
        "domains": 2,
        "mutations_per_node": 2,
        "max_active_records": 1,
        "frontier_exhausted": True,
    }
    return [
        result(
            "domain_grouping_overlap", "current",
            reach(Grouping(), grouping_edges(True)),
            lambda s: sum(1 for value in (s.a, s.b) if value == ACTIVE) > 1,
            bounds,
            "一个节点最多一条 active 记录：并发建组与加入不同域时后到的那条返回 conflict，不写第二条"
            " active 记录（0011:9、:35、:52）。",
            False,
        ),
        result(
            "domain_grouping_overlap_probe", "control",
            reach(Grouping(), grouping_edges(False)),
            lambda s: sum(1 for value in (s.a, s.b) if value == ACTIVE) > 1,
            bounds,
            "反事实：去掉单 active 约束后，节点同时声明两个 active 域。",
            True,
        ),
    ]


# --- 7. 域关系：退出后的迟到加入与历史重投 ------------------------------------


@dataclass(frozen=True)
class Membership:
    state: str = NONE
    revision: int = 0
    left_once: bool = False
    revived_by: str = ""
    carried_revision: int = 0
    stale_attempts: int = 0
    mutations: int = 0


def membership_edges(guard):
    def edges(s):
        if s.mutations >= 3:
            return
        if s.state == NONE:
            yield "domains.join", replace(
                s, state=ACTIVE, revision=1, mutations=s.mutations + 1)
            return
        if s.state == ACTIVE:
            yield "domains.leave", replace(
                s, state=LEFT, revision=s.revision + 1, left_once=True,
                mutations=s.mutations + 1)
            return
        if s.stale_attempts < 1:
            if guard:
                yield "domains.join_stale", replace(
                    s,
                    carried_revision=1,
                    stale_attempts=1,
                    mutations=s.mutations + 1,
                )
            else:
                yield "domains.join_stale", replace(
                    s,
                    state=ACTIVE,
                    revision=s.revision + 1,
                    revived_by="stale",
                    carried_revision=1,
                    stale_attempts=1,
                    mutations=s.mutations + 1,
                )
        yield "domains.join_fresh", replace(
            s,
            state=ACTIVE,
            revision=s.revision + 1,
            revived_by="fresh",
            mutations=s.mutations + 1,
        )

    return edges


def membership():
    bounds = {
        "domains": 1,
        "mutations": 3,
        "stale_joins_per_relation": 1,
        "frontier_exhausted": True,
    }
    return [
        result(
            "domain_stale_join_after_leave", "current",
            reach(Membership(), membership_edges(True)),
            lambda s: s.revived_by == "stale",
            bounds,
            "退出保留 left 记录与 revision；携带退出前 revision 的迟到加入被拒，墓碑不被覆盖，关系"
            "仍为 left（0011:35、:54）。用新幂等键的合法重新加入记为 fresh，不算重合。",
            False,
        ),
        result(
            "domain_stale_join_after_leave_probe", "control",
            reach(Membership(), membership_edges(False)),
            lambda s: s.revived_by == "stale",
            bounds,
            "反事实：去掉版本比较后，退出前的加入请求把关系改回 active。",
            True,
        ),
    ]


@dataclass(frozen=True)
class Replay:
    state: str = NONE
    revision: int = 0
    replays: int = 0
    reactivated: bool = False


def replay_edges(guard):
    def edges(s):
        if s.state == NONE:
            yield "domains.join", replace(s, state=ACTIVE, revision=1)
            return
        if s.state == ACTIVE:
            yield "domains.leave", replace(s, state=LEFT, revision=s.revision + 1)
            return
        if s.replays >= 2:
            return
        if guard:
            yield "domains.replay_join", replace(s, replays=s.replays + 1)
        else:
            yield "domains.replay_join", replace(
                s,
                state=ACTIVE,
                revision=s.revision + 1,
                replays=s.replays + 1,
                reactivated=True,
            )

    return edges


def replay():
    bounds = {"idempotency_keys": 1, "replays": 2, "frontier_exhausted": True}
    return [
        result(
            "domain_replay_no_reactivation", "current",
            reach(Replay(), replay_edges(True)),
            lambda s: s.reactivated,
            bounds,
            "退出之后重投原加入请求返回历史成功结果，当前成员状态不变，关系不复活"
            "（0011:52-54）。",
            False,
        ),
        result(
            "domain_replay_no_reactivation_probe", "control",
            reach(Replay(), replay_edges(False)),
            lambda s: s.reactivated,
            bounds,
            "反事实：把历史结果当作当前状态写回后，重投把关系改回 active。",
            True,
        ),
    ]


def run():
    results = []
    results.extend(admission())
    results.extend(obligation())
    results.extend(terminal())
    results.extend(prepare())
    results.extend(operation())
    results.extend(operation_crash())
    results.extend(grouping())
    results.extend(membership())
    results.extend(replay())
    return results


def main() -> int:
    results = run()
    ok = all(item["check_pass"] for item in results)
    print(json.dumps({
        "scenario": "effect_overlap",
        "results": results,
        "all_checks_pass": ok,
        "claim": (
            "bounded search; check_pass means each guarded result, counterfactual witness, "
            "ambiguity and retry-policy boundary matches its declared expectation"
        ),
    }, ensure_ascii=False, indent=2))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
