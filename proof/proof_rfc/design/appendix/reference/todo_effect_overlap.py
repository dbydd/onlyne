#!/usr/bin/env python3
"""todo_effect_overlap: 一条来信经一次崩溃和一次重放后，可见待办是否仍然只有一条。

固定一条 MessageRef（research/103），可见待办创建次数上限 2，至多一次崩溃、一次
重放。现行规则与候选修复共用同一套动作名，差别只在写入原子性和重放的判据。

现行规则按 0008 的书写顺序建模：
  - 0008:196 首版用 pi.appendEntry 写完整快照，再 setTodoPhases 更新宿主投影；
  - 0008:198-203 来源分两处：explanation 首段给人看，Onlyne 本地记录按消息引用建
    键，作重启恢复和去重的依据；
  - 0008:213 本地记录缺失时只报告未建立关联，不按标题猜测；
  - 0008:217 快照和本地来源记录都写入后，接入才记录关联成功；重启恢复按消息引用
    查本地记录，避免重复建项。
两次写入之间存在崩溃窗口：快照已落、本地记录未落。恢复按 0008:213 只能报告未建立
关联，重放按 0008:217 查不到记录，于是再追加一条。0008:291 要求“消息和 todo 的
恢复关联不会重复创建执行上下文或待办”，这个窗口直接违反它——缺口在程序顺序，不
在验收条目本身，候选修复必须补上原子提交，不能靠改写验收条目绕过。

把本地记录写在快照之前不是修复：同一窗口内重放查得到记录而跳过追加，待办丢失。
两个顺序各堵一个洞、开另一个洞。

候选修复：快照与稳定来源链接在一次可恢复提交里同时生效，派生索引允许落后；恢复
先从稳定提交重建链接，再决定是否追加；同一 MessageRef 至多一条待办。

标题和 explanation 不作关联键：本模型的身份只有消息引用，同标题的重建条目是另一
条条目，标题是否复用不改变任何迁移或判据。

依据：
  docs/rfcs/0008-omp-surface.md:192-219、:264-270、:291

有界搜索只否证性质：absence 只在本文件的边界内成立。
"""

from __future__ import annotations

import json
import sys
from collections import deque
from dataclasses import asdict, dataclass, replace

MAX_APPENDS = 2


@dataclass(frozen=True)
class State:
    """一条 MessageRef 在接入侧的可见状态。"""

    queued: bool = False              # 来信停在本地队列
    snapshot: bool = False            # 快照已写入分支
    stable_link: bool = False         # 按消息引用可查的稳定来源记录
    derived_index: bool = False       # 派生索引，允许落后于稳定记录
    appends: int = 0                  # 已创建的可见待办条数，上限 2
    reported_unassociated: bool = False
    crash_used: bool = False
    online: bool = True
    restarted: bool = False
    replay_used: bool = False


def reach(start, step):
    """有界 BFS；状态有限（布尔位加 appends ≤ 2），队列必然耗尽。"""
    parents = {start: (None, "")}
    queue = deque([start])
    while queue:
        state = queue.popleft()
        for action, nxt in step(state):
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


def deepest(parents):
    state = next(iter(parents))
    best = trace(parents, state)
    for candidate in parents:
        path = trace(parents, candidate)
        if len(path) > len(best):
            state, best = candidate, path
    return state, best


def edges(rule, replay_key="stable_link", crashes=True):
    """rule: current | candidate；replay_key: stable_link | derived_index | none。"""

    def step(s):
        if not s.queued:
            return [("queue_incoming", replace(s, queued=True))]

        out = []
        if rule == "current":
            # 0008:198-203 — 本地记录是一次独立写入。
            if s.online and not s.stable_link:
                out.append(("write_source_link", replace(s, stable_link=True)))
        else:
            # 候选 — 该动作只推进派生索引，稳定链接随快照提交一起落。
            if s.online and not s.derived_index:
                out.append(("write_source_link", replace(s, derived_index=True)))

        # 快照只能由在线的接入写；重放之后不再有第二次快照写入。
        if not s.snapshot and s.online and not s.replay_used:
            if rule == "current":
                out.append(
                    ("persist_snapshot",
                     replace(s, snapshot=True, appends=s.appends + 1))
                )
            else:
                out.append(
                    ("persist_snapshot",
                     replace(s, snapshot=True, stable_link=True,
                             appends=s.appends + 1))
                )

        if crashes and s.online and not s.crash_used:
            out.append(("crash", replace(s, online=False, crash_used=True)))

        if not s.online and not s.restarted:
            if rule == "current":
                if s.stable_link:
                    out.append(
                        ("recover_with_record",
                         replace(s, online=True, restarted=True))
                    )
                else:
                    # 0008:213 — 记录缺失只报未建立关联，不猜、不补写。
                    out.append(
                        ("recover_report_unassociated",
                         replace(s, online=True, restarted=True,
                                 reported_unassociated=True))
                    )
            else:
                # 先从稳定提交重建链接，再谈追加。
                out.append(
                    ("recover_rebuild_link",
                     replace(s, online=True, restarted=True,
                             derived_index=s.derived_index or s.stable_link))
                )

        if s.restarted and not s.replay_used and s.appends < MAX_APPENDS:
            skip = (
                (replay_key == "stable_link" and s.stable_link)
                or (replay_key == "derived_index" and s.derived_index)
            )
            out.append(
                ("replay_skips_append" if skip else "replay_appends_snapshot",
                 replace(s, replay_used=True,
                         appends=s.appends + (0 if skip else 1)))
            )
        return out

    return step


def duplicate(s):
    return s.appends >= 2


def lost(s):
    return s.replay_used and s.appends == 0


BOUNDS = {
    "message_refs": 1,
    "message_ref": "research/103",
    "crash_windows": 1,
    "replays": 1,
    "snapshot_writes": 1,
    "link_writes": 1,
    "visible_todo_creations": MAX_APPENDS,
    "identity_key": "message_ref",
    "frontier_exhausted": True,
}


def result(name, kind, parents, predicate, label, note):
    witness = next((s for s in parents if predicate(s)), None)
    expected = kind in {"current", "alternative", "control"}
    replayed = next((s for s in parents if s.replay_used), next(iter(parents)))
    deepest_state, longest = deepest(parents)
    return {
        "scenario": name,
        "kind": kind,
        "classification": {
            "current": "procedural_gap_against_0008_291",
            "alternative": "source_first_repair_gap",
            "candidate": "candidate_holds",
            "control": "detector_probe",
        }[kind],
        "bounds": dict(BOUNDS),
        "states_explored": len(parents),
        "witness_expected": expected,
        "check_pass": (witness is not None) == expected,
        "witness": label if witness is not None else None,
        "trace": trace(parents, witness) if witness is not None else [],
        "longest_trace": longest,
        "final": {
            "witness_state": asdict(witness) if witness is not None else None,
            "state_after_replay": asdict(replayed) if replayed.replay_used else None,
            "deepest_state": asdict(deepest_state),
            "max_appends_overall": max(s.appends for s in parents),
            "replay_states": sum(1 for s in parents if s.replay_used),
        },
        "note": note,
    }


def run():
    current = reach(State(), edges("current"))
    candidate = reach(State(), edges("candidate"))
    return [
        result(
            "todo_duplicate_current_crash_window", "current", current,
            duplicate, "duplicate_todo",
            "快照已落、本地记录未落时崩溃；恢复只报未建立关联，重放查不到记录再追加，"
            "同一 MessageRef 出现两条待办，违反 0008:291。",
        ),
        result(
            "todo_loss_source_first_alternative", "alternative", current,
            lost, "lost_todo",
            "反向排序的替代方案：先写本地记录再写快照，崩溃后重放查得到记录而跳过追加，待办丢失。"
            "此行不声称 RFC 强制了该排序，说明仅调换两次写入不能补足恢复契约。",
        ),
        result(
            "todo_duplicate_candidate", "candidate", candidate,
            duplicate, "duplicate_todo",
            "快照与稳定来源链接同一次可恢复提交；恢复重建链接后再决定追加，"
            "同一 MessageRef 至多一条待办。",
        ),
        result(
            "todo_loss_candidate_source_first", "candidate", candidate,
            lost, "lost_todo",
            "先写派生索引再崩溃不会丢项：重放只认稳定链接，索引落后不作跳过依据。",
        ),
        result(
            "todo_duplicate_control_no_dedupe", "control",
            reach(State(), edges("candidate", replay_key="none")),
            duplicate, "duplicate_todo",
            "去掉重放前的去重判据，候选规则同样追加第二条；探针证明检测器不是空转。",
        ),
        result(
            "todo_loss_control_index_keyed", "control",
            reach(State(), edges("candidate", replay_key="derived_index")),
            lost, "lost_todo",
            "把落后的派生索引当去重键，崩溃前写下的索引让重放跳过追加；"
            "这条探针证明修复的判据必须是稳定链接。",
        ),
    ]


def main() -> int:
    results = run()
    ok = all(item["check_pass"] for item in results)
    print(json.dumps({
        "scenario": "todo_effect_overlap",
        "results": results,
        "all_checks_pass": ok,
        "claim": "有界搜索：通过表示现行过程缺口和反事实探针复现，候选提交方案在这些边界内未见重复或丢项",
    }, ensure_ascii=False, indent=2))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
