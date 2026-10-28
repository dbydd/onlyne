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

### 1.1 准入与信任边界

握手权限拆成三道闸门，各有自己的所有者、状态来源和失败方式，任何一道都不能代替另一道：

| 闸门 | 回答的问题 | 实现 | 状态来源 |
|---|---|---|---|
| 传输认证 | 谁能在线上说话 | 库的 `trust.authenticate`：签名、时间窗、nonce、行状态 | `trust/` 持久行 |
| 准入 | 谁能开 thread、谁能进已有 thread | `src/node.ts` 的 `mayAdmit` 与 `inboundThread` | `trust/` 持久行的阶段投影 |
| 同意 | 用户批准过谁 | `/onlyne` 的指纹卡片与 `trust.begin`/`accept` | 人类动作，密封在已展示的指纹上 |

准入 MUST 读信任阶段投影（`exchanged`），MUST NOT 读发现目录。两个理由：发现只给出线索级候选（RFC 0017 §4）；而 `discovery.known` 是进程内集合，重启后为空，会把已经认证过的对端拒在门外。准入问的是用户批过没有，不是本进程见过没有。

投影经 `AdmissionContext` 从接纳事务里读（`context.trust(actor)`）。策略在接纳事务内部执行，自己再开一个 `storage.transaction` 会和外层互相等待，因此库只给这一个只读入口。

无密钥的回环开发绑定（RFC 0001 §1）是唯一不设准入闸门的形态：它接受任意声明的 actor，由 `server.bind` 必须是回环地址兜底。密钥存在时没有这条退路。

模型不能自行建立信任：`trust.begin`、`accept`、`revoke` 都限定节点 owner 身份，`msg` 在缺少可信密钥时只报告并停止（`src/tool.ts`）。确认必须重复 `begin` 展示过的指纹，确认一个没展示过的值会被库拒绝。

## 2. 模块布局

| 文件 | 职责 |
|---|---|
| `index.ts` | 扩展工厂。注册 URL 处理器、消息渲染、`msg` 工具、上下文绑定、todo 恢复、界面；在 `session_start`（仅主会话）启动运行时并挂状态行，`session_shutdown` 停止 |
| `src/runtime.ts` | 运行时生命周期：配置装载、SQLite、`Bun.serve`、端点、判定器、传输、密钥目录、维护时钟、域成员关系、发现、`recover` 与 `runMaintenance`；对外提供 `snapshot`、`status`、`notify`、信任动作、发现刷新 |
| `src/node.ts` | `OnlyneNode` 的装配与宿主插槽：认证、地址解析、密钥钩子、决策回调、执行上下文目录、mention 解析、准入与首接触许可（读信任阶段，见 §1.1） |
| `src/config.ts` | `config.toml` 解析、校验、原子写入；region 与 node id 的短名规范化 |
| `src/types.ts` | 插件内部类型：配置、执行上下文记录、界面快照、状态行快照 |
| `@onlyne/node-host/keys` | 密钥文件的读取、生成、权限检查、git ignore 检查、原子写入 |
| `src/store.ts` | `SqliteStorage`（`kv` 表，WAL，事务串行）与 `ContextRegistry`（执行上下文登记） |
| `@onlyne/node-host/network` | OMP 与网关共用的 DNS-SD 发布与浏览；候选交给 `discovery.observe` |
| `@onlyne/node-host/i18n` | `en` 与 `zh-CN` 的语言选择和插值；各宿主模块持有带相同键集合的双语目录 |
| `src/runtime.ts` 的发现部分 | 刷新、种子联系、同域候选核对；库只保存记录 |
| `src/decider.ts` | SystemOne：`msg` 出站判定、入站动作判定、todo 判定；状态裁剪、判定记录、机械前置检查 |
| `src/messages.ts` | `Runtime` 桥：注入、排队、子代理派发、自定义消息渲染、await 结果观察 |
| `src/tool.ts` | `msg` 工具：参数 schema、描述、调用判定、返回结构化事实、await 消费标记 |
| `src/binding.ts` | `task` 子代理与上下文别名的绑定；状态跟随 agent id，不跟随显示名 |
| `src/todo.ts` | 待办写入与恢复：todo-more 优先，原生 todo 兜底；来源关联的持久提交 |
| `src/todo-native.ts` | 原生 todo 的读取与相位转换 |
| `src/urls.ts` | `onlyne://` 内部路径注册；根、threads、inbox、contexts、outbound、neighbors、conversations、resources 的渲染与枚举 |
| `src/resources.ts` | 资源查询参数解析；默认 owner 集合为本节点和已完成交换的可信节点；实际读取交给库的 `client.readResources` |
| `src/tern.ts` | `TERN_PANE` 与 `tern ls --json` 查询；只用于 bootstrap 的 session 名建议 |
| `src/ui/` | `/onlyne` 的覆盖界面：`viewer.ts`（四页）、`bootstrap.ts`（首次表单）、`status.ts`（固定状态行）、`format.ts`、`index.ts`（命令注册） |

`decider.ts` 是最大的模块（约 1600 行），因为它同时持有判定输入构造、选择项文案、机械校验和记录。拆分它应当按判定点切，而不是按工具函数切；每个判定点的输入布局在 [systemone-context.md](systemone-context.md) 中有对应小节。

## 3. 启动与关闭

主会话的 `session_start` 依次执行：

1. 挂状态行（有 UI 时）；读取配置。缺配置是正常的首次状态，不报错，等 `/onlyne init`。
2. 打开 `omp.db`，`Bun.serve` 绑定 `server.bind` 与 `server.port`；端口为空时取 `0` 由系统分配，绑定成功后把实际端口写回配置，下一次启动复用。被占用时改用系统端口并写回新值，各记一条日志。由实际端口生成 endpoint，广播地址取 `advertisedHost(bind)`。
3. 构造判定器与 `Runtime` 桥，解析密钥目录（`project`/`global`/`none`），用会话定时器实现 `MaintenanceClock`。
4. 创建节点；把域成员关系对齐到配置的 region（先退出旧域，再加入新域，各用独立幂等键）。
5. 启动 DNS-SD（失败只记日志），登记主执行上下文，`client.recover()`，`client.runMaintenance()`。
6. 联系配置中保存的种子，立刻跑一轮发现，然后每 30 秒刷新一次。发现刷新是插件唯一的周期任务；投递节奏属于库。

关闭时停掉发现、HTTP 服务、维护环与存储。重启（`/onlyne config` 保存后）走同一条路径，先关旧运行时再建新的。

配置的顶层 `language` 选择 `en` 或 `zh-CN`，省略时读取 `ONLYNE_LANG`、`LC_ALL` 或 `LANG`，默认英文。`/onlyne language en|zh-CN` 保存配置并更新当前进程语言。动态文案在渲染或判定时读取语言；工具描述和参数说明在启动后及显式切换时重新注册，下一次模型请求使用新文本。已有消息正文、上下文摘要和待办内容保留写入时的语言。诊断事件名、协议字段、选项标识和判定关键词保持稳定，判定记录包含 `language`。

`/onlyne` 的发现页提供地址输入，接受节点服务 URL，并把成功联系的地址保存为启动种子。用户在同一界面继续核对指纹并交换密钥。自动发现覆盖同机和同链路节点，地址输入支持域名和远端 HTTPS 服务。

失败分级：配置缺失或非法只影响 `/onlyne`；密钥目录被拒（项目内未 ignore、权限不合格）使这次启动失败，因为无密钥绑定不允许非回环监听；发现失败只影响目录新鲜度。

## 4. 文件与存储

| 路径 | 内容 | 谁写 |
|---|---|---|
| `.onlyne/config.toml` | region、node id、bind、`port`、`key_scope`、`threads.page_size`、可选 language、种子与固定指纹 | bootstrap / config 界面、language 命令，以及运行时写回端口 |
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

资源读取用 `{ kind: "read", scope: "resources", query }` 表达。`meta.scope = "resources"` 的解析先于 thread 的 `op` 和资源操作，支持清单、详情、事件日志、单条事件、操作记录清单与单条操作状态。资源查询限定主会话，因为上下文登记没有资源授权字段。URL 的解析和枚举使用同一限制。

`onlyne://resources` 聚合各 owner 的第一页，保留每个 owner 的分页或错误。`onlyne://resources/<owner>` 读取单个清单；`/<owner>/<id>`、`/log[/<event-id>]` 和 `/operations[/<operation-id>]` 读取资源详情与记录。路径段采用百分号编码。游标只用于一个 owner，日志的 `since` 不包含起点。远端查询使用节点的签名传输并由资源 owner 校验权限，查询不经过 handler。读取操作状态直接调用已有操作记录接口，原调用者在交接后仍可读回自己的结果。

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
| todo | 有 `omp-todo-more` 时写它的快照与投影；否则写原生 todo。来源首行随所选语言渲染，消息引用保持不变 |
| 内部路径 | `onlyne://` 由 `InternalUrlRouter` 注册，`resolve` 渲染、`enumerate` 展开叶子文档 |
| 状态行 | 主会话 UI 组件，固定一行，2 秒刷新加 `notify` 触发 |
| Tern | 只用于 bootstrap 的 region 建议。界面本身不依赖 Tern：同一组件在纯终端走 ANSI，在 Tern 走 `describe` |

## 7. 当前实现与设计的偏差

设计以 RFC 为准。下面这些位置的实际代码仍按旧设定工作，修代码时按本节核对：

| 位置 | 现状 | 设计 |
|---|---|---|
| `ui/viewer.ts` 候选卡片 | 只有阶段文案与选中底色，蓝框/绿框未实现 | 本设计第 7 节：边框表达认证阶段 |

除上表外，本文描述的行为与 `packages/lib`、`packages/omp-onlyne` 的当前实现一致。协议层的符合性场景仍未在真实多进程环境验收，这一状态记录在证据文档，而不是本文。
