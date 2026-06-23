# Onlyne Cargo examples

Examples are runnable with `cargo run --example <name>`. They talk to a running workspace-local Onlyne daemon over `.onlyne/run/onlyne.sock`.

## Common flow

```bash
cargo build
cargo run -- init
cargo run -- auth feishu
# or: cargo run -- auth qqbot --app-id '<app-id>' --app-secret '<app-secret>'
cargo run -- run
```

In another terminal:

```bash
ONLYNE_FEISHU_CONVERSATION_ID='oc_xxx' cargo run --example feishu
ONLYNE_TARGETS='feishu:oc_xxx,qqbot:group_openid' cargo run --example rich_media
```

## Common variables

| Variable | Meaning |
| --- | --- |
| `ONLYNE_SOCKET` | Explicit Unix socket path. If unset, examples discover nearest `.onlyne/run/onlyne.sock`. |
| `ONLYNE_TEXT` | Outbound text. Defaults to `zig`, except `rich_media` defaults to markdown content. |
| `ONLYNE_FORMAT` | `plain` or `markdown`. Defaults to `plain`, except `rich_media` defaults to `markdown`. |
| `ONLYNE_ATTACHMENTS` | JSON array of attachment refs. Defaults to `[]`. |
| `ONLYNE_TARGETS` | `channel:conversation[,channel:conversation...]`, used by `broadcast`, `multicast`, `multi_channel`, and `rich_media`. |
| `ONLYNE_<CHANNEL>_CONVERSATION_ID` | Known target conversation id for channel examples. |

Channel variable names: `ONLYNE_TELEGRAM_CONVERSATION_ID`, `ONLYNE_FEISHU_CONVERSATION_ID`, `ONLYNE_QQBOT_CONVERSATION_ID`, `ONLYNE_WECHAT_CONVERSATION_ID`.

## Examples

| Cargo example | Purpose |
| --- | --- |
| `telegram` | Send one Telegram text message. |
| `feishu` | Send one Feishu/Lark text message. |
| `qqbot` | Send one QQ Bot text message. |
| `wechat` | Send one WeChat text message. |
| `broadcast` | Send one text to many conversations. |
| `multicast` | Alias-style many-target sender across channels. |
| `multi_channel` | List channels, send to explicit targets, read merged history. |
| `rich_media` | Send `format=markdown` messages and optional attachments. |

Examples require the daemon to already be running; they do not spawn or supervise Onlyne.
