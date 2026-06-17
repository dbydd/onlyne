# QQ Bot CLI smoke example

Pure CLI smoke for the QQ Bot adapter. Real gateway validation is manual.

## Setup

```bash
cargo build
cd examples/qqbot
../../target/debug/onlyne init
printf 'QQBOT_APP_ID=%s\nQQBOT_APP_SECRET=%s\n' '<app-id>' '<app-secret>' >> .onlyne/.env
python3 ../shared/enable_adapter.py qqbot
```

## Run

```bash
ONLYNE_QQBOT_CONVERSATION_ID='<group_openid>' ./smoke-qqbot.sh
```

If `ONLYNE_QQBOT_CONVERSATION_ID` is absent, the script waits for any inbound QQ Bot text, sends `zig`, then fetches channel/all history.
