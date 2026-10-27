"""Structure decision slice: thread and domain plans under real project context.

Every case is a hand-authored scenario reconstruction built from RFC 0002
(threads and membership), RFC 0004 (message views), RFC 0008 (the OMP surface)
and RFC 0011 (domains: one active membership, no owner, discovery never treats
unreachable as left). None of these are captured production traces; the
provenance string on each case names the rule it exercises.

The slice decides two semantic plans:

- thread: create a thread, reuse one existing thread, read one existing thread
  or the message list, add or stop one peer's thread membership, ask for
  clarification, or no thread operation.
- domain: create, join, leave, list, describe, discover, ask for clarification,
  or no domain operation.

Permissions, revisions, idempotency and unreachable-versus-left are library
facts, not model answers. The plan stays the same when the library would refuse
it; the expected labels never encode an ACL outcome.

Profiles:

- minimal: request, node identity, and the candidate identifiers with title and
  state. No membership, histories, goal, assistant context, or topology.
- focused: the task-specific evidence a real plan uses: current membership,
  role topology, thread histories and member rows, domain observations,
  the user's current goal, assistant proposals and todo.
- expanded: focused plus genuine distractors (workspace path, resource nodes,
  stale notes, untrusted peer text, superseded decisions). Candidate sets do not
  change between profiles, so no profile can remove a competing option.

Every choice keeps at least two alternatives; candidate criteria only restate
observed candidate properties. No expected label, rationale or route hint is
placed in a request.
"""
from __future__ import annotations

import copy

LAZULI = "urn:onlyne:agent:lazuli"
DPDP = "urn:onlyne:agent:dpdp"
CAERULEA = "urn:onlyne:agent:caerulea"
ALEXANDRIA = "urn:onlyne:agent:alexandria"
REVIEWER = "urn:onlyne:agent:reviewer"
TERN3 = "urn:onlyne:agent:tern-3"

D_RESEARCH_CHAMBER = "urn:onlyne:domain:research-chamber"
D_RESEARCH = "urn:onlyne:domain:research"
D_SEA_ICE = "urn:onlyne:domain:sea-ice"
D_PACKAGING = "urn:onlyne:domain:packaging"
D_ENCAPS = "urn:onlyne:domain:encaps-2026"

AT = "2026-10-24T02:00:00Z"
EARLIER = "2026-10-23T09:10:00Z"


# ---------------------------------------------------------------- helpers

def member(peer, *, state="active", access=("post", "read"), scope=None, relation="member"):
    return {"peer": peer, "state": state, "access": list(access),
            "read_scope": dict(scope or {}), "relation": relation}


def message(seq, sender, text, *, at, mentions=(), in_reply_to=None):
    return {"seq": seq, "sender": sender, "at": at, "text": text,
            "mentions": list(mentions), "in_reply_to": in_reply_to}


def thread(thread_id, title, *, owner=LAZULI, state="open", members, recent=(),
           unread=0, closed_at=None):
    return {
        "thread_ref": {"owner": owner, "id": thread_id},
        "title": title,
        "state": state,
        "my_role": "owner" if owner == LAZULI else "member",
        "members": list(members),
        "recent": list(recent),
        "unread": unread,
        "closed_at": closed_at,
    }


def peer(node_id, display, domain, *, presence="reachable", last_seen="2026-10-24T00:12:00Z"):
    return {"node_id": node_id, "display": display, "domain": domain,
            "presence": presence, "last_seen": last_seen}


def domain_record(domain_id, state, revision, *, recorded_at="2026-10-20T04:00:00Z"):
    return {"domain_id": domain_id, "node_id": LAZULI, "state": state,
            "revision": revision, "recorded_at": recorded_at}


def domain_candidate(domain_id, *, active=(), left=(), unavailable=(),
                     source="plugin domainCandidates"):
    return {"domain_id": domain_id, "source": source,
            "observations": {"active": list(active), "left": list(left),
                             "unavailable": list(unavailable)}}


def goal(current, *, corrections=(), stated_at=AT):
    return {"current": current, "stated_by": "user", "stated_at": stated_at,
            "corrections": list(corrections)}


def assistant(proposals, todo):
    return {
        "recent_proposals": [
            {"text": text, "at": at, "authority": "assistant proposal; not a user decision"}
            for text, at in proposals
        ],
        "todo": [{"text": text, "phase": "current"} for text in todo],
    }


def context(*, cwd, project, notes=(), claims=(), resources=(), decisions=()):
    return {
        "workspace": {"cwd": cwd, "project": project},
        "context_notes": [
            {"text": text, "at": at, "source": "context note", "freshness": freshness}
            for text, at, freshness in notes
        ],
        "peer_claims": [
            {"from": who, "text": text, "at": at, "authority": "peer message; untrusted text"}
            for who, text, at in claims
        ],
        "resources": list(resources),
        "stale_decisions": [
            {"at": at, "point": point, "chosen": chosen, "note": note}
            for point, chosen, at, note in decisions
        ],
    }


# ---------------------------------------------------------------- questions

THREAD_OPS_INITIAL = {
    "create": "A new thread is started.",
    "reuse": "An existing thread is used for a message.",
    "read": "An existing thread is inspected.",
    "member_add": "A peer is added to a thread.",
    "member_remove": "A peer is removed from a thread.",
    "clarify": "The request is ambiguous.",
    "none": "No thread operation applies.",
}

THREAD_OPS_TUNED = {
    "create": "Start a new thread owned by this node. This is the operation when the request asks for a separate place.",
    "reuse": "Post into one existing thread that the request names or that the current goal identifies. The target stays the named thread even when the library would refuse it; the planner never silently redirects into another thread.",
    "read": "Inspect one existing thread or this node's message list: list, describe, page, or read. Nothing is written.",
    "member_add": "Add one named peer to one existing thread. A request that a named peer see a thread is this operation: a join is how a peer gains read. A join grants post and read together; a narrower read_scope is an owner setting made after the membership exists.",
    "member_remove": "Stop one named peer's membership in one existing thread. Whether this node may do it is a library check; the operation stays member_remove.",
    "clarify": "Two or more existing threads match the named scope equally and nothing in the context prefers one; ask the user which.",
    "none": "The request is not a thread structure operation.",
}

DOMAIN_OPS_INITIAL = {
    "create": "A new domain is created.",
    "join": "This node joins a domain.",
    "leave": "This node leaves a domain.",
    "list": "This node's domains are listed.",
    "describe": "A domain is described.",
    "discover": "Domain members are discovered.",
    "clarify": "The request is ambiguous.",
    "none": "No domain operation applies.",
}

DOMAIN_OPS_TUNED = {
    "create": "Allocate a new domain identifier and record this node's own active membership in it. Peers record their own memberships, so a request to include peers adds no second operation here.",
    "join": "Record this node's own active membership in one named domain identifier.",
    "leave": "Record this node's own membership as left in one named domain.",
    "list": "Return this node's own membership records when no domain identifier is named.",
    "describe": "Return this node's own record for one named domain identifier.",
    "discover": "Ask candidate nodes to confirm their membership in the domain this node is active in.",
    "clarify": "Two or more domain identifiers match the named scope equally and nothing in the context prefers one; ask the user which.",
    "none": "No domain operation applies: a domain operation changes or reads only this node's own membership record.",
}

THREAD_OP_INSTRUCTIONS = {
    "initial": "Which thread operation does the request ask for?",
    "tuned": "Which single thread structure operation does the request ask for? Answer the semantic operation; membership validity and permissions are checked by the library after this choice.",
}

THREAD_INSTRUCTIONS = {
    "initial": "Which existing thread does the operation name?",
    "tuned": "Which one existing thread does the operation name? A create names no existing thread, so an older thread mentioned as context is not its target; a message list that spans threads names none either.",
}

PEER_INSTRUCTIONS = {
    "initial": "Which peer does the operation name?",
    "tuned": "Which peer does the membership operation name? For every other thread operation, choose none.",
}

DOMAIN_OP_INSTRUCTIONS = {
    "initial": "Which domain operation does the request ask for?",
    "tuned": "Which single domain membership operation does the request ask for? The operation always acts on this node's own membership record; another node's record is never its target. Answer the semantic operation; authentication, revisions and reachability are checked by the library after this choice.",
}

DOMAIN_INSTRUCTIONS = {
    "initial": "Which domain does the operation name?",
    "tuned": "Which one domain does the operation name? For a create, a list, or a request that names no domain identifier, choose none.",
}


def _thread_choices(case, revision):
    options = {}
    for item in case["_data"]["threads"]:
        thread_id = item["thread_ref"]["id"]
        if revision == "initial":
            options[thread_id] = f"thread {thread_id}; title={item['title']}"
        else:
            owner = "this node" if item["thread_ref"]["owner"] == LAZULI else item["thread_ref"]["owner"]
            members = ",".join(row["peer"] for row in item["members"])
            options[thread_id] = (
                f"thread {thread_id}; title={item['title']}; owner={owner}; "
                f"state={item['state']}; members={members}"
            )
    options["none"] = ("no single existing thread is named"
                       if revision == "initial"
                       else "no single existing thread is named: the request creates one or spans the message list")
    return options


def _peer_choices(case, revision):
    options = {}
    for item in case["_data"]["topology"]:
        if revision == "initial":
            options[item["display"]] = f"peer {item['display']}; node={item['node_id']}"
        else:
            options[item["display"]] = (
                f"peer {item['display']}; node={item['node_id']}; domain={item['domain']}; "
                f"presence={item['presence']}"
            )
    options["none"] = ("no peer is named"
                       if revision == "initial"
                       else "no peer is named by a membership operation")
    return options


def _domain_choices(case, revision):
    data = case["_data"]
    seen = []
    for item in data["domains"]["candidates"]:
        seen.append((item["domain_id"], item["source"]))
    if data["domains"]["current"] is not None:
        seen.append((data["domains"]["current"]["domain_id"], "this node's membership record"))
    for item in data["domains"]["records"]:
        seen.append((item["domain_id"], "this node's membership record"))
    options = {}
    for domain_id, source in seen:
        if domain_id in options:
            continue
        options[domain_id] = (f"domain {domain_id}" if revision == "initial"
                              else f"domain {domain_id}; source={source}")
    options["none"] = ("no single domain is named"
                       if revision == "initial"
                       else "no single domain identifier is named by the operation")
    return options


def questions(case, revision):
    if revision not in ("initial", "tuned"):
        raise ValueError(f"unknown revision: {revision}")
    if case["point"] == "thread":
        return {
            "op": {"type": "choice",
                   "instructions": THREAD_OP_INSTRUCTIONS[revision],
                   "criteria": THREAD_OPS_INITIAL if revision == "initial" else THREAD_OPS_TUNED},
            "thread": {"type": "choice",
                       "instructions": THREAD_INSTRUCTIONS[revision],
                       "criteria": _thread_choices(case, revision)},
            "peer": {"type": "choice",
                     "instructions": PEER_INSTRUCTIONS[revision],
                     "criteria": _peer_choices(case, revision)},
        }
    if case["point"] == "domain":
        return {
            "op": {"type": "choice",
                   "instructions": DOMAIN_OP_INSTRUCTIONS[revision],
                   "criteria": DOMAIN_OPS_INITIAL if revision == "initial" else DOMAIN_OPS_TUNED},
            "domain": {"type": "choice",
                       "instructions": DOMAIN_INSTRUCTIONS[revision],
                       "criteria": _domain_choices(case, revision)},
        }
    raise ValueError(f"unknown point: {case['point']}")


# ---------------------------------------------------------------- profiles

def _state(data, profile):
    if profile == "minimal":
        return copy.deepcopy({
            "intent": data["intent"],
            "node": {"node_id": LAZULI, "display": "lazuli"},
            "candidates": {
                "threads": [
                    {"thread_ref": item["thread_ref"], "title": item["title"], "state": item["state"]}
                    for item in data["threads"]
                ],
                "domains": [{"domain_id": item["domain_id"]} for item in data["domains"]["candidates"]],
            },
        })
    state = {
        "intent": data["intent"],
        "node": {
            "node_id": LAZULI,
            "display": "lazuli",
            "domain": (data["domains"]["current"] or {}).get("domain_id"),
        },
        "goal": data["goal"],
        "assistant": data["assistant"],
        "topology": data["topology"],
        "threads": data["threads"],
        "domains": data["domains"],
    }
    if profile == "expanded":
        state["context"] = data["context"]
    return copy.deepcopy(state)


def case(case_id, point, split, provenance, intent, expected, *, goal, assistant,
         topology, threads, domains, context):
    data = {
        "intent": {"from": "user", "at": AT, "text": intent},
        "goal": goal,
        "assistant": assistant,
        "topology": topology,
        "threads": threads,
        "domains": domains,
        "context": context,
    }
    return {
        "id": case_id,
        "point": point,
        "split": split,
        "provenance": provenance,
        "profiles": {profile: _state(data, profile) for profile in ("minimal", "focused", "expanded")},
        "expected": expected,
        "_data": data,
    }


# ---------------------------------------------------------------- peers

def rc_peers(*_, **__):
    return [
        peer(DPDP, "dpdp", D_RESEARCH_CHAMBER),
        peer(CAERULEA, "caerulea", D_RESEARCH_CHAMBER),
        peer(ALEXANDRIA, "alexandria", D_RESEARCH_CHAMBER),
        peer(REVIEWER, "reviewer", D_RESEARCH_CHAMBER),
    ]


def rc_domains(*, current=None, records=(), extra=()):
    candidates = [
        domain_candidate(D_RESEARCH_CHAMBER, active=("alexandria", "caerulea")),
        domain_candidate(D_RESEARCH, active=("tern-3",)),
        domain_candidate(D_PACKAGING, active=("dpdp",)),
    ]
    candidates.extend(extra)
    return {"current": current, "records": list(records), "candidates": candidates}


# ---------------------------------------------------------------- threads

SEA_ICE = thread(
    "thr-sea-ice-review", "海冰评审",
    members=[member("lazuli", relation="owner"), member("dpdp"), member("caerulea")],
    recent=[
        message("40", "dpdp", "第一组的对照表我放这了，阈值还是 0.7",
                at="2026-10-23T08:20:00Z"),
        message("41", "caerulea", "第二组我按 0.8 阈值算的，要改我重跑",
                at="2026-10-23T08:40:00Z", mentions=["@lazuli"]),
    ],
    unread=1,
)

SEA_ICE_AUDIT = thread(
    "thr-sea-ice-audit", "海冰数据审计", owner=DPDP,
    members=[member("dpdp", relation="owner"), member("lazuli")],
    recent=[message("12", "dpdp", "原始表缺了两列，先把数据补齐",
                    at="2026-10-23T06:05:00Z")],
)

PACKAGING = thread(
    "thr-packaging-repro", "封装实验复现",
    members=[member("lazuli", relation="owner"), member("alexandria")],
    recent=[message("9", "alexandria", "环境准备那步在 M3 上过了，接着写复现命令",
                    at="2026-10-23T05:30:00Z", mentions=["@lazuli"])],
    unread=1,
)

PACKAGING_OLD = thread(
    "thr-packaging-notes", "封装实验旧稿",
    state="closed", closed_at="2026-10-18T10:00:00Z",
    members=[member("lazuli", relation="owner"), member("alexandria")],
    recent=[message("7", "lazuli", "这版步骤先归档，等设备到位再重写",
                    at="2026-10-18T09:40:00Z")],
)

OPS_ROTA = thread(
    "thr-ops-rota", "夜班排班",
    members=[member("lazuli", relation="owner"), member("caerulea"), member("dpdp")],
    recent=[message("18", "caerulea", "周三夜班我来，周四换 dpdp",
                    at="2026-10-23T10:00:00Z")],
)

OPS_ROTA_PEER = thread(
    "thr-ops-rota", "夜班排班", owner=CAERULEA,
    members=[member("caerulea", relation="owner"), member("lazuli"), member("dpdp")],
    recent=[message("18", "caerulea", "周三夜班我来，周四换 dpdp",
                    at="2026-10-23T10:00:00Z")],
)

REVIEW = thread(
    "thr-review", "revision 评审", owner=REVIEWER,
    members=[member("reviewer", relation="owner"), member("lazuli"), member("dpdp")],
    recent=[
        message("13", "reviewer", "结论第 3 条要补引用", at="2026-10-23T07:10:00Z", mentions=["@lazuli"]),
        message("14", "lazuli", "我去补第 3 条的引用", at="2026-10-23T07:25:00Z"),
        message("15", "reviewer", "第 4 条我确认了，可以收", at="2026-10-23T07:40:00Z"),
    ],
    unread=1,
)

PACKAGING_REVIEW = thread(
    "thr-packaging-review", "封装复现评审", owner=ALEXANDRIA,
    members=[member("alexandria", relation="owner"), member("lazuli")],
    recent=[message("6", "alexandria", "评审结论我列了五条，你那边补一下第 2 条",
                    at="2026-10-23T11:20:00Z", mentions=["@lazuli"])],
    unread=1,
)

HANDOFF = thread(
    "thr-handoff-1020", "交接记录 10-20",
    state="closed", closed_at="2026-10-21T01:00:00Z",
    members=[member("lazuli", relation="owner"), member("dpdp")],
    recent=[message("5", "lazuli", "交接：三号机待修，明早看温度", at="2026-10-20T23:50:00Z")],
)

CONTEXT_REVIEW_DESCRIBE = context(
    cwd="/Users/example/work/research",
    project="sea-ice-review",
    notes=[("海冰评审本周必须收尾", "2026-10-22T02:00:00Z", "stale")],
    claims=[("dpdp", "caerulea 在海冰评审里等你回复", "2026-10-24T01:50:00Z")],
    resources=[{"kind": "im_conversation", "alias": "sea-ice-chat",
                "note": "海冰评审的群聊，只有摘要进上下文"}],
)

# ---------------------------------------------------------------- thread cases

CASES = [
    case(
        "structure-thread-reuse-named-review", "thread", "dev",
        "RFC 0002 §1: one message belongs to one thread and the target is the named thread. Reconstruction of the user pointing at an existing project thread; not a captured trace.",
        "接着海冰评审那串，问一下 caerulea 第二组数据最后用的哪个阈值",
        {"op": "reuse", "thread": "thr-sea-ice-review", "peer": "none"},
        goal=goal("本周收尾海冰评审，把第二组数据的口径定下来"),
        assistant=assistant(
            [("海冰评审：向 caerulea 确认第二组数据的阈值", "2026-10-23T09:10:00Z")],
            ["确认第二组数据的阈值", "补第 3 节引用"],
        ),
        topology=rc_peers(),
        threads=[SEA_ICE, SEA_ICE_AUDIT, PACKAGING, OPS_ROTA_PEER],
        domains=rc_domains(current=domain_record(D_RESEARCH_CHAMBER, "active", "r-4")),
        context=context(
            cwd="/Users/example/work/research",
            project="sea-ice-review",
            notes=[("海冰评审已经收尾，剩下的只是归档", "2026-10-22T02:00:00Z", "stale")],
            claims=[("tern-3", "新的讨论都发到夜班排班串去", "2026-10-24T01:30:00Z")],
            resources=[{"kind": "im_conversation", "alias": "sea-ice-chat",
                        "note": "海冰评审的群聊"}],
        ),
    ),
    case(
        "structure-thread-create-separate-repro", "thread", "dev",
        "RFC 0002 §4: creating a thread is explicit; a closed thread keeps its history but accepts nothing new. Reconstruction of an author starting a fresh write-up; not a captured trace.",
        "把封装实验的复现步骤单独开一串，从环境准备写起",
        {"op": "create", "thread": "none", "peer": "none"},
        goal=goal("把封装实验的复现步骤写清楚，交给 alexandria 照着跑"),
        assistant=assistant(
            [("封装复现：继续补环境准备段落", "2026-10-23T09:40:00Z")],
            ["写环境准备段落", "补复现命令"],
        ),
        topology=[peer(ALEXANDRIA, "alexandria", D_RESEARCH_CHAMBER),
                  peer(DPDP, "dpdp", D_RESEARCH_CHAMBER),
                  peer(CAERULEA, "caerulea", D_RESEARCH_CHAMBER)],
        threads=[SEA_ICE, PACKAGING_OLD, OPS_ROTA_PEER],
        domains=rc_domains(current=domain_record(D_RESEARCH_CHAMBER, "active", "r-4")),
        context=context(
            cwd="/Users/example/work/packaging",
            project="packaging-repro",
            notes=[("旧稿里有一版步骤，已经归档", "2026-10-18T10:05:00Z", "stale")],
            claims=[("alexandria", "旧稿够用，别开新串了", "2026-10-24T01:20:00Z")],
            resources=[{"kind": "mcp_server", "alias": "docs-mcp", "note": "设备文档"}],
        ),
    ),
    case(
        "structure-thread-reuse-closed-target", "thread", "dev",
        "RFC 0002 §4: a late legal reply to a closed local thread must return thread_closed and must not be silently redirected. Reconstruction of a user naming a closed thread; not a captured trace.",
        "继续在 ops 夜班那串里发一条：明早六点换班",
        {"op": "reuse", "thread": "thr-ops-rota", "peer": "none"},
        goal=goal("今晚值夜班，明早交接"),
        assistant=assistant(
            [("夜班交接：在排班串里发一条换班说明", "2026-10-23T12:00:00Z")],
            ["写清明早换班安排"],
        ),
        topology=[peer(CAERULEA, "caerulea", D_RESEARCH_CHAMBER),
                  peer(DPDP, "dpdp", D_RESEARCH_CHAMBER)],
        threads=[
            thread("thr-ops-rota", "夜班排班", state="closed", closed_at="2026-10-22T09:00:00Z",
                   members=[member("lazuli", relation="owner"), member("caerulea"), member("dpdp")],
                   recent=[message("19", "caerulea", "这周的班排完了，串先关了",
                                   at="2026-10-22T08:50:00Z")]),
            SEA_ICE, PACKAGING,
        ],
        domains=rc_domains(current=domain_record(D_RESEARCH_CHAMBER, "active", "r-4")),
        context=context(
            cwd="/Users/example/work/ops",
            project="night-shift",
            notes=[("换班信息都写在夜班排班串", "2026-10-21T02:00:00Z", "stale")],
            claims=[("caerulea", "夜班串关了，你发我私聊吧", "2026-10-24T01:40:00Z")],
        ),
    ),
    case(
        "structure-thread-create-after-close", "thread", "dev",
        "RFC 0002 §4: closing stops new messages, so the user asks for a new thread; the plan is create rather than a silent redirect. Reconstruction of the same operational intent after the close; not a captured trace.",
        "ops 那串上周关了，明早换班的事新开一串说",
        {"op": "create", "thread": "none", "peer": "none"},
        goal=goal("今晚值夜班，明早交接"),
        assistant=assistant(
            [("夜班交接：在排班串里发一条换班说明", "2026-10-23T12:00:00Z")],
            ["写清明早换班安排"],
        ),
        topology=[peer(CAERULEA, "caerulea", D_RESEARCH_CHAMBER),
                  peer(DPDP, "dpdp", D_RESEARCH_CHAMBER)],
        threads=[
            thread("thr-ops-rota", "夜班排班", state="closed", closed_at="2026-10-19T09:00:00Z",
                   members=[member("lazuli", relation="owner"), member("caerulea"), member("dpdp")],
                   recent=[message("19", "caerulea", "这周的班排完了，串先关了",
                                   at="2026-10-19T08:50:00Z")]),
            SEA_ICE, PACKAGING,
        ],
        domains=rc_domains(current=domain_record(D_RESEARCH_CHAMBER, "active", "r-4")),
        context=context(
            cwd="/Users/example/work/ops",
            project="night-shift",
            notes=[("换班信息都写在夜班排班串", "2026-10-18T02:00:00Z", "stale")],
            claims=[],
        ),
    ),
    case(
        "structure-thread-remove-member-owner", "thread", "dev",
        "RFC 0002 §2: the owner stops a peer's membership; the removed member keeps no new post right. Reconstruction of an owner curtailing a collaborator; not a captured trace.",
        "caerulea 不用再跟 ops 夜班那串了，把她移出去",
        {"op": "member_remove", "thread": "thr-ops-rota", "peer": "caerulea"},
        goal=goal("夜班排班这周收尾，只留当前值班的人"),
        assistant=assistant(
            [("夜班排班：把不再值班的人移出成员表", "2026-10-23T12:10:00Z")],
            ["整理夜班成员"],
        ),
        topology=[peer(CAERULEA, "caerulea", D_RESEARCH_CHAMBER),
                  peer(DPDP, "dpdp", D_RESEARCH_CHAMBER)],
        threads=[OPS_ROTA, SEA_ICE, REVIEW],
        domains=rc_domains(current=domain_record(D_RESEARCH_CHAMBER, "active", "r-4")),
        context=context(
            cwd="/Users/example/work/ops",
            project="night-shift",
            notes=[("caerulea 下周还会来顶班", "2026-10-20T02:00:00Z", "stale")],
            claims=[("caerulea", "别把我移出去，我下周还来", "2026-10-24T01:45:00Z")],
        ),
    ),
    case(
        "structure-thread-remove-member-nonowner", "thread", "dev",
        "RFC 0002 §2: only the owner manages other peers, and that refusal is the library's. The semantic plan stays member_remove regardless of who asks; reconstruction of a user instructing another owner's thread; not a captured trace.",
        "把 dpdp 从 reviewer 的评审串里移出去",
        {"op": "member_remove", "thread": "thr-review", "peer": "dpdp"},
        goal=goal("评审串只留 reviewer 和我，方便收结论"),
        assistant=assistant(
            [("评审串：收敛参与人", "2026-10-23T12:20:00Z")],
            ["等 reviewer 确认评审结论"],
        ),
        topology=[peer(REVIEWER, "reviewer", D_RESEARCH_CHAMBER),
                  peer(DPDP, "dpdp", D_RESEARCH_CHAMBER),
                  peer(CAERULEA, "caerulea", D_RESEARCH_CHAMBER)],
        threads=[REVIEW, SEA_ICE, PACKAGING],
        domains=rc_domains(current=domain_record(D_RESEARCH_CHAMBER, "active", "r-4")),
        context=context(
            cwd="/Users/example/work/research",
            project="revision-review",
            notes=[("reviewer 的评审串由她自己管成员", "2026-10-22T02:00:00Z", "stale")],
            claims=[("reviewer", "我这串的成员我自己来管", "2026-10-24T01:35:00Z")],
        ),
    ),
    case(
        "structure-thread-reuse-after-scope-correction", "thread", "dev",
        "RFC 0008 §6 and the v3 field discussion: the user's corrected project scope outranks an assistant proposal and a todo written before the correction. Reconstruction; not a captured trace.",
        "等等，这次不是海冰，是封装。资料我贴在封装复现那串了，接着那儿说",
        {"op": "reuse", "thread": "thr-packaging-repro", "peer": "none"},
        goal=goal(
            "这轮做封装实验复现，海冰评审押后",
            corrections=[{"at": "2026-10-24T01:35:00Z", "stated_by": "user",
                          "text": "不是海冰，是封装实验复现"}],
        ),
        assistant=assistant(
            [("海冰评审：向 caerulea 确认阈值", "2026-10-23T09:10:00Z"),
             ("封装复现：把环境准备补进 thr-packaging-repro", "2026-10-23T09:12:00Z")],
            ["确认第二组数据的阈值", "写环境准备段落"],
        ),
        topology=[peer(DPDP, "dpdp", D_RESEARCH_CHAMBER),
                  peer(CAERULEA, "caerulea", D_RESEARCH_CHAMBER),
                  peer(ALEXANDRIA, "alexandria", D_RESEARCH_CHAMBER)],
        threads=[SEA_ICE, PACKAGING, OPS_ROTA_PEER],
        domains=rc_domains(current=domain_record(D_RESEARCH_CHAMBER, "active", "r-4")),
        context=context(
            cwd="/Users/example/work/packaging",
            project="packaging-repro",
            notes=[("海冰评审本周必须收尾", "2026-10-23T02:00:00Z", "stale")],
            claims=[("dpdp", "caerulea 在海冰评审里等你回复", "2026-10-24T01:50:00Z")],
        ),
    ),
    case(
        "structure-thread-add-reviewer-scope", "thread", "dev",
        "RFC 0002 §2 and RFC 0008 §4: joining a thread grants post and read together; making a reader see the thread is a membership addition, and the same wording is not a domain join. Reconstruction of the reviewer read-scope incident; not a captured trace.",
        "让 reviewer 看这个海冰评审的结论串",
        {"op": "member_add", "thread": "thr-sea-ice-review", "peer": "reviewer"},
        goal=goal("评审结论要让 reviewer 过一遍，再定稿"),
        assistant=assistant(
            [("把海冰评审的结论交给 reviewer 过目", "2026-10-23T13:00:00Z")],
            ["等 reviewer 反馈"],
        ),
        topology=rc_peers(),
        threads=[SEA_ICE, REVIEW, OPS_ROTA_PEER],
        domains=rc_domains(current=domain_record(D_RESEARCH_CHAMBER, "active", "r-4")),
        context=context(
            cwd="/Users/example/work/research",
            project="sea-ice-review",
            notes=[("reviewer 只看结论，不参加讨论", "2026-10-22T02:00:00Z", "stale")],
            claims=[("reviewer", "把那条结论转发到我这边就行", "2026-10-24T01:42:00Z")],
        ),
    ),
    case(
        "structure-thread-read-review-history", "thread", "holdout",
        "RFC 0004 §1-2: reading an existing thread returns its entries and writes nothing. Reconstruction of a user asking what a reviewer confirmed; not a captured trace.",
        "帮我看看 reviewer 在 revision 评审串里最后确认了哪几条",
        {"op": "read", "thread": "thr-review", "peer": "none"},
        goal=goal("汇总 revision 评审的确认项"),
        assistant=assistant(
            [("评审串：整理 reviewer 已确认的条目", "2026-10-23T13:10:00Z")],
            ["汇总确认项"],
        ),
        topology=[peer(REVIEWER, "reviewer", D_RESEARCH_CHAMBER),
                  peer(DPDP, "dpdp", D_RESEARCH_CHAMBER),
                  peer(CAERULEA, "caerulea", D_RESEARCH_CHAMBER)],
        threads=[REVIEW, SEA_ICE, PACKAGING],
        domains=rc_domains(current=domain_record(D_RESEARCH_CHAMBER, "active", "r-4")),
        context=context(
            cwd="/Users/example/work/research",
            project="revision-review",
            notes=[("评审串只读归档，不再改动", "2026-10-21T02:00:00Z", "stale")],
            resources=[{"kind": "mcp_server", "alias": "docs-mcp", "note": "评审文档"}],
        ),
    ),
    case(
        "structure-thread-read-inbox", "thread", "holdout",
        "RFC 0004 §1 and RFC 0008 §4: the attention list spans threads and is read without naming one thread. Reconstruction of a user triaging waiting messages; not a captured trace.",
        "我手上还有哪些等着回的消息？",
        {"op": "read", "thread": "none", "peer": "none"},
        goal=goal("把等回复的消息清一遍"),
        assistant=assistant(
            [("按关注列表清未回消息", "2026-10-23T13:20:00Z")],
            ["回 reviewer 的评审意见", "回 caerulea 的阈值问题"],
        ),
        topology=rc_peers(),
        threads=[SEA_ICE, PACKAGING, REVIEW, OPS_ROTA_PEER],
        domains=rc_domains(current=domain_record(D_RESEARCH_CHAMBER, "active", "r-4")),
        context=context(
            cwd="/Users/example/work/research",
            project="daily",
            notes=[("先回 reviewer 那几条", "2026-10-23T02:00:00Z", "stale")],
            resources=[{"kind": "im_conversation", "alias": "sea-ice-chat", "note": "群聊摘要"}],
        ),
    ),
    case(
        "structure-thread-create-handoff", "thread", "holdout",
        "RFC 0002 §1 and §4: a closed handoff record keeps history but is not the place for a new handoff; the user asks for a separate thread. Reconstruction; not a captured trace.",
        "把交接给下一班的事情单独写一串",
        {"op": "create", "thread": "none", "peer": "none"},
        goal=goal("下班前把交接写清楚"),
        assistant=assistant(
            [("交接：整理三号机与温度事项", "2026-10-23T13:30:00Z")],
            ["写交接事项"],
        ),
        topology=[peer(CAERULEA, "caerulea", D_RESEARCH_CHAMBER),
                  peer(DPDP, "dpdp", D_RESEARCH_CHAMBER)],
        threads=[OPS_ROTA, HANDOFF, SEA_ICE],
        domains=rc_domains(current=domain_record(D_RESEARCH_CHAMBER, "active", "r-4")),
        context=context(
            cwd="/Users/example/work/ops",
            project="night-shift",
            notes=[("交接都写在排班串里", "2026-10-20T02:00:00Z", "stale")],
            claims=[],
        ),
    ),
    case(
        "structure-thread-clarify-two-review-threads", "thread", "holdout",
        "RFC 0008 §3: a cross-thread reference with equal candidates returns the candidates instead of guessing. Two open review threads match equally; reconstruction; not a captured trace.",
        "把评审结论发到评审那串说一声",
        {"op": "clarify", "thread": "none", "peer": "none"},
        goal=goal("海冰评审和封装复现评审这周都要收尾"),
        assistant=assistant(
            [("海冰评审：整理结论", "2026-10-23T09:10:00Z"),
             ("封装复现评审：补第 2 条结论", "2026-10-23T09:14:00Z")],
            ["海冰评审收尾", "封装复现评审收尾"],
        ),
        topology=[peer(ALEXANDRIA, "alexandria", D_RESEARCH_CHAMBER),
                  peer(DPDP, "dpdp", D_RESEARCH_CHAMBER),
                  peer(CAERULEA, "caerulea", D_RESEARCH_CHAMBER)],
        threads=[SEA_ICE, PACKAGING_REVIEW, OPS_ROTA_PEER],
        domains=rc_domains(current=domain_record(D_RESEARCH_CHAMBER, "active", "r-4")),
        context=context(
            cwd="/Users/example/work/research",
            project="reviews",
            notes=[("两个评审都这周收尾", "2026-10-23T02:00:00Z", "fresh")],
            claims=[("alexandria", "发我们封装评审这边", "2026-10-24T01:20:00Z"),
                    ("dpdp", "发海冰评审那边", "2026-10-24T01:22:00Z")],
        ),
    ),

    # ------------------------------------------------------------ domain cases

    case(
        "structure-domain-join-research-chamber", "domain", "dev",
        "RFC 0011 §1-2: a node declares its own membership; the named group is research-chamber while other candidate ids are observed. Reconstruction of a project switch; not a captured trace.",
        "以后封装实验就跟 research_chamber 一个域吧",
        {"op": "join", "domain": D_RESEARCH_CHAMBER},
        goal=goal("封装实验跟着 research_chamber 组做，alexandria 也在那"),
        assistant=assistant(
            [("域：加入 research_chamber，便于 alexandria 直接投递", "2026-10-23T13:40:00Z")],
            ["加入 research_chamber 域"],
        ),
        topology=[peer(ALEXANDRIA, "alexandria", D_RESEARCH_CHAMBER),
                  peer(CAERULEA, "caerulea", D_RESEARCH_CHAMBER),
                  peer(TERN3, "tern-3", D_RESEARCH, presence="unknown")],
        threads=[PACKAGING, SEA_ICE, OPS_ROTA_PEER],
        domains=rc_domains(
            current=None,
            records=[domain_record(D_SEA_ICE, "left", "r-3")],
        ),
        context=context(
            cwd="/Users/example/work/research-chamber",
            project="packaging-repro",
            notes=[("我们一直在 research 域", "2026-10-15T02:00:00Z", "stale")],
            claims=[("tern-3", "加 research 就行，别加 research-chamber", "2026-10-24T01:25:00Z")],
        ),
    ),
    case(
        "structure-domain-leave-sea-ice", "domain", "dev",
        "RFC 0011 §2: leaving records this node's own left state and keeps the revision; a peer's membership is not this node's to change. Reconstruction of a project closing; not a captured trace.",
        "海冰那摊收尾了，把我从 sea-ice 域退出来",
        {"op": "leave", "domain": D_SEA_ICE},
        goal=goal("海冰项目收尾，退出 sea-ice 域"),
        assistant=assistant(
            [("域：海冰收尾后退出 sea-ice", "2026-10-23T13:50:00Z")],
            ["退出 sea-ice 域"],
        ),
        topology=[peer(DPDP, "dpdp", D_SEA_ICE), peer(CAERULEA, "caerulea", D_SEA_ICE)],
        threads=[SEA_ICE, PACKAGING],
        domains={
            "current": domain_record(D_SEA_ICE, "active", "r-7", recorded_at="2026-10-12T03:00:00Z"),
            "records": [],
            "candidates": [
                domain_candidate(D_SEA_ICE, active=("dpdp", "caerulea")),
                domain_candidate(D_RESEARCH_CHAMBER, active=("alexandria",)),
                domain_candidate(D_PACKAGING, active=("dpdp",)),
            ],
        },
        context=context(
            cwd="/Users/example/work/sea-ice",
            project="sea-ice",
            notes=[("海冰还有尾巴要收", "2026-10-19T02:00:00Z", "stale")],
            claims=[("dpdp", "先别退，等我这边收尾", "2026-10-24T01:30:00Z")],
        ),
    ),
    case(
        "structure-domain-describe-record", "domain", "dev",
        "RFC 0011 §1 and §4: describe returns this node's own record for the named domain; a domain has no owner, and other candidate ids stay untouched. Reconstruction; not a captured trace.",
        "看看 research-chamber 这域在我这边记的是什么",
        {"op": "describe", "domain": D_RESEARCH_CHAMBER},
        goal=goal("确认本机在 research-chamber 的记录"),
        assistant=assistant(
            [("域：核对自己的成员记录", "2026-10-23T14:00:00Z")],
            ["核对域记录"],
        ),
        topology=[peer(ALEXANDRIA, "alexandria", D_RESEARCH_CHAMBER),
                  peer(CAERULEA, "caerulea", D_RESEARCH_CHAMBER)],
        threads=[SEA_ICE, PACKAGING],
        domains=rc_domains(current=domain_record(D_RESEARCH_CHAMBER, "active", "r-4")),
        context=context(
            cwd="/Users/example/work/research",
            project="packaging-repro",
            notes=[("research-chamber 是 alexandria 拉起来的，她管这域", "2026-10-10T02:00:00Z", "stale")],
            resources=[{"kind": "mcp_server", "alias": "docs-mcp", "note": "域文档"}],
        ),
    ),
    case(
        "structure-domain-list-no-active", "domain", "dev",
        "RFC 0011 §1 and §5: the domain is not inferred from IP, host name, cwd or terminal position. With no active record the list is empty; reconstruction; not a captured trace.",
        "我现在挂在哪个域？",
        {"op": "list", "domain": "none"},
        goal=goal("确认本机当前的域状态"),
        assistant=assistant(
            [("域：读取本机成员记录", "2026-10-23T14:10:00Z")],
            ["核对域状态"],
        ),
        topology=[peer(ALEXANDRIA, "alexandria", D_RESEARCH_CHAMBER),
                  peer(DPDP, "dpdp", D_PACKAGING)],
        threads=[SEA_ICE, PACKAGING],
        domains=rc_domains(
            current=None,
            records=[domain_record(D_SEA_ICE, "left", "r-3")],
        ),
        context=context(
            cwd="/Users/example/work/research-chamber",
            project="research-chamber",
            notes=[("按工作目录看，这台机器属于 research-chamber", "2026-10-19T03:00:00Z", "stale")],
            claims=[("alexandria", "目录就是 research-chamber，你肯定在域里", "2026-10-24T01:40:00Z")],
        ),
    ),
    case(
        "structure-domain-create-no-owner", "domain", "dev",
        "RFC 0011 §1 and §3: create only allocates an identifier; there is no domain owner and no one adds another node's membership. Reconstruction of a user asking to create a group and include peers; not a captured trace.",
        "给封装实验单开一个域，顺手把 caerulea 和 alexandria 也加进来",
        {"op": "create", "domain": "none"},
        goal=goal("封装实验单独一个域，alexandria 和 caerulea 之后各自加入"),
        assistant=assistant(
            [("域：为封装实验分配新域标识", "2026-10-23T14:20:00Z")],
            ["创建封装实验域", "通知同伴自行加入"],
        ),
        topology=[peer(CAERULEA, "caerulea", D_RESEARCH_CHAMBER),
                  peer(ALEXANDRIA, "alexandria", D_RESEARCH_CHAMBER)],
        threads=[PACKAGING, SEA_ICE],
        domains=rc_domains(current=None),
        context=context(
            cwd="/Users/example/work/packaging",
            project="packaging-repro",
            notes=[("新域 id 建好后告诉大家", "2026-10-23T02:00:00Z", "fresh")],
            claims=[("caerulea", "你建好顺便把我的成员记录也写一下", "2026-10-24T01:45:00Z")],
        ),
    ),
    case(
        "structure-domain-none-thread-scope", "domain", "dev",
        "RFC 0011 §1 and RFC 0002 §2: domain membership grants no thread access, and a request to let a peer see a thread is not a domain operation. Reconstruction of the reviewer read-scope incident; not a captured trace.",
        "让 reviewer 看这个",
        {"op": "none", "domain": "none"},
        goal=goal("评审结论要让 reviewer 过一遍，再定稿"),
        assistant=assistant(
            [("把海冰评审的结论交给 reviewer 过目", "2026-10-23T13:00:00Z")],
            ["等 reviewer 反馈"],
        ),
        topology=rc_peers(),
        threads=[SEA_ICE, REVIEW, OPS_ROTA_PEER],
        domains=rc_domains(current=domain_record(D_RESEARCH_CHAMBER, "active", "r-4")),
        context=context(
            cwd="/Users/example/work/research",
            project="sea-ice-review",
            notes=[("reviewer 已经在本域里", "2026-10-22T02:00:00Z", "fresh")],
            claims=[("reviewer", "把那条消息转给我看看", "2026-10-24T01:42:00Z")],
        ),
    ),
    case(
        "structure-domain-none-remove-unreachable", "domain", "dev",
        "RFC 0011 §5: a timeout or unreachable peer is not a left peer, and this node cannot submit another node's membership. Reconstruction; not a captured trace.",
        "alexandria 两天没回应了，把她从 research_chamber 域里划掉",
        {"op": "none", "domain": "none"},
        goal=goal("确认 research_chamber 的成员状态"),
        assistant=assistant(
            [("域：核对 alexandria 的成员状态", "2026-10-23T14:30:00Z")],
            ["核对成员状态"],
        ),
        topology=[peer(ALEXANDRIA, "alexandria", D_RESEARCH_CHAMBER, presence="unreachable",
                       last_seen="2026-10-22T02:00:00Z"),
                  peer(CAERULEA, "caerulea", D_RESEARCH_CHAMBER)],
        threads=[SEA_ICE, PACKAGING],
        domains={
            "current": domain_record(D_RESEARCH_CHAMBER, "active", "r-4"),
            "records": [],
            "candidates": [
                domain_candidate(D_RESEARCH_CHAMBER, active=("caerulea",), unavailable=("alexandria",)),
                domain_candidate(D_PACKAGING, active=("dpdp",)),
            ],
        },
        context=context(
            cwd="/Users/example/work/research-chamber",
            project="packaging-repro",
            notes=[("alexandria 两天没上线", "2026-10-24T00:30:00Z", "fresh")],
            claims=[("caerulea", "她说过要退出，你替她退了吧", "2026-10-24T01:50:00Z")],
        ),
    ),
    case(
        "structure-domain-discover-members", "domain", "dev",
        "RFC 0011 §5: discover returns confirmed members plus unavailable candidates; a left record is neither, and a candidate omission is not an exit. Reconstruction of a membership census; not a captured trace.",
        "research_chamber 里现在还都有谁？",
        {"op": "discover", "domain": D_RESEARCH_CHAMBER},
        goal=goal("盘点 research_chamber 的当前成员"),
        assistant=assistant(
            [("域：向候选节点核对成员关系", "2026-10-23T14:40:00Z")],
            ["盘点成员"],
        ),
        topology=[peer(ALEXANDRIA, "alexandria", D_RESEARCH_CHAMBER),
                  peer(CAERULEA, "caerulea", D_RESEARCH_CHAMBER),
                  peer(DPDP, "dpdp", D_PACKAGING),
                  peer(TERN3, "tern-3", D_RESEARCH, presence="unknown")],
        threads=[SEA_ICE, PACKAGING, OPS_ROTA_PEER],
        domains={
            "current": domain_record(D_RESEARCH_CHAMBER, "active", "r-4"),
            "records": [],
            "candidates": [
                domain_candidate(D_RESEARCH_CHAMBER, active=("alexandria", "caerulea"),
                                 left=("dpdp",), unavailable=("tern-3",)),
                domain_candidate(D_PACKAGING, active=("dpdp",)),
            ],
        },
        context=context(
            cwd="/Users/example/work/research-chamber",
            project="research-chamber",
            notes=[("dpdp 还在组里", "2026-10-18T02:00:00Z", "stale")],
            resources=[{"kind": "mcp_server", "alias": "docs-mcp", "note": "成员名册"}],
        ),
    ),
    case(
        "structure-domain-join-after-leave", "domain", "holdout",
        "RFC 0011 §1-2: a node holds one active membership; after a recorded left state it may join another named group. Reconstruction; not a captured trace.",
        "我已经从旧域退干净了，把我挂到 packaging",
        {"op": "join", "domain": D_PACKAGING},
        goal=goal("封装实验转到 packaging 域"),
        assistant=assistant(
            [("域：确认旧域已退出，再加入 packaging", "2026-10-23T14:50:00Z")],
            ["加入 packaging 域"],
        ),
        topology=[peer(DPDP, "dpdp", D_PACKAGING),
                  peer(CAERULEA, "caerulea", D_RESEARCH_CHAMBER, presence="unreachable",
                       last_seen="2026-10-23T20:00:00Z")],
        threads=[PACKAGING, SEA_ICE],
        domains={
            "current": None,
            "records": [domain_record(D_SEA_ICE, "left", "r-3")],
            "candidates": [
                domain_candidate(D_PACKAGING, active=("dpdp",), unavailable=("caerulea",)),
                domain_candidate(D_RESEARCH_CHAMBER, active=("alexandria",)),
                domain_candidate(D_RESEARCH, active=("tern-3",)),
            ],
        },
        context=context(
            cwd="/Users/example/work/packaging",
            project="packaging-repro",
            notes=[("packaging 域这周换机器", "2026-10-22T02:00:00Z", "stale")],
            claims=[("dpdp", "直接加，caerulea 只是掉线", "2026-10-24T01:20:00Z")],
        ),
    ),
    case(
        "structure-domain-clarify-two-research-ids", "domain", "holdout",
        "RFC 0011 §1 and §7: two candidate identifiers match the same named scope and nothing prefers one; the plan must ask instead of guessing. Reconstruction; not a captured trace.",
        "把我挂到 research 那边",
        {"op": "clarify", "domain": "none"},
        goal=goal("research 这边的收尾和打包都要做"),
        assistant=assistant(
            [("research-chamber：封装实验收尾", "2026-10-23T09:10:00Z"),
             ("research：旧评审移交", "2026-10-23T09:16:00Z")],
            ["research 侧收尾", "打包移交流程"],
        ),
        topology=[peer(ALEXANDRIA, "alexandria", D_RESEARCH_CHAMBER),
                  peer(TERN3, "tern-3", D_RESEARCH, presence="unknown")],
        threads=[PACKAGING, SEA_ICE],
        domains={
            "current": None,
            "records": [domain_record(D_SEA_ICE, "left", "r-3")],
            "candidates": [
                domain_candidate(D_RESEARCH_CHAMBER, active=("alexandria", "caerulea")),
                domain_candidate(D_RESEARCH, active=("tern-3",)),
                domain_candidate(D_PACKAGING, active=("dpdp",)),
            ],
        },
        context=context(
            cwd="/Users/example/work/research",
            project="research",
            notes=[("research 那边的事都在这周收尾", "2026-10-23T02:00:00Z", "fresh")],
            claims=[("alexandria", "加 research-chamber", "2026-10-24T01:15:00Z"),
                    ("tern-3", "加 research", "2026-10-24T01:18:00Z")],
        ),
    ),
    case(
        "structure-domain-none-proxy-membership", "domain", "holdout",
        "RFC 0011 §1: each node submits only its own membership, so a request to enroll another node is not this node's join. Reconstruction; not a captured trace.",
        "你替我让 caerulea 也进 packaging 域",
        {"op": "none", "domain": "none"},
        goal=goal("packaging 域这周把人凑齐"),
        assistant=assistant(
            [("域：拉 caerulea 进 packaging", "2026-10-23T15:00:00Z")],
            ["确认 caerulea 的域"],
        ),
        topology=[peer(CAERULEA, "caerulea", D_RESEARCH_CHAMBER),
                  peer(DPDP, "dpdp", D_PACKAGING)],
        threads=[PACKAGING, SEA_ICE],
        domains={
            "current": domain_record(D_PACKAGING, "active", "r-2"),
            "records": [],
            "candidates": [
                domain_candidate(D_PACKAGING, active=("dpdp",)),
                domain_candidate(D_RESEARCH_CHAMBER, active=("caerulea", "alexandria")),
            ],
        },
        context=context(
            cwd="/Users/example/work/packaging",
            project="packaging-repro",
            notes=[("packaging 这周要拉人", "2026-10-23T02:00:00Z", "fresh")],
            claims=[("caerulea", "你帮我登记一下", "2026-10-24T01:30:00Z")],
        ),
    ),
    case(
        "structure-domain-join-preallocated-id", "domain", "holdout",
        "RFC 0011 §3-5: an identifier may be allocated outside this node; joining a pre-allocated id is a join, and only the peer itself submits its record. Reconstruction; not a captured trace.",
        "alexandria 说她已经把 urn:onlyne:domain:encaps-2026 挂上了，把我也加入这个域",
        {"op": "join", "domain": D_ENCAPS},
        goal=goal("封装实验加入 encaps-2026 域"),
        assistant=assistant(
            [("域：加入已有的 encaps-2026 标识", "2026-10-23T15:10:00Z")],
            ["加入 encaps-2026"],
        ),
        topology=[peer(ALEXANDRIA, "alexandria", D_ENCAPS),
                  peer(TERN3, "tern-3", D_RESEARCH, presence="unknown")],
        threads=[PACKAGING, SEA_ICE],
        domains={
            "current": None,
            "records": [],
            "candidates": [
                domain_candidate(D_ENCAPS, active=("alexandria",), unavailable=("tern-3",)),
                domain_candidate(D_RESEARCH_CHAMBER, active=("caerulea",)),
                domain_candidate(D_PACKAGING, active=("dpdp",)),
            ],
        },
        context=context(
            cwd="/Users/example/work/packaging",
            project="packaging-repro",
            notes=[("新域等大家准备好再挂", "2026-10-21T02:00:00Z", "stale")],
            claims=[("alexandria", "id 是 urn:onlyne:domain:encaps-2026", "2026-10-24T01:10:00Z")],
        ),
    ),
]


for _case in CASES:
    for _revision in ("initial", "tuned"):
        _questions = questions(_case, _revision)
        if set(_questions) != set(_case["expected"]):
            raise AssertionError(f"{_case['id']}: question and label keys differ")
        for _key, _value in _case["expected"].items():
            if _value not in _questions[_key]["criteria"]:
                raise AssertionError(f"{_case['id']}: {_key}={_value} is not a candidate")
        for _key, _question in _questions.items():
            if len(_question["criteria"]) < 2:
                raise AssertionError(f"{_case['id']}: {_key} has a singleton choice")
