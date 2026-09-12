# supervisor-demo — 最小版集群操作 agent 与流水灯环

整套东西只用 Onlyne 自己的产品面：spec 里的 prose、`generate` 出的工作区、
`onlyne-client run` 拉起的会话、`send` 派活、ledger 收尾。演示没有发明新机制。

集群里有一个操作 agent `_supervisor`，外加一条五元环 `a,b,c,d,e`。`_supervisor`
是用户的集群操作 agent，它的 pi 进程跑在用户自己的终端里（在 Orca 里就是一个
标签页）。它用 `onlyne` CLI 派活、看账、开关集群。环上五个角色归服务器管：每来
一个任务，客户端就拉起一个 pi 会话（在 Orca 里各占一个标签页）；会话跑完即退出，
标签页随之关闭。

## 结构

```
examples/supervisor/
├── README.md                 本文件
├── run.py                    一键脚本（python3 标准库；无 shell 依赖）
└── templates/                进 spec.template_root 的角色模板
    ├── _supervisor/          AGENTS.md + .pi/{settings.json,onlyne.json}
    └── a/ b/ c/ d/ e/        同一份 ring worker 内容
```

运行时树在 `/tmp/onlyne-sup`（`SUP_DEMO_ROOT` 可改）。集群 spec、服务器状态、
六个角色的工作区、记录文件 `lights.txt` 全在那里。路径这么短是因为 macOS 的
unix socket 路径上限只有 104 字节，而 worktree 本身已经深了 93 字节；运行时再进
仓库就会撞上 `bind: Unsupported socket address`。仓库里只留源（模板与脚本）。

## 模型（D15）

- **worker 角色的生命周期归服务器**：除 `_supervisor` 外，每个 `[[client]]` 条目
  都有客户端。任务一到就拉起会话，会话的终态由插件回执。
- **supervisor 的进程归用户和终端**：它就是普通的 pi 会话，用户在标签页里跟它
  说话。服务器从不 spawn、recycle 或 probe 它。它的 `[[client]]` 条目只干两件事：
  登记操作者身份（`admin = true`；admin socket 的 `send` 要求 `--from` 已登记且
  带 admin 位），以及给出 ACL 锚点。它的 key 由 `generate` 一并产出。
- 环上每个成员只认识自己的前后邻居：`allowed_targets` 放下行边和回执边，
  `allowed_senders` 是配套的另一半。`Spec::acl_edges` 只在两边都认这条边时才发射它。

## 一键：`run.py`

```bash
cargo build --workspace                            # 前置
python3 examples/supervisor/run.py up              # 起集群，把 supervisor 标签页推到眼前
python3 examples/supervisor/run.py up "再开一轮"    # 自定义交给 supervisor 的第一句话
python3 examples/supervisor/run.py lights          # 脚本直接派一轮环（派活不经模型）
python3 examples/supervisor/run.py send "RING=a,b,c,d,e FILE=/tmp/onlyne-sup/lights.txt K=1 TOTAL=10"
python3 examples/supervisor/run.py status          # roles / sessions / ledger
python3 examples/supervisor/run.py stop            # 收摊
```

`up` 做的每一步都是产品原话（幂等，可以反复跑）：

1. `onlyne-server init --root /tmp/onlyne-sup` 建集群根，把 `templates/` 拷进
   `/tmp/onlyne-sup/.onlyne/templates`（拷贝时把脚本自己的路径占位符渲染成真值）。
2. 往 spec 里写七条 `[[client]]` 条目：`_supervisor` 带 `admin = true`，
   `a..e` 各带自己的邻居表。密钥先用形状合法的全零占位。
3. `onlyne generate --out /tmp/onlyne-sup/ws` 生成
   `/tmp/onlyne-sup/ws/demo/{_supervisor,a,b,c,d,e}` 六个工作区（含真实密钥与模板
   内容）。脚本用打印出的 `[[client]]` 片段**替换**掉占位条目。密钥进 spec 这一步，
   是脚本照 e2e case 9 的先例替操作者做的；真实部署里这由人决定。
4. `onlyne server start` + `onlyne-client run` ×5：脚本自己把前台 client 放到后台
   （自己的 session，日志在 `<ws>/.onlyne/logs/client.log`），并记下 pid 供 `stop` 用。
5. 等环上五个角色上线，把 `_supervisor` 的 pi 开成一个标签页（`orca terminal
   create`），第一句话就是 `up` 的参数。**脚本不等环跑完**：派活由 supervisor 自己
   发，`up` 打完提示就退出。

每个会话标签页里跑的是完整的交互式 pi TUI（`session_command` 里没有
`--mode rpc`）。任务正文不走 argv：`{task}` 在客户端渲染成任务 **id**，正文由
pi-onlyne 插件在 `assign` 时注入。所以 TUI 里能直接看见模型读任务、干活、交差。

## 一轮流水灯

环是 `RING=a,b,c,d,e`，跑两圈，共 `5 × 2 = 10` 跳。任务文本只有四个字段：

```text
RING=a,b,c,d,e FILE=/tmp/onlyne-sup/lights.txt K=1 TOTAL=10
```

`a` 收到 K=1：追加 `1:a` 到记录文件，再用 `onlyne handoff` 把同样的文本交给 `b`，
K 加一；`b` 追加 `2:b` 交给 `c`……`e` 到了 K=10 不再转发，它读文件、用
`onlyne_complete` 把文件内容交回。账本里每个任务一行（根任务加九个 handoff 子任务），
每个子任务都记着自己的 `parent_task` 和 `hop`。`handoff` 走产品自己的路径：回读
父任务行，把新任务挂在 `parent_task` 下，`hop` 取父行加一。

整条链要冷启动 `len(RING) * 2` 次 pi，再加十轮模型回合，跑完按分钟计。`run.py`
的默认任务是让 supervisor 去开这一轮；记录文件长到 `TOTAL` 行，灯就全亮。

## 实地观看

这套演示的看点就是它真的在动。在 Orca 标签页里跑 `run.py up`，你会看到：

1. **supervisor 标签页**：标题是 `onlyne-supervisor`，里面是交互式 pi，cwd 是
   `ws/demo/_supervisor`，所以它的 `AGENTS.md` 和集群路径都已经加载好。它先用
   `onlyne ... send --from _supervisor --to a` 派活，再用 `wc -l lights.txt` 和
   `ledger --task <id>` 盯进度，最后把文件内容和账本行报回来。这个标签页跑完不关，
   **继续跟它说话**就行：`再派一单`、`看看账本`、`停掉 planner 之外的角色`。
2. **环上的会话标签页**：每跳一个 pi 会话，按 `a → b → c → d → e → a → …` 挨个亮起。
   标签页里是完整的 pi TUI（初始会话，正文由适配器投进来），能看见模型读任务、用
   `echo` 追加一行、再 `handoff` 给下一个字母；会话完成时 pi 自己退出，Orca 跟着
   回收标签页。十跳就是十次亮灭，这是演示的视觉主体。`lights.txt` 实时变长。
   `onlyne --server-root /tmp/onlyne-sup sessions --json` 的每一行都带
   `observed.host.orca.pane_key`，也就是这条会话绑定的真实 pane：
   `pane_key = "<tabId>:<leafId>"`。
3. **TUI**：另开一个标签页，跑 `target/debug/onlyne-tui --server-root /tmp/onlyne-sup`。
   第 1 页是角色网络：五个环成员在线，`_supervisor` 显示离线——这里的"离线"只表示
   "没有客户端"，它这个进程活在 Orca 标签页里。第 2 页是逐任务的 swarm 视图，
   十行按 hop 排开，状态跟着 ack 往前走。按键看 TUI 自己的帮助行。
4. **收摊**：`run.py stop` 关掉它开的 supervisor 标签页（关完会再查一遍，没关掉就
   报错），清掉留在 Orca 里的会话标签页，再 SIGTERM 掉脚本自己拉起的五个 client
   和服务器。

两种会话载体是同一个协议的两种摆法。默认（Orca 机器上）是 TUI：标签页给人看，
初始会话由适配器注入任务正文。无头场合（CI、没有 Orca）用 `export ONLYNE_BACKEND=exec`，
会话变成客户端自己的子进程，supervisor 也作为子进程把输出写进
`/tmp/onlyne-sup/logs/supervisor.log`。想在带标签页的机器上走脚本驱动的 stdin 管道，
把 `session_command` 换回 rpc 形态即可（`pi --mode rpc --session-id {session} --session-dir .pi/sessions -ns`）。
代价是标签页里只剩 JSON 流，好处是宿主能用 stdin 推后续轮次。两条路的任务正文投递
一样，argv 都不是正文的通道。

## 单独跑某个角色

生成之后，工作区就是普通的 Onlyne 工作区，模板脚本只负责把它们摆好：

```bash
onlyne --server-root /tmp/onlyne-sup roles --json
onlyne --server-root /tmp/onlyne-sup ledger --task <task id>
onlyne --server-root /tmp/onlyne-sup watch --follow     # 看这一单的流转
```

会话列表把 pane 绑定原样带出来：

```bash
onlyne --server-root /tmp/onlyne-sup sessions --json | jq '.data[0].observed'
```

## 看板接线（可选）

把 `onlyne-board` 的 `serverRoots` 指向集群根（`~/.config/onlyne-sessions/config.json`），
刷新就得到连接视图：标签页 ∩ 活着的会话，亮着的只有连着这个 workspace 的 pane。

## 边界

- 演示证明委派机制：任务下发、跨角色消息、handoff 的父子账、回执闭环、账本可查。
  模型答什么不在验证范围。
- 会话按任务生灭（`reuse = false`）。pi 冷启动每单要几秒，这是这套架构的固有成本。
- `_supervisor` 的入口是管理员面（admin socket）。这不是演示的例外：操作 agent
  本来就该拿本机 admin socket 的访问权。
- 环首把根任务的完成回执发给 `_supervisor`，这是内置的上行通道。spec 里环成员的
  `allowed_targets` 只含环内成员，所以回执不走那条边，而是走任务的出发角色。
  `_supervisor` 离线时，回执在账本里排队（`state = queued`）。这批排队行就是操作
  agent 的收件箱：接上客户端就被拉走并结算；操作者也可以先用 `ledger` 看清，再决定
  怎么收尾。
- Windows 适配是 daemon IPC 的事。run.py 本身不依赖 shell。
