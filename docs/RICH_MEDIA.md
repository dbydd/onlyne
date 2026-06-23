# Rich media and Markdown

Onlyne keeps the agent-facing API simple: callers still send one `send_message` request with `text`, `format`, and optional `attachments`.

```json
{
  "op": "send_message",
  "channel_id": "telegram",
  "conversation_id": "...",
  "text": "# Report\n\n| A | B |\n| --- | --- |\n| 1 | 2 |",
  "format": "markdown",
  "attachments": []
}
```

## External API contract

- `format` defaults to `plain`.
- `format=markdown` means `text` is the whole Markdown document.
- Attachment inputs stay `path` or `url` only.
- If one request fans out into multiple platform messages, history still stores one logical outbound message with `platform_metadata.delivery_parts`.

Agents do not pre-split Markdown. They keep writing a whole Markdown string to the socket/CLI/harness.

## Current per-channel behavior

| Channel | Markdown behavior |
| --- | --- |
| QQ Bot | Sends the whole Markdown document directly as `msg_type=2` / `markdown.content`. QQ's extended Markdown handles tables and formulas. |
| Telegram | Converts supported Markdown to Telegram HTML. Tables are split and sent as rendered PNG image parts. |
| Feishu/Lark | Converts supported Markdown to an interactive card. Tables are split and sent as rendered PNG image parts. |
| Weixin/WeChat | Sends readable text segments. Tables are split and sent as rendered PNG image parts. |

Unsupported rendering should be fixed in the adapter that mishandles it. There is no app-level raw-Markdown fallback pipeline.

## Table rendering

For non-QQ adapters, Markdown tables are rendered in-process:

- parser/splitter: small line-based GFM table detector in `markdown::split_tables`
- renderer: Rust `resvg` + system fonts
- output: `.onlyne/cache/rendered/{hash}.png`

No ImageMagick, browser, `qlmanage`, renderer subprocess, timeout, or rendered-image size knob.

## Delivery metadata

Typical multipart logical message:

```json
{
  "format": "markdown",
  "segmented": true,
  "delivery_parts": [
    {"kind": "markdown_text", "message_id": "..."},
    {"kind": "rendered_table", "message_id": "..."},
    {"kind": "markdown_text", "message_id": "..."}
  ]
}
```

## Verification

```bash
cargo fmt
cargo check --examples
cargo test
cargo run -- --workspace examples run
cargo run --example rich_media
ONLYNE_TARGETS='qqbot:<conversation>' cargo run --example rich_media
```
