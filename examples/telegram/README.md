# Telegram CLI smoke example

Pure CLI smoke for the Telegram adapter. Real platform validation is manual.

## Setup

```bash
cargo build
cd examples/telegram
../../target/debug/onlyne init
printf 'TELEGRAM_BOT_TOKEN=%s\n' '<bot-token>' >> .onlyne/.env
python3 ../shared/enable_adapter.py telegram
```

## Run

```bash
ONLYNE_TELEGRAM_CONVERSATION_ID='<chat_id>' ./smoke-telegram.sh
```

If `ONLYNE_TELEGRAM_CONVERSATION_ID` is absent, the script waits for any inbound Telegram text, sends `zig`, then fetches channel/all history.
