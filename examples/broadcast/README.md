# Broadcast CLI example

See `../README.md` for the shared validation flow and variables.

Broadcast sends the same text to many conversations on one channel by looping over `send_message`; no new daemon protocol is required.

```bash
cargo build
cd examples/broadcast
../../target/debug/onlyne init
# configure one adapter in .onlyne/config.toml and .onlyne/.env
../../target/debug/onlyne run --debug &
ONLYNE_TARGETS='weixin:peer1,weixin:peer2' ONLYNE_TEXT='zig' ./smoke-broadcast.sh
```

Target format: `ONLYNE_TARGETS='channel:conversation,channel:conversation'`.

```bash
./smoke-broadcast.sh --local-check
```
