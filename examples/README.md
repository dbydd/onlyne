# Onlyne Cargo examples

Examples are runnable with `cargo run --example <name>`. By default they use the workspace under `examples/.onlyne/`, matching `cargo run -- --workspace examples ...`.

## Common flow

```bash
cargo build
cargo run -- --workspace examples init
cargo run -- --workspace examples auth feishu
# or: cargo run -- --workspace examples auth qqbot --app-id '<app-id>' --app-secret '<app-secret>'
cargo run -- --workspace examples run
```

In another terminal:

```bash
cargo run --example feishu
cargo run --example rich_media
```

## Common variables

| Variable | Meaning |
| --- | --- |
| `ONLYNE_SOCKET` | Explicit Unix socket path. If unset, examples use `examples/.onlyne/run/onlyne.sock`, then nearest parent `.onlyne`. |
| `ONLYNE_TEXT` | Outbound text. Defaults to `zig`, except `rich_media` defaults to markdown content. |
| `ONLYNE_FORMAT` | `plain` or `markdown`. Defaults to `plain`, except `rich_media` defaults to `markdown`. |
| `ONLYNE_ATTACHMENTS` | JSON array of attachment refs. Defaults to `[]`. |
| `ONLYNE_TARGETS` | Optional `channel[,channel...]`; if unset, examples read stored channels from `examples/.onlyne/state.db` through the daemon. |
| `ONLYNE_<CHANNEL>_CONVERSATION_ID` | Known target conversation id for channel smoke scripts; scripts write it to `bind_conversation_id` and may fall back to stored history. |

Channel variable names: `ONLYNE_TELEGRAM_CONVERSATION_ID`, `ONLYNE_FEISHU_CONVERSATION_ID`, `ONLYNE_QQBOT_CONVERSATION_ID`, `ONLYNE_WECHAT_CONVERSATION_ID`.

## Examples

| Cargo example | Purpose |
| --- | --- |
| `telegram` | Send one Telegram text message. |
| `feishu` | Send one Feishu/Lark text message. |
| `qqbot` | Send one QQ Bot text message. |
| `wechat` | Send one WeChat text message. |
| `broadcast` | Send one text to many configured channels. |
| `multicast` | Alias-style many-channel sender. |
| `multi_channel` | List channels, send to explicit targets, read merged history. |
| `fifo/smoke-fifo-all-qq.sh` | Write all outbound channels through `.onlyne/channels/<channel>/in`, then read QQ inbound from `.onlyne/channels/qqbot/out`. |
| `rich_media` | Send `format=markdown` messages and optional attachments. |

Examples require the daemon to already be running; they do not spawn or supervise Onlyne. Telegram/WeChat Markdown tables are split out and rendered to PNG with Rust `resvg`; Feishu keeps tables inside the interactive card.
