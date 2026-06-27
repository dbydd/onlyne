# Onlyne Status

This file tracks implementation and verification notes. The root README is the product manual.

## Implemented locally

- Workspace-local `.onlyne/` bootstrap.
- TOML config plus root `.env` / `.onlyne/.env` secret lookup.
- Foreground daemon with Unix socket.
- Stdio mode using the same NDJSON schema.
- SQLite history store.
- Event subscription over local IPC.
- Per-channel FIFO IO under `.onlyne/channels/<channel>/`, including configurable Markdown/raw-text input.
- Loopback activation messages over local IPC.
- Singleton channel routing via per-adapter `bind_conversation_id`.
- Workspace-local agent skill export via `onlyne export-skill`.
- Feishu/Lark and WeChat auth helpers.
- Zsh/fish completion generation.
- CLI examples for single-channel, broadcast, multicast, and multi-channel workflows.

## Adapter notes

| Platform | Implemented | Needs live validation |
| --- | --- | --- |
| Telegram | Bot token, `getUpdates`, send text/media, media download. | Real bot polling and send. |
| Feishu/Lark | QR/app credential auth, tenant token, websocket receive, OpenAPI send. | Tenant QR completion, permissions, websocket in target tenant. |
| QQ Bot | App access token, gateway websocket, group text/media send. | Real gateway session and long-run token refresh behavior. |
| WeChat ilink | QR/token auth, long-poll receive, context-token send, CDN media helpers. | Live CDN edge cases and expired context-token recovery. |

## Latest local checks

- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test` currently covers 49 tests.
- `cd harness/pi-onlyne && npm run check`
- Example scripts support `--local-check` where applicable.
- `onlyne shell-completions zsh` and `onlyne shell-completions fish` generate completion scripts.

Live platform smoke is intentionally manual because it requires real credentials and may send external messages.

## Channel FIFO IO notes

See [CHANNEL_IO.md](CHANNEL_IO.md) for the design.

Onlyne exposes per-channel file-descriptor style IO under `.onlyne/channels/<channel>/`:

- `in`: write side, so local scripts can `echo 'message' > .onlyne/channels/telegram/in` and send through that channel's bound conversation.
- `out`: read side, so local scripts can `cat .onlyne/channels/telegram/out`.
- `out` read behavior must be configurable as the 2x2 combination of:
  - content mode: latest new message with conversation history context, or latest message only.
  - cursor mode: retain after read, or consume/advance after read.
- File IO cursors must synchronize with other local consumers. Example: if pi-onlyne has already surfaced an inbound notification, then a consume-mode `out` for the same channel must not later include that already-consumed new message.
- Include a `loopback` channel directory with the same `in`/`out` behavior.
- Keep paths stable enough for symlinks.
- Preserve singleton channel routing: channel name selects the adapter; conversation is still the configured or `/handshake`-bound `bind_conversation_id`.
