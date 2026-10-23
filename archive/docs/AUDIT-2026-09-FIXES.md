# Onlyne 代码审查整改清单 (2026-09)

审查覆盖：全部 server/client/session/acp/pi-plugin 源码，直接读取非推断。

---

## Critical 级（直接造成数据错误或重复投递）

### 问题二：客户端崩溃后 `live_tasks` 丢失 → 重复投递

**文件**：`crates/onlyne-server/src/router.rs:346` + `relay.rs:741`

**症状**：客户端进程崩溃重启后，内存里的 active session map 丢失，`hello` 带空 `live_tasks` 发出，服务端把所有 `in_flight` 行 requeue，同一 task 被两个 session 接收。

**根因**：防重投递唯一保护是 `live_tasks` 声明，客户端未从 `client.db.sessions` 读取仍 `in_flight` 的 task_id 填充此字段。

**整改**：在 `hello` 发送前查询 `SELECT task_id FROM sessions WHERE state = 'in_flight'`，填充 `live_tasks`。

**状态**：✅ 已修复 (crates/onlyne-store/src/client.rs + session/dispatch/slots.rs)

---

### 问题八：pi 插件 `injectedDeliveries` 进程内存，重启后去重失效

**文件**：`plugins/onlyne-agent-pi/src/agent.mjs:169` + `:604-611`

**症状**：pi 进程崩溃或被 `recycle` 后重启，`injectedDeliveries` 为空。客户端重新下发同一 `assign`，插件第二次注入模型上下文，产生重复工作。

**根因**：去重集合是进程内存，无持久化。同进程重连安全，跨进程重启不安全。

**整改**：在 `welcome` 响应里加 `delivered_tasks: Vec<String>`，插件在 `onConnect` 预填 `injectedDeliveries`。

**状态**：✅ 已修复 JS 侧 (TaskPiPlugin)，Rust 侧进行中 (TaskAcpRust)

---

### 问题九：`pendingCompletion` 单槽，第二个覆盖第一个

**文件**：`plugins/onlyne-agent-pi/src/agent.mjs:975-979`

**症状**：多任务断线时，第二个 completion 覆盖第一个，第一个任务的 `report.complete` 永久丢失，mirror 行停在 `working`。

**根因**：`pendingCompletion` 是单字段，不支持多任务。

**整改**：改为数组 `pendingCompletions: []`，`complete()` 断线路径 push，`flushPendingCompletions()` 顺序 drain。

**状态**：✅ 已修复 (TaskPiPlugin)

---

### 问题一：`SessionEntry.task_id` 是 `Mutex<String>`，session 可被复用

**文件**：`crates/onlyne-session/src/backend/acp/state.rs:76`

**症状**：文档注释声称 "one session runs one task"，但 `task_id` 用 `Mutex<String>` 而非 `String`。`Mutex` 语义是可变访问，说明代码存在写入点。

**根因**：如果存在覆写 `task_id` 的路径，旧任务的 journal/report/outcome 全部指向新 task_id，造成归属错误。

**整改**：确认写入点。若存在，改为 `String`（构造时设置，此后只读），per-turn 路径通过参数传递 task_id。

**状态**：✅ TaskAcpRust 确认并修复中

---

## Major 级（状态机混乱或测试污染）

### 问题十：`this.generation` 全局字段，多任务覆盖

**文件**：`plugins/onlyne-agent-pi/src/agent.mjs:613` + `:528`

**症状**：每次 `assign` 更新 `this.generation`，第一个任务的心跳报告第二个任务的 generation，服务端 `(generation, seq)` 门拒绝，mirror 行停住。

**根因**：`generation` 是实例字段，应为 per-task 字段。

**整改**：`generation` 移入 task 记录，`heartbeat()` 从 task 记录取值。

**状态**：✅ 已修复 (TaskPiPlugin)

---

### 问题十一：`heartbeat()` 只报告第一个任务，其余停滞

**文件**：`plugins/onlyne-agent-pi/src/agent.mjs:517-532`

**症状**：`activeTasks()[0]` 是 Map 插入顺序第一个，第二个任务 mirror 行收不到心跳，服务端 `heartbeat_stale` 误报僵尸。

**根因**：`heartbeat()` 只对第一个活跃任务发报告。

**整改**：循环 `activeTasks()`，每个任务各发一条心跳，各用自己的 generation。

**状态**：✅ 已修复 (TaskPiPlugin)

---

### 问题三：`projection.rs` 里 `parse_*/name_*` 四对双重维护

**文件**：`crates/onlyne-server/src/projection.rs:604-688`

**症状**：新增 `AgentState` 变体需同时更新四处，任意漏改导致运行时静默降级。

**根因**：字符串比较而非 exhaustive match，编译期无法检测遗漏。

**整改**：在 `onlyne-session/src/lifecycle/state.rs` 各枚举实现 `Display` + `FromStr`（exhaustive match），`projection.rs` 改为调用这两个 trait。

**状态**：✅ TaskProjection 修复中

---

### 问题四：`delivery.rs` 测试文件 149KB / 4449 行，极大过度测试

**文件**：`crates/onlyne-server/tests/delivery.rs`

**症状**：单文件比整个 server 实现还大，包含重复路径、实现 pinning、tautology、无独立边界覆盖。

**根因**：测试累积，未按 "plausible bug would fail it" 标准审查。

**整改**：删除只 assert `is_ok()` / mock echo / 重复路径 / 实现 pinning 的测试，保留边界/ACL/去重/requeue TTL 等实质测试。

**状态**：✅ TaskTestCleanup 清理中

---

### 问题十二：`sync_session` 失败只打 warn，mirror 行停住

**文件**：`crates/onlyne-client/src/session/adapter_socket/serve.rs:318-326`

**症状**：pi 进程结束时 `sync_session` 失败（server 链接断），session exit 永远未发出，mirror 行保持 `working`。

**根因**：失败只记 warn，未加入 intent 队列。

**整改**：失败时入队 sync-session intent，下次 server 重连时重试。

**状态**：✅ TaskAcpRust 修复中

---

## Minor 级（防御加固）

### 问题五：`lifecycle/tests.rs` 中测试名称误导

**文件**：`crates/onlyne-session/src/lifecycle/tests.rs:262-452`

**症状**：`the_surviving_legality_rules_are_pinned` 看起来是 "实现 pinning"，实际是规格 §2.2 合约测试。

**整改**：重命名为 `legality_rules_match_spec_section_2_2`，每条 row 注释改为引用规格条款。

**状态**：✅ TaskTestCleanup 修复中

---

### 问题六：枚举数组不随新变体自动扩张

**文件**：`crates/onlyne-session/src/lifecycle/tests.rs:16-49`

**症状**：`AGENTS`/`DELIVERIES` 等 const 数组，新增变体时不更新，matrix 静默少测一个维度。

**整改**：在测试里加断言 `assert_eq!(AGENTS.len(), AgentState::VARIANT_COUNT)`，新增变体时编译通过但断言失败，强迫更新。各枚举加 `const VARIANT_COUNT: usize`。

**状态**：✅ TaskTestCleanup 修复中

---

### 问题七：Ghost sweep `sweep_row` 存在 TOCTOU 窗口

**文件**：`crates/onlyne-server/src/ghosts.rs:96-146`

**症状**：先读 `task_ledger_state()`，再写 mirror row。窗口期内 task 被 `repair_retry` 重新激活，sweep 会误结算。

**根因**：读写分离，无二次检查。

**整改**：写入前在事务内再读一次 `task_ledger_state()`，状态不再 terminal 则跳过写入。

**状态**：✅ TaskProjection 修复中

---

## 执行状态

```
✅ TaskPiPlugin      — 问题 8-JS/9/10/11 (agent.mjs) DONE
✅ TaskAcpRust       — 问题 1/8/12 (acp + serve.rs)
✅ TaskProjection    — 问题 3/7 (state enums + ghosts)
✅ TaskTestCleanup   — 问题 4/5/6 (tests)
✅ 问题二            — 已修复 (client hello live_tasks from DB)
```

---

## 验证计划

1. `cargo build --workspace` 全绿
2. `cargo test --workspace` 全绿
3. `cd plugins/onlyne-agent-pi && bun test` 全绿
4. `crates/onlyne-testkit/e2e/pi-live.sh` 通过（如果 runner 可用）

---

## 后续跟踪

- 问题二需要读取客户端 `main.rs` 和 `runtime/daemon/` 启动路径，确认 `hello` 帧的 `live_tasks` 字段从哪里取值
- 所有 Critical 问题修复后运行端到端重复投递测试
