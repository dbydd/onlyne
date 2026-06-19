# Onlyne examples

All examples are plain CLI smoke flows. By default, commands run from any child example discover and share the nearest parent workspace at `examples/.onlyne/`; use `ONLYNE_WORKSPACE` or `--workspace <dir>` when you want an isolated workspace.

## Common flow

```bash
cargo build
cd examples
../target/debug/onlyne init
# add auth/secrets once into examples/.onlyne
cd <name>
./smoke-<name>.sh
```

Run a script syntax check without real platform credentials:

```bash
./smoke-<name>.sh --local-check
```

## Common variables

| Variable | Meaning |
| --- | --- |
| `ONLYNE_BIN` | Path to the `onlyne` binary. Defaults to `../../target/debug/onlyne` from the shared smoke scripts. |
| `ONLYNE_WORKSPACE` | Explicit workspace directory. If unset, Onlyne walks upward and normally uses `examples/.onlyne` when it exists. |
| `ONLYNE_TEXT` | Outbound text. Defaults to `zig`. |
| `ONLYNE_FORMAT` | Outbound text format for scripts using `send-many.py`: `plain` or `markdown`. Defaults to `plain`. |
| `ONLYNE_ATTACHMENTS` | JSON array of attachment refs for scripts using `send-many.py`. Defaults to `[]`. |
| `ONLYNE_TIMEOUT` | Seconds to wait for an inbound message when no conversation id is set. Defaults to `180`. |
| `ONLYNE_EVENT_LOG` | Event subscription capture file. Defaults to `<channel>-events.ndjson`. |
| `ONLYNE_DAEMON_LOG` | Daemon stdout/stderr capture file. Defaults to `<channel>-daemon.log`. |
| `ONLYNE_<CHANNEL>_CONVERSATION_ID` | Known target conversation id. If absent, channel smoke scripts wait for one inbound text. |

Channel variable names: `ONLYNE_TELEGRAM_CONVERSATION_ID`, `ONLYNE_FEISHU_CONVERSATION_ID`, `ONLYNE_QQBOT_CONVERSATION_ID`, `ONLYNE_WECHAT_CONVERSATION_ID`.

## Scripts

| Example | Script | Purpose |
| --- | --- | --- |
| `telegram/` | `smoke-telegram.sh` | Telegram subscribe/send/history smoke. |
| `feishu/` | `smoke-feishu.sh` | Feishu/Lark subscribe/send/history smoke. |
| `qqbot/` | `smoke-qqbot.sh` | QQ Bot subscribe/send/history smoke. |
| `wechat/` | `smoke-wechat.sh` | WeChat subscribe/send/history smoke. |
| `broadcast/` | `smoke-broadcast.sh` | Send one text to many conversations on one channel. |
| `multicast/` | `smoke-multicast.sh` | Send one text to selected conversations across channels. |
| `multi-channel/` | `smoke-multi-channel.sh` | List channels, send to explicit targets, read all history. |
| `rich-media/` | `smoke-rich-media.sh` | Send `format=markdown` messages and optional attachments. |

Broadcast/multicast targets use:

```bash
ONLYNE_TARGETS='channel:conversation,channel:conversation' ONLYNE_TEXT='zig' ../shared/send-many.py
```
