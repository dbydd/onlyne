# Rich media / Markdown smoke example

Sends one `format=markdown` message to one or more conversations.

This is meant for the first rich-media pass:

- Feishu/Lark: sends an interactive card.
- QQ Bot: sends `msg_type=2` with `markdown.content`.
- If a channel falls back and `[rich_text.renderer]` is enabled, Onlyne sends raw Markdown text first, then the rendered PNG.

## Prepare

```bash
cargo build
cd examples/rich-media
../../target/debug/onlyne init
# configure Feishu/QQ credentials in the selected .onlyne/.env and enable adapters
```

Optional renderer config in `.onlyne/config.toml`:

```toml
[rich_text.renderer]
enabled = true
command = "onlyne-md2png"
args = ["--out", "{output}"]
timeout_seconds = 20
```

The renderer receives Markdown on stdin and writes PNG to `{output}`.

## Run

```bash
# one target
ONLYNE_TARGETS='feishu:oc_xxx' ./smoke-rich-media.sh

# many targets
ONLYNE_TARGETS='feishu:oc_xxx,qqbot:group_openid' ./smoke-rich-media.sh

# custom markdown
ONLYNE_TARGETS='qqbot:group_openid' ONLYNE_TEXT=$'# Title\n\n- item\n- `code`' ./smoke-rich-media.sh

# with attachments
ONLYNE_TARGETS='feishu:oc_xxx' \
ONLYNE_ATTACHMENTS='[{"kind":"file","path":"/tmp/report.pdf","file_name":"report.pdf","mime_type":"application/pdf"}]' \
./smoke-rich-media.sh
```

Local script check:

```bash
./smoke-rich-media.sh --local-check
```
