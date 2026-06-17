# Onlyne Codex Execution Contract

This repository is for building a **small, Rust-based, workspace-local IM channel daemon / broker**.

Read this file before changing anything.

## 0. Product boundary

Onlyne is **not**:
- a full agent runtime
- a model runner
- a cron / workflow engine
- a web admin product
- a chat UI platform
- a session-planning / memory / prompt-management system

Onlyne **is**:
- a thin local channel layer for agents that have no native messaging tools
- a workspace-local daemon that can be launched by CLI
- a local broker exposing Unix socket / stdio / pipe friendly interfaces
- a multiprotocol adapter layer for QQ / WeChat / Feishu / Telegram
- a lightweight event + history substrate

If you find yourself building anything beyond that boundary, stop and cut scope back.

## 1. Core product requirements

The implementation must satisfy all of the following:

1. **Daemon is not unique**
   - do not design a single global singleton service for the machine
   - each workspace can have its own daemon instance
   - multiple workspaces may run separate daemons simultaneously

2. **Workspace-local model**
   - the current directory is the workspace root
   - all config, state, runtime files, history, pid, logs, sockets live under:
     - `./.onlyne/`
   - running `onlyne` in a directory reads and writes that directory’s `.onlyne`

3. **CLI-first launch model**
   - primary entrypoint is CLI
   - daemon can be launched from CLI in foreground or detached/background-compatible mode
   - design must be compatible with launchd/systemd wrapping, but do not overbuild system integration inside core logic

4. **Local agent-facing interfaces**
   - Unix domain socket is first-class
   - stdio mode is first-class
   - named pipe / raw pipe friendly framing is supported by design
   - local loopback HTTP is optional at most, never the primary control plane

5. **Channel abstraction**
   - must support adapter architecture for:
     - Telegram
     - Feishu/Lark
     - QQ
     - WeChat
   - first implementation may land adapters incrementally, but architecture must not hardcode the first adapter everywhere

6. **History**
   - per-channel history browsing
   - merged all-channel history browsing
   - history should be local, minimal, inspectable, and not require heavy infra

7. **Broadcast/event bus**
   - allow local clients to subscribe to update events
   - allow broadcast of important daemon/channel updates to connected local clients
   - events should include inbound message, outbound message, delivery state, adapter health changes, and permission/update notices

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
- local DB: sqlite via sqlx or rusqlite
- Unix socket IPC: tokio + serde JSON / JSON-RPC or line-delimited JSON
- logging: tracing

Do not introduce unnecessary heavyweight dependencies.

## 3. Architecture constraints

Use a narrow layered architecture.

Suggested high-level modules:

- `src/main.rs`
- `src/cli/`
- `src/app/`
- `src/config/`
- `src/workspace/`
- `src/ipc/`
- `src/core/`
- `src/store/`
- `src/history/`
- `src/events/`
- `src/adapters/telegram/`
- `src/adapters/feishu/`
- `src/adapters/qq/`
- `src/adapters/wechat/`
- `src/util/`

Dependency rule:
- adapters depend on core traits
- IPC depends on core/event types
- store depends on domain models, not adapter-specific SDK types
- core must not depend on any single adapter implementation
- workspace resolution must be reusable by CLI, daemon, and tests

## 4. Reference repo usage rule

Reference repo:
- `/Users/dbydd/vibe-agent-working-dir/git-projects/onlyne_ref_cc_connect`

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

The current working directory defines the active workspace.

Inside each workspace:

`.onlyne/` should contain at least a design-ready layout like:

- `.onlyne/config.toml`
- `.onlyne/state.db`
- `.onlyne/run/onlyne.sock`
- `.onlyne/run/onlyne.pid`
- `.onlyne/logs/daemon.log`
- `.onlyne/history/`
- `.onlyne/cache/`
- `.onlyne/adapters/`

Exact naming can evolve, but the workspace-local invariant cannot be broken.

Never default to global mutable state under `~/.config/onlyne` for active workspace data.
Global config may exist later for convenience, but workspace data stays local.

## 6. IPC contract expectations

Primary local IPC should be one of:
- JSON-RPC 2.0 over Unix socket
or
- line-delimited JSON command/event framing over Unix socket

stdio mode should use the same or nearly the same message schema.

Minimum local operations to support early:
- ping
- status
- list_channels
- list_conversations
- subscribe_events
- unsubscribe_events
- send_message
- reply_message
- fetch_history
- fetch_channel_history
- fetch_all_history
- start_adapter
- stop_adapter
- restart_adapter

Event push model must be explicit.
Clients should be able to subscribe and receive async updates without polling-only design.

## 7. Channel model expectations

Define stable internal abstractions.
At minimum:

- Adapter
- ChannelId
- ConversationId
- MessageId
- MessageEnvelope
- OutboundMessage
- AttachmentRef
- DeliveryState
- AdapterHealth
- Event

Internal message model should preserve enough metadata to support:
- reply threading where platform supports it
- sender identity
- timestamps
- attachments
- raw transport metadata retention when needed

But do not let raw platform payloads leak through every layer.

## 8. Persistence expectations

Keep persistence minimal and robust.

Preferred:
- sqlite for structured state/history indexes
- optional file blobs/logs under `.onlyne/`

Persist at least:
- workspace config
- adapter runtime state needed for restart/reconnect
- conversation index
- message history index/content
- pending outbound jobs if you implement queueing
- event cursor/checkpoint state where platform protocol requires it

Do not introduce Redis, Kafka, Postgres, Docker services, or anything similarly heavy.

## 9. Broadcast and update events

The daemon should provide a local pub/sub style event stream.

Important event classes:
- inbound_message
- outbound_message
- delivery_update
- adapter_started
- adapter_stopped
- adapter_reconnecting
- adapter_failed
- history_appended
- workspace_state_changed
- warning
- error

Broadcast means local connected clients can observe daemon changes.
This is not an internet-scale bus; keep it local and simple.

## 10. Service model

Onlyne must run well in these modes:

1. foreground daemon from CLI
2. background-capable process wrapped by launchd
3. background-capable process wrapped by systemd
4. stdio/pipe mode for agent-spawned subprocess usage

Do not tightly couple daemon logic to one supervisor.
No assumptions that systemd is always present.
No launchd-specific logic in core business code.

## 11. Implementation style rules

- Make surgical, bounded changes
- Prefer boring, robust code over abstraction theatre
- Avoid framework addiction
- Avoid giant generic trait hierarchies unless they clearly reduce complexity
- Do not invent plugin systems prematurely
- Do not add web frontend, TUI, or dashboard unless explicitly asked
- Do not implement cron, scheduler, prompt engine, model provider, or agent shelling features
- Do not overdesign for 20 future platforms before the first working local path exists

## 12. Delivery strategy

Even though the project is small, do not return a half-product.
The first serious implementation pass should aim to land:

- Rust project scaffold
- workspace resolution
- `.onlyne/` bootstrap
- CLI entrypoint
- daemon start path
- Unix socket IPC skeleton
- event bus skeleton
- store skeleton
- one adapter path chosen as first real adapter
- history read/write baseline
- enough tests to validate workspace-local behavior and IPC framing

But when working in this repository, always respect the current task asked by the user. Do not jump ahead if the current ask is only planning or scaffolding.

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
3. easier for an agent to call via socket/stdio
4. less coupled to a specific runtime
5. easier to supervise with launchd/systemd without internalizing those supervisors

That is the product.
