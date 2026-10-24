"""出站消息判定样本（send 点）。

每个 case 的 `message` 只有 delivery_kind / msg / meta 三段；宿主能力放在
state.host，来源材料按 user / assistant / tool / note / todo / topology 分开。
provenance 不进入请求，只说明场景是构造还是改写自哪段工作记录。

16 个 dev、8 个 holdout。holdout 在本轮不运行，也不据此改模板。
"""
from __future__ import annotations

ME_NODE = "urn:onlyne:agent:lazuli"

ME = {
    "node": ME_NODE,
    "display": "lazuli",
    "role": "research-lead",
    "cluster": "home",
    "domain": "urn:onlyne:domain:research",
}

HOST = {
    "agent": ME_NODE,
    "cluster": "home",
    "supports_await": True,
    "is_idle": True,
}

PEERS = {
    "dpdp": {
        "alias": "dpdp", "node": "urn:onlyne:agent:dpdp", "display": "dpdp",
        "role": "research-coordinator", "cluster": "home",
        "relation": "thread_member", "can_post": True, "can_read": True,
    },
    "caerulea": {
        "alias": "caerulea", "node": "urn:onlyne:agent:caerulea", "display": "caerulea",
        "role": "cluster-ops", "cluster": "home",
        "relation": "thread_member", "can_post": True, "can_read": True,
    },
    "alexandria": {
        "alias": "alexandria", "node": "urn:onlyne:agent:alexandria", "display": "alexandria",
        "role": "docs-librarian", "cluster": "research_chamber",
        "relation": "domain_member", "can_post": True, "can_read": True,
        "address": "ice-lab.local:7420",
    },
    "reviewer-a": {
        "alias": "reviewer-a", "node": "urn:onlyne:agent:reviewer-a", "display": "reviewer",
        "role": "reviewer", "cluster": "home",
        "relation": "thread_member", "can_post": True, "can_read": True,
    },
    "reviewer-b": {
        "alias": "reviewer-b", "node": "urn:onlyne:agent:reviewer-b", "display": "reviewer",
        "role": "reviewer", "cluster": "research_chamber",
        "relation": "thread_member", "can_post": True, "can_read": True,
        "address": "ice-lab.local:7420",
    },
    "reviewer-local": {
        "alias": "reviewer-local", "node": "urn:onlyne:agent:reviewer-local", "display": "reviewer",
        "role": "release-manager", "cluster": "home",
        "relation": "domain_member", "can_post": True, "can_read": True,
    },
    "reviewer-chamber": {
        "alias": "reviewer-chamber", "node": "urn:onlyne:agent:reviewer-chamber", "display": "reviewer",
        "role": "reviewer", "cluster": "research_chamber",
        "relation": "domain_member", "can_post": True, "can_read": True,
        "address": "ice-lab.local:7420",
    },
    "tern-host": {
        "alias": "tern-host", "node": "urn:onlyne:agent:tern-host", "display": "tern-host",
        "role": "terminal-host", "cluster": "home",
        "relation": "domain_member", "can_post": True, "can_read": True,
    },
}

RESEARCH = {
    "alias": "research", "owner": ME_NODE, "state": "open",
    "my_role": "owner", "my_access": ["post", "read"],
    "members": ["lazuli", "dpdp", "caerulea"], "next_seq": "107",
    "purpose": "v3 RFC 与 proof 结果",
}
REVIEW = {
    "alias": "review", "owner": "urn:onlyne:agent:reviewer-a", "state": "open",
    "my_role": "member", "my_access": ["post", "read"],
    "members": ["reviewer-a", "reviewer-b", "lazuli", "alexandria"], "next_seq": "231",
    "purpose": "RFC review comments",
}
DOCS = {
    "alias": "docs", "owner": "urn:onlyne:agent:alexandria", "state": "open",
    "my_role": "member", "my_access": ["post", "read"],
    "members": ["alexandria", "lazuli"], "next_seq": "47",
    "purpose": "reference documents",
}
OPS = {
    "alias": "ops", "owner": "urn:onlyne:agent:caerulea", "state": "closed",
    "my_role": "member", "my_access": ["post", "read"],
    "members": ["caerulea", "lazuli"], "next_seq": "44",
    "purpose": "cluster maintenance",
}

C_ALEX_DOCS = {
    "alias": "c-3", "thread": "docs",
    "root": {"sender": ME_NODE, "id": "msg-41"},
    "task": "把 A2A 参考收进 docs", "assignee": "urn:onlyne:agent:alexandria",
    "state": "running", "started": "2026-10-24T09:05:00+08:00",
}
C_CAERULEA_RECALC = {
    "alias": "c-1", "thread": "research",
    "root": {"sender": ME_NODE, "id": "msg-31"},
    "task": "重算第二组数据", "assignee": "urn:onlyne:agent:caerulea",
    "state": "running", "started": "2026-10-24T08:52:00+08:00",
}
C_DPDP_REVIEW = {
    "alias": "c-2", "thread": "research",
    "root": {"sender": ME_NODE, "id": "msg-18"},
    "task": "复核第 3 节实验结论", "assignee": "urn:onlyne:agent:dpdp",
    "state": "running", "started": "2026-10-23T16:20:00+08:00",
}
C_REVIEW_B = {
    "alias": "c-4", "thread": "review",
    "root": {"sender": ME_NODE, "id": "msg-88"},
    "task": "批注 0003 消息格式", "assignee": "urn:onlyne:agent:reviewer-b",
    "state": "running", "started": "2026-10-24T10:10:00+08:00",
}
C_CAERULEA_THIRD = {
    "alias": "c-5", "thread": "research",
    "root": {"sender": ME_NODE, "id": "msg-52"},
    "task": "重算第三组数据", "assignee": "urn:onlyne:agent:caerulea",
    "state": "running", "started": "2026-10-24T11:02:00+08:00",
}
C_OLD_PLOT = {
    "alias": "c-9", "thread": "research",
    "root": {"sender": ME_NODE, "id": "msg-12"},
    "task": "出第二组对比图", "assignee": "urn:onlyne:agent:caerulea",
    "state": "completed", "started": "2026-10-22T09:00:00+08:00",
}


def msg(delivery_kind: str, body: str, meta: dict | None = None) -> dict:
    return {"delivery_kind": delivery_kind, "msg": body, "meta": dict(meta or {})}


def peers(*aliases: str) -> list[dict]:
    return [dict(PEERS[alias], ) for alias in aliases]


def receipt(message_id: str, to: str, thread: str | None, seq: str, *,
            context: str | None = None, response: str = "async",
            execution: str = "new", state: str = "accepted") -> dict:
    return {
        "tool": "msg",
        "at": "2026-10-24T09:03:00+08:00",
        "result": {
            "state": state,
            "message": {"sender": ME_NODE, "id": message_id},
            "to": to,
            "thread": thread,
            "seq": seq,
            "plan": {"response": response, "execution": execution},
            "context": context,
        },
    }


def turn(role: str, at: str, text: str) -> dict:
    return {"role": role, "at": at, "text": text}


def send_plan(to: str, thread: str | None, response: str, execution: str,
              context: str | None = None) -> tuple[str, str]:
    if execution == "existing" and not context:
        raise ValueError("existing plan needs a context alias")
    key = f"send_{to}_{thread or 'direct'}_{response}_{execution}"
    if context:
        key += f"_{context}"
    place = f"in thread {thread}" if thread else "as a direct message"
    wait = {
        "none": "the sender does not expect a reply",
        "async": "the sender keeps working and reads any reply later",
        "await": "the sender stops and waits for the reply",
    }[response]
    where = (
        f"the receiver reuses execution context {context}"
        if execution == "existing"
        else "the receiver starts a fresh execution context"
    )
    return key, f"Send to {to} {place}; {wait}; {where}."


def read_plan(key: str, description: str) -> tuple[str, str]:
    return f"read_{key}", description


def clarify_plan() -> tuple[str, str]:
    return "clarify", "Ask the user for the missing target or intent; send nothing yet."


def peer_option(alias: str) -> tuple[str, str]:
    peer = PEERS[alias]
    return alias, (
        f"display {peer['display']}; role {peer['role']}; "
        f"cluster {peer['cluster']}; relation {peer['relation']}"
    )


def profiles(message: dict, peer_aliases: list[str], threads: list[dict],
             contexts: list[dict] | None = None, *, focused: dict | None = None,
             expanded=None) -> dict:
    minimal = {
        "decision": "send_outbound",
        "me": ME,
        "message": message,
        "peers": peers(*peer_aliases),
        "threads": list(threads),
        "contexts": list(contexts or []),
        "host": HOST,
    }
    focused_state = {**minimal, **(focused or {})}
    expanded_state = expanded(focused_state) if callable(expanded) else {**focused_state, **(expanded or {})}
    return {"minimal": minimal, "focused": focused_state, "expanded": expanded_state}


def case(case_id: str, split: str, provenance: str, message: dict,
         peer_aliases: list[str], threads: list[dict], contexts: list[dict],
         ask: str, options: list[tuple[str, str]], expected: dict,
         *, focused: dict | None = None, expanded=None) -> dict:
    return {
        "id": case_id,
        "point": "send",
        "split": split,
        "provenance": provenance,
        "profiles": profiles(message, peer_aliases, threads, contexts,
                             focused=focused, expanded=expanded),
        "ask": ask,
        "options": options,
        "expected": expected,
    }


CASES = [

    # ---------------------------------------------------------------- dev
    case(
        "send-dev-01", "dev",
        "构造场景：用户 2026-10-24 在 docs 收参考文档的对话里追加 ANP；"
        "peer/thread/context 字段按 RFC 0001、0003 的实际结构填写，非流量抓取。",
        msg("把 ANP 参考也发给刚才收 A2A 文档的人",
            "ANP 的参考链接和本地副本也补上。"),
        ["alexandria", "caerulea", "dpdp"], [DOCS, RESEARCH], [C_ALEX_DOCS],
        "plan",
        [
            send_plan("alexandria", "docs", "none", "existing", "c-3"),
            send_plan("alexandria", "docs", "async", "existing", "c-3"),
            send_plan("caerulea", "research", "none", "new"),
            clarify_plan(),
        ],
        {"plan": "send_alexandria_docs_none_existing_c-3"},
        focused={
            "user_turns": [
                turn("user", "2026-10-24T09:00:00+08:00",
                     "让 alexandria 把 A2A 的参考文档收进 docs，顺便补个来源说明。"),
                turn("user", "2026-10-24T09:22:00+08:00",
                     "ANP 那篇也给他吧，他昨天问过。"),
            ],
            "assistant_turns": [
                turn("assistant", "2026-10-24T09:03:00+08:00",
                     "已派给 alexandria：docs thread seq 41，落在上下文 c-3。"),
            ],
            "tool_results": [receipt("msg-41", "alexandria", "docs", "41",
                                     context="c-3", response="none", execution="new")],
            "outbound": [receipt("msg-41", "alexandria", "docs", "41",
                                 context="c-3", response="none", execution="new")["result"]],
            "notes": [{
                "kind": "context_note",
                "custom_type": "experimental_context_notes",
                "text": "当前任务：把 A2A 与 ANP 参考收进 docs，对象 alexandria。",
                "updated": "2026-10-24T09:10:00+08:00",
                "covers": "active branch through msg 40",
                "reset_boundary": "none since note",
            }],
            "todo": {
                "source": "omp-todo-more:read (public event)",
                "goal": "补齐 v3 协议参考资料",
                "open_items": [{"title": "ANP 参考补充到 docs", "source": "msg-41"}],
            },
        },
        expanded=lambda s: {
            **s,
            "user_turns": s["user_turns"] + [
                turn("user", "2026-10-23T17:40:00+08:00",
                     "dpdp 说明天实验环境能好，到时候再跑那组。"),
            ],
            "peers": s["peers"] + peers("reviewer-local", "tern-host"),
            "threads": s["threads"] + [OPS],
            "contexts": s["contexts"] + [C_DPDP_REVIEW],
            "outbound": s["outbound"] + [{
                "message": {"sender": ME_NODE, "id": "msg-95"},
                "to": "dpdp", "thread": "research", "seq": "95",
                "plan": {"response": "async", "execution": "new"},
                "context": "c-2", "state": "accepted",
            }],
        },
    ),
    case(
        "send-dev-02", "dev",
        "构造场景：proof 报告作为第二组重算任务的补充发出；上下文来自回执而不是 meta。"
        "表达取自本机 RFC/proof 工作流，非生产抓包。",
        msg("把 proof 报告也发给刚接手重算的人",
            "proof/systemone_context/evidence 里那份 dev 报告也给你。"),
        ["caerulea", "dpdp", "alexandria"], [RESEARCH, DOCS], [C_CAERULEA_RECALC],
        "plan",
        [
            send_plan("caerulea", "research", "none", "existing", "c-1"),
            send_plan("caerulea", "research", "async", "existing", "c-1"),
            send_plan("dpdp", "research", "none", "new"),
            clarify_plan(),
        ],
        {"plan": "send_caerulea_research_none_existing_c-1"},
        focused={
            "user_turns": [
                turn("user", "2026-10-24T09:30:00+08:00",
                     "这份 proof 报告也给他。"),
            ],
            "assistant_turns": [
                turn("assistant", "2026-10-24T08:53:00+08:00",
                     "第二组重算已派给 caerulea：research seq 103，上下文 c-1，状态 running。"),
            ],
            "tool_results": [receipt("msg-31", "caerulea", "research", "103",
                                     context="c-1", response="async", execution="new")],
            "outbound": [receipt("msg-31", "caerulea", "research", "103",
                                 context="c-1", response="async", execution="new")["result"]],
            "todo": {
                "source": "omp-todo-more:read (public event)",
                "goal": "第二组数据重算",
                "open_items": [{"title": "等 caerulea 回参数", "source": "msg-31"}],
            },
        },
        expanded=lambda s: {
            **s,
            "threads": s["threads"] + [OPS],
            "contexts": s["contexts"] + [C_DPDP_REVIEW],
            "outbound": s["outbound"] + [{
                "message": {"sender": ME_NODE, "id": "msg-95"},
                "to": "dpdp", "thread": "research", "seq": "95",
                "plan": {"response": "async", "execution": "new"},
                "context": "c-2", "state": "accepted",
            }],
            "notes": [{
                "kind": "context_note",
                "custom_type": "experimental_context_notes",
                "text": "旧记录：proof 报告先发 dpdp 存档。",
                "updated": "2026-10-23T18:00:00+08:00",
                "covers": "branch main through msg 30",
                "reset_boundary": "cleared 2026-10-24T08:00:00+08:00 by /clear",
            }],
        },
    ),
    case(
        "send-dev-03", "dev",
        "构造场景：重启窗口前给 caerulea 的单点通知；ops 线程已关闭，当前会话里没有新线程。"
        "拓扑字段按 RFC 0011，场景非抓取。",
        msg("通知对方重启先跳过 research 域",
            "重启前先跳过 research 域，等我确认。"),
        ["caerulea", "dpdp"], [RESEARCH, OPS], [],
        "plan",
        [
            send_plan("caerulea", None, "none", "new"),
            send_plan("caerulea", "research", "none", "new"),
            send_plan("dpdp", None, "none", "new"),
            clarify_plan(),
        ],
        {"plan": "send_caerulea_direct_none_new"},
        focused={
            "user_turns": [
                turn("user", "2026-10-24T13:10:00+08:00",
                     "跟 caerulea 说一声，集群重启先别动 lazuli 那个域。"),
            ],
            "assistant_turns": [
                turn("assistant", "2026-10-24T13:09:00+08:00",
                     "重启计划里 research 域会先停，caerulea 在等确认。"),
            ],
            "tool_results": [{
                "tool": "read", "at": "2026-10-24T13:08:00+08:00",
                "result": {"scope": "threads", "items": [
                    {"alias": "research", "state": "open"},
                    {"alias": "ops", "state": "closed"},
                ]},
            }],
            "notes": [{
                "kind": "context_note",
                "custom_type": "experimental_context_notes",
                "text": "重启窗口 22:00；research 域最后停。",
                "updated": "2026-10-24T12:50:00+08:00",
                "covers": "active branch through msg 60",
                "reset_boundary": "none since note",
            }],
        },
        expanded=lambda s: {
            **s,
            "peers": s["peers"] + peers("reviewer-chamber"),
            "threads": s["threads"] + [DOCS],
            "outbound": [{
                "message": {"sender": ME_NODE, "id": "msg-58"},
                "to": "dpdp", "thread": "research", "seq": "58",
                "plan": {"response": "async", "execution": "new"},
                "state": "accepted",
            }],
        },
    ),
    case(
        "send-dev-04", "dev",
        "构造场景：assistant 上一轮提出让 dpdp 复核，用户只回“发吧”；目标只能从 assistant 计划得到。"
        "RFC 0005 任务派发表述，非抓取。",
        msg("请人复核 0005 的等待语义，重点 await 超时",
            "0005 §2 的 context_lost 处理请复核，重点看 await 超时。"),
        ["dpdp", "caerulea", "reviewer-chamber"], [RESEARCH, REVIEW], [],
        "plan",
        [
            send_plan("dpdp", "research", "async", "new"),
            send_plan("dpdp", "research", "await", "new"),
            send_plan("caerulea", "research", "async", "new"),
            clarify_plan(),
        ],
        {"plan": "send_dpdp_research_async_new"},
        focused={
            "user_turns": [
                turn("user", "2026-10-24T14:02:00+08:00", "行，发吧。"),
            ],
            "assistant_turns": [
                turn("assistant", "2026-10-24T14:01:00+08:00",
                     "我准备让 dpdp 复核这版 0005 的等待语义，草稿已经写在 msg 里。"),
            ],
            "tool_results": [{
                "tool": "read", "at": "2026-10-24T14:00:00+08:00",
                "result": {"scope": "message", "thread": "review", "seq": "229",
                           "summary": "0005 等待语义讨论"},
            }],
            "notes": [{
                "kind": "context_note",
                "custom_type": "experimental_context_notes",
                "text": "待复核：0005 §2 context_lost 与 await 超时。",
                "updated": "2026-10-24T13:58:00+08:00",
                "covers": "active branch through msg 58",
                "reset_boundary": "none since note",
            }],
        },
        expanded=lambda s: {
            **s,
            "peers": s["peers"] + peers("alexandria", "reviewer-local"),
            "threads": s["threads"] + [OPS],
            "outbound": [{
                "message": {"sender": ME_NODE, "id": "msg-50"},
                "to": "caerulea", "thread": "research", "seq": "50",
                "plan": {"response": "async", "execution": "new"},
                "state": "accepted",
            }],
        },
    ),
    case(
        "send-dev-05", "dev",
        "构造场景：reviewer 角色 2026-10-22 从本机节点转到 research_chamber；两个节点显示名相同。"
        "角色表按 RFC 0011 的域/职责视图重建，非抓取。",
        msg("请 reviewer 复核 0016 的来源权威表",
            "0016 §2 的来源权威表请复核，尤其 A2A 参考的时效。"),
        ["reviewer-chamber", "reviewer-local", "alexandria"], [REVIEW, RESEARCH], [],
        "plan",
        [
            send_plan("reviewer-chamber", None, "async", "new"),
            send_plan("reviewer-local", None, "async", "new"),
            send_plan("alexandria", "docs", "async", "new"),
            clarify_plan(),
        ],
        {"plan": "send_reviewer-chamber_direct_async_new"},
        focused={
            "user_turns": [
                turn("user", "2026-10-24T14:20:00+08:00",
                     "让 reviewer 看一下 0016 的上下文来源表，他熟 A2A。"),
            ],
            "assistant_turns": [
                turn("assistant", "2026-10-24T14:19:00+08:00",
                     "reviewer 角色 10-22 转到 research_chamber 了。"),
            ],
            "topology": {
                "role_assignments": [
                    {"role": "reviewer", "holder": "urn:onlyne:agent:reviewer-chamber",
                     "since": "2026-10-22T10:00:00+08:00"},
                    {"role": "release-manager", "holder": "urn:onlyne:agent:reviewer-local",
                     "since": "2026-10-22T10:00:00+08:00"},
                ],
            },
            "notes": [{
                "kind": "context_note",
                "custom_type": "experimental_context_notes",
                "text": "0016 来源表待复核；A2A 参考的时效是重点。",
                "updated": "2026-10-24T14:15:00+08:00",
                "covers": "active branch through msg 62",
                "reset_boundary": "none since note",
            }],
        },
        expanded=lambda s: {
            **s,
            "peers": s["peers"] + peers("reviewer-a", "dpdp"),
            "threads": s["threads"] + [DOCS],
            "thread_messages": [{
                "thread": "review", "seq": "214",
                "sender": "urn:onlyne:agent:reviewer-a",
                "at": "2026-10-20T11:00:00+08:00",
                "text": "0003 的批注我出过了。",
            }],
        },
    ),
    case(
        "send-dev-06", "dev",
        "构造场景：两个 reviewer 实例同名同职责，近期分别看过不同 RFC，当前消息没有指向证据。"
        "按 RFC 0016 §1 的澄清路径设计，非抓取。",
        msg("把这份 review 意见发给 reviewer",
            "这份 review 意见请收一下，前面几节先看。"),
        ["reviewer-a", "reviewer-b"], [REVIEW], [],
        "plan",
        [
            send_plan("reviewer-a", "review", "async", "new"),
            send_plan("reviewer-b", "review", "async", "new"),
            clarify_plan(),
        ],
        {"plan": "clarify"},
        focused={
            "user_turns": [
                turn("user", "2026-10-24T14:40:00+08:00",
                     "把这份意见给 reviewer 发过去。"),
            ],
            "assistant_turns": [
                turn("assistant", "2026-10-24T14:39:00+08:00",
                     "reviewer-a 昨天看过 0003，reviewer-b 今天上午看过 0005。"),
            ],
            "thread_messages": [
                {"thread": "review", "seq": "226", "sender": "urn:onlyne:agent:reviewer-a",
                 "at": "2026-10-23T16:00:00+08:00", "text": "0003 我看完了。"},
                {"thread": "review", "seq": "228", "sender": "urn:onlyne:agent:reviewer-b",
                 "at": "2026-10-24T09:40:00+08:00", "text": "0005 的意见在附件。"},
            ],
            "notes": [{
                "kind": "context_note",
                "custom_type": "experimental_context_notes",
                "text": "两份 review 意见：0003 与 0005，收件人都写作 reviewer。",
                "updated": "2026-10-24T09:45:00+08:00",
                "covers": "active branch through msg 64",
                "reset_boundary": "none since note",
            }],
        },
        expanded=lambda s: {
            **s,
            "peers": s["peers"] + peers("alexandria"),
            "contexts": s["contexts"] + [C_REVIEW_B],
            "outbound": [{
                "message": {"sender": ME_NODE, "id": "msg-88"},
                "to": "reviewer-b", "thread": "review", "seq": "88",
                "plan": {"response": "none", "execution": "new"},
                "context": "c-4", "state": "accepted",
            }],
        },
    ),
    case(
        "send-dev-07", "dev",
        "构造场景：review 线程里 reviewer-a 提过 0008 的三点意见，用户现在要求回给他；"
        "同显示名的 reviewer-b 也在同一线程。线程记录按 RFC 0002 结构重建。",
        msg("回复 reviewer，0008 已按三点意见修改",
            "0008 已按你提的三点修改，附件是 diff。",
            {"thread": "review"}),
        ["reviewer-a", "reviewer-b", "alexandria"], [REVIEW], [],
        "plan",
        [
            send_plan("reviewer-a", "review", "async", "new"),
            send_plan("reviewer-b", "review", "async", "new"),
            send_plan("reviewer-a", "review", "none", "new"),
            clarify_plan(),
        ],
        {"plan": "send_reviewer-a_review_async_new"},
        focused={
            "user_turns": [
                turn("user", "2026-10-24T15:00:00+08:00",
                     "改完了，给 reviewer 回一句，按他的三点意见都改了。"),
            ],
            "thread_messages": [
                {"thread": "review", "seq": "223", "sender": "urn:onlyne:agent:reviewer-a",
                 "at": "2026-10-23T15:10:00+08:00",
                 "text": "0008 三点：候选描述、回复权威、超时处置。"},
                {"thread": "review", "seq": "224", "sender": ME_NODE,
                 "at": "2026-10-23T15:20:00+08:00",
                 "text": "我改完再给你。"},
            ],
            "notes": [{
                "kind": "context_note",
                "custom_type": "experimental_context_notes",
                "text": "0008 三点意见已处理，待回 reviewer-a 复核。",
                "updated": "2026-10-23T15:22:00+08:00",
                "covers": "review thread through seq 224",
                "reset_boundary": "none since note",
            }],
        },
        expanded=lambda s: {
            **s,
            "peers": s["peers"] + peers("reviewer-chamber"),
            "threads": s["threads"] + [DOCS, OPS],
            "thread_messages": s["thread_messages"] + [
                {"thread": "review", "seq": "200", "sender": "urn:onlyne:agent:reviewer-b",
                 "at": "2026-10-18T10:00:00+08:00", "text": "0005 我可以看。"},
            ],
        },
    ),
    case(
        "send-dev-08", "dev",
        "改写自用户 2026-10-24 让 dpdp 有空看草稿、自己继续跑 proof 的指令；时间与回执字段为构造。",
        msg("让 dpdp 有空看看 0011 草稿，意见回头给我，我继续跑 proof",
            "0011 域发现草稿发你，回头把意见丢给我。",
            {"to": "dpdp"}),
        ["dpdp", "caerulea"], [RESEARCH], [],
        "response",
        [
            ("none", "No reply is expected; the message is informational."),
            ("async", "A reply is expected, but the sender keeps working and reads it later."),
            ("await", "A reply is expected and the sender stops until it arrives."),
        ],
        {"response": "async"},
        focused={
            "user_turns": [
                turn("user", "2026-10-24T15:20:00+08:00",
                     "把 0011 的草稿发给 dpdp，让他有空看看，我继续跑 proof。"),
            ],
            "assistant_turns": [
                turn("assistant", "2026-10-24T15:21:00+08:00",
                     "草稿目标 dpdp，意见回来后再合并。"),
            ],
        },
        expanded=lambda s: {
            **s,
            "threads": s["threads"] + [DOCS],
            "outbound": [{
                "message": {"sender": ME_NODE, "id": "msg-104"},
                "to": "dpdp", "thread": "research", "seq": "104",
                "plan": {"response": "await", "execution": "new"},
                "state": "accepted",
            }],
        },
    ),
    case(
        "send-dev-09", "dev",
        "改写自用户 2026-10-24 询问参数并在确认前不改代码的指令；回执字段为构造。",
        msg("问 caerulea 参数对不对，问清楚之前先不改代码",
            "第二组的 batch size 还是 64 吗？确认后我再改。",
            {"to": "caerulea"}),
        ["caerulea", "dpdp"], [RESEARCH], [],
        "response",
        [
            ("none", "No reply is expected; the message is informational."),
            ("async", "A reply is expected, but the sender keeps working and reads it later."),
            ("await", "A reply is expected and the sender stops until it arrives."),
        ],
        {"response": "await"},
        focused={
            "user_turns": [
                turn("user", "2026-10-24T15:35:00+08:00",
                     "发给 caerulea，问她这个参数对不对，问清楚之前先别改代码。"),
            ],
            "assistant_turns": [
                turn("assistant", "2026-10-24T15:36:00+08:00",
                     "参数确认前先停下，不再改第二组脚本。"),
            ],
        },
        expanded=lambda s: {
            **s,
            "threads": s["threads"] + [OPS],
            "outbound": [{
                "message": {"sender": ME_NODE, "id": "msg-106"},
                "to": "caerulea", "thread": "research", "seq": "106",
                "plan": {"response": "none", "execution": "new"},
                "state": "accepted",
            }],
        },
    ),
    case(
        "send-dev-10", "dev",
        "改写自用户 2026-10-24 的 docs 维护通知；场景为构造，非生产流量。",
        msg("通知 alexandria docs 维护，不需要回复",
            "docs 目录今晚 22:00 维护十分钟，不影响你手上的。",
            {"to": "alexandria"}),
        ["alexandria", "dpdp"], [DOCS], [],
        "response",
        [
            ("none", "No reply is expected; the message is informational."),
            ("async", "A reply is expected, but the sender keeps working and reads it later."),
            ("await", "A reply is expected and the sender stops until it arrives."),
        ],
        {"response": "none"},
        focused={
            "user_turns": [
                turn("user", "2026-10-24T15:50:00+08:00",
                     "通知 alexandria 一声，docs 目录今晚会维护，不用回。"),
            ],
            "assistant_turns": [
                turn("assistant", "2026-10-24T15:51:00+08:00",
                     "维护窗口不影响她正在收的参考文档。"),
            ],
        },
        expanded=lambda s: {
            **s,
            "threads": s["threads"] + [REVIEW],
            "outbound": [{
                "message": {"sender": ME_NODE, "id": "msg-108"},
                "to": "alexandria", "thread": "docs", "seq": "108",
                "plan": {"response": "async", "execution": "new"},
                "state": "accepted",
            }],
        },
    ),
    case(
        "send-dev-11", "dev",
        "构造场景：第二组重算任务已落在 c-1，用户追加日志范围；meta 没有 context 字段。"
        "回执与 todo 结构按 RFC 0005 与 todo-more 公开事件重建。",
        msg("补充 caerulea 那个重算任务：日志只看 3425 那段",
            "日志只看 3425 端口那段，其他先跳过。"),
        ["caerulea", "dpdp", "alexandria"], [RESEARCH, DOCS],
        [C_CAERULEA_RECALC, C_DPDP_REVIEW],
        "plan",
        [
            send_plan("caerulea", "research", "none", "existing", "c-1"),
            send_plan("caerulea", "research", "async", "existing", "c-1"),
            send_plan("caerulea", "research", "none", "new"),
            clarify_plan(),
        ],
        {"plan": "send_caerulea_research_none_existing_c-1"},
        focused={
            "user_turns": [
                turn("user", "2026-10-24T08:50:00+08:00",
                     "让 caerulea 重算第二组，重点看 3425 端口的日志。"),
                turn("user", "2026-10-24T09:10:00+08:00",
                     "再补一句，日志只看 3425 端口那段。"),
            ],
            "assistant_turns": [
                turn("assistant", "2026-10-24T08:53:00+08:00",
                     "第二组重算已派给 caerulea：research seq 103，上下文 c-1，还在跑。"),
            ],
            "tool_results": [receipt("msg-31", "caerulea", "research", "103",
                                     context="c-1", response="async", execution="new")],
            "outbound": [receipt("msg-31", "caerulea", "research", "103",
                                 context="c-1", response="async", execution="new")["result"]],
            "todo": {
                "source": "omp-todo-more:read (public event)",
                "goal": "第二组数据重算",
                "open_items": [{"title": "等 caerulea 回参数", "source": "msg-31"}],
            },
            "notes": [{
                "kind": "context_note",
                "custom_type": "experimental_context_notes",
                "text": "c-1：第二组重算未完成，3425 端口日志是输入。",
                "updated": "2026-10-24T09:05:00+08:00",
                "covers": "active branch through msg 44",
                "reset_boundary": "none since note",
            }],
        },
        expanded=lambda s: {
            **s,
            "threads": s["threads"] + [OPS],
            "contexts": s["contexts"] + [C_CAERULEA_THIRD],
            "outbound": s["outbound"] + [{
                "message": {"sender": ME_NODE, "id": "msg-95"},
                "to": "dpdp", "thread": "research", "seq": "95",
                "plan": {"response": "async", "execution": "new"},
                "context": "c-2", "state": "accepted",
            }],
        },
    ),
    case(
        "send-dev-12", "dev",
        "构造场景：用户明确不续 c-1，要求另开第三组任务；同一 peer 已有运行中的上下文作为对照。"
        "按 RFC 0005 §1 execution 语义设计。",
        msg("让 caerulea 另开任务重做第三组",
            "第三组数据按旧参数重做一遍，不要动 c-1 里的结果。",
            {"to": "caerulea"}),
        ["caerulea", "dpdp"], [RESEARCH], [C_CAERULEA_RECALC],
        "plan",
        [
            send_plan("caerulea", "research", "async", "new"),
            send_plan("caerulea", "research", "async", "existing", "c-1"),
            send_plan("caerulea", "research", "none", "new"),
            clarify_plan(),
        ],
        {"plan": "send_caerulea_research_async_new"},
        focused={
            "user_turns": [
                turn("user", "2026-10-24T16:05:00+08:00",
                     "别接着 c-1 了，让 caerulea 另开一个任务重做第三组。"),
            ],
            "assistant_turns": [
                turn("assistant", "2026-10-24T16:04:00+08:00",
                     "c-1 还在跑第二组，第三组没有单独任务。"),
            ],
            "tool_results": [receipt("msg-31", "caerulea", "research", "103",
                                     context="c-1", response="async", execution="new")],
            "outbound": [receipt("msg-31", "caerulea", "research", "103",
                                 context="c-1", response="async", execution="new")["result"]],
            "todo": {
                "source": "omp-todo-more:read (public event)",
                "goal": "第二组数据重算",
                "open_items": [{"title": "第二组结果", "source": "msg-31"}],
            },
        },
        expanded=lambda s: {
            **s,
            "threads": s["threads"] + [OPS],
            "contexts": s["contexts"] + [C_OLD_PLOT, C_DPDP_REVIEW],
            "notes": [{
                "kind": "context_note",
                "custom_type": "experimental_context_notes",
                "text": "旧安排：第三组并进 c-1 一起跑。",
                "updated": "2026-10-23T20:00:00+08:00",
                "covers": "branch main through msg 28",
                "reset_boundary": "cleared 2026-10-24T07:30:00+08:00 by /clear",
            }],
        },
    ),
    case(
        "send-dev-13", "dev",
        "构造场景：用户在 research 提过 0016 来源表问题，现在只想知道有没有人接话；"
        "读范围按 RFC 0005 §1 read.scope 设计，非抓取。",
        msg("查 research 里有没有人接我昨天的话", "",
            {"thread": "research"}),
        ["dpdp", "caerulea", "reviewer-chamber"], [RESEARCH, REVIEW], [],
        "plan",
        [
            read_plan("inbox_thread_research",
                      "Read local attention entries filtered to thread research; "
                      "this lists messages that mention or reply to this agent."),
            read_plan("messages_thread_research",
                      "Read the full stored history of thread research."),
            read_plan("evidence_msg-77",
                      "Read the stored delivery state of outbound message msg-77."),
            clarify_plan(),
        ],
        {"plan": "read_inbox_thread_research"},
        focused={
            "user_turns": [
                turn("user", "2026-10-24T16:20:00+08:00",
                     "看看 research 里有没有人接我昨天的话。"),
            ],
            "assistant_turns": [
                turn("assistant", "2026-10-24T16:19:00+08:00",
                     "昨天在 research 提了 0016 来源表的问题，等 dpdp 和 caerulea 回。"),
            ],
            "outbound": [{
                "message": {"sender": ME_NODE, "id": "msg-96"},
                "to": "research", "thread": "research", "seq": "96",
                "mentions": ["@all"], "state": "accepted",
            }],
            "notes": [{
                "kind": "context_note",
                "custom_type": "experimental_context_notes",
                "text": "待关注：research 里 0016 来源表讨论。",
                "updated": "2026-10-24T16:10:00+08:00",
                "covers": "active branch through msg 60",
                "reset_boundary": "none since note",
            }],
        },
        expanded=lambda s: {
            **s,
            "threads": s["threads"] + [DOCS],
            "outbound": s["outbound"] + [{
                "message": {"sender": ME_NODE, "id": "msg-77"},
                "to": "reviewer-a", "thread": "review", "seq": "77",
                "state": "unknown",
            }],
            "notes": s["notes"] + [{
                "kind": "context_note",
                "custom_type": "experimental_context_notes",
                "text": "旧记录：等 alexandria 回 docs 目录清单。",
                "updated": "2026-10-23T10:00:00+08:00",
                "covers": "branch docs through msg 20",
                "reset_boundary": "cleared 2026-10-24T07:30:00+08:00 by /clear",
            }],
        },
    ),
    case(
        "send-dev-14", "dev",
        "构造场景：0008 批注发出时传输返回未知，用户问是否送达；read.evidence 的范围按 RFC 0005。",
        msg("查昨天发给 reviewer 的 0008 批注是否送达", "",
            {"message": {"sender": ME_NODE, "id": "msg-77"}}),
        ["reviewer-a", "reviewer-b"], [REVIEW], [],
        "plan",
        [
            read_plan("evidence_msg-77",
                      "Read the stored delivery state of outbound message msg-77."),
            read_plan("messages_thread_review",
                      "Read the full stored history of thread review."),
            read_plan("inbox_thread_review",
                      "Read local attention entries filtered to thread review."),
            clarify_plan(),
        ],
        {"plan": "read_evidence_msg-77"},
        focused={
            "user_turns": [
                turn("user", "2026-10-24T16:40:00+08:00",
                     "我昨天发给 reviewer 的那条，到底发出去了没有？"),
            ],
            "assistant_turns": [
                turn("assistant", "2026-10-24T16:39:00+08:00",
                     "昨天 21:40 发了 0008 批注，回执没有回来。"),
            ],
            "tool_results": [{
                "tool": "msg", "at": "2026-10-23T21:40:12+08:00",
                "result": {"state": "unknown", "message": {"sender": ME_NODE, "id": "msg-77"}},
            }],
            "outbound": [{
                "message": {"sender": ME_NODE, "id": "msg-77"},
                "to": "reviewer-a", "thread": "review", "seq": None,
                "state": "unknown",
            }],
        },
        expanded=lambda s: {
            **s,
            "threads": s["threads"] + [RESEARCH],
            "outbound": s["outbound"] + [{
                "message": {"sender": ME_NODE, "id": "msg-70"},
                "to": "dpdp", "thread": "research", "seq": "70",
                "plan": {"response": "async", "execution": "new"},
                "state": "accepted",
            }],
            "thread_messages": [{
                "thread": "review", "seq": "225",
                "sender": "urn:onlyne:agent:reviewer-b",
                "at": "2026-10-23T18:00:00+08:00",
                "text": "0005 的意见晚点给。",
            }],
        },
    ),
    case(
        "send-dev-15", "dev",
        "构造场景：用户先让发 dpdp，随后改口发 caerulea；message.meta 仍是改口前的草稿。"
        "对应 RFC 0016 §2 的“保留后续更正”，非抓取。",
        msg("请 caerulea 看 0011 草稿的域划分",
            "0011 草稿发你，重点看域划分那节。",
            {"to": "dpdp"}),
        ["caerulea", "dpdp", "alexandria"], [RESEARCH, DOCS], [],
        "target",
        [peer_option("caerulea"), peer_option("dpdp"), peer_option("alexandria"),
         ("clarify", "Ask the user which recipient is meant.")],
        {"target": "caerulea"},
        focused={
            "user_turns": [
                turn("user", "2026-10-24T10:00:00+08:00",
                     "把 0011 草稿发给 dpdp，让他看域划分。"),
                turn("user", "2026-10-24T10:03:00+08:00",
                     "等等，别发 dpdp，发 caerulea，她一起看域划分。"),
            ],
            "assistant_turns": [
                turn("assistant", "2026-10-24T10:01:00+08:00",
                     "草稿目标先写的 dpdp。"),
            ],
        },
        expanded=lambda s: {
            **s,
            "peers": s["peers"] + peers("reviewer-chamber"),
            "contexts": [C_DPDP_REVIEW],
            "notes": [{
                "kind": "context_note",
                "custom_type": "experimental_context_notes",
                "text": "0011 域划分初稿已写，收件人待定。",
                "updated": "2026-10-24T09:55:00+08:00",
                "covers": "active branch through msg 50",
                "reset_boundary": "none since note",
            }],
        },
    ),
    case(
        "send-dev-16", "dev",
        "构造场景：assistant 计划发 dpdp，用户改口发给 alexandria 并给出职责理由；"
        "message 没有写 to，需要按当前用户指令解析。非抓取。",
        msg("把 0016 结果发出去", "0016 的 dev 结果整理在这份报告里。"),
        ["alexandria", "dpdp", "reviewer-chamber"], [DOCS, RESEARCH], [],
        "target",
        [peer_option("alexandria"), peer_option("dpdp"), peer_option("reviewer-chamber"),
         ("clarify", "Ask the user which recipient is meant.")],
        {"target": "alexandria"},
        focused={
            "user_turns": [
                turn("user", "2026-10-24T11:20:00+08:00",
                     "0016 实验结果整理好就发 dpdp。"),
                turn("user", "2026-10-24T11:41:00+08:00",
                     "不用给 dpdp，发给 alexandria，她管参考文档。"),
            ],
            "assistant_turns": [
                turn("assistant", "2026-10-24T11:21:00+08:00",
                     "原计划发 dpdp。"),
                turn("assistant", "2026-10-24T11:42:00+08:00",
                     "报告已整理，目标按最新指令改成 alexandria。"),
            ],
        },
        expanded=lambda s: {
            **s,
            "threads": s["threads"] + [OPS],
            "outbound": [{
                "message": {"sender": ME_NODE, "id": "msg-95"},
                "to": "dpdp", "thread": "research", "seq": "95",
                "plan": {"response": "async", "execution": "new"},
                "state": "accepted",
            }],
            "notes": [{
                "kind": "context_note",
                "custom_type": "experimental_context_notes",
                "text": "旧记录：0016 结果发 dpdp。",
                "updated": "2026-10-24T08:30:00+08:00",
                "covers": "active branch through msg 41",
                "reset_boundary": "none since note",
            }],
        },
    ),

    # -------------------------------------------------------------- holdout
    case(
        "send-hold-01", "holdout",
        "构造场景：0003 批注的后续补充，目标由上一轮回执确定；与 dev 的同名场景不是同一任务。"
        "非抓取。",
        msg("把 0005 批注也发给刚收到 0003 批注的人",
            "0005 的批注也补给你。",
            {"thread": "review"}),
        ["reviewer-b", "reviewer-a", "alexandria"], [REVIEW], [C_REVIEW_B],
        "plan",
        [
            send_plan("reviewer-b", "review", "none", "existing", "c-4"),
            send_plan("reviewer-a", "review", "none", "existing", "c-4"),
            send_plan("reviewer-b", "review", "async", "new"),
            clarify_plan(),
        ],
        {"plan": "send_reviewer-b_review_none_existing_c-4"},
        focused={
            "user_turns": [
                turn("user", "2026-10-24T17:00:00+08:00",
                     "那份 0005 的批注也给他。"),
            ],
            "assistant_turns": [
                turn("assistant", "2026-10-24T10:11:00+08:00",
                     "0003 的批注发给了 reviewer-b：review seq 88，上下文 c-4。"),
            ],
            "tool_results": [receipt("msg-88", "reviewer-b", "review", "88",
                                     context="c-4", response="none", execution="new")],
            "outbound": [receipt("msg-88", "reviewer-b", "review", "88",
                                 context="c-4", response="none", execution="new")["result"]],
        },
        expanded=lambda s: {
            **s,
            "threads": s["threads"] + [DOCS],
            "contexts": s["contexts"] + [{
                "alias": "c-6", "thread": "review",
                "root": {"sender": ME_NODE, "id": "msg-70"},
                "task": "0003 批注（旧副本）", "assignee": "urn:onlyne:agent:reviewer-a",
                "state": "completed", "started": "2026-10-20T09:00:00+08:00",
            }],
            "outbound": s["outbound"] + [{
                "message": {"sender": ME_NODE, "id": "msg-70"},
                "to": "reviewer-a", "thread": "review", "seq": "70",
                "plan": {"response": "none", "execution": "new"},
                "context": "c-6", "state": "accepted",
            }],
        },
    ),
    case(
        "send-hold-02", "holdout",
        "构造场景：reviewer 角色已转给 research_chamber，但用户要求找“原来审过 0008 的那个”，"
        "稳定身份与历史优先于当前角色名。角色表按 RFC 0011 重建。",
        msg("请原来审过 0008 的 reviewer 复看修改",
            "0008 按上次三点意见改完了，麻烦再看一遍。"),
        ["reviewer-local", "reviewer-chamber", "reviewer-a"], [REVIEW, RESEARCH], [],
        "plan",
        [
            send_plan("reviewer-local", None, "async", "new"),
            send_plan("reviewer-chamber", None, "async", "new"),
            send_plan("reviewer-local", "review", "async", "new"),
            clarify_plan(),
        ],
        {"plan": "send_reviewer-local_direct_async_new"},
        focused={
            "user_turns": [
                turn("user", "2026-10-24T17:20:00+08:00",
                     "还是找原来那个 reviewer 看 0008 的修改，他熟上下文。"),
            ],
            "assistant_turns": [
                turn("assistant", "2026-10-24T17:19:00+08:00",
                     "0008 上次是 reviewer-local 审的，提了三点。"),
            ],
            "thread_messages": [{
                "thread": "review", "seq": "201",
                "sender": "urn:onlyne:agent:reviewer-local",
                "at": "2026-10-18T15:00:00+08:00",
                "text": "0008 三点：批注范围、回复权威、超时处置。",
            }],
            "topology": {
                "role_assignments": [
                    {"role": "reviewer", "holder": "urn:onlyne:agent:reviewer-chamber",
                     "since": "2026-10-22T10:00:00+08:00"},
                    {"role": "release-manager", "holder": "urn:onlyne:agent:reviewer-local",
                     "since": "2026-10-22T10:00:00+08:00"},
                ],
            },
        },
        expanded=lambda s: {
            **s,
            "threads": s["threads"] + [DOCS],
            "notes": [{
                "kind": "context_note",
                "custom_type": "experimental_context_notes",
                "text": "reviewer 角色现在归 reviewer-chamber，找它处理 review。",
                "updated": "2026-10-22T10:05:00+08:00",
                "covers": "active branch through msg 12",
                "reset_boundary": "none since note",
            }],
            "outbound": [{
                "message": {"sender": ME_NODE, "id": "msg-60"},
                "to": "reviewer-chamber", "thread": None, "seq": "60",
                "plan": {"response": "async", "execution": "new"},
                "state": "accepted",
            }],
        },
    ),
    case(
        "send-hold-03", "holdout",
        "构造场景：第三组重算刚落到 c-5，用户追加“先不用跑”；与 dev 的第二组补充是不同任务。"
        "回执字段按 RFC 0005 构造。",
        msg("补充刚才那个任务：第三组先不用跑",
            "第三组先停，先出第二组的图。"),
        ["caerulea", "dpdp"], [RESEARCH], [C_CAERULEA_RECALC, C_CAERULEA_THIRD],
        "plan",
        [
            send_plan("caerulea", "research", "none", "existing", "c-5"),
            send_plan("caerulea", "research", "none", "existing", "c-1"),
            send_plan("caerulea", "research", "none", "new"),
            clarify_plan(),
        ],
        {"plan": "send_caerulea_research_none_existing_c-5"},
        focused={
            "user_turns": [
                turn("user", "2026-10-24T17:40:00+08:00",
                     "接着刚才那个任务说，第三组先不用跑。"),
            ],
            "assistant_turns": [
                turn("assistant", "2026-10-24T11:03:00+08:00",
                     "第三组重算已派给 caerulea：research seq 118，上下文 c-5，正在跑。"),
            ],
            "tool_results": [receipt("msg-52", "caerulea", "research", "118",
                                     context="c-5", response="async", execution="new")],
            "outbound": [receipt("msg-52", "caerulea", "research", "118",
                                 context="c-5", response="async", execution="new")["result"]],
            "todo": {
                "source": "omp-todo-more:read (public event)",
                "goal": "第三组数据重算",
                "open_items": [{"title": "第三组重算", "source": "msg-52"}],
            },
        },
        expanded=lambda s: {
            **s,
            "threads": s["threads"] + [OPS],
            "contexts": s["contexts"] + [C_OLD_PLOT, C_DPDP_REVIEW],
            "outbound": s["outbound"] + [{
                "message": {"sender": ME_NODE, "id": "msg-95"},
                "to": "dpdp", "thread": "research", "seq": "95",
                "plan": {"response": "async", "execution": "new"},
                "context": "c-2", "state": "accepted",
            }],
        },
    ),
    case(
        "send-hold-04", "holdout",
        "构造场景：用户明确不续 c-5，另开 0011 域发现任务；与 dev 的第三组对照是另一任务。",
        msg("让 caerulea 另开任务查 0011 的域发现",
            "0011 的域发现逻辑单独查一遍，别混进第三组。",
            {"to": "caerulea"}),
        ["caerulea", "dpdp"], [RESEARCH], [C_CAERULEA_THIRD, C_CAERULEA_RECALC],
        "plan",
        [
            send_plan("caerulea", "research", "async", "new"),
            send_plan("caerulea", "research", "async", "existing", "c-5"),
            send_plan("caerulea", "research", "none", "new"),
            clarify_plan(),
        ],
        {"plan": "send_caerulea_research_async_new"},
        focused={
            "user_turns": [
                turn("user", "2026-10-24T18:00:00+08:00",
                     "这个不要续 c-5，另开一个任务让 caerulea 查 0011 的域发现。"),
            ],
            "assistant_turns": [
                turn("assistant", "2026-10-24T17:59:00+08:00",
                     "c-5 正在跑第三组重算。"),
            ],
            "tool_results": [receipt("msg-52", "caerulea", "research", "118",
                                     context="c-5", response="async", execution="new")],
            "todo": {
                "source": "omp-todo-more:read (public event)",
                "goal": "第三组数据重算",
                "open_items": [{"title": "第三组重算", "source": "msg-52"}],
            },
        },
        expanded=lambda s: {
            **s,
            "threads": s["threads"] + [DOCS],
            "contexts": s["contexts"] + [C_OLD_PLOT],
            "notes": [{
                "kind": "context_note",
                "custom_type": "experimental_context_notes",
                "text": "旧安排：域发现跟第三组一起查。",
                "updated": "2026-10-23T19:00:00+08:00",
                "covers": "branch main through msg 30",
                "reset_boundary": "cleared 2026-10-24T07:30:00+08:00 by /clear",
            }],
        },
    ),
    case(
        "send-hold-05", "holdout",
        "构造场景：压缩 note 写于 09:12，09:25 用户改口；meta 仍是旧草稿的 dpdp。"
        "对应 RFC 0016 §2.1 的 note 时效与后续更正，非抓取。",
        msg("把 0016 结果发给现在等结果的人",
            "0016 dev 结果和失败项都在报告里。",
            {"to": "dpdp"}),
        ["caerulea", "dpdp", "alexandria"], [RESEARCH, DOCS], [],
        "target",
        [peer_option("caerulea"), peer_option("dpdp"), peer_option("alexandria"),
         ("clarify", "Ask the user which recipient is meant.")],
        {"target": "caerulea"},
        focused={
            "notes": [{
                "kind": "context_note",
                "custom_type": "experimental_context_notes",
                "text": "当前任务：把 0016 实验结果发给 dpdp，等他确认。",
                "updated": "2026-10-24T09:12:00+08:00",
                "covers": "active branch through msg 41",
                "reset_boundary": "none since note",
            }],
            "user_turns": [
                turn("user", "2026-10-24T09:25:00+08:00",
                     "0016 结果改发给 caerulea，她那边也在等，dpdp 先不用了。"),
                turn("user", "2026-10-24T09:26:00+08:00", "把结果发了吧。"),
            ],
            "assistant_turns": [
                turn("assistant", "2026-10-24T09:27:00+08:00",
                     "0016 报告已整理，目标从 dpdp 改成 caerulea。"),
            ],
            "tool_results": [{
                "tool": "read", "at": "2026-10-24T09:20:00+08:00",
                "result": {"scope": "messages", "thread": "research", "seq": "96",
                           "text": "caerulea：0016 结果出来发我一份。"},
            }],
        },
        expanded=lambda s: {
            **s,
            "threads": s["threads"] + [OPS],
            "outbound": [{
                "message": {"sender": ME_NODE, "id": "msg-95"},
                "to": "dpdp", "thread": "research", "seq": "95",
                "plan": {"response": "async", "execution": "new"},
                "state": "accepted",
            }],
        },
    ),
    case(
        "send-hold-06", "holdout",
        "构造场景：用户上一句同时提到 dpdp 与 caerulea，随后用“他”指代；没有唯一先行词。"
        "按 RFC 0016 §1 的证据不足路径设计。",
        msg("把重启配置发给他", "重启用的配置在这里。"),
        ["dpdp", "caerulea", "alexandria"], [RESEARCH, OPS], [],
        "target",
        [peer_option("dpdp"), peer_option("caerulea"), peer_option("alexandria"),
         ("clarify", "Ask the user which recipient is meant.")],
        {"target": "clarify"},
        focused={
            "user_turns": [
                turn("user", "2026-10-24T12:01:00+08:00",
                     "dpdp 说她那边实验环境好了，caerulea 也说集群能重启。"),
                turn("user", "2026-10-24T12:02:00+08:00", "那把这个发给他。"),
            ],
            "assistant_turns": [
                turn("assistant", "2026-10-24T12:01:30+08:00",
                     "两边都能接手，配置只有一份。"),
            ],
            "notes": [{
                "kind": "context_note",
                "custom_type": "experimental_context_notes",
                "text": "待办：重启配置分发，接收方未定。",
                "updated": "2026-10-24T12:00:00+08:00",
                "covers": "active branch through msg 70",
                "reset_boundary": "none since note",
            }],
        },
        expanded=lambda s: {
            **s,
            "threads": s["threads"] + [DOCS],
            "outbound": [{
                "message": {"sender": ME_NODE, "id": "msg-68"},
                "to": "dpdp", "thread": "research", "seq": "68",
                "plan": {"response": "async", "execution": "new"},
                "state": "accepted",
            }],
        },
    ),
    case(
        "send-hold-07", "holdout",
        "构造场景：用户问 review thread 里 reviewer 是否回复；本地有 await 记录，"
        "读范围按 RFC 0005。非抓取。",
        msg("查 review thread 里 reviewer 有没有回我", "",
            {"thread": "review"}),
        ["reviewer-a", "reviewer-b"], [REVIEW], [],
        "plan",
        [
            read_plan("inbox_thread_review",
                      "Read local attention entries filtered to thread review; "
                      "this lists messages that mention or reply to this agent."),
            read_plan("messages_thread_review",
                      "Read the full stored history of thread review."),
            read_plan("evidence_msg-223",
                      "Read the stored delivery state of outbound message msg-223."),
            clarify_plan(),
        ],
        {"plan": "read_inbox_thread_review"},
        focused={
            "user_turns": [
                turn("user", "2026-10-24T18:20:00+08:00", "看看 reviewer 回我没。"),
            ],
            "assistant_turns": [
                turn("assistant", "2026-10-24T18:19:00+08:00",
                     "0008 的回复我发在 review thread，等 reviewer-a 回。"),
            ],
            "outbound": [{
                "message": {"sender": ME_NODE, "id": "msg-223"},
                "to": "reviewer-a", "thread": "review", "seq": "223",
                "plan": {"response": "await", "execution": "new"},
                "state": "accepted",
            }],
            "waits": [{
                "kind": "reply_wait",
                "message": {"sender": ME_NODE, "id": "msg-223"},
                "peer": "reviewer-a", "thread": "review",
                "since": "2026-10-24T18:10:00+08:00",
            }],
        },
        expanded=lambda s: {
            **s,
            "threads": s["threads"] + [RESEARCH, DOCS],
            "outbound": s["outbound"] + [{
                "message": {"sender": ME_NODE, "id": "msg-218"},
                "to": "reviewer-b", "thread": "review", "seq": "218",
                "plan": {"response": "none", "execution": "new"},
                "state": "accepted",
            }],
            "thread_messages": [{
                "thread": "review", "seq": "225",
                "sender": "urn:onlyne:agent:reviewer-b",
                "at": "2026-10-24T18:15:00+08:00",
                "text": "0005 的意见我晚点补。",
            }],
        },
    ),
    case(
        "send-hold-08", "holdout",
        "构造场景：草稿按 in_reply_to 回 reviewer-a，用户latest 指令改成回 caerulea 确认参数；"
        "显式指令覆盖引用推断。非抓取。",
        msg("回 caerulea 确认参数",
            "参数按 64 还是 128？确认后我改。",
            {"thread": "review",
             "in_reply_to": {"sender": "urn:onlyne:agent:reviewer-a", "id": "msg-223"}}),
        ["caerulea", "reviewer-a", "reviewer-b"], [REVIEW, RESEARCH], [],
        "target",
        [peer_option("caerulea"), peer_option("reviewer-a"), peer_option("reviewer-b"),
         ("clarify", "Ask the user which recipient is meant.")],
        {"target": "caerulea"},
        focused={
            "user_turns": [
                turn("user", "2026-10-24T18:40:00+08:00",
                     "这条回给 caerulea，让她确认参数，reviewer 那边我来解释。"),
            ],
            "assistant_turns": [
                turn("assistant", "2026-10-24T18:39:00+08:00",
                     "原草稿是回 reviewer-a 的。"),
            ],
            "thread_messages": [{
                "thread": "review", "seq": "223",
                "sender": "urn:onlyne:agent:reviewer-a",
                "at": "2026-10-23T15:10:00+08:00",
                "text": "参数用哪个版本？",
            }],
        },
        expanded=lambda s: {
            **s,
            "threads": s["threads"] + [OPS],
            "contexts": [C_CAERULEA_RECALC],
            "outbound": [{
                "message": {"sender": ME_NODE, "id": "msg-223"},
                "to": "reviewer-a", "thread": "review", "seq": "223",
                "plan": {"response": "await", "execution": "new"},
                "state": "accepted",
            }],
        },
    ),
]


_QUESTION_TEXT = {
    "plan": (
        "Choose the one plan supported by the message intent and the supplied "
        "evidence. A plan fixes the action, its recipient or scope, the thread or "
        "execution context it uses, and how the sender waits. Choose clarify when "
        "the decisive element cannot be determined from the evidence."
    ),
    "target": "Which recipient should this message be delivered to?",
    "response": "How does the sender intend to wait after this message?",
}

_REVISION_POLICY = {
    "initial": "",
    "tuned": (
        " The message triple is a draft by the main model, not a user decision. "
        "The latest explicit user instruction outranks earlier drafts, assistant "
        "plans, notes and stale message fields. A pronoun resolves only when one "
        "antecedent is uniquely supported. A followup keeps the execution context "
        "of the earlier request; an explicitly new task does not. "
        "Response is none when the intent only delivers material, status or a "
        "notice with no answer requested; async when an answer is expected but "
        "the sender keeps working; await only when the intent states the sender "
        "stops for the answer. A thread is used only when the intent names it or "
        "the message continues that thread's subject; a direct operational notice "
        "to one person about another topic goes direct. When two peers share a "
        "display name and no evidence names one of them, choose clarify. When the "
        "user asks whether a message reached anyone, read the attention entries "
        "for that thread; read the full thread history only when the user asks "
        "for the thread's contents, and delivery state only when the user asks "
        "about one outbound message."
    ),
}


def questions(case, revision):
    key = case["ask"]
    return {
        key: {
            "type": "choice",
            "instructions": _QUESTION_TEXT[key] + _REVISION_POLICY[revision],
            "criteria": dict(case["options"]),
        }
    }
