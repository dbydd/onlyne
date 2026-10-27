# Onlyne v3 库设计

本文描述 `packages/lib` 的内部组织、存储布局、维护环、密钥存放与签名传输。它记录的是实现选择：模块怎么分、常量取多少、重试由谁推进。线上身份、消息、提交与恢复语义在 RFC 中规定；库必须完成什么、宿主必须提供什么，在 [lib 宿主契约](../../contracts/lib-host.md) 中规定。设计改变不需要改协议，但改变可观察行为的实现调整仍需回到契约检查。

## 1. 库与宿主的边界

库处理协议逻辑，宿主处理运行环境。这个分工决定了下面所有模块的输入输出形状。

| 库负责 | 宿主负责 |
|---|---|
| 消息接纳、thread 与成员表、注意力记录、资源操作记录、信任状态、幂等与去重 | 持久存储实现、身份密钥的读写、地址解析、决策模型调用、进程与定时器 |
| 生成 HTTP handler（`createHttpHandler`）、发起带签名的请求（`FetchTransport`） | 绑定端口、启动进程、在 `session_start` 时装配 |
| 维护环的节奏、待办重放、出站重驱、信任重驱 | 提供 `MaintenanceClock` 的三原语，或自己按需调用 `maintain()` |

库不主动访问文件系统。唯一的 Node 内置依赖是 `node:crypto`，用于 Ed25519 签名、SHA-256 摘要与随机标识。所有持久事实都写进宿主给的 `Storage`，并落在同一个事务里。

HTTP 资源操作在认证并解析请求后调用宿主的 `prepareResourceOperation(auth, ref, request)`，供 IM 网关按会话坐标创建动态资源。库随后检查资源权限并执行操作。宿主通过这个钩子复用已验证身份，请求的签名与 nonce 只校验一次。

## 2. 模块布局

| 文件 | 内容 | 对外符号 |
|---|---|---|
| `core.ts` | 上下文、错误码到 HTTP 状态的映射、持久键编码、规范序列化、摘要、分页、接纳事务的只读视图 | `OnlyneError`、`fail`、`key`、`canonical`、`same`、`fingerprint`、`page`、`createContext`、`admissionView` |
| `types.ts` | 协议类型、插槽接口、通知与追踪事件 | 全部数据类型；`trustPhase` |
| `node.ts` | 把各服务装成一个节点，决定传输层是否启用签名 | `OnlyneNode`（`threads`、`messages`、`resources`、`domains`、`discovery`、`keys`、`trust`、`client`、`transport`、`fetch`、`auth`） |
| `threads.ts` | thread 生命周期、成员与读写权限、授权通知、已知 thread 表 | `ThreadService`、`canRead`、`getThread` |
| `messages.ts` | 发送接纳、首接触建 thread、回复许可、注意力记录、列表与回执 | `MessageService` |
| `resources.ts` | 资源注册、操作记录、事件积压与确认、交接邀请 | `createResourceService`、`ResourceService` |
| `domains.ts` | 单域成员关系、加入/退出/查询、候选成员发现 | `DomainService` |
| `discovery.ts` | 服务描述核对、候选记录新鲜度、解析与刷新 | `DiscoveryService` |
| `keys.ts` | 密钥材料校验、请求与响应签名、信任交换状态、轮换、重驱 | `KeyService`、`TrustService`、`keyFingerprint`、`CLOCK_SKEW_MS`、`EXCHANGE_RETRY_MS` |
| `transport.ts` | 带身份解析与签名校验的 HTTP 客户端 | `FetchTransport`、`SigningTransportOptions` |
| `http.ts` | `/onlyne/v3` 路由、认证、分页、流式读取（15 秒轮询分页） | `createHttpHandler` |
| `client.ts` | 单一入口 `msg`、发送与等待、消费确认、待办重放、维护环 | `OnlyneClient` |
| `memory.ts` | 测试用内存存储 | `MemoryStorage` |

`OnlyneNode` 的装配顺序有依赖：`KeyService` → `TrustService` → `FetchTransport`（需要 trust 才能签名）→ `DomainService`（经传输访问对端）→ `OnlyneClient` → 把 client 绑回 trust（信任提交要能唤醒维护环）。宿主若自己实现 `Transport`，必须复用节点的签名逻辑，否则对端会按未认证拒绝。

## 3. 存储与键空间

`Storage.transaction` 提供一次原子提交：读到本事务已写入的值，`scan(prefix)` 返回匹配行。返回顺序由实现决定，库不依赖它，需要顺序的调用方自己排序。库不假设跨事务隔离级别，也不做乐观版本比较以外的并发控制；宿主的 SQLite 实现把事务串行化（`BEGIN IMMEDIATE` 加尾部队列），并按主键排序返回扫描结果。

接纳策略插件（`mayAdmit`、`inboundThread`）在接纳事务内部执行，所以库通过 `AdmissionContext` 把该事务的只读视图交给它。策略 MUST 用它读信任阶段，MUST NOT 自己调用 `storage.transaction`：宿主的事务是串行的，嵌套事务要等外层释放，而外层正等着策略返回。

所有键由 `key(...parts)` 生成：每段 `encodeURIComponent` 后用 `/` 连接。因此扫描前缀必须是完整的第一段或前几段，不能用编码后的片段猜行。

| 前缀 | 值 | 写入者 | 用途 |
|---|---|---|---|
| `thread/<owner>/<id>` | `Thread` | threads | 序号、状态、事件序号、标题 |
| `<owner>/<thread>/<seq>` | `Entry` | messages | thread 条目本身，不带前缀 |
| `entry-ref/<sender>/<message-id>` | `Entry` | messages | 按全局消息身份反查落点与回复权威 |
| `member/<owner>/<thread>/<peer>` | `Member` | threads | 成员状态、`read`/`post` 权限、读范围 |
| `event/<owner>/<thread>/<id>` | `Event` | threads | 成员与消息的 thread 事件流 |
| `notice/<id>` | `Grant` | threads | 尚未确认的读授权通知 |
| `known/<owner>/<thread>` | `Known` | threads | 本节点已获准读取的 thread |
| `default/<owner>` | `ThreadRef` | threads | 首接触消息的默认落点 |
| `attention/<owner>/<thread>/<obligation>` | `Attention` | messages | 需关注条目及其计划 |
| `inbox/<obligation>` | `LocalAttention` | messages | 本节点持有的注意力项：是否注入、落在哪个上下文 |
| `permit/<sender>/<id>/<reply-author>` | `ReplyPermit` | messages | 回复许可与剩余次数 |
| `message/<sender>/<id>` | `Remembered` | messages | 已接纳消息的摘要、回执或拒绝记录 |
| `message-key/<actor>/<idempotency-key>` | `Binding` | messages | 发送幂等键到消息身份的绑定 |
| `idempotency/<actor>/<operation>/<key>` | `{digest, result}` | core | 修改类接口的幂等槽 |
| `outbound/<sender>/<id>` | `StoredOutbound` | client | 出站记录：请求、计划、状态、摘要、上下文 |
| `consumed/<context>/<owner>/<thread>/<seq>` | 消费记录 | client | 所有投送路径共享的消费槽 |
| `todo/<context>/<owner>/<thread>/<seq>` | `PendingTodo` | client | 尚未成功写入宿主的待办 |
| `decision/<id>` | 判定输入与结果 | client | 每个判定点的请求快照、模型回答或失败 |
| `domain/<domain-id>` | `DomainMembership` | domains | 本节点的成员关系 |
| `discovery/<node-id>/<base-url>` | `DiscoveryRecord` | discovery | 候选地址、实例、声明、核对时刻、失败原因 |
| `trust/<node>` | `TrustEntry` | trust | 对端公钥、两个批准位、历史指纹 |
| `trust-outbound/<node>` | `TrustOutbound` | trust | 待送达的签名陈述：nonce、本方密钥指纹、上次尝试时刻；轮换义务另带旧私钥 |
| `nonce/<actor>/<nonce>` | 时间戳 | trust | 已消费 nonce，按 5 分钟窗口清理 |
| `resource/<owner>/<id>` | `Resource` | resources | 资源描述与可见范围 |
| `resource/<owner>/<id>/operation/<id>` | `OperationRecord` / `Intent` | resources | 操作记录与待结算意图 |
| `resource/<owner>/<id>/event/<id>` | `ResourceEvent` | resources | 资源事件 |
| `resource/<owner>/<id>/source/<id>` | 事件来源去重 | resources | 平台事件 ID 的作用域内去重 |
| `resource/<owner>/<id>/pending` | `ResourceEvent[]` | resources | 尚未投给驱动的积压 |
| `resource/<owner>/<id>/handover/<id>` | `Handover` | resources | 交接邀请与结果 |

进程内状态不落盘：`#waiters`（按原消息索引的等待者）、`#held`（已作为工具结果返回、尚未确认消费的回复）、`#loop`（维护环句柄）。进程重启后等待者消失，出站记录与消费记录仍可恢复，这也是恢复逻辑必须区分「等待者还在」与「出站记录未结案」的原因。

## 4. 维护环

维护环由库拥有，宿主只借出时钟。构造函数只在 `plugins.maintenanceClock` 存在时建环；`runMaintenance()` 在缺时钟时返回 `unsupported_delivery`，并说明「自己调度 `maintain()`」。

| 参数 | 值 | 含义 |
|---|---|---|
| `ACTIVE_INTERVAL_MS` | 2000 | 有欠账、或刚被唤醒时的节奏 |
| `IDLE_INTERVAL_MS` | 30000 | 持久账本无欠账后的下限 |
| `EXCHANGE_RETRY_MS` | 60000 | 介绍信发送失败后，多久允许重驱 |
| `CLOCK_SKEW_MS` | 300000 | 签名时间戳窗口，也是 nonce 保留期 |
| `DRIVER_WAIT_MS` | 30000 | 资源驱动单次调用最多等多久，超时记为 `unknown` |
| `FRESH_MS` | 120000 | 发现记录的新鲜期 |

一次 `#tick` 的顺序固定：

1. `maintain()`：先重放欠下的 todo，再重放未注入的 inbox 行，最后 `reconcile()` 结算出站记录。单项失败不阻断其余项，整轮结束后以单错误或 `AggregateError` 抛出，失败记录保持欠账状态。
2. `recover()`：扫描 `outbound/` 中 `digest` 存在且状态为 `pending`/`unknown` 的行。`retry_until` 已过则落为 `unknown` 并记 `outbound.expired`；未过则重新发送并记 `outbound.redriven`。
3. `trust.redrive()`：对仍有送达义务的陈述，超过重试窗口后用新 nonce 重发。义务覆盖 `requested` 与 `exchanged` 行；没有响应、响应丢失、响应验签失败都保留义务，下一轮再试。

一轮结束后才计算 `hasPendingWork()`，只在结果变化时发 `work-pending` 通知，并按结果重设节奏：

```text
inbox 行未确认或未注入（且未过期）
或 todo 行未 done
或 outbound 行 digest 存在且状态 pending/unknown
或信任送达义务 / 临时尝试标记存在
→ 2000ms；否则 30000ms
```

这个判据同时是「空转成本」的定义：空闲节点的下限是 30 秒一次空事务扫描，不产生模型调用、不产生网络请求。

### 4.1 信任送达义务

本地批准与「批准陈述已被对端持久记录」是两个事实，维护环分别对待：

- 本地 `confirm` 与 `rotate` 在提交信任变更的同一事务里写入一份待送达陈述（出站义务）。义务与信任行同生命周期，但不由信任行的阶段推导。重驱在 `maintain()` 内执行，因此无时钟宿主只调 `maintain()` 也能送达。
- 义务只在对端针对**本次投递**返回签名的信任陈述时清除。该陈述与本请求 nonce 绑定，并证明对端已持久处理这份陈述；接受与拒绝（含 `approved=false`）都属于送达事实。
- 经其他路径收到的入站陈述不解除出站义务。对端自己发起的交换不代表我们这份陈述已经到达。
- 重驱使用新的 nonce，因此重放不会被对端的 nonce 缓存误判为重复。重驱闸门是 `EXCHANGE_RETRY_MS`，不是信任行的阶段。
- 不可认证的拒绝不结束义务：`/introductions` 的 `forbidden`/`id_conflict` 只把 `blocked` 设为「下次允许尝试的时刻」（`REFUSED_RETRY_MS` = 600 秒），到期后清掉并重试。`send` 保留对端返回的错误码；验签失败与时钟拒绝直接保留义务。
- `rotate` 在有未清除的轮换义务时返回 `conflict`：一次只有一个在途轮换，对端不会收到跳代的证明。
- 轮换窗口 `security/rotation` 与义务同事务写入；最后一条轮换义务清除或该对端被撤销时关窗。没有对端时轮换不写窗口。`redrive` 遇到「窗口在、本方钥仍是旧的」时分两种：本进程正在换钥（进程内标志）就等，宿主 hook 多慢都等；进程重启后的遗留准备则丢弃窗口与义务，因为此时旧钥仍是本节点当前钥，对端没有丢失任何可验证的连续性。
- 未被退避的义务存在时 `hasPendingWork` 为真，维护环保持活跃节奏，直到义务清除或用户撤销该对端；退避中的义务由空闲节奏照看，到期后恢复为待办。

实现对应：`TrustService.request` 与 `rotate` 在同一事务里写入 `trust-outbound` 行；`send` 只在收到 `reply_nonce` 匹配的签名回执后删除该行；`redrive` 扫描这些行而不看信任行阶段，`exchanged` 行同样补发；本方密钥指纹与行不一致（轮换前的旧义务）或对端已撤销时直接删除。

唤醒路径只有一条：`wakeLoop()` 把节奏切回 2000ms。`send()`、新入站接纳、信任提交都会触发它。正在执行的一轮不会重入；期间到达的唤醒只把下一次 tick 排到活跃节奏。`stopMaintenance()` 中止控制器并清掉计时器，重启时新建控制器。

维护环不做判定。入站消息要做什么由插件调用判定模型决定，库只执行已提交的计划，所以 2 秒节奏不会变成模型轮询。插件侧对同一条目缓存判定结果，重驱时复用记录而不是再问一次。

## 5. 密钥存放指导

库只消费 `plugins.keys` 钩子：一个返回 `KeyMaterial` 的函数，可选 `replace(material)` 支持轮换。密钥落在哪、权限如何检查，属于宿主实现；下面是 OMP 插件采用的规则，也是其他宿主应当照做的基线。

| 项 | 规则 |
|---|---|
| 算法与格式 | Ed25519。私钥是 PKCS#8 DER，按 base64 存放；公钥是 32 字节 raw，按 base64 存放；指纹是公钥 SHA-256 的前 16 位十六进制 |
| 作用域 | `project`：`<项目>/.onlyne/keys/<node-id>/`；`global`：`~/.config/onlyne/keys/<node-id>/`；`none`：不建密钥，只允许回环开发绑定 |
| 文件 | `priv.pem` 与 `pub.key`。私钥文件权限 `0600`，目录 `0700`。普通创建不覆盖已有私钥；轮换只能经 `replace` |
| 属主与链接 | 拒绝符号链接、拒绝非当前用户拥有的文件，拒绝组/其他可读 |
| git 检查 | 项目作用域必须被 `git check-ignore` 覆盖。未覆盖时拒绝启动，并同时给出两条出路：把 `.onlyne/keys` 加入 ignore，或改用全局作用域 |
| 写入方式 | 临时文件加原子改名。轮换时先为每个非撤销关系写一条带旧私钥的送达义务，再换钥：对端没收到证明时由维护环补发，进程重启也能继续。旧私钥随义务留在 `trust-outbound`，义务清除即删除；轮换期间不得再轮换，因此同时最多留一把旧钥。这些行落在宿主的 storage 里，storage 的保密要求见《lib 宿主契约》§1 |
| 材料校验 | 加载时用私钥派生公钥；与给定公钥不一致返回 `unauthenticated`。改换密钥后清空内存缓存 |
| 无密钥绑定 | `plugins.keys` 缺省即无签名绑定：只允许回环主机，认证退化为 `Onlyne-Actor` 头。面向远程地址启动必须建密钥 |

密钥不写进配置、不进仓库、不进日志。`pub.key` 只是本节点公钥的便利副本；对端公钥只存 `trust/<node>`，文件系统里没有对端公钥。已确认记录的权威来源始终是信任表。

## 6. 签名传输

`FetchTransport` 把身份解析、地址约束、请求签名和响应验签放在一处，任何宿主自定义传输都应复刻这些检查。

- **地址**：`resolve(node)` 返回的 `Target.node_id` 必须等于请求目标，否则 `unavailable`。`base_url` 可以带服务前缀；请求路径相对它拼接，不允许跳出 origin。回环地址允许明文 HTTP，其他主机必须是 HTTPS。
- **头部**：`Target.headers` 原样带入；`Onlyne-Actor` 由解析层提供。正文里的 `idempotency_key` 同时放进 `Idempotency-Key` 头，服务端会核对两者一致。
- **发送前检查**：启用签名时，目标必须处于 `trusted` 阶段。查不到信任记录不发送，返回可重试的 `unavailable`，让出站记录保持打开，等用户完成交换。不得降级成明文或未签名请求。
- **请求签名**：签名覆盖 `onlyne-request-v1` 的规范序列化：actor、target、method、path 与 query、正文摘要、时间戳、nonce、content-type、幂等键、`x-onlyne-retry-until`、`last-event-id`。头部形状为 `Onlyne-Sig: v1 <actor> <timestamp> <nonce> <signature>`。
- **接收校验**：服务端解析同一负载并验签，要求 actor 与自己记录的可信对端一致、时间戳在 5 分钟窗口内。验签通过后，在事务里再次确认对端仍为 `trusted` 并消费 nonce；重放返回 `unauthenticated`。
- **响应验签**：带签名请求的成功响应带 `Onlyne-Response-Sig`，覆盖 `onlyne-response-v1` 的 target、请求 nonce、状态码与正文摘要。缺头或验不过即视为不可信。
- **信任建立的例外**：`GET /keys` 与 `POST /introductions` 在信任建立之前调用，本身不能带签名。它们只提供候选身份，真正生效的是用户对指纹的确认，以及随后双方各自记录的批准位。

信任交换的三段式由 `TrustService` 提供：`begin` 读取对端实时 `/keys` 并返回已证明的指纹与提交函数；`confirm` 记录本地批准、写入送达义务并发送介绍信；对端针对本次投递返回签名陈述时清除义务，收到对端自己发起的入站陈述时只清临时尝试标记；两种情况都在同一事务里写信任行并发 `trust` 通知。`inspect` 与 `entry` 是只读查询。

## 7. 诊断接口

库对外只有两个观察通道，宿主把它们映射到自己的日志与界面：

| 通道 | 内容 | 使用者 |
|---|---|---|
| `Runtime.trace` | 入站接纳、重复、确认、注入结果、消费确认、出站接纳/重驱/过期、等待解除 | 离线排查一次投递从接受到消费的全过程 |
| `Runtime.notify` | 信任转移、新入站条目、`work-pending` 变化 | 界面重绘；宿主不需要自己轮询存储 |

两条通道的字段都是标识、状态名或计数，不含正文与凭据。`trace` 抛错不影响投递；`notify` 应快速返回，重活放到监听者自己的下一轮。

## 8. 绑定选型

线上绑定必须提供什么由 RFC 0007 规定；本节记录参考实现选哪套库、用它的哪一部分。

| 绑定 | 选择 | 理由 |
|---|---|---|
| Onlyne HTTP/JSON | 自己写 | 对象模型是 thread、成员、mentions 和窄回复许可，上游 SDK 的数据模型装不下 |
| A2A | `@a2a-js/sdk` | 官方 TypeScript SDK，v1.0 稳定；客户端三种传输、AgentCard 拉取、JWS 验签、数据库 store 都现成 |
| ANP | 自己写外层，借 sidecar 做 DID 和签名 | `@anp/typescript-sdk` 是 0.1.0 本地预览，未发布，不能作为依赖 |

用 A2A SDK 的部分限于传输和卡片：`ClientFactory.createFromUrl` 取卡，`JsonRpcTransportFactory` 或 `RestTransportFactory` 发消息，`verifyAgentCardSignature` 验签，`DatabaseTaskStore` 存对端任务报告。Onlyne 的语义映射自己实现，SDK 的 Task 状态机不进入 Onlyne 的记录。

自写 ANP 外层的成本可控：P1 的限制很死，`params` 必须是对象、禁止 batch、请求 `id` 必须是非空字符串、成功响应只含 `result`。DID 文档解析和 RFC 9421 签名可以先由 Python 或 Rust 的 `anp` 包做本地 sidecar，等 TypeScript SDK 发布再替换。
