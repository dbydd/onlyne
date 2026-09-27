# pi-onlyne — the onlyne agent adapter for pi

This pi extension makes one pi process serve one onlyne role session. It connects to the
socket that workspace's client serves — `<digest>.sock` in the machine-level runtime
directory (`/tmp/onlyne-<uid>/`, `ONLYNE_RUNTIME_DIR` overriding it), never a path inside
the tree — speaks the adapter protocol in
`crates/onlyne-adapter/PROTOCOL.md`, and drives a session through
`hello → welcome → assign → work → complete → detach`. No Rust code runs here: the
protocol is reimplemented on Node's `node:net`, with a hand-written four-byte
length-prefixed JSON codec, and the runtime has no npm dependencies.

Outside an onlyne session the extension is inert. The client injects `ONLYNE_ROLE`,
`ONLYNE_SESSION_ID` and `ONLYNE_TASK_ID` into every process it spawns
(`crates/onlyne-client/src/session/dispatch.rs`). With any of the three missing, this is an
ordinary pi session: the plugin registers nothing and opens nothing.

```
pi session (spawned by onlyne-client)
  │  env: ONLYNE_ROLE / ONLYNE_SESSION_ID / ONLYNE_TASK_ID
  │  .pi/onlyne.json: { "enabled": true, "watch": { "autoStart": true } }
  ▼
hello{protocol:1, plugin:"pi-onlyne", kind:"agent", capabilities:[…], mount:{role,session,task_id,pid}}
  ◀── welcome{role, prose, generation, server, host_capabilities}
  ├─ prose ──► one `onlyne-role-prose` section of the system prompt, once
  ├─ report.ready ──► the barrier the task payload waits behind
  ◀── assign{envelope, prose, text, attachments, task_id, generation}
  ├─ prose, when the welcome did not already deliver it ──► the same section
  ├─ text ──► pi user message (deliverAs:"followUp"), byte for byte; `body.image`
  │    rides as a pi image part, and `attachments` names files the client wrote
  ├─ assign_ack{accepted:true}
  ├─ report.heartbeat{agent} — `running` per turn and every 10s while a task is live,
  │    `idle` only while pi waits for input; every beat re-derives it from pi
  ├─ the client owns the turn-end rule: a turn that ends without a completion
  │    earns one nudge, and the second such ending settles the delivery
  ◀── nudge{task_id, text} ──► pi user message, byte for byte (the client's sentence)
  ├─ report.complete{outcome, head, details, files} — the ledger's terminal fact, and the last report
  │    └─ the client's answer is the handover: pi is asked to shut down, then detaches
  ├─ probe ──► one heartbeat
  ◀── recycle ──► complete (if unsettled) → stop → pi exits
  └─ detach{reason} when pi shuts down
```

## 1. Install

The plugin is a pi package: `package.json` declares `pi.extensions: ["./src/index.ts"]`,
so pi loads the TypeScript source directly (no build step).

### With a generated workspace (the normal path)

`onlyne server generate` copies `[server].agent_package` into
`<ws>/.onlyne/agent/<pkg-name>/`, and writes that package into `.pi/settings.json` as a
path relative to the settings file itself: `../.onlyne/agent/<pkg-name>`
(`crates/onlyne-server/src/generate.rs`). pi 0.85.1 loads only that spelling. A project
`packages` path resolves against the directory holding the settings file (`<ws>/.pi`), so
the `../` form reaches `<ws>/.onlyne/agent/<pkg-name>`, while a bare
`.onlyne/agent/<pkg-name>` entry would resolve to `<ws>/.pi/.onlyne/agent/<pkg-name>` and
list the package without loading it. A supervisor starts the generated workspace, and the
extension travels with it: nothing is installed globally.

```toml
# spec.toml
[server]
agent_package = "/abs/path/to/plugins/onlyne-agent-pi"   # read once, at generate time
```

```bash
onlyne server generate --root <server-root> --out <dir>
```

The generated `.pi/settings.json` then carries:

```json
{ "packages": ["../.onlyne/agent/onlyne-agent-pi"] }
```

`pi list` shows the entry under "Project packages". To verify the load itself, make the
copied `index.ts` throw and watch for the failure.

### Manual (no generator)

```bash
cp -R plugins/onlyne-agent-pi <ws>/.onlyne/agent/onlyne-agent-pi
printf '{"packages":["../.onlyne/agent/onlyne-agent-pi"]}\n' > <ws>/.pi/settings.json
```

### From npm

```bash
pi install npm:pi-onlyne          # user-level: every pi process on this box loads it
```

The published package is `pi-onlyne` on npm; `pi install npm:pi-onlyne@<version>` pins
one. This route reaches ordinary interactive sessions too, and there the extension
stays inert (no `ONLYNE_ROLE`, so no adapter). A role workspace needs no global
install to get a panel: the file-level copy above, or `onlyne server generate`,
scopes the plugin to the workspace that serves the role.

### One-off / testing

```bash
pi --session-id <id> -e /abs/path/to/plugins/onlyne-agent-pi -ns -nc
```

### The switch file

`<cwd>/.pi/onlyne.json` (see `onlyne.json.example`):

| key | default | effect |
| --- | --- | --- |
| `enabled` | `true` | `false` turns the extension off for this workspace |
| `watch.autoStart` | `true` | `false` registers the tools but opens no socket until `/onlyne connect` |

A missing file means every default. A malformed file prints one warning on stderr and
keeps the defaults: a typo must not silently disable a role. The client does not read
this file (§11 of the plan downgraded the old readiness gates to generate-time template
advice), so only this extension consumes it; the key shape stays the one the templates
carry.

Nothing else is needed. The workspace's `session_command` in `spec.toml` already spawns
`pi` per task (`["pi", "--session-id", "{session}"]`), and the client injects the
environment this extension keys on.

## 2. Capabilities

The `hello` frame declares what this plugin actually implements:

| capability | declared | what it means here |
| --- | --- | --- |
| `register` | always | `session_register{session_id, task_id, generation, pid, title}` after `welcome` |
| `report` | always | `report.ready` / `report.heartbeat` / `report.complete` |
| `inject` | when `pi.sendUserMessage` exists | the delivery arrives as `assign{…, text}` and `text` is injected as a pi user message, unchanged |
| `recycle` | always | `recycle` settles the task if it is unsettled, then stops the plugin and exits pi |

What happens when a pi API is missing, and what the host does then:

| gap | detection | behaviour |
| --- | --- | --- |
| no `registerTool` (older pi) | probed at `session_start` | no tools are registered; the protocol path is unaffected, and `/onlyne status` still works |
| no `sendUserMessage` | probed at `session_start` | `inject` is dropped from the capability list, so the host delivers the task through `config_get{key:"stdin:<delivery text>"}`, which the plugin injects through whatever channel remains |
| no `sections` on `before_agent_start` | guarded at each run | the role prose reaches no instruction layer — the run was offered no prompt sections to write into; one stderr line says so, and the delivery text still arrives |
| no `appendEntry` | probed | no `onlyne-assign` / `onlyne-complete` session entries are recorded |
| no `ui.setStatus` | guarded | the footer status line is skipped |
| no `ui.setWidget` | guarded | routine notices continue through the footer status line and the `[pi-onlyne]` stderr line |
| no `ctx.shutdown` | guarded | `recycle` and a completion still settle the task; the process stays up for the operator to close |

### Activity panel

When the host reports a UI (`ctx.hasUI`, true in the TUI and RPC modes, false in print and JSON modes) and `ctx.ui.setWidget` is available, routine onlyne notices draw in the panel above the editor with widget key `onlyne`. The header shows role, connection state, generation, the current task id, and phase. Below it, up to six newest-first events use `<=` for inbound frames, `=>` for outbound frames, `!!` for warnings, `..` for state changes, and `~~` for duplicate deliveries. Repeated identical events fold into one line with `xN`; the panel holds at most eight lines, each capped at 96 cells, and `session_shutdown` clears it.

## 3. Tools

Registered only inside an onlyne session, with the schemas the MCP face carries
(`docs/v2-CONTRACT.md`, "3b's interface: the `tools` mount"): one obligation vocabulary
that two drives mount.

Each result is one plain sentence — the recipient for a send or a handoff, the outcome
for a completion. A tool result is model-visible, and nothing about the plugin's own
bookkeeping belongs in it.

### `onlyne_send{to, text, kind?, image?}`

Sends one envelope on the `send` frame. `kind: "note"` (the default) is free text and
carries no `op_id`. `kind: "task"` hands work to a role, so it carries an `o-<uuid>`
idempotency key and a fresh `causality.task`. `image` is an absolute path to a
png/jpeg/gif/webp file: the plugin reads it, base64-encodes it and attaches it as
`body.image`. The core caps that at 2 MiB and accepts four mime types. The result is
`sent to <role>`.

### `onlyne_complete{outcome, summary, details?, files?}`

Ends the current task with an explicit outcome — the proto's `Outcome`, so `done`,
`failed`, `cancelled` or `blocked`. `summary` is the display line, and it becomes the
ledger `head` verbatim: whitespace collapses to one line and the text stops at 200
characters. An empty `summary` carries no display line, so the head falls back to the last
assistant text. `details` is the full result and `files` the absolute paths it names; both
ride the `report.complete` frame unchanged and are what the next hop and the originator
receive, with the client holding the ceiling and refusing an oversize body with its own
sentence (§3c of the contract). The call also ends the session's process: once the client
has acknowledged the completion report (§4), the plugin asks pi to shut down through
`ctx.shutdown()`. pi 0.85.1 has no tool-result `terminate` handling. The result is
`reported <outcome>`.

### `onlyne_handoff{to, text, image?}`

Hands this session's task on to the next hop of its family. The plugin sends one `handoff`
frame naming the task the session currently holds, and the host mints one child task for
`to` under it: the child names this task as its `parent_task`, sits one hop further along,
and carries the same family id, hop budget, origin, deadline and labels. A client refusal
comes back as the tool's error, verbatim. `image` is the same absolute png/jpeg/gif/webp
path the send tool takes. The child's id and its hop stay protocol data — the result is
`handed on to <role>`, and the child is proven from the ledger row the host writes, not
from a sentence this plugin printed. The delivery text is the client's own rendering
(sender, body, attachment paths) and this plugin injects it as it stands.
`onlyne_send{kind: "task"}` is the other way to reach a role: that envelope starts a family
of its own at hop 0.

## 4. Outcome rules

`onlyne_complete` is the only path to `done`. The plugin sends one completion per task,
at the first of these events:

1. **`onlyne_complete`** — the model gives an explicit outcome: `done`, `failed`,
   `cancelled` or `blocked`. A later completion for the same task is refused (not
   re-reported). Its non-empty `summary` is the head.
2. **An errored turn** — the turn ended with a provider error (`stopReason: "error"`).
   That is proof on its own, so the plugin reports `failed` at once, with the error as
   the head. The report itself waits until pi is waiting for input, so the beat that
   carries it states a phase the session really has.
3. **`recycle{outcome}`** — the host is tearing the session down. The plugin settles an
   unsettled task with the host's outcome first, then stops and exits pi.

A turn that ends cleanly without a completion settles nothing here. The client owns that
rule (`docs/v2-CONTRACT.md`, "3c. One turn-end rule"): it counts the endings, sends its
own sentence as a `nudge`, and decides what a delivery that never reports becomes. The
plugin hands the sentence to pi and answers only that it did, so the wording, the count
and the settlement have one owner rather than two. A task whose injected message has not
run a turn is left alone whatever a settle signal says: reporting now would claim work
that never happened.

`head` is a single line, capped at 200 characters; it matches what the client puts in
`out_head` and what the receipt carries. Each task has one source for it: the `summary`
of the explicit `onlyne_complete` call when that call carried one, or the error a failed
turn reported. The last assistant text is the fallback for a call whose `summary` is
empty — a sentence spoken after such a call cannot replace what the call handed over, and
nothing else reads it.

A reported completion ends the session's process. `report.complete` goes out as a request,
and the client answers it only after it has settled the session row, acked the delivery
and written the `Completion` envelope. The plugin asks pi to shut down at that answer. An
outcome the socket could not carry is queued and flushed after the next `hello`, and that
flush's answer is the handover that ends the process. A completion the host refused leaves
the process running, so an exit never loses the task.

The completion is the plugin's last report, and a session that has completed one answers
no further beat and no `probe`. The terminal agent state is the client's own write: the
`detach` frame that follows retires the task-free session, and that path feeds
`AgentGone` and publishes the row (`retire_idle_locked`,
`crates/onlyne-client/src/session/dispatch/retire.rs`). A process on its way out states
no phase for itself.

### The idle claim

A session is idle only while it waits for user input. Every other moment reads
`running`: a turn in flight, a queued steering or follow-up message, a retry, a
compaction, and work a background-task extension took off the agent loop.

The plugin asks pi instead of assuming. `ctx.isIdle()` answers whether a run, a
compaction or a queued continuation is still open, `ctx.hasPendingMessages()` answers
whether input is already on its way, and a probe that is missing or throws reads as
`running`. `agent_settled` is where the claim normally lands, because pi fires that
event only after a run has fully settled; the end of one turn is a different moment, and
the 10-second beat re-derives the phase from pi on every tick, so a stale one cannot
survive a run that started again.

A background-task extension changes the question. `bg_run` and its siblings return at
once and the work continues in a child process, so pi waits for input while the session's
task is still in flight. The plugin recognises the extension by the tools it registered
and asks its EventBus service for the live task list; a task in `running` status holds
the session at `running`, and the one report the plugin owes at a turn end — the failure
it witnessed itself — waits that out. On a session without the extension there is no tool
to recognise, no query, and nothing to wait for.

## 5. Protocol notes and deviations

Each item below is either a deliberate reading of `PROTOCOL.md` or a behaviour measured on
the shipped client.

- **Report sequence base.** The plugin's own `report` sequence starts at 1000, not 1. The
  client stamps its own dispatch events (`created`, resource attach, `ready`) into the
  same `(generation, seq)` watermark, and the reducer silently drops any report at or
  below it (`crates/onlyne-client/src/reconcile/`). A plugin sequence starting at 1
  would lose its first observations. **One plugin, one counter:** every task the session
  holds beats off the same sequence, because the client takes `row.seq + 1` for its own
  event on a row between two of that task's beats, and a counter that advanced one per
  task per round would land on exactly that number. The gate is per task row all the
  same, so each task record also carries the `seq` of its last report (`task.lastSeq`,
  visible as `taskSeqs` in `/onlyne status`), and an allocation is clamped above it:
  `A@1001, B@1002, A@1003` is the shape that works, and no task can be handed back a seq
  its own row has already accepted. Rounds never overlap either — a beat asked for while
  one is writing folds into it and buys one more pass, not a second snapshot of the same
  tick. Everything else about the versioning is per spec.
- **`observed` is a full `Observation`.** `report.heartbeat` carries the state
  tuple (`version`, `generation_live`, `isolate_after`, `terminate_after`,
  `mismatch_count`, `agent`, `delivery`, `resource`, `recovery`), not
  a `{"state": "running"}` shorthand: the host deserialises it, overwrites the six
  keys the client owns, and applies only a tuple `is_legal` accepts. This plugin owns the `agent` dimension (turn hooks), the
  `resource` claim — its process is live in the pane the attach was recorded on —
  and the `host` binding. It has no witness for `delivery`, `recovery`,
  `generation_live`, `isolate_after`, `terminate_after` or `mismatch_count`: the
  client rewrites all six from its own intent queue, reducer history and role
  config before the tuple is applied, so whatever this plugin sends there is
  never read. Neither a
  task outcome nor a public view travels in a tuple at all.
- **`ready` is reported once per connection.** The host's own hand-off path
  (`crates/onlyne-client/src/session/dispatch/delivery.rs::hand_session`) already reports `ready` when the
  client stages the session for a mounting plugin, so a second report from the plugin is a
  no-op at the host. The plugin sends it anyway: a plugin that mounts *before* any work
  exists is the case the ready barrier names, and it costs one frame.
- **`cluster_ref` is never sent.** This plugin speaks for a local role, never for an
  aggregate; the field is `skip_serializing_if` absent on the Rust side for the same
  reason.
- **`probe` is answered with a heartbeat**, per `PROTOCOL.md`'s "a `probe` declares fresh
  resource observations".
- **`config_get` is read as a delivery text only when it starts with `stdin:`**, which is
  the overload `PROTOCOL.md` documents for plugins without `inject`: the key carries the
  same rendered bytes an `assign` puts in `text`, and they are injected as they stand.
  Any other key is logged and ignored, never misread.
- **`frame_too_large` / `bad_frame`**: an oversize body is refused before any byte is
  written, and a framing fault closes the connection and reconnects. Framing cannot
  resynchronise after a corrupt body, which is the same conclusion
  `crates/onlyne-wire/src/frame.rs` reaches.
- **Deliveries are idempotent; tasks are not.** The dedup key is the envelope id. The
  same delivery twice gets one injection and an ack with `reason: "duplicate"`, and a
  new envelope for a task that is already running reaches that session as another
  message. The client mints a fresh uuid per envelope, so `duplicate` fires on a genuine
  re-offer and on nothing else.

- **Pane binding (Orca tabs).** Inside an Orca pane the plugin reports the pane it runs in on every
  heartbeat, as `observed.host.orca.pane_key` in the report's `Observation`
  (`crates/onlyne-client/src/host.rs`), beside `tab_id` / `leaf_id` and the terminal `handle` when
  the environment names them. The binding is *inherited*, never guessed: an Orca pane exports
  `ORCA_PANE_KEY` / `ORCA_TAB_ID` / `ORCA_LEAF_ID` / `ORCA_TERMINAL_HANDLE` into the command it
  starts (measured 2026-09-11, Orca 1.4.198), and the client passes its own environment on to the
  session command. So the process inside a pane is the only component that can state, from the
  inside, which pane an onlyne session is; nothing downstream of pi can recover that. Outside a
  pane the `host` key is absent altogether: a pi on a plain terminal reports an observation with no
  host field, rather than one with an empty pane.
- **Nothing is written to the workspace for this.** There is no claim file any more: the binding
  rides the observation the client already mirrors. A stale one cannot exist, because nothing
  creates one, and the workspace's cache directory is not touched. That is what lets
  `integrations/orca-plugin` scope its tab axis to real sessions without reading any path, and what
  lets a supervisor still say where a *finished* session ran: `report.complete` carries `host`
  forward.

## 6. Configuration reference

| env var | required | effect |
| --- | --- | --- |
| `ONLYNE_ROLE` | yes | the mount role |
| `ONLYNE_SESSION_ID` | yes | mounted session id; `session_id` equals `task_id` in the shipped client |
| `ONLYNE_TASK_ID` | yes | the task this process serves; drives `session_register` and the initial `ready` |
| `ONLYNE_SOCKET` | no | the socket the client serves for this workspace, injected into every session process it spawns; with the variable unset the plugin finds that client itself, by reading the runtime directory's registration files (`<digest>.json`) for the one whose `root` is this workspace |
| `ORCA_PANE_KEY` | no | where this process runs (`<tab_id>:<leaf_id>`), reported on every heartbeat as `observed.host.orca.pane_key`; unset outside an Orca pane, which is why the field is then absent |
| `ORCA_TAB_ID` / `ORCA_LEAF_ID` | no | the pane ids separately; the pane key is parsed when only the key itself is set |
| `ORCA_TERMINAL_HANDLE` | no | the terminal handle, reported beside the pane key as `host.orca.handle`, and the value `orca terminal switch` takes |

Constants worth knowing: the plugin heartbeats every 10 s (`heartbeat_timeout_ms` is 30 s),
allows 5 s for `hello` and 30 s per request, and reconnects on a 1/2/4/8/16/30 s ladder.

The plugin reads one file of its own: `<cwd>/.pi/onlyne.json` (the switch, §1). A second
read belongs to the machine rather than the tree: when `ONLYNE_SOCKET` is unset the plugin
lists the machine-level runtime directory for the client registrations that name this
workspace (§7).

## 7. Troubleshooting

| symptom | cause | check |
| --- | --- | --- |
| `[pi-onlyne] session …` never appears | one of the three env vars is missing, or `enabled` is false | `env \| grep ONLYNE_`; `cat .pi/onlyne.json` |
| `socket unresolved: onlyne: no client is registered for <workspace> …` | no `onlyne-client run` for this workspace, so the runtime directory holds no registration whose `root` is this tree | start the client, or `onlyne-client status`; the message names the runtime directory and every registration it did find |
| `socket unresolved: … N clients there name runtime pi … ambiguous` | more than one registered client runs pi sessions and none of their roots contains this workspace, so there is no single client to dial | name the socket explicitly with `ONLYNE_SOCKET`, or start the client for this workspace |
| `socket error: connect ENOENT <path>` | the path in the message is not bound: the client that injected it stopped | `onlyne-client status` for the socket it is serving, and the client log line carrying `socket = <path>` |
| `reconnecting in 4000ms` in a loop | the client is down or the socket was replaced | `onlyne --server-root … roles` |
| `ready refused: internal: unknown session for …` | the plugin mounted and reported for a task the client never staged (normal when pi is started by hand outside a task) | start pi under the client, not by hand |
| `assign` never arrives | the client's `session_command` did not spawn pi, or `inject` was dropped | the client log for the spawn line; `/onlyne status` for the capability set |
| ledger stays `in_flight` | no completion yet: no turn has run (the injected message has not executed), or the turn ended without one and the client has not settled the delivery. On pi-onlyne 1.2.1 against pi 0.87 an envelope carrying an image left nothing injected at all: pi read the flat `ImageContent` this plugin now sends, refused the nested part the older plugin built, and took the task text down with it | the pi session file for the `onlyne-assign` entry and any `nudge` sentence injected after it; `/onlyne status` for the task and phase; for that refusal, the pane's `Extension "<runtime>" error` line and, on 1.2.2, the plugin's `attachment carried no base64 data or no media type` log |
| `hello … forbidden` / connection closed right after `hello` | the mount role does not match the client's role | `hello.args.mount.role` vs the workspace's role |
| `frame_too_large` | a body above 8 MiB | only reachable through an oversize outbound image; the ceiling is the core's |
| tools missing | `pi.registerTool` is absent in that pi version | `/onlyne status`; the capability table above |
| session reads `idle` while a background task still runs | the background-task extension is absent, or its EventBus service did not answer the status query in time, so the plugin cannot see the work it left running | the `[pi-onlyne]` log line for the background probe; `bg_status` in that same pi session names the live task |
| the supervisor board lists no tabs | no live session reported a pane: the adapter predates the report, or this pi is not inside an Orca pane | `onlyne --server-root … sessions --json` for `projection.observed.host.orca.pane_key`; `env \| grep ORCA_` inside the pane |

`/onlyne status` prints the live state (`connected`, `socket`, `role`, `sessionId`,
`generation`, `agentState`, `seq`, `taskSeqs`, `tasks`, `pendingCompletions`, `lastError`,
counters), and `/onlyne connect` / `/onlyne disconnect` open and close the socket by hand.

## 8. Development

```bash
cd plugins/onlyne-agent-pi
node --test src/*.test.mjs        # framing, protocol, agent state machine, config, socket path
```

`src/agent.live.test.mjs` skips itself unless `target/debug/onlyne-client` and
`onlyne-server` exist. `crates/onlyne-testkit/e2e/pi-live.sh` is the end-to-end case: it
skips (exit 0) when pi is absent or has no working model credentials, and otherwise runs
one real task through a real client to `acked`.

```bash
cd ../..
ONLYNE_BACKEND=fake BIN_DIR=target/debug bash crates/onlyne-testkit/e2e/pi-live.sh
```

After sourcing the shared helpers, the case exports `ONLYNE_BACKEND=exec`, so the client
spawns pi itself with a stdin pipe it keeps open for the life of the session. The
agent's own output lands in `<ws>/.onlyne/logs/session-<task>.log`.

# 中文说明 / Chinese Translation

## pi-onlyne — onlyne 的 pi 代理适配器

此 pi 扩展让一个 pi 进程承载一个 onlyne 角色会话。它连接到该工作区的 client 所服务的 socket——机器级运行目录里的 `<digest>.sock`（`/tmp/onlyne-<uid>/`，`ONLYNE_RUNTIME_DIR` 可覆盖），而不是树内的任何路径——使用 `crates/onlyne-adapter/PROTOCOL.md` 中的适配器协议，并按照 `hello → welcome → assign → work → complete → detach` 驱动会话。此处不运行 Rust 代码：协议基于 Node 的 `node:net` 重新实现，使用手写的四字节长度前缀 JSON 编解码器，运行时没有 npm 依赖。

在一个 onlyne 会话之外，扩展不会执行任何操作。客户端会向其启动的每个进程注入 `ONLYNE_ROLE`、`ONLYNE_SESSION_ID` 和 `ONLYNE_TASK_ID`（`crates/onlyne-client/src/session/dispatch.rs`）。任一变量缺失时，这就是一个普通的 pi 会话：插件不注册任何内容，也不打开任何内容。

```
pi session (spawned by onlyne-client)
  │  env: ONLYNE_ROLE / ONLYNE_SESSION_ID / ONLYNE_TASK_ID
  │  .pi/onlyne.json: { "enabled": true, "watch": { "autoStart": true } }
  ▼
hello{protocol:1, plugin:"pi-onlyne", kind:"agent", capabilities:[…], mount:{role,session,task_id,pid}}
  ◀── welcome{role, prose, generation, server, host_capabilities}
  ├─ prose ──► one `onlyne-role-prose` section of the system prompt, once
  ├─ report.ready ──► the barrier the task payload waits behind
  ◀── assign{envelope, prose, text, attachments, task_id, generation}
  ├─ prose（仅当 welcome 尚未投递过） ──► 同一个系统提示 section
  ├─ text ──► pi user message（deliverAs:"followUp"），逐字节原样；`body.image`
  │    作为 pi image part 同行，`attachments` 里的路径是 client 已写好的文件
  ├─ assign_ack{accepted:true}
  ├─ report.heartbeat{agent} — `running` per turn and every 10s while a task is live,
  │    `idle` only while pi waits for input; every beat re-derives it from pi
  ├─ 回合结束的规则由 client 拥有：没有 completion 的回合结束换一次 nudge，
  │    第二次这样的结束就结算这次投递
  ◀── nudge{task_id, text} ──► pi user message，逐字节原样（client 自己的句子）
  ├─ report.complete{outcome, head, details, files} — ledger 的终态事实，也是最后一份报告
  │    └─ the client's answer is the handover: pi is asked to shut down, then detaches
  ├─ probe ──► one heartbeat
  ◀── recycle ──► complete (if unsettled) → stop → pi exits
  └─ detach{reason} when pi shuts down
```

## 1. 安装

该插件是一个 pi 包：`package.json` 声明了 `pi.extensions: ["./src/index.ts"]`，因此 pi 会直接加载 TypeScript 源码，无需构建步骤。

### 使用生成的工作区（常规路径）

`onlyne server generate` 会将 `[server].agent_package` 复制到 `<ws>/.onlyne/agent/<pkg-name>/`，并将该包以相对于设置文件本身的路径写入 `.pi/settings.json`：`../.onlyne/agent/<pkg-name>`（`crates/onlyne-server/src/generate.rs`）。pi 0.85.1 仅加载这种写法。项目的 `packages` 路径相对于包含设置文件的目录（`<ws>/.pi`）解析，因此 `../` 形式可到达 `<ws>/.onlyne/agent/<pkg-name>`。裸的 `.onlyne/agent/<pkg-name>` 条目会解析到 `<ws>/.pi/.onlyne/agent/<pkg-name>`，并将该包列在列表中，但不会加载它。监管器会启动生成的工作区，扩展也会随其一同分发，无需进行全局安装。

```toml
# spec.toml
[server]
agent_package = "/abs/path/to/plugins/onlyne-agent-pi"   # read once, at generate time
```

```bash
onlyne server generate --root <server-root> --out <dir>
```

生成的 `.pi/settings.json` 随后会包含：

```json
{ "packages": ["../.onlyne/agent/onlyne-agent-pi"] }
```

`pi list` 会在“项目包”下列出该条目。要验证实际加载情况，可以让复制后的 `index.ts` 抛出异常并观察失败。

### 手动安装（不使用生成器）

```bash
cp -R plugins/onlyne-agent-pi <ws>/.onlyne/agent/onlyne-agent-pi
printf '{"packages":["../.onlyne/agent/onlyne-agent-pi"]}\n' > <ws>/.pi/settings.json
```

### 从 npm 安装

```bash
pi install npm:pi-onlyne          # user-level: every pi process on this box loads it
```

已发布的 npm 包是 `pi-onlyne`；`pi install npm:pi-onlyne@<version>` 可以固定一个版本。此安装路径也会覆盖普通的交互式会话，在这些会话中扩展保持不活动状态（没有 `ONLYNE_ROLE`，因此没有适配器）。角色工作区无需全局安装即可使用面板：上面的文件级复制或 `onlyne server generate` 会将插件的作用域限定到承载角色的工作区。

### 一次性使用／测试

```bash
pi --session-id <id> -e /abs/path/to/plugins/onlyne-agent-pi -ns -nc
```

### 开关文件

`<cwd>/.pi/onlyne.json`（参见 `onlyne.json.example`）：

| 键 | 默认值 | 效果 |
| --- | --- | --- |
| `enabled` | `true` | `false` 会为此工作区关闭扩展 |
| `watch.autoStart` | `true` | `false` 会注册工具，但在 `/onlyne connect` 前不打开套接字 |

文件缺失时，所有项均使用默认值。文件格式错误时，会在 stderr 打印一条警告并保留默认值：拼写错误不能使角色在无提示的情况下停用。客户端不读取此文件（计划 §11 已将旧的就绪门控降级为生成时模板建议），因此只有此扩展会读取它；键结构仍采用模板所带的结构。

无需其他设置。工作区在 `spec.toml` 中的 `session_command` 已按任务启动 `pi`（`["pi", "--session-id", "{session}"]`），客户端也会注入此扩展所依赖的环境变量。

## 2. 能力

`hello` 帧声明此插件实际实现的功能：

| 能力 | 声明 | 此处的含义 |
| --- | --- | --- |
| `register` | 始终 | 在 `welcome` 之后发送 `session_register{session_id, task_id, generation, pid, title}` |
| `report` | 始终 | `report.ready` / `report.heartbeat` / `report.complete` |
| `inject` | 当 `pi.sendUserMessage` 存在时 | 投递以 `assign{…, text}` 到达，`text` 原样注入为 pi 用户消息 |
| `recycle` | 始终 | `recycle` 会在任务尚未确定最终状态时将其确定，然后停止插件并退出 pi |

pi API 缺失时会发生什么，以及主机随后如何处理：

| 缺口 | 检测方式 | 行为 |
| --- | --- | --- |
| 没有 `registerTool`（较旧的 pi） | 在 `session_start` 时探测 | 不注册任何工具；协议路径不受影响，`/onlyne status` 仍可使用 |
| 没有 `sendUserMessage` | 在 `session_start` 时探测 | 从能力列表中移除 `inject`，主机通过 `config_get{key:"stdin:<投递文本>"}` 传递任务，插件通过仍然可用的通道注入该内容 |
| 没有可写 section 的提示选项 | 每次 run 保护性检测 | 角色说明进不了指令层——`before_agent_start` 没有可供写 section 的对象；stderr 打一行说明，投递文本照常送达 |
| 没有 `appendEntry` | 探测 | 不记录 `onlyne-assign` / `onlyne-complete` 会话条目 |
| 没有 `ui.setStatus` | 保护性检测 | 跳过页脚状态行 |
| 没有 `ui.setWidget` | 保护性检测 | 常规通知继续通过页脚状态行和 stderr 上的 `[pi-onlyne]` 行传递 |
| 没有 `ctx.shutdown` | 保护性检测 | `recycle` 和任务完成仍会确定任务的最终状态；进程会保持运行，等待操作员关闭 |

### 活动面板

当主机报告存在 UI（`ctx.hasUI` 在 TUI 和 RPC 模式下为 `true`，在 print 和 JSON 模式下为 `false`），且 `ctx.ui.setWidget` 可用时，常规 onlyne 通知会显示在编辑器上方、键为 `onlyne` 的面板中。页眉显示角色、连接状态、代次、当前任务 id 和阶段。其下最多显示六条事件，按从新到旧排列：入站帧使用 `<=`，出站帧使用 `=>`，警告使用 `!!`，状态变化使用 `..`，重复投递使用 `~~`。重复的相同事件会合并为一行，并带 `xN`；面板最多容纳八行，每行上限为 96 个单元，`session_shutdown` 会清空面板。

## 3. 工具

仅在 onlyne 会话内注册，采用 MCP 面所携带的同一套 schema（`docs/v2-CONTRACT.md`“3b's interface: the `tools` mount”）：一份义务词汇，两个驱动共用。

每个结果只有一句平实的话——send 与 handoff 给出发往的角色，complete 给出结果。工具结果是模型可见的，插件自己的簿记没有理由出现在里面。

### `onlyne_send{to, text, kind?, image?}`

通过 `send` 帧发送一个信封。`kind: "note"`（默认值）是自由文本，不携带 `op_id`。`kind: "task"` 将工作移交给一个角色，因此携带 `o-<uuid>` 幂等键和新的 `causality.task`。`image` 是 `png/jpeg/gif/webp` 文件的绝对路径：插件读取该文件，进行 base64 编码，并将其作为 `body.image` 附加。核心将该文件限制为 2 MiB，并接受四种 mime 类型。

结果为 `sent to <role>`。

### `onlyne_complete{outcome, summary, details?, files?}`

以明确结果结束当前任务——即 proto 的 `Outcome`：`done`、`failed`、`cancelled` 或 `blocked`。`summary` 是展示用的一行，原样成为账本的 `head`：空白折叠为一行，文本在 200 个字符处截断；`summary` 为空时不携带展示行，head 回退到最后一条助手文本。`details` 是完整结果，`files` 是它所点名的绝对路径：两者原样搭在 `report.complete` 帧上，也正是下游与发起方收到的东西，上限由客户端把守，超限正文由客户端用自己的句子拒绝（契约 §3c）。该调用也会结束会话进程：客户端确认完成报告后（见 §4），插件会通过 `ctx.shutdown()` 请求 pi 关闭。pi 0.85.1 没有工具结果的 `terminate` 处理。结果为 `reported <outcome>`。

### `onlyne_handoff{to, text, image?}`

将此会话的任务交予其族的下一个节点。插件发送一个 `handoff` 帧，指明会话当前持有的任务，主机随后在 `to` 之下创建一个子任务：子任务将本任务命名为其 `parent_task`，位置向后一跳，并携带相同的族 id、跳数预算、origin、deadline 和 labels。结果是 `handed on to <role>`：子任务 id 与跳数是 ledger 的行，不是模型需要读回的东西。客户端拒绝会作为工具错误原样返回。`image` 是发送工具所接受的同类绝对 `png/jpeg/gif/webp` 路径。跳数与跳数预算留在信封的 `causality` 里，属于协议数据：投递文本由 client 自己渲染（发送方、正文、附件路径），本插件原样注入。`onlyne_send{kind: "task"}` 是到达角色的另一种方式：该信封在第 0 跳启动一个新族。

## 4. 结果规则

`onlyne_complete` 是通向 `done` 的唯一路径。插件为每个任务发送一次完成报告，在以下事件中第一个发生时发送：

1. **`onlyne_complete`**——模型给出明确结果：`done`、`failed`、`cancelled` 或 `blocked`。同一任务后续的完成调用会被拒绝，不会再次报告。其非空 `summary` 就是 head。
2. **出错的轮次**——该轮次以模型提供方错误结束（`stopReason: "error"`）。这本身即可证明出错，因此插件立即报告 `failed`，并将错误作为 head。报告本身要等到 pi 正在等待输入时才发出，因此承载它的那次心跳陈述的是会话真实的阶段。
3. **`recycle{outcome}`**——主机正在拆除会话。插件先使用主机给出的结果确定尚未确定状态的任务，然后停止并退出 pi。

正常结束、但没有完成的轮次在这里不结算任何东西。那条规则由 client 拥有（`docs/v2-CONTRACT.md` 的「3c. One turn-end rule」）：它统计这类结束、把自己的一句话作为 `nudge` 发来，并决定一次始终不上报的投递会变成什么。插件把那句话交给 pi，并且只回答自己交出去了，于是措辞、计数与结算都只有一个拥有者，而不是两个。注入消息尚未跑过任何轮次的任务，无论结算信号说什么都不动：现在就上报等于宣称从未发生的工作已经完成。

`head` 是单行文本，上限为 200 个字符；它与客户端写入 `out_head` 的内容以及回执携带的内容一致。每个任务的 `head` 只有一个来源：显式 `onlyne_complete` 调用携带 `summary` 时就是它，否则是失败轮次报告的错误。完全没有携带 `summary` 的调用，回退到最后一条助手文本；此类调用之后说出的句子不能替代调用已经交付的内容，也不会被其他任何内容读取。

已报告的完成会结束会话进程。`report.complete` 以请求形式发出；客户端仅在完成会话记录的处理、确认投递并写入 `Completion` 信封后才回复。插件在该回复到达时请求 pi 关闭。套接字无法传输的结果会进入队列，并在下一次 `hello` 之后刷新；该次刷新收到的回复即为结束进程的交接。主机拒绝的完成会让进程继续运行，因此进程退出不会导致任务丢失。

完成报告是插件发出的最后一份报告；已经完成任务的会话不再回应心跳，也不再回应 `probe`。终态的 agent 维度由客户端自己写入：随后到达的 `detach` 会退役这个已无任务的会话，该路径会喂入 `AgentGone` 并发布这一行（`retire_idle_locked`，`crates/onlyne-client/src/session/dispatch/retire.rs`）。正在离开的进程不为自己声明阶段。

### 空闲判定

只有会话正在等待用户输入时，它才是空闲的。其余时刻一律读作 `running`：正在运行的轮次、已排队的 steer 或 follow-up 消息、重试、压缩，以及被后台任务扩展移出 agent 循环的工作。

插件向 pi 查询，而不是自行假设。`ctx.isIdle()` 回答是否还有运行、压缩或排队中的续跑，`ctx.hasPendingMessages()` 回答输入是否已经在路上；缺失或抛错的探针一律读作 `running`。`agent_settled` 通常是空闲声明落地的时刻，因为 pi 只在一次运行彻底结算之后才触发该事件；单个轮次的结束是另一个时刻。每 10 秒的心跳会在每次触发时重新向 pi 推导阶段，因此重新开始的运行不会留下过期的空闲读数。

后台任务扩展改变了这个问题。`bg_run` 及其同类工具立即返回，工作继续在子进程中运行，于是 pi 在会话任务仍在进行时等待输入。插件通过该扩展注册的工具识别它，并向它的 EventBus 服务查询存活任务列表；处于 `running` 状态的任务会让会话保持 `running`，插件在轮次结束时欠下的那份报告——它自己目睹的失败——也要等它结束。没有安装该扩展的会话没有可识别的工具、没有查询，也就没有可等待的东西。

## 5. 协议说明与差异

下面每项都是对 `PROTOCOL.md` 的有意解读，或是在已发布客户端上测得的行为。

- **报告序列基线。** 插件自身的 `report` 序列从 1000 开始，而非 1。客户端将自己的调度事件（`created`、资源附加、`ready`）记入同一个 `(generation, seq)` 水位，归约器会静默丢弃任何小于或等于该水位的报告（`crates/onlyne-client/src/reconcile/`）。从 1 开始的插件序列会丢失最初几条观测。**一个插件只有一个计数器：** 会话持有的每个任务都沿用同一条序列发送心跳，因为客户端会在该任务两次心跳之间，为它自己写入该行的事件取 `row.seq + 1`；若每个任务每轮只推进一次，心跳恰好会撞在那个数上。但闸门是按任务行判断的，所以每条任务记录也会记下自己最后一次上报的 `seq`（`task.lastSeq`，在 `/onlyne status` 中以 `taskSeqs` 呈现），新的分配会被抬到它之上：`A@1001、B@1002、A@1003` 才是可用的形状，任何任务都不会被交回自己该行已经接受过的 seq。心跳轮次也不会重叠：一轮正在写时到来的心跳请求会并入这一轮，换来多做一遍，而不是同一刻的第二次快照。版本控制的其他部分均遵循规范。
- **`observed` 是一个完整的 `Observation`。** `report.heartbeat` 携带状态元组（`version`、`generation_live`、`isolate_after`、`terminate_after`、`mismatch_count`、`agent`、`delivery`、`resource`、`recovery`），而不是 `{"state": "running"}` 简写：主机对其进行反序列化，覆盖客户端拥有的六个键，并仅应用 `is_legal` 接受的元组。此插件拥有 `agent` 维度（轮次钩子）、`resource` 声明——其进程在记录了挂载操作的窗格中处于活动状态——以及 `host` 绑定。它没有 `delivery`、`recovery`、`generation_live`、`isolate_after`、`terminate_after` 或 `mismatch_count` 的观测依据：在应用元组之前，客户端会依据自己的意图队列、归约器历史和角色配置重写全部六项，因此此插件在这些位置发送的内容不会被读取。任务结果和公开视图均不通过元组传输。
- **`ready` 每次连接报告一次。** 主机自身的交接路径（`crates/onlyne-client/src/session/dispatch/delivery.rs::hand_session`）已会在客户端为挂载插件暂存会话时报告 `ready`，因此主机会将插件的第二次报告视为空操作。插件仍会发送：在任何工作存在之前完成挂载的插件正是就绪屏障所涵盖的情况，而且该报告只占用一个帧。
- **从不发送 `cluster_ref`。** 此插件代表本地角色，不代表聚合角色；出于相同原因，Rust 一侧会将该字段设为 `skip_serializing_if`，使其缺省。
- **使用心跳响应 `probe`**，遵循 `PROTOCOL.md` 中“`probe` 声明新的资源观测”的说明。
- **仅当 `config_get` 以 `stdin:` 开头时，才将其读取为投递文本**，这是 `PROTOCOL.md` 为缺少 `inject` 的插件记录的重载：该键携带的是与 `assign` 的 `text` 相同的已渲染字节，原样注入。任何其他键都会记录到日志并被忽略，不会被误读。
- **`frame_too_large` / `bad_frame`**：正文过大时，会在写入任何字节之前拒绝；分帧错误会关闭连接并重新连接。正文损坏后，分帧无法重新同步，这也与 `crates/onlyne-wire/src/frame.rs` 得出的结论相同。
- **投递具备幂等性；任务不具备。** 去重键是信封 id。同一投递出现两次只会产生一次注入，以及带有 `reason: "duplicate"` 的确认；为已在运行的任务创建的新信封，会作为另一条消息到达该会话。客户端为每个信封生成新的 uuid，因此 `duplicate` 仅会在真正重新提供任务时触发。

- **窗格绑定（Orca 标签页）。** 在 Orca 窗格内，插件会在每次心跳中通过报告 `Observation` 里的 `observed.host.orca.pane_key` 报告其运行所在的窗格（`crates/onlyne-client/src/host.rs`），并在环境变量指明时一并报告 `tab_id` / `leaf_id` 和终端 `handle`。绑定是从*内部*继承的，从不猜测：Orca 窗格会将其 `ORCA_PANE_KEY` / `ORCA_TAB_ID` / `ORCA_LEAF_ID` / `ORCA_TERMINAL_HANDLE` 导出到所启动的命令中（测于 2026-09-11，Orca 1.4.198），客户端会将自己的环境传递给会话命令。因此，窗格内的进程是唯一能够从内部说明 onlyne 会话位于哪个窗格的组件；pi 下游的任何内容都无法恢复此信息。在窗格之外，`host` 键会完全缺失：普通终端中的 pi 会报告不含 host 字段的观测，不会生成空窗格字段。
- **不会为此向工作区写入任何内容。** 已不再有绑定声明文件：绑定信息随客户端已经镜像的观测一同传递。不存在声明文件，因为没有组件创建它，工作区的缓存目录也不会被触及。这使 `integrations/orca-plugin` 无需读取任何路径即可将标签页轴限定为真实会话，也让监管器仍能说明一个*已结束*会话的运行位置：`report.complete` 会继续携带 `host`。

## 6. 配置参考

| 环境变量 | 是否必需 | 效果 |
| --- | --- | --- |
| `ONLYNE_ROLE` | 是 | 挂载角色 |
| `ONLYNE_SESSION_ID` | 是 | 挂载的会话 id；已发布客户端中的 `session_id` 等于 `task_id` |
| `ONLYNE_TASK_ID` | 是 | 此进程承载的任务；驱动 `session_register` 和初始的 `ready` |
| `ONLYNE_SOCKET` | 否 | 客户端为此工作区提供服务的套接字，会注入所启动的每个会话进程；变量未设置时，插件自己去运行目录读注册文件（`<digest>.json`），挑出 `root` 就是本工作区的那个 client |
| `ORCA_PANE_KEY` | 否 | 此进程的运行位置（`<tab_id>:<leaf_id>`），每次心跳通过 `observed.host.orca.pane_key` 上报；在 Orca 窗格之外未设置，因此该字段会缺失 |
| `ORCA_TAB_ID` / `ORCA_LEAF_ID` | 否 | 单独的窗格 id；仅设置窗格键本身时，会对该键进行解析 |
| `ORCA_TERMINAL_HANDLE` | 否 | 终端句柄，在窗格键旁以 `host.orca.handle` 上报，其值即 `orca terminal switch` 所使用的值 |

需要知道的常量：插件每 10 s 发送一次心跳（`heartbeat_timeout_ms` 为 30 s），为 `hello` 留出 5 s，每个请求留出 30 s，并按 1/2/4/8/16/30 s 的阶梯重新连接。

插件会读取自己的一个文件：`<cwd>/.pi/onlyne.json`（开关，§1）。另一处读取属于机器而不是工作区：`ONLYNE_SOCKET` 未设置时，插件遍历机器级运行目录里的 client 注册文件，找出写下本工作区的那个（§7）。

## 7. 故障排除

| 症状 | 原因 | 检查 |
| --- | --- | --- |
| `[pi-onlyne] session …` 始终未出现 | 三个环境变量中缺少一个，或 `enabled` 为 false | `env \| grep ONLYNE_`；`cat .pi/onlyne.json` |
| `socket unresolved: onlyne: no client is registered for <workspace> …` | 该工作区没有 `onlyne-client run`，所以运行目录里没有哪个注册文件的 `root` 是这棵树 | 起 client，或 `onlyne-client status`；这条消息会点出运行目录，以及它实际读到的每个注册文件 |
| `socket unresolved: … N clients there name runtime pi … ambiguous` | 有多个已注册的 client 都在跑 pi 会话，而它们的 root 都不包含本工作区，于是没有唯一可拨的 client | 用 `ONLYNE_SOCKET` 显式指定 socket，或为本工作区起 client |
| `socket error: connect ENOENT <路径>` | 消息里的路径没人 bind：注入它的那个 client 已经停了 | `onlyne-client status` 看它当前服务的 socket，再看 client 日志里带 `socket = <路径>` 的那行 |
| `reconnecting in 4000ms` 持续循环 | 客户端已停止，或套接字已被替换 | `onlyne --server-root … roles` |
| `ready refused: internal: unknown session for …` | 插件完成挂载并为一个客户端从未暂存的任务进行了报告（在任务之外手动启动 pi 时属于正常情况） | 在客户端下启动 pi，不手动启动 |
| `assign` 始终未到达 | 客户端的 `session_command` 未启动 pi，或 `inject` 已被移除 | 在客户端日志中查看启动行；通过 `/onlyne status` 查看能力集合 |
| 账本保持 `in_flight` | 尚未完成：还没有轮次运行（注入消息尚未执行），或者该轮次结束时没有完成，而客户端尚未结算这次投递。在 pi-onlyne 1.2.1 对上 pi 0.87 时，带图片附件的信封会什么都没注入：pi 读取本插件现在送出的扁平 `ImageContent`，拒掉旧插件构造的嵌套 part，任务正文随之一起丢失 | 在 pi 会话文件中查看 `onlyne-assign` 条目及其后注入的 `nudge` 句子；通过 `/onlyne status` 查看任务和阶段；要找那次拒绝，看窗格里的 `Extension "<runtime>" error` 行，以及 1.2.2 上插件的 `attachment carried no base64 data or no media type` 日志 |
| `hello … forbidden`／在 `hello` 后立即关闭连接 | 挂载角色与客户端角色不匹配 | `hello.args.mount.role` 与工作区角色 |
| `frame_too_large` | 正文超过 8 MiB | 仅可通过超大的出站图像达到；此上限来自核心 |
| 工具缺失 | 该 pi 版本中不存在 `pi.registerTool` | `/onlyne status`；上方的能力表 |
| 后台任务仍在运行时，会话显示 `idle` | 未安装后台任务扩展，或其 EventBus 服务未在时限内回应状态查询，插件看不到自己留下的运行中工作 | `[pi-onlyne]` 日志中关于后台探针的行；同一 pi 会话中的 `bg_status` 会列出存活任务 |
| 监管器面板未列出任何标签页 | 没有活动会话报告窗格：适配器早于该报告功能，或此 pi 不在 Orca 窗格内 | `onlyne --server-root … sessions --json` 中的 `projection.observed.host.orca.pane_key`；在窗格内执行 `env \| grep ORCA_` |

`/onlyne status` 会打印实时状态（`connected`、`socket`、`role`、`sessionId`、`generation`、`agentState`、`seq`、`taskSeqs`、`tasks`、`pendingCompletions`、`lastError`、计数器），`/onlyne connect` / `/onlyne disconnect` 可手动打开和关闭套接字。

## 8. 开发

```bash
cd plugins/onlyne-agent-pi
node --test src/*.test.mjs        # framing, protocol, agent state machine, config, socket path
```

除非 `target/debug/onlyne-client` 和 `onlyne-server` 存在，否则 `src/agent.live.test.mjs` 会自行跳过。`crates/onlyne-testkit/e2e/pi-live.sh` 是端到端用例：pi 缺失或没有可用的模型凭据时，它会跳过（退出码 0）；否则，它会通过真实客户端运行一个真实任务，直到 `acked`。

```bash
cd ../..
ONLYNE_BACKEND=fake BIN_DIR=target/debug bash crates/onlyne-testkit/e2e/pi-live.sh
```

该用例加载共享辅助函数后，会导出 `ONLYNE_BACKEND=exec`，因此客户端会自行启动 pi，并使用在整个会话期间保持打开的 stdin 管道。代理自身的输出会写入 `<ws>/.onlyne/logs/session-<task>.log`。
