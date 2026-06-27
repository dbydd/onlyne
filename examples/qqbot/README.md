# QQ Bot CLI smoke example

See `../README.md` for the shared validation flow, variables, and log files.

## Prepare

```bash
cargo build
cd examples/qqbot
../../target/debug/onlyne init
../../target/debug/onlyne auth qqbot --app-id '<app-id>' --app-secret '<app-secret>'
# add --sandbox when using QQ Bot sandbox credentials
```

## Run

```bash
ONLYNE_QQBOT_CONVERSATION_ID='<group_openid>' ONLYNE_TEXT='zig' ./smoke-qqbot.sh
```

The script writes `ONLYNE_QQBOT_CONVERSATION_ID` to `[adapters.qqbot].bind_conversation_id`, sends `zig`, then fetches QQ Bot/all history.

```bash
./smoke-qqbot.sh --local-check
```
