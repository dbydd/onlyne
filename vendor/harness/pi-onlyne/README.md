# pi-onlyne

`pi-onlyne` gives [Pi](https://github.com/badlogic/pi-mono) agents a local IM inbox and outbox through [Onlyne](https://github.com/dbydd/onlyne). The extension connects Pi to an Onlyne workspace, exposes message tools, and delivers subscribed events as Pi follow-ups.

## Runtime requirements

- Node.js 20 or newer
- Pi 0.84 or newer with the `pi` command available in `PATH`
- `onlyne` 0.4.x installed with `cargo install onlyne`, or a compatible local build
- An initialized Onlyne workspace with `.onlyne/config.toml`
- A configured model/provider for Pi agent replies
- Unix domain socket support on the host

The extension supports macOS and Linux. Each workspace keeps daemon state, channel credentials, history, sockets, and logs under its own `.onlyne/` directory.

## Install

Install the published Pi package:

```bash
pi install npm:pi-onlyne
```

Run it for one Pi process:

```bash
pi -e npm:pi-onlyne
```

Install the package from a local checkout during development:

```bash
cd path/to/pi-onlyne
npm install
npm run check
pi install .
```

The package publishes `dist/`, `README.md`, `SPEC.md`, and `LICENSE`. `prepublishOnly` runs the build and test suite.

## Prepare an Onlyne workspace

Run these commands from the project that should receive the messages:

```bash
cargo install onlyne
onlyne init
onlyne export-skill
```

Configure a channel in `.onlyne/config.toml` and place secrets in `.onlyne/.env`. Examples:

```toml
[adapters.telegram]
enabled = true

[adapters.feishu]
enabled = true

[adapters.qqbot]
enabled = true

[adapters.wechat]
enabled = true
```

Use the matching `onlyne auth` command for Feishu, QQ Bot, or WeChat. Telegram uses `TELEGRAM_BOT_TOKEN` in `.onlyne/.env`. Bind a target conversation with `bind_conversation_id`, or send `/handshake` from the desired conversation after the adapter starts.

Start the daemon from the project root:

```bash
onlyne run
```

A Pi session can start or connect to the daemon through `/onlyne daemon start`.

## Configure Pi behavior

The extension reads `.pi/onlyne.json` from the current Pi project. The default configuration is:

```json
{
  "watch": { "autoStart": false },
  "inbound": { "defaultMode": "auto-handle", "rules": [] },
  "outbound": {
    "defaultReplyMode": "guarded-explicit",
    "guardedExplicit": {
      "reminders": 2,
      "noOutputFallbackText": "Onlyne/Pi error: no valid reply was produced."
    },
    "retry": { "attempts": 2, "concurrency": 8 }
  }
}
```

Enable automatic subscription when Pi starts:

```json
{
  "watch": { "autoStart": true }
}
```

The extension merges partial JSON with the defaults. `inbound.rules` accepts channel and optional conversation selectors with `auto-handle`, `queue-only`, or `muted` modes. `outbound.defaultReplyMode` accepts `guarded-explicit`, `explicit-only`, or `implicit-final`.

## Commands

```text
/onlyne status
/onlyne daemon start
/onlyne daemon stop
/onlyne daemon restart
/onlyne watch on
/onlyne watch off
/onlyne config auto-start
/onlyne swarm on
/onlyne swarm off
/onlyne swarm status
```

`watch on` subscribes to the current workspace event stream. Incoming channel messages become Pi follow-ups. A normal inbound message receives `onlyne_reply`, and an intentional omission receives `onlyne_mark_no_reply`.

## Agent tools

Normal mode:

```text
onlyne_daemon_start()
onlyne_daemon_stop()
onlyne_daemon_restart()
onlyne_reply({ text })
onlyne_send({ channelId, text, rawText? })
onlyne_broadcast({ targets, text, rawText? })
onlyne_loopback({ text, rawText? })
onlyne_mark_no_reply({ reason? })
```

Swarm mode (`[swarm] enabled`):

```text
onlyne_daemon_start()
onlyne_daemon_stop()
onlyne_daemon_restart()
swarm_complete({ text })
swarm_quit({ reason? })
swarm_send({ to, text })
swarm_status()
```

One session sees one toolset, chosen at session start. Generic send/reply
tools stay out of the swarm surface so unheaded writes cannot pollute the
protocol.

Reclaim uses a control wire on the same loopback path: the scheduler writes a
header-only `---swarm-ctl` message (`op: recycle`), pi-onlyne intercepts it
before delivery, acks `swarm_recycled { task_id, reason }`, stops watching,
clears the slot, and exits its own process. The scheduler then closes the
Orca tab. Missing acks are logged and the tab still closes.

Messages use Markdown by default. `rawText: true` preserves literal text for scripts and protocol payloads.

### Send one message

```ts
onlyne_send({
  channelId: "telegram",
  text: "# Build report\n\nAll checks passed."
})
```

### Broadcast

```ts
onlyne_broadcast({
  targets: [{ channelId: "telegram" }, { channelId: "feishu" }],
  text: "# Release shipped\n\nVersion 0.6.0 is live."
})
```

### Loopback wake-up

A local script can wake the current Pi session through the daemon socket:

```bash
onlyne client '{"id":"wake","op":"loopback","text":"background job finished","raw_text":true}'
```

The extension also supports `.onlyne/channels/loopback/in` when FIFO IO is enabled.

## Swarm mode

Swarm mode lets `onlyne-swarm >= 0.5.0` own task routing for a generated agent workspace. `pi-onlyne 0.8.1` uses the scheduler's `delivery: scheduler` header for task claims, so these versions upgrade together. Enable it in `.onlyne/config.toml`:

```toml
[swarm]
enabled = true
```

A swarm Pi session subscribes to loopback events, reports `swarm_ready`, accepts one scheduler-delivered hop atomically, spawns continuations with `swarm_send` (fire-and-forget), and writes `swarm_complete` (done signal). A raw `swarm_send` relay carries no `delivery` header and only creates the scheduler task row; the scheduler's second delivery adds `delivery: scheduler`, which pi-onlyne uses as the claim gate. This keeps arbitrary role prose out of transport authentication. Scheduler then sends a header-only recycle control wire; pi-onlyne intercepts it before model delivery, sends `swarm_recycled`, stops the watch, clears the slot, and self-exits. `swarm_quit` sends the same ack with `quit:<reason>` then self-exits, so the scheduler records failed immediately. Downstream results travel through files and the ledger; nothing waits.

For automatic startup in a generated workspace, add `.pi/onlyne.json`:

```json
{
  "watch": { "autoStart": true },
  "outbound": {
    "defaultReplyMode": "explicit-only",
    "retry": { "attempts": 4, "concurrency": 8 }
  }
}
```

The swarm scheduler starts Pi with normal extension discovery. The configured retry extension remains available in swarm sessions. See the [onlyne-swarm README](https://github.com/dbydd/onlyne-swarm) for the graph template, scheduler commands, Orca requirements, and test runner.

## Local state and security

Pi-side settings live at `.pi/onlyne.json`. Onlyne stores credentials, history, sockets, logs, and adapter state under `.onlyne/`. Keep `.onlyne/.env` private. Review package source before installing third-party extensions because Pi extensions run with the permissions of the Pi process.

## Release notes

The checkout in this repository tracks the published version in `package.json`.
Run `npm run check` before any release so the build and tests regenerate
`dist/`. Publish with `npm publish` after reviewing the generated tarball.

## Development

```bash
npm install
npm run check
npm pack --dry-run
```

`npm run check` compiles TypeScript and runs the Node test suite. The tests cover configuration, workspace discovery, daemon connection, swarm header parsing, and the atomic swarm task slot.

## Links

- Onlyne: https://github.com/dbydd/onlyne
- Onlyne documentation: https://github.com/dbydd/onlyne/tree/dev/docs
- npm package: https://www.npmjs.com/package/pi-onlyne
- pi-onlyne source: https://github.com/dbydd/pi-onlyne
- onlyne-swarm: https://github.com/dbydd/onlyne-swarm

## License

MIT
