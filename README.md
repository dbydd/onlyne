# Onlyne

Rust workspace-local IM channel daemon / broker.

Onlyne stores runtime data under the current workspace's `.onlyne/` and exposes local agent-facing IPC over Unix socket or stdio using newline-delimited JSON.

Agent runtimes, web dashboards, schedulers, supervisor installers, and model/provider orchestration are intentionally out of scope.

## Commands

```bash
onlyne init
onlyne config-check
onlyne auth feishu
onlyne auth weixin
onlyne run
onlyne stdio

echo '{"id":"1","op":"ping"}' | onlyne stdio
onlyne client '{"id":"1","op":"status"}'
```

## Local status

Verified locally:

- workspace-local `.onlyne/` bootstrap:
  - `.onlyne/config.toml`
  - `.onlyne/.env`
  - `.onlyne/run/`
  - `.onlyne/logs/`
  - `.onlyne/cache/media/`
  - `.onlyne/adapters/`
- TOML config loading plus root `.env` and `.onlyne/.env` secret lookup
- workspace-local `auth` command for Feishu/Lark and WeChat ilink login/bind
- foreground daemon with Unix socket at `.onlyne/run/onlyne.sock`
- stdio mode with the same NDJSON request/response schema
- SQLite history store via `rusqlite`
- local event bus and IPC subscribe/unsubscribe operations
- malformed JSON and unknown-operation error responses

Implemented IPC operations:

- `ping`
- `status`
- `list_channels`
- `list_conversations`
- `subscribe_events`
- `unsubscribe_events`
- `send_message`
- `reply_message`
- `fetch_history`
- `fetch_channel_history`
- `fetch_all_history`
- `start_adapter`
- `stop_adapter`
- `restart_adapter`

Adapter lifecycle note: in this build, adapters start with `onlyne run` / `onlyne stdio`. Runtime `start_adapter`, `stop_adapter`, and `restart_adapter` are compatibility operations; they do not hot-start or hot-restart adapters.

## Platform adapter status

These paths are implemented in code but require live platform credentials and reachable vendor endpoints for real smoke tests.

| Platform | Auth / pairing shape | Implemented path | Not locally verified |
| --- | --- | --- | --- |
| Telegram | Bot token via `TELEGRAM_BOT_TOKEN` or config; no QR pairing. | `getUpdates` receive, send text/media, download inbound media. | Real bot polling and send against Telegram. |
| Feishu / Lark | `onlyne auth feishu` QR onboarding, or `--app-id` + `--app-secret` bind. | Saves app credentials to workspace `.onlyne`, tenant token, websocket receive path, OpenAPI text/media send. | Real tenant QR completion in this environment, real websocket connection, webhook/url-verification mode, tenant permissions. |
| QQ Bot | `QQBOT_APP_ID` + `QQBOT_APP_SECRET`, optional sandbox. | Access token, gateway websocket, group text/media send. | Real gateway session, token expiry/401 retry behavior under long runs. |
| WeChat ilink | `onlyne auth weixin` QR login, or `--token` bind. Sending needs a fresh per-peer `context_token` learned from inbound messages. | Saves ilink token to workspace `.onlyne`, long-poll receive, context-token text/media send, encrypted CDN media download/upload helpers. | Live WeChat QR completion in this environment, live CDN reachability, expired context-token recovery. |

Auth command examples:

```bash
# QR onboarding; writes only to cwd/.onlyne/
onlyne auth feishu
onlyne auth weixin

# Bind known credentials instead of scanning QR
onlyne auth feishu --app-id cli_xxx --app-secret sec_xxx
onlyne auth weixin --token eyJ...

# WeChat operator overrides, when needed
onlyne auth weixin --api-url https://ilinkai.weixin.qq.com --bot-type 3
```

Auth is intentionally workspace-local: it updates `.onlyne/config.toml` and `.onlyne/.env` in the current directory only.

WeChat CLI smoke example: see `examples/wechat/`.

## Smoke checks

Run from the repo after building:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Minimal workspace smoke:

```bash
tmp=$(mktemp -d)
cd "$tmp"
/path/to/onlyne init
/path/to/onlyne stdio <<'JSON'
{"id":"1","op":"ping"}
{"id":"2","op":"status"}
{"id":"3","op":"list_channels"}
{"id":"4","op":"fetch_all_history","limit":5}
{"id":"5","op":"not_real"}
{
JSON
```

Unix socket smoke:

```bash
/path/to/onlyne run &
pid=$!
/path/to/onlyne client '{"id":"1","op":"ping"}'
/path/to/onlyne client '{"id":"2","op":"status"}'
kill "$pid"
```

## Current verification evidence

Last local verification in this workspace:

- `cargo test`: 13 tests passed.
- `cargo fmt --check`: passed.
- `cargo clippy --all-targets -- -D warnings`: passed.
- Temporary-workspace `onlyne init`: created the expected `.onlyne/` layout.
- `onlyne stdio`: verified `ping`, `status`, `list_channels`, `fetch_all_history`, event subscription warning delivery, unknown op, and malformed JSON responses.
- `onlyne auth --help`: verified Feishu/Weixin auth command surface.
- `onlyne run` + `onlyne client`: verified Unix socket `ping` and `status`.

No live platform credential or QR-completion smoke was run.
# onlyne
