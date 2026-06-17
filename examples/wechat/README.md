# WeChat CLI smoke example

This example verifies Onlyne's WeChat path from a plain one-shot CLI workspace. It does not store secrets in the repo.

## 1. Build Onlyne

```bash
cargo build
```

## 2. Login/bind WeChat in this example workspace

```bash
cd examples/wechat
../../target/debug/onlyne auth weixin
# or bind an existing ilink token:
../../target/debug/onlyne auth weixin --token '<token>'
```

`auth` writes only to `examples/wechat/.onlyne/config.toml` and `examples/wechat/.onlyne/.env`.

## 3. Run the full CLI smoke

```bash
./smoke-wechat.sh
```

The script starts the daemon, subscribes to events, waits for an inbound WeChat text if `ONLYNE_WECHAT_CONVERSATION_ID` is not set, sends `zig` once, reads per-channel/all history, then exits. The daemon runs with `--debug`, so each inbound message also receives a metadata/debug reply for finding conversation/thread IDs.

To send directly to a known peer/conversation:

```bash
ONLYNE_WECHAT_CONVERSATION_ID='<peer_user_id>' ONLYNE_TEXT='zig' ./smoke-wechat.sh
```

Logs:

- `wechat-events.ndjson` — subscribed event stream
- `wechat-daemon.log` — daemon stdout/stderr

Local script check without WeChat credentials:

```bash
./smoke-wechat.sh --local-check
```
