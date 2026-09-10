# TUI 监控面板（修订二 2026-09-08：左栏 role 图）

修订记录：

- 修订一 2026-09-06：tree tabs、`f` 聚焦 Orca tab、ledger 面板。
- 层级视图 2026-09-07：左栏按 `.ws/` 树分组挂 hop 行。
- 修订二 2026-09-08：左栏改为 role 图（§2），右上改为可过滤历史浏览器（§4），
  右下改为选中 session 详情（§5）。可游标栏只留 graph 与 history，`Tab` 在两栏之间切焦；
  detail 是纯展示。ledger 面板退出主视图：终态流水走 history 的 `state=all`，
  单条正文走 detail。

ratatui 实现的 swarm.sock 客户端（2s 全量轮询 + 事件驱动刷新）。`onlyne-swarm tui` 启动（需调度器在运行，
否则提示先 `run`）。只读展示 + 三个动作键（focus-tab、cancel、toggle_swarm）。
告警只标红展示，不得干扰正常运行。

## 1. 布局

```text
┌─ graph ────────────────────┬─ history ────────────────────────┐
│ ╭ scout ● ──────────╮      │ filter: state=active role=any    │
│ │ ▶ ■ 2a7d9931 run ◉│   ╵  │         text="" win=any  3/3     │
│ │   · 5c1f88a0 pend │   ╵  ├──────────────────────────────────┤
│ │     +12 done      │   ╵  │ created   id8      from→to  att  │
│ ╰─────────────╯ ╵  │ 12:04:31  2a7d9931 .→scout    1  │
│        ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄╡◀── │ 12:04:02  5c1f88a0 s→model   1  │
│ ╭ model ○ ──────────╮      │ 12:01:44  1f2c33d4 s→bench   2  │
│ │ (idle)   +3 done  │      ├─ session 2a7d9931 ──────────────┤
│ ╰─────────────╯    │ . → scout  running  att1  12:04:31 │
│      ━━━━━━━━━━━━━━╯    │ terminal orca:t-4417 ◉ live        │
│ ┄声明  ━已通信  ◉活tab     │ parent (root)  children 5c1f,7b3d │
│                        │ payload  目标：取 queued idea …   │
│                        │ out: (未写出)  reason: (空)         │
├─ [Tab]graph↔history [↑↓]行走 [/]文本 [s][r][e][w][x] 过滤 ─────┤
│  [J/K] 滚正文  [f]ocus orca tab  [c]ancel 血缘族  [t]oggle [q]uit │
└────────────────────────────────────────────────────┘
```

- 左栏 `graph`：role 图，见 §2。
- 右上 `history`：可过滤 session 清单，默认只看活跃，见 §4。
- 右下 `detail`：焦点栏选中 session 的正文与血缘，见 §5。
- 状态栏：`Tab` 在 graph ↔ history 切焦、`↑↓/jk` 走焦点栏游标、`J/K` 滚正文、
  `f`/Enter 聚焦、`c` 取消血缘族、`t` 切换 root swarm 开关、`q` 退出。

图示按等宽近似摆放，CJK 字符占双宽，行边界会错位。以实现规格为准：§2、§4、§5、§3。

## 2. 左栏 role 图（修订二）

### 2.1 模型

- 节点 = workspace = 一个 role = 一个 agent。`list_workspaces` 的每条模板项是一个框。
- 框内 = session 列表。session 是 agent 干的活，session 本身没有 agent 地位，
  session 与 session 之间不画连线。
- 边 = role 之间信息可传递的方向。箭头落在被通知方（目标 role）。

### 2.2 节点框

- 排布：单列堆叠。root（`path == "."`）在顶，其余按 `list_workspaces` 返回顺序往下。
- 标题行：`╭ <name> ●/○ ─╮`，`●` daemon 在线、`○` 离线（daemon 字段缺口见 §3.2）。
- 活跃 session 行：`■ id8 running ◉` / `· id8 pending`。`◉` = 有真实 terminal
  （handle 非空且不以 `stub-` 开头）。
- 活跃 session 行的符号按 hop 子状态细分（R4）：`busy` → `■`、`idle` → `◌`、
  `dispatched`/`ready` → `…`；标签用 hop 子状态代替 `running`。旧数据或
  hop 为空时回退到 `■ id8 running`。
- busy hop 超过 role 的 `busy_secs` 上限时，该行标红（`hop_overlong`），
  但不杀会话：红是给操作员的信号，回收权力仍只属 `cancel`。
- 折叠行：终态按 role 聚合成一行 `+N done`（有 failed/cancelled 时追加计数）。
- 空闲框保留一行 `(idle)`，框高最小 1，让边锚点的行号稳定。
- 框高上限：`floor((栏可用高 - role数 × 3) / role数)`，最小 1 行，活跃 session 超出部分
  折成 `+M active` 一行。大扇出场景下全部框仍同屏可见，边锚点不会顶出画面。
  未显示的活跃行靠 `history` 栏看：按 `r` 过滤到该 role 即可拿到全量清单。
- 游标只走 session 行，选中行 `▶` 或反白。`f`/`c` 沿用现有语义。
- 告警行（orphan / dangling）留在栏底，标红，见 §3.3。

### 2.3 边集合

| 边 | 来源 | 线型 |
|---|---|---|
| 声明边 | `list_workspaces[].back_edges`（模板里归一化为树绝对路径） | 细虚线 `┄` |
| 通信边 | `tasks` 的 distinct(`from_ws`, `to_ws`)，排除自环 | 粗实线 `━` |
| 活跃边（R4） | 该 `(from_ws,to_ws)` 上当前有 `hop_state=busy` 的活跃 task | 粗实线 `━` + `Modifier::BOLD` + 绿色 |

`tasks` 表没有清理逻辑，distinct 结果是全量通信史，所以粗线语义是"这对 role 之间
传递过信息"。同一对 role 同时有声明与通信记录时取粗线；有 busy hop 时升级为活跃边。
活跃边的权重靠 ANSI bold 承担，笔画只表语义（CR4）：去色终端下 `━` 与通信边同形，
bold 仍能区分"正在跑"与"跑过"。

### 2.4 布线场

- 位置在栏内侧、距右边框留 1 列，宽度 8–12 列，与标签文本分栏，边贴边框的情况要避开。
- 向下且跨相邻框的边：走框间隙 `╵`，入目标框用 `╡` 或 `◀`。
- 向上回边：源框右缘出 `┐`，导轨 `│` 上行，目标行入 `◀`。
- channel 分配：按跨度降序贪心占列；同列同格冲突时后来者外移一列；超出预算的边
  降级为目标框内的 `→ out: <target>` 文本行，线数增长不会毁掉图面。
- 每帧重算行号。锚点是 role 名，role 行位置稳定，线的位置就稳定。
- 栏底固定图例一行：`细 ┄=声明边　粗 ━=已发生通信　bold ━=活跃 hop　◉=活 tab`。

### 2.5 渲染与实现约束

- 左栏由 ratatui `List` 改为两遍构建：第一遍出框与 session 行并记录 role 锚点行号，
  第二遍把边画进列网（`Paragraph` + `Spans`，或按格子 `Buffer` 落图）。
  `List` 承载不了同列叠加的多条边。
- 布局与路由要写成纯函数，输入 `workspaces + tasks + 计数`，输出行向量，行内单元带样式
  标记，沿用 `tree_tab_lines` 那种可单测风格。`tree_tab_lines` 被 role 图渲染取代。
- 栏宽：从 `Percentage(32)` 提到能容 session 行与布线场，约 46–52 列；
  窄终端下 session 行截断策略：保 `id8` 与 state，截 role 名。

## 3. 后端配套（修订二新增）

### 3.1 聚合查询

一个 IPC op 足够，例如 `graph`：

```jsonc
// req: {"id":"t","op":"graph"}
{"data": {
  "by_ws": [{"ws": "scout", "state": "done", "n": 12}],   // GROUP BY to_ws, state
  "edges": [{"from": ".", "to": "scout", "n": 3}]          // DISTINCT from_ws,to_ws
}}
```

有了它，折叠计数与通信边都读全量，不依赖 `list_tasks` 的 `ORDER BY created_at DESC LIMIT 100`
窗口。约 15 行改动（`db.rs` 一个查询 + `ipc.rs` 一个分支）。

### 3.2 `daemon` 字段缺口

`list_workspaces` 目前只返回 `path/name/model/back_edges`。`tui.rs` 读 `w["daemon"]` 恒为缺省，
所以 `●/○` 全部显示离线。修订二要补该字段（`status` 或 `list_workspaces` 给出 daemon 活性），
左栏的在线指示才有意义。

### 3.3 orphan / dangling 告警

`status` 返回 `orphans` 与 `dangling`。它们由只读检查计算，复用 `sync` 的
实例与 loopback 链接规则；左图栏底按红色告警行展示。`workspace sync` 的输出
仍保留同一组告警，便于无 TUI 的排障。

### 3.4 `list_tasks` 扩参（修订二）

现在只有 `state` + `limit`，无 offset，无总数。修订二把它扩成带过滤的分页查询：

```jsonc
// req: {"id":"t","op":"list_tasks","state":"active","to_ws":"scout","text":"lean",
//             "from":"model","since":1757000000,"retry_only":true,"limit":50,"offset":100}
{"data": {
  "rows":  [{"task_id":"…","from_ws":"…","to_ws":"…","state":"running","attempt":2,
             "terminal":"…","created_at":1757000123}],
  "total": 1435
}}
```

- `state` 取值：`active`（= `state IN ('pending','running')`）、单个终态词、`all`。
- SQL 谓词按参数拼接：`to_ws`、`from_ws`、`(payload LIKE ? OR out_head LIKE ? OR reason LIKE ?)`
 （前缀匹配可走索引）、`created_at >= ?`、`attempt > 1`。
- 排序固定 `ORDER BY created_at DESC, rowid DESC`。`tasks` 没声明 `WITHOUT ROWID`，隐式 rowid
  可用；同秒并列的行靠它保证翻页不跳行。
- `total` 用同谓词的 `COUNT(*)`，栏头据此显示 `shown/total`，取代静默截断。
- 响应形状从数组变成 `{rows,total}`：`main.rs` 里 `tasks` / `watch` 子命令的调用点要同步改。
- rows 要补 `created_at`：现在 `TaskRow` 的 SELECT 只有
  `task_id/from_ws/to_ws/transfer_send_to/attempt/state/terminal/payload`，无时间列，
  `history` 栏的时间列需要它。`payload` 带 `#[serde(skip_serializing)]`，正文走 §3.5。

### 3.5 `task_detail` op（修订二）

列表行只携元数据，正文按选中行拉，避开每 2 秒搬 100 条 payload：

```jsonc
// req: {"id":"t","op":"task_detail","task_id":"2a7d…"}
{"data": {
  "task": {"task_id":"…","from_ws":".","to_ws":"scout","state":"running","attempt":1,
            "terminal":"orca:t-4417","created_at":1757000123,
            "payload":"目标：…","out_head":"","reason":"","hop_state":"busy","ledger_state":""},
  "hop": {"state":"busy","secs":95,"adopted":false},   // R4: 调度器内存时钟
  "parent":  {"task_id":"…","state":"done","out_head":"…"},
  "children":[{"task_id":"…","to_ws":"model","state":"pending","attempt":1}]
}}
```

- 组成：`db.get(task_id)` 全列 + 父行（按 `transfer_send_to`）+ `db.family()` 的子节点。
  现成函数已盖住查询，缺的是一个拼三者的 op 分支与 `out_head`/`reason`/`created_at` 的 SELECT 列。
- R4 新增 `hop`：`{state, secs, adopted}`。`secs` 来自调度器内存时钟
  `hop_since`，重启清零；`adopted=true` 表示时钟起点是本次重启（收养），
  detail 写 `hop busy for 1m35s (adopted)`，避免把重启后的 20 秒读成干活时长。
- 触发时机：焦点栏选中行变化时拉一次。2s 轮询只刷列表与图；选中行仍在列表里时保留已有正文，
  正文拉取失败只写状态行，清掉旧正文会让画面闪烁。

## 4. 右上 history 浏览器（修订二）

### 4.1 职责与默认

- 职责：扁平 session 清单 + 过滤。左图给结构与归属，此栏给可检索、可翻页的行清单。
- 默认过滤：`state=active`。初态只显示在干的 session，历史要看得自己改过滤器。
- 列：`created  id8  from→to  att  state  ◉`。
- 时间只显 `created_at`（领取时刻）。终态时刻在 tasks 表内没有列，只在
  `.onlyne/ledger.jsonl` 的 `ts` 里，本修订不引入 `finished_at`。
- 栏头常显当前过滤串与 `shown/total`：
  `history  state=active role=any text="" win=any retry=any   7/7`

### 4.2 过滤轴与键位

| 键 | 轴 | 取值 |
|---|---|---|
| `s` | state | active（默认）→ all → pending → running → done → failed → cancelled → closed |
| `r` | role（to_ws，谁在干） | any → 逐个 role |
| `e` | 通信对（from→to） | any → 逐个实际出现过的边（复用 `graph` 的 edges） |
| `/` | 文本子串 | 输入态，Enter 应用，Esc 退出（payload / out_head / reason） |
| `w` | 时间窗 | any → 1h → 24h → 7d |
| `x` | 重试 | any → 只看 `attempt > 1` |
| `PgUp/PgDn` | 翻页 | history 焦点下 offset 按页移动，graph 焦点下滚 detail 正文 |

过滤串变化时 offset 归零。输入态中按 `Esc` 回到原过滤串。

### 4.3 游标与焦点

- `Tab` 在 `graph` 与 `history` 两栏间移焦，状态栏显示 `[focus: history]`。
- `↑↓/jk` 只移动焦点栏游标。左图游标只走活跃 session 行，history 游标只走当前过滤结果行，
  两套行序解耦。
- `f`（聚焦 Orca tab）与 `c`（取消血缘族）作用于焦点栏选中行。
- 刷新后游标越界时钳到本栏末行；两栏各自保持自己的位置。

### 4.4 数据源

每轮拉 `list_tasks`（带当前过滤参数，§3.4）。过滤轴的候选 role 列表与边集合来自
`list_workspaces` 与 `graph`，过滤串变化不增加额外往返。

## 5. 右下 session 详情（修订二）

- 职责：单条 session 的正文与血缘。`graph` 与 `history` 回答“谁在干”“历史上有哪些”，
  这一栏回答“这一条写了什么”。
- 无游标：可游标的两栏都是能对 session 动手的栏（`f`/`c`），detail 纯展示，
  永远跟随焦点栏的选中行。`Tab` 只在 graph ↔ history 之间循环。
- 内容块（自上而下）：
  1. `from → to`、`state`、`attempt`、`created_at`（转本地时:分:秒）
  2. `terminal` handle 与活性：`◉ live` / `stub` / `无 tab`
  3. 血缘：`parent <id8>` 与子节点 `id8` 列表（带 state 短词）
  4. `payload` 全文（按栏宽折行）
  5. `out_head` 与 `reason`（空值写 `(未写出)` / `(空)`）
- 滚动：`J/K` 始终滚正文。`PgUp/PgDn` 在 graph 焦点下也按页滚正文；history 焦点下
  由同一按键负责 history 分页。选中行变化时滚动位置归零。
- 数据源：`task_detail`（§3.5），按选中行拉，不进 2s 轮询。
- 空态：无选中行时写 `(选中一个 session 看正文)`。detail 保持固定高度，画面会稳。
- `status.ledger_tail` 在 TUI 的消耗方就此结束：状态计数由左栏框内 `+N` 担当，
  终态流水走 `history` 的 `state=all`，单条正文走 detail。`status` op 本身保留该字段，
  CLI 侧 `onlyne-swarm status` 仍可速查。

## 6. 聚焦 Orca tab

选中 task 后按 `f` 或 Enter，TUI 取该 task 的 terminal handle 调
`orca terminal switch --terminal <handle>`，把 Orca UI 切到对应 hop tab
（等价于手点 tab）。三种情况只写状态行、不中断 TUI：

- handle 为空：任务还没建 terminal（pending 或已回收），报 `no live tab for <id8>`
- handle 以 `stub-` 开头：headless stub 会话，无真实 tab
- Orca 报 `terminal_handle_stale` 等错误：会话已关闭，原样回显错误

## 7. 数据源

2s 间隔全量 `list_workspaces` + `list_tasks`（带过滤参数）+ `graph` + `status` 轮询刷新
（`subscribe` 流为后续增量源，当前版本轮询已满足排障需求）。`task_detail` 按选中行变化拉。
按键：`Tab` 在 graph ↔ history 切焦点栏，`↑↓/jk` 走焦点栏游标，`J/K` 滚 detail 正文，
`/` 进文本检索，`s`/`r`/`e`/`w`/`x` 改过滤轴，`f`/Enter 切 Orca tab，`c` 取消选中任务血缘族，
`t` 切换 root swarm 开关，`q` 退出。

## 8. 取消语义（cancel）

`c` 取消选中 task 的血缘族（`transfer_send_to` 下游）。默认走自回收协议：
调度器向各 workspace loopback 写 `---swarm-ctl recycle`，session ack
`swarm_recycled` 后自行退出，调度器最多等 5 秒再关 tab。调度器不注入 shell
kill 命令；无 ack 只记 `recycle_no_ack` 日志。CLI 侧 `cancel --force` 跳过
ack 等待直接关 tab，是人工逃生口。

## 9. 开关语义（toggle）

关闭某 workspace 的 swarm 开关：排空后停止监听。存量任务执行到 out 写出，
新 in 任务不再建 session，daemon 恢复通用 in/out 自动处理，
状态写入该 workspace `[swarm] enabled = false`。首次宽容；排空中重复 toggle
要求则强制关闭该 workspace 全部 swarm terminal（running 记 failed 台账行）。
