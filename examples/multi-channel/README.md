# Multi-channel CLI example

Multi-channel smoke runs several adapters in one workspace, subscribes to one event stream, and sends to explicit targets.

```bash
cargo build
cd examples/multi-channel
../../target/debug/onlyne init
# configure multiple adapters in .onlyne/config.toml and .onlyne/.env
../../target/debug/onlyne run --debug &
../../target/debug/onlyne client '{"id":"channels","op":"list_channels"}'
ONLYNE_TARGETS='telegram:12345,feishu:oc_xxx,qqbot:group_openid,weixin:peer@im.wechat' ONLYNE_TEXT='zig' ../shared/send-many.py
../../target/debug/onlyne client '{"id":"hist","op":"fetch_all_history","limit":50}'
```
