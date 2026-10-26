# SystemOne 分层决策体系架构与上下文工程完整规范

## 1. 体系重构背景与物理运行条件

### 1.1 根本目的与核心痛点
Onlyne 在接入 SystemOne 模型辅助决策时，过去的实现试图在一个扁平请求中同时抛出 8~9 个独立选择题，导致以下严重问题：
1. **解空间指数级爆炸**：潜在状态组合空间超过 30 万种（$5 \times 3 \times 2 \times 3 \times 5 \times 3 \times 8 \times 7 \approx 317,520$）。快速直觉模型根本无法做复杂的跨字段联合约束，导致判断摇摆。
2. **跨意图逻辑自相矛盾**：模型判定 `action: send` 的同时选了 `read_scope: messages`，执行层按 send 处理，把读请求误发成网络消息（`dpdp-found1` 问题 4）。
3. **无关选项诱导误判**：只要本地存在线程，就无条件提问 `thread`，诱导模型把发往外部的投递改成本地追加（`caerulea-found1/2` 问题 1）。
4. **上下文布局不良**：无差异地序列化全量上下文，杂乱的 `history` 和长文本抢占注意力，将核心输入与关键问题挤散。
5. **入站权限与生命周期盲区**：将必须回复的消息派发给不能发信的只读子会话，或者派发给已经销毁（disposed）的死会话黑洞（`cyanea-found1` 问题 5、`caerulea-found3` 问题 1）。
6. **入站主上下文插入模式粗糙**：旧逻辑仅区分了 `wake` 和 `fragment`，未充分利用 OMP 宿主提供的 `steer`（当前工具调用结束立即插话纠偏）与 `followUp`（本轮工作彻底完成后追加交付）机制，无法区分紧急打断与常规协作。

### 1.2 物理运行条件与架构原则
1. **本地部署，零推理成本（Budget-Free Local Inference）**：
   - SystemOne 决策模型部署在本地（遵循 Typesafe AI 规范），延迟极低（单次判定毫秒级）、吞吐高、调用预算实质无限。
   - **架构推论**：坚决摒弃“为了少调一次模型而把所有问题塞在一题”的做法。**多步级联（Cascading Multi-Tier）在本地环境中性能、吞吐与准确率达到最佳平衡**。
2. **单步认知状态严格收敛（<= 10 States per Step）**：
   - 每次交互仅回答 **1 个主导问题** 或 **至多 2 个高度正交的紧密子问题**。
   - 单次选择题的选项数严格控制在 **6 种以内**，绝对上限不超过 **10 种**。
3. **确定性代码先行（Deterministic Pre-filtering & Pruning）**：
   - 凡是调用方 `meta` 已确定的事实，直接固化为硬约束；
   - 凡是物理/协议不可能成立的选项（死会话、无发信权的只读会话、非本节点拥有的线程），在构建题目时**从候选中彻底剪枝（Prune）**，绝不呈现在 Prompt 中。
4. **禁止反向提示词（No Negative Rules in Prompts）**：
   - 严禁书写“若是 send 则选 none”等脆弱的反向规则；通过物理剪枝直接消除非法选项，题目中仅提供正面可行的选项。

---

## 2. 决策节点清单与全景架构图

### 2.1 决策节点总览（Decision Nodes Inventory）
整个出站、入站和待办系统拆分为 11 个原子决策节点：

| 节点编号 | 节点标识符 | 所在管线 | 核心职责 | 状态空间 |
|---|---|---|---|---|
| **O-1** | `outbound.intent` | 出站 Tier 1 | 判定顶层大类意图 | 5 选 1 |
| **O-2A** | `outbound.communicate.relation_target` | 出站 Tier 2A | 判定发信因果关系（new/reply/followup）与目标对端 | $\le 5$ 选 1 |
| **O-3A** | `outbound.communicate.plan` | 出站 Tier 3A | 判定交付期望（await/async/none）与执行方式（existing/new） | $3 \times 2$ 正交 |
| **O-3A-Ctx** | `outbound.communicate.receiver_context` | 出站 Tier 3A+ | 选择对端公开发布的执行上下文（仅在 existing 时触发） | $\le 5$ 选 1 |
| **O-2B** | `outbound.inspect.scope` | 出站 Tier 2B | 判定查阅的只读频道范围（peers/inbox/messages/evidence） | 5 选 1 |
| **O-3B-Th** | `outbound.inspect.thread` | 出站 Tier 3B | 选择要读取消息流水的具体本地线程（仅在 messages 时触发） | $\le 8$ 选 1 |
| **O-2C** | `outbound.thread.op` | 出站 Tier 2C | 判定本地线程运维操作类型 | 6 选 1 |
| **O-2D** | `outbound.domain.op` | 出站 Tier 2D | 判定域拓扑管理操作类型 | 5 选 1 |
| **I-1** | `inbound.routing_target` | 入站 Tier 1 | 选择入站消息承载实体（main_agent / 活跃上下文 / new_subagent） | $\le 4$ 选 1 |
| **I-2** | `inbound.main_insertion_mode` | 入站 Tier 2 | 针对进入主会话的消息，判定 OMP 插入时机（steer / follow_up / fragment） | 3 选 1 |
| **T-1** | `todo.action` | 待办 Tier 1 | 判定入站消息对待办列表的影响（append/associate/none） | 3 选 1 |

### 2.2 全景架构路由图
```mermaid
graph TD
    subgraph Outbound_Pipeline [出站决策管线]
    MsgIn[msg 调用输入] --> Det_Short{是否完全显式?}
    Det_Short -- 是 --> Exec_Direct[直接执行 / 0次 Judge]
    Det_Short -- 否 --> RO_Check{是否为只读子代理?}
    RO_Check -- 是且包含写意图 --> Refuse_RO[直接拦截: 子代理只读]
    RO_Check -- 否 --> Node_O1[O-1: outbound.intent<br/>5 选 1]

    Node_O1 -->|communicate| Node_O2A[O-2A: relation & target<br/>关系与目标节点判定]
    Node_O1 -->|inspect| Node_O2B[O-2B: inspect.scope<br/>查阅范围 5 选 1]
    Node_O1 -->|manage_thread| Node_O2C[O-2C: thread.op 6 选 1]
    Node_O1 -->|manage_domain| Node_O2D[O-2D: domain.op 5 选 1]
    Node_O1 -->|operate_resource| Check_Res[资源参数合法性校验]

    Node_O2A --> Node_O3A[O-3A: delivery plan<br/>响应期望与落地方式]
    Node_O3A -- execution: existing --> Node_O3A_Ctx[O-3A-Ctx: receiver_context<br/>选择对端上下文]
    Node_O3A -- execution: new --> Send_Assemble[装配 SendAction]
    Node_O3A_Ctx --> Send_Assemble

    Node_O2B -- scope: messages --> Node_O3B_Th[O-3B-Th: 选择具体线程]
    Node_O2B -- peers / inbox / evidence --> Read_Assemble[装配 ReadAction]
    Node_O3B_Th --> Read_Assemble
    end

    subgraph Inbound_Pipeline [入站决策管线 (含 OMP Steer/Follow-up)]
    EntryIn[入站消息到达 Entry] --> PreFilter[Pre-Filtering: 回复义务识别 & 死会话/无权会话剪枝]
    PreFilter --> Node_I1[I-1: inbound.routing_target<br/>路由目标选择 <= 4 选 1]

    Node_I1 -->|main_agent| Node_I2[I-2: inbound.main_insertion_mode<br/>OMP 插入时机 3 选 1]
    Node_I1 -->|existing context| Act_Ctx[生成 context 决策: 投递目标子代理]
    Node_I1 -->|new_subagent| Act_NewSub[生成 new_subagent 决策: 开新任务]

    Node_I2 -->|steer| Mode_Steer[即时插话: deliverAs: steer<br/>本次工具调用结束立即介入转向]
    Node_I2 -->|follow_up| Mode_FollowUp[事后追加: deliverAs: followUp<br/>整轮工作彻底完成后开启新回合]
    Node_I2 -->|fragment| Mode_Frag[背景注入: deliverAs: nextTurn<br/>不开启新回合，静默注入事实]
    end
```

---

## 3. 上下文组合矩阵（Context Composition Matrix）

### 3.1 黄金三段式 Prompt 布局
每个决策节点的输入 payload 严格按以下 JSON 顺序进行组装序列化：
1. **`focus`（调用焦点）**：当前触发调用的核心输入（`request` 或 `incoming`）与最新前因（用户目标）。
2. **`ground_truth`（剪枝后事实）**：该节点做出抉择所**唯独需要**的客观事实（在线节点列表、存活上下文等）。
3. **`questions`（判定题目）**：紧随事实之后，选项携带明确的正向特征词汇（Positive Invariants）。

### 3.2 全节点上下文详细构成矩阵

```
┌──────────────┬────────────────────────────────────────────────────────┬──────────────────────────────────────┐
│ 节点编号      │ 注入字段清单 (按出现顺序排序)                            │ 严禁包含的字段 (Forbidden List)        │
├──────────────┼────────────────────────────────────────────────────────┼──────────────────────────────────────┤
│ O-1          │ 1. focus: { delivery_kind, msg, meta_keys }            │ 1. peers (在线节点列表)               │
│ (intent)     │ 2. trigger: { recent_user_prompt, last_thought }       │ 2. threads (本地线程列表)             │
│              │ 3. host_env: { session_type, read_only }               │ 3. full_history (完整历史对话)         │
│              │                                                        │ 4. receiver_contexts (对端上下文)    │
├──────────────┼────────────────────────────────────────────────────────┼──────────────────────────────────────┤
│ O-2A         │ 1. focus: { delivery_kind, msg, meta_to, in_reply_to } │ 1. local_threads (本地线程列表)       │
│ (relation &  │ 2. candidate_peers: PeerRecord[] (在线且已握手对端)     │ 2. read_scopes (只读频道)             │
│  target)     │ 3. recent_inbound_unanswered: MessageSummary[]         │ 3. task_dispatches (任务派发流水)     │
├──────────────┼────────────────────────────────────────────────────────┼──────────────────────────────────────┤
│ O-3A         │ 1. focus: { delivery_kind, msg }                       │ 1. peers (已锁定目标，无需再传)       │
│ (plan)       │ 2. target_peer: { node_id, alias, capabilities }       │ 2. local_threads                      │
│              │ 3. relation: "reply" | "followup" | "new"              │ 3. history                            │
├──────────────┼────────────────────────────────────────────────────────┼──────────────────────────────────────┤
│ O-3A-Ctx     │ 1. focus: { delivery_kind, msg }                       │ 1. local_contexts (本地上下文)        │
│ (rcv_ctx)    │ 2. target_published_contexts: ReceiverContext[]        │ 2. peers                              │
│              │    [{ alias, summary, state, updated_at }]             │ 3. threads                            │
├──────────────┼────────────────────────────────────────────────────────┼──────────────────────────────────────┤
│ O-2B         │ 1. focus: { delivery_kind, msg, meta }                 │ 1. peers (发送候选列表)               │
│ (read_scope) │ 2. supported_scopes: [{ name, description, schema }]   │ 2. receiver_contexts                  │
│              │                                                        │ 3. send_plans                         │
├──────────────┼────────────────────────────────────────────────────────┼──────────────────────────────────────┤
│ O-3B-Th      │ 1. focus: { delivery_kind, msg }                       │ 1. peers                              │
│ (read_th)    │ 2. local_threads: ThreadSummary[]                      │ 2. full_thread_dialogues              │
│              │    [{ id, title, owner, state, last_snippet }]         │                                      │
├──────────────┼────────────────────────────────────────────────────────┼──────────────────────────────────────┤
│ I-1          │ 1. incoming: { author, delivery_kind, msg, meta }      │ 1. disposed_sessions (已销毁的会话)   │
│ (in_route)   │ 2. needs_reply: boolean                                │ 2. readonly_subagents (若需回复)      │
│              │ 3. valid_contexts: FilteredContextRecord[]             │ 3. peers                              │
│              │    [{ alias, summary, state, agent_id, can_answer }]   │                                      │
├──────────────┼────────────────────────────────────────────────────────┼──────────────────────────────────────┤
│ I-2          │ 1. incoming: { author, delivery_kind, msg, meta }      │ 1. registered_contexts (已确定为主会话│
│ (in_mode)    │ 2. host_running_state: { is_turn_active, plan_mode }   │ 2. peers                              │
│              │ 3. urgency_signals: { has_stop_or_change: bool }       │ 3. threads                            │
├──────────────┼────────────────────────────────────────────────────────┼──────────────────────────────────────┤
│ T-1          │ 1. incoming: { author, msg, meta }                     │ 1. peers                              │
│ (todo_act)   │ 2. current_todo_summary: [{ title, status }]           │ 2. receiver_contexts                  │
└──────────────┴────────────────────────────────────────────────────────┴──────────────────────────────────────┘
```

---

## 4. 出站管线深度技术规范（Outbound Deep Dive）

### 4.1 节点 O-1: 顶层意图判定（Intent Classification）
- **触发条件**：`meta` 未完全指明底层动作（即不是纯显式调用）。
- **问题定义**：
  - **ID**：`intent`（5 选 1）
  - **选项集合**：
    - `communicate`: 向其他智能体或节点发送消息、回复、进度补充。
    - `inspect`: 查阅节点级状态（看谁在线、看收件箱、读某个线程的消息、查投递凭证）。
    - `manage_thread`: 线程本身的操作（列举、描述成员、新建、关闭、成员增删）。
    - `manage_domain`: 节点域拓扑管理（加入、离开、发现域成员）。
    - `operate_resource`: 操作显式命名的外部系统资源。
- **边界与冲突裁决**：
  - 若输入既包含“发送”又包含“查询”（例如“发信问对方在线状态”），优先判定为 `communicate`，由后续分支处理。
  - 若意图模糊且包含“查看/读取/谁在线/收件箱”，归入 `inspect`；但“列出线程”“这个线程谁在里面”属于线程本身的操作，归入 `manage_thread`。

---

### 4.2 Communicate 管线（发送与回复）

#### 节点 O-2A: 目标与回复关系（Target & Relationship）
- **确定性预剪枝逻辑**：
  1. **回复权威绑定**：若 `meta.in_reply_to` 存在，解析该引用对应的 `reply_authority`（统一规范为合法 URN，短名如 `caerulea` 自动解析为 `urn:onlyne:agent:caerulea`）。
     - 此时 **`target` 强制锁定为该 authority**，不发起 target 提问！
     - 若 `meta.relation` 已显式提供（`reply` 或 `followup`），则本节点完全跳过，0 次模型调用！
  2. **显式目标锁定**：若 `meta.to` 已显式提供且非回复，`target` 确定性锁定，仅提问 `relation`。
  3. **绝不出本地线程题目**：彻底移除旧代码中 `if (threads.length > 0) questions.thread = ...` 的致命逻辑。
- **问题定义**：
  - **问题 1（若未锁定）**：`relation`（3 选 1）
    - `reply`: 针对前序消息的正式答复（将释放对端的等待阻塞）。
    - `followup`: 过程补充、进度更新或追问（不释放等待阻塞）。
    - `new`: 全新的独立发信，与历史消息无回复因果关系。
  - **问题 2（若未锁定）**：`target`（N+1 选 1）
    - 选项由当前在线握手对端生成：`${node_id}` -> `${node_id} (alias: ${short}, last_seen: ${time})`。
    - `none`: 未指向任何明确对端（将触发清晰的澄清报错）。

#### 节点 O-3A: 交付计划（Delivery Plan）
- **确定性前置**：若 `meta` 已经显式指定 `response` 或 `execution`，继承该值。
- **问题定义**：
  - **问题 1**：`response`（3 选 1）
    - `await`: 同步阻塞等待对端合法回复（适用于提问、求助、依赖对端输出的任务）。
    - `async`: 异步交付，不阻塞当前调用回合（适用于通知、自由协作、后台任务派发）。
    - `none`: 纯单向通知，无须对端做任何回复。
  - **问题 2**：`execution`（2 选 1）
    - `existing`: 延续对端某个正在运行的既有任务上下文。
    - `new`: 对端开启全新的独立子任务上下文。

#### 节点 O-3A-Ctx: 对端 Receiver Context 级联选择
- **触发条件**：`execution === "existing"` 且 `meta.context` 为空。
- **流程**：
  1. 调用 `listReceiverContexts(target_node_id)`。
  2. 若对端返回可用上下文列表 `ReceiverContext[]`：
     - 发起单选题 `receiver_context`：候选为各 context 的 `${alias}`，判据包含 `${summary} (state: ${state})`，附加 `none`。
     - 若模型选择某个别名，采用该别名；若模型选 `none`，**平滑回退为 `execution: "new"`**。
  3. 若对端公开上下文为空或读取不可用：
     - **确定性安全回退**：自动将 `execution` 降级为 `"new"`，不向模型提问，绝不报错或中断投递。

---

### 4.3 Inspect 管线（查阅与状态）

#### 节点 O-2B: 查阅领域分类（Inspection Scope）
- **问题 ID**：`read_scope`（5 选 1）
- **选项与严格语义**：
  1. `peers`: 查阅当前在线/可达的节点与联系人（回答“谁在线”、“有哪些邻居”、“当前网络拓扑”）。
  2. `inbox`: 查阅收件箱与待处理注意力事项（Attention items）。
  3. `messages`: 查阅某个具体线程内的历史消息流水。
  4. `evidence`: 查阅本节点某条出站消息的投递与回执凭证。
  5. `none`: 不属于上述任何可读频道（抛出结构化不可用原因）。

线程列表与单个线程的成员描述不属于 inspect：库里 `read threads` 与 `thread op=list` 是同一个读取（`#threads.list`），`describe` 只有 thread 动作，因此两者统一走 O-2C，`read threads` 只在调用方显式写 `meta.scope` 时保留。

#### 节点 O-3B-Th: 实体绑定定位
- **触发条件**：`read_scope === "messages"` 且 `meta.thread` 未指定。
- **问题 ID**：`thread`（M+1 选 1）
- **候选集合**：仅列出本地现存线程（`ThreadRef[]`），格式：`${ref.id}` -> `${title} (owner: ${ref.owner}, state: ${state})`，加 `none`。
- **免提问短路**：当 `read_scope` 为 `peers` / `inbox` / `evidence` 时，0 次额外调用，直接组装返回结果。

---

## 5. 入站管线深度技术规范（Inbound Deep Dive - 含 OMP Steer/Follow-up 机制）

### 5.1 预处理确定性剪枝（Pre-filtering Invariants）
在构建入站题目之前，必须完成代码层事实提炼：
1. **回复义务识别（`needs_answer`）**：
   - 由判定模型回答一个 yes/no 问题：这条来信是否在等本节点的答复。
   - 证据：正文、`delivery_kind`、发送方计划（`response` / `execution`）、关系与引用。
   - 问候、提问、命令式请求与“已完成的答复/状态通知”必须分开判断；`in_reply_to` 存在只说明它在回复某条消息，不说明它还需要回答。关键词表只作提示，不作判据。
2. **上下文生命周期与发信权限硬过滤（Fatal Pitfall Guard）**：
   - **已销毁过滤**：任何 `disposed === true` 或对应 session 进程已退出的会话上下文，**直接从候选列表中剔除**（彻底消除 `caerulea-found3` 消息入黑洞问题）。
   - **发信权限过滤**：若 `needs_answer === true`，所有 `can_answer !== true`（如只读子会话）**直接从候选列表中剔除**（彻底消除 `cyanea-found1` 消息误入只读子代理导致死锁问题）。
   - **发送方偏好**：`execution: new` 只是偏好，需要回答时改投有发信权的落点并记录改判；`plan.context` 是绑定，指向已丢失上下文时返回 `context_lost`。

---

### 5.2 节点 I-1: 路由目标选择（Execution Routing Target）
- **问题 ID**：`routing_target`（$\le 5$ 选 1）
- **候选集合与准则**：
  1. `main_agent`: 由本节点主会话承载处理（默认首选）。
  2. 针对每一个合法的活跃子上下文（`alias`）：
     - 标签格式：`${alias} (${summary}, state: ${state}, agent: ${agent_id})`。
  3. `new_subagent`: 开启全新子会话（**硬约束**：仅当 `needs_answer === false` 且消息带来完整独立背景时允许出现）。
- **映射动作**：
  - 若选具体上下文 `alias` -> 生成 `{ action: "context", context: alias }`，投递至子代理。
  - 若选 `new_subagent` -> 生成 `{ action: "new_subagent" }`，宿主启动新子代理。
  - 若选 `main_agent` -> **进入节点 I-2，进一步判定主会话插入模式**。

---

### 5.3 节点 I-2: 主会话 OMP 插入模式（Main Context Insertion Mode）
当消息确定进入主会话时，系统利用 OMP 强大的消息介入能力，判定其插入时机与打断深度：

- **问题 ID**：`insertion_mode`（3 选 1）
- **选项定义与判据**：
  1. `steer`: **即时插话（Turn Interruption / Steer）**
     - **OMP 行为**：`session.sendMessage(payload, { deliverAs: "steer" })`。在 Agent 当前这一步工具调用结束后**立即插话**，模型在下一思考步立刻感知并调整后续行动。
     - **适用场景**：紧急中止、方向纠偏、关键错误中断、参数变更。例如：“停下，接口换了”、“不要改那个文件，改这边的”、“对方节点报错拒绝了，改方案”。
  2. `follow_up`: **事后追加（Turn Completion / Follow-up）**
     - **OMP 行为**：`session.sendMessage(payload, { deliverAs: "followUp" })` 或空闲时 `{ triggerTurn: true }`。不打扰当前正在进行的连续工具链，等 Agent 本轮工作彻底完成（Turn 结束、即将 Yield）时，再作为后续消息追加并自动启动新回合。
     - **适用场景**：正常的外部问候回复、异步任务执行结果到达、协作成果交付、常规询问。Agent 专心做完手头操作后自然接单处理。
  3. `fragment`: **背景注入（Silent Fragment）**
     - **OMP 行为**：`session.sendMessage(payload, { deliverAs: "nextTurn" })`。不触发任何回合唤醒，仅作为静默背景条目记入上下文。
     - **适用场景**：低优先级的状态广播、非关键日志旁路、纯只读环境事实同步。

---

## 6. Todo 管线深度技术规范（Todo Deep Dive）

### 6.1 节点 T-1: 待办动作分类（Todo Action）
- **问题 ID**：`action`（3 选 1）
- **选项与判据**：
  - `append`: 消息带来了全新的、需要持续跟踪处理的独立外部工作项。
  - `associate`: 消息属于当前已有某个开放待办项的进展补充或交付成果。
  - `none`: 仅为信息通知、问候或一次性应答，不对待办清单做变更。

### 6.2 节点 T-2: 候选提取与关联（Todo Association）
- **当 `action === "append"` 时**：
  - 从消息提取 1~3 个规范的标题候选（`candidate_titles`）与说明候选（`candidate_explanations`），由模型选定最佳摘要。
- **当 `action === "associate"` 时**：
  - 列出当前状态为 open 或 in_progress 的待办项列表，由模型选定关联合适的条目。

---

## 7. 真实场景模拟与验证

### 场景 1：方向纠偏消息（Steer 验证）
- **现场**：主 Agent 正在执行一个多步重构（连续发起 5 个工具调用，当前刚执行到第 2 个）。此时外部协作者发来消息：“停下，刚刚发现 upstream 仓库分支重命名了，改用 main 分支！”
- **新流程判定**：
  - Pre-filter: 识别到紧急正文线索（“停下”）。
  - Node I-1: 判定 `routing_target: "main_agent"`。
  - Node I-2: 判定 `insertion_mode: "steer"`。
  - **宿主行为**：在第 2 个工具调用返回后，OMP 调度器立即注入该消息，Agent 在第 3 步思考前看到了纠偏信号，立刻中断后续无用工具调用，改向 main 分支，**零无用消耗**！

### 场景 2：常规任务成果到达（Follow-up 验证）
- **现场**：主 Agent 正在编写本地测试，外部协作者发来回复：“这是你要的接口签名定义：`type Spec = ...`，期待你的回复。”
- **新流程判定**：
  - Pre-filter: 识别到回复义务（`needs_reply = true`）。
  - Node I-1: 排除所有只读子代理，选定 `main_agent`。
  - Node I-2: 判定 `insertion_mode: "follow_up"`。
  - **宿主行为**：主 Agent 平稳完成手头的测试用例编写，整轮回合结束后，OMP 自动送入该 follow-up 消息并开启新一轮回合进行回复，**既不打断当前思路，又不漏掉外部消息**！

---

## 8. 交付结论

本规范：
1. **全链路原子化拆解为 11 个独立决策节点**，单步状态数严格 $\le 6$（至多 10 种）；
2. **制定精确到字段级别的上下文组合矩阵**，杜绝无关字段污染模型注意力，贯彻黄金三段式布局；
3. **建立确定性代码预剪枝机制**，把物理与权限不变量由代码严格把守，消灭反向提示词与不可行选项；
4. **将 OMP 的 `steer`（即时插话）与 `followUp`（事后追加）深度融入入站决策**，赋予系统精确区分“紧急纠偏”与“正常协作”的执行能力；
5. **历史事故全数闭环规避**，为 Agent 带来极致可靠、自然顺畅的跨节点交互体验。
