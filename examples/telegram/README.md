# Telegram CLI smoke example

See `../README.md` for the shared validation flow, variables, and log files.

## Prepare

```bash
cargo build
cd examples/telegram
../../target/debug/onlyne init
printf 'TELEGRAM_BOT_TOKEN=%s\n' '<bot-token>' >> .onlyne/.env
python3 ../shared/enable_adapter.py telegram
```

## Run

```bash
./smoke-telegram.sh
# or send directly:
ONLYNE_TELEGRAM_CONVERSATION_ID='<chat_id>' ONLYNE_TEXT='zig' ./smoke-telegram.sh
```

Without `ONLYNE_TELEGRAM_CONVERSATION_ID`, the script waits for any inbound Telegram text, sends `zig`, then fetches Telegram/all history.

```bash
./smoke-telegram.sh --local-check
```
