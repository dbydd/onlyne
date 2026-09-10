# Swarm 生命周期重构 Grillme 实施细则

## 1. 目标与边界

Onlyne-swarm 管理 workspace-local 的任务、Pi logical session、消息交付、状态收敛和故障闭环。

Pi logical session 使用 `task_id == session_id`。Pi 原生启动参数使用 `pi --session-id <task_id>`。backend 保存进程、pane、tab、terminal 等承载资源的 opaque ref。

上层 workspace backend 持有 Pi 进程和布局资源。swarm core 持有 task/session 绑定、期望状态、观测状态、intent、故障和 lineage。Pi 与 pi-onlyne 提供 session 生命周期事实。backend 在 Pi 快照不可达时提供资源存活探针。

Orca、Herdr、Zellij 通过窄的 `SessionBackend` 接口接入。核心调度逻辑读取能力和 session ref。核心逻辑不读取 tab、focus、pane 树等 backend 私有概念。

TUI 重构留在后续阶段。TUI 读取持久化状态和事件即可。

## 2. 已锁定决策

### 2.1 工程原则

- 遵循 Unix/KISS。组件保持小、可组合、高可用和高内聚。
- 组件负责自身边界内的输入、状态和资源。
- 边界外的错误输入沿可控崩溃路径暴露。监督层负责重启、重试和人工修复。
- core invariant 破坏和 backend 管理错误触发 scheduler 受控退出。
- 单个 task/session 的协议错误、投递耗尽和恢复失败终止当前 work，生成 fault 和 recovery task。兄弟 task 继续运行。
- 进程状态、消息状态、资源状态、任务结果各自有明确 owner。
- 每个可恢复转换先持久化，再对外确认。

### 2.2 生命周期

公共生命周期投影：`created`、`working`、`idle`、`exited`。

持久化事实分层：

- task outcome：`pending`、`done`、`failed`、`cancelled`。
- agent state：`booting`、`ready`、`running`、`idle`、`gone`。
- delivery state：`none`、`pending`、`retrying`、`accepted`、`exhausted`。
- resource state：`detached`、`attached`、`closing`、`closed`。
- recovery substate：`idle_waiting`、`idle_fault`、`draining`。

`idle_waiting` 表示 active task 在 agent turn 结束时缺少 completion 出口。pi-onlyne 发一次强化提示。下一次 agent turn 启动后回到 `working`。

`idle_fault` 表示 heartbeat、snapshot、generation、resource 或 delivery 事实出现失配。supervisor 或 backend adoption 提供证据后回到 `working`。证据不足时进入终止和 recovery 流程。

`draining` 表示 agent turn 已结束，completion 或其他 intent 处于异步发送过程。公共投影保持 `working`。receipt 到达后进入 `exited`。

普通 idle ready pool 只保存无 task 绑定的干净 session。带 task 的 idle session 永远保持原 task/context 绑定。

### 2.3 状态事实源与同步

- Pi 与 pi-onlyne 是 session lifecycle 的事实源。
- pi-onlyne 每 10 秒发送 `swarm_heartbeat`。
- core 接收生命周期事件后立即归约。
- core 每 30 秒执行 active session 全量 reconcile。
- core 启动时执行一次全量 reconcile 和 workspace history 扫描。
- snapshot 不可达时调用 backend liveness probe。
- 每个 session 的事件带 `(generation, seq)`。
- 旧 generation、旧 seq 直接丢弃并记录诊断。
- 相同版本事件幂等处理。
- 新版本事件原子归约并持久化。
- reconcile 使用配置项 `isolate_after`、`terminate_after`。默认值为 `1`、`3`。巡检间隔默认 30 秒。
- 第 `m` 次连续失配进入 `idle_fault`。
- 第 `n` 次连续失配终止或释放 work，写入 fault，创建 recovery task。
- 单个 session 的探针超时独立记录。单个 session 不阻塞其他 session 的巡检。

### 2.4 Intent

`swarm_send`、`swarm_complete`、recycle、fault report 使用持久化 intent。

每个 intent 独立重试。默认最多 3 次，退避 1 秒、2 秒、4 秒。intent 耗尽进入 supervisor fault queue。

`swarm_complete` receipt 到达后允许 session 退出。未完成的独立 send intent 进入 supervisor queue 并产生 fault。completion 不等待其他 send intent。

Pi session JSONL custom entry 保存 task identity、generation、seq、lifecycle snapshot、pending intents、attempts、receipts、last errors。session restore 读取 custom entries 并恢复 slot 与 intent worker。

### 2.5 恢复与故障

普通 task 的恢复失败创建一个单层 recovery task。recovery task 记录 `failure_of`。目标 role 优先使用直接 parent role。parent role 缺失或不可调度时使用 root supervisor。

recovery task 自身失败写入 root fault queue。系统不自动创建第二层 recovery task。

故障 payload 保存 task、session、generation、seq、desired、observed、intent、attempt、backend ref 摘要和错误原因。

兄弟 task 保持独立。故障 task 的终止、重试、rebind、ack 不改变兄弟 task 状态。

supervisor 可直接维护 SQLite 表。CLI repair 命令提供事务封装。表结构、字段含义、状态不变量和修复顺序属于稳定管理契约。

## 3. 生命周期中间层

### 3.1 文件布局

```text
harness/onlyne-swarm/src/
  lifecycle.rs          # 纯状态 enum、事件 enum、reducer、投影和表驱动测试
  runtime/mod.rs        # SessionBackend trait、opaque ref、capabilities
  runtime/herdr.rs      # Herdr backend
  runtime/zellij.rs     # Zellij backend
  runtime/orca.rs       # Orca backend
  runtime/fake.rs       # 测试 backend，可放 tests 模块
  sched.rs              # task graph、dispatch、reconcile、recovery
  db.rs                 # tasks、sessions、intents、faults persistence
  events.rs             # Onlyne event subscription、history scan、事件归约
```

现有 `orca_term.rs` 的直接调用逐步迁移到 `runtime/orca.rs`。迁移完成后 scheduler 不直接引用 Orca CLI。

### 3.2 SessionBackend

```rust
trait SessionBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> Capabilities;
    fn available(&self) -> anyhow::Result<bool>;
    fn spawn(&self, spec: SpawnSpec) -> anyhow::Result<SessionRef>;
    fn attach(&self, session: &SessionRef) -> anyhow::Result<SessionRef>;
    fn probe(&self, session: &SessionRef) -> anyhow::Result<ResourceProbe>;
    fn close(&self, session: &SessionRef, reason: CloseReason, force: bool)
        -> anyhow::Result<()>;
}
```

`SessionRef` 字段：`task_id`、`backend`、`backend_ref`、`created_generation`。

`backend_ref` 使用 JSON opaque descriptor。Orca terminal handle、Herdr agent/pane ref、Zellij session/pane ref 都保存在 descriptor 内。

`Capabilities` 只描述 `spawn`、`attach`、`probe`、`close`、`focus`、`rename` 等能力。`focus` 和 `rename` 属于可选 UI 能力。核心状态机依赖 spawn、attach、probe、close。

backend 自动探测优先级：`herdr`、`zellij`、`orca`。第一个可用且满足核心能力的 backend 在 scheduler 生命周期内固定。backend 探测失败、spawn 管理接口失败、attach 管理接口失败触发 scheduler 受控退出。task 级 Pi/session 故障进入本 task recovery 流程。

### 3.3 退出与收养

正常 SIGTERM：停止新 dispatch，向 active sessions 发送 recycle intent，等待有限 ack，调用 backend close，写 resource state，退出 scheduler。

scheduler 崩溃或机器重启：Pi/backend session 保持现场。新 scheduler 启动时加载 session refs，执行 attach 和 Pi snapshot reconcile。证明同一 task/context 后收养。backend ref 无法证明、Pi snapshot 不可达、上下文版本失配时进入 `idle_fault`。

同一 task 出现第二个 Pi 进程：已知首个 generation 仍存活时拒绝新 generation。新进程进入 `idle_fault`，收到 targeted recycle 后受控退出。core 保留首个 generation。旧 generation 已消失时允许新 generation adoption。

## 4. Pi / pi-onlyne 协议

### 4.1 Pi ready barrier

pi-onlyne 启动顺序：

1. `session_start` 完成扩展绑定和配置加载。
2. 建立 Onlyne daemon socket。
3. 等待 `subscribe_events` 响应确认。
4. 从 session JSONL custom entries 恢复 task、generation、seq、intents。
5. 使用 `ctx.isIdle()`、`ctx.hasPendingMessages()` 和队列状态确认 Pi 等待输入。
6. 发送 `swarm_ready`，携带 task、generation、seq、workspace、backend handle。
7. 接收 scheduler 的 exact task loopback delivery。
8. 调用 `sendUserMessage(..., { deliverAs: "followUp" })`。
9. 观察 `queue_update`、`message_start`、`turn_start` 或 `agent_start`，发送 `swarm_turn_started`。
10. core 收到 `swarm_turn_started` 后将 public lifecycle 置为 `working`。

`swarm_ready` 代表 Pi 已完成输入等待屏障。daemon 接受 ready 代表控制面收到事件。`swarm_turn_started` 代表实际工作已开始。

Pi API 的 `sendMessage`、`sendUserMessage` 返回 void。pi-onlyne 使用 queue、turn 和 agent 生命周期事件推断 receipt。enqueue intent 先写 custom entry。缺少 turn-start 证据时保持 delivery pending/retrying。

### 4.2 Agent turn

active task 的 agent turn 在 `agent_end` 后没有 completion receipt 时进入 `idle_waiting`。pi-onlyne 发一次强化提示。提示沿现有 idle reminder 机制实现。

下一次 `agent_start` 发送 `swarm_turn_started` 并回到 `working`。

`swarm_complete` 成功 receipt 后清除 active task slot，写 completion custom entry，进入 draining/exited 流程。完成 receipt 失败时保留 intent，后台重试。

### 4.3 Heartbeat 与 Snapshot

heartbeat body：

```json
{
  "protocol": 2,
  "task_id": "...",
  "generation": 2,
  "seq": 41,
  "agent_state": "idle",
  "delivery_state": "retrying",
  "resource_state": "attached",
  "lifecycle": "working",
  "pending_intents": ["..."],
  "at": "..."
}
```

core 在 heartbeat 缺失或 snapshot 与 desired mismatch 时，通过 loopback control wire 请求 `swarm_snapshot`。Pi 返回完整 snapshot。backend probe 处理 Pi snapshot 不可达的资源层事实。

### 4.4 协议版本

swarm wire 使用 `protocol: 2`。`---swarm-ctl` 同样携带 protocol。缺失、未知或结构错误的 swarm wire 进入 protocol fault。普通文本保持普通 Onlyne 消息路径。带 swarm 前缀的错误 wire 不降级为普通任务。

协议错误关联当前 task 时终止当前 work并创建 recovery task。core invariant 错误触发 scheduler 受控退出。

## 5. Onlyne transport 与 FIFO

### 5.1 RPC 分工

- scheduler 投递 task：目标 workspace daemon 的 `loopback` op。
- pi-onlyne `swarm_send`：目标 workspace daemon 的 `loopback` op。
- scheduler recycle/control：当前 workspace daemon 的 `loopback` op。
- pi-onlyne `swarm_complete`：当前 workspace daemon 的 `send_message`，channel 为 loopback。
- lifecycle reports：专用 state notification op，沿现有 daemon event stream 发布。
- history：Onlyne store 记录 inbound、outbound 和 state notification。

`loopback` op 必须支持 `op_id`/idempotency key。重复请求返回已有 message receipt。请求超时允许重试。重复重试不会生成重复 task。

core 重启后扫描各 workspace loopback history，按 task_id、op_id 去重并补建缺失 task。历史扫描使用完整分页结果。现有固定 30/100 条窗口升级为完整扫描接口。

### 5.2 FIFO 策略

swarm workspace 的 loopback transport 设置为 RPC。daemon 不创建 swarm loopback `in/out` FIFO。普通 Onlyne workspace 继续使用 FIFO。

scheduler 删除 `write_loopback_in`。pi-onlyne `swarm_send` 删除 `onlyne_in` 写入。recycle control 删除 FIFO 写入。生成的 swarm workspace 不依赖 `onlyne_in` symlink。

历史 `.onlyne_in` 链接由 sync 作为 legacy artifact 报告。新 workspace 不生成此类链接。

## 6. Workspace bootstrap

### 6.1 首次创建

创建 role workspace 时一次性生成：

- `.onlyne/config.toml`：loopback RPC、swarm enabled、外部 adapters disabled。
- `.onlyne/swarm.workspace.jsonc`：effective snapshot。
- `.pi/onlyne.json`：`watch.autoStart = true` 和 swarm defaults。
- `.pi/settings.json`：从 root settings 继承 packages，保留其他 package，重写相对路径，使每个 workspace 指向同一 `pi-onlyne` source。
- runtime directories、logs、state files。

root settings 必须包含可解析的 `pi-onlyne` package entry。缺失 entry 时 bootstrap fail-fast，并给出 source path。bootstrap 不执行网络安装。

package path rewrite 使用 workspace 相对路径。路径解析结果必须指向包含 `package.json` 且 package name 为 `pi-onlyne` 的目录或包。

### 6.2 后续 sync

后续 sync 只验证配置和 readiness，报告 drift，保留 supervisor 修改。sync 不自动重写 `.pi/settings.json`、`.pi/onlyne.json`、`.onlyne/config.toml` 的现有内容。

run、submit、session spawn 在 readiness gap 存在时 fail-fast。readiness gap 至少包含：swarm flag、RPC transport、watch autoStart、pi-onlyne package、settings JSON 合法性、onlyne JSON 合法性。

## 7. SQLite contract

### 7.1 tasks

保留现有 task lineage 和 outcome 字段，新增：

- `kind`: `normal | recovery`。
- `failure_of`: failed task id，可空。
- `protocol_version`。
- `operator_revision`。

`task_id` 唯一。`transfer_send_to` 表示生成该 task 的 task id。recovery task 使用独立 UUID。

### 7.2 sessions

```text
sessions(
  task_id PRIMARY KEY,
  agent_state,
  delivery_state,
  resource_state,
  public_lifecycle,
  recovery_substate,
  desired_json,
  observed_json,
  generation,
  last_seq,
  heartbeat_at,
  mismatch_count,
  backend,
  backend_ref_json,
  last_error,
  created_at,
  updated_at,
  operator_revision
)
```

### 7.3 intents

```text
intents(
  op_id PRIMARY KEY,
  task_id,
  kind,
  payload_json,
  status,
  attempts,
  next_retry_at,
  last_error,
  accepted_receipt_json,
  created_at,
  updated_at
)
```

### 7.4 faults

```text
faults(
  fault_id PRIMARY KEY,
  task_id,
  failure_of,
  class,
  desired_json,
  observed_json,
  generation,
  seq,
  intent_id,
  attempts,
  reason,
  recovery_task_id,
  acknowledged_at,
  created_at
)
```

### 7.5 直接修复契约

supervisor 可直接执行事务性 SQL。核心操作：

- adopt/rebind：写 backend、backend ref、generation、desired/observed。
- retry：把 intent/status 或 task outcome 改为可调度值，递增 operator revision。
- fail：写 outcome、fault、reason、resource close。
- close：写 resource closed，完成 ledger。
- ack：写 fault acknowledged_at。

core 每次 reconcile 校验 tuple、operator revision 和版本序列。非法 tuple 进入 controlled fault。SQLite 损坏触发 core 退出并保留原始错误。

## 8. 状态机测试

`lifecycle.rs` 使用纯 reducer 和 table-driven exhaustive tests。

测试覆盖：

- Agent × Delivery × Resource 的合法组合投影。
- created → ready → working。
- working → idle_waiting → working。
- working → idle_fault → working。
- working + completion intent → draining/working → exited。
- intent retry、receipt、exhaustion。
- heartbeat、snapshot、probe 的版本排序。
- 旧 generation、旧 seq、重复事件、新事件。
- duplicate Pi generation 拒绝。
- explicit exit、cancel、fault、operator repair。
- mismatch m/n 配置与计数复位。
- recovery task 单层限制与 root fault queue。
- 每个失败路径的持久化顺序和幂等行为。

## 9. 实施顺序

1. `lifecycle.rs`：纯 enum、事件、reducer、投影、SQLite state types、全表测试。
2. `runtime/mod.rs`：SessionBackend、SessionRef、ResourceProbe、capabilities、fake backend。
3. Herdr backend：spawn、attach、probe、close、task id 命名和 Pi `--session-id`。
4. Onlyne daemon：loopback RPC idempotency、history full scan、swarm FIFO disable。
5. pi-onlyne：session custom entries、generation/seq、ready barrier、heartbeat、snapshot、intent worker、turn receipt。
6. Orca adapter：迁移现有 `orca_term` 能力，删除 scheduler 直接 Orca 调用。
7. scheduler：dispatch、adopt、reconcile、recovery、fault、repair CLI。
8. bootstrap：settings package path rewrite、首次 materialization、drift validation。
9. protocol v2：wire parser、control wire、错误分类、旧数据一次迁移。
10. end-to-end：fake backend、Herdr smoke、真实 loopback delivery、completion、recycle、cancel、restart adoption。

## 10. 完成条件

- scheduler 代码不直接依赖 Orca terminal/tab API。
- task/session 绑定在 scheduler、Pi session JSONL、SQLite、backend ref 中一致。
- core 视图在事件和 30 秒巡检后收敛到 Pi observed state。
- active task 不会长期以 running 投影覆盖实际 idle、fault、gone 或 exited。
- swarm task/control/send 路径不打开 FIFO。
- 新 workspace 可直接启动带 pi-onlyne 的 Pi session。
- 配置 drift 有明确诊断和 fail-fast 行为。
- intent、fault、recovery、operator repair 都可序列化和恢复。
- 单个 work 的失败不会阻塞兄弟 role。
- core invariant 与 backend 管理错误沿受控退出路径交给 supervisor。
- 状态机、恢复、bootstrap、端到端测试全部通过。
