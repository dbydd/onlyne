# QQ Bot CLI smoke example

See `../README.md` for the shared validation flow, variables, and log files.

## Prepare

```bash
cargo build
cd examples/qqbot
../../target/debug/onlyne init
printf 'QQBOT_APP_ID=%s\nQQBOT_APP_SECRET=%s\n' '<app-id>' '<app-secret>' >> .onlyne/.env
python3 ../shared/enable_adapter.py qqbot
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
