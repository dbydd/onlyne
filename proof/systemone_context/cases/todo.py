"""SystemOne 判定样本：持续工作与 todo 效果。

判定点只覆盖 RFC 0016 §3.7 交给 SystemOne 的语义选择：一份新到的材料应当
新建工作项、补充已有工作、仅留作信息、要求澄清，还是改变阻塞条目的状态。
授权、版本比较、持久提交、去重键属于库与运行时，不进入问题集。

每个样本给出一份 todo-more 快照（goal、阶段、条目、状态、explanation 首段的
来源），加一份材料（用户指令、thread 来信、工具结果或重投）。三档 profile：

- minimal：只给本节点身份、材料和当前快照。
- focused：补上与该判断相关的用户最近指令、assistant 承诺、thread 往来、
  子代理目录和 context note。
- expanded：再加现实中的干扰项：别的会话里的同名清单、被更正的旧指令、
  过期 note、无关子代理与无关 thread 消息。

快照按安装包的完整状态读取器标注来源：公开事件 `omp-todo-more:read` 只返回
goal 与 openItems，blocked 与 completed 条目需要 `state.readTodoMore(branch)`
适配器，样本一律标注该适配器与覆盖范围。

候选方案包含所有合理条目与常见误判（重开已完成的同名条目、按同名条目合并、
把回执当完成），不按关键词、thread 或根引用预筛。expected 与 provenance
不进入模型请求。
"""

from __future__ import annotations

ME = {
    "node_id": "urn:onlyne:agent:lazuli",
    "display": "lazuli",
    "domain": "urn:onlyne:domain:main",
    "session": "omp-main",
}

DPDP = {"alias": "dpdp", "node_id": "urn:onlyne:agent:dpdp"}
CAERULEA = {"alias": "caerulea", "node_id": "urn:onlyne:agent:caerulea"}
ALEXANDRIA = {"alias": "alexandria", "node_id": "urn:onlyne:agent:alexandria"}

RESEARCH = {"owner": "urn:onlyne:agent:lazuli", "id": "research"}
OPS = {"owner": "urn:onlyne:agent:caerulea", "id": "ops"}
REVIEW = {"owner": "urn:onlyne:agent:alexandria", "id": "review"}

GOAL = {
    "objective": "把 Onlyne v3 做到可发布",
    "measure": "RFC 结论、proof 结果、库实现、OMP 接入与发布各有可回查证据",
    "done_when": "v3 包已发布，OMP 里能收发消息并看到待办",
}

CAPS = {
    "todo_provider": "omp-todo-more",
    "todo_write_path": "pi.appendEntry(snapshot) + session.setTodoPhases",
    "subagent_spawn": "task tool",
    "branch_mode": "execute, read from a mode_change entry",
}

PHASES = ("RFC", "Proof", "Plugin", "Bench", "Docs", "Release")


def ref(sender: str, mid: str) -> dict:
    return {"sender": sender, "id": mid}


def item(alias: str, phase: str, title: str, status: str, explanation: str,
         blocked_on: str | None = None, source: dict | None = None,
         context: str | None = None) -> dict:
    entry = {"alias": alias, "phase": phase, "title": title, "status": status,
             "explanation": explanation}
    if blocked_on is not None:
        entry["blocked_on"] = blocked_on
    if source is not None:
        entry["source"] = source
    if context is not None:
        entry["context"] = context
    return entry


def snapshot(items: list[dict], revision: str = "snap-41") -> dict:
    order: list[str] = []
    grouped: dict[str, list[dict]] = {}
    for raw in items:
        entry = {key: value for key, value in raw.items() if key != "phase"}
        phase = raw["phase"]
        if phase not in grouped:
            grouped[phase] = []
            order.append(phase)
        grouped[phase].append(entry)
    return {"goal": dict(GOAL), "phases": [{"name": name, "items": grouped[name]} for name in order],
            "revision": revision,
            "source": {"adapter": "state.readTodoMore(branch)",
                       "captured_at": "2026-10-24T03:10:00Z",
                       "coverage": "goal, phases and every item status",
                       "survives_session_clear": True,
                       "public_event": {"name": "omp-todo-more:read",
                                        "returns": "goal and open items only",
                                        "conflates": "no items, all items closed, source missing"}}}


def thread_message(sender: dict, thread: dict, seq: str, kind: str, body: str,
                   message_id: str, relation: str | None = None,
                   in_reply_to: dict | None = None, mentions: list[str] | None = None,
                   recorded_as: dict | None = None) -> dict:
    material = {
        "kind": "thread_message",
        "sender": sender["node_id"],
        "sender_display": sender["alias"],
        "thread_ref": thread,
        "seq": seq,
        "message_ref": ref(sender["node_id"], message_id),
        "relation": relation,
        "in_reply_to": in_reply_to,
        "mentions": mentions or [],
        "delivery_kind": kind,
        "body": body,
        "authenticated": True,
    }
    if recorded_as is not None:
        material["recorded_as"] = recorded_as
    return material


def user_instruction(text: str, turn: str) -> dict:
    return {"kind": "user_instruction", "turn": turn, "text": text}


def tool_result(tool: str, status: str, detail: dict) -> dict:
    return {"kind": "tool_result", "tool": tool, "status": status, "detail": detail}


# ------------------------------------------------------------------ 候选方案

def plan_new(detail: str) -> str:
    return f"Open a new item for this material; existing items keep their recorded scope and state. {detail}"


def plan_supp(alias: str, detail: str) -> str:
    return f"Extend the open item {alias} with this material; add no second item. {detail}"


def plan_info(detail: str) -> str:
    return f"Change nothing in the list; the material stays as message evidence only. {detail}"


def plan_clarify(detail: str) -> str:
    return f"Ask which work this belongs to before writing anything. {detail}"


def plan_resume(alias: str, detail: str) -> str:
    return f"Clear the blocked state on {alias} and continue it; add no item. {detail}"


def plan_hold(alias: str, detail: str) -> str:
    return f"Leave {alias} blocked and attach this material to it; start nothing. {detail}"


def plan_complete(alias: str, detail: str) -> str:
    return f"Mark {alias} completed; add no item and send no reply. {detail}"


def plan_reopen(alias: str, detail: str) -> str:
    return f"Return the completed item {alias} to pending and put this work there; add no item. {detail}"


# ------------------------------------------------------------------ profile

DISTRACT_THREAD = {
    "thread_ref": OPS,
    "messages": [
        {"seq": "39", "sender": "urn:onlyne:agent:caerulea", "body": "旧的部署脚本我删了，别再引用。"},
        {"seq": "43", "sender": "urn:onlyne:agent:caerulea", "body": "v2 的运维手册先冻住，等 v3 再写。"},
    ],
}

DISTRACT_OTHER_LIST = {
    "session": "omp-review",
    "goal": {"objective": "整理评审材料"},
    "items": [
        {"title": "README 重设计", "status": "completed", "phase": "Docs"},
        {"title": "重跑对照组", "status": "completed", "phase": "Bench"},
        {"title": "跑 dev 集 benchmark", "status": "completed", "phase": "Bench"},
    ],
}

STALE_NOTE = {
    "updated_at": "2026-10-20T09:12:00Z",
    "covers_until": "turn 18",
    "text": "结论：发布依赖用户手动发布；所有待办都只在主会话里维护。",
    "status": "superseded by later user instruction",
}

EXTRA_CHILDREN = [
    {"alias": "c-docs", "thread": "docs", "root": ref("urn:onlyne:agent:lazuli", "msg-19"),
     "summary": "wiki 页面整理", "state": "idle", "session_file": "/sessions/docs-19.jsonl"},
    {"alias": "c-bench", "thread": "bench", "root": ref("urn:onlyne:agent:lazuli", "msg-24"),
     "summary": "跑 dev 集 benchmark", "state": "aborted", "session_file": "/sessions/bench-24.jsonl"},
]

SUPERSEDED_INSTRUCTIONS = [
    {"turn": "u-31", "text": "benchmark 先按全量跑一遍看基线。", "status": "corrected by u-33"},
    {"turn": "u-33", "text": "算了，先只跑 dev 集。", "status": "current"},
]

CASES: list[dict] = []


def add(case_id: str, split: str, material: dict, items: list[dict], plans: dict[str, str],
        expected: str, provenance: dict, *, user_recent: list[dict] | None = None,
        assistant_recent: list[dict] | None = None, thread_recent: list[dict] | None = None,
        children: list[dict] | None = None, note: dict | None = None,
        capabilities: dict | None = None, extra: dict | None = None,
        branch_history: list[dict] | None = None) -> None:
    todo = snapshot(items)
    minimal = {"decision": "todo", "me": dict(ME), "material": material, "todo": todo}
    focused = {key: value for key, value in minimal.items()}
    for name, value in (("user_recent", user_recent), ("assistant_recent", assistant_recent),
                        ("thread_recent", thread_recent), ("children", children), ("note", note),
                        ("capabilities", capabilities)):
        if value is not None:
            focused[name] = value
    if extra:
        focused.update(extra)
    expanded = {key: value for key, value in focused.items()}
    expanded["other_sessions"] = [DISTRACT_OTHER_LIST]
    expanded["older_instructions"] = [dict(entry) for entry in SUPERSEDED_INSTRUCTIONS]
    expanded["note_history"] = [STALE_NOTE]
    expanded["children"] = (children or []) + [dict(entry) for entry in EXTRA_CHILDREN]
    expanded["unrelated_thread_messages"] = [DISTRACT_THREAD]
    if branch_history is not None:
        expanded["branch_history"] = branch_history
    CASES.append({
        "id": case_id,
        "point": "todo",
        "split": split,
        "provenance": provenance,
        "profiles": {"minimal": minimal, "focused": focused, "expanded": expanded},
        "plans": plans,
        "expected": {"plan": expected},
    })


INSTRUCTIONS = {
    "initial": (
        "Choose the plan that the recorded material and the current todo list support."
    ),
    "tuned": (
        "Choose the plan the recorded evidence supports for the todo list. "
        "Match material to an item by its recorded scope and source reference, never by a shared title. "
        "An open item keeps material that corrects or narrows its own recorded scope. "
        "An instruction aimed at work an item already dispatched extends that item. "
        "A completed item is history: further work gets its own item unless the user explicitly reopens it. "
        "A user statement that a step is finished completes that item; a review remark alone extends it. "
        "A reply, receipt or delivery result shows a message moved; it does not show work finished, "
        "and a repeated source reference adds nothing to the list. "
        "A blocked item continues only when the evidence shows the named dependency changed; "
        "partial news keeps it blocked. "
        "Ask only when two existing items are equally supported."
    ),
}


def questions(case: dict, revision: str) -> dict:
    return {
        "plan": {
            "type": "choice",
            "instructions": INSTRUCTIONS[revision],
            "criteria": dict(case["plans"]),
        },
    }


PROV = {
    "origin": "constructed",
    "user_words": "none: 没有使用真实用户原文或转写",
    "basis": "RFC 0008 §6（todo 快照、来源关联、恢复与完成规则）、RFC 0016 §3.7（持续工作判定输入）",
}


def prov(note: str, basis: str | None = None) -> dict:
    entry = dict(PROV)
    entry["note"] = note
    if basis:
        entry["basis"] = basis
    return entry


# ------------------------------------------------------------------ 通用条目

PROOF_LIB = item("proof-lib", "Proof", "对着最新 RFC 实现 lib 并跑 proof_lib", "in_progress",
                 "Source: research/101 from dpdp\n\n按修订后的 RFC 重跑 proof_lib，记录剩余缺口。",
                 source={"thread": "research", "seq": "101", "sender": "urn:onlyne:agent:dpdp"})
RFC_DOMAINS = item("rfc-domains", "RFC", "补逻辑域划分与无 owner 节点组", "in_progress",
                   "Source: research/96 from caerulea\n\n节点只属于一个域；跨域走 thread。",
                   source={"thread": "research", "seq": "96", "sender": "urn:onlyne:agent:caerulea"})
RFC_MESSAGES = item("rfc-messages", "RFC", "把消息归属改成单 thread 引用", "completed",
                    "Source: research/88 from dpdp\n\n一条消息只归属一个 thread，跨 thread 用引用。",
                    source={"thread": "research", "seq": "88", "sender": "urn:onlyne:agent:dpdp"})
PLUGIN_SURFACE = item("plugin-surface", "Plugin", "OMP 插件消息面接入", "in_progress",
                      "Source: research/112 from dpdp\n\nmsg 单一入口、列表注入与子代理投递。",
                      source={"thread": "research", "seq": "112", "sender": "urn:onlyne:agent:dpdp"})
PUBLISH = item("publish", "Release", "发布 v3 包到 npm", "blocked",
               "Source: ops/44 from caerulea\n\npackage.json 已填好，等用户手动发布。",
               blocked_on="等用户完成 npm 2FA 手动发布",
               source={"thread": "ops", "seq": "44", "sender": "urn:onlyne:agent:caerulea"})
RECHECK = item("recheck-section3", "Proof", "复核第 3 节实验结论并回复 dpdp", "in_progress",
               "Source: research/103 from dpdp\n\n复核第 3 节 2025 年的实验结论，答复在 research/108。",
               source={"thread": "research", "seq": "103", "sender": "urn:onlyne:agent:dpdp"})
PROOF_RFC = item("proof-rfc", "Proof", "对照 RFC 复核 proof 结论", "blocked",
                 "Source: research/107 from dpdp\n\n等 dpdp 的 proof 报告，再决定是否重跑。",
                 blocked_on="等 dpdp 的 proof 报告",
                 source={"thread": "research", "seq": "107", "sender": "urn:onlyne:agent:dpdp"})
README_V2 = item("readme-v2", "Docs", "README 重设计（含封面图）", "completed",
                 "Source: research/90 from dpdp\n\n按评审意见重做，封面图已换。",
                 source={"thread": "research", "seq": "90", "sender": "urn:onlyne:agent:dpdp"})
README_REDESIGN = item("readme-redesign", "Docs", "按评审意见改 README 结构与图", "in_progress",
                       "Source: research/95 from dpdp\n\n结构调整加图，等主模型改完。",
                       source={"thread": "research", "seq": "95", "sender": "urn:onlyne:agent:dpdp"})
A2A_COMPARE = item("a2a-compare", "Docs", "A2A 对照表整理", "completed",
                   "Source: research/77 from alexandria\n\n对照 A2A 与 Onlyne 的消息字段。",
                   source={"thread": "research", "seq": "77", "sender": "urn:onlyne:agent:alexandria"})
BENCH_RUN = item("bench-run", "Bench", "跑全量 benchmark 并汇总", "in_progress",
                 "Source: u-31\n\n全量跑一遍，拿基线。")
BENCH_DEV = item("bench-dev", "Bench", "跑 dev 集 benchmark", "pending",
                 "Source: u-33\n\n只跑 dev 集，先看趋势。")
WIKI_DRAFT = item("wiki-draft", "Docs", "GitHub wiki 初稿", "completed",
                  "Source: research/104 from alexandria\n\n首页与快速开始两页。",
                  source={"thread": "research", "seq": "104", "sender": "urn:onlyne:agent:alexandria"})
RFC_A2A = item("rfc-a2a", "RFC", "补 A2A 参考与映射", "blocked",
               "Source: review/9 from alexandria\n\n等 alexandria 的 A2A RFC 摘要。",
               blocked_on="等 alexandria 的 A2A RFC 摘要",
               source={"thread": "review", "seq": "9", "sender": "urn:onlyne:agent:alexandria"})

# ------------------------------------------------------------------ dev

add(
    "todo-dev-01", "dev",
    user_instruction("顺便把飞书 IM 网关也做成资源节点接进来，和计算作业一样走 resource 动作。", "u-57"),
    [RFC_MESSAGES, RFC_DOMAINS, PROOF_LIB, PLUGIN_SURFACE, PUBLISH],
    {
        "new": plan_new("The list has RFC, proof, plugin and release work; no item covers an IM gateway."),
        "supp:plugin-surface": plan_supp("plugin-surface", "The plugin item covers the OMP message surface only."),
        "supp:proof-lib": plan_supp("proof-lib", "The proof item covers library conformance runs."),
        "info": plan_info("A new user instruction that names unstarted work."),
        "clarify": plan_clarify("Two or more existing items cover gateways equally."),
    },
    "new",
    prov("用户要求新增资源节点类工作；快照中没有对应条目。"),
    user_recent=[{"turn": "u-55", "role": "user", "text": "v3 先按资源节点分四类，IM、计算、数据、MCP。"}],
    assistant_recent=[{"turn": "u-56", "role": "assistant", "text": "四类里已经落的是计算与 MCP，IM 还没排进来。"}],
    capabilities=dict(CAPS),
)

add(
    "todo-dev-02", "dev",
    thread_message(DPDP, RESEARCH, "121", "麻烦你们把 A2A 参考那篇补一份摘要", "参考那篇补个摘要，我们后面对照要用。", "msg-58"),
    [PROOF_LIB, RFC_DOMAINS, PUBLISH],
    {
        "new": plan_new("The request is a document task; the open items cover proof, domains and release."),
        "supp:proof-lib": plan_supp("proof-lib", "The proof item covers the library conformance run."),
        "supp:rfc-domains": plan_supp("rfc-domains", "The domain item covers node grouping and owner rules."),
        "info": plan_info("A peer request that asks for reading only."),
        "clarify": plan_clarify("The peer's request matches two open items equally."),
    },
    "new",
    prov("同 thread 的来信提出清单未覆盖的文档工作。"),
    thread_recent=[{"seq": "118", "sender": "urn:onlyne:agent:dpdp", "body": "第 3 节我这边复核完了，结论在 proof/reports 里。"}],
    assistant_recent=[{"turn": "u-52", "role": "assistant", "text": "接下来只动 proof_lib 和域那篇，文档先不动。"}],
    capabilities=dict(CAPS),
)

add(
    "todo-dev-03", "dev",
    thread_message(DPDP, RESEARCH, "122", "第 3 节的复核范围也包含 2024 全年", "第 3 节的复核范围也包含 2024 全年，别只看今年的。", "msg-59",
                   relation="followup", in_reply_to=ref("urn:onlyne:agent:dpdp", "msg-51")),
    [RECHECK, PROOF_LIB, PUBLISH],
    {
        "supp:recheck-section3": plan_supp("recheck-section3", "Its recorded scope is the 2025 conclusions of section 3."),
        "new": plan_new("The request is a wider data range, which no item records."),
        "info": plan_info("A scope note for work already running."),
        "clarify": plan_clarify("The range note could belong to either proof item."),
    },
    "supp:recheck-section3",
    prov("同一来源消息的补充扩大已有条目的记录范围。"),
    thread_recent=[{"seq": "119", "sender": "urn:onlyne:agent:lazuli", "body": "第 3 节按 2025 年的口径复核，结果今晚给。"}],
    capabilities=dict(CAPS),
)

add(
    "todo-dev-04", "dev",
    user_instruction("把那个也一起改了。", "u-61"),
    [RFC_DOMAINS, PLUGIN_SURFACE, PROOF_LIB],
    {
        "clarify": plan_clarify("The instruction names no scope; two open items match it equally."),
        "supp:rfc-domains": plan_supp("rfc-domains", "The domain item covers node grouping and cross-domain threads."),
        "supp:plugin-surface": plan_supp("plugin-surface", "The plugin item covers domain-aware routing in OMP."),
        "new": plan_new("Both open items would be left untouched."),
        "info": plan_info("An instruction that changes nothing in the list."),
    },
    "clarify",
    prov("指代不明的用户更正；两个已有条目的记录范围都成立。"),
    user_recent=[{"turn": "u-59", "role": "user", "text": "域那块和插件都得改，具体先哪个你定。"},
                 {"turn": "u-60", "role": "user", "text": "两边都还没动。"}],
    assistant_recent=[{"turn": "u-60", "role": "assistant", "text": "域条目管节点组，插件条目管消息面，两条互不覆盖。"}],
    capabilities=dict(CAPS),
)

add(
    "todo-dev-05", "dev",
    user_instruction("npm 已发布，release 那条继续。", "u-63"),
    [PUBLISH, PROOF_LIB, RFC_DOMAINS],
    {
        "resume:publish": plan_resume("publish", "Its blocker was the manual npm release, which the user now reports done."),
        "hold:publish": plan_hold("publish", "It waits on the manual npm release."),
        "new": plan_new("A separate release item for the same package."),
        "info": plan_info("A status note about the release step."),
    },
    "resume:publish",
    prov("用户报告阻塞依赖已完成；同一快照下的另一半对照是 todo-dev-06。"),
    user_recent=[{"turn": "u-58", "role": "user", "text": "npm 哪个你填好 package.json 我手动发，npm 要 2fa。"}],
    assistant_recent=[{"turn": "u-59", "role": "assistant", "text": "package.json 已填好，等你发布后我再验安装。"}],
    note={"source": "experimental_context_notes", "updated_at": "2026-10-24T02:40:00Z",
          "reset_boundary": "turn 62", "covers_until": "turn 62",
          "text": "release 卡在用户手动 npm 发布；package.json 已填好，发布后验安装。"},
    branch_history=[
        {"turn": "u-55", "role": "assistant", "text": "v3 包准备发布，package.json 我改好了。"},
        {"turn": "u-56", "role": "user", "text": "npm 哪个你填好 package.json 我手动发，npm 要 2fa。"},
        {"turn": "u-57", "role": "tool", "text": "todo-more 写入：publish 标为 blocked，原因：等用户完成 npm 2FA 手动发布。"},
        {"turn": "u-63", "role": "user", "text": "npm 已发布，release 那条继续。"},
        {"turn": "u-64", "role": "tool", "text": "npm view @onlyne/lib → 3.0.0"},
    ],
    capabilities=dict(CAPS),
)

add(
    "todo-dev-06", "dev",
    user_instruction("npm 还没发，你先别动 release。", "u-63"),
    [PUBLISH, PROOF_LIB, RFC_DOMAINS],
    {
        "hold:publish": plan_hold("publish", "Its blocker is the manual npm release, which has not happened."),
        "resume:publish": plan_resume("publish", "It waits on the manual npm release."),
        "new": plan_new("A separate release item for the same package."),
        "info": plan_info("A status note that names no item."),
    },
    "hold:publish",
    prov("同一阻塞条目的对照样本：依赖仍未完成。"),
    user_recent=[{"turn": "u-58", "role": "user", "text": "npm 哪个你填好 package.json 我手动发，npm 要 2fa。"}],
    assistant_recent=[{"turn": "u-59", "role": "assistant", "text": "package.json 已填好，等你发布后我再验安装。"}],
    capabilities=dict(CAPS),
)

add(
    "todo-dev-07", "dev",
    thread_message(DPDP, RESEARCH, "123", "proof 报告还在跑，明天早上给你", "proof 报告还在跑，明天早上给你。", "msg-60",
                   relation="followup", in_reply_to=ref("urn:onlyne:agent:dpdp", "msg-52")),
    [PROOF_RFC, PROOF_LIB, RECHECK],
    {
        "hold:proof-rfc": plan_hold("proof-rfc", "Its blocker is exactly this proof report."),
        "resume:proof-rfc": plan_resume("proof-rfc", "It waits for the proof report."),
        "new": plan_new("A separate item that tracks the report itself."),
        "info": plan_info("A progress note with no item of its own."),
    },
    "hold:proof-rfc",
    prov("阻塞依赖未变；与 todo-dev-08 构成对照。"),
    thread_recent=[{"seq": "117", "sender": "urn:onlyne:agent:lazuli", "body": "proof 报告什么时候能给？我这边卡在等它。"}],
    capabilities=dict(CAPS),
)

add(
    "todo-dev-08", "dev",
    thread_message(DPDP, RESEARCH, "123", "proof 报告写完了", "proof 报告写完了，在 proof/reports/rfc-round2.md。", "msg-60",
                   relation="followup", in_reply_to=ref("urn:onlyne:agent:dpdp", "msg-52")),
    [PROOF_RFC, PROOF_LIB, RECHECK],
    {
        "resume:proof-rfc": plan_resume("proof-rfc", "Its blocker was the proof report, which now exists."),
        "hold:proof-rfc": plan_hold("proof-rfc", "It waits for the proof report."),
        "new": plan_new("A separate item for reading the report."),
        "info": plan_info("A delivery note about a file."),
    },
    "resume:proof-rfc",
    prov("阻塞依赖已完成；与 todo-dev-07 只差这一个事实。"),
    thread_recent=[{"seq": "117", "sender": "urn:onlyne:agent:lazuli", "body": "proof 报告什么时候能给？我这边卡在等它。"}],
    capabilities=dict(CAPS),
)

add(
    "todo-dev-09", "dev",
    user_instruction("README 再改一版，把联邦集群的例子也写进去。", "u-66"),
    [README_V2, RFC_DOMAINS, PUBLISH],
    {
        "new": plan_new("The list holds a completed README redesign; this is a further round with new content."),
        "reopen:readme-v2": plan_reopen("readme-v2", "README redesign, completed."),
        "info": plan_info("A request that reads as already satisfied."),
        "clarify": plan_clarify("The new round could also be a reopening of the completed item."),
    },
    "new",
    prov("同名条目已完成的场景；RFC 0008 规定标题不是关联键。"),
    user_recent=[{"turn": "u-64", "role": "user", "text": "README 那版图好了，我看过了。"}],
    assistant_recent=[{"turn": "u-65", "role": "assistant", "text": "README 重设计那条已经标完成。"}],
    capabilities=dict(CAPS),
)

add(
    "todo-dev-10", "dev",
    user_instruction("README 那版我看过了，这一步标完成。", "u-66"),
    [README_REDESIGN, RFC_DOMAINS, PUBLISH],
    {
        "complete:readme-redesign": plan_complete("readme-redesign", "README structure and figures, in progress."),
        "supp:readme-redesign": plan_supp("readme-redesign", "README structure and figures, in progress."),
        "info": plan_info("A review remark that leaves the item running."),
        "new": plan_new("A separate item for the remaining review remarks."),
    },
    "complete:readme-redesign",
    prov("用户显式结束一项在办工作；完成由用户与模型驱动。"),
    user_recent=[{"turn": "u-64", "role": "user", "text": "README 的图再小一点，别占半页。"}],
    assistant_recent=[{"turn": "u-65", "role": "assistant", "text": "图已缩到三分之一页，等你再看一遍。"}],
    capabilities=dict(CAPS),
)

add(
    "todo-dev-11", "dev",
    thread_message(DPDP, RESEARCH, "124", "第 3 节 2024 的口径错了", "第 3 节里 2024 那部分口径错了，按季末重算再回我。", "msg-61",
                   relation="reply", in_reply_to=ref("urn:onlyne:agent:lazuli", "msg-55")),
    [RECHECK, PROOF_LIB, PUBLISH],
    {
        "supp:recheck-section3": plan_supp("recheck-section3", "Its recorded next step is the reply that just arrived."),
        "complete:recheck-section3": plan_complete("recheck-section3", "Its recorded next step is the reply that just arrived."),
        "info": plan_info("A peer reply that asks for no further work."),
        "new": plan_new("A separate item for recomputing the second group."),
    },
    "supp:recheck-section3",
    prov("回信给出同一节的口径更正；答复不等于完成。"),
    assistant_recent=[{"turn": "u-58", "role": "assistant", "text": "已回复 dpdp：第 3 节结论按原样给出。"}],
    thread_recent=[{"seq": "108", "sender": "urn:onlyne:agent:lazuli", "body": "第 3 节复核完了，2024 的数据按年度口径处理。"}],
    capabilities=dict(CAPS),
)

add(
    "todo-dev-12", "dev",
    tool_result("msg", "sent", {"to": "urn:onlyne:agent:dpdp", "thread_ref": RESEARCH, "seq": "108", "body": "第 3 节结论已发。"}),
    [RECHECK, PROOF_LIB, PUBLISH],
    {
        "info": plan_info("A delivery result for a message that is already out."),
        "complete:recheck-section3": plan_complete("recheck-section3", "Its message to dpdp was delivered."),
        "supp:recheck-section3": plan_supp("recheck-section3", "Its message to dpdp was delivered."),
        "new": plan_new("A separate item for tracking delivery."),
    },
    "info",
    prov("投送回执只证明消息发出；不证明工作完成。"),
    assistant_recent=[{"turn": "u-58", "role": "assistant", "text": "已把第 3 节结论发给 dpdp，等他回。"}],
    capabilities=dict(CAPS),
)

add(
    "todo-dev-13", "dev",
    user_instruction("子代理那边再补一个 settings 页的注册，别开新任务。", "u-61"),
    [PLUGIN_SURFACE, PROOF_LIB, RFC_DOMAINS],
    {
        "supp:plugin-surface": plan_supp("plugin-surface", "Its running child holds the plugin skeleton."),
        "new": plan_new("A separate item for the settings page."),
        "clarify": plan_clarify("The child could also belong to the proof item."),
        "info": plan_info("An instruction that only the child needs."),
    },
    "supp:plugin-surface",
    prov("补充说明落到子代理时，待办仍属于原条目。"),
    assistant_recent=[{"turn": "u-60", "role": "assistant", "text": "插件骨架已经派给子代理 c-plugin，消息面先做。"}],
    children=[{"alias": "c-plugin", "thread": "plugin", "root": ref("urn:onlyne:agent:dpdp", "msg-49"),
               "summary": "实现 OMP 插件消息面骨架", "state": "running",
               "session_file": "/sessions/plugin-49.jsonl"}],
    capabilities=dict(CAPS),
)

add(
    "todo-dev-14", "dev",
    thread_message(DPDP, RESEARCH, "103", "复核第 3 节的实验结论", "麻烦复核第 3 节的实验结论，答复我。", "msg-51",
                   recorded_as={"item": "recheck-section3", "commit": "todo-commit-77", "seq": "103"}),
    [RECHECK, PROOF_LIB, PUBLISH],
    {
        "info": plan_info("The same source reference is already attached to an item."),
        "new": plan_new("A second item for the repeated delivery."),
        "supp:recheck-section3": plan_supp("recheck-section3", "Already sourced from this message reference."),
        "clarify": plan_clarify("The repeat could belong to another item."),
    },
    "info",
    prov("同一 MessageRef 与同一 seq 的重投；关联已由可恢复提交建立。"),
    extra={"local_record": {"message_ref": ref("urn:onlyne:agent:dpdp", "msg-51"),
                            "item": "recheck-section3", "commit": "todo-commit-77",
                            "written_at": "2026-10-23T11:04:00Z"}},
    capabilities=dict(CAPS),
)

add(
    "todo-dev-15", "dev",
    user_instruction("不是全量，只跑 dev 集就行。", "u-33"),
    [BENCH_RUN, RFC_DOMAINS, PUBLISH],
    {
        "supp:bench-run": plan_supp("bench-run", "Its recorded scope is the full benchmark run."),
        "new": plan_new("A separate item for the dev-set run."),
        "clarify": plan_clarify("The narrowed scope could belong to a new item."),
        "info": plan_info("A scope correction that changes no item."),
    },
    "supp:bench-run",
    prov("用户更正缩小已有条目的范围；与 todo-dev-16 构成对照。"),
    user_recent=[{"turn": "u-31", "role": "user", "text": "benchmark 先按全量跑一遍看基线。"},
                 {"turn": "u-33", "role": "user", "text": "不是全量，只跑 dev 集就行。"}],
    assistant_recent=[{"turn": "u-32", "role": "assistant", "text": "已按全量排队，跑完给基线。"}],
    capabilities=dict(CAPS),
)

add(
    "todo-dev-16", "dev",
    user_instruction("只跑 dev 集就行，别跑全量。", "u-33"),
    [BENCH_DEV, BENCH_RUN, RFC_DOMAINS],
    {
        "supp:bench-dev": plan_supp("bench-dev", "Its recorded scope is the dev-set benchmark, still pending."),
        "supp:bench-run": plan_supp("bench-run", "Its recorded scope is the full benchmark run."),
        "new": plan_new("A third benchmark item beside the two recorded ones."),
        "clarify": plan_clarify("Both benchmark items are pending or running."),
    },
    "supp:bench-dev",
    prov("两个相似条目并存；用户点名 dev 集。"),
    user_recent=[{"turn": "u-31", "role": "user", "text": "benchmark 先按全量跑一遍看基线。"},
                 {"turn": "u-33", "role": "user", "text": "算了，先只跑 dev 集。"}],
    capabilities=dict(CAPS),
)

# ------------------------------------------------------------------ holdout

add(
    "todo-hold-01", "holdout",
    thread_message(CAERULEA, OPS, "45", "job-42 跑完了，麻烦按新数据出对照分析", "job-42 跑完了，结果在新的一批数据上；麻烦再按新数据出一份对照分析。", "msg-70"),
    [A2A_COMPARE, PROOF_LIB, PUBLISH],
    {
        "new": plan_new("The comparison table is complete and the new data asks for a further analysis."),
        "reopen:a2a-compare": plan_reopen("a2a-compare", "A2A comparison table, completed."),
        "supp:proof-lib": plan_supp("proof-lib", "The proof item covers library conformance."),
        "info": plan_info("A job result that needs no work."),
    },
    "new",
    prov("计算作业结果带来新的分析请求；同名对照表已完成。"),
    thread_recent=[{"seq": "44", "sender": "urn:onlyne:agent:caerulea", "body": "job-42 提交了，预计两小时。"}],
    capabilities=dict(CAPS),
)

add(
    "todo-hold-02", "holdout",
    thread_message(ALEXANDRIA, REVIEW, "14", "继续那份域划分的整理", "继续之前那份域划分的整理，把 ANP 的域间部分也加进去。", "msg-71"),
    [RFC_DOMAINS, {"alias": "review-notes", "phase": "Docs", "title": "整理评审意见清单",
                   "status": "pending", "explanation": "Source: review/11 from alexandria\n\n把评审意见按模块归类。"}],
    {
        "supp:rfc-domains": plan_supp("rfc-domains", "Domain grouping and owner rules, in progress in another thread."),
        "supp:review-notes": plan_supp("review-notes", "Review comment list, pending, in this thread."),
        "new": plan_new("A separate item for the ANP domain comparison."),
        "info": plan_info("A cross-thread continuation with no item of its own."),
    },
    "supp:rfc-domains",
    prov("跨 thread 的继续要求；同 thread 的条目只是干扰。"),
    thread_recent=[{"seq": "9", "sender": "urn:onlyne:agent:alexandria", "body": "A2A 的摘要我先写，ANP 的域间部分随后。"}],
    capabilities=dict(CAPS),
)

add(
    "todo-hold-03", "holdout",
    user_instruction("把重跑对照组这事做了。", "u-70"),
    [item("rerun-bench", "Bench", "重跑对照组", "pending",
          "Source: u-34\n\n重跑 benchmark 里的对照组，取三次中位数。"),
     item("rerun-proof", "Proof", "重跑对照组", "blocked",
          "Source: research/99 from dpdp\n\nproof 里的对照组重跑，等 dpdp 给脚本。",
          blocked_on="等 dpdp 给对照组脚本")],
    {
        "clarify": plan_clarify("Two items share the title and their recorded scopes differ."),
        "supp:rerun-bench": plan_supp("rerun-bench", "Bench phase, pending, benchmark control group."),
        "supp:rerun-proof": plan_supp("rerun-proof", "Proof phase, blocked on a peer script."),
        "new": plan_new("A third control-group item beside the two recorded ones."),
    },
    "clarify",
    prov("同名条目分属不同阶段；标题不是关联键。"),
    user_recent=[{"turn": "u-68", "role": "user", "text": "对照组那个脚本 dpdp 还没给。"}],
    capabilities=dict(CAPS),
)

add(
    "todo-hold-04", "holdout",
    thread_message(ALEXANDRIA, REVIEW, "15", "A2A 摘要写好了", "A2A 摘要写好了，docs/rfcs/0009 那版对照可以直接用。", "msg-72"),
    [RFC_A2A, RFC_DOMAINS, PROOF_LIB],
    {
        "resume:rfc-a2a": plan_resume("rfc-a2a", "Its blocker was this summary."),
        "hold:rfc-a2a": plan_hold("rfc-a2a", "It waits for the A2A summary."),
        "new": plan_new("A separate item for reading the summary."),
        "info": plan_info("A delivery note about a document."),
    },
    "resume:rfc-a2a",
    prov("阻塞依赖已完成；与 todo-hold-05 构成对照。"),
    thread_recent=[{"seq": "9", "sender": "urn:onlyne:agent:alexandria", "body": "A2A 的摘要我先写，ANP 的域间部分随后。"}],
    note={"source": "experimental_context_notes", "updated_at": "2026-10-24T02:05:00Z",
          "reset_boundary": "turn 66", "covers_until": "turn 66",
          "text": "rfc-a2a 等 alexandria 的摘要；参考文档已在 docs/rfcs/0009。"},
    branch_history=[
        {"turn": "u-64", "role": "assistant", "text": "A2A 参考已经归档到 docs/rfcs/0009。"},
        {"turn": "u-65", "role": "user", "text": "映射那篇先等 alexandria 的摘要，别自己重写。"},
        {"turn": "u-66", "role": "tool", "text": "todo-more 写入：rfc-a2a 标为 blocked，原因：等 alexandria 的 A2A RFC 摘要。"},
        {"turn": "u-69", "role": "tool", "text": "review/15 到达：alexandria 说摘要写好，在 docs/rfcs/0009。"},
    ],
    capabilities=dict(CAPS),
)

add(
    "todo-hold-05", "holdout",
    thread_message(ALEXANDRIA, REVIEW, "15", "还在翻译，今天给不了", "A2A 那篇还在翻译，今天给不了。", "msg-72"),
    [RFC_A2A, RFC_DOMAINS, PROOF_LIB],
    {
        "hold:rfc-a2a": plan_hold("rfc-a2a", "Its blocker is this summary, still missing."),
        "resume:rfc-a2a": plan_resume("rfc-a2a", "It waits for the A2A summary."),
        "new": plan_new("A separate item that tracks the translation."),
        "info": plan_info("A progress note with no item of its own."),
    },
    "hold:rfc-a2a",
    prov("同一个阻塞条目，依赖仍未完成。"),
    thread_recent=[{"seq": "9", "sender": "urn:onlyne:agent:alexandria", "body": "A2A 的摘要我先写，ANP 的域间部分随后。"}],
    capabilities=dict(CAPS),
)

add(
    "todo-hold-06", "holdout",
    thread_message(DPDP, RESEARCH, "125", "收到，我晚点看", "收到，我晚点看。", "msg-73",
                   relation="reply", in_reply_to=ref("urn:onlyne:agent:lazuli", "msg-55")),
    [RECHECK, PROOF_LIB, PUBLISH],
    {
        "info": plan_info("A bare acknowledgement with no new requirement."),
        "complete:recheck-section3": plan_complete("recheck-section3", "The peer answered this message."),
        "supp:recheck-section3": plan_supp("recheck-section3", "Its recorded next step is this reply."),
        "new": plan_new("A separate item for the peer's later review."),
    },
    "info",
    prov("只有回执语义的回信；不构成完成。"),
    assistant_recent=[{"turn": "u-58", "role": "assistant", "text": "已回复 dpdp：第 3 节结论按原样给出。"}],
    capabilities=dict(CAPS),
)

add(
    "todo-hold-07", "holdout",
    user_instruction("wiki 再加一页，写清楚插件怎么接。", "u-72"),
    [WIKI_DRAFT, RFC_DOMAINS, PROOF_LIB],
    {
        "new": plan_new("The wiki draft is complete; the plugin page is new content."),
        "reopen:wiki-draft": plan_reopen("wiki-draft", "Wiki first draft, completed."),
        "info": plan_info("A request that reads as already satisfied."),
        "clarify": plan_clarify("The new page could also be a reopening of the draft."),
    },
    "new",
    prov("已完成交付物上追加新内容；不重开旧条目。"),
    user_recent=[{"turn": "u-71", "role": "user", "text": "wiki 初稿那两页我看过了，可以。"}],
    capabilities=dict(CAPS),
)

add(
    "todo-hold-08", "holdout",
    user_instruction("我发完了，release 这条收了。", "u-74"),
    [PUBLISH, PROOF_LIB, RFC_DOMAINS],
    {
        "complete:publish": plan_complete("publish", "It was blocked only on the manual npm release, now done."),
        "resume:publish": plan_resume("publish", "It waits on the manual npm release."),
        "hold:publish": plan_hold("publish", "It waits on the manual npm release."),
        "info": plan_info("A status note about the release step."),
    },
    "complete:publish",
    prov("用户显式宣布发布完成；阻塞与完成在同一材料里给出。"),
    user_recent=[{"turn": "u-58", "role": "user", "text": "npm 哪个你填好 package.json 我手动发，npm 要 2fa。"}],
    assistant_recent=[{"turn": "u-73", "role": "assistant", "text": "package.json 已填好，等你发完我验安装。"}],
    capabilities=dict(CAPS),
)


def _validate() -> None:
    identifiers = set()
    for case in CASES:
        if case["id"] in identifiers:
            raise ValueError(f"duplicate id: {case['id']}")
        identifiers.add(case["id"])
        label = case["expected"]["plan"]
        if label not in case["plans"]:
            raise ValueError(f"{case['id']}: expected {label} is not a candidate")
        if len(case["plans"]) < 2:
            raise ValueError(f"{case['id']}: a choice requires competing options")
        for profile in ("minimal", "focused", "expanded"):
            if profile not in case["profiles"]:
                raise ValueError(f"{case['id']}: missing profile {profile}")
    dev = sum(case["split"] == "dev" for case in CASES)
    holdout = sum(case["split"] == "holdout" for case in CASES)
    if (dev, holdout) != (16, 8):
        raise ValueError(f"split sizes: dev={dev} holdout={holdout}")


_validate()
