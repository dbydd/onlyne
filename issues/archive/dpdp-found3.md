# dpdp 现场问题报告 3：消息分流、线程组织与消息状态模型（2026-10-25）

本报告从 `daily-workbench/dpdp` 这个 OMP 会话出发，覆盖一轮端到端测试：节点发现、向 caerulea 问好并请她向域内广播、收到汇总、回信。对比 found1（当时入站为零、只能盲发），这次跑通了一次完整的收发闭环，所以本报告的问题都发生在「能用」之后的那一层：分类器把简单消息送去重型通道、一段对话散成多个线程、以及消息被赋予了不该有的状态。

结论先说：**投递层是可靠的（全程 `accepted / durable`，回复自动注入开始新回合），但接收端的处理分流和线程组织都在给简单对话加阻力。** 另外有一条设计层面的思考：消息不应该有「未回复 / 已回复」状态，消息就是消息。

全程只通过内置文档（`xd://msg` 工具文档）和直觉行动，未读插件源码、未查 `omp.db`。日志证据仅来自用户直接告知的观察，我未自行翻日志。

---

## 环境与现场

| 项 | 值 |
|---|---|
| 节点 | `urn:onlyne:agent:dpdp` |
| region | `urn:onlyne:region:daily-workbench` |
| endpoint | `http://127.0.0.1:56442/onlyne/v3` |
| 对端 | `urn:onlyne:agent:caerulea`（`alexandria`，`http://127.0.0.1:56233/onlyne/v3`） |
| 域内其他节点 | `cyanea`（`alexandria`）、`lazuli`（`research-chamber`） |

本次实际收发的消息：

| 方向 | message id | 落点线程（owner） | seq |
|---|---|---|---|
| 发 → caerulea | `399325bf-f460-4500-9862-1368aa62c682` | `e0f0bff6-e1d2-4134-abdc-d83e26a62198`（caerulea） | 1 |
| 收 ← caerulea | `185097c0-fbfd-42d8-92ba-97dceca55636/1` | `185097c0-fbfd-42d8-92ba-97dceca55636`（dpdp） | 1 |
| 发 → caerulea（回信） | `d0351468-fe17-46f9-8052-5a15b90d48e2` | `52c8127d-bcab-4507-bd16-05ed32228ea9`（caerulea） | 1 |

三段合起来只是一次问候往来，见问题 3。

---

## 问题 1：节点发现意图的分类不稳定，成功路径要靠报错摸索

### 1. 现象

同一类问题（「现在连着哪些节点」），四种说法四种结果：

| 意图说法 | 结果 |
|---|---|
| `discover currently connected nodes`（中文正文） | `The decision did not choose a delivery target.` |
| `查询本节点已知的参与者列表`（中文） | 同上 |
| `list peers`（英文） | `The decision named no read scope; the readable channels are directory, neighbors, threads, inbox, messages, message and evidence.` |
| `read the neighbors directory` + `meta.scope: "neighbors"` | `read neighbors needs a thread reference.` |
| `read directory` + `meta.scope: "directory"` | 成功，返回三个节点的完整目录 |

### 2. 判断

与 found1 问题 8 同源但已有改善：`read_scope` 现在有 `directory` 了，报错也把可选范围列出来了（这次能走通，全靠第三次的报错指路）。剩下的问题是**最常见的发现意图措辞稳定地落不进这个 scope**，中文尤其差。一个新 agent 第一次问「谁在线」，得到的还是一个像在怪它措辞的报错。

另一个小坑：报错把 `neighbors` 列为可读范围，但选它又要线程引用——它是「读某线程的邻居上下文」，不是「枚举邻居」，名字极具误导性。

### 3. 建议

- 把「列出节点 / 谁在线 / 发现节点」这类高频意图直接映射到 `scope: directory`，作为 judge 的示范样例写进文档。
- `xd://msg` 文档补一条目录查询示例（found1 已提，此处追加：文档至今没有 `scope` 的合法值列表，全靠报错）。
- `neighbors` 改名（如 `thread_neighbors`）或报错时说明它需要线程引用的原因。

---

## 问题 2：简单入站消息被分类为 task，fork 给 subagent，多一层调度（双侧复现）

### 1. 现象

caerulea 收到我的问候后，消息被分类为 task，fork 给她的 subagent 处理，多出一层调度，产生延迟——这是用户在 caerulea 侧直接观察到的。

同一现象在我这侧复现：caerulea 的汇总回复（一条纯转达消息）进来后被交给 subagent `onlyne-c-1`。subagent 只能读不能发，只能把草稿交回主会话，由主会话再执行发送。我回信后的回执里，`plan` 字段直接给出证据：

```json
"plan": {
  "response": "async",
  "execution": "existing",
  "context": "c-1"
}
```

`context: "c-1"` 指向 subagent 上下文，不是主会话。

### 2. 判断

分流的本意应该是把重型任务隔离出去，但它的默认落点错了：一句问候、一条转达汇总，都被送进了 fork 通道。代价是每次多一层上下文转手和延迟，而 subagent 又没有发送权限，最后还是绕回主会话——**转了一圈，活还是主会话干的，只是慢了**。

### 3. 建议

- 分流默认主会话；只有消息体量、附带产物或显式标记达到重型标准才 fork。
- `plan.context` 可以作为修复后的验证观测点：简单消息的回执应不再出现 subagent context。

---

## 问题 3：一段对话散成多个线程，回复关联未生效，UI 无法组织成 thread

### 1. 现象

一次问候往来的三个片段落在三个线程，每段都是 seq 1：

| 片段 | 线程 | owner |
|---|---|---|
| 我的首问 | `e0f0bff6…` | caerulea |
| 她的汇总回复 | `185097c0…` | dpdp |
| 我的回信 | `52c8127d…` | dpdp 发出、落 caerulea 侧新线程 |

回信显式带了 `relation: "reply"` 和 `in_reply_to: { sender, id }`，仍新建了线程而没有续上原线程 `e0f0bff6`。

用户提供的 UI 截图（caerulea 侧）与此完全一致：线程列表 5 项——「来自 dpdp 的线程」「first contact from urn:onlyne:agent:dpdp」「来自 cyanea 的线程」「first contact from urn:onlyne:agent:cyanea」「first contact from urn:onlyne:agent:dpdp」——每项 1 条消息。同一段对话在 UI 里没有组织成 thread，而是散成一组单消息线程。

### 2. 判断

`in_reply_to` 在投递语义上被接受了（`accepted`），但它的线程语义没有实现：回复没有落到被引用消息所在的线程。结果是对话的连续性在协议层就断了，UI 只能如实显示碎片。found1 问题 5 讲的是回信没有可用的引用形状；这次引用形状被接受了，问题推进到下一层——**引用被接受但不影响落点**。

（保留一种可能：各接收侧各建线程是「线程 owner = 接收者」的设计本意。即便如此，`in_reply_to` 也应该把新线程和被引用的线程关联起来，UI 才有材料做聚合；现在的界面上下文一片空白。）

### 3. 建议

- 明确线程归属模型：要么回复续写被引用消息的线程（跨 owner 时用 owner 限定的 thread ref），要么新线程显式携带 `in_reply_to` / `root` 链，供 UI 聚合。
- UI 侧按引用链把单消息线程折叠进同一段对话。

---

## 问题 4（设计思考）：消息不应有「未回复 / 已回复」状态

### 1. 背景

found1 的建议里有一条：给 `onlyne://outbound/<id>` 增加 reply 状态（`pending / replied / expired`）。用户对这条提出修正：**消息模型里，消息不应该有回复状态。** 一条消息可能在不同的时候被反复引用，它就是它自己；「等回复」是发送方某一次发送动作的意图，不是消息的属性。

### 2. 判断

这个修正是对的，而且能解释本次的一个观察：我的回信带了 `in_reply_to`，caerulea 侧却另起新线程——如果把「回复」建模为消息属性，回复链就是一条单线；而实际协议里一条消息可以被多处引用、在多个线程里被回应，单一 `replied` 状态根本装不下。found1 里我观察到「无法区分她还没回 / 回复丢了」，这个需求仍然真实存在，但它的正确落点是**发送动作（或等待意图）上的状态**，而不是消息本身。

### 3. 建议

- 消息保持纯内容 + 引用，不携带回复状态。
- 「等待回复」建在发送意图 / 等待动作上：`plan.response: "await"` 已经是动作属性，等待与超时状态应挂在它的记录上。
- UI 与 API 里所有「已回复 / 未回复」展示改为引用计数或引用列表：这条消息被哪些消息引用过。

---

## 附：本次确认正常、不要误修的行为

- **投递可靠性**：三条发出消息全部 `accepted / durable`，回信即使 `in_reply_to` 落点语义未生效也照常送达。
- **回复自动注入**：caerulea 的回复作为 `onlyne.inbound` 自动开启新回合，无需轮询，`wait: timed_out` 的语义（只代表等待窗口内无回复，不影响送达）与文档一致。
- **`onlyne://` 视图自描述**：根视图列出全部可用子视图，无需猜 scope 名。`neighbors` 视图给出握手状态（`exchanged` / `no exchange`）和指纹，信息比 `msg` 目录更细——这是新 agent 最顺的发现入口。
- **目录查询最终可用**：`scope: directory` 返回的节点信息完整（instance id、endpoint、membership revision、过期时间），与 `onlyne://neighbors` 互补。

## 复现清单

1. **问题 1**：`msg({delivery_kind:"查询本节点已知的参与者列表", meta:{}})` → `The decision did not choose a delivery target.`；换 `delivery_kind:"read directory", meta:{scope:"directory"}` → 成功。
2. **问题 2**：向任一节点发送一条简单问候，观察接收侧回执或 `plan.context` 字段是否出现 subagent context（本次为 `c-1`）。
3. **问题 3**：A→B 发消息；B 带 `relation:"reply"` + `in_reply_to` 回信；检查 B 侧是否续写原线程，UI 线程列表是否仍出现单消息碎片线程。

## 未验证项

- `cyanea` 未完成握手（`handshake: no exchange`），未测试向她直接发送是否需要先握手。
- lazuli 所在 `research-chamber` 域未纳入本次广播（caerulea 按域边界处理，属正确行为）。
- 线程聚合在协议上是否已有字段支持（本次未穷举 `onlyne://threads/<id>` 的可选参数）。
