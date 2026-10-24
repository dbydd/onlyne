"""reply 判定点：引用回复的关系、权威、落点、新问题与视图选择。

每个 case 是手工构造的场景重建（reconstruction），参考 RFC 0003/0005/0008 的
回复路径、本仓库的工作流与用户原始措辞；不是生产流量，也不是抓取的实际会话。

三份 profile 的差别只在于证据多少，不改变事实来源的权威：

- minimal：判断所需的最小证据。仍保留回复权威事实、正文中的声明、等待匹配事实。
- focused：在 minimal 上补齐判断需要的本地上下文（用户/助手历史轮次、thread 记录、
  本地发送记录、资源结果），候选一次给全。
- expanded：在 focused 上加入真实存在的干扰项（其他 thread 的消息、旧轮次、过期等待、
  显示名重复、正文冒充的陈述、被历史覆盖的提交），同一事实的权威等级不变。

候选由确定性层一次给全并全部保留：本 case 有责任考虑到的端点、thread、消息与视图都在列，
不因正文、话题或关键字预先删除。选择由语义判断完成。

问题模板在整条 slice 内共享；case 之间只有候选 id、候选属性、上下文与证据不同。
首轮 dev 暴露了若干手工场景与标签错误（见最终报告），据此修正场景后重新冻结；
tuned 修订只改问题模板与证据，不再改期望标签。
"""

from __future__ import annotations

import copy

ME = {"node_id": "urn:onlyne:agent:lazuli", "display": "lazuli"}
DPDP = "urn:onlyne:agent:dpdp"
CAERULEA = "urn:onlyne:agent:caerulea"
REVIEWER = "urn:onlyne:agent:reviewer"
ARJEN = "urn:onlyne:agent:arjen"
OMLX_OPS = "urn:onlyne:agent:omlx-ops"
RESEARCH = {"owner": "urn:onlyne:agent:lazuli", "id": "research"}
OPS = {"owner": "urn:onlyne:agent:caerulea", "id": "ops"}
REVIEW = {"owner": "urn:onlyne:agent:reviewer", "id": "review"}
IM = {"owner": "urn:onlyne:gateway:im", "id": "im-sea-ice"}
PRIVATE = {"owner": "urn:onlyne:agent:stranger", "id": "private"}
LAB = {"owner": "urn:onlyne:agent:lazuli", "id": "lab"}

INITIAL_QUESTIONS = {
    "relation": {
        "type": "choice",
        "instructions": (
            "Classify what this outbound message does to the original message it references. "
            "Weigh the stated intention first, then the body: a body that only restates the original request, "
            "or a remark without an answer, is not an answer. A body that delivers the requested result or a "
            "decision on the request is an answer even when phrased loosely; the stated intention can only "
            "decide when the body is genuinely in between."
        ),
        "criteria": {
            "reply": "Delivers the requested answer or result, or a refusal of the request; only this relation releases the author's wait when everything else matches.",
            "followup": "Carries progress, a supplement, or another instruction that continues the exchange without answering or refusing the original request; it never releases a wait.",
            "unresolved": "The evidence does not let the relation be decided; the caller must ask the agent to state it with meta.relation.",
        },
    },
    "target": {
        "type": "choice",
        "instructions": (
            "Choose the endpoint the reply must be delivered to. The endpoint is the reply authority recorded "
            "with the original message; credentials, thread membership, or who sent text in this case do not "
            "replace it. A peer that holds no acknowledgement of the original can still be the right endpoint, "
            "and a thread member that stands in for the author is not the authority. When no credible authority "
            "is available, choose the abstaining candidate."
        ),
        "criteria": {
            "peers": "One candidate per observable endpoint, carrying node identity, relation to this case, and recorded reply-authority facts.",
            "none": "Abstain: the original's authority is outside this node's read scope, or the evidence contradicts itself, so no endpoint can be chosen.",
        },
    },
    "thread": {
        "type": "choice",
        "instructions": (
            "Choose the thread the reply lands in. The reply goes to the destination bound to the original "
            "message; a closed thread is still the destination, and a thread of this node is not the destination "
            "only because it is open. Choose the abstaining candidate when the original is outside read scope, "
            "when the thread is unknown, or when two candidates claim the same landing equally."
        ),
        "criteria": {
            "threads": "One candidate per observable thread, carrying its reference, state, access, and whether it is the recorded landing of the original.",
            "none": "Abstain: no destination can be chosen from the evidence.",
        },
    },
    "response": {
        "type": "choice",
        "instructions": (
            "Does this outbound message itself ask its recipient for a new reply, and how does this sender wait "
            "for it? This is about the message being sent, not the original message. An instruction the user "
            "adds for this node is work assigned here, not a question to the peer. Asking the peer for something "
            "is `await` only when the message says this sender stops for the answer; otherwise it is `async`."
        ),
        "criteria": {
            "none": "The message answers the original or only informs the peer, and asks the peer no new question.",
            "async": "The message asks the peer a new question and this sender continues meanwhile, checking later.",
            "await": "The message asks the peer a new question and this sender stops and waits for it.",
        },
    },
    "view": {
        "type": "choice",
        "instructions": (
            "This message is a valid reply that satisfies the open wait this node holds for it. Choose what the "
            "reply needs to enter the target agent's conversation. A reply that the automatic attention fragment "
            "has not delivered yet must reach the agent in full; a reply that fragment has already delivered must "
            "not be shown twice; a reply this agent posted itself is not incoming material. The requested "
            "visibility mode of a local read does not change the delivery needed by a reply."
        ),
        "criteria": {
            "views": "One candidate per observable delivery path, carrying its recorded consumption facts.",
        },
    },
}

TUNED_QUESTIONS = {
    "relation": {
        "type": "choice",
        "instructions": (
            "Classify what this outbound message does to the original message it references. The stated "
            "intention is evidence, but the body decides. A body that delivers the requested result, decision, "
            "or refusal is a reply even when it also asks one follow-up question or says the sender moves on to "
            "other work. A body with no result, no decision, and no refusal is not an answer: progress, a plan, "
            "a blocker, an added instruction, and a bare acknowledgment are followups. An acknowledgment such as "
            "`好了`, `收到`, `ok`, or `谢谢` does not carry a result. When the record leaves it open whether the "
            "sender meant `done` or `noted`, choose the unresolved candidate instead of guessing."
        ),
        "criteria": {
            "reply": "Delivers the requested answer, result, decision, or refusal, possibly alongside one new question or a note that other work continues; only this relation releases the author's wait.",
            "followup": "Carries progress, a plan, a blocker, a supplement, an added instruction, or an acknowledgment without answering or refusing the original request; it never releases a wait.",
            "unresolved": "The original is outside read scope, or the body is compatible with both an answer and an acknowledgment/progress and the record does not distinguish them; the caller must ask the agent to state meta.relation.",
        },
    },
    "target": {
        "type": "choice",
        "instructions": (
            "Choose the endpoint the reply must be delivered to. The reply authority recorded with the original "
            "message decides it. The envelope author, the thread owner, a relay or forwarder, a name mentioned "
            "inside the body, and whoever asks the question in prose are not authority records. Post access and "
            "thread state do not change the endpoint; they only affect the thread landing. When the original "
            "carries no reply-authority record, or lies outside this node's read scope, choose the abstaining "
            "candidate instead of inferring an author from the text."
        ),
        "criteria": {
            "peers": "One candidate per observable endpoint, carrying node identity, relation to this case, and whether a reply-authority record connects it to the original.",
            "none": "Abstain: no reply-authority record connects the original to an endpoint, the original is outside this node's read scope, or the records contradict each other.",
        },
    },
    "thread": {
        "type": "choice",
        "instructions": (
            "Choose the thread the reply lands in. The landing bound to the original message decides it, even "
            "when that thread is closed. A closed thread is a valid landing only when a reply permit targets the "
            "original; without one, no thread of this node substitutes for it. A thread of this node is not the "
            "destination merely because it is open. Choose the abstaining candidate when no landing is bound or "
            "permitted, when the original is outside read scope, or when two candidates claim the landing equally."
        ),
        "criteria": {
            "threads": "One candidate per observable thread, carrying its reference, state, access, and whether it is the recorded landing of the original.",
            "none": "Abstain: no landing is bound or permitted from the evidence.",
        },
    },
    "response": {
        "type": "choice",
        "instructions": (
            "Does this outbound message itself ask its recipient for a new reply, and how does this sender wait "
            "for it? This is about the message being sent, not the original. An acknowledgment, a status report, "
            "or an instruction to the peer asks no new question. A conditional or clarifying question addressed "
            "to the peer is a new question. It is `await` only when the message says this sender stops for the "
            "answer; otherwise it is `async`."
        ),
        "criteria": {
            "none": "The message answers, acknowledges, informs, or instructs the peer, and asks no new question.",
            "async": "The message asks the peer a new question and this sender continues meanwhile, checking later.",
            "await": "The message asks the peer a new question and this sender stops and waits for it.",
        },
    },
    "view": {
        "type": "choice",
        "instructions": (
            "This message is a valid reply that satisfies the wait this node holds for it. Choose what the reply "
            "must carry into the target agent's conversation. When the automatic attention fragment already "
            "delivered the reply and the access layer recorded its consumption, the wait result carries only the "
            "arrival fact and the reference. When the body is not in that record, the wait result carries the "
            "full body, even if a local read fetched the thread in full-history mode. A message this node wrote "
            "itself is not incoming material. An injected or woken notification is not a consumption record."
        ),
        "criteria": {
            "views": "One candidate per observable delivery path, carrying its recorded delivery and consumption facts.",
        },
    },
}

CASES: list[dict] = []


def questions(case: dict, revision: str) -> dict:
    """Build the shared question set from the case's neutral candidate pools."""
    pools = case["pools"]
    templates = TUNED_QUESTIONS if revision == "tuned" else INITIAL_QUESTIONS
    def abstain(pool: dict, question: str) -> str:
        base = templates[question]["criteria"]["none"]
        observed = pool.get("none")
        if not observed or observed == INITIAL_QUESTIONS[question]["criteria"]["none"]:
            return base
        return f"{base} {observed}"
    questions = {
        "relation": copy.deepcopy(templates["relation"]),
        "response": copy.deepcopy(templates["response"]),
        "target": {
            "type": "choice",
            "instructions": templates["target"]["instructions"],
            "criteria": {**copy.deepcopy(pools["peer"]), "none": abstain(pools["peer"], "target")},
        },
        "thread": {
            "type": "choice",
            "instructions": templates["thread"]["instructions"],
            "criteria": {**copy.deepcopy(pools["thread"]), "none": abstain(pools["thread"], "thread")},
        },
    }
    if "view" in case["expected"]:
        questions["view"] = {
            "type": "choice",
            "instructions": templates["view"]["instructions"],
            "criteria": copy.deepcopy(pools["view"]),
        }
    return questions


def _materialize(value) -> dict:
    built = value() if callable(value) else value
    if isinstance(built, tuple):
        state, peers, threads = built
        return {"state": state, "candidates": {"peer": peers, "thread": threads}}
    return built


def _case(ident, split, scenario, profiles, expected, note=""):
    profiles = {name: _materialize(value) for name, value in profiles.items()}
    focused = profiles["focused"]
    pools = {"peer": focused["candidates"]["peer"], "thread": focused["candidates"]["thread"]}
    if "view" in focused["candidates"]:
        pools["view"] = focused["candidates"]["view"]
    return {
        "id": ident,
        "point": "reply",
        "split": split,
        "provenance": (
            "reconstruction: hand-authored scenario based on RFC 0003 reply rules, RFC 0005 waits, "
            f"RFC 0008 surface and the repository workflow [{scenario}]; not a captured production trace"
            + (f"; {note}" if note else "")
        ),
        "profiles": profiles,
        "expected": expected,
        "pools": pools,
    }


def _peer(alias: str, node_id: str, **facts) -> dict:
    return {"id": alias, "properties": {"node_id": node_id, **facts}}


def _thread(alias: str, thread_ref: dict, **facts) -> dict:
    return {"id": alias, "properties": {"thread_ref": thread_ref, **facts}}


def _peers(entries: list[dict], none: str | None = None) -> dict:
    pool = {entry["id"]: entry for entry in entries}
    pool["none"] = none or INITIAL_QUESTIONS["target"]["criteria"]["none"]
    return pool


def _threads(entries: list[dict], none: str | None = None) -> dict:
    pool = {entry["id"]: entry for entry in entries}
    pool["none"] = none or INITIAL_QUESTIONS["thread"]["criteria"]["none"]
    return pool


def _message(alias: str, message_ref: dict, **facts) -> dict:
    return {"id": alias, "properties": {"message_ref": message_ref, **facts}}


def _turn(turn: str, role: str, text: str) -> dict:
    return {"turn": turn, "role": role, "text": text}


def _local_text(text: str) -> dict:
    return {"role": "local", "text": text}


def _entry(seq: str, direction: str, sender: str | None, delivery_kind: str, body: str, **facts) -> dict:
    entry = {"seq": seq, "direction": direction}
    if sender is not None:
        entry["sender"] = sender
    entry.update({"delivery_kind": delivery_kind, "body": body})
    entry.update(facts)
    return entry


def _orig(sender, thread_ref, seq, ident, delivery_kind, body, **facts) -> dict:
    original = {
        "message_ref": {"sender": sender, "id": ident},
        "thread_ref": thread_ref,
        "seq": seq,
        "author": sender,
        "delivery_kind": delivery_kind,
        "body": body,
    }
    original.update(facts)
    return original


def _intent(delivery_kind, msg, thread_ref, seq, sender, ident, **meta) -> dict:
    return {
        "delivery_kind": delivery_kind,
        "msg": msg,
        "meta": {"in_reply_to": {"thread_ref": thread_ref, "seq": seq}, "in_reply_to_ref": {"sender": sender, "id": ident}, **meta},
    }


ALL_PEERS = [
    _peer("dpdp", DPDP, relation="thread member of the original's thread; author of the original"),
    _peer("caerulea", CAERULEA, relation="thread member of the original's thread; not the author"),
    _peer("reviewer", REVIEWER, relation="member of another thread of this node"),
]


def _base_state(original, intent, thread, wait, me_note=None) -> dict:
    state = {
        "decision": "reply",
        "me": dict(ME),
        "original": original,
        "intent": intent,
        "thread": thread,
        "wait": wait,
    }
    if me_note:
        state["me"]["local"] = me_note
    return state


# -- 1. 最终结论，句号式答复 -------------------------------------------------

def _c01_minimal():
    state = _base_state(
        _orig(DPDP, RESEARCH, "103", "msg-01", "等你看完给我结论", "请核对这次实验的结论。"),
        _intent("答复这条请求，结论如下", "第二组数据需要重算。", RESEARCH, "103", DPDP, "msg-01"),
        {"state": "open", "my_access": ["post", "read"], "reply_authority": DPDP},
        {"expected_author": DPDP, "expected_ref": {"sender": DPDP, "id": "msg-01"}, "deadline": "2026-10-24T04:00:00Z"},
    )
    return state, _peers(ALL_PEERS), _threads([
        _thread("research", RESEARCH, state="open", my_access=["post", "read"], landing="holds the original at seq 103"),
        _thread("ops", OPS, state="closed", my_access=[]),
    ])


def _c01_focused():
    local = {
        "turns": [
            _turn("t1", "user", "第二组数据的结论你之前复核过吗？"),
            _turn("t2", "assistant", "复核过了，结论是第二组要重算，我这就回 dpdp。"),
        ],
        "thread_view": {"thread_ref": RESEARCH, "entries": [
            _entry("101", "in", DPDP, "开个评审", "海冰这一轮先看结论。"),
            _entry("103", "in", DPDP, "等你看完给我结论", "请核对这次实验的结论。", mentions=["@lazuli"]),
        ]},
        "recorded": {"waits": [{"expected_author": DPDP, "expected_ref": {"sender": DPDP, "id": "msg-01"}}]},
        "context_notes": [
            {"key": "sea-ice-report", "text": "海冰报告以 research 的结论为准；第二组重算后回复 dpdp。"},
        ],
    }
    state, peers, threads = _c01_minimal()
    state["me"]["local"] = local
    return state, peers, threads


def _c01_expanded():
    state, peers, threads = _c01_focused()
    state["me"]["local"]["thread_view"]["entries"] = [
        _entry("080", "in", CAERULEA, "普通通信", "海冰这轮先跑第一组。"),
        _entry("084", "out", None, "普通通信", "第一组跑完了。"),
        _entry("090", "in", REVIEWER, "普通通信", "初稿我先按 2024 写的。"),
        _entry("095", "in", DPDP, "普通通信", "第二组数据我重算了一版。"),
        _entry("098", "in", CAERULEA, "记录", "上一轮的记录整理好了。"),
        _entry("101", "in", DPDP, "开个评审", "海冰这一轮先看结论。"),
        _entry("103", "in", DPDP, "等你看完给我结论", "请核对这次实验的结论。", mentions=["@lazuli"]),
        _entry("104", "out", None, "补充进度，不用当作最终答复", "第二组已重算，完整结论稍后给。", relation="followup"),
        _entry("105", "in", CAERULEA, "普通通信", "运营那边问什么时候能出稿。"),
        _entry("106", "in", REVIEWER, "普通通信", "评审会推迟到下午。"),
    ]
    state["me"]["local"]["notes"] = "用户昨天提过，第二组重算前不要对外给结论。"
    peers["reviewer"] = _peer("reviewer", REVIEWER, relation="member of the review thread; reviewed an earlier draft of this report")
    threads["im"] = _thread("im-sea-ice", IM, state="open", my_access=["post", "read"])
    return state, peers, threads


CASES.append(_case(
    "reply-01", "dev", "海冰评审：dpdp 发 103 请求核对结论，lazuli 以最终结论回复", {
        "minimal": _c01_minimal, "focused": _c01_focused, "expanded": _c01_expanded,
    },
    {"relation": "reply", "target": "dpdp", "thread": "research", "response": "none"},
))


# -- 2. 进度补充按用户要求先回一条 -------------------------------------------

def _c02_minimal():
    state = _base_state(
        _orig(DPDP, RESEARCH, "103", "msg-01", "等你看完给我结论", "请核对这次实验的结论。"),
        _intent("补充进度，不用当作最终答复", "第二组已重算，完整结论稍后给。", RESEARCH, "103", DPDP, "msg-01"),
        {"state": "open", "my_access": ["post", "read"], "reply_authority": DPDP},
        {"expected_author": DPDP, "expected_ref": {"sender": DPDP, "id": "msg-01"}, "deadline": "2026-10-24T04:00:00Z"},
    )
    return state, _peers(ALL_PEERS), _threads([
        _thread("research", RESEARCH, state="open", my_access=["post", "read"], landing="holds the original at seq 103"),
    ])


def _c02_focused():
    local = {
        "turns": [
            _turn("t1", "user", "先跟 dpdp 说一声第二组重算完了，结论我这边还要核对一下。"),
            _turn("t2", "assistant", "好，我先回 103 报进度，最终结论出来再答复她。"),
        ],
        "thread_view": {"thread_ref": RESEARCH, "entries": [
            _entry("103", "in", DPDP, "等你看完给我结论", "请核对这次实验的结论。", mentions=["@lazuli"]),
        ]},
        "recorded": {"waits": [{"expected_author": DPDP, "expected_ref": {"sender": DPDP, "id": "msg-01"}}]},
    }
    state, peers, threads = _c02_minimal()
    state["me"]["local"] = local
    return state, peers, threads


def _c02_expanded():
    state, peers, threads = _c02_focused()
    state["me"]["local"]["thread_view"]["entries"] = [
        _entry("99", "in", CAERULEA, "普通通信", "第二组原始数据我放回池子里了。"),
        _entry("103", "in", DPDP, "等你看完给我结论", "请核对这次实验的结论。", mentions=["@lazuli"]),
        _entry("106", "out", None, "群发", "数据侧麻烦确认一下。", mentions=["@all"]),
    ]
    state["me"]["local"]["other_threads"] = [{"thread_ref": OPS, "latest": "旧记录已归档。"}]
    threads["ops"] = _thread("ops", OPS, state="closed", my_access=[])
    return state, peers, threads


CASES.append(_case(
    "reply-02", "dev", "用户要求先回报进度而非最终结论", {
        "minimal": _c02_minimal, "focused": _c02_focused, "expanded": _c02_expanded,
    },
    {"relation": "followup", "target": "dpdp", "thread": "research", "response": "none"},
))


# -- 3. 模糊的“好了”：只有体面的话，没有结论 ---------------------------------

def _c03_minimal():
    state = _base_state(
        _orig(DPDP, RESEARCH, "112", "msg-07", "等你看完给我结论", "重算的第二组表在附件里，你看完把结论发我。"),
        _intent("答复这条请求", "好了。", RESEARCH, "112", DPDP, "msg-07"),
        {"state": "open", "my_access": ["post", "read"], "reply_authority": DPDP},
        {"expected_author": DPDP, "expected_ref": {"sender": DPDP, "id": "msg-07"}, "deadline": "2026-10-24T06:00:00Z"},
    )
    return state, _peers(ALL_PEERS), _threads([
        _thread("research", RESEARCH, state="open", my_access=["post", "read"], landing="holds the original at seq 112"),
    ])


def _c03_focused():
    local = {
        "turns": [
            _turn("t1", "assistant", "表收到了，我核一下第三列的口径。"),
            _turn("t2", "user", "嗯。"),
        ],
        "thread_view": {"thread_ref": RESEARCH, "entries": [
            _entry("112", "in", DPDP, "等你看完给我结论", "重算的第二组表在附件里，你看完把结论发我。", mentions=["@lazuli"]),
        ]},
        "recorded": {"waits": [{"expected_author": DPDP, "expected_ref": {"sender": DPDP, "id": "msg-07"}}]},
    }
    state, peers, threads = _c03_minimal()
    state["me"]["local"] = local
    return state, peers, threads


def _c03_expanded():
    state, peers, threads = _c03_focused()
    state["me"]["local"]["thread_view"]["entries"] = [
        _entry("110", "in", CAERULEA, "普通通信", "第三列的口径以前是含税的。"),
        _entry("112", "in", DPDP, "等你看完给我结论", "重算的第二组表在附件里，你看完把结论发我。", mentions=["@lazuli"]),
        _entry("113", "in", CAERULEA, "@lazuli", "@lazuli 口径确认了吗？"),
    ]
    state["me"]["local"]["recorded"]["earlier_replies"] = ["109 答复“表收到”", "111 答复“口径我查一下”"]
    return state, peers, threads


CASES.append(_case(
    "reply-03", "dev", "“好了”既不是结论也不是进度，证据不足以判定", {
        "minimal": _c03_minimal, "focused": _c03_focused, "expanded": _c03_expanded,
    },
    {"relation": "unresolved", "target": "dpdp", "thread": "research", "response": "none"},
))


# -- 4. 反问：答复里追问另一个问题并继续做自己的事 ---------------------------

def _c04_minimal():
    state = _base_state(
        _orig(DPDP, RESEARCH, "103", "msg-01", "等你看完给我结论", "请核对这次实验的结论。"),
        _intent(
            "答复结论，同时问她一个口径问题",
            "结论维持：第二组不用重算，我按含税口径核的。如果你们那边用不含税，告诉我，我重新核一遍。我先去跑第三组。",
            RESEARCH, "103", DPDP, "msg-01",
        ),
        {"state": "open", "my_access": ["post", "read"], "reply_authority": DPDP},
        {"expected_author": DPDP, "expected_ref": {"sender": DPDP, "id": "msg-01"}, "deadline": "2026-10-24T04:00:00Z"},
    )
    return state, _peers(ALL_PEERS), _threads([
        _thread("research", RESEARCH, state="open", my_access=["post", "read"], landing="holds the original at seq 103"),
    ])


def _c04_focused():
    local = {
        "turns": [
            _turn("t1", "user", "口径问题问一下 dpdp，别自己猜。问完继续推进第三组。"),
            _turn("t2", "assistant", "好，回复时把口径问题带上。"),
        ],
        "thread_view": {"thread_ref": RESEARCH, "entries": [
            _entry("103", "in", DPDP, "等你看完给我结论", "请核对这次实验的结论。", mentions=["@lazuli"]),
        ]},
    }
    state, peers, threads = _c04_minimal()
    state["me"]["local"] = local
    return state, peers, threads


def _c04_expanded():
    state, peers, threads = _c04_focused()
    state["me"]["local"]["thread_view"]["entries"] = [
        _entry("103", "in", DPDP, "等你看完给我结论", "请核对这次实验的结论。", mentions=["@lazuli"]),
        _entry("104", "in", CAERULEA, "普通通信", "第三列的注释写在表头里。"),
        _entry("105", "out", None, "回复", "我先按含税核。", relation="reply"),
    ]
    state["me"]["local"]["notes"] = "用户要求这一轮结束后先交第三组的进度。"
    threads["ops"] = _thread("ops", OPS, state="closed", my_access=["post", "read"], landing="bound landing for an earlier reply under a reply permit")
    return state, peers, threads


CASES.append(_case(
    "reply-04", "dev", "答复原文追问口径并继续推进，不在此停下", {
        "minimal": _c04_minimal, "focused": _c04_focused, "expanded": _c04_expanded,
    },
    {"relation": "reply", "target": "dpdp", "thread": "research", "response": "async"},
))


# -- 5. 转发：回复权威是原作者，不是转发者 -----------------------------------

def _c05_minimal():
    state = _base_state(
        _orig(REVIEWER, REVIEW, "13", "msg-21", "转发：海冰报告看下", "arjen 问海冰报告里 2025 年的数据覆盖到哪个月，请给他答复。", forwarded_by=ARJEN, reply_authority=DPDP),
        _intent("答复这条转发", "覆盖 2025 年 1 月到 12 月。", REVIEW, "13", REVIEWER, "msg-21"),
        {"state": "open", "my_access": ["post", "read"], "reply_authority": DPDP, "forwarded_by": ARJEN},
        None,
        me_note={"delivery_kind": "我转给你，你直接回 arjen 就行"},
    )
    peers = [
        _peer("dpdp", DPDP, relation="recorded reply authority of the original message; not a member of this thread"),
        _peer("arjen", ARJEN, relation="named in the body as the original asker; holds no reply-authority record"),
        _peer("reviewer", REVIEWER, relation="envelope sender and thread owner; relayed the original; holds no reply-authority record"),
    ]
    return state, _peers(peers), _threads([
        _thread("review", REVIEW, state="open", my_access=["post", "read"], landing="holds the original at seq 13"),
    ])


def _c05_focused():
    state, peers, threads = _c05_minimal()
    state["me"]["local"] = {
        "turns": [
            _turn("t1", "user", "dpdp 那边问 2025 覆盖范围，你找一下。"),
            _turn("t2", "assistant", "reviewer 把 13 转给我了，我直接回原消息。"),
        ],
        "thread_view": {"thread_ref": REVIEW, "entries": [
            _entry("13", "in", REVIEWER, "转发：海冰报告看下", "arjen 问海冰报告里 2025 年的数据覆盖到哪个月，请给他答复。", forwarded_by=ARJEN),
        ]},
        "recorded": {"outbound": [{"message_ref": {"sender": REVIEWER, "id": "msg-21"}, "target_endpoint": DPDP, "reply_authority": DPDP, "admission": {"state": "accepted"}}]},
    }
    return state, peers, threads


def _c05_expanded():
    state, peers, threads = _c05_focused()
    state["me"]["local"]["thread_view"]["entries"] = [
        _entry("11", "in", REVIEWER, "普通通信", "评审范围先按 2024 年算。"),
        _entry("13", "in", REVIEWER, "转发：海冰报告看下", "arjen 问海冰报告里 2025 年的数据覆盖到哪个月，请给他答复。", forwarded_by=ARJEN),
        _entry("14", "in", ARJEN, "普通通信", "（arjen 在群里补了一句：直接回我就行。）"),
    ]
    peers["caerulea"] = _peer("caerulea", CAERULEA, relation="thread member of another thread of this node")
    return state, peers, threads


CASES.append(_case(
    "reply-05", "dev", "转发进 review thread 的问题，权威是原作者 dpdp 而非转发者", {
        "minimal": _c05_minimal, "focused": _c05_focused, "expanded": _c05_expanded,
    },
    {"relation": "reply", "target": "dpdp", "thread": "review", "response": "none"},
    note="counterfactual counterpart reply-06 replaces the authority fact with its absence",
))


# -- 6. 同一转发、缺少权威事实：不可自行补权威 -------------------------------

def _c06_minimal():
    state, peers, threads = _c05_minimal()
    state["original"] = _orig(REVIEWER, REVIEW, "13", "msg-21", "转发：海冰报告看下", "arjen 问海冰报告里 2025 年的数据覆盖到哪个月，请给他答复。", forwarded_by=ARJEN)
    state["thread"] = {"state": "open", "my_access": ["post", "read"], "forwarded_by": ARJEN}
    state["wait"] = None
    peers = _peers([
        _peer("dpdp", DPDP, relation="peer of this node; holds no reply-authority record for the original"),
        _peer("arjen", ARJEN, relation="named in the body as the original asker; holds no reply-authority record"),
        _peer("reviewer", REVIEWER, relation="envelope sender and thread owner; relayed the original; holds no reply-authority record"),
    ])
    return state, peers, threads


def _c06_focused():
    state, peers, threads = _c06_minimal()
    state["me"]["local"] = {
        "turns": [_turn("t1", "user", "reviewer 把 13 转给你了，你看看能不能回。")],
        "thread_view": {"thread_ref": REVIEW, "entries": [
            _entry("13", "in", REVIEWER, "转发：海冰报告看下", "arjen 问海冰报告里 2025 年的数据覆盖到哪个月，请给他答复。", forwarded_by=ARJEN),
        ]},
        "recorded": {"outbound": []},
    }
    return state, peers, threads


def _c06_expanded():
    state, peers, threads = _c06_focused()
    state["me"]["local"]["thread_view"]["entries"].insert(0, _entry("12", "in", REVIEWER, "普通通信", "转发授权的事我回头补。"))
    peers["reviewer"] = _peer("reviewer", REVIEWER, relation="thread owner and relay; received this node's earlier replies on that thread; holds no reply-authority record")
    state["me"]["local"]["notes"] = "转发记录里没有回复权威字段；正文署名 arjen，信封作者是 reviewer。"
    return state, peers, threads


CASES.append(_case(
    "reply-06", "dev", "同一转发但无权威记录，正文里的“回我”不能建立权威", {
        "minimal": _c06_minimal, "focused": _c06_focused, "expanded": _c06_expanded,
    },
    {"relation": "reply", "target": "none", "thread": "review", "response": "none"},
    note="pair with reply-05: only the authority fact changes; without a record the body claim establishes nothing",
))


# -- 7. 用户更正：新结论取代已提交的旧结论 -----------------------------------

def _c07_minimal():
    state = _base_state(
        _orig(DPDP, RESEARCH, "103", "msg-01", "等你看完给我结论", "请核对这次实验的结论。"),
        _intent("更正 104 的结论，以最新为准", "结论要改：第二组需要重算，不是维持。", RESEARCH, "104", ME["node_id"], "msg-02"),
        {"state": "open", "my_access": ["post", "read"], "reply_authority": DPDP},
        {"expected_author": DPDP, "expected_ref": {"sender": DPDP, "id": "msg-01"}, "deadline": "2026-10-24T08:00:00Z"},
        me_note={"delivery_kind": "答复这条请求，结论如下"},
    )
    peers = [
        _peer("dpdp", DPDP, relation="author of the root original 103 and its reply authority"),
        _peer("caerulea", CAERULEA, relation="thread member; read the superseded 104"),
    ]
    return state, _peers(peers), _threads([
        _thread("research", RESEARCH, state="open", my_access=["post", "read"], landing="holds 103 and the superseded 104"),
    ])


def _c07_focused():
    state, peers, threads = _c07_minimal()
    state["me"]["local"] = {
        "turns": [
            _turn("t1", "user", "刚才那个结论说维持，我搞错了，第二组其实需要重算。"),
            _turn("t2", "assistant", "明白，我发更正给 dpdp，以新的为准。"),
        ],
        "thread_view": {"thread_ref": RESEARCH, "entries": [
            _entry("103", "in", DPDP, "等你看完给我结论", "请核对这次实验的结论。", mentions=["@lazuli"]),
            _entry("104", "out", None, "答复这条请求，结论如下", "结论维持，第二组数据不需要重算。", relation="reply"),
        ]},
        "context_notes": [
            {"key": "second-group", "text": "第二组结论以本机用户最新更正为准；旧结论 104 仍是已发送事实。"},
        ],
        "branch": {"reset_boundary": "before turn t1", "entries": ["t1 user: 刚才那个结论说维持，我搞错了", "t2 assistant: 我发更正给 dpdp"]},
    }
    return state, peers, threads


def _c07_expanded():
    state, peers, threads = _c07_focused()
    state["me"]["local"]["thread_view"]["entries"] = [
        _entry("099", "in", CAERULEA, "普通通信", "第二组表在这。"),
        _entry("101", "in", DPDP, "普通通信", "这轮先看结论。"),
        _entry("103", "in", DPDP, "等你看完给我结论", "请核对这次实验的结论。", mentions=["@lazuli"]),
        _entry("104", "out", None, "答复这条请求，结论如下", "结论维持，第二组数据不需要重算。", relation="reply"),
        _entry("105", "in", CAERULEA, "普通通信", "104 的结论我先引用了。"),
        _entry("106", "in", DPDP, "@lazuli", "@lazuli 那我按“维持”写进报告了。"),
        _entry("107", "in", REVIEWER, "普通通信", "报告我先不改。"),
    ]
    state["me"]["local"]["recorded"] = {"notes": "dpdp 已经按 104 写进报告草稿。"}
    return state, peers, threads


CASES.append(_case(
    "reply-07", "dev", "用户更正旧结论，新消息引用本节点自己的 104 并落到同一 thread", {
        "minimal": _c07_minimal, "focused": _c07_focused, "expanded": _c07_expanded,
    },
    {"relation": "reply", "target": "dpdp", "thread": "research", "response": "none"},
    note="the user's correction overrides the assistant's earlier commit; the earlier reply stays a fact",
))


# -- 8. 助手承诺不算结果，读者问题要回给读者 --------------------------------

def _c08_minimal():
    state = _base_state(
        _orig(ARJEN, IM, "107", "msg-31", "用户问：图里的单位是什么？", "问：图里的单位是什么？"),
        _intent("答复读者，先说明查证进度", "图例原始设置我这边没权限查，单位先按万元看，我确认了再补。", IM, "107", ARJEN, "msg-31"),
        {"state": "open", "my_access": ["post", "read"], "reply_authority": ARJEN, "scope_note": "post carried over from an earlier binding"},
        None,
        me_note={"delivery_kind": "用户问：图里的单位是什么？"},
    )
    peers = [
        _peer("arjen", ARJEN, relation="gateway-verified author of the im message"),
        _peer("dpdp", DPDP, relation="thread member elsewhere"),
    ]
    return state, _peers(peers), _threads([
        _thread("im-sea-ice", IM, state="open", my_access=["post", "read"], landing="holds the im message at seq 107"),
    ])


def _c08_focused():
    state, peers, threads = _c08_minimal()
    state["me"]["local"] = {
        "turns": [
            _turn("t1", "user", "把海冰群的单位问题答一下。"),
            _turn("t2", "assistant", "我在查图例的原始设置，单位我去核对一下再答。"),
            _turn("t3", "assistant", "我这边没法直接查图例。"),
        ],
        "resources": [{"alias": "figs-mcp", "state": "reachable", "calls": [{"operation": "get", "result": "error: resource not permitted"}]}],
    }
    return state, peers, threads


def _c08_expanded():
    state, peers, threads = _c08_focused()
    state["me"]["local"]["thread_view"] = {"thread_ref": IM, "entries": [
        _entry("106", "in", ARJEN, "普通通信", "新一期数据发群里了。"),
        _entry("107", "in", ARJEN, "用户问：图里的单位是什么？", "问：图里的单位是什么？", mentions=["@lazuli"]),
    ]}
    state["me"]["local"]["notes"] = "本机轮次 t2 说在核对图例，t3 说无法直接查；figs-mcp 的唯一调用结果是 error: resource not permitted。"
    peers["reviewer"] = _peer("reviewer", REVIEWER, relation="reviewer who asked about the same figure in another thread")
    return state, peers, threads


CASES.append(_case(
    "reply-08", "dev", "图例单位：助手承诺不算结果；读者的提问只能回到读者", {
        "minimal": _c08_minimal, "focused": _c08_focused, "expanded": _c08_expanded,
    },
    {"relation": "followup", "target": "arjen", "thread": "im-sea-ice", "response": "none"},
))


# -- 9. 追加说明：跟随上下文派生的子代理而不是另开一条 -----------------------

def _c09_minimal():
    state = _base_state(
        _orig(DPDP, RESEARCH, "118", "msg-41", "独立查一下，查完告诉我，我先做别的", "warmup 阶段的结论和正式版差多少？"),
        _intent("补充说明，接上一条", "补一句：只看第二阶段，第一阶段别算。", RESEARCH, "118", DPDP, "msg-41"),
        {"state": "open", "my_access": ["post", "read"], "reply_authority": DPDP},
        {"expected_author": DPDP, "expected_ref": {"sender": DPDP, "id": "msg-41"}, "deadline": "2026-10-24T10:00:00Z"},
        me_note={"contexts": [{"alias": "c-7", "root": {"sender": DPDP, "id": "msg-41"}, "state": "running"}]},
    )
    return state, _peers(ALL_PEERS), _threads([
        _thread("research", RESEARCH, state="open", my_access=["post", "read"], landing="holds the original at seq 118"),
    ])


def _c09_focused():
    state, peers, threads = _c09_minimal()
    state["me"]["local"] = {
        "turns": [_turn("t1", "user", "dpdp 那条你派个子代理去查。")],
        "recorded": {"waits": [{"expected_author": DPDP, "expected_ref": {"sender": DPDP, "id": "msg-41"}}]},
        "context_index": [
            {"alias": "c-7", "thread_ref": RESEARCH, "root": {"sender": DPDP, "id": "msg-41"}, "summary": "查 warmup 与正式版结论差", "state": "running", "session_file": "/sessions/research-103.jsonl"},
            {"alias": "c-3", "thread_ref": OPS, "root": {"sender": CAERULEA, "id": "msg-18"}, "summary": "归档旧记录", "state": "idle"},
        ],
        "todo_event": {
            "source": "omp-todo-more:read",
            "goal": "回答 dpdp 关于 warmup 与正式版差距的问题",
            "open_items": [{"title": "查 warmup 与正式版结论差", "status": "in_progress"}],
        },
        "source_record": {"thread_ref": RESEARCH, "message_ref": {"sender": DPDP, "id": "msg-41"}, "todo_title": "查 warmup 与正式版结论差"},
    }
    return state, peers, threads


def _c09_expanded():
    state, peers, threads = _c09_focused()
    state["me"]["local"]["thread_view"] = {"thread_ref": RESEARCH, "entries": [
        _entry("110", "in", CAERULEA, "普通通信", "warmup 数据在池子里。"),
        _entry("114", "out", None, "普通通信", "第二组先不动。"),
        _entry("116", "in", DPDP, "独立查一下，查完告诉我，我先做别的", "warmup 和正式版差多少？", mentions=["@lazuli"]),
        _entry("117", "in", CAERULEA, "普通通信", "第一阶段的数据我标好了。"),
        _entry("118", "in", DPDP, "独立查一下，查完告诉我，我先做别的", "warmup 阶段的结论和正式版差多少？", mentions=["@lazuli"]),
        _entry("119", "in", REVIEWER, "普通通信", "这个问题和我无关。"),
    ]}
    state["me"]["local"]["context_index"].append(
        {"alias": "c-9", "thread_ref": RESEARCH, "root": {"sender": CAERULEA, "id": "msg-77"}, "summary": "同上一条问题的旧子代理", "state": "done"})
    state["me"]["local"]["notes"] = "dpdp 上一条 116 也问过类似的话，那个子代理已经结束了。"
    return state, peers, threads


CASES.append(_case(
    "reply-09", "dev", "对上一条任务消息的追问，作为补充说明发往原 thread", {
        "minimal": _c09_minimal, "focused": _c09_focused, "expanded": _c09_expanded,
    },
    {"relation": "followup", "target": "dpdp", "thread": "research", "response": "none"},
))


# -- 10. 封闭 thread 的窄许可回复 -------------------------------------------

def _c10_minimal():
    state = _base_state(
        _orig(CAERULEA, OPS, "44", "msg-44", "记录一下", "旧记录已归档，结论已定。"),
        _intent("回复 caerulea", "收到。", OPS, "44", CAERULEA, "msg-44"),
        {"state": "closed", "my_access": [], "reply_authority": CAERULEA, "reply_permit": {"grantee": ME["node_id"], "revoked_at": None}},
        None,
    )
    peers = [
        _peer("caerulea", CAERULEA, relation="author and reply authority of the closed-thread message"),
        _peer("reviewer", REVIEWER, relation="thread member elsewhere"),
    ]
    return state, _peers(peers), _threads([
        _thread("ops", OPS, state="closed", my_access=[], enclosed="closed to new messages; a narrow reply permit targets the original", landing="holds the original at seq 44"),
        _thread("research", RESEARCH, state="open", my_access=["post", "read"]),
    ])


def _c10_focused():
    state, peers, threads = _c10_minimal()
    state["me"]["local"] = {
        "turns": [_turn("t1", "user", "运营那个归档记录你回个收到就完了。")],
        "recorded": {"permits": [{"message_ref": {"sender": CAERULEA, "id": "msg-44"}, "grantee": ME["node_id"], "expires_at": "2026-10-25T00:00:00Z", "revoked_at": None}]},
    }
    return state, peers, threads


def _c10_expanded():
    state, peers, threads = _c10_focused()
    state["me"]["local"]["notes"] = "research thread 今天很活跃，但那和这条归档记录无关。"
    peers["dpdp"] = _peer("dpdp", DPDP, relation="research thread member, active today")
    threads["review"] = _thread("review", REVIEW, state="open", my_access=[])
    return state, peers, threads


CASES.append(_case(
    "reply-10", "dev", "封闭 thread 中经窄许可回复，落点仍是原 thread", {
        "minimal": _c10_minimal, "focused": _c10_focused, "expanded": _c10_expanded,
    },
    {"relation": "followup", "target": "caerulea", "thread": "ops", "response": "none"},
))


# -- 11. 原生 thread 中拒绝请求 ---------------------------------------------

def _c11_minimal():
    state = _base_state(
        _orig(REVIEWER, REVIEW, "12", "msg-09", "请确认数据范围", "你给的范围包含 2025 年吗？"),
        _intent("答复 reviewer", "范围不包含 2025 年，这次只核 2024 年，2025 年这版不做。", REVIEW, "12", REVIEWER, "msg-09"),
        {"state": "open", "my_access": ["post", "read"], "reply_authority": REVIEWER},
        {"expected_author": REVIEWER, "expected_ref": {"sender": REVIEWER, "id": "msg-09"}, "deadline": "2026-10-24T12:00:00Z"},
    )
    peers = [
        _peer("reviewer", REVIEWER, relation="thread owner, author, and reply authority of the original"),
        _peer("dpdp", DPDP, relation="member of another thread"),
    ]
    return state, _peers(peers), _threads([
        _thread("review", REVIEW, state="open", my_access=["post", "read"], landing="holds the original at seq 12"),
        _thread("research", RESEARCH, state="open", my_access=["post", "read"]),
    ])


def _c11_focused():
    state, peers, threads = _c11_minimal()
    state["me"]["local"] = {
        "turns": [
            _turn("t1", "user", "2025 年这版不做，直接跟 reviewer 说清楚。"),
            _turn("t2", "assistant", "好，回复 12 说明范围只到 2024。"),
        ],
        "thread_view": {"thread_ref": REVIEW, "entries": [
            _entry("12", "in", REVIEWER, "请确认数据范围", "你给的范围包含 2025 年吗？", mentions=["@lazuli"]),
        ]},
        "context_notes": [
            {"key": "review-scope", "text": "reviewer 的数据范围这一版只到 2024 年。"},
        ],
    }
    return state, peers, threads


def _c11_expanded():
    state, peers, threads = _c11_focused()
    state["me"]["local"]["thread_view"]["entries"].insert(0, _entry("10", "in", REVIEWER, "普通通信", "报告初稿我先按 2024 写的。"))
    state["me"]["local"]["notes"] = "reviewer 在另一个 thread 里催过进度，和范围问题无关。"
    return state, peers, threads


CASES.append(_case(
    "reply-11", "dev", "明确拒绝范围也要算答复，解除对方等待", {
        "minimal": _c11_minimal, "focused": _c11_focused, "expanded": _c11_expanded,
    },
    {"relation": "reply", "target": "reviewer", "thread": "review", "response": "none"},
))


# -- 12. 关闭的 thread 上没有许可：不能顺手改发活跃 thread -------------------

def _c12_minimal():
    state = _base_state(
        _orig(CAERULEA, OPS, "44", "msg-44", "记录一下", "旧记录已归档。"),
        _intent("回复 caerulea", "收到。", OPS, "44", CAERULEA, "msg-44"),
        {"state": "closed", "my_access": [], "reply_authority": CAERULEA, "reply_permit": None},
        None,
    )
    peers = [
        _peer("caerulea", CAERULEA, relation="author of the closed-thread message"),
        _peer("dpdp", DPDP, relation="member of this node's open thread"),
    ]
    return state, _peers(peers), _threads([
        _thread("ops", OPS, state="closed", my_access=[], enclosed="closed to new messages; no reply permit targets the original"),
        _thread("research", RESEARCH, state="open", my_access=["post", "read"]),
    ])


def _c12_focused():
    state, peers, threads = _c12_minimal()
    state["me"]["local"] = {
        "turns": [_turn("t1", "user", "运营那个归档记录回一下。")],
        "recorded": {"permits": []},
    }
    return state, peers, threads


def _c12_expanded():
    state, peers, threads = _c12_focused()
    threads["ops"]["properties"]["enclosed"] = "closed four days ago; 4 entries after closure were refused"
    state["me"]["local"]["notes"] = "caerulea 同时是 research thread 的成员，research 目前可写。"
    return state, peers, threads


CASES.append(_case(
    "reply-12", "dev", "无许可的封闭 thread 回复：不猜落点", {
        "minimal": _c12_minimal, "focused": _c12_focused, "expanded": _c12_expanded,
    },
    {"relation": "followup", "target": "caerulea", "thread": "none", "response": "none"},
    note="the closed thread and the open substitute both stay in the candidate pool; deciding none is the test",
))


# -- 13. 原消息在读取范围外 -------------------------------------------------

def _c13_minimal():
    state = _base_state(
        _orig("urn:onlyne:agent:stranger", PRIVATE, "7", "msg-77", "秘密", "不在你的读取范围内。"),
        _intent("回复这条", "收到。", PRIVATE, "7", "urn:onlyne:agent:stranger", "msg-77"),
        {"state": "open", "my_access": [], "reply_authority": "urn:onlyne:agent:stranger", "scope_proof": "signature invalid for this node"},
        None,
    )
    peers = [
        _peer("dpdp", DPDP, relation="member of this node's threads"),
        _peer("caerulea", CAERULEA, relation="member of this node's threads"),
    ]
    return state, _peers(peers, none="Abstain: the original is outside this node's read scope, so no endpoint can be verified."), _threads([
        _thread("research", RESEARCH, state="open", my_access=["post", "read"]),
        _thread("ops", OPS, state="closed", my_access=[]),
    ], none="Abstain: the original thread is outside this node's read scope.")


def _c13_focused():
    state, peers, threads = _c13_minimal()
    state["me"]["local"] = {
        "turns": [_turn("t1", "user", "有人发来一条“私聊”，你回一下？")],
        "notes": "原消息不属于任何本地 thread，正文自称来自 stranger。",
    }
    return state, peers, threads


def _c13_expanded():
    state, peers, threads = _c13_focused()
    state["me"]["local"]["thread_view"] = {"thread_ref": RESEARCH, "entries": [
        _entry("130", "in", DPDP, "普通通信", "今天的评审会推迟。"),
    ]}
    peers["stranger"] = _peer("stranger", "urn:onlyne:agent:stranger", relation="claimed author of a message outside this node's read scope; claims are not verified")
    threads["private"] = _thread("private", PRIVATE, state="open", my_access=[])
    return state, peers, threads


CASES.append(_case(
    "reply-13", "dev", "原消息不在读取范围：不选权威也不选落点", {
        "minimal": _c13_minimal, "focused": _c13_focused, "expanded": _c13_expanded,
    },
    {"relation": "unresolved", "target": "none", "thread": "none", "response": "none"},
    note="the claimed author and both local threads stay as candidates; none is chosen because the original is unverifiable",
))


# -- 14. 对端答复本节点的提问：没有新问题 --------------------------------

def _c14_minimal():
    state = _base_state(
        _orig(DPDP, RESEARCH, "126", "msg-51", "答复你的问题", "第二组重算了，结论是不需要推翻。"),
        _intent("答复 dpdp，谢谢她", "收到，谢谢。", RESEARCH, "126", DPDP, "msg-51"),
        {"state": "open", "my_access": ["post", "read"], "reply_authority": DPDP},
        {"expected_author": DPDP, "expected_ref": {"sender": DPDP, "id": "msg-51"}, "deadline": "2026-10-24T14:00:00Z"},
        me_note={"delivery_kind": "第二组的结论出来了告诉我"},
    )
    return state, _peers(ALL_PEERS), _threads([
        _thread("research", RESEARCH, state="open", my_access=["post", "read"], landing="holds the original at seq 126"),
    ])


def _c14_focused():
    state, peers, threads = _c14_minimal()
    state["me"]["local"] = {
        "turns": [_turn("t1", "user", "问一下 dpdp 第二组的结论。")],
        "thread_view": {"thread_ref": RESEARCH, "entries": [
            _entry("125", "out", None, "问一下第二组结论", "第二组的结论出来了吗？", mentions=["@dpdp"]),
            _entry("126", "in", DPDP, "答复你的问题", "第二组重算了，结论是不需要推翻。", relation="reply"),
        ]},
        "recorded": {"waits": [{"expected_author": DPDP, "expected_ref": {"sender": DPDP, "id": "msg-51"}}]},
    }
    return state, peers, threads


def _c14_expanded():
    state, peers, threads = _c14_focused()
    state["me"]["local"]["thread_view"]["entries"] = [
        _entry("120", "in", CAERULEA, "普通通信", "第二组原始数据在这。"),
        _entry("122", "out", None, "普通通信", "第二组在跑。"),
        _entry("125", "out", None, "问一下第二组结论", "第二组的结论出来了吗？", mentions=["@dpdp"]),
        _entry("126", "in", DPDP, "答复你的问题", "第二组重算了，结论是不需要推翻。", relation="reply"),
        _entry("127", "in", CAERULEA, "普通通信", "第三组的数据明天到。"),
        _entry("128", "in", REVIEWER, "普通通信", "报告格式我再看。"),
    ]
    state["me"]["local"]["notes"] = "dpdp 在 126 末尾写了一句“有问题随时喊我”，那是客套，不是提问。"
    return state, peers, threads


CASES.append(_case(
    "reply-14", "holdout", "对方答复本节点的提问，回一句“收到”不构成新问题", {
        "minimal": _c14_minimal, "focused": _c14_focused, "expanded": _c14_expanded,
    },
    {"relation": "followup", "target": "dpdp", "thread": "research", "response": "none"},
))


# -- 15. 重发“结论如下”覆盖上一条误发 -------------------------------------

def _c15_minimal():
    state = _base_state(
        _orig(DPDP, RESEARCH, "103", "msg-01", "等你看完给我结论", "请核对这次实验的结论。"),
        _intent("答复这条请求，结论如下", "刚才发错了，以这条为准：结论是第二组需要重算。", RESEARCH, "103", DPDP, "msg-01"),
        {"state": "open", "my_access": ["post", "read"], "reply_authority": DPDP},
        {"expected_author": DPDP, "expected_ref": {"sender": DPDP, "id": "msg-01"}, "deadline": "2026-10-24T04:00:00Z"},
        me_note={"delivery_kind": "答复这条请求，结论如下"},
    )
    return state, _peers(ALL_PEERS), _threads([
        _thread("research", RESEARCH, state="open", my_access=["post", "read"], landing="holds the original at seq 103"),
    ])


def _c15_focused():
    state, peers, threads = _c15_minimal()
    state["me"]["local"] = {
        "turns": [_turn("t1", "user", "你刚才那条发错了，重发一条。")],
        "thread_view": {"thread_ref": RESEARCH, "entries": [
            _entry("103", "in", DPDP, "等你看完给我结论", "请核对这次实验的结论。", mentions=["@lazuli"]),
            _entry("104", "out", None, "答复这条请求，结论如下", "结论维持，不需要重算。", relation="reply"),
        ]},
    }
    return state, peers, threads


def _c15_expanded():
    state, peers, threads = _c15_focused()
    state["me"]["local"]["thread_view"]["entries"].append(_entry("105", "in", DPDP, "@lazuli", "@lazuli 那第二组到底要不要重算？"))
    state["me"]["local"]["notes"] = "104 已被 dpdp 读到。"
    return state, peers, threads


CASES.append(_case(
    "reply-15", "dev", "重发改正误发的结论，仍是对同一原消息的最终答复", {
        "minimal": _c15_minimal, "focused": _c15_focused, "expanded": _c15_expanded,
    },
    {"relation": "reply", "target": "dpdp", "thread": "research", "response": "none"},
))


# -- 16. 顺手交代的性质：给追加指令，不向对端提问 --------------------------

def _c16_minimal():
    state = _base_state(
        _orig(REVIEWER, REVIEW, "12", "msg-09", "请确认数据范围", "你给的范围包含 2025 年吗？"),
        _intent("答复 reviewer", "包含 2025 年 1 月到 12 月。顺便你这轮把图例单位也改一下，按万元。", REVIEW, "12", REVIEWER, "msg-09"),
        {"state": "open", "my_access": ["post", "read"], "reply_authority": REVIEWER},
        {"expected_author": REVIEWER, "expected_ref": {"sender": REVIEWER, "id": "msg-09"}, "deadline": "2026-10-24T12:00:00Z"},
    )
    peers = [
        _peer("reviewer", REVIEWER, relation="thread owner, author, and reply authority of the original"),
        _peer("dpdp", DPDP, relation="member of another thread"),
    ]
    return state, _peers(peers), _threads([
        _thread("review", REVIEW, state="open", my_access=["post", "read"], landing="holds the original at seq 12"),
    ])


def _c16_focused():
    state, peers, threads = _c16_minimal()
    state["me"]["local"] = {
        "turns": [_turn("t1", "user", "顺便让 reviewer 把图例单位改成万元。")],
        "thread_view": {"thread_ref": REVIEW, "entries": [
            _entry("12", "in", REVIEWER, "请确认数据范围", "你给的范围包含 2025 年吗？", mentions=["@lazuli"]),
        ]},
    }
    return state, peers, threads


def _c16_expanded():
    state, peers, threads = _c16_focused()
    state["me"]["local"]["thread_view"]["entries"].insert(0, _entry("11", "in", REVIEWER, "普通通信", "图例单位我这边统一成万元。"))
    threads["research"] = _thread("research", RESEARCH, state="open", my_access=["post", "read"])
    return state, peers, threads


CASES.append(_case(
    "reply-16", "dev", "答复里附加一条给对方的工作要求，不构成新的提问", {
        "minimal": _c16_minimal, "focused": _c16_focused, "expanded": _c16_expanded,
    },
    {"relation": "reply", "target": "reviewer", "thread": "review", "response": "none"},
))


# ================================================================ holdout


# -- 17. 委派：用户授权转给 caerulea，她是权威 ------------------------------

def _c17_minimal():
    state = _base_state(
        _orig(DPDP, RESEARCH, "103", "msg-01", "等你看完给我结论", "请核对这次实验的结论。"),
        _intent("答复这条请求，caerulea 那边代我回", "核对完了，结论由 caerulea 汇总后发你。", RESEARCH, "103", DPDP, "msg-01"),
        {"state": "open", "my_access": ["post", "read"], "reply_authority": CAERULEA, "reply_authority_basis": "binding forwards this message with caerulea designated as the reply authority"},
        {"expected_author": DPDP, "expected_ref": {"sender": DPDP, "id": "msg-01"}, "deadline": "2026-10-24T04:00:00Z"},
        me_note={"reply_binding": {"authority": CAERULEA, "revision": "rev-c-2"}},
    )
    peers = [
        _peer("dpdp", DPDP, relation="author of the original"),
        _peer("caerulea", CAERULEA, relation="designated reply authority by the current binding"),
    ]
    return state, _peers(peers), _threads([
        _thread("research", RESEARCH, state="open", my_access=["post", "read"], landing="holds the original at seq 103"),
    ])


def _c17_focused():
    state, peers, threads = _c17_minimal()
    state["me"]["local"] = {
        "turns": [_turn("t1", "user", "这次结论让 caerulea 来答，你只负责核对。")],
        "thread_view": {"thread_ref": RESEARCH, "entries": [
            _entry("103", "in", DPDP, "等你看完给我结论", "请核对这次实验的结论。", mentions=["@lazuli"]),
        ]},
        "recorded": {"outbound": [{"message_ref": {"sender": DPDP, "id": "msg-01"}, "reply_authority": CAERULEA, "binding_revision": "rev-c-2"}]},
    }
    return state, peers, threads


def _c17_expanded():
    state, peers, threads = _c17_focused()
    state["me"]["local"]["thread_view"]["entries"].append(_entry("104", "in", CAERULEA, "普通通信", "结论我这边整理，你别直接发 dpdp。"))
    state["me"]["local"]["notes"] = "dpdp 在 101 里说“结论直接找我”，但当前 binding 指定 caerulea。"
    return state, peers, threads


CASES.append(_case(
    "reply-17", "holdout", "转发绑定指定 caerulea 为权威，覆盖作者的默认期待", {
        "minimal": _c17_minimal, "focused": _c17_focused, "expanded": _c17_expanded,
    },
    {"relation": "reply", "target": "caerulea", "thread": "research", "response": "none"},
))


# -- 18. 订阅作业：结果未出，用户要求不等 ----------------------------------

def _c18_minimal():
    state = _base_state(
        _orig(OMLX_OPS, LAB, "20", "msg-61", "跑完告诉我", "decider 的压测什么时候能出结果？"),
        _intent("答复 job-42 的进度", "压测还在跑，大概还剩 40 分钟。", LAB, "20", OMLX_OPS, "msg-61"),
        {"state": "open", "my_access": ["post", "read"], "reply_authority": OMLX_OPS},
        {"expected_author": OMLX_OPS, "expected_ref": {"sender": OMLX_OPS, "id": "msg-61"}, "deadline": "2026-10-24T16:00:00Z"},
        me_note={"subscriptions": [{"resource_ref": {"owner": "urn:onlyne:compute:lab", "id": "job-42"}, "state": "running", "result": None}]},
    )
    peers = [
        _peer("omlx-ops", OMLX_OPS, relation="author and reply authority of the original"),
        _peer("caerulea", CAERULEA, relation="member of a different thread"),
    ]
    return state, _peers(peers), _threads([
        _thread("lab", LAB, state="open", my_access=["post", "read"], landing="holds the original at seq 20"),
    ])


def _c18_focused():
    state, peers, threads = _c18_minimal()
    state["me"]["local"] = {
        "turns": [
            _turn("t1", "user", "omlx 那边问压测时间，你回一下，别停在这等结果。"),
            _turn("t2", "assistant", "好，我先报进度，结果出来再发。"),
        ],
        "recorded": {"resources": [{"alias": "job-42", "operation": "describe", "result": {"state": "running", "eta_minutes": 40}}]},
        "todo_event": {
            "source": "omp-todo-more:read",
            "goal": "把 decider 压测结果交给 omlx-ops",
            "open_items": [{"title": "等 job-42 压测结果", "status": "in_progress"}],
        },
        "source_record": {"thread_ref": LAB, "message_ref": {"sender": OMLX_OPS, "id": "msg-61"}, "todo_title": "等 job-42 压测结果"},
    }
    return state, peers, threads


def _c18_expanded():
    state, peers, threads = _c18_focused()
    state["me"]["local"]["notes"] = "用户说过，作业跑完直接发结果，不必等用户确认。"
    state["me"]["local"]["recorded"]["resources"].append({"alias": "job-41", "operation": "describe", "result": {"state": "done"}})
    threads["ops"] = _thread("ops", OPS, state="closed", my_access=[])
    return state, peers, threads


CASES.append(_case(
    "reply-18", "holdout", "订阅作业未出结果，按要求只报进度并继续查看", {
        "minimal": _c18_minimal, "focused": _c18_focused, "expanded": _c18_expanded,
    },
    {"relation": "followup", "target": "omlx-ops", "thread": "lab", "response": "none"},
))


# -- 19. 用户原话“等我看完” -> 本节点等对端用户 ---------------------------

def _c19_minimal():
    state = _base_state(
        _orig(DPDP, RESEARCH, "130", "msg-69", "等我看完给你回复", "文档改完了，你看下格式对不对。"),
        _intent("答复 dpdp，格式没问题，让她确认一下要不要按这个发", "格式没问题。按这个发吧，你看完回我一句就行。", RESEARCH, "130", DPDP, "msg-70"),
        {"state": "open", "my_access": ["post", "read"], "reply_authority": DPDP},
        None,
        me_note={"wait": {"expected_author": DPDP, "expected_ref": {"sender": DPDP, "id": "msg-70"}, "deadline": "2026-10-24T18:00:00Z"}},
    )
    return state, _peers(ALL_PEERS), _threads([
        _thread("research", RESEARCH, state="open", my_access=["post", "read"], landing="holds the original at seq 130"),
    ])


def _c19_focused():
    state, peers, threads = _c19_minimal()
    state["me"]["local"] = {
        "turns": [_turn("t1", "user", "格式你看一下，没问题就让她按这个发，让她回你一句。")],
        "thread_view": {"thread_ref": RESEARCH, "entries": [
            _entry("130", "in", DPDP, "等我看完给你回复", "文档改完了，你看下格式对不对。", mentions=["@lazuli"]),
        ]},
    }
    return state, peers, threads


def _c19_expanded():
    state, peers, threads = _c19_focused()
    state["me"]["local"]["thread_view"]["entries"].insert(0, _entry("129", "in", CAERULEA, "普通通信", "文档我过了一遍。"))
    state["me"]["local"]["notes"] = "本节点之前等待过 129 的原消息，那次等待已超时结束。"
    return state, peers, threads


CASES.append(_case(
    "reply-19", "holdout", "答复里明确请对端回一句，本节点为用户停下等待", {
        "minimal": _c19_minimal, "focused": _c19_focused, "expanded": _c19_expanded,
    },
    {"relation": "reply", "target": "dpdp", "thread": "research", "response": "await"},
))


# -- 20. 正文自问自答，实际等对端回复 -------------------------------------

def _c20_minimal():
    state = _base_state(
        _orig(REVIEWER, REVIEW, "15", "msg-81", "看看要不要改", "投稿前要不要把图换成新版本？"),
        _intent("答复这条", "要换新图。新版本你审完给我个结论，我等你。", REVIEW, "15", REVIEWER, "msg-81"),
        {"state": "open", "my_access": ["post", "read"], "reply_authority": REVIEWER},
        None,
        me_note={"wait": {"expected_author": REVIEWER, "expected_ref": {"sender": REVIEWER, "id": "msg-81"}, "deadline": "2026-10-25T00:00:00Z"}},
    )
    peers = [
        _peer("reviewer", REVIEWER, relation="thread owner, author, and reply authority of the original"),
        _peer("caerulea", CAERULEA, relation="member of another thread"),
    ]
    return state, _peers(peers), _threads([
        _thread("review", REVIEW, state="open", my_access=["post", "read"], landing="holds the original at seq 15"),
    ])


def _c20_focused():
    state, peers, threads = _c20_minimal()
    state["me"]["local"] = {
        "turns": [_turn("t1", "user", "图换成新的，审完让她回你结论。")],
        "thread_view": {"thread_ref": REVIEW, "entries": [
            _entry("15", "in", REVIEWER, "看看要不要改", "投稿前要不要把图换成新版本？", mentions=["@lazuli"]),
        ]},
    }
    return state, peers, threads


def _c20_expanded():
    state, peers, threads = _c20_focused()
    state["me"]["local"]["thread_view"]["entries"].append(_entry("16", "in", REVIEWER, "普通通信", "图我这边先没动。"))
    state["me"]["local"]["notes"] = "reviewer 回复通常要一天，上次她隔天才回。"
    return state, peers, threads


CASES.append(_case(
    "reply-20", "holdout", "用户措辞“我等你”使本节点在库等待上停住", {
        "minimal": _c20_minimal, "focused": _c20_focused, "expanded": _c20_expanded,
    },
    {"relation": "reply", "target": "reviewer", "thread": "review", "response": "await"},
))


# -- 21. 视图：等待由自动片段先注入并确认消费 -----------------------------

def _c21_minimal():
    state = _base_state(
        _orig(CAERULEA, RESEARCH, "104", "msg-02", "答复一下范围", "范围确认了，按 2024 年算。"),
        _intent("答复 caerulea", "好，那就按 2024。", RESEARCH, "104", CAERULEA, "msg-02"),
        {"state": "open", "my_access": ["post", "read"], "reply_authority": CAERULEA},
        {"expected_author": CAERULEA, "expected_ref": {"sender": CAERULEA, "id": "msg-02"}, "deadline": "2026-10-24T20:00:00Z"},
        me_note={"consumed": {"context": "main", "thread_ref": RESEARCH, "seq": "104", "mode": "fragment", "confirmed": True}},
    )
    peers = [
        _peer("caerulea", CAERULEA, relation="author of the original reply and the message being replied to"),
        _peer("dpdp", DPDP, relation="thread member"),
    ]
    return state, _peers(peers), _threads([
        _thread("research", RESEARCH, state="open", my_access=["post", "read"], landing="holds the original at seq 104"),
    ])


def _c21_focused():
    state, peers, threads = _c21_minimal()
    state["me"]["local"]["turns"] = [_turn("t1", "user", "104 那条自动片段已经看到了，回一句。")]
    state["me"]["local"]["thread_view"] = {"thread_ref": RESEARCH, "entries": [
        _entry("104", "in", CAERULEA, "答复一下范围", "范围确认了，按 2024 年算。", relation="reply"),
        _entry("107", "out", None, "问范围", "我等 104 的范围确认。", mentions=["@caerulea"]),
    ]}
    return state, peers, threads


def _c21_expanded():
    state, peers, threads = _c21_focused()
    state["me"]["local"]["thread_view"]["entries"].append(_entry("108", "in", DPDP, "普通通信", "第二组的事先放一放。"))
    state["me"]["local"]["notes"] = "模型在本地读 thread 时用的是完整历史模式，但 104 已经由片段注入过。"
    threads["ops"] = _thread("ops", OPS, state="closed", my_access=[])
    return state, peers, threads


def _c21_view():
    return {
        "result_ref_is_full": _message("full-by-ref", {"sender": CAERULEA, "id": "msg-02"}, payload="full body", consumption="fragment delivered this entry and the agent confirmed reading it"),
        "result_ref_only": _message("ref-only", {"sender": CAERULEA, "id": "msg-02"}, payload="no body", consumption="same entry; body already delivered by the fragment"),
        "replay_full": _message("replay", {"sender": CAERULEA, "id": "msg-02"}, payload="full body", consumption="entry not yet delivered to any context of this node"),
        "local_read": _message("local-read", {"sender": CAERULEA, "id": "msg-02"}, payload="full thread history page", consumption="a local read request, not the wait result"),
        "no_delivery": _message("none", {"sender": CAERULEA, "id": "msg-02"}, payload="no body", consumption="the reply is persisted but nothing is delivered to the agent"),
    }


def _c21_profiles():
    out = {}
    for name, builder in (("minimal", _c21_minimal), ("focused", _c21_focused), ("expanded", _c21_expanded)):
        state, peers, threads = builder()
        out[name] = {"state": state, "candidates": {"peer": peers, "thread": threads, "view": _c21_view()}}
    return out


CASES.append(_case(
    "reply-21", "dev", "等待的回复已由自动片段确认消费，结果只回到达事实", _c21_profiles(),
    {"relation": "followup", "target": "caerulea", "thread": "research", "response": "none", "view": "result_ref_only"},
))


# -- 22. 视图：等待结果的正文尚未消费 ------------------------------------

def _c22_minimal():
    state = _base_state(
        _orig(DPDP, RESEARCH, "140", "msg-89", "答复你的问题", "第三组的结论出来了。"),
        _intent("答复 dpdp", "太好了，收到。", RESEARCH, "140", DPDP, "msg-89"),
        {"state": "open", "my_access": ["post", "read"], "reply_authority": DPDP},
        {"expected_author": DPDP, "expected_ref": {"sender": DPDP, "id": "msg-90"}, "deadline": "2026-10-24T22:00:00Z"},
        me_note={"consumed": {"context": "main", "thread_ref": RESEARCH, "seq": "140", "mode": None, "confirmed": False}},
    )
    return state, _peers(ALL_PEERS), _threads([
        _thread("research", RESEARCH, state="open", my_access=["post", "read"], landing="holds the original at seq 140"),
    ])


def _c22_focused():
    state, peers, threads = _c22_minimal()
    state["me"]["local"] = {
        "turns": [_turn("t1", "user", "第三组的事你问了 dpdp 吧？")],
        "thread_view": {"thread_ref": RESEARCH, "entries": [
            _entry("140", "in", DPDP, "答复你的问题", "第三组的结论出来了。", relation="reply"),
            _entry("143", "out", None, "问第三组结论", "第三组的结论出来了吗？", mentions=["@dpdp"]),
        ]},
    }
    return state, peers, threads


def _c22_expanded():
    state, peers, threads = _c22_focused()
    state["me"]["local"]["thread_view"]["entries"].append(_entry("144", "in", CAERULEA, "普通通信", "第三组数据我复核过了。"))
    state["me"]["local"]["notes"] = "注入游标停在 139，140 还没进过任何上下文。"
    state["me"]["local"]["branch"] = {
        "reset_boundary": "after turn t1",
        "entries": ["t1 user: 第三组的事你问了 dpdp 吧？"],
    }
    return state, peers, threads


def _c22_view():
    return {
        "full-by-ref": _message("full-by-ref", {"sender": DPDP, "id": "msg-90"}, payload="full body", consumption="not yet delivered to any context of this node"),
        "ref-only": _message("ref-only", {"sender": DPDP, "id": "msg-90"}, payload="no body", consumption="not yet delivered; body still unread"),
        "replay": _message("replay", {"sender": DPDP, "id": "msg-90"}, payload="full body", consumption="this node authored the message, so it is its own outbound record"),
        "high-fragment": _message("fragment", {"sender": DPDP, "id": "msg-90"}, payload="full body with the fragment header", consumption="not yet delivered; the next fragment would carry it"),
        "no-delivery": _message("none", {"sender": DPDP, "id": "msg-90"}, payload="no body", consumption="nothing is delivered after the reply lands"),
    }


def _c22_profiles():
    out = {}
    for name, builder in (("minimal", _c22_minimal), ("focused", _c22_focused), ("expanded", _c22_expanded)):
        state, peers, threads = builder()
        out[name] = {"state": state, "candidates": {"peer": peers, "thread": threads, "view": _c22_view()}}
    return out


CASES.append(_case(
    "reply-22", "holdout", "等待回复尚未消费，结果必须带全文", _c22_profiles(),
    {"relation": "followup", "target": "dpdp", "thread": "research", "response": "none", "view": "full-by-ref"},
))


# -- 23. 视图：本节点自己发的消息不回到自己 --------------------------------

def _c23_minimal():
    state = _base_state(
        _orig(ME["node_id"], RESEARCH, "150", "msg-98", "补充说明", "第三组我改成明天再跑。"),
        _intent("答复自己那条", "好，那就明天跑。", RESEARCH, "150", ME["node_id"], "msg-a0"),
        {"state": "open", "my_access": ["post", "read"], "reply_authority": ME["node_id"]},
        {"expected_author": ME["node_id"], "expected_ref": {"sender": ME["node_id"], "id": "msg-a0"}, "deadline": "2026-10-25T02:00:00Z"},
        me_note={"consumed": {"context": "main", "thread_ref": RESEARCH, "seq": "150", "mode": None, "confirmed": False}},
    )
    peers = [
        _peer("dpdp", DPDP, relation="thread member"),
        _peer("lazuli", ME["node_id"], relation="this node; author of the original"),
    ]
    return state, _peers(peers), _threads([
        _thread("research", RESEARCH, state="open", my_access=["post", "read"], landing="holds the original at seq 150"),
    ])


def _c23_focused():
    state, peers, threads = _c23_minimal()
    state["me"]["local"] = {
        "turns": [_turn("t1", "user", "那就明天跑吧。")],
        "thread_view": {"thread_ref": RESEARCH, "entries": [
            _entry("150", "out", None, "补充说明", "第三组我改成明天再跑。"),
            _entry("151", "in", DPDP, "普通通信", "明天我没空，数据放池子里。"),
        ]},
    }
    return state, peers, threads


def _c23_expanded():
    state, peers, threads = _c23_focused()
    state["me"]["local"]["notes"] = "dpdp 在 151 说数据照旧；151 没有 @ 本节点，也没有引用 150。"
    return state, peers, threads


def _c23_view():
    return {
        "result_ref_is_full": _message("full-by-ref", {"sender": ME["node_id"], "id": "msg-a0"}, payload="full body", consumption="the agent wrote this message; its own outbound record"),
        "result_ref_only": _message("ref-only", {"sender": ME["node_id"], "id": "msg-a0"}, payload="no body", consumption="the agent wrote this message; nothing came in"),
        "replay_full": _message("replay", {"sender": ME["node_id"], "id": "msg-a0"}, payload="full body", consumption="not yet delivered to any context"),
        "local_read": _message("local-read", {"sender": ME["node_id"], "id": "msg-a0"}, payload="full thread history page", consumption="a local read request"),
    }


def _c23_profiles():
    out = {}
    for name, builder in (("minimal", _c23_minimal), ("focused", _c23_focused), ("expanded", _c23_expanded)):
        state, peers, threads = builder()
        out[name] = {"state": state, "candidates": {"peer": peers, "thread": threads, "view": _c23_view()}}
    return out


CASES.append(_case(
    "reply-23", "holdout", "回复自己发出的消息，不重复向自己投递", _c23_profiles(),
    {"relation": "followup", "target": "lazuli", "thread": "research", "response": "none", "view": "result_ref_only"},
))


# -- 24. 视图：本地读历史模式不改变等待结果的投递 -------------------------

def _c24_minimal():
    state = _base_state(
        _orig(REVIEWER, REVIEW, "12", "msg-09", "请确认数据范围", "你给的范围包含 2025 年吗？"),
        _intent("答复这条", "包含 2025 年 1 月到 12 月。", REVIEW, "12", REVIEWER, "msg-09"),
        {"state": "open", "my_access": ["post", "read"], "reply_authority": REVIEWER},
        {"expected_author": REVIEWER, "expected_ref": {"sender": REVIEWER, "id": "msg-09"}, "deadline": "2026-10-25T04:00:00Z"},
        me_note={"consumed": {"context": "main", "thread_ref": REVIEW, "seq": "12", "mode": "thread", "confirmed": False}, "next_read": {"attention": "thread", "thread_ref": REVIEW}},
    )
    peers = [
        _peer("reviewer", REVIEWER, relation="thread owner, author, and reply authority of the original"),
        _peer("dpdp", DPDP, relation="member of another thread"),
    ]
    return state, _peers(peers), _threads([
        _thread("review", REVIEW, state="open", my_access=["post", "read"], landing="holds the original at seq 12"),
        _thread("research", RESEARCH, state="open", my_access=["post", "read"]),
    ])


def _c24_focused():
    state, peers, threads = _c24_minimal()
    state["me"]["local"] = {
        "turns": [_turn("t1", "user", "review 的回答你回一下。")],
        "thread_view": {"thread_ref": REVIEW, "entries": [
            _entry("12", "in", REVIEWER, "请确认数据范围", "你给的范围包含 2025 年吗？", mentions=["@lazuli"]),
        ]},
    }
    return state, peers, threads


def _c24_expanded():
    state, peers, threads = _c24_focused()
    state["me"]["local"]["notes"] = "用户说等 review 的回复，读消息时用完整历史模式。"
    threads["ops"] = _thread("ops", OPS, state="closed", my_access=[])
    return state, peers, threads


def _c24_view():
    return {
        "full-by-ref": _message("full-by-ref", {"sender": REVIEWER, "id": "msg-09"}, payload="full body", consumption="not yet delivered as a wait result"),
        "thread-read": _message("thread-read", {"sender": REVIEWER, "id": "msg-09"}, payload="full thread history page", consumption="a local read request in full-history mode"),
        "fragment": _message("fragment", {"sender": REVIEWER, "id": "msg-09"}, payload="full body with the fragment header", consumption="not yet delivered; the next fragment would carry it"),
        "no-delivery": _message("none", {"sender": REVIEWER, "id": "msg-09"}, payload="no body", consumption="nothing is delivered after the reply lands"),
    }


def _c24_profiles():
    out = {}
    for name, builder in (("minimal", _c24_minimal), ("focused", _c24_focused), ("expanded", _c24_expanded)):
        state, peers, threads = builder()
        out[name] = {"state": state, "candidates": {"peer": peers, "thread": threads, "view": _c24_view()}}
    return out


CASES.append(_case(
    "reply-24", "holdout", "本地读用完整历史模式，等待结果仍需按引用投递全文", _c24_profiles(),
    {"relation": "reply", "target": "reviewer", "thread": "review", "response": "none", "view": "full-by-ref"},
))
