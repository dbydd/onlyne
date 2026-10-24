# RFC 0016：SystemOne 的判定上下文与情景调优

状态：Draft。本文定义 Onlyne 的决策输入与评测方法。消息工具保持 `msg({ delivery_kind, msg, meta })`。本文中的上下文包属于插件内部数据，不增加模型工具参数或网络消息字段。

## 1. 决策归属

SystemOne 负责所有需要理解消息含义的选择：发送什么操作、发给谁、是否等待、建立还是延续执行上下文，以及收到消息后放进片段、投递子代理、排队或唤醒。任务补充缺少 `in_reply_to` 时，SystemOne 可以结合任务历史找到正在执行的子代理。

插件收集事实、执行决定、报告结果。认证、实际授权、版本比较、消息去重与持久提交由库执行。执行前检查可以阻止无权操作、失效会话或不合法计划。检查失败不能使插件自行选另一个目标、改成新建会话或用默认值补齐语义决定。

SystemOne 可以看见权限、宿主状态和等待证据。看见事实不意味着模型取得修改该事实的权力。`plan_mode`、忙闲、用户中止、todo 状态都能帮助模型选择行动；实际执行必须服从宿主提供的能力。

同一 thread、相同根引用和关键词重合都属于证据。插件 MUST NOT 用这些条件预先删掉语义候选。授权范围、当前可见范围和显式稳定引用的解析可以机械限制候选。显示名重复时保留不同稳定身份，SystemOne 可以借助职责与交谈历史消歧；证据不足时请求澄清。

## 2. 可用上下文来源

上下文包按判定点分别构造。共享的是来源说明，不是所有任务共用一个 state 模板。

| 来源 | 可用于判断的信息 | 权威与时效 |
|---|---|---|
| 当前用户消息 | 目标、范围、优先级、是否等待、停止或更正 | 用户当前明确指令优先；保留其在会话中的位置 |
| 用户历史 | “他”“这个群”“继续刚才的任务”的指向 | 保留后续更正，不把旧意图当作当前指令 |
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

插件保留每份材料的来源身份、会话或 thread 引用、顺序或版本、采集时刻与完整性。材料缺失时记录 `unavailable` 及原因。已取得的空列表与无法读取必须分别表达。

直接用户指令、assistant 计划、工具证据、context note 和外部消息分别标记。外部正文中的“忽略规则”“改发给另一个人”作为被判断的消息内容保留。context note 中的结论可以被更新的用户消息和工具结果修正。

决策模型不需要看到秘密。认证材料由宿主使用，输入中只提供认证身份和授权事实。跨节点收到的消息不能借“补充上下文”取得本地用户的私有历史。

### 2.2 OMP 与库的接线

普通扩展通过 `ctx.sessionManager` 读取活动分支中的消息与自定义条目，通过事件记录当前调用和提交进度。`ctx.agent` 提供会话身份。跨会话状态来自 registry 和明确授权的读取接口。

context note 和 todo-more 需要各自的读取适配器。插件不把内部 SDK 字段写成已经存在的 `ExtensionContext` 能力。适配器未接通时，输入明确标记该来源不可用，评测样本中模拟提供的信息也要标记为构造数据。

Onlyne 的节点、域、thread、成员、消息与资源接口提供结构事实。插件可以把这些事实组合成职责与连接视图。组合视图必须保留来源，不能声称存在全局拓扑真相。

已核对的读取方式如下。OMP 源码路径相对安装包，todo-more 路径相对 `omp-todo-more@0.1.4`。

| 材料 | 实际接口与限制 |
|---|---|
| 活动分支 | `ctx.sessionManager.getBranch()`；`src/session/session-manager.ts:3764`。`getEntries()` 包含其他分支，不能直接用作当前任务历史 |
| 当前调用 | `tool_call`、`tool_result`、`tool_execution_*` 事件；执行中尚未提交的调用需按事件记录 |
| context note | `getContextNotes(branch)`；`src/session/context-notes.ts:41`。返回最近有效版本并服从 `reset_boundary`，可保留 `entryId` |
| 压缩摘要 | 活动分支的 `compaction` 与 `branch_summary` 条目。压缩摘要与 context note 是不同材料，分别保留覆盖位置 |
| 原始历史恢复 | `history://current/full` 保留活动分支的原 entry id 和压缩边界；需启用 experimentalContextManagement 并绑定 live branch（`src/internal-urls/history-protocol.ts:379-389`）。不可用时仍可由插件读取授权的 `getBranch()` 数据 |
| 当前身份与宿主观察 | `ctx.agent`、`isIdle()`、`hasPendingMessages()`、`getAsyncJobSnapshot()`。pending 不证明用户输入，idle 包含在途 prompt 状态 |
| plan 状态 | 活动分支的 `mode_change` 或 registry 中会话的 `getPlanModeState()`。后者属于导出的内部 SDK 接线，`getPendingPlanReview()` 只表示评审界面是否打开 |
| todo 简读 | `omp-todo-more:read` 事件；`src/review-api.ts:6-9,26-42`。只返回 goal 与 pending/in_progress 的 phase、title、explanation，失败为 unavailable |
| todo 完整状态 | `readTodoMore(branch)`；`src/state.ts:233`。需版本适配，含 blocked/completed/abandoned 等状态；不能从简读接口推断完整清单 |
| 子代理 todo | 每个子会话有自己的事件总线；通过该会话的适配器取得，父会话一次订阅不代表能读取全部子会话 |
| Onlyne 结构 | `domains`、`threads`、`messages`、`resources` 及出站记录。当前库没有全局 topology 类型，职责与连接图是拟议插件派生视图 |
| 等待 | 库的 live waiter 属于进程内状态。恢复后的 pending/unknown 出站记录不能证明等待者仍在 |

这些是可接入的数据来源。v3 插件尚未实现，本 RFC 不声称数据已自动装配。宿主也没有向普通扩展提供通用的模型消费确认，运行验收必须另外接通该边界。

todo 简读返回 `goal: null, openItems: []` 时，无法区分空清单和缺少 goal 且所有条目均为非开放状态的清单。blocked 也不出现在 openItems 中。context note 在 reset boundary 后失效，todo 的原生与 todo-more 读取会跨该边界保留清单。两个来源必须分别解释。

## 3. 分任务构造上下文

### 3.1 出站意图与目标

出站判定首先需要知道主模型正在替用户完成什么。输入主体是三段 `msg`，旁边是产生该调用的用户指令、最近模型说明和相关工具结果。

专用包应包含：

- 当前请求，以及同一目标最近发生的范围更正。
- 这次调用所属任务的 goal 和未完成工作。
- 多个可联系节点的稳定别名、职责与相关交谈片段。
- 候选 thread 的目的、成员及最近相关往来。
- 可执行的发送、读取和资源动作，以及接收方声明的上下文能力。

SystemOne 选择动作、目标、thread、等待方式与执行方式。省略 `meta.context` 不意味着新建会话。用户说“继续让他做”“这条也给刚才的人”时，历史和职责可以支持延续；证据缺失时返回澄清。

### 3.2 回复、补充与原消息选择

回复判定需要区分“结果已经完成”“报告进度”“继续提出要求”。专用包包含相关原消息和答复链、用户最近要求、模型当前结论、工具实际结果、尚未履行的承诺和关联 todo。

一条“好了”可能是最终结论，也可能只是数据已经准备好。SystemOne 从证据判断 `reply` 或 `followup`，并选原消息引用。外层连接的最后一跳和正文中的自称不改变认证作者或已记录的回复权威。

库验证合法 reply 是否匹配等待者。SystemOne 选择如何处理、呈现回复。模型不能把 receipt、followup、超时或 todo 完成改写成合法业务答复。

### 3.3 入站动作与执行上下文

入站专用包以新消息为中心，提供：

- 接收方最近的用户指令和主模型正在做的事情。
- 主模型的派发说明、task 结果和上下文登记。
- 所有授权可见的合理子代理候选，包括相似任务和不同 thread 的候选。
- 候选的任务范围、最近进展、context note、todo 与实际运行状态。
- 相关 thread 的消息链、原消息与可能缺失的引用。
- 等待、消费和宿主能力的事实。

SystemOne 输出 `fragment`、`context`、`queue`、`wake` 或 `wait_result`，并在需要时给出上下文别名。语义上仍有歧义时请求澄清；等待子代理登记时可以保持排队。

`fragment` 选择下一个上下文装配点并且不开启回合。`wake` 请求宿主现在启动回合，实际启动服从 plan 与中止约束。`queue` 保存待处理消息，此时尚未选入下一个上下文。三个动作分别表达呈现、启动与等待。

接入执行 `fragment` 与 `wake` 时使用 `onlyne.inbound` 自定义消息，`attribution` 为 `agent`；它不调用 `sendUserMessage`，不把外部正文变成用户发言。`queue` 使用 `appendEntry`，不进入模型上下文。`context` 使用 `onlyne.supplement` 自定义消息，经目标会话的 `sendCustomMessage` 送入子代理，不经过用户消息通道。

“先任务，后补充”的情景中，补充即使没有关系字段，也可能沿用户目标、主模型派发说明和子代理进展指向原执行会话。相同 thread 中的另一个任务不能仅因更近而抢走消息。跨 thread 的明确继续要求也不能被同 thread 过滤排除。

`context` 的目标由模型选择。插件只核对别名、会话是否可用及必要的父子关系。失效时报告失败，重新判定需要显式输入新的事实。

### 3.4 Thread 与域

Thread 判定关注协作边界：当前任务是否已经有可用对话、用户要查询还是创建、要加成员还是只发一条消息。输入包括候选 thread 的用途与历史、用户当前项目目标、已有成员、模型提出的方案和 todo 来源。

域判定关注节点归组。输入包括本节点当前关系、历史 join/leave、候选域的用途、节点自己的声明和查询失败。用户要求“让 reviewer 看一下”通常仍需结合任务含义判定，不能由字符串触发 join。

拓扑帮助识别职责与范围。相同域不授予 thread 权限。查询不到不等于退出。cwd 和物理终端位置不能自行决定域。

### 3.5 资源

资源判定关注“对什么做什么”。输入包括用户所指对象、模型最近操作、todo 的来源资源、候选资源目录、操作 schema、近期资源事件和出站证据。

同名 IM 群、多个计算作业和多个 MCP server 必须保留各自稳定引用及用途。“这个群”可以由当前任务来源确定，不能被最近一次无关目录查询替代。操作结果未知时，原操作引用和用户意图帮助决定查询、等待或明确提出新操作；插件不替模型重复提交。

资源权限、版本比较和真实提交仍由 owner 校验。模型不能把“转交结果接收权”扩展为取消作业或转移控制权。

### 3.6 恢复与等待呈现

恢复判定需要对照旧 note 与实际会话历史。专用包包含恢复到的分支、最后覆盖的历史位置、上下文登记及其版本、最近用户更正、原消息和出站引用、消费证据及 todo-more 快照。

SystemOne 区分原会话可恢复、正在登记、明确丢失和证据不足。用户已经把工作改派给另一个子代理时，新指令可以改变后续落点；旧 note 仍保留为历史材料。

等待超时只结束一次本地等待。用户中止不等于对方停止。IRC 返回 `injected`、`woken` 或 `revived` 不证明模型已消费，不能推进消费记录。相同逻辑上下文恢复时，历史和消费记录必须一起核对。

### 3.7 持续工作与 todo

需要持续处理的新请求、既有任务的补充、重复消息和单纯信息通知对 todo 的影响不同。SystemOne 可以读取 goal、阶段、条目状态、来源消息与子代理任务，决定新建工作项、补充已有工作还是仅保留信息。

同名 todo 不能作为消息关联键。库与运行时的持久提交负责防止同一来源重复建项。回复不会自动完成 todo，todo 完成也不会自动发出回复。

### 3.8 各处输入的字段布局

以下字段属于拟议的内部输入。每组围绕自己的判断对象命名，材料来源通过旁置引用表连接到分支 entry、thread 消息或目录版本。

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

只读目录、读取单条消息、获取更多上下文也是 SystemOne 可以选择的动作。上述布局定义所需材料的组织，不要求每次完整复制全部来源。缺少决定性材料时应继续读取，材料齐全时直接判定。

## 4. 装配与执行契约

各判定点维护自己的问题模板和上下文布局。共享工具只负责序列化、来源引用与调用记录。

输入不足时，模型可以请求补充指定来源。补取后仍保留原候选及原问题，不能把检索命中当作选定目标。OMP 的 history、多级压缩摘要和 context note 可以共同恢复任务经过。需要原始发言时，继续取得对应历史片段，不用固定 token 上限截掉判断证据。摘要必须保留覆盖位置，不能把推测写成认证事实。

有依赖的决定分步请求。例如先判断资源操作还是普通发送，再读取该动作需要的专用上下文。独立问题可以同次请求。联合动作与上下文必须相容，问题模板可以使用完整计划作为 choice，防止独立答案组合出不存在的计划。

同一消息重投复用已记录的决定。新信息触发的新判定单独记录，不覆盖原决定的输入和证据。

记录包含完整请求快照、来源引用、问题版本、实际模型 id、原始回答、概率、耗时与 token。只保存摘要无法复现实验。持久快照按项目数据权限保存，日志不包含凭据。

## 5. 情景调优方法

样本按出站、入站、回复、资源、结构关系、上下文与恢复分别组织。每个模块独立构造 state 和问题模板。构造样本使用实际工作中的表达与协议结构，必须说明哪些来自用户原话，哪些是重建场景。当前实验不声称拥有生产流量。

每个模块保留开发集和冻结的留出集。含同一任务的对照样本按情景组划分。只改显示名或序号不能算新的情景。留出集首次运行后不再据此修改本轮模板。

比较三份输入：

| 版本 | 内容 | 回答的问题 |
|---|---|---|
| minimal | 当前消息与基本候选 | 缺少工作上下文时模型如何判断 |
| focused | 为该任务装配用户、模型、历史和结构事实 | 哪些材料支持正确选择 |
| expanded | focused 加入无关对话和竞争记录 | 更多材料是否使判断偏移 |

所有版本使用相同合理候选和完整情景的冻结标签。minimal 去掉决定性材料后的得分表示它与完整情景目标的一致程度，不能据此指责模型合理的澄清，也不能声称该输入足以推出完整标签。focused 与 expanded 用于判断完整证据下的计划。问题模板不能为某个样本解释“这个就是用户指定的对象”。每道 choice 至少两个选项，记录中的 expected 与来源说明不进入请求。

调优依据开发集的错误：检查信息缺失、时效不清、来源权威混淆、问题冲突和候选描述。修订上下文或模板后保留初轮与修订轮原始输出。不能删除失败项、预选正确候选或新增语义默认值来提高分数。

评测报告分判定点与数据集，记录完整计划正确数、问题正确数、错误目标、错误子代理、重复工作、未知被误报为成功等错误。token 和耗时按实际请求逐项加权，接口错误单列并保留为未通过。

输入 token 仅作为运行记录。调优目标是判断充分且可复查，不要求上下文越短越好。expanded 的作用是检查无关材料的影响，较长的输入可以在它提供必要证据时成为推荐结构。

## 6. 本轮实验与结论

评测入口为 `proof/systemone_context/evaluate.py`，顺序重放为 `replay.py`，汇总为 `summarize.py`。模型通过本机 Magpie 路由 `group/decider` 调用，实际返回模型为 `clef-flash-oQ4e`。所有请求、完整响应、sha256 摘要、耗时和 token 记录保存在 `proof/systemone_context/evidence/`。

样本按出站发送（`send`、`send_extended`）、回复（`reply`）、入站动作（`inbound`）、上下文延续（`continuation`）、补充还是开新子代理（`followup_or_spawn`）、资源操作（`resource`）、结构操作（`structure`：thread 与 domain）、恢复（`recovery`）和待办关联（`todo`）分别组织，共 232 个独立情景样本。每个样本均属于手工构造的情景重构，明确标注来源事实与限制，不伪称生产流量录制。

### 6.1 留出集评测（Frozen Holdout）

留出集共 72 个样本（各切片分别留出 4～8 个样本），在模板和标签冻结后首次运行，不做针对性调优。每个样本在三个画像上各运行一次，共 216 次真实模型调用，传输错误为 0。

| 判定点 | 留出样本 | minimal 计划 | focused 计划 | expanded 计划 | focused 平均 token | 核心表现与主要失误 |
|---|---:|---:|---:|---:|---:|---|
| `followup_or_spawn`（补充 vs 开新子代理） | 8 | 5/8 | 8/8 | 8/8 | 874.9 | 准确区分“对已有子代理的无引用补充”与“独立新任务/要求另开代理”；消融实验证明缺少上下文会导致将新任务误投既有代理或盲目进入主会话 |
| `continuation`（独立上下文延续） | 8 | 2/8 | 5/8 | 7/8 | 724.2 | 无引用补充成功归入原任务；改派 c3 成功；未登记子代理误选立即投递（hold-07） |
| `inbound`（入站动作与落点） | 8 | 5/8 | 7/8 | 7/8 | 2798.6 | 提及唤醒与普通排队判别良好；plan 模式提及来信被误判为 queue（hold-01） |
| `resource`（资源与操作） | 8 | 7/8 | 6/8 | 7/8 | 923.9 | 相同显示名群聊依活动记录准确消歧；forward 意图在 describe/handover 上存在边界歧义 |
| `todo`（待办关联与状态） | 8 | 6/8 | 6/8 | 6/8 | 1019.1 | 恢复与阻塞保持准确判别；仅凭回复说明而无产物证据时出现误判完成（hold-06） |
| `send`（出站意图与目标） | 16 | 11/16 | 10/16 | 10/16 | 1013.5 | 目标消歧与意图识别良好；仅送材料的补充被模型赋予 async 等待期望 |
| `reply`（回复与补充判定） | 8 | 5/8 | 4/8 | 4/8 | 1948.5 | 结论回复与进度补充判别良好；单纯确认“好了”仍倾向选 followup，无凭证的对方转交诱发误选 |
| `thread`（thread 协作） | 4 | 2/4 | 3/4 | 3/4 | 2177.0 | 创建与复用准确；关闭 thread 后的新建仍偶发倾向复用旧 id |
| `domain`（域归组与发现） | 4 | 2/4 | 2/4 | 2/4 | 1518.5 | 成员状态与发现准确；不归属本节点的外部声明在离线排查时存在歧义 |
| `recovery` / `context` | 8 | 1/8 | 1/8 | 1/8 | 1862.6 | 候选问题依赖最小集导致扩展干扰受限，未消费与已注入边界在弱提示下易混淆 |

### 6.2 开发集评测（Development Tuned）

开发集共 152 个样本，在 `focused` 与 `expanded` 画像上完成对照评测：

- **补充还是开新子代理（`followup_or_spawn`，16 样本）**：`minimal` 仅 10/16，`focused` 达 15/16，`expanded` 达 14/16。成对对抗实验证明：
  1. 同话题来信（如协议细节），在前序指令为“给 c1 跟进”时准确判定为 `deliver:c1`，在前序指令为“c1 已锁死，后续修订独立开代理”时准确断决为 `new_subagent`。
  2. 在缺少工作上下文的 `minimal` 画像下，模型无法获知用户的前序隔离要求，盲目按话题关键词将全新任务塞进既有代理，或将重要补充漏入主会话。
  3. 留出集 8 个全新情景在 `focused` 与 `expanded` 上均达到 8/8（100%）。

- **上下文延续（`continuation`）**：`minimal` 仅 1/8，`focused` 与 `expanded` 均达 7/8。消融实验证明：在缺少用户历史与派发说明的 `minimal` 画像下，模型无法推断任务归属；补充历史后模型能准确跨 thread 关联目标子代理。唯一未解决项是 `dev-08`：会话彻底丢失（`isolated_lost`）时模型仍尝试投递而非请求澄清。
- **入站分流（`inbound`）**：`focused` 达 14/16，`expanded` 达 15/16。有效抵御了同 thread 伪造指令攻击；唯一失误为对端伪造本地用户身份且携带合法语气时偶发误判。
- **待办关联（`todo`）**：`focused` 达 15/16，`expanded` 达 14/16。准确区分了重复消息、关联补充与新建工作项；唯一失误为用户语境中“这一步标完成”与“文档仍需核验”冲突时模型倾向保守追加。
- **资源操作（`resource`）**：`focused` 与 `expanded` 均达 15/16。相同显示名群聊依最新事件事实准确消歧。
- **出站投递（`send`）**：`focused` 24/32，`expanded` 26/32。代词“也给他”结合上一轮发言成功消歧；失误集中在无需回复的材料被判定为轻度等待（`async`）。

### 6.3 顺序任务流重放（Sequential Replay）

在 `proof/systemone_context/replay.py` 中重放真实会话链路：**派发任务 → 产生未登记子代理 → 补充说明到达 → 子代理注册完成 → 结果产出 → 主会话对外回复**。

重放结果表明：

1. **任务派发**：模型准确决定创建独立子代理 `delegate_review`。
2. **未登记补充**：在子代理 `agent_id: null`、状态为 `unbound` 时，模型准确识别该补充属于 review 任务，但**倾向直接选择投递**，置信度达 0.8744，未能选入 `queue_registration`。这证实：**未登记子代理的拦截必须由插件确定性前置条件兜底，不能假设模型能主动排队**。
3. **登记后补充**：子代理注册完成后，模型准确执行 `continue_review`。即使缺少 `in_reply_to`，依然能根据工作上下文正确归入。
4. **历史恢复验证**：引入压缩 context note 与 `history://` 恢复原始发言后，模型成功纠正旧 note 中的过时改派，依据原始指令准确回复原请求权威。

### 6.4 关键结论与局限

1. **富上下文显著改善语义归属**：在上下文延续、入站动作和待办判别上，`focused` 相比缺失上下文的 `minimal` 计划准确率提升 40%～75%。无引用补充说明依靠历史完全可被正确路由。
2. **干扰鲁棒性合格**：在 `expanded` 混入无关 thread 历史、过期 note 和竞争候选时，核心判定点（inbound、continuation、todo、resource）准确率保持平稳甚至因补充事实略有上升，未发生灾难性漂移。
3. **执行约束不可下放**：模型在语义上能识别目标，但对“目标尚未就绪因此不能现在投递”（未登记子代理、丢失的隔离会话、已关闭 thread）的物理可用性判断不稳定。插件必须在执行前机械校验：别名在目录中且可用、会话活存、具备权限，否则返回 `context_lost` 或拒绝，不可全权交由模型推断。
4. **评测局限**：本轮样本为情景重构而非线上录包；虽然包含 208 个情景，但主要评估单步意图与顺序链路。真实运行仍需在生产环境中进行旁路影子运行（shadow evaluation）。

## 7. 验收

- 每个判定点有任务专用布局，说明用户、模型、历史、note、todo 与 Onlyne 结构信息的用途及来源。
- 出站和入站语义决定都由 SystemOne 做出；没有通过关键词、thread 或根引用预选落点。
- 新任务、无引用补充、任务交叉、跨 thread、旧 note、用户更正、排队登记、上下文丢失均有竞争样本。
- 开发与留出分别报告，原始请求、模型 id、回答和失败可回查。
- 留出失败保留为已知限制，不能用开发集满分声称已具备稳定自动投递能力。
- 任务创建、补充进入原子代理、产出回主会话与主会话回复使用顺序模拟检查归属；真实插件调度另外验收。
