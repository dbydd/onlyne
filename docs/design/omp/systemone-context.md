# SystemOne 判定上下文设计

SystemOne 是 OMP 的 `judge` 角色在本插件里的用法：凡是需要理解消息含义的选择，都交给它；插件负责收集材料、执行决定、报告结果。本文描述每个判定点看到什么材料、材料怎么标记来源、字段怎么组织、装配时怎么裁剪。判定归属、权限边界与机械前置条件属于规范要求，在 RFC 0016 与《OMP 接入契约》中规定；具体问题模板、字段名与预算属于实现设计。

## 1. 判定点与实现位置

| 判定点 | 判断什么 | 实现 |
|---|---|---|
| 出站意图 | 发什么大类动作 | `decide` 的 O-1（`#intent`） |
| 回复与补充 | `reply` 还是 `followup`，发给谁 | `#relationQuestion`（O-2A relation）、`#targetQuestion`（O-2A target） |
| 交付计划 | 是否等待、续对端上下文还是新建 | `#deliveryPlan`（O-3A） |
| 上下文选择 | 目标已发布多个执行上下文时选哪一个 | `#receiverContext`（O-3A-Ctx） |
| 入站动作 | `fragment`、`wake`、`context`、`new_subagent`，落在哪个上下文 | `decideInbound` 的 I-1 |
| 入站插入时机 | 即时插话、回合结束后追加、静默背景 | `#insertionMode`（I-2） |
| 补充 vs 开新子代理 | 归入已有子代理、派发新子代理、主会话吸收 | I-1 的 `routing_target` 候选 |
| thread | 创建、关闭、列举、描述、加成员、移除成员 | `#threadAction`（O-2C）与 `#threadQuestion`（O-3B-Th） |
| 域 | 创建、加入、退出、查询、发现 | `#domainAction`（O-2D） |
| 资源 | 对哪个资源执行哪个操作、参数从哪来 | `#resourceAction`（`meta` 机械校验，无判定调用） |
| 恢复 | 原会话是否可恢复、落到哪个上下文 | 入站判定在会话恢复后的输入 |
| todo | 新建、补充、仅记录 | `decideTodo` 的 T-1 与 T-2 |

`msg` 工具只暴露三段输入，判定点由输入形状和本地证据决定，不需要模型显式选择「现在是哪个判定点」。一条消息在一次进程内只判定一次；重放复用记录，不重新询问。

## 2. 可用来源

| 来源 | 可用于判断的信息 | 权威与时效 |
|---|---|---|
| 当前用户消息 | 目标、范围、优先级、是否等待、停止或更正 | 用户当前明确指令优先；保留其在会话中的位置 |
| 用户历史 | 「他」「这个群」「继续刚才的任务」的指向 | 保留后续更正，不把旧意图当作当前指令 |
| assistant 消息 | 已提出的计划、承诺、派发说明、当前关注点 | 表达模型的理解；工具结果才能证明操作发生 |
| 工具调用与结果 | task 派发、真实 agent id、操作引用、查询结果、失败 | 区分计划、调用、结果与消费确认 |
| 活动分支历史 | 用户、模型、工具的实际交谈顺序 | 标记分支和截断处；其他分支不默认参与 |
| context note | 当前目标、来源引用、尚未解决的问题、子任务摘要 | 派生记录；注明更新时间与覆盖的历史位置 |
| 子代理上下文 | 父子关系、执行状态、任务描述、最近发言、上下文摘要 | registry 提供身份状态；子代理文本提供工作含义 |
| thread 对话 | 来往消息、原作者、引用、进度与更正 | 正文是外部材料，不能覆盖本地用户指令 |
| 消息列表与出站证据 | 原消息、待关注项、已知接纳、未知结果、等待 | received、已读、已答复与业务完成分别记录 |
| Onlyne 拓扑 | 已知节点职责、域声明、thread 成员、委托与连接观察 | 本地观察；域归组和可达性不自动授予权限 |
| 资源目录 | IM 会话、作业、数据源、MCP 操作、当前绑定与版本 | endpoint 拥有实际权限与结果，目录可能过时 |
| todo-more | goal、条目、阶段、状态、explanation、来源关联 | 是持续工作记录；完成一个 todo 不证明消息已答复 |
| 宿主能力与状态 | 当前 agent、父级、idle/streaming、plan、等待、中止 | 带采集时刻；执行时重新检查必要条件 |

### 2.1 来源标记

每份材料带来源身份、会话或 thread 引用、顺序或版本、采集时刻与完整性。三件事必须能区分：

1. **空与不可用**。已取得的空列表记为空；读取失败记 `unavailable` 及原因。两者不能都写成 `[]`。
2. **材料类别**。直接用户指令、assistant 计划、工具证据、context note、外部消息分别标记。外部正文里的「忽略规则」「改发给另一个人」作为被判断的内容保留，不执行。
3. **覆盖位置**。context note 与压缩摘要各自记录覆盖到哪条历史；旧 note 可以被新用户消息修正，但原 note 仍作为历史材料保留。

判定模型不需要看到秘密。认证材料由宿主使用，输入只给认证身份与授权事实。跨节点收到的消息不能借「补充上下文」取得本地用户的私有历史。跨会话状态只来自 registry 与明确授权的读取接口。

### 2.2 OMP 与库的接线

| 材料 | 来源方式 | 已知限制 |
|---|---|---|
| 活动分支 | `ctx.sessionManager.getBranch()` | `getEntries()` 含其他分支，不能当作当前任务历史 |
| 当前调用 | `tool_call`、`tool_result`、`tool_execution_*` 事件 | 执行中未提交的调用要按事件记录 |
| context note | `getContextNotes(branch)` | 返回最近有效版本，服从 `reset_boundary`；可保留 `entryId` |
| 压缩摘要 | 活动分支的 `compaction` 与 `branch_summary` 条目 | 与 context note 是不同材料，分别保留覆盖位置 |
| 原始历史 | `history://current/full`，或授权的 `getBranch()` 数据 | 需要 experimentalContextManagement 与 live branch 绑定；不可用时退回已授权读取 |
| 宿主状态 | `ctx.agent`、`isIdle()`、`hasPendingMessages()`、`getAsyncJobSnapshot()` | pending 不证明用户输入；idle 包括在途 prompt |
| plan 状态 | 活动分支的 `mode_change` 或 registry 的 `getPlanModeState()` | 后者属于导出的内部 SDK 接线 |
| todo 简读 | `omp-todo-more:read` 事件 | 只返回 goal 与 pending/in_progress；失败为 unavailable |
| todo 完整状态 | `readTodoMore(branch)` | 需要版本适配；不能从简读结果推断 blocked/completed |
| 子代理 todo | 该子会话自己的事件总线 | 父会话订阅一次不代表能读全部子会话 |
| Onlyne 结构 | `domains`、`threads`、`messages`、`resources` 与出站记录 | 库没有全局 topology 类型，职责与连接图是插件派生视图 |
| 等待 | 库的 live waiter | 进程内状态；恢复后的 pending/unknown 出站记录不证明等待者还在 |

具体函数签名、版本与源码位置见 [OMP 宿主接口参考](../../references/omp-extension-api.md)。适配器未接通时，输入标记该来源不可用；评测样本里模拟提供的信息也标记为构造数据。

todo 简读返回 `goal: null, openItems: []` 时不区分「空清单」与「缺少 goal 且所有条目均非开放」，blocked 也不在 `openItems` 里。context note 在 reset boundary 后失效，而 todo 的读取会跨过该边界保留清单；两个来源分别解释。

## 3. 各判定点的上下文

### 3.0 调用结构与节点清单

判定按**节点级联**进行：每个节点只问一件事（至多两个互相正交的子问题），只看到该节点需要的字段；`meta` 已固定的事实直接继承，不再提问。节点、问题 id 与实现对应如下（实现在 `packages/omp-onlyne/src/decider.ts`）：

| 节点 | 问题 id | 选项 | 触发 |
|---|---|---|---|
| O-1 `#intent` | `intent` | communicate / inspect / manage_thread / manage_domain / operate_resource | 非显式调用 |
| O-2A `#relationQuestion` | `relation` | reply / followup | `meta.in_reply_to` 存在且 `meta.relation` 未给 |
| O-2A `#targetQuestion` | `target` | 每个已发现 peer 一项 | 无引用且 `meta.to` 未给 |
| O-3A `#deliveryPlan` | `response`、`execution` | none / async / await；existing / new | 该字段 `meta` 未给 |
| O-3A-Ctx `#receiverContext` | `context` | 对端已发布上下文各一项、none | `execution: existing`、`meta.context` 未给、对端发布了上下文 |
| O-2B `#inspect` | `read_scope` | peers / inbox / messages / evidence / none | 判为 inspect |
| O-3B-Th `#threadQuestion` | `thread` | 每个本地线程一项 | 读 messages 且 `meta.thread` 未给；线程操作需要线程且未给 |
| O-2C `#threadAction` | `thread_op` | create / close / list / describe / member_add / member_remove | 判为 manage_thread |
| O-2D `#domainAction` | `domain_op` | create / join / leave / list / describe / discover | 判为 manage_domain |
| I-1 `decideInbound` | `routing_target` | main_agent / 每个可用上下文 / new_subagent | 每条来信 |
| I-2 `#insertionMode` | `insertion_mode` | steer / follow_up / fragment | I-1 选 main_agent |
| T-1 `decideTodo` | `action` | append / associate / none | 每条来信 |
| T-2 `decideTodo` | `task`，或 `title` + `explanation` | 既有条目；消息自身的文本候选 | T-1 选 associate 或 append |

**调用次数**

| 管线 | 次数 | 说明 |
|---|---|---|
| 显式（`meta.op` / `meta.resource` / `meta.scope`） | 0 | `#explicit` 直接装配 |
| communicate | 1–3 | O-1；O-2A（0/1）；O-3A（0/1）；O-3A-Ctx（0/1） |
| inspect | 2–3 | O-1；O-2B；O-3B-Th（仅 messages 且无 `meta.thread`） |
| manage_thread | 2–3 | O-1；O-2C；线程绑定（仅需要线程且无 `meta.thread`） |
| manage_domain | 2 | O-1；O-2D |
| operate_resource | 1 | 仅 O-1；字段由 `meta` 机械校验，缺字段报 `invalid_request` |
| 入站 | 1–2 | I-1；I-2（仅主会话） |
| todo | 1–2 | T-1；T-2（仅 append / associate） |

**确定性剪枝**（先于提问发生，被剪掉的候选不进入 prompt）：

1. `meta.in_reply_to` 存在时目标锁定为回复权威，不问 `target`；`meta.relation` 同时显式时 O-2A 完全跳过。
2. 无 `in_reply_to` 时不存在 reply / followup 关系（协议要求引用），关系直接定为新消息，不问 `relation`。
3. `meta.response`、`meta.execution`、`meta.context`、`meta.thread` 已给出时继承，不再问对应节点。
4. `execution: existing` 且对端公开发布了主会话时，`context` 落为 `main`；对端没有公开任何上下文或读取失败时返回 `ambiguous_target` 或 `unavailable`，MUST NOT 静默降级为 `new`（发送方以为在续写，接收方却开了新上下文）。
5. 入站：未绑定会话的上下文一律剔除；判定认为需要回答时，不能发信的上下文一并剔除，`fragment` 插入模式也一并剔除（静默片段永远不会启动回合，回复义务必然落空）；`new_subagent` 仅在不需要回答时出现；主会话自身不作为上下文候选。入站没有「压住」动作：真正起作用的只有插入时机（`steer` / `follow_up` / `fragment`），plan 模式由宿主自己约束注入。发送方的 `execution: new` 只是偏好：需要回答时改投有发信权的落点并记改判；`plan.context` 是绑定，指向已丢失上下文时返回 `context_lost`。
6. 只读子会话先照常分类，再由守卫按动作拒绝写入：读自己的切片放行，写操作给出读入口与交回主会话的方式。
7. 候选被剪到只剩一个选项时，题目本身取消，直接返回该选项：判定后端拒绝单选项选择题（探针实测），而只有一个答案的问题也不是判定。

**节点 state 的键**（每个节点只带自己的字段）：

| 节点 | state |
|---|---|
| O-1 | `focus`（delivery_kind、msg、meta_keys）、`trigger`（recent_user_prompt、last_thought）、`host_env`（session_type、read_only） |
| O-2A target | `focus`（delivery_kind、msg、meta_keys、meta_to）、`candidate_peers`（node_id、alias、last_seen）、`recent_inbound_unanswered` |
| O-2A relation | `focus`、`in_reply_to` |
| O-3A | `focus`、`relation` |
| O-3A-Ctx | `focus`、`target_published_contexts` |
| O-2B | `focus`（delivery_kind、msg、meta） |
| O-3B-Th | `focus`、`local_threads`（id、title、owner、state、next_seq） |
| O-2C / O-2D | `focus` |
| I-1 | `focus`（node、incoming）、`needs_answer`、`valid_contexts`（alias、summary、state、agent_id、can_answer、thread_ref）、`thread_dialogue` |
| I-2 | `focus`（incoming）、`host_running_state`（is_turn_active、plan_mode）、`urgency_signals`（has_stop_or_change） |
| T-1 | `focus`、`current_todo_summary`（title、status） |
| T-2 | `focus` 加 `current_todo_items`（associate），或 `title_candidates`、`explanation_candidates`（append） |

`focus` 在出站为 `{ delivery_kind, msg, meta_keys }`，在入站与 todo 为 `{ node, incoming }`。全量 `history`、`task_dispatches`、`children`、`context_note`、本地线程列表（O-2A、O-2B、O-3A 都不带）、发送候选（O-2B 不带）不进入任何节点。

### 3.1 出站意图与目标

O-1 只看三样东西：三段输入本身（`delivery_kind`、`msg`、`meta` 的键名）、触发这次调用的最近用户发言与模型最后一段说明、宿主身份（主会话或只读子会话）。在线节点、本地线程、对端上下文和全量历史都不进入这一层——它们会把「这条消息要做什么」拉向自己所在的那一类。

专用包包含：

- 当前请求，以及同一目标最近发生的范围更正。
- 产生该调用的用户指令（`trigger.recent_user_prompt`）与模型最后的说明（`trigger.last_thought`）。
- 宿主身份与只读标记；只读标记不改变分类，只改变可执行的动作（读放行、写拒绝）。

O-2A 才引入收件人：目标候选来自实时发现记录（`node_id`、短名、最近出现时间），并附上本节点尚未答复的来信摘要，帮助判断「回复刚才那个人」指的是谁。`meta.in_reply_to` 存在时目标锁定为回复权威；`meta.to` 存在时目标锁定；两者都没有才问 `target`。`meta.relation` 未给且存在引用时才问 `relation`。用户说「继续让他做」「这条也给刚才的人」时，历史与来信摘要可以支持延续；证据缺失时返回 `ambiguous_target` 或 `invalid_request`。

### 3.2 回复、补充与原消息选择

回复判定要区分「结果已经完成」「报告进度」「继续提出要求」。O-2A 的 relation 子问题只带 `focus` 与被引用的 `in_reply_to`：引用已经确定「在答复哪条」，问题只剩「这是最终答复还是进度补充」。原消息、答复链与用户最近要求由 `msg` 本身承载，不再额外摊开。

一条「好了」可能是最终结论，也可能只是数据已经准备好。SystemOne 从消息本身判断 `reply` 或 `followup`。外层连接的最后一跳和正文中的自称不改变认证作者或已记录的回复权威；引用解析（`{thread,seq}` → `MessageRef` 与回复权威）是确定性动作。

库验证合法 reply 是否匹配等待者；SystemOne 选择如何处理、呈现回复。模型不能把 receipt、followup、超时或 todo 完成改写成合法业务答复。

### 3.3 入站动作与执行上下文

入站分两段。**预过滤**先于路由提问：`needs_answer` 由判定模型读来信本身回答（正文、`delivery_kind`、发送方计划、引用只作证据），问候、提问、命令式请求与「已完成的答复」要分开；关键词表只作为提示，不作路由依据。未绑定会话的上下文剔除；`needs_answer` 为真时，不能发信的上下文剔除；`new_subagent` 候选只在不需要回答时出现；主会话自身不作为上下文候选。被剔除的候选不出现在题目里，因此没有「不要选它」这类反向提示词。

I-1 的输入是来信本身（作者、`delivery_kind`、正文、`meta`）、`needs_answer`、过滤后的 `valid_contexts`（别名、摘要、状态、agent id、能否发信、绑定的 thread），以及该 thread 的最近对话。选项是 `main_agent`、每个可用上下文别名、`new_subagent`（仅当不需要回答）。判定仍选中未提供的目标时，改投主会话并记 `onlyne.judge.overridden`。

I-2 只在 I-1 选 `main_agent` 时触发，输入是来信、宿主运行状态（当前回合是否进行中、是否 plan 模式）与紧迫信号（正文是否含停止、改向一类措辞）。选项三个：`steer`、`follow_up`、`fragment`。

映射到宿主：`steer` 以 `deliverAs: "steer"` 在当前工具调用结束后立即插话，空闲会话改为启动回合；`follow_up` 在忙时以 `deliverAs: "followUp"` 等本轮结束后追加并开启新回合，空闲时直接启动回合；`fragment` 以 `deliverAs: "nextTurn"` 静默进入下一个上下文装配点，不开启回合。`context` 投递到子会话（忙时 `steer`，空闲时启动回合）。plan 模式只影响宿主：注入被保存，不启动回合。

接入执行 `fragment` 与 `wake` 时使用 `onlyne.inbound` 自定义消息，`attribution` 为 `agent`；不调用 `sendUserMessage`，不把外部正文变成用户发言。`context` 使用 `onlyne.supplement` 自定义消息，经目标会话的 `sendCustomMessage` 送入子代理，不经过用户消息通道。

「先任务，后补充」的情景中，补充即使没有关系字段，也可能沿用户目标、主模型派发说明和子代理进展指向原执行会话。相同 thread 中的另一个任务不能仅因更近而抢走消息。跨 thread 的明确继续要求也不能被同 thread 过滤排除。

`context` 的目标由模型选择；插件只核对别名、会话是否可用及必要的父子关系。失效时报告失败，重新判定需要显式输入新的事实。

### 3.4 Thread 与域

Thread 判定关注协作边界：当前任务是否已经有可用对话、用户要查询还是创建、要加成员还是只发一条消息。输入包括候选 thread 的用途与历史、用户当前项目目标、已有成员、模型提出的方案和 todo 来源。线程列表与单个线程的成员描述属于线程操作（O-2C 的 `list` / `describe`），线程内的消息流水属于 inspect（O-2B 的 `messages`）；库里 `read threads` 与 `thread op=list` 是同一个读取，判定侧只保留 O-2C 一条路。

域判定关注节点归组。输入包括本节点当前关系、历史 join/leave、候选域的用途、节点自己的声明和查询失败。用户要求「让 reviewer 看一下」通常仍需结合任务含义判定，不能由字符串触发 join。

拓扑帮助识别职责与范围。相同域不授予 thread 权限；查询不到不等于退出；cwd 和物理终端位置不能自行决定域。

### 3.5 资源

资源判定关注「对什么做什么」。输入包括用户所指对象、模型最近操作、todo 的来源资源、候选资源目录、操作 schema、近期资源事件和出站证据。

同名 IM 群、多个计算作业和多个 MCP server 必须保留各自稳定引用及用途。「这个群」可以由当前任务来源确定，不能被最近一次无关目录查询替代。操作结果未知时，原操作引用和用户意图帮助决定查询、等待或明确提出新操作；插件不替模型重复提交。

资源权限、版本比较和真实提交仍由 owner 校验。模型不能把「转交结果接收权」扩展为取消作业或转移控制权。

### 3.6 恢复与等待呈现

恢复判定需要对照旧 note 与实际会话历史。专用包包含恢复到的分支、最后覆盖的历史位置、上下文登记及其版本、最近用户更正、原消息和出站引用、消费证据及 todo-more 快照。

SystemOne 区分原会话可恢复、正在登记、明确丢失和证据不足。用户已经把工作改派给另一个子代理时，新指令可以改变后续落点；旧 note 仍保留为历史材料。

等待超时只结束一次本地等待。用户中止不等于对方停止。IRC 返回 `injected`、`woken` 或 `revived` 不证明模型已消费，不能推进消费记录。相同逻辑上下文恢复时，历史和消费记录必须一起核对。

### 3.7 持续工作与 todo

需要持续处理的新请求、既有任务的补充、重复消息和单纯信息通知对 todo 的影响不同。SystemOne 可以读取 goal、阶段、条目状态、来源消息与子代理任务，决定新建工作项、补充已有工作还是仅保留信息。

同名 todo 不能作为消息关联键。写入什么信息、存放在哪里属于插件职责；库只暴露可恢复的本地记录接口（出站证据与消费记录），插件凭这些接口实现防重复建项的持久提交。回复不会自动完成 todo，todo 完成也不会自动发出回复。

### 3.8 各处输入的字段布局

每组围绕自己的判断对象命名，材料来源通过旁置引用表连接到分支 entry、thread 消息或目录版本。

| 判定点 | 输入布局 | 主要产出 |
|---|---|---|
| 出站 O-1 | `focus`（三段输入与 `meta` 键名）、`trigger`（最近用户发言、模型最后说明）、`host_env` | 大类意图 |
| 出站 O-2A | `focus`、`candidate_peers`、`recent_inbound_unanswered`；relation 子问题只带 `focus` 与 `in_reply_to` | 目标与 reply/followup |
| 出站 O-3A / O-3A-Ctx | `focus`、`relation`、`target_published_contexts` | response、execution 与接收上下文 |
| 回复 | 同 O-2A：引用已定，问题只区分最终答复与进度补充 | reply/followup 与目标锁定 |
| 入站 I-1 | `focus`（来信）、`needs_reply`、`valid_contexts`、`thread_dialogue` | fragment/context/wake/new_subagent 与落点 |
| 入站 I-2 | `focus`、`host_running_state`、`urgency_signals` | steer/follow_up/fragment |
| 上下文选择 | `continuation_material`、`user_reassignments`、`dispatch_history`、`context_registry`、`candidate_notes_and_recent_turns`、`task_source_links` | 延续某上下文、新工作提案、等待登记或澄清 |
| 补充 vs 开新子代理 | `arrival`、`candidate_executions`（含各子代理当前任务范围与状态）、`local_user_turns`（前序指令与最新改派）、`dispatch_and_thread_history`、`todo_more`、`host_observation` | 归入既有子代理（deliver）、派发新子代理（new_subagent）、主会话吸收或澄清 |
| thread | `conversation_intent`、`existing_conversations`、`membership_observations`、`work_goal`、`source_messages`、`local_user_decisions` | create/read/describe/member 操作与对象 |
| 域 | `grouping_intent`、`own_membership_history`、`candidate_domain_purposes`、`node_declarations`、`unavailable_observations`、`project_scope_decisions` | 归组或发现动作及域 |
| 资源 | `operation_intent`、`work_origin`、`resource_catalogue`、`operation_schemas`、`recent_resource_events`、`prior_operation_evidence`、`user_scope_changes` | 资源、操作及参数来源 |
| 恢复 | `restored_branch`、`compaction_summaries`、`context_note_revision`、`recovered_source_entries`、`registry_generations`、`outbound_and_consumption_evidence`、`current_user_changes` | 恢复原落点、查回事实、排队或请求材料 |
| todo T-1 | `focus`、`current_todo_summary` | append/associate/none |
| todo T-2 | `focus` 加既有条目（associate）或消息自身的文本候选（append） | 关联条目或新条目标题、说明 |

`valid_contexts` 每项保留摘要、状态、agent id、能否发信与绑定 thread；被预过滤剔除的上下文不出现，因此候选之间不需要「哪个不能用」的说明。`prior_operation_evidence` 保留 operation_id 与未知结局，不能由 assistant 的完成承诺代替。

只读目录、读取单条消息、获取更多上下文也是 SystemOne 可以选择的动作。这些布局定义材料的组织方式，不要求每次完整复制全部来源。缺少决定性材料时应继续读取，材料齐全时直接判定。

## 4. 装配与执行

各判定点维护自己的问题模板和上下文布局。共享工具只负责序列化、来源引用与调用记录。

**输入不足时继续取材料。** 模型可以请求补充指定来源。补取后仍保留原候选与原问题，不能把检索命中当作选定目标。OMP 的 history、多级压缩摘要和 context note 可以共同恢复任务经过；需要原始发言时继续取得对应历史片段。摘要必须保留覆盖位置，不能把推测写成认证事实。

**有依赖的决定分步请求。** 例如先判断资源操作还是普通发送，再读取该动作需要的专用上下文。独立问题可以同次请求。联合动作与上下文必须相容，问题模板可以使用完整计划作为 choice，防止独立答案组合出不存在的计划。

**同一消息重投复用已记录的决定。** 新信息触发的新判定单独记录，不覆盖原决定的输入和证据。判定记录包含完整请求快照、来源引用、问题版本、实际模型 id、原始回答、概率、耗时与 token；只保存摘要无法复现。持久快照按项目数据权限保存，日志不含凭据。

**装配预算。** 后端单次请求上限 16384 token，且按序列化后的前若干字符截断，长历史会先挤掉当前请求。插件因此自己设上限：整个 state 序列化后不超过 40000 字符；超限时按 `history` → `task_dispatches` → `context_note` 的顺序整段丢弃（数组从最旧的一半开始丢），请求与问题永不裁剪；仍超限则判定失败并记录，不静默截断。节点级 state 不再携带全量历史，这三段通常为空，预算作为硬上限保留。单条消息文本最多 4000 字符，`trigger` 片段最多 800 字符；图片只保留 mime 类型，字节以占位说明代替。

**机械前置。** 别名是否在当前目录、会话是否活存、操作权限是否成立、子代理是否已登记，由插件执行前核对，不交给模型推断。引用解析（别名到稳定身份）是确定性动作；显示名歧义时返回候选清单。这部分要求的规范条文在 RFC 0016 §5，实现集中在 `decider.ts` 的显式解析与只读守卫。

**judge 参数。** 每个判定点只调用 `judge(state, questions)` 这一种入口，参数按下面的形状填：

| 参数 | 填什么 |
|---|---|
| `state` | 上表的键组成的对象，经 `fitState` 裁剪后 `JSON.stringify`（`jsonState`）成字符串；不是模板文本，没有提示词拼接 |
| `questions` | `{ id: { type: "choice", instructions, criteria } }`；`criteria` 是「选项 → 一句判据」，判据里可以写该选项何时不适用 |
| 模型 | `resolveJudge({ settings, registry, sessionModel, purpose: "onlyne", sessionId, onUsage })`，取宿主的 `judge` 角色；判定不指定具体模型 |
| 答案 | `{ id: { choice, probabilities, confidence } }`；插件只读 `choice`，概率随判定记录一起留存 |

记录四条：`onlyne.judge.decided`（节点、输入、全部答案、耗时；节点取值 `outbound.o1`、`outbound.o2a`、`outbound.o3a`、`outbound.o3a-ctx`、`outbound.o2b`、`outbound.o3b-th`、`outbound.o2c`、`outbound.o2c-thread`、`outbound.o2d`、`inbound.i1`、`inbound.i2`、`todo.t1`、`todo.t2`）、`onlyne.judge.answered`（入站路由、插入时机与落点）、`onlyne.judge.overridden`（判据与机械事实冲突时的改判，附原因）、`onlyne.judge.trimmed`（`fitState` 丢了哪几段）。判定失败不写默认值，抛 `decision_unavailable` 并由库保持条目欠账。

**调优项（未实现，按现场报告排）。**

1. 判据补真实样例：`steer` 与 `follow_up` 的区分、`new_subagent` 与主会话的区分，各补两三条现场措辞（来自 `onlyne.judge.decided` 的输入与 `overridden` 的原因）。
2. `needs_answer` 由判定模型回答，正文与发送方计划都进入证据；`needsReplyOf` 的关键词表只留给不能判定的宿主作提示，不参与路由。若现场仍出现误判，按回复链（该 thread 是否有本节点未答复的来信）补充证据。
3. `recent_inbound_unanswered` 目前取每个 thread 尾部若干条、排除自带引用的来信；`needs_answer` 稳定后可改为「本节点最后一次发信之后」。
4. 入站 I-1 仍带整段 `thread_dialogue`；若报告显示注意力被拉走，改为只给最近三条。
5. 线程操作与线程绑定是两次调用；常驻多线程的节点若报告延迟明显，可合并为一次两问（op 与 thread 正交时成立，`create`/`list` 不需要线程，需按 op 预剪枝后再定）。
6. 探针（`/tmp/onlyne-probe`，9 条 lane、230 个样本、411 个断言，按路径各 10 条）的结论：整体 95.1%（391/411）。已修：`response` 的两维判据（20/20）；线程列表/描述归 `manage_thread` 后 `read-a` 与 `ops` 的线程路径全中（`thread_op` 32/32，`read_scope` 48/48）。剩余成簇偏差四处——命令式请求（"Read the messages of the thread…"）不算 `needs_reply`；`target` 在两个 URN 都被提及时缺职责（7/10）；`operate_resource` 无 `meta.resource` 时与 communicate 混淆；状态通知被派给 `new_subagent`（4 条）。前两处改输入（扩启发式、给 peer 补职责），后两处改判据。
7. 探针方法注意：`judge_batch` 的判定缓存不把 `criteria` 文本计入键，改判据后批量重跑会返回旧答案；改判据后必须逐条 `judge()` 重问（本轮 `fragment` 组因此从 20/30 修正为 29/30）。
