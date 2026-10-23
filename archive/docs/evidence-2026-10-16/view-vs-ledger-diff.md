# 视图 vs 账本：全量对照（2026-10-16，alexandria）

方法：`GET /api/view`（web 与 TUI 共用的 reducer 的实时视图）里的 `view.deliveries`
与 `onlyne ledger --limit 500` 的账本行逐条比 `state`。

```
web deliveries:  314
ledger rows:     442        (账本保留 442 行；视图只持有快照窗口 + 流上见过的行)
state 不一致:      1
```

唯一不一致行：

| 字段 | 视图 (`/api/view`) | 账本 (`onlyne ledger`) |
|---|---|---|
| msg_id | `81b5e247-df6e-420e-91f1-3d524ec044a3` | 同 |
| state | **`in_flight`** | **`rejected`** |
| reason | `task_settled` | `task_settled` |
| acked_at | `null` | `null` |
| kind / hop / family | `completion` / 6 / `051c77f6-…` | 同 |
| outcome | `null` | （账本无 outcome 列值） |
| 看板列 | **`waiting`**（master 板） | 应落 `failed_or_blocked` |

要点：同一行里 `reason` 已是结算原因（`task_settled`）而 `state` 仍是 `in_flight`，
两者自相矛盾；视图的愈合路径（`merge_ledger` → `absorb_causality`）只补因果列、
不写 `state`，而全量重建只在链路重连时发生——链路一直 `live`，所以永不修回。

复现：
```bash
onlyne ledger --server-root <root> --msg-id 81b5e247-df6e-420e-91f1-3d524ec044a3
curl -s "http://127.0.0.1:<port>/api/view?token=<token>" \
  | jq '.view.deliveries["81b5e247-df6e-420e-91f1-3d524ec044a3"].state'
```
