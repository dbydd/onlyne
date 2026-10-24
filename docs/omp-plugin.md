# OMP 插件设计规范

本文档定义 Onlyne v3 面向 OMP（`@oh-my-pi/pi-coding-agent`）运行时的扩展插件设计。插件承担 Onlyne 客户端在 OMP 环境下的完整接入，连接 `@onlyne/lib` 与 OMP 的扩展系统。

接口结论按已安装的 OMP 18.12.6 源码核对。行号指向 `@oh-my-pi/pi-coding-agent` 与 `@oh-my-pi/pi-tui` 包内的 `src/`。



OMP提供了额外的接口，omp-onlyne不需要兼容pi！

## 1. 架构与边界

插件以 OMP Extension 形式加载运行。它执行 SystemOne 的判定，把 Onlyne 网络连接、消息收发、上下文登记、内部路径浏览与来信注入整合在一个入口。

```text
+----------------------------------------------------------------+
| OMP Session (Main Agent)                                       |
|  - 持有 NodeId 凭据与对外出站权限                                 |
|  - 驱动单一 msg 工具发信与回复                                   |
|  - 用 task 工具派发 Subagent                                     |
+-------------------+----------------------------+---------------+
                    |                            |
      调用 msg 发信 / 回复                读取 onlyne:// 路径
                    |                            |
+-------------------v----------------------------v---------------+
| Onlyne OMP Plugin (Extension)                                  |
|  +--------------------+  +-----------------------------------+ |
|  | Tool: msg          |  | InternalUrlRouter: onlyne://      | |
|  | Event: tool_result |  | - 只读虚拟资源                    | |
|  | - SystemOne 判定   |  | - 消息、线索与出站证据            | |
|  | - 上下文登记表     |  +-----------------------------------+ |
|  +--------------------+                                        |
|  +-----------------------------------------------------------+ |
|  | Client & Protocol State                                   | |
|  | - OnlyneClient 实例                                       | |
|  | - 消费记录协同 (context, ThreadRef, seq)                  | |
|  | - 上下文目录与投递执行                                    | |
|  | - 来源关联与待办集成                                      | |
|  +-----------------------------------------------------------+ |
+-----------------------------------+----------------------------+
                                    |
                            HTTP / 网络调用
                                    |
                                    v
                         远端 Onlyne 节点与资源
```

插件明确以下边界分工：

1. **Onlyne 库与网络绑定**持有通信、消息去重、身份验证接线、线索管理、资源交接与 region 发现逻辑。当前 Bearer 适配器没有公私钥交换与签名认证实现。
2. **SystemOne** 判定每条消息的处理：出站投递参数、入站处理动作、消息进入哪个执行上下文。各判定点分别装配用户与模型消息、活动分支历史、context note、thread 对话、拓扑、资源目录和 todo-more。来源与模拟方法定义在 [RFC 0016](rfcs/0016-systemone-context.md)。
3. **OMP Extension** 执行 SystemOne 的判定，提供工具挂载、内部 URL 路由、会话生命周期捕获、上下文登记与注入调度。插件不做消息处理判定。
4. **宿主 Todo 机制**（如 `omp-todo-more`）持有任务持续工作项。插件仅维护消息与待办的本地映射关联。
5. **OMP 子代理机制**持有执行会话本身。插件登记上下文别名与子代理 id 的对应关系，按判定投递。

## 2. 模块加载与路由注册

插件采用标准 OMP Extension 模块结构，在 `~/.omp/agent/extensions/onlyne/index.ts` 或已安装 npm 插件包中提供默认工厂函数导出：

```typescript
import type { ExtensionAPI } from "@oh-my-pi/pi-coding-agent";
import { InternalUrlRouter } from "@oh-my-pi/pi-coding-agent/internal-urls";
import { OnlyneClient } from "@onlyne/lib";

export default function onlynePlugin(pi: ExtensionAPI): void {
  // 注册单一 msg 工具
  // 注册 /onlyne 命令：无参数打开查看界面，settings 打开配置界面
  // 注册进程级 onlyne:// 内部 URL 路由
  // 注册 tool_result 与 before_subagent_spawn 事件处理器
  // 挂载会话生命周期与消息监听
}
```

扩展工厂在每个会话执行一次，包括每个子代理会话。模块级变量由整个进程共享，逐会话状态放进工厂闭包或按 `ctx.agent.id` 索引。

### 2.1 进程级路由单例注册

`InternalUrlRouter` 为进程全局单例。插件在模块顶层设置一次性注册守卫，防止重复注册抛出冲突：

1. 模块检查 `InternalUrlRouter.instance().canHandle("onlyne://")`（`src/internal-urls/router.ts:220`）。
2. 未注册时，调用 `register(handler)` 提交 `onlyne` 协议处理器（`src/internal-urls/router.ts:148`）。
3. 已注册时，跳过协议注册，复用已有路由分发实例。
4. 会话结束时，保留进程级 `InternalUrlRouter` 实例，由插件清理对应会话的局部登记。

### 2.2 启动与状态恢复

扩展工厂执行期间禁止调用会话动作 API。插件将状态恢复安排在 `session_start` 事件阶段：

1. 在 `session_start` 处理器中记录 `ctx.agent`，得到 `kind`、`id`、`depth` 与 `parentId`。
2. 读取 `ctx.sessionManager` 保存的自定义条目，恢复本会话的 `(context, ThreadRef, seq)` 消费记录与消息游标。
3. 从项目存储恢复上下文登记表，核对每个上下文的会话文件是否仍存在。
4. 启动后台长连接或轮询监听任务。
5. 扫描出站未决记录，调用 `client.recover()` 推进过期重投。

`tool_result` 与 `before_subagent_spawn` 处理器在工厂执行期间注册，属于扩展加载阶段。它们不依赖会话状态，只查上下文登记表。

### 2.3 项目级配置与服务界面

所有 Onlyne 配置属于项目。插件读取 `<project>/.onlyne/config.toml`，不把节点身份、端点或访问控制写进用户级配置。

OMP 原生插件设置页不承载这些配置。该页由插件 `package.json` 的 `omp.settings` 声明生成，读写的是 `~/.omp/plugins/omp-plugins.lock.json`。项目覆盖文件 `.omp/plugin-overrides.json` 只有读取路径，没有写入路径（`src/extensibility/plugins/manager.ts:981-1012`、`src/extensibility/plugins/types.ts:128-163`）。节点身份是项目事实，放进用户级文件会让同一台机器上的多个项目互相覆盖。

插件的配置读写规则：

1. 用户通过 bootstrap 生成 `<project>/.onlyne/config.toml`，无需预先编写文件。本节点配置使用 TOML，发现目录与动态地址存入项目 SQLite。
2. 密钥引用可选。首次回环 E2E 使用 RFC 0001 的 none 开发绑定；后续密钥交换独立提案，配置不保存密钥原文。
3. 配置变更由插件面板提交。面板只写当前项目文件，写入成功后重载 `OnlyneClient`。
4. 普通终端提供完整的逐行引导与设置界面。无交互环境需要由宿主提交同一引导数据；当前插件尚未实现独立配置 CLI。
5. 插件不在 `omp.settings` 中声明任何条目。两套配置来源会让同一个值出现两个真相，原生页的编辑还会写入用户级文件。

配置文件示意：

```toml
[node]
region = "urn:onlyne:region:research-chamber"
id = "urn:onlyne:agent:lazuli"

[server]
bind = "127.0.0.1"

[threads]
page_size = 50
```

node.id 在引导中初始为空，用户填写稳定唯一身份。region 是独立的协议归属，启动时提交域成员记录。监听端口由操作系统分配，实际 endpoint 由运行时公布。对端候选通过引导种子地址和 HTTP 目录取得，逐节点核对后存入 SQLite；同链路 DNS-SD 是可选加速器。配置不保存 endpoint、port、peers 或语义调度参数。任务派生与补充调度使用 OMP judge 角色的实际判定与宿主观察。

## 3. 会话隔离与 Subagent 协作模型

OMP 运行环境中存在主 Agent 会话与由任务派生的 Subagent 子会话。插件实施分级权限、上下文登记与路由隔离。

### 3.1 角色分工

- **主 Agent**：拥有当前工作区对应的 Onlyne 节点身份（`NodeId`）。持有唯一的对外发信、接收新消息、登记回复与交接资源权限。派生子代理的入口只在这里开放。
- **Subagent**：继承主会话上下文。专注执行具体的计算、检索与代码编写任务。仅持有本会话内部的数据读取权限，以及对派发它的主 Agent 的进程内通信权限。

判断会话类型用 `ctx.agent.kind`，取值为 `"main"` 或 `"sub"`（`src/extensibility/extensions/types.ts:520-537`）。`depth === 0` 不能用作判断依据：`/tan` 克隆是子代理且深度为 0。

### 3.2 任务派发与回复流转

对于需要派生子任务的来信，执行流程如下：

1. 主 Agent 收到来信，记录原消息的 `MessageRef`、`ThreadRef` 与回复权威。
2. 主 Agent 调用 `task`，把任务描述与必要上下文传入子代理。
3. 子代理注册后，插件登记上下文别名与子代理 id；登记在模型看到 `task` 结果之前完成。
4. Subagent 在独立执行环境中完成工作，产出执行结果与产物。
5. Subagent 将产物交付给主 Agent，子会话转入空闲或按运行时策略挂起。
6. 主 Agent 调用 `msg` 工具向原消息回复权威发出 `reply`，携带子任务执行结论。

Subagent 调用 `msg` 发起对外网络投递时，插件直接拒绝该请求。对外回复点始终保持单一本节点身份，避免并发子副本造成的身份混淆与回复冲突。

### 3.3 上下文登记与任务控制

任务是否创建、消息进入哪个执行上下文，都由 SystemOne 判定。插件只登记判定的结果，提供 SystemOne 读取的上下文目录，并按判定投递。插件不按消息关系自行改选上下文。

OMP 的 `task` 工具每次调用都创建新会话。同一个 `name` 再调用一次会得到 `Name-2`，并启动一个全新的子会话（`src/task/output-manager.ts:83-91`）。`task` 没有向已有子代理追加消息的入口，追加只能走进程内投递。

上下文登记流程：

1. 出站 SystemOne 判定 `execution: "new"` 时，发送方只提出接收方创建上下文的要求。发送方不在自己的 registry 分配接收方别名，也不要求本地模型替接收方派生任务。
2. 接收方先检查是否具备创建并驱动独立会话的宿主能力。能力缺失时在接纳前返回 `unsupported_delivery`；接纳后由接收方分配本地别名，例如 `c-7`，并持久关联来源消息。
3. 接收方按已确认的派发动作启动子会话，并以真实 task 调用及其结果绑定别名。registry 的 id、sessionFile、parentId 与 task 返回的 `details.progress[].id` 或 `details.results[].id` 共同提供登记证据。
4. 执行记录必须包含所属 NodeId、OMP 主会话及子代理身份。发送方引用既有上下文时，别名属于目标节点的目录；相同的 c-7 在不同节点上属于不同命名空间。
5. 子代理注册完成前记录保持 `unbound`，补充保留原引用等待登记。消费记录使用实际接收会话身份。发送方等待 reply 的消费上下文单独取自发送方工具调用，不能复用接收方执行别名。

上下文目录是 SystemOne 的判定输入，列出别名、所属 thread、任务摘要与状态。目录来自 `AgentRegistry` 和本地记录，不由模型填写。同一份目录以 `onlyne://contexts` 暴露给模型，供排查与主动读取。

插件可用的宿主控制点如下。它们是登记与排障手段，不承担投递判定：

| 控制点 | 用途 | 局限 |
|---|---|---|
| `AgentRegistry.onChange` | 发现新子代理并补全 `sessionFile` | 只给出身份与状态 |
| `tool_result` 事件 | 读取 `details` 中的子代理 id | 依赖工具名与 details 结构 |
| `before_subagent_spawn` 事件 | 记录派生发生、替换模型或拒绝派生 | 不携带解析后的 id |

插件在工厂中注册事件处理器，并保留原生 `task`。`before_subagent_spawn` 的实际能力是替换模型或拒绝派生，子代理 id 由结果与 registry 补齐。`types.ts:1746-1750` 的加载时要求属于文件写入 fallback 注册，不能作为通用事件订阅规则。

上下文登记存放在项目存储的 SQLite 表中，随项目一起迁移。恢复时以登记表为准。

异步派生让补充说明能在子代理运行期间到达。宿主关闭异步任务时（`task.async.enabled = false`），阻塞调用期间主 Agent 无法送出补充。登记仍然有效；补充排队到调用返回后投递，落点由 SystemOne 判定。

### 3.4 补充说明的落点

补充说明指同一条根消息之后的追加内容：`relation: "followup"` 的消息，或用 `in_reply_to` 指向某条消息的补充。

落点由 SystemOne 判定。来信、用户最近要求、主模型派发说明、子代理最近进展、todo 和 thread 对话共同提供判断依据。没有 `in_reply_to` 的补充也可以进入原子代理。插件保留合理的竞争上下文，不能按根引用或同一 thread 预选。判定为 `execution: "existing"` 时，`context` 必须是目录中的一个别名。

插件只执行判定，不改写判定。执行按上下文状态分流：

| 上下文状态 | 执行动作 | 结果 |
|---|---|---|
| 运行中 | 调用该会话的 `sendCustomMessage`，`deliverAs: "steer"` | 补充以 developer 角色进入当前回合 |
| 空闲 | 调用 `sendCustomMessage`，`triggerTurn: true` | 宿主在同一会话上启动回合 |
| 挂起或已中止且可恢复 | `AgentLifecycleManager.ensureLive` 恢复后调用 `sendCustomMessage` | 宿主从会话文件恢复并投递 |
| 已中止且不可恢复，或会话丢失 | 不投递 | 返回 `context_lost`，判定失败 |
| 未登记 | 不投递 | 返回 `not_found`，判定失败 |

判定失败时插件不选择替代上下文，也不创建新的子代理。失败原因分两类：`context_lost` 是上下文不可恢复或会话丢失，`not_found` 是目录中没有该别名。主 Agent 收到失败后重新调用 SystemOne，由新的判定决定下一步。

投递使用目标会话的自定义消息接口，不调用 `sendUserMessage`：

```typescript
import { AgentRegistry } from "@oh-my-pi/pi-coding-agent/registry/agent-registry";
import { AgentLifecycleManager } from "@oh-my-pi/pi-coding-agent/registry/agent-lifecycle";

const ref = AgentRegistry.global().get(context.agent_id);
const session = ref?.session ?? (await AgentLifecycleManager.global().ensureLive(context.agent_id));
const startedTurn = await session.sendCustomMessage(
  {
    customType: "onlyne.supplement",
    content: renderedSupplement,
    display: true,
    attribution: "agent",
    details: { thread_ref, seq, message_ref, consumption: "pending" },
  },
  context.state === "running" ? { deliverAs: "steer" } : { triggerTurn: true },
);
```

`ensureLive` 对挂起和已中止的会话按会话文件恢复，无法恢复时抛出错误（`src/registry/agent-lifecycle.ts:379`）。`sendCustomMessage` 返回是否同步启动了新回合（`src/session/agent-session.ts:9606-9616`）。三种情况都不创建新会话。

`onlyne.supplement` 与 `onlyne.inbound` 使用同一渲染约定。补充内容以 developer 角色进入子代理上下文，保留来源、thread/seq 和消息引用；子代理不会把它当成用户本人发言。

机械检查只做执行前校验，不参与判定：别名必须存在于上下文目录，状态必须可用，接收方的 `parentId` 必须是主 Agent。这三项不满足时返回上表的失败，不换一个上下文重试。

投递回执按以下规则记录：

1. `sendCustomMessage` 的返回值只记录“是否启动了回合”。模型消费必须另有确认，不能据此推进 `(context, ThreadRef, seq)`。
2. `ensureLive` 或 `sendCustomMessage` 抛出的错误原样保留。插件不自动改选目标或重新创建子代理。
3. 返回状态未知时保留补充、原投递引用与上下文。是否再次投递由 SystemOne 根据新证据决定，重投仍遵守库的幂等规则。
4. 投递成功只证明消息进入了子代理会话。它不证明子代理已经理解、已经执行或已经答复。

SystemOne 已安排到子上下文的补充保留其安排记录，主会话查询时显示落点和未确认消费状态。消费确认之前不能把条目标为已读。`onlyne://threads/<alias>` 可以显示 `routed to <agent_id>`，该标注不证明模型已读。

同一 thread 存在多个可用上下文时，SystemOne 从目录中取得候选。消息没有写明原消息引用时，判定可以返回 `ambiguous_target` 与候选列表，由模型补明引用后重试。

### 3.5 无闭包上下文路由

`InternalUrlRouter` 的处理器为全局单例，处理函数不得在闭包中捕获特定会话实例。每次解析请求通过 `ResolveContext` 提取调用方身份：

- 通过 `context.session.getAgentId()` 与 `context.session.taskDepth` 区分主会话与子代理。
- 依据 `context.session.getSessionFile()` 校验工作区归属。
- 子代理只能读取上下文登记中属于自己的消息与 thread 切片。
- 缺少会话上下文的外部查询仅允许访问节点公开目录，不返回会话私有信箱。

## 4. 只读 `onlyne://` 内部路径

插件将 Onlyne 节点运行状态映射为只读虚拟路径，接入 OMP 内部 URL 路由系统。OMP 的 `read`、`grep`、`glob`、`find` 工具以及 Bash 环境下的虚拟文件系统均可直接访问该路径。

### 4.1 协议规范声明

协议处理器声明以下规范：

- `backing: "virtual"`：虚拟只读内容，由处理器动态生成。
- `selectors: "lines"`：允许使用 `:N-M` 等行号选择器。
- `immutable: true`：禁止模型通过 `edit`、`write`、`ast_edit` 篡改协议状态。
- `write` 策略：缺省。任何写操作在路由层直接拒绝。

### 4.2 路径分层结构

| 内部路径 | 格式 | 内容与用途 |
|---|---|---|
| `onlyne://` | Markdown | 当前节点信息、网络端点、可用域声明及根目录清单 |
| `onlyne://inbox` | Markdown | 当前会话已持久接收、待模型主动关注的消息列表 |
| `onlyne://threads` | Markdown | 当前节点加入的全部 thread 摘要与未读提示 |
| `onlyne://threads/<alias>` | Markdown | 指定线索的元数据、成员授权表、双向历史与补充说明的落点标注 |
| `onlyne://threads/<alias>/messages/<seq>` | JSON | 指定序号消息的完整不可变正文、作者与关系引用 |
| `onlyne://contexts` | Markdown | 上下文目录：别名、所属 thread、任务摘要、状态与会话文件；只读视图 |
| `onlyne://outbound` | Markdown | 本节点近期发出的出站投递列表与接纳状态 |
| `onlyne://outbound/<msg_id>` | JSON | 单条出站记录、回执详情或终态拒绝证据 |
| `onlyne://resources` | Markdown | 本节点当前管理的会话、作业与数据源资源列表 |
| `onlyne://domains` | Markdown | 本节点已加入的唯一活动域记录与候选发现结果 |

### 4.3 内容检索与枚举

协议处理器实现 `enumerate(url, context)` 接口：

1. 针对 `onlyne://inbox`、`onlyne://threads/<alias>` 提供叶子文档列表。
2. 模型使用 `grep` 或 `find` 检索 `onlyne://` 路径时，路由系统自动拉取对应叶子文档展开全文检索。
3. 列表结果由存储事务即时导出，保证内容自洽。

## 5. 单一模型工具 `msg` 与发送调度

按照 RFC 0008 规范，插件向模型暴露唯一的 `msg` 工具，统一处理发信、回复、读取与资源操作。

### 5.1 工具定义与参数模型

使用 `pi.registerTool` 注册：

```typescript
pi.registerTool({
  name: "msg",
  label: "Onlyne Message",
  description: "Send and reply to messages, browse threads, and manage resources via Onlyne.",
  parameters: pi.zod.object({
    delivery_kind: pi.zod.string().describe("Intent classification text"),
    msg: pi.zod.string().describe("Message body text"),
    meta: pi.zod.record(pi.zod.any()).optional().describe("Routing, thread, and relation metadata"),
  }),
  async execute(toolCallId, params, signal, onUpdate, ctx) {
    // 权限校验与发信处理
  },
});
```

工具的执行上下文带有 `sessionManager`、`model`、`ui` 与 `localProtocolOptions`，不带 agent id。会话身份来自工厂闭包中由 `session_start` 记录的 `ctx.agent`。

工具处理结果分三类：

1. **对外投递**：主会话执行 `send`、`reply`、`followup`。子会话的对外投递被拒绝。
2. **本地读取**：主会话与子会话都能执行。子会话读到的是上下文目录授权给它的 thread 切片。
3. **上下文读取**：只有主会话能读取全部上下文目录。目录是 SystemOne 的判定输入。

### 5.2 发送与 Await 结果协同

当发送计划指定 `response: "await"` 时：

1. 插件调用 `client.send`，该调用阻塞等待对端返回合法 `reply`。
2. 等待期间，网络接收路径照常运行，回执与普通消息排队入库。
3. 对端回复到达并解除等待后，工具返回携带该回复正文与事实引用。
4. 宿主确认在模型上下文完成该工具结果的渲染后，插件调用 `client.confirmToolResult(reply, context)` 提交已消费事实。
5. 提交动作将已消费状态原子写入 `(context, ThreadRef, seq)` 消费记录中。

## 6. 来信注入与消费确认流转

来信入站由后台监听服务接收。每条来信都交给 SystemOne 判定投递计划，插件按计划执行。插件不按宿主忙闲自行决定注入、排队或唤醒。

判定通过 OMP 的 `judge` 模型角色执行（`resolveJudge`），插件不直连判定服务 URL。角色链、凭据、候选回退与用量记账由 OMP 管理；没有可用候选时返回 `decision_unavailable`，插件不自行补一个判定。

### 6.1 判定与执行

| SystemOne 判定动作 | 插件执行 | 模型可见形式 |
|---|---|---|
| `fragment` | 以自定义消息类型注入，交到下一个上下文装配点 | 关注列表摘要以 developer 角色进入上下文 |
| `context` | 按 `onlyne://contexts` 的别名投递到指定上下文 | 目标上下文收到补充说明 |
| `queue` | 写入不进入模型上下文的本地条目，记录排队状态 | 状态栏显示排队 |
| `wake` | 以自定义消息类型注入并请求启动回合 | 新回合以 developer 角色读取该来信 |
| `wait_result` | 由在途等待返回该回复 | 作为 `msg` 工具结果呈现 |

执行细节由宿主能力决定，判定不变：

- `fragment` 进入下一个上下文装配点，不主动开启回合。需要立即开始执行时由 SystemOne 选择 `wake`。
- 流式生成期间，注入排在下一步边界，工具并发不改变来信归属。
- Plan 模式只保存，不启动执行回合。恢复执行后把当前状态交给 SystemOne 重新决定排队消息的处理。
- 在途等待由库的匹配规则解除，解除后的呈现走 `wait_result`。其他来信继续持久排队。
- `context` 的投递使用目标会话的 `sendCustomMessage`；子代理不可达时返回错误，不改选上下文。
- 用户中止时释放本地等待，记录 `wait cancelled`，原发送的接纳事实保持不变。

### 6.2 注入使用自定义消息类型

插件注入来信与路由说明时使用 OMP 的自定义消息类型，不调用 `sendUserMessage`。自定义消息在会话中记为 `custom_message`，`attribution` 为 `agent`，在 TUI 中以独立样式显示。OMP 把它转换为 developer 角色的上下文，不冒充本地用户发言。

消息类型与用途：

| customType | 内容 | `display` | 用途 |
|---|---|---|---|
| `onlyne.inbound` | 渲染后的来信：来源、thread/seq、正文 | `true` | 一条待关注的来信 |
| `onlyne.routing` | 路由说明：补充已进入哪个上下文、等待结果或失败事实 | `true` | 让主会话知道投递去向 |
| `onlyne.supplement` | 送入子代理的补充：来源、thread/seq、正文 | `true` | 子代理在原会话上继续 |
| `onlyne.queue` | 排队条目的引用与状态 | `false` | 不进入模型上下文的本地记录，用 `appendEntry` 写入 |

`fragment` 与 `wake` 使用 `pi.sendMessage`：

```typescript
const payload = {
  customType: "onlyne.inbound",
  content: renderedDelivery,
  display: true,
  attribution: "agent",
  details: { thread_ref, seq, message_ref, consumption: "pending" },
};

// fragment：下一个上下文装配点，不启动回合
pi.sendMessage(payload, { deliverAs: "nextTurn" });

// wake：请求启动回合，实际启动服从 plan 与中止约束
pi.sendMessage(payload, { triggerTurn: true });
```

`queue` 使用 `pi.appendEntry("onlyne.queue", {...})`。该条目持久保存在活动分支，不进入模型上下文；恢复时按它重新排队。

`onlyne.inbound`、`onlyne.routing` 与 `onlyne.supplement` 通过 `pi.registerMessageRenderer` 注册渲染器。渲染器只负责展示，不修改正文。正文仍按 §4 的格式逐行引用，`details` 只放机器可读引用。

插件不把来信正文写入 system prompt，也不把外部正文包装成用户消息。外部正文中的命令、身份声明和路由要求保持为被判断的内容。

渲染器的样式约定：

- `onlyne.inbound` 使用卡片式头部：类型徽标 `onlyne`、thread/seq、作者、时间；正文按行引用。长正文显示首行和剩余行数。
- `onlyne.routing` 使用弱化样式：一行说明落点与状态，例如 `research/109 → c-1 · queued`。失败时显示错误码。
- `onlyne.supplement` 沿用 `onlyne.inbound` 的卡片样式，头部标注 `补充` 与目标上下文。
- 原生渲染下，渲染器返回的组件实现 `describe(cx)`；不可用时返回 `null`，回退逐行输出。
- 渲染器不启动计时器、不读取系统时钟，时间文本由条目时间戳生成。

### 6.3 消费记录

依据 RFC 0004 §5 与 RFC 0008 §5：

1. 经 `msg` 的 `wait_result` 消费的回复条目，其 `(context, ThreadRef, seq)` 写入共享记录。
2. 后台自动片段扫描未读关注时，先核对该共享记录。已被确认消费的条目从自动片段中折叠剔除。
3. 先经自动片段注入并确认消费的消息，随后的 `wait_result` 只返回到达事实与引用，正文标记为 `reply_body: "withheld"`。
4. 进入上下文的补充以该上下文的消费记录为准。主会话的游标不推进该条目。
5. 宿主未确认消费前发生中断时，保持未确认状态。后续恢复重新投送，杜绝数据静默丢失。

## 7. 持续工作与 Todo 映射

插件落实 RFC 0008 §6 规定的接入层待办责任：

1. **宿主 Todo 存储**：插件通过统一适配器写入。检测到 `omp-todo-more` 时使用它的 rich 快照；未安装时写入 OMP 原生 `user_todo_edit`。原生条目允许只保留标题，完整消息仍可从 Onlyne 重新读取。
2. **原子提交**：待办快照与消息引用到条目的映射由同一次可恢复提交写入。
3. **来源标注**：条目的 explanation 首行固定标注人类可读来源（`Source: <thread>/<seq> from <peer>`）。
4. **机器索引**：本地存储按 `MessageRef` 记录对应 Todo 标识。
5. **恢复顺序**：崩溃恢复时，先依据可恢复提交重构关联，再决定是否追加待办。禁止因本地缓存丢失而重复追加同名条目。
6. **任务结算**：回复消息只解除等待，不自动将 Todo 标记为完成。完成状态完全由模型和用户显式驱动。
7. **子代理任务**：SystemOne 结合已有 goal、条目和来源，判断补充已有工作或新建工作项。插件不能只按标题或消息关系决定合并。来源持久关联负责防止同一消息重复建项。

## 8. 配置界面

配置界面读写项目文件，展示作用域与来源，提供键盘与指针两条操作路径。

### 8.1 入口与结构

`/onlyne` 打开查看界面；未配置时直接进入引导界面。`/onlyne init` 主动打开引导界面。`/onlyne settings` 打开配置面板。面板标题为 `Onlyne · <project>`，页脚显示配置文件路径与作用域标记 `项目级`。同一项目在多个终端打开时，页脚路径相同。

面板按用途分组：

| 分组 | 行 |
|---|---|
| 节点 | region、节点 id、监听 host、可选密钥引用 |
| 消息 | 每页条数 |
| 运行观察 | 实际 endpoint、网络发现状态、身份验证方式、上下文数量 |

运行观察是只读行。region、身份、监听 host、密钥引用与每页条数可编辑。region 变更通过协议 leave/join 提交；节点 id 变更属于身份变更，界面必须说明既有消息引用仍属于旧身份。

### 8.2 行与控件

每一行显示当前生效值、来源与控件。来源取 `file`、`default`、`env` 三种之一，写入文件后来源变为 `file`。

| 值类型 | 控件 | 草稿行为 |
|---|---|---|
| 布尔 | 开关 | 切换草稿值 |
| 枚举 | 选择器 | 选中草稿值 |
| 字符串 | 文本输入 | 保存时校验 |
| 密钥引用 | 只读文本加“设置”“清除”两个动作 | 只改引用名，不显示密钥 |

密钥行显示 `已设置` 或 `未设置`。密钥原文不进入界面、日志或工具结果。

编辑采用草稿状态：所有控件只修改草稿，保存动作一次提交全部变更。保存前校验类型、范围与必填项，失败时在对应行显示原因并保留草稿。`Esc` 放弃草稿前要求确认。保存成功后重新加载 `OnlyneClient`，页脚短暂显示 `已保存`。

### 8.3 Tern 下的呈现

面板实现 `describe(cx)`，按能力选择形态：

1. 终端支持 `prefs` 且支持 `aside`：描述为右侧停靠的 `prefs` 页。
2. 终端支持 `prefs` 且不支持 `aside`：描述为占满屏幕的 `prefs` 页。
3. 其余终端：`describe` 返回 `null`，回退为逐行输出。

布尔行在原生形态下是开关控件。枚举、字符串与密钥行使用原生可编辑控件或选择器。所有动作保留键盘路径，指针事件经 `handleNativeEvent` 处理。

### 8.4 错误与恢复

1. 配置文件解析失败时，面板以只读方式打开，显示出错行号、路径与解析器消息，并提供一个“打开文件”动作。
2. 写入失败时保留草稿，显示系统错误，不修改已有文件。原子重命名保证文件要么是旧内容，要么是新内容。
3. `OnlyneClient` 重载失败时保留旧连接，状态行显示 `配置已保存，连接未重载`，并提供“重试”动作。
4. 配置文件被外部修改时，面板在下次打开时重新读取。面板不监视文件，也不覆盖未提交的草稿。

### 8.5 引导界面（/onlyne init）

项目首次接入时运行 `/onlyne init`，或者在未配置时直接运行 `/onlyne` 打开引导界面。

引导界面收集最小必需配置并提供默认值：

1. **region**：必填。有 Tern 时提供当前 session 名作为建议值，用户确认后保存独立的 region URI。普通终端由用户填写，同样具备网络发现能力。
2. **node.id**：必填，初始为空，由用户填写稳定唯一标识。
3. **server.bind**：必填，默认 `127.0.0.1`。端口由操作系统在启动时分配。
4. **node.key_ref**：可选，初始为空；首次回环 E2E 可使用 none 开发绑定。
5. **threads.page_size**：默认 50，范围 1～100。

提交时生成配置、启动监听、提交 region 成员关系并发布发现，界面逐项报告实际结果。对端自动发现后存入 SQLite。多播不可达时，引导可以请用户提供一个已有节点地址，立即联系并读取 HTTP 候选目录。取消时不写文件、不启动节点。Tern 原生使用 `nativeOverlay`，普通终端提供完整的逐行界面。

## 9. 查看界面

查看界面让操作者直接看到当前 OMP 进程里的 Onlyne 状态。它只读，数据来自插件内存与 Onlyne 本地记录，不发起网络请求。

### 9.1 入口与结构

`/onlyne` 打开查看界面。`/onlyne settings` 打开配置界面。两个界面互不替代。

查看界面分两页，页签固定为 `1 队列`、`2 对话`。页脚显示 `1/2`、thread 总数与未读总数。

| 页 | 内容 |
|---|---|
| 1 队列 | 待处理来信、排队条目、运行中上下文、未决等待、未知出站结果。每项显示来源、目标、状态与等待时长 |
| 2 对话 | 左侧 thread 列表，右侧消息流。消息按 seq 排列，区分 in/out，显示作者、时间、关系、路由标记与消费状态 |

队列页按状态分组，组内按时间排序。对话页的 thread 列表按最近活动排序，未读 thread 带计数。消息流的选中条目显示完整正文，其余条目显示首行。

操作：

| 按键 | 作用 |
|---|---|
| `Tab` / `Shift+Tab` | 切换页 |
| `j` / `k` | 上下移动 |
| `g` / `G` | 到顶 / 到底 |
| `Ctrl+d` / `Ctrl+u` | 半页滚动 |
| `[` / `]` | 对话页切换 thread |
| `r` | 重新读取本地状态 |
| `Esc` / `q` | 关闭 |
| `Enter` | 在对话页打开选中消息的全文 |

### 9.2 数据与刷新

查看界面读取插件已持有的状态：上下文登记表、活动分支里的队列条目、Onlyne 的 thread 与消息记录。它不调用 SystemOne，不改变消费记录，不发送消息。对话页对 `onlyne.inbound` 与 `onlyne.routing` 使用与消息渲染器相同的样式，队列页显示它们的投递状态。

数据变化时界面按事件刷新。刷新失败时保留上一帧并显示 `刷新失败: <原因>`，不把旧数据当成当前状态。没有数据时显示 `没有待处理消息`，与读取失败分开。

### 9.3 Tern 下的呈现

查看界面实现 `describe(cx)`，按终端能力选择形态：

1. 使用 `nativeOverlay`，`role` 为 `omp.overlay.onlyne`，`size` 为 `lg`，`head` 显示 `Onlyne`。两页是组件内部的页签，不使用 `prefs`。
2. 终端支持 `aside` 时，OMP 把该 overlay 描述为可停靠的 sheet；TSP v1 没有左侧锚点，实际停靠边由终端决定。终端不支持 `aside` 时居中浮动。
3. 原生描述返回 `null` 时回退逐行输出。

**左侧浮动的限制。** TSP v1 的 overlay anchor 只接受 `center`、`top`、`bottom`，`aside` 的停靠边由终端决定。经典终端使用 `overlayOptions.anchor: "left-center"`，浮动在左侧。Tern 原生下没有左侧锚点，插件无法请求该位置；界面停在终端给出的 sheet 边，并在标题中显示实际位置。TSP 增加左侧能力后再使用它，插件不伪造该能力。

查看界面可关闭：`Esc` 与 `q` 关闭。经典 overlay 在标题行提供关闭按钮；Tern 原生模式由终端 overlay 的关闭动作处理。所有动作保留键盘路径；指针点击页签与 thread 行经 `handleNativeEvent` 处理。

## 10. Tern 兼容

Tern 下的原生渲染由 Tern Surface Protocol（TSP）驱动。OMP 用 `TERM_PROGRAM=tern` 发起探测，以 `hello` 回复的版本决定是否进入原生渲染（`pi-tui/src/terminal.ts:1940-2030`）。探测失败时回退到逐行 ANSI 绘制。

插件的实现规则：

1. **能力判断**：用 `cx.supports(kind)` 与 `cx.feature(name)` 判断节点和特性。不缓存判断结果；首帧的乐观 `hello` 缺少 `media` 与 `aside`。
2. **自定义组件**：配置面板与线程视图实现 `describe(cx)`，在状态未变时返回同一个节点对象。返回 `null` 时回退到 `render()` 逐行输出。
3. **面板形态**：终端支持 `prefs` 时把配置面板描述为 `prefs` 页；查看界面使用 `nativeOverlay`。支持 `aside` 时由终端停靠 sheet，否则占满一个屏幕表面。两种形态都要可用。
4. **工具呈现**：`msg` 实现 `describeCall` 与 `describeResult`。实现后原生模式不再调用 `renderCall` 与 `renderResult`。`onlyne.inbound` 与 `onlyne.routing` 的消息渲染器同时实现 `describe`，原生模式下按描述渲染。原生 `task` 保留宿主的呈现。
5. **指针与键盘**：原生模式不开启鼠标上报，也没有备用屏幕。每个可点击控件同时保留键盘路径。指针事件通过 `handleNativeEvent` 处理。
6. **动效**：`describe` 不读时钟、不启动定时器。进度、耗时与加载状态交给 `spinner`、`shimmer`、`elapsed`、`progress` 节点，由终端计时。
7. **输出纪律**：插件禁止写标准输出。原生帧与 ANSI 输出共用同一个流，交错写入会破坏帧边界。日志经 `logger` 写入文件。
8. **界面外壳**：标题经 `ctx.ui.setTitle` 设置，通知经 `ctx.ui.notify` 发出，状态行经 `setStatus` 更新。插件不自行发送 OSC 或 APC 序列。
9. **配色**：使用主题令牌与 `theme.fg`、`theme.bg`、`theme.symbol`。原生渲染会丢弃非主题颜色。
10. **测试矩阵**：Tern 原生、Tern 加 `PI_TUI_NATIVE=0`、Tern 内 tmux 带与不带 `@tern-tsp`、普通终端，四种环境都要验证。`PI_TUI_NATIVE_STATS=1` 输出的回退计数用于发现静默降级。

## 11. 错误处理与清理规范

1. **退出清理**：`session_shutdown` 触发时，中止当前会话的所有在途等待 Promise，注销会话本地监听器，保留进程级路由注册。
2. **上下文清理**：子代理进入 `aborted` 且会话文件不可恢复时，上下文记录标记为 `lost`。不删除记录，历史查询仍需要它。
3. **故障隔离**：对端不可达、超时或服务崩溃时，操作明确记录为 `unknown`，保留原始报错至 `details.source`，严禁伪报失败或静默丢弃。
4. **降级路径**：Tern 能力缺失时按 `describe` 返回 `null` 回退到逐行输出。原生渲染不可用时功能保持完整。
5. **日志排查**：插件运行日志写入 `<project>/.onlyne/logs/plugin.log`，记录节点握手、线索订阅、补充说明路由与消费确认流水。日志不输出认证凭据。

## 12. 验证状态

当前代码已经接入 SQLite 事务存储、HTTP 节点、msg 调库、OMP judge 角色和自定义消息 API，bootstrap 与查看界面也有组件代码。此前实际运行证明了扩展可加载和显式 msg 列表查询；这些证据没有覆盖集群消息往返或界面操作。

当前实现仍有以下缺口，完成修复及验收前不能称为完整插件：

| 能力 | 代码现状 |
|---|---|
| 网络发现与 region | 引导种子、HTTP 候选目录和 SQLite 发现目录已接入；启动时提交 region 成员关系；DNS-SD 可选。真实多进程发现未验收 |
| 新执行上下文 | 接收方通过原生 task 工具启动子代理并绑定 c-N；发送方不再分配接收方上下文。真实派生未验收 |
| 消费确认 | 自定义消息提交返回 consumed:false；provider 请求头命中后确认。真实请求边界未验收 |
| Await 去重 | 发送方本地 context、hold、confirmConsumption 已接入；库套件覆盖。插件工具路径未验收 |
| Todo | todo-more 可选；存在时原子写 rich+native，缺失时写原生 `user_todo_edit` 与决策标记。用户将在实战验证 |
| 子代理读取 | msg 写路径受限；onlyne:// 与 decider 读取按 agent 与 context source 过滤。未验收 |
| 配置与调度 | endpoint、port、peers、default_attention、spawn、supplement_window 已从配置和执行路径删除 |
| 界面 | 两页、bootstrap 与设置组件；真实 Tern 操作、刷新和分页未验收 |

已按 18.12.6 源码核对：

- `task` 不复用已有子代理，重复 `name` 得到 `-2` 后缀。
- 子代理 id 出现在 `details.progress[].id` 与 `details.results[].id`。
- `executeSend` 的签名与导入路径；父级发送转为 steer 的判断条件。
- `ctx.agent` 的 `kind`、`id`、`depth`、`parentId` 字段。
- 原生插件设置页的声明格式，以及写入用户级配置的行为。
- Tern 的探测条件、`prefs` 与 `aside` 能力、`describe` 契约。
- 扩展工具覆盖内置工具，`ctx.invokeTool` 调用原生实现。
- 活动分支可经 `ctx.sessionManager.getBranch()` 读取。context note 来自 `experimental_context_notes` 自定义条目，读取应服从 reset boundary。
- `CustomMessagePayload` 接受 `customType`、`content`、`display`、`details` 与 `attribution`；默认 `attribution` 为 `agent`。
- 自定义消息经 `convertMessageToLlm` 转为 developer 角色上下文，用户消息不参与该路径。
- `pi.registerMessageRenderer(customType, renderer)` 可注册消息渲染器。
- TSP v1 的 overlay anchor 只有 `center`、`top`、`bottom`；`left-center` 属于经典 ANSI overlay。

已运行：

- 根目录 `bun run check`：`@onlyne/lib` 与 `omp-onlyne` 类型检查通过。
- 根目录 `bun run test`：库的 verify、domain、first-contact 套件全部通过；插件 28 个测试通过。
- 该证据只覆盖库行为、插件单元路径和单机两节点发现/首次接触；没有启动真实 OMP 集群或 GUI 操作。

尚未实跑：

- steer 注入与空闲唤醒在同一子代理上的实际时序。
- 上下文登记写入与子代理启动之间的竞争窗口。
- 配置面板在 Tern 原生与逐行回退两种模式下的实际渲染。
- 查看界面的两页切换、Tern 原生描述与关闭动作。
- Tern 的左侧浮动请求；TSP v1 没有左侧锚点，实际停靠边由终端决定。
- `onlyne.inbound` 与 `onlyne.routing` 渲染器在原生与 ANSI 两种模式下的实际显示。
- 阻塞派生下补充说明排队到调用返回后的投递。
- SystemOne 判定动作与宿主调度实际结合的运行结果。
