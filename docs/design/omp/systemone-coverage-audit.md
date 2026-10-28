# SystemOne 决策覆盖度审计

审计对象是两棵平行实现：`packages/omp-onlyne/src/decider.ts`（2050 行）与 `external-integrations/tern-onlyne/server/decider.ts`（346 行），加上两树中会把消息送进判定层、或者绕过判定层的入口。设计基准是 `docs/design/omp/systemone-hierarchical-decision.md`（下称「设计」），行为规范基准是 `docs/rfcs/0005-delivery-and-runtime.md`（下称 RFC 0005）与 `docs/rfcs/0016-systemone-context.md`（下称 RFC 0016）。

分类用语固定三种：**判定**指该节点至少一次 judge 调用；**推导**指无模型调用、由本地记录算出的确定性动作；**缺失**指该节点在两树中都没有对应代码。两棵树分开写的格子用「omp / tern」顺序。

## 1. 覆盖度表

### 1.1 设计清单的 11 个节点

| 决策节点 | omp-onlyne 实现位置 | tern-onlyne 实现位置 | 分类 | 证据 |
|---|---|---|---|---|
| O-1 `outbound.intent` | `JudgeDecider.#intent`，五选一 | `CarlyDecider.#intent`，五选一（`meta.to` 存在时被短路） | 判定 / 判定（带短路） | omp `decider.ts:263-293`；tern `decider.ts:78-82`, `128-147`, `101-114` |
| O-2A `relation_target` | `#reply` 解析引用 → `#relationQuestion` → `#targetQuestion`；回复权威锁定目标，冲突的 `to` 报错 | `#communicate` 直接取 `meta.to` 或 `reply.sender` 作为目标；`relation` 缺省即 `reply` | 判定 / 部分（关系与目标都不判） | omp `decider.ts:296-347`, `349-373`, `376-422`, `1379-1413`；tern `decider.ts:149-171`, `173-192` |
| O-3A `communicate.plan` | `#deliveryPlan`，`meta` 已给的值直接继承，其余提问 | `#plan` 提问；`meta.to` 短路时写死 `async`/`existing` | 判定 / 部分 | omp `decider.ts:425-488`；tern `decider.ts:209-238`, `112` |
| O-3A-Ctx `receiver_context` | `#receiverContext`：读对端 `/contexts`，可用多于一个才提问，`existing` 而无线索时报 `ambiguous_target` | 无：`send` 动作从不设置 `action.context`；接收侧也不读 `plan.context` | 判定 / 缺失 | omp `decider.ts:495-539`, `327-334`；`runtime.ts:168-177`；tern `main.ts:265`, `decider.ts:163-170`, `319-345` |
| O-2B `inspect.scope` | `#inspect`，候选 peers/inbox/messages/evidence/resources/none；`meta.scope` 走 `#explicit` 短路 | `#scope` 候选 directory/neighbors/threads/inbox/messages/message；`meta.scope` 在意图判定之后才生效 | 判定 / 判定（选项集不同） | omp `decider.ts:1024-1086`, `960-966`；tern `decider.ts:240-276`, `241-242` |
| O-3B-Th `inspect.thread` | `#threadQuestion`；只在 `messages` 且 `meta.thread` 空时提问 | 无：线程只从 `meta.thread` 的 `{owner,id}` 取 | 判定 / 缺失 | omp `decider.ts:1089-1130`, `1058-1060`；tern `decider.ts:243` |
| O-2C `thread.op` | `#threadAction`，六选一 | `#thread`：`meta.op` 命中即用，否则 `#op` 提问 | 判定 / 判定（带短路） | omp `decider.ts:1133-1179`；tern `decider.ts:278-291`, `300-313` |
| O-2D `domain.op` | `#domainAction`，六选一 | `#domain`，同构 | 判定 / 判定 | omp `decider.ts:1182-1218`；tern `decider.ts:293-298` |
| I-1 `inbound.routing_target` | `decideInbound`：绑定检查 → 回复义务 → 生命周期与发信权剪枝 → 提问 | `decideInbound` 恒返回 `wake`，没有候选集 | 判定 / 缺失 | omp `decider.ts:570-672`；tern `decider.ts:319-345` |
| I-2 `main_insertion_mode` | `#insertionMode`，三选一；`needsAnswer` 时撤掉 `fragment` 选项 | 无：恒 `follow_up` | 判定 / 缺失 | omp `decider.ts:738-773`；tern `decider.ts:344` |
| T-1 `todo.action` | `decideTodo`，按可用选项剪枝后判定 | 无：`CarlyBridge` 没有 `todo` 方法，库不欠待办 | 判定 / 缺失 | omp `decider.ts:780-876`；`messages.ts:161-175`；tern `bridge.ts:90-252` |
| T-2 候选提取与关联 | 与 T-1 同函数：associate 选题、append 选标题与说明 | 无 | 判定 / 缺失 | omp `decider.ts:812-875` |

### 1.2 代码里有、设计清单里没有的判定与推导

| 决策点 | omp-onlyne | tern-onlyne | 分类 | 证据 |
|---|---|---|---|---|
| I-0 `needs_answer`（回复义务） | `#needsAnswer`，语义提问；`needsReplyOf` 只作为 `wording_hint` 证据 | 发送方计划 `response === "none"` 直接定为否，其余才提问 | 判定 / 部分 | omp `decider.ts:679-711`, `1764-1769`；design `systemone-hierarchical-decision.md:229-232`；tern `decider.ts:320-322` |
| 发送侧 thread 归属 | `#threaded` + `reuseThread`：回复落原线程，`execution: new` 开新话题，其余复用与该对端最新的开放线程 | 无：只认 `meta.thread` | 推导 / 缺失 | omp `decider.ts:255-260`, `115-134`, `547-564`；tern `decider.ts:194-207` |
| 身份解析（别名 → 稳定 node id） | `#peerId` → `#knownIds`：trust 中非 `revoked` 的行 ∪ discovery 记录 ∪ 自身 | 无：`meta.to` 原样返回，`reply.sender` 原样返回 | 推导 / 缺失 | omp `decider.ts:1311-1358`, `1394`；RFC 0016 `0016-systemone-context.md:51`；tern `decider.ts:174-176` |
| `mentions` 解析 | `#mentions`：逐项过 `#peerId`，`@all` 保留 | 只过滤字符串，不解析 | 推导 / 缺失 | omp `decider.ts:1360-1377`；tern `decider.ts:160-162` |
| 引用解析 `{thread,seq}` → `{sender,id}` | `#reply` 用 `#entryAt` 在本地翻页定位 | 只接受 `{sender,id}` | 推导 / 缺失 | omp `decider.ts:1397-1412`, `1450-1460`；tern `decider.ts:56-61`, `150` |
| 投递可确认性（本条是否还能被确认消费） | 无节点：`inject` 内的内存缓存 + 上下文状态 | 无节点：`inject` 用发送方计划推断 | 缺失 / 缺失 | `packages/omp-onlyne/src/messages.ts:66-117`；tern `bridge.ts:157`；`packages/lib/src/client.ts:1161-1188` |

### 1.3 入口与旁路：谁进了判定层

| 入口 | 是否过 decider | 跳过的推导 | 证据 |
|---|---|---|---|
| `msg` 工具单发 | 是，完整链路 | 无 | `tool.ts:102-107` → `client.ts:675-683`, `766-787` → `runtime.ts:202-207` |
| `msg` 工具 `meta.to` 为数组 | 是，逐目标各跑一次 | 无；`#threaded` 每个目标各推导一次 | `tool.ts:81-101` |
| `meta.op = "draft"` | 是，经 `client.prepare` | 无；`relation` 与引用由模板强制先定型 | `drafts.ts:14-31`, `41-56` |
| `meta.draft_id` | **否**：直接 `client.send`，使用草稿时刻的 `plan` | 意图、目标、关系、执行计划、身份解析全部沿用旧值 | `drafts.ts:33-39` |
| `meta.op = "trust_accept" / "trust_revoke"` | **否**：走获取策略判定，模型不参与 | 意图（动作已给定）；目标由 `node_id` 字面量给出 | `tool.ts:63-75`；`handshake.ts:225-239`, `253-268` |
| 握手通知投递 | **否**：直接 `client.send`，计划写死 | 意图、目标、关系、执行计划、线程归属 | `runtime.ts:330-353`, `347` |
| `onlyne://` 读取 | **否**：自带路由，自判调用方权限 | 意图、查阅范围、子代理权限（另一套实现） | `urls.ts:126-134`, `136`, `77-79` |
| 面板（viewer）内发送 | 名义上过 `client.msg`，但未包 `runDecision` | 无判定作用域，`decide` 直接抛出 | `ui/viewer.ts:1273-1286`；`runtime.ts:202-205` |
| tern `/admin/msg` | 是 | 无 | `main.ts:412-419`, `456-464` |
| tern `meta.to` 显式 | **否**：`#explicit` 直接返回 `send` | 意图、关系、执行计划、接收上下文、线程归属、身份解析 | `decider.ts:101-114` |
| tern `meta.op` 为线程/域操作 | **否**：`#explicit` 直接返回 | 意图、操作类型判定 | `decider.ts:115-124` |
| tern 入站注入 | 部分：握手条目跳过判定 | 回复义务（改为读 `trust.actionable`）、路由、插入模式 | `bridge.ts:109-159`, `118-121` |
| tern 信任操作与握手投递 | **否**：`/admin/trust/*` 走 `TrustPipeline`；握手投递直接 `admit`，计划写死 | 意图、执行计划、上下文选择 | `main.ts:378-411`；`handshake.ts:446` |

## 2. 缺口清单

按影响面从大到小。每条先给证据，再给观察，建议另起一段并标明为建议。

### 2.1 投递确认没有决策节点，`new_subagent` 分支连存活检查都到不了

**证据**

- `messages.ts:96-100`：`decision.action === "new_subagent"` 时 `attempt.landed ??= await startSubagent(...)`，随后无条件返回 `{ consumed: false, outcome: "delivered" }`。
- `messages.ts:104-117`：存活检查写在这个返回之后，只覆盖 `context` 与 `wake` 两条路径。`new_subagent` 的决定永远走不到这段。
- `client.ts:1184-1187`：`injected.consumed` 为假就直接返回，`record.injected` 保持假；`client.ts:649-665` 的每轮回放因此每次都再调用一次 `inject`。
- `client.ts:173-175`：有未完成工作时的维护周期是 2000 ms，空闲 30000 ms。`client.ts:607` 把「有未注入的 inbox 行」算作未完成工作。
- `types.ts:51` 与 `binding.ts:119-124`：上下文状态取自会话事件，`"aborted"` 映射为 `lost`，其余状态一律映射为 `idle`。`messages.ts:110` 把 `idle` 与 `running` 都当作存活。
- 现场日志 `~/.omp/logs/omp.2026-10-27.2874.log`：`onlyne.inbound.inject` 共 3303 行（`grep -c`）。该文件第 196 行是条目 `urn:onlyne:agent:carly/f4d18cb7…` seq 1 的判定结果 `new_subagent`（21:00:35），第 3765 行是它的最后一次注入（22:47:32），间隔约 1 小时 47 分。
- 既有记录：`issues/injection-not-consumed.md:3`, `:29-33`。

**观察**

`new_subagent` 是一个确定性分支（`decider.ts:576-578`：发送方计划 `execution: "new"` 且本节点不欠回复），它不是 I-1 的候选项，所以设计 §5.1 的两道剪枝（`decider.ts:597-603`）对它不生效。子代理退出后，其上下文行停留在 `idle`，而 `idle` 同时表示「活着且安静」与「进程已不存在」。`messages.ts` 用同一个字段回答「这条投递还需要重试吗」，于是每 2 秒报一次 `delivered`，`consumed` 永远是假，维护环永远认为有工作。

`3235f2f` 的存活检查位置在 `new_subagent` 返回之后，只修了另一半。这一点可以单独验证：把该条目重新判定为 `new_subagent`，修复后的代码仍会每 2 秒报 `delivered`。

**建议**（未实施，供设计取舍）

把「这次投递是否仍可确认」列为一个独立节点，输入是可观测事实（会话是否仍被注册、其状态是否允许确认），输出三态：已确认、不可确认、拒绝。`client` 收到「不可确认」时把该 inbox 行标为不可投递并计数；当前行为是继续按 2 秒重投。上下文的 `live` 字段（`decider.ts:1539`）与 `#boundContext` 的 `dead` 判断（`decider.ts:727-728`）用的是同一个 `state` 字段，需要一并改为读可观测事实，否则同一类空洞会在三处重复出现。

### 2.2 「是否存在」「能不能发」「发去哪里」用了两套数据源

**证据**

- omp 目标候选：`runtime.ts:166` 的 `listPeers` 只收 `verified_at !== undefined && expires_at > now` 的 discovery 记录；`decider.ts:376-383` 在候选为空时抛 `not_found`，文案是「没有已发现的节点可以接收这条消息」（`decider-text.ts:130`）。这句文案把候选来源限定为 discovery 记录。
- omp 别名解析：`decider.ts:1329-1358` 的 `#knownIds` 用 trust 中非 `revoked` 的行 ∪ discovery ∪ 自身，trust 行先入集合。
- 同一节点因此有两种待遇：在 trust 表里有行、discovery 记录过期时，`meta.to` 能解析成稳定 id，I-1 的目标提问里它不存在，报「没有已发现的节点」。
- 接收上下文：`decider.ts:498-501` 读不到对端 `/contexts` 时抛 `unavailable`；`decider.ts:509` 对端没有可用上下文时返回 `undefined`，再由 `decider.ts:327-334` 抛 `ambiguous_target`。三种不可达分别映射到 `unavailable`、`not_found`、`ambiguous_target`。
- tern 候选：`main.ts:230-253` 把 discovery 记录与 trust 行（phase 为 `exchanged` 或 `requested`，且带 `base_url`）合并成一个 `node_id → base_url` 表；`decider.ts:174-176` 对 `reply.sender` 与 `meta.to` 都原样返回，不做解析。

**观察**

两侧对「候选对端」的定义不同：omp 用 discovery 新鲜度，tern 用 trust 相位加地址。omp 内部还有第三条规则（`#knownIds`）。RFC 0016 `0016-systemone-context.md:49` 要求「目标别名在当前目录中且可用」，把可用性写成判定前置条件；RFC 0016 `0016-systemone-context.md:51` 要求别名解析是确定性动作。代码把「目录里有」与「现在能连上」混在候选集里，于是过期但存在的对端既进不了候选，也拿不到一条说明它为什么不在的日志。

日志里另有一条同源现场：22:38 的入站消息 `b0256d47` 写着「你之前按 `to` 解析不到我是因为我在重启中换了端口」。重启期间 discovery 记录过期，`meta.to` 名字解析发生在 `#peerId`（能过），目标提名发生在 `listPeers`（过不了），两条路径给出相反结论。

**建议**（未实施）

把三件事各自的数据源写死：存在性读 trust 行（除 `revoked`）；可提名性等于存在性加一个可用地址（trust 的 `base_url` 或未过期的 discovery 记录）；可达性只由 transport 的错误码表达。目标提问的候选集改成「可提名性」，并把 `last_seen`、`stale` 作为候选标签里的事实交给判定，不在构建候选时删掉。

### 2.3 三条绕过判定层的入口有实质后果

**证据**

- 面板发送：`ui/viewer.ts:1285` 调用 `runtime.node.client.msg(input, { context: runtime.mainAgentId })`，外层没有 `runDecision`。`runtime.ts:202-205` 在 `currentDecision()` 为空时抛 `decision_unavailable`。该调用点不会被 `tool.ts:44-49` 的作用域覆盖。
- 草稿发送：`drafts.ts:37-38` 直接用 `draft.prepared` 里的 `to`、`message`、`plan` 调 `client.send`。`Draft` 的存储键是 `draft/<uuid>`（`drafts.ts:27-28`），没有 `(agent, 引用)` 索引。
- 信任动作：`tool.ts:63-75` 直接调 `runtime.acquireAccept/acquireRevoke`；判定由 `handshake.ts:225-239` 的 `judgeApproval` 完成，与 `JudgeDecider` 无关，`decider.ts` 里没有任何 trust 动作类型。

**观察**

面板入口按当前代码会失败：调用点没有作用域，`decide` 抛错，错误经 `ui/viewer.ts:1294-1296` 显示成发送失败。草稿入口能工作，代价是草稿之后的对端状态、线程状态、接收上下文变化全部被忽略。信任动作不进判定层是设计选择（RFC 0018 把审批交给获取策略），代码与设计一致；它的问题只在记录里：`decider.ts` 与 `decider-text.ts` 都没有 trust 动作的痕迹，审计设计文档的 11 个节点时容易误以为这条链路不存在。

**建议**（未实施）

面板发送用主会话 `ctx` 包一层 `runDecision`（形状同 `tool.ts:44-49`），让人类输入也走同一套判定与审计记录。草稿索引增加 `(created_by, in_reply_to)`，让「发上一份草稿」可以由插件解析，模型不必回填 `draft_id`。

### 2.4 模型被迫抄写结构体与长标识

判断依据固定两条：能否由插件从本地记录算出，以及推导输入是否已经在手。

| 字段 | 能否确定性推导 | 推导输入是否在手 | 证据 |
|---|---|---|---|
| `in_reply_to.thread` / `.seq` | 能：本地可把 `{thread,seq}` 翻成 `{sender,id}` | 在手；子代理任务文本里已经写入 thread 与 seq | omp `decider.ts:1397-1412`, `1450-1460`；`messages.ts:265-292` |
| `in_reply_to.sender` / `.id` | 能（经上一条） | 在手；`Entry.message.id` 就在注入的条目里 | 同上；`messages.ts:322-336` |
| `thread.owner` | 部分：发起者才拥有线程，成员写入时 owner 是对端，只有回执里有 | 部分；本地线程列表不带「对端线程」 | omp `decider.ts:547-564`；`client.ts:754-756` |
| `thread.id` | 能：`#threadQuestion` 已能从本地线程里选 | 在手 | `decider.ts:1089-1130` |
| `context` 别名 | 能：对端已发布，`#receiverContext` 就在这些别名里选 | 在手 | `decider.ts:495-539`, `512-513` |
| `draft_id` | 不能（当前无索引）；有 `(agent, 引用)` 索引后可推导 | 不在手 | `drafts.ts:27-28`, `76-80`（`tool.ts`） |
| `mentions` | 不能：「提到谁」是语义 | 名字解析在手，语义不在手 | `decider.ts:1360-1377` |
| `node_id` | 能：`#peerId` 已支持别名 | 在手 | `decider.ts:1329-1339` |
| `fingerprint` | 不能凭空推导，能从 trust 行或握手条目读 | 在 handshake 记录与 trust 表里 | `handshake.ts:134-137`, `151-157`；`tool.ts:258-262` |
| `resource.owner` / `.id` | owner 常是本节点或已知对端，可推导；`id` 是资源自身标识，不可推导 | owner 在手，id 不在手 | `decider.ts:943-957`, `1221-1236` |
| `page_size` / `cursor` / `seq` | 不能：属于调用方分页语义 | 不适用 | `decider.ts:1723-1729` |
| `operation` / `domain_id` / `expected_revision` | 不能 | 不适用 | `decider.ts:947-956`, `1208-1216` |

**观察**

`in_reply_to`、`thread.id`、`context`、`node_id` 四项的推导输入都在手，代码里也已有对应函数，模型仍在 `meta` 里手写。`tool-text.ts:10`, `:36`, `:42-44` 的用法示例把 `{ sender: "urn:onlyne:agent:dpdp", id: "<message id>" }`、`node_id`、`fingerprint` 直接摆在提示词里，等于把推导责任写在模板上。tern 的 `messageRef`（`decider.ts:56-61`）只认 `{sender,id}`，同一份模板在 tern 上多要求一次手抄。

**建议**（未实施）

在 `#explicit` 与 `#communicate` 之前加一层 `#resolve`（纯本地，无模型调用）：`meta.in_reply_to` 缺 `sender`/`id` 时用 `{thread,seq}` 补齐，`meta.thread` 缺 `id` 时用本地线程补齐，`meta.to` 一律过 `#peerId`，`thread.owner` 缺失时按「本节点拥有则本节点，否则对端」推断。`trust_accept` 接受 `node_id` 后由插件从 trust 表取当次 fingerprint，模型只在一个待批条目上作答。

### 2.5 模板常量替代了本该判定的东西

| 位置 | 常量 | 它替代了什么判断 |
|---|---|---|
| tern `decider.ts:112` | `response ?? "async"`、`execution ?? "existing"` | O-3A 的两个问题。RFC 0005 `0005-delivery-and-runtime.md:61` 要求无明确目标上下文时 `existing` 必须消歧或拒绝，这里直接给出 `existing` 且不带 `context` |
| tern `bridge.ts:98-100` | `supports()` 拒绝 `await` 与 `new` | O-3A 的候选剪枝，用宿主能力常量代替 |
| tern `bridge.ts:157` | `consumed = plan?.response === "none" \|\| needs_answer === false` | 「本条是否已被读到」。发送方声明的意图被当成读取证据 |
| tern `main.ts:265` | `receiverContexts = [{ alias: "carly", state: "idle" }]` | O-3A-Ctx；`state` 恒为 `idle`，与会话真实状态无关 |
| tern `main.ts:110` | `threads.page_size = 50` | 分页缺省值，写在配置里，判定层看不到 |
| tern `handshake.ts:446`、omp `runtime.ts:347` | `{ response: "none", execution: "existing", context: <固定别名> }` | 握手通知的投递计划，两树都不进 O-3A/O-3A-Ctx |
| omp `decider.ts:512` | `context.alias === "main"` 作为免消歧回退 | O-3A-Ctx 的判定；对端别名不叫 `main` 时回退成 `undefined`，随后 `existing` 报错 |
| omp `decider.ts:1734` | `attention` 缺省 `"thread"` | RFC 0005 `0005-delivery-and-runtime.md:53` 规定缺省 `inbox`，此处相反 |
| omp `decider.ts:169-172` | `INBOUND_CONTEXT_CAP = 3`、`RECENT_INBOUND_THREADS`、`RECENT_INBOUND_PER_THREAD` | I-1 候选集上限。设计 `systemone-hierarchical-decision.md:44` 允许 4 选 1，第 4 个以后的可答上下文被静默丢掉，没有日志 |
| omp `decider.ts:166-178` | `STATE_BUDGET_CHARS`、`TRIM_ORDER`、`HISTORY_ROWS`、`TEXT_CHARS`、`TRIGGER_CHARS` | 判定材料预算。裁剪有日志（`decider.ts:885`），顺序固定 |
| omp `decider.ts:180-185` | `REPLY_CUES`、`URGENCY_CUES` | 设计 `systemone-hierarchical-decision.md:232` 允许关键词作提示。omp 只把它们作为 `wording_hint` 与 `urgency_signals` 证据（`decider.ts:689`, `753`），与规范一致 |
| omp `tool.ts:151-156` | `error.retryable` 决定「重试」还是「改法」文案 | 「下一步做什么」由宿主模板决定，判定层没有输入 |
| `client.ts:173-175`（库） | 2000 ms / 30000 ms | 重试节奏。`decider` 不接收任何节奏参数 |
| omp `runtime.ts:78`, `:84-86` | 30 s discovery 刷新、60 s `retry_until` | 发现与变更窗口的时长 |

**观察**

`response`、`execution`、`context`、`page_size`、`attention` 这五项在两树中至少各有一处不经过判定层。其中 `attention` 的缺省值与 RFC 相反而没有测试覆盖，属于静默错误；`INBOUND_CONTEXT_CAP = 3` 与设计允许的 4 选 1 相差一个，属于静默丢弃候选。

**建议**（未实施）

缺省值只保留两种合法形态：规范明文规定的缺省，以及「无候选时报结构化错误」。其它一律交给判定。`attention` 缺省改回 `inbox` 以符合 RFC 0005 `0005-delivery-and-runtime.md:53`；`INBOUND_CONTEXT_CAP` 提到 4 并在超出时写一条 `onlyne.judge.trimmed` 记录。

### 2.6 两棵树的语义漂移

| 项 | omp-onlyne | tern-onlyne | 与规范一致的一侧 | 证据 |
|---|---|---|---|---|
| `meta.to` 显式时的处理 | 只锁定目标，关系与计划仍判 | 整条 `send` 直接返回，计划取默认值 | omp；RFC 0005 `0005-delivery-and-runtime.md:59` 要求一次调用只产生一个动作且意图匹配时返回歧义，设计 `systemone-hierarchical-decision.md:148` 规定「纯显式调用」才跳过 O-1 | omp `decider.ts:216-243`, `296-347`；tern `decider.ts:101-114` |
| 回复目标 | 解析 `reply_authority`，冲突的 `to` 报 `invalid_request` | 用 `reply.sender` 原值 | omp；设计 `systemone-hierarchical-decision.md:167-169` | omp `decider.ts:306-316`, `1394`；tern `decider.ts:174` |
| `relation` 缺省 | 有引用时提问 | 有引用时直接定为 `reply` | omp；设计 `systemone-hierarchical-decision.md:169` 只说 `meta.relation` 显式时跳过整个节点 | omp `decider.ts:299-305`, `350-373`；tern `decider.ts:151` |
| 接收上下文 | 读对端发布列表，缺失时 `ambiguous_target` | 从不设置 `action.context`，接收端也不读 `plan.context` | omp；RFC 0005 `0005-delivery-and-runtime.md:61`、`0005-delivery-and-runtime.md:27` | omp `decider.ts:495-539`；tern `decider.ts:163-170`, `319-345` |
| 查阅范围 | peers/inbox/messages/evidence/resources/none；线程列表只在 `meta.scope` 显式时保留 | directory/neighbors/threads/inbox/messages/message | omp；设计 `systemone-hierarchical-decision.md:215` 把线程列表划归 O-2C | omp `decider.ts:1030-1041`, `960-966`；tern `decider.ts:34`, `256-276` |
| 线程操作成员字段 | `meta.to` | `meta.member` | omp；RFC 0005 `0005-delivery-and-runtime.md:55` | omp `decider.ts:925`, `931`；tern `decider.ts:119`, `281-289` |
| 入站路由 | 候选剪枝 + `routing_target` 提问 + 插入模式 | 恒 `wake` + `follow_up` | omp；设计 `systemone-hierarchical-decision.md:233-236`、`:240-250` | omp `decider.ts:596-671`；tern `decider.ts:319-345` |
| 回复义务 | 语义提问，计划作为证据 | `response === "none"` 直接定为否 | omp；设计 `systemone-hierarchical-decision.md:229-232` 要求由判定模型回答 | omp `decider.ts:679-711`；tern `decider.ts:320-322` |
| 消费确认 | 只承认宿主观察，`inject` 从不自报 `consumed` | 首轮注入即按计划判 `consumed` | 两侧都不完全符合；规范要求宿主观察（RFC 0016 `0016-systemone-context.md:49`）。tern 会漏消息，omp 会重投 | omp `messages.ts:100`, `116`, `139`, `159`；tern `bridge.ts:157` |
| `mentions` | 逐项解析，未知别名报错 | 只过滤字符串 | omp；RFC 0005 `0005-delivery-and-runtime.md:53` | omp `decider.ts:1360-1377`；tern `decider.ts:160-162` |
| `expected_revision` | 资源与域操作都透传 | 资源与域操作都丢弃 | 域操作以 omp 为准，RFC 0005 `0005-delivery-and-runtime.md:49`、`:57` 列出该字段；资源动作字段表（`:47`）未列出，omp 的资源透传属于扩展 | omp `decider.ts:956`, `1216`；tern `decider.ts:95`, `123`, `297` |
| `page_size` 校验 | 1..100，越界抛 `invalid_request` | 任何数字都透传 | omp；`urls-text.ts:34` 的 1..100 是同一约定 | omp `decider.ts:1723-1729`；tern `decider.ts:251` |
| 身份解析 | 别名 → 稳定 id | 原样透传 | omp；RFC 0016 `0016-systemone-context.md:51` | omp `decider.ts:1311-1358`；tern `decider.ts:174-176` |

**观察**

12 项漂移里，omp 有 11 项与规范一致，tern 只有「消费确认」一项比 omp 更接近规范（tern 至少不会无限重投）。tern 的偏差集中在 `meta.to` 那一处短路：目标未解析、关系未判、计划取默认、上下文缺失、线程缺失五项都从 `decider.ts:101-114` 一个分支出来。

**建议**（未实施）

`meta.to` 短路改为「只锁定 `target`，其余问题照常提问」，缺省值交给判定层，无候选时报结构化错误。改完再跑一轮两侧的对照样本，漂移项里的其余各条逐条收敛。

### 2.7 同一字段在同一条路径上有两套解析规则（omp 内部）

**证据**

- `meta.thread` 为裸字符串：发送路径 `decider.ts:547-564` 在本地找不到匹配时，把裸 id 当作对端线程，得到 `{ owner: to, id: value }`；读取与线程操作路径 `decider.ts:1287-1297` → `1299-1309` 对同样的输入抛 `not_found`。
- `meta.scope = "messages"` 缺 `meta.thread`：`decider.ts:994-996` 抛 `invalid_request`；经意图判定进入 `#inspect` 时，`decider.ts:1058-1060` 会提问 O-3B-Th。
- O-2B 选项为 6 个（`decider.ts:1033-1040`），设计写 5 选 1（`systemone-hierarchical-decision.md:40`），多出来的是 `resources`。

**观察**

前两条让同一个缺失字段在不同入口得到相反结果：一处报错，一处提问。第三条是扩展，不违反「单步 ≤ 10 态」，与设计清单的 5 选 1 表述不一致。

**建议**（未实施）

发送路径遇到「本地无匹配且不含 `/`」的裸 id 时，先按「对端线程 id」处理并记一条 `onlyne.judge.derived` 日志，读取路径保持一致或明确要求 `{owner,id}`，两处择一并写进设计。`resources` 是否进入 O-2B，由设计文档补一行说明。

## 3. 结论：把「固定参数套模板」改成 decider 驱动，最小的第一步

三件事，按依赖顺序。

**第一步：把三条已有的确定性推导收进判定层的唯一出口。**

对象是线程归属（`decider.ts:255-260`, `115-134`）、身份解析（`decider.ts:1311-1358`）、`mentions` 与引用解析（`decider.ts:1360-1377`, `1397-1412`）。这三条都已实现、都不调用模型、输入也都在手，问题只在于它们散落在各个节点内部，且 tern 一侧完全缺失（`decider.ts:101-114`, `174-176`, `160-162`）。合并成一个 `#resolve` 出口后，模型不再手写 `owner`、`sender`、`message id`、`node_id`，模板里那些尖括号示例才能删掉。

选它作为第一步的理由：它同时解决「模型填结构体」（2.4）与「两侧身份语义不同」（2.2、2.6 的一半），改动集中在判定层入口，不触碰投递与存储。

**第二步：给投递确认补一个节点，让 `inject` 返回三态。**

对象是 2.1 的整条链路：`messages.ts:96-100` 的 `new_subagent` 短路、`messages.ts:104-117` 的存活检查位置、`client.ts:1184-1187` 的消费判定。这是唯一有生产事故证据的缺口（3303 次注入，1 小时 47 分，`issues/injection-not-consumed.md:3`），也是所有下游收敛的前提：只要还有条目停在「已投递未确认」，判定层再怎么改，维护环都在空转，该节点之后的来信都得不到处理。

顺序上它排第二：`#resolve` 改动会影响 `new_subagent` 的判定输入（发送方计划与身份），先把输入固定下来，再改投递状态机，避免两处同时变动难以归因。

**第三步：删掉 tern `#explicit` 的 `meta.to` 短路与写死缺省值。**

对象是 `external-integrations/tern-onlyne/server/decider.ts:99-126` 一个函数：`meta.to` 只锁定目标，`relation`、`response`、`execution` 照常判定，无候选时报结构化错误，不默认 `async`/`existing`。这一处是 2.6 里 5 条漂移与 2.5 里 2 条常量的共同源头，改完两侧才跑得动同一套对照样本。

选它作为第三步、并且只改一个函数的理由：漂移的其余各项（`context` 级联、入站路由、`expected_revision`、`mentions`）都依赖「`send` 动作的字段集合先一致」。字段集合一致之后，剩下的偏差才能用逐项样本判定。

三件事之外暂不动的项：`attention` 缺省（2.5，单点改动）、`INBOUND_CONTEXT_CAP`（同上）、面板入口（2.3，与判定正确性正交）、`meta.thread` 两套规则（2.7，需要先定设计取向）。

## 4. 只读自查

基线取在审计开始后第 10 分钟：`git status --porcelain` 37 行，其中 30 行 `M`、7 行 `??`，全部是并行会话的在途改动，与本次审计无关。审计期间 `git log` 读到 HEAD 为 `b4d2682`，基线那一刻的 `git rev-parse` 读到 `f23ffe6`，说明并行会话在审计过程中提交过。

审计写完后重新执行 `git status --porcelain`，与基线逐行 `diff`，差异只有本文件一行：

```
30a31
> ?? docs/design/omp/systemone-coverage-audit.md
```

没有修改任何源码、测试、配置、设计文档或 RFC。
