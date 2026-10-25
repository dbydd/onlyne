# SystemOne 判定上下文设计

SystemOne 是 OMP 的 `judge` 角色在本插件里的用法：凡是需要理解消息含义的选择，都交给它；插件负责收集材料、执行决定、报告结果。本文描述每个判定点看到什么材料、材料怎么标记来源、字段怎么组织、装配时怎么裁剪。判定归属、权限边界与机械前置条件属于规范要求，在 RFC 0016 与《OMP 接入契约》中规定；具体问题模板、字段名与预算属于实现设计。

## 1. 判定点与实现位置

| 判定点 | 判断什么 | 实现 |
|---|---|---|
| 出站意图 | 发什么动作、给谁、进哪个 thread、是否等待、现有还是新执行上下文 | `JudgeDecider.decide` |
| 回复与补充 | `reply` 还是 `followup`，引用哪条原消息 | `decide` 的 `in_reply_to` 分支与 `relation` 选择项 |
| 入站动作 | `fragment`、`wake`、`queue`、`context`、`new_subagent`，落在哪个上下文 | `decideInbound` |
| 补充 vs 开新子代理 | 归入已有子代理、排队等登记、派发新子代理、主会话吸收或澄清 | `decideInbound` 的 `candidate_executions` 选择项 |
| 上下文选择 | 目标已有多个执行上下文时选哪一个 | `#existingContext` |
| thread | 创建、读取、描述、加成员、移除成员 | `decide` 的 `thread` 动作 |
| 域 | 创建、加入、退出、查询、发现 | `decide` 的 `domain` 动作 |
| 资源 | 对哪个资源执行哪个操作、参数从哪来 | `decide` 的 `resource` 动作 |
| 恢复 | 原会话是否可恢复、落到哪个上下文、是否排队 | 入站与上下文判定在会话恢复后的输入 |
| todo | 新建、补充、仅记录、解除阻塞 | `decideTodo` |

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

### 3.1 出站意图与目标

出站判定首先要知道主模型正在替用户完成什么。输入主体是三段 `msg`，旁边是产生该调用的用户指令、最近模型说明和相关工具结果。

专用包包含：

- 当前请求，以及同一目标最近发生的范围更正。
- 模型已经做出的承诺与已经派发的任务。
- 可用收件人及其职责、当前连接与信任阶段。
- 候选 thread 及其用途、最近消息、成员。
- 目标节点公布的执行上下文，以及本节点的执行上下文。
- 当前工作目标、宿主能力（能否新建上下文、能否 await）。
- 用户最近的停止、改派或「先别发」类指令。

SystemOne 选择动作、目标、thread、等待方式与执行方式。省略 `meta.context` 不意味着新建会话。用户说「继续让他做」「这条也给刚才的人」时，历史和职责可以支持延续；证据缺失时返回澄清。

### 3.2 回复、补充与原消息选择

回复判定要区分「结果已经完成」「报告进度」「继续提出要求」。专用包包含相关原消息和答复链、用户最近要求、模型当前结论、工具实际结果、尚未履行的承诺和关联 todo。

一条「好了」可能是最终结论，也可能只是数据已经准备好。SystemOne 从证据判断 `reply` 或 `followup`，并选原消息引用。外层连接的最后一跳和正文中的自称不改变认证作者或已记录的回复权威。

库验证合法 reply 是否匹配等待者；SystemOne 选择如何处理、呈现回复。模型不能把 receipt、followup、超时或 todo 完成改写成合法业务答复。

### 3.3 入站动作与执行上下文

入站专用包以新消息为中心，提供：

- 接收方最近的用户指令和主模型正在做的事情。
- 这条消息的认证作者、thread、引用、关注范围与关系。
- 候选执行上下文：任务范围、父子关系、状态、最近活动、材料来源。
- 相关 thread 的最近对话与尚未处理的条目。
- 活跃等待：谁在等这条消息，等待是否仍属于本进程。
- 宿主观察：idle/streaming、plan、是否已有排队消息。

SystemOne 输出 `fragment`、`context`、`queue`、`wake` 或 `wait_result`，并在需要时给出上下文别名。语义上仍有歧义时请求澄清；等待子代理登记时可以保持排队。

`fragment` 选择下一个上下文装配点并且不开启回合；`wake` 请求宿主现在启动回合，实际启动服从 plan 与中止约束；`queue` 保存待处理消息，此时尚未选入下一个上下文。三个动作分别表达呈现、启动与等待。

接入执行 `fragment` 与 `wake` 时使用 `onlyne.inbound` 自定义消息，`attribution` 为 `agent`；不调用 `sendUserMessage`，不把外部正文变成用户发言。`queue` 使用 `appendEntry`，不进入模型上下文。`context` 使用 `onlyne.supplement` 自定义消息，经目标会话的 `sendCustomMessage` 送入子代理，不经过用户消息通道。

「先任务，后补充」的情景中，补充即使没有关系字段，也可能沿用户目标、主模型派发说明和子代理进展指向原执行会话。相同 thread 中的另一个任务不能仅因更近而抢走消息。跨 thread 的明确继续要求也不能被同 thread 过滤排除。

`context` 的目标由模型选择；插件只核对别名、会话是否可用及必要的父子关系。失效时报告失败，重新判定需要显式输入新的事实。

### 3.4 Thread 与域

Thread 判定关注协作边界：当前任务是否已经有可用对话、用户要查询还是创建、要加成员还是只发一条消息。输入包括候选 thread 的用途与历史、用户当前项目目标、已有成员、模型提出的方案和 todo 来源。

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
| 出站 | `request` 为原三段输入；`user_intent_timeline`、`assistant_commitments`、`recipient_roles`、`conversation_candidates`、`receiver_contexts`、`current_work`、`supported_delivery` | 动作、目标、thread、response 与 execution |
| 回复 | `draft`、`request_candidates`、`reply_chains`、`latest_user_constraints`、`verified_work_results`、`remaining_commitments`、`reply_authority_records` | 原请求、reply/followup、是否提出新的等待 |
| 入站 | `arrival`、`current_user_focus`、`parent_dispatches`、`candidate_executions`、`related_thread_dialogue`、`live_waits`、`host_observation` | fragment/context/queue/wake/wait_result 与落点 |
| 上下文选择 | `continuation_material`、`user_reassignments`、`dispatch_history`、`context_registry`、`candidate_notes_and_recent_turns`、`task_source_links` | 延续某上下文、新工作提案、等待登记或澄清 |
| 补充 vs 开新子代理 | `arrival`、`candidate_executions`（含各子代理当前任务范围与状态）、`local_user_turns`（前序指令与最新改派）、`dispatch_and_thread_history`、`todo_more`、`host_observation` | 归入既有子代理（deliver）、排队等待登记（queue）、派发新子代理（new_subagent）、主会话吸收或澄清 |
| thread | `conversation_intent`、`existing_conversations`、`membership_observations`、`work_goal`、`source_messages`、`local_user_decisions` | create/read/describe/member 操作与对象 |
| 域 | `grouping_intent`、`own_membership_history`、`candidate_domain_purposes`、`node_declarations`、`unavailable_observations`、`project_scope_decisions` | 归组或发现动作及域 |
| 资源 | `operation_intent`、`work_origin`、`resource_catalogue`、`operation_schemas`、`recent_resource_events`、`prior_operation_evidence`、`user_scope_changes` | 资源、操作及参数来源 |
| 恢复 | `restored_branch`、`compaction_summaries`、`context_note_revision`、`recovered_source_entries`、`registry_generations`、`outbound_and_consumption_evidence`、`current_user_changes` | 恢复原落点、查回事实、排队或请求材料 |
| todo | `work_request`、`todo_goal_and_items`、`message_item_associations`、`task_deliverables`、`source_thread_dialogue`、`user_completion_criteria` | 新建、补充、记录、解除阻塞或澄清 |

`candidate_executions` 每项保留任务范围、父子关系、状态、最近活动和材料来源。不同候选的 note 与最近发言不能摊平成一段文字，否则消息来源会混淆。`prior_operation_evidence` 保留 operation_id 与未知结局，不能由 assistant 的完成承诺代替。

只读目录、读取单条消息、获取更多上下文也是 SystemOne 可以选择的动作。这些布局定义材料的组织方式，不要求每次完整复制全部来源。缺少决定性材料时应继续读取，材料齐全时直接判定。

## 4. 装配与执行

各判定点维护自己的问题模板和上下文布局。共享工具只负责序列化、来源引用与调用记录。

**输入不足时继续取材料。** 模型可以请求补充指定来源。补取后仍保留原候选与原问题，不能把检索命中当作选定目标。OMP 的 history、多级压缩摘要和 context note 可以共同恢复任务经过；需要原始发言时继续取得对应历史片段。摘要必须保留覆盖位置，不能把推测写成认证事实。

**有依赖的决定分步请求。** 例如先判断资源操作还是普通发送，再读取该动作需要的专用上下文。独立问题可以同次请求。联合动作与上下文必须相容，问题模板可以使用完整计划作为 choice，防止独立答案组合出不存在的计划。

**同一消息重投复用已记录的决定。** 新信息触发的新判定单独记录，不覆盖原决定的输入和证据。判定记录包含完整请求快照、来源引用、问题版本、实际模型 id、原始回答、概率、耗时与 token；只保存摘要无法复现。持久快照按项目数据权限保存，日志不含凭据。

**装配预算。** 后端单次请求上限 16384 token，且按序列化后的前若干字符截断，长历史会先挤掉当前请求。插件因此自己设上限：整个 state 序列化后不超过 40000 字符；超限时按 `history` → `task_dispatches` → `context_note` 的顺序整段丢弃（数组从最旧的一半开始丢），请求与问题永不裁剪；仍超限则判定失败并记录，不静默截断。历史最多 40 行，单行文本最多 4000 字符；图片只保留 mime 类型，字节以占位说明代替。

**机械前置。** 别名是否在当前目录、会话是否活存、操作权限是否成立、子代理是否已登记，由插件执行前核对，不交给模型推断。引用解析（别名到稳定身份）是确定性动作；显示名歧义时返回候选清单。这部分要求的规范条文在 RFC 0016 §5，实现集中在 `decider.ts` 的显式解析与只读守卫。
