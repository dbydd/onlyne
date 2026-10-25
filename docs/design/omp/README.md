# Onlyne v3 OMP 插件设计

`packages/omp-onlyne` 把 `@onlyne/lib` 接到 OMP 会话上：一个 OMP 进程是一个节点，`msg` 是模型唯一的 Onlyne 入口，判定交给 OMP 的 `judge` 角色，运行环境（存储、时钟、子代理、todo、Tern）由插件提供。

本文是软件设计，不定义协议，也不重复契约条文。模型可见的参数与结果形状在 RFC 0008；接入必须满足的约束在 [OMP 接入契约](../../contracts/omp.md)；宿主接口签名与已观察限制在 [OMP 宿主接口参考](../../references/omp-extension-api.md)；人机界面细节在 [interaction.md](interaction.md)；判定上下文在 [systemone-context.md](systemone-context.md)。

## 1. 形态与边界

| 项 | 选择 |
|---|---|
| 交付形态 | 一个 OMP 扩展包 `omp-onlyne`，`package.json` 的 `omp.extensions` 指向 `./index.ts` |
| 进程模型 | 每个角色一个 OMP 进程；主会话持有节点与网络端口，子代理共用扩展代码但不持有节点 |
| 配置边界 | 项目目录。`<cwd>/.onlyne/config.toml` 是唯一手写配置，其余状态进 `<cwd>/.onlyne/omp.db` |
| 依赖 | `@onlyne/lib`；OMP `18.12.6` 的 `pi-coding-agent`、`pi-agent-core`、`pi-tui`；`omp-todo-more` 可选 |
| 不做什么 | 不发布 CLI、不做独立守护进程、不实现第二套消息协议。网络入口只有库生成的 HTTP handler |

## 2. 模块布局

| 文件 | 职责 |
|---|---|
| `index.ts` | 扩展工厂。注册 URL 处理器、消息渲染、`msg` 工具、上下文绑定、todo 恢复、界面；在 `session_start`（仅主会话）启动运行时并挂状态行，`session_shutdown` 停止 |
| `src/runtime.ts` | 运行时生命周期：配置装载、SQLite、`Bun.serve`、端点、判定器、传输、密钥目录、维护时钟、域成员关系、发现、`recover` 与 `runMaintenance`；对外提供 `snapshot`、`status`、`notify`、信任动作、发现刷新 |
| `src/node.ts` | `OnlyneNode` 的装配与宿主插槽：认证、地址解析、密钥钩子、决策回调、执行上下文目录、mention 解析、首接触许可 |
| `src/config.ts` | `config.toml` 解析、校验、原子写入；region 与 node id 的短名规范化 |
| `src/types.ts` | 插件内部类型：配置、执行上下文记录、界面快照、状态行快照 |
| `src/keys.ts` | 密钥文件的读取、生成、权限检查、git ignore 检查、原子写入 |
| `src/store.ts` | `SqliteStorage`（`kv` 表，WAL，事务串行）与 `ContextRegistry`（执行上下文登记） |
| `src/network.ts` | DNS-SD 发布与浏览（可选加速器）；候选交给 `discovery.observe` |
| `src/runtime.ts` 的发现部分 | 刷新、种子联系、同域候选核对；库只保存记录 |
| `src/decider.ts` | SystemOne：`msg` 出站判定、入站动作判定、todo 判定；状态裁剪、判定记录、机械前置检查 |
| `src/messages.ts` | `Runtime` 桥：注入、排队、子代理派发、自定义消息渲染、await 结果观察 |
| `src/tool.ts` | `msg` 工具：参数 schema、描述、调用判定、返回结构化事实、await 消费标记 |
| `src/binding.ts` | `task` 子代理与上下文别名的绑定；状态跟随 agent id，不跟随显示名 |
| `src/todo.ts` | 待办写入与恢复：todo-more 优先，原生 todo 兜底；来源关联的持久提交 |
| `src/todo-native.ts` | 原生 todo 的读取与相位转换 |
| `src/urls.ts` | `onlyne://` 内部路径注册；根、threads、inbox、contexts、outbound、neighbors 的渲染与枚举 |
| `src/tern.ts` | `TERN_PANE` 与 `tern ls --json` 查询；只用于 bootstrap 的 session 名建议 |
| `src/ui/` | `/onlyne` 的覆盖界面：`viewer.ts`（四页）、`bootstrap.ts`（首次表单）、`status.ts`（固定状态行）、`format.ts`、`index.ts`（命令注册） |

`decider.ts` 是最大的模块（约 1600 行），因为它同时持有判定输入构造、选择项文案、机械校验和记录。拆分它应当按判定点切，而不是按工具函数切；每个判定点的输入布局在 [systemone-context.md](systemone-context.md) 中有对应小节。

## 3. 启动与关闭

主会话的 `session_start` 依次执行：

1. 挂状态行（有 UI 时）；读取配置。缺配置是正常的首次状态，不报错，等 `/onlyne init`。
2. 打开 `omp.db`，`Bun.serve` 绑定 `server.bind` 与端口 `0`；由实际端口生成 endpoint，广播地址取 `advertisedHost(bind)`。
3. 构造判定器与 `Runtime` 桥，解析密钥目录（`project`/`global`/`none`），用会话定时器实现 `MaintenanceClock`。
4. 创建节点；把域成员关系对齐到配置的 region（先退出旧域，再加入新域，各用独立幂等键）。
5. 启动 DNS-SD（失败只记日志），登记主执行上下文，`client.recover()`，`client.runMaintenance()`。
6. 立刻跑一轮发现，然后每 30 秒刷新一次。发现刷新是插件唯一的周期任务；投递节奏属于库。

关闭时停掉发现、HTTP 服务、维护环与存储。重启（`/onlyne config` 保存后）走同一条路径，先关旧运行时再建新的。

失败分级：配置缺失或非法只影响 `/onlyne`；密钥目录被拒（项目内未 ignore、权限不合格）使这次启动失败，因为无密钥绑定不允许非回环监听；发现失败只影响目录新鲜度。

## 4. 文件与存储

| 路径 | 内容 | 谁写 |
|---|---|---|
| `.onlyne/config.toml` | region、node id、bind、`key_scope`、`threads.page_size`、可选种子与固定指纹 | bootstrap / config 界面 |
| `.onlyne/omp.db` | `kv` 表：库的全部协议状态 + `contexts/` 执行上下文登记 | 库与插件共用同一事务实现 |
| `.onlyne/keys/<node-id>/` | `priv.pem`、`pub.key`（`key_scope = "project"`） | 密钥服务 |
| `~/.config/onlyne/keys/<node-id>/` | 同上（`key_scope = "global"`，缺省） | 密钥服务 |

`omp.db` 的 `kv` 表是唯一的持久结构。库的键空间见 `docs/design/lib/README.md`；插件额外使用 `context/` 前缀登记 `(alias, agent_id, thread_ref, state, summary, created_at)`，用 `meta/context-counter` 分配 `c-N` 别名，以及出站记录扫描所依赖的 `outbound/`。

## 5. 判定边界

| 位置 | 谁决定 | 谁执行 |
|---|---|---|
| `msg` 调用 | `judge` 角色从三段输入与本地证据选动作、目标、thread、响应方式、执行方式 | 插件把动作翻译成库调用（`send`、`read`、`thread`、`domain`、`resource`） |
| 入站条目 | `judge` 选 `fragment`/`wake`/`queue`/`context`/`new_subagent` | 桥按动作注入自定义消息、`appendEntry` 排队或派发子代理 |
| todo 变更 | `judge` 选 `none`/`associate`/`append` 与标题、解释、目标 | todo 模块写 todo-more 或原生 todo，并记录来源 |

明确引用（`op`、`scope`、`thread`、`message`、`resource` 等）走确定性解析，避免把已知事实再问一次模型；只要存在语义选择，就交给判定。子代理会话只读：动作不是 `read` 时直接 `forbidden`。

判定状态有硬预算：整个 state 序列化后不超过 40000 字符，超限时按 `history` → `task_dispatches` → `context_note` 的顺序整段丢弃，请求与问题永不裁剪；仍超限则判定失败而不是静默截断。历史最多 40 行，单行最多 4000 字符。理由在 `decider.ts` 顶部注释：后端按 token 截断会先丢掉请求，模型回答的是一个它没看过的输入。

同一条入站消息在一个进程内只判定一次。重驱复用记录的动作，最多重放它指定的投递；否则维护环会变成模型轮询。

## 6. OMP 接线

| 能力 | 用法 |
|---|---|
| 工具 | `registerTool("msg")`，`strict`，子代理只读 |
| 自定义消息 | `onlyne.inbound`（来信）、`onlyne.routing`（去向说明）、`onlyne.supplement`（补充到子代理），`attribution: "agent"` |
| 排队 | `appendEntry` 写本地条目，不进模型上下文 |
| await 消费 | `onlyne.await-result` 与 `onlyne.await-consumed` 自定义条目，配合 `confirmToolResult` |
| 子代理 | `task` 派发，别名随任务名下发；`binding.ts` 以 registry 中的父子关系为准，不信任显示名 |
| todo | 有 `omp-todo-more` 时写它的快照与投影；否则写原生 todo。来源首行固定 `Source: <thread>/<seq> from <author>` |
| 内部路径 | `onlyne://` 由 `InternalUrlRouter` 注册，`resolve` 渲染、`enumerate` 展开叶子文档 |
| 状态行 | 主会话 UI 组件，固定一行，2 秒刷新加 `notify` 触发 |
| Tern | 只用于 bootstrap 的 region 建议。界面本身不依赖 Tern：同一组件在纯终端走 ANSI，在 Tern 走 `describe` |

## 7. 当前实现与设计的偏差

设计以 RFC 为准。下面这些位置的实际代码仍按旧设定工作，修代码时按本节核对：

| 位置 | 现状 | 设计 |
|---|---|---|
| `ui/viewer.ts` 候选卡片 | 只有阶段文案与选中底色，蓝框/绿框未实现 | 本设计第 7 节：边框表达认证阶段 |

除上表外，本文描述的行为与 `packages/lib`、`packages/omp-onlyne` 的当前实现一致。协议层的符合性场景仍未在真实多进程环境验收，这一状态记录在证据文档，而不是本文。
