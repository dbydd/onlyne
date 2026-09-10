# Goal

重构 onlyne-swarm 的 Pi session 生命周期与调度器边界，建立独立于 Orca 的通用 session runtime，接入 Herdr、Zellij、Orca 的能力模型；修正 swarm 与 pi-onlyne 的启动握手、状态同步、异步 intent、workspace bootstrap 和 FIFO 策略；形成可自愈、可收养、可人工修复的 task/session 闭环。

详细设计已持久化于：

- `docs/SWARM-REFACTOR-GRILLME.md`

## Scope

- 新增纯 `lifecycle` reducer：公共投影 `created / working / idle / exited`，正交 agent、delivery、resource 状态，完整事件矩阵。
- 新增 `SessionBackend` runtime 层。`SWARM_RUNTIME=auto` 时按 `herdr → zellij → orca` 自动探测；默认保持 `orca`，也可显式指定 backend。选定 backend 后运行期固定。backend 管理错误触发 scheduler 受控退出。
- 使用 `task_id == session_id` 和 Pi `--session-id` 建立逻辑 session 身份。backend ref 使用 opaque JSON 持久化。
- Pi 与 pi-onlyne 提供 session lifecycle 事实。Pi heartbeat 每 10 秒，core 事件即时归约，每 30 秒全量 reconcile，启动时全量 reconcile。
- 使用 `(generation, seq)` 处理事件乱序、重放和重复。Pi 每次 session start 递增 generation。
- `idle_waiting`、`idle_fault` 均为恢复态。带 task 的 idle session 保留原 context 绑定。普通 ready pool 处理无 task session。
- `swarm_complete`、`swarm_send`、recycle、fault report 使用持久化 intent。默认 3 次重试和 1/2/4 秒退避。完成 receipt 到达后异步退出。耗尽 intent 进入 supervisor fault queue。
- 普通 task 故障创建单层 recovery task，优先发往直接 parent role，根 supervisor 作为 fallback。recovery task 故障进入 root fault queue。兄弟 task 继续运行。
- swarm task delivery、downstream send、recycle/control 使用 Onlyne daemon `loopback` RPC。completion 使用 `send_message`。swarm 路径不打开 FIFO。普通 Onlyne 模式保留 FIFO。
- workspace 首次创建时 materialize `.onlyne`、`.pi/onlyne.json`、`.pi/settings.json`，复制并重写 pi-onlyne package 路径，启用 watcher。后续 sync 只验证和报告 drift，保留 supervisor 修改。
- supervisor 可直接维护 `tasks`、`sessions`、`intents`、`faults` SQLite 表。CLI repair 提供事务封装。支持 inspect、adopt、rebind、retry、fail、close、ack。
- 协议使用 `protocol: 2`。错误 swarm wire 进入 protocol fault。普通文本保持普通消息路径。core invariant 错误沿受控退出路径暴露。
- TUI 重构留到后续阶段。

## Engineering principles

- 遵循 Unix/KISS。组件保持小、可组合、高可用和高内聚。
- 组件负责自身边界内的输入、状态和资源。
- 边界外的错误输入沿可控崩溃路径暴露。监督层负责重启、重试和人工修复。
- 优先显式失败、进程监督、结构化状态和清晰诊断。
- 避免静默恢复、宽泛兜底、隐式跨层耦合和大型通用框架。
- 配置、状态、session 绑定和 intent 先持久化，再发送确认。
- 状态观测与期望状态持续 reconcile。实际 idle、fault、gone、exited 必须在 core 视图中收敛。
- 恢复证据不足时终止受影响 work，记录 fault，沿 lineage 创建 recovery task。
- 单个 work 的故障保持局部。其他 role 和 task 继续调度。

## Out of scope

- TUI 重构。
- agent runtime、模型适配、prompt orchestration、web UI、cron。
- 非本地 channel 的 swarm 业务语义。
- 全局任务完成定义、预算熔断和远程监督服务。

## Done criteria

- `onlyne-swarm` scheduler 通过 runtime trait 工作，核心代码无直接 Orca terminal/tab 调用。
- Herdr、Zellij、Orca 的能力边界有明确 adapter contract，首个可用 backend 完成真实路径。
- lifecycle reducer 通过 Agent×Delivery×Resource 与事件全集测试。
- Pi session JSONL、swarm SQLite、backend ref、task lineage 的身份和版本保持一致。
- ready barrier 等待 daemon subscription ack、Pi extension 初始化和 Pi idle input 状态。
- `turn_started` 成为 working 证据。core 周期 reconcile 实际 session 状态。
- idle、draining、fault、recovery、replay、cancel、adopt、duplicate generation 都有确定状态和持久化记录。
- swarm task/control/send 路径不打开 FIFO。loopback RPC 具备幂等键和重复请求 receipt。
- 新 workspace 生成可直接运行的 pi-onlyne 配置。后续 sync 的 drift 行为可诊断。
- core 重启能够扫描完整 loopback history，收养可证明的 session，隔离和回传无法恢复的 work。
- supervisor 能通过 SQLite 表和 repair CLI 完成现场修复。
- 状态机、崩溃恢复、bootstrap、transport、intent、recovery、端到端测试通过。

## Implementation order

1. lifecycle reducer、状态类型、迁移 schema、全表测试。
2. SessionBackend、opaque ref、capabilities、fake backend。
3. Herdr backend。
4. Onlyne loopback RPC 幂等、history 全量扫描、swarm FIFO disable。
5. pi-onlyne custom entries、generation/seq、ready barrier、heartbeat、snapshot、intent worker。
6. Orca adapter 迁移和直接调用删除。
7. scheduler reconcile、adopt、recovery、fault、repair CLI。
8. workspace bootstrap 和 package path rewrite。
9. protocol v2 和旧数据一次迁移。
10. fake/Herdr/Orca 端到端验证。
