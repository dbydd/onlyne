# R4 设计稿：hop 状态机（0.7.0，先设计不动代码）

目标：把今天压在 `TaskState::Running` 一个值里的四种现场拆开，让
"pane 刚建 / 会话启动中 / 空闲等输入 / 正在干活" 各自有超时、有 emit、
有 TUI 表达。R5 的收养与丢弃 emit 名一并进事件清单，巡检与 TUI 按同一套词表读。

非目标：`--detach` 的进程模型（§7 另定）；`Busy` 内部的细粒度进度
（token 数、工具调用序列不在 swarm 层建模）。

## 1. 状态表

DB 现有 `Pending/Running/Done/Failed/Cancelled/Closed` 不动（ledger 与
TUI 历史查询都依赖它们）。新增的是 hop 运行态子状态，存 `tasks` 新列
`hop_state TEXT DEFAULT ''`，取值如下。空值 = 老数据 / 未进入运行态，
查询时按 `Running` 对待。

| hop_state | 进入条件 | 退出条件 | 超时 | 超时动作 |
|---|---|---|---|---|
| `dispatched` | `dispatch()` 新建 pane 并写库（复用 idle pane 直进 `busy`，见 §3） | 收到 `swarm_ready` 且 handle 匹配 | 120s，可配（见下） | `swarm_ready timeout` failed + ledger，和今天一样；`running_since` 为空的行才判（R5 已修对的守卫，原样保留） |
| `ready` | `on_ready` 匹配上 task，但 `write_loopback_in` 尚未成功 | 正文写出成功 | 30s，全局固定 | 同上 failed 路径，reason `delivery timeout`；写出是本地 FIFO 操作，30s 写不出说明 daemon 已死，不必可配 |
| `busy` | `write_loopback_in` 成功（今天 `running_since` 插入点）；收养（R5）直接进 `busy`（CR2，探活即干活证据，后续 idle/busy 自然纠正） | 收到 `swarm_busy`/`swarm_idle`/`out`/recycle | 按 role 配长跑上限（见下），默认无上限 | 超限只 emit `hop_overlong` + TUI 标红，不杀会话（杀的权力留给人 `cancel`；swarm 层不猜长跑是死是活） |
| `idle` | 收到 `swarm_idle`（turn 结束且本轮无 out） | 收到 `swarm_busy`（新 turn 开始）或 `out` 或 recycle | 空闲 TTL，默认 60s，复用今天 idle 池的 TTL（§4） | TTL 到期判 hop 停滞：`swarm_idle timeout` failed + ledger + 回收 pane。注意这不是今天的静默池过期（今天过期只丢 handle 不记账）；改名义：过期 = 失败事件 |

不变量（CR1）：任何超时判死必须与终端回收同事务发生（`on_early_exit`
末尾就是 `close_terminal`，今天靠这一行巧合成立）。将来谁把 close
拆出去，R5 的丢活类就回来——§6 用 `timeout_fail_closes_terminal`
单测锁住它。

`Pending`（submit 落库，pane 未建）保持原样，不进 hop_state。

超时可配位：`dispatched` 120s 与 `busy` 长跑上限进 root
`.onlyne/swarm.workspace.jsonc` 的 `"timeouts": {"dispatched_secs": N,
"busy_secs": {"<role>": N}}`（role 级只给 busy，因为只有干活时长是按
role 差异化的；handshake 超时是传输属性，全局统一）。缺省 dispatched
120、busy 不限、idle 60。`ready` 30s 写死不开放——开放它只会让人把
daemon 已死的现场调成更长的静默。

## 2. 事件与幂等

`swarm_busy` / `swarm_idle` 走 pi-onlyne 现有事件通道，与 `swarm_ready`、
`swarm_recycled` 同形态：插件调 daemon `op`（`onlyne.ts` 新增
`swarmBusy`/`swarmIdle`，与 `swarmReady` 并列），daemon 以
`WorkspaceStateChanged { message: "swarm_busy {…}" }` 单 publish（与
`swarm_ready` 同一"单 publish、无 HistoryAppended"约束，否则 consume
窗口翻倍），scheduler 在 `route_event` 的 `workspace_state_changed`
分支里与 `swarm_ready` 并列解析。

事件体字段：`{workspace, terminal_handle, task_id}`，与现有两个事件
完全同形。task_id 是匹配键：scheduler 按 task_id 找行，不按 handle
（handle 在 adopt 后可能换过，task_id 不变）。

幂等口径：
- 同一 task 连续两次 `swarm_idle`：第二次是 no-op（hop_state 已是
  idle，只刷新 idle 进入时刻——不刷新会让长 idle 被误判超时吗？不：
  超时起点是"本轮无 out 的空闲开始"，连续 idle 说明还是同一轮空闲，
  起点不动。刷新的是"最后一次 idle 心跳"显示位，供 TUI 停留时长用）。
- `swarm_busy` 到达时 hop_state 已是 `busy`：no-op（同态重复）；到达时
  为 `idle`：进 `busy`（新 turn 开始，正常迁移）。对 `''`/`dispatched`/`ready`
  到达的 busy 直接进 `busy`（CR2：pane 复用与收养两条路径下“插件比调度器快”
  是正常时序，不是乱序。旧稿“不是 idle 就 no-op”会把收养后的第一发 busy
  丢掉，特此更正）。
- `swarm_idle` 到达时 task 已终态：与 `on_out` 终态行同规则，emit
  `out_for_terminal_task_dropped`（复用，不新增事件名）。
- `swarm_busy`/`swarm_idle` 到达时 task 行不存在：静默丢弃（与未知
  task 的 out 同规则，不 emit——插件只会给自己活着的 task 发，不存在
  即是竞态残留）。

空闲的两种语义区分（第 2 问）：
- turn 结束但 hop 未交活（等着 `swarm_complete`）：发 `swarm_idle`
  且 body 带 `"pending_exit": true`。scheduler 记 idle，只 emit
  `hop_state` 与 TUI 显示，不发第二份提醒、不判失败。提醒的唯一
  actor 仍是插件本地 `scheduleSwarmExitReminder`（不动；CR3：两代
  插件混跑时 scheduler 再发一份就会把同一会话 nag 两次）。
- 已交活待回收（out 已写，等 recycle ack 关 tab）：不发 idle。out
  落库即终态，回收窗口的 5s `wait_recycled_ack` 覆盖它；这段空闲不进
  状态机。
- 区分位由插件填：发 idle 时查 `state.swarmTask` 是否还存在。存在 =
  未交活（pending_exit true）；不存在说明 out 已写，不发。

数据源（插件侧 hook，无需 orca 轮询）：
- `pi.on("agent_end")`：turn 内无 out → 发 `swarm_idle`。今天这个
  handler 只做 pin title + exit reminder；加一行 fire-and-forget 的
  idle 上报（失败静默，scheduler 侧 TTL 是 backstop 不是依赖）。
- `pi.on("agent_start")`：有未完成 `state.swarmTask` → 发
  `swarm_busy`。今天这个 handler 只 `clearReminder()`；同位置加一行。
- 两个 hook 都已存在且与 swarm 强相关（index.ts:324-325），不引入新
  hook 面。`message_end` 不用（它是消息级，turn 内多条消息会抖动；
  agent_start/agent_end 是 turn 级，正好是 busy/idle 的粒度）。
- 退路（零协议变更）：`sweep_dead_terminals` 顺带读 orca agent state
  判 idle。本稿不选：轮询 + orca 依赖 + 粒度对不齐（orca 的 idle 是
  pane 级，swarm 要的是 hop 级）。hook 推送是正路；退路只在插件版本
  滞后（<0.9.0）时由 scheduler 侧降级，降级时 idle 相关超时全部停用，
  只保留今天的 running 语义。

## 3. 与 idle 终端池的合并

今天的 `idle: HashMap<path, Vec<IdleTerminal{handle, since}>>` 是"无
task 认领的 ready pane"池（`on_ready` 无 task 可配时 parking）。
R4 后它改由 hop 状态机拥有：

- 池里 pane 的本质 = "上一个 hop 已交活、tab 未关、session 空着的
  pane"。今天它没有 task 行所以没有状态；R4 给它一个轻量身份：
  `idle` 表值改为 `{handle, since, last_task_id}`（last_task 只做
  TUI 显示"上次干过什么"，不做匹配键）。
- TTL 拥有者：状态机。`since` 语义从"parking 时刻"改为"进入 idle
  时刻"（parking 即 idle，无差）；reaper 的 60s retain 逻辑不动，
  只是超时后走"过期 = 失败事件"还是"静默丢 handle"取决于该 pane
  是否有关联 hop：有关联（idle 状态的 task）→ 记 ledger 失败事件；
  无关联（纯 parking 池）→ 今天行为（静默丢）。
- `dispatch` 复用判据：今天是"同 path 有池 pane 就拿"（时间判据隐含
  在 60s TTL 里）。R4 改为"同 path 有池 pane 且其 `since` 未超 TTL
  就拿"——判据字面没变，但 TTL 的语义从"缓存有效期"变成"hop 空闲
  上限"，同一数字、同一代码行、不同词表。`deliver_to_terminal`
  成功后新 task 直接进 `busy`（跳过 dispatched/ready：pane 已 ready、
  正文当场写出；`running_since` 照常插入）。
- 回收路径（recycle ack）不用改：`close_terminal`/`close_tab_only`
  只认 handle 不认状态，idle 池的 pane 回收走同一函数。唯一增量：
  回收 idle 关联 pane 时把对应 task 行 hop_state 清空（终态行不需要
  残留子状态，查询按终态走）。

## 4. TUI 映射

- 边：`EdgeKind` 加第三种 `HopActive`（笔画 `━` + `Modifier::BOLD` +
  绿；CR4：TUI.md 口径“粗细靠 ANSI bold、笔画只表语义”，去色终端下
  同形换色会同义，hop 是否在跑是主信息，必须占 bold 权重）。
  `Communicated` 保持常规笔画与默认色。`busy`
  的 hop 所属边画 HopActive；`idle` 的 hop 边退回 Declared 细边
  `┄`；dispatched/ready 中间态不画边（pane 未干活，无通信事实）。
  图例行加一段：`━ hop active`。
- role 框：`graph_task_text` 的 `■`（running 符号）按 hop_state 细分：
  busy `■`、idle `◌`、dispatched/ready `…`。`◉ live tab` 后缀保留，
  只看 tab 存活不看 hop 状态（两回事不混）。
- 停留时长：detail 栏加一行 `hop <state> for <dur>`，dur 从状态进入
  时刻（scheduler 内存 `hop_since: HashMap<task_id, (state, Instant)>`，
  不进 DB，重启清零——重启前的停留时长不可考，清零是诚实的）算起。
  状态栏（keys 栏上一行，现有 `focus:` 行）追加 `busy 3m12s` 这类
  当前选中 hop 的时长。
- 新 emit 归栏：
  - `task_adopted` → alerts 区（红色？不，黄色/默认色：收养是正常
    事件不是故障；现有 alerts 只有红色的 orphan/dangling，加一种
    `Modifier::BOLD` 默认色行 `adopted: <id8> <to>`）。
  - `out_for_terminal_task_dropped` → alerts 区红色行
    `dropped-out: <id8> <state>`（这是真丢活，和 dangling 同级）。
  - `hop_overlong`（busy 超限）→ role 框内 task 行标红（已有 Failed
    红色样式复用），不进 alerts（ palert 会淹没真正的丢活）。
  - `task_stalled_ready_timeout`（已有 emit）→ 保持现状（history 事件
    流可见即可）。

## 5. 事件清单（巡检与 TUI 共用词表）

scheduler → bus（已有 + 新增）：

| 事件 | 发射点 | 含义 |
|---|---|---|
| `task_created` | submit / inbound | task 落库（已有） |
| `task_running` | dispatch | pane 建好（已有；R4 后语义 = 进入 dispatched） |
| `task_done` / `task_closed` | on_out | 交活归档（已有） |
| `task_failed` | early_exit/recycle/timeout | 失败（已有） |
| `task_retried` | sweep/timeout replay | 重试派生（已有） |
| `task_stalled_ready_timeout` | `ipc.rs::reap_loop` | handshake 超时（已有） |
| `task_adopted` | reap_previous_run（R5） | 重启收养活会话 |
| `out_for_terminal_task_dropped` | on_out 终态行（R5） | 活会话的 out 因行已终态被丢弃 |
| `hop_state` | dispatched→ready→busy→idle 每次迁移 | `{task_id, from_state, to_state, at}`（CR5：`from`/`to` 在别处指 workspace，此处必须写全；TUI 与巡检都按此解析）；TUI 状态栏与停留时长的数据源 |
| `hop_overlong` | busy 超 role 上限 | 只告警不杀；首报在超限点，此后每 `max(10min, busy_secs/2)` 重报（CR6），体带 `{limit_secs, elapsed_secs}`，巡检按 `elapsed` 增长判“还在跑/卡住不动” |

插件 → daemon → scheduler（`WorkspaceStateChanged.message` 前缀）：

| 前缀 | 体字段 | 含义 |
|---|---|---|
| `swarm_ready {…}` | workspace, terminal_handle | 可投递（已有） |
| `swarm_busy {…}` | workspace, terminal_handle, task_id | turn 开始（新增，pi 0.9.0） |
| `swarm_idle {…}` | workspace, terminal_handle, task_id, pending_exit | turn 结束无 out（新增，pi 0.9.0） |
| `swarm_recycled {…}` | task_id, terminal_handle, reason | 接受回收/自退（已有） |

版本门：scheduler 收到未知前缀一律忽略（今天 `route_event` 的
`_ => {}` 默认分支，原样保留）。插件 <0.9.0 不发 busy/idle 时，
scheduler 侧 hop_state 恒为 dispatched→busy（ready 瞬时，idle 永不
进入），行为退化为今天语义——升级是渐进的，不要求原子切换。

## 6. 测试矩阵

- `collect_dirs` dot 过滤（已有）+ 物化 copy-if-absent（已有）+
  `[swarm]` 追加幂等（已有）+ 收养/判死（已有）：不动。
- 新增：hop 迁移单测（dispatched→ready→busy→idle→(busy)→终态的
  允许边非法边表）；`swarm_idle` 连续两次幂等；`pending_exit`
  true/false 分流；TTL 过期记失败事件；`dispatch` 复用 idle 池 pane
  直接进 busy；未知前缀忽略（现有 `_ => {}` 加单测锁定）；
  `timeout_fail_closes_terminal`（CR1：超时路径后 handle
  不再存活）；收养写 `hop_state='busy'` + `swarm_busy` 对
  `''`/dispatched/ready 进 busy（CR2 两条）。
- E2E：stub 模式发合成 busy/idle 事件（stub 经 FIFO 写
  `swarm_busy {…}` 文本行，和今天 stub 写 out 一样），断言 TUI
  snapshot 的边色与 `hop_state` emit 序列。
- 插件侧：`agent_start/agent_end` 发 busy/idle 的单测（mock pi
  ExtensionAPI，与现有 swarm-slot 单测同构）。

## 7. `--detach` 定版（归本稿）

与状态机同版的原因：detach 后 scheduler 与 pane 的生命周期解耦，
"谁拥有 idle TTL、谁回收 tab"必须按 detached 语义重写（前台版答案
是"进程退出即全停"，detached 版必须回答"daemon 退出后 pane 谁管"）。
定版：

- `run --detach`：double-fork（或 `nohup` 自举，二选一，实现时定；
  不引入 launchd/systemd 依赖——这是 onlyne 既有约束），pid 写
  `.onlyne/run/scheduler.pid`，stdout/stderr 进
  `.onlyne/logs/scheduler.log`。前台 `run` 行为不变。
- 新增 `stop` 子命令：读 pid 文件发 SIGTERM，等 5s 未退出发 SIGKILL，
  删 sock + pid 文件。`status` 在无 pid 且无 sock 时报
  `no scheduler`（消灭 ECONNREFUSED 窗口的另一半：R5 修了优雅退出
  的 unlink，`stop` 覆盖 kill -9 与 crash 后的残留判断）。
- detached 下重连语义不变（R5 收养照走：pid 文件在就说明是计划内
  重启，收养；pid 文件不在而 DB 有 running 行 = crash，同样收养——
  收养不区分退出原因，只看现场是否还活着）。
- 不做：开机自启、看门狗、远端 supervisor（超出 onlyne 边界）。
