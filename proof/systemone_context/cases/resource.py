"""Resource decision slice: resource and operation selection.

Cases rebuild documented v3 workflows — IM conversations, compute jobs, MCP servers,
datasets, handover between agents — and ask the two questions RFC 0016 publishes for
the resource point: which resource the intent acts on, and which declared operation
it asks for. The question text is one task template shared by every case; candidate
criteria restate observed facts only.

Authorization is never asked of the model: every candidate carries the authority set
the deterministic layer already checked. Ordering, revision equality, backlog and
alias uniqueness are deterministic facts and stay out of the questions.

Profiles:
- minimal: decisive facts — intent and competing resources.
- focused: task evidence (activity, todo sources, notes, events, schemas, open
  operations, topology) with source authority attached.
- expanded: the same evidence plus genuine distractors (retired entries, unrelated
  peer notes, another logical domain, same-display duplicates) that never raise an
  untrusted source above the user.

Provenance is reconstruction: hand-authored scenarios modelled on the RFC run
examples and observed agent workflows, not captured production traffic.
"""

from __future__ import annotations

import copy


# ---------------------------------------------------------------- questions

def choice(instructions: str, options: dict[str, str]) -> dict:
    return {"type": "choice", "instructions": instructions, "criteria": options}


# One template for the whole slice. initial is the un-tuned wording kept for
# comparison; tuned states the task and the deterministic boundary.
RESOURCE_INITIAL = "Which resource does the intent refer to?"
OPERATION_INITIAL = "Which operation does the intent ask for?"

RESOURCE_TASK = (
    "Choose the resource the intent acts on. Each candidate lists its observed facts and "
    "the operations it declares; the candidate set is every resource this node may "
    "operate. Pick unresolved only when the intent does not single out one candidate."
)
OPERATION_TASK = (
    "Choose the operation the intent asks for on the chosen resource. Each option is a "
    "declared operation with the resources that declare it; the intent's outcome, not "
    "its wording, decides which operation is meant."
)

UNRESOLVED = {
    "unresolved": "The intent plus the supplied evidence does not single out one candidate; ask the user.",
}


def questions(case: dict, revision: str) -> dict:
    candidates = case["profiles"]["focused"]["resources"]
    resource_options = {
        c["alias"]: c["observed"] for c in candidates
    }
    if revision == "initial":
        resource_question = choice(RESOURCE_INITIAL, resource_options)
    else:
        resource_question = choice(RESOURCE_TASK, {**resource_options, **UNRESOLVED})
    declared: dict[str, list[str]] = {}
    for candidate in candidates:
        for name, description in candidate["operations"].items():
            declared.setdefault(name, []).append(f"{candidate['alias']}: {description}")
    operation_options = {
        name: "; ".join(entries) for name, entries in declared.items()
    }
    operation_question = choice(
        OPERATION_INITIAL if revision == "initial" else OPERATION_TASK,
        operation_options,
    )
    return {"resource": resource_question, "operation": operation_question}


# ---------------------------------------------------------------- fixtures

CHAT_OPS = {
    "read": "return recent platform messages",
    "send": "post a message to the platform conversation",
    "list_events": "return platform events recorded for this conversation",
    "handover": "transfer future inbound events and the send right",
    "describe": "return this resource's description, binding and recorded operation states",
}
JOB_OPS = {
    "read": "return the job's artifacts",
    "cancel": "request that the job stop",
    "list_events": "return recorded job events",
    "transfer": "move a named authority (event, result or control) to another agent",
    "describe": "return the job and the state the service reports",
}
MCP_OPS_DOCS = {
    "list_tools": "return the server's declared tools",
    "search_pages": "run the read-only page search tool",
    "deploy_site": "run the site deployment tool",
}
MCP_OPS_OPS = {
    "list_tools": "return the server's declared tools",
    "restart": "restart a named service",
    "quotas": "read current quotas",
}
DATA_OPS = {
    "query": "select data items by a query",
    "read": "read one addressed data item",
    "describe": "return the dataset description",
    "list_events": "return dataset change events",
}


def res(alias, owner, rid, kind, operations, revision, observed, **facts):
    entry = {
        "alias": alias,
        "ref": {"owner": owner, "id": rid},
        "kind": kind,
        "operations": dict(operations),
        "binding": {"assignee": "urn:onlyne:agent:lazuli", "revision": revision},
        "my_authority": list(operations),
        "observed": observed,
    }
    entry.update(facts)
    return entry


SEA = res("sea-ice-chat", "urn:onlyne:gateway:im", "chat-01", "im_conversation", CHAT_OPS, "rev-a",
          "platform label 海冰数据评审, chat_key sea-ice-review, workspace research, 17 recent events",
          platform_label="海冰数据评审", chat_key="sea-ice-review", workspace="research")
SEA_ARCHIVE = res("sea-ice-archive-chat", "urn:onlyne:gateway:im", "chat-09", "im_conversation", CHAT_OPS, "rev-ar",
                  "platform label 海冰数据评审, chat_key sea-ice-archive, workspace archive, 0 events this month",
                  platform_label="海冰数据评审", chat_key="sea-ice-archive", workspace="archive")
SEA_ROADMAP = res("sea-ice-roadmap", "urn:onlyne:gateway:im", "chat-04", "im_conversation", CHAT_OPS, "rev-f",
                  "platform label 下季度海冰排期, chat_key sea-ice-roadmap, workspace research",
                  platform_label="下季度海冰排期", chat_key="sea-ice-roadmap", workspace="research")
OPS_ROOM = res("ops-room", "urn:onlyne:gateway:im", "chat-02", "im_conversation", CHAT_OPS, "rev-o",
               "platform label 值班室, chat_key ops-duty, workspace ops",
               platform_label="值班室", chat_key="ops-duty", workspace="ops")
DPDP_DM = res("dpdp-dm", "urn:onlyne:gateway:im", "chat-03", "im_conversation", CHAT_OPS, "rev-d",
              "platform label dpdp 私聊, chat_key dm-dpdp, workspace research, 2 recent events",
              platform_label="dpdp 私聊", chat_key="dm-dpdp", workspace="research")
ALERT_ROOM = res("alert-room", "urn:onlyne:gateway:im", "chat-07", "im_conversation", CHAT_OPS, "rev-al",
                 "platform label 告警机器人, chat_key alerts, workspace research",
                 platform_label="告警机器人", chat_key="alerts", workspace="research")

JOB_42 = res("job-42", "urn:onlyne:compute:lab", "job-42", "compute_job", JOB_OPS, "rev-job-3",
             "state running, submitted 2026-10-23, artifacts pending, 14 events")
JOB_43 = res("job-43", "urn:onlyne:compute:lab", "job-43", "compute_job", JOB_OPS, "rev-job-4",
             "state queued, submitted 2026-10-24, 3 events")
JOB_SEA = res("job-seaice-recalc", "urn:onlyne:compute:lab", "job-seaice", "compute_job", JOB_OPS, "rev-job-9",
              "state succeeded, submitted 2026-10-22, result table w/m2")
JOB_99 = res("job-99", "urn:onlyne:compute:ops", "job-99", "compute_job", JOB_OPS, "rev-job-1",
             "state running, submitted 2026-10-24, logs available")

DOCS_MCP = res("docs-mcp", "urn:onlyne:gateway:mcp", "docs", "mcp_server", MCP_OPS_DOCS, "rev-mcp-7",
               "platform label 运维工具, workspace research, tools search_pages deploy_site",
               platform_label="运维工具", workspace="research")
OPS_MCP = res("ops-mcp", "urn:onlyne:gateway:mcp", "ops", "mcp_server", MCP_OPS_OPS, "rev-mcp-2",
              "platform label 运维工具, workspace ops, tools restart quotas",
              platform_label="运维工具", workspace="ops")

SEA_DATA = res("seaice-archive", "urn:onlyne:data:lab", "seaice-archive", "dataset", DATA_OPS, "rev-ds-5",
               "collections arctic, items obs-2026-10-01 onward, subscription supported")

TOPOLOGY = {
    "me": {"node_id": "urn:onlyne:agent:lazuli", "domain": "urn:onlyne:domain:research"},
    "domains": [
        {"domain_id": "urn:onlyne:domain:research", "members": ["lazuli", "dpdp", "caerulea"],
         "resources": ["sea-ice-chat", "dpdp-dm", "job-42", "job-43", "docs-mcp", "seaice-archive"]},
        {"domain_id": "urn:onlyne:domain:ops", "members": ["ops-bot", "lazuli"],
         "resources": ["ops-room", "job-99", "ops-mcp"]},
    ],
    "logical_domains": [
        {"domain_id": "urn:onlyne:domain:tern-4f2", "members": ["lazuli@tern-4f2", "tern-4f2"],
         "note": "same host session, no resources attached"},
    ],
}

USER_NOTES = {
    "note:chat-routing": {"author": "user", "updated": "2026-10-24T09:10:00Z",
                          "text": "海冰评审的讨论在 sea-ice-chat；下季度排期另开了群。"},
    "note:handover-pending": {"author": "user", "updated": "2026-10-24T08:40:00Z",
                              "text": "值班室的交接等我确认，别先答应 ops-bot。"},
}
PEER_NOTES = {
    "note:peer-claim": {"author": "peer caerulea", "updated": "2026-10-23T21:00:00Z",
                        "text": "海冰评审群我已经转过来了，你直接发就行。"},
    "note:old-plan": {"author": "assistant", "updated": "2026-10-22T11:00:00Z",
                      "text": "计划把海冰讨论搬到海冰评审群。"},
}

JOB_PEER_NOTE = {
    "note:job-99-peer": {"author": "peer ops-bot", "updated": "2026-10-24T09:06:00Z",
                         "text": "job-99 我这边看已经跑完了，日志不用再拉。"},
}

UNRELATED_NOTES = {
    "note:build-cache": {"author": "user", "updated": "2026-10-20T16:00:00Z",
                         "text": "构建缓存周日清一次。"},
    "note:job-77": {"author": "peer caerulea", "updated": "2026-10-19T20:30:00Z",
                    "text": "job-77 已经取消，不用再等。"},
}

RETIRED_CHAT = res("sea-ice-old", "urn:onlyne:gateway:im", "chat-00", "im_conversation",
                   CHAT_OPS, "rev-z",
                   "platform label 海冰数据评审（旧）, chat_key sea-ice-old, workspace archive, retired",
                   platform_label="海冰数据评审（旧）", chat_key="sea-ice-old",
                   workspace="archive", retired=True)
RETIRED_JOB = res("job-77", "urn:onlyne:compute:lab", "job-77", "compute_job", JOB_OPS, "rev-job-0",
                  "state canceled, submitted 2026-10-12, retained for records", retired=True)

ACTIVITY_SEA = {
    "thread": "research",
    "recent": [
        {"sender": "dpdp", "at": "2026-10-24T08:12:00Z", "summary": "第 3 节单位核对按 W/m² 重算"},
        {"sender": "lazuli", "at": "2026-10-24T08:50:00Z", "summary": "把图贴回海冰评审群，等评审"},
    ],
}
ACTIVITY_OPS = {
    "thread": "ops",
    "recent": [{"sender": "ops-bot", "at": "2026-10-24T09:02:00Z", "summary": "部署窗口定在周四凌晨"}],
}
ACTIVITY_JOB = {
    "thread": "research",
    "recent": [
        {"sender": "lazuli", "at": "2026-10-23T22:10:00Z", "summary": "提交 job-42 跑第二组数据"},
        {"sender": "caerulea", "at": "2026-10-24T07:45:00Z", "summary": "job-43 还在排队"},
    ],
}

THREAD_SEA = [
    {"seq": 98, "sender": "dpdp", "at": "2026-10-24T08:10:00Z",
     "text": "单位核完了吗？海冰评审群在等图。"},
    {"seq": 101, "sender": "lazuli", "at": "2026-10-24T08:45:00Z",
     "text": "核完了，我贴到海冰评审群。"},
    {"seq": 104, "sender": "user", "at": "2026-10-24T08:46:00Z",
     "text": "对，就那个群，别再让我两边找。"},
]

THREAD_OPS = [
    {"seq": 51, "sender": "ops-bot", "at": "2026-10-24T09:02:00Z",
     "text": "值班室在问周四凌晨的部署窗口，等你回复。"},
    {"seq": 52, "sender": "user", "at": "2026-10-24T09:03:00Z",
     "text": "先看看他们最后说了什么再回。"},
]

THREAD_DPDP = [
    {"seq": 12, "sender": "lazuli", "at": "2026-10-24T08:55:00Z",
     "text": "给 dpdp 发了交接材料。"},
    {"seq": 13, "sender": "user", "at": "2026-10-24T08:56:00Z",
     "text": "她收到没？刚才网断了。"},
]

THREAD_HANDOVER = [
    {"seq": 77, "sender": "caerulea", "at": "2026-10-24T08:35:00Z",
     "text": "海冰评审后面的消息能不能交给 reviewer？我要出差。"},
    {"seq": 78, "sender": "user", "at": "2026-10-24T08:38:00Z",
     "text": "可以，走正式交接，别口头说一声就算。"},
]

COMPRESSED_NOTE = {
    "note:compressed-review": {
        "author": "context note (compressed assistant summary)",
        "updated": "2026-10-24T08:47:00Z",
        "text": "海冰图已重算并准备贴出；目标群与用户确认见 thread research seq 101-104。",
    },
}

SCHEMAS = {
    "send": {"fields": {"text": "string"}, "requires": ["binding.revision"]},
    "handover": {"fields": {"to": "node_id"}, "requires": ["binding.revision", "backlog zero"]},
    "cancel": {"fields": {"reason": "string"}, "requires": ["binding.revision"]},
    "transfer": {"fields": {"authority": "event|result|control", "to": "node_id"},
                 "requires": ["binding.revision", "backlog zero"]},
    "query": {"fields": {"collections": "string[]", "since": "timestamp"}, "requires": []},
    "deploy_site": {"fields": {"site": "string"}, "requires": []},
    "quotas": {"fields": {}, "requires": []},
}


def add(case_id, split, intent, resources, expected, provenance, *,
        activity=None, todo=None, notes=None, events=None, schemas=None,
        open_operations=None, topology=False,
        thread_history=None, extra_resources=None, extra_notes=None):
    minimal = {"decision": "resource", "intent": intent, "resources": copy.deepcopy(resources)}
    focused = {"decision": "resource", "intent": copy.deepcopy(intent),
               "resources": copy.deepcopy(resources)}
    for key, value in (("recent_activity", activity), ("todo_sources", todo),
                       ("context_notes", notes), ("resource_events", events),
                       ("operation_schemas", schemas), ("open_operations", open_operations),
                       ("thread_history", thread_history)):
        if value is not None:
            focused[key] = copy.deepcopy(value)
    if topology:
        focused["topology"] = copy.deepcopy(TOPOLOGY)
    expanded = copy.deepcopy(focused)
    expanded["resources"] = copy.deepcopy(resources) + [RETIRED_CHAT, RETIRED_JOB]
    if extra_resources:
        expanded["resources"] += copy.deepcopy(extra_resources)
    expanded["context_notes"] = {**(expanded.get("context_notes") or {}),
                                 **UNRELATED_NOTES,
                                 **copy.deepcopy(extra_notes or {})}
    return {
        "id": case_id, "point": "resource", "split": split,
        "provenance": provenance,
        "profiles": {"minimal": minimal, "focused": focused, "expanded": expanded},
        "expected": expected,
    }


def intent(kind, msg, **meta):
    return {"delivery_kind": kind, "msg": msg, "meta": meta}


CASES: list[dict] = []

# ------------------------------------------------------------------ dev

# 01: "这个群" is resolved by the open todo and current work, not by the most recent
# resource event (which belongs to an unrelated ops conversation).
CASES.append(add(
    "resource-01", "dev",
    intent("把刚才那张图发到这个群", "图已更新，单位是 W/m²。", operation="send", expected_revision="rev-a"),
    [SEA, OPS_ROOM, DPDP_DM],
    {"resource": "sea-ice-chat", "operation": "send"},
    "reconstruction: 1:1 IM workflow — lazuli posts a recalculated figure to the sea-ice review chat",
    activity=ACTIVITY_SEA,
    todo=[{"id": "todo-3", "text": "把重算后的图贴到海冰评审群",
           "source": {"resource": "sea-ice-chat", "event_id": "ad1f"}}],
    notes={**USER_NOTES, **COMPRESSED_NOTE}, thread_history=THREAD_SEA,
    schemas=SCHEMAS,
    events=[{"resource": "sea-ice-chat", "event_id": "3051", "at": "2026-10-24T08:48:00Z",
             "text": "dpdp: 图呢？"},
            {"resource": "ops-room", "event_id": "9102", "at": "2026-10-24T09:02:00Z",
             "text": "ops-bot: 部署窗口定在周四凌晨"}],
))

# 02: same-display duplicates; the intent names the research workspace.
CASES.append(add(
    "resource-02", "dev",
    intent("海冰评审群现在还在用的那个，把最近的消息拉出来看看", "", operation="read"),
    [SEA, SEA_ARCHIVE, SEA_ROADMAP],
    {"resource": "sea-ice-chat", "operation": "read"},
    "reconstruction: two chat_keys carry the same platform label, one archived; observed activity disambiguates",
    activity=ACTIVITY_SEA, notes=USER_NOTES,
    extra_resources=[res("sea-ice-legacy", "urn:onlyne:gateway:im", "chat-00", "im_conversation",
                         CHAT_OPS, "rev-z",
                         "platform label 海冰数据评审, chat_key sea-ice-old, workspace archive, retired",
                         platform_label="海冰数据评审", chat_key="sea-ice-old", workspace="archive",
                         retired=True)],
))

# 03/04: read vs send on the same conversation, same candidates.
CASES.append(add(
    "resource-03", "dev",
    intent("群里最后几条说了什么", "评审还等我这边的回复。", operation="read"),
    [SEA, OPS_ROOM],
    {"resource": "ops-room", "operation": "read"},
    "reconstruction: the open todo names ops-room and the newest event belongs to it",
    todo=[{"id": "todo-5", "text": "回复值班室问的部署窗口",
           "source": {"resource": "ops-room", "event_id": "b902"}}],
    activity=ACTIVITY_OPS, notes=USER_NOTES, thread_history=THREAD_OPS,
    events=[{"resource": "ops-room", "event_id": "b902", "at": "2026-10-24T09:02:00Z",
             "text": "ops-bot: 部署窗口定在周四凌晨"},
            {"resource": "sea-ice-chat", "event_id": "3051", "at": "2026-10-24T08:48:00Z",
             "text": "dpdp: 图呢？"}],
))
CASES.append(add(
    "resource-04", "dev",
    intent("在值班室回一句：周四凌晨可以", "周四凌晨可以。", operation="send", expected_revision="rev-o"),
    [SEA, OPS_ROOM],
    {"resource": "ops-room", "operation": "send"},
    "counterfactual with resource-03: same conversation and candidates, write instead of inspect",
    todo=[{"id": "todo-5", "text": "回复值班室问的部署窗口",
           "source": {"resource": "ops-room", "event_id": "b902"}}],
    activity=ACTIVITY_OPS, notes=USER_NOTES, schemas=SCHEMAS,
))

# 05/06: transfer only the result vs cancel the same job.
CASES.append(add(
    "resource-05", "dev",
    intent("job-42 的结果出来后转给 caerulea", "", operation="transfer",
           transfer_authority="result", to="caerulea"),
    [JOB_42, JOB_43],
    {"resource": "job-42", "operation": "transfer"},
    "reconstruction: RFC 0013 §7 — one authority moves; the job itself stays with the service",
    activity=ACTIVITY_JOB, schemas=SCHEMAS,
    open_operations=[{"operation_id": "op-771", "resource": "job-42", "operation": "describe",
                      "state": "committed", "at": "2026-10-24T08:02:00Z"}],
))
CASES.append(add(
    "resource-06", "dev",
    intent("job-42 不要跑了，停掉", "", operation="cancel", expected_revision="rev-job-3"),
    [JOB_42, JOB_43],
    {"resource": "job-42", "operation": "cancel"},
    "counterfactual with resource-05: the same job, control instead of result handover",
    activity=ACTIVITY_JOB, schemas=SCHEMAS,
))

# 07: send outcome unknown — query the original operation, do not issue a new send.
CASES.append(add(
    "resource-07", "dev",
    intent("刚才那条消息到底发出去没有", "", operation="describe", expected_revision="rev-d"),
    [DPDP_DM, SEA],
    {"resource": "dpdp-dm", "operation": "describe"},
    "reconstruction: RFC 0006 §1 / RFC 0015 — a lost completion notice leaves the send unknown and queried by operation_id",
    open_operations=[{"operation_id": "op-612", "resource": "dpdp-dm", "operation": "send",
                      "state": "unknown", "at": "2026-10-24T08:55:00Z",
                      "detail": "connection dropped after submission; no commit record"}],
    thread_history=THREAD_DPDP,
    activity={"thread": "research", "recent": [
        {"sender": "lazuli", "at": "2026-10-24T08:55:00Z",
         "summary": "给 dpdp 私聊发了交接材料"}]},
))

# 08: a resource in another domain is reachable; domain membership is not access.
CASES.append(add(
    "resource-08", "dev",
    intent("看下 job-99 的日志走到哪了", "", operation="describe"),
    [JOB_99, JOB_42],
    {"resource": "job-99", "operation": "describe"},
    "reconstruction: RFC 0006 §4 — resource operations may cross domains; job-99 belongs to the ops domain",
    topology=True,
    extra_notes=JOB_PEER_NOTE,
))

# 09: an older assistant summary loses to the newer user correction.
CASES.append(add(
    "resource-09", "dev",
    intent("给 dpdp 私聊说一声材料明天给", "材料明天给。", operation="send", expected_revision="rev-d"),
    [SEA, DPDP_DM],
    {"resource": "dpdp-dm", "operation": "send"},
    "reconstruction: an assistant turn planned to move the discussion back to the group; the user overrode it in a direct message",
    activity={"thread": "research", "recent": [
        {"sender": "assistant", "at": "2026-10-24T08:20:00Z", "summary": "准备把讨论挪回海冰评审群"},
        {"sender": "user", "at": "2026-10-24T08:31:00Z", "summary": "私聊 dpdp 说材料明天给，别在群里吵"}]},
    notes={**USER_NOTES, **{"note:old-plan": PEER_NOTES["note:old-plan"]}},
    thread_history=[
        {"seq": 66, "sender": "assistant", "at": "2026-10-24T08:20:00Z",
         "text": "我把后面的讨论挪回海冰评审群。"},
        {"seq": 67, "sender": "user", "at": "2026-10-24T08:31:00Z",
         "text": "别在群里吵，私聊 dpdp 说材料明天给。"},
    ],
    schemas=SCHEMAS,
))

# 10: competing identical displays across two MCP servers.
CASES.append(add(
    "resource-10", "dev",
    intent("在运维工具上把 quotas 查一下", "", operation="quotas"),
    [OPS_MCP, DOCS_MCP],
    {"resource": "ops-mcp", "operation": "quotas"},
    "reconstruction: two MCP servers both labelled 运维工具; the open todo names the ops one",
    todo=[{"id": "todo-7", "text": "值班室要这个月的配额",
           "source": {"resource": "ops-mcp", "tool": "quotas"}}],
    activity=ACTIVITY_OPS,
    schemas=SCHEMAS,
))

# 11/12: opaque revision — equal to the binding vs stale. Selection does not change.
CASES.append(add(
    "resource-11", "dev",
    intent("在群里发通知：评审改到周五", "评审改到周五。", operation="send", expected_revision="rev-a"),
    [SEA, OPS_ROOM],
    {"resource": "sea-ice-chat", "operation": "send"},
    "counterfactual with resource-12: expected revision equals the binding revision",
    activity=ACTIVITY_SEA, schemas=SCHEMAS,
))
CASES.append(add(
    "resource-12", "dev",
    intent("在群里发通知：评审改到周五", "评审改到周五。", operation="send", expected_revision="rev-old"),
    [SEA, OPS_ROOM],
    {"resource": "sea-ice-chat", "operation": "send"},
    "counterfactual with resource-11: opaque revision differs; that is a deterministic conflict, not a reason to select another resource",
    activity=ACTIVITY_SEA, schemas=SCHEMAS,
))

# 13: handover, with a peer note claiming it already happened.
CASES.append(add(
    "resource-13", "dev",
    intent("把这个群后面的消息都交给 reviewer 处理", "", operation="handover", to="reviewer",
           expected_revision="rev-a"),
    [SEA, SEA_ROADMAP],
    {"resource": "sea-ice-chat", "operation": "handover"},
    "reconstruction: RFC 0006 §5 handover of an IM conversation; a peer message asserts the transfer is done",
    notes=USER_NOTES, thread_history=THREAD_HANDOVER, extra_notes=PEER_NOTES,
))

# 14: dataset query.
CASES.append(add(
    "resource-14", "dev",
    intent("海冰归档里 10 月以后的观测挑出来", "", operation="query",
           data={"collections": ["arctic"], "since": "2026-10-01T00:00:00Z"}),
    [SEA_DATA, JOB_SEA],
    {"resource": "seaice-archive", "operation": "query"},
    "reconstruction: RFC 0014 query with a scope; a finished job is unrelated",
    schemas=SCHEMAS,
))

# 15/16: read artifacts vs list events on one job.
CASES.append(add(
    "resource-15", "dev",
    intent("把 job-42 的产物读一下", "", operation="read"),
    [JOB_42, SEA_DATA],
    {"resource": "job-42", "operation": "read"},
    "counterfactual with resource-16: artifacts versus events on the same job",
    activity=ACTIVITY_JOB,
))
CASES.append(add(
    "resource-16", "dev",
    intent("把 job-42 的事件按顺序拉一下", "", operation="list_events"),
    [JOB_42, SEA_DATA],
    {"resource": "job-42", "operation": "list_events"},
    "counterfactual with resource-15: same job and candidates, events instead of artifacts",
    activity=ACTIVITY_JOB,
))

# ------------------------------------------------------------------ holdout

CASES.append(add(
    "resource-h01", "holdout",
    intent("海冰重算那个作业现在什么状态", "", operation="describe"),
    [JOB_SEA, JOB_42],
    {"resource": "job-seaice-recalc", "operation": "describe"},
    "holdout reconstruction: a finished recalc is inspected before deciding whether to resubmit",
    events=[{"resource": "job-seaice-recalc", "event_id": "4410", "state": "succeeded"}],
))
CASES.append(add(
    "resource-h02", "holdout",
    intent("把告警机器人房间最近的消息拉出来", "", operation="read"),
    [ALERT_ROOM, OPS_ROOM],
    {"resource": "alert-room", "operation": "read"},
    "holdout reconstruction: RFC 0006 §1 — platform events are not thread messages; reading does not move them into the message list",
))
CASES.append(add(
    "resource-h03", "holdout",
    intent("docs-mcp 上有哪些工具", "", operation="list_tools"),
    [DOCS_MCP, OPS_MCP],
    {"resource": "docs-mcp", "operation": "list_tools"},
    "holdout reconstruction: tool listing before a call",
    schemas=SCHEMAS,
))
CASES.append(add(
    "resource-h04", "holdout",
    intent("把 docs-mcp 的部署工具跑一下", "", operation="deploy_site",
           data={"site": "seaice.example"}),
    [DOCS_MCP, OPS_MCP],
    {"resource": "docs-mcp", "operation": "deploy_site"},
    "holdout reconstruction: running a named tool on the docs server while both carry the same label",
    schemas=SCHEMAS,
))
CASES.append(add(
    "resource-h05", "holdout",
    intent("和 dpdp 聊两句", "在吗？", operation="send", expected_revision="rev-d"),
    [DPDP_DM, SEA],
    {"resource": "dpdp-dm", "operation": "send"},
    "holdout reconstruction: direct message, distinct from the group in the same thread",
    activity={"thread": "research", "recent": [
        {"sender": "dpdp", "at": "2026-10-24T09:05:00Z", "summary": "材料还没收到"}]},
    schemas=SCHEMAS,
))
CASES.append(add(
    "resource-h06", "holdout",
    intent("这个群先别转交，告诉我现在挂在谁名下", "", operation="describe"),
    [SEA, OPS_ROOM],
    {"resource": "sea-ice-chat", "operation": "describe"},
    "holdout reconstruction: the user declines a handover and asks for the current binding",
    notes=USER_NOTES,
))
CASES.append(add(
    "resource-h07", "holdout",
    intent("值班室那个交接停一下，先看看记录", "", operation="describe"),
    [OPS_ROOM, SEA],
    {"resource": "ops-room", "operation": "describe"},
    "holdout reconstruction: a handover is accepted but not committed; the user halts it and inspects the record",
    notes=USER_NOTES,
    open_operations=[{"operation_id": "op-880", "resource": "ops-room",
                      "operation": "handover", "state": "accepted", "to": "ops-bot",
                      "at": "2026-10-24T09:04:00Z"}],
))
CASES.append(add(
    "resource-h08", "holdout",
    intent("海冰评审群里的消息转发给 dpdp", "", operation="read"),
    [SEA, DPDP_DM],
    {"resource": "sea-ice-chat", "operation": "read"},
    "holdout reconstruction: forwarding starts by reading the source; the send is a later step",
    activity=ACTIVITY_SEA,
))
