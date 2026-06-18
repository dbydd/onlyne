# Onlyne

Onlyne is a small Rust daemon for local IM channel brokering. It gives local agents a thin, workspace-local way to send, receive, subscribe to, and browse messages across chat platforms.

中文说明见 [README.zh-CN.md](README.zh-CN.md).

## What it is

- Workspace-local: the active workspace is the nearest parent directory with `.onlyne/`; each workspace owns its own config, state, socket, logs, and cache.
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
# Optional: export an agent skill into this workspace
onlyne init --export-skill
onlyne run
```

In another terminal under the same workspace tree:

```bash
onlyne client '{"id":"1","op":"ping"}'
onlyne client '{"id":"2","op":"status"}'
```

Stdio mode uses the same request schema:

```bash
echo '{"id":"1","op":"ping"}' | onlyne stdio
```

## Workspace layout

By default, commands start at the current directory and walk upward until they find the nearest `.onlyne/`. If no `.onlyne/` exists, the current directory is used, so `onlyne init` initializes the directory where it is run. Use `--workspace <dir>` to explicitly choose a workspace root.

`onlyne init` creates runtime files under the selected workspace root:

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

`onlyne init --export-skill` also writes a local agent skill to `.agents/skills/onlyne/SKILL.md` under the selected workspace root. This is intentionally workspace-local and does not touch `~/.agents/skills`.

## Channels

| Channel | Setup |
| --- | --- |
| Telegram | Put `TELEGRAM_BOT_TOKEN` in `.onlyne/.env` and enable `[adapters.telegram]`. |
| Feishu/Lark | Run `onlyne auth feishu`, or bind with `--app-id` and `--app-secret`. |
| QQ Bot | Put `QQBOT_APP_ID` and `QQBOT_APP_SECRET` in `.onlyne/.env` and enable `[adapters.qqbot]`. |
| WeChat ilink | Run `onlyne auth weixin`, or bind with `--token`. |

Auth commands write only to the selected workspace `.onlyne/` directory.

Adapter SDKs: Feishu uses `openlark`, Telegram uses `teloxide`, and WeChat ilink uses `wechat-ilink`. QQ Bot stays on a small direct API/gateway adapter because current Rust community crates are less mature for this project path.

## Common commands

```bash
onlyne [--workspace <dir>] init [--export-skill]
onlyne [--workspace <dir>] run [--debug]
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

The examples are pure CLI workflows. Run `onlyne init` in `examples/` to share one ignored `examples/.onlyne/` workspace across child examples, or pass `--workspace <dir>` / `ONLYNE_WORKSPACE` for isolation.

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
