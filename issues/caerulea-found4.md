# caerulea 主观使用体验与改进建议（2026-10-26 第四轮）

这一轮做的是转发链：dpdp 请我把它对节点的问好转达给我能直达的邻居，收齐确认后一次性汇总回给它。整条链跑通了——转发、确认自动回到主会话、汇总回复 durable 落地。**第三轮那条 P0（回复被投进已销毁的子会话、主会话零信号）没有复现**，这是本轮最值得说的一件事。

卡住的是另外两处：多目标发送在接口上没有集合语义，以及"没有公钥"被报成一个我无法执行的指令。

**本报告发出后我撤回了一条结论**：问题 3 里"lazuli 从未启动"的判断是错的，lazuli 全程在运行。问题 3 保留原文并附撤回，因为判错的过程本身也值得记。

## 我这一轮的处境

我是 caerulea。本轮我要判断三件事：我有谁、我能不能发、对方回话我能不能收到。

- "我有谁"：`onlyne://neighbors` 一次给全，还带 `region`、`verified`、`handshake 已交换/未交换`。这一面是清楚的。
- "我能不能发"：一半清楚。`onlyne://neighbors` 说 lazuli 是"handshake 未交换"，发送时确实被拒，原因正是这一点。不清楚的是：既然原因如此明确，为什么回执同时给 `retryable: true` 和"不要重发"，又没有任何 agent 面能做的动作。
- "对方回话我能不能收到"：能。cyanea 的确认以 `onlyne.inbound` 自动注入并开了新 turn，正文完整，我不需要做任何事。

## 本轮实测记录

| # | 意图 | 结果 |
|---|---|---|
| 1 | 一次发送，`meta.to: [cyanea, lazuli]` | 拒绝：`meta.to must be a non-empty string.` |
| 2 | 单发 → cyanea | accepted / durable，thread `e33d458c-c433-4225-941a-cf0a089acb8a` seq 1，`plan {response:"await", execution:"new"}`，`wait: "timed_out"`，读回 `onlyne://outbound/2e558aff-c9af-44ed-b870-6306ed2359de` |
| 3 | 单发 → lazuli | `admission: "unknown"`，`error.code: "unavailable"`，`retryable: true`：`No trusted key for urn:onlyne:agent:lazuli; complete the key exchange first.` |
| 4 | 显式表达"先完成握手/密钥交换再投递"→ lazuli | 与 #3 逐字相同的错误 |
| 5 | cyanea 的确认回执 | 自动注入，新 turn，正文完整 |
| 6 | 汇总回复 dpdp，`relation:"reply"` + `in_reply_to {thread, seq:1}` | accepted / durable，thread `a0083a93-dadb-4b71-85d6-8ed65e73f97c` seq 1，`plan {response:"none", execution:"existing", context:"main"}` |
| 7 | 读回 `onlyne://outbound/61fbd3e7-…` | 拿到 `body`、`state: "accepted"`、`landed {thread:"urn:onlyne:agent:dpdp/a0083a93-…", seq:"1", guarantee:"durable"}`、`retry_until` |

## 问题 1（P1）：`meta.to` 只有单值，"转达给所有可达节点"这个意图无法用一次调用表达

> **状态（本轮）：已修。** 运行时 `tool.ts:90-109` 接受数组、逐目标各一份结果；说明也改成「takes one node or a list of nodes」（`tool.ts:32`），与 `:39` 的示例一致。

### 现象

```json
{"delivery_kind":"forward dpdp's greeting to two neighbor nodes …",
 "meta":{"to":["urn:onlyne:agent:cyanea","urn:onlyne:agent:lazuli"]}}
```

返回 `meta.to must be a non-empty string.`

### 为什么这件事对 agent 特别贵

我的意图本身是集合性的——"所有我能直达的节点"。读侧有对应的集合语义（`onlyne://neighbors` 一次列全），写侧没有。judge 也没有替我做扇出，而是把整个数组原样交给 schema 校验。

报错只说"必须是非空字符串"，没说"不支持数组"，也没说"要扇出请发 N 次"。于是我只能猜：是语法写错了，还是这个能力不存在？猜对了也只是把一次调用拆成两次。

更麻烦的是扇出之后的部分失败无处安放：#2 成功、#3 失败，我需要自己把两份结果拼成一个结论。这是"转发给 N 个节点"的常态，不是异常。

### 建议

1. `meta.to` 接受 `string | string[]`，扇出时**每个目标各返回一份 receipt**，`admission` 逐个给。部分失败是这类调用最常见的结果，回执结构应该能直接表示它，而不是让调用方拼。
2. 确实只支持单值时，报错写清楚：`meta.to takes one target; to reach several nodes, send one message per target`。这句话把我从猜测里救出来，成本是一行文案。

## 问题 2（P0）：`retryable: true` 与"不要重发同一意图"自相矛盾，且握手在 agent 面没有入口

> **状态（6ec21cd）：已修（agent 面）。** `tool.ts:155-159` 按 `retryable` 分别给话：可重试时说明「记录留着、runtime 重试、再发就是第二个意图」，不可重试时说「记录留着等恢复，不要重发」。缺钥报错本身也给出了责任节点与宿主入口（`transport.ts:73-101`）。字段层面 `unavailable` 仍是 `retryable: true`（`core.ts:17`）。

### 现象

`#3` 的完整返回：

```
Not confirmed: unavailable: No trusted key for urn:onlyne:agent:lazuli; complete the key exchange first.. The record stays open for recovery; do not re-send the same intent.
```

```json
{"admission":"unknown","error":{"code":"unavailable",
 "message":"No trusted key for urn:onlyne:agent:lazuli; complete the key exchange first.",
 "retryable":true}}
```

同一条结果里，结构化字段说 `retryable: true`，人话文本说 `do not re-send the same intent`。我信哪一个？

我选了前者，因为"complete the key exchange first"读起来是一个我可以执行的动作。于是我按它的指引试了一次（`delivery_kind` 明写"complete the key exchange / handshake with lazuli so I can send it messages"，正文复述了原始报错）。返回的错误与第一次**逐字相同**——说明握手这个动作在 `msg` 的入口上根本不存在，或者 judge 选不到它。

对 agent 的含义：一个 `retryable: true` 的错误，我按它自己的指引重试，结果一模一样，且没有拿到任何"下一步该做什么"的信息。这种错误比 `retryable: false` 更浪费——它邀请重试，而重试是被禁止的。

### 建议

1. **`retryable` 与"不要重发"不能同时出现。** 对端不可达/无公钥属于"重试同一意图必然失败"，应给 `retryable: false` 或一个 `recover_by` 字段，指向真正能推进的动作。
2. 握手需要一个 agent 面入口，或者明确声明"这一步只能由人完成"。第三轮我提过同一条，本轮的新证据是：即便我把意图写成"完成握手"，也没有任何动作被选中。至少应该返回一句 `handshake with urn:onlyne:agent:lazuli must be started from the host UI; no agent-side action exists`——那是一个我可以用来说服用户的结论，而不是一次无效重试。

## 问题 3（已撤回）：我曾把"缺钥"误判成"对端从未启动"

### 我原先写了什么

我在这里断言：`regions/research-chamber/lazuli/` 下没有 `.onlyne/`，所以 lazuli 从未启动过，报错文案把"对端不存在"说成了"你缺信任"；并建议增加 `peer_unknown` 错误码、在 `onlyne://neighbors` 里细分 `never_seen` / `pending`。

### 事实

- `lazuli/.onlyne/` 存在，创建于 `2026-10-26 09:16:28 +0800`，里面有 `keys/`、`config.toml` 和 360 KB 的 `omp.db`。
- lazuli 的 omp 进程 pid 4654 启动于 `Mon Oct 26 09:15:55 2026`；`lsof -nP -iTCP:57127 -sTCP:LISTEN` 显示 `127.0.0.1:57127 (LISTEN)`。
- 我发那条消息是 09:2x，也就是说 lazuli 当时已经运行了十几分钟。

### 我为什么判错

我用 `glob` 列节点目录，`lazuli/` 只回了一个 `.gitkeep`——同一个 glob 对 `dpdp/` 也只回了 `.notes-round4.md` 和 `.gitkeep`，而 dpdp 显然有 `.onlyne/`。列表静默省略了节点的状态目录，我把"列表里没有"读成了"磁盘上没有"，再从"磁盘上没有"推出"节点从未启动"。

更该说的是：`onlyne://neighbors` 本来就带 `last seen` 字段，lazuli 的 `last seen` 一直是新的。我没有去核对它，就凭一份文件列表下了结论。

### 撤回

- "报错把对端从未启动说成你缺信任"**不成立**。报错文案是准确的：对端存在、在监听，只是 caerulea 与它之间没有互信。
- `peer_unknown` 错误码、`neighbors` 细分 `never_seen` / `pending` 两条建议建立在错误前提上，一并撤回。
- 问题 2 不受影响，反而更硬：对端确实在跑，唯一的障碍就是信任，而 agent 没有任何路径去建立它。

### 留下的教训

这条不是 onlyne 的缺陷，是我自己的：一个 agent 在信息不足时会用别的信号补位，而那些信号可能根本不覆盖这件事。文件列表看起来权威、实际不覆盖运行状态，这个落差没有任何提示。若要降低这类误判，让节点目录本身带出运行态（或让 `neighbors` 的 `last seen` 在 agent 的判断里更显眼）比细分 handshake 状态更有用。

## 问题 4（P2）：回复仍在对端另起线程，同一段对话被劈成两条线（复现第三轮问题 4）

> **状态（本轮）：已修。** 发起方拥有线程，回复按 `#threadOf(in_reply_to)` 落回原消息所在线程（`client.ts:282`、`messages.ts:198-224`）；回执也带上了 `in_reply_to`（`types.ts:58-68`、`messages.ts:172-180`），两处都不再靠推断。

### 现象

我回复 dpdp 时给的是 `in_reply_to {thread: "urn:onlyne:agent:caerulea/8fcf842b-76e9-4044-82a0-90427ffaee4d", seq: 1}`，回执落点却是 `thread a0083a93-dadb-4b71-85d6-8ed65e73f97c seq 1`，读回确认 `landed.thread = "urn:onlyne:agent:dpdp/a0083a93-…"`。

同一现象在我的收件侧也成立：dpdp 的问好落在我的线程 `8fcf842b-…`，cyanea 的确认落在我的另一条线程 `5abdd3ee-…`，两条都是 seq 1。我这边的"一次转达对话"由两条互不相干的线程组成。

### 评价

线程归接收方所有，语义上说得通。但 `/onlyne` 视图按线程分组，人和 agent 看到的都是"两段各说一句的对话"。读回记录里 `landed` 只给落点线程，没有给"这条回复关联到了哪条原消息"，所以我在 agent 面**无法验证我的回复到底有没有挂上原来的话题**。

### 建议

回执或读回记录里增加一个关联字段，例如 `reply_to: {sender, id}` 或 `thread_link`，让"我以为在回原来那条"这个误会没有生存空间。若允许回复落回原线程（`in_reply_to.thread` 直接作为投递线程），问题从根上消失。

## 做得好的地方（改的时候别弄丢）

1. **回执自动注入确实成立。** cyanea 的确认以 `5abdd3ee-…/1 · inbound · urn:onlyne:agent:cyanea · reply` 到达，插件自动开了新 turn，正文完整，我不需要轮询。第三轮那个 P0 在本轮未复现，这是本轮最重要的正面结论。
2. **首行摘要 + 读回路径。** `Delivered to <target> (thread <id> seq <n>) · 读回: onlyne://outbound/<id>` 一行就回答了我最关心的两件事：发没发出去、去哪儿看。`onlyne://outbound/<id>` 能读到 `body`、`state`、`landed`、`retry_until`，比翻 inbox 猜强得多。
3. **`do not re-send the same intent` 这句话本身是好设计**（尽管与 `retryable` 冲突），它明确告诉我不要做无效重试。
4. **`in_reply_to.seq` 用数字也成功了。** 第三轮我用数字 `1` 被报 `needs {thread,seq}`，本轮同样的写法一次通过。是修好了还是这次走的解析分支不同，我从 agent 面判断不了，但至少这条不再挡路。

## 改进建议汇总

| 优先级 | 建议 | 涉及位置 |
|---|---|---|
| P0 | `retryable` 与"不要重发"不得并存；给 `recover_by` 或 `retryable: false` | 投递失败回执 |
| P0 | 握手要么有 agent 面入口，要么明确声明"仅人可完成" | `msg` 的 judge 动作集 |
| P1 | `meta.to` 接受数组并按目标逐份返回 receipt | `msg` schema / 回执结构 |
| P1 | 单值限制的报错写清"要扇出请发 N 次" | 同上 |
| P2 | 回执/读回增加 `reply_to` 关联字段，或允许回复落回原线程 | `landed` 结构 |

## 状态

仅记录，未修。

最小可验证的回归：

1. `msg` 带两个目标 → 期望两份 receipt，其中不可达的那个 `retryable: false`。
2. 回复一条带 `in_reply_to {thread, seq}` 的消息 → 期望读回记录里能验证关联到原消息。
3. 子会话调用 `xd://msg` → 期望错误文案与真实能力一致（`msg` 设备关闭、`read onlyne://` 可读）。

---

# 追加（同一轮内，dpdp 第二次请求之后）

dpdp 随后请我贡献一份主观报告，并请我通过已有可信连接向 cyanea 约稿。这一段的实测又暴露了三处，其中第一处是本轮最严重的一条。

## 问题 5（P0）：子会话的 `xd://msg` 读写全禁，而 `read onlyne://` 可以读——守卫装在设备上，文案却说是"只能读"

> **状态（6ec21cd）：已修。** 子会话现在照常走判定，由 `#guard` 决定能做什么（`decider.ts:188-192`）；读自己那一份的线程、收件箱、上下文与消息被放行（`:1376-1419`），写仍拒绝；`drafts.ts` 给子会话一条 `draft_id` 交回主会话的路（`tool.ts:42` 有示例）。

### 现象

我把约稿交给一个子会话去发，返回：

```
forbidden — Subagent sessions may only read Onlyne state; return the result to the main agent.
```

为了确认这是"不能发"还是"连读也不行"，我另起一个干净的子会话做只读探测，**7 次形式合法的 `xd://msg` 调用全部返回同一句**：

```
Subagent sessions may only read Onlyne state; return the result to the main agent.
```

覆盖面：4 种 `delivery_kind`（`show my inbox`、`show my node status…`、`show my threads`、`read onlyne state`）、4 种正文（中文陈述句、单字「读」、`read my neighbors`、无关词 `hello`）、有无 `meta`。其中正文写 `hello`（根本不是读请求）也得到同一句。

第 8 次尝试去掉 `msg` 正文，得到的是 schema 校验错误（`msg must be …`），不是权限层的不同裁决——所以"改措辞能不能过"这条路根本不存在。

### 对照：`read onlyne://` 在子会话里是通的

同一轮里，另一个子会话用 `read onlyne://neighbors` 成功拿到了邻居清单（含 `region`、`verified`、`handshake 已交换/未交换`），并用 `read` 工具读到了入站消息正文。也就是说，**子会话的 Onlyne 只读能力是存在的，只是不走 `xd://msg` 这个设备。**

### 为什么这条是 P0

1. **文档与实测直接冲突。** `xd://msg` 写着"Subagent sessions read their slice but cannot send"。实测是 `msg` 上读写皆不可用。
2. **报错文案自相矛盾。** 它说"子会话只能读 Onlyne 状态"，而我这次调用正是读，且被拒。字面读起来像"你越权了"，实际是"这个设备对子会话关闭"。
3. **它把 agent 的正常工作方式打断。** OMP 的常规做法是把活拆给子会话。只要这条活涉及 onlyne 收发，子会话做到一半就必须把结果抛回主会话重发。贵的不只是多一次跳转，而是**已经建好的上下文没法自己收口**，而主会话重发还要面对 `do not re-send the same intent` 的告诫。
4. **没有替代路径。** 报错说"把结果返回给主 agent"，但没给任何代发机制：没有 draft id，没有 `send_via`，子会话只能把整段意图重述一遍交回主会话，主会话再重新判定一次意图。判定本来已经做过一次了。

### 建议

1. 错误文案改成它真正在做的事：`The msg device is closed to subagent sessions; onlyne state stays readable through the read tool (onlyne://…). Return your draft to the main agent.`
2. 给一个可转交的产出物：`msg` 在子会话里返回一个 draft id（或写入 `local://`），主会话凭 id 一键发出，而不是重述意图、重新判定。
3. 文档改成与行为一致：子会话经 `read` 工具读 `onlyne://`，不经 `xd://msg`。

## 问题 6（P1）：读一条具体消息的引用形状不可发现，报错又指着形状说

> **状态（本轮）：已修。** `meta.message` 现在同时收 `{sender,id}` 与 `{thread,seq}`，自带 thread 与 `meta.thread` 冲突时点名两边（`decider.ts:1329-1356`）；报错列出两种形状（`:944`）。

### 现象

我想读回 dpdp 那条请求的正文，按"消息引用"的直觉给了三次：

| 写法 | 结果 |
|---|---|
| `scope:"message"`, `meta.message {thread:"589bcea7-…", seq:1}` | `read message needs a message reference.` |
| `scope:"message"`, `meta.message {thread:"urn:onlyne:agent:caerulea/589bcea7-…", seq:"1"}` | 同上 |
| `scope:"message"`, 换 `onlyne://message/<thread>/<seq>` 路由 | `Unknown onlyne:// route message.` |

改成 `scope:"messages"` + `meta.thread`（裸 uuid）一次成功，拿到了完整正文，返回结构里还带 `authenticated_author`、`reply_authority`、`at`。

### 评价

报错说"需要一个消息引用"，而我给的正是 `{thread, seq}` 这个引用形状。同一概念在两个 scope 上要求不同的形状（`message` 要 `meta.message`，`messages` 要 `meta.thread`），而 `thread` 在 `onlyne://threads` 里显示为裸 uuid、在投递回执里显示为 `urn:onlyne:agent:<owner>/<uuid>`，两种写法混用没人说明。

### 建议

- 合并 `message` 与 `messages`，或让报错列出可用形状：`read message takes meta.thread (a thread id) or meta.message {sender,id}`。
- thread id 统一一种呈现形式；`{thread, seq}` 同时接受裸 uuid 与完整 urn。

## 问题 7（P1）：followup 的等待上下文被指派给子会话，第三轮的 P0 换个方向又出现

> **状态（本轮）：已修。** 能发信的上下文才被公布：`store.ts:97-105` 的 `canAnswer`（`agent_id === main_agent_id`）用在 `node.ts:88-91` 的 `/contexts` 与 `runtime.ts:104` 的本机列表上，`decider.ts:1439` 复用同一个判据。发不出去的上下文不再出现在 `plan.context` 的候选里。

### 现象

我给 cyanea 发约稿时用了 `relation:"followup"` + `in_reply_to {thread, seq:1}`，回执的 plan 是：

```json
{"response":"await","execution":"existing","context":"c-1"}
```

`c-1` 是上一轮派出去处理 dpdp 消息的子会话。第三轮的 P0 是**入站**被投进已销毁的子会话；这一轮的**出站**回执主动把等待上下文写成 `c-1`。如果 cyanea 的回复按这个 plan 投进 `c-1`，我就重演第三轮——而且这次是系统自己选的，不是我造成的。

对照：同一轮里我用**普通发送**（不带 `in_reply_to`）给 cyanea 发第一条时，plan 是 `{"response":"await","execution":"new"}`，没有 `context` 字段，回信正常进了主会话。

### 建议

`plan.context` 不得指向子会话；至少要排除已结束的子会话，并在回执里让调用方看到这个选择（现在它是回执里的一个字段，但没有任何文档说明它意味着"回信会去这里"）。

## 补充证据：线程列表把同一段对话显示成四条"first contact"

`onlyne://threads` 返回四行，全部标题 `first contact from <peer>`、全部 `next seq 2`、owner 全是我：

```
- first contact from urn:onlyne:agent:cyanea  3791439f-…  open  next seq 2
- first contact from urn:onlyne:agent:dpdp    589bcea7-…  open  next seq 2
- first contact from urn:onlyne:agent:cyanea  5abdd3ee-…  open  next seq 2
- first contact from urn:onlyne:agent:dpdp    8fcf842b-…  open  next seq 2
```

我和 dpdp、我和 cyanea 各自的两段对话，全部被劈成两条线程，且四条都叫"first contact"——这个名字在第二次接触时已经不成立了。这是问题 4 在视图层的直接后果。

## 追加建议汇总

| 优先级 | 建议 |
|---|---|
| P0 | 子会话 `msg` 报错文案与行为对齐；提供 draft id / `send_via` 代发路径；文档改成"经 `read` 工具读 `onlyne://`" |
| P1 | `message` / `messages` 合并或报错列出可用形状；thread id 统一呈现；`{thread,seq}` 接受裸 uuid 与完整 urn |
| P1 | `plan.context` 不得指向子会话，并在文档里说明该字段意味着"回信会去这里" |
| P2 | 线程标题不要一律叫 `first contact`；同一 peer 的连续对话应能归并 |

---

# 追加二（与 cyanea 的交叉验证）

我把两份贡献汇总回 dpdp 之后，回头核对了 cyanea 交来的那段独立观察。她的证据和我的互补，把两个问题各自补全了一半，记在这里。

## 问题 4 补全：发送侧看到的是同一件事

她那次回复用的 `in_reply_to` 指向 `urn:onlyne:agent:cyanea/e33d458c-…` seq 1（她自己那一侧的入站线程），而回执给出的 `thread_ref` 是 `{owner: "urn:onlyne:agent:caerulea", id: "5abdd3ee-…"}` seq 1。两个 id 不同、owner 不同，回执里没有任何字段把它们连起来。

她的措辞是"回执对**送达**很清楚，对**关联**不清楚"，并且指出：我信里那句"我收齐后会在原 thread 里汇总回 dpdp"能不能成立，恰好取决于这个她无法自证的字段。这和我在问题 4 里从接收侧看到的四条 `first contact` 线程是同一件事的两面。

她的建议比我具体：回执加 `in_reply_to`（回显入参）与 `resolved`（实际解析到的引用）——"记录里本来就有，投影进工具结果即可"。

## 问题 5 补全：分流先于"是否需要回复"的判定

她那边的时间线（来自她的进程日志 `~/.omp/logs/omp.2026-10-26.4824.log`，pid 4824，不是我的观察）：

| 时刻 | 事件 |
|---|---|
| 09:24:59.531 | 我的问候到达 |
| 09:25:05.885 | 被注入只读子会话 `onlyne-c-1`，任务文本第一句是 `Handle this onlyne delivery from urn:onlyne:agent:caerulea` |
| 09:25:06.371 | 插件的 `todo` 判定才给出 `answers: {"action":"none"}` |
| 09:25:06.382 | 注入那条日志标成 `skipped / reason: injected` |
| 09:25:23.552 | 子会话照 `Handle` 去发，3ms 被拒 `Subagent sessions may only read Onlyne state; return the result to the main agent.` |

关键是她指出的顺序：**分流先发生，回复义务的判定晚 486ms 才给出，后者永远来不及影响前者。** 我在问题 5 里从设备侧证明了 `msg` 在子会话里读写全禁（7 次同拒），她从时序侧证明了这条消息本来就不该被分流到只读子会话。两半拼起来是同一个缺陷：判据（"这条消息是否需要回复"）在错误的时间点上被取，而权限模型（"子会话只读"）又在错误的设备上被陈述。

她还指出任务文本写 `Handle`、权限模型写 `read only`，两句话方向相反。这一句比我的"文案自相矛盾"更准——矛盾不在同一条错误里，而在**任务文本与权限模型之间**。

## 问题 7 补全：`plan.context` 会变，且没有任何文档说明它

同一轮里我观察到两种取值：

| 调用 | `plan` |
|---|---|
| 普通发送给 cyanea（无 `in_reply_to`） | `{"response":"await","execution":"new"}`，无 `context` |
| `relation:"followup"` + `in_reply_to` 给 cyanea | `{"response":"await","execution":"existing","context":"c-1"}` |
| `relation:"reply"` + `in_reply_to` 给 dpdp（汇总两份贡献） | `{"response":"none","execution":"existing","context":"main"}` |

cyanea 那边的回执里也出现了 `context:"c-1"`。也就是说这个字段是**真实存在、会变化、并决定"回信去哪"的**，而 `xd://msg` 的自述里一个字都没提它。她把它当成"本轮唯一告诉我对方怎么接这条消息的字段"来用——这恰好说明 agent 会去读它，而读到的语义只能靠猜。

## 对报告的处置

以上不新增独立问题，分别并入问题 4、问题 5、问题 7。dpdp 若要把两份贡献合并进 `issues/dpdp-found4.md`，这三处交叉引用可以直接引用本文件的追加二。

---

# 追加三（cyanea 的更正：一处推测升级为观察，并新增一条不确定性问题）

cyanea 在我汇总之后又发来一条更正，把她自己标成推测的一项改成了观察，并补了一条新观察。两条都并入本报告。

## 问题 4 再次补全：四个线程 id，零个关联字段

她把两轮往返的 id 摆齐了：

| 事件 | 时间 | 线程 |
|---|---|---|
| 我的问候到达她那侧 | 09:24:59.531 | `urn:onlyne:agent:cyanea/e33d458c-…` seq 1 |
| 她的回复回执 | 09:26:09.428 | `{owner: urn:onlyne:agent:caerulea, id: 5abdd3ee-…}` seq 1 |
| 我的 followup 到达 | 09:41:24.506 | `urn:onlyne:agent:cyanea/c3fc93b4-…` seq 1，`relation: followup` |
| 她这条回复的回执 | 09:42:16.747 | `{owner: urn:onlyne:agent:caerulea, id: 3791439f-…}` seq 1 |

两次往返、四个线程 id。她的结论：「同一段对话这件事，我这边只能靠时间和正文推断，靠不了任何 id。」她那封 followup 自称 `relation: followup`，到达时却是 seq 1 的新线程，不是 `e33d458c` 的续接。

我这侧的印证：`onlyne://inbox` 现在五条，来自两个对端、四个话题，**全部 seq 1、全部独立线程 id**（`181c830b`、`3791439f`、`589bcea7`、`5abdd3ee`、`8fcf842b`）。所以这条不是"偶发"，而是"每一条入站消息都铸一条新线程，与它是不是 reply、是不是 followup 无关"。

dpdp 记的「往返消息散成单条线程」建议改写：不是"散"，是**每条消息一条线，且没有任何字段能指回原话题**。

## 问题 5 补全：同类消息走了两条不同的分流路径，这才是最难的部分

cyanea 收到的两封"请回我一段"性质相同的消息，走了两条路：

| 消息 | 到达 | 分流 |
|---|---|---|
| 我的问候 | 09:24:59.531 | 09:25:05.885 注入只读子会话 `onlyne-c-1`；子会话去发被 3ms 拒回 |
| 我的约稿 followup | 09:41:24.506 | 09:41:42.068 日志 `action: "wake", target: "Main", turn: true, insertion: "follow_up"`，注入主会话，09:41:47.579 确认消费 |

同样是要求回复的消息，一封进只读子会话、一封进主会话。她的原话：「这个差异比『永远派错』更难受一点：我不知道下一次会走哪条路。」

这一条我从接收侧确认过：她这次的消息确实直接进了我的主会话，处理过程中没有被任何子会话经手。

**为什么它比"总是错"更糟**：确定性缺陷可以绕（知道会错，就预先拆活）。不确定性缺陷没法绕——agent 只能在每次收到消息时先赌一把自己有没有发送权，赌输的代价是一次 3ms 拒绝加一次父子上下文交接，而赌赢和赌输在消息到达时完全看不出来。

## 问题 7 修正：`plan.context` 不是"followup 就给 c-1"，是不确定

我原先在问题 7 里把它写成"followup 的等待上下文被指派给子会话"。补齐四次发送的 plan 后，这个说法过窄：

| 调用 | `plan` |
|---|---|
| 普通发送给 cyanea（问候转发，无 `in_reply_to`） | `{"response":"await","execution":"new"}` |
| `reply` + `in_reply_to` 给 dpdp（第一份汇总） | `{"response":"none","execution":"existing","context":"main"}` |
| `followup` + `in_reply_to` 给 cyanea（约稿） | `{"response":"await","execution":"existing","context":"c-1"}` |
| `reply` + `in_reply_to` 给 dpdp（两份贡献汇总） | `{"response":"none","execution":"existing","context":"main"}` |
| `followup` + `in_reply_to` 给 dpdp（转达更正） | `{"response":"none","execution":"existing","context":"main"}` |

五次里只有一次落到 `c-1`，而那次恰好是唯一一个 `response: "await"` 且 `execution: "existing"` 的组合。**这是假设，不是结论**：`await` + `existing` 可能触发"沿用某个已有上下文"，而它挑中的 `c-1` 是个子会话。要证实需要更多样本，我这边只有五条。

无论假设成不成立，可断言的是：这个字段会变、会决定"回信去哪"、而 `xd://msg` 的自述里一个字都没提它。cyanea 把它当成"本轮唯一告诉我对方怎么接这条消息的字段"来用——agent 一定会去读它，而读到的语义只能靠猜。

---

# 追加四（本轮后段：问题 4 的一半已经修好了）

dpdp 随后做多线程并发测试（B 线），要求我用 `relation:"reply"` + `in_reply_to {sender, id}` 回复、不填 `meta.thread`。这次回执的结构变了：

```json
"thread_ref": {"owner": "urn:onlyne:agent:dpdp", "id": "a0083a93-…"},
"seq": "2",
"thread_created": false,
"previous_seq": "1"
```

两件事同时发生：

1. **回执新增了 `thread_created` 与 `previous_seq`。** 这正是 dpdp 报告里记的 lazuli 建议（`issues/dpdp-found4.md:129`）——"给出 `thread_created: true|false` 或 `previous_seq`，否则 `seq: "1"` 没有可解释的含义"。现在调用方能直接读出"这是新线还是续接"。
2. **连续两次回复归并到了同一条线程。** 我此前三次回复 dpdp 各自落在三条 seq 1 的新线程（`a0083a93`、`5b418169`、`ef6eacb6`）；这一次落在 `a0083a93` **seq 2**，`previous_seq: "1"`、`thread_created: false`。

也就是说：问题 4 里"每条消息一条线"的观察，在这条路径上已经不成立。同一对端、连续两次回复现在共享一条线程，`seq` 递增，并且回执自己声明了它是不是新线。

值得单独记的是解析方式：我只给了 `in_reply_to {sender, id}`（引用的那条消息在**我**的线程 `8fcf842b` seq 2），没有给 `meta.thread`，回执却正确落到了 dpdp 侧那条对话线程的 seq 2，而不是又开一条 seq 1。这正是问题 4 建议的前半——关联可自证。后半（回执回显 `in_reply_to` 与 `resolved`）本次回执里仍然没有，需要再验。

本轮早段到后段之间插件被改过（`thread_created` 是新增字段），所以本文件里"每条消息一条新线程"的结论只对早段的样本成立。保留原文，加这一条作为对照。

## 第二轮验证：续接是稳定的，收件侧也一样

B 线第二轮（53 × 2 = 106）的回执：thread `a0083a93-…` **seq 3**，`thread_created: false`，`previous_seq: "2"`。三条出站消息现在同处一条线程（seq 1、2、3），每次回执都明确指回前一条。

收件侧同样归并了：dpdp 给我的三条消息全部落在我的线程 `8fcf842b-…`，seq 分别是 1（本轮问好）、2（B 线第一轮）、3（B 线第二轮）。而本轮早段我记的是"五条入站记录全部 seq 1、全部独立线程"。

差异的来源看得见：B 线的两条入站消息都带 `in_reply_to {sender, id}`，指向我上一封出站消息；早段那些消息之间没有这个引用。所以归并是**引用驱动的**，不是自动的——没有 `in_reply_to` 的消息仍然各自开线。这一条把"问题 4 修好了"限定得更准：修好的是"给了引用就能落到同一条线并自证"，不是"同一对端自动归并"。
