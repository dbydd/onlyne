# Feishu/Lark CLI smoke example

See `../README.md` for the shared validation flow, variables, and log files.

## Prepare

```bash
cargo build
cd examples/feishu
../../target/debug/onlyne auth feishu
# or: ../../target/debug/onlyne auth feishu --app-id cli_xxx --app-secret sec_xxx
```

## Run

```bash
./smoke-feishu.sh
# or send directly:
ONLYNE_FEISHU_CONVERSATION_ID='<chat_id>' ONLYNE_TEXT='zig' ./smoke-feishu.sh
```

Without `ONLYNE_FEISHU_CONVERSATION_ID`, the script waits for any inbound Feishu/Lark text, sends `zig`, then fetches Feishu/all history.

```bash
./smoke-feishu.sh --local-check
```
