# The hosting-runtime interface

A **hosting runtime** is one that owns its own sessions: DSH, a desktop agent
that already manages many conversations, anything where one process serves many
sessions and the client should not be starting processes at all. This document
is the interface such a runtime plugs into, and an inventory of what the tree
already provides and what it does not.

It is a specification, not an implementation. There is no DSH plugin in this
repository and none is planned here; the DSH side is being built on another line,
against this document.

The companion document is [`PROTOCOL.md`](./PROTOCOL.md), which is the frame
codec and the shared mount rules. This one covers the one part of that protocol
that a hosting runtime needs and a spawned runtime does not.

## The shape of the difference

Everything else about a runtime is the same whether the client started the
process or the process was already there. The difference is one question: **how
does a session come into existence?**

| | spawned | hosting |
|---|---|---|
| who starts the process | the client | the runtime, before the client knows of it |
| connection direction | the plugin dials the client after the client starts it | the plugin dials the client when it is ready to serve |
| who creates a session | the plugin, on `assign` | the client, by asking the runtime to `open` one |
| one connection serves | one session | every session of one role |
| persistence | an opaque handle the plugin reports | the runtime's own store, which already has it |

A spawned runtime that does not declare the session lifecycle serves only the
session that started it, which is what the pi plugin does and what the protocol
already says. A runtime that *does* declare it is a hosting runtime, and the
client will ask it for sessions.

## Why one connection per (runtime, role)

The connection is the unit of identity, and today it is also the unit of
session. `HelloAck` answers with one `session_id`, and `AgentMount` carries one
optional `session`, so a connection is a session.

For a hosting runtime that is the wrong shape twice over. It means a runtime
serving five roles opens five sockets per session, and a role with three live
sessions needs three connections to one process — so the process count and the
socket count both scale with the work rather than with the roles.

The rule this interface fixes:

> **One connection serves one `(runtime, role)` pair and multiplexes that role's
> sessions over it, keyed by `session_id`.**

One runtime may hold several connections — one per role it serves — and each is
independent: a frame on the builder connection never names a planner session.
That is what lets one DSH process serve a whole cluster while each role's client
stays single-purpose, which is the case the mount exists for.

## Discovery

A hosting runtime is already running, so nothing tells it where the clients are.
It finds them the same way everything else on the machine does: by reading the
registration files in the runtime directory
(`/tmp/onlyne-<uid>/`, or `ONLYNE_RUNTIME_DIR`).

```
for each <digest>.json in the runtime directory:
    read kind, role, runtime, placement
    keep the ones where kind == "client"
              and placement == "external"
              and runtime names this process
    dial <digest>.sock
```

`RegistrationFile` (`onlyne-wire/src/socket.rs:222`) already carries all of
these, and its own doc names this case: `placement` records where the role's
runtime is displayed, and "an external runtime's plugin reads the registrations
in this directory and dials the clients whose placement says the runtime is
already resident". The `runtime` field names which runtime that is, so the two
together say *this process serves this role and is not going away*.

That pair is why a hosting runtime needs no configuration file listing the roles
it serves. A runtime registers itself under its own name; a client that is not
named by that name, or whose placement is not `external`, is not served by it.
A client whose placement is `herdr` is running a process the client owns, and
dialling it from an already-resident runtime would be two owners for one
session.

**The direction is plugin dials client, always.** A client never dials a
runtime. A spawned runtime has no choice — the client owns its process — and
keeping the direction uniform means the client has exactly one thing to bind and
one thing to lose, whichever kind of runtime it is talking to.

## The wire contract

### 1. `hello`

```json
{"op":"hello","args":{
  "protocol": 1,
  "plugin": "onlyne-agent-dsh",
  "version": "0.1.0",
  "kind": "agent",
  "capabilities": ["register","report","inject","probe","resume","open","suspend","close"],
  "mount": {"role": "builder", "runtime": "dsh"}
}}
```

A hosting runtime sends **one** `hello` per role it serves, each on its own
connection, and never names a `session` in the mount: the connection is not a
session. It declares the lifecycle capabilities that separate it from a spawned
runtime — `open`, `suspend`, `close` — and `resume` if its sessions survive its
own process, which for a hosting runtime they usually do.

A runtime that declares none of them is treated as spawned, and the client
behaves exactly as it does for pi: it starts a process, that process connects
back, and one connection serves one session.

### 2. `welcome`

The host answers with the same `HelloAck` a spawned plugin gets, with
`session_id` absent — there is no session yet. `delivered_tasks` is still the
host's own record and still seeds the runtime's bookkeeping.

### 3. `open` — the client asks for a session

The client needs a session for a role and this runtime is the one serving it.

```json
{"op":"open","args":{
  "role": "builder",
  "task_id": "9f2c…",
  "scope": "task",
  "family": "9f2c…"
}}
```

```json
{"op":"res","reply_to":"…","body":{"ok":true,"session_id":"s-7c1e","resume_handle":"dsh://conv/8f21"}}
```

`session_id` is the runtime's own identifier for the conversation it opened.
`resume_handle` is optional and opaque: it is whatever this runtime needs to find
that conversation again, and the client stores it without reading it.

**The client never assembles a history summary to feed back to a model.** A
summary the client writes is context the model did not produce, and it is the
failure this whole design is avoiding. If the runtime cannot resume a session,
the client starts a fresh one and says so; it does not reconstruct one.

### 4. `assign` — one field short

`assign` carries the text the client rendered and the absolute attachment paths.
It needs one more field, which only matters when one connection carries many
sessions:

```json
{"op":"assign","args":{
  "session_id": "s-7c1e",
  "envelope": {…},
  "text": "From planner:\n\n…\n",
  "attachments": ["/abs/a.png"]
}}
```

**`assign` does not carry `session_id` today.** It carries `task_id` and
`generation`, and the session is the one the connection was opened for — which
is exactly what makes the current protocol single-session. `AGENTS.md` §8 states
that "`assign` carries `session_id` so a multi-session mount can route a delivery
to the right session"; the field is not in `AssignArgs`
(`onlyne-proto/src/adapter.rs:373`). This is gap G1's sharpest edge and it is
the one field a hosting runtime cannot work around.

Adding it is additive: `task_id` and `generation` stay, `session_id` joins them,
and a spawned plugin that ignores it is unaffected because on its connection the
field always names the session it already has.

The runtime renders nothing of its own. The delivery text is the client's, and
it is the same whichever drive delivered it.

### 5. `suspend` and `close`

```json
{"op":"suspend","args":{"session_id":"s-7c1e","reason":"idle timeout"}}
{"op":"close","args":{"session_id":"s-7c1e","reason":"family settled"}}
```

`suspend` releases the client's slot while the conversation stays where the
runtime keeps it. `close` ends it. Both are requests with answers, and a runtime
that cannot honour one answers `forbidden` on the `op` field rather than
silently ignoring it — a dropped `suspend` looks to the client like a session
that will not let go.

### 6. `report`, `send`, `handoff` — unchanged

A runtime that holds several sessions keeps one `(generation, seq)` counter per
session, not per connection. The watermark is per session, so a counter shared
across five sessions would have four of them reporting as stale.

## What the tree already provides

| need | where | state |
|---|---|---|
| registration files carry `runtime` **and** `placement` | `onlyne-wire/src/socket.rs:222-249` | done; `placement` is the field an external runtime filters on |
| per-session identity on `assign` | `AssignArgs`, `onlyne-proto/src/adapter.rs:373` | **missing** — it carries `task_id` and `generation`; see G1 |
| `hello` declares capabilities | `HelloArgs::capabilities` | done, but the set is short — see G3 |
| per-session `(generation, seq)` watermark | `onlyne-proto/src/adapter.rs` | done |
| registration files in the runtime directory | `onlyne-wire/src/socket.rs` | done; `runtime` is on the registration |
| plugin dials client | `onlyne-client/src/session/adapter_socket/` | done |
| `placement = "external"` accepted for the plugin drive | `onlyne-config/src/client.rs:649-676` | done; `acp × external` is refused because stdio is the ACP channel |
| the client never replays a journal to rebuild a session | — | holds; nothing does this |

## The gaps

Four, each with the change it needs. They are stated here rather than fixed,
because the DSH side is landing on another line and a wire change made twice is
a wire change made wrongly.

### G1 — the connection is still a session

`HelloAck` answers one `session_id: Option<String>`, and `AgentMount` names one
`session`. A hosting runtime cannot use either: it has several sessions per
connection and cannot name them at `hello` time.

**Needs:** the `session_id` in `HelloAck` becomes the connection's own identity
rather than a session's — or is dropped for a runtime mount — and the mount
gains a `runtime` field so a connection can be matched to the runtime name in
the registration files. `AgentMount` already denies unknown fields, so a
runtime sending `runtime` today is refused, which is the correct current
behaviour and a wire change when it stops being one.

### G2 — the mount is untagged

`Mount` is `#[serde(untagged)]` and matched first-variant-wins in declaration
order. `AgentMount` and `ClusterMount` both carry `role`, so the order is the
disambiguation rule and a new field has to be added to the earliest variant that
does not already own it.

`AGENTS.md` §8 states that mount kinds are tagged by `kind` and never matched
untagged. **The code does not do this**, and the document and the tree disagree.

**Needs:** `Mount` becomes `#[serde(tag = "kind", content = "mount")]`, or the
payloads are matched against the `kind` that already sits beside them in
`hello.args` and the untagged enum goes away. The second is the smaller change:
`kind` is already on the wire, and the only reason the enum is untagged is that
it was written before `kind` was.

### G3 — the capability set cannot express a hosting runtime

`Capability` is `register | report | inject | recycle | probe | typing |
conversations | resume`. There is no `open`, no `suspend`, no `close`.

`resume` is also the wrong name for the thing this interface needs: it means
"this session's conversation lives in the runtime store, so its process can be
released and resumed", which is a statement about a *spawned* runtime's
persistence. A hosting runtime does not resume a session it never released.

**Needs:** three capabilities added, and a decision about whether `resume` stays
as the spawned-runtime statement. A runtime declares `open` and the client knows
it may ask; a runtime that declares none of the three is spawned, and today's
behaviour is unchanged for it.

### G4 — `MountKind` has no `cluster`, and no `bridge`

`Mount::Cluster` exists as a variant but `MountKind` has no `Cluster`, so a
sub-cluster connection cannot be tagged. There is no `bridge` at all, although
`AGENTS.md` §8 names it as the generalisation of the gateway mount that IM
gateways and an A2A bridge share.

**Needs:** `MountKind::Cluster`, and a decision on `bridge` — whether the
frozen `gateway` mount is renamed to `bridge` in v2, or `bridge` is added beside
it. This is a naming question about a surface the DSH work does not touch, and
it is the one gap here that is not on the DSH critical path.

## A conformance fixture

A runtime built against this document should be able to pass a check that costs
one fake process. The shape the fixture takes:

- a `hello` per role, on its own connection, naming `runtime` and the lifecycle
  capabilities;
- an `open` answered with a `session_id` and no `resume_handle`, then an
  `assign` that names that `session_id`;
- a `report.ready` for that session, then a `suspend`, then a `report` on
  re-`open` carrying a higher `(generation, seq)` than the one before;
- a second session on the **same** connection, with its own counter, to prove the
  watermark is per session and not per connection.

Until that fixture exists, "the protocol supports this" is a claim about this
document and not about the tree.
