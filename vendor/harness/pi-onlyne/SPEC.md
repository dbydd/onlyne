# pi-onlyne SPEC

## Scope

Pi extension for Onlyne. Onlyne remains a workspace-local IM broker; this extension owns Pi session lifecycle, watch behavior, message injection, send tools, and a small config surface.

## v1 Decisions

- Watch is configurable; default manual.
- `/onlyne` provides argument completions for its supported subcommands, including daemon lifecycle commands.
- `watch on` connects to the workspace-local `.onlyne/run/s`; if unavailable, it starts a Pi-owned workspace daemon.
- `/onlyne daemon start|stop|restart` is the preferred lifecycle surface. Agents must not use ad-hoc `nohup onlyne run`, `pkill -f 'onlyne run'`, or global launchd/systemd jobs when pi-onlyne owns the daemon.
- Inbound events come from Onlyne `subscribe_events`; no polling.
- Inbound mode is rule-based: `auto-handle`, `queue-only`, or `muted`.
- Outbound defaults to `guarded-explicit`: prefer tool reply, fallback to final text, else send configured error text.
- Send tools default to Markdown and may pass `raw_text: true` to Onlyne for literal text.
- Broadcast sends concurrently with per-target retry and per-target results.
- Loopback inbound messages wake Pi without creating a reply obligation.
- `/handshake` inbound messages are Onlyne control messages and must not be surfaced to Pi as agent work.
- After pi surfaces an inbound follow-up, it calls `mark_io_consumed` so Onlyne FIFO `out_cursor = "consume"` stays synchronized with pi notifications.
- FIFO IO itself remains owned by the Onlyne daemon; pi-onlyne does not open `.onlyne/channels/*/in|out` directly.

## Config

Stored in project `.pi/onlyne.json`:

```json
{
  "watch": { "autoStart": false },
  "inbound": { "defaultMode": "auto-handle", "rules": [] },
  "outbound": {
    "defaultReplyMode": "guarded-explicit",
    "guardedExplicit": { "reminders": 2, "noOutputFallbackText": "Onlyne/Pi error: no valid reply was produced." },
    "retry": { "attempts": 2, "concurrency": 8 }
  },
  "swarm_prompt": { "template": "prompts/swarm-task.md" }
}
```

- `swarm_prompt` externalizes the swarm task injection wrapper
  (`src/swarm-prompt.ts`). `template` is a workspace-relative path whose
  content is injected as the followUp message; placeholders `{task_id}`,
  `{from}`, `{transfer_send_to}`, `{attempt}`, `{payload}` are substituted.
  `false` disables the wrapper (raw payload injected). Omitting the key
  keeps the built-in default text. Operators with relay-only nodes
  (no history worth restoring) should ship a template that names the role
  as the complete instruction set and warns that `onlyne_in/` and
  `.onlyne/` are OS pipes (opening one freezes the session).

## Tools

Normal mode (default):

- `onlyne_reply({ text })`
- `onlyne_send({ channelId, text, rawText? })`
- `onlyne_broadcast({ targets, text, rawText? })`
- `onlyne_loopback({ text, rawText? })`
- `onlyne_mark_no_reply({ reason? })`
- `onlyne_daemon_start/stop/restart`

Swarm mode (`[swarm] enabled`, see below):

- `swarm_complete({ text })`
- `swarm_quit({ reason? })`
- `swarm_send({ to, text })`
- `swarm_status()`
- `onlyne_daemon_start/stop/restart`

One session sees one toolset. The surface is chosen at `session_start` from
`[swarm]` and applied with `setActiveTools` when the Pi API exists.

## Deferred

- Attachments.
- Auth QR/secret editing TUI.
- Schedules.
- Target groups.

## Swarm mode (v2, amendment-1)

- Switch: `/onlyne swarm on|off|status`. On/off persists to the workspace
  `.onlyne/config.toml` `[swarm] enabled` flag and restarts watch. Status line
  and `session_start` banner report `swarm` vs `ready`.
- When swarm is on, generic in/out auto-handling is disabled: the scheduler owns
  input/output. Only loopback messages carrying a `---swarm` body header enter
  the session, via the `followUp` task queue.
- Session model: one session carries exactly one scheduler-delivered hop. A
  `delivery: scheduler` header field claims the slot; raw `swarm_send` relay
  wires and out wires omit it and never claim. Any further delivered header
  while claimed is ignored (the scheduler always opens a new session, so this
  guard never fires on the normal path). No waiting, no callbacks, no parent
  bookkeeping.
- Startup handshake: swarm watch sends the `swarm_ready` op
  (`{workspace, terminal_handle}`) so the scheduler can match a pending task.
  `ONLYNE_SWARM_TASK` env and `ORCA_TERMINAL_HANDLE`/`ONLYNE_TERMINAL_HANDLE`
  provide fallback correlation.
- Tools: `swarm_complete({text})` writes the out message carrying this hop's
  header (the scheduler's done signal). Scheduler sends a `---swarm-ctl`
  recycle wire; pi-onlyne intercepts it, sends `swarm_recycled`, stops the
  watch, clears the slot, and exits its own process. `swarm_quit({reason?})`
  sends `swarm_recycled` with `quit:<reason>` then self-exits; scheduler marks
  the hop failed with no retry. The second empty-exit guard runs this same path.
- `swarm_send({to, text})` spawns a downstream task with
  `transfer_send_to` set to the current task and returns the child id without
  waiting. `swarm_status()` reports the current task and spawned ids. Daemon
  lifecycle tools stay available in both modes.
- Generic send/reply tools are not registered in the swarm surface: unheaded
  or misheaded writes would pollute the protocol. All swarm IO goes through
  the `swarm_*` tools, whose headers are constructed inside the plugin
  (`renderSwarmHeader`).
- Body protocol lives in `src/swarm.ts` (`parseSwarmHeader`,
  `renderSwarmHeader`, `readSwarmEnabled`); covered by `test/swarm.test.mjs`.
  Old `reply_to` headers parse as ordinary (non-swarm) messages.
