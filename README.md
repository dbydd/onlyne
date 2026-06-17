# Onlyne

Onlyne is a small Rust daemon for local IM channel brokering. It gives local agents a thin, workspace-local way to send, receive, subscribe to, and browse messages across chat platforms.

中文说明见 [README.zh-CN.md](README.zh-CN.md).

## What it is

- Workspace-local: each working directory owns its own `.onlyne/` config, state, socket, logs, and cache.
- CLI-first: run it in the foreground, wrap it with a supervisor, or use stdio mode from another process.
- Local IPC: newline-delimited JSON over Unix socket or stdio.
- Multi-channel: Telegram, Feishu/Lark, QQ Bot, and WeChat ilink adapters.
- Lightweight history: local SQLite state and message history.
- Event stream: local clients can subscribe to inbound/outbound and adapter events.

Onlyne is not an agent runtime, model runner, scheduler, web dashboard, or prompt/memory system.

## Install

```bash
cargo build --release
```

Use the built binary at `target/release/onlyne`, or run from source with `cargo run --`.

## Quick start

```bash
onlyne init
onlyne run
```

In another terminal from the same workspace:

```bash
onlyne client '{"id":"1","op":"ping"}'
onlyne client '{"id":"2","op":"status"}'
```

Stdio mode uses the same request schema:

```bash
echo '{"id":"1","op":"ping"}' | onlyne stdio
```

## Workspace layout

`onlyne init` creates runtime files under the current directory:

```text
.onlyne/
  config.toml
  .env
  state.db
  run/onlyne.sock
  logs/daemon.log
  cache/media/
  adapters/
```

Workspace data intentionally does not default to global mutable state.

## Channels

| Channel | Setup |
| --- | --- |
| Telegram | Put `TELEGRAM_BOT_TOKEN` in `.onlyne/.env` and enable `[adapters.telegram]`. |
| Feishu/Lark | Run `onlyne auth feishu`, or bind with `--app-id` and `--app-secret`. |
| QQ Bot | Put `QQBOT_APP_ID` and `QQBOT_APP_SECRET` in `.onlyne/.env` and enable `[adapters.qqbot]`. |
| WeChat ilink | Run `onlyne auth weixin`, or bind with `--token`. |

Auth commands write only to the current workspace `.onlyne/` directory.

## Common commands

```bash
onlyne init
onlyne run [--debug]
onlyne stdio
onlyne client '<json-request>'
onlyne config-check
onlyne auth feishu [--app-id <id> --app-secret <secret>]
onlyne auth weixin [--token <token>]
onlyne shell-completions zsh
onlyne shell-completions fish
```

`onlyne run --debug` replies to inbound messages with redacted channel/conversation/thread metadata. Use it only while finding conversation IDs or platform thread fields.

## Examples

- `examples/telegram/`
- `examples/feishu/`
- `examples/qqbot/`
- `examples/wechat/`
- `examples/broadcast/`
- `examples/multicast/`
- `examples/multi-channel/`

The examples are pure CLI workflows. They keep secrets and runtime data in each example workspace's `.onlyne/`, which is ignored by git.

## IPC

Onlyne accepts newline-delimited JSON requests. See [docs/IPC.md](docs/IPC.md) for operation details.

Minimal request:

```json
{"id":"1","op":"ping"}
```

Minimal response:

```json
{"id":"1","ok":true,"data":{"pong":true}}
```

## Project status

See [docs/STATUS.md](docs/STATUS.md) for current implementation notes and verification evidence.
