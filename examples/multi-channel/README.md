# Multi-channel CLI example

See `../README.md` for the shared validation flow and variables.

Multi-channel smoke runs several adapters in one workspace, subscribes to one event stream, sends to explicit targets, then reads merged history.

```bash
cargo build
cd examples/multi-channel
../../target/debug/onlyne init
# configure multiple adapters in .onlyne/config.toml and .onlyne/.env
../../target/debug/onlyne run --debug &
ONLYNE_TARGETS='telegram:12345,feishu:oc_xxx,qqbot:group_openid,weixin:peer@im.wechat' ONLYNE_TEXT='zig' ./smoke-multi-channel.sh
```

Target format: `ONLYNE_TARGETS='channel:conversation,channel:conversation'`.

```bash
./smoke-multi-channel.sh --local-check
```
