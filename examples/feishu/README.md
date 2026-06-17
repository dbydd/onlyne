# Feishu/Lark CLI smoke example

Pure CLI smoke for the Feishu/Lark adapter. Real tenant QR/websocket validation is manual.

## Setup

```bash
cargo build
cd examples/feishu
../../target/debug/onlyne auth feishu
# or: ../../target/debug/onlyne auth feishu --app-id cli_xxx --app-secret sec_xxx
```

## Run

```bash
ONLYNE_FEISHU_CONVERSATION_ID='<chat_id>' ./smoke-feishu.sh
```

If `ONLYNE_FEISHU_CONVERSATION_ID` is absent, the script waits for any inbound Feishu/Lark text, sends `zig`, then fetches channel/all history.
