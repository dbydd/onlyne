This file documents the pre-plan design discussion. `docs/v1-PLAN.md` is the settled specification. The file remains for provenance.

# Swarm Lifecycle Refactor Grillme Implementation Rules

## 1. Goals and Boundaries

Onlyne-swarm manages workspace-local tasks, Pi logical sessions, message delivery, state convergence, and failure closure.

A Pi logical session uses `task_id == session_id`. Its native launch arguments use `pi --session-id <task_id>`. The backend stores an opaque ref for process, pane, tab, terminal, and other hosting resources.

The upper workspace backend owns the Pi process and layout resources. The swarm core owns task/session bindings, desired state, observed state, intents, faults, and lineage. Pi and pi-onlyne provide session lifecycle facts. When a Pi snapshot is unreachable, the backend supplies a resource-liveness probe.

Orca, Herdr, and Zellij connect through the narrow `SessionBackend` interface. Core scheduling reads capabilities and session refs. Core logic does not read backend-private concepts such as tab, focus, or pane trees.

TUI refactoring remains a later phase. The TUI only reads persisted state and events.

## 2. Locked Decisions

### 2.1 Engineering Principles

- Follow Unix/KISS. Keep components small, composable, available, and cohesive.
- Each component owns inputs, state, and resources within its boundary.
- Invalid input outside the boundary is exposed through a controlled crash path. The supervisor owns restarts, retries, and manual repair.
- Breached core invariants and backend management errors trigger a controlled scheduler exit.
- Protocol errors, exhausted delivery, or failed recovery for one task/session end the current work, generate a fault and recovery task, and let sibling tasks continue.
- Process state, message state, resource state, and task outcome each have a clear owner.
- Persist every recoverable transition before acknowledging it externally.

### 2.2 Lifecycle

Public lifecycle projections: `created`, `working`, `idle`, `exited`.

Persisted facts are layered as follows:

- task outcome: `pending`, `done`, `failed`, `cancelled`.
- agent state: `booting`, `ready`, `running`, `idle`, `gone`.
- delivery state: `none`, `pending`, `retrying`, `accepted`, `exhausted`.
- resource state: `detached`, `attached`, `closing`, `closed`.
- recovery substate: `idle_waiting`, `idle_fault`, `draining`.

`idle_waiting` means an active task reaches the end of an agent turn without a completion exit. pi-onlyne sends one reinforcement prompt. The next agent turn returns to `working`.

`idle_fault` means heartbeat, snapshot, generation, resource, or delivery facts disagree. The state returns to `working` after the supervisor or backend adoption provides evidence. Insufficient evidence starts termination and recovery.

`draining` means the agent turn has ended while completion or another intent is being sent asynchronously. The public projection remains `working`, then becomes `exited` after receipt.

The normal idle ready pool contains only clean sessions without task bindings. An idle session with a task always retains its original task/context binding.

### 2.3 State Sources and Synchronization

- Pi and pi-onlyne are the sources of session lifecycle facts.
- pi-onlyne sends `swarm_heartbeat` every 10 seconds.
- The core reduces lifecycle events immediately.
- The core performs a full reconcile of active sessions every 30 seconds.
- At startup, the core performs one full reconcile and scans workspace history.
- When a snapshot is unreachable, it calls the backend liveness probe.
- Every session event carries `(generation, seq)`.
- Older generations and sequences are dropped directly and recorded diagnostically.
- Events with the same version are handled idempotently.
- A newer event is reduced and persisted atomically.
- Reconcile uses `isolate_after` and `terminate_after`. Defaults are `1` and `3`; the inspection interval defaults to 30 seconds.
- The `m`th consecutive mismatch enters `idle_fault`.
- The `n`th consecutive mismatch terminates or releases the work, writes a fault, and creates a recovery task.
- A probe timeout for one session is recorded independently. One session cannot block inspection of others.

### 2.4 Intent

`swarm_send`, `swarm_complete`, recycle, and fault report use persisted intents.

Each intent retries independently. The default maximum is 3 attempts with backoff of 1, 2, and 4 seconds. An exhausted intent enters the supervisor fault queue.

A session may exit after the `swarm_complete` receipt arrives. An unfinished independent send intent enters the supervisor queue and produces a fault. Completion does not wait for other send intents.

The Pi session JSONL custom entry stores task identity, generation, seq, lifecycle snapshot, pending intents, attempts, receipts, and last errors. Session restore reads custom entries and restores the slot and intent worker.

### 2.5 Recovery and Faults

Failure to recover a normal task creates one recovery task. The recovery task records `failure_of`. It prefers the direct parent role as its target; if the parent role is missing or unschedulable, it uses the root supervisor.

Failure of a recovery task itself is written to the root fault queue. The system does not automatically create a second-level recovery task.

A fault payload stores task, session, generation, seq, desired, observed, intent, attempt, a backend ref summary, and the error reason.

Sibling tasks remain independent. Termination, retry, rebind, or ack of a faulted task does not change sibling task state.

The supervisor may maintain SQLite tables directly. A CLI repair command provides transaction wrappers. Table structure, field meaning, state invariants, and repair order form a stable management contract.

## 3. Lifecycle Middle Layer

### 3.1 File Layout

```text
harness/onlyne-swarm/src/
  lifecycle.rs          # pure state enums, event enums, reducer, projections, table-driven tests
  runtime/mod.rs        # SessionBackend trait, opaque ref, capabilities
  runtime/herdr.rs      # Herdr backend
  runtime/zellij.rs     # Zellij backend
  runtime/orca.rs       # Orca backend
  runtime/fake.rs       # test backend; may live in the tests module
  sched.rs              # task graph, dispatch, reconcile, recovery
  db.rs                 # tasks, sessions, intents, faults persistence
  events.rs             # Onlyne event subscription, history scan, event reduction
```

Direct calls in the existing `orca_term.rs` migrate incrementally to `runtime/orca.rs`. After migration, the scheduler does not directly reference the Orca CLI.

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

`SessionRef` fields: `task_id`, `backend`, `backend_ref`, `created_generation`.

`backend_ref` uses a JSON opaque descriptor. It contains the Orca terminal handle, Herdr agent/pane ref, and Zellij session/pane ref.

`Capabilities` describes only capabilities such as `spawn`, `attach`, `probe`, `close`, `focus`, and `rename`. `focus` and `rename` are optional UI capabilities. The core state machine depends on spawn, attach, probe, and close.

Backend auto-detection priority: `herdr`, `zellij`, `orca`. The first available backend satisfying the core capabilities remains fixed for the scheduler lifetime. Backend probe failure, spawn management-interface failure, or attach management-interface failure triggers a controlled scheduler exit. Task-level Pi/session failures enter that task's recovery flow.

### 3.3 Exit and Adoption

Normal SIGTERM: stop new dispatch, send recycle intents to active sessions, await bounded acks, call backend close, write resource state, and exit the scheduler.

After a scheduler crash or machine restart, Pi/backend sessions preserve the scene. A new scheduler loads session refs, performs attach and Pi snapshot reconcile, and adopts them after proving the same task/context. It enters `idle_fault` when the backend ref cannot prove identity, the Pi snapshot is unreachable, or the context version mismatches.

If a second Pi process appears for the same task, reject the new generation while the known first generation remains alive. The new process enters `idle_fault` and exits in a controlled manner after a targeted recycle. The core retains the first generation. New-generation adoption is allowed after the old generation has disappeared.

## 4. Pi / pi-onlyne Protocol

### 4.1 Pi Ready Barrier

pi-onlyne startup order:

1. `session_start` completes extension binding and configuration loading.
2. Establish the Onlyne daemon socket.
3. Wait for confirmation of the `subscribe_events` response.
4. Restore task, generation, seq, and intents from session JSONL custom entries.
5. Use `ctx.isIdle()`, `ctx.hasPendingMessages()`, and queue state to confirm Pi is waiting for input.
6. Send `swarm_ready` with task, generation, seq, workspace, and backend handle.
7. Receive the scheduler's exact task loopback delivery.
8. Call `sendUserMessage(..., { deliverAs: "followUp" })`.
9. Observe `queue_update`, `message_start`, `turn_start`, or `agent_start`, then send `swarm_turn_started`.
10. After receiving `swarm_turn_started`, the core sets public lifecycle to `working`.

`swarm_ready` means Pi has completed the input-wait barrier. Daemon acceptance means the control plane received the event. `swarm_turn_started` means actual work has started.

The Pi API's `sendMessage` and `sendUserMessage` return void. pi-onlyne infers receipt from queue, turn, and agent lifecycle events. An enqueue intent first writes a custom entry. Without turn-start evidence, delivery remains pending/retrying.

### 4.2 Agent Turn

When an active task's agent turn ends after `agent_end` without a completion receipt, it enters `idle_waiting`. pi-onlyne sends one reinforcement prompt through the existing idle reminder mechanism.

The next `agent_start` sends `swarm_turn_started` and returns to `working`.

After a successful `swarm_complete` receipt, clear the active task slot, write the completion custom entry, and enter the draining/exited flow. If the completion receipt fails, retain the intent and retry in the background.

### 4.3 Heartbeat and Snapshot

Heartbeat body:

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

When a heartbeat is missing or a snapshot mismatches desired state, the core requests `swarm_snapshot` through the loopback control wire. Pi returns a complete snapshot. The backend probe handles resource-layer facts when the Pi snapshot is unreachable.

### 4.4 Protocol Version

The swarm wire uses `protocol: 2`. `---swarm-ctl` also carries the protocol. Missing, unknown, or structurally invalid swarm wire enters protocol fault. Ordinary text remains on the ordinary Onlyne message path. Invalid wire with a swarm prefix does not fall back to an ordinary task.

A protocol error associated with the current task terminates current work and creates a recovery task. A core invariant error triggers a controlled scheduler exit.

## 5. Onlyne Transport and FIFO

### 5.1 RPC Division

- Scheduler task delivery: the target workspace daemon's `loopback` op.
- pi-onlyne `swarm_send`: the target workspace daemon's `loopback` op.
- Scheduler recycle/control: the current workspace daemon's `loopback` op.
- pi-onlyne `swarm_complete`: the current workspace daemon's `send_message`, with channel set to loopback.
- Lifecycle reports: a dedicated state notification op published on the existing daemon event stream.
- History: the Onlyne store records inbound, outbound, and state notifications.

The `loopback` op must support `op_id`/idempotency key. A duplicate request returns the existing message receipt. A timed-out request may be retried. Repeated retries do not create duplicate tasks.

After a core restart, scan each workspace's loopback history, deduplicate by `task_id` and `op_id`, and recreate missing tasks. Historical scans use complete paginated results. The existing fixed 30/100-item window is upgraded to a complete scan interface.

### 5.2 FIFO Policy

A swarm workspace configures loopback transport as RPC. The daemon does not create swarm loopback `in/out` FIFO files. Ordinary Onlyne workspaces continue to use FIFO.

Remove the scheduler's `write_loopback_in`. Remove the `onlyne_in` write from pi-onlyne `swarm_send`. Remove FIFO writes from recycle control. Generated swarm workspaces do not depend on an `onlyne_in` symlink.

Sync reports historical `.onlyne_in` links as legacy artifacts. New workspaces do not create such links.

## 6. Workspace Bootstrap

### 6.1 Initial Creation

Creating a role workspace generates all of the following at once:

- `.onlyne/config.toml`: loopback RPC, swarm enabled, external adapters disabled.
- `.onlyne/swarm.workspace.jsonc`: effective snapshot.
- `.pi/onlyne.json`: `watch.autoStart = true` and swarm defaults.
- `.pi/settings.json`: inherit packages from root settings, preserve other packages, rewrite relative paths so every workspace points to the same `pi-onlyne` source.
- Runtime directories, logs, and state files.

Root settings must contain a resolvable `pi-onlyne` package entry. If it is missing, bootstrap fails fast and reports the source path. Bootstrap performs no network installation.

Package path rewriting uses workspace-relative paths. The resolved path must point to a directory or package containing `package.json` whose package name is `pi-onlyne`.

### 6.2 Later Sync

Later sync only validates configuration and readiness, reports drift, and preserves supervisor modifications. Sync does not automatically rewrite existing `.pi/settings.json`, `.pi/onlyne.json`, or `.onlyne/config.toml` content.

Run, submit, and session spawn fail fast when a readiness gap exists. Readiness gaps include at least the swarm flag, RPC transport, watch autoStart, pi-onlyne package, settings JSON validity, and Onlyne JSON validity.

## 7. SQLite Contract

### 7.1 tasks

Preserve the existing task lineage and outcome fields, and add:

- `kind`: `normal | recovery`.
- `failure_of`: failed task id, nullable.
- `protocol_version`.
- `operator_revision`.

`task_id` is unique. `transfer_send_to` identifies the task id that created this task. A recovery task uses an independent UUID.

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

### 7.5 Direct Repair Contract

The supervisor may execute transactional SQL directly. Core operations:

- adopt/rebind: write backend, backend ref, generation, desired/observed.
- retry: change intent/status or task outcome to a schedulable value and increment operator revision.
- fail: write outcome, fault, reason, and resource close.
- close: write resource closed and complete ledger.
- ack: write fault acknowledged_at.

Every core reconcile validates the tuple, operator revision, and version sequence. An illegal tuple enters controlled fault. SQLite corruption triggers core exit and preserves the original error.

## 8. State-Machine Tests

`lifecycle.rs` uses a pure reducer and table-driven exhaustive tests.

Coverage:

- Legal projections for Agent × Delivery × Resource combinations.
- created → ready → working.
- working → idle_waiting → working.
- working → idle_fault → working.
- working + completion intent → draining/working → exited.
- Intent retry, receipt, and exhaustion.
- Version ordering for heartbeat, snapshot, and probe.
- Old generation, old seq, duplicate events, and new events.
- Duplicate Pi generation rejection.
- Explicit exit, cancel, fault, and operator repair.
- Mismatch m/n configuration and count reset.
- Single-level recovery-task limit and root fault queue.
- Persistence order and idempotence for every failure path.

## 9. Implementation Order

1. `lifecycle.rs`: pure enums, events, reducer, projections, SQLite state types, and exhaustive table tests.
2. `runtime/mod.rs`: SessionBackend, SessionRef, ResourceProbe, capabilities, and fake backend.
3. Herdr backend: spawn, attach, probe, close, task-id naming, and Pi `--session-id`.
4. Onlyne daemon: loopback RPC idempotency, history full scan, and swarm FIFO disable.
5. pi-onlyne: session custom entries, generation/seq, ready barrier, heartbeat, snapshot, intent worker, and turn receipt.
6. Orca adapter: migrate existing `orca_term` capabilities and remove direct Orca calls from the scheduler.
7. Scheduler: dispatch, adopt, reconcile, recovery, fault, and repair CLI.
8. Bootstrap: settings package-path rewrite, initial materialization, and drift validation.
9. Protocol v2: wire parser, control wire, error classification, and one-time legacy-data migration.
10. End-to-end: fake backend, Herdr smoke, real loopback delivery, completion, recycle, cancel, and restart adoption.

## 10. Completion Conditions

- Scheduler code does not directly depend on Orca terminal/tab APIs.
- task/session bindings agree in the scheduler, Pi session JSONL, SQLite, and backend ref.
- The core view converges to Pi observed state after events and 30-second inspection.
- An active task does not remain projected as running while actually idle, faulted, gone, or exited.
- Swarm task/control/send paths do not open FIFO files.
- A new workspace can directly start a Pi session with pi-onlyne.
- Configuration drift has explicit diagnostics and fail-fast behavior.
- Intent, fault, recovery, and operator repair state can be serialized and recovered.
- Failure of one piece of work does not block a sibling role.
- Core invariant and backend management errors reach the supervisor through controlled exit paths.
- State-machine, recovery, bootstrap, and end-to-end tests all pass.

---

# 中文镜像：Swarm 生命周期重构 Grillme 实施细则

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

core 重启后扫描各 workspace loopback history，按 `task_id`、`op_id` 去重并补建缺失 task。历史扫描使用完整分页结果。现有固定 30/100 条窗口升级为完整扫描接口。

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
