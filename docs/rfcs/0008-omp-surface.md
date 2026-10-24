# RFC 0008：OMP 中的 agent 交互界面

状态：Draft。本文定义首版 OMP 接入的模型界面。库的消息、权限和等待规则沿用 [投递与运行时契约](0005-delivery-and-runtime.md)。

## 1. 界面组成

OMP 中的 agent 通过一个 `msg` 工具发信、回复、读取消息、管理自己的 thread 和操作资源。新来信以消息列表片段进入模型上下文。需要持续处理的来信进入该 OMP 会话自己的 todo，由运行时提醒。

| 表面 | 模型看到什么 | 谁持有数据 |
|---|---|---|
| 常驻说明 | 当前身份、工具用法、目标别名和引用规则 | OMP 接入 |
| `msg` 参数与结果 | 意图、正文、明确目标或消息引用、已发生的事实 | Onlyne 库 |
| 来信片段 | 本地索引、来源、关系和正文 | 本地 thread 与 OMP 注入游标 |
| todo | 持续工作、来源和需回复的来信引用 | OMP 运行时 |

工具名称为 `msg`，调用形状固定为 `msg({ delivery_kind, msg, meta })`。thread 的创建、关闭、列举、成员变更是模型的可见操作，模型只给出意图和本地别名；ThreadRef、成员状态、seq 和授权细节由库保存和校验。资源操作经同一入口表达。

### 1.1 初次引导与配置

首次加载无需预写配置。`/onlyne init` 和未配置时的 `/onlyne` 打开 bootstrap，收集并生成本节点配置：

| 项目 | 引导行为 |
|---|---|
| region | 必填；有 Tern 时提供当前 session 名作为建议值，用户确认后提交协议成员关系；普通终端可完整填写 |
| node.id | 必填，初始为空，由用户填写稳定唯一身份；region 独立保存 |
| server.bind | 必填，默认 127.0.0.1 |
| node.key_ref | 可选，首次回环 E2E 可空；后续认证方案独立提案 |
| threads.page_size | 默认 50，范围 1～100 |

region 输入接受绝对 URI；用户输入短名时，界面展示将保存的 `urn:onlyne:region:<经 URI 编码的短名>` 并请用户确认。Tern 改名或工作目录变化不自动修改已保存的协议归属。

监听端口由操作系统在本次启动分配，endpoint 由实际监听结果生成并发布。配置不包含 endpoint、port、peers、default_attention、omp.spawn 或 supplement_window。读取默认使用关注列表，完整 thread 由调用显式选择。任务派生和补充消息调度由 judge 决定。

配置由界面写入项目 `.onlyne/config.toml`。对端通过网络自动发现，地址与核对记录写入项目 SQLite；多播不可用时，引导可收集一个初始联系地址并立即核对，后续候选经 HTTP 取得。取消引导不写配置、不启动节点。Tern 与普通终端提供相同的 bootstrap 功能。

引导保存、启动监听、提交 region 成员关系、发布发现是分别可失败的步骤。界面必须显示实际完成位置；重开引导按已保存事实继续，不把配置写入成功称作集群接入成功。

## 2. 参数与目标

`delivery_kind` 是自然语言投递意图。`msg` 是消息正文或本地查询要求。`meta` 存放明确引用，所有字段使用 snake_case。

```json
{
  "delivery_kind": "独立查一下，查完告诉我，我先做别的",
  "msg": "请核对第二组数据。",
  "meta": {"to": "reviewer"}
}
```

| meta 字段 | 模型填什么 | 接入层解析为什么 |
|---|---|---|
| `to` | 当前目录里的目标别名 | 已认证 endpoint 的稳定身份 |
| `in_reply_to` | 展示的 `{thread, seq}` | 原 MessageRef、认证来源和本地落点 |
| `thread` | 需要读取或指定的 thread 别名 | 本地或已授权远端 ThreadRef |
| `context` | 目标已有执行上下文的别名 | 接收方的一个执行上下文，可以是主会话，或为某条根消息派生的子代理会话 |
| `resource` | 当前可访问的资源别名 | ResourceRef |
| `mentions` | `["@all"]` 或 `["@reviewer"]` | 这条消息希望谁主动关注 |
| `attention` | `inbox` 或 `thread` | 读取模式，默认 inbox |
| `op` | `create`、`close`、`list`、`describe`、`member_add`、`member_remove` | 对本地 thread 的管理动作 |
| `relation` | `reply` 或 `followup` | 消息关系 |
| `title` | 新 thread 的标题 | 本地 thread 标题 |
| `data` | 操作要求的结构化参数 | 资源声明的参数 schema |
| `cursor` | 上一页返回的不透明句柄 | 本地分页位置 |
| `page_size` | 1～100，默认 50 | 本次读取上限 |

别名由接入层分配，在当前上下文内唯一且不重新绑定。显示名重名时显示不同别名。过期或歧义引用返回 `not_found` 或 `ambiguous_target`，模型补充明确引用后再调用。

回复时使用来信索引：

```json
{
  "delivery_kind": "答复这条请求，结论如下",
  "msg": "第二组数据需要重算。",
  "meta": {"in_reply_to": {"thread": "research", "seq": "103"}}
}
```

引用确定回复去向。提供 to 且与原来源不一致时返回 `invalid_request`。引用不授予历史读取权或资源控制权。模型无需提供消息 ID、幂等键、版本或网络期限。

进度补充使用同一引用并写明 `relation: "followup"`：

```json
{
  "delivery_kind": "补充进度，不用当作最终答复",
  "msg": "第二组已重算，完整结论稍后给。",
  "meta": {"in_reply_to": {"thread": "research", "seq": "103"}, "relation": "followup"}
}
```

省略 `relation` 时，SystemOne 按 `delivery_kind` 判断。意图是进度或补充时使用 `followup`。意图明确是答复时使用 `reply`。两者都无法判断时，返回 `invalid_request` 并要求模型写明关系，不按线上默认值猜成 reply。关系决定是否解除对方的等待，因此工具结果必须回报实际使用的关系。

`execution: existing` 需要明确接收上下文。目录返回每个目标的 context 别名。SystemOne 结合用户与模型历史、派发记录、context note、todo 和 thread 对话选择落点。多个候选可以由这些证据消歧，证据不足时返回 `ambiguous_target`。`in_reply_to` 提供原消息证据，插件不能把引用匹配当成全部语义判断。

## 3. 发现、读取与资源操作

模型需要的目录和历史同样经 msg 查询。SystemOne 将本地查询映射到类型化读取，查询不产生发给对端的业务消息。查询和修改的实际计划都出现在工具结果中。

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

本地 cursor 用不透明句柄展示，模型通过同一 msg 入口请求下一页。每页最多 100 条，默认 50 条。读取权限在每次读取时检查。页面大小由调用方通过 `meta.page_size` 指定。

目录结果同时给出每个资源声明的操作名和参数 schema 引用。模型不需要猜资源支持什么；SystemOne 只接受 schema 允许的操作和字段。参数不合法时返回 `invalid_request`，不发送到资源 endpoint 再解释失败。

交接结果为 `pending` 时显示“交接待决”。只有资源 owner 返回 `committed` 和新绑定，工具才显示“交接已生效”。平台凭据留在资源 endpoint。

## 4. 消息列表

一个 thread 对应一个消息列表。thread 是列表的身份和容器，列表是 thread 按 seq 排列的条目，双向往来都在一起。同一模型上下文里可以同时看到多个 thread 的列表。

模型看到的列表有两种读取模式：

| 模式 | 内容 | 何时用 |
|---|---|---|
| 关注列表（默认） | @ 自己、@ all、以及回复自己消息的条目 | 及时处理需要关注的事 |
| 完整 thread | 该 thread 的全部双向历史 | 没事时看看 thread 里有什么进展 |

模型看到的列表按三层组织，每层都可以按上面两种模式取：

| 层 | 关注模式 | 完整模式 | 模型操作 |
|---|---|---|---|
| thread | 有待处理关注条目的 thread | 本地可读的全部 thread | `op: "list"` 取全部，`op: "describe"` 取一个 |
| 消息 | 该 thread 内需关注的条目 | 该 thread 内按 seq 排列的全部条目 | `thread: <别名>` 取列表，`attention` 切换模式，`cursor` 翻页 |
| 单条 | 需关注条目的全文 | 任何一条消息的全文 | `thread` 加 `seq` 取全文，或按来信索引回复 |

关注列表是默认片段。完整 thread 由模型主动查询，不自动进入上下文。

加入 thread 的成员默认看到完整双向往来。被收窄 read_scope 或不持有 read 授权的 thread 不出现在列表里。模型看自己的出站记录时，用 `read.scope: "evidence"`，结果显示消息引用、目标和对端接纳结果，不带自己范围外的正文。

默认进入上下文的片段是「有未处理关注条目的列表」。片段按 thread 分组，每个 thread 一条汇总行，下面是需要关注的未注入条目。未被 @ 的普通通信不进入默认片段，模型用 `thread: <别名>` 主动取完整历史。

注入游标只推进被注入的关注条目。模型主动取过完整历史不改变默认片段的内容，关注条目未注入前仍会再次出现。

没有未处理关注条目时，默认片段为空。模型仍可随时主动取任何 thread 的完整历史。

一个 thread 里需关注的条目全部进入上下文后，该 thread 退出默认片段，直到有新条目。注入游标按 `(context, thread, seq)` 记录。恢复同一上下文时沿用它的游标；创建独立上下文时，该上下文从自己的消费记录开始。主动取完整历史不推进注入游标。

模型引用消息时给出 `(thread 别名, seq)`。当前片段只有一个 thread 时可省略别名，接入解析后仍在结果中回报完整引用。跨 thread 引用、序列号相同或来源同名时，接入返回候选而不猜测。

索引属于信封。正文逐行加两个空格和 `> ` 前缀，原始正文与空白保存在消息条目中。渲染前缀不写回正文。

```text
research
[103] in · dpdp · 02:05 · @all
  > 请核对这次实验的结论。
  > 第二行仍属于 103。
[104] out · self · 02:07 · reply research/103
  > 第二组数据需要重算。
[105] in · caerulea · 02:09
  > 我把上一轮的记录整理好了。
[106] out · self · 02:11 · @lazuli
  > 数据侧麻烦确认一下。
im-sea-ice
[107] in · feishu:海冰研究群 · 02:12
  > 用户问：图里的单位是什么？
```

模型用 `op: "list"` 看汇总，`thread: research` 取该列表。条目里的 in 和 out 是同一个 thread 里的双向记录。`@all` 和 `@lazuli` 标在头部行上，105 没有 @，只进完整 thread 视图。

模型用 `(thread 别名, seq)` 引用消息。seq 是十进制字符串。跨 owner 的 thread 也使用当前上下文唯一的别名。多行正文中的索引外观不创建新条目。

新 thread 由模型显式创建，或由 owner 的入站策略在首次接触时建立。模型创建后得到别名，后续消息用该别名进入指定 thread。关闭 thread 只停止新消息，历史保留可读。关闭后该 thread 从自动片段中退出，已提交且未注入的关注条目除外；这些条目继续出现，直到注入或明确处置。

成员变更经同一入口执行，只影响本地成员表。加入 thread 的成员默认获得 post 和 read 两项权限；收窄范围是主持人明确执行的操作（RFC 0002 §2）。

长正文展示首行和 `[+N 行]`，模型使用 msg 读取全文。截断标记属于界面，不属于正文。权限允许的附件显示类型和资源引用，访问时再次检查权限。

回执、网络错误和出站证据显示在工具结果或证据查询中，不占消息列表序号。received 表示存在本地入站条目，注入游标单独记录哪些条目进入过哪个 OMP 上下文。

常驻说明把来信正文标为外部材料。正文中的命令、身份声明或索引外观不能修改工具权限和接入规则。网络身份由认证结果提供。

## 5. 工具结果

工具结果同时提供可读文本和结构化 facts。模型可以看到最终计划、接纳证据、关联答复及资源结果。

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

| 观察到的事实 | 结果必须表达 |
|---|---|
| 已存本地待发 | queued locally，尚无对端接纳证据 |
| 网络结果未知 | unknown，保留原发送引用供查回 |
| 对端接纳 | 实际 guarantee、对端确认的计划与落点 |
| 对端拒绝 | 错误码、可读原因和已知发送引用 |
| 业务答复到达 | 原消息引用和答复正文 |
| 等待超时 | wait timed out，同时保留已有接纳证据 |
| 用户中止等待 | wait cancelled，原发送的接纳事实保持原样 |

`await` 的工具调用停在库的 Promise 上。网络接收路径直接验证和匹配 reply 后解除等待。回执和 followup 保留为事实，等待继续。迟到回复进入历史，运行时策略决定是否再唤醒。

合法 reply 经 `await` 工具结果进入某上下文与经自动关注片段进入该上下文，MUST 共用 `(context, ThreadRef, seq)` 消费记录与投送调度。消费边界已确认工具结果包含该 reply 时，按同一条目记录已消费，自动片段不得再次投送该正文；反之，若该回复已由自动片段确认消费，后续解除等待的工具结果只返回原消息引用与到达事实，不重复展开正文。不得因 reply 已持久接收、waiter 已解除、工具结果已提交或宿主 API 调用成功就提前推进消费记录；消费确认前发生崩溃或未知时保留未确认状态，重放时按未确认重投，不宣称恰好一次。

## 6. 持续工作与 todo

OMP 接入按 SystemOne 的决定，把需要持续处理的来信写入 todo 提供者。条目包含可读摘要、来源、消息引用和本地上下文关联。已有清单保留原目标和条目，补充可以关联已有工作，新任务才追加。

已核对的 OMP 18.12.6 环境使用 `omp-todo-more@0.1.4`，原生 todo 已关闭。拟议接入用 `pi.appendEntry` 写入完整快照，再更新宿主投影。快照携带 goal，重放以快照为准。SystemOne 结合原清单判断新增工作、补充已有任务或仅记录信息，不能对每条来信机械追加。

持续来信的 todo 快照与 MessageRef 来源关联 MUST 由同一可恢复的持久提交记录确定；该记录同时包含新快照及消息引用到待办项的稳定关联。宿主投影与本地索引在该记录成功后派生。恢复 MUST 先重放该提交记录并重建关联，再决定是否为来信建立待办；不得仅因派生的本地关联索引缺失而再次追加。只有该提交记录成功后才向队列确认关联建立。

来源引用结构如下：

| 内容 | 存放位置 | 作用 |
|---|---|---|
| 人类可读来源 | 条目 explanation 的首段 | 模型看见这条待办来自哪条来信 |
| 机器可解析来源 | Onlyne 本地记录，键是消息引用 | 重启恢复和去重建项的依据 |

explanation 首段的格式固定，模型编辑其余部分不影响解析：

```text
Source: research/103 from dpdp

核对第二组数据并回复 dpdp。
```

机器可解析来源是稳定依据。Onlyne 本地记录保存消息引用、thread 引用、todo 标题和写入时间。标题仅作展示和回查，不作关联键：删除后重建同名条目时，旧来源不自动转移到新条目。本地记录缺失时，接入向来信队列报告未建立关联，不按标题猜测。

模型通过 OMP 的 todo 操作完成、阻塞或继续工作。回复消息只产生回复关系。接入层不得仅凭已经回复就自动完成 todo。交接也不迁移发送方的 todo。

可恢复提交记录写入失败时，来信保留在本地队列，界面说明未建立待办，不报告安排完成。重启恢复首先按提交记录重建消息引用与待办关联，避免重复建项。提醒由 todo-more 发出，Onlyne 不触发提醒。

模型改写 explanation 时，Onlyne 的本地来源记录保持原消息引用。界面需要回显来源时以本地记录为准，explanation 只作提示。

## 7. 上下文、注入和中止

接入显式维护 `(node_id, OMP session_id, context, ThreadRef)` 映射。一个 OMP 会话可以读取多个 thread。新执行上下文获得自己的映射、权限、列表游标和 todo。任务派生的子代理是独立执行上下文，接入把上下文别名与子代理 id 记入目录，供 SystemOne 判定补充说明的落点。

每条来信由 SystemOne 判定处理动作，接入按动作执行：

| SystemOne 判定 | 接入行为 |
|---|---|
| `fragment` | 以 `onlyne.inbound` 自定义消息进入下一个上下文装配点，不主动开启回合 |
| `context` | 投递到目录中的上下文别名，主会话不启动回合，线程视图标注落点 |
| `queue` | 持久保存并显示 queued，不启动回合 |
| `wake` | 空闲时启动回合，忙时排到下一步边界 |
| `wait_result` | 由在途等待返回该回复 |

宿主状态决定执行方式，不改变判定：流式生成期间注入排在下一步边界；plan 模式只保存；用户中止通过 AbortSignal 释放当前本地等待；恢复会话按持久映射与注入游标补齐，并重新检查权限。

宿主状态本身也进入 SystemOne 的判定输入。用户最近的停止或改派、当前工作目标、模型已经发出的任务、thread 历史和资源关系都可以改变应选动作。各处上下文分别构造，完整输入与情景模拟定义在 [RFC 0016](0016-systemone-context.md)。

接入注入来信时使用自定义消息类型，不调用 `sendUserMessage`。`onlyne.inbound` 承载来信，`onlyne.routing` 承载投递去向说明，两者 `attribution` 为 `agent`，在会话中记为 `custom_message`，在 TUI 中以独立样式显示。OMP 把它们转换为 developer 角色的上下文，不冒充本地用户发言。排队条目用 `appendEntry` 写入，不进入模型上下文。

`execution: new` 要求宿主能创建并驱动独立上下文。能力未接通时返回 unsupported_delivery。系统不得把消息发到任意现有会话代替新上下文。

新执行上下文由接收方分配并启动。发送方不能把本地 c-N 别名当作远端的新上下文，也不能要求自己的模型替接收方创建子代理。既有 context 别名在目标 NodeId 的命名空间内解析。发送方 await 工具结果的消费身份与接收方执行身份分别记录。

宿主无法让工具调用保持在库的 Promise 上时，接入 MUST 在发送前拒绝 `await`，并说明缺少的能力。它不得先发送再返回一个 handle，因为那会改变副作用和等待语义。handle 模式只能作为单独协商的能力出现。

## 8. OMP 接线证据

本节记录实际宿主 API 和限制，与拟议界面分别核对。接口证据来自已安装的 OMP 18.12.6 源码及当前 todo 提供者；安装包自带 `src/`，行号指向该版本。首版接入还需实跑验收场景才能宣称实现通过。

| 能力 | 实际接口 | 对接入的含义 |
|---|---|---|
| 注册工具 | `ExtensionAPI.registerTool`，`execute(toolCallId, params, signal, onUpdate, ctx)` | 工具可以持有库的 Promise 并响应中止信号 |
| 工具中断 | `ToolDefinition` 未声明 `interruptible`，运行时只机械转发自有属性 | 不依赖未声明的中断字段；用 `signal` 和库自己的取消路径 |
| 主动注入 | `sendMessage(customMessage, {deliverAs})` 注入自定义消息；`appendEntry` 只持久化本地条目 | 不调用 `sendUserMessage`；`sendMessage` 返回 `void`，调用结束不提供消费确认 |
| 注入调度 | `deliverAs` 取 `steer`、`nextTurn`、`aside`、`followUp`；忙闲由 `isStreaming` 与 plan 模式决定 | 来信排队与唤醒必须按这个矩阵设计 |
| 新上下文 | `createAgentSession` 支持 `agentId`、`taskDepth`、`enableIrc` | 新上下文需要自己的身份，不能复用父会话身份 |
| 进程内投递 | `AgentSession.deliverIrcMessage` 返回 `injected` 或 `woken` | IRC 总线另有 revived/failed，均不证明模型已读或业务完成 |
| 等待 | `IrcBus.wait(agentId, {from}, timeoutMs, signal)` | 可作为进程内唤醒通道，不替代 Onlyne 的业务答复等待 |
| 任务派发 | `task` 不复用子会话，重复 `name` 得到 `-2`；子代理 id 在 `details.progress[].id` 或 `details.results[].id` | 上下文登记在模型看到结果之前完成 |
| 补充投递 | `executeSend({registry, senderId}, {to, message})`，父级发送转为 steer | 追加内容进入原会话，不创建第二个子代理 |
| 子代理身份 | `ctx.agent` 提供 `kind`、`id`、`depth`、`parentId` | 主会话与子代理共用扩展代码，按 `kind` 分权 |
| 插件设置页 | `package.json` 的 `omp.settings`；读写 `~/.omp/plugins/omp-plugins.lock.json`；项目覆盖文件只读 | 页面写入用户级配置，项目级配置需由插件自建界面 |
| Tern 渲染 | `TERM_PROGRAM=tern` 探测加 TSP `hello`；组件实现 `describe(cx)` | 自定义面板与工具视图需要原生描述，未实现时回退逐行输出 |
| todo 写入 | `pi.appendEntry("mouriya.omp.todo-more", snapshot)` 加 `session.setTodoPhases` | 保留 goal 和 explanation；人类可读来源写进 explanation 首段 |
| todo 提醒 | `omp-todo-more` 的 stop、turn-end 与 plan-check 提醒 | 提醒归提供者；原生 todo 已关闭 |

调度细节（`session/irc-bridge.ts` 与 `session/agent-session.ts`）：

- 流式期间，来自父会话的消息走 steer，其他消息压入下一步边界的注入队列。两者都回报 `injected`。
- 非流式的 plan 模式只追加消息，不启动回合。
- 空闲时 `aside` 会启动回合，除非处于 plan 模式或用户刚打断。
- 默认 `deliverAs` 只在调用方要求时启动回合，否则只追加并持久化。
- `isStreaming` 包含已受理但尚未派发的提交，因此“忙”不等于模型正在生成。

接入按 SystemOne 动作选择宿主调用，不对全部来信固定使用 `aside`。扩展的 void 返回只支持记录提交尝试。宿主未提供通用的模型消费确认；消费边界未接通时保留未确认记录，不能凭调用成功推进游标。

这些是宿主行为，不改变 Onlyne 的消息事实。接入把“已注入”和“已唤醒”记录为本地证据，不提升为持久接纳或业务答复。

todo 路径的额外限制：todo-more 的 schema 会丢弃未知字段，因此来源不能作为独立键挂在条目上；人类可读来源只能放在 explanation 首段，机器可解析来源由 Onlyne 本地记录保存。程序化追加不产生 todo 工具结果，界面是否立即刷新尚未实测。首版必须验证刷新，否则改用会产生工具结果的写入路径，或明确显示待刷新状态。

源码索引以安装包为根：`src/extensibility/extensions/types.ts:1861-1872` 定义发送与持久化 API，`src/session/agent-session.ts:11991-11994` 定义直接 IRC 投递结果，`src/irc/messaging.ts:45-107` 定义 `executeSend`。todo-more 的 `src/commit.ts:15-33` 定义快照与投影提交，`src/state.ts:71-107` 定义条目和快照字段。`commitTodoState` 的 saved 表示分支接受记录，源码未承诺磁盘刷写确认，恢复验收必须覆盖这个边界。

IM 会话在本设计里是独立资源节点，不是 agent 的 thread。它由资源 endpoint 持有事件和绑定，经 `resource` 动作操作；本 RFC 不定义它的接入细节。

## 9. 验收

- agent 全程使用一个 msg 入口完成目标发现、thread 管理、列表读取、普通发送、引用回复、等待和资源交接。
- 两个 thread 使用相同 seq、两个目标使用相同显示名时，回复仍到达明确来源。
- 新 thread 创建后立即得到别名，后续发送和读取都用该别名定位。
- 多个 thread 有未处理条目时，自动片段按 thread 分组；已读完的 thread 不重复占上下文。
- thread 引用不唯一时返回候选，不猜测目标 thread。
- 关闭 thread 后仍可按别名读历史，不能向它发送新消息。
- 关闭 thread 后，已提交未注入的关注条目仍会出现在自动片段，直到消费或明确处置。
- 成员变更只改本地成员表，读权限需要单独的 read_scope 授予。
- 目标有多个上下文时，接入返回候选而不擅自选择；模型用 context 别名重试后到达指定上下文。
- 进度补充被记录为 followup，不解除对方的等待；工具结果回报实际关系。
- 资源操作按目录返回的 schema 校验，非法参数在本地被拒绝。
- 正文包含伪造头部和多行文本时，渲染保留条目边界及原正文。
- await 期间网络路径继续接收，合法 reply 解除等待，用户中止只取消本地等待。
- 宿主不能保持工具调用时不发送 `await` 消息，并明确说明缺失能力。
- received、已注入、已答复和 todo 完成各有自己的证据，互不推定。
- 消息和 todo 的恢复关联不会重复创建执行上下文或待办。
- plan 模式的新来信显示 queued，切回执行后按原引用处理。
- 交接只在资源 owner 提交后显示生效，旧发送者的权限按当前绑定校验。
