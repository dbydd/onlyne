# onlyne-swarm SPEC（修订一已折回，冻结 2026-09-06 dringende）

Onlyne 的衍生项目：响应式多 agent 有向有环图工作流调度系统。
独立 git 仓库（`harness/onlyne-swarm`），Rust 实现，CLI 入口 `onlyne-swarm`。

细节规范见同目录：`PROTOCOL.md`（swarm 正文协议头）、`TEMPLATE.md`（描述文件与合并）、
`IPC.md`（swarm.sock）、`TUI.md`（监控面板）、`ARCH.md`（调度器架构）、`TEST.md`（验证场景）。

## 1. 核心模型

- 工作区 = agent（逻辑体）。一次入站消息 = 一个任务。
- 任务 = session = 一跳。二者同一 id：`task_id == session_id`，调度器生成 UUID v4。
- session 早停（无 out 写出即退出）= 本次调用失败：记 failed 台账行，回收 terminal，不重放任务，不写任何回调。
  重试是外部 pi 插件的职责，不是调度器的职责。
- session 原子执行，无任何例外：一个 session 从生到死只见这一个任务。
  工作流：恢复上下文 → 工作 → 需要接力就激发下游 → 交活退出。下游成果走文件与台账。
- agent 之间经 loopback channel 通信。通信粒度是 workspace 到 workspace，不存在 session 到 session 直连。
  发送方直写目标 workspace 的 in。调度器订阅各 workspace daemon 的 socket 事件流做路由。
- 环路不限制、不熔断。自激发无限循环是预期用途，由用户经 TUI / CLI 手动终结。
  TUI 可做计数告警，但告警不得干扰正常运行。
- 不设超时（给 agent 任务设 deadline 是反模式），不设并发上限，并发写风险忽略。

## 2. 目录布局（终版）

调度器以启动时 cwd 为根（`cwd 即根`），不向上搜索。误在子目录启动的防护：
祖先目录含 `.ws` 或 `.onlyne/swarm.db` 即拒绝启动，提示回根目录 attach，
`--force` 可显式越过。根本身也是 workspace，可做 supervisor 节点。

```text
<root>/                                   # swarm root = 调度器 cwd
  .agents/.schedule/<名>/template.workspace.jsonc
                 └─ <子名>/template.workspace.jsonc ...   # 文件夹嵌套反映真实嵌套
  .ws/<嵌套路径>/           # 调度器按描述单向生成的真实目录
    .onlyne/{config.toml,swarm.workspace.jsonc,channels/loopback/{in,out},run/s,logs/,state.db}
    onlyne_in/<目标树相对路径>/ -> symlink
  onlyne_in/<目标树相对路径>/ -> symlink   # root 自有的一份
  .onlyne/{config.toml,swarm.db,run/swarm.sock,channels/...}
```

- 以 `.schedule` 描述为唯一来源，其余目录一律视为当前工作区管理的项目文件。
  `run` 启动时自动 sync 一次生成缺失目录；多余的真实目录不删除，只在 TUI 告警。
- `onlyne_in/<目标>/` 是发送侧便捷视图：每个真实 workspace（含 root）各一份，
  `<目标>` 为目标 workspace 的树相对路径（如 `onlyne_in/a/b/`），是一个指向目标
  `.onlyne/channels/loopback/in` 文件的软链（绝对路径目标）。发送方写入该路径
  等同于直写目标 in。目标缺失时显示悬链，TUI 告警。相对当前 workspace 而言它是出口。
- 调度器生成的真实 workspace 内 `.onlyne/config.toml` 默认值：外部 adapters 全 disabled，
  只留 loopback 可用，`[swarm] enabled = true`，io 沿用 onlyne 默认。swarm 流量纯本地，
  外部通道由 root 显式开启。由调度器自动创建的工作区默认为 swarm 模式。

## 3. 描述文件与合并

- 单个 workspace 描述：`template.workspace.jsonc`，子目录名即 workspace 名。
  终版字段只保留四项：`name`、内联 `role`（系统提示词直接写进 jsonc，不再设 `.roles/`，
  不纠结 md 格式约定）、`model{provider,model,effort}`、`back_edges[]`。
- `back_edges` 为目标名数组，指向树内 workspace 路径，支持相对路径（`../reviewer`、
  `sibling/worker`、`../../a/b/c`，以该 workspace 在树中的位置为基准解析）。
  启动时校验目标存在，缺失即报错；环路允许存在，执行期靠 `task_id` 去重防无限重入。
- 深覆盖优先级：上层 template < 本层 template < 本层真实路径下
  `.ws/.../.onlyne/swarm.workspace.jsonc`（手写回边与覆盖，调度器永不重写）。
  自动生成的实例初始内容一定匹配上层各层合并后的结果。合并规则：对象递归合并，
  标量下层覆盖上层，`back_edges` 取并集去重，其余数组替换。

## 4. Swarm 正文协议头（上层协议）

最小开闭原则：少改 onlyne 本体。swarm 元数据夹带在消息正文里做一层上层协议，
外加一层解析器。格式为 YAML 头加分隔符（见 `PROTOCOL.md`）：

```text
---swarm
task_id: <uuid = session id>
from: <发起方树相对路径，root 为 ".">
transfer_send_to: <生成本任务的那个 task_id，顶层为空>
attempt: <整数，首次为 1>
---
<人类可读 Markdown 载荷；调度器投递时前置 ## role 段>
```

无头即非 swarm 消息，按普通消息处理（不建 session）。头解析失败同样按普通消息处理。
`task_id` 由调度器在 submit 与 in 监听入口生成，swarm.db 内唯一索引，重复投递直接丢弃并记日志。

## 5. 调度器（独立 daemon，Rust）

- 职责：根据 cwd 递归管理各工作区；监视对应 out 端，收到任务后创建 session，
  把任务插入 session 的 pi-onlyne 任务队列；out 落地即记 done + 台账并回收 terminal。
  无回调转发，无等待记账。
- Orca 映射：swarm root 是一个 Orca worktree；每个 hop 的 terminal 建在根
  worktree 下，标题 `swarm:<to>:<id8>` 即 workspace 身份（见
  `src/orca_term.rs::create_argv`：刻意不用 `--worktree`，folder-kind 节点
  在 UI 侧不可见——`worktree list` 不列、`terminal list` 不列其 tab，只剩
  Unknown 幽灵分组）。terminal 与 pi 进程视为一回事：调度器结束 pi 进程，
  orca 自动回收 terminal。不维护常驻 idle 池（空闲是瞬态），允许实现一个按
  workspace 路径分组的瞬态 ready 池应对突发并发，超期无任务认领即回收。
  （SPEC §12 的“无 git 映射”保留：`.ws/*` 是同一 checkout 内的普通目录，
  不建 git worktree。`sync` 顺手清理 folder-kind 残留节点，见
  `src/hierarchy.rs`：目录删了 Orca 不回收，只删 setup 记录。）
- 可见性：terminal 默认建在后台（`create` 不带 `--focus`，`SWARM_FOCUS=all|new`
  可选 opt-in；fan-out 下默认抢焦点不可用）。标题由 session 侧钉选（claim 时 +
  每次 idle，`43d7a80`，谁最后写谁赢）。同 workspace 的并发 hop 是根 worktree
  下的多个 `swarm:<to>:<id8>` tab——按标题前缀找 session，不按 Orca 侧栏结构。
  层级视图在 TUI 左树（见 TUI.md），不在 Orca 侧栏。
- 投递时序：`orca terminal create` 起 pi 有启动延迟。pi-onlyne swarm 模式启动后向本地 daemon
  发送新增的 `swarm_ready` 操作（载荷：workspace 路径 + 自报 terminal 句柄 + 时间戳，
  句柄只做 TUI 展示与 kill 用）。调度器经事件订阅收集 ready 信号，按 workspace 路径匹配
  pending 任务，随后写入 loopback/in。ready 的 pi 上下文是干净的（fork+exec 语义），
  同路径下任意可调度的 ready terminal 都可认领。ready 超时（可配，默认 120s）无任务则回收；
  建 terminal 后超时无 ready 则杀 terminal，任务回 pending 队列，TUI 计数告警。
- 拦截层面：订阅各 workspace daemon 的 `run/s` 事件流（`subscribe_events`），
  按 swarm 头路由。onlyne 本体只做一处小改：订阅者携带 `priority` 数字，daemon 按数字
  从高到低投递，任一订阅者返回 consumed 即停止后续传递。调度器用最高级，TUI 调试订阅用次高级。
  仅含 swarm 头的消息进入优先级链；无头消息走原有广播路径。
- Daemon 归属：调度器统一保活树内全部 workspace daemon（启动时拉起，崩溃重启，关闭时统一回收，
  TUI 展示每个 daemon 在线状态）。swarm workspace 下 daemon 的生命周期管理移交给调度器。

## 6. 状态机与持久化

- 状态保存在 swarm root 的 `.onlyne/swarm.db`（sqlite）：任务表 + ledger 列
  （terminal state + out head + reason）。root `.onlyne/ledger.jsonl` 为 append-only
  镜像。重启后 pending 任务继续，丢失 terminal 的记 failed 台账行。TUI 展示任务表。
- 关闭条件：进程退出即 closed。done/failed/cancelled 是终态标记，closed 是资源回收完成。
- 扇出：一发 N 个下游 = N 个独立任务，各自运行互不阻塞，汇合点在文件与台账。

## 7. 成功 / 失败 / 取消

- 成功：session 在目标 workspace out 中写出含 swarm 头（含 `task_id`）的结果消息。
  收到 out 即 done，记台账；调度器向 session 发送 `---swarm-ctl recycle`，
  session ack `swarm_recycled` 后自行退出，调度器关闭 Orca tab。
- 失败：无 out 的早停即 failed：记台账，不写任何回调，不重放。死 tab reaper 只在
  Orca 明确报告 `status=exited` 时介入；attempt 小于 3 时创建新 task_id 重投。
- `swarm_quit`：session uplink `swarm_recycled`（`quit:<reason>`）后自行退出；
  scheduler 立即记 failed，不重投。
- 取消：`onlyne-swarm cancel <task_id>` / TUI cancel 键按 `transfer_send_to` 血缘树终结任务族。
  默认发送 recycle 信令、等待最多 5 秒 ack、关闭 tab；`cancel --force` 立即关闭 tab，
  是人工逃生口。调度器不再向 session 注入 shell kill 命令。
- 完成信号：out 写入即成功信号；显式出口由 pi-onlyne swarm_* 工具承载。

## 8. pi-onlyne 改写（跨仓库，不另建插件）

- 新增 swarm 模式切换开关，on/off 绑定当前 workspace `.onlyne/config.toml` 的 `[swarm]` 表
  （`enabled = true`，后续字段可扩展）。开关可在 TUI 内 / `onlyne-swarm` 切换。
- swarm 模式行为增量（相对现有版本）：单任务原子执行，无任何例外；不再订阅通用 in/out 自动处理，
  输入输出由调度器接管，只走 session 内消息输入通道（`sendUserMessage followUp` 高优插入，
  优先级高于用户输入）；启动发送 `swarm_ready`；出口为 `swarm_complete` / `swarm_quit`；
  下游激发走 `swarm_send`（射后不理）；状态查询走 `swarm_status`。
  复用现有空回提醒机制做失败前保障（两次无出口自动 `swarm_quit`）。

## 9. Supervisor

- 用户在 pi 进程内激活任务：该 pi 进程为 supervisor 节点。
- 提交：supervisor 内运行 `onlyne-swarm submit --to <树相对路径> --payload <文件>`，
  经 swarm.sock 到调度器，返回 `task_id`。
- 唤醒：提交后不挂起。下游接力任务若 `to: "."`，调度器在 root 新建 swarm session 推进，
  它读台账与 runs/ 后继续工作。

## 10. CLI（`onlyne-swarm`）

`run`（前台拉起调度器，启动自动 sync；子目录误启动按祖先标记拒绝）、`attach`（连接已运行调度器）、
`submit`、`cancel`、`list`（tasks|workspaces）、`status`、`tui`（监控面板客户端）、
`workspace create|sync`（按 `.schedule` 生成 `.ws` 树与 `onlyne_in` 软链，
`run` 启动时自动 sync 一次）。`toggle_swarm` 走 TUI 键与 swarm.sock op。

## 11. IPC 与 TUI

- 调度器监听独立 `swarm.sock`（root `.onlyne/run/swarm.sock`），行分隔 JSON，
  op 含 `status / list_workspaces / list_tasks / submit / cancel / toggle_swarm / subscribe`。
  TUI 为该 socket 的订阅客户端，ratatui 实现（见 `IPC.md`、`TUI.md`）。
- TUI 首版：左树（workspace 层级 + `back_edges` 虚线），右上任务表
 （task_id/from/to/attempt/state/xfer），右下 daemon/terminal 存活与 ledger tail，附 swarm 开关 toggle 键。
- swarm 开关关闭语义：排空后停止监听——存量任务继续执行到 out 写出，新 in 任务不再建 session，
  daemon 恢复通用 in/out 自动处理，状态写入 `[swarm] enabled = false`。首次宽容，
  排空中重复要求则强制杀进程。

## 12. 非目标

- 不做全局任务完成定义、不做 Hop 上限、不做预算熔断（用户手动终止）。
- 不做 git/worktree 映射（子工作区就是目录）。
- 不做超时、不做并发上限、不做调度器侧自动重试。
- 不建新插件、不做 web 前端、不做 cron/调度器之外的执行语义。
