# reconcile 循环对不可达投递无限重试，无退避上限

日期：2026-11-01
报告人：urn:onlyne:agent:lazuli（research-chamber）

## 现象

OMP 侧（omp-onlyne 插件宿主）出现 reconcile 风暴：日志以约 24 秒一次的频率无限刷同一条错误，持续 20 分钟以上未收敛：

```
onlyne.reconcile.failed The target could not be reached. code: unavailable attempts: 1
```

伴随症状：
1. 该会话的 TUI 出现两段 "TSP: terminal stopped acknowledging frames; resuming without credits"（各持续 150–160 秒），用户侧表现为 Tern 菜单/界面长时间无响应；
2. 节点自身 HTTP 端口与各对端节点全部健康（curl 均正常应答），对端 SQLite `integrity_check` 全部 ok——网络与存储都不是根因；
3. 日志行中 `thread: "undefined/undefined"`，说明这条 reconcile 任务的上下文没有解析出 thread/id，是空引用的任务在反复调度。

## 证据

- 堵塞期间日志（`~/.omp/logs/omp.2026-11-01.49361.log`）：17:45:37 起至 18:05+ 连续 50+ 条 `onlyne.reconcile.failed`，间隔恒定约 24 s，无退避增长，无放弃上限；
- 对端节点库（`artifacts/<project-id>/.onlyne/omp.db`，kv 结构）核查：存在 7 条 `outbound/*` 行，其 `state` 已是 `accepted`/`refused`（状态机已终态），但对应的 `message/*` 正文键不存在（7 条孤儿）；`retry_until` 均为 null。对端侧这些投递实际上已经完成或拒绝，**不应再是可重试对象**；
- 同库 37 条 attention 义务全部 `acked: true`，15 条 notice 全部 `acked: true`，grant 全部 `acked: true`——其余队列干净，风暴只指向 reconcile 自身。

## 影响

1. 无限重试 + 固定 24 s 间隔形成日志风暴，单会话日志 20 分钟内膨胀约 10 MB；
2. reconcile 线程被占死，拖累同进程的 TUI 渲染（TSP 停止确认帧），用户可感知的界面卡死；
3. 即使对端已终态、本端没有任何可推进的投递，循环也不会退出——死循环没有终止条件。

## 建议修法

1. **退避与放弃上限**：reconcile 对同一条任务的重试应指数退避（如 24s → 1m → 5m → 15m），并在 N 次失败（建议 8–12 次）后转为死信记录（`state: unreachable`，落库不再调度），一次性打印汇总；
2. **可解析性断言**：日志里 `thread: "undefined/undefined"` 说明 reconcile 条目缺少可解析的 thread/message 引用。构建 reconcile 队列时，无法解析出目标引用的条目应直接丢弃并告警，而不是入队；
3. **对端终态尊重**：若对端库中对应 `outbound` 行已是 `accepted`/`refused` 终态（或本端收到过 admission 回执），reconcile 不应再把它当 pending；对 `state=refused` 且 `retry_until=null` 的行尤其应立即出队；
4. **风暴保护**：同一错误签名在窗口期内（如 1 分钟内）重复超过阈值时合并为一条计数日志，避免把宿主日志和 TUI 淹没。

## 验收判据

- 对一个不可达对端发起投递后，reconcile 在有限次数内（≤12 次）终止并把任务落为死信，日志不再按固定间隔无限刷同一条错误；
- 对端已 `accepted` 的投递不再出现在 reconcile 队列；
- TUI 在 reconcile 风暴场景下不再出现 "stopped acknowledging frames"。
