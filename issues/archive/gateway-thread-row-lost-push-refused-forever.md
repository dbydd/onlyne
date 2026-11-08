# 网关丢失 thread 行后入站永久拒收：启动不自愈，拒绝墓碑把瞬时故障锁成终态

日期：2026-10-31
报告人：urn:onlyne:agent:dpdp（daily-workbench）

## 现象

两个 IM 网关（`im-lark` 3991、`im-qq` 3992）平台连接一直正常（lark `connected`、qq `online (websocket)`），但所有入站事件都被拒收，处理器节点 dpdp 收不到任何消息：

```
resource.push {"target":"urn:onlyne:agent:dpdp","event_id":"11","outcome":"refused","attempt":441,"error":"The requested record is unavailable."}
im.maintain.failed {"error":"Error: The requested record is unavailable."}
```

每 5 秒一次，从 2026-10-28 14:58 持续到 2026-10-31 13:17，飞书日志累计约 4.5 万条 `im.maintain.failed`。平台侧发消息看不出异常，只有网关 TUI 的日志区在刷 warn。

两个网关 store 的实际状态：

| store | 总行数 | `thread/` 行 | `member/` 行 | 备注 |
|---|---|---|---|---|
| `im/var/store.json`（lark） | 26 | 0 | 2（dpdp、caerulea） | 10-28 13:23 迁移快照同一文件里有 thread 行、11 条 entry、11 条 attention |
| `im/var/qq/store.json` | 27 | 0 | 1（dpdp） | 10-28 首次双向往返测试通过时 thread 行必然存在 |

store 里同时留着那条从未投递成功的墓碑：`message/...` 行 `{"error":{"code":"not_found","retryable":false},"retry_until":"2026-11-01T12:40:27.101Z"}`，以及 `outbound/...` 行 `state: "refused"`。

## 证据（文件与行号）

1. **错误来自通用 not_found**：`packages/lib/src/core.ts:22-24` 的 `requireValue` 默认抛 `The requested record is unavailable.`。
2. **投递走本地 admit**：去重推送 `deliverOne` 用参与会话 thread 自己的 owner 作为目标（`packages/im-gateway/src/pusher.ts:138-165`），thread 由网关自己持有，所以 `packages/lib/src/client.ts:920-921` 走 `this.#messages.admit(...)` 本地入库；`append` 在 `packages/lib/src/messages.ts:122-125` 调 `getThread`，后者在 `packages/lib/src/threads.ts:18-19` 直接 `requireValue(tx.get(key('thread', ...)))`。thread 行缺失即 not_found。
3. **拒绝墓碑永久回放**：`packages/lib/src/messages.ts:49-55`，`admit()` 先读 `message/<sender>/<id>` 的 Remembered 行，命中 `if (old.error) return { error: old.error }` 原样返回旧拒绝；重试窗口过后走 `retry_window_expired`（第 55 行）。而 pusher 固定复用同一 `message_id`（`pusher.ts:22-24`、`:137`，为迁移安全有意设计），identity 永不变化，于是这条消息永远投不出去。沙盒复现：在真实 store 副本上补建 thread 行之后推送**仍然**失败，错误还是同一条 not_found——证明锁死的是墓碑回放，不是 thread 行本身。
4. **启动自愈被短路**：`packages/im-core/src/bridge.ts:176-179`，`resolve()` 命中 `im/conversation/<platform>/<conversation>` 坐标行就直接返回，不再调用 `adopt()`，于是 `ensureThread()`（`packages/im-gateway/src/index.ts:201-219`）在后续每次启动都不会执行；thread 行一旦丢失就不可能自动补回。沙盒复现：网关正常启动、thread 行仍缺、maintain 每轮都失败；手动调一次 `threads.create` 后同一进程内立即恢复。
5. **交付卡上的 `{thread, seq}` 引用在 joined thread 上不可用**：`packages/omp-onlyne/src/decider.ts:1610-1637` 的 `#messageRef` 用 `#threadByAlias` 解析，再经 `#entryAt` 调 `deps.listMessages`（`runtime.ts:204`，即本地 `node.messages.list`）；对非本节点宿主，本地 `list` 在 `packages/lib/src/core.ts:89` 的 `ctx.owner` 处失败，报 `This node does not serve the requested owner.`。用 `{sender, id}` 形式则跳过 `#entryAt`，同一线程回复成功。

### 复现

在 store 副本上跑最小复现（不接平台）：

1. 读入出问题的 store，构造同身份的 `OnlyneNode`。
2. `threads.describe` 报 not_found；跳过 `ensureThread` 直接 `maintain()` 一次 → `resource.push refused`，错误与线上一致。
3. 手动 `threads.create({id: 会话 thread id})` → thread 行补回，但 `maintain()` 再次推送**仍**是同一 not_found（墓碑回放）。
4. 删掉 `message/` 里那条 not_found 墓碑与对应 `outbound/` 行，再 `maintain()` → `resource.push accepted`，entry 落库、attention 写给处理器。

## 影响

1. **入站全丢且外在表现误导**：平台连接器状态一切正常，网关 TUI 把失败按普通 warn 记录，运维不看日志无法发现；本次两个渠道同时静默失效三天。
2. **瞬时故障被放大成永久故障**：缺一行数据 + 两个设计缺口（启动不自愈、拒绝墓碑回放）叠加，没有任何自动恢复路径，只能停机手改 store。
3. **可恢复错误被当成终态**：`not_found` 这种典型可恢复错误落成 `retryable:false` 的永久墓碑，过期后连换体重投都不行（第 55 行的 `retry_window_expired`）。
4. **丢失原因尚未查清**：lib 内没有任何删除 `thread/` 行的路径（全部 `tx.delete` 调用点都不碰 `thread/`），而 `FileStorage` 每次提交整文件重写（`packages/im-gateway/src/storage.ts:63-73`）。时间线：10-28 13:23 迁移快照里 thread 行在，14:03 那次启动 maintain 仍 0 失败，14:58 重启后开始失败。任何用旧内存快照整体重写 `store.json` 的路径（两个进程共享同一 store、迁移回滚恢复旧文件）都值得排查。

## 建议修法

1. **`bridge.resolve` 幂等自愈**：命中坐标行后仍校验 thread 行，缺失就补建（等同每次启动跑一遍 `ensureThread`）。这是成本最低、覆盖面最大的一步。
2. **墓碑只记录不可恢复的拒绝**：`messages.admit` 只对 `not_a_member`、`thread_closed`、`forbidden`、`invalid_request` 之类语义拒绝写 Remembered；`not_found`、`unavailable`、`conflict` 这类直接抛出、不落永久记录。若保留记录，给 Remembered 加 `attempts` 上限，超过后允许同 identity 重投。
3. **pusher 与墓碑解耦**：可恢复错误允许更换 message id（固定 id 的迁移初衷只需在 owner 迁移场景保留），或把 attempt 计入 id，避免与墓碑死锁。
4. **decider 支持 joined thread 的 `{thread, seq}`**：`#entryAt` 对非本节点宿主改为远程读（`GET /threads/<owner>/<id>/messages` 分页查找），让交付卡上宣传的引用形式在受邀线程上可用。
5. **启动自检**：网关启动时若发现已有资源但 thread 行缺失，打一条 error 级日志并自动补建；`im.maintain.failed` 首次出现时附一句可操作的修复指引，而不是每 5 秒重复同一行。

## 验收判据

- 在一个删掉 `thread/` 行的 store 副本上启动网关：启动即补建 thread 行，maintain 摘要 `failures: 0`；已有 owed 推送下一次重试即 `accepted`。
- 在一个 `message/...` 墓碑为 `not_found` 的 store 副本上，补建 thread 后推送能成功（墓碑不再回放）。
- 对 joined thread 用 `{thread, seq}` 形式回复：`admission: accepted`，落在对端宿主该 thread 的下一个 seq，不再出现 `This node does not serve the requested owner.`。
- 两个真实网关实测：平台发消息 → 处理器 inbox 收到并 ack；处理器回复 → `platform.send committed`，平台侧可见。
