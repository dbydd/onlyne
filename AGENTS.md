# Onlyne Execution Contract (v2)

This repository builds a **small, Rust-based channel and routing layer for agents, scoped to a workspace**.

Read this file before changing anything. `docs/v2-PLAN.md` is the settled design and this file's source; where the two disagree, the plan wins and this file is stale.

## 0. Status: what has landed

v2 lands in phases. This file describes the v2 contract as a whole, so a section
can still describe behavior that is designed and not yet built — `docs/v2-REMAINING.md`
is the record of what is in which state, and it is the one to read for that.

| phase | content | state |
|---|---|---|
| zero | v1 defect fixes on paths v2 keeps | done |
| one | structure, no behavior change: `onlyne-wire`, runtime-directory sockets and registration files, crate merges and splits, forwarding-layer removal, this file | done |
| two | behavior: session table rekey and scopes, drive/placement split, delivery rendering, settlement rules, declarative routes, spec edit ops, event hooks, liveness in memory, one retry classification | done |
| three | interfaces: `view` reducer, TUI, `onlyne-web` | done |

Landed as v2.3: the **chat addressing protocol** (`docs/v2.3-CHAT-PROTOCOL.md`). A
conversation's identity is an application-layer chat key carried in the delivery text's first
line; the gateway is one `[[client]]` serving every conversation in its registry; a role is
the synthesis of a set of conversations. The core changed not at all: `Principal`, both
schemas, and `crates/onlyne-client/src/delivery.rs` are untouched, and the scenario suite's
case 22 proves the shape end to end.

Landed on top of it, in the same release, are four shapes the gateway needed rather than the
core: **reply threads** (`docs/v2.4-REPLY-THREADS.md`) where a conversation is a message list,
a reply names any message by a unique id prefix, and a reply and a reference are two different
links; **a control word settles the task it names** (`docs/v2.5-CANCEL-NOT-ENFORCING.md`), so a
`cancel` the server reported `ok` for cannot be revived by a restart; **the pi resume argv**
(`[client.runtime].resume`), because re-running the command that opens a conversation starts a
second one; and **a view that adopts a ledger's terminal state**
(`docs/v2.6-VIEW-KEEPS-A-STATE-THE-STREAM-GOT-WRONG.md`), so a board stops showing a settled
delivery as in flight.

One phase-two item is done with a named gap rather than closed: a **hosting
runtime** — one that owns its sessions, like DSH — has an interface and a
specification (`crates/onlyne-adapter/HOSTING-RUNTIME.md`) and four protocol
gaps listed there, because that runtime is being built on another line.

Landed in phase one: the `onlyne-wire` crate (frame codec plus the runtime
directory and registration files), sockets moved out of the workspace tree, the crate
consolidation in §3, the forwarding layer's removal, and a scenario suite under
`crates/onlyne-testkit/tests/`.

Landed in phase two, by slice:

- **Sessions.** Both session tables are keyed by `session_id` with bindings in
  `session_tasks`; `HandshakeArgs.live_sessions` replaced `live_tasks`; `Outcome::Blocked`
  exists; `[client.session]` carries `scope` and `idle_close`; the scopes
  (`oneshot`/`task`/`role`) are enforced on the client; suspend and resume ride the runtime's
  `resume` capability; and a held session survives a server-link loss by re-reporting itself
  at the next hello.
- **Drive and placement.** §11's split is in: `[client.runtime]` carries `drive` and
  `command`, the workspace config carries `placement`, and the validator's matrix is the one
  checkpoint for which pairs are legal.
- **What reaches the model.** §12 is in: the client renders the delivery from one template
  (`onlyne-client`'s `delivery` module) and it travels in `AssignArgs.text` with the attachment
  paths it names; role prose reaches pi as a system-prompt section and an ACP session as a
  client-owned block in `<workspace>/AGENTS.md`; pi keeps three tools and an ACP session gets
  the same three through `onlyne mcp` mounted in `session/new`'s `mcpServers`; and the turn-end
  rule is the client's — one neutral nudge, then settlement — with the plugin's own idle ladder
  and relay guard deleted rather than kept beside it.
- **payload-v2 is deleted.** No report file, no grammar, no `onlyne report check|write|path`,
  and no `onlyne-role-payload-v2` skill. A completion's `details` rides the completion
  envelope's body to the originator, and an operator reads a task's ending from the ledger.
- **Declarative route edges.** Reach and obligation are two declarations.
  `[[client]].allowed_targets` is the ACL — a role may address exactly those roles — and
  `[[client]].owes_targets` is the obligation: a session of that role owes those roles a
  delivery before it may report a terminal outcome. Absent `owes_targets` owes nothing, an
  obligation the reach cannot satisfy is refused at load, and the client's completion guard
  enforces it on both drives at one checkpoint. `relay_required`, `relay_required_count` and
  `relay_count` are refused by name, and the hop budget is checked at the client rather than
  left to the model.
- **The spec surface.** `SpecGet` returns the parsed spec beside its source hash;
  `SpecApply` takes typed edits, applies them with `toml_edit` so the operator's comments
  survive, and reloads. `subscribe` is the one continuous stream, and `spec_reloaded` makes
  both front ends re-read the registry — a reload usually arrives in the subscribe's replay
  page, not on the live stream.
- **Event hooks.** `[[hook]]` in `spec.toml` runs an operator's script after an event is
  persisted, at-least-once, resuming from the last successful `seq`.
- **Liveness in memory.** A heartbeat refreshes an in-memory `last_seen`; only a change in
  projection content is persisted and published, and every mirrored row carries `last_seen`
  so a reader can judge freshness.
- **One retry classification.** `onlyne_proto::Retry` answers `Never`, `AfterHuman` or
  `UnderBackoff`, and both the wire's error codes and the network's failures answer in it.
  A redial delay is jittered, so a cluster that lost its server does not come back as a crowd.

Landed in phase three: the `view` reducer in `onlyne-proto` that both front ends share,
the TUI rebuilt on it as three pages and no map, and `onlyne-web` as an optional
separately-installed binary. `onlyne-web` is excluded from the workspace, so the core
build needs no Node.

Landed after phase three, as v2.2: the **IM gateway**, a host process rather than a role
(`docs/v2.2-IM-GATEWAY-PLAN.md`, `docs/v2.2-IM-GATEWAY-3B.md`). One IM conversation is one
role: each conversation is an ordinary `[[client]]` with its own key, ACL row, and inbox, and
the gateway holds one ordinary role link per conversation. An inbound platform event runs the
operator's `[[im.push]]` scripts; an outbound delivery is rendered by a platform backend the
gateway spawns. The server changed not at all: a conversation's mail arrives as ordinary role
delivery.

**That identity model was superseded by v2.3** (`docs/v2.3-CHAT-PROTOCOL.md`): a conversation's
identity is now an application-layer chat key in the delivery text, the gateway is one
`[[client]]` serving every conversation in its registry, and a role is the synthesis of a set
of conversations — a persona that serves several of them at once. The paragraph above
describes the 2.2.0 release, not the tree.

Its own module, not a `onlyne` verb: `onlyne-im` is a separate binary (`init`, `generate`,
`login` (with `--interactive`), `setup`, `run`, `check`), the way `onlyne-web` is, and the core build links in no chat SDK. A
gateway root holds `.onlyne/config.toml` (`[server]`, `[[im.backend]]`, `[[im.push]]`) and one
workspace per conversation under `conversations/<slug>/`, where the directory name is an
operator label with no protocol meaning and the conversation's identity is the chat key beside
it. A conversation's runtime files (`chat.jsonl`, `media/`) live in that workspace. `generate`
writes the workspace; the cluster half is the gateway role's, written once by `init`-then-first-
`generate` — one `[[client]]` row and both ACL lists through the server's admin socket, not one
per conversation (`docs/v2.3-CHAT-PROTOCOL.md` §7.5).

When you finish a phase-one or phase-two item, update this table and the section it
touches in the same change.

## 1. Product boundary

Onlyne is:
- a server that routes envelopes, keeps the ledger, mirrors session state, records faults, and exposes admin operations with zero orchestration policy
- a client that runs one role's sessions inside one workspace
- one adapter protocol with tagged mount kinds, mounted on two sides: runtime and tool mounts attach to the client, bridge and admin mounts attach to the server
- a local channel layer for agents that need role-addressed messaging

Onlyne is not:
- workspace file sync
- agent work artifacts
- large media transfer
- a model runtime
- prompt management beyond role prose in `spec.toml`
- cron or workflow scheduling

If you catch yourself building outside that boundary, stop and cut scope back.

## 2. Principles

v1's three hold: zero orchestration, local-first, plainly predictable. v2 adds five.

1. **The transport delivers mail; identity belongs to the runtime and the role.** Onlyne writes exactly three things into a session: the delivery (source and body), the descriptions of the tools it registers, and at most one neutral nudge per turn. "Who you are" comes from role prose through the runtime's instruction layer, and protocol obligations exist as tools.
2. **Every class of fact has one owner.** Delivery state belongs to the server ledger; task verdict and session binding to the client; session content to the runtime; role definitions to `spec.toml`; placement to the machine running it. Everywhere else holds a copy.
3. **Declarative constraints are enforced mechanically at one checkpoint.** ACL, hop budget, and relay requirements are each enforced in exactly one place, and the model sees a constraint only when it touches one.
4. **Code is organized by meaning.** A file is a unit a reader can hold in their head at once.
5. **Tests prove usability.** Tests assert user-visible contracts, and what runs in the gate is the product.

## 3. Vocabulary

One word, one meaning, in prose and in code:

| word | meaning |
|---|---|
| role | one `[[client]]` entry in `spec.toml`: name, ACL, prose, session policy |
| client | one daemon per role: holds the server link, the session table, and the drive |
| runtime | the program that actually runs the model conversation: pi, DSH, an ACP agent |
| session | one conversation inside a runtime; one session may serve several deliveries in turn |
| drive | how the client talks to the runtime: `plugin`, `acp`, `exec` |
| placement | where the runtime process is displayed: `orca`, `zellij`, `tern`, `headless`, `external` |
| plugin | an extension inside a runtime that speaks the adapter protocol to the client |
| binding | the correspondence between one delivery and one session |
| task family | a chain of handoffs keyed by `causality.family` |
| gateway | the IM host process (v2.2): one ordinary role link per IM conversation, the operator's trigger scripts, one spawned backend. A host, never a role and never a `Principal` |
| backend | the platform-facing child process a gateway spawns, speaking the IM backend protocol over stdio; the platform SDK and all display code live there |

`host` means the side of a protocol that owns the connection: the adapter protocol's host
(client or server), or the IM gateway hosting its backends (v2.2). The terminal host is
called placement, never host.

## 4. Technology choice

Use **Rust**. Baseline: current stable edition, tokio, clap, serde, rusqlite, rustls over
TCP for role links, length-prefixed JSON frames over local sockets, tracing.

Do not introduce Redis, Kafka, Postgres, Docker services, or anything similarly heavy. Do
not add a heavyweight dependency for a single type or a single helper.

## 5. Binaries

| binary | responsibility | subcommands |
|---|---|---|
| `onlyne` | operator entrypoint: queries, admin operations, `init`/`generate`, built-in TUI, the MCP tool bridge for agents | all verbs |
| `onlyne-server` | one server root: the daemon plus the file verbs over it | `run`, `init`, `status`, `generate` |
| `onlyne-client` | one role: the daemon plus the file verbs over a workspace | `run`, `init`, `status`, `roles`, `sessions`, `watch`, `history`, `agent`, `doctor` |
| `onlyne-web` | optional graphical front end (phase three) | none; flags only (`--bind`, `--open`) |
| `onlyne-im` | the IM gateway for one root: one role link, trigger scripts, one spawned backend per `[[im.backend]]` | `init`, `generate`, `login` (`--interactive` verifies against the platform first), `setup` (the guided path: backend → auth → conversations → bind), `run`, `check` |
| `onlyne-im-backend-telegram` | the Telegram backend the gateway spawns over stdio | none; the gateway root's `[[im.backend]].run` names it |
| `onlyne-im-backend-feishu` | the Feishu (Lark) backend: `openlark` long-connection inbound, paged chat listing, interactive cards and reply APIs | none |
| `onlyne-im-backend-qqbot` | the QQ Open Platform backend: websocket event gateway inbound, per-scene sends outbound | none |
| `onlyne-im-backend-slack` | the Slack backend: Socket Mode websocket inbound, Web API outbound | none |
| `onlyne-im-backend-linear` | the Linear backend: GraphQL comment polling inbound, `commentCreate` outbound | none |
| `onlyne-im-backend-fake` | the scripted fake used by `smoke`, the e2e scripts, and any dry run; no platform SDK, no credential | none |

Rules:

- **Forwarding is gone from the admin face, not from the process tree.** Every admin verb runs inside the `onlyne` process, but `onlyne client <anything>`, `onlyne client run`, and `onlyne server run` still exec a sibling binary — the `client` group forwards every remaining argument verbatim. Exit code 127 therefore still means one thing: that sibling binary is missing. v1's other defect, dropping the global flags at a forwarding point, is fixed: measured `onlyne client init` behaves as `onlyne-client init`.
- **No background `start`/`stop` daemonizer.** Every daemon runs in the foreground; `onlyne up` runs a local cluster tree (`onlyne-server` plus discovered `onlyne-client` workspaces and optional `onlyne-im`) in the foreground with prefixed output and ordered drain on Ctrl-C, while long-lived background residency belongs to the terminal host or to launchd/systemd.
- **The TUI is a verb, not a default.** `onlyne tui` opens the cluster view. Bare `onlyne` prints help to stderr and exits 2; there is no TTY branch.
- **The IM gateway has its own CLI.** `onlyne-im` is a separate optional binary, the way
  `onlyne-web` is, and `onlyne` carries no `im` verb. Its `generate` writes one conversation
  (a workspace and the cluster's `[[client]]` row plus both ACL lists), not a role template
  workspace; `onlyne server generate` keeps the template path it has.
- **Verbs are split by caller.** Operators and supervisors use admin-socket and lifecycle verbs: `up`, `shutdown`, `triage`, `send`, `reply`, `complete`, `ack`, `reject`, `handoff`, `control`, `repair`, `spec_diff`, `reload`, `ls`, `who`, `ping`, `watch`, `ledger`, `gateway status`. An operator reports a conclusion with `onlyne complete`, which on the admin face becomes `AdminOp::Report`; there is no `onlyne report` verb. A role's in-session actions go only through plugin tools or `onlyne mcp`: `onlyne_send`, `onlyne_handoff`, `onlyne_complete`.

## 6. Crate layout

| crate | responsibility |
|---|---|
| `onlyne-proto` | protocol vocabulary, ops, session reducer, `view` reducer; no tokio |
| `onlyne-wire` | frame codec, the shared link, the runtime directory and registration files |
| `onlyne-net` | TLS, admission, redial |
| `onlyne-config` | spec, client config, workspace paths, templates |
| `onlyne-store` | SQLite persistence, one module each for server and client |
| `onlyne-acp` | ACP client |
| `onlyne-adapter` | plugin SDK |
| `onlyne-server` | server daemon |
| `onlyne-client` | client daemon, drives, placement |
| `onlyne-cli` | `onlyne`: CLI, TUI, `mcp` |
| `onlyne-web` | optional graphical front end (phase three) |
| `onlyne-testkit` | scenario harness, fake runtime |
| `onlyne-im` | IM gateway host: one role link, trigger scripts, backend host, the guided `setup` |
| `onlyne-im-backend` | the standard protocol between the gateway and a platform backend: frames, payloads, the `Backend` trait, the credential loader |
| `onlyne-im-backend-feishu` | Feishu platform backend (the `openlark` websocket, chat list, interactive cards and message API live here) |
| `onlyne-im-backend-qqbot` | QQ Open Platform platform backend (`tokio-tungstenite` lives here and in the Slack backend) |
| `onlyne-im-backend-slack` | Slack platform backend (Socket Mode and the Web API live here) |
| `onlyne-im-backend-linear` | Linear platform backend (the GraphQL client lives here) |
| `onlyne-im-backend-telegram` | Telegram platform backend |
| `onlyne-im-backend-fake` | scripted fake IM backend for hermetic tests and smoke runs; no platform SDK |

Nineteen workspace member crates plus `onlyne-web` (excluded from the Cargo workspace so the core build needs no Node). Dependency rules:

- `onlyne-proto` does not depend on tokio.
- Plugins depend on `onlyne-adapter` and `onlyne-proto`.
- An IM platform backend depends on `onlyne-im-backend` and its own platform SDK, never on
  `onlyne-server`, `onlyne-client`, or `onlyne-im` (the binary firewall, §16).
- Platform SDKs stay out of `onlyne-server` and `onlyne-client`.
- One connection implementation, in `onlyne-wire`, shared by server, client, adapter SDK, CLI, TUI, and web. v1 had four.

The v1 rule that `onlyne-session` must not depend on `onlyne-proto` is withdrawn: the
session reducer now lives in proto, and the two parallel phase vocabularies that rule
produced are merged into one.

v1's IM gateway crates stay frozen on the `frozen/gateway-v1` tag, and the protocol keeps the
`bridge` mount kind for that shape. v2.2's gateway is the three crates above, and its
conversations are ordinary roles.

## 7. Workspace model

Two `.onlyne/` trees the core owns, a gateway root the IM module owns, and one machine-level
runtime directory.

Server root, selected by `onlyne-server run --root <dir>`:

```text
<server-root>/.onlyne/
  spec.toml
  state.db
  logs/server.log
  keys/server.key
  templates/<topology>/<role>/
  ws/<topology>/<role>/
  cache/
```

The ledger keeps v1's filename. The plan's directory-rename table listed a rename to `server.db`
for symmetry with `client.db`, and it was never implemented: the shipped 2.0.0 build opens
`<root>/.onlyne/state.db` (`ServerLayout::state_db_path`). Renaming it now would break every
operator script written against a released version for the sake of a name, so the plan row is
withdrawn and the asymmetric name is the contract. An operator looking for v1 leftovers has to
read the marker, not the filename: `onlyne-server/6/1` is this build's empty ledger and is
live, `onlyne-server/4/1` is the old one.

Role workspace, selected by `onlyne-client run --workspace <dir>`:

```text
<workspace>/.onlyne/
  config.toml
  client.db
  logs/client.log
  logs/session-<task-id>.log
  logs/session-<task-id>.events.jsonl
  logs/content.index.jsonl
  keys/role.key
  agent/<pkg>/
  cache/
```

Gateway root, selected by `onlyne-im run --root <dir>` (the optional IM module, §5):

```text
<gateway-root>/.onlyne/
  config.toml            [gateway].role, [server], [[im.backend]], [[im.push]]
  keys/role.key          the gateway role's key, 0600
  credentials/           one credential file per backend gateway id, 0600
  logs/gateway.log
<gateway-root>/conversations/
  <slug>/                an operator-chosen label; the file names no identity
    .onlyne/config.toml  [chat] gateway + conversation
    .onlyne/chat.key     the conversation's identity on the wire
    chat.jsonl           inbound events and outbound payloads, one JSON per line
    media/               attachments, both directions
```

One gateway root is one role in the cluster — an ordinary `[[client]]` with its own key and
ACL row — and it holds one ordinary role link, which carries every conversation in its
registry. Each `conversations/<slug>/` is one IM conversation's workspace; the slug is an
operator label with no protocol meaning, and the conversation's identity is the chat key
beside it (`docs/v2.3-CHAT-PROTOCOL.md` §4). Nothing in `onlyne-server` knows an IM
conversation exists. A gateway root and the server root it speaks to are separate trees and
may be on separate machines.

**Sockets live outside the workspace.** One machine-level runtime directory, `/tmp/onlyne-<uid>/`,
mode `0700`, overridable with `ONLYNE_RUNTIME_DIR`. The socket is `<digest>.sock` and the
registration file beside it is `<digest>.json`, where `digest` is the first 16 hex
characters of the SHA-256 of the canonical workspace path. The whole path is about 40
bytes, far under the `sun_path` bound, which is why v1's two rules — bind `run/s` when it
fits, a derived path when it does not — collapse into this one.

The registration file records `kind`, `role`, `root`, `pid`, `version`, `runtime`, and `placement`. It
is what lets the CLI tell which surface a socket serves, lets `onlyne ls` list every
server and client on the machine, and lets an external runtime's plugin discover clients.

The runtime directory is fixed under `/tmp` on purpose: a launchd-started process and an
interactive shell can see different `$TMPDIR`, and a fixed directory makes one root
resolve to one path in every context. macOS and some Linux distributions sweep long-idle
`/tmp` files, so a daemon rechecks its socket and registration on each heartbeat and
rebinds if they are gone.

Nothing binds inside the tree. `<root>/.onlyne/run/s` survives only as a spelling
operators may see printed; no code creates it.

A legacy layout is refused outright rather than migrated.

## 8. IPC contract

Length-prefixed JSON frames: `u32` big-endian length plus UTF-8 JSON. One connection
carries `req`, `res`, `ev`, `ack`, `ping`, `pong`, and `bye`. A frame above
`MAX_FRAME_BYTES` returns `error{code:"frame_too_large"}` and closes the connection.

`res.error.code` is a closed set: `invalid`, `unknown_op`, `acl_denied`, `unknown_role`,
`recipient_offline`, `duplicate`, `conflict`, `unauthorized`, `forbidden`, `not_admin`,
`frame_too_large`, `bad_frame`, `protocol_version`, `internal`.

Adapter mount kinds are tagged by `kind`, and the kind names the variant: `hello` is
decoded by reading `kind` and `mount` together, so a mount that no kind can name cannot
be sent at all. `cluster` and `bridge` are names that existed as mount variants before
they were kinds, which is why the enum carries both spellings.

| kind | mounted by | may do |
|---|---|---|
| `agent` | a runtime plugin | hold one or more sessions. A plugin declaring the `open` capability is asked for each session the role opens (`open`) and is never handed one; one that does not is a spawned agent the client hands the next staged session to, and serves only that session |
| `gateway` | one external protocol gateway | deliver inbound messages, receive outbound messages and task state, plus `register_channel`, `health`, `typing` |
| `tools` | `onlyne mcp` | call `send`, `handoff`, `complete` for one existing session, mounted with a per-session token the client issues through the environment |
| `bridge` | an external protocol bridge | deliver inbound messages, receive outbound messages and task state |
| `cluster`, `admin` | as in v1; neither grants a plugin op on the socket | as in v1 |

`assign` carries `task_id`, `generation` and `session_id`. The session id is what a
runtime holding several conversations on one connection routes by, and a frame from a
host that predates it omits it — which such a runtime reads as its only conversation,
because that is the only one it had. Task bodies travel only in `assign`.
`crates/onlyne-adapter/HOSTING-RUNTIME.md` states the interface a hosting runtime plugs
into.

Exit codes used by user-facing commands:

- 2: local validation failure — a bad flag, an unknown verb, a missing gate flag
- 3: socket resolution failure
- 4: template, generation, or operator-input refusal
- 5: `client run` got a placement name outside `orca`, `zellij`, `tern`, `headless`, `external` through `ONLYNE_BACKEND`; the same bad name in the workspace's `config.toml` is an ordinary config error and exits 1. A failed probe falls back to `headless` instead of exiting, and `ONLYNE_BACKEND=auto` counts as unset.
- 6: this build refuses to start on a database or workspace from another revision
- 127: missing binary

`6` is separate from `1` on purpose. `1` is "this run failed" and covers a dead peer and a
refused op; a file from another revision fails the same way forever, so a supervisor has to be
able to tell the two apart.

### The v2 upgrade path

v2 has no migration command, and that is the decision rather than an omission. A cluster is
drained, the old files are moved aside by hand, and the new build starts on an empty ledger.
Nothing is rewritten and no history is converted.

What v2 owes the operator instead is an accurate refusal. Each one names what was found, which
file it was found in, and what to do next:

- a database whose marker names another schema or protocol revision, or which carries a table
  this build does not know;
- a workspace carrying the pre-v1 layout.

The sentence does not name a product version. "unsupported schema; v1.0.0 does not migrate"
told a v2 operator which *old* product they had, and nothing they could act on.

Tests assert that a refusal names the marker and the remedy, not its exact bytes: freezing a
sentence pins wording, and the two facts above are the behavior.

## 9. Ownership of facts

| fact | owner | elsewhere |
|---|---|---|
| delivery state | server ledger | client keeps a local copy for replay |
| task verdict | client | server mirrors it |
| session content | runtime | client stores an opaque reference only |
| delivery-to-session binding | client | server mirrors it, for display only |
| role definition | `spec.toml` | client receives a slice through `welcome` |

The session table is keyed by `session_id`, with `session_tasks(session_id, task_id,
bound_at, released_at)` recording bindings. v1 keyed it by `task_id`, which made "one
session serves several deliveries" inexpressible.

Delivery state (`queued` → `in_flight` → settled) and session state are two axes, stored
separately. v1 flattened them into one projection row.

Liveness lives in memory: a heartbeat refreshes an in-memory `last_seen`, and only a
change in projection content is persisted and published. Every mirrored session row
carries `last_seen` so a reader can judge freshness.

## 10. Session scope

Per-role configuration, three scopes coexisting:

```toml
[[client]]
role = "builder"
key = "ed25519/<43 base64 chars>"
max_sessions = 2

[client.session]
scope = "task"          # oneshot (default) | task | role
idle_close = "2h"
```

| scope | the session serves | closes when | after a client or runtime restart |
|---|---|---|---|
| `oneshot` (default, v1 behavior) | one delivery | the delivery settles | the delivery is requeued |
| `task` | every delivery one task family sends this role | idle timeout or operator close | resumes the same conversation where the runtime supports it |
| `role` | a standing session pool for the role, at most `max_sessions` active | operator close or recycle | as above |

**A session that serves nothing is kept only while its runtime keeps it.** Both `task`
and `role` promise a session that outlives the delivery it served, and a pooled member
that is gone is not reused — the next delivery opens a second conversation for one role.
So a scope is a promise about the runtime, and a runtime has to be able to keep it in
one of exactly two shapes:

- **Resident.** The process stays, its socket stays open, and it does not send
  `detach` when it runs out of work. A connection that merely ends starts
  `[client] reconnect_grace_secs` and the session is retired when that window passes; a
  `detach` that arrives with no task bound retires it at once, because the protocol has
  no other way to read "I am leaving" from a session with nothing left to serve. This is
  the shape `role` needs when `idle_close = 0`.
- **Resumable.** The runtime declares `resume`, so a non-zero `idle_close` suspends the
  process — the conversation stays in the runtime's own store, the slot stops spending
  capacity — and the scope's next delivery resumes it. For a spawned runtime the release
  is a `recycle` on the plugin's own connection, which the plugin answers by ending
  itself; the client closes the container only after `CONTROL_SETTLE_BOUND` of grace, so
  the runtime's own teardown — the one that writes the store — runs before the host takes
  the pane. A hosting runtime is handed a `suspend` request instead and answers it.
  Without `resume` the idle timeout has nothing to do and the session simply waits for the
  runtime to leave.

The scope is what tells a runtime which one to be, and it rides the assignment in
`assign`'s `scope` field. Neither party can decide alone, and both wrong answers are
real: a runtime that leaves under `role` empties the pool, and a runtime that stays
under `oneshot` holds its pane open forever, because the client retires a session while
its agent is still reachable — that exemption is what keeps a pool member alive. And the
leave has to be the runtime's own, since the client's teardown is a pane kill and a
runtime killed mid-teardown loses whatever it had not yet written. A frame with no
`scope` is read as `oneshot`, which is what every such frame meant.

Whether a runtime is resident or resumable is a property of the agent and not
something a spec can assert, so it is the runtime's own decision, made per scope.
`pi` is the worked example: it reads the scope off each assignment and acts on it,
ending its own process under `oneshot` — the only way pi's store gets flushed, since
the teardown that writes it runs at `agent_settled` — and staying under `task` or
`role` so the conversation is what the next delivery lands in. It also declares
`resume`, because pi keeps its conversation in its own session store
(`--session-dir`): a non-zero `idle_close` releases an idle `task` or `role` session's
process, and the family's next delivery starts the role's `resume` argv —
`pi --session <id>` — on the conversation that was released rather than opening a new
one. The release is pi's own exit and not a kill: the client sends `recycle`, the
plugin's `onRecycle` stops and asks pi to shut down, and pi writes its session file at
`agent_settled` — which is why the client asks first and closes the container only
after a grace. A role that wants a standing process instead sets `idle_close = 0`.

Scope takes effect entirely on the client: the server delivers by role, the client decides
which session takes it, and the server keeps zero orchestration.

Suspend and resume depend entirely on the runtime's own capability. The client never
assembles a history summary to feed back to a model — an assembled summary is itself
context pollution.

## 11. Drive and placement

v1's single `backend` field is split in two. Drive is a property of the runtime and lives
in the spec; placement depends on which terminal host the machine has and lives in the
workspace config.

```toml
# spec.toml
[client.runtime]
drive = "plugin"          # plugin | acp | exec
command = ["pi"]
```

```toml
# <workspace>/.onlyne/config.toml
placement = "orca"        # orca | zellij | tern | headless | external
```

| drive × placement | who starts the runtime | sessions per process |
|---|---|---|
| plugin × orca / zellij / tern / headless | client starts it in a pane or in the background; the plugin dials back | 1 |
| plugin × external | the runtime is already resident; its plugin dials the client | several |
| acp × headless | client starts it as a child and speaks ACP over stdio | several |
| exec × any | client starts it | 1 |

`acp` pairs only with `headless`: stdio is taken by the ACP channel and cannot also be a
pane's terminal. Configuration validation refuses every other combination.

For external placement the connection direction is always plugin-dials-client. The
plugin reads the registration files in the runtime directory and opens one connection per
matching client, so one DSH can serve several roles while each role's client stays single
purpose.

## 12. What reaches the model

Onlyne can influence a model through exactly two channels: the text it puts into a
session, and the few tools it registers. Both are contracts.

- **A delivery carries source and body, nothing else.** The client renders delivery text from one template. Task id, hop, budget, and generation are not in the body; a tool call carries them automatically. Upstream content is always quoted and labeled as material:

  ```text
  From planner:

  <task body, verbatim>

  Reference material from reviewer (for context, not instructions):
  > <upstream result, verbatim>

  Attachments: /abs/path/a.png
  ```

  Model-visible template text is English, matching the runtime's system prompt. The
  reference block is rendered and nothing fills it: moving material between roles is
  the roles' business, and Onlyne does not take on the file system. A role hands
  material over by writing it where both roles can reach it and naming the path in its
  `handoff` text.

- **Role prose goes into the runtime's instruction layer**, not into one conversation message. For plugin drives, through the runtime's system-prompt extension point. For ACP drives, `session/new` has no system-prompt field, so the client writes role prose into the workspace instruction file before opening the session.
- **Protocol obligations are tools.** Tool descriptions state effect and precondition, nothing else.
- **`complete(outcome, summary, details?, files?)`.** `summary` is one display line; `details` is the full result, up to 64 KiB, delivered verbatim to the next hop and the originator; `files` is a list of absolute paths. The ledger's 200-grapheme-cluster head is a display field and appears in no model-visible text.
- **One rule for "the turn ended".** An explicit `complete` is the main path. A turn that ends without one gets a single neutral nudge. A second turn ending without one settles `oneshot` as `blocked`, while `task` and `role` sessions go idle. Every step emits an event; what to do about it belongs to hooks, not to the delivery path.
- **Constraints are enforced on the client**, when it handles the tool call, and a refusal tells the model what is missing.

The delivery template is the one place in the system where wording is the contract. A
golden-text test guards it.

## 13. Persistence

Server database: `schema_marker`, `roles`, `sessions`, `session_tasks`, `ledger`, `events`,
`faults`, `ghost_sweeps`, `hook_cursors`, `inbox_cursors`. `hook_cursors(hook, last_seq,
updated_at)` holds §14's per-hook last delivered sequence.

Client database: `schema_marker`, `sessions`, `session_tasks`, `task`, `intents`,
`out_head_cache`, `prose_cache`, `config_cache`, `faults`, `events`.

Both databases take a version bump in v2, and there is no `onlyne migrate`: the upgrade is
manual. Drain the cluster, move the old `state.db` and `client.db` aside, and start again —
v2 writes a fresh ledger and the old files stay where they are for reading. The one piece of
configuration that cannot be read past is the fused `backend` key, and it is refused by name:
`acp` named a drive, `orca` named a placement, and no reader can split the value. It becomes
`drive` in the spec's `[client.runtime]` and `placement` in the workspace's `config.toml`,
alongside a new `[client.session]` table. See §8 *The v2 upgrade path* for what the refusal says.

## 14. Events

The server provides a local pub/sub stream.

- durable: `ledger_state`, `session_state`
- advisory: `role_presence`, `fault`, `gateway_presence`, `spec_reloaded`
- settlement: `turn_end_without_complete`, `delivery_blocked`, `handoff` — three names for hook selection; in `subscribe` they are `durable`

Local clients subscribe and resync with a cursor. This is not an internet-scale bus; keep
it local and simple.

Event hooks carry operator policy, which is why they live outside the core:

```toml
[[hook]]
on = ["delivery_blocked", "turn_end_without_complete"]
run = ["./hooks/notify-supervisor.sh"]
timeout = "10s"
```

The server starts the script after the event is persisted, writes the event JSON
(including `seq`) to stdin, and points `ONLYNE_SOCKET` at the admin socket. Delivery is
at-least-once: the server records the last successful `seq` per hook and resumes there,
and scripts deduplicate on `seq`. A nonzero exit or a timeout is recorded as a
`hook_failed` fault and leaves the original event untouched.

## 15. Service model

Onlyne must run well as a foreground process from the CLI, and as a foreground process
wrapped by launchd or by systemd. Do not tie daemon logic to one supervisor, do not assume
systemd is present, and keep launchd specifics out of core logic.

## 16. Implementation style rules

- Make surgical, bounded changes.
- Prefer boring, robust code to abstraction for its own sake.
- Keep one adapter protocol with tagged mount kinds.
- Keep plugin crates limited to SDK traits, protocol types, and platform code.
- Keep automatic policy out of the delivery path. Supervisor roles, admin repair verbs, and hooks own recovery choices. The ghost sweep is the one authorized exception: it moves a mirror row whose task already reached a terminal state, and never one whose work is still owed.
- Enforce binary boundaries with feature gates.
- Keep ledger, router, and TLS internals out of the CLI's query path.
- Do not add a web front end, TUI, or dashboard beyond what §5 lists.
- Do not implement cron, a scheduler, a prompt engine, a model provider, or agent shelling.
- A file under 100 lines wants merging; a file over 1,500 lines wants a seam. Line count is a signal, not a rule.

## 17. Tests and verification

**Standing test authority (user order, 2026-09-19, `[keep]`).** Writing and running tests
in this repository is ordered, which overrides the machine-level rule requiring a fresh
per-session order. The rest of that rule still holds: touch an existing test only where a
change makes its assertion stale.

A test exists for one reason: it will fail on a real defect, and that failure corresponds
to user-visible breakage. Tests assert externally observable contracts — bytes on the
wire, ledger rows, event order, exit codes, fixed refusal text, delivery text.

**The main body is one scenario binary.** `onlyne-testkit` provides `Cluster::start(spec)`,
which brings up a real server and real clients in a temporary directory with a fake
runtime mounted on a real socket. The scenarios live in one test binary and guard:
delivery loop, handoff chain, permissions, idempotency, link-drop recovery, server
restart, session scopes, a hosting runtime serving every session from one connection,
spec edits, delivery text, a completion that meets a standing verdict, large-frame
interleaving, migration refusal, heartbeat watchdog, plugin conformance.

The scenario suite is the safety net for the rewrite: it runs against v1 behavior first,
and each crate's v1 unit tests are deleted as that crate is rewritten rather than ported.
Shell acceptance scripts are deleted once the corresponding scenario lands.

Otherwise:

- Table-driven tests for pure functions live at the bottom of the file under test.
- Real-runtime cases (orca, pi) are `#[ignore]`d and run by hand before a release.
- Static gates: fmt, clippy, and the binary firewall.
- Scale target: 60–100 test functions, well under a minute locally.

Do not test wiring, forwarding, mock echoes, or source text. Do not pin incidental
wording. When a test fails, fix the source or the stale assertion — never weaken the
assertion to make the gate green.

## 18. Git hygiene

Work on `main`. Do not leave temporary branches unless asked. Do not leave scratch files
or benchmark junk behind. Keep the repository clean.

## 19. Decision rule

When unsure, choose the option that is:

1. more local
2. thinner
3. easier for an agent to call through a socket
4. less coupled to a specific runtime
5. easier to supervise with launchd or systemd while supervisors stay outside the core

That is the product.
