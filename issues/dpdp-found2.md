# dpdp 现场问题报告 2：agent 暴露面盘点与握手阻塞（2026-10-25）

本轮测试从「空白机器上新装 onlyne 的 agent」视角出发，只使用运行时暴露的三个面：`msg` 工具、`onlyne://` 内部命名空间、PTC（`eval` 内程序化调用 `tool.msg`）。只读探测之后做了 4 次写入尝试。全程没有读插件源码、没有查 `omp.db`、没有绕过。

结论先说：**只读面这一轮是合格的**——错误信息能自举、节点枚举完整、视图清晰；**写入面是断的**——发送要求先有信任 key，而建立 key 的握手只在 TUI 上，agent 无法发起；TUI 侧在首次接触后也失去了响应。于是 agent 在一个新节点上永远迈不过第一条消息。

TUI 的现象来自用户观察，agent 面不可见，本报告按「用户陈述」标注，与 agent 可验证的证据分开列。

## 与上一轮的差异（`dpdp-found1` → 本轮）

- **补齐**：`onlyne://neighbors` 存在了，`msg` 的 `scope: directory` 也能列出全部节点与联系人。这是 `dpdp-found1` 问题 2（邻居与联系人 agent 侧无通道）的直接修复，本轮确认可用。
- **新增阻塞**：发送前要求信任 key，错误为 `No trusted key for <node>; complete the key exchange first.`。`dpdp-found1` 时发送可以直接拿到 `accepted`，本轮 4 次尝试全部被拒。

## 环境与现场

| 项 | 值 |
|---|---|
| 本节点 | `urn:onlyne:agent:dpdp` |
| 区域 | `urn:onlyne:region:daily-workbench` |
| endpoint | `http://127.0.0.1:56018/onlyne/v3` |
| 已发现对端 | `caerulea`、`cyanea`（`alexandria`）、`lazuli`（`research-chamber`），mdns 发现，`verified`，成员状态 `active` |
| 写入尝试 | 4 次（3 次握手意图 → lazuli，1 次问候 → caerulea），0 次到达发送阶段 |
| 入站 | 0 条（`onlyne://inbox` 全程 `No attention items.`） |

## 只读面盘点（本轮通过项）

### 1. `msg` 工具的自举

`read xd://msg` 一次拿到完整自述：入口形状 `{delivery_kind, msg, meta}`、结果判读规则、禁轮询契约、TS schema。参数校验错误逐字段说明缺什么，并把完整文档回显一遍：

```
Invalid args for xd://msg: Validation failed for tool "msg":
  - delivery_kind: delivery_kind must be The delivery intent in natural language (was missing)
  - msg: msg must be The message body or the local query (was missing)
```

一个没有任何先验的 agent 靠「读一次文档 + 发一次空参数」就能完成自举，这一段是好的。

### 2. 可读通道与 `onlyne://` 路由对照

`msg` 的可读通道由错误信息枚举：`directory, neighbors, threads, inbox, messages, message and evidence`。`onlyne://` 根页枚举的视图是：`neighbors, threads, inbox, contexts, outbound`。

| 主题 | `msg` scope | `onlyne://` 路由 |
|---|---|---|
| 节点目录 | `directory`（实测可用，返回完整 JSON） | 无（`Unknown onlyne:// route directory.`） |
| 邻居 | `neighbors`（报 `read neighbors needs a thread reference.`） | 可用（markdown 列表 + 全 URN 钻取 JSON） |
| 线程 | `threads`（未实测） | 可用（空态 `No threads.`） |
| 收件箱 | `inbox`（未实测） | 可用（空态 `No attention items.`） |
| 线程内消息 | `messages` / `message`（未实测） | 无 |
| 证据 | `evidence`（未实测） | 无 |
| 执行上下文 | 无 | 可用（`contexts`，空态；`contexts/main` 报 `onlyne://contexts has no subroutes.`） |
| 出站记录 | 无 | 可用（`outbound`） |

两套枚举只有 `neighbors` / `threads` / `inbox` 三个主题重叠，其余各覆盖一半。agent 需要维护两份心智地图，且两边的报错不互相指路。

### 3. `onlyne://` 的行为细节

- 路由大小写不敏感：`onlyne://Neighbors` 正常返回。
- 尾斜杠无碍：`onlyne://threads/` 正常返回。
- 未知路由明确报错：`Unknown onlyne:// route nonexistent.`
- 邻居钻取要**全 URN**：`onlyne://neighbors/urn:onlyne:agent:lazuli` 返回单邻居 JSON（`node_id`、`base_url`、`region`、`last_seen`、`verified`）；短名 `onlyne://neighbors/lazuli` 被拒为 `No neighbor is named lazuli.`
- 视图数据是实时的：`neighbors` 的 `last_seen` 随每次读取刷新（08:06 → 08:09 → 08:15），不是缓存旧值。
- 通用 selector 静默无效：`onlyne://neighbors:2-3` 与 `onlyne://inbox?limit=5` 既不报错也不生效，原样返回整页。
- 写入被 tool policy 明确拦截：`Tool "write" is blocked by tool policy. Reason: onlyne:// URLs are read-only`。

### 4. PTC（`eval` 内 `await tool.msg(...)`）

| 调用 | 形态 | 结果 |
|---|---|---|
| 空参数 | **抛 `ValidationError`** | 逐字段报缺 `delivery_kind` / `msg`（不带文档回显） |
| 无 `scope` 的读意图 | 返回 `{text, details: {}, hasError: true}` | `The decision named no read scope; the readable channels are …` |
| `scope: "neighbors"` | 返回 `{text, hasError: true}` | `read neighbors needs a thread reference.` |
| `scope: "directory"` | 返回 `{text, details: {result: <结构化 JSON>}, hasError: undefined}` | 成功；`text` 与 `details.result` 内容重复，`text` 带 `Onlyne answered; the structured result follows.` 前缀 |

同一套工具在 PTC 下可用，不需要额外桥接；问题只在错误形态不统一（见问题 9、问题 10）。

## 写入面：4 次尝试全表

| # | 目标 | `delivery_kind` | 附加 `meta` | 结果 |
|---|---|---|---|---|
| 1 | `urn:onlyne:agent:lazuli` | `request a connection and key exchange handshake with lazuli` | `to` | `admission: "unknown"`，无信任 key |
| 2 | `urn:onlyne:agent:lazuli` | `perform the key exchange with urn:onlyne:agent:lazuli` | `to`、`operation: "key_exchange"` | 同上，op 字段未改变判决 |
| 3 | `urn:onlyne:agent:lazuli` | `connect to urn:onlyne:agent:lazuli` | `to`、`op: "connect"` | 同上 |
| 4 | `urn:onlyne:agent:caerulea` | `send a greeting to caerulea` | `to` | 同上 |

统一错误：

```json
{
  "admission": "unknown",
  "error": {
    "code": "unavailable",
    "message": "No trusted key for urn:onlyne:agent:<target>; complete the key exchange first.",
    "retryable": true
  }
}
```

附言：`The record stays open for recovery; do not re-send the same intent.`

`onlyne://outbound` 因此累积了 4 条记录，全部 `unknown · unavailable`：

- `082d793d-3b69-4ab0-baa3-31c3d3a9499a` → lazuli
- `2982b7dd-b96b-4cfe-a1c2-60cc69866035` → lazuli
- `4f826c20-2129-4508-a101-65e6de8513fb` → caerulea
- `cf1e22fe-75e7-4257-a8e4-18d8faed4f9f` → lazuli

### 用户侧 TUI 现象（用户陈述，非 agent 可见）

用户在 TUI 上完成了对 caerulea 的连接，只成功这一次；之后再无法发起更多请求，出站和入站都没有反应。TUI 的握手系统不实时显示对端发来的消息，疑似死锁或死路径。这是本轮测试终止的直接原因。

### 握手成功在 agent 面上零痕迹

用户报告与 caerulea 建立联系之后，agent 侧能读到的状态没有变化：

- `onlyne://neighbors` 三个节点全部仍是 `handshake: no exchange`，包括 caerulea。
- `onlyne://inbox` 仍是 `No attention items.`，没有待处理的连接请求。
- `onlyne://threads` 仍是 `No threads.`。
- 给 caerulea 的问候仍然被 `No trusted key` 拒绝，并新增一条 outbound 记录。

agent 无法区分「握手进行中」「握手已完成但 key 未生效」「握手根本没发生」三种情况。

## 问题清单

### 问题 1：握手不在 agent 暴露面上，发送永远迈不过信任 key（P0）

**现象**：3 种不同措辞的握手/连接意图全部被判成 send，返回 `No trusted key for …; complete the key exchange first.`；显式给 `operation: "key_exchange"` 或 `op: "connect"` 不改变判决。

**判断**：`msg` 的 judge 只有「发送」这一条动作路径。建立信任 key 的握手是 TUI 上的用户操作（README 记载为 `/onlyne` 第 3 页 Space），agent 没有任何等价入口。纯 agent 工作流（新机器、无人操作 TUI）在第一条消息前就停了。

**建议**：给 `msg` 一个显式的握手动作（例如 `meta.op: "handshake"` 或独立 `read_scope` 式的操作），返回握手状态与下一步；与 TUI 同源。

### 问题 2：TUI 握手在首次接触后失去响应，疑似死锁（P0，用户观察）

**现象**：用户完成一次对 caerulea 的连接后，后续请求出站入站均无反应；TUI 不实时显示对端消息。

**判断**：agent 无法从任何视图复现或确认这一现象，只能引用用户陈述。若确为死锁，它是本轮唯一一条「用户侧也无法推进」的故障。

**建议**：握手流程增加超时与可见的状态机（pending / exchanged / failed），TUI 与 agent 视图共享同一份状态。

### 问题 3：TUI 动作对 agent 不可观测（P0）

**现象**：用户报告成功连接 caerulea 后，`onlyne://neighbors` 仍是 `handshake: no exchange`，inbox、threads 均无变化。

**判断**：agent 无法基于可见状态判断能否发送，只能盲试，每次盲试还留下一条失败记录。

**建议**：邻居条目增加握手状态字段（`none` / `requested` / `exchanged` / `failed`），TUI 的请求与结果即时反映到 `onlyne://neighbors` 与 `msg scope: directory`。

### 问题 4：失败重试按意图文本去重，记录无限累积（P1）

**现象**：错误附言要求「不要重发同一意图」，但 3 次换措辞的握手尝试产生 3 条独立 outbound 记录，问候尝试又加 1 条；记录之间没有关联，也没有回收手段。

**判断**：去重键是意图原文。工具文档本身鼓励「用自然语言表达意图」，会反复改写的 LLM 天然绕过这个键。附言也没有给出 recovery 的具体动作。

**建议**：去重按 `(target, normalized operation)` 而非原文；错误里给出可执行的下一步（取消、等待、或「已完成握手」的指引）。

### 问题 5：`msg` 通道与 `onlyne://` 路由两套枚举不对应（P1）

**现象**：`directory` / `messages` / `message` / `evidence` 在 `onlyne://` 下无路由；`contexts` / `outbound` 不在 `msg` 可读通道清单里。

**判断**：同一份状态两套入口，agent 要维护两份地图；`Unknown onlyne:// route` 与 `named no read scope` 互不指路。

**建议**：两套枚举合并，或至少在报错里互相指路（路由不存在时提示对应的 scope 名，反之亦然）。

### 问题 6：`neighbors` 通道要求 thread 引用，但不说明怎么给（P1）

**现象**：`scope: "neighbors"` 返回 `read neighbors needs a thread reference.`，而 `onlyne://neighbors` 直接可读；同主题两个入口的语义不一致。短名钻取被拒时（`No neighbor is named lazuli.`）也不提示要用全 URN。

**建议**：补文档或在报错里说明该通道的引用形状；短名拒绝时提示「use the full node URN」。

### 问题 7：视图上的通用 selector 静默无效（P1）

**现象**：`onlyne://neighbors:2-3`、`onlyne://inbox?limit=5` 不报错、不生效，原样返回整页。

**判断**：静默无效比报错更糟——agent 会以为分页生效了，并基于错误假设继续推理。

**建议**：不支持的 selector 一律显式报错。

### 问题 8：`outbound` 记录状态无解释字段、无关联（P2）

**现象**：记录只有 `unknown · unavailable` 两个词，没有原因码、重试建议、关联的握手尝试。

**建议**：附上错误码与下一步动作；与握手记录建立关联，至少在 `outbound` 行内可区分「发送失败」与「等待握手」。

### 问题 9：PTC 错误双形态（P2）

**现象**：schema 校验错抛 `ValidationError`（需 `try/catch`），judge 拒绝与运行时错返回 `{text, hasError: true}`（不抛）。

**判断**：同一工具两种错误通道，PTC 调用方必须两头防，容易漏。

**建议**：统一成一种形态（推荐都包裹返回，`hasError: true`）。

### 问题 10：PTC 成功返回 `text` 与 `details.result` 双份内容（P2）

**现象**：成功时 `text` 是 `Onlyne answered; the structured result follows.` 加完整 JSON，`details.result` 又是同一份 JSON。

**判断**：对直接读文本的 LLM 友好，对程序侧是纯冗余。

**建议**：程序侧文档明确「只取 `details.result`」，或 `text` 只保留一句摘要。

### 问题 11：短名钻取被拒但不指路（P2）

**现象**：见问题 6 后半。建议合并处理。

## 已确认好用、不要误修

- `msg` 的自描述与参数校验错误（逐字段解释 + 文档回显）。
- `scope` 缺失时的错误枚举了全部可读通道——一遍就能学到词汇表。
- `onlyne://` 根页与全部视图：标题、归属节点、空态文案、未知路由报错都清晰。
- `onlyne://` 只读边界明确（tool policy 拦截写入）。
- `directory` scope 的节点枚举：字段完整（`instance_id`、`base_url`、`last_seen`、`source`、`membership`），时间戳秒级新鲜。
- 路由大小写不敏感、尾斜杠容错。
- 全 URN 邻居钻取返回机器可读 JSON。
- PTC 下 `tool.msg` 直接可用，成功调用保留结构化 `details.result`。

## 建议（按修复优先级）

1. **P0**：把握手做成 agent 可调用的 `msg` 动作，并与 TUI 同源。
2. **P0**：握手状态进入 `onlyne://neighbors` 与 `directory`，TUI 动作实时可见。
3. **P0**：修复 TUI 握手在首次接触后的无响应（用户观察的死锁）。
4. **P1**：失败记录按目标+操作去重，错误给出可执行的 recovery。
5. **P1**：合并 `msg` 通道与 `onlyne://` 路由的枚举，或互相指路。
6. **P1**：`neighbors` 通道的引用要求写进文档/报错；短名拒绝提示全 URN。
7. **P1**：视图 selector 不支持时显式报错。
8. **P2**：`outbound` 记录补错误码与关联。
9. **P2**：PTC 错误统一形态；`text`/`details` 去重。

## 复现清单

1. **问题 1**：在任意未握手的邻居上执行 `msg({delivery_kind: "connect to <urn>", meta: {to: "<urn>", op: "connect"}})` → `admission: "unknown"`，`No trusted key …`；`onlyne://outbound` 新增一条 `unknown · unavailable`。
2. **问题 3**：TUI 完成一次连接后读 `onlyne://neighbors` → 目标节点仍是 `handshake: no exchange`。
3. **问题 5**：`onlyne://directory` / `messages` / `evidence` → `Unknown onlyne:// route …`；`msg({meta: {scope: "neighbors"}})` → `read neighbors needs a thread reference.`
4. **问题 7**：`onlyne://neighbors:2-3` 与 `onlyne://inbox?limit=5` → 返回整页、无报错。
5. **问题 9**：`tool.msg({})` → 抛 `ValidationError`；`tool.msg({delivery_kind: "list the nodes", msg: "list nodes"})` → 返回 `hasError: true` 不抛。

## 未验证项

- 入站消息的读取与回信路径：本轮零入站，没有机会验证。
- `msg` 的 `messages` / `message` / `evidence` / `threads` / `inbox` scope 未逐个实测（只验证了 `directory` 与 `neighbors`）。
- TUI 侧行为全部来自用户陈述，agent 面无对应证据。
- 握手完成后的发送、delivery fact、`onlyne.inbound` 回灌：因握手阻塞未能测试。

## 遗留状态

`onlyne://outbound` 4 条记录（3 → lazuli，1 → caerulea，全部 `unknown · unavailable`）；`threads`、`inbox` 为空；无握手记录可见。测试停在握手环节。
