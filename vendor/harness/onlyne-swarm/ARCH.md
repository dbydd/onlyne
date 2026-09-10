# 调度器架构（ARCH，修订一 2026-09-06）

Rust 实现的独立 daemon。`onlyne-swarm run` 在 swarm root（启动时 cwd）前台运行，
`attach` 连接已运行实例。子目录误启动按祖先标记（`.ws` 或
`.onlyne/swarm.db`）拒绝，`--force` 越过。

射后不理：任务 = session = 一跳。out 即 done，退出即回收。无等待、无回调、
无记账、无写锁。血缘只走 `transfer_send_to`，用途是家族视图与 cancel 级联。

## 1. 模块

```text
src/main.rs        CLI 入口（run/attach/submit/cancel/list/status/tui/workspace/init/export-skill/shell-completions）
src/root.rs        root 定位与祖先标记检查、树枚举
src/template.rs    jsonc 解析、深合并、back_edges 归一化
src/sync.rs        .schedule -> .ws 生成、onlyne_in 软链维护
src/db.rs          swarm.db（rusqlite）：tasks + ledger 列；ledger.jsonl 镜像
src/proto.rs       swarm 正文头解析与构造（PROTOCOL.md）
src/daemon.rs      各 workspace onlyne daemon 拉起/保活/回收
src/events.rs      订阅各 run/s（priority 最高级），路由 in/out 事件
src/sched.rs       任务状态机：pending 调度、ready 匹配、投递、out 记账、回收
src/orca.rs        swarm.sock 单请求客户端
src/orca_term.rs   orca terminal create/close/send 封装（JSON 输出解析）
src/ipc.rs         swarm.sock 服务：status/list/submit/cancel/toggle_swarm/subscribe
src/tui.rs         ratatui 监控面板（swarm.sock 订阅客户端）
src/init.rs        init：Onlyne 存量转换 + 全量 starter 生成
src/skill.rs       export-skill：supervisor 视角 SKILL.md
```

依赖建议：tokio、clap、serde/serde_json、rusqlite、ratatui、uuid、json5（jsonc 解析）、
tracing。Unix socket 行分隔 JSON 与 onlyne 主仓库同构。

## 2. 启动序列（run）

1. root 检查（祖先标记拒绝 / `--force` 越过）。
2. `sync` 一次：生成目录、实例快照、onlyne_in 软链；back_edges 缺失目标直接报错退出。
3. 打开（或创建）`.onlyne/swarm.db`。旧等待模型库直接废弃重建，不迁移。
4. 拉起树内全部 workspace 的 onlyne daemon（统一保活；崩溃重启；关闭时统一回收）。
5. 以最高 priority 订阅各 daemon `subscribe_events`；启动 swarm.sock 监听。
6. 主循环：in 任务事件 → 建 task（UUID）→ 建 terminal（orca）→ 等 ready →
   写 loopback/in → out 事件 → 记 done + ledger → 回收 terminal。

## 3. 任务状态机

```text
pending → running → done（out 已写）→ closed（terminal 已回收）
   │          ├→ failed（早停，无 out）→ closed
   └──────────┴→ cancelled（族终结）→ closed
```

- `pending`：任务已建档，无可调度 terminal（等待 orca create 或 ready）。
- `running`：已投递 loopback/in，terminal 存活。
- `done`：收到含本 `task_id` 的 out 事件。记台账，杀 terminal，`closed`。
- `failed`：session 早停（无 out）。记台账，不写任何回调，不重放。
- `cancelled`：cancel 按 `transfer_send_to` 血缘树取子族，逐个记台账。
- `closed`：资源回收完成。退出即 closed 是关闭条件本身，无额外闸门。
- 台账：root `.onlyne/ledger.jsonl` append-only，`list` / TUI 读同一数据源。
- 调度器重启：running 任务无 terminal 存活 → failed 台账行，任务不重放。

## 4. ready 池（瞬态）

- `swarm_ready` 载荷：`{workspace, terminal_handle, ts}`。句柄只做展示与 kill 用，
  路由按 `workspace` 路径匹配 pending 任务（同路径亲和）。
- ready 到达时若有同路径 pending：直接投递（不建新 terminal）。
- ready 到达时无 pending：入瞬态池，默认 60s 无人认领即回收（杀 pi 进程）。
- 建 terminal 后默认 120s 无 ready：杀 terminal，任务回 pending，TUI 计数告警。

## 5. onlyne 本体改动（唯一一处）

`subscribe_events` 订阅者携带 `priority: u32`（缺省 0 保持兼容）。
daemon 按 priority 从高到低投递事件；任一订阅者返回 `{"consumed": true}` 即停止
向更低优先级传递。调度器用 `u32::MAX`，TUI 调试订阅用 `u32::MAX - 1`。
仅含 swarm 头的消息进入优先级链；无头消息走原有广播路径。
