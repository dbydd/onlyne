# Onlyne v2 work split

`docs/v2-PLAN.md` is the design and this file is the work split, the way `docs/v1-CONTRACT.md`
was for v1. The plan wins on any disagreement; this file exists so two slices running in
parallel cannot invent divergent shapes for the same interface.

Every slice names: the files it owns, the interface it must land, and what proves it done.
A slice that needs a shape this file does not fix should stop and ask rather than settle it
locally, because the other side of the interface is being written at the same time.

## Rules that apply to every slice

- One class of fact has one owner (§2 of `AGENTS.md`). A slice that finds itself holding a
  second copy of a fact is building the bug the plan exists to remove.
- A refusal is a contract: its code and its text are asserted by tests. Do not reword one
  without changing the test in the same change.
- Schema changes bump the marker in the same change that changes the DDL, and the
  `unsupported schema` refusal stays byte-exact.
- Every slice leaves the workspace green: `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.

---

## Slice 1: the session table rekeys on `session_id`, and scopes appear

The plan's §"server、client、会话的重新划分" lines 200-251.

v1's session table is keyed by `task_id` (`projection.rs` backfilled the task id when a
session id was missing), which makes "one session serves several deliveries" inexpressible.
v2 keys it by `session_id` and records bindings in a second table.

### Interface

**Server schema (`crates/onlyne-store/src/server.rs`), marker `onlyne-server` 4 → 5:**

```sql
CREATE TABLE IF NOT EXISTS sessions(
  session_id TEXT PRIMARY KEY,
  role TEXT NOT NULL,
  generation INTEGER NOT NULL,
  seq INTEGER NOT NULL,
  agent_state TEXT NOT NULL,
  delivery_state TEXT NOT NULL,
  resource_state TEXT NOT NULL,
  recovery_substate TEXT NOT NULL,
  desired_json TEXT NOT NULL,
  observed_json TEXT NOT NULL,
  mismatch_count INTEGER NOT NULL,
  -- The mirror's own freshness: what a reader judges a stale row by. v1's
  -- mirror could be hours old and read as current.
  last_seen TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS sessions_role_idx ON sessions(role);
CREATE INDEX IF NOT EXISTS sessions_updated_idx ON sessions(updated_at);
CREATE TABLE IF NOT EXISTS session_tasks(
  session_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  bound_at TEXT NOT NULL,
  released_at TEXT,
  PRIMARY KEY (session_id, task_id)
);
CREATE INDEX IF NOT EXISTS session_tasks_task_idx ON session_tasks(task_id);
CREATE INDEX IF NOT EXISTS session_tasks_open_idx ON session_tasks(session_id, released_at);
```

**Client schema (`crates/onlyne-store/src/client.rs`), marker `onlyne-client` 2 → 3:** the
same two tables, same columns, same keys. The client's `task` table keeps its own key
(`task_id`) and is unchanged: it answers for a task, the session tables answer for a
session, and neither borrows the other's key.

**Proto (`crates/onlyne-proto`):**

- `Outcome` gains `Blocked`. It is a first-class result, not a synonym for `Failed`: the
  plan's ending rule settles a `oneshot` delivery whose second turn ended without a
  `complete` as `blocked`, and the board shows it as "waiting" rather than "failed".
- `SessionRow` (in `ops.rs`) is addressed by `session_id`. It keeps
  `task_id: Option<String>` because a reader wants to know which delivery a session is
  currently serving, but that field is now *derived from the open `session_tasks` row*,
  not the row's key.
- `SessionRow` gains `last_seen: Option<String>` beside `updated_at`: the caller prints it
  and judges freshness, and the server never decides staleness from it.
- `HandshakeArgs.live_tasks: Vec<String>` becomes
  `live_sessions: Vec<LiveSession>`, where

  ```rust
  pub struct LiveSession {
      pub session_id: String,
      /// The delivery this session is bound to, if any.
      pub task_id: Option<String>,
      /// A session the client holds but has released its process for.
      pub suspended: bool,
  }
  ```

  An absent or empty list requeues every unacknowledged row, which is what a client from
  an earlier build sends: the field is additive and the old spelling is the old behavior.

**Client session scopes** (`<workspace>/.onlyne/config.toml`, parsed in
`crates/onlyne-config`):

```toml
[client.session]
scope = "oneshot"   # oneshot (default) | task | role
idle_close = "2h"   # absent means the scope's own default
```

Semantics are the plan's table (lines 222-228) and are enforced entirely on the client:
the server delivers by role and knows nothing about scope. `task` keys on the task family
(`causality.family`), so a second delivery to one role in a family lands in the
session that served the first.

Whatever an unsupported `scope` value does, it MUST NOT silently become `oneshot`: it is a
configuration refusal with the line number, like every other spec error.

### What the implementation settled, and one gap it left

Three things were not fixed above and are recorded here so nobody invents a second
spelling of them:

- **A publish that names no delivery.** The mirror side expresses this as
  `SessionRow.task_id: Option<String>`, but `Report::Heartbeat.task_id` is a plain
  `String`, so the empty string is the only spelling a report has for "this session is
  between deliveries". The server reads `""` and an absent field alike. A future change
  that gives the report path an `Option` should delete the empty-string reading in the
  same change rather than keeping both.
- **A no-op heartbeat must not advance the projection tuple.** The acceptance bullet below
  states the observable; the client-side rule is that a beat whose verdict is "nothing
  changed" refreshes the in-memory liveness stamp and reaches the mirror so `last_seen`
  moves, while `generation`, `seq`, and `updated_at` stay where they were. A beat does
  advance the tuple when it reports a change — that gate is what orders every projection
  write, so it is not decoration.
- **The family-to-session map is in memory only.** `task` scope's key (a family) lives in
  the client's slot table, and no table persists it, so "the next delivery of the family
  resumes the same conversation" holds while the client process lives and does not survive
  its restart. This is a known gap, not an oversight: crossing a client restart needs a
  durable family key that no table carries yet. The plan's `task`-scope row says
  "resumes the same conversation where the runtime supports it", and that promise is
  currently bounded by the client's lifetime.

### Acceptance

Scenarios (one test binary, `crates/onlyne-testkit/tests/scenarios.rs`), each asserting an
externally visible contract:

- **Rekey.** One session serves two deliveries in turn; `query_sessions` answers one row
  addressed by the session id, and `session_tasks` carries two bindings, the first with a
  `released_at`.
- **Scope `oneshot`.** Two deliveries to one role land in two sessions.
- **Scope `task`.** Two deliveries of one family to one role land in one session; a
  delivery of a different family lands in a different one.
- **Scope `role`.** Deliveries up to `max_sessions` reuse the pooled sessions, and a third
  concurrent delivery waits rather than opening a session past the bound.
- **Suspension.** An idle `task`-scope session suspends (process released, slot counted as
  free), and the next delivery of its family resumes it rather than opening a new session.
  A runtime that cannot resume degrades to "process alive, session alive" and the scenario
  asserts that degradation rather than skipping it.
- **Adoption.** A client that restarts while a pane-hosted runtime lives re-reports its
  sessions through `hello.live_sessions` and the server's adoption requeue skips those
  deliveries. A client that reports nothing requeues everything.
- **Freshness.** A session whose heartbeat only refreshes `last_seen` writes no new
  `updated_at`, publishes no event, and still answers a reader with the `last_seen` it
  holds.

A v1 database (marker 4 server / 2 client) is refused with the byte-exact
`onlyne: unsupported schema; v1.0.0 does not migrate`.

---

## Slice 2: `drive` and `placement` replace `backend`

The plan's §"驱动与放置" lines 253-289.

v1's `backend` enum presses two questions into one value: *how the client talks to the
runtime* (`acp` is a way of talking) and *where the runtime process is displayed*
(`herdr`, `orca`, `zellij` are places). `headless` is `exec` under another name. A role
therefore cannot say "an ACP agent, started by the client, with no pane", which is the
ordinary ACP shape.

### Interface

**Drive is a property of the runtime and lives in the spec** (`crates/onlyne-config`,
`spec.toml`). `[[client]].session_command` and `[[client]].timeout`'s command half are
replaced by one table per role:

```toml
[[client]]
role = "builder"

[client.runtime]
drive = "plugin"          # plugin | acp | exec
command = ["pi", "--mode", "rpc", …]   # placeholders {session} and {task} keep working
```

**Placement is a property of the machine and lives in the workspace config**
(`<workspace>/.onlyne/config.toml`):

```toml
placement = "herdr"       # herdr | orca | zellij | headless | external
```

An absent `placement` probes `herdr`, `orca`, `zellij` in that order and falls back to
`headless`. An absent `drive` is `plugin`, which is what every role in the tree is today.

The `backend` key is **deleted** from both files. A configuration that still carries it is
refused with the file and line and a message naming the replacement — not silently
ignored, because a cluster that keeps running under a policy nobody set is the failure
mode this slice exists to prevent.

`acp` pairs only with `headless`, and the configuration validator refuses every other
combination by name. The reason is physical: stdio carries the ACP channel and cannot also
be a pane's terminal.

`ONLYNE_BACKEND` keeps working for tests and for a host that has no terminal host to
probe; it now selects **placement**, not a fused backend.

### Acceptance

- Every combination in the plan's `drive × placement` table is either accepted or refused
  with the validator's own message; a table-driven test walks the matrix.
- `acp × headless` runs a session to `acked` through the exec-driven ACP path.
- A workspace with `placement = "external"` starts no process of its own and accepts a
  plugin that dials in.
- A config carrying `backend` is refused, with the line number, in both files.

---

## Slice 3: what reaches the model, and how a turn ends

The plan's §"投递格式与角色能力" lines 291-334 and §"事件钩子" lines 336-351. This is the
slice that removes the reason a model reads itself as a relay node, so its contracts are
textual and must be asserted byte for byte.

Three parts, landable in this order.

### 3a. One delivery template

The client renders delivery text from **one** template; v1 rendered it once per plugin
(JavaScript in the pi plugin, Rust in the ACP backend). Task id, hop, budget, and
generation leave the body — a tool call carries them — and upstream content is always
quoted and labelled as material. The exact shape is the plan's block at line 314.

Model-visible template text is English, matching the runtime's system prompt. The template
is the one place in this system where wording is a contract: a **golden-text test** pins
the rendered bytes for a delivery with a body, one reference block, and one attachment.

A delivery with no upstream reference material must not render an empty labelled block.

### What the implementation settled, and one gap it left

This above did not fix these, and they are recorded here so nobody invents a
second spelling of them:

- **The renderer is `onlyne-client`'s `delivery` module**, one pure function over
  the source, the body, the upstream material, and the attachment paths. Its
  golden test is a table at the bottom of the same file, so the bytes are pinned
  without a server, a client, or a runtime.
- **The rendered text travels in `AssignArgs`**, as `text`, with the absolute
  paths it names in `attachments`. That one field is what every drive injects:
  the plugin injects it, a self-driven backend is prompted with it, and the
  `config_get{key:"stdin:<text>"}` route carries it for a plugin without
  `inject`. `prose` stays beside it, because role prose is delivered through the
  runtime's instruction layer (3b) and the plugin's own marker decides when.
- **The client writes a delivery's attachment**, under
  `<workspace>/.onlyne/tmp/attachments/`, before the text that names it is
  rendered. The path in the text has to name a file that exists, and a plugin
  that both wrote the file and injected the path could name one that does not.
  A failed write drops the line and leaves the rest of the delivery standing.
- **Upstream reference material has no producer yet.** The template renders the
  block, `render` takes it as an optional input, and no path in the tree fills
  it: `complete(details)` delivering to the next hop is 3b/3c, and the envelope
  or frame field it rides is that slice's to settle. Until then every delivery
  renders the no-reference shape.

  **3b/3c settled this by deferring it, and the reason is a real choice rather
  than an omission.** Two answers are defensible — the reference is the sending
  session's own `details`, or it is the body of the envelope that session is
  serving, which is what the golden text above shows (`Reference material from
  reviewer` under a task sent by *planner*). They differ in whose words travel
  and in where the attribution goes, since the reference's `from` is not the
  envelope's sender in the second reading; and a handoff can be minted before
  the session completes, so the value has to be answerable at envelope-build
  time. `details` therefore goes upward only for now — the completion envelope's
  body, read by the originator — and the reference's own slice carries the
  two-client chain as its proof (A→B task, B completes with `details`, A reads
  the body; B→C handoff shows the reference).

### 3b. Obligations as tools

- **pi** keeps three tools (`onlyne_send`, `onlyne_handoff`, `onlyne_complete`). Their
  descriptions state effect and precondition only — no identity language, no protocol
  vocabulary, no urging.
- **ACP** sessions mount `onlyne mcp` through `session/new`'s `mcpServers`, which every ACP
  agent must support. The client issues the token and passes it in the child's
  environment; the `tools` mount kind carries it (`AGENTS.md` §8).
- **Role prose is injected at the runtime's instruction layer**, not as one conversation
  message: for pi, through the runtime's system-prompt extension point; for ACP, written
  into the workspace instruction file before the session opens.

  For ACP that file is **`<workspace>/AGENTS.md`**, in a delimited client-owned
  block: replaced when present, appended when absent, and no operator byte
  outside the block is ever touched. `onlyne-acp` carries no instruction field,
  so a file is the only vehicle, and the agents.md convention is the one this
  repository's own world uses. **Known gap:** agents differ in which filename
  they read — claude-code reads `CLAUDE.md` — so an agent that reads another name
  will not see the prose until the filename is wired to the spec's agent package,
  which is its own slice rather than a table of guesses written now.
- **payload-v2 is deleted**: `out/<task-id>.md`, the grammar block, `onlyne report
  check|write|path`, and the `onlyne-role-payload-v2` skill all go. Its invariants (one
  verdict per turn, handoff lines naming their recipient) move into the client-side check
  the `complete` tool performs, so a malformed completion is refused by the client rather
  than discovered by a file read.

#### 3b's interface: the `tools` mount

An ACP session has no plugin connection — the client spawned the agent itself, so there is
nothing on the adapter socket to carry the session's obligations. `onlyne mcp` is that
connection, and these are the pieces both halves build against.

**The mount.** `MountKind` gains `tools`, and `Mount` gains `Tools(ToolsMount)` placed after
`Cluster` and before `Admin`. Untagged matching is first-match-wins, and `ToolsMount` carries
a field no earlier variant owns, so a payload that reaches it has already failed agent,
gateway, and cluster; `Admin` stays the `null` arm.

```json
{"op":"hello","args":{"protocol":1,"plugin":"onlyne-mcp","version":"<crate version>",
  "kind":"tools","capabilities":[],"mount":{"token":"<uuid>"}}}
```

`ToolsMount { token: String }` and nothing else. The token *is* the binding: the client mints
one when it opens a session and records it against `(role, session_id, generation)`, so the
connection's role and session come from the client's own record rather than from a field the
caller supplies. A mount that names its own role would let a caller speak for a session it
never held.

**The ops.** A tools mount may send `send`, `handoff`, `report`, and `detach`: the agent
mount's set minus everything about process lifecycle (`session_register`, `assign_ack`,
`hello`'s task bookkeeping), because this connection holds no process. Anything else answers
`forbidden` naming the op and the kind, like every other mount. `hello` is answered with the
same `HelloAck`, so a tools mount learns the host's protocol and the role it speaks for. An
unknown, expired, or retired token answers `unauthorized` with field `token` and the
connection closes without a welcome.

**The client is the checkpoint.** The tools mount carries no policy of its own: the hop
budget, the relay requirement, the family rules of `handoff`, the completion's shape, and
3c's ≤ 64 KiB `details` cap are enforced where they already live — in the client's handling
of the op. A constraint added to the pi plugin's tools must therefore be added to the
client's op handling, not to `onlyne mcp`, or the two drives would refuse differently.

**The MCP face.** `onlyne mcp` speaks MCP over stdio (`initialize`,
`notifications/initialized`, `tools/list`, `tools/call`) and carries three tools — the same
three names pi's plugin registers, so a role's obligation vocabulary is one vocabulary:

| tool | required | optional |
|---|---|---|
| `onlyne_send` | `to`, `text` | `kind` (`note` default, or `task`), `image` |
| `onlyne_handoff` | `to`, `text` | `image` |
| `onlyne_complete` | `outcome`, `summary` | `details`, `files` |

`outcome` is one of `done`, `failed`, `cancelled`, `blocked` — the proto's `Outcome`, not a
second list. The process dials the client's adapter socket lazily on the first tool call and
keeps that one connection; the socket comes from `ONLYNE_SOCKET` and the token from
`ONLYNE_MCP_TOKEN`, both placed in the agent child's environment by the client. A refused
call travels back as the tool result's error text verbatim, so the model reads the host's own
sentence rather than a paraphrase.

**The three tools and pi's three tools are one vocabulary**, which is why `onlyne_send`
carries `kind`: without it an ACP role could note and hand on, but never assign work, and the
drives would differ in what a role can *do* rather than only in how the model reaches the
tools. Same names, same argument names, same meanings — one obligation vocabulary that two
drives mount. `kind`'s default is `note`, because a model that means to hand work on says so.

**The client's half.** `crates/onlyne-acp` already models the mount point —
`McpServer::Stdio { name, command, args, env }` and `new_session(cwd, mcp_servers)` — and
`crates/onlyne-client/src/backend/acp/session.rs` passes an empty list today. The client fills
it with one entry:

```json
{"type":"stdio","name":"onlyne","command":"<the onlyne entrypoint>","args":["mcp"],
 "env":[{"name":"ONLYNE_SOCKET","value":"<the client's adapter socket>"},
        {"name":"ONLYNE_MCP_TOKEN","value":"<the session's token>"}]}
```

`command` is the `onlyne` entrypoint the client resolves for its own daemons — the same
resolution that produces `onlyne: missing binary <path>`, not a fresh PATH lookup, because the
two answers must not disagree on a machine where several builds are installed. The token is
minted when the session opens and lives in the session's own state, not in a table: nothing
outside the session may hand it out, it dies with the session, and a session that reopens gets
a new one. The env is the only place it is written — it is a capability, so it never reaches
a log line, a fault, or a ledger row.

**The tools mount is not a plugin.** The client accepts it on the same adapter socket, but
nothing about it registers a task, holds a process, or reports lifecycle: a tools call is an
obligation performed for the session its token names, and the client refuses one whose token
belongs to a session that has ended.

**A tools mount names nothing session-scoped; the token does.** On this path the client
*stamps* `task_id` on `report` and `handoff` from the session's open binding, and stamps
`envelope.from` on `send` as the token's role — the bridge supplies the recipient, the text,
the kind, and the image, and nothing else. That is why no tool argument names a task id, and
why `HelloAck.delivered_tasks` is not consulted by this path: a caller that had to be told its
own task id could be lied to about it, and one that could claim another role's name would turn
the ACL into a check of a claim rather than of a fact.

**`send` is not a continuation of the session's task; `handoff` is.** So the two drives must
agree with the shape a plugin already builds (`sendEnvelope`, `plugins/onlyne-agent-pi/src/protocol.mjs`),
which means `send` carries none of the session's family:

- `kind: "task"` is a **root**: a fresh task id, `hop: 0`, `attempt: 0`, a fresh `op_id`, and
  no family, budget, origin, or deadline taken from the session being served. It is never
  `Causality::child_of` — that is `handoff`'s, and it is what makes asking another role for
  work a different act from handing them yours.
- `kind: "note"` carries no causality and no `op_id` at all, which is also that function's
  shape.

The **hop budget is therefore checked on `handoff`**, where a family is being continued; a
`send` whose root starts a budget of its own is never refused for budget, so a spent budget
stops a forward and never new work.

An empty `task_id` on a tools-mount frame means "the session's own open task". A non-empty one
that disagrees with it is refused `forbidden` on field `task_id` rather than silently
overwritten — a caller wrong about the session it speaks for is a bug in the caller, and a
silent correction hides it. A session with no open task answers `invalid` on field `task_id`
and names that, because a completion for a task nobody holds cannot be recorded.

### 3c. One turn-end rule

`complete(outcome, summary, details?, files?)`:

- `summary` is one display line; `details` is the full result (≤ 64 KiB) delivered
  verbatim to the next hop and the originator; `files` is absolute paths.
- The ledger's 200-character head is a display field and appears in **no** model-visible
  text.

- An explicit `complete` is the main path.
- A turn that ends without one gets **one** neutral nudge:
  `If this task is finished, report it with onlyne_complete; if something is missing, say what.`
- A second turn ending without one settles the delivery: `oneshot` becomes `blocked`
  (`Outcome::Blocked`, added in slice 1), while `task` and `role` sessions go idle and the
  board shows "waiting".
- A turn that ends in a `handoff` without a `complete` travels the same path — the nudge,
  then the same settlement.

Each step publishes an event: `turn_end_without_complete`, `delivery_blocked`, `handoff`.
What to *do* about a blocked delivery is operator policy and belongs to hooks, never to
the delivery path.

#### 3c's interface: how a nudge travels

The turn-end rule has one owner, the client, so the client is what tells a session that its
turn ended without a completion. A session the client drives itself (`acp`) takes the text as
its next prompt. A plugin-driven session is the plugin's own process, so the text needs a
frame: **`nudge { task_id, text }`**, added to the host-to-plugin set beside `assign` and
`probe`.

- It carries no envelope, no prose, no attachments, and it is not a delivery: the task stays
  open, the delivery's own rendered text is not sent again, and nothing in the plugin's turn
  bookkeeping is reset by it. v1's ladder re-injected the whole assignment and counted the
  rungs; this frame exists so that it cannot.
- The plugin injects `text` through the same channel an assign's text takes, and answers `ok`
  once it has handed the text over — handing over is what the answer claims, not that the
  model read it.
- **A plugin that declared no `inject` is never sent one.** The client settles the delivery at
  that turn end instead: a drive that cannot be nudged must not be told it was. That is also
  the rule for a one-shot drive whose process has already exited — the turn that ended is the
  last one there will be, so the first ending settles.
- The text is 3c's sentence verbatim. The plugin composes none of it, and keeps no copy of it.

With this frame in place, the plugin's own idle ladder, its `idleReminders` knob, and its
reminder wording have no remaining caller: the client owns the count, the wording, and the
settlement, and a second copy of them in a plugin is the bug this slice removes.

Constraints the client enforces while handling a tool call (hop budget, relay
requirement) refuse with a message naming what is missing. The plan moves these checks
here from the pi plugin, where they were the plugin's private guard.

### Acceptance

- The golden-text test pins the rendered delivery, including the no-reference-material
  case.
- A scripted session that never calls `complete` receives exactly one nudge and then
  settles `blocked`; its events are asserted in order.
- A session that hands off and never completes follows the same two steps.
- An ACP session reaches `onlyne mcp`'s tools through `mcpServers` and completes through
  them.
- No file under `.onlyne/out/` is written by any path, and grep finds no reader of it.
- A `complete` carrying a `details` body over the cap is refused with the cap named, and
  one at the cap passes.
