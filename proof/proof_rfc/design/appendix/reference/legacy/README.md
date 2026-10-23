# 修订前轨迹

本目录保留第一轮故障搜索脚本。它们编码的是修订前的 RFC 规则，用来复现当时的见证轨迹。当前规则以 `docs/rfcs/` 为准，当前检查不运行这些脚本。

| 脚本 | 修订前的规则 | 修订后的位置 |
|---|---|---|
| `closed_attention.py` | 关闭 thread 后自动片段排除未注入关注 | 0008 已加未消费例外 |
| `remote_reply.py` | owner 追加 reply 不产生远程关注义务 | 0003 已加关注义务与关注清单 |
| `old_event_read.py` | 交接后旧积压无人可读 | 0006 已改为有积压不得交接 |
| `dispatch_race.py` | 首次推送读取当前绑定 | 0012 已改为按首次归属推送 |
| `timeout_outcome.py` | 超时归 `aborted` | 0006 已改为 `unknown` |
| `pending_notice.py` | 未决操作没有必做恢复动作 | 0006 已加查询责任 |
| `queue_starvation.py` | 严格 FIFO 的实现风险 | 仍属实现选择，未改协议 |

这些脚本可以单独运行，输出只代表修订前的状态。
