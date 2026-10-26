# OMP 宿主契约

状态：Draft（规范性）。MUST、MUST NOT、SHOULD、MAY 按 RFC 2119 使用；Draft 中的这些词约束拟议实现，不表示已有实现通过验收。

适用对象：OMP 扩展接口上的 Onlyne 接入实现。OMP 是首个参考宿主，本契约的条款对任何声称接入 OMP 的实现同样有效。

追溯：RFC 0008 §1–§7 定义模型交互界面的组成，本契约给出其规范性要求；RFC 0005 §5 定义模型可见列表需要证明的能力，RFC 0007 §5 定义运行时绑定必须提供的能力，两者在 OMP 上的具体义务由本契约承接。库与宿主共通的接口、时钟、生命周期与恢复义务见 [lib 宿主契约](lib-host.md)。

本文是 OMP 宿主要求的唯一详细来源。RFC 0008 只说明界面由哪些表面组成、每部分的权威文档在哪；宿主 API 的接口事实、安装版本与已观察限制在 [OMP 扩展接口调查](../references/omp-extension-api.md)；模块划分、存储布局、界面视觉与 Tern 特化在 [OMP 接入设计](../design/omp/)；某次实验得到什么在 `proof/` 的对应报告。

## 1. 范围、术语与权威

### 1.1 约束对象

本契约约束接入层向模型暴露什么、向 Onlyne 库提交什么，以及宿主状态变化时它怎么做。它不规定库的内部实现，也不把某台机器、某个安装版本的观察结果写成要求。

- OMP-1.1 接入 MUST 只执行 SystemOne 的判定与库的机械校验。它 MUST NOT 自行决定语义落点、投递方式或处理动作。
- OMP-1.2 本契约的每条 MUST 是接入的符合性条件。未实现的能力 MUST 显式拒绝，MUST NOT 静默降级；拒绝使用 RFC 0001 的错误码，缺失的宿主能力写在 `details`。

术语：

| 术语 | 含义 |
|---|---|
| 模型上下文 | 一次对话中模型可见的历史与工具结果 |
| 执行上下文（context） | 接入为模型上下文分配的身份；可以是主会话，也可以是任务派生的子代理会话。别名（如 `c-1`）只在目标 NodeId 的命名空间内唯一 |
| 消息条目 | 本地 thread 中的一条入站或出站记录，标识为 `(ThreadRef, seq)` |
| 注入游标 | 某执行上下文已把哪些消息条目注入过模型上下文，键为 `(context, ThreadRef, seq)` |
| 消费记录 | 某执行上下文已确认消费的消息条目，与注入游标共用同一条记录 |
| 目录 | 当前节点可联系的目标、能力与资源别名集合 |
| 判定点 | SystemOne 的一次独立判断：出站投递参数、入站处理动作、补充消息落点 |

### 1.2 权威关系

| 内容 | 权威位置 |
|---|---|
| 线上协议事实：身份、消息、接纳、thread、资源、恢复 | `docs/rfcs/` |
| 库与宿主共通的接口、时钟、维护环、恢复义务 | [lib 宿主契约](lib-host.md) |
| OMP 宿主必须提供什么 | 本文件 |
| 模块、存储、调度、界面实现 | `docs/design/omp/`、`docs/design/lib/` |
| 宿主 API 事实、版本与限制 | [OMP 扩展接口调查](../references/omp-extension-api.md) |
| 某次实验的结果 | `proof/` 对应报告 |

- OMP-1.3 接入实现或宿主接口变化 MUST NOT 改写协议语义。本文件与 RFC 冲突时，协议事实以 RFC 为准、宿主义务以本文件为准，冲突 MUST 先登记修订，不得按实现现状自行取舍。

## 2. 分工

| 角色 | 拥有 |
|---|---|
| Onlyne 库 | 网络收发、去重、认证接线、thread 与消息事实、资源操作、消费记录读写接口 |
| SystemOne（宿主 judge 角色） | 出站投递参数、入站处理动作、补充消息落点、派发方式 |
| 接入层 | 工具挂载、内部路径、来信注入、上下文登记、目录别名、todo 关联、恢复重放 |
| 宿主 todo 提供者 | 待办内容、完成与阻塞状态、提醒 |
| 宿主子代理机制 | 独立执行上下文的创建与驱动 |

- OMP-2.1 判定 MUST 通过宿主的 judge 模型角色执行，复用该角色的凭据、路由、概率与用量信息。
- OMP-2.2 每个判定点的输入 MUST 分别构造，材料注明来源与时效。完整输入与情景评测见 RFC 0016。
- OMP-2.3 judge 不提供所需信息时，接入 MUST 记录 unavailable 并保持原引用等待显式判定，MUST NOT 用配置或启发式补默认值。
- OMP-2.4 任务派生方式属于判定结果：配置 MUST NOT 提供 `omp.spawn`。补充消息的等待与重新判定时机 MUST 结合宿主实际状态、judge 返回的信息和任务上下文动态确定，配置 MUST NOT 提供固定 `supplement_window`；接收时间差本身 MUST NOT 决定消息属于哪个任务。
- OMP-2.5 接入 MUST NOT 按同一 thread、根引用或关键词先选好语义落点再把结果报给模型。引用匹配只作证据。

## 3. 能力矩阵与拒绝规则

| 需求 | 库能力 | 接入 MUST 提供 | 可观察结果 |
|---|---|---|---|
| OMP-3.1 | 模型消息入口 | 一个工具或扩展入口调用 `msg` | 模型没有第二套发信、读信或 thread 管理工具 |
| OMP-3.2 | 消息列表可见 | 条目按 §6 的索引渲染进模型上下文或工具结果 | 模型能引用 `(thread, seq)` 并读全文 |
| OMP-3.3 | 等待及中断 | 工具调用停在库的 Promise 上 | 合法 reply 解除等待，用户中止只取消本地等待 |
| OMP-3.4 | 入站判定 | SystemOne 决定动作，接入按动作执行 | 忙时排到宿主边界，plan 模式有明确队列状态 |
| OMP-3.5 | 独立执行 | 新执行上下文有独立身份、权限、游标与 todo | 子代理会话能回复且不影响主会话的消费记录 |
| OMP-3.6 | 补充说明 | 同一根消息的后续内容进入它已有的执行上下文 | 上下文可用时不产生第二个执行上下文 |
| OMP-3.7 | 恢复 | 按持久记录重建未决消息、上下文映射、注入游标与 todo 关联 | 重启后不重复建上下文，不重复建待办 |
| OMP-3.8 | todo 写入 | 来信可写进宿主 todo，快照与消息关联同一次可恢复提交 | 完成与提醒由宿主负责，恢复不重复建项 |

- OMP-3.9 接入 MUST 能让工具调用保持在库的 Promise 上，才接受带 `await` 意图的发送。它做不到时 MUST 在发送前拒绝，返回 `unsupported_delivery` 并在 `details` 指出缺失的宿主能力；MUST NOT 先发送再返回一个 handle，因为那改变副作用和等待语义。
- OMP-3.10 handle 模式是单独协商的能力，MUST NOT 与 `await` 共用同一个能力名，MUST NOT 作为 `await` 的降级实现；提供时 MUST 同时给出查询结果与恢复上下文的操作。
- OMP-3.11 接入 MUST NOT 把库的等待改成“模型结束回合等通知”，MUST NOT 用宿主的进程内消息总线替代业务答复等待。
- OMP-3.12 接入限制 MUST NOT 改写库语义。库已判定为拒绝的请求，接入 MUST NOT 改写成成功，也 MUST NOT 换一个动作重试以绕过判定。

## 4. 引导与配置

- OMP-4.1 首次加载 MUST 无需预写配置文件。未配置时 `/onlyne` 打开引导；`/onlyne init` 与 `/onlyne menu` 打开同一界面。Tern 与普通终端 MUST 提供相同的引导能力，Tern 只额外提供当前 session 名作为 region 建议值。取消引导 MUST NOT 写配置，MUST NOT 启动节点。
- OMP-4.2 引导 MUST 收集并生成下列项目，没默认值的项目 MUST 留空由用户填写：

| 项目 | 引导行为 | 默认 |
|---|---|---|
| region | 必填；有 Tern 时提供当前 session 名作为建议值，用户确认后提交协议成员关系；普通终端可完整填写 | 无 |
| node.id | 必填，初始为空，由用户填写稳定唯一身份 | 无 |
| server.bind | 必填 | `127.0.0.1` |
| node.key_scope | 密钥创建时选择：`project`、`global` 或 `none`（仅回环免密绑定） | `global` |
| threads.page_size | 读取分页大小，范围 1～100 | 50 |

- OMP-4.3 region 接受绝对 URI。用户输入短名时，界面 MUST 展示将保存的 `urn:onlyne:region:<经 URI 编码的短名>` 并请确认。Tern 改名或工作目录变化 MUST NOT 自动修改已保存的协议归属。
- OMP-4.4 密钥位置的规则：`project` 为 `.onlyne/keys`，`global` 为 `~/.config/onlyne/keys`。选 `project` 时该目录 MUST 已被 git 忽略，否则拒绝创建并说明修复方式。目录权限 MUST 为 0700、私钥 MUST 为 0600，不依赖 umask；加载时私钥组可读或全局可读 MUST 拒绝使用。选中位置拒绝创建时启动 MUST 失败，MUST NOT 静默改用另一位置。
- OMP-4.5 监听端口 MUST 由操作系统在本次启动分配；endpoint MUST 由实际监听结果生成并发布。配置 MUST NOT 包含 `endpoint`、`port`、`peers`、`default_attention`、`omp.spawn`、`supplement_window`。
- OMP-4.6 配置写入项目 `.onlyne/config.toml`。对端地址、公钥与核对记录写入项目 SQLite；`peers` 不作为配置文件条目维护。
- OMP-4.7 对端 MUST 经网络自动发现。引导 MUST 允许用户提供一个可达节点地址并立即核对，后续候选经 HTTP 取得；多播只是可选加速器。
- OMP-4.8 region 成员关系 MUST 由本节点自己提交；一个节点同一时间只属于一个域。界面 MUST NOT 把 region 当成密钥、凭据或连接许可。
- OMP-4.9 保存配置、启动监听、提交 region 成员关系、发布发现 MUST 分别可失败。界面 MUST 显示实际完成到哪一步；重开引导 MUST 按已保存事实继续。MUST NOT 把配置写入成功报成集群接入成功。

## 5. 模型入口

- OMP-5.1 工具名 MUST 为 `msg`，调用形状 MUST 固定为 `msg({ delivery_kind, msg, meta })`。`delivery_kind` 是自然语言投递意图，`msg` 是消息正文或本地查询要求，`meta` 只放明确引用，全部字段使用 snake_case。
- OMP-5.2 模型 MUST NOT 需要提供消息 ID、幂等键、版本或网络期限；这些由库生成。
- OMP-5.3 thread 的创建、关闭、列举、描述与成员变更 MUST 经同一入口表达，模型只给意图和本地别名；ThreadRef、成员状态、seq 与授权细节由库保存和校验。成员变更只影响本地成员表。资源操作同样经这个入口。

`meta` 字段：

| 字段 | 模型填什么 | 接入解析为什么 |
|---|---|---|
| `to` | 当前目录里的目标别名 | 已认证 endpoint 的稳定身份 |
| `in_reply_to` | 展示的 `{thread, seq}` | 原 MessageRef、认证来源与本地落点 |
| `thread` | 需读取或指定的 thread 别名 | 本地或已授权远端 ThreadRef |
| `context` | 目标已有执行上下文的别名 | 接收方的一个执行上下文，可以是主会话或某条根消息派生的子代理会话 |
| `resource` | 当前可访问的资源别名 | ResourceRef |
| `mentions` | `["@all"]` 或 `["@reviewer"]` | 这条消息希望谁主动关注 |
| `attention` | `inbox` 或 `thread` | 读取模式，默认 `inbox` |
| `op` | `create`、`close`、`list`、`describe`、`member_add`、`member_remove` | 对本地 thread 的管理动作 |
| `relation` | `reply` 或 `followup` | 消息关系 |
| `title` | 新 thread 的标题 | 本地 thread 标题 |
| `data` | 操作要求的结构化参数 | 资源声明的参数 schema |
| `cursor` | 上一页返回的不透明句柄 | 本地分页位置 |
| `page_size` | 1～100 | 本次读取上限 |

- OMP-5.4 下列意图与动作的映射是符合性样例，接入 MUST 至少覆盖这些意图；模型不需要知道具体动作名：

| 意图示例 | meta | 实际动作 |
|---|---|---|
| “列出我可联系的目标” | 省略 | 返回当前授权的目标、能力与资源别名 |
| “看看 thread 里最近怎么了” | `thread: research, attention: "thread"` | 返回该 thread 完整双向历史 |
| “这个目标有哪些上下文” | `to: reviewer` | 返回 context 别名与能力 |
| “我有哪些对话” | `op: "list"` | 返回本节点 thread、成员和最近活动 |
| “这个对话现在谁在里面” | `thread: research, op: "describe"` | 返回该 thread 的成员与访问范围 |
| “开一个只跟 reviewer 用的对话” | `op: "create", title: "..."` | 创建本地 thread 并返回新别名 |
| “把 reviewer 加进这个对话” | `thread: research, op: "member_add", to: reviewer` | 变更本地成员表 |
| “读 research 中 103 的全文” | `thread: research` | 按展示引用读取原条目 |
| “从 103 之后读新消息” | `thread: research` | 有界分页，返回 next_cursor |
| “查这条发送是否已被接纳” | `in_reply_to: {thread, seq}` | 查询出站证据，保持未知结果 |
| “把这个群交给 reviewer” | `resource: sea-ice-chat, to: reviewer` | 提出资源交接，等待资源 owner 提交 |
| “接受这次接手邀请” | `in_reply_to: {thread, seq}` | 向发出邀请的资源 endpoint 回复 |

- OMP-5.5 别名 MUST 由接入分配，在当前上下文内唯一且不重新绑定。显示名重名时 MUST 显示不同别名。过期或歧义引用返回 `not_found` 或 `ambiguous_target`，由模型补充明确引用后重试。
- OMP-5.6 回复 MUST 用 `in_reply_to` 的 `{thread, seq}` 表达。引用确定回复去向；同时给 `to` 且与原来源不一致时 MUST 返回 `invalid_request`。引用不授予历史读取权或资源控制权。
- OMP-5.7 `relation` 省略时由 SystemOne 按 `delivery_kind` 判定：意图是进度或补充用 `followup`，意图明确是答复用 `reply`，两者都判不出时 MUST 返回 `invalid_request` 并要求模型写明，MUST NOT 按线上默认值猜成 `reply`。关系决定是否解除对方等待，工具结果 MUST 回报实际使用的关系。
- OMP-5.8 `meta.context` 指向目标已有的执行上下文。`execution: existing` MUST 有明确的接收上下文：SystemOne 结合用户与模型历史、派发记录、context note、todo 和 thread 对话选择落点，多个候选可由这些证据消歧，证据不足时 MUST 返回 `ambiguous_target`。`in_reply_to` 是证据之一，接入 MUST NOT 把引用匹配当成全部语义判断。
- OMP-5.9 目录与历史读取 MUST 经同一入口。本地查询 MUST NOT 产生发给对端的业务消息；工具结果 MUST 显示实际计划、失败原因与已发生的事实。
- OMP-5.10 目录结果 MUST 给出每个资源声明的操作名与参数 schema 引用。接入 MUST 只接受 schema 允许的操作和字段，参数不合法时在本地返回 `invalid_request`，MUST NOT 发到资源 endpoint 后再解释失败。
- OMP-5.11 工具 schema 暴露的每个可填字段 MUST 对应一条 SystemOne 判据，或一条确定的机械解析路径。没有判据、只能靠模型猜的字段 MUST NOT 出现在 schema 里。字段语义与判据不一致时 MUST 修 schema 或判据，MUST NOT 用提示词补丁掩盖。
- OMP-5.12 分页用不透明 cursor；每页最多 100 条，默认 50，页面大小由 `meta.page_size` 指定。读取权限 MUST 在每次读取时检查。
- OMP-5.13 交接结果为 `pending` 时 MUST 显示“交接待决”；只有资源 owner 返回 `committed` 和新绑定，工具才显示“交接已生效”。平台凭据 MUST 留在资源 endpoint。
- OMP-5.14 投递时命名的 thread MUST 解析为目标节点拥有的 ThreadRef。模型命名本节点拥有的线程时，接入 MUST 返回 `invalid_request` 并说明落点由接收方按原消息所在线程决定，MUST NOT 静默改投默认位置或首触线程。回复省略 `thread` 时，落点 MUST 是保存原消息的线程。
- OMP-5.15 发送回执给出的读回地址 MUST 在当前节点可读，并显示落点、投递状态与关联引用。落点线程属于对端时，读取 MUST 报出该 owner 与可执行的读路径，MUST NOT 把本地 owner 守卫的拒绝原样交给读者。
- OMP-5.16 多个收件人 MUST 表达为 `meta.to` 的数组。接入 MUST 逐个目标各发一次，MUST 为每个目标返回独立的接纳结果，MUST NOT 让一个目标的失败掩盖其他目标；集合意图 MUST NOT 合并成一条消息或一个回执。`meta.to` 为单值时行为不变。
- OMP-5.17 只读子会话的回复交回 MUST 有结构化形状：`meta.op: "draft"` 携带 `to`、`relation`、`in_reply_to` 与正文，返回 `draft_id`；主会话用 `meta.draft_id` 发送同一条已准备的消息。草稿 MUST 绑定创建它的上下文与它被授权的原消息；子会话 MUST NOT 直接发送，MUST NOT 为不属于自己切片的消息建草稿。
- OMP-5.18 `meta.context` 与回执中的 `plan.context` 指接收方的一个执行上下文；它决定这次投递由哪个上下文消费，MUST NOT 被解释为消息落点的 thread。执行上下文丢失或不再绑定时，接入 MUST 返回 `context_lost`，MUST NOT 静默改投其他上下文。

## 6. 消息列表与渲染

- OMP-6.1 一个 thread 对应一个模型可见消息列表。thread 是列表的身份和容器，列表是 thread 按 seq 排列的条目，双向往来都在同一个列表里。同一模型上下文可以同时看到多个 thread 的列表。
- OMP-6.2 列表 MUST 支持两种读取模式：关注列表（默认，含 @ 自己、@ all 以及回复自己的条目）与完整 thread（该 thread 的全部双向历史）。
- OMP-6.3 列表 MUST 按三层组织，每层都能用上面的两种模式取：

| 层 | 关注模式 | 完整模式 | 模型操作 |
|---|---|---|---|
| thread | 有待处理关注条目的 thread | 本地可读的全部 thread | `op: "list"` 取全部，`op: "describe"` 取一个 |
| 消息 | 该 thread 内需关注的条目 | 按 seq 排列的全部条目 | `thread: <别名>` 取列表，`attention` 切换模式，`cursor` 翻页 |
| 单条 | 需关注条目的全文 | 任何一条消息的全文 | `thread` 加 `seq` 取全文，或按索引回复 |

- OMP-6.4 默认进入上下文的片段 MUST 是「有未处理关注条目的列表」：按 thread 分组，每个 thread 一条汇总行，下面是需要关注的未注入条目。未被 @ 的普通通信 MUST NOT 进入默认片段，模型用 `thread: <别名>` 主动取完整历史。
- OMP-6.5 注入游标 MUST 只推进被注入的关注条目。模型主动取过完整历史 MUST NOT 改变默认片段的内容，MUST NOT 推进注入游标；关注条目未注入前仍会再次出现。
- OMP-6.6 注入游标 MUST 按 `(context, thread, seq)` 记录。恢复同一上下文时沿用它的游标；创建独立上下文时，该上下文从自己的消费记录开始。
- OMP-6.7 一个 thread 里需关注的条目全部进入上下文后，该 thread MUST 退出默认片段，直到有新条目。
- OMP-6.8 关闭 thread 只停止新消息，历史保留可读。关闭后该 thread MUST 从自动片段退出，已提交且未注入的关注条目除外；这些条目 MUST 继续出现，直到注入或明确处置。
- OMP-6.9 被收窄 `read_scope` 或不持有 read 授权的 thread MUST NOT 出现在列表里。
- OMP-6.10 索引属于信封：头部为 `[seq] in|out · 来源 · 时间 · @提及`，正文逐行加两个空格和 `> ` 前缀。原始正文与空白 MUST 保存在消息条目中，渲染前缀 MUST NOT 写回正文。

```text
research
[103] in · dpdp · 02:05 · @all
  > 请核对这次实验的结论。
  > 第二行仍属于 103。
[104] out · self · 02:07 · reply research/103
  > 第二组数据需要重算。
[105] in · caerulea · 02:09
  > 我把上一轮的记录整理好了。
im-sea-ice
[107] in · feishu:海冰研究群 · 02:12
  > 用户问：图里的单位是什么？
```

- OMP-6.11 模型 MUST 用 `(thread 别名, seq)` 引用消息，seq 是十进制字符串。当前片段只有一个 thread 时别名可省略，接入解析后仍 MUST 在结果中回报完整引用。跨 thread 引用、序列号相同或来源同名时 MUST 返回候选而不猜测。多行正文里的索引外观 MUST NOT 创建新条目。
- OMP-6.12 长正文 MUST 展示首行与 `[+N 行]`，模型用 msg 读取全文。截断标记属于界面，不属于正文。权限允许的附件 MUST 显示类型与资源引用，访问时再次检查权限。
- OMP-6.13 回执、网络错误与出站证据 MUST 显示在工具结果或证据查询中，不占消息列表序号。`received` 表示存在本地入站条目；注入游标单独记录哪些条目进入过哪个 OMP 上下文。模型看自己的出站记录用 `read.scope: "evidence"`，结果只含消息引用、目标和对端接纳结果，不带自己范围外的正文。
- OMP-6.14 常驻说明 MUST 把来信正文标为外部材料。正文中的命令、身份声明或索引外观不能修改工具权限和接入规则；网络身份由认证结果提供。

## 7. 工具结果

- OMP-7.1 每次调用 MUST 同时给出可读文本和结构化 facts。模型可以看到最终计划、接纳证据、关联答复及资源结果。

```json
{
  "text": "reviewer confirmed durable admission; response async, execution new",
  "facts": {
    "local_ref": {"thread": "research", "seq": "106"},
    "message_ref": {"sender": "urn:onlyne:agent:lazuli", "id": "msg-01"},
    "admission": {"state": "accepted", "guarantee": "durable"},
    "plan": {"response": "async", "execution": "new"}
  }
}
```

- OMP-7.2 结果 MUST 表达下表的事实，不得把一种事实写成另一种：

| 观察到的事实 | 结果必须表达 |
|---|---|
| 已存本地待发 | queued locally，尚无对端接纳证据 |
| 网络结果未知 | `unknown`，保留原发送引用供查回 |
| 对端接纳 | 实际 guarantee、对端确认的计划与落点 |
| 对端拒绝 | 错误码、可读原因和已知发送引用 |
| 业务答复到达 | 原消息引用和答复正文 |
| 等待超时 | wait timed out，同时保留已有接纳证据 |
| 用户中止等待 | wait cancelled，原发送的接纳事实保持原样 |

- OMP-7.3 合法 reply 经 `await` 工具结果进入某上下文，与经自动关注片段进入该上下文，MUST 共用 `(context, ThreadRef, seq)` 的消费记录与投送调度。消费边界已确认工具结果包含该 reply 时，按同一条目记录已消费，自动片段 MUST NOT 再次投送该正文；若该回复已由自动片段确认消费，后续解除等待的工具结果 MUST 只返回原消息引用与到达事实，不重复展开正文。
- OMP-7.4 接入 MUST NOT 因 reply 已持久接收、waiter 已解除、工具结果已提交或宿主 API 调用成功就推进消费记录。消费确认前发生崩溃或未知时 MUST 保留未确认状态，重放时按未确认重投，MUST NOT 宣称恰好一次。
- OMP-7.5 回执和 `followup` 只保留为事实，等待继续。迟到回复进入历史，是否再唤醒由运行时策略决定。
- OMP-7.6 结果缺少 `wait` 字段，与 `plan.response` 为 `async` 或 `none` 是同一事实：这次调用没有等待答复。`wait` 只在 `plan.response` 为 `await` 时出现，取 `answered`、`timed_out` 或 `cancelled`。
- OMP-7.7 一次往返 MUST 能从一个入口读完：`onlyne://conversations` 列出会话（最近一条摘要、方向与时间），`onlyne://conversations/<sender>/<id>` 按回复引用聚合两个方向的全部消息，`onlyne://messages/<sender>/<id>` 给出单条。每条消息 MUST 带 `direction`、时间、投递状态，入站条目另带消费状态；MUST NOT 用对端 owner 的地址冒充本地可读地址。
- OMP-7.8 回执 MUST 说明这次投递是新建落点还是续写既有落点：`thread_created` 与 `previous_seq` 由接纳方在同一事务里写入，MUST NOT 由接入推断。

## 8. 持续工作与 todo

- OMP-8.1 来信是否转成待办、补充已有工作还是仅记录信息，由 SystemOne 判定。接入 MUST NOT 对每条来信机械追加。
- OMP-8.2 待办条目 MUST 包含可读摘要、来源、消息引用和本地上下文关联。已有清单 MUST 保留原目标和条目；补充可以关联已有工作，新任务才追加。
- OMP-8.3 快照与 MessageRef→待办项的稳定关联 MUST 由同一条可恢复的持久提交记录确定。宿主投影与本地索引在该记录成功后派生。恢复 MUST 先重放该提交记录并重建关联，再决定是否为来信建立待办；MUST NOT 仅因派生的本地关联索引缺失而再次追加。只有该提交记录成功后，接入才可向队列确认关联已建立。
- OMP-8.4 写入路径 MUST 与具体 todo 提供者无关：宿主装有 `omp-todo-more` 时写入它，没有时写入宿主原生 todo 与 goal。接入 MUST NOT 把某个提供者的专有字段当作关联键，因为提供者的 schema 会丢弃未知字段。
- OMP-8.5 来源分两处存放，MUST NOT 合并成一处：

| 内容 | 存放位置 | 作用 |
|---|---|---|
| 人类可读来源 | 条目 explanation 的首段 | 模型看见这条待办来自哪条来信 |
| 机器可解析来源 | Onlyne 本地记录，键是消息引用 | 重启恢复和去重建项的依据 |

  explanation 首段格式固定，模型编辑其余部分不影响解析：

```text
Source: research/103 from dpdp

核对第二组数据并回复 dpdp。
```

- OMP-8.6 本地记录 MUST 保存消息引用、thread 引用、todo 标题和写入时间。标题只作展示和回查，MUST NOT 作关联键：删除后重建同名条目时，旧来源不自动转移到新条目。本地记录缺失时，接入 MUST 向来信队列报告未建立关联，MUST NOT 按标题猜测。
- OMP-8.7 模型通过宿主 todo 操作完成、阻塞或继续工作。回复消息只产生回复关系，接入 MUST NOT 仅凭已经回复就自动完成待办。交接 MUST NOT 迁移发送方的 todo。
- OMP-8.8 可恢复提交记录写入失败时，来信 MUST 保留在本地队列，界面 MUST 说明未建立待办，MUST NOT 报告安排完成。
- OMP-8.9 提醒由宿主 todo 提供者发出，Onlyne MUST NOT 触发提醒，MUST NOT 为消息生成 complete、pending 或 failed 等工作状态。
- OMP-8.10 todo 数据归宿主所有，Onlyne 核心不读写、不投影、不同步它；接入层只通过宿主 API 建立本地关联。宿主描述能力有限时，摘要 MAY 只含标题。

## 9. 上下文、注入与中止

- OMP-9.1 接入 MUST 显式维护 `(node_id, OMP session_id, context, ThreadRef)` 映射。一个 OMP 会话可以读取多个 thread。新执行上下文 MUST 获得自己的映射、权限、列表游标和 todo。
- OMP-9.2 任务派生的子代理是独立执行上下文。接入 MUST 把上下文别名与子代理 id 记入目录，并在模型看到子代理结果之前完成登记，供 SystemOne 判定补充说明落点。
- OMP-9.3 每条来信由 SystemOne 判定处理动作，接入 MUST 按动作执行：

| 判定 | 接入行为 |
|---|---|
| `fragment` | 以 `onlyne.inbound` 自定义消息进入下一个上下文装配点，不主动开启回合 |
| `context` | 投递到目录中的上下文别名，主会话不启动回合，thread 视图标注落点 |
| `wake`（`insertion: steer`） | 以 `onlyne.inbound` 注入，当前工具调用结束后立即插话；空闲会话没有可插话的回合，改为启动回合 |
| `wake`（`insertion: follow_up`） | 忙时以 `followUp` 排队，本轮结束后追加并开启新回合；空闲时启动回合 |
| `wake`（未给插入模式） | 空闲时启动回合，忙时排到下一步边界 |
| `wait_result` | 由在途等待返回该回复 |

- OMP-9.4 宿主状态决定执行方式，不改变判定：流式生成期间注入 MUST 排在下一步边界；plan 模式 MUST 只保存；用户中止 MUST 通过 AbortSignal 释放当前本地等待；恢复会话 MUST 按持久映射与注入游标补齐，并重新检查权限。
- OMP-9.5 接入注入来信时 MUST 使用自定义消息类型，MUST NOT 调用 `sendUserMessage`。`onlyne.inbound` 承载来信，`onlyne.routing` 承载投递去向说明，两者 `attribution` 为 `agent`，在会话中记为 `custom_message`，在 TUI 中以独立样式显示。宿主把它们转换为 developer 角色的上下文，MUST NOT 冒充本地用户发言。
- OMP-9.6 `execution: new` 要求宿主能创建并驱动独立上下文。能力未接通时 MUST 返回 `unsupported_delivery`；MUST NOT 把消息发到任意现有会话代替新上下文。它是发送方的偏好，MUST NOT 覆盖接收方对来信是否需要回答的判断（OMP-9.11）。
- OMP-9.7 新执行上下文由接收方分配并启动。发送方 MUST NOT 把本地 `c-N` 别名当作远端的新上下文，MUST NOT 要求自己的模型替接收方创建子代理。既有 context 别名 MUST 在目标 NodeId 的命名空间内解析。发送方 `await` 的消费身份与接收方执行身份 MUST 分别记录。
- OMP-9.8 同一根消息的后续内容 MUST 进入该根消息已有的执行上下文；上下文存在且可用时 MUST NOT 创建第二个。判定指向已丢失的上下文时 MUST 返回 `context_lost`，MUST NOT 静默新建或改选；新事实可以触发下一次显式判定。
- OMP-9.9 宿主状态本身 MUST 作为判定输入上交：用户最近的停止或改派、当前工作目标、模型已经发出的任务、thread 历史和资源关系都可以改变应选动作。
- OMP-9.10 子会话 MUST 能读取自己的切片（`onlyne://threads`、`onlyne://inbox`、`onlyne://contexts`），写入 MUST 被拒绝。拒绝文案 MUST 说明这是写边界、给出读入口，并说明结果交回主会话发送，MUST NOT 让读者从“只能读”的措辞去猜设备是否可用。
- OMP-9.11 每条来信 MUST 经过 SystemOne 判定，`execution: new` 不构成豁免。`execution: new` 是发送方的偏好：来信需要回答时，接入 MUST 选有发信权的落点并记录改判；`plan.context` 是绑定，指向已丢失上下文时 MUST 返回 `context_lost`。需要回答的判断 MUST 由判定模型基于来信内容做出，MUST NOT 只靠关键词表决定。
- OMP-9.12 注入的来信 MUST 显示可核对的投递与消费状态。状态 MUST 来自本地记录（入站条目、消费记录、出站记录）；读不到时 MUST 显示 unknown，MUST NOT 用固定值冒充已消费或已送达。

## 10. 信息面

- OMP-10.1 用户可见的 Onlyne 状态 MUST 能经内部路径读取：thread、消息列表、队列、目录、发现记录、认证阶段、可达节点。模型与用户 MUST 看到同一份事实，密钥材料除外。
- OMP-10.2 对内有语义的输入输出 MUST 收敛到「内部路径 + 外部入口包装」：`msg` 是入口包装，界面的取数与模型的读取 MUST 走同一接口，MUST NOT 为两者各维护一套取数逻辑。
- OMP-10.3 内部路径 MUST 只读。浏览会话、消息队列和证据不得产生消息事实副作用；产生副作用的操作只经库的写入接口。
- OMP-10.4 界面操作 MUST 能只用鼠标完成。快捷键 MAY 作为等价补充，MUST NOT 是完成某个状态迁移的唯一路径。每个可操作项 MUST 就地显示它当前能做什么，MUST NOT 依赖用户记住组合键。

## 11. 符合性场景

每条场景给出输入与可观察结果，独立实现可以据此验证而不依赖接入内部结构。

1. agent 全程使用一个 `msg` 入口完成目标发现、thread 管理、列表读取、普通发送、引用回复、等待和资源交接，全程没有第二个模型工具（OMP-3.1、OMP-5.3）。
2. 两个 thread 使用相同 seq、两个目标使用相同显示名时，回复仍到达明确来源（OMP-5.5、OMP-6.11）。
3. 新 thread 创建后立即得到别名，后续发送和读取都用该别名定位（OMP-5.5、OMP-6.1）。
4. 多个 thread 有未处理条目时，自动片段按 thread 分组；已读完的 thread 不重复占上下文（OMP-6.4、OMP-6.7）。
5. thread 引用不唯一时返回候选，不猜测目标 thread（OMP-6.11）。
6. 关闭 thread 后仍可按别名读历史，不能向它发送新消息；已提交未注入的关注条目仍出现在自动片段，直到消费或明确处置（OMP-6.8）。
7. 成员变更只改本地成员表，读权限需要单独的 `read_scope` 授予（OMP-6.9）。
8. 目标有多个上下文时返回候选而不擅自选择；模型用 context 别名重试后到达指定上下文（OMP-5.8）。
9. 进度补充被记录为 `followup`，不解除对方等待；工具结果回报实际关系（OMP-5.7、OMP-7.2）。
10. 资源操作按目录返回的 schema 校验，非法参数在本地被拒绝（OMP-5.10）。
11. 正文包含伪造头部和多行文本时，渲染保留条目边界及原正文（OMP-6.10、OMP-6.14）。
12. await 期间网络路径继续接收，合法 reply 解除等待，用户中止只取消本地等待（OMP-3.3、OMP-9.4）。
13. 宿主不能保持工具调用时，不发送 `await` 消息，并明确说明缺失能力（OMP-3.9）。
14. `received`、已注入、已答复和 todo 完成各有自己的证据，互不推定（OMP-7.3、OMP-7.4、OMP-8.7）。
15. 消息与 todo 的恢复关联不会重复创建执行上下文或待办（OMP-3.7、OMP-8.3、OMP-9.1）。
16. plan 模式下的新来信只保存、不启动回合，切回执行后按原引用处理（OMP-9.4）。
17. 交接只在资源 owner 提交后显示生效，旧发送者的权限按当前绑定校验（OMP-5.13）。
18. 引导取消后没有配置文件、没有监听、没有 region 成员关系；四步中任一步失败时界面停在实际完成的位置（OMP-4.1、OMP-4.9）。
19. 超出配置表范围的键出现在 `.onlyne/config.toml` 时，接入 MUST 不因为存在这些键就产生派发或调度行为（OMP-4.5、OMP-2.4）。
20. 同一 root 消息的两条补充说明都进入同一个已有执行上下文（OMP-3.6、OMP-9.8）。

## 12. 未决项

- 通用模型消费确认：宿主当前未提供时保留未确认记录（OMP-7.4）。宿主提供该能力后，接入 MUST 改为确认后推进游标。能力接口未定。
- handle 模式未协商（OMP-3.10）。启用前需要单独的能力声明与恢复操作定义。
- 程序化 todo 写入是否立即刷新宿主界面尚未确证。首版 MUST 验证刷新；不刷新时改用会产生工具结果的写入路径，或明确显示待刷新状态。
- IM 会话是独立资源节点，不是 agent 的 thread；其接入细节由资源 profile 与设计文档规定，不在本契约范围。

## 13. 引用与证据

规范性引用：协议本体见 `docs/rfcs/` 的 RFC 0001～0007、0011、0016、0017；模型界面的范围与权威归属见 [RFC 0008](../rfcs/0008-omp-surface.md)；库与宿主共通义务见 [lib 宿主契约](lib-host.md)。

说明性引用与证据入口：[OMP 扩展接口调查](../references/omp-extension-api.md) 记录宿主 API 事实、安装版本和已观察到的限制；`proof/` 下的报告记录某次实验的结果。两者都不改写本契约。

追溯表：

| 本契约 | 来源 |
|---|---|
| §3 能力矩阵、OMP-3.9～3.12 | RFC 0007 §5；RFC 0005 §5 |
| §4 引导与配置、OMP-4.4 | RFC 0008 §1.1；RFC 0017 §2；RFC 0011 引导条目 |
| §5 模型入口 | RFC 0008 §2、§3；RFC 0005 §1 |
| §6 消息列表与渲染 | RFC 0008 §4；RFC 0004 |
| §7 工具结果与消费记录 | RFC 0008 §5；RFC 0003、RFC 0005 §3 |
| §8 持续工作与 todo | RFC 0008 §6；RFC 0005 §4 |
| §9 上下文、注入与中止 | RFC 0008 §7；RFC 0005 §1 |
| §10 信息面 | RFC 0004 视图；RFC 0008 §1 |
