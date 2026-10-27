# dpdp 第五轮：更新后的会话阅读与真实回复续写

2026-10-26。测试节点为 daily-workbench/dpdp。本轮沿用第四轮运行数据与既有配对，重新从 `xd://msg`、`onlyne://` 发现功能。没有读取源码、数据库、日志或密钥，没有新增配对，没有修改实现。

最明显的进展有两项：历史往返现在能在 conversation 页完整阅读；真实回复能续写双方原线程。这两项分别解决上一轮的阅读困难与回复另建线程问题，在本轮执行的路径上均有直接结果。子会话草稿的权限前提仍需要解释，文档里的多目标约定存在直接矛盾。

## 已验证的变化

| 项目 | 本轮结果 | 证据与范围 |
|---|---|---|
| 入口导航 | `xd://msg` 公布 conversations、messages、回复省略 thread 的示例、草稿交接与人工配对边界 | 主会话直接读文档 |
| 历史会话聚合 | 旧 lazuli 问候链的 5 条消息同时显示 in/out、reply to、单消息地址 | conversation `dpdp/1d340808-8b74-4c0e-ac25-95ee14fcc251` |
| 分页 | 首次读取 page_size=2，跟随返回的 Next 地址得到后两组并显示 End of list | 非空 conversations 集合实际读取 |
| 旧读回失败 | `onlyne://outbound/dac1435e-d00d-4193-8bbf-fb3deccac4e0` 现在可读原文、landing、回复与阅读地址 | 同一条上一轮失败的记录 |
| 出站引用 | `onlyne://outbound/56d21725-0def-4247-80a4-86667839e7c5` 含 in_reply_to | 不再只在列表或 entry 中可见 |
| 线程导航 | 带完整 owner/id 的已发布本地 thread URI 可读，原样 URN 子路由也可读 | `dpdp/40d4a001…/messages/1` |
| 邻居详情 | 未配对的 cyanea 明确显示 phase:no_exchange、fingerprint:null | 与列表含义一致 |
| 未知路由恢复 | `onlyne://help` 报错指向 `onlyne://` 根页 | 无需继续猜路由 |
| 标题与列表 | threads 用正文摘要、方向、时间、message/conversation 地址辅助识别 | 取代列表中的重复 first contact 标题 |

以上为本轮实际观察，不代表所有旧问题或所有节点路径已经验证。

## 真实发送与回复

新版文档要求回复提供 relation 与 in_reply_to，并省略 meta.thread。dpdp 使用上一轮 lazuli 的真实问候回复作为引用：

```json
{
  "relation": "reply",
  "in_reply_to": {
    "sender": "urn:onlyne:agent:lazuli",
    "id": "ae623699-6657-4e54-a24c-0293a81a70e7"
  }
}
```

本轮只发送一次，请 lazuli 回一句确认。发送结果本身包含接收方确认，没有通过 inbox、线程或日志轮询。

| 消息 | 实际落点 | 投递或等待结果 |
|---|---|---|
| dpdp 的重测回复 `aa0799a8-17b5-4f48-875d-56912a3824bc` | lazuli 原线程 `8e6b197c-d86d-46b8-9f00-ba8000b114bd`，seq 2 | accepted / durable，thread_created:false，previous_seq:1 |
| lazuli 的确认 `b64b465a-5691-43c1-b184-22471c003652` | dpdp 原线程 `40d4a001-c4a9-4df3-b087-973284d37e47`，seq 2 | 发送结果直接返回 reply，wait:answered |

确认消息的 meta.in_reply_to 正确指向 `aa0799a8…`，正文说明未填 meta.thread、未转发。回执现在明确告诉发送方是否新建线程及之前的 seq，这些字段比只给 seq 1 更容易解释。

这证明本轮已配对的 dpdp/lazuli 路径能够在更新后续写旧线程。多目标、显式 thread 的拒绝行为、其它节点回复、未配对失败的新文案均未在本轮实测。

## 剩余问题与边界

### 多目标示例与约定互相冲突

> **状态（本轮）：已修。** `tool.ts:32` 改成「takes one node or a list of nodes」，并说明数组是逐目标投递、每目标一份结果；与 `:39` 的示例一致。

当前 `xd://msg` 第15行声明 meta.to 恰好一个节点，多目标需要每个节点一次调用。第22行的完整示例使用 `to: ["caerulea", "lazuli"]`，并承诺每目标一份结果。

一个新 agent 无法据此判断该用哪个形状。本轮没有为核对文字而批量发送，运行时是否接受数组仍未测。建议文档统一真实契约，并说明部分失败的结果结构。

### 历史消息的聚合顺序不一致

> **状态（6ec21cd）：已修。** `history.ts:108-122` 先按引用把同一条链并成一组（`:80-99` 的并查集），再按父先子后的顺序输出，缺时间的旧出站记录因此仍排在它回答的那条之后；`at` 为空只影响显示，不影响顺序。

同一组历史消息的 conversation 页顺序为问好出站、lazuli 回复入站、dpdp 确认出站、lazuli 结果入站。打开对应本地 thread 详情，前两条变成两次出站，然后才是第一次入站回复。

出站确认明确引用那条入站回复，页面仍把确认放在被引用消息之前。旧出站的时间在详情是 at:null，列表是 no time。本轮未查排序原因，也未判断新消息是否同样缺时间。

建议在缺时间的旧数据上保持回复引用的先后关系，至少让 thread 与 conversation 的同组消息顺序一致。

### 原始线程计数与聚合消息数需要区分

> **状态（6ec21cd）：已修。** 索引与详情现在都从 `history.list` 取（`urls.ts:779-781`、`:797-798`），同一个「messages」不再对应两个数量。

threads 索引把 `dpdp/40d4a001…` 显示为 1 messages，打开该 thread 详情则为 5 messages，同时保留 next seq 2。详情已聚合完整往返，这本身有用；同一“messages”标签对应两个数量，agent 难以判断 seq 属于哪个集合。

建议分别标注原始线程条目数与聚合会话消息数，并给各条消息的落点引用。conversation 页中多条记录显示相同的 [1]，也应解释这是各自原始线程的 seq。

### conversation 中的远端线程链接在本地无法读取

> **状态（6ec21cd）：按设计保留。** 对端拥有的线程不在本地读，`urls.ts:480-481` 注明这条边界；conversation 与 message 路由是本地可读的那一对。

旧问候 conversation 列出的 lazuli 线程地址 `onlyne://threads/urn%3Aonlyne%3Aagent%3Alazuli/8e6b197c-d86d-46b8-9f00-ba8000b114bd`，在 dpdp 本地返回 No thread is named …。

conversation 聚合是本地可读的，所列远端 thread URI 可用性则有边界。建议标注远端归属，并优先给本地可读的 conversation/message 地址。

### 子会话草稿有严格的源上下文限制

> **状态（6ec21cd）：限制保留，文档补齐。** `drafts.ts:16` 仍要求 `relation` 与 `in_reply_to`，`:19-21` 仍只允许子会话为自己上下文收到的那条源消息起草；`tool.ts:42` 与 `:45` 现在把这两条前提写在示例旁边。

普通 task 子会话 ExploreSubagentDraft 实测 read onlyne://threads、contexts、inbox 均可读；它没有持有任何 Onlyne context，结果为空。msg inbox 查询明确提示使用自己的 URI slice，整节点列表属于主会话。上一轮“只能读”的模糊拒绝现在有了实际阅读入口说明。

草稿仍有两项限制：

- 新问候草稿缺少 relation 与 in_reply_to，被拒为 `A reply draft needs relation reply or followup and in_reply_to.`。
- 主会话把真实 lazuli 来信引用交给它后，回复草稿被拒为 `A child may draft a reply only to the source message assigned to its context.`。

子会话未得到 draft_id，未发送、未绕过权限。这与它持有 0 个 context 的观察一致。不能据此断言分配了入站消息的子会话也无法起草。

文档给了草稿示例，示例旁尚未明确“仅限当前 context 被分配的源消息”。建议补齐这一前提，并说明主会话通常派出的 task 子会话与 Onlyne 入站上下文之间的区别。要验证 draft_id 交接成功，需要一次真正分配给子会话的入站，当前普通 task 无法替代该条件。

### 自然语言状态查询的返回范围仍难预测

> **状态（6ec21cd）：未修。** 状态查询仍只投影邻居数组，不区分自身节点、邻居与发现候选。

“show my node status and region membership”本轮仅返回邻居数组，没有自身 node 与 region。数组同时包含 cyanea 的两个 endpoint，一条 verified:true，一条 verified:false。

本轮未查各来源候选的语义，不把两条记录直接判定为发现故障。建议查询结果说明覆盖范围，让 agent 区分节点状态、邻居与发现候选。

### 资源节点在 agent 面没有入口

> **状态（本轮，黑盒）：未找到入口。** 以下为 agent 可见面的实测，未读实现，不判断注册侧是否已接上。

用户接入 Feishu IM 后询问能否看到。dpdp 从五个方向探测，全部没有资源信息：

| 探测 | 结果 |
|---|---|
| `onlyne://` 根页 Views | 只有 neighbors、threads、inbox、contexts、outbound、conversations |
| `onlyne://resources` | `Unknown onlyne:// route resources. onlyne:// lists the available views.` |
| msg 意图「列出资源与 IM 网关」 | `The decision named no read scope; the readable channels are peers, inbox, messages and evidence.` |
| msg 加 `meta.scope:"resources"`、`meta.op:"resource"` | 同一拒绝 |
| `onlyne://neighbors` 及详情 | 只有三个 agent，字段仅 node_id、base_url、region、last_seen、verified、phase、fingerprint |
| inbox、conversations、contexts、outbound | 无任何资源记录 |
| `.onlyne/config.toml` | 无 resources 段 |

`xd://msg` 的标题写「messages, threads and resources」，schema 里也有 `resource {owner,id}` 和 `operation`，但全文没有资源读取路径、操作列表或示例。

后果是 agent 无法回答三个最基本的问题：我有没有资源、它在不在线、它能做什么。没有 owner/id 可引用，也无法凭空猜一个去试，所以这一轮没有向资源发送任何调用。

建议在根页加一条资源视图，或在 neighbors 详情里列出该节点对外提供的资源与操作；至少在 `xd://msg` 说明资源引用的形状与可执行操作。若资源注册仍属调试中，报错应直说「本节点未注册资源」，而不是把意图判成非法读范围。

### 网关接入后：资源以邻居和消息的形式出现

> **状态（同日稍后）：部分可读。** 上面那张表是接入前的现场，保留不动。以下是接入 Feishu IM 之后重新探测的结果。

节点重启后（endpoint 变为 `http://127.0.0.1:62923/onlyne/v3`），`onlyne://neighbors` 多出两条：

```
- urn:onlyne:gateway:im · no region · verified · handshake: exchanged · last seen 2026-10-27T03:55:58.738Z · fp 0123456789abcdef
- urn:onlyne:gateway:mcp · no region · verified · handshake: no_exchange · last seen 2026-10-27T03:55:32.911Z
```

一条来自 Feishu（Lark）的消息已经落库。`onlyne://inbox` 最后一页列出它，`onlyne://conversations` 里对应标题为 notify 的一组：

| 字段 | 值 |
|---|---|
| 消息 id | `im-notify-urn:onlyne:gateway:im-lark-oc_example-1` |
| 发送者 | `urn:onlyne:gateway:im` |
| 正文 | `111` |
| 时间 | `2026-10-27T03:55:29.958Z` |
| 平台与来源 | `platform: "lark"`，`conversation: "oc_example"`，`from: "ou_example"` |
| 资源引用 | `resource_ref { owner: "urn:onlyne:gateway:im", id: "lark-oc_example" }` |
| 提及 | `mentions: ["urn:onlyne:agent:dpdp"]` |
| 状态 | entry 写 `delivery: "received"`、`consumption: "pending"`；inbox 行写 `acked` |

根页的 contexts 也新增了一条由它派生的上下文：

```
- `c-1` · idle · onlyne urn:onlyne:gateway:im/im-lark-oc_example/1 · urn:onlyne:gateway:im/im-lark-oc_example · onlyne-c-1 · source urn:onlyne:gateway:im/im-notify-urn:onlyne:gateway:im-lark-oc_example-1
```

> **状态（同日更正）：上述结论不成立。** 网关投递确实派生了 `c-1`（alias `onlyne-c-1`）并起了回合，只是处理耗时约 5 分钟，dpdp 查看时它尚未完成、状态显示 idle。该子会话读到来信全文并起草了回复 `c0901da8-7b81-4bb4-beb3-a4dfeed0dc88`。主会话当时读到的是未完成的中间态。`consumption: pending` 是当时的实时值，不是漏投证据。

由此可以确认：资源是可发现的，路径是邻居列表加消息元数据，而不是根页的资源视图。根页 Views 仍是原来六项，`onlyne://resources` 依旧返回未知路由。agent 只有收到过该资源的消息，才拿得到 `resource_ref`。

同一组数据里的地址一致性问题：inbox 行给出的 thread 地址是 `onlyne://threads/im-lark-oc_…/messages/1`（不带 owner），conversation 页给出的是 `onlyne://threads/urn%3Aonlyne%3Agateway%3Aim/im-lark-oc_…`。两种写法在本地都返回 `No thread is named …`。conversation 与 message 两条路由可读，`?thread=` 与 `?message=` 过滤都能定位到这一组。

建议：

1. 根页增加资源视图，或在 neighbors 里区分 agent 与 gateway，并列出 gateway 提供的操作。
2. 消息已经带 `mentions`，但被提及的节点没有收到回合。若这是刻意（资源消息只入库不打扰），应在 entry 与文档里说明；若不是，`consumption: pending` 就是可观测的漏投信号。
3. 资源拥有的 thread 在本地不可读，建议标注归属，或直接给本地可读的 conversation/message 地址，避免列出两种都打不开的写法。

## 主观体验与未测事项

这轮我能从工具说明直接找到导航，并在一个 conversation 页读完上轮往返。旧读回地址能打开，出站关联也能读到，明显减少了需要对端用正文解释的工作。真实回复落到 seq 2，且回执带 previous_seq 和 thread_created，我不再需要猜“又新开了吗”。

当前最需要完善的是说明的一致性：单目标规则与数组示例冲突、同一 thread 的两种计数、普通子会话与入站子会话的草稿权限前提。历史 thread 页的消息顺序也会影响阅读连续性。

用户随后确认新版 TSP 渲染没有问题，本报告将其记为用户验收通过。dpdp 未自行查看 Tern 画面，steer/followup 的选择比例仍未验证。未测试缺钥错误的新结构、多目标部分失败、显式 thread 的拒绝，也未成功走完子会话 draft_id 交接。资源节点与人工配对继续按用户既定边界处理。

## 多 thread 操作实测

用户要求同时操作多个 thread。dpdp 分三个批次提交共六次发送：先同时提交 lazuli 的 A 线与 caerulea 的 B 线，再以 B、A 的顺序提交第二轮，最后同时提交 lazuli 名下的 A、C 两条不同会话。每条请求都带独立的计算或标签，并以对应来信 sender/id 回复，省略 meta.thread。

| 会话 | 远端原线程 | 本地原线程 | 本轮远端及本地 seq | 回复内容 |
|---|---|---|---|---|
| A：lazuli 问候线 | `8e6b197c…` | `40d4a001…` | 3、4、5 | A线 蓝鲸 43；A线 蓝鲸 51 结束；A线 蓝鲸 仍独立 |
| B：caerulea 问候线 | `8fcf842b…` | `a0083a93…` | 2、3 | B线 珊瑚 53；B线 珊瑚 106 结束 |
| C：lazuli 报告线 | `bd47c343…` | `6066de43…` | 2 | C线 海星 仍独立 |

六次均为 accepted/durable、thread_created:false、wait:answered。每份回复的 meta.in_reply_to 均准确指回本次请求的消息 id，回复正文、发送者、线程落点与 seq 均各归各线。

同一节点的双线程是关键对照：A 线最后一次发出消息 `67fc2518-d1b0-4105-9511-eac164d2f828`，落到 lazuli/8e6b197c 的 seq 5，回复落到 dpdp/40d4a001 的 seq 5。C 线发出消息 `4e7428ef-df47-4556-a1dc-0c20ef6cd0ea`，落到 lazuli/bd47c343 的 seq 2，回复落到 dpdp/6066de43 的 seq 2。同一接收节点没有把两条会话合并。

主会话没有为等回复而轮询，六份回复都直接随发送结果返回。可确认的是主会话能在多个 thread 之间交错操作，并保持消息关联与落点隔离。工具采用同批提交；本轮没有测量网络请求时间是否重叠，因此不把它写成底层并发或压力测试。多节点多线程操作与多目标数组发送也分别计数，后者仍未测试。

补充一处已修：新版发出的消息在 outbound 里带时间戳（`aa0799a8…` 显示 `2026-10-26T04:03:55.118Z`），`no time` 只留在更早的旧记录上。

本报告是新版重新探索的记录，保留上一轮报告作为历史基线，不覆盖旧现场证据。
