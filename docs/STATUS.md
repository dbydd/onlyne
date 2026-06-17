# Onlyne Status

This file tracks implementation and verification notes. The root README is the product manual.

## Implemented locally

- Workspace-local `.onlyne/` bootstrap.
- TOML config plus root `.env` / `.onlyne/.env` secret lookup.
- Foreground daemon with Unix socket.
- Stdio mode using the same NDJSON schema.
- SQLite history store.
- Event subscription over local IPC.
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
- `cargo test` currently covers 19 tests.
- Example scripts support `--local-check` where applicable.
- `onlyne shell-completions zsh` and `onlyne shell-completions fish` generate completion scripts.

Live platform smoke is intentionally manual because it requires real credentials and may send external messages.
