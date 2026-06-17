# Multicast CLI example

Multicast sends one text to a selected subset of conversations, possibly across platforms.
It is a CLI-side loop over `send_message`.

```bash
cargo build
cd examples/multicast
../../target/debug/onlyne init
# configure needed adapters
../../target/debug/onlyne run --debug &
ONLYNE_TARGETS='telegram:12345,weixin:peer@im.wechat' ONLYNE_TEXT='zig' ../shared/send-many.py
```
