# Rich media / Markdown smoke example

Sends one `format=markdown` message to one or more conversations.

This is meant for the first rich-media pass:

- Feishu/Lark: sends an interactive card.
- QQ Bot: sends `msg_type=2` with `markdown.content`.
- If a channel falls back and `[rich_text.renderer]` is enabled, Onlyne sends raw Markdown text first, then the rendered PNG.

## Prepare

```bash
cargo build
cargo run -- --workspace examples init
# configure Feishu/QQ credentials, for example:
cargo run -- --workspace examples auth qqbot --app-id '<app-id>' --app-secret '<app-secret>'
```

Optional renderer config in `.onlyne/config.toml`:

```toml
[rich_text.renderer]
enabled = true
command = "./examples/render-md-table.sh"
args = ["--out", "{output}"]
timeout_seconds = 20
```

The renderer receives Markdown on stdin and writes PNG to `{output}`. The bundled example renderer requires ImageMagick `magick` and is intentionally boring: it is for table fallback smoke tests, not pretty output.

## Run

```bash
# one target
cargo run --example rich_media
# or explicit target:
ONLYNE_TARGETS='feishu:oc_xxx' cargo run --example rich_media

# many targets
ONLYNE_TARGETS='feishu:oc_xxx,qqbot:group_openid' cargo run --example rich_media

# custom markdown
ONLYNE_TARGETS='qqbot:group_openid' ONLYNE_TEXT=$'# Title\n\n- item\n- `code`' cargo run --example rich_media

# with attachments
ONLYNE_TARGETS='feishu:oc_xxx' \
ONLYNE_ATTACHMENTS='[{"kind":"file","path":"/tmp/report.pdf","file_name":"report.pdf","mime_type":"application/pdf"}]' \
cargo run --example rich_media
```
