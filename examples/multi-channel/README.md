# Multi-channel CLI example

See `../README.md` for the shared validation flow and variables.

Multi-channel smoke runs several adapters in one workspace, subscribes to one event stream, sends to configured channel targets, then reads merged history.

```bash
cargo build
cd examples/multi-channel
../../target/debug/onlyne init
# configure multiple adapters in .onlyne/config.toml and .onlyne/.env
../../target/debug/onlyne run --debug &
ONLYNE_TARGETS='telegram,feishu,qqbot,wechat' ONLYNE_TEXT='zig' ./smoke-multi-channel.sh
```

Target format: `ONLYNE_TARGETS='channel,channel'`.

```bash
./smoke-multi-channel.sh --local-check
```
