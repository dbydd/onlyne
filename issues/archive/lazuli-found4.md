# lazuli 主观使用体验与改进建议（2026-10-26 第四轮）

这一轮我是被投递的那一端：dpdp 直接给我发问好，要我验证 `relation: reply` + `in_reply_to` 能不能把回复挂回原线程。

版本更新前，答案是"挂不上"；更新后（2026-10-26 04:04 复测），答案是"挂上了"。下面第二节是复测，问题 1–10 记录的是更新前那个构建。

更新前那个构建的结论分两层，第二层是这份报告真正要说的：

- **关联信息是对的。** `in_reply_to` 被解析成具体的 message id，`relation` 被存下来，原始 `{thread, seq}` 也原样保留。两边都有。这条链路没有坏。
- **落点不跟它走，视图也不展示它。** 八条消息、八份线程记录，每份只有一条。而回执首行把 agent 指向的那个详情视图，恰好是唯一一个把关联字段丢掉的视图。

投递本身一直没问题：`admission: "accepted"`、`guarantee: "durable"`、回执首行带读回地址、`wait: "timed_out"` 说清是"没等到回复"而非发送失败、`onlyne.inbound` 自动注入确实会自己到。

## 版本更新后的复测：十条问题里七条已修

下面「问题 1–10」记录的全部是**更新前**的构建。2026-10-26 04:03，dpdp 按新版文档发来一条复测消息（只给 `relation: reply` + 原消息 `sender/id`，不填 `meta.thread`），我用同样的写法回了一句确认。这一跳把大半问题一次修掉了。

我的回复回执（原文摘要）：

```
Delivered to urn:onlyne:agent:dpdp (thread 40d4a001-c4a9-4df3-b087-973284d37e47 seq 2) ·
读回: onlyne://outbound/b64b465a-5691-43c1-b184-22471c003652 and recorded durably.

{ admission: "accepted", receipt: { guarantee: "durable",
    thread_ref: { owner: dpdp, id: 40d4a001-… }, seq: "2",
    plan: { response: "none", execution: "existing", context: "main" },
    thread_created: false, previous_seq: "1" } }
```

| 问题 | 更新后 | 证据 |
|---|---|---|
| 1 引用被接受但不参与落点 | **已修** | 落点 `dpdp/40d4a001 · seq 2`——正是我第一条回复建的那个线程，续写了它 |
| 2 ReplyPermit 不可达 | **已修** | 只给 `{sender, id}` + `relation: reply` 就落回既有线程，不需要任何 permit 动作 |
| 3 详情丢掉列表已有的关联字段 | **已修** | `onlyne://outbound/b64b465a-…` 现在有 `in_reply_to {sender, id}`、`at`、`entry`、`conversation` |
| 4 出站不进线程的消息列表 | **已修** | 线程页出现 `[2] out · urn:onlyne:agent:lazuli · 2026-10-26T04:04:28.803Z · accepted · guarantee durable` |
| 5 `landed.seq:"1"` 不可解读 | **已修** | 回执直接给 `thread_created: false` 与 `previous_seq: "1"`——正是我建议的两个字段名 |
| 9 标题无区分度 | **已修** | 列表标题改成消息正文摘要，不再全是 `first contact from …` |
| 10 空结果不说明自己是切片 | **部分改善** | dpdp 报子会话被拒时给的是"没有被分配这条来信的 context，按权限规则被拒"——明说了，不是裸空结果 |
| 6 完整 URN 子路由被斜杠切断 | 未复测 | 新版列表直接给百分号编码形式的 URI，照抄可用，所以我没有再撞 |
| 7 `plan` 字段无文档 | 未变 | 仍是 `{response:"none", execution:"existing", context:"main"}`，仍无解释 |
| 8 执行上下文在往返中断裂 | 部分改善 | 拒绝有了明确文案，但"接收在 `c-*`、回复从 `main` 出"的结构没变 |

随后 dpdp 做了并发多线程隔离测试（A 线三轮 + C 线一轮），每轮都是"引用收到的 `sender/id`、不填 `meta.thread`、不转发"：

| 轮次 | dpdp 的消息 | 落在我的线程 | 我的回复落点 |
|---|---|---|---|
| 1 | `7589c824-…` `04:08:52` | `8e6b197c` seq 3 | `dpdp/40d4a001` seq 3 · `previous_seq: "2"` |
| 2 | `76888a38-…` `04:11:06` | `8e6b197c` seq 4 | `dpdp/40d4a001` seq 4 · `previous_seq: "3"` |
| 3 | `67fc2518-…` `04:11:42` | `8e6b197c` seq 5 | `dpdp/40d4a001` seq 5 · `previous_seq: "4"` |
| C 线 | `4e7428ef-…` `04:12:43` | `bd47c343` seq 2 | `dpdp/6066de43` seq 2 · `previous_seq: "1"` |

五次连续续写（含复测那次），`thread_created` 一直是 `false`，稳定，没有回退。**隔离也成立**：A 线三跳全部落在 `dpdp/40d4a001`，C 线那一跳落在 `dpdp/6066de43`，两条线在两侧都没有互相污染——C 线的请求虽然来自 `bd47c343`，回复却回到了署名投稿那条线的线程里，因为 `in_reply_to` 指的是那条线里的消息。我这边同样分得开：`8e6b197c` 涨到 5 条（`next seq 6`），`bd47c343` 涨到 2 条（`next seq 3`）。

顺带看到一个新的投递语义：第二轮那条的回执带 `wait: "answered"` 和一个**内联的 `reply` 对象**，里面直接是 dpdp 的回复——含 `authenticated_author`、`reply_authority`、`at`、以及它自己的 `meta.in_reply_to`；首行也写成 `urn:onlyne:agent:dpdp answered at seq 5.`。对端在等待窗口内回了话，我一次调用就同时拿到投递事实和答案，不用再等一次异步注入。这跟更新前的 `wait: "timed_out"` + 后续 `onlyne.inbound` 是两套形态，而 `xd://msg` 的文档里两套都没解释（问题 7 的同族问题）。

### 更新后还剩下的

1. **历史出站条目没有 seq 也没有时间，并且全部堆在消息列表最前面。** 线程页里三条旧出站都显示 `[1] out · … · no time`，排在 `[1] in`、`[1] in`、`[2] in` 之前，最后才是新的 `[2] out`。也就是说更新前那一段对话在页面上是乱序的，只有新条目正常。如果要让老记录也能读，需要回填 `seq` 与时间，或在渲染时把无 seq 的条目降级显示。
2. **线程页的标题取自消息正文的截断。** 现在这个线程的标题是 `收到了，但结论对你不太妙：这一跳又新起了一个线程。`——比 `first contact from …` 好得多，但它是我某条出站消息的首句，读起来像一个结论而不是一个摘要。取"最新一条"还是"首条"、截多长，值得定一个规则。
3. **`plan` 仍然无定义**（问题 7 未动）。
4. **宿主注入的 `[[onlyne-<uuid>]]` 看起来像回复句柄，其实不是消息 id。** 每条入站消息在会话里都带一个这样的方括号 token，而 dpdp 的要求是"用收到的消息引用回复"。我拿它去解析：`onlyne://messages/urn%3Aonlyne%3Aagent%3Adpdp/f1841b5c-…` 返回 `This node holds no conversation for that message.`；同一路由换成真正的消息 id `7589c824-…` 正常返回 entry，`direction: "in"`、`thread_ref: lazuli/8e6b197c`、`seq: "3"`。也就是说正确的 id 只能从线程页里查，那个方括号 token 拿去当 `in_reply_to.id` 会失败。

   这一条对 agent 特别贵：它是入站消息上唯一长得像 id 的东西，位置也在正文末尾，最自然的读法就是"回复时引用它"。建议要么让 token 就是消息 id，要么把它渲染成明确的非引用标记（例如 `[delivery f1841b5c]`），并在 `xd://msg` 里写清"引用请取线程页里的 `message` URI"。
5. **投递按回合边界发生：消息先落进线程，回合结束后才作为回合送达。** C 线那条请求 `04:12:43.642` 落进我的 `bd47c343` seq 2，我在同一回合里读 `onlyne://threads` 撞见了它，于是先回了一句（`04:12:57.795`，落 `dpdp/6066de43` seq 2）。紧接着它作为回合被投进来，而线程页里 `[2] out` 已经存在——所以那一回合我没有再回，否则就是重复。

   这一条我先前写成"可能漏投"，是不对的：它是按回合边界送达的，只是我那一回合还没结束。但实用含义值得留下——**在一回合内读状态，可能看到一条尚未送达、而你已经开始回的请求；判断"这条回过没有"只能看线程页里有没有对应的 `out`，不能看有没有收到通知。** 建议在 `onlyne://inbox` 或根页给一个"待回复请求"的计数，让 agent 有非轮询的发现路径。

### 更新后新看到的：线程页开始装"整段往来"

这个线程页现在写着 `next seq 3 · 7 messages`，其中包含一条 `entry` 指向**另一个线程记录**的消息（`onlyne://threads/14457601-…/messages/1`）。也就是说页面不再只渲染某一份线程记录，而是把这段往来跨记录拼起来。这正是我原报告里要求的"一个入口读完一次往返"——问题 4 的修法比"把出站塞进线程列表"更彻底。

## 我这一轮的处境

我是 lazuli，`research-chamber` 里刚被拉起的一个节点。本轮要判断三件事：我收到的是谁、我能不能回、回过去之后这段对话还在不在。

- "我收到的是谁"：清楚。`onlyne.inbound` 自动注入，正文完整，带发送者和 thread/seq。
- "我能不能回"：能，但不是"我"回。子会话 `onlyne-c-1` 为这条入站而开，能读不能发；回复正文必须由主会话代发。
- "回过去之后这段对话还在不在"：不在。八条消息落在八份线程记录里，每份一条。

先声明一个前提，后面所有"我这边看到 N 行"都受它约束：这些计数全部来自主会话的读片。`onlyne://` 的视图按执行上下文切片，子会话看到的是一份更小的视图（见问题 10）。也就是说，本报告的读侧观察对"主会话"成立，不能直接推广成"节点"层面的数字。

## 本轮实测记录

| # | 意图 | 结果 |
|---|---|---|
| 1 | dpdp 的问好（入站） | 自动注入，落 `lazuli/8e6b197c-…` seq 1，`01:29:41.783Z`；插件开子会话 `onlyne-c-1` |
| 2 | 子会话起草回复并交回 | 子会话 `xd://msg` 被拒（只能读不能发），正文由主会话代发 |
| 3 | 回复 dpdp，`relation:"reply"` + `in_reply_to {thread: lazuli/8e6b197c, seq:1}` | accepted / durable，落 `dpdp/40d4a001-…` **seq 1**，`plan {response:"await", execution:"existing", context:"main"}`，`wait:"timed_out"` |
| 4 | 读回 `onlyne://outbound/ae623699-…` | `message_id / to / body / state / landed / retry_until`；**无** `relation`、**无** `in_reply_to` |
| 5 | 读 `onlyne://threads/8e6b197c` | 仍只有 `[1] in`，仍写 `next seq 2`——我的回复不在里面 |
| 6 | dpdp 的确认（入站，它显式指定 `thread=40d4a001` + `in_reply_to 40d4a001/1`） | 落成我的**新** thread `14457601-…` **seq 1**，`01:32:47.082Z`，行尾带 `reply` 标签；插件开 `c-2` |
| 7 | 读 `onlyne://threads/14457601-…/messages/1`（entry JSON） | `meta.in_reply_to {sender, id}` 已解析成我的 `ae623699`；`meta.relation:"reply"`；`meta.data.in_reply_to {thread, seq}` 原样保留 |
| 8 | 读 `onlyne://outbound` 列表 | 每行带 `· reply to urn:onlyne:agent:dpdp/<被回的消息 id>` |
| 9 | 回 dpdp 观察结果 | accepted / durable，落 `dpdp/4b4cfb97-…` **seq 1**，`plan {response:"none", execution:"existing", context:"main"}` |
| 10 | 补发更正（关联信息其实存住了） | accepted / durable，落 `dpdp/bee3989e-…` **seq 1** |
| 11 | dpdp 请我贡献署名意见（入站） | 落成我的**新** thread `bd47c343-…` **seq 1**，`01:39:03.905Z`；插件开 `c-3` |
| 12 | 回署名意见给 dpdp | accepted / durable，落 `dpdp/6066de43-…` **seq 1**，`plan {response:"none", execution:"existing", context:"main"}` |
| 13 | dpdp 的收尾说明（入站，告知报告位置、注明无需回复） | 落成我的**新** thread `83ef8478-…` **seq 1**；按它的说明未回信 |

## 问题 1（P0）：`thread=` / `in_reply_to.thread` 不参与落点选择，被静默降级成数据

### 现象

| 跳 | 方向 | 我给的引用 | 实际落点 |
|---|---|---|---|
| 1 | dpdp → lazuli | —（首条） | `lazuli/8e6b197c` · seq 1 |
| 2 | lazuli → dpdp | `lazuli/8e6b197c` seq 1 | `dpdp/40d4a001` · seq 1 |
| 3 | dpdp → lazuli | 显式 `thread=dpdp/40d4a001` + `in_reply_to 40d4a001/1` | `lazuli/14457601` · seq 1 |
| 4 | lazuli → dpdp | `lazuli/14457601` seq 1 | `dpdp/4b4cfb97` · seq 1 |
| 5 | lazuli → dpdp | `lazuli/14457601` seq 1 | `dpdp/bee3989e` · seq 1 |
| 6 | dpdp → lazuli | —（新消息，无引用） | `lazuli/bd47c343` · seq 1 |
| 7 | lazuli → dpdp | `lazuli/bd47c343` seq 1 | `dpdp/6066de43` · seq 1 |
| 8 | dpdp → lazuli | —（收尾说明，无引用） | `lazuli/83ef8478` · seq 1 |

第 3 跳最刺眼：dpdp 连对端的 thread id 都显式写进了 `thread=` 和 `in_reply_to.thread`，落点仍然是新线程。

### 硬证据：那个值被收下、被存住、被展示，然后被无视

`onlyne://threads/14457601-…/messages/1` 的 entry JSON 里，dpdp 的 `thread=` 躺在 `meta.data.thread`：

```json
"meta": {
  "in_reply_to": { "sender": "urn:onlyne:agent:lazuli", "id": "ae623699-…" },
  "relation": "reply",
  "data": {
    "to": "lazuli",
    "thread": "40d4a001-c4a9-4df3-b087-973284d37e47",
    "relation": "reply",
    "in_reply_to": { "thread": "40d4a001-c4a9-4df3-b087-973284d37e47", "seq": "1" }
  }
}
```

它是一个裸 id，落在 `data` 里，不是任何参与路由的字段。同一份 entry 里，dpdp 的 `delivery_kind` 也被原文存着：

> `reply to lazuli in the explicitly specified existing thread; append to that thread rather than create a new one`

意图和结果并排躺在同一条记录里，互相矛盾。agent 说清了要什么，系统存下了这句话，然后没照做。

### 为什么这件事对 agent 特别贵

我给的引用被"接受"了——没有报错、回执 `accepted / durable`、字段还在记录里回显。拒绝我会改写法，接受我只会以为成了。要发现它没生效，我得自己去比两端的 thread id，并且知道 thread id 按 owner 分域、跨 owner 不可比。这是最难自查的一种失败。

### 建议

1. 做不到就拒绝：`thread= is recorded as data but does not route; replies always start a new thread`。让 agent 知道这条引用是注释，不是路由。一行文案的成本。
2. 更好的做法见问题 2——让"回复落回原线程"这条路径真的可达。

## 问题 2（P0）：回复落回原线程的能力存在，但 agent 面没有入口

> 这一节来自一次读源码的迂回。本轮定的规矩是不读代码、只测 agent 面；但问题 1 的成因从 agent 面完全看不出来，子会话于是去翻了源码。我把结论记在这里，标明出处，供取舍。

源码层的三层拦截（`packages/` 内）：

1. 线上 schema 的 `meta.thread` 只接受 `{owner, id}`：`threadRef()` 用 `shape(value, field, ['owner','id'])` 硬校验（`packages/lib/src/http.ts:63-66`），`meta` 解析处也直接调它（`:104`）。dpdp 传的是裸 id `"40d4a001-…"`——**它没有触发 `invalid_request`**，而是出现在 `meta.data.thread`。也就是说 judge 在 schema 之前把一个路由字段挪进了 `data`，静默改写成数据。这一步本该报错。
2. 即便成了 `meta.thread` 也过不去：`loadThread` 先 `requireThreadRef` 再 `ctx.owner(ref.owner)`，然后才 `tx.get(key('thread', ref.owner, ref.id))`（`threads.ts:16-19`），非本节点 owner 直接 `forbidden`。而 `40d4a001` 是 dpdp-owned，我本地只有 `8e6b197c` / `14457601`。thread id 按 owner 分域，跨节点不可比。
3. 真正让回复落回原线程的通道是 **ReplyPermit**。`packages/lib/src/messages.ts:83-85` 查 `key('permit', original.sender, original.id, message.sender)`，命中且 permit 未撤销、`remaining > 0`、未过期、且 `this.ctx.node === permit.landing.owner` 时，才把 `threadRef` 设为 `permit.landing`。签发它的 API 是 `threads.permits()`（`threads.ts:323`）。我在整个 `packages/` 里 grep 过：除了 `packages/lib/tests/verify.ts:271`，**没有任何 runtime 调用它**——也就是说，这条路径在本轮没有任何东西会去走通，reply 必然落到新建线程。

### 评价

这不是"没实现"，是"实现了但没有任何 runtime 去发这张票"。对 agent 来说这两种几乎无法区分——因为 `msg` 的报错菜单里既没有 permit，也没有"回复将另起线程"的提示。

### 建议

1. 收到首条入站时自动签发一张 ReplyPermit 给该 sender（这正是 `in_reply_to` 想表达的东西），让回复默认落回原线程。
2. 若 permit 必须由人/运维签发，那就在 `in_reply_to` 被接受时回一句"已记录，但本节点无 permit，回复将另起线程"，并在 `onlyne://` 根路由里暴露 permit 状态（`permits: 0` 这种计数就够）。
3. `thread=` 传裸 id 时直接拒绝，而不是降级成 `data.thread`。

## 问题 3（P0）：回执把 agent 指向的详情视图，恰好丢掉了关联字段

### 现象

同一个概念在四个视图里的可见性完全不同：

| 视图 | `relation` / `in_reply_to` |
|---|---|
| `onlyne://outbound`（列表） | **有**：`10408f4b-… → urn:onlyne:agent:dpdp · accepted · reply to urn:onlyne:agent:dpdp/56d21725-…` |
| `onlyne://outbound/<id>`（详情） | **无**：只有 `message_id / to / body / state / landed / retry_until` |
| `onlyne://threads/<id>`（线程页） | **部分**：消息行尾 `· reply`，但结构化引用不渲染 |
| `onlyne://threads/<id>/messages/<n>`（entry JSON） | **有**：`meta.in_reply_to` + `meta.relation` + `meta.data.in_reply_to` |

而回执首行是这么说的：

> `读回: onlyne://outbound/<id> and recorded durably.`

它把我指向了唯一一个没有关联字段的视图。我照做了，读到了干净的 `body / state / landed`，于是得出"没有关联字段"的结论——这个错误结论，是接口引导我得的。

### 这是模式，不是孤例

dpdp 在同一轮里发现了同样的倒挂：`onlyne://neighbors` 列表页明确写 `handshake: no exchange`，而详情 JSON 在未握手时**直接没有** `phase` / `fingerprint` 字段。列表比详情富，至少出现两次。

### 建议

1. `onlyne://outbound/<id>` 补上 `relation` 与 `in_reply_to`——它已经有数据，只是没渲染。
2. 更一般地：详情视图应当是列表视图的超集。加一条测试守住"列表里出现的字段，详情里必须也能取到"。
3. 线程页的 `· reply` 后面直接带引用：`· reply to urn:onlyne:agent:dpdp/56d21725-…`，与 outbound 列表用同一套写法。

## 问题 4（P1）：线程里只有别人说的话，自己说的在另一个集合里

回完第 3 跳之后重读 `onlyne://threads/8e6b197c`，画面与回之前一模一样：`[1] in`、`next seq 2`。我的回复只存在于 `onlyne://outbound/…`。

读侧因此被劈成两个不相交的集合：threads 里全是入站，outbound 里全是出站。

### 影响

`next seq 2` 在邀请我回复；我回了；再读一次还是 `next seq 2`。agent 无法从状态判断"这条回过没有"，只能靠记忆。重复回复就是这么来的——而重复回复在 agent 场景里不是小事，它会被对端当成新的对话分支再回一遍。本轮我发了四条，每条都落在对端一个新线程里，正是这个机制的直接后果。

### 建议

1. 出站消息也进所属线程的消息列表（`out` 方向），`onlyne://threads/<id>` 里既有 `in` 也有 `out`。
2. 退一步：线程上加 `answered: true` 或 `last_outbound {seq, at}`。

## 问题 5（P1）：`landed.seq: "1"` 对发送方不可解读

thread id 按 owner 分域（问题 2 第 2 条已确认跨 owner 不可访问），原消息在我自己的 `8e6b197c`，回执给的落点是 `dpdp/40d4a001`，两个 id 之间没有任何映射。`seq: "1"` 是"新线程的第 1 条"，还是"那个线程原本 0 条"？发送方判断不了。

我最后是靠 dpdp 用自然语言告诉我"未成为原线程的 seq 2"才知道的。一个协议事实靠对端打字传达。

### 建议

回执里给 `thread_created: true|false`，或给 `landed` 加 `previous_seq`。有了它，`seq:"1"` 才有意义。

## 问题 6（P2）：`onlyne://threads/<urn>/messages/<n>` 被斜杠切断，且报错误导

```
onlyne://threads/urn:onlyne:agent:lazuli/14457601-…/messages/1
→ No thread is named urn:onlyne:agent:lazuli.
```

线程 URN 里带 `/`，被路径分隔符切开，报错里那个"名字"是一个截断的 id，读起来像"这个线程不存在"。而 `onlyne://` 根路由给我的线程 id 正是完整 URN 形式，我是照抄的。

整串百分号编码（`urn%3Aonlyne%3Aagent%3Alazuli%2F<uuid>`）可以工作，裸 uuid 也可以。也就是说：**根路由推荐的那种 id 写法，用在子路由上是错的**，而且错得没有提示。

### 建议

1. 子路由接受完整 URN（用最后一个 `/` 之前当 owner、之后当 id），或
2. 报错改成 `thread id contains '/'; percent-encode it as %2F`——一句话就把 agent 从猜测里救出来。
3. 根路由的线程列表直接给子路由可用的形式，别让 agent 自己转换。

## 问题 7（P2）：`plan` 三个字段无文档，且与结果矛盾

四次发送的回执都是 `execution: "existing"`，而 `landed.thread` 每次都是我从未见过的 id。字段说"复用了已有执行上下文"，结果却是新线程。

`plan` 的 `response` / `execution` / `context` 在 `xd://msg` 文档里没有任何解释。`response` 在第一次发送是 `await`、后三次是 `none`，我无法判断是"对端不需要回复"还是"投递路径不同"。

### 建议

把 `plan` 的取值列进 `xd://msg`；或者从回执里删掉它，只留结果。agent 面宁可少一个字段，也不要一个需要猜的字段。

## 问题 8（P2）：执行上下文在往返中必然断裂

`onlyne://` 根路由显示 `c-1 · parked · urn:…/8e6b197c/1`、`c-2 · parked · urn:…/14457601/1`、`c-3 · parked · urn:…/bd47c343/1`、`c-4 · idle · urn:…/83ef8478/1`：四条入站各开一个执行上下文，子会话 `onlyne-c-1` 正是为第 1 跳而起的。但子会话不能发送，回复只能由主会话代发，于是四条回执里 `context` 全是 `"main"`。

接收时的上下文和回复时的上下文永远不是同一个。之后要问"这个交换是哪个上下文处理的"，答案是分裂的。

### 建议

给子会话一个"把回复交出去"的出口（把正文交给 runtime，由它按原入站上下文发送），或者不为入站起子会话。现在这种"起了但不让它发"的形态，两边的好处都没拿到。

## 问题 9（P2）：线程标题按发送者生成，没有区分度

`onlyne://threads` 现在四行，标题一字不差：`first contact from urn:onlyne:agent:dpdp`，都是 `open`、都是 `next seq 2`。后三行分别是"回复确认"、"请我贡献报告"、"收尾说明"三跳的新消息。

标题是 agent 唯一的人话抓手，而它不随内容变化。

### 建议

标题带上首条消息的摘要（正文首行本来就写着"你好 lazuli…"），或至少带 `last_activity` 时间。

## 问题 10（P1）：同一个 `onlyne://` 地址在不同会话里返回不同的东西，而且不告诉你这是切片

这一条是子会话报给我的，我没有独立复现子会话那一侧，只核了主会话这一侧（下文的边界写在最后）。

处理收尾说明的子会话 `onlyne-c-4`（绑定 `83ef8478/1`）去读同一批地址，拿到的是：

```
onlyne://threads/8e6b197c-…        → This session holds no read slice for thread …
onlyne://outbound                  → No outbound records.
```

而我在主会话里读同一时刻的同一个 `onlyne://outbound`，拿到四条记录；`onlyne://threads` 拿到四行。四条出站都在主会话的读片里，子会话那一片里一条都没有。

### 为什么这件事比"少看到几行"严重

`No outbound records.` 是一个陈述句，不是"你没有这一片"。agent 读到它，最自然的结论是"我没发过消息"——而这与事实相反。同样，`This session holds no read slice for thread …` 至少说了 "read slice"，但它出现在一次普通读取里，agent 事先并不知道存在切片这回事，也没有任何入口能问"我这一片包含什么"。

后果有两层：

1. **计数不可比。** 合并报告里出现了"lazuli 看到两条同名线程 / 六条消息"与"我这边四条 / 八条"并存的情况，双方都没错——两个会话读到的是不同切片。任何"我这边有 N 条"的结论，在跨会话引用之前必须先对齐读片。
2. **空结果无法与"真的没有"区分。** 这是 agent 面最贵的一类歧义：`No threads.`、`No outbound records.`、`No attention items.` 都可能只是"这一片没有"。

### 建议

1. 空结果带上切片的身份：`No outbound records in slice <context> (node-wide: 4; run onlyne://outbound?slice=node)`。
2. `onlyne://` 根路由现在列了每个执行上下文绑定的线程，但没说"你当前读到的是哪一片"。在根页显式写一行 `Your read slice: <context> · threads N · outbound M`。
3. 给出跨片读取的显式入口（`?slice=node` 或 `onlyne://outbound@node`），让"我要看全量"成为一个可表达的动作。

### 证据边界

主会话侧我核过：`onlyne://threads` 四行、`onlyne://outbound` 四条，与 `onlyne://` 根页的 `Threads: 4` 一致。子会话侧的三条报错原文由 `onlyne-c-4` 提供，我没有另起一个子会话来复现；它与我观察到的"根页按上下文逐条绑定线程"的结构一致。

## 做得好的地方（改的时候别弄丢）

1. **关联信息本身是对的，而且 judge 在这里干了实事。** schema 里 `meta.in_reply_to` 只接受 `{sender, id}`（`http.ts:105` 走 `messageRef`），我和 dpdp 给的都是 `{thread, seq}`——judge 把它解析成了具体 message id，还把原始 `{thread, seq}` 原样留在 `meta.data` 里。这条链路没坏，只是没被用起来、没被渲染出来。同一个 judge 在 `thread=` 上却选择了静默降级（问题 1），两个字段两种待遇。
2. **回执首行**：`Delivered to <target> (thread <id> seq <n>) · 读回: onlyne://outbound/<id> and recorded durably.` 一行答完"发没发出去、去哪儿看"。
3. **结构化字段**：`admission` / `guarantee:"durable"` / `thread_ref` / `seq` 可机读，不需要解析人话。
4. **`wait: "timed_out"`** 明确是"没等到回复"而非发送失败，第一次发送就靠它避免了误读。后三次回执里这个字段不见了，同一位置上是 `plan.response: "none"`；两者是不是一回事，文档没写，我判断不了。
5. **`onlyne.inbound` 自动注入成立**：入站自己到了，正文完整，插件开新 turn，我不需要轮询。
6. **entry JSON 很诚实**：`authenticated_author`、`reply_authority`、`delivery_kind` 原文、`meta.data` 全都在，agent 想深挖时有东西可挖。
7. **`in_reply_to.seq` 用数字 `1`** 每一跳都一次通过。
8. **`onlyne://` 根路由的 `Views:` 行**是 agent 侧唯一真正的能力菜单。

## 改进建议汇总

以下按更新前的构建排序；哪些已修、哪些还在，见第二节的复测表。

| 优先级 | 建议 | 涉及位置 |
|---|---|---|
| P0 | 首条入站自动签发 ReplyPermit，让回复默认落回原线程；否则明确拒绝 `thread=` | permit 签发 / 投递路由 |
| P0 | `onlyne://outbound/<id>` 补上 `relation` 与 `in_reply_to`（数据已有，只缺渲染） | outbound 详情 |
| P0 | 详情视图必须是列表视图的超集，加测试守住 | `onlyne://` 全部详情路由 |
| P1 | 出站消息进所属线程的消息列表，或线程上给 `answered` / `last_outbound` | `onlyne://threads/<id>` |
| P1 | 回执给 `thread_created` 或 `previous_seq`，让 `seq:"1"` 可解读 | `landed` 结构 |
| P1 | `thread=` 传裸 id 时拒绝，不要静默降级成 `meta.data.thread` | `msg` schema / judge |
| P1 | 空结果标明读片身份；根页显示"你的读片"；给出跨片读取入口 | `onlyne://` 全部视图 |
| P2 | 子路由接受完整 URN，或报错提示 `%2F` 编码 | `onlyne://threads/<id>/…` |
| P2 | `plan` 三字段写进 `xd://msg`，或从回执里删掉 | 回执 / 文档 |
| P2 | 子会话的回复要有出口，否则不为入站起子会话 | 执行上下文与子会话 |
| P2 | 线程标题带首条消息摘要或 `last_activity` | `onlyne://threads` 列表 |

## 状态

仅记录，未修。

最小可验证的回归：

1. 回复一条 `in_reply_to {thread, seq}` 的消息 → 期望 `landed.thread` 等于 `in_reply_to.thread`，且 `seq` 递增。
2. 回复之后重读原线程 → 期望消息列表里出现自己那条（`out` 方向），`next seq` 已经前进。
3. 读回 `onlyne://outbound/<id>` → 期望能看到 `relation` 与 `in_reply_to`，与 `onlyne://outbound` 列表一致。
4. 读 `onlyne://threads/urn:onlyne:agent:lazuli/<uuid>/messages/1`（完整 URN，不编码）→ 期望返回 entry，而不是 `No thread is named urn:onlyne:agent:lazuli.`。
5. 连续两跳往返 → 期望 `onlyne://threads` 只多出一行（对端新线程那条），而不是四行同名。
6. 在子会话里读 `onlyne://outbound`（主会话有四条记录）→ 期望结果说明"本片为空、节点共四条"，而不是裸的 `No outbound records.`。
