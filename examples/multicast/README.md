# Multicast CLI example

See `../README.md` for the shared validation flow and variables.

Multicast sends one text to selected conversations, possibly across platforms. It is a CLI-side loop over `send_message`.

```bash
cargo build
cd examples/multicast
../../target/debug/onlyne init
# configure needed adapters in .onlyne/config.toml and .onlyne/.env
../../target/debug/onlyne run --debug &
ONLYNE_TARGETS='telegram:12345,weixin:peer@im.wechat' ONLYNE_TEXT='zig' ./smoke-multicast.sh
```

Target format: `ONLYNE_TARGETS='channel:conversation,channel:conversation'`.

```bash
./smoke-multicast.sh --local-check
```
