# Operations

Onlyne 运维以 server 账本、client 工作区、admin unix socket 为边界。

## 值守入口

`onlyne status` 通过 admin socket 读取 server 状态。

`onlyne roles` 读取 role 注册表和在线状态。

`onlyne sessions` 读取 session 投影。

`onlyne ledger` 读取投递账本。

`onlyne faults` 读取 faults 表。

`onlyne watch` 读取 durable 与 advisory 事件流。

`onlyne history` 回放事件记录。

`onlyne spec_diff` 对比运行中 spec 与磁盘 spec。

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

repair 族走 `<server-root>/.onlyne/run/s` 的 admin 面。

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

此时恢复入口是 admin unix socket 的 repair 族。

```bash
onlyne --server-root <server-root> repair inspect --task <id>
onlyne --server-root <server-root> repair fail --task <id> --reason session_dead
```

第一条命令读取残账和 session 投影。

第二条命令把任务收敛为失败并保留 `session_dead` 原因。

## Headless（exec）会话

`exec` 是无头后端的正名；`headless` 只是 parse 别名，投影与事件里的 backend 字符串仍是 `exec`。选择链是 env `ONLYNE_BACKEND`（非空）> 工作区 `config.toml` 的 `backend` > auto。`exec` / `headless` / `fake` 不会被宿主探测选中。

会话子进程的 stdout/stderr 并进 `<workspace>/.onlyne/logs/session-<task>.log`。进程退出时，持柄 `probe` 把该文件尾部最多 200 行（先截约 16KiB 再按整行切）写入 `ResourceProbe.detail.output_tail`；log 缺失或读失败则省略该键，`exit` 码仍在。

关闭阶梯：

- unix：会话是独立进程组。`close` 先向组发 `SIGTERM`，宽限 5 秒后再 `SIGKILL`，最后 `child.kill` 收尸。组信号失败时退回到对 leader pid 的同名信号。
- windows：spawn 带 `CREATE_NEW_PROCESS_GROUP`，停机先 `GenerateConsoleCtrlEvent(CTRL_BREAK)`，宽限后再 `child.kill()`（TerminateProcess）。客户端没有控制台时 CTRL_BREAK 失败，直接走终止。Windows 没有 SIGTERM；运维面的优雅停机用 `onlyne shutdown`。

`pi --mode rpc` 是这条后端的典型 `session_command`：stdin 由 client 持开（EOF 对 rpc 意味着操作者离开），stdout 进 session log，消息面走 adapter socket，不走子进程的 stdio。

```toml
# <workspace>/.onlyne/config.toml
backend = "headless"

# <server-root>/.onlyne/spec.toml [[client]]
session_command = ["pi", "--mode", "rpc", "--session-id", "{session}"]
```
