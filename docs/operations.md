# Operations

Onlyne 运维以 server 账本、client 工作区、admin 本地 socket（规范名 `.onlyne/run/s`；路径不超过 103 字节时绑在这一条，超限时绑到系统临时目录下的短派生路径，实际服务的路径记在同目录的 `run/socket` 标记里）为边界。

## 值守入口

`onlyne status` 通过 admin 本地 socket 读取 server 状态。

`onlyne roles` 读取 role 注册表和在线状态。

`onlyne sessions` 读取 session 投影。

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

`reuse = true` 让 settled 槽即时归还容量。

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

`onlyne repair adopt --task <id> --session-id <session> --backend <backend> --reason <reason>` 把任务接到已知 session。

`onlyne repair rebind --task <id> --session-id <session> --backend <backend> --reason <reason>` 重写任务的 backend 绑定。

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

`reason` 的读法：`onlyne ledger` 的行键为 `msg_id`、`task`、`state`、`reason`、`out_head`、`body`；该键只在这一行有值时出现，无值的行与列加入之前逐字节一致。TUI 第二页的 task 详情面板在账本行尾追加 `reason=<text>`。落进这一列的取值：`requeue_exhausted` 与 `requeue_ttl` 来自上面两道闸，`expired` 来自到期扫描，`session_dead` 来自超期残账的结清（见「会话残影与属主判定」一节），pane 后端（`herdr` / `orca` / `zellij`）在开页前拒收协议 `session_command` 时整句拒收文案落 `rejected` 行（见「Headless（exec）会话」一节末段）；操作者经 `onlyne reject --reason` 或 `onlyne repair fail --reason` 自填的文本原样进这一列，`onlyne ack` 收下时该文本随结清事件走，行上的 `reason` 保持原样。字符串 `operator ack` 是 faults 表 `reason` 列的用例数据（`crates/onlyne-store/src/tests.rs` 的 `update_fault_state` 用例），账本列没有它的记录。

push 投递与 pull 投递的 `in_flight` 翻面都各有一条 `ledger_state` 事件；离线读账的 ledger 状态与会话投影在任何采样点互相对得上。

完整链路（server 在活 link 下死亡、client 重连接管、单一会话自然结清）由 `crates/onlyne-testkit/e2e/requeue-claim.sh` 在真实进程上验证。

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

旧 `working` 账由重启后的同 role client 开机自检收敛。

残影判定与冻结上报有四个旋钮：

| 配置文件 | 字段 | 默认 | 作用 |
|---|---|---|---|
| `<workspace>/.onlyne/config.toml` | `stale_grace_secs` | 300 | client 开机自检宽限，单位秒 |
| `<workspace>/.onlyne/config.toml` | `stall_report_secs` | 1800 | 会话投影 tuple 冻结时长上限，client 据此上报 `stalled` fault，0 关闭，单位秒 |
| `<server-root>/.onlyne/spec.toml` 的 `[server]` | `stale_watch_secs` | 60 | server 观察器扫描周期，单位秒；0 关闭观察器 |
| `<server-root>/.onlyne/spec.toml` 的 `[server]` | `heartbeat_grace_secs` | 90 | 属主在线时 `working` 行允许的心跳静默时长，单位秒 |

宽限期内，自检等待 adapter 重挂。

自检跳过仍有活 slot 的 task。

超过宽限期的残账通过 report 路径上报终态。

超期残账的终态是 `failed`。

超期残账的 reason 是 `session_dead`。

server 侧观察器按 `[server].stale_watch_secs` 周期扫描，一次扫描跑两个探测器。

探测器一扫描 `working` 且属主离线的行。

离线探测器超过 600 秒记录 kind 为 `stale_working` 的 fault。

探测器二扫描 `working` 且属主在线的行，判据是心跳新鲜度。

pi 插件每 10 秒发一个 heartbeat 包，client 每收到一拍就重发一次 `session_sync`，服务端行的 `updated_at` 随心跳前进。

静默一拍即翻面的 no-op 心跳同样携带存活事实，client 对其抬升本地版本号并重发，健康会话的 `updated_at` 保持新鲜。

属主在线、行龄超过 `[server].heartbeat_grace_secs`（默认 90 秒）、且本进程见过该行的写入时，探测器二记录 kind 为 `heartbeat_missing` 的 fault。

`heartbeat_missing` 的行同时出现在 `sessions` 答案的 `heartbeat_stale` 字段上，TUI 呈现为 `working+stale`。

同一任务已有未确认的同类 fault 时，探测器二不再重复记录。

server 启动时把当时 `working` 的行全部登记为已见，上一进程遗留的静默行同样可被标记。

completion 落定为 `exited` 之后同 generation 的 heartbeat 把行抬回 `working` 时，观察器记录 kind 为 `heartbeat_after_complete` 的 fault，投影照常应用。

两个探测器都只记 fault 并推送 advisory `Event::Fault`。

`stalled` 由 client 上报，走 role 的 `report` 面：会话的投影 tuple 连续超过 `stall_report_secs` 没有任何一次 `Applied` 变化时，client 发一条 kind 为 `stalled` 的 `Report::Fault`，带 task 与 session 身份。

no-op 心跳抬存活水位，不抬进展水位；`stalled` 只看后者。

`stalled` 的 reason 文本是 `no applied progress`。

一条已结清的任务走不进这条判据：进展时钟只由 task 分配建立，`note_applied` 只刷新已经建立的时钟，plugin 在完成回执之后送出的最后一拍 `agent: "idle"` 心跳落在空处。到期扫描与发送边界各查一次存储 lifecycle，投影为 `exited` 的 task 被抑制并被遗忘；连接释放顺手忘掉它服务过的每个 session 的时钟。

同一冻结 episode 只报一次，下一次 `Applied` 解除去重。`stall_report_secs = 0` 关闭这条判定。

行不翻面：`stalled` 只落 faults 表并推事件，恢复决策留给 supervisor 与 repair 族。

faults 表的 kind 字段保存 `stale_working`、`heartbeat_missing`、`heartbeat_after_complete`、`stalled` 文本。

server 侧观察器不改 ledger 状态。

server 侧观察器不触发 retry。

server 侧观察器不触发 fail。

server 侧观察器遵守 §8 的零政策红线。

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
- 250 ms readiness tick：扫描被跟踪的会话，存储 lifecycle 已投影 `exited`、已存 outcome 有值、且 agent 已离开的会话关闭掉，reason 由该 outcome 推导（`done` 得 `Completed`，`failed` 得 `Fault`，`cancelled` 得 `Cancelled`）。

两条保留路径：连接在 plugin 未发 `detach` 的情况下断开，该 agent 还可能重连；会话已结清、agent 仍挂载，`reuse` 还能把下一个任务交给它。

每一次回收在存储资源状态仍为开时先经 `backend.attach` 刷新过期 ref，投影 `resource_closed`，并在 client 日志记一行 `retiring idle session resource`，字段是 `task`、`backend`、`resource`、`reason`；槽位随后从跟踪表里移除。关闭失败落一条 warning，run 照常继续。

herdr 的关闭是幂等的：`herdr pane close` 回 `pane_not_found` 记为成功，日志落一行 debug `herdr pane already closed`，字段 `task` 与 `pane`。一个已经消失的 workspace 在此之后读作已关闭。

`stalled` 的含义因此收窄到真实静默：一条已完成的任务带 `no applied progress` 的 `stalled` 从这条面上消失，fault 表里的 `stalled` 只描述仍在跑的会话。判据细节见上一节的进展时钟条目。

## Headless（exec）会话

`exec` 是无头后端的正名；`headless` 只是 parse 别名，投影与事件里的 backend 字符串仍是 `exec`。选择链是 env `ONLYNE_BACKEND`（非空）> 工作区 `config.toml` 的 `backend` > auto。`exec` / `acp` / `fake` 不会被宿主探测选中，须点名启用；`headless` 随 `exec` 同理。

会话子进程的 stdout/stderr 并进 `<workspace>/.onlyne/logs/session-<task>.log`。进程退出时，持柄 `probe` 把该文件尾部最多 200 行（先截约 16KiB 再按整行切）写入 `ResourceProbe.detail.output_tail`；log 缺失或读失败则省略该键，`exit` 码仍在。

关闭阶梯：

- unix：会话是独立进程组。`close` 用 `kill(2)` 只打记录的 pgid（`backend_ref.pgid`，与 leader pid 相同），先 `SIGTERM`，宽限 5 秒后再 `SIGKILL`，最后 `child.kill` 收尸。组信号失败时退回到对 leader pid 的同名信号。pid 0/`-1` 拒绝发送（那是“本进程组 / 一切可杀进程”，不是会话）。从不按 cmdline 通配杀进程。
- windows：spawn 带 `CREATE_NEW_PROCESS_GROUP`，停机先 `GenerateConsoleCtrlEvent(CTRL_BREAK)`，宽限后再 `child.kill()`（TerminateProcess）。客户端没有控制台时 CTRL_BREAK 失败，直接走终止。Windows 没有 SIGTERM；运维面的优雅停机用 `onlyne shutdown`。

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

## 会话内容

ACP 会话没有终端：agent 是 client 持有的子进程，会话对 client 之外的进程不可见。留下的面是落盘的 journal。`<workspace>/.onlyne/logs/session-<task>.events.jsonl` 每行一个 JSON 对象，内容是该 agent 的 `session/update` 通知，加上 client 自己的 `dispatch` 与 `turn` 记录；`<workspace>/.onlyne/logs/session-<task>.log` 是给人看的渲染件。`<workspace>/.onlyne/logs/content.index.jsonl` 每条记录一行元数据，记下它在任务 journal 里的偏移与长度，role 级的内容序号由此在 client 重启后仍可续。三个都是普通文件，谁在读写它们由本机权限决定，client 不向任何会话外的进程提供实时流。

## Windows 关停

Windows 没有 SIGTERM / SIGHUP。`tokio::signal::windows::ctrl_c` 接到现有 SIGINT 收尾路径。运维优雅关停走 `onlyne shutdown`；spec 热加载走 `onlyne reload`。exec 会话子进程的杀阶梯见上一节。

`.onlyne/run/s` 在 Windows 是 marker 文件（内容 `v1:onlyne-<32hex>`），named pipe 名由路径的 lexical-absolute 小写 sha256 派生。`--socket \\.\pipe\` 原样透传。`ERROR_PIPE_BUSY` 在 CLI `--timeout` 内重试。Unix 上 AF_UNIX 仍是文件系统 UDS。
