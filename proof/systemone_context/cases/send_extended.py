"""Extended send-decision scenarios (independent of cases/send.py) for the outbound `msg` judgment point.

Every case is a hand-authored reconstruction of the workflows in RFC 0003,
RFC 0005, RFC 0008, and the user's own delegation turns. None is a captured
production trace. The state keeps the `delivery_kind` / `msg` / `meta` triple
intact under `intent`; host facts live under `host`, never inside the triple.

Profiles:
- minimal: only the decisive context for this case (the intent plus the one
  turn, context directory, or topology fact the decision needs).
- focused: the full task-specific evidence (branch turns with authority and
  freshness, thread and context directories, local waits).
- expanded: focused plus distractors (older turns, notes, host state) that do
  not raise their source authority.

`questions(case, revision)` uses shared instruction templates. Only candidate
ids and their observed properties vary between cases.
"""

from __future__ import annotations

ME = {
    "node_id": "urn:onlyne:agent:lazuli",
    "display": "lazuli",
    "domain": "urn:onlyne:domain:research",
    "limits": {"request_bytes": 1048576, "page_size": 50},
}


def peer(alias, display, responsibility, relation="domain_member", threads=(), node=None,
         can_post=True, can_read=True):
    return {
        "alias": alias,
        "node_id": node or f"urn:onlyne:agent:{alias}",
        "display": display,
        "responsibility": responsibility,
        "relation": relation,
        "threads": list(threads),
        "can_post": can_post,
        "can_read": can_read,
    }


def thread(alias, owner, state, my_role, members, next_seq, peer_access=("post", "read")):
    return {
        "alias": alias,
        "owner": owner,
        "state": state,
        "my_access": ["post", "read"],
        "my_role": my_role,
        "peer_access": list(peer_access),
        "next_seq": next_seq,
        "members": list(members),
    }


def context(alias, thread_alias, sender, message_id, summary, state):
    return {
        "alias": alias,
        "thread": thread_alias,
        "root": {"sender": sender, "id": message_id},
        "summary": summary,
        "state": state,
        "session_file": f"/sessions/{alias}.jsonl",
    }


def turn(seq, authority, at, text, **extra):
    entry = {"seq": seq, "authority": authority, "at": at, "text": text}
    if "from_" in extra:
        entry["from"] = extra.pop("from_")
    entry.update(extra)
    return entry


def note(at, text, *, active=True, reset_boundary=None):
    entry = {
        "authority": "note",
        "source": "experimental_context_notes",
        "at": at,
        "text": text,
        "active": active,
    }
    if reset_boundary:
        entry["reset_boundary"] = reset_boundary
    return entry


def state(intent, peers, threads, *, conversation=None, contexts=None, waits=None,
          host=None, notes=None, history_modes=None):
    entry = {
        "decision": "send",
        "me": ME,
        "intent": intent,
        "peers": list(peers),
        "threads": list(threads),
        "local": {
            "unknown_outbound": [],
            "open_waits": list(waits or []),
        },
    }
    if conversation:
        entry["conversation"] = list(conversation)
        entry["conversation_source"] = "ctx.sessionManager.getBranch() (active branch only)"
    if contexts is not None:
        entry["contexts"] = list(contexts)
    if notes:
        entry["notes"] = list(notes)
    if history_modes:
        entry["history_modes"] = history_modes
    if host:
        entry["host"] = dict(host)
    return entry


def caseless(kind, msg, meta):
    return {"delivery_kind": kind, "msg": msg, "meta": meta}


DPDP = peer("dpdp", "dpdp", "research analysis", relation="thread_member", threads=["research"])
CAERULEA = peer("caerulea", "caerulea", "data engineering", relation="thread_member", threads=["research"])
REVIEWER = peer("reviewer", "reviewer", "release windows", threads=[])
AUDITOR = peer("auditor", "auditor", "review and audit", threads=["review"])
REVIEWER_A = peer("reviewer-a", "reviewer", "review thread host", threads=["review"])
REVIEWER_B = peer("reviewer-b", "reviewer", "ops coordination", relation="thread_member", threads=["ops"])
REVIEWER_OLD = peer("reviewer-old", "reviewer", "review (until last week)", threads=[])
LAZULI_1 = peer("lazuli-1", "lazuli", "research chamber compute", threads=["chamber"])
LAZULI_2 = peer("lazuli-2", "lazuli", "research chamber data", threads=["chamber"])

RESEARCH = thread("research", "urn:onlyne:agent:lazuli", "open", "owner",
                  ["lazuli", "dpdp", "caerulea"], "107")
REVIEW = thread("review", "urn:onlyne:agent:reviewer-a", "open", "member",
                ["reviewer-a", "auditor", "lazuli"], "13")
REVIEW_AUDIT = thread("review", "urn:onlyne:agent:auditor", "open", "member",
                      ["auditor", "reviewer", "lazuli"], "21")
OPS = thread("ops", "urn:onlyne:agent:caerulea", "closed", "member",
             ["caerulea", "lazuli", "reviewer-b"], "44", peer_access=("post",))
CHAMBER = thread("chamber", "urn:onlyne:agent:lazuli-1", "open", "member",
                 ["lazuli-1", "lazuli-2", "lazuli"], "31")

C1 = context("c-1", "research", "urn:onlyne:agent:dpdp", "msg-01", "复核第 3 节实验结论", "running")
C2 = context("c-2", "research", "urn:onlyne:agent:caerulea", "msg-05", "重算第二组数据", "idle")
C7 = context("c-7", "research", "urn:onlyne:agent:caerulea", "msg-33", "讨论第 2 组数据", "closed")

HOST_MAIN = {
    "session": "main",
    "idle": True,
    "idle_source": "ctx.isIdle (includes prompt in flight)",
    "plan_mode": False,
    "plan_mode_source": "branch mode_change record",
    "todo_source": "state.readTodoMore(branch)",
    "todo_adapter_available": True,
    "todos": [{"text": "复核第 2 组数据", "state": "pending"}],
}

HOST_TODO_EVENT = {
    **HOST_MAIN,
    "todo_source": "omp-todo-more:read",
    "todo_adapter_available": False,
    "todos_public": {"goal": None, "openItems": []},
    "todo_ambiguity": "empty, all closed, or unavailable read the same",
}


def choice(instructions, options):
    return {"type": "choice", "instructions": instructions, "criteria": options}


ACTION_INSTRUCTIONS = "Which single action should this turn take?"
TARGET_INSTRUCTIONS = "Which peer is the delivery target?"
THREAD_INSTRUCTIONS = "Which thread carries the message?"
RESPONSE_INSTRUCTIONS = "What reply expectation does the sender express?"
EXECUTION_INSTRUCTIONS = "Which execution context should the receiving side use?"

ACTION = {
    "send": "deliver one message to one peer",
    "read": "inspect local onlyne state; deliver nothing",
    "local": "continue local work or accept a local plan; deliver nothing",
    "reject": "the turn cannot become one legal action",
}
TARGET_FALLBACK = {
    "none": "no peer is addressed and nothing is delivered",
    "unresolved": "a delivery is intended but the context does not determine one peer",
}
THREAD_FALLBACK = {"none": "no thread carries the message"}
RESPONSE = {
    "none": "the sender expects no reply",
    "async": "the sender continues and reads a reply later",
    "await": "the sender stops and waits for the reply before continuing",
}
EXECUTION = {
    "new": "the receiver starts a fresh execution context",
    "existing": "the receiver reuses a previously recorded execution context",
}

ACTION_TUNED = {
    "send": "deliver one message to the peer the intent addresses",
    "read": "inspect local onlyne state; deliver nothing",
    "local": "continue local work or accept a local proposal; deliver nothing, even when peers appear in the branch history",
    "reject": "the turn cannot become one legal action",
}
RESPONSE_TUNED = {
    "none": "the sender asks for no reply and states no question",
    "async": "the sender keeps working after this send and reads any reply later; a question without a stated pause belongs here",
    "await": "the sender pauses this work until the reply arrives; only an explicit stop-and-wait belongs here",
}
EXECUTION_TUNED = {
    "new": "start a fresh execution context; a prior context is reused only when the intent names or describes it",
    "existing": "reuse the execution context that the intent names or describes",
}
TARGET_TUNED_INSTRUCTIONS = (
    TARGET_INSTRUCTIONS
    + " Match the intent against each candidate's responsibility and the latest recorded assignment;"
      " a display name or an older turn does not override a newer assignment."
)


def peer_label(item):
    parts = [
        f"alias={item['alias']}",
        f"node={item['node_id']}",
        f"display={item['display']}",
        f"responsibility={item['responsibility']}",
        f"relation={item['relation']}",
        f"threads={','.join(item['threads']) or 'none'}",
        f"can_post={str(item['can_post']).lower()}",
        f"can_read={str(item['can_read']).lower()}",
    ]
    return "; ".join(parts)


def thread_label(item):
    return "; ".join([
        f"alias={item['alias']}",
        f"owner={item['owner']}",
        f"state={item['state']}",
        f"my_role={item['my_role']}",
        f"my_access={','.join(item['my_access'])}",
        f"peer_access={','.join(item['peer_access'])}",
    ])


def questions(case, revision):
    ask = case["ask"]
    candidates = case["candidates"]
    built = {}
    if "action" in ask:
        built["action"] = choice(ACTION_INSTRUCTIONS,
                                 dict(ACTION_TUNED if revision == "tuned" else ACTION))
    if "target" in ask:
        options = {item["alias"]: peer_label(item) for item in candidates["peers"]}
        options.update(TARGET_FALLBACK)
        built["target"] = choice(
            TARGET_TUNED_INSTRUCTIONS if revision == "tuned" else TARGET_INSTRUCTIONS, options)
    if "thread" in ask:
        options = {item["alias"]: thread_label(item) for item in candidates["threads"]}
        options.update(THREAD_FALLBACK)
        built["thread"] = choice(THREAD_INSTRUCTIONS, options)
    if "response" in ask:
        built["response"] = choice(RESPONSE_INSTRUCTIONS,
                                   dict(RESPONSE_TUNED if revision == "tuned" else RESPONSE))
    if "execution" in ask:
        built["execution"] = choice(EXECUTION_INSTRUCTIONS,
                                    dict(EXECUTION_TUNED if revision == "tuned" else EXECUTION))
    return built


CASES = []


def add(case):
    case.setdefault("point", "send")
    CASES.append(case)


# ---------------------------------------------------------------- dev 01
# Delegated review with a reply expected after the sender moves on.
add({
    "id": "sendx-01",
    "split": "dev",
    "provenance": "reconstruction of RFC 0003 review delegation; hand-authored, not a captured trace",
    "ask": ["target", "response"],
    "candidates": {"peers": [DPDP, CAERULEA, REVIEWER], "threads": [RESEARCH, REVIEW]},
    "profiles": {
        "minimal": state(
            caseless("让 reviewer 看一下第 3 节，我先做别的，弄完告诉我", "第 3 节的实验结论需要复核。",
                     {"to": "reviewer", "thread": "research"}),
            [DPDP, CAERULEA, REVIEWER], [RESEARCH, REVIEW],
            conversation=[turn(41, "user", "2026-10-24T02:10:00Z", "让 reviewer 看一下第 3 节，我先做别的，弄完告诉我。")],
        ),
        "focused": state(
            caseless("让 reviewer 看一下第 3 节，我先做别的，弄完告诉我", "第 3 节的实验结论需要复核。",
                     {"to": "reviewer", "thread": "research"}),
            [DPDP, CAERULEA, REVIEWER], [RESEARCH, REVIEW],
            conversation=[
                turn(40, "user", "2026-10-24T02:08:00Z", "第 3 节我整理完了，找个人复核。"),
                turn(41, "user", "2026-10-24T02:10:00Z", "让 reviewer 看一下第 3 节，我先做别的，弄完告诉我。"),
                turn(41, "assistant", "2026-10-24T02:10:20Z", "我把第 3 节发到 research，请 reviewer 复核。"),
            ],
            waits=[],
        ),
        "expanded": state(
            caseless("让 reviewer 看一下第 3 节，我先做别的，弄完告诉我", "第 3 节的实验结论需要复核。",
                     {"to": "reviewer", "thread": "research"}),
            [DPDP, CAERULEA, REVIEWER], [RESEARCH, REVIEW],
            conversation=[
                turn(30, "user", "2026-10-23T09:00:00Z", "第 2 组数据先不动，等机器空出来。"),
                turn(31, "assistant", "2026-10-23T09:01:00Z", "好，第 2 组留到明天。"),
                turn(40, "user", "2026-10-24T02:08:00Z", "第 3 节我整理完了，找个人复核。"),
                turn(41, "user", "2026-10-24T02:10:00Z", "让 reviewer 看一下第 3 节，我先做别的，弄完告诉我。"),
                turn(41, "assistant", "2026-10-24T02:10:20Z", "我把第 3 节发到 research，请 reviewer 复核。"),
            ],
            notes=[note("2026-10-20T06:00:00Z", "reviewer 上个月负责发布窗口，本周不排发布。")],
            host=HOST_MAIN,
        ),
    },
    "expected": {"target": "reviewer", "response": "async"},
})


# ---------------------------------------------------------------- dev 02
# "把这个也给他": the antecedent comes from the assistant's previous turn.
add({
    "id": "sendx-02",
    "split": "dev",
    "provenance": "reconstruction of the user's '把这个也给他' delegation pattern; hand-authored, not a captured trace",
    "ask": ["action", "target"],
    "candidates": {"peers": [DPDP, CAERULEA, REVIEWER], "threads": [RESEARCH]},
    "profiles": {
        "minimal": state(
            caseless("把这个也给他", "同一份数据清单。", {}),
            [DPDP, CAERULEA, REVIEWER], [RESEARCH],
            conversation=[turn(52, "assistant", "2026-10-24T03:00:00Z",
                               "已经把你说的第 3 节结论发给 dpdp 了。")],
        ),
        "focused": state(
            caseless("把这个也给他", "同一份数据清单。", {}),
            [DPDP, CAERULEA, REVIEWER], [RESEARCH],
            conversation=[
                turn(50, "user", "2026-10-24T02:56:00Z", "第 3 节结论整理好了吗？"),
                turn(51, "assistant", "2026-10-24T02:58:00Z", "整理好了。"),
                turn(52, "assistant", "2026-10-24T03:00:00Z", "已经把你说的第 3 节结论发给 dpdp 了。"),
                turn(53, "user", "2026-10-24T03:01:00Z", "把这个也给他。"),
            ],
        ),
        "expanded": state(
            caseless("把这个也给他", "同一份数据清单。", {}),
            [DPDP, CAERULEA, REVIEWER], [RESEARCH],
            conversation=[
                turn(30, "user", "2026-10-23T08:00:00Z", "caerulea 那边的数据晚点再说。"),
                turn(50, "user", "2026-10-24T02:56:00Z", "第 3 节结论整理好了吗？"),
                turn(51, "assistant", "2026-10-24T02:58:00Z", "整理好了。"),
                turn(52, "assistant", "2026-10-24T03:00:00Z", "已经把你说的第 3 节结论发给 dpdp 了。"),
                turn(53, "user", "2026-10-24T03:01:00Z", "把这个也给他。"),
            ],
            notes=[note("2026-10-22T04:00:00Z", "数据清单模板放在共享盘，谁要都可以给。")],
            host=HOST_MAIN,
        ),
    },
    "expected": {"action": "send", "target": "dpdp"},
})


# ---------------------------------------------------------------- dev 03
# Same pronoun, different antecedent: caerulea.
add({
    "id": "sendx-03",
    "split": "dev",
    "provenance": "counterfactual to send-02 with the assistant turn naming caerulea; hand-authored, not a captured trace",
    "ask": ["action", "target"],
    "candidates": {"peers": [DPDP, CAERULEA, REVIEWER], "threads": [RESEARCH]},
    "profiles": {
        "minimal": state(
            caseless("把这个也给她", "第二组数据的结论。", {}),
            [DPDP, CAERULEA, REVIEWER], [RESEARCH],
            conversation=[turn(61, "assistant", "2026-10-24T03:20:00Z",
                               "caerulea 问起第二组数据的结论。")],
        ),
        "focused": state(
            caseless("把这个也给她", "第二组数据的结论。", {}),
            [DPDP, CAERULEA, REVIEWER], [RESEARCH],
            conversation=[
                turn(60, "user", "2026-10-24T03:16:00Z", "caerulea 那边在催第二组数据。"),
                turn(61, "assistant", "2026-10-24T03:20:00Z", "caerulea 问起第二组数据的结论。"),
                turn(62, "user", "2026-10-24T03:21:00Z", "把这个也给她。"),
            ],
        ),
        "expanded": state(
            caseless("把这个也给她", "第二组数据的结论。", {}),
            [DPDP, CAERULEA, REVIEWER], [RESEARCH],
            conversation=[
                turn(20, "user", "2026-10-22T07:00:00Z", "先给 dpdp 一份第 2 组数据。"),
                turn(60, "user", "2026-10-24T03:16:00Z", "caerulea 那边在催第二组数据。"),
                turn(61, "assistant", "2026-10-24T03:20:00Z", "caerulea 问起第二组数据的结论。"),
                turn(62, "user", "2026-10-24T03:21:00Z", "把这个也给她。"),
            ],
            host={**HOST_MAIN, "idle": False, "idle_source": "ctx.isIdle (prompt in flight)"},
        ),
    },
    "expected": {"action": "send", "target": "caerulea"},
})


# ---------------------------------------------------------------- dev 04
# The sender explicitly stops and waits.
add({
    "id": "sendx-04",
    "split": "dev",
    "provenance": "reconstruction of RFC 0005 await example with the user's explicit wait wording; hand-authored, not a captured trace",
    "ask": ["target", "response"],
    "candidates": {"peers": [CAERULEA, DPDP, REVIEWER], "threads": [RESEARCH]},
    "profiles": {
        "minimal": state(
            caseless("把复核结果发给 caerulea，我在这里等她确认后再继续", "数据已重算，请确认。",
                     {"to": "caerulea", "thread": "research"}),
            [CAERULEA, DPDP, REVIEWER], [RESEARCH],
            conversation=[turn(72, "user", "2026-10-24T04:00:00Z",
                               "把复核结果发给 caerulea，我在这里等她确认后再继续。")],
        ),
        "focused": state(
            caseless("把复核结果发给 caerulea，我在这里等她确认后再继续", "数据已重算，请确认。",
                     {"to": "caerulea", "thread": "research"}),
            [CAERULEA, DPDP, REVIEWER], [RESEARCH],
            conversation=[
                turn(70, "user", "2026-10-24T03:50:00Z", "第二组数据重算完了。"),
                turn(71, "assistant", "2026-10-24T03:55:00Z", "我把结果发到 research，请 caerulea 确认。"),
                turn(72, "user", "2026-10-24T04:00:00Z",
                     "把复核结果发给 caerulea，我在这里等她确认后再继续。"),
            ],
            contexts=[C2],
            waits=[],
        ),
        "expanded": state(
            caseless("把复核结果发给 caerulea，我在这里等她确认后再继续", "数据已重算，请确认。",
                     {"to": "caerulea", "thread": "research"}),
            [CAERULEA, DPDP, REVIEWER], [RESEARCH],
            conversation=[
                turn(40, "user", "2026-10-23T10:00:00Z", "先把文档骨架搭起来。"),
                turn(41, "assistant", "2026-10-23T10:02:00Z", "骨架搭好了，等数据。"),
                turn(72, "user", "2026-10-24T04:00:00Z",
                     "把复核结果发给 caerulea，我在这里等她确认后再继续。"),
            ],
            contexts=[C2, C1],
            host={**HOST_MAIN, "todos": [{"text": "等 caerulea 确认第二组数据", "state": "in_progress"}]},
        ),
    },
    "expected": {"target": "caerulea", "response": "await"},
})


# ---------------------------------------------------------------- dev 05
# A question, but the sender keeps working: async.
add({
    "id": "sendx-05",
    "split": "dev",
    "provenance": "reconstruction of RFC 0005 async example from the user's continue-while-asking workflow; hand-authored, not a captured trace",
    "ask": ["response", "target"],
    "candidates": {"peers": [REVIEWER, DPDP, CAERULEA], "threads": [REVIEW]},
    "profiles": {
        "minimal": state(
            caseless("问一下 reviewer 第 2 组数据核对完没有，我继续改文档", "第 2 组数据核对完了吗？",
                     {"to": "reviewer"}),
            [REVIEWER, DPDP, CAERULEA], [REVIEW],
            conversation=[turn(82, "user", "2026-10-24T04:20:00Z",
                               "问一下 reviewer 第 2 组数据核对完没有，我继续改文档。")],
        ),
        "focused": state(
            caseless("问一下 reviewer 第 2 组数据核对完没有，我继续改文档", "第 2 组数据核对完了吗？",
                     {"to": "reviewer"}),
            [REVIEWER, DPDP, CAERULEA], [REVIEW],
            conversation=[
                turn(80, "user", "2026-10-24T04:16:00Z", "我先去改文档。"),
                turn(81, "assistant", "2026-10-24T04:18:00Z", "那我问一下 reviewer 第 2 组数据。"),
                turn(82, "user", "2026-10-24T04:20:00Z",
                     "问一下 reviewer 第 2 组数据核对完没有，我继续改文档。"),
            ],
        ),
        "expanded": state(
            caseless("问一下 reviewer 第 2 组数据核对完没有，我继续改文档", "第 2 组数据核对完了吗？",
                     {"to": "reviewer"}),
            [REVIEWER, DPDP, CAERULEA], [REVIEW],
            conversation=[
                turn(60, "user", "2026-10-23T11:00:00Z", "文档今天要出初稿。"),
                turn(82, "user", "2026-10-24T04:20:00Z",
                     "问一下 reviewer 第 2 组数据核对完没有，我继续改文档。"),
            ],
            notes=[note("2026-10-23T12:00:00Z", "reviewer 周四下午不在。")],
            host={**HOST_MAIN, "idle": True,
                  "todos": [{"text": "改文档初稿", "state": "in_progress"}]},
        ),
    },
    "expected": {"response": "async", "target": "reviewer"},
})


# ---------------------------------------------------------------- dev 06
# A notice that asks for no reply.
add({
    "id": "sendx-06",
    "split": "dev",
    "provenance": "reconstruction of a one-way notice in the user's review workflow; hand-authored, not a captured trace",
    "ask": ["target", "response"],
    "candidates": {"peers": [REVIEWER, DPDP, CAERULEA], "threads": [REVIEW]},
    "profiles": {
        "minimal": state(
            caseless("通知 reviewer 评审改到周五，不用回", "评审时间改到周五。", {"to": "reviewer"}),
            [REVIEWER, DPDP, CAERULEA], [REVIEW],
            conversation=[turn(92, "user", "2026-10-24T05:00:00Z", "通知 reviewer 评审改到周五，不用回。")],
        ),
        "focused": state(
            caseless("通知 reviewer 评审改到周五，不用回", "评审时间改到周五。", {"to": "reviewer"}),
            [REVIEWER, DPDP, CAERULEA], [REVIEW],
            conversation=[
                turn(90, "user", "2026-10-24T04:55:00Z", "评审挪到周五，跟 reviewer 说一声。"),
                turn(91, "assistant", "2026-10-24T04:57:00Z", "好，只发通知，不问他回不回。"),
                turn(92, "user", "2026-10-24T05:00:00Z", "通知 reviewer 评审改到周五，不用回。"),
            ],
        ),
        "expanded": state(
            caseless("通知 reviewer 评审改到周五，不用回", "评审时间改到周五。", {"to": "reviewer"}),
            [REVIEWER, DPDP, CAERULEA], [REVIEW],
            conversation=[
                turn(70, "user", "2026-10-22T09:00:00Z", "reviewer 最近在忙发布。"),
                turn(92, "user", "2026-10-24T05:00:00Z", "通知 reviewer 评审改到周五，不用回。"),
            ],
            notes=[note("2026-10-22T10:00:00Z", "评审日历周三更新。")],
            host=HOST_MAIN,
        ),
    },
    "expected": {"target": "reviewer", "response": "none"},
})


# ---------------------------------------------------------------- dev 07
# The review responsibility moved from reviewer to auditor.
add({
    "id": "sendx-07",
    "split": "dev",
    "provenance": "reconstruction of the user's review-role handover; hand-authored, not a captured trace",
    "ask": ["target", "action"],
    "candidates": {"peers": [REVIEWER, AUDITOR, DPDP], "threads": [REVIEW_AUDIT]},
    "profiles": {
        "minimal": state(
            caseless("把这份结论文档发给评审", "第 3 节结论文档已整理完。", {}),
            [REVIEWER, AUDITOR, DPDP], [REVIEW_AUDIT],
            conversation=[turn(102, "assistant", "2026-10-24T05:30:00Z",
                               "评审已转给 auditor，reviewer 现在只管发布。")],
        ),
        "focused": state(
            caseless("把这份结论文档发给评审", "第 3 节结论文档已整理完。", {}),
            [REVIEWER, AUDITOR, DPDP], [REVIEW_AUDIT],
            conversation=[
                turn(100, "user", "2026-10-20T06:00:00Z", "reviewer 负责评审。"),
                turn(102, "user", "2026-10-24T05:30:00Z", "评审转给 auditor 了，reviewer 现在只管发布。"),
                turn(103, "user", "2026-10-24T05:32:00Z", "把这份结论文档发给评审。"),
            ],
        ),
        "expanded": state(
            caseless("把这份结论文档发给评审", "第 3 节结论文档已整理完。", {}),
            [REVIEWER, AUDITOR, DPDP], [REVIEW_AUDIT],
            conversation=[
                turn(100, "user", "2026-10-20T06:00:00Z", "reviewer 负责评审。"),
                turn(101, "assistant", "2026-10-20T06:01:00Z", "记下了。"),
                turn(102, "user", "2026-10-24T05:30:00Z", "评审转给 auditor 了，reviewer 现在只管发布。"),
                turn(103, "user", "2026-10-24T05:32:00Z", "把这份结论文档发给评审。"),
            ],
            notes=[note("2026-10-20T07:00:00Z", "reviewer 负责评审。", active=False,
                       reset_boundary="2026-10-24T05:00:00Z")],
            history_modes={
                "compressed_note": {
                    "authority": "note",
                    "source": "compaction summary",
                    "at": "2026-10-24T05:29:00Z",
                    "text": "评审负责人是 auditor；reviewer 只做发布。",
                },
                "recovered_source": {
                    "source": "history recovery",
                    "turns": [
                        turn(100, "user", "2026-10-20T06:00:00Z", "reviewer 负责评审。"),
                        turn(102, "user", "2026-10-24T05:30:00Z", "评审转给 auditor 了，reviewer 现在只管发布。"),
                    ],
                },
            },
            host=HOST_MAIN,
        ),
    },
    "expected": {"target": "auditor", "action": "send"},
})


# ---------------------------------------------------------------- dev 08
# Two peers share the display name reviewer; their hosts differ.
add({
    "id": "sendx-08",
    "split": "dev",
    "provenance": "reconstruction of the user's duplicate-display-name cluster; hand-authored, not a captured trace",
    "ask": ["target", "thread"],
    "candidates": {"peers": [REVIEWER_A, REVIEWER_B, DPDP], "threads": [REVIEW, OPS]},
    "profiles": {
        "minimal": state(
            caseless("让 review thread 的负责人看一下这份结论", "第 3 节结论请复核。", {}),
            [REVIEWER_A, REVIEWER_B, DPDP], [REVIEW, OPS],
            conversation=[turn(112, "user", "2026-10-24T06:00:00Z",
                               "让 review thread 的负责人看一下这份结论。")],
        ),
        "focused": state(
            caseless("让 review thread 的负责人看一下这份结论", "第 3 节结论请复核。", {}),
            [REVIEWER_A, REVIEWER_B, DPDP], [REVIEW, OPS],
            conversation=[
                turn(110, "user", "2026-10-24T05:50:00Z", "review 和 ops 的成员表贴到 onlyne 了吗？"),
                turn(111, "assistant", "2026-10-24T05:52:00Z", "贴了，两个名字都叫 reviewer，按 thread 分开。"),
                turn(112, "user", "2026-10-24T06:00:00Z", "让 review thread 的负责人看一下这份结论。"),
            ],
        ),
        "expanded": state(
            caseless("让 review thread 的负责人看一下这份结论", "第 3 节结论请复核。", {}),
            [REVIEWER_A, REVIEWER_B, DPDP], [REVIEW, OPS],
            conversation=[
                turn(90, "user", "2026-10-23T02:00:00Z", "ops 那边先别动。"),
                turn(112, "user", "2026-10-24T06:00:00Z", "让 review thread 的负责人看一下这份结论。"),
            ],
            notes=[note("2026-10-23T03:00:00Z", "两个 reviewer 的显示名相同，靠 thread 区分。")],
            host=HOST_MAIN,
        ),
    },
    "expected": {"target": "reviewer-a", "thread": "review"},
})


# ---------------------------------------------------------------- dev 09
# A read request while every peer is reachable.
add({
    "id": "sendx-09",
    "split": "dev",
    "provenance": "reconstruction of the user's read-only thread inspection; hand-authored, not a captured trace",
    "ask": ["action", "thread"],
    "candidates": {"peers": [DPDP, CAERULEA, REVIEWER], "threads": [RESEARCH, OPS]},
    "profiles": {
        "minimal": state(
            caseless("看看 research 里最近聊了什么，不用回复", "",
                     {"thread": "research", "attention": "thread"}),
            [DPDP, CAERULEA, REVIEWER], [RESEARCH, OPS],
            conversation=[turn(122, "user", "2026-10-24T06:30:00Z",
                               "看看 research 里最近聊了什么，不用回复。")],
        ),
        "focused": state(
            caseless("看看 research 里最近聊了什么，不用回复", "",
                     {"thread": "research", "attention": "thread"}),
            [DPDP, CAERULEA, REVIEWER], [RESEARCH, OPS],
            conversation=[
                turn(120, "user", "2026-10-24T06:25:00Z", "我先看看 research 的动静。"),
                turn(121, "assistant", "2026-10-24T06:27:00Z", "只看不发言，也不打扰他们。"),
                turn(122, "user", "2026-10-24T06:30:00Z", "看看 research 里最近聊了什么，不用回复。"),
            ],
        ),
        "expanded": state(
            caseless("看看 research 里最近聊了什么，不用回复", "",
                     {"thread": "research", "attention": "thread"}),
            [DPDP, CAERULEA, REVIEWER], [RESEARCH, OPS],
            conversation=[
                turn(80, "user", "2026-10-22T12:00:00Z", "ops 已经关了，别往里发。"),
                turn(122, "user", "2026-10-24T06:30:00Z", "看看 research 里最近聊了什么，不用回复。"),
            ],
            notes=[note("2026-10-23T13:00:00Z", "dpdp 昨天在 research 问了第 3 节。")],
            host={**HOST_TODO_EVENT, "idle": False, "idle_source": "ctx.isIdle (prompt in flight)"},
        ),
    },
    "expected": {"action": "read", "thread": "research"},
})


# ---------------------------------------------------------------- dev 10
# The quoted approval of a read-only local plan.
add({
    "id": "sendx-10",
    "split": "dev",
    "provenance": "reconstruction of the user's '也行，那就先做成只读' approval during plugin design; hand-authored, not a captured trace",
    "ask": ["action"],
    "candidates": {"peers": [DPDP, CAERULEA, REVIEWER], "threads": [RESEARCH]},
    "profiles": {
        "minimal": state(
            caseless("也行，那就先做成只读", "", {}),
            [DPDP, CAERULEA, REVIEWER], [RESEARCH],
            conversation=[
                turn(130, "assistant", "2026-10-24T07:00:00Z",
                     "插件这边只注册只读的 onlyne:// 路径，写入能力先不做。"),
                turn(131, "user", "2026-10-24T07:01:00Z", "也行，那就先做成只读。"),
            ],
        ),
        "focused": state(
            caseless("也行，那就先做成只读", "", {}),
            [DPDP, CAERULEA, REVIEWER], [RESEARCH],
            conversation=[
                turn(128, "user", "2026-10-24T06:50:00Z", "插件接口这块怎么接？"),
                turn(129, "assistant", "2026-10-24T06:55:00Z",
                     "OMP 的 internal path 没有写入回执，先只读最稳。"),
                turn(130, "assistant", "2026-10-24T07:00:00Z",
                     "插件这边只注册只读的 onlyne:// 路径，写入能力先不做。"),
                turn(131, "user", "2026-10-24T07:01:00Z", "也行，那就先做成只读。"),
                turn(132, "peer", "2026-10-24T07:02:00Z", "接口什么时候好？", from_="urn:onlyne:agent:dpdp"),
            ],
        ),
        "expanded": state(
            caseless("也行，那就先做成只读", "", {}),
            [DPDP, CAERULEA, REVIEWER], [RESEARCH],
            conversation=[
                turn(120, "user", "2026-10-23T08:00:00Z", "先把消息投递跑通。"),
                turn(130, "assistant", "2026-10-24T07:00:00Z",
                     "插件这边只注册只读的 onlyne:// 路径，写入能力先不做。"),
                turn(131, "user", "2026-10-24T07:01:00Z", "也行，那就先做成只读。"),
                turn(132, "peer", "2026-10-24T07:02:00Z", "接口什么时候好？", from_="urn:onlyne:agent:dpdp"),
            ],
            notes=[note("2026-10-23T09:00:00Z", "internal path 的写入能力还没接。")],
            host=HOST_MAIN,
        ),
    },
    "expected": {"action": "local"},
})


# ---------------------------------------------------------------- dev 11
# Same approval wording, now with an explicit recipient.
add({
    "id": "sendx-11",
    "split": "dev",
    "provenance": "counterfactual to send-10 that adds an explicit recipient; hand-authored, not a captured trace",
    "ask": ["action", "target"],
    "candidates": {"peers": [DPDP, CAERULEA, REVIEWER], "threads": [RESEARCH]},
    "profiles": {
        "minimal": state(
            caseless("那就把只读接口发给 dpdp 看看", "只读接口已经能列出 threads 和 inbox。", {"to": "dpdp"}),
            [DPDP, CAERULEA, REVIEWER], [RESEARCH],
            conversation=[turn(141, "user", "2026-10-24T07:10:00Z",
                               "那就把只读接口发给 dpdp 看看。")],
        ),
        "focused": state(
            caseless("那就把只读接口发给 dpdp 看看", "只读接口已经能列出 threads 和 inbox。", {"to": "dpdp"}),
            [DPDP, CAERULEA, REVIEWER], [RESEARCH],
            conversation=[
                turn(130, "assistant", "2026-10-24T07:00:00Z",
                     "插件这边只注册只读的 onlyne:// 路径，写入能力先不做。"),
                turn(131, "user", "2026-10-24T07:01:00Z", "也行，那就先做成只读。"),
                turn(140, "assistant", "2026-10-24T07:08:00Z", "只读路径能列出 threads 和 inbox 了。"),
                turn(141, "user", "2026-10-24T07:10:00Z", "那就把只读接口发给 dpdp 看看。"),
            ],
        ),
        "expanded": state(
            caseless("那就把只读接口发给 dpdp 看看", "只读接口已经能列出 threads 和 inbox。", {"to": "dpdp"}),
            [DPDP, CAERULEA, REVIEWER], [RESEARCH],
            conversation=[
                turn(120, "user", "2026-10-23T08:00:00Z", "先把消息投递跑通。"),
                turn(141, "user", "2026-10-24T07:10:00Z", "那就把只读接口发给 dpdp 看看。"),
            ],
            notes=[note("2026-10-23T09:00:00Z", "dpdp 负责接入侧验证。")],
            host=HOST_MAIN,
        ),
    },
    "expected": {"action": "send", "target": "dpdp"},
})


# ---------------------------------------------------------------- dev 12
# Only a thread is named; the thread is not a peer.
add({
    "id": "sendx-12",
    "split": "dev",
    "provenance": "reconstruction of the user's '把结论发到 research 里' wording; hand-authored, not a captured trace",
    "ask": ["target", "thread"],
    "candidates": {"peers": [DPDP, CAERULEA, REVIEWER], "threads": [RESEARCH]},
    "profiles": {
        "minimal": state(
            caseless("把结论发到 research 里", "第 3 节结论如下。", {"thread": "research"}),
            [DPDP, CAERULEA, REVIEWER], [RESEARCH],
            conversation=[turn(151, "user", "2026-10-24T07:30:00Z", "把结论发到 research 里。")],
        ),
        "focused": state(
            caseless("把结论发到 research 里", "第 3 节结论如下。", {"thread": "research"}),
            [DPDP, CAERULEA, REVIEWER], [RESEARCH],
            conversation=[
                turn(149, "peer", "2026-10-24T07:26:00Z", "结论好了吗？", from_="urn:onlyne:agent:dpdp"),
                turn(150, "assistant", "2026-10-24T07:28:00Z", "research 里有 dpdp 和 caerulea。"),
                turn(151, "user", "2026-10-24T07:30:00Z", "把结论发到 research 里。"),
            ],
        ),
        "expanded": state(
            caseless("把结论发到 research 里", "第 3 节结论如下。", {"thread": "research"}),
            [DPDP, CAERULEA, REVIEWER], [RESEARCH],
            conversation=[
                turn(130, "user", "2026-10-23T08:00:00Z", "research 的成员先不动。"),
                turn(149, "peer", "2026-10-24T07:26:00Z", "结论好了吗？", from_="urn:onlyne:agent:dpdp"),
                turn(151, "user", "2026-10-24T07:30:00Z", "把结论发到 research 里。"),
            ],
            notes=[note("2026-10-23T09:00:00Z", "caerulea 也在 research。")],
            host=HOST_MAIN,
        ),
    },
    "expected": {"target": "unresolved", "thread": "research"},
})


# ---------------------------------------------------------------- dev 13
# A thread plus an explicit mention resolves the peer.
add({
    "id": "sendx-13",
    "split": "dev",
    "provenance": "counterfactual to send-12 with an explicit mention; hand-authored, not a captured trace",
    "ask": ["target", "thread"],
    "candidates": {"peers": [DPDP, CAERULEA, REVIEWER], "threads": [RESEARCH]},
    "profiles": {
        "minimal": state(
            caseless("在 research 里 @dpdp，把结论同步一下", "第 3 节结论如下。",
                     {"thread": "research", "mentions": ["dpdp"]}),
            [DPDP, CAERULEA, REVIEWER], [RESEARCH],
            conversation=[turn(161, "user", "2026-10-24T07:40:00Z",
                               "在 research 里 @dpdp，把结论同步一下。")],
        ),
        "focused": state(
            caseless("在 research 里 @dpdp，把结论同步一下", "第 3 节结论如下。",
                     {"thread": "research", "mentions": ["dpdp"]}),
            [DPDP, CAERULEA, REVIEWER], [RESEARCH],
            conversation=[
                turn(159, "peer", "2026-10-24T07:36:00Z", "结论好了吗？", from_="urn:onlyne:agent:dpdp"),
                turn(160, "assistant", "2026-10-24T07:38:00Z", "我发到 research，@ 一下 dpdp。"),
                turn(161, "user", "2026-10-24T07:40:00Z", "在 research 里 @dpdp，把结论同步一下。"),
            ],
        ),
        "expanded": state(
            caseless("在 research 里 @dpdp，把结论同步一下", "第 3 节结论如下。",
                     {"thread": "research", "mentions": ["dpdp"]}),
            [DPDP, CAERULEA, REVIEWER], [RESEARCH],
            conversation=[
                turn(150, "user", "2026-10-24T07:20:00Z", "caerulea 那边先不用发。"),
                turn(161, "user", "2026-10-24T07:40:00Z", "在 research 里 @dpdp，把结论同步一下。"),
            ],
            notes=[note("2026-10-23T10:00:00Z", "research 的消息默认全组可见。")],
            host=HOST_MAIN,
        ),
    },
    "expected": {"target": "dpdp", "thread": "research"},
})


# ---------------------------------------------------------------- dev 14
# No context is named, although a matching context exists: new.
add({
    "id": "sendx-14",
    "split": "dev",
    "provenance": "reconstruction of RFC 0005 new-context rule with a running context as lure; hand-authored, not a captured trace",
    "ask": ["execution", "response"],
    "candidates": {"peers": [DPDP, CAERULEA, REVIEWER], "threads": [RESEARCH]},
    "profiles": {
        "minimal": state(
            caseless("把结论发给 dpdp，不用等回复", "第 3 节结论如下。", {"to": "dpdp"}),
            [DPDP, CAERULEA, REVIEWER], [RESEARCH],
            contexts=[C1, C2],
            conversation=[turn(171, "user", "2026-10-24T08:00:00Z",
                               "把结论发给 dpdp，不用等回复。")],
        ),
        "focused": state(
            caseless("把结论发给 dpdp，不用等回复", "第 3 节结论如下。", {"to": "dpdp"}),
            [DPDP, CAERULEA, REVIEWER], [RESEARCH],
            contexts=[C1, C2],
            conversation=[
                turn(169, "user", "2026-10-24T07:55:00Z", "dpdp 那边还在复核第 3 节吗？"),
                turn(170, "assistant", "2026-10-24T07:57:00Z", "c-1 还开着，就在 research。"),
                turn(171, "user", "2026-10-24T08:00:00Z", "把结论发给 dpdp，不用等回复。"),
            ],
        ),
        "expanded": state(
            caseless("把结论发给 dpdp，不用等回复", "第 3 节结论如下。", {"to": "dpdp"}),
            [DPDP, CAERULEA, REVIEWER], [RESEARCH],
            contexts=[C1, C2],
            conversation=[
                turn(150, "user", "2026-10-24T07:00:00Z", "第二组数据的会先不开。"),
                turn(169, "user", "2026-10-24T07:55:00Z", "dpdp 那边还在复核第 3 节吗？"),
                turn(170, "assistant", "2026-10-24T07:57:00Z", "c-1 还开着，就在 research。"),
                turn(171, "user", "2026-10-24T08:00:00Z", "把结论发给 dpdp，不用等回复。"),
            ],
            notes=[note("2026-10-24T07:58:00Z", "c-1 是复核第 3 节的上下文。")],
            host=HOST_MAIN,
        ),
    },
    "expected": {"execution": "new", "response": "none"},
})


# ---------------------------------------------------------------- dev 15
# The intent names c-1: existing.
add({
    "id": "sendx-15",
    "split": "dev",
    "provenance": "counterfactual to send-14 with an explicit context alias; hand-authored, not a captured trace",
    "ask": ["execution", "response"],
    "candidates": {"peers": [DPDP, CAERULEA, REVIEWER], "threads": [RESEARCH]},
    "profiles": {
        "minimal": state(
            caseless("接着 c-1 让 dpdp 复核第 3 节，我这边继续写文档", "请继续复核第 3 节。",
                     {"to": "dpdp", "thread": "research", "context": "c-1"}),
            [DPDP, CAERULEA, REVIEWER], [RESEARCH],
            contexts=[C1, C2],
            conversation=[turn(181, "user", "2026-10-24T08:20:00Z",
                               "接着 c-1 让 dpdp 复核第 3 节，我这边继续写文档。")],
        ),
        "focused": state(
            caseless("接着 c-1 让 dpdp 复核第 3 节，我这边继续写文档", "请继续复核第 3 节。",
                     {"to": "dpdp", "thread": "research", "context": "c-1"}),
            [DPDP, CAERULEA, REVIEWER], [RESEARCH],
            contexts=[C1, C2],
            conversation=[
                turn(179, "user", "2026-10-24T08:15:00Z", "dpdp 复核到哪了？"),
                turn(180, "assistant", "2026-10-24T08:17:00Z", "c-1 还在跑，第 3 节复核。"),
                turn(181, "user", "2026-10-24T08:20:00Z",
                     "接着 c-1 让 dpdp 复核第 3 节，我这边继续写文档。"),
            ],
        ),
        "expanded": state(
            caseless("接着 c-1 让 dpdp 复核第 3 节，我这边继续写文档", "请继续复核第 3 节。",
                     {"to": "dpdp", "thread": "research", "context": "c-1"}),
            [DPDP, CAERULEA, REVIEWER], [RESEARCH],
            contexts=[C1, C2],
            conversation=[
                turn(160, "user", "2026-10-24T07:40:00Z", "文档今天要交初稿。"),
                turn(181, "user", "2026-10-24T08:20:00Z",
                     "接着 c-1 让 dpdp 复核第 3 节，我这边继续写文档。"),
            ],
            notes=[note("2026-10-24T08:18:00Z", "c-1 的会话文件在 /sessions/c-1.jsonl。")],
            host={**HOST_MAIN, "todos": [{"text": "写文档初稿", "state": "in_progress"}]},
        ),
    },
    "expected": {"execution": "existing", "response": "async"},
})


# ---------------------------------------------------------------- dev 16
# The target comes from the assistant's commitment, accepted by the user.
add({
    "id": "sendx-16",
    "split": "dev",
    "provenance": "reconstruction of the user accepting an assistant commitment ('嗯，发吧'); hand-authored, not a captured trace",
    "ask": ["action", "target"],
    "candidates": {"peers": [CAERULEA, DPDP, REVIEWER], "threads": [RESEARCH]},
    "profiles": {
        "minimal": state(
            caseless("嗯，发吧", "第 2 组数据的结论。", {}),
            [CAERULEA, DPDP, REVIEWER], [RESEARCH],
            conversation=[turn(192, "assistant", "2026-10-24T08:50:00Z",
                               "我把第 2 组数据的结论整理好发给 caerulea。")],
        ),
        "focused": state(
            caseless("嗯，发吧", "第 2 组数据的结论。", {}),
            [CAERULEA, DPDP, REVIEWER], [RESEARCH],
            conversation=[
                turn(190, "user", "2026-10-24T08:46:00Z", "第二组数据重算完了吗？"),
                turn(191, "assistant", "2026-10-24T08:48:00Z", "算完了，我把结论整理好发给 caerulea。"),
                turn(192, "user", "2026-10-24T08:50:00Z", "嗯，发吧。"),
            ],
        ),
        "expanded": state(
            caseless("嗯，发吧", "第 2 组数据的结论。", {}),
            [CAERULEA, DPDP, REVIEWER], [RESEARCH],
            conversation=[
                turn(170, "user", "2026-10-24T08:30:00Z", "dpdp 要的那份清单先放一放。"),
                turn(190, "user", "2026-10-24T08:46:00Z", "第二组数据重算完了吗？"),
                turn(191, "assistant", "2026-10-24T08:48:00Z", "算完了，我把结论整理好发给 caerulea。"),
                turn(192, "user", "2026-10-24T08:50:00Z", "嗯，发吧。"),
            ],
            notes=[note("2026-10-24T08:40:00Z", "caerulea 在等第 2 组数据。")],
            host=HOST_MAIN,
        ),
    },
    "expected": {"action": "send", "target": "caerulea"},
})


# ---------------------------------------------------------------- holdout 01
add({
    "id": "sendx-h01",
    "split": "holdout",
    "provenance": "reconstruction of an ambiguous '把这个也给他' with two antecedents in one turn; hand-authored, not a captured trace",
    "ask": ["action", "target"],
    "candidates": {"peers": [DPDP, CAERULEA, REVIEWER], "threads": [RESEARCH]},
    "profiles": {
        "minimal": state(
            caseless("把这个也给他", "同一份清单。", {}),
            [DPDP, CAERULEA, REVIEWER], [RESEARCH],
            conversation=[turn(202, "assistant", "2026-10-24T09:10:00Z",
                               "dpdp 要一份数据清单，caerulea 也要同一份。")],
        ),
        "focused": state(
            caseless("把这个也给他", "同一份清单。", {}),
            [DPDP, CAERULEA, REVIEWER], [RESEARCH],
            conversation=[
                turn(200, "user", "2026-10-24T09:00:00Z", "两个人都来要数据清单。"),
                turn(201, "assistant", "2026-10-24T09:05:00Z", "我整理一份。"),
                turn(202, "assistant", "2026-10-24T09:10:00Z", "dpdp 要一份数据清单，caerulea 也要同一份。"),
                turn(203, "user", "2026-10-24T09:11:00Z", "把这个也给他。"),
            ],
        ),
        "expanded": state(
            caseless("把这个也给他", "同一份清单。", {}),
            [DPDP, CAERULEA, REVIEWER], [RESEARCH],
            conversation=[
                turn(180, "user", "2026-10-23T09:00:00Z", "清单模板别外发。"),
                turn(200, "user", "2026-10-24T09:00:00Z", "两个人都来要数据清单。"),
                turn(202, "assistant", "2026-10-24T09:10:00Z", "dpdp 要一份数据清单，caerulea 也要同一份。"),
                turn(203, "user", "2026-10-24T09:11:00Z", "把这个也给他。"),
            ],
            notes=[note("2026-10-23T10:00:00Z", "caerulea 和 dpdp 都在 research。")],
            host=HOST_MAIN,
        ),
    },
    "expected": {"action": "send", "target": "unresolved"},
})


# ---------------------------------------------------------------- holdout 02
add({
    "id": "sendx-h02",
    "split": "holdout",
    "provenance": "reconstruction of the user accepting an assistant commitment that names a peer; hand-authored, not a captured trace",
    "ask": ["action", "target"],
    "candidates": {"peers": [AUDITOR, REVIEWER, DPDP], "threads": [REVIEW_AUDIT]},
    "profiles": {
        "minimal": state(
            caseless("行，发给他", "复核意见如下。", {}),
            [AUDITOR, REVIEWER, DPDP], [REVIEW_AUDIT],
            conversation=[turn(212, "assistant", "2026-10-24T09:30:00Z",
                               "我让 auditor 复核这份结论。")],
        ),
        "focused": state(
            caseless("行，发给他", "复核意见如下。", {}),
            [AUDITOR, REVIEWER, DPDP], [REVIEW_AUDIT],
            conversation=[
                turn(210, "user", "2026-10-24T09:25:00Z", "这份结论谁看？"),
                turn(211, "assistant", "2026-10-24T09:28:00Z", "review 现在由 auditor 负责。"),
                turn(212, "assistant", "2026-10-24T09:30:00Z", "我让 auditor 复核这份结论。"),
                turn(213, "user", "2026-10-24T09:31:00Z", "行，发给他。"),
            ],
        ),
        "expanded": state(
            caseless("行，发给他", "复核意见如下。", {}),
            [AUDITOR, REVIEWER, DPDP], [REVIEW_AUDIT],
            conversation=[
                turn(190, "user", "2026-10-23T11:00:00Z", "reviewer 先忙发布。"),
                turn(213, "user", "2026-10-24T09:31:00Z", "行，发给他。"),
            ],
            notes=[note("2026-10-23T12:00:00Z", "review 的负责人是 auditor。")],
            host=HOST_MAIN,
        ),
    },
    "expected": {"action": "send", "target": "auditor"},
})


# ---------------------------------------------------------------- holdout 03
add({
    "id": "sendx-h03",
    "split": "holdout",
    "provenance": "reconstruction of a thread-addressed review request with a later check; hand-authored, not a captured trace",
    "ask": ["target", "response"],
    "candidates": {"peers": [REVIEWER_A, REVIEWER_B, DPDP], "threads": [REVIEW]},
    "profiles": {
        "minimal": state(
            caseless("在 review 里请负责人看一下，我出去一趟，回来再看", "请复核第 3 节。",
                     {"thread": "review"}),
            [REVIEWER_A, REVIEWER_B, DPDP], [REVIEW],
            conversation=[turn(222, "user", "2026-10-24T10:00:00Z",
                               "在 review 里请负责人看一下，我出去一趟，回来再看。")],
        ),
        "focused": state(
            caseless("在 review 里请负责人看一下，我出去一趟，回来再看", "请复核第 3 节。",
                     {"thread": "review"}),
            [REVIEWER_A, REVIEWER_B, DPDP], [REVIEW],
            conversation=[
                turn(220, "user", "2026-10-24T09:55:00Z", "review 现在谁主持？"),
                turn(221, "assistant", "2026-10-24T09:57:00Z", "reviewer-a 主持 review，reviewer-b 在 ops。"),
                turn(222, "user", "2026-10-24T10:00:00Z",
                     "在 review 里请负责人看一下，我出去一趟，回来再看。"),
            ],
        ),
        "expanded": state(
            caseless("在 review 里请负责人看一下，我出去一趟，回来再看", "请复核第 3 节。",
                     {"thread": "review"}),
            [REVIEWER_A, REVIEWER_B, DPDP], [REVIEW],
            conversation=[
                turn(200, "user", "2026-10-23T12:00:00Z", "ops 暂时不用管。"),
                turn(222, "user", "2026-10-24T10:00:00Z",
                     "在 review 里请负责人看一下，我出去一趟，回来再看。"),
            ],
            notes=[note("2026-10-23T13:00:00Z", "reviewer-a 和 reviewer-b 的显示名相同。")],
            host={**HOST_MAIN, "idle": False, "idle_source": "ctx.isIdle (prompt in flight)"},
        ),
    },
    "expected": {"target": "reviewer-a", "response": "async"},
})


# ---------------------------------------------------------------- holdout 04
add({
    "id": "sendx-h04",
    "split": "holdout",
    "provenance": "reconstruction of the cloned-lazuli resource split; hand-authored, not a captured trace",
    "ask": ["action", "target"],
    "candidates": {"peers": [LAZULI_1, LAZULI_2, DPDP], "threads": [CHAMBER]},
    "profiles": {
        "minimal": state(
            caseless("问问负责 research chamber 数据的那个 lazuli，作业跑完没有", "作业跑完了吗？", {}),
            [LAZULI_1, LAZULI_2, DPDP], [CHAMBER],
            conversation=[turn(232, "user", "2026-10-24T10:30:00Z",
                               "问问负责 research chamber 数据的那个 lazuli，作业跑完没有。")],
        ),
        "focused": state(
            caseless("问问负责 research chamber 数据的那个 lazuli，作业跑完没有", "作业跑完了吗？", {}),
            [LAZULI_1, LAZULI_2, DPDP], [CHAMBER],
            conversation=[
                turn(230, "user", "2026-10-24T10:25:00Z", "chamber 里怎么有两个 lazuli？"),
                turn(231, "assistant", "2026-10-24T10:27:00Z", "一个管计算，一个管数据，都在 chamber。"),
                turn(232, "user", "2026-10-24T10:30:00Z",
                     "问问负责 research chamber 数据的那个 lazuli，作业跑完没有。"),
            ],
        ),
        "expanded": state(
            caseless("问问负责 research chamber 数据的那个 lazuli，作业跑完没有", "作业跑完了吗？", {}),
            [LAZULI_1, LAZULI_2, DPDP], [CHAMBER],
            conversation=[
                turn(210, "user", "2026-10-23T14:00:00Z", "chamber 的作业先别取消。"),
                turn(232, "user", "2026-10-24T10:30:00Z",
                     "问问负责 research chamber 数据的那个 lazuli，作业跑完没有。"),
            ],
            notes=[note("2026-10-23T15:00:00Z", "两个 lazuli 共用显示名，责任不同。")],
            host=HOST_MAIN,
        ),
    },
    "expected": {"action": "send", "target": "lazuli-2"},
})


# ---------------------------------------------------------------- holdout 05
add({
    "id": "sendx-h05",
    "split": "holdout",
    "provenance": "reconstruction of the user asking for the historical reviewer after a handover; hand-authored, not a captured trace",
    "ask": ["target", "response"],
    "candidates": {"peers": [REVIEWER_OLD, AUDITOR, DPDP], "threads": []},
    "profiles": {
        "minimal": state(
            caseless("问问之前那个 reviewer，他手上还有没有旧记录，我接着整理", "旧评审记录还在吗？", {}),
            [REVIEWER_OLD, AUDITOR, DPDP], [],
            conversation=[turn(242, "assistant", "2026-10-24T11:00:00Z",
                               "reviewer 上周负责评审，现在评审由 auditor 负责。")],
        ),
        "focused": state(
            caseless("问问之前那个 reviewer，他手上还有没有旧记录，我接着整理", "旧评审记录还在吗？", {}),
            [REVIEWER_OLD, AUDITOR, DPDP], [],
            conversation=[
                turn(240, "user", "2026-10-24T10:56:00Z", "评审这边换人了吗？"),
                turn(241, "assistant", "2026-10-24T10:58:00Z", "换了，auditor 接手，reviewer 去忙发布。"),
                turn(242, "assistant", "2026-10-24T11:00:00Z",
                     "reviewer 上周负责评审，现在评审由 auditor 负责。"),
                turn(243, "user", "2026-10-24T11:02:00Z",
                     "问问之前那个 reviewer，他手上还有没有旧记录，我接着整理。"),
            ],
        ),
        "expanded": state(
            caseless("问问之前那个 reviewer，他手上还有没有旧记录，我接着整理", "旧评审记录还在吗？", {}),
            [REVIEWER_OLD, AUDITOR, DPDP], [],
            conversation=[
                turn(220, "user", "2026-10-23T15:00:00Z", "发布窗口对齐周五。"),
                turn(243, "user", "2026-10-24T11:02:00Z",
                     "问问之前那个 reviewer，他手上还有没有旧记录，我接着整理。"),
            ],
            notes=[note("2026-10-23T16:00:00Z", "旧评审记录还在 reviewer 手上。")],
            history_modes={
                "compressed_note": {
                    "authority": "note",
                    "source": "compaction summary",
                    "at": "2026-10-24T10:59:00Z",
                    "text": "review 已交给 auditor；reviewer 去忙发布。",
                },
                "recovered_source": {
                    "source": "history recovery",
                    "turns": [
                        turn(241, "assistant", "2026-10-24T10:58:00Z", "换了，auditor 接手，reviewer 去忙发布。"),
                        turn(242, "assistant", "2026-10-24T11:00:00Z", "reviewer 上周负责评审，现在评审由 auditor 负责。"),
                    ],
                },
            },
            host=HOST_MAIN,
        ),
    },
    "expected": {"target": "reviewer-old", "response": "async"},
})


# ---------------------------------------------------------------- holdout 06
add({
    "id": "sendx-h06",
    "split": "holdout",
    "provenance": "reconstruction of the user accepting a read-only resource integration; hand-authored, not a captured trace",
    "ask": ["action"],
    "candidates": {"peers": [DPDP, CAERULEA, REVIEWER], "threads": []},
    "profiles": {
        "minimal": state(
            caseless("行，那就按只读接", "", {}),
            [DPDP, CAERULEA, REVIEWER], [],
            conversation=[
                turn(252, "assistant", "2026-10-24T11:30:00Z",
                     "MCP 资源节点先按只读接入，只暴露 list_tools 和 read_resource。"),
                turn(253, "user", "2026-10-24T11:31:00Z", "行，那就按只读接。"),
            ],
        ),
        "focused": state(
            caseless("行，那就按只读接", "", {}),
            [DPDP, CAERULEA, REVIEWER], [],
            conversation=[
                turn(250, "user", "2026-10-24T11:20:00Z", "MCP 节点怎么接？"),
                turn(251, "assistant", "2026-10-24T11:25:00Z", "写操作没有回执，先只读。"),
                turn(252, "assistant", "2026-10-24T11:30:00Z",
                     "MCP 资源节点先按只读接入，只暴露 list_tools 和 read_resource。"),
                turn(253, "user", "2026-10-24T11:31:00Z", "行，那就按只读接。"),
                turn(254, "peer", "2026-10-24T11:32:00Z", "接入好了吗？", from_="urn:onlyne:agent:caerulea"),
            ],
        ),
        "expanded": state(
            caseless("行，那就按只读接", "", {}),
            [DPDP, CAERULEA, REVIEWER], [],
            conversation=[
                turn(230, "user", "2026-10-23T16:00:00Z", "资源节点先做连接。"),
                turn(252, "assistant", "2026-10-24T11:30:00Z",
                     "MCP 资源节点先按只读接入，只暴露 list_tools 和 read_resource。"),
                turn(253, "user", "2026-10-24T11:31:00Z", "行，那就按只读接。"),
                turn(254, "peer", "2026-10-24T11:32:00Z", "接入好了吗？", from_="urn:onlyne:agent:caerulea"),
            ],
            notes=[note("2026-10-23T17:00:00Z", "写入操作的回执还没设计。")],
            host=HOST_MAIN,
        ),
    },
    "expected": {"action": "local"},
})


# ---------------------------------------------------------------- holdout 07
add({
    "id": "sendx-h07",
    "split": "holdout",
    "provenance": "reconstruction of a topology listing request; hand-authored, not a captured trace",
    "ask": ["action"],
    "candidates": {"peers": [DPDP, CAERULEA, REVIEWER], "threads": []},
    "profiles": {
        "minimal": state(
            caseless("列一下现在哪些节点在线", "", {}),
            [DPDP, CAERULEA, REVIEWER], [],
            conversation=[turn(262, "user", "2026-10-24T12:00:00Z", "列一下现在哪些节点在线。")],
        ),
        "focused": state(
            caseless("列一下现在哪些节点在线", "", {}),
            [DPDP, CAERULEA, REVIEWER], [],
            conversation=[
                turn(260, "user", "2026-10-24T11:55:00Z", "先看看拓扑。"),
                turn(261, "assistant", "2026-10-24T11:57:00Z", "三个节点可达，我先列出来。"),
                turn(262, "user", "2026-10-24T12:00:00Z", "列一下现在哪些节点在线。"),
            ],
        ),
        "expanded": state(
            caseless("列一下现在哪些节点在线", "", {}),
            [DPDP, CAERULEA, REVIEWER], [],
            conversation=[
                turn(240, "user", "2026-10-23T17:00:00Z", "reviewer 最近可能掉线。"),
                turn(262, "user", "2026-10-24T12:00:00Z", "列一下现在哪些节点在线。"),
            ],
            notes=[note("2026-10-23T18:00:00Z", "拓扑十分钟刷新一次。")],
            host=HOST_MAIN,
        ),
    },
    "expected": {"action": "read"},
})


# ---------------------------------------------------------------- holdout 08
add({
    "id": "sendx-h08",
    "split": "holdout",
    "provenance": "reconstruction of RFC 0005 new-context rule with a closed prior context; hand-authored, not a captured trace",
    "ask": ["execution", "response"],
    "candidates": {"peers": [CAERULEA, DPDP, REVIEWER], "threads": [RESEARCH]},
    "profiles": {
        "minimal": state(
            caseless("接着刚才跟 caerulea 说的那件事，把结论发过去，我继续弄别的", "第 3 节结论如下。",
                     {"to": "caerulea"}),
            [CAERULEA, DPDP, REVIEWER], [RESEARCH],
            contexts=[C7],
            conversation=[turn(272, "user", "2026-10-24T12:30:00Z",
                               "接着刚才跟 caerulea 说的那件事，把结论发过去，我继续弄别的。")],
        ),
        "focused": state(
            caseless("接着刚才跟 caerulea 说的那件事，把结论发过去，我继续弄别的", "第 3 节结论如下。",
                     {"to": "caerulea"}),
            [CAERULEA, DPDP, REVIEWER], [RESEARCH],
            contexts=[C7],
            conversation=[
                turn(270, "user", "2026-10-24T12:20:00Z", "刚才跟 caerulea 聊的第 2 组数据，c-7 结束了吗？"),
                turn(271, "assistant", "2026-10-24T12:25:00Z", "c-7 已经关了，第 2 组数据聊完。"),
                turn(272, "user", "2026-10-24T12:30:00Z",
                     "接着刚才跟 caerulea 说的那件事，把结论发过去，我继续弄别的。"),
            ],
        ),
        "expanded": state(
            caseless("接着刚才跟 caerulea 说的那件事，把结论发过去，我继续弄别的", "第 3 节结论如下。",
                     {"to": "caerulea"}),
            [CAERULEA, DPDP, REVIEWER], [RESEARCH],
            contexts=[C7],
            conversation=[
                turn(250, "user", "2026-10-24T12:00:00Z", "第 3 节文档收尾。"),
                turn(270, "user", "2026-10-24T12:20:00Z", "刚才跟 caerulea 聊的第 2 组数据，c-7 结束了吗？"),
                turn(271, "assistant", "2026-10-24T12:25:00Z", "c-7 已经关了，第 2 组数据聊完。"),
                turn(272, "user", "2026-10-24T12:30:00Z",
                     "接着刚才跟 caerulea 说的那件事，把结论发过去，我继续弄别的。"),
            ],
            notes=[note("2026-10-24T12:28:00Z", "c-7 已关闭。")],
            host={**HOST_MAIN, "todos": [{"text": "收尾第 3 节文档", "state": "in_progress"}]},
        ),
    },
    "expected": {"execution": "new", "response": "none"},
})
