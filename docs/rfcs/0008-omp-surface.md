# RFC 0008：OMP 中的 agent 交互界面

状态：Draft。本文说明 OMP 接入向模型暴露哪些表面，以及每部分的规范来源。规范性要求已迁至 [《OMP 宿主契约》](../contracts/omp.md)；宿主接口事实与版本限制见 [OMP 扩展接口调查](../references/omp-extension-api.md)；模块、存储与界面实现见 [OMP 接入设计](../design/omp/)。库的消息、权限和等待规则沿用 [投递与运行时契约](0005-delivery-and-runtime.md)。本文的陈述都是契约条款的摘要；与契约冲突时以契约和协议 RFC 为准。

## 1. 界面组成

OMP 中的 agent 通过一个 `msg` 工具发信、回复、读取消息、管理自己的 thread 和操作资源。新来信以消息列表片段进入模型上下文。需要持续处理的来信进入该 OMP 会话自己的 todo，由运行时提醒。

| 表面 | 模型看到什么 | 谁持有数据 |
|---|---|---|
| 常驻说明 | 当前身份、工具用法、目标别名和引用规则 | OMP 接入 |
| `msg` 参数与结果 | 意图、正文、明确目标或消息引用、已发生的事实 | Onlyne 库 |
| 来信片段 | 本地索引、来源、关系和正文 | 本地 thread 与 OMP 注入游标 |
| todo | 持续工作、来源和需回复的来信引用 | OMP 运行时 |

工具名称固定为 `msg`，调用形状固定为 `msg({ delivery_kind, msg, meta })`。thread 的创建、关闭、列举、成员变更是模型的可见操作，模型只给意图和本地别名；ThreadRef、成员状态、seq 和授权细节由库保存和校验。资源操作经同一入口表达。

能力矩阵、拒绝前置条件和各表面的分工见[《OMP 宿主契约》](../contracts/omp.md)§2、§3；界面布局与渲染实现见 [OMP 接入设计](../design/omp/)。

### 1.1 初次引导与配置

首次加载无需预写配置。`/onlyne init`、`/onlyne menu` 和未配置时的 `/onlyne` 打开同一个 bootstrap 界面，收集 region、node.id、server.bind、node.key_scope 和 threads.page_size，写入宿主配置。region 输入短名时界面展示将保存的 `urn:onlyne:region:<编码后短名>` 并请确认。

监听端口由操作系统在本次启动分配，endpoint 由实际监听结果生成并发布；配置不包含 endpoint、port、peers、default_attention、omp.spawn 或 supplement_window。对端经网络自动发现，地址与核对记录写入本地发现目录。保存配置、启动监听、提交 region 成员关系、发布发现是分别可失败的步骤，界面显示实际完成位置，取消引导不写配置也不启动节点。

字段取值与默认值、密钥位置与权限、git ignore 检查、四步失败与续做规则见[《OMP 宿主契约》](../contracts/omp.md)§4；发现与 region 的协议规则见 [RFC 0011](0011-discovery-and-domains.md)。

## 2. 参数与目标

`delivery_kind` 是自然语言投递意图，`msg` 是消息正文或本地查询要求，`meta` 存放明确引用，所有字段使用 snake_case。模型只给意图和本地别名；ThreadRef、成员状态、seq 与授权细节由库保存和校验。别名由接入分配，在当前上下文内唯一且不重新绑定，显示名重名时显示不同别名；过期或歧义引用返回 `not_found` 或 `ambiguous_target`。

回复使用 `in_reply_to` 的 `(thread, seq)` 引用，引用确定回复去向；同时给 `to` 且与原来源不一致时返回 `invalid_request`。进度补充写明 `relation: "followup"`。省略 `relation` 时由 SystemOne 按 `delivery_kind` 判断：进度或补充用 followup，明确答复用 reply，都判不出时返回 `invalid_request` 并要求模型写明关系，不按线上默认值猜成 reply。`execution: existing` 需要明确接收上下文，证据不足时返回 `ambiguous_target`。

`meta` 字段表、别名解析规则、关系回报要求和落点消歧证据见[《OMP 宿主契约》](../contracts/omp.md)§5；投递计划的两端协商见 [RFC 0005](0005-delivery-and-runtime.md) §1。

## 3. 发现、读取与资源操作

模型需要的目录和历史同样经 msg 查询。SystemOne 把本地查询映射到类型化读取，查询不产生发给对端的业务消息；查询和修改的实际计划都出现在工具结果中。目录结果给出每个资源声明的操作名与参数 schema 引用，接入只接受 schema 允许的操作和字段，非法参数在本地返回 `invalid_request`，不发送到资源 endpoint 再解释失败。

分页用不透明 cursor，每页最多 100 条、默认 50，页面大小由 `meta.page_size` 指定，读取权限在每次读取时检查。交接结果为 `pending` 时显示交接待决，只有资源 owner 返回 `committed` 和新绑定才显示交接已生效；平台凭据留在资源 endpoint。

读取动作的类型与必填字段见 [RFC 0005](0005-delivery-and-runtime.md) §1，资源操作与交接的语义见 [RFC 0006](0006-resources-and-handover.md)；OMP 侧的读取入口、分页与交接显示规则见[《OMP 宿主契约》](../contracts/omp.md)§5。

## 4. 消息列表

一个 thread 对应一个消息列表。thread 是列表的身份和容器，列表是 thread 按 seq 排列的条目，双向往来都在一起；同一模型上下文里可以同时看到多个 thread 的列表。

模型看到的列表有两种读取模式：关注列表（默认，含 @ 自己、@ all 以及回复自己的条目）与完整 thread（该 thread 的全部双向历史）。两种模式都可用于 thread、消息、单条三层。默认进入上下文的片段是「有未处理关注条目的列表」，按 thread 分组，每个 thread 一条汇总行；未被 @ 的普通通信不进入默认片段，模型主动取完整历史。注入游标按 `(context, thread, seq)` 记录，只推进被注入的关注条目，主动取完整历史不改变片段内容。关闭 thread 只停止新消息，历史保留可读。

索引属于信封：头部给出 `[seq] in|out · 来源 · 时间 · @提及`，正文逐行加两个空格和 `> ` 前缀，原始正文与空白保存在消息条目中，渲染前缀不写回正文。多行正文中的索引外观不创建新条目。长正文展示首行与 `[+N 行]`，模型用 msg 读取全文。

三层结构、两种模式的规则、引用歧义处理与完整渲染要求见[《OMP 宿主契约》](../contracts/omp.md)§6；消息顺序、关注义务与读取权限见 [RFC 0002](0002-threads.md)、[RFC 0003](0003-messages.md)、[RFC 0004](0004-events-and-views.md)。

## 5. 工具结果

工具结果同时提供可读文本和结构化 facts，模型能看到最终计划、接纳证据、关联答复及资源结果。结果必须区分已存本地待发、网络结果未知、对端接纳、对端拒绝、业务答复到达、等待超时和用户中止等待，不得把一种事实写成另一种。

`await` 的工具调用停在库的 Promise 上，网络接收路径直接验证和匹配 reply 后解除等待；回执和 followup 只保留为事实。合法 reply 经 `await` 工具结果进入某上下文与经自动关注片段进入该上下文，共用 `(context, ThreadRef, seq)` 消费记录：一边确认消费后，另一边不重复展开正文。宿主无法让工具调用保持在库的 Promise 上时，在发送前拒绝 `await` 并说明缺失的能力，不先发送再返回 handle。

facts 的字段、消费记录的推进条件与 await 前置条件见[《OMP 宿主契约》](../contracts/omp.md)§7 与 §3；接纳事实、回复关系与消费记录定义见 [RFC 0003](0003-messages.md)、[RFC 0005](0005-delivery-and-runtime.md) §3。

## 6. 持续工作与 todo

OMP 接入按 SystemOne 的决定，把需要持续处理的来信写入宿主 todo 提供者。条目包含可读摘要、来源、消息引用和本地上下文关联；已有清单保留原目标和条目，补充可以关联已有工作，新任务才追加。todo 数据归宿主所有，Onlyne 不保存任务状态，只为消息与待办项建立本地关联。

快照与 MessageRef→待办项的稳定关联由同一次可恢复的持久提交记录确定，宿主投影与本地索引在该记录成功后派生；恢复先重放该记录、重建关联，再决定是否建立待办，不因派生索引缺失而重复建项。提醒由 todo 提供者发出，回复消息只产生回复关系，接入不因已经回复就自动完成待办。

来源分两处存放：人类可读来源写在条目 explanation 首段，机器可解析来源存 Onlyne 本地记录，键是消息引用；标题只作展示和回查，不作关联键。提交记录的结构、来源格式、写入失败与恢复行为见[《OMP 宿主契约》](../contracts/omp.md)§8；库侧的 todo 边界见 [RFC 0005](0005-delivery-and-runtime.md) §4。

## 7. 上下文、注入和中止

接入显式维护 `(node_id, OMP session_id, context, ThreadRef)` 映射。一个 OMP 会话可以读取多个 thread。新执行上下文获得自己的映射、权限、列表游标和 todo；任务派生的子代理是独立执行上下文，接入把上下文别名与子代理 id 记入目录，供 SystemOne 判定补充说明的落点。

每条来信由 SystemOne 判定处理动作，接入按动作执行：进入自动关注片段、投递到已有执行上下文、持久排队、启动回合，或作为在途等待的结果返回。宿主状态只决定执行方式，不改变判定：流式生成期间注入排在下一步边界，plan 模式只保存，用户中止释放当前本地等待，恢复会话按持久映射与注入游标补齐并重新检查权限。接入注入来信时使用自定义消息类型，不调用 `sendUserMessage`，不冒充本地用户发言。

动作表、注入渠道与 message 类型、新上下文能力、补充说明落点和中止语义见[《OMP 宿主契约》](../contracts/omp.md)§9；判定输入与情景评测见 [RFC 0016](0016-systemone-context.md)。

## 8. OMP 接线证据

宿主接口、注入调度、todo 写入限制与源码索引的完整调查记录见 [OMP 扩展 API 调查](../references/omp-extension-api.md)。本文只保留由调查得出的界面要求；调查结论描述宿主行为，不改变 Onlyne 的消息事实，也不构成运行证据。

IM 会话在本设计里是独立资源节点，不是 agent 的 thread。它由资源 endpoint 持有事件和绑定，经 `resource` 动作操作；本 RFC 不定义它的接入细节。

## 9. 验收

符合性场景列在[《OMP 宿主契约》](../contracts/omp.md)§11，每条标注对应的需求标识，本文不重复场景正文。实现状态、安装版本与某次实测结果按 §8 和其他证据单独记录，不写进验收条款。
