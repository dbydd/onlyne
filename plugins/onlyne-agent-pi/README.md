# pi-onlyne — the onlyne agent adapter for pi

This pi extension makes one pi process serve one onlyne role session. It connects to
`<role workspace>/.onlyne/run/s`, speaks the adapter protocol in
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
  ├─ prose ──► pi context, once (custom message, no turn)
  ├─ report.ready ──► the barrier the task payload waits behind
  ◀── assign{envelope, prose, task_id, generation}
  ├─ task text (+ image path) ──► pi user message (deliverAs:"followUp")
  ├─ assign_ack{accepted:true}
  ├─ report.heartbeat{running|idle} — per turn, and every 10s while a task is live
  ├─ a turn that ends without `onlyne_complete` ──► the idle ladder: the same
  │    message again (at most `idleReminders` times), then `failed` and out
  ├─ report.complete{outcome, head} — the ledger's terminal fact
  │    └─ then one report.heartbeat{agent:"idle"} stating the agent only
  │       └─ the client's answer is the handover: pi is asked to shut down, then detaches
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
| `idleReminders` | `2` | how many times an idle turn end re-sends the assignment before the task fails (§4); 0 means the first idle without a completion fails it |

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
| `inject` | when `pi.sendUserMessage` exists | the payload arrives as `assign` and is injected as a pi user message |
| `recycle` | always | `recycle` settles the task if it is unsettled, then stops the plugin and exits pi |

What happens when a pi API is missing, and what the host does then:

| gap | detection | behaviour |
| --- | --- | --- |
| no `registerTool` (older pi) | probed at `session_start` | no tools are registered; the protocol path is unaffected, and `/onlyne status` still works |
| no `sendUserMessage` | probed at `session_start` | `inject` is dropped from the capability list, so the host delivers the task through `config_get{key:"stdin:<text>"}`, which the plugin injects through whatever channel remains |
| no `sendMessage` | probed | the role prose from `welcome` is not injected as context; the task itself still arrives |
| no `appendEntry` | probed | no `onlyne-assign` / `onlyne-complete` session entries are recorded |
| no `ui.setStatus` | guarded | the footer status line is skipped |
| no `ui.setWidget` | guarded | routine notices continue through the footer status line and the `[pi-onlyne]` stderr line |
| no `ctx.shutdown` | guarded | `recycle` and a completion still settle the task; the process stays up for the operator to close |

### Activity panel

When the host reports a UI (`ctx.hasUI`, true in the TUI and RPC modes, false in print and JSON modes) and `ctx.ui.setWidget` is available, routine onlyne notices draw in the panel above the editor with widget key `onlyne`. The header shows role, connection state, generation, the current task id, and phase. Below it, up to six newest-first events use `<=` for inbound frames, `=>` for outbound frames, `!!` for warnings, `..` for state changes, and `~~` for duplicate deliveries. Repeated identical events fold into one line with `xN`; the panel holds at most eight lines, each capped at 96 cells, and `session_shutdown` clears it.

## 3. Tools

Registered only inside an onlyne session.

### `onlyne_send{to, text, kind?, image?}`

Sends one envelope on the `send` frame. `kind: "note"` (the default) is free text and
carries no `op_id`. `kind: "task"` hands work to a role, so it carries an `o-<uuid>`
idempotency key and a fresh `causality.task`. `image` is an absolute path to a
png/jpeg/gif/webp file: the plugin reads it, base64-encodes it and attaches it as
`body.image`. The core caps that at 2 MiB and accepts four mime types.

### `onlyne_complete{outcome?, text?, force?, reason?}`

Ends the current task with an explicit outcome (`done` by default, or `failed`). It is the
only path to `done`: a turn that ends without it is reminded and then failed (§4). A
non-empty `text` becomes the ledger `head` verbatim: whitespace collapses to one line and
the text stops at 200 characters. An absent or blank `text` carries no summary, so the
completion falls back to the last assistant text. The call also ends the session's
process: once the client has acknowledged the completion report (see §4), the plugin asks
pi to shut down through `ctx.shutdown()`. pi 0.85.1 has no tool-result `terminate`
handling. When the workspace carries a relay policy (§5), `force: true` with a non-empty
`reason` is the deliberate way past a handoff the session still owes.

### `onlyne_handoff{to, text, image?}`

Hands this session's task on to the next hop of its family. The plugin sends one `handoff`
frame naming the task the session currently holds, and the host mints one child task for
`to` under it: the child names this task as its `parent_task`, sits one hop further along,
and carries the same family id, hop budget, origin, deadline and labels. The tool's result
names the child task id and its hop. A client refusal comes back as the tool's error,
verbatim. `image` is the same absolute png/jpeg/gif/webp path the send tool takes. An
assignment whose causality names a hop budget states the hop and the budget in its
injected header line. `onlyne_send{kind: "task"}` is the other way to reach a role: that
envelope starts a family of its own at hop 0.

## 4. Outcome rules

`onlyne_complete` is the only path to `done`. The plugin sends one completion per task,
at the first of these events:

1. **`onlyne_complete`** — the model gives an explicit outcome (`done` by default, or
   `failed` / `cancelled`). A later completion for the same task is refused (not
   re-reported). Its non-empty `text` is the head.
2. **An errored turn** — the turn ended with a provider error (`stopReason: "error"`).
   That is proof on its own, so the plugin reports `failed` at once, with the error as
   the head.
3. **The idle ladder** — the turn ended cleanly without a completion, and the task is
   still open. The plugin re-sends the assignment and counts the rung. The idle that
   finds the bound `idleReminders` names already spent reports `failed` — head
   `no completion after <n> idle reminders` — and the session exits the way any
   completion makes it exit.
4. **`recycle{outcome}`** — the host is tearing the session down. The plugin settles an
   unsettled task with the host's outcome first, then stops and exits pi.

Two things settle nothing: a task that was assigned but whose turn has not run yet (the
injected message has not executed, so completing now would claim work that never
happened), and an idle the ladder still has a rung for. The ladder re-sends the
assignment the task arrived with — the same header, task text and attachment paths the
injection carried, under one line saying the previous turn ended without a completion —
and never the role prose, which is already in the session's context. Its images are not
re-attached: the paths travel as text, so the same bytes are not put into the context
twice. A new envelope for the same task restarts the count.

`head` is a single line, capped at 200 characters; it matches what the client puts in
`out_head` and what the receipt carries. Each task has one source for it: the `text` of
the explicit `onlyne_complete` call when that call carried one, the error a failed turn
reported, or the ladder's own line. The last assistant text is the fallback for an
`onlyne_complete` call that carried no text at all — a sentence spoken after such a call
cannot replace what the call handed over, and nothing else reads it.

A reported completion ends the session's process. `report.complete` goes out as a request,
and the client answers it only after it has settled the session row, acked the delivery
and written the `Completion` envelope. The plugin asks pi to shut down at that answer. An
outcome the socket could not carry is queued and flushed after the next `hello`, and that
flush's answer is the handover that ends the process. A completion the host refused leaves
the process running, so an exit never loses the task.

The last report is one observation with `agent: "idle"`, sent
after the completion is acknowledged and before the process leaves. The completion settles
the row from the tuple the client holds, and that tuple still reads `running` when the
finishing turn was the last heartbeat. Nothing observes the process afterwards, so without
this report an exited session keeps saying `running`. The plugin skips it when the last
beat was already idle, and a refused settled observation does not hold up the exit the
completion earned.

## 5. Relay guard

A session can hand no work over and still report `done`. That is the accident the guard
closes: a bench session narrated its progress, called `onlyne_complete` with its todos
untouched, and the downstream writer waited for a handoff that was never sent. The guard
judges delivery facts only — whether a role was reached — and never the shape or quality
of the text that was sent.

The policy lives next to the plugin's `package.json`, so it travels inside the copy a
generated workspace loads: `<ws>/.onlyne/agent/onlyne-agent-pi/relay.toml` in a generated
workspace, `relay.toml` in a manual installation.

```toml
relay_required = ["writer"]        # these roles must have received a handoff
relay_required_count = 2           # legacy alias of relay_count: this many distinct downstream roles
```

`relay_count` is the canonical count key. `relay_required_count` is its legacy alias, the spelling `relay.toml` itself uses. `relay_required` wins when both the list and the count keys are present.

The policy belongs in the spec, not in the vendor directory. `onlyne generate --force`
rewrites the copy this package is vendored into and takes a hand-written `relay.toml`
with it, so a `[[client]]` entry states the policy once and the client injects it into
every session process it spawns:

```toml
[[client]]
role = "planner"
relay_count = 2                    # this many distinct downstream roles
relay_required = ["writer"]        # these roles must have received a handoff
```

The sources rank `environment > relay.toml > none`: `ONLYNE_RELAY_REQUIRED` (the list,
comma-separated) and `ONLYNE_RELAY_COUNT` (the count, decimal) are the variables the
client fills from the entry above; a `relay.toml` beside `package.json` is read only
when the environment names no policy at all; and neither one means no guard. Both
variables are injected when the spec names both, so the list still wins. A hand-written
`relay.toml` remains the manual installation's escape hatch — for a box whose spec
never states the policy — and a file shadowed by the environment is ignored outright. A
variable that is set but unparsable is reported on stderr and ignored, which gives the
file its turn.

| | |
| --- | --- |
| default | neither source names a policy: no guard, and the completion path is the one this plugin shipped before the guard existed |
| evidence | the roles this session's own successful `onlyne_send` calls reached, `note` and `task` alike; a refused envelope counts for nothing |
| refusal | `onlyne_complete` throws `onlyne: relay guard: missing handoff to: writer (…)`, naming what is missing and how to clear it |
| after a refusal | nothing is reported, queued or detached: the session stays mounted, and the same call lands once the handoff has gone out |
| list mode | every named role must be in the delivered set, literally |
| count mode | distinct downstream roles; a send to this role itself or back to the role that assigned the task is not one |
| scope | this session's own sends, in process memory: a reconnect keeps them, a restarted session starts empty rather than guessing at what an earlier process sent |
| waiver | `force: true` with a non-empty `reason`; it only matters when the guard refuses |
| audit | a waived completion's ledger head starts with `relay-guard-forced: <reason>`, followed by the model's `text` when the call carried one |
| not guarded | outcomes the plugin reports without the model: an errored turn, the idle ladder's failure, and `recycle{outcome}` |

`relay.toml` is a closed subset of TOML: flat `key = value` lines, the two keys above,
one-line arrays of double-quoted strings, `#` comments. Anything outside that warns on
stderr and is ignored. It is deliberately not `.onlyne/config.toml`: the client parses
that file with `deny_unknown_fields`, so a plugin key there would stop the client from
starting at all.

`force` and `reason` are inert when no policy is in force.

## 6. Protocol notes and deviations

Each item below is either a deliberate reading of `PROTOCOL.md` or a behaviour measured on
the shipped client.

- **Report sequence base.** The plugin's own `report` sequence starts at 1000, not 1. The
  client stamps its own dispatch events (`created`, resource attach, `ready`) into the
  same `(generation, seq)` watermark, and the reducer silently drops any report at or
  below it (`crates/onlyne-session/src/reconcile/`). A plugin sequence starting at 1
  would lose its first observations. Everything else about the versioning is per spec.
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
- **`config_get` is read as a task body only when it starts with `stdin:`**, which is the
  overload `PROTOCOL.md` documents for plugins without `inject`. Any other key is logged
  and ignored, never misread.
- **`frame_too_large` / `bad_frame`**: an oversize body is refused before any byte is
  written, and a framing fault closes the connection and reconnects. Framing cannot
  resynchronise after a corrupt body, which is the same conclusion
  `crates/onlyne-frame/src/lib.rs` reaches.
- **Deliveries are idempotent; tasks are not.** The dedup key is the envelope id. The
  same delivery twice gets one injection and an ack with `reason: "duplicate"`, and a
  new envelope for a task that is already running reaches that session as another
  message — the work record keeps its counters and its relay ledger, and only its
  "turns since this instruction" watchdog restarts. The client mints a fresh uuid per
  envelope, so `duplicate` fires on a genuine re-offer and on nothing else.

- **Pane binding (Orca tabs).** Inside an Orca pane the plugin reports the pane it runs in on every
  heartbeat, as `observed.host.orca.pane_key` in the report's `Observation`
  (`crates/onlyne-session/src/host.rs`), beside `tab_id` / `leaf_id` and the terminal `handle` when
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

## 7. Configuration reference

| env var | required | effect |
| --- | --- | --- |
| `ONLYNE_ROLE` | yes | the mount role |
| `ONLYNE_SESSION_ID` | yes | mounted session id; `session_id` equals `task_id` in the shipped client |
| `ONLYNE_TASK_ID` | yes | the task this process serves; drives `session_register` and the initial `ready` |
| `ONLYNE_SOCKET` | no | the socket the client serves for this workspace, injected into every session process it spawns; with the variable unset the plugin reads the marker `<cwd>/.onlyne/run/socket` for the path the daemon published, and falls back to `<cwd>/.onlyne/run/s` |
| `ONLYNE_RELAY_REQUIRED` | no | the role's spec `relay_required`, comma-joined: the guard's list mode (§5) |
| `ONLYNE_RELAY_COUNT` | no | the role's spec `relay_count`: the guard's count mode, which decides only when the list is empty (§5) |
| `ORCA_PANE_KEY` | no | where this process runs (`<tab_id>:<leaf_id>`), reported on every heartbeat as `observed.host.orca.pane_key`; unset outside an Orca pane, which is why the field is then absent |
| `ORCA_TAB_ID` / `ORCA_LEAF_ID` | no | the pane ids separately; the pane key is parsed when only the key itself is set |
| `ORCA_TERMINAL_HANDLE` | no | the terminal handle, reported beside the pane key as `host.orca.handle`, and the value `orca terminal switch` takes |

Constants worth knowing: the plugin heartbeats every 10 s (`heartbeat_timeout_ms` is 30 s),
allows 5 s for `hello` and 30 s per request, and reconnects on a 1/2/4/8/16/30 s ladder.

The plugin reads three files of its own: `<cwd>/.pi/onlyne.json` (the switch, §1),
`relay.toml` next to its `package.json` (the relay policy's fallback, read only when the
client injected none, §5), and `<cwd>/.onlyne/run/socket` (the marker naming the socket
path the client's daemon bound, read when the environment carried none, §8).

## 8. Troubleshooting

| symptom | cause | check |
| --- | --- | --- |
| `[pi-onlyne] session …` never appears | one of the three env vars is missing, or `enabled` is false | `env \| grep ONLYNE_`; `cat .pi/onlyne.json` |
| `socket error: connect ENOENT …/.onlyne/run/s` | no `onlyne-client run` for this workspace | start the client, or `onlyne-client status` |
| `socket error: connect EINVAL …/.onlyne/run/s` on a deep workspace | macOS gives `sun_path` 104 bytes, so a socket path past 103 is refused; a generated role workspace nests three levels under its server root and a long root carries the canonical spelling over the bound. The client serves such a workspace from a short path under the temporary directory and publishes it in `<workspace>/.onlyne/run/socket` | `onlyne-client status` for the line `onlyne: client running … socket <path>`, which names the served path, plus the client log line carrying `socket = <path>`; `cat <workspace>/.onlyne/run/socket` holds that same path, and the plugin dials it when the environment injected nothing |
| `reconnecting in 4000ms` in a loop | the client is down or the socket was replaced | `onlyne --server-root … roles` |
| `ready refused: internal: unknown session for …` | the plugin mounted and reported for a task the client never staged (normal when pi is started by hand outside a task) | start pi under the client, not by hand |
| `assign` never arrives | the client's `session_command` did not spawn pi, or `inject` was dropped | the client log for the spawn line; `/onlyne status` for the capability set |
| ledger stays `in_flight` | no completion yet: no turn has run (the injected message has not executed), or the ladder is still reminding it (`idleReminders`) | the pi session file for the `onlyne-assign` entry and the reminders injected after it; the `onlyne` panel for `reminder n of m`; `/onlyne status` for the task and phase |
| `onlyne_complete` answers `relay guard: missing handoff to: …` | the workspace's spec (or a `relay.toml` standing in for it) names a role this session never sent to | routine notices appear in the `onlyne` panel; stderr keeps refusals such as `relay guard from …`, socket errors, timeouts and framing faults; `required=…` names the policy; `relay guard: missing handoff …` names the delivered set |
| `hello … forbidden` / connection closed right after `hello` | the mount role does not match the client's role | `hello.args.mount.role` vs the workspace's role |
| `frame_too_large` | a body above 8 MiB | only reachable through an oversize outbound image; the ceiling is the core's |
| tools missing | `pi.registerTool` is absent in that pi version | `/onlyne status`; the capability table above |
| session reads `idle` again after `exited` | a turn-end heartbeat landed after the completion, moving the agent dimension back | the session log for the report order after `completion`; the plugin stops reporting for a completed task, and the client's own `delivery` survives either way |
| the supervisor board lists no tabs | no live session reported a pane: the adapter predates the report, or this pi is not inside an Orca pane | `onlyne --server-root … sessions --json` for `projection.observed.host.orca.pane_key`; `env \| grep ORCA_` inside the pane |

`/onlyne status` prints the live state (`connected`, `socket`, `role`, `sessionId`,
`generation`, `agentState`, `tasks`, `pendingCompletion`, `lastError`, counters), and
`/onlyne connect` / `/onlyne disconnect` open and close the socket by hand.

## 9. Development

```bash
cd plugins/onlyne-agent-pi
node --test src/*.test.mjs        # framing, protocol, agent state machine, config, relay guard, socket path
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

此 pi 扩展让一个 pi 进程承载一个 onlyne 角色会话。它连接到 `<role workspace>/.onlyne/run/s`，使用 `crates/onlyne-adapter/PROTOCOL.md` 中的适配器协议，并按照 `hello → welcome → assign → work → complete → detach` 驱动会话。此处不运行 Rust 代码：协议基于 Node 的 `node:net` 重新实现，使用手写的四字节长度前缀 JSON 编解码器，运行时没有 npm 依赖。

在一个 onlyne 会话之外，扩展不会执行任何操作。客户端会向其启动的每个进程注入 `ONLYNE_ROLE`、`ONLYNE_SESSION_ID` 和 `ONLYNE_TASK_ID`（`crates/onlyne-client/src/session/dispatch.rs`）。任一变量缺失时，这就是一个普通的 pi 会话：插件不注册任何内容，也不打开任何内容。

```
pi session (spawned by onlyne-client)
  │  env: ONLYNE_ROLE / ONLYNE_SESSION_ID / ONLYNE_TASK_ID
  │  .pi/onlyne.json: { "enabled": true, "watch": { "autoStart": true } }
  ▼
hello{protocol:1, plugin:"pi-onlyne", kind:"agent", capabilities:[…], mount:{role,session,task_id,pid}}
  ◀── welcome{role, prose, generation, server, host_capabilities}
  ├─ prose ──► pi context, once (custom message, no turn)
  ├─ report.ready ──► the barrier the task payload waits behind
  ◀── assign{envelope, prose, task_id, generation}
  ├─ task text (+ image path) ──► pi user message (deliverAs:"followUp")
  ├─ assign_ack{accepted:true}
  ├─ report.heartbeat{running|idle} — per turn, and every 10s while a task is live
  ├─ a turn that ends without `onlyne_complete` ──► the idle ladder: the same
  │    message again (at most `idleReminders` times), then `failed` and out
  ├─ report.complete{outcome, head} — the ledger's terminal fact
  │    └─ then one report.heartbeat{agent:"idle"} stating the agent only
  │       └─ the client's answer is the handover: pi is asked to shut down, then detaches
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
| `idleReminders` | `2` | 空闲轮次结束前重新发送任务分配信息的次数，随后任务失败（§4）；`0` 表示第一次没有完成的空闲轮次就会使任务失败 |

文件缺失时，所有项均使用默认值。文件格式错误时，会在 stderr 打印一条警告并保留默认值：拼写错误不能使角色在无提示的情况下停用。客户端不读取此文件（计划 §11 已将旧的就绪门控降级为生成时模板建议），因此只有此扩展会读取它；键结构仍采用模板所带的结构。

无需其他设置。工作区在 `spec.toml` 中的 `session_command` 已按任务启动 `pi`（`["pi", "--session-id", "{session}"]`），客户端也会注入此扩展所依赖的环境变量。

## 2. 能力

`hello` 帧声明此插件实际实现的功能：

| 能力 | 声明 | 此处的含义 |
| --- | --- | --- |
| `register` | 始终 | 在 `welcome` 之后发送 `session_register{session_id, task_id, generation, pid, title}` |
| `report` | 始终 | `report.ready` / `report.heartbeat` / `report.complete` |
| `inject` | 当 `pi.sendUserMessage` 存在时 | 任务正文通过 `assign` 到达，并作为 pi 用户消息注入 |
| `recycle` | 始终 | `recycle` 会在任务尚未确定最终状态时将其确定，然后停止插件并退出 pi |

pi API 缺失时会发生什么，以及主机随后如何处理：

| 缺口 | 检测方式 | 行为 |
| --- | --- | --- |
| 没有 `registerTool`（较旧的 pi） | 在 `session_start` 时探测 | 不注册任何工具；协议路径不受影响，`/onlyne status` 仍可使用 |
| 没有 `sendUserMessage` | 在 `session_start` 时探测 | 从能力列表中移除 `inject`，主机通过 `config_get{key:"stdin:<text>"}` 传递任务，插件通过仍然可用的通道注入该内容 |
| 没有 `sendMessage` | 探测 | 来自 `welcome` 的角色说明不会作为上下文注入；任务本身仍会送达 |
| 没有 `appendEntry` | 探测 | 不记录 `onlyne-assign` / `onlyne-complete` 会话条目 |
| 没有 `ui.setStatus` | 保护性检测 | 跳过页脚状态行 |
| 没有 `ui.setWidget` | 保护性检测 | 常规通知继续通过页脚状态行和 stderr 上的 `[pi-onlyne]` 行传递 |
| 没有 `ctx.shutdown` | 保护性检测 | `recycle` 和任务完成仍会确定任务的最终状态；进程会保持运行，等待操作员关闭 |

### 活动面板

当主机报告存在 UI（`ctx.hasUI` 在 TUI 和 RPC 模式下为 `true`，在 print 和 JSON 模式下为 `false`），且 `ctx.ui.setWidget` 可用时，常规 onlyne 通知会显示在编辑器上方、键为 `onlyne` 的面板中。页眉显示角色、连接状态、代次、当前任务 id 和阶段。其下最多显示六条事件，按从新到旧排列：入站帧使用 `<=`，出站帧使用 `=>`，警告使用 `!!`，状态变化使用 `..`，重复投递使用 `~~`。重复的相同事件会合并为一行，并带 `xN`；面板最多容纳八行，每行上限为 96 个单元，`session_shutdown` 会清空面板。

## 3. 工具

仅在 onlyne 会话内注册。

### `onlyne_send{to, text, kind?, image?}`

通过 `send` 帧发送一个信封。`kind: "note"`（默认值）是自由文本，不携带 `op_id`。`kind: "task"` 将工作移交给一个角色，因此携带 `o-<uuid>` 幂等键和新的 `causality.task`。`image` 是 `png/jpeg/gif/webp` 文件的绝对路径：插件读取该文件，进行 base64 编码，并将其作为 `body.image` 附加。核心将该文件限制为 2 MiB，并接受四种 mime 类型。

### `onlyne_complete{outcome?, text?, force?, reason?}`

以明确结果结束当前任务（默认为 `done`，或为 `failed`）。这是通向 `done` 的唯一路径：没有调用它的轮次会先收到提醒，随后失败（§4）。非空 `text` 会原样成为账本的 `head`：空白折叠为一行，文本在 200 个字符处截断。缺失或为空的 `text` 不携带摘要，因此完成时回退到最后一条助手文本。该调用也会结束会话进程：客户端确认完成报告后（见 §4），插件会通过 `ctx.shutdown()` 请求 pi 关闭。pi 0.85.1 没有工具结果的 `terminate` 处理。工作区带有中继策略时（§5），`force: true` 搭配非空 `reason`，可明确绕过会话仍需完成的交接。

### `onlyne_handoff{to, text, image?}`

将此会话的任务交予其族的下一个节点。插件发送一个 `handoff` 帧，指明会话当前持有的任务，主机随后在 `to` 之下创建一个子任务：子任务将本任务命名为其 `parent_task`，位置向后一跳，并携带相同的族 id、跳数预算、origin、deadline 和 labels。工具结果会给出子任务 id 及其跳数。客户端拒绝会作为工具错误原样返回。`image` 是发送工具所接受的同类绝对 `png/jpeg/gif/webp` 路径。causality 指明跳数预算的任务分配信息，会在其注入的页眉行中写明当前跳数与预算。`onlyne_send{kind: "task"}` 是到达角色的另一种方式：该信封在第 0 跳启动一个新族。

## 4. 结果规则

`onlyne_complete` 是通向 `done` 的唯一路径。插件为每个任务发送一次完成报告，在以下事件中第一个发生时发送：

1. **`onlyne_complete`**——模型给出明确结果（默认为 `done`，或为 `failed` / `cancelled`）。同一任务后续的完成调用会被拒绝，不会再次报告。其非空 `text` 就是 head。
2. **出错的轮次**——该轮次以模型提供方错误结束（`stopReason: "error"`）。这本身即可证明出错，因此插件立即报告 `failed`，并将错误作为 head。
3. **空闲阶梯**——该轮次正常结束但未完成，且任务仍处于打开状态。插件重新发送任务分配信息，并计入当前级数。当某次空闲发现 `idleReminders` 指定的上限已经用尽时，会报告 `failed`——head 为 `no completion after <n> idle reminders`——并以完成时相同的方式退出会话。
4. **`recycle{outcome}`**——主机正在拆除会话。插件先使用主机给出的结果确定尚未确定状态的任务，然后停止并退出 pi。

有两种情况不会确定最终状态：任务已经分配，但其轮次尚未运行，此时注入消息尚未执行，立即完成会声称完成了从未发生的工作；以及阶梯仍有下一级可走的空闲。阶梯重新发送任务最初到达时的分配信息，使用与注入时相同的页眉、任务文本和附件路径，并附上一行说明上一轮次结束时没有完成。阶梯不会重新发送角色说明，因为角色说明已存在于会话上下文中。其图像不会再次附加：路径以文本形式传递，因此相同的字节不会两次进入上下文。同任务的新信封会重新开始计数。

`head` 是单行文本，上限为 200 个字符；它与客户端写入 `out_head` 的内容以及回执携带的内容一致。每个任务的 `head` 只有一个来源：显式 `onlyne_complete` 调用携带的 `text`、失败轮次所报告的错误，或者阶梯自身的文本行。对于完全没有携带文本的 `onlyne_complete` 调用，最后一条助手文本作为回退；此类调用之后说出的句子不能替代调用已经交付的内容，也不会被其他任何内容读取。

已报告的完成会结束会话进程。`report.complete` 以请求形式发出；客户端仅在完成会话记录的处理、确认投递并写入 `Completion` 信封后才回复。插件在该回复到达时请求 pi 关闭。套接字无法传输的结果会进入队列，并在下一次 `hello` 之后刷新；该次刷新收到的回复即为结束进程的交接。主机拒绝的完成会让进程继续运行，因此进程退出不会导致任务丢失。

最后一次报告是带有 `agent: "idle"` 的一次观测，在完成报告获得确认后、进程退出前发送。完成报告根据客户端持有的元组完成会话记录的状态；如果结束轮次同时也是最后一次心跳，该元组仍为 `running`。之后没有组件观察此进程，因此缺少这份报告时，已退出的会话会持续显示 `running`。如果最后一次心跳已经是 idle，插件会跳过此报告；已被拒绝且状态已经确定的观测不会延迟此完成操作促成的退出。

## 5. 中继守卫

会话即使没有交接任何工作，也可能报告 `done`。中继守卫用于防止此类意外：一个 bench 会话叙述了自身的进度，在待办事项尚未处理时调用了 `onlyne_complete`，下游的 `writer` 一直等待一个从未发送的交接。中继守卫仅判定投递事实——某个角色是否收到消息——不会判断已发送文本的形式或质量。

策略位于插件的 `package.json` 旁边，因此会随生成工作区所加载的副本一同分发：生成工作区中为 `<ws>/.onlyne/agent/onlyne-agent-pi/relay.toml`，手动安装中为 `relay.toml`。

```toml
relay_required = ["writer"]        # these roles must have received a handoff
relay_required_count = 2           # legacy alias of relay_count: this many distinct downstream roles
```

`relay_count` 是规范计数键。`relay_required_count` 是其旧版别名，也是 `relay.toml` 自身采用的拼写。列表键和计数键同时存在时，`relay_required` 优先。

策略应放在规范中，而不是供应商目录中。`onlyne generate --force` 会重写此包被供应到的副本，并连同其中的手写 `relay.toml` 一同处理，因此一个 `[[client]]` 条目只需声明一次策略，客户端就会将其注入所启动的每个会话进程：

```toml
[[client]]
role = "planner"
relay_count = 2                    # this many distinct downstream roles
relay_required = ["writer"]        # these roles must have received a handoff
```

来源的优先级为 `environment > relay.toml > none`：`ONLYNE_RELAY_REQUIRED`（列表，逗号分隔）和 `ONLYNE_RELAY_COUNT`（计数，十进制）是客户端根据上述条目填充的变量；只有环境变量完全未指明策略时，才读取 `package.json` 旁边的 `relay.toml`；两个来源均未指定策略时，不启用守卫。规范同时命名两个变量时，两者都会注入，因此列表仍然优先。手写 `relay.toml` 仍是手动安装的应急入口，适用于规范始终未声明策略的机器；被环境变量遮蔽的文件会被直接忽略。变量已设置但无法解析时，会在 stderr 上报告并忽略，此时由文件决定策略。

|  |  |
| --- | --- |
| 默认 | 两个来源均未指明策略：不启用守卫，完成路径沿用守卫加入前此插件随包提供的路径 |
| 证据 | 此会话自身成功执行的 `onlyne_send` 调用所触达的角色，包括 `note` 和 `task`；被拒绝的信封不计入任何角色 |
| 拒绝 | `onlyne_complete` 抛出 `onlyne: relay guard: missing handoff to: writer (…)`，并指出缺少的内容及解除方法 |
| 拒绝之后 | 不会报告任何内容，不会将结果排队，也不会分离：会话保持挂载状态；交接发出后，同一调用即可通过 |
| 列表模式 | 每个列出的角色都必须按字面出现在已送达集合中 |
| 计数模式 | 不同的下游角色；发送到当前角色自身或发回分配任务的角色不计入 |
| 范围 | 此会话自身在进程内存中的发送记录：重新连接会保留记录，重启后的会话从空状态开始，不推测早先进程发送过什么 |
| 豁免 | `force: true` 搭配非空 `reason`；仅在守卫拒绝时发挥作用 |
| 审计 | 获豁免的完成报告，其账本 head 以 `relay-guard-forced: <reason>` 开头；若调用携带了模型的 `text`，则将其附加在后面 |
| 不受守卫保护 | 插件在没有模型参与时报告的结果：出错的轮次、阶梯导致的失败，以及 `recycle{outcome}` |

`relay.toml` 是 TOML 的封闭子集：扁平的 `key = value` 行、上述两个键、由双引号字符串构成的单行数组，以及 `#` 注释。超出此范围的内容会在 stderr 上警告并被忽略。它不采用 `.onlyne/config.toml`：客户端使用 `deny_unknown_fields` 解析该文件，因此在其中加入插件键会阻止客户端启动。

没有生效策略时，`force` 和 `reason` 不起作用。

## 6. 协议说明与差异

下面每项都是对 `PROTOCOL.md` 的有意解读，或是在已发布客户端上测得的行为。

- **报告序列基线。** 插件自身的 `report` 序列从 1000 开始，而非 1。客户端将自己的调度事件（`created`、资源附加、`ready`）记入同一个 `(generation, seq)` 水位，归约器会静默丢弃任何小于或等于该水位的报告（`crates/onlyne-session/src/reconcile/`）。从 1 开始的插件序列会丢失最初几条观测。版本控制的其他部分均遵循规范。
- **`observed` 是一个完整的 `Observation`。** `report.heartbeat` 携带状态元组（`version`、`generation_live`、`isolate_after`、`terminate_after`、`mismatch_count`、`agent`、`delivery`、`resource`、`recovery`），而不是 `{"state": "running"}` 简写：主机对其进行反序列化，覆盖客户端拥有的六个键，并仅应用 `is_legal` 接受的元组。此插件拥有 `agent` 维度（轮次钩子）、`resource` 声明——其进程在记录了挂载操作的窗格中处于活动状态——以及 `host` 绑定。它没有 `delivery`、`recovery`、`generation_live`、`isolate_after`、`terminate_after` 或 `mismatch_count` 的观测依据：在应用元组之前，客户端会依据自己的意图队列、归约器历史和角色配置重写全部六项，因此此插件在这些位置发送的内容不会被读取。任务结果和公开视图均不通过元组传输。
- **`ready` 每次连接报告一次。** 主机自身的交接路径（`crates/onlyne-client/src/session/dispatch/delivery.rs::hand_session`）已会在客户端为挂载插件暂存会话时报告 `ready`，因此主机会将插件的第二次报告视为空操作。插件仍会发送：在任何工作存在之前完成挂载的插件正是就绪屏障所涵盖的情况，而且该报告只占用一个帧。
- **从不发送 `cluster_ref`。** 此插件代表本地角色，不代表聚合角色；出于相同原因，Rust 一侧会将该字段设为 `skip_serializing_if`，使其缺省。
- **使用心跳响应 `probe`**，遵循 `PROTOCOL.md` 中“`probe` 声明新的资源观测”的说明。
- **仅当 `config_get` 以 `stdin:` 开头时，才将其读取为任务正文**，这是 `PROTOCOL.md` 为缺少 `inject` 的插件记录的重载。任何其他键都会记录到日志并被忽略，不会被误读。
- **`frame_too_large` / `bad_frame`**：正文过大时，会在写入任何字节之前拒绝；分帧错误会关闭连接并重新连接。正文损坏后，分帧无法重新同步，这也与 `crates/onlyne-frame/src/lib.rs` 得出的结论相同。
- **投递具备幂等性；任务不具备。** 去重键是信封 id。同一投递出现两次只会产生一次注入，以及带有 `reason: "duplicate"` 的确认；为已在运行的任务创建的新信封，会作为另一条消息到达该会话——工作记录保留其计数器和其中继账本，仅其“自该指令以来的轮次”看门狗重新启动。客户端为每个信封生成新的 uuid，因此 `duplicate` 仅会在真正重新提供任务时触发。

- **窗格绑定（Orca 标签页）。** 在 Orca 窗格内，插件会在每次心跳中通过报告 `Observation` 里的 `observed.host.orca.pane_key` 报告其运行所在的窗格（`crates/onlyne-session/src/host.rs`），并在环境变量指明时一并报告 `tab_id` / `leaf_id` 和终端 `handle`。绑定是从*内部*继承的，从不猜测：Orca 窗格会将其 `ORCA_PANE_KEY` / `ORCA_TAB_ID` / `ORCA_LEAF_ID` / `ORCA_TERMINAL_HANDLE` 导出到所启动的命令中（测于 2026-09-11，Orca 1.4.198），客户端会将自己的环境传递给会话命令。因此，窗格内的进程是唯一能够从内部说明 onlyne 会话位于哪个窗格的组件；pi 下游的任何内容都无法恢复此信息。在窗格之外，`host` 键会完全缺失：普通终端中的 pi 会报告不含 host 字段的观测，不会生成空窗格字段。
- **不会为此向工作区写入任何内容。** 已不再有绑定声明文件：绑定信息随客户端已经镜像的观测一同传递。不存在声明文件，因为没有组件创建它，工作区的缓存目录也不会被触及。这使 `integrations/orca-plugin` 无需读取任何路径即可将标签页轴限定为真实会话，也让监管器仍能说明一个*已结束*会话的运行位置：`report.complete` 会继续携带 `host`。

## 7. 配置参考

| 环境变量 | 是否必需 | 效果 |
| --- | --- | --- |
| `ONLYNE_ROLE` | 是 | 挂载角色 |
| `ONLYNE_SESSION_ID` | 是 | 挂载的会话 id；已发布客户端中的 `session_id` 等于 `task_id` |
| `ONLYNE_TASK_ID` | 是 | 此进程承载的任务；驱动 `session_register` 和初始的 `ready` |
| `ONLYNE_SOCKET` | 否 | 客户端为此工作区提供服务的套接字，会注入所启动的每个会话进程；变量未设置时，插件读取标记 `<cwd>/.onlyne/run/socket` 以获取守护进程公布的路径，并回退到 `<cwd>/.onlyne/run/s` |
| `ONLYNE_RELAY_REQUIRED` | 否 | 角色规范中的 `relay_required`，以逗号连接：守卫的列表模式（§5） |
| `ONLYNE_RELAY_COUNT` | 否 | 角色规范中的 `relay_count`：守卫的计数模式，仅在列表为空时决定结果（§5） |
| `ORCA_PANE_KEY` | 否 | 此进程的运行位置（`<tab_id>:<leaf_id>`），每次心跳通过 `observed.host.orca.pane_key` 上报；在 Orca 窗格之外未设置，因此该字段会缺失 |
| `ORCA_TAB_ID` / `ORCA_LEAF_ID` | 否 | 单独的窗格 id；仅设置窗格键本身时，会对该键进行解析 |
| `ORCA_TERMINAL_HANDLE` | 否 | 终端句柄，在窗格键旁以 `host.orca.handle` 上报，其值即 `orca terminal switch` 所使用的值 |

需要知道的常量：插件每 10 s 发送一次心跳（`heartbeat_timeout_ms` 为 30 s），为 `hello` 留出 5 s，每个请求留出 30 s，并按 1/2/4/8/16/30 s 的阶梯重新连接。

插件会读取自己的三个文件：`<cwd>/.pi/onlyne.json`（开关，§1）、`package.json` 旁边的 `relay.toml`（中继策略的回退来源，仅在客户端没有注入策略时读取，§5），以及 `<cwd>/.onlyne/run/socket`（标记客户端守护进程所绑定的套接字路径，当环境变量未携带该路径时读取，§8）。

## 8. 故障排除

| 症状 | 原因 | 检查 |
| --- | --- | --- |
| `[pi-onlyne] session …` 始终未出现 | 三个环境变量中缺少一个，或 `enabled` 为 false | `env \| grep ONLYNE_`；`cat .pi/onlyne.json` |
| `socket error: connect ENOENT …/.onlyne/run/s` | 此工作区没有运行 `onlyne-client run` | 启动客户端，或 `onlyne-client status` |
| 深层工作区出现 `socket error: connect EINVAL …/.onlyne/run/s` | macOS 为 `sun_path` 提供 104 字节，因此超过 103 的套接字路径会被拒绝；生成的角色工作区嵌套在服务器根目录下三层，过长的根路径会使规范拼写超过此上限。客户端会从临时目录下的短路径为此类工作区提供服务，并将其公布在 `<workspace>/.onlyne/run/socket` | 通过 `onlyne-client status` 查找 `onlyne: client running … socket <path>` 这一行，其中会指明所服务的路径；还需查看带有 `socket = <path>` 的客户端日志行；`cat <workspace>/.onlyne/run/socket` 包含同一路径，环境变量未注入任何值时，插件会连接该路径 |
| `reconnecting in 4000ms` 持续循环 | 客户端已停止，或套接字已被替换 | `onlyne --server-root … roles` |
| `ready refused: internal: unknown session for …` | 插件完成挂载并为一个客户端从未暂存的任务进行了报告（在任务之外手动启动 pi 时属于正常情况） | 在客户端下启动 pi，不手动启动 |
| `assign` 始终未到达 | 客户端的 `session_command` 未启动 pi，或 `inject` 已被移除 | 在客户端日志中查看启动行；通过 `/onlyne status` 查看能力集合 |
| 账本保持 `in_flight` | 尚未完成：还没有轮次运行（注入消息尚未执行），或者阶梯仍在提醒（`idleReminders`） | 在 pi 会话文件中查看 `onlyne-assign` 条目及其后注入的提醒；通过 `onlyne` 面板查看 `reminder n of m`；通过 `/onlyne status` 查看任务和阶段 |
| `onlyne_complete` 回复 `relay guard: missing handoff to: …` | 工作区的规范（或替代该规范的 `relay.toml`）指定了一个此会话从未向其发送的角色 | 常规通知会出现在 `onlyne` 面板中；stderr 会保留拒绝消息，例如 `relay guard from …`、套接字错误、超时和分帧错误；`required=…` 指明策略；`relay guard: missing handoff …` 指明已送达的集合 |
| `hello … forbidden`／在 `hello` 后立即关闭连接 | 挂载角色与客户端角色不匹配 | `hello.args.mount.role` 与工作区角色 |
| `frame_too_large` | 正文超过 8 MiB | 仅可通过超大的出站图像达到；此上限来自核心 |
| 工具缺失 | 该 pi 版本中不存在 `pi.registerTool` | `/onlyne status`；上方的能力表 |
| 会话在 `exited` 之后再次显示 `idle` | 轮次结束时的心跳在完成报告之后到达，将 agent 维度移回 | 在会话日志中查看 `completion` 之后的报告顺序；插件会停止为已完成的任务进行报告，无论哪种顺序，客户端自身的 `delivery` 都会保留 |
| 监管器面板未列出任何标签页 | 没有活动会话报告窗格：适配器早于该报告功能，或此 pi 不在 Orca 窗格内 | `onlyne --server-root … sessions --json` 中的 `projection.observed.host.orca.pane_key`；在窗格内执行 `env \| grep ORCA_` |

`/onlyne status` 会打印实时状态（`connected`、`socket`、`role`、`sessionId`、`generation`、`agentState`、`tasks`、`pendingCompletion`、`lastError`、计数器），`/onlyne connect` / `/onlyne disconnect` 可手动打开和关闭套接字。

## 9. 开发

```bash
cd plugins/onlyne-agent-pi
node --test src/*.test.mjs        # framing, protocol, agent state machine, config, relay guard, socket path
```

除非 `target/debug/onlyne-client` 和 `onlyne-server` 存在，否则 `src/agent.live.test.mjs` 会自行跳过。`crates/onlyne-testkit/e2e/pi-live.sh` 是端到端用例：pi 缺失或没有可用的模型凭据时，它会跳过（退出码 0）；否则，它会通过真实客户端运行一个真实任务，直到 `acked`。

```bash
cd ../..
ONLYNE_BACKEND=fake BIN_DIR=target/debug bash crates/onlyne-testkit/e2e/pi-live.sh
```

该用例加载共享辅助函数后，会导出 `ONLYNE_BACKEND=exec`，因此客户端会自行启动 pi，并使用在整个会话期间保持打开的 stdin 管道。代理自身的输出会写入 `<ws>/.onlyne/logs/session-<task>.log`。
