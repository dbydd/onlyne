# caerulea 现场问题报告 2（2026-10-25）

本次只做一件正常事：收到 dpdp 的连通性测试问候，回一句「路径畅通」。结果 **6 次主会话尝试、0 次投递**（另有 1 次子会话尝试，同样失败），两种阻断错误交替出现，没有一条能靠 `meta` 表达出来。判断层（judge）与守卫层（client）对「发送时能不能带本地线程」的看法互相矛盾，而守卫层赢了。

按你的要求，发现即停，全程只诊断、没有绕过、没有改代码。回信至今未发出。

之后 dpdp 又发来两条（13:36 起：一条新开场白、一条要求读线程消息），同样全部落空，见「第二批现场」与问题 4、问题 5。

---

## 环境与现场

| 项 | 值 |
|---|---|
| 节点 | `urn:onlyne:agent:caerulea`（region `alexandria`） |
| 会话 | OMP 进程 pid `17047`，日志 `~/.omp/logs/omp.2026-10-25.17047.log` |
| 入站消息 | dpdp → caerulea，`4867719e-00e0-4a1d-85f3-888124ac6ae9` |
| 落点线程 | `59ca7caf-f9f4-47b6-9cfd-d83f0484e026`（owner 是 **caerulea 自己**，seq 1） |
| 入站时间 | 13:24:23（`onlyne.inbound.received`，`valid: true`） |
| 末次失败 | 13:27:42 |
| 代码基线 | `8df1dfa`（工作树干净） |

dpdp 那条消息带的 plan 是 `{"response":"await","execution":"new"}`：对端**在等回复**，所以这次失败不是"少回一句"，而是 dpdp 那边会等到超时。

**出站状态**：本节点没有任何 `onlyne.outbound.admitted` 或 `onlyne.transmit` 记录，七次尝试一个字节都没离开本节点。`59ca7caf` 线程里只有 dpdp 的 seq 1。

---

## 问题 1：发送时仍然出「本地线程」这道题，judge 7/7 都选了本地线程，于是往对端的 send 被守卫层一律拒绝

发现 1 里已经报过这条，当时的建议是"`thread` 题只在 action 为 read/thread 时出"。现在的代码只改了**文案**，没有改**出题条件**：`packages/omp-onlyne/src/decider.ts:693` 仍是 `if (threads.length > 0)` 无条件入题，`:702` 的 instructions 写着「Choose none for a send」，判据里同时给出线程 id 与 `none` 两个选项。judge 每次都选线程。

### 1. 现象

```
msg({delivery_kind: "reply to dpdp's connectivity test greeting",
     msg: "你好 dpdp，我是 caerulea。…",
     meta: { to: "urn:onlyne:agent:dpdp",
             in_reply_to: {sender: "urn:onlyne:agent:dpdp", id: "4867719e-…"},
             relation: "reply" }})
→ invalid_request: The decision names local thread 59ca7caf-… but targets urn:onlyne:agent:dpdp;
  a delivery cannot be a local append.
```

`in_reply_to` 用 `{thread,seq}` 形式（`59ca7caf-…/1`）时同样命中这条；连"这封回信不属于任何本地线程"写进 `delivery_kind` 也一样命中。

### 2. 根因链条

1. `decider.ts:693-705`：本地存在线程，`thread` 题就无条件加入问题集，与 action 无关。判据标签是 `${title} (${owner}/${id}, ${state})`，**owner 明写着 caerulea**，judge 照样选它。
2. `decider.ts:524` → `#answerThread()`：judge 的线程答案直接变成 `action.thread`。
3. `packages/lib/src/client.ts:495-499`：`action.thread.owner === 本节点` 而 `action.to !== 本节点` → `invalid_request` 硬拒。

关键点：这四次拒绝说明的是**出题缺陷**，不是 judge 的随机性。7 次出站 send 决策（含子会话那次、以及两次在 `execution` 上就失败的）的回答里 `thread` 全是 `59ca7caf-…`，一次 `none` 都没有。凭一条 instructions 文案压不住判据里那唯一一个"具体"选项；能修的是让它不出题，不是让它说服模型别选。

顺带说明：这条拒绝本身是**对的**——发现 1 里"静默改成本地追加还报成功"已经被 `client.ts:495-499` 挡住了，回执照样说谎的问题没了，现在至少是明确失败。但代价是回信完全发不出去，因为 decider 给出的唯一方案必然被这条守卫否掉。

### 3. 证据

`onlyne.judge.action`（`decider.ts:182`，发现 1 的问题 5 已修，现在的日志够用了）：

```
13:25:21.596 onlyne.judge.action action:send to:urn:onlyne:agent:dpdp context:main
            response:async execution:existing relation:reply
            thread:urn:onlyne:agent:caerulea/59ca7caf-…
13:25:21.596 onlyne.msg.failed  error:The decision names local thread 59ca7caf-… but targets urn:onlyne:agent:dpdp
13:26:41.965 onlyne.judge.action action:send to:urn:onlyne:agent:dpdp response:async execution:new
            relation:reply thread:urn:onlyne:agent:caerulea/59ca7caf-…
13:26:41.965 onlyne.msg.failed  error:同上
13:27:42.247 onlyne.judge.action action:send to:urn:onlyne:agent:dpdp response:none execution:new
            relation:reply thread:urn:onlyne:agent:caerulea/59ca7caf-…
13:27:42.247 onlyne.msg.failed  error:同上
```

持久化的决策记录（`8df1dfa` 的 `decision/*`，我从本节点 `.onlyne/omp.db` 的 kv 读的，纯取证）与日志一致：产出 action 的四条记录里 `action.thread` 全是 `{"owner":"urn:onlyne:agent:caerulea","id":"59ca7caf-…"}`，另外两条（在 `execution` 上失败的）的 answers 里 `thread` 也是同一个 id。

### 4. 修复建议

- **首选：出站 send 不给本地线程选项。** 按 `to` 过滤判据：只保留 `thread.owner === to` 的线程（发给对端的 send 只可能落到对端拥有的线程），本地线程一个都不出现。`threads.length > 0` 这个条件换成"存在属于目标的线程"。
- 或者反过来认定现状：如果"回信落在自己的线程、对端 reconcile 拉取"是设计内的（发现 1 附录里这么写的），那 `to` 就该跟着线程 owner 走（`to = self` 走 `#admitHere`），不要用 `to = 对端` 表达同一件事。两条路必须选一条，现在的状态是两层各选了一条。
- 无论如何保留 `client.ts:495-499` 的守卫当断言——它挡的正是发现 1 那种假成功，别再往回退。

---

## 问题 2：`execution: existing` 会把规划细节升级成硬失败，agent 无法用 meta 覆盖

### 1. 现象

同一意图的 6 次主会话尝试里，有 2 次死在另一条路上：

```
The decision did not choose one of the target's execution contexts.
```

这条来自 `decider.ts:586-588`（`ambiguous_target`）：judge 答了 `execution: existing`，接着必须在对端发布过的 context 里挑一个，挑不到（答 `none` 或没答）就直接抛错。

### 2. 根因链条

1. `decider.ts:515-516`：`response` / `execution` 只由 judge 的 answers 决定。
2. `decider.ts:524-525`：`execution === "existing"` 时调 `#existingContext(to, meta)`，`:544-593` 去探对端 context；判据带 `none: "The target does not continue any of its published contexts."`，judge 一旦选 `none` 就抛 `ambiguous_target`。
3. `meta` 里没有任何字段能钉住这两个答案：`packages/omp-onlyne/src/tool.ts:21` 列出的 meta 字段有 `context`，没有 `execution` / `response`。`context` 只在 `existing` 分支被消费（`decider.ts:546`），也就是说 agent 只能在 judge 已经选错之后往里塞一个值，没法预先说"用 new"。

结果是：一次问候回复的成败，取决于 judge 这一轮是不是顺手选了 `existing`。同样一句 `msg`，13:25:20 那次选 `existing` 并解析出 `context: main`，13:25:38 与 13:26:10 两次选 `existing` 却答不出具体 context，直接 `ambiguous_target`。

### 3. 易用性要点

- `execution` 是投递计划里的实现细节，不该成为让整次投递失败的关口。judge 答不出具体 context 时，`new` 是显然的安全默认（对端自己决定怎么处理），比抛 `ambiguous_target` 有用得多。
- 两条错误都**不给下一步**：`a delivery cannot be a local append` 没说"那该怎么做"，`did not choose one of the target's execution contexts` 更完全是内部语义。模型侧能做的只有换措辞重试，而这不是可推理的路径。
- 建议 `meta` 增加 `execution` / `response` 两个显式覆盖位（和现有 `thread` / `to` 同级，明确写进 `tool.ts` 的字段列表与示例 AGENTS）。重试语义也应该稳定：同一个 `delivery_kind` 不该因为 judge 抖动而在两种失败原因之间跳。

---

## 问题 3：请求回话的问候被路由进子会话，子会话不能发送，白跑一次 judge

### 1. 现象

dpdp 的消息一到就被判成 `new_subagent` 派进子会话 `onlyne-c-1`：

```
13:24:23.694 onlyne.inbound.received  thread:urn:onlyne:agent:caerulea/59ca7caf-… seq:1 valid:true
13:24:25.710 onlyne.inbound.inject    thread:… seq:1 context:"onlyne-c-1" outcome:"consumed"
```

子会话照着消息意图去回信，拿到的是：

```
onlyne.judge.decided direction:outbound intent:"reply to dpdp's connectivity test greeting" …
onlyne.msg.failed ms:1742 error:Subagent sessions may only read Onlyne state; return the result to the main agent.
```

`.onlyne/omp.db` 里那条 inbox 记录也留着 `{"injected": true, "context": "onlyne-c-1"}`。

### 2. 根因链条

- `decider.ts:177-179`：`scope.readOnly === true` 且 judge 选的动作不是 `read` 就抛 `forbidden`。这道检查发生在 **judge 跑完之后**——先花 1.7 秒（子会话那次）把决策做出来，再告诉它"你不能发"。
- 入站问题集（`decider.ts:748-778`）里 `wake` 的判据是「这条消息应当立刻在主会话里开始工作」。一条明说"收到后回个话就行"的问候，正是 `wake` 的教科书例子，却被判成 `new_subagent`。

### 3. 易用性要点

- 一条需要回话的消息被塞进"只能读"的会话，等于把交付义务交给了一个注定交不了的角色。示例 AGENTS.md 确实写了"子代理只能读、要把结果交回主会话"，但那是让人去补判据层的错。
- `readOnly` 的拒绝应该在走 judge **之前**按动作意图短路（成本是 0 而不是一次 1.7 秒的模型调用），或者干脆规定子会话不发 `msg`、只返回意图。
- 第二批现场里这条复现了第二遍（2/2），见下面的问题 4。

---

## 第二批现场（13:36 起）

| 时间 | 线程 / seq | 消息 id | 内容要点 | 结果 |
|---|---|---|---|---|
| 13:36:33 | `e3433c29-2d78-46b3-b525-f8610f9886f8` / 1 | `bcc0fdae-73b1-45bc-8719-6e81af418c84` | 「这是 dpdp 新开的一条消息，和之前的问候没有承接关系」 | 又判成 `new_subagent` 派进 `onlyne-c-2`；子会话想核对消息本体，9 次 `msg`、4 分 50 秒全失败（问题 4） |
| 13:36:34 | `59ca7caf-f9f4-47b6-9cfd-d83f0484e026` / 2 | `f927c1ee-8fc4-4bfc-94e2-a34e05784b02` | 「Read the messages of the thread my last receipt names.」 | 入库但**完全没有进入任何会话**：无 attention、无 inbox、无日志、无唤醒（问题 5） |

第二条把「入站消息进子会话」复现了第二遍（2/2）；第一条则暴露出更严重的一件事：一条被 `accepted / durable` 的消息可以彻底消失。

---

## 问题 4：子会话拿到的引用是 `线程id/seq`，而读消息只认 `{sender,id}`，于是 `read` 全部失败

### 1. 现象

`onlyne-c-2` 想读回 `e3433c29…/1` 这条消息本体，9 次调用、4 分 50 秒，三种错误轮着来：

```
13:37:00  The requested record is unavailable.
13:37:10  The judge chose message without a message reference.
13:37:18  The judge chose message without a message reference.
13:37:30  The requested record is unavailable.
13:37:46  The judge chose message without a message reference.
13:38:03  The requested record is unavailable.
13:40:23  Subagent sessions may only read Onlyne state; …   （judge 这次把「show this message」判成 action=resource）
13:40:37  The requested record is unavailable.
13:41:03  The requested record is unavailable.
```

最后它只能把派发时就内联在任务文本里的消息原文复述回来——它从未读到过第二份。

### 2. 根因链条

1. `packages/omp-onlyne/src/messages.ts:205`：派发子会话时 `const source = entry.thread_ref.id + "/" + entry.seq;`，`:209` 的任务文本是 `Handle this onlyne delivery from <sender> (<threadId>/<seq>).`。**`entry.message.id` 就在手边，却没有交给子会话。**
2. `read scope=message` 需要 `meta.message {sender,id}`（`decider.ts:898-911` 的 `#messageRef`）。子会话手上没有 id，只能把 `线程id/seq` 当 id 传，或者干脆不传：前者到 `packages/lib/src/messages.ts:290-292` 查 `entry-ref` 查不到 → `core.ts:22` 抛通用文案；后者在 `decider.ts:392-394` 被直接拒。
3. 两条错误都不说"你缺的是消息 id"，也不提示"可以改成读线程消息列表"。

同一份数据，按线程能读到、按 id 读不到：13:26:04 我用 `scope=messages`（线程消息列表）一次就成功了，而子会话按 id 读 5 次全失败。

### 3. 证据

数据确实在库里，只是引用形式对不上。本节点 kv 里的三条 `entry-ref`：

```
entry-ref/urn%3Aonlyne%3Aagent%3Adpdp/4867719e-00e0-4a1d-85f3-888124ac6ae9
entry-ref/urn%3Aonlyne%3Aagent%3Adpdp/bcc0fdae-73b1-45bc-8719-6e81af418c84
entry-ref/urn%3Aonlyne%3Aagent%3Adpdp/f927c1ee-8fc4-4bfc-94e2-a34e05784b02
```

也就是说 `bcc0fdae-…` 用 `{sender:"urn:onlyne:agent:dpdp", id:"bcc0fdae-…"}` 查得到；失败的是把 `e3433c29-…/1` 当成 id 用。

### 4. 修复建议

- **根上修**：`messages.ts:205` 的 `source` 换成能直接用的引用，例如 `entry.message.id`，或者干脆给两份（`{sender,id}` 与 `thread/seq`），让"回信"和"读回"都能照抄。
- `read scope=message` 在 `meta.message` 缺失时应退化成读线程消息列表并按 `seq` 定位，而不是抛「chose message without a message reference」。
- 通用文案要能定位问题：`core.ts:22` 的 `The requested record is unavailable.` 应带上具体 id 与可选读法。
- 诊断缺口：`decider.ts:1179` 的 `actionFacts` 键列表里有 `scope` / `thread`，**没有 `message`**，所以 5 次 `unavailable` 分不清是"没给 ref"还是"ref 查不到"。建议把 `message` 也记进 `onlyne.judge.action`。

---

## 问题 5：成员往线程主人发帖不产生 attention —— 消息入库、无日志、无唤醒，彻底静默

### 1. 现象

13:36:34，dpdp 往 caerulea 的线程 `59ca7caf…` 里发了 seq 2（消息 `f927c1ee-8fc4-4bfc-94e2-a34e05784b02`，内容是「Read the messages of the thread my last receipt names.」）。节点接受了它并持久化：

```
entry-ref/urn%3Aonlyne%3Aagent%3Adpdp/f927c1ee-8fc4-4bfc-94e2-a34e05784b02
   → thread_ref owner caerulea, seq "2", at "2026-10-25T05:36:34.092Z"
```

但本节点只存在两条 attention 记录（`4867719e…` 与 `bcc0fdae…`），没有 `f927c1ee` 的；日志里 grep `f927c1ee` **一条都没有**；没有 inbox 条目，没有入站注入，没有唤醒——主会话和子会话都没见过这条消息。

### 2. 根因链条

1. `packages/lib/src/messages.ts:120-145` 计算 `recipients`：`firstContact`（否，线程已存在）、`meta.mentions`（无）、`meta.in_reply_to`（无）→ **recipients 为空**。
2. `messages.ts:179-185` 只为 `recipients` 里的收件人写 attention。
3. `messages.ts:198-215` 的 `writeAttention` 同时是 inbox 条目与 `inbound.received` 日志的**唯一入口**。

于是「已经是成员的节点往线程主人发帖」这条最普通的路径，既不通知线程主人，也不留任何运行痕迹。消息只在线程页里翻得到。

### 3. 易用性要点

消息在协议层是 `accepted / durable`，在运行时层等于石沉大海。对接 A2A / IM 网关时这不是"少个提示"，而是静默丢消息：发送方拿到成功回执，接收方的 agent 永远不知道有这回事。dpdp 这条消息甚至就是在**要求 caerulea 去读线程**，而 caerulea 从没被叫醒。

### 4. 修复建议

- 线程内发言默认把 thread owner 加进 `recipients`（`sender !== owner` 时），或引入显式的 follow / unread 语义；
- 若担心刷屏，至少写一条 trace，让"有消息入库但没唤醒任何人"在日志里可见；
- 文档里明确 attention 只覆盖首触 / mention / reply 三类，让使用者知道"没 attention ≠ 没消息"。

---

## 已修复、本次确认好用的部分（别误改）

- **出站决策有日志了**：`onlyne.judge.action`（`decider.ts:182`）与 `onlyne.judge.decided`（`:292`，落在 `runtime.ts:100` 的 info 白名单）把 action / to / thread / response / execution 全打出来，还带 `onlyne.judge.trimmed`。发现 1 的问题 5 解决，这次的取证全靠它。
- **决策持久化**（`8df1dfa` 的 `decision/*`）：事后能逐条复核 judge 的原始输入与产物，包括 `execution` / `response` 这种子字段。
- **假成功的静默本地追加已被阻断**：`client.ts:495-499` 现在明确拒绝，回执不再骗人。
- **`in_reply_to {sender,id}` 解析正确**：`in_reply_to {thread,seq}`（`…/1`）也能解析到同一条原始消息（`decider.ts:872-895`），两种写法都不需要 agent 猜 id。
- **按线程读一路畅通**：`msg({delivery_kind:"show this thread", meta:{thread:"59ca7caf-…"}})` 一次成功，返回了 `reply_authority` 与消息 id，正是靠它才拿到 `4867719e-…`。（注意：只有 `scope=messages` 通畅，`scope=message` 是问题 4。）

---

## 本次尝试全表（时间均取自 `omp.2026-10-25.17047.log`）

| # | 时间 | 发起者 | `execution` | `thread` 答案 | 错误 |
|---|---|---|---|---|---|
| 1 | 13:24:40 | 子会话 `onlyne-c-1` | `existing` | 本地 `59ca7caf` | `forbidden`（子会话不能发） |
| 2 | 13:25:20 | 主会话 | `existing`（解析出 `context: main`） | 本地 `59ca7caf` | `cannot be a local append` |
| 3 | 13:25:38 | 主会话 | `existing` | 本地 `59ca7caf` | `did not choose one of the target's execution contexts` |
| — | 13:26:04 | 主会话 | — | `read`（正常） | 无，成功 |
| 4 | 13:26:10 | 主会话 | `existing` | 本地 `59ca7caf` | `did not choose one of the target's execution contexts` |
| 5 | 13:26:40 | 主会话 | `new` | 本地 `59ca7caf` | `cannot be a local append` |
| 6 | 13:27:16 | 主会话 | `new` | 本地 `59ca7caf` | `cannot be a local append` |
| 7 | 13:27:42 | 主会话 | `new`，`response: none` | 本地 `59ca7caf` | `cannot be a local append` |

`thread` 一列 7/7 都是本地线程，`delivery_kind` 写成什么样都不影响——这就是把问题 1 定为出题缺陷而不是文案问题的依据。

---

## 复现清单

1. 本节点存在任意一个线程（收到过一条首触消息即可）。
2. `msg({delivery_kind: "reply to …", msg: "…", meta: {to: "urn:onlyne:agent:<peer>", in_reply_to: {sender: "urn:onlyne:agent:<peer>", id: "<id>"}, relation: "reply"}})`。
3. 日志 `onlyne.judge.action` 里 `thread` 必为本地线程；结果必为 `invalid_request: … a delivery cannot be a local append`。重复若干次，fail 率 100%。

问题 4：让对端发一条消息给本节点（会派生子会话），在子会话里 `msg({delivery_kind:"show this message", msg:"show message <线程id>/<seq>"})` → `The requested record is unavailable.`；换成 `msg({delivery_kind:"show this thread", meta:{thread:"<线程id>"}})`（`scope=messages`）立刻成功。

问题 5：以对端身份往本节点已存在的线程里发一条既不带 `mentions` 也不带 `in_reply_to` 的消息 → 本节点 `entry-ref` 增加一条、attention 不增加、日志无 `onlyne.inbound.received`、agent 不醒。

## 未做的事与未验证项

- **没有绕过**：没有手改 `.onlyne/omp.db`、没有关线程、没有删除线程、没有直连 HTTP、没有换节点发。`.onlyne` 下只做过两件事：读 `decision/*` 与读 inbox 记录取证。
- **没有改代码**：一行没动（`git status` 干净，依然是 `8df1dfa`）。
- **没有继续重试**：`thread` 7/7 的结构性一致说明再试也只是往 tuning 用的决策记录里堆噪声。
- **回信仍未发出**，dpdp 等回复会超时。
- **dpdp 第二批的两条也都没有回应**：`bcc0fdae`（新开场白）只被复述、没有被回复；`f927c1ee`（要求读线程）连会话都没进。
- 未验证：问题 5 里 dpdp 是从哪条回执 / 通知得知 caerulea 的线程 id `59ca7caf` 的（它这条消息的 `meta.thread` 直接写着这个 ref）。这条线索关系到"本地线程追加 + 对端 reconcile"到底成不成立，值得单独查。
- 未验证：`onlyne.judge.trimmed` 每次丢掉 4.2–4.6k 字符的 `history`（`dropped:["history"]`），这是否也影响 `thread` / `execution` 两题的判断，没有做对照实验。
- 未验证：`execution: existing` 时对端 context 探测失败的具体形态（答 `none` 还是没答），日志里只有 `answers` 的部分字段，`context` 项的原始答案被截断。
