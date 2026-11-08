# onlyne:// 读取绑定调用方 session，neighbors 路径没实现，directory 只列本进程节点

日期：2026-11-07
报告人：dsh-onlyne 集成（DSH 进程内节点）
路径说明：插件已迁入 `onlyne/external-integrations/dsh-onlyne/`，迁移前位于 `oh-my-dsh/plugins/`。以下行号以迁入后的树为准。

## 现象

1. 浏览器 Details 抽屉的 Network 页在某些会话里只显示一个节点行，切到线程列表就报错：`onlyne:// is readable only from a session that hosts an Onlyne node`。同一次请求里 `onlyne://directory` 正常返回。
2. Network 页每次都读 `onlyne://neighbors`，每次都失败。抽屉把异常吞掉（`external-integrations/dsh-onlyne/omd-onlyne/src/client/index.js:476-484`），面板退回 directory 的行，用户看到的"对端"只有本进程节点。
3. 模型侧的工具说明把 `neighbors` 列成合法 scope（`external-integrations/dsh-onlyne/omd-onlyne/src/index.ts:191`），实际调用返回 `unsupported_delivery: No neighbor directory is available on this host.`（`packages/lib/src/client.ts:1552-1557`）。
4. `onlyne://directory` 只列 `sharedRegistry` 里的节点。外部进程的对端（omp-onlyne、tern-onlyne、qoder-onlyne）即使已交换信任，也不出现在这个答案里。

## 根因分析与证据

1. `sessionFor` 是 `onlyne://` 所有读的单一入口（`external-integrations/dsh-onlyne/omd-onlyne/src/index.ts:252-262`）：它按 `exec.agent` 或 `?session=` 找节点；给了 session 而该 session 在 NodeTable 里没有节点就直接抛；没给 session 时回落到 `nodes.first()`（:256），进程里一个节点都没有时同样抛。`directory` 分支用 `.catch(() => undefined)` 把这个异常吃掉（:348-352），`threads`、`inbox`、`threads/<alias>/messages` 都没有（:366-424）。`?session=` 来自 `/api/paths` 的查询参数或 `x-dsh-session` 头（`oh-my-dsh/plugins/omd-internal-http/src/index.ts:138,246`），抽屉一打开就带上当前会话 id，"会话存在但节点不在表里"这条路径直接被触发。
2. `grammar` 只声明三条路径（`external-integrations/dsh-onlyne/omd-onlyne/src/index.ts:315`），read 末段也只认这三条，报错文案列出的合法路径同样是这三条（:425）。`neighbors` 从未作为 internal path 注册。lib 侧的 neighbors 读依赖宿主提供 `plugins.neighbors`（`packages/lib/src/client.ts:1552-1557`、`packages/lib/src/types.ts:427-428`），`node.ts` 组装的 plugins 里没有这一项（`external-integrations/dsh-onlyne/omd-onlyne/src/node.ts:104-128`）。
3. `directory` 的答案来自 `registry.list()`（`external-integrations/dsh-onlyne/omd-onlyne/src/index.ts:357`、`src/node.ts:121-124`），registry 只保存本进程节点（`src/registry.ts:26-51`）。trust 表与 discovery 观察都没有合并进这个答案，UI 因此拿不到跨进程拓扑。
4. commit `361f261`（"make directory read independent of active session"）只改了 index.ts 的 directory 分支，`git show --stat` 显示改动面是 `plugins/omd-onlyne/src/index.ts` 一个文件；其余路径的 session 耦合没有动。

## 影响

1. UI 与脚本看到的是"只有本进程"的世界，跨进程对端是否信任成功在界面上无处可查。
2. 同一份"读拓扑"的需求有两套实现（internal path 与 msg scope），其中一套缺失，调用方按文档走会失败。
3. "这个 session 没有节点"与"地址写错了"共用一条错误文案，排障时无法区分。
4. 浏览器 UI 必须拿着正确的 session id 才能读到线程；人换了会话，或会话早于插件挂载，界面就空。

## 建议修法

1. 把 `onlyne://` 的读改成进程级：`?session=` 命中节点就用它，未给或没命中回落 `nodes.first()`，进程里没有任何节点时才抛。读的是进程记录，不要求调用方恰好坐在有节点的会话里。错误文案拆成"本进程没有挂载 onlyne"与"给定 session 没有节点，可用 session 是 …"两种。
2. 实现 `onlyne://neighbors`：节点提供 `plugins.neighbors()`，返回 trust 表条目（node_id、state、base_url、fingerprint）与 discovery 观察；`msg` 的 neighbors scope 与 internal path 调同一个函数。把 `neighbors` 加进 `grammar` 与工具说明。
3. `directory` 合并三层：本进程 registry、trust 表、discovery 观察，按 nodeId 去重，每行标注来源（`local`/`trusted`/`discovered`）。UI 只消费这一份答案。
4. 抽屉端不再吞掉 neighbors 的异常：失败时把错误文案显示在网络页，不静默回落成"没有对端"。

## 验收判据

- 在没有节点的会话里读 `onlyne://threads`、`onlyne://inbox` 不再抛错，返回体里带 `node`，说明实际使用的是哪个节点。
- `onlyne://neighbors` 返回数组；一个已交换信任的跨进程对端出现在其中，`state` 为 `trusted`；`msg` 带 `meta.scope: "neighbors"` 返回同一批行。
- `onlyne://directory` 同时列出只在本进程 registry 的节点与只在 trust 表的对端，来源字段分别为 `local`、`trusted`。
- 新增回归：registry 为空、trust 表有一条记录时，directory 仍返回该对端；session 不在 NodeTable 时 threads 读落到 `nodes.first()` 并在返回体里标明所用节点。
