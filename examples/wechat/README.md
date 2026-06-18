# WeChat CLI smoke example

See `../README.md` for the shared validation flow, variables, and log files.

## Prepare

```bash
cargo build
cd examples/wechat
../../target/debug/onlyne auth weixin
# or: ../../target/debug/onlyne auth weixin --token '<token>'
```

`auth` writes only to the selected workspace `.onlyne/` directory. If you initialized `examples/.onlyne`, this example reuses that shared config.

## Run

```bash
./smoke-wechat.sh
# or send directly:
ONLYNE_WECHAT_CONVERSATION_ID='<peer_user_id>' ONLYNE_TEXT='zig' ./smoke-wechat.sh
```

Without `ONLYNE_WECHAT_CONVERSATION_ID`, the script waits for any inbound WeChat text, sends `zig`, then fetches WeChat/all history. The daemon runs with `--debug`, so inbound messages also receive a redacted metadata reply for finding conversation/thread ids.

```bash
./smoke-wechat.sh --local-check
```
