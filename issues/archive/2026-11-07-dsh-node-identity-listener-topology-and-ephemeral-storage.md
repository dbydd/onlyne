# DSH 节点地址随会话漂移，一节点一端口，重启后存储与信任脱节

日期：2026-11-07
报告人：dsh-onlyne 集成（DSH 进程内节点）
路径说明：插件已迁入 `onlyne/external-integrations/dsh-onlyne/`，迁移前位于 `oh-my-dsh/plugins/`。行号以迁入后的树为准。

## 现象

1. 节点地址是 `urn:onlyne:agent:<sessionId>`（`external-integrations/dsh-onlyne/omd-onlyne/src/index.ts:551-553`），每开一个新会话就是一段新 UUID。对端与模型看到的是裸 UUID，无从判断这是哪个 Bot。
2. 每个 `agent/created` 都起一条 HTTP listener（`external-integrations/dsh-onlyne/omd-onlyne/src/node.ts:82-86`）。会话与 subagent 走同一条路径，ds-bot 一次任务常见几十个 subagent，端口数与节点数同阶。
3. 新节点启动时对 `registry.list()` 里每个既有节点做一次信任握手（`external-integrations/dsh-onlyne/omd-onlyne/src/node.ts:157-163`），代价是节点数的平方。
4. 进程密钥落盘（`external-integrations/dsh-onlyne/omd-onlyne/src/keys.ts:18-20`），节点状态用 `MemoryStorage`（`src/node.ts:105`）。重启后远端节点仍持有旧的信任行，DSH 侧线程与 grant 全空。
5. DSH 节点发不出首条消息给陌生对端。没有 seed 配置（`src/index.ts:46-75` 只有 enabled、region、threadPageSize、host、port），没有 discovery provider（`src/node.ts:91-96` 只查本进程 registry），发送时报 `No record answers for <peer>.`（`src/node.ts:95`、`src/transport.ts:56`）。
6. 浏览器 POST `/api/paths?uri=onlyne://threads` 不带 `session`（`oh-my-dsh/plugins/omd-context-tavern/src/client/index.js:2358,2381,3636`），`senderFor` 回落到 `nodes.first()`（`external-integrations/dsh-onlyne/omd-onlyne/src/index.ts:272-277`），人类的插话与收回被记在任意一个 Bot 名下。

## 根因分析与证据

1. 身份与传输绑在一次 `agent/created` 上：地址由会话 id 派生，节点生命周期跟随 agent（`external-integrations/dsh-onlyne/omd-onlyne/src/index.ts:551-570`）。会话是可抛弃的，地址因此无法长期寻址。
2. `network.port` 原样传给每个节点的 `startHttpListener`（`src/node.ts:84`，`src/service.ts:72`）。配置注释声称固定端口时每个后续节点绑定下一个端口（`src/index.ts:56-58`），实现没有递增逻辑。固定端口时第二个节点直接 EADDRINUSE，`startHttpListener` 的 `server.once('error', reject)`（`src/server.ts:38-39`）让该会话的节点启动失败，只在日志里留一行 warn（`src/index.ts:560-562`）。
3. 启动握手是 `Promise.all(registry.list())` 的急切全对全（`src/node.ts:157-163`）。transport 里本来就有按对端单飞的 `#pair`（`src/transport.ts:76-91`），启动期没有用它。
4. 存储默认内存（`src/node.ts:105`），密钥在文件（`src/keys.ts:18-20`），两者寿命不一致。对端 store 重建导致 grant 不补发的问题在 `issues/archive/2026-11-01-owner-grant-not-replayed-to-rebuilt-peer-store.md` 处理过一次；这次是 DSH 自己重建，方向相反，同样没有 reconcile。
5. `resolve` 只查 registry 与 `node.discovery`（`src/node.ts:91-96`），发现服务是 lib 的默认实现，本部署没有配任何种子或发现源。首连只能由对端发起，部署顺序被锁死。
6. `senderFor` 的兜底是为人类点卡片准备的，注释写明"第一个活节点够用"（`src/index.ts:264-271`）。它把发送者身份换成进程里的第一个节点，消息头里的 sender 因此与操作者无关。

## 影响

1. 线程无法跨会话续用：对端持有的 grant 指向一个下次不会再出现的地址。
2. 端口与握手随 agent 数增长，启动变慢；固定端口配置直接不可用，且失败只体现为某个会话没有 Onlyne 能力。
3. 重启后 DSH 认为自己没有任何线程，远端认为自己已信任，两边视图长期不一致，消息沉在 owner 侧。
4. 新部署必须先让外部节点主动握手，DSH 不能自举。
5. 人类消息的 sender 是随机 Bot，回执与线程归属可能落到别的会话。

## 建议修法

1. 身份与传输解耦：节点地址用稳定身份（例如 `urn:onlyne:agent:<bot 名>`，或 profile 里配置的名字），会话与 subagent 作为同一身份的多个 context，用 `context` 字段区分。地址可寻址即可，格式不必强求，但必须在重启与换会话后保持不变。
2. 一条 listener 服务本进程全部节点：按请求路径或 header 分发到对应节点，`network.port` 只决定这一个端口。删掉"每个后续节点下一个端口"的注释，或实现递增绑定。
3. 启动握手改成按需：用 transport 已有的 `#pair` 单飞，首次发送时配对；需要预热时按 registry 分组批量做，不做全对全。
4. 默认存储换成 `SqliteStorage`，与密钥同目录；启动时按信任表反向 reconcile 缺失的 owner 线程 grant，沿用 archive 里那条修法的方向。
5. `Config` 增加 `seeds: [{ nodeId, fingerprint, baseUrl }]`，填进 `registry.seeds` 与 `trust_seeds`；再接一个 discovery provider，让 DSH 能首连。
6. `senderFor` 不再静默选第一个节点：浏览器 POST 必须带 `session` 或显式 `meta.from`，缺失时返回错误并列出可用 session。

## 验收判据

- 同一个 Bot 重启后地址不变；外部对端不需要重新交换信任即可续用原线程，`/onlyne` 报告的 node id 前后一致。
- 并发 50 个 subagent 时进程只有一个监听端口，握手次数为 O(n)（用 `trust.entry`/`trust.request` 计数断言）。
- 配置固定 `network.port` 并起第二个会话，不再出现 EADDRINUSE。
- 配置一个外部 seed 后，DSH 主动发出首条消息成功，无需对端先握手。
- 不带 session 的浏览器 POST 返回明确错误并列出可用 session id，消息不以任意 Bot 身份出站。
