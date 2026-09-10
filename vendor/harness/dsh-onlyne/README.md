# dsh-onlyne

English | [中文](README.zh.md)

**Give DeepSeek Harness agents a real IM inbox/outbox through [Onlyne](https://github.com/dbydd/onlyne).**

`dsh-onlyne` is the [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) (`dsh`) plugin for Onlyne, the workspace-local IM channel daemon. It adds model-facing tools and a watch loop so a dsh agent can receive messages from IM channels (Telegram, Feishu/Lark, QQ Bot, WeChat) and send replies — without pretending a chat platform is a terminal or a workflow engine.

It is the dsh counterpart of [pi-onlyne](https://github.com/dbydd/pi-onlyne) and shares the same per-project config file (`.pi/onlyne.json`), so one workspace can be bridged by either harness without reconfiguration.

## Install

Onlyne is a single binary; see [the Onlyne README](https://github.com/dbydd/onlyne) to install and initialize a workspace:

```bash
onlyne init
```

Then install the plugin into a dsh profile and mount it:

```bash
dsh plugin --profile web add dsh-onlyne
```

Add a row to the profile's `cordis.patch.yml` (`$DSH_HOME/profiles/<name>/cordis.patch.yml`):

```yaml
- insert:
    - id: onlyne
      name: dsh-onlyne
```

The plugin finds the workspace by walking up from the dsh process's invoking directory to the nearest `.onlyne/`.

## Tools

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

### Send one message

```
onlyne_send({
  channelId: "telegram",
  text: "# Build report\n\nAll checks passed."
})
```

Set `rawText: true` to send literal text instead of Markdown.

### Broadcast

```
onlyne_broadcast({
  targets: [{ channelId: "telegram" }, { channelId: "feishu" }],
  text: "# Release shipped"
})
```

### Loopback wake-up

From any local script, inject an inbound message into the running daemon:

```bash
onlyne client '{"id":"wake","op":"loopback","text":"background job finished","raw_text":true}'
```

Channel `loopback` is wake-up-only: it surfaces a follow-up in the session but does not expect `onlyne_reply`.

## Command

```text
/onlyne status
/onlyne watch on
/onlyne watch off
/onlyne daemon start
/onlyne daemon stop
/onlyne daemon restart
/onlyne config auto-start
```

`watch on` subscribes to the daemon's event stream; inbound messages are surfaced into the current dsh session as user follow-ups, and the agent replies with `onlyne_reply` or acknowledges with `onlyne_mark_no_reply`. Control messages such as `/handshake` are consumed silently. `config auto-start` toggles watching automatically at session start.

## Config

Shared with pi-onlyne at `.pi/onlyne.json`:

```json
{
  "watch": { "autoStart": false },
  "inbound": {
    "defaultMode": "auto-handle",
    "rules": [{ "channel": "telegram", "mode": "queue-only" }]
  },
  "outbound": {
    "defaultReplyMode": "guarded-explicit",
    "retry": { "attempts": 2, "concurrency": 8 }
  }
}
```

## Development

```bash
npm install
npm run check     # build + tests
npm pack          # build the publishable tarball
```

## Links

- Onlyne main repository: https://github.com/dbydd/onlyne
- dsh-onlyne package: https://www.npmjs.com/package/dsh-onlyne
