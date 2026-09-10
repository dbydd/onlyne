# Onlyne Codex Execution Contract

This repository is for building a **small, Rust-based, workspace-local agent channel and routing layer**.

Read this file before changing anything.

## 0. Product boundary

Onlyne v1.0.0 is:
- a server that routes envelopes, holds the ledger, projects session state, records faults, and exposes admin operations with zero orchestration policy
- a client that owns one role's session execution inside one workspace
- a gateway that owns chat-platform translation, rendering, auth, and platform-local correlation state
- one adapter protocol mounted on two sides: agent plugins attach to the client, and IM gateway plugins attach to the server-side gateway path
- a local channel layer for agents that need role-addressed messaging

Onlyne v1.0.0 leaves outside:
- workspace file sync
- agent work artifacts
- large media transfer
- model runtime
- prompt management beyond role prose in `spec.toml`
- cron and workflow scheduling
- web admin

If you find yourself building outside that boundary, stop and cut scope back.

## 1. Core product requirements

The implementation must satisfy all of the following:

1. **Process scope**
   - one `onlyne-server` serves one server root
   - one `onlyne-client` serves one role workspace
   - one `onlyne-gateway` process serves one selected platform
   - multiple server roots and role workspaces may run simultaneously

2. **Workspace-local model**
   - the selected server root owns server config, ledger, events, faults, sockets, keys, logs, templates, generated workspaces
   - the selected role workspace owns client config, sessions, intents, keys, socket, logs
   - active data stays under the relevant `.onlyne/` tree

3. **CLI-first launch model**
   - primary entrypoint is CLI
   - server, client, and gateway run in the foreground from CLI
   - launchd/systemd wrappers stay outside core logic

4. **Three socket surfaces**
   - role connections use TCP plus TLS 1.3 with certificate pinning and ed25519 admission
   - agent adapters connect to the client unix socket
   - gateway adapters and admin commands connect to the server unix socket

5. **Gateway abstraction**
   - v1.0.0 ships four feature-gated gateway plugins: Telegram, Feishu/Lark, QQ, WeChat
   - each plugin depends on `onlyne-adapter` and `onlyne-proto`

6. **Ledger and event history**
   - `onlyne history` reads persisted events
   - `onlyne ledger` reads delivery rows
   - `onlyne sessions` reads session projections
   - `onlyne faults` reads recorded faults

7. **Observation stream**
   - local clients subscribe to update events with cursor resync
   - durable classes: `ledger_state`, `session_state`
   - advisory classes: `role_presence`, `fault`, `gateway_presence`, `spec_reloaded`

8. **Agent integration out of scope**
   - do not implement model adapters, prompt orchestration, tool routing, coding-agent lifecycle management, or any runtime-specific coupling
   - Onlyne solves the “agent has no messaging tool” problem only

## 2. Technology choice

Use **Rust**.

Preferred baseline:
- edition: current stable Rust
- async runtime: tokio
- CLI: clap
- config/state serialization: serde
- local DB: sqlite via rusqlite
- role connections: tokio plus rustls over TCP
- local sockets: tokio plus length-prefixed JSON frames
- logging: tracing

Do not introduce unnecessary heavyweight dependencies.

## 3. Architecture constraints

Use a narrow layered architecture.

High-level layout for v1.0.0:

- `crates/onlyne-proto/`
- `crates/onlyne-frame/`
- `crates/onlyne-config/`
- `crates/onlyne-layout/`
- `crates/onlyne-store/`
- `crates/onlyne-session/`
- `crates/onlyne-net/`
- `crates/onlyne-adapter/`
- `crates/onlyne-server/`
- `crates/onlyne-client/`
- `crates/onlyne-gateway/`
- `crates/onlyne-cli/`
- `crates/onlyne-testkit/`
- `plugins/onlyne-gateway-telegram/`
- `plugins/onlyne-gateway-feishu/`
- `plugins/onlyne-gateway-qqbot/`
- `plugins/onlyne-gateway-weixin/`

Dependency rule:
- `onlyne-proto` has no tokio dependency
- `onlyne-session` stays reducer plus backend trait with no store, proto, net dependency
- `onlyne-server` and `onlyne-client` share proto, frame, net, store, config, layout
- plugins depend on `onlyne-adapter` and `onlyne-proto`
- layout resolution stays reusable by CLI, daemons, and tests

## 4. Reference repo usage rule

Reference repo:
- `../onlyne_ref_cc_connect` (sibling directory, outside this repo)

Use cc-connect **only as protocol / adapter behavior reference**.

Do **not** copy its product boundary.
Do **not** import its heavy session/runtime concepts.
Do **not** rebuild its web UI/admin/provider stack.

From the reference study, keep only what matters:
- how each platform authenticates
- how each platform receives inbound events
- how each platform sends outbound messages
- how reconnect/backoff is handled
- how attachment/media constraints are handled
- how session keys / conversation identifiers are derived at transport level

## 5. Workspace model

Onlyne v1.0.0 has two `.onlyne/` trees.

Server root, selected by `onlyne-server run --root <dir>`:

```text
<server-root>/.onlyne/
  spec.toml
  state.db
  run/s
  run/server.pid
  logs/server.log
  keys/server.key
  templates/<topology>/<role>/
  ws/<topology>/<role>/
  cache/
```

Role workspace, selected by `onlyne-client run --workspace <dir>`:

```text
<workspace>/.onlyne/
  config.toml
  client.db
  run/s
  run/client.pid
  logs/client.log
  keys/role.key
  agent/<pkg>/
```

A legacy workspace layout is a hard refusal. If `.onlyne/state.db` contains `io_cursors` or `loopback_idempotency`, or `.onlyne/channels/` exists, the command prints `onlyne: legacy workspace layout; v1.0.0 does not migrate` and exits 2.

Active workspace data stays local to the selected server root or role workspace. Runtime data must never default to global mutable state under `~/.config/onlyne`.

## 6. IPC contract expectations

Onlyne v1.0.0 uses length-prefixed JSON frames: `u32` big-endian length plus UTF-8 JSON. One connection carries `req`, `res`, `ev`, `ack`, `ping`, `pong`, and `bye` frames. Frames above `MAX_FRAME_BYTES` return `error{code:"frame_too_large"}` and close the connection.

Client to server op vocabulary has thirteen closed verbs:
- `hello`
- `send`
- `pull`
- `ack`
- `report`
- `session_sync`
- `subscribe`
- `query_ledger`
- `query_sessions`
- `query_roles`
- `query_faults`
- `control`
- `bye`

Admin op vocabulary has nineteen closed verbs: eight reads plus `reload`, `send`, `control`, seven `repair_*` verbs with suffixes `inspect`, `adopt`, `rebind`, `retry`, `fail`, `close`, `ack`, plus `shutdown`:
- `status`
- `roles`
- `sessions`
- `ledger`
- `faults`
- `watch`
- `history`
- `spec_diff`
- `reload`
- `send`
- `control`
- `repair_inspect`
- `repair_adopt`
- `repair_rebind`
- `repair_retry`
- `repair_fail`
- `repair_close`
- `repair_ack`
- `shutdown`

Gateway to server op vocabulary has five closed verbs:
- `hello`
- `register_channel`
- `deliver`
- `health`
- `bye`

`render_send` travels host to gateway plugin. `typing` stays an optional gateway capability.

Adapter plugin vocabulary uses the same protocol on both mount kinds. Plugin-to-host ops are `hello`, `welcome`, `report`, `session_register`, `assign_ack`, `send`, `deliver`, and `detach`. Host-to-plugin ops are `welcome`, `assign`, `render_send`, `probe`, `recycle`, `config_get`, and `bye`.

`res.error.code` is a closed set:
- `invalid`
- `unknown_op`
- `acl_denied`
- `unknown_role`
- `recipient_offline`
- `duplicate`
- `conflict`
- `unauthorized`
- `forbidden`
- `not_admin`
- `frame_too_large`
- `bad_frame`
- `protocol_version`
- `internal`

Process exit codes used by user-facing commands:
- exit 2: legacy workspace layout, with `onlyne: legacy workspace layout; v1.0.0 does not migrate`
- exit 3: socket resolution failure, with `onlyne: no onlyne socket found; pass --socket, --server-root, or --workspace`
- exit 4: template, generation, or operator input refusal, including `onlyne: refusing to overwrite <path>; pass --force`, `onlyne: template for role <r> is ambiguous: <p1>, <p2>`, `onlyne: no template directory named <r> under <template_root>`, `onlyne: no role matches the requested templates/roles`, `onlyne: generated workspace embeds absolute path <path>`, and `onlyne: agent_package not set in spec.toml [server]`

Spec parse failures print `spec.toml:<line>: <message>`. Schema marker mismatch prints `onlyne: unsupported schema; v1.0.0 does not migrate`. Missing daemon binaries exit 127 with `onlyne: missing binary <path>; run cargo build --workspace`. Old wire format failures use `protocol_version` or `bad_frame`.

Event push model must be explicit. Clients should be able to subscribe and receive async updates with cursor resync.

## 7. Message model expectations

Define stable protocol and projection types.
The public vocabulary lives in `onlyne-proto`:

- Principal
- MsgKind
- Envelope
- Body
- ImagePart
- Causality
- ControlOp
- Outcome
- Frame
- ErrorCode
- Event
- LedgerState
- Lifecycle
- Report
- Receipt
- Welcome
- HandshakeArgs
- AdapterMsg
- Capability
- Mount
- HelloArgs
- HelloAck

Internal message model should preserve enough metadata to support:
- reply threading where platform supports it
- sender identity
- timestamps
- text plus one inline image
- causality with task, parent task, reply target, hop, and attempt
- gateway-local correlation for raw platform payloads

Raw platform payloads stay inside gateway-local storage. Cross-process envelopes carry `Principal::Gateway`, `reply_to`, and causality fields.

## 8. Persistence expectations

Keep persistence minimal and robust.

Server database persists:
- `schema_marker`
- `roles`
- `sessions`
- `ledger`
- `events`
- `faults`
- `inbox_cursors`

Client database persists:
- `schema_marker`
- `sessions`
- `intents`
- `out_head_cache`
- `prose_cache`
- `config_cache`

Persist at least:
- server spec source hash per role
- server ledger state and body JSON retention
- session projection mirror on the server
- client-authoritative lifecycle rows
- outbound intents with `op_id`, attempt, state, next attempt time, receipt, and last error
- event cursor/checkpoint state where protocol requires it

Schema gates expect `('onlyne-server',1,1)` or `('onlyne-client',1,1)`. A mismatch, old table, or `swarm` prefix prints `onlyne: unsupported schema; v1.0.0 does not migrate`.

Do not introduce Redis, Kafka, Postgres, Docker services, or anything similarly heavy.

## 9. Broadcast and update events

The server should provide a local pub/sub style event stream.

- durable: `ledger_state`, `session_state`
- advisory: `role_presence`, `fault`, `gateway_presence`, `spec_reloaded`

Broadcast means local connected clients can observe daemon changes.
This is not an internet-scale bus; keep it local and simple.

## 10. Service model

Onlyne must run well in these modes:

1. foreground server, client, or gateway from CLI
2. background-capable process wrapped by launchd
3. background-capable process wrapped by systemd

Do not tightly couple daemon logic to one supervisor.
No assumptions that systemd is always present.
No launchd-specific logic in core business code.

## 11. Implementation style rules

- Make surgical, bounded changes
- Prefer boring, robust code over abstraction theatre
- Avoid framework addiction
- Avoid giant generic trait hierarchies unless they clearly reduce complexity
- Keep one adapter protocol with two mount kinds: agent on client and gateway on server
- Keep plugin crates limited to SDK traits, protocol types, and platform implementation code
- Keep automatic policy out of the delivery path; supervisor roles and admin repair verbs own recovery choices
- Enforce binary boundaries through feature gates
- Keep platform SDKs out of `onlyne-server` and `onlyne-client`
- Keep ledger, router, and TLS server internals out of `onlyne-gateway`
- Do not add web frontend, TUI, or dashboard unless explicitly asked
- Do not implement cron, scheduler, prompt engine, model provider, or agent shelling features
- Do not overdesign for 20 future platforms before the first working local path exists

## 12. Delivery strategy

v1.0.0 delivery is complete when these are true:

- three daemon binaries ship: `onlyne-server`, `onlyne-client`, and `onlyne-gateway`
- one thin entrypoint ships: `onlyne`
- adapter SDK ships with conformance fixtures and `onlyne-agent-fake`
- four platform gateway plugins ship behind Cargo features: `telegram`, `feishu`, `qqbot`, and `weixin`
- `onlyne server generate` creates relocatable role workspaces from templates
- `onlyne-client init` registers a role by printing a `[[client]]` fragment
- verification case 1 proves single-machine task delivery through fake backend and fake agent
- verification case 2 proves ACL hard refusal
- verification case 3 proves `op_id` idempotency and conflict handling
- verification case 4 proves disconnect and recovery ordering
- verification case 5 proves aggregate-role federation
- verification case 6 proves gateway mount consistency
- verification case 7 proves legacy layout refusal at exit 2
- verification case 8 proves formatting, linting, tests, and binary firewall checks
- verification case 9 proves generate plus relocate

When working in this repository, always respect the current task asked by the user. Avoid jumping ahead when the current ask is planning or scaffolding.

## 13. What to study in cc-connect before coding

Focus review on these files/directories first:

- `cmd/cc-connect/main.go`
- `platform/telegram/telegram.go`
- `platform/feishu/feishu.go`
- `platform/qqbot/qqbot.go`
- `platform/weixin/weixin.go`
- related support files in those adapter directories

Extract from them:
- auth shape
- connection lifecycle
- reconnection behavior
- message send flow
- inbound parse flow
- media/attachment handling limits
- session/conversation key derivation ideas

Ignore most of:
- web UI
- provider system
- cron/timer
- agent lifecycle complexity
- large management surfaces unrelated to channel transport

## 14. Tests and verification expectations

For every meaningful implementation step, verify with real evidence.

At minimum, add tests for:
- workspace root detection
- `.onlyne/` bootstrap behavior
- socket path generation
- config loading in current directory
- local history query behavior
- event subscription lifecycle
- adapter trait conformance where practical

If implementing IPC framing, include regression tests for malformed messages and reconnect cases.

## 15. Git/worktree hygiene

- work on `master`
- do not leave temporary branches unless explicitly requested
- do not leave random scratch files or benchmark junk behind
- keep the repository clean

## 16. Decision rule

Whenever uncertain, choose the option that is:
1. more local
2. thinner
3. easier for an agent to call through a socket
4. less coupled to a specific runtime
5. easier to supervise with launchd/systemd while supervisors stay outside core

That is the product.
