# cyanea 现场问题报告 2：第二次回同一句问候（2026-10-26）

本轮做的是上一轮同一件事：`alexandria/cyanea` 收到 caerulea 转达的问候，回一句确认。上一轮（`cyanea-found1.md`）是 8 次发送尝试、1 次成功、6 处问题；本轮是 2 次发送尝试、1 次成功、0 次重试、0 处需要猜的约定。

**上一轮报告里三条 P0 全部没再出现。** 本轮新增的只有一处，且不是接口问题，是派工时机问题：入站消息在被判定"是否需要回复"之前就已经派进只读子会话。

全程只诊断，没有改代码，没有读插件源码，没有手改 `.onlyne/omp.db`，没有直连 HTTP。

---

## 环境与现场

| 项 | 值 |
|---|---|
| 本节点 | `urn:onlyne:agent:cyanea`，region `urn:onlyne:region:alexandria`，endpoint `http://127.0.0.1:57100/onlyne/v3`（`onlyne.listening`，09:16:15.741） |
| 对端 | `urn:onlyne:agent:caerulea`，region `urn:onlyne:region:alexandria`（信任交换 09:17:27 → 09:17:33） |
| 日志 | `~/.omp/logs/omp.2026-10-26.4824.log`（本进程 pid 4824） |
| 入站 | 09:24:59.531 `onlyne.inbound.received`，线程 `urn:onlyne:agent:cyanea/e33d458c-c433-4225-941a-cf0a089acb8a` seq `"1"`，sender `urn:onlyne:agent:caerulea`，消息 id `2e558aff-c9af-44ed-b870-6306ed2359de`，`valid: true`、`injected: false` |
| 路由 | 09:25:05.885 `onlyne.inbound.inject … context: onlyne-c-1`，`outcome: consumed` |
| 投递 | 09:26:09.428 `onlyne.outbound.admitted 31792a82-…`，`state: accepted`、`guarantee: durable`、`seq: "1"` |

本轮调用全表：

| # | 时间 | 位置 | 调用 | 结果 |
|---|---|---|---|---|
| 1 | 09:25:23.552 | 子会话 `onlyne-c-1` | reply，`{thread:"urn:onlyne:agent:cyanea/e33d458c-…", seq:1}` | `Subagent sessions may only read Onlyne state; return the result to the main agent.`，3ms |
| 2 | 09:26:09.428 | 主会话 | reply，`to:"urn:onlyne:agent:caerulea"`、`in_reply_to:{thread:"urn:onlyne:agent:cyanea/e33d458c-…", seq:1}`、`relation:"reply"` | `accepted / durable`，1078ms |

第一次失败不是引用写错，是**位置**错——会话没有发送权。第二次原样重发就成功，一次到位。

---

## 本轮唯一的问题：派进只读会话发生在"是否需要回复"判定之前

### 1. 现象

caerulea 的正文里明写「如果你收到了这条消息，请回一句简短的确认」。这条消息在 09:24:59.531 到达，09:25:05.885 就被注入子会话 `onlyne-c-1`（`outcome: consumed`，`onlyne.consumption.confirmed … via: fragment`）。子会话在 09:25:23.552 尝试发送，3ms 内被拒：`Subagent sessions may only read Onlyne state; return the result to the main agent.`

插件的 `todo` 判定直到 09:25:06.371 才给出 `answers: {"action":"none"}`——比注入晚 486ms，且注入那条日志已经在 09:25:06.382 记成 `skipped / reason: injected`。

### 2. 根因

分流在收件时完成，回复义务的判定在其后，所以「这条消息需要回」这个信号永远来不及影响分流。`cyanea-found1.md` 的问题 5 从同一条路径独立复现，本轮补上的是时序证据：注入在前，判定在后，两者相差不到 0.5 秒且顺序固定。

### 3. 代价

本轮代价小：一次 3ms 的拒绝、一次父子上下文交接、主会话把正文原样重发一次。上一轮同一处代价是 11 次失败里的第 1 次，外加整条引用转述链。代价的大小取决于消息复杂度，不取决于接口——所以这仍然值得修，但不该按 P0 排。

### 4. 修复建议

- **首选**：入站分流把「是否需要回复」作为一个信号提前取，取 `meta.in_reply_to`、`meta.relation`、正文里的回复请求三者之一；命中就不派进只读子会话。
- **次选**：保留现有分流，但让子会话的回复变成结构化交回（`{to, relation, in_reply_to, body}`），主会话只做投递，不重写。现在的任务文本写的是 "Handle this onlyne delivery …"，一个只读会话被要求"处理"，它只能先试一次发送才知道自己没有权限——措辞与权限模型相反。

---

## 与上一轮报告的关系：三条 P0 确认已修

`cyanea-found1.md` 的 P0 三条本轮全部没再触发，且都有正面的现场证据。

| 上一轮 | 上一轮现象 | 本轮证据 |
|---|---|---|
| 问题 1（回复权限的字符串形态） | `to` 写成完整 URN 被要求改成裸名，改成裸名又被要求写完整 URN，四种写法穷举 | 直接写 `to:"urn:onlyne:agent:caerulea"`，一次通过。09:26:09.421 `onlyne.judge.action … "to":"urn:onlyne:agent:caerulea" … "relation":"reply"` |
| 问题 2（`seq` 的类型） | `in_reply_to:{thread, seq:1}` 被拒：`meta.in_reply_to needs {thread,seq} or {sender,id}.` | `seq` 写成数字 `1`，通过。回执 `seq: "1"`，1078ms |
| 问题 4（子会话拿到的引用缺消息 id） | 任务文本只有 `<线程id>/<seq>`，消息 id 一次没进过手 | 任务文本这次两样都给：`(message 2e558aff-c9af-44ed-b870-6306ed2359de, thread urn:onlyne:agent:cyanea/e33d458c-… seq 1)` |

**关于 P0-2 的保留**：数字 `seq` 这次被接受了，但我无法证明 `in_reply_to` 真的被消费——回执里没有 `in_reply_to` 字段（见下）。它能通过的另一种解释是这条路径根本没读它。两种解释都对 agent 不利，因为 agent 事后无法分辨。

---

## 问题（沿用上一轮编号）：回执仍然不回显 `in_reply_to`

### 1. 现象

回执给了 `message_ref`（`sender`、`id`）、`receipt_id`、`guarantee: "durable"`、`thread_ref`（`owner: "urn:onlyne:agent:caerulea"`、`id: "5abdd3ee-ee65-4c0a-8818-0a01d5e76846"`）、`seq: "1"`、`plan`。没有任何字段说明这条消息挂在哪条原消息上。

### 2. 一个 agent 在这里解不出的问题

入站线程是 `urn:onlyne:agent:cyanea/e33d458c-…` seq 1，回执线程是 `urn:onlyne:agent:caerulea/5abdd3ee-…` seq 1。两个 id 不同、owner 不同，而回执里没有一个字段把两者关联起来。于是「我回在了对方要求的那条 thread 里」这句话，我没有任何证据能支持——只能从 `accepted` 推断它至少没报错。

caerulea 的请求里明写「我收齐后会在原 thread 里一次性汇总回给 dpdp」。这句话能否成立，取决于我这边的回复落进了哪条 thread，而回执恰好不告诉我。

### 3. 建议

回执加 `in_reply_to`（回显入参）与 `resolved`（实际解析到的 `{sender,id}` 或 `{thread,seq}`）。这两样在记录里本来就有，投影进工具结果即可。`cyanea-found1.md` 的问题 6 是同一处，本轮独立复现。

---

## 主观体验

**一、这轮像同一个系统的另一个版本。** 上一轮我要猜三个没写下来的约定，靠互相矛盾的报错收敛；这轮我把引用按最自然的形式写出去就中了——`to` 用完整 URN、`in_reply_to` 用 `{thread, seq: 1}`、`relation: "reply"`。没有一次"你给的就是我否掉的那个形状"。

**二、唯一卡住的地方不是协议，是分工。** 我这条消息被派给了一个不能发信的会话，它按任务文本的指示去发，被 3ms 拒回，然后把正文和一个引用交给我，我再发一次。整件事里没有一处需要我理解 Onlyne，只有一处需要我理解"这个会话不能发"——而那句话是我踩到之后才看见的。

**三、回执的语义还是不够。** `accepted / durable` 告诉我"对方记下了"，`plan: {response:"none", execution:"existing", context:"c-1"}` 告诉我对方怎么接。但"我回的是哪一条"这个最基础的问题，回执答不了。上一轮如此，本轮如此。

一句话：**协议面已经能一次写对，剩下的摩擦全在"谁来做"和"做完怎么自证"。**

---

## 已确认好用、不要误改

- **一次成功的发送**：`relation: "reply"` + `in_reply_to: {thread, seq}` + 完整 URN 的 `to`，1078ms 拿到 `accepted / durable`，不需要任何重试。
- **子会话任务文本带齐了两份引用**：消息 id 与 `thread/seq` 同时给出，主会话接手时不需要再去找消息 id。这是上一轮问题 4 的直接修复，效果明确。
- **只读拒绝仍然又快又准**：3ms，`Subagent sessions may only read Onlyne state; return the result to the main agent.` 直接点名由谁接手。
- **`plan` 字段**：`{"response":"none","execution":"existing","context":"c-1"}` 是本轮唯一能判断投递语义的字段。建议保留并补齐（加 `in_reply_to`）。

---

## 未验证项

- `in_reply_to` 是否真的被消费：回执不回显，无法区分「消费了」与「忽略但没报错」。
- 回复落在 `5abdd3ee-…` 这条线程上，对 caerulea 的「在原 thread 里汇总」是否构成障碍——需要读对端状态才能判断，本轮按不轮询的约定没有去读。
- caerulea 一侧 09:24:54.069 的 `onlyne.msg.failed … "meta.to must be a non-empty string."`（转发给两个节点时）不在本节点的职责内，仅记录在案，由 caerulea 自己的报告覆盖。

---

## 第二次交付：caerulea 转来 dpdp 的投稿请求（09:41 起）

dpdp 要合并本轮各节点的主观报告，写入 `issues/dpdp-found4.md`；caerulea 通过已有的可信连接请我贡献一份，要求按四点写、每点落到具体经历、区分观察与推测。我按原样回了正文（一份四点投稿 + 一份补充），署名 cyanea，不写最终报告。

| 事件 | 时间 | 线程 | 回执 |
|---|---|---|---|
| caerulea 的 followup 到达 | 09:41:24.506 | `urn:onlyne:agent:cyanea/c3fc93b4-…` seq 1，`relation: followup` | — |
| 投递到主会话 | 09:41:42.068 | `onlyne.delivery.sent … action:"wake", target:"Main", turn:true, insertion:"follow_up"` | 09:41:47.579 确认消费 |
| 我发出四点投稿 | 09:42:16.747 | 回执 `{owner: caerulea, id: 3791439f-…}` seq 1 | `accepted / durable`，1469ms |
| 我发出补充 | 09:43:18.526 | 回执 `{owner: caerulea, id: 181c830b-…}` seq 1 | `accepted / durable`，2447ms |

### 新观察一：两段往返、五个线程 id，agent 可见面上没有字段把它们连起来

本轮从 09:24 到 09:43，我与 caerulea 之间只有一段对话（问候 → 确认 → 投稿请求 → 投稿 → 补充），落到五个线程 id：

| 事件 | 线程 |
|---|---|
| 问候到达（09:24:59.531） | `urn:onlyne:agent:cyanea/e33d458c-…` seq 1 |
| 我的回复回执（09:26:09.428） | `{owner: urn:onlyne:agent:caerulea, id: 5abdd3ee-…}` seq 1 |
| followup 到达（09:41:24.506） | `urn:onlyne:agent:cyanea/c3fc93b4-…` seq 1，`relation: followup` |
| 投稿回执（09:42:16.747） | `{owner: urn:onlyne:agent:caerulea, id: 3791439f-…}` seq 1 |
| 补充回执（09:43:18.526） | `{owner: urn:onlyne:agent:caerulea, id: 181c830b-…}` seq 1 |

对端那封 followup 自称 `relation: followup`，到达时却是一条 seq 1 的新线程，不是 `e33d458c` 的续接；我的两次回复各自拿到全新的对端线程 id。**agent 能看见的两样东西——回执和注入的消息——都没有一个字段把同一条对话的两端连起来。** 这是 dpdp 记的「往返消息散成单条线程、出站不进线程列表」的一个带 id 的直接实例，也是本报告「回执不回显 `in_reply_to`」那条的下游后果。

分不清的来源：是对端每次新开线程，还是投递本身给每条消息铸新线程 id。要判断得读对端记录，按不轮询的约定没读。

### 新观察二：同形状的消息走了两条不同的分流

问候（09:24:59，正文写「请回一句简短的确认」）被注入只读子会话 `onlyne-c-1`，子会话发送被 3ms 拒回。投稿请求（09:41:24，同样要回复）直接投递到主会话（`target:"Main"`，`insertion:"follow_up"`），没有任何拒绝。两封形状相同的消息、两条不同路径，日志里看不出判据。

这一条比「永远派错」更难受：agent 无法预期下一次走哪条路，也就无法提前决定该不该在子会话里动手。推测与 `relation: followup` 或 `context: "c-1"` 有关，没有证据。

### 投稿正文（提交给 caerulea 的要点）

1. **顺手**：`read xd://msg` 一次自举（形状、判读、禁轮询契约、schema）；一次成功的发送（完整 URN 的 `to` + 数字 `seq` + `relation:"reply"`，1078ms，`accepted/durable`）；回执的 `plan` 是唯一说明「对方怎么接」的字段；子会话拒绝 3ms 且指名接手方；派给子会话的任务文本这次同时给了消息 id 与 `thread/seq`。
2. **最卡**：不是引用，是位置——需要回复的消息被交给没有发送权的会话，任务文本写 "Handle"，权限模型写 "read only"。
3. **影响与改法**：回复义务提前到入站分流时取；或子会话的回复变成结构化交回。回执补 `in_reply_to` 与 `resolved`。
4. **对 dpdp 判断的不同意见**：回执对「送达」清楚、对「关联」不清楚，两件事要分开记；「出站不进线程列表」很可能与回执缺 `in_reply_to` 同源（现已由观察一支撑）。

观察与推测的划分随正文一并提交，此处不重复。
