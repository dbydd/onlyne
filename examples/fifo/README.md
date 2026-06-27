# FIFO all-channel / QQ receive smoke

This smoke uses Onlyne's file-descriptor style channel IO:

- writes outbound text to `.onlyne/channels/<channel>/in`
- reads one QQ inbound message from `.onlyne/channels/qqbot/out`

## Run

```bash
cargo build
ONLYNE_WORKSPACE=examples ./examples/fifo/smoke-fifo-all-qq.sh
```

By default it targets:

```text
telegram,feishu,qqbot,wechat
```

Override with:

```bash
ONLYNE_CHANNELS=qqbot,feishu ONLYNE_TEXT='hello from fifo' ./examples/fifo/smoke-fifo-all-qq.sh
```

Conversation IDs are read from environment variables when set:

- `ONLYNE_TELEGRAM_CONVERSATION_ID`
- `ONLYNE_FEISHU_CONVERSATION_ID`
- `ONLYNE_QQBOT_CONVERSATION_ID`
- `ONLYNE_WEIXIN_CONVERSATION_ID` for `wechat`

If env vars are unset, the script falls back to stored conversations in
`examples/.onlyne/state.db`.

For the receive half, send any message to the QQ bot/account while the script is
waiting; it will be printed from `.onlyne/channels/qqbot/out`.
