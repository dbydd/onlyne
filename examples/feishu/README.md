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
ONLYNE_FEISHU_CONVERSATION_ID='<chat_id>' ONLYNE_TEXT='zig' ./smoke-feishu.sh
```

The script writes `ONLYNE_FEISHU_CONVERSATION_ID` to `[adapters.feishu].bind_conversation_id`, sends `zig`, then fetches Feishu/all history.

```bash
./smoke-feishu.sh --local-check
```
