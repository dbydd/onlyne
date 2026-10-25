# OMP 扩展 API 调查

状态：参考材料，非规范。本文记录 OMP 宿主实际提供的扩展接口与限制，供 Onlyne 接入核对。接口证据来自已安装的 OMP 18.12.6 源码及当时的 todo 提供者；安装包自带 `src/`，行号指向该版本。调查结论不是契约，也不构成运行证据：首版接入仍需实跑验收场景才能宣称实现通过。

来源：2026-10-25 从 RFC 0008 §8「OMP 接线证据」迁出。OMP 接入的规范性要求见 OMP 接入契约（`docs/contracts/omp.md`），协议侧的运行时绑定见 [RFC 0007 §5](../rfcs/0007-bindings.md)。

## 1. 宿主接口与限制

| 能力 | 实际接口 | 对接入的含义 |
|---|---|---|
| 注册工具 | `ExtensionAPI.registerTool`，`execute(toolCallId, params, signal, onUpdate, ctx)` | 工具可以持有库的 Promise 并响应中止信号 |
| 工具中断 | `ToolDefinition` 未声明 `interruptible`，运行时只机械转发自有属性 | 不依赖未声明的中断字段；用 `signal` 和库自己的取消路径 |
| 主动注入 | `sendMessage(customMessage, {deliverAs})` 注入自定义消息；`appendEntry` 只持久化本地条目 | 不调用 `sendUserMessage`；`sendMessage` 返回 `void`，调用结束不提供消费确认 |
| 注入调度 | `deliverAs` 取 `steer`、`nextTurn`、`aside`、`followUp`；忙闲由 `isStreaming` 与 plan 模式决定 | 来信排队与唤醒必须按这个矩阵设计 |
| 新上下文 | `createAgentSession` 支持 `agentId`、`taskDepth`、`enableIrc` | 新上下文需要自己的身份，不能复用父会话身份 |
| 进程内投递 | `AgentSession.deliverIrcMessage` 返回 `injected` 或 `woken` | IRC 总线另有 revived/failed，均不证明模型已读或业务完成 |
| 等待 | `IrcBus.wait(agentId, {from}, timeoutMs, signal)` | 可作为进程内唤醒通道，不替代 Onlyne 的业务答复等待 |
| 任务派发 | `task` 不复用子会话，重复 `name` 得到 `-2`；子代理 id 在 `details.progress[].id` 或 `details.results[].id` | 上下文登记在模型看到结果之前完成 |
| 补充投递 | `executeSend({registry, senderId}, {to, message})`，父级发送转为 steer | 追加内容进入原会话，不创建第二个子代理 |
| 子代理身份 | `ctx.agent` 提供 `kind`、`id`、`depth`、`parentId` | 主会话与子代理共用扩展代码，按 `kind` 分权 |
| 插件设置页 | `package.json` 的 `omp.settings`；读写 `~/.omp/plugins/omp-plugins.lock.json`；项目覆盖文件只读 | 页面写入用户级配置，项目级配置需由插件自建界面 |
| Tern 渲染 | `TERM_PROGRAM=tern` 探测加 TSP `hello`；组件实现 `describe(cx)` | 自定义面板与工具视图需要原生描述，未实现时回退逐行输出 |
| todo 写入 | `pi.appendEntry("mouriya.omp.todo-more", snapshot)` 加 `session.setTodoPhases` | 保留 goal 和 explanation；人类可读来源写进 explanation 首段 |
| todo 提醒 | `omp-todo-more` 的 stop、turn-end 与 plan-check 提醒 | 提醒归提供者；原生 todo 已关闭 |

## 2. 注入调度细节

源码位置：`session/irc-bridge.ts` 与 `session/agent-session.ts`。

- 流式期间，来自父会话的消息走 steer，其他消息压入下一步边界的注入队列。两者都回报 `injected`。
- 非流式的 plan 模式只追加消息，不启动回合。
- 空闲时 `aside` 会启动回合，除非处于 plan 模式或用户刚打断。
- 默认 `deliverAs` 只在调用方要求时启动回合，否则只追加并持久化。
- `isStreaming` 包含已受理但尚未派发的提交，因此“忙”不等于模型正在生成。

接入按 SystemOne 动作选择宿主调用，不对全部来信固定使用 `aside`。扩展的 void 返回只支持记录提交尝试。宿主未提供通用的模型消费确认；消费边界未接通时保留未确认记录，不能凭调用成功推进游标。

这些是宿主行为，不改变 Onlyne 的消息事实。接入把“已注入”和“已唤醒”记录为本地证据，不提升为持久接纳或业务答复。

## 3. todo 路径的额外限制

todo-more 的 schema 会丢弃未知字段，因此来源不能作为独立键挂在条目上；人类可读来源只能放在 explanation 首段，机器可解析来源由 Onlyne 本地记录保存。程序化追加不产生 todo 工具结果，界面是否立即刷新尚未实测。首版必须验证刷新，否则改用会产生工具结果的写入路径，或明确显示待刷新状态。

## 4. 源码索引

以安装包为根：

- `src/extensibility/extensions/types.ts:1861-1872` 定义发送与持久化 API。
- `src/session/agent-session.ts:11991-11994` 定义直接 IRC 投递结果。
- `src/irc/messaging.ts:45-107` 定义 `executeSend`。
- todo-more 的 `src/commit.ts:15-33` 定义快照与投影提交，`src/state.ts:71-107` 定义条目和快照字段。

`commitTodoState` 的 saved 表示分支接受记录，源码未承诺磁盘刷写确认，恢复验收必须覆盖这个边界。
