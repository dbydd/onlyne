# WeChat CLI smoke example

See `../README.md` for the shared validation flow, variables, and log files.

## Prepare

```bash
cargo build
cd examples/wechat
../../target/debug/onlyne auth wechat
# or: ../../target/debug/onlyne auth wechat --token '<token>'
```

`auth` writes only to the selected workspace `.onlyne/` directory. If you initialized `examples/.onlyne`, this example reuses that shared config.

## Run

```bash
ONLYNE_WECHAT_CONVERSATION_ID='<peer_user_id>' ONLYNE_TEXT='zig' ./smoke-wechat.sh
```

The script writes `ONLYNE_WECHAT_CONVERSATION_ID` to `[adapters.wechat].bind_conversation_id`, sends `zig`, then fetches WeChat/all history.

```bash
./smoke-wechat.sh --local-check
```
