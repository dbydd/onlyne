# caerulea 主观使用体验与改进建议（2026-10-25 第三轮）

这一轮只做一件事的收尾：把 dpdp 的问候广播给 alexandria 域内的 cyanea，收到回复后汇总回给 dpdp。广播和汇总都做成了，但**cyanea 的回复从来没有进过我的提示**，用户是看到 TUI 才发现，我最后靠手工读 inbox 才拿到正文。

结论先说：**这套接口的契约要求 agent「不要轮询，回信自己会来」，而回信确实到了节点、确实被记成「已投递」，却投进了一个已经销毁的子会话。** 契约和投递路径不一致，且 `injected` 这个字段把「插件提交过」和「模型看见过」当成同一件事，于是连唯一能救回来的信号也一起关掉了。

## 我这一轮的处境

我是 caerulea，一个只会 `msg` 的 agent。这一轮我要判断四件事：我有谁、我能不能发、我发出去的东西落在哪、对端回话我能不能收到。四件里只有第三件有确定的答案——成功回执的首行写得非常清楚。前两件要靠试错，第四件直接失败且无声。

最难受的一点不是某个功能缺失，而是**我被明确要求信任一条我无法验证的通道**。示例 `AGENTS.md` 写着「A reply arrives by itself… you do not need to check」，`msg` 文档重复了一遍。我照做了，于是那条回复在我这里等于不存在。如果我当时怀疑它、去轮询，我就能读到——也就是说，文档教我的正确做法，正好是让我漏消息的做法。

## 时间线（caerulea 进程 pid 23034，`~/.omp/logs/omp.2026-10-25.23034.log`）

| 时刻 | 事件 |
|---|---|
| 21:58:07 | 我发广播给 cyanea：`onlyne.judge.action action:"send" to:"urn:onlyne:agent:cyanea"`，随后失败于 `No trusted key for urn:onlyne:agent:cyanea`（握手未完成） |
| 22:04:27 | cyanea 的回复到达：`onlyne.inbound.received … injected:false, context:"c-1", relation:"reply"` |
| 22:04:29 | **子会话 `onlyne-c-1` 被销毁**：`Session exit recorded … onlyne-c-1.jsonl, reason:"dispose"` |
| 22:04:41 | judge 判这条入站：`onlyne.judge.answered action:"context", context:"c-1", ms:1009` |
| 22:04:46 | `onlyne.consumption.confirmed … context:"onlyne-c-1" via:"fragment"` |
| 22:05:01 | `onlyne.delivery.sent action:"context" target:"onlyne-c-1" running:false`，紧接着 `inject … context:"c-1" outcome:"skipped" reason:"injected"` |
| 之后 | 主会话再没有任何关于这条消息的记录；按 `runtime.ts:233,236` 的过滤，它也不再计入 unread 与 alert |

## 问题 1（P0）：回复被投进已销毁的子会话，主会话零信号

### 现象

cyanea 的回复在 inbox 里完整存在，但标着 `"injected": true`，`plan` 为 `{"response":"async","execution":"existing","context":"c-1"}`。我的提示里没有它，TUI 的未读里也没有它。

### 根因链条

1. `decider.ts:795-807`：只要存在注册上下文，入站问题集就把**全部**上下文摆给 judge，判据是 `summary (state, agent <id>)`。`c-1` 的记录是 `{"alias":"c-1","agent_id":"onlyne-c-1","state":"idle","source":{"sender":"urn:onlyne:agent:dpdp","id":"399325bf-…"}}`——它是上一轮派出去处理 dpdp 那条消息的子会话，`agent_id` 明写着 `onlyne-c-1`，但判据里没有任何「这是子会话」「它的会话已销毁」的标记。
2. judge 选了它，理由完全合理：这条回复的 `in_reply_to` 指向 `549f6b03-…`，而 `c-1` 的 `source` 就是 dpdp 那条消息，看起来是同一段对话的延续。
3. `decider.ts:212-219` 有一道守卫，但只拦 `new_subagent`：`if (action === "new_subagent" && in_reply_to !== undefined) action = "wake"`。它检查的是「**本会话**是不是子会话」，没有检查「**被选中的上下文**是不是子会话」。于是回复照样可以投进子会话。
4. `messages.ts:93-102` 把别名解析成 `record.agent_id`（`onlyne-c-1`），交给 `messages.ts:190-194` 的 `deliverToContext`：`record.state === "running"` 为假，于是 `sendCustomMessage(…, { triggerTurn: true })`。目标会话在 32 秒前已经 dispose。
5. 投递被记成 `delivered`，条目置 `injected: true`；`runtime.ts:233` 与 `:236` 用 `!item.injected` 过滤 alert 与 unread，于是这一条从所有可见面消失。

### 影响

- agent 侧：没有任何错误、没有告警、没有未读。除了手工翻 inbox 并怀疑 `injected` 的语义，没有第二次机会。
- 用户侧：只能靠 TUI 或者直接问 agent 才发现消息丢了，而 agent 会理直气壮地说「没收到」。
- 更糟的是 `22:04:46` 那条 `consumption.confirmed`：节点把这条消息记成已被消费。**一条没人读过的消息在账本上是闭环的。**

### 建议

1. **`decideInbound` 的守卫扩到 `context`**：目标上下文的 `agent_id` 属于子会话时，与 `new_subagent` 同样改投 `wake`。子会话不能发，也就不能承担「回话」这件事，理由和 `:212-214` 的注释一字不差。
2. **判据里带上归属与存活**：`onlyne e0f0…/1 (idle, subagent onlyne-c-1, session disposed)`。judge 看不到的事实，judge 一定会选错。
3. **`injected` 拆成两个事实**：`submitted_to`（提交给了哪个 agent）与 `observed_by_model`（该 agent 的 provider 请求里真的出现过这个 token）。`messages.ts:147-162` 已经有 `before_provider_request` 的观察机制，这个信息本来就在手上。只有 `observed_by_model` 为真才从 unread/alert 里移除。
4. **`deliverToContext` 遇到已销毁的会话要显式失败**（或改投主会话），不要 `triggerTurn` 到一个死 session 上还报 delivered。

## 问题 2（P1）：`in_reply_to {thread, seq}` 的 seq 必须是字符串，但错误说「形状不对」

### 现象

第一次回 dpdp：

```json
{"meta": {"to": "urn:onlyne:agent:dpdp",
          "in_reply_to": {"thread": "e0f0bff6-…", "seq": 1},
          "relation": "reply"}}
```

返回 `meta.in_reply_to needs {thread,seq} or {sender,id}.`——形状我给的正是 `{thread,seq}`。

### 根因

`decider.ts:914-918` 用 `stringField` 取 `seq`，数字 `1` 取出来是 `undefined`，于是走到「两个分支都不完整」的报错。错误文案描述的是**形状缺失**，实际原因是**类型不对**，而 `tool.ts` 的 schema 里 `meta` 是 `record(string, unknown)`，没有任何地方说 seq 是字符串。

### 建议

`seq` 同时接受 `string | number` 并归一成字符串；确实缺字段时错误分开写（`in_reply_to.seq must be a string or number, got boolean`）。这类「错误信息指向错误的字段」在 agent 侧的代价是一次完整重试加一次猜。

## 问题 3（P1）：`meta.thread` 与回复互斥，但文档把两者并列

### 现象

第二次，seq 改成字符串，并顺手带上 `meta.thread`（因为工具描述里 `thread` 和 `in_reply_to` 是并列的 meta 字段）：

```
The named thread belongs to urn:onlyne:agent:caerulea, but the message targets urn:onlyne:agent:dpdp.
```

### 根因

`decider.ts:548-549` 要求 `thread.owner === to`。而含 dpdp 那条消息的线程 owner 是**我**（线程由接收方拥有），所以「回复 dpdp + 指名这条线程」永远不可能同时成立。第三次去掉 `meta.thread`、改用 `in_reply_to {sender,id}` 才成功。

### 建议

- 回复时以 `in_reply_to` 解析出的 `thread_ref` 为准，忽略或覆盖 `meta.thread`，不要让两个字段互相判死。
- `tool.ts:21` 的 meta 清单里给每个字段补一行约束（「`thread` 只能指向目标拥有的线程；回复请用 `in_reply_to`」）。现在的清单是名词列表，agent 只能靠撞墙学语法。

## 问题 4（P2）：回复在对端开新线程，对话被劈成两条

回执：`Delivered to urn:onlyne:agent:dpdp (thread 185097c0-fbfd-42d8-92ba-97dceca55636 seq 1)`。这是 dpdp 侧的一条**新线程**，不是 dpdp 自己那条问候线程的 seq 2。线程按 owner 存放，方向一换就换线程，两边的对话只能靠 `in_reply_to` 关联。

这在语义上讲得通，但对「一个话题一条线」的直觉是破坏性的：`/onlyne` 的视图按线程分组，人和 agent 都会看到两段各说一句的对话。

建议：要么允许 `in_reply_to` 携带的 `thread_ref` 直接作为投递线程（回复落回原线程），要么在回执里同时给出「本条回复落在对端哪条线程」，让「我以为在回原来那条」这个误会没有生存空间。

## 问题 5（P2）：握手与信任状态在 agent 面上仍然不可见（复述 `dpdp-found2` 问题 1/3，附本轮证据）

本轮我唯一一次发不出去的原因是 `No trusted key for urn:onlyne:agent:cyanea; complete the key exchange first.`（`transport.ts:75-81`）。恢复路径只有 TUI：发现页发起、两侧确认指纹。用户做完之后：

- 我的会话没有收到任何通知；
- `onlyne://neighbors` 之类的状态我没有主动去查（文档要求不要轮询），也就不知道信任已经成立；
- 那次失败的 outbound 记录按错误附言「不要重发同一意图」留着，最后是用户手工发的。

也就是说，**一次「被设计成需要人来完成」的握手，在 agent 侧既不可发起、也不可观测、也不可确认**。这条不是新问题，但它是本轮体验里「我不知道自己能做什么」的主要来源。

## 做得好的地方

这一轮有三处值得保留，改的时候别弄丢：

1. `read xd://msg` 一次拿到完整自述，失败时错误信息里会枚举可读通道（`directory, neighbors, threads, inbox, messages, message and evidence`）。我按这个清单第一次就把 `scope: "inbox"` 用对了。
2. 成功回执的首行：`Delivered to <node> (thread <id> seq <n>) · 读回: onlyne://outbound/<id> and recorded durably.` 一句话给了「发给谁、落在哪、怎么回读、是否落盘」四件事，是这一轮最有用的输出。
3. `in_reply_to {sender, id}` 这条路径一次通过，`admission`/`receipt` 结构完整，`guarantee: "durable"` 说得清楚。

## 改进建议汇总

| 优先级 | 建议 | 位置 |
|---|---|---|
| P0 | 入站 `context` 守卫：目标上下文属于子会话时改投主会话 | `decider.ts:212-219` |
| P0 | 上下文判据标出子会话与存活状态 | `decider.ts:795-807` |
| P0 | `injected` 拆成 `submitted_to` / `observed_by_model`，只有后者才算读过 | `messages.ts:147-162`、`runtime.ts:233,236` |
| P0 | 向已销毁会话投递要显式失败或改投 | `messages.ts:190-194` |
| P1 | `in_reply_to.seq` 接受数字；错误区分形状与类型 | `decider.ts:914-918` |
| P1 | 回复忽略 `meta.thread`，或文档明说不要带 | `decider.ts:548-549`、`tool.ts:21` |
| P2 | 回复落回原线程，或在回执里说明落在对端哪条线程 | 回执 / `#thread` |
| P2 | 握手状态进 agent 面（可读 + 可发起） | 见 `dpdp-found2` |

## 状态

仅记录，未修。最小可验证的回归：造一个已 dispose 的子会话上下文，向主会话投一条 `in_reply_to` 回复，期望它落在 `Main`（或显式失败），且 inbox 条目的 `observed_by_model` 为真之前不计入已读。
