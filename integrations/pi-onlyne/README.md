# pi-onlyne — the onlyne agent adapter for pi

A pi extension that makes one pi process serve one onlyne role session. It connects to
`<role workspace>/.onlyne/run/s`, speaks the adapter protocol from
`crates/onlyne-adapter/PROTOCOL.md`, and carries a session through
`hello → welcome → assign → work → complete → detach`. No Rust code is involved: the
protocol is reimplemented here over Node's `node:net` with a hand-written four-byte
length-prefixed JSON codec, and there are no runtime npm dependencies.

The extension is inert outside an onlyne session: the client injects
`ONLYNE_ROLE`, `ONLYNE_SESSION_ID` and `ONLYNE_TASK_ID` into the process it spawns
(`crates/onlyne-client/src/dispatch.rs`), and with any of the three missing this is an
ordinary pi session where the plugin registers nothing and opens nothing.

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
  ├─ report.complete{outcome, head} — the ledger's terminal fact
  │    └─ then one report.heartbeat{agent:"idle"} carrying the settled tuple
  │       └─ the client's answer is the handover: pi is asked to shut down, then detaches
  ├─ probe ──► one heartbeat
  ◀── recycle ──► complete (if unsettled) → stop → pi exits
  └─ detach{reason} when pi shuts down
```

## 1. Install

The plugin is a pi package: `package.json` declares `pi.extensions: ["./src/index.ts"]`,
so pi loads the TypeScript source directly (no build step).

### With a generated workspace (the normal path)

`onlyne server generate` vendors `[server].agent_package` into
`<ws>/.onlyne/agent/<pkg-name>/` and writes that package into `.pi/settings.json` as a
path relative to the settings file itself, `../.onlyne/agent/<pkg-name>`
(`crates/onlyne-server/src/generate.rs`). That spelling is the one pi 0.85.1 loads: a
project `packages` path resolves against the directory holding the settings file
(`<ws>/.pi`), so the `../` form reaches `<ws>/.onlyne/agent/<pkg-name>` while a bare
`.onlyne/agent/<pkg-name>` entry would resolve to `<ws>/.pi/.onlyne/agent/<pkg-name>`
and list the package without loading it. The generated workspace is what a supervisor
starts, and the extension travels with it: nothing is installed globally.

```toml
# spec.toml
[server]
agent_package = "/abs/path/to/integrations/pi-onlyne"   # read once, at generate time
```

```bash
onlyne server generate --root <server-root> --out <dir>
```

The generated `.pi/settings.json` then carries:

```json
{ "packages": ["../.onlyne/agent/pi-onlyne"] }
```

`pi list` shows the entry under "Project packages". The load itself is verified by
making the vendored `index.ts` throw and watching the failure surface.

### Manual (no generator)

```bash
cp -R integrations/pi-onlyne <ws>/.onlyne/agent/pi-onlyne
printf '{"packages":["../.onlyne/agent/pi-onlyne"]}\n' > <ws>/.pi/settings.json
```

### One-off / testing

```bash
pi --session-id <id> -e /abs/path/to/integrations/pi-onlyne -ns -nc
```

### The switch file

`<cwd>/.pi/onlyne.json` (see `onlyne.json.example`):

| key | default | effect |
| --- | --- | --- |
| `enabled` | `true` | `false` turns the extension off for this workspace |
| `watch.autoStart` | `true` | `false` registers the tools but opens no socket until `/onlyne connect` |

A missing file means both defaults. A malformed file warns on stderr and keeps both
defaults — a typo must not silently disable a role. The client does not read this file
(§11 of the plan downgraded the old readiness gates to generate-time template advice), so
only this extension consumes it; the key shape stays the one the templates carry.

Nothing else is needed: the workspace's `session_command` in `spec.toml` already spawns
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

Degradations, and what the host does with each:

| gap | detection | behaviour |
| --- | --- | --- |
| no `registerTool` (older pi) | probed at `session_start` | no tools are registered; the protocol path is unaffected, and `/onlyne status` still works |
| no `sendUserMessage` | probed at `session_start` | `inject` is dropped from the capability list, so the host delivers the task through `config_get{key:"stdin:<text>"}`, which the plugin injects through whatever channel remains |
| no `sendMessage` | probed | the role prose from `welcome` is not injected as context; the task itself still arrives |
| no `appendEntry` | probed | no `onlyne-assign` / `onlyne-complete` session entries are recorded |
| no `ui.setStatus` | guarded | the footer status line is skipped |
| no `ctx.shutdown` | guarded | `recycle` and a completion still settle the task; the process stays up for the operator to close |

## 3. Tools

Registered only inside an onlyne session.

### `onlyne_send{to, text, kind?, image?}`

Submits one envelope on the `send` frame. `kind: "note"` (default) is free text and
carries no `op_id`; `kind: "task"` hands work to a role, so it carries an `o-<uuid>`
idempotency key and a fresh `causality.task`. `image` is an absolute path to a
png/jpeg/gif/webp file; it is read, base64-encoded and attached as `body.image`, subject
to the core's 2 MiB ceiling and its four accepted mime types.

### `onlyne_complete{outcome?, text?}`

Ends the current task with an explicit outcome (`done` default, or `failed`). A non-empty
`text` becomes the ledger `head` verbatim (whitespace-collapsed to one line, capped at 200
characters); an absent or blank `text` carries no summary, so the completion falls back to
the last assistant text. The call also ends the session's process: once the client has
acknowledged the completion report (see §4), the plugin asks pi to shut down through
`ctx.shutdown()`. pi 0.85.1 has no tool-result `terminate` handling.

## 4. Outcome rules

One completion is sent per task, at the first of these events:

1. **`onlyne_complete`** — an explicit outcome from the model. It wins over everything
   else, and a later completion for the same task is refused (not re-reported). Its
   non-empty `text` is the head.
2. **`agent_settled`** — pi will not continue on its own (no retry, compaction, or queued
   continuation pending). The plugin reports:
   - `failed` when the turn ended with a provider error (`stopReason: "error"`), with the
     error as the head;
   - `done` otherwise, with the last assistant text as the head;
   - nothing at all when the task was assigned but no turn has run yet — the injected
     message has not executed, and completing then would claim work that never happened.
3. **`recycle{outcome}`** — the host is tearing the session down. An unsettled task is
   settled with the host's outcome first, then the plugin stops and exits pi.

`head` is a single line, capped at 200 characters, matching what the client puts in
`out_head` and what the receipt carries. It has one source per task: the `text` of the
explicit `onlyne_complete` call when that call carried one, and the last assistant text
otherwise. The auto rule is that fallback path — it reports the text of the turn it
settles, and a sentence spoken after the call cannot replace what the call handed over.

A reported completion ends the session's process. `report.complete` goes out as a request,
and the client answers it only after it has settled the session row, acked the delivery
and written the `Completion` envelope; the plugin asks pi to shut down at that answer.
An outcome the socket could not carry is queued and flushed after the next `hello`, and
that flush's answer is the handover that ends the process. A completion the host refused
leaves the process running, so the task is never lost to an exit.

The last report is one observation with `agent: "idle"` beside the settled outcome, sent
after the completion is acknowledged and before the process leaves. The completion settles
the row from the tuple the client holds, which still reads `running` when the finishing
turn was the last heartbeat, and nothing observes the process afterwards — so without this
report an exited session keeps saying `running`. It is skipped when the last beat was
already idle, and a refused settled observation does not hold up the exit the completion
earned.

## 5. Protocol notes and deviations

Everything below is either a deliberate reading of `PROTOCOL.md` or a measured behaviour
of the shipped client.

- **Report sequence base.** The plugin's own `report` sequence starts at 1000, not 1. The
  client stamps its own dispatch events (`created`, resource attach, `ready`) into the
  same `(generation, seq)` watermark and the reducer silently drops a report at or below
  it (`crates/onlyne-session/src/reconcile.rs`), so a plugin sequence starting at 1 would
  lose its first observations. Everything else about the versioning is per spec.
- **`observed` is a full `Observation`.** `report.heartbeat` carries the whole legal state
  tuple (`version`, `generation_live`, `isolate_after`, `terminate_after`,
  `mismatch_count`, `agent`, `delivery`, `resource`, `recovery`, `outcome`, `public`), not
  a `{"state": "running"}` shorthand: the host deserialises it and rejects anything
  `is_legal` refuses. This plugin owns only the `agent` dimension (turn hooks); it leaves
  `delivery` at `none` and `outcome` at `pending`, which is its own truth until it reports
  a completion. `resource` is reported `attached` because the host's own dispatch path
  already recorded the attach.
- **`ready` is reported once per connection.** The host's own hand-off path
  (`crates/onlyne-client/src/dispatch.rs::hand_session`) already reports `ready` when the
  client stages the session for a mounting plugin; a second report from the plugin is a
  no-op at the host. It is sent anyway, because a plugin that mounts *before* any work
  exists is the case the ready barrier names, and it costs one frame.
- **`cluster_ref` is never sent.** This plugin speaks for a local role, never for an
  aggregate; the field is `skip_serializing_if` absent on the Rust side for the same
  reason.
- **`probe` is answered with a heartbeat**, per `PROTOCOL.md`'s "a `probe` declares fresh
  resource observations".
- **`config_get` is read as a task body only when it starts with `stdin:`**, which is the
  overload `PROTOCOL.md` documents for plugins without `inject`. Any other key is logged
  and ignored rather than misread.
- **`frame_too_large` / `bad_frame`**: an oversize body is refused before any byte is
  written, and a framing fault closes the connection and reconnects — framing cannot
  resynchronise after a corrupt body, which is the same conclusion
  `crates/onlyne-frame/src/lib.rs` reaches.
- **Task ids here are single-use.** Duplicate `assign` deliveries for the same task are
  acked (`reason: "duplicate"`) without a second injection; the id is remembered for the
  life of the connection. The client today mints a fresh uuid per task, so this only ever
  fires on a genuine redelivery.

- **Pane binding (Orca tabs).** Inside an Orca pane the plugin reports the pane it runs in on every
  heartbeat, as `observed.host.orca.pane_key` in the report's `Observation`
  (`crates/onlyne-session/src/host.rs`), beside `tab_id` / `leaf_id` and the terminal `handle` when
  the environment names them. The binding is *inherited*, never guessed: an Orca pane exports
  `ORCA_PANE_KEY` / `ORCA_TAB_ID` / `ORCA_LEAF_ID` / `ORCA_TERMINAL_HANDLE` into the command it
  starts (measured 2026-09-11, Orca 1.4.198), and the client passes its environment on to the
  session command — so the process running inside a pane is the one component that can state, from
  the inside, which pane an onlyne session is, and nothing downstream of pi can recover that.
  Outside a pane the `host` key is absent altogether: a pi on a plain terminal reports an
  observation with no host field, rather than one with an empty pane.
- **Nothing is written to the workspace for this.** There is no claim file any more — the binding
  rides the observation the client already mirrors, so a stale one cannot exist because nothing
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
| `ONLYNE_SOCKET` | no | overrides the socket path (default `<cwd>/.onlyne/run/s`) |
| `ORCA_PANE_KEY` | no | where this process runs (`<tab_id>:<leaf_id>`), reported on every heartbeat as `observed.host.orca.pane_key`; unset outside an Orca pane, which is why the field is then absent |
| `ORCA_TAB_ID` / `ORCA_LEAF_ID` | no | the pane ids separately; the pane key is parsed when only the key itself is set |
| `ORCA_TERMINAL_HANDLE` | no | the terminal handle, reported beside the pane key as `host.orca.handle`, and the value `orca terminal switch` takes |

Constants worth knowing: heartbeat every 10 s (`heartbeat_timeout_ms` is 30 s), 5 s hello
budget, 30 s request timeout, reconnect ladder 1/2/4/8/16/30 s.

## 7. Troubleshooting

| symptom | cause | check |
| --- | --- | --- |
| `[pi-onlyne] session …` never appears | one of the three env vars is missing, or `enabled` is false | `env \| grep ONLYNE_`; `cat .pi/onlyne.json` |
| `socket error: connect ENOENT …/.onlyne/run/s` | no `onlyne-client run` for this workspace | start the client, or `onlyne-client status` |
| `reconnecting in 4000ms` in a loop | the client is down or the socket was replaced | `onlyne --server-root … roles` |
| `ready refused: internal: unknown session for …` | the plugin mounted and reported for a task the client never staged (normal when pi is started by hand outside a task) | start pi under the client, not by hand |
| `assign` never arrives | the client's `session_command` did not spawn pi, or `inject` was dropped | the client log for the spawn line; `/onlyne status` for the capability set |
| ledger stays `in_flight` | no completion was reported: no turn ran, or `agent_settled` never fired | the pi session file for `onlyne-assign` / `onlyne-complete` entries |
| `hello … forbidden` / connection closed right after `hello` | the mount role does not match the client's role | `hello.args.mount.role` vs the workspace's role |
| `frame_too_large` | a body above 8 MiB | only reachable through an oversize outbound image; the ceiling is the core's |
| tools missing | `pi.registerTool` is absent in that pi version | `/onlyne status`; the capability table above |
| session reads `idle` again after `exited` | a heartbeat snapshot landed after the completion, carrying `outcome: pending` | the session log for the report order after `completion`; the plugin stops reporting for a completed task |
| the supervisor board lists no tabs | no live session reported a pane: the adapter predates the report, or this pi is not inside an Orca pane | `onlyne --server-root … sessions --json` for `projection.observed.host.orca.pane_key`; `env \| grep ORCA_` inside the pane |

`/onlyne status` prints the live state (`connected`, `socket`, `role`, `sessionId`,
`generation`, `agentState`, `tasks`, `pendingCompletion`, `lastError`, counters), and
`/onlyne connect` / `/onlyne disconnect` open and close the socket by hand.

## 8. Development

```bash
cd integrations/pi-onlyne
node --test src/*.test.mjs        # framing, protocol, agent state machine, config
```

`src/agent.live.test.mjs` skips itself unless `target/debug/onlyne-client` and
`onlyne-server` exist. `crates/onlyne-testkit/e2e/pi-live.sh` is the end-to-end case: it
skips (exit 0) when pi is absent or has no working model credentials, and otherwise runs
one real task through a real client to `acked`.

```bash
cd ../..
ONLYNE_BACKEND=fake BIN_DIR=target/debug bash crates/onlyne-testkit/e2e/pi-live.sh
```

The case exports `ONLYNE_BACKEND=exec` after sourcing the shared helpers, so the client
spawns pi itself with a stdin pipe it keeps open for the life of the session. The
agent's own output lands in `<ws>/.onlyne/logs/session-<task>.log`.
