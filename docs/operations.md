# Operations

Onlyne 运维以 server 账本、client 工作区、admin 本地 socket（规范名 `.onlyne/run/s`；路径不超过 103 字节时绑在这一条，超限时绑到系统临时目录下的短派生路径，实际服务的路径记在同目录的 `run/socket` 标记里）为边界。

## 值守入口

`onlyne status` 通过 admin 本地 socket 读取 server 状态。

`onlyne roles` 读取 role 注册表和在线状态。

`onlyne sessions` 读取 session 投影，答案就是心跳落下的那一行镜像，`updated_at` 是它的年龄。

`onlyne sessions --fresh --task <task>` 现场取一次真值：server 把既有的 `control` op `probe` 发给该 task 的属主 client，等这一行越过读取开始时的 `(generation, seq)`，再按这一行作答。admin 词表不加动词，client 侧也不加代码。

等待的上界是这次读取自己的 `--timeout` 减去帧往返预留的 250ms；probe 没落地时照常按存下的镜像作答，读取不会超出这个界，也不会为了等它而放宽这个界。

`--fresh` 的答案在行上带 `fresh` 字段：`probed` 是 probe 落地后的观察，`offline` 是没有可问的对象（没给 `--task`、该 task 没有行、没有 client 拥有它、或属主不在线），`unanswered` 是 probe 出去了但界内没有重发布。三种情况都答那一行，不报错、不挂起。

`--fresh` 必须带 `--task`：fresh 读取问的是一个具名 task 的 client。

不带 `--fresh` 的读取与从前逐字节相同：不发 control 帧、不等待、答案里没有 `fresh` 键。

`onlyne ledger` 读取投递账本。

`onlyne faults` 读取 faults 表。

`onlyne watch` 读取 durable 与 advisory 事件流。

`onlyne history` 回放事件记录。

`onlyne spec_diff` 对比运行中 spec 与磁盘 spec。

## 服务路径的读法

每个守护进程绑定 socket 时把实际服务的路径发布在 `<owner>/.onlyne/run/socket`（mode `0600`，一条绝对路径加一个换行）。操作者读这条路径有三个入口：

`onlyne-client status --workspace <dir>` 打印 `socket <path>`，字段值就是这条服务路径。

client 日志在启动时点名这条路径：短路径场景一行同时给出规范路径、其字节长度、服务路径与 marker（`adapter socket moved to the short path`）；规范路径场景一行给出 socket 路径（`adapter socket serving`）。server 侧同口径：短路径场景一行给出 served 与 canonical 两条路径及其长度（`the run socket is served from a short path; the marker names it`），常规场景一行 `the run socket is open`。

`cat <workspace>/.onlyne/run/socket` 直接读 marker 文件。

session 进程带着 `ONLYNE_SOCKET` 启动，值是这条服务路径；role pane 里的 `onlyne` 命令凭它直达 socket。CLI 的解析次序是 `--socket` > `ONLYNE_SOCKET` > `--server-root` > `--workspace`/cwd 上行查找，查找以 `.onlyne/run/s` 或 `.onlyne/run/socket` 认定属主目录，路径经 `socket_path()` 解析。

bind 失败的 client 以 exit 1 结束，stderr 一行 `onlyne-client: bind the workspace socket <规范路径>: <明细>`，明细给出服务路径、两条路径各自的字节长度与 OS 原因；一个绑不上 socket 的 client 选择退出，保持 TLS 链路静默重试的循环已移除。绑定成功之后的 `accept` 错误以 `error` 级记日志（`adapter socket accept failed; retrying`），每 100 毫秒重试一次，listener 保持在手。

验证链路由 `crates/onlyne-testkit/e2e/socket-path-length.sh` 钉住：垫深的工作区、短服务路径、marker 发布、规范路径保持空位、任务端到端结清。

## 配置加载

spec.toml 或角色工作区 config.toml 里的未识别键被忽略，进程照常启动；每个被忽略的键在加载时于 daemon 日志落一行 `tracing` warning。真实键的取值错误仍是拒绝启动的硬错误。推论：拼错的键静默回退到默认值，除那行 warning 外无其他信号。

一次版本升级后，role client 与其 server 必须跑同一 build：`hello` 回复少了一个字段，二者不匹配时连不上。升级时把 server 和每个 `onlyne-client` 一起重启。

## 并发度

同 role 多 session 并行的旋钮是 `[[client]].max_sessions`。

`max_sessions` 的语义是同时在飞 session 上限。

每个 task 拥有独立 session。

role 达到 `max_sessions` 后停止 pull 新任务。

满容量的 role 把 `pull` 换成 `control_only = true` 继续发。

control 行与任务行共用一条 pull 队列，容量闸门挡的是任务。

`recycle`、`cancel`、`focus` 是腾容量和看现场用的命令，恰好要在满容量时到达，所以 server 在这一路只交 control 行，任务行的 `queued` 状态与 ticket 都不动。

server 保留挂账并在下次 pull 时再次 offer。

`onlyne-client init` 的种子值是 `max_sessions = 1`。

种子值保护单 pane 手工环境。

并行度按 role 在 spec 里显式设置。

spec 改完后执行 `onlyne reload` 生效。

存量 client 收到 `SpecReloaded` 事件后自动刷新 role slice。

存量 client 刷新 role slice 后立即使用新的 `max_sessions` 闸门。

存量 client 无需重启。

`crates/onlyne-testkit/e2e/reconnect-requeue.sh` 用 `max_sessions = 2` 和三条 task 覆盖挂账再 offer 路径。

满容量时 control 仍到达这一条，由 `crates/onlyne-server/tests/delivery.rs` 的 `a_control_only_pull_hands_the_command_and_leaves_the_work_queued` 在协议面钉住，并由 `crates/onlyne-testkit/e2e/herdr-live.sh` 的 d 步在活宿主上验一次：该 case 的 role 用种子值 `max_sessions = 1`，唯一槽被一条 `sleep` 占满，`control focus` 依然落到 session 的 pane。

## 焦点

`onlyne control --from <role> focus --task <id>` 把某个 session 的 pane 摆到前台。TUI 的入口是 `F`，作用在选中的那一行上。

控制平面用 `ControlOp::Focus{task_id}`，账本行 `kind = control`。命令落到 session 的 `backend_ref`，herdr 后端按三段链路走：`herdr workspace focus <W>`、`herdr tab focus <T>`、第三段按 pane 的来历分岔 —— managed agent 走 `herdr agent focus <pane_id>`，`herdr pane run` 拉起的 shell pane 走 `herdr pane focus --pane <base_pane> --direction <split_direction>`，这两个值是分屏时记下的。`base_pane` 与 `split_direction` 存在 `backend_ref` 里，所以锚点跟着 pane 活。

`herdr pane get <pane_id>` 是确认那一步。`result.pane.focused` 为 true 才算送达；落在别处时命令报错，并指名当前持焦的 pane。`focus()` 失败记一条 `Report::Fault{kind:"focus"}`，TUI 把后端原文打在这一行的反馈位。

`--from` 是 admin 面的全局旗标，写在 `control` 之后。焦点命令的 ACL 与投递同口径：对目标有 `send` 边的 role 掌握该目标会话的控制权，`ControlOp::Broadcast` 需要全局边。

herdr 后端按 label 认 workspace（`onlyne:<cluster>`），按名字认 tab（role 自己的名字）。想让 session 落进手上这个 workspace 与 role tab，操作者在拉起 session 之前先改名：`herdr workspace rename <WORKSPACE_ID> onlyne:<cluster>`、`herdr tab rename <TAB_ID> <role>`。label 对不上的 workspace 会拿到第二个 workspace，tab 名对不上会拿到第二个 tab，这时 client 打一条 warning，点名该 label 与新建出来的 workspace。

## 故障恢复

fault 是 server 记录的可审计事实。

fault 进入 `faults` 表。

fault 通过 advisory `Event::Fault` 推给观察者。

`onlyne repair inspect --task <id>` 读取一条任务的恢复上下文。

`onlyne repair adopt --task <id> --backend <backend> [--backend-ref <值>] --reason <reason>` 换掉这一行 desired 里的 backend 绑定，行内的 session id 与 generation 保持原样，seq 前进一格。要把任务搬到另一个 session，用下面的 `rebind`。

`onlyne repair rebind --task <id> --session-id <session> --backend <backend> [--backend-ref <值>] --reason <reason>` 重写任务的 backend 绑定，把行内的 session id 换成给定值，generation 加一、seq 归零，旧 generation 的上报从此不再被采信。

两个动词的 `--backend-ref` 同一规则：能整体解析为 JSON 的取值按解析结果上线（pane 引用这类对象形值因此可直接写 `--backend-ref '{"id":"p-7"}'`），其余文本按一个 JSON 字符串上线，旗标缺省上线 null。服务端与 client 按对象消费该值（`crates/onlyne-server/src/faults.rs:253-257,273-277`、`crates/onlyne-client/src/session/dispatch.rs`）。

`onlyne repair retry --task <id> --reason <reason>` 把可重试任务送回队列。

`onlyne repair fail --task <id> --reason <reason>` 把任务收敛为失败。

`onlyne repair close --task <id> --reason <reason>` 关闭恢复工作。

`onlyne repair ack --fault-id <fault-id> --reason <reason>` 确认一条 fault。

repair 族走 `<server-root>/.onlyne/run/s` 的 admin 面；树深过 103 字节界限时走 `run/socket` 记下的那条短服务路径。

repair 族不经过 role 工作区的 adapter socket。

## 投递与重投

role link 死亡时，服务端把该 role 的 `in_flight` 投递行重投回 `queued`，等待下一次 pull 再交付。

新 link 落地时的接管重投走同一条路。`hello` 的 `live_tasks` 字段申报该 client 内存里仍活着的会话任务；被申报的行保持 `in_flight`，其 delivery ticket 改挂新 link 的 generation，此后该 link 终止时照常被重投。

旧版 client 的 `hello` 没有 `live_tasks` 字段，行为与 1.0.8 一致：全部重投。

被申报的会话若在结清之前死亡，client 发布 `exited` 投影，服务端见到与该会话 ticket 同 `session_id` 的 `in_flight` 行时把该行重投回队列，同样经过下面的闸。

自动重投受两个预算旋钮约束；手动 `onlyne repair retry` 不经过闸。

| 配置文件 | 字段 | 默认 | 作用 |
|---|---|---|---|
| `<server-root>/.onlyne/spec.toml` 的 `[server]` | `requeue_max_attempts` | 0 | 一条行允许的自动重投次数上限，0 为不限；超限的行落 `rejected`，reason 为 `requeue_exhausted` |
| `<server-root>/.onlyne/spec.toml` 的 `[server]` | `requeue_ttl_secs` | 0 | 自动重投允许的行龄上限，按入队时间计，0 为关闭；超龄的行落 `expired`，reason 为 `requeue_ttl` |

先判 TTL，再判次数，两者都各发一条 `ledger_state` 事件。

`reason` 的读法：`onlyne ledger` 的行键为 `msg_id`、`task`、`state`、`reason`、`out_head`、`body`；该键只在这一行有值时出现，无值的行与列加入之前逐字节一致。TUI 第二页的 task 详情面板在账本行尾追加 `reason=<text>`。落进这一列的取值：`requeue_exhausted` 与 `requeue_ttl` 来自上面两道闸，`expired` 来自到期扫描，`session_dead` 来自 client 结清掉线 session 时写下的拒收（见「会话残影与属主判定」一节），pane 后端（`herdr` / `orca` / `zellij`）在开页前拒收协议 `session_command` 时整句拒收文案落 `rejected` 行（见「Headless（exec）会话」一节末段）；操作者经 `onlyne reject --reason` 或 `onlyne repair fail --reason` 自填的文本原样进这一列，`onlyne ack` 收下时该文本随结清事件走，行上的 `reason` 保持原样。字符串 `operator ack` 是 faults 表 `reason` 列的用例数据（`crates/onlyne-store/src/tests.rs` 的 `update_fault_state` 用例），账本列没有它的记录。

push 投递与 pull 投递的 `in_flight` 翻面都各有一条 `ledger_state` 事件；离线读账的 ledger 状态与会话投影在任何采样点互相对得上。

完整链路（server 在活 link 下死亡、client 重连接管、单一会话自然结清）由 `crates/onlyne-testkit/e2e/requeue-claim.sh` 在真实进程上验证。

本角色已经以 `Done` 结项的 task 再次投来时，client 认得它。判定读 `client.db` 的会话终态，入口是 `onlyne-client` 的 `DispatchState::task_completed_here`，动作在 `accept_delivery` 的容量闸之前：这一行就地 ack，`accepted = true`，reason 为 `task already completed by this role`，会话不 stage，容量不占。该 reason 进 `ledger_state` 事件，行本身落 `acked`。日志面同一时刻记一条 `redelivery of a finished task settled without running it`，带 `msg_id` 与 `task`。

这道判定读的是 `Done`。会话被终止、崩溃、`Failed` 的 task 保持可重投，`onlyne repair retry` 与 `control retry` 对这一类行照常生效。要重跑一个已完成的 task，操作者发新 task（`onlyne send`）。已 `Done` 的行等到的是这条 ack 和一次空跑。

## 拒收面

`onlyne ack --msg-id <id> --reason <text>` 把一条投递结为 `acked`。

`onlyne reject --msg-id <id> --reason <text>` 把一条投递结为 `rejected`。

`--reason` 在两个动词上都是必填。

`--op-id` 在两个动词上都可选。

拒收的 reason 落账本行。

两个动词都走角色工作区的 adapter socket。

`--request` 被这两个动词拒绝。

插件在 `assign` 上回 `accepted = false` 时，client 以同一 `msg_id` 入队一条拒收 ack。

该拒收 ack 与 completion 共用 durable intent 队列，断连后在重连时补发。

拒收理由取插件给的 reason，插件没给时记 `assign rejected`。

已结清的投递再收一次 ack 或 reject，服务端回同一状态事件。

## 会话残影与属主判定

lifecycle 属主是 role 自己的 client 进程。

client 死亡期间无人代该 role 判定 session 生命周期。

属主进程活着时，掉线的 session 由 client 结清：plugin 连接断开、`reconnect_grace_secs` 超期后，退役它留下的 session。

结清写两条：该 task 落 `failed`，它占着的投递行以 `session_dead` 拒收；退役的同一趟再把该 session 自己的投影随一条 heartbeat 报告发出，server 行随即读 `exited`，不必等观察器的 `stale_working` 或 `heartbeat_missing`。

该拒收是终态，工作只由 operator 的 `repair retry` 唤回。

重启的 client 不对旧账做终态判定：`Acked` 的行是它自己已经答过的，重启不上报它们的死，也不替别的属主判定。

残影判定与冻结上报有五个旋钮：

| 配置文件 | 字段 | 默认 | 作用 |
|---|---|---|---|
| `<workspace>/.onlyne/config.toml` | `stall_report_secs` | 1800 | 会话投影 tuple 冻结时长上限，client 据此上报 `stalled` fault，0 关闭，单位秒 |
| `<workspace>/.onlyne/config.toml` | `reconnect_grace_secs` | 60 | plugin 连接断开后允许其离席的时长，超期由 client 退役它留下的 session 并结清它欠的那件事；仍绑着 task 时该 task 落 `failed`、其投递行以 `session_dead` 拒收，0 关闭，单位秒 |
| `<server-root>/.onlyne/spec.toml` 的 `[server]` | `stale_watch_secs` | 60 | server 观察器扫描周期，单位秒；0 关闭观察器 |
| `<server-root>/.onlyne/spec.toml` 的 `[server]` | `heartbeat_grace_secs` | 90 | 属主在线时 `working` 行允许的心跳静默时长，单位秒 |
| `<server-root>/.onlyne/spec.toml` 的 `[server]` | `ghost_sweep_secs` | 60 | server ghost sweep 扫描周期，单位秒；0 关闭这一趟 |

server 侧观察器按 `[server].stale_watch_secs` 周期扫描，一次扫描跑两个探测器。

探测器一扫描 `working` 且属主离线的行。

离线探测器超过 600 秒记录 kind 为 `stale_working` 的 fault。

探测器二扫描 `working` 且属主在线的行，判据是心跳新鲜度。

pi 插件每 10 秒发一个 heartbeat 包，client 每收到一拍就在自己的 heartbeat 报告里重发一次完整投影，服务端行的 `updated_at` 随心跳前进。

不等下一拍心跳、要当下观察时用 `onlyne sessions --fresh --task <task>`：它把 `probe` 发给属主 client，等这一行越过读取开始时的水位，插件用一条 heartbeat 应答这个 `probe`。

静默一拍即翻面的 no-op 心跳同样携带存活事实，client 对其抬升本地版本号并重发，健康会话的 `updated_at` 保持新鲜。

属主在线、行龄超过 `[server].heartbeat_grace_secs`（默认 90 秒）、且本进程见过该行的写入时，探测器二记录 kind 为 `heartbeat_missing` 的 fault。

`heartbeat_missing` 的行同时出现在 `sessions` 答案的 `heartbeat_stale` 字段上，TUI 呈现为 `working+stale`。

同一任务已有未确认的同类 fault 时，探测器二不再重复记录。

server 启动时把当时 `working` 的行全部登记为已见，上一进程遗留的静默行同样可被标记。

completion 落定为 `exited` 之后同 generation 的 heartbeat 把行抬回 `working` 时，观察器记录 kind 为 `heartbeat_after_complete` 的 fault，投影照常应用。

两个探测器都只记 fault 并推送 advisory `Event::Fault`。

观察器之外另有一趟 ghost sweep，按 `[server].ghost_sweep_secs` 周期扫描，首个 tick 落在半个周期处。

它读取镜像里 `working` 的行，并把每一行与该任务自己的 ledger 行对齐。

ledger 行已落终态的那一行 `working` 镜像是化石：账已结清，镜像还留着旧字节。

一趟扫描把该行 ledger 已经载明的判定写进镜像：`acked` 读作 `done`，`rejected` 与 `expired` 读作 `failed`。

写入走 `repair` 族共用的那条结清路径：重写 `observed_json`、抬升 `seq`、落库、推送 durable `session_state`。

ledger 行仍是 `queued` 或 `in_flight` 时，这一趟留下该行原样。

每移动一行就在 `ghost_sweeps` 审计表落一行。该行每个字段都取自服务端已经持有的行：task、role、session、generation、`seq_before`、`seq_after`、outcome、evidence、`swept_at`。

`evidence` 文本是标签加上为这次写入作证的那个 ledger 状态，例如 `task_settled:acked`。

`onlyne ghosts [--limit N]` 读这张表，最新的一趟在前。它是 admin 面的读取，client 面上拒收。

`stalled` 由 client 上报，走 role 的 `report` 面：会话的投影 tuple 连续超过 `stall_report_secs` 没有任何一次 `Applied` 变化时，client 发一条 kind 为 `stalled` 的 `Report::Fault`，带 task 与 session 身份。

no-op 心跳抬存活水位，不抬进展水位；`stalled` 只看后者。

`stalled` 的 reason 文本是 `no applied progress`。

一条已结清的任务走不进这条判据：进展时钟只由 task 分配建立，`note_applied` 只刷新已经建立的时钟，plugin 在完成回执之后送出的最后一拍 `agent: "idle"` 心跳落在空处。到期扫描与发送边界各派生一次 lifecycle——存储元组与 `task` 表里该任务的结清值一起过 `project`——得出 `exited` 的 task 被抑制并被遗忘；连接释放顺手忘掉它服务过的每个 session 的时钟。

同一冻结 episode 只报一次，下一次 `Applied` 解除去重。`stall_report_secs = 0` 关闭这条判定。

行不翻面：`stalled` 只落 faults 表并推事件，恢复决策留给 supervisor 与 repair 族；会移动镜像的只有 ghost sweep 一趟，口径见本节末。

连接断开宽限走另一个旋钮。plugin 连接在没有 `detach` 帧的情况下结束时，client 保留它的 session 与宿主资源，`reconnect_grace_secs` 从这一刻起计。

窗内重连清掉这个时钟：agent 回到它原来那个 session，照常领下一个任务。

超过窗口仍未回来时，client 退役它：关掉宿主资源，吐出容量槽位。

退役前先结清这个 session 欠的那件事：仍绑着 task 时，该 task 落 `failed`，它占着的投递行以 `session_dead` 拒收；退役的同一趟再发出该 session 自己的投影，server 行随即读 `exited`。未绑 task 的 slot 只退役。

退役的 reason 取该 session 名下 task 已落的终态，无终态可取时记 `Fault`。`reconnect_grace_secs = 0` 关闭这条判定。

退役面只此一类。「重试 session 永不到来」不另设计时器或缓冲超时：仍活着的 task 的静默由 `stalled` 与服务端心跳两个面兜底，这是这轮运维定的边界。

一个更新的 session 已经在服务同一 task 时，旧 id 的连接回来即降级为只读：它不再收到 `assign`、`deliver`、`render_send`，也不占该 session 的投递面。

只读连接本身仍被接纳，它送出的 `send` 帧不入 durable 队列、不发往服务端，而是攒进该 task 的缓冲，等一次合并。

降级只在那条更新的连接在线期间成立：它结束时，为该 session 攒着的第一个连接即被提升为其 transport，只读标记同时解除，所以插件抢在 client 察觉旧 socket 已死之前重挂不会被永久静音。


重试那条 session 结项时，缓冲与它自己的 handoff 按下游 role 合并成一条：一个 role 一条 envelope，正文每行带来源标注，`[retry]` 是结项那条 session 写的，`[zombie]` 是攒着的旧连接写的。

合并投递之后，只读连接收到 `bye` 并被摘掉；它若还占着自己的 slot，该 slot 以 `Replaced` 退役。结项的账只付一次，这一步不再 settle，也不再 release。

一条例外：促成这次合并的那份 report 就是从这条只读连接上收进来的，那么它在这一轮收不到 `bye`。client 先把这条 report 的应答写出去，连接的收尾交给它自己的 `detach` 帧或 socket 结束。插件对 `bye` 的处理是断开 socket 并把所有在途请求判为失败，抢在应答之前的 `bye` 会把一笔已经落账的完成读成失败，agent 因此重发终态。顺序由 `crates/onlyne-client/tests/scenarios/reconnect.rs` 的 `a_read_only_completion_is_answered_before_any_bye` 钉住。

faults 表的 kind 字段保存 `stale_working`、`heartbeat_missing`、`heartbeat_after_complete`、`stalled` 文本。

server 侧观察器不改 ledger 状态。

server 侧观察器不触发 retry。

server 侧观察器不触发 fail。

server 侧观察器遵守 §8 的零政策红线。

ghost sweep 在这条红线内移动一类行：镜像仍读 `working`、而该任务自己的 ledger 行已落终态的那一类。

它写进镜像的判定来自那条 ledger 行，同一趟把该任务名下仍未结清的投递行一并结清。

另一类 `working` 行它不动：属主离线、而任务仍未结清的那一类。

给这类行写判定就是替活着的活儿定终局，它该得的重排队列会被吞掉。

`stale_working` 仍是这类行唯一的输出，恢复决定仍归 supervisor 与 `repair_*` 族。

存活判定的信源是 pi-onlyne 心跳包本身，pane 与宿主终端的存活状态不在服务端判定面内。

恢复决定归人和 supervisor 角色。

人使用 repair 族执行 `inspect`、`adopt`、`rebind`、`retry`、`fail`、`close`、`ack`。

supervisor 角色使用 control 动词执行恢复动作。

`onlyne control --task <id> recycle --reason <text>` 与 `onlyne control --task <id> cancel --reason <text>` 的 reason 是必填。

supervisor 角色的 control 动词需要 spec 授权。

spec 未声明 `admin = true` 的角色时，admin 身份不存在。

admin 身份的 control 免 role 边表判定。

admin socket 上的 `onlyne control` 以 admin 身份执行。

非 admin 角色的 `control` 仍要属主身份或 `admin = true` 的边。

角色间没有 control 授权时，代 hop 的 `control cancel` 返回 `forbidden`。

`control cancel` 返回 `forbidden` 是设计行为。

此时恢复入口是 admin 本地 socket 的 repair 族。

```bash
onlyne --server-root <server-root> repair inspect --task <id>
onlyne --server-root <server-root> repair fail --task <id> --reason session_dead
```

第一条命令读取残账和 session 投影。

第二条命令把任务收敛为失败并保留 `session_dead` 原因。

## 宿主资源回收

一条会话结束，它的宿主资源跟着回收：herdr pane、Orca 标签页、zellij session、exec 子进程在“该会话不持任务且无 plugin transport 挂载”时由 client 关闭。会话结清后留在 role tab 里的空 shell 由这三条路径收走，手工 `herdr pane close` 退到兜底位置。

三条触发路径：

- plugin 优雅 `detach`：这条连接服务过的每个空闲会话就地关闭资源。
- 结清且 agent 未挂载：关闭发生在结清那一刻。
- 250 ms readiness tick：扫描被跟踪的会话。一个会话是否结束是派生出来的：存储元组与 `task` 表里该任务的结清值一起过 `project`，得出 `exited` 才关掉，reason 由 task 表的那个结清值推导（`done` 得 `Completed`，`failed` 得 `Fault`，`cancelled` 得 `Cancelled`；`pending` 或无记录不产生 reason）。会话行本身不再存有 lifecycle，也不再自报任务结果。

会话结清后不再接新任务：一单一个 session，槽位随即归还，不再占用 `max_sessions`，宿主资源按上面三条路径收走。一条保留路径：连接在 plugin 未发 `detach` 的情况下断开，该 agent 还可能重连。

每一次回收在存储资源状态仍为开时先经 `backend.attach` 刷新过期 ref，投影 `resource_closed`，并在 client 日志记一行 `retiring idle session resource`，字段是 `task`、`backend`、`resource`、`reason`；槽位随后从跟踪表里移除。关闭失败落一条 warning，run 照常继续。

herdr 的关闭是幂等的：`herdr pane close` 回 `pane_not_found` 记为成功，日志落一行 debug `herdr pane already closed`，字段 `task` 与 `pane`。一个已经消失的 workspace 在此之后读作已关闭。

`stalled` 的含义因此收窄到真实静默：一条已完成的任务带 `no applied progress` 的 `stalled` 从这条面上消失，fault 表里的 `stalled` 只描述仍在跑的会话。判据细节见上一节的进展时钟条目。

## Headless（exec）会话

`exec` 是无头后端的正名；`headless` 只是 parse 别名，投影与事件里的 backend 字符串仍是 `exec`。选择链是 env `ONLYNE_BACKEND`（非空）> 工作区 `config.toml` 的 `backend` > auto。`exec` / `acp` / `fake` 不会被宿主探测选中，须点名启用；`headless` 随 `exec` 同理。

会话子进程的 stdout/stderr 并进 `<workspace>/.onlyne/logs/session-<task>.log`。进程退出时，持柄 `probe` 把该文件尾部最多 200 行（先截约 16KiB 再按整行切）写入 `ResourceProbe.detail.output_tail`；log 缺失或读失败则省略该键，`exit` 码仍在。

关闭阶梯：

- unix：会话是独立进程组。`close` 用 `kill(2)` 只打记录的 pgid（`backend_ref.pgid`，与 leader pid 相同），先 `SIGTERM`，宽限 5 秒后再 `SIGKILL`，最后 `child.kill` 收尸。组信号失败时退回到对 leader pid 的同名信号。pid 0/`-1` 拒绝发送（那是“本进程组 / 一切可杀进程”，不是会话）。从不按 cmdline 通配杀进程。
- windows：spawn 带 `CREATE_NEW_PROCESS_GROUP`，停机先 `GenerateConsoleCtrlEvent(CTRL_BREAK)`，宽限后再 `child.kill()`（TerminateProcess）。客户端没有控制台时 CTRL_BREAK 失败，直接走终止。Windows 没有 SIGTERM；关停由 supervisor 在 server root 所在主机执行 `onlyne server stop`。

`pi --mode rpc` 是这条后端的典型 `session_command`：stdin 由 client 持开（EOF 对 rpc 意味着操作者离开），stdout 进 session log，消息面走 adapter socket，不走子进程的 stdio。

```toml
# <workspace>/.onlyne/config.toml
backend = "headless"

# <server-root>/.onlyne/spec.toml [[client]]
session_command = ["pi", "--mode", "rpc", "--session-id", "{session}"]
```

协议类 `session_command`（`pi --mode rpc`、`agent --acp` 这类在自己的 stdio 上说 JSON-RPC 的命令）只认 `backend = "exec"` 与 `backend = "acp"` 两种配置；写成 `herdr` / `orca` / `zellij` 时 client 在投递处拒绝，拒绝文案作为 reason 落进 ledger。改法是改工作区配置的 `backend`，运行期不会替你换。

## ACP 会话后端

`acp` 是显式选择的后端：它不进入宿主探测的候选集，由 env `ONLYNE_BACKEND=acp` 或工作区 `config.toml` 的 `backend = "acp"` 指定。`session_command` 是该 agent 的 ACP 启动命令，例如 `qoderclicn --acp`。

一个 agent 进程托管该 role 的全部会话，进程按渲染后的命令复用，会话按 agent 分配的 id 区分。

配置面是工作区 `config.toml` 的 `[acp]` 表：

| 配置文件 | 字段 | 默认 | 作用 |
|---|---|---|---|
| `<workspace>/.onlyne/config.toml` 的 `[acp]` | `mode` | 空 | 会话模式，经 `session/set_mode` 交给 agent；空值用 agent 自己的默认 |
| `<workspace>/.onlyne/config.toml` 的 `[acp]` | `model` | 空 | 模型配置项的值；空值用 agent 自己的默认 |
| `<workspace>/.onlyne/config.toml` 的 `[acp]` | `reasoning_effort` | 空 | 推理档位配置项的值；空值用 agent 自己的默认 |
| `<workspace>/.onlyne/config.toml` 的 `[acp]` | `permission` | `deny` | agent 请求权限时本机的答复：`deny` 拒绝并落一条 fault，`allow` 放行 |

被拒的权限请求落一条 `permission` fault，其 reason 列出被拒的工具调用与本机策略；同一任务的终态照常进 ledger。

### 结项报告（payload-v2）

ACP 会话的结项由 client 完成，agent 会话内没有 `onlyne` CLI，也不需要它。`deliver` 在每次投递的 prompt 尾部注入一段报告指令。这段指令首行给出绝对路径 `<workspace>/.onlyne/out/<task-id>.md`，并原样打印整份文法。指令要求 agent 在停止前把结果写进该文件：先写同目录的临时名，再 rename 进位。报告正文的取值沿用 `out_head` 的既有规则：单行、空白折叠、200 字符截断。

文法 v2 规定：一个报告文件由一行 verdict 加零或多行 handoff 组成（v1 的单行文件同样合法）：

| 行 | 含义 |
|---|---|
| `hop-done: <一行结果>` | verdict：任务做完了，正文是结论 |
| `hop-failed: <一句话原因>` | verdict：任务失败，正文是原因 |
| `hop-blocked: <一行阻塞>` | verdict：任务停在外部依赖上，正文是所等之物 |
| `handoff: <目标 role> \| <交给该 role 的一句话>` | 转手一行，可出现零到八条；`\|` 之后可缺省，缺省即把 verdict 正文当交付内容 |

行首 `#` 是注释行，空行忽略。除此之外任何不合文法的行 ⇒ 整份文件 Invalid（fail closed：零转手、零路由）。单文件上限 16 行、handoff 上限 8 条，超限同样 Invalid。CRLF 与裸 CR 先归一为 LF 再逐行分类。

报告目录由 client 在投递前创建。创建失败的那次 prompt 不带指令块，本轮按缺位情形照常结项，journal 在 `dispatch` 记录旁补一条 `warning` 记录。

turn 结束、该轮全部 `session/update` 落账之后，client 读取报告文件一次：先解析，再路由 handoff，最后删文件。结项取值：

| 报告情形 | 结果 |
|---|---|
| 文件缺位或读不到 | 维持契约前的行为：outcome 与 head 由 stopReason 与该轮末条 assistant 文本推出 |
| `hop-done: <非空>` | 报告文本作为 head；outcome 仍由 stopReason 判定，被 stopReason 判为非正常终止的那一轮保持原判；handoff 行照常路由 |
| `hop-failed: <非空>` | outcome 为 failed，报告文本同时是 head 与 fault reason，正常的 `end_turn` 也被降级；handoff 行照常路由 |
| `hop-blocked: <非空>` | outcome 与 head 取报告的阻塞正文，但任务不转手：活没干完，没有可交给下一 role 的东西 |
| Invalid（多余行、未知前缀、超限、空文件、坏 UTF-8） | outcome 为 cancelled，fault reason 以 `acp payload invalid:` 开头并写明类别与行号，head 为空，零转手；文件保留在原地，重写后同一 task 重投即可消费 |

`done|failed|blocked` 三种 verdict 由 `Outcome::Finalized` 的 `head_kind` 字段带上报文层，client 据此区分 blocked 与另两种。

handoff 的路由由 client 用该 role 已有的 server 连接发出，不冒充人类请求。单条路由失败（ACL 拒、目标 role 不在本机连接面上）只记一条 `handoff_denied`：journal 记 `handoff_denied` 事件（带 `to_role` 与拒绝原因），faults 面报同名的 fault。verdict 不删，Outcome kind 不变，其余 handoff 继续。

一条报告可以同时交给至多八个 role，每个 role 拿到属于自己那一条的正文：行内带 `| <一行>` 时收件人读那一句，缺省时读 verdict 正文（`Handoff::text_or`，`crates/onlyne-proto/src/payload.rs:29`）。

hop 记录一次转手在链条上的步深。转手链条的深度按产品初衷保持开放：协议与 server 都对 hop 计数不做上限判定，hop 只作因果记录随信封走（`crates/onlyne-proto/src/envelope.rs:331-339`、`crates/onlyne-server/src/relay.rs:640`）。一个 role 能交给谁由 `spec.toml` 的 `allowed_targets` 边决定，server 的 ACL 闸门按这些边逐条回答每一次 send（`crates/onlyne-server/src/relay.rs:379-393`）。

操作员要收束链条，改法就一条：剪掉对应的 `allowed_targets` 边再 `onlyne reload`。

每次读取都向该任务的 journal 追加一条 `payload` 记录，字段为 `task_id`、`path`、`payload_kind`（取值 `done`、`failed`、`blocked`、`invalid`、`absent` 之一）、`head`、`handoffs`（本轮可读出的转手线条数）。报告被拒时记录再多带一个 `error` 字段，写明拒绝原因与行号；缺位同样记录，账上因此能看出这一轮有没有上报。

每条值得路由的转手线在删除结项文件之前另记一条 `handoff` 记录，字段为 `task_id`、`to_role`、`head`；被拒的转手线在 client 侧落 `handoff_denied`。

结项事实经 `dispatch::on_out` 这一条通路落账：settle、`out_head`、ack 与 completion receipt 都在那里发出。每个终态任务都发 receipt，报告与末条文本都缺位的那次落一条正文为空文本的 `completion` 行。

### 本地校验动词族（`onlyne report`）

同一份文法解析器（`onlyne_proto::payload`）暴露成本地 CLI 动词，全部只读写工作区文件，不开任何 socket：

| 动词 | 行为 |
|---|---|
| `onlyne report path --task <id>` | 打印该任务的结项文件绝对路径，连同 `log:` / `events:` / `content:` 三条会话落盘路径 |
| `onlyne report check --task <id>`（或 `--path <file>`） | 合法：打印 verdict kind、head/reason 与全部 handoff 行，退出 0；非法：`onlyne: <精确原因（含行号）>` 加整份文法进 stderr，退出 2；文件不存在或读不到：stderr 报 `absent`/读失败原因，退出 2（3 只留给 socket 解析） |
| `onlyne report write --task <id> --verdict <done\|failed\|blocked> --head <text> [--handoff <role\|text>]...` | 按部件构造合法报告，临时名 + rename 原子写入，打印最终路径 |
| `onlyne report validate --text <s>`（或 `--from -` 读 stdin） | 对字符串跑同一解析器，不碰文件 |

`--workspace` 的定位与 socket 解析同一惯例：给定目录起、沿祖先目录找 `.onlyne/config.toml`；找不到就用给定目录本身。文法全文内嵌在 `onlyne report --help` 与 `onlyne report check --help` 里，装机用户不查文档也能核格式。

## 会话内容

ACP 会话没有终端：agent 是 client 持有的子进程，会话对 client 之外的进程不可见。留下的面是落盘的 journal。`<workspace>/.onlyne/logs/session-<task>.events.jsonl` 每行一个 JSON 对象，内容是该 agent 的 `session/update` 通知，加上 client 自己的 `dispatch`、`payload` 与 `turn` 记录；`<workspace>/.onlyne/logs/session-<task>.log` 是给人看的渲染件。`<workspace>/.onlyne/logs/content.index.jsonl` 每条记录一行元数据，记下它在任务 journal 里的偏移与长度，role 级的内容序号由此在 client 重启后仍可续。三个都是普通文件，谁在读写它们由本机权限决定，client 不向任何会话外的进程提供实时流。

## Windows 关停

Windows 没有 SIGTERM / SIGHUP。`tokio::signal::windows::ctrl_c` 接到现有 SIGINT 收尾路径。关停由 supervisor 在 server root 所在主机执行 `onlyne server stop`；spec 热加载走 `onlyne reload`。exec 会话子进程的杀阶梯见上一节。

`.onlyne/run/s` 在 Windows 是 marker 文件（内容 `v1:onlyne-<32hex>`），named pipe 名由路径的 lexical-absolute 小写 sha256 派生。`--socket \\.\pipe\` 原样透传。`ERROR_PIPE_BUSY` 在 CLI `--timeout` 内重试。Unix 上 AF_UNIX 仍是文件系统 UDS。
