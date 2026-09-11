# pi-onlyne —— pi 的 onlyne agent 适配器

一个 pi 扩展，让一个 pi 进程承载一个 onlyne role session。它连接
`<role workspace>/.onlyne/run/s`，按 `crates/onlyne-adapter/PROTOCOL.md` 说话，把 session 走完
`hello → welcome → assign → 工作 → complete → detach`。全程无 Rust 代码：协议在 Node 的
`node:net` 之上重写，四字节大端长度前缀 + UTF-8 JSON 的编解码是手写的，运行时零 npm 依赖。

扩展在 onlyne 之外完全静默：客户端在 spawn 时注入 `ONLYNE_ROLE`、`ONLYNE_SESSION_ID`、
`ONLYNE_TASK_ID`（`crates/onlyne-client/src/dispatch.rs`），三者缺一即为普通 pi session，
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
  ├─ probe ──► 一条 heartbeat
  ◀── recycle ──► （未终态则先 complete）→ 停插件 → pi 退出
  └─ pi 退出时 detach{reason}
```

## 1. 安装

这是一个 pi package：`package.json` 里声明 `pi.extensions: ["./src/index.ts"]`，pi 用 jiti
直载 TypeScript，无需构建产物。

### 配合生成的工作区（正规路径）

`onlyne server generate` 会把 `[server].agent_package` 复制进
`<ws>/.onlyne/agent/<pkg-name>/`，并把这条 package 以相对 settings 文件自身的路径
`../.onlyne/agent/<pkg-name>` 写进 `.pi/settings.json`
（`crates/onlyne-server/src/generate.rs`）。这个写法是 pi 0.85.1 真正加载的那一个：项目
`packages` 里的路径以该 settings 文件所在目录（`<ws>/.pi`）为基准解析，于是 `../` 那份落到
`<ws>/.onlyne/agent/<pkg-name>`，裸写的 `.onlyne/agent/<pkg-name>` 会解析成
`<ws>/.pi/.onlyne/agent/<pkg-name>`——包被列出来却不加载。生成的工作区就是 supervisor 拉起
的那份，插件随目录一起走，不装全局。

```toml
# spec.toml
[server]
agent_package = "/abs/path/to/integrations/pi-onlyne"   # 只在 generate 时读一次
```

```bash
onlyne server generate --root <server-root> --out <dir>
```

生成的 `.pi/settings.json` 形如：

```json
{ "packages": ["../.onlyne/agent/pi-onlyne"] }
```

`pi list` 会把这条列在 “Project packages” 下。是否真正加载的验证方式：让 vendored
`index.ts` 抛错，观察报错是否出现。

### 手工（不经过 generate）

```bash
cp -R integrations/pi-onlyne <ws>/.onlyne/agent/pi-onlyne
printf '{"packages":["../.onlyne/agent/pi-onlyne"]}\n' > <ws>/.pi/settings.json
```

### 一次性 / 测试

```bash
pi --session-id <id> -e /abs/path/to/integrations/pi-onlyne -ns -nc
```

### 开关文件

`<cwd>/.pi/onlyne.json`（见 `onlyne.json.example`）：

| 键 | 默认 | 作用 |
| --- | --- | --- |
| `enabled` | `true` | `false` 时该工作区禁用扩展 |
| `watch.autoStart` | `true` | `false` 时注册工具但不建连接，需 `/onlyne connect` |

文件缺失即两个默认值。文件格式错误时打印一行警告并保留默认值——一个笔误不该静默关掉一个
role。client 不读这个文件（计划 §11 已把旧 readiness 门降级为 generate 期模板提示），所以它
的唯一消费者是本扩展；键名沿用模板里既有的形状。

其余无需配置：工作区 `spec.toml` 的 `session_command` 已经按任务拉起 `pi`
（`["pi", "--session-id", "{session}"]`），client 负责注入本扩展识别的环境变量。

## 2. 能力表

`hello` 只声明真实实现的能力：

| 能力 | 声明条件 | 含义 |
| --- | --- | --- |
| `register` | 始终 | `welcome` 之后发 `session_register{session_id, task_id, generation, pid, title}` |
| `report` | 始终 | `report.ready` / `report.heartbeat` / `report.complete` |
| `inject` | `pi.sendUserMessage` 存在 | 载荷以 `assign` 到达，并作为 pi user message 注入 |
| `recycle` | 始终 | 收到 `recycle` 先补终态，再停插件并让 pi 退出 |

降级路径与宿主对应行为：

| 缺失项 | 探测时机 | 行为 |
| --- | --- | --- |
| `registerTool`（老 pi） | `session_start` | 不注册任何工具；协议通路不受影响，`/onlyne status` 仍可用 |
| `sendUserMessage` | `session_start` | capability 里去掉 `inject`，宿主改走 `config_get{key:"stdin:<text>"}`，插件用剩余的注入通道投递 |
| `sendMessage` | `session_start` | `welcome` 的 role prose 不再作为上下文注入；任务本身照常到达 |
| `appendEntry` | `session_start` | 不再写 `onlyne-assign` / `onlyne-complete` 会话条目 |
| `ui.setStatus` | 调用点保护 | 跳过 footer 状态行 |
| `ctx.shutdown` | 调用点保护 | `recycle` 只停插件，不退出 pi |

## 3. 工具面

仅在 onlyne session 内注册。

### `onlyne_send{to, text, kind?, image?}`

经 `send` 帧提交一个 envelope。`kind: "note"`（默认）是自由文本，不带 `op_id`；
`kind: "task"` 是派人办事，因此带 `o-<uuid>` 幂等键和新生成的 `causality.task`。`image` 是
png/jpeg/gif/webp 的绝对路径，读取后 base64 编码成 `body.image`，受核心 2 MiB 上限与四种
mime 约束。

### `onlyne_complete{outcome?, text?}`

显式结束当前任务，`outcome` 缺省 `done`，也可 `failed`。`text` 成为 ledger 的 `head`
（空白折叠，截到 200 字符）。工具返回 `terminate: true`，pi 因此结束本批而不再多跑一轮。

## 4. outcome 判定规则

每个任务只发一次 completion，取以下三者的先到者：

1. **`onlyne_complete`** —— 模型给显式 outcome，优先级最高；同一任务的第二次 completion 被
   拒（不重报）。
2. **`agent_settled`** —— pi 不会自己继续（无重试、无压缩、无排队续跑）。此时：
   - turn 以 provider 错误告终（`stopReason: "error"`）→ `failed`，错误信息当 head；
   - 其余 → `done`，最后一段 assistant 文本当 head；
   - 任务已投递但还没跑过任何 turn → 不发 completion：注入的消息尚未执行，这时报终态就是撒谎。
3. **`recycle{outcome}`** —— 宿主拆 session。先按宿主给的 outcome 结算未终态的任务，再停插件
   并退出 pi。

`head` 恒为单行、上限 200 字符，与 client 写入 `out_head` 和回执携带的内容一致。

completion 在 client 重启时也不丢：如果决定 outcome 时 socket 已断，报告被记住，并在下一次
`hello` 应答后立刻补发。

## 5. 协议说明与偏差

以下每条要么是对 `PROTOCOL.md` 的明确解读，要么是对实际 client 行为的实测。

- **report 序号基址。** 插件自己的 `report` 序号从 1000 起，不是 1。client 把自身的派发事件
  （`created`、资源 attach、`ready`）写进同一个 `(generation, seq)` 水位，reducer 会静默丢弃
  水位之下的报告（`crates/onlyne-session/src/reconcile.rs`），所以从 1 起会丢掉最初的观测。
  其余版本语义与规范一致。
- **`observed` 是完整的 `Observation`。** `report.heartbeat` 携带整个合法状态元组
  （`version`、`generation_live`、`isolate_after`、`terminate_after`、`mismatch_count`、
  `agent`、`delivery`、`resource`、`recovery`、`outcome`、`public`），不是
  `{"state": "running"}` 这种简写：宿主会反序列化它，`is_legal` 不接受的一律拒绝。本插件只
  拥有 `agent` 这一维（turn hooks），`delivery` 保持 `none`、`outcome` 保持 `pending`——在它
  报出 completion 之前这就是它的事实；`resource` 报 `attached`，因为宿主的派发路径已经记过
  这次 attach。
- **`ready` 每连接报一次。** 宿主的 hand-off 路径
  （`crates/onlyne-client/src/dispatch.rs::hand_session`）在把 session 交给挂载的插件时已经报过
  `ready`，插件再报一次在宿主侧是 no-op。仍然发送，因为「先挂载、后有活」正是 ready barrier
  描述的情形，且只花一帧。
- **从不发 `cluster_ref`。** 本插件代表本地 role 说话，从不代表 aggregate；Rust 侧对同一情形
  也是 `skip_serializing_if` 缺省。
- **`probe` 用一条 heartbeat 应答**，对应 `PROTOCOL.md` 里 “`probe` declares fresh resource
  observations”。
- **`config_get` 只有当键以 `stdin:` 开头时按任务正文处理**，这正是 `PROTOCOL.md` 为无
  `inject` 插件记录的重载。其他键记日志后忽略，绝不误读。
- **`frame_too_large` / `bad_frame`**：超限正文在写出任何字节之前就被拒；帧错误关闭连接并重
  连——帧一旦损坏无法重新同步，这与 `crates/onlyne-frame/src/lib.rs` 的结论一致。
- **任务 id 在连接生命周期内一次性使用**：同一任务的重复 `assign` 只回
  `reason: "duplicate"` 的 ack，不重复注入。当今 client 每个任务都是新 uuid，所以这条只在真
  正的重投上生效。

- **pane 申报（Orca tab）。** 在 Orca pane 里，握手完成时插件会写
  `<workspace>/.onlyne/cache/pi-pane.json`，`assign` 时刷新，`bye`、断开与退出时删除。它不是
  协议的一部分：`integrations/orca-plugin` 靠它判断哪些 Orca tab 属于同一个 swarm——pane 会把
  `ORCA_PANE_KEY` / `ORCA_TAB_ID` / `ORCA_TERMINAL_HANDLE` / `ORCA_WORKTREE_ID` 导出给 client
  拉起的进程（2026-09-11 实测，Orca 1.4.198），而 pi 之后没有任何环节能恢复这个绑定。不在 pane
  里、或缓存目录不可写时，写入是静默 no-op：申报永远不会让 session 失败。

## 6. 配置项

| 环境变量 | 必需 | 作用 |
| --- | --- | --- |
| `ONLYNE_ROLE` | 是 | 挂载的 role |
| `ONLYNE_SESSION_ID` | 是 | 挂载的 session id；当前 client 中 session_id 等于 task_id |
| `ONLYNE_TASK_ID` | 是 | 本进程服务的任务；驱动 `session_register` 与首条 `ready` |
| `ONLYNE_SOCKET` | 否 | 覆盖 socket 路径（默认 `<cwd>/.onlyne/run/s`） |

值得记住的常量：心跳 10 秒（`heartbeat_timeout_ms` 是 30 秒）、hello 预算 5 秒、请求超时
30 秒、重连阶梯 1/2/4/8/16/30 秒。

## 7. 故障排查

| 现象 | 原因 | 检查 |
| --- | --- | --- |
| 看不到 `[pi-onlyne] session …` | 三个环境变量缺一，或 `enabled` 为 false | `env \| grep ONLYNE_`；`cat .pi/onlyne.json` |
| `socket error: connect ENOENT …/.onlyne/run/s` | 该工作区没有 `onlyne-client run` | 起 client，或 `onlyne-client status` |
| 反复 `reconnecting in 4000ms` | client 已停或 socket 被替换 | `onlyne --server-root … roles` |
| `ready refused: internal: unknown session for …` | 插件为 client 从未暂存的任务报了 ready（手工起 pi 时的正常现象） | 让 client 拉起 pi，而不是手工起 |
| `assign` 一直不来 | client 的 `session_command` 没能拉起 pi，或 `inject` 被降级 | client 日志里的 spawn 行；`/onlyne status` 看能力集 |
| ledger 停在 `in_flight` | 没有 completion：没跑 turn，或 `agent_settled` 没触发 | pi session 文件里的 `onlyne-assign` / `onlyne-complete` 条目 |
| `hello` 后立刻 `forbidden` / 断连 | mount role 与 client 的 role 不一致 | `hello.args.mount.role` 对该工作区的 role |
| `frame_too_large` | 正文超过 8 MiB | 只会由超限的出站图片触发；上限来自核心 |
| 工具缺失 | 该 pi 版本没有 `pi.registerTool` | `/onlyne status`；对照上面的能力表 |
| 会话在 `exited` 之后又回到 `idle` | completion 之后还落进了一条 heartbeat 快照，带着 `outcome: pending` | 看 session 日志里 `completion` 之后的 report 顺序；插件对已完成任务不再上报 |

`/onlyne status` 打印实时状态（`connected`、`socket`、`role`、`sessionId`、`generation`、
`agentState`、`tasks`、`pendingCompletion`、`lastError` 与计数器）；`/onlyne connect` /
`/onlyne disconnect` 手工开合连接。

## 8. 开发与验证

```bash
cd integrations/pi-onlyne
node --test src/*.test.mjs        # 帧编解码、协议词汇、agent 状态机、配置
```

`src/agent.live.test.mjs` 只在 `target/debug/onlyne-client` 与 `onlyne-server` 存在时运行。
`crates/onlyne-testkit/e2e/pi-live.sh` 是端到端用例：pi 不在 PATH 或没有可用模型凭据时 SKIP
（exit 0），否则用真 client 跑一个真任务到 `acked`。

```bash
cd ../..
ONLYNE_BACKEND=fake BIN_DIR=target/debug bash crates/onlyne-testkit/e2e/pi-live.sh
```

用例在 source 公共 helper 之后自己导出 `ONLYNE_BACKEND=exec`，于是 pi 由 client 亲自 spawn，
stdin 是一条 client 持住不关的管道。agent 自己的输出落在
`<ws>/.onlyne/logs/session-<task>.log`。
