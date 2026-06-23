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
./smoke-qqbot.sh
# or send directly:
ONLYNE_QQBOT_CONVERSATION_ID='<group_openid>' ONLYNE_TEXT='zig' ./smoke-qqbot.sh
```

Without `ONLYNE_QQBOT_CONVERSATION_ID`, the script waits for any inbound QQ Bot text, sends `zig`, then fetches QQ Bot/all history.

```bash
./smoke-qqbot.sh --local-check
```
