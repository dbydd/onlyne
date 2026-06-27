# Channel FIFO IO Design

Config schema is generated from Rust config types with `schemars`:

```bash
cargo run --features schema --bin gen-schema > onlyne-config.schema.json
```

Workspace configs include Taplo's schema hint:

```toml
#:schema ./onlyne-config.schema.json
```

Onlyne exposes each singleton channel as file-descriptor style IO under the workspace-local `.onlyne/channels/<channel>/` directory.

## Paths

For every enabled adapter channel plus `loopback`:

```text
.onlyne/channels/<channel>/in
.onlyne/channels/<channel>/out
```

Both `in` and `out` are Unix FIFOs. Paths are stable so users can create symlinks to them.

Channels use the public string IDs:

- `telegram`
- `feishu`
- `qqbot`
- `wechat`
- `loopback`

## Write side: `in`

Writing to `in` sends one message through that channel:

```bash
echo '# report' > .onlyne/channels/telegram/in
```

Rules:

- One open/write/close lifecycle is one message; EOF terminates the message.
- Multi-line writes are preserved.
- Content is treated as Markdown by default, matching `send_message` IPC semantics.
- Set `in_format = "raw_text"` to send FIFO input literally.
- External channels route through the channel's configured or `/handshake`-bound `bind_conversation_id`.
- `loopback/in` injects a local loopback inbound activation message.
- No conversation id is accepted through the FIFO path.

## Read side: `out`

Reading `out` returns one inbound message for that channel:

```bash
cat .onlyne/channels/telegram/out
```

Rules:

- No new message: block until one is available.
- Default output format: plain text.
- If configured for history context, output a small transcript:

```text
--- history ---
[2026-06-27T12:00:00Z alice] previous message
--- new ---
[2026-06-27T12:01:00Z alice] latest message
```

## Configuration

Global defaults live in `[io]`:

```toml
[io]
in_format = "markdown"      # markdown | raw_text
out_content = "latest_only" # latest_only | with_history
out_cursor = "consume"      # retain | consume
history_context_messages = 20
```

Per-channel overrides live under adapter IO tables:

```toml
[adapters.telegram.io]
in_format = "raw_text"
out_content = "with_history"
out_cursor = "retain"
history_context_messages = 50
```

`loopback` uses its own table:

```toml
[loopback.io]
in_format = "markdown"
out_content = "latest_only"
out_cursor = "consume"
```

## `out` modes

`out` behavior is a 2x2 combination:

| Content mode | Cursor mode | Behavior |
| --- | --- | --- |
| `latest_only` | `retain` | Print latest unread message; do not advance shared cursor. |
| `latest_only` | `consume` | Print latest unread message; advance shared cursor. |
| `with_history` | `retain` | Print recent transcript ending in latest unread message; do not advance shared cursor. |
| `with_history` | `consume` | Print recent transcript ending in latest unread message; advance shared cursor. |

History context is limited by `history_context_messages`.

## Shared consume cursor

Consume mode uses one shared per-channel cursor across local consumers:

- FIFO `out`
- pi-onlyne inbound notifications
- future CLI consumers that opt into consume behavior

If pi-onlyne has already surfaced an inbound notification successfully, consume-mode FIFO `out` for that channel must not return that same new message later.

Retain mode ignores the shared cursor for reads and does not advance it.

## Non-goals for first implementation

- No socket/nc protocol for channel files.
- No per-consumer named cursors.
- No JSON command format in `in`.
- No attachment upload through FIFO `in`.
