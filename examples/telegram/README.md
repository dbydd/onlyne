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
ONLYNE_TELEGRAM_CONVERSATION_ID='<chat_id>' ONLYNE_TEXT='zig' ./smoke-telegram.sh
```

The script writes `ONLYNE_TELEGRAM_CONVERSATION_ID` to `[adapters.telegram].bind_conversation_id`, sends `zig`, then fetches Telegram/all history.

```bash
./smoke-telegram.sh --local-check
```
