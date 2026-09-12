# pi-onlyne —— pi 的 onlyne agent 适配器

一个 pi 扩展：一个 pi 进程承载一个 onlyne role session。它连接
`<role workspace>/.onlyne/run/s`，按 `crates/onlyne-adapter/PROTOCOL.md` 通信，带 session 走完
`hello → welcome → assign → 工作 → complete → detach`。全程没有 Rust 代码：协议在 Node 的
`node:net` 上重写，四字节大端长度前缀加 UTF-8 JSON 的编解码是手写的，运行时零 npm 依赖。

扩展在 onlyne 之外完全静默。客户端 spawn 进程时会注入 `ONLYNE_ROLE`、`ONLYNE_SESSION_ID`、
`ONLYNE_TASK_ID`（`crates/onlyne-client/src/dispatch.rs`）；三者缺一，就是普通 pi session，
插件不注册任何工具、不打开任何 socket。

```
pi session（由 onlyne-client spawn）
  │  环境变量：ONLYNE_ROLE / ONLYNE_SESSION_ID / ONLYNE_TASK_ID
  │  .pi/onlyne.json：{ "enabled": true, "watch": { "autoStart": true } }
  ▼
hello{protocol:1, plugin:"pi-onlyne", kind:"agent", capabilities:[…], mount:{role,session,task_id,pid}}
  ◀── welcome{role, prose, generation, server, host_capabilities}
  ├─ prose ──► 注入 pi 上下文一次（custom message，不触发 turn）
  ├─ report.ready ──► 载荷等待的那道 barrier
  ◀── assign{envelope, prose, task_id, generation}
  ├─ 任务文本（含图片路径）──► pi user message（deliverAs:"followUp"）
  ├─ assign_ack{accepted:true}
  ├─ report.heartbeat{running|idle} —— 每个 turn，以及任务存续期间每 10 秒
  ├─ report.complete{outcome, head} —— ledger 的终态事实
  │    └─ client 的应答就是交接点：插件据此让 pi 退出，随后 detach
  ├─ probe ──► 一条 heartbeat
  ◀── recycle ──► （未终态则先 complete）→ 停插件 → pi 退出
  └─ pi 退出时 detach{reason}
```

## 1. 安装

这是一个 pi package：`package.json` 里声明 `pi.extensions: ["./src/index.ts"]`，pi 用 jiti
直接加载 TypeScript，不需要构建产物。

### 配合生成的工作区（正规路径）

`onlyne server generate` 会把 `[server].agent_package` 复制进
`<ws>/.onlyne/agent/<pkg-name>/`，再把这条 package 写进 `.pi/settings.json`，路径相对
settings 文件自身：`../.onlyne/agent/<pkg-name>`（`crates/onlyne-server/src/generate.rs`）。
pi 0.85.1 只加载这个写法。项目 `packages` 里的路径以 settings 文件所在目录（`<ws>/.pi`）
为基准解析，所以 `../` 那份落到 `<ws>/.onlyne/agent/<pkg-name>`；裸写的
`.onlyne/agent/<pkg-name>` 会解析成 `<ws>/.pi/.onlyne/agent/<pkg-name>`，包被列出来却不
加载。生成的工作区就是 supervisor 拉起的那份，插件随目录一起走，不装全局。

```toml
# spec.toml
[server]
agent_package = "/abs/path/to/plugins/onlyne-agent-pi"   # 只在 generate 时读一次
```

```bash
onlyne server generate --root <server-root> --out <dir>
```

生成的 `.pi/settings.json` 形如：

```json
{ "packages": ["../.onlyne/agent/pi-onlyne"] }
```

`pi list` 会把这条列在 “Project packages” 下。要验证真的加载了，就让复制进来的 `index.ts`
抛错，看报错是否出现。

### 手工（不经过 generate）

```bash
cp -R plugins/onlyne-agent-pi <ws>/.onlyne/agent/pi-onlyne
printf '{"packages":["../.onlyne/agent/pi-onlyne"]}\n' > <ws>/.pi/settings.json
```

### 一次性 / 测试

```bash
pi --session-id <id> -e /abs/path/to/plugins/onlyne-agent-pi -ns -nc
```

### 开关文件

`<cwd>/.pi/onlyne.json`（见 `onlyne.json.example`）：

| 键 | 默认 | 作用 |
| --- | --- | --- |
| `enabled` | `true` | `false` 时该工作区禁用扩展 |
| `watch.autoStart` | `true` | `false` 时注册工具但不建连接，需 `/onlyne connect` |

文件缺失即两个默认值。文件格式错误时打印一行警告，并保留默认值：一个笔误不该静默关掉一个
role。client 不读这个文件（计划 §11 已把旧 readiness 门降级为 generate 期模板提示），所以
只有本扩展消费它；键名沿用模板里既有的形状。

其余无需配置。工作区 `spec.toml` 的 `session_command` 已经按任务拉起 `pi`
（`["pi", "--session-id", "{session}"]`），client 负责注入本扩展识别的环境变量。

## 2. 能力表

`hello` 只声明真实实现的能力：

| 能力 | 声明条件 | 含义 |
| --- | --- | --- |
| `register` | 始终 | `welcome` 之后发 `session_register{session_id, task_id, generation, pid, title}` |
| `report` | 始终 | `report.ready` / `report.heartbeat` / `report.complete` |
| `inject` | `pi.sendUserMessage` 存在 | 载荷以 `assign` 到达，并作为 pi user message 注入 |
| `recycle` | 始终 | 收到 `recycle` 先补终态，再停插件并让 pi 退出 |

缺了某个 pi API 时会怎样，宿主怎么应对：

| 缺失项 | 探测时机 | 行为 |
| --- | --- | --- |
| `registerTool`（老 pi） | `session_start` | 不注册任何工具；协议通路不受影响，`/onlyne status` 仍可用 |
| `sendUserMessage` | `session_start` | capability 里去掉 `inject`，宿主改走 `config_get{key:"stdin:<text>"}`，插件用剩余的注入通道投递 |
| `sendMessage` | `session_start` | `welcome` 的 role prose 不再作为上下文注入；任务本身照常到达 |
| `appendEntry` | `session_start` | 不再写 `onlyne-assign` / `onlyne-complete` 会话条目 |
| `ui.setStatus` | 调用点保护 | 跳过 footer 状态行 |
| `ctx.shutdown` | 调用点保护 | `recycle` 与 completion 照常结算任务；进程留给操作者自己关闭 |

## 3. 工具面

仅在 onlyne session 内注册。

### `onlyne_send{to, text, kind?, image?}`

经 `send` 帧提交一个 envelope。`kind: "note"`（默认）是自由文本，不带 `op_id`。
`kind: "task"` 是派人办事，因此带 `o-<uuid>` 幂等键和新生成的 `causality.task`。`image` 是
png/jpeg/gif/webp 的绝对路径：插件读出内容，base64 编码后挂成 `body.image`。核心限 2 MiB，
只收四种 mime。

### `onlyne_complete{outcome?, text?, force?, reason?}`

显式结束当前任务，`outcome` 缺省 `done`，也可 `failed`。`text` 非空时就是 ledger 的 `head`，
原样写出：空白折叠成单行，截到 200 字符。`text` 缺失或全空白时不带摘要，completion 退回
最后一段 assistant 文本。这一调用同时结束所在 session 的进程。client 应答完 completion
报告（见 §4）之后，插件通过 `ctx.shutdown()` 让 pi 退出。pi 0.85.1 没有 tool-result
`terminate` 处理。工作区带接力策略（§5）时，`force: true` 加非空 `reason` 是绕过一个仍欠着的
接力的正规通道。

## 4. outcome 判定规则

插件每个任务只发一次 completion，取以下三者的先到者：

1. **`onlyne_complete`** —— 模型给显式 outcome，优先级最高；同一任务的第二次 completion 被
   拒（不重报）。`text` 非空时即 head，原样写出。
2. **`agent_settled`** —— pi 不会自己继续：没有待重试、待压缩或排队续跑。此时：
   - turn 以 provider 错误告终（`stopReason: "error"`）→ `failed`，错误信息当 head；
   - 其余 → `done`，最后一段 assistant 文本当 head；
   - 任务已投递但还没跑过任何 turn → 不发 completion。注入的消息尚未执行，这时报终态就是撒谎。
3. **`recycle{outcome}`** —— 宿主拆 session。插件先按宿主给的 outcome 结算未终态的任务，再停
   插件并退出 pi。

`head` 恒为单行、上限 200 字符，与 client 写入 `out_head` 和回执携带的内容一致。每个任务的
head 只有一个来源：显式 `onlyne_complete` 带的 `text`（有则原样采用），否则是最后一段
assistant 文本。自动规则就是那条退路：它报的是自己那一轮的文字，工具调用之后再说的话，顶不掉
调用交出的内容。

报出去的 completion 会结束所在 session 的进程。`report.complete` 以请求形式发出，client 只有
在结算 session 行、ack 掉投递、并写好 `Completion` envelope 之后才应答，插件就在这个应答处
让 pi 退出。socket 当时送不出去的 outcome 会被记住，并在下一次 `hello` 后补发，那次补发的
应答就是结束进程的交接点。被宿主拒掉的 completion 不会让进程退出，任务不会因为退出而丢失。

最后一条上报是：在已结算的 outcome 旁边带一个 `agent: "idle"` 的观测，发在 completion 被
ack 之后、进程退出之前。completion 是按 client 手里的元组结算 session 行的，而收尾那一轮
就是最后一次 heartbeat 时，这个元组读到的仍是 `running`；此后没有任何东西再观测这个进程，
所以缺了这条上报，已退出的 session 会一直说 `running`。最后一次心跳本来就是 idle 时，插件
跳过这条；已结算的观测被拒，也不拖着 completion 挣来的那次退出不走。

## 5. 接力守卫

会话可以一件活都没交出去，就把 `done` 报掉。守卫堵的就是这个事故：一个 bench 会话边叙述进度边
调 `onlyne_complete`，四个 todo 一个没动，下游 writer 永远等一条从未发出的接力。判据只是投递
事实——某个 role 有没有被触达——绝不看发出去的文本长什么样、写得好不好。

策略文件放在插件自己的 `package.json` 旁边，因此随 generate 出的工作区一起被带进去：生成的工作
区里是 `<ws>/.onlyne/agent/pi-onlyne/relay.toml`，手工安装则是插件目录下的 `relay.toml`。

```toml
relay_required = ["writer"]        # 这些 role 必须收到过接力
relay_required_count = 2           # ……或至少这么多个不同的下游 role
```

两个键同时存在时以 `relay_required` 为准。

策略属于 spec，不属于 vendor 目录。`onlyne generate --force` 会重写本插件被拷进去的那份副本，
连带抹掉手写的 `relay.toml`；所以在 `[[client]]` 条目里写一次，client 就会把它注入到它拉起的
每一个 session 进程：

```toml
[[client]]
role = "planner"
relay_required = ["writer"]        # 这些 role 必须收到过接力
relay_count = 2                    # ……或至少这么多个不同的下游 role
```

来源优先级是 `环境变量 > relay.toml > 都没有`：`ONLYNE_RELAY_REQUIRED`（名单，逗号分隔）与
`ONLYNE_RELAY_COUNT`（数量，十进制）就是 client 按上面的条目填进去的两个变量；只有环境变量
一个都没给出策略时，才去读 `package.json` 旁边的 `relay.toml`；两者都没有 = 无守卫。spec 两个键
都写时 client 两个变量都注入，仍然以名单为准。手写的 `relay.toml` 仍是手工安装的逃生门——服务
那些 spec 里根本没写策略的机器——被环境变量盖住的文件则完全不参与。设了但解析不了的变量，会在
stderr 告警并忽略，把机会让回文件。

| | |
| --- | --- |
| 默认 | 两个来源都没给策略 = 无守卫，completion 路径与守卫存在之前逐字节相同 |
| 判据材料 | 本会话自己成功 `onlyne_send` 触达过的 role，`note` 与 `task` 都算；被 client 拒掉的 envelope 不算 |
| 拒绝 | `onlyne_complete` 抛 `onlyne: relay guard: missing handoff to: writer (…)`，点名缺哪条边、怎么解除 |
| 拒绝之后 | 不上报、不排队、不 detach：session 仍然挂着，补上接力后同一次调用即可落地 |
| 名单模式 | 名单里每个 role 都要字面出现在已投递集合里 |
| count 模式 | 数不同的下游 role；发给本 role 自己、或回指派活的上游，都不算一个 |
| 作用域 | 本会话自己的投递，仅进程内存：重连不丢，会话重启从空开始，不去猜上一个进程发过什么 |
| 豁免 | `force: true` 加非空 `reason`；只在守卫拒绝时才起作用 |
| 审计 | 被豁免的 completion，ledger head 以 `relay-guard-forced: <reason>` 开头；调用带了 `text` 时紧接其后 |
| 不管的路 | 自动终态：`agent_settled` 与 `recycle{outcome}` 照旧结算欠着接力的任务 |

`relay.toml` 是 TOML 的封闭子集：扁平的 `key = value` 行、上面两个键、单行双引号字符串数组、
`#` 注释。子集之外一律 stderr 告警并忽略。它刻意不放 `.onlyne/config.toml`：client 以
`deny_unknown_fields` 解析那个文件，插件往里加键会让 client 直接起不来。

没有策略时，`force` 与 `reason` 两个参数是惰性的。

## 6. 协议说明与偏差

下面每条要么是对 `PROTOCOL.md` 的明确解读，要么是在实际 client 上实测到的行为。

- **report 序号基址。** 插件自己的 `report` 序号从 1000 起，不是 1。client 把自身的派发事件
  （`created`、资源 attach、`ready`）写进同一个 `(generation, seq)` 水位，reducer 会静默丢弃
  水位及以下的报告（`crates/onlyne-session/src/reconcile.rs`），所以从 1 起会丢掉最初的观测。
  其余版本语义与规范一致。
- **`observed` 是完整的 `Observation`。** `report.heartbeat` 携带整个合法状态元组
  （`version`、`generation_live`、`isolate_after`、`terminate_after`、`mismatch_count`、
  `agent`、`delivery`、`resource`、`recovery`、`outcome`、`public`），不是
  `{"state": "running"}` 这种简写。宿主会反序列化它，`is_legal` 不接受的一律拒绝。本插件只管
  `agent` 这一维（turn hooks），`delivery` 保持 `none`、`outcome` 保持 `pending`——在它报出
  completion 之前这就是它的事实。`resource` 报 `attached`，因为宿主的派发路径已经记过这次
  attach。
- **`ready` 每连接报一次。** 宿主的 hand-off 路径
  （`crates/onlyne-client/src/dispatch.rs::hand_session`）在把 session 交给挂载的插件时已经报过
  `ready`，所以插件再报一次在宿主侧是 no-op。插件仍然发送：先挂载、后有活正是 ready barrier
  描述的情形，而且只花一帧。
- **从不发 `cluster_ref`。** 本插件代表本地 role 说话，从不代表 aggregate；Rust 侧出于同样的
  原因把该字段写成 `skip_serializing_if` 缺省。
- **`probe` 用一条 heartbeat 应答**，对应 `PROTOCOL.md` 里 “`probe` declares fresh resource
  observations”。
- **`config_get` 只有当键以 `stdin:` 开头时按任务正文处理**，这正是 `PROTOCOL.md` 为无
  `inject` 插件记录的重载。其他键记日志后忽略，绝不误读。
- **`frame_too_large` / `bad_frame`**：超限正文在写出任何字节之前就被拒；帧错误关闭连接并重
  连。帧一旦损坏无法重新同步，这与 `crates/onlyne-frame/src/lib.rs` 的结论一致。
- **任务 id 在连接生命周期内一次性使用**：同一任务的重复 `assign` 插件只回
  `reason: "duplicate"` 的 ack，不重复注入，并记住这个 id 直到连接结束。当今 client 每个任务
  都是新 uuid，所以这条只在真正的重投上生效。

- **pane 绑定（Orca tab）。** 在 Orca pane 里，插件在每个 heartbeat 上报自己跑在哪：报告
  `Observation` 里的 `observed.host.orca.pane_key`（`crates/onlyne-session/src/host.rs`），环境
  报得出时还带上 `tab_id` / `leaf_id` 和终端的 `handle`。这个绑定是**继承**来的，不是猜的：Orca
  pane 会把自己那四个 `ORCA_PANE_KEY` / `ORCA_TAB_ID` / `ORCA_LEAF_ID` / `ORCA_TERMINAL_HANDLE`
  导出给它启动的命令（2026-09-11 实测，Orca 1.4.198），而 client 会把自己的环境继续传给
  session 命令。所以跑在 pane 里的那个进程，是唯一能从内部说出「这是哪个 pane」的组件；pi
  之后没有任何环节能恢复这个绑定。不在 pane 里时 `host` 键整个缺席：普通终端上的 pi 报的是
  一条没有 host 字段的 observation，而不是一条 pane 为空的。
- **为此不往 workspace 写任何东西。** 已经没有申报文件了：绑定搭在 client 本来就逐帧镜像的
  那份 observation 上。没有东西会创建它，所以不存在过期的申报，workspace 的缓存目录也不会
  被碰。这既让 `integrations/orca-plugin` 能不读任何路径就把 tab 轴收窄到真会话，也让
  supervisor 在会话 *结束之后*仍然说得出它跑在哪：`report.complete` 会把 `host` 带过去。

## 7. 配置项

| 环境变量 | 必需 | 作用 |
| --- | --- | --- |
| `ONLYNE_ROLE` | 是 | 挂载的 role |
| `ONLYNE_SESSION_ID` | 是 | 挂载的 session id；当前 client 中 session_id 等于 task_id |
| `ONLYNE_TASK_ID` | 是 | 本进程服务的任务；驱动 `session_register` 与首条 `ready` |
| `ONLYNE_SOCKET` | 否 | 覆盖 socket 路径（默认 `<cwd>/.onlyne/run/s`） |
| `ONLYNE_RELAY_REQUIRED` | 否 | 该 role 在 spec 里的 `relay_required`，逗号分隔：守卫的名单模式（§5） |
| `ONLYNE_RELAY_COUNT` | 否 | 该 role 在 spec 里的 `relay_count`：守卫的 count 模式，只在名单为空时起作用（§5） |
| `ORCA_PANE_KEY` | 否 | 本进程跑在哪（`<tab_id>:<leaf_id>`），每个 heartbeat 以 `observed.host.orca.pane_key` 上报；不在 Orca pane 里时未设置，这也是该字段缺席的原因 |
| `ORCA_TAB_ID` / `ORCA_LEAF_ID` | 否 | pane 的两个 id；只设了 pane key 时插件会自己解析 |
| `ORCA_TERMINAL_HANDLE` | 否 | 终端 handle，随 pane key 一起上报为 `host.orca.handle`，也是 `orca terminal switch` 要的那个值 |

值得记住的常量：插件每 10 秒发一次心跳（`heartbeat_timeout_ms` 是 30 秒），`hello` 最多等
5 秒，单次请求超时 30 秒，重连按 1/2/4/8/16/30 秒阶梯退避。

插件自己读两个文件：`<cwd>/.pi/onlyne.json`（开关，§1）与 `package.json` 旁边的
`relay.toml`（接力策略的兜底，只在 client 没注入策略时才读，§5）。

## 8. 故障排查

| 现象 | 原因 | 检查 |
| --- | --- | --- |
| 看不到 `[pi-onlyne] session …` | 三个环境变量缺一，或 `enabled` 为 false | `env \| grep ONLYNE_`；`cat .pi/onlyne.json` |
| `socket error: connect ENOENT …/.onlyne/run/s` | 该工作区没有 `onlyne-client run` | 起 client，或 `onlyne-client status` |
| 反复 `reconnecting in 4000ms` | client 已停或 socket 被替换 | `onlyne --server-root … roles` |
| `ready refused: internal: unknown session for …` | 插件为 client 从未暂存的任务报了 ready（手工起 pi 时的正常现象） | 让 client 拉起 pi，而不是手工起 |
| `assign` 一直不来 | client 的 `session_command` 没能拉起 pi，或 `inject` 被降级 | client 日志里的 spawn 行；`/onlyne status` 看能力集 |
| ledger 停在 `in_flight` | 没有 completion：没跑 turn，或 `agent_settled` 没触发 | pi session 文件里的 `onlyne-assign` / `onlyne-complete` 条目 |
| `onlyne_complete` 回答 `relay guard: missing handoff to: …` | 工作区的 spec（或顶替它的 `relay.toml`）点名了一个本会话从未触达的 role | 插件 stderr 的 `relay guard from …` 说明来源、`required=…` 说明策略；`relay guard: missing handoff …` 列出已投递集合 |
| `hello` 后立刻 `forbidden` / 断连 | mount role 与 client 的 role 不一致 | `hello.args.mount.role` 对该工作区的 role |
| `frame_too_large` | 正文超过 8 MiB | 只会由超限的出站图片触发；上限来自核心 |
| 工具缺失 | 该 pi 版本没有 `pi.registerTool` | `/onlyne status`；对照上面的能力表 |
| 会话在 `exited` 之后又回到 `idle` | completion 之后还落进了一条 heartbeat 快照，带着 `outcome: pending` | 看 session 日志里 `completion` 之后的 report 顺序；插件对已完成任务不再上报 |
| supervisor 看板一个 tab 都不列 | 没有 live session 上报过 pane：适配器版本早于这条上报，或这个 pi 不在 Orca pane 里 | `onlyne --server-root … sessions --json` 看 `projection.observed.host.orca.pane_key`；在 pane 里跑 `env \| grep ORCA_` |

`/onlyne status` 打印实时状态（`connected`、`socket`、`role`、`sessionId`、`generation`、
`agentState`、`tasks`、`pendingCompletion`、`lastError` 与计数器）；`/onlyne connect` /
`/onlyne disconnect` 手工开合连接。

## 9. 开发与验证

```bash
cd plugins/onlyne-agent-pi
node --test src/*.test.mjs        # 帧编解码、协议词汇、agent 状态机、配置、接力守卫
```

`src/agent.live.test.mjs` 只在 `target/debug/onlyne-client` 与 `onlyne-server` 存在时运行。
`crates/onlyne-testkit/e2e/pi-live.sh` 是端到端用例：pi 不在 PATH 或没有可用模型凭据时 SKIP
（exit 0），否则用真 client 跑一个真任务到 `acked`。

```bash
cd ../..
ONLYNE_BACKEND=fake BIN_DIR=target/debug bash crates/onlyne-testkit/e2e/pi-live.sh
```

用例先 source 公共 helper，再自己导出 `ONLYNE_BACKEND=exec`，于是 pi 由 client 亲自 spawn，
stdin 是一条 client 持住不关的管道。agent 自己的输出落在
`<ws>/.onlyne/logs/session-<task>.log`。
