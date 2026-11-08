# 2026-11-07 协议收敛与 DSH-Onlyne 集成缺陷修复设计及潜在风险审查（BDFL 决议）

日期：2026-11-07
范围：`docs/rfcs/`（0003、0005、0019）、`@onlyne/*` 全家桶构建导出、`external-integrations/dsh-onlyne/`（`omd-onlyne`、`omd-compute`、`omd-issue-runner`）

---

## 一、 六张缺陷单的全局耦合关系与架构根因

本次收到的 6 张缺陷单表面分布在打包、宿主接入、算力管理与任务编排四层，实则共享三个核心架构断层：

1. **线协议信封缺少工作单元一等公民（`meta.work`），且宿主用易失标识（`sessionId`、机器名）冒充协议身份**：
   - `omd-onlyne` 用会话 UUID 拼出 `urn:onlyne:agent:<sessionId>`，每开一个会话或子代理就起一个端口、做全对全 $O(n^2)$ 握手，且存储用 `MemoryStorage`，重启后密钥还在但信任表与 grant 全部蒸发；
   - `omd-issue-runner` 没有协议级 `meta.work` 幂等键，只能靠对话 thread 或机器名后缀反推 worker，跨线程发 `review` 直接脱靶并重开第二台 VM。
2. **协议能力声明与通知唤醒失真**：
   - `omd-onlyne` 的 `runtime.supports` 无脑返回 `true`（违背 RFC 0005 §3/§5），导致外部 `response: "await"` 白等超时；
   - `runtime.notify` 为空函数，导致跨进程 HTTP 入站无法唤醒 `client.wakeLoop()`，只能干等 30 秒空闲 tick；
   - `onlyne://` 读取强绑调用方 session，`onlyne://neighbors` 未实现，`onlyne://directory` 只列本进程内存表，跨进程已信任节点在 UI 中完全隐身。
3. **OECP v1 算力契约未闭环 + `@onlyne/*` 包导出指向 `.ts` 源码**：
   - `omd-compute` 只读 `meta.data` 下的扁平字段，回执缺失 `lease_id`、`connection`、`resolved_env`、`allocated`，且无租约 `owner` 校验、无 `max_ttl_ms`（用 `Math.min` 静默截断）、无独立 `renew` 与续期心跳；
   - `@onlyne/*` 14 个包的 `main`/`exports` 直接指向 `./src/*.ts`，在 Node 22+ / Electron ASAR 的 `node_modules` 下触发 `ERR_UNSUPPORTED_NODE_MODULES_TYPE_STRIPPING`。

---

## 二、 协议层修改决议（已落地 RFC 0003 / 0005 / 0019 & Host Contract）

### 1. RFC 0003 & RFC 0005：一级工作单元字段 `meta.work`
* **线协议形状**：
  ```ts
  export interface WorkRef {
    unit: string;
    parent?: string | null;
    attempt?: number;
  }
  ```
  挂载于 `Message['meta']['work']` 以及 `Action(kind: 'send')['work']`。
* **身份摘要（Digest）与路由规则**：
  - `messageDigest(request)` 与 `MessageService.admit` 的请求摘要覆盖 `meta.work ?? null`；
  - `routes/shared.ts` 的 `messageMeta` 合法键集合加入 `'work'`，校验 `unit` 为非空字符串、`parent` 为可选字符串或 null、`attempt` 为可选正整数；
  - `OnlyneClient.#prepare`：当 `action.work` 或 `input.meta?.work` 合法时，写入 `message.meta.work`（同时保留在 `meta.data` 中以兼容旧读取方）。

### 2. RFC 0019 & OECP v1：租约所有权、容量硬上限与续期心跳
* **报文双层兼容**：Broker 从 `entry.message.meta` 顶层及 `entry.message.meta.data` 归并读取 `operation`、`name`、`lease_id`、`container`、`resources`、`network`、`provider_meta`、`lease`、`purpose`（顶层优先）。
* **回执补齐**：`describeLease` 完整返回 `lease_id`（稳定派生或持久化 ID）、`name`、`owner`、`state`、`created_at`/`createdAt`、`expires_at`/`expiresAt`、`retention_until`/`retentionUntil`、`ttl_ms`/`ttlMs`、`workspacePath`、`connection`（含 `exec_transport`、`endpoints`、`portal_urls.desktop`）、`resolved_env`、`allocated`。
* **所有权（Ownership）铁律**：
  - `acquire` 记录 `owner = authenticated_author`（`compute://` POST 从 `exec` 解析真实会话节点 ID，禁止写死 `'compute://'`）；
  - 当同名 `acquire` 命中已有活跃或 `stopped` 租约时，若 `existing.owner !== owner`，拒绝并抛出 `forbidden`；
  - `renew`、`release`、`destroy` 必须校验 `lease.owner === callerOwner`，不匹配则拒绝并返回 `forbidden`。
* **容量上限（`max_ttl_ms`）铁律**：
  - 配置新增 `maxTtlMs`（默认 `86_400_000` 即 24h，或由配置指定；当请求带 `lease.max_ttl_ms` 时受限于配置 `maxTtlMs`）与 `idleTimeoutMs`；
  - 请求的 `ttl_ms`（或 `renew` 后的剩余/累计窗口）大于有效 `max_ttl_ms` 时，拒绝并返回 `capacity_exceeded`（机器绝不启动），废除 `Math.min` 静默截断。
* **独立 `renew` 与心跳保活**：
  - `compute.renew(nameOrLeaseId, ttlMs, owner)` 独立实现，仅刷新 `running` 租约的 `expiresAt`；
  - `omd-issue-runner` 在持有算力租约运行期间，按 `ttl / 2` 周期自动向 `omd-compute` 发送 `renew` 心跳，任务结束或 `dispose` 时停止心跳；
  - `sweep` 将到期租约停机（或达到 `max_ttl_ms` 强制回收）时，通过通知回调向租约 `owner` 发送到期通知。

---

## 三、 潜在问题与边界风险自检（Pitfall Analysis）

在派工实现前，针对 6 个模块的交互边界排查出以下 7 个高危潜在问题，并制定强制规避方案：

### 风险 1：`@onlyne/lib` 新增 `meta.work` 后导致旧测试或严格 Schema 校验失败
* **原因**：`packages/lib/src/routes/shared.ts:103` 的 `shape(value, field, ['thread', 'in_reply_to', 'relation', 'mentions', 'parts', 'data'])` 是白名单严格检查。如果只改类型不改 `shared.ts`，HTTP 传输携带 `meta.work` 会直接报 `invalid_request: message.meta.work is not a field this endpoint accepts.`。
* **还需注意**：`delivery.ts:131` 的 `messageDigest` 若对未传 `work` 的消息改变 JSON key 顺序或字段集合，是否会影响既有测试？在 `packages/lib/tests/verify.ts` 中，测试都是动态计算 digest，只要对 `meta?.work === undefined` 时保持向后兼容（或在 `messageDigest` 中仅当 `meta?.work !== undefined` 时包含，或者统一加 `work: meta?.work ?? null`——检查 `verify.ts` 是否有硬编码 digest 字符串：全文无硬编码 digest，但保持清晰一致最佳）。
* **对策**：在 `routes/shared.ts` 中同步加入 `work` 解析器 `workRef()`，在 `types.ts`、`delivery.ts`、`client.ts` 中端到端打通。

### 风险 2：`@onlyne/*` 编译产物指向 `dist/` 后，Bun 工作区测试与 `bun:sqlite` 兼容性问题
* **原因**：
  1. `packages/node-host` 等包使用了 `import { Database } from "bun:sqlite"`，而 `@onlyne/lib` 是纯标准 Web Crypto / Fetch / Node API，无 `bun:*` 依赖。
  2. 如果用 `tsc` 编译所有包到 `dist/`，源码中的相对导入写的是 `from './types.ts'`（开启了 `allowImportingTsExtensions`）。直接跑 `tsc` 生成 `.js` 时，如果不开启 `rewriteRelativeImportExtensions: true`（TypeScript 5.7+ 原生支持，当前仓库 TS 版本为 `^5.9.0`！），输出的 `dist/index.js` 里还会保留 `import ... from './types.ts'`，在 Node 22+ 下依然会因为 `.ts` 后缀崩掉！
* **验证与对策**：
  - 本机 TypeScript 为 5.9，原生支持 `"rewriteRelativeImportExtensions": true`！开启后，`tsc` 会自动把 `import './core.ts'` 改写为 `import './core.js'`，同时生成 `.js` 和 `.d.ts`！
  - `package.json` 的 `exports` 采用标准条件导出：
    ```json
    "main": "./dist/index.js",
    "types": "./dist/index.d.ts",
    "exports": {
      ".": {
        "types": "./dist/index.d.ts",
        "default": "./dist/index.js"
      }
    }
    ```
  - 这样无论是 Node 22+、Electron ASAR、还是 Bun，全部统一加载编译后的标准 ESM `dist/*.js`，彻底消除开发态与打包态分裂！且在每个包的 `check` / `build` / `prepack` 脚本中生成 `dist/`。

### 风险 3：`omd-onlyne` 改为“单进程单 HTTP Listener + 稳定身份”后，多节点路由与签名验签冲突
* **原因**：
  - `packages/lib/src/keys.ts:156` 的 `requestPayload` 签名载荷包含 `path: url.pathname + url.search`。
  - 如果单端口复用采用 URL 前缀路由（如 `http://127.0.0.1:PORT/nodes/<nodeId>/onlyne/v3/...`），而 `createHttpHandler`（`packages/lib/src/http.ts:65`）硬编码要求 `url.pathname.startsWith('/onlyne/v3/')`！直接加前缀会导致 `createHttpHandler` 报 `not_found: Unknown route.`；而如果在转发给 `node.fetch` 前剥离前缀，又会导致接收方验签计算的 `url.pathname` 与发送方签名时的 `url.pathname` 不一致，触发 `401 Invalid request signature`！
* **对策（关键架构设计）**：
  - 仔细看 `packages/lib/src/keys.ts` 与 `http.ts`：
    1. 认证路由全部带有 `Onlyne-Sig: v1 <actor> <timestamp> <nonce> <signature>` 或 `Onlyne-Actor` 请求头，且在签名时 `requestPayload` 包含 `target`（接收方 `node_id`）；对于公开路由（`GET /onlyne/v3/keys`、`GET /onlyne/v3/description`、`POST /onlyne/v3/introductions`）：
       - `POST /onlyne/v3/introductions` 的请求体 JSON 里直接有 `target: <nodeId>`！
       - `GET /onlyne/v3/keys` 和 `GET /onlyne/v3/description`：如果支持查询参数 `?node=<nodeId>`（或者路径 `/onlyne/v3/...` 根据 query/header/body 分发，且因为 `createHttpHandler` 只检查 `url.pathname.startsWith('/onlyne/v3/')` 而忽略未知 query 参数），那么 `baseUrl` 可以是 `http://127.0.0.1:PORT/onlyne/v3?node=<encodeURIComponent(nodeId)>` 吗？
       - 等等！看 `keys.ts:215`：
         `if (url.username || url.password || url.search || url.hash ...) fail('invalid_request', 'Expected an absolute node service URL.')`！`peerBase` 严禁 `base_url` 带有 `search` 或 `hash`！
       - 那 `createHttpHandler` 呢？看 `http.ts:65`：
         `if (!url.pathname.startsWith(`${PREFIX}/`)) fail('not_found', 'Unknown route.');`
         为什么 `http.ts` 只认 `/onlyne/v3/`？如果我们在 `@onlyne/lib` 的 `http.ts` 中支持可选的挂载前缀（或者匹配最后一个 `/onlyne/v3/` 段：`const idx = url.pathname.indexOf(`${PREFIX}/`)`），会发生什么？
       - **神级发现**：在 `@onlyne/lib/src/http.ts:65` 中：
         目前是：
         ```ts
         if (!url.pathname.startsWith(`${PREFIX}/`)) fail('not_found', 'Unknown route.');
         const path = url.pathname.slice(PREFIX.length).split('/')...
         ```
         如果改为查找 `PREFIX`（或支持 `ctx.baseUrl` 的 pathname 前缀，例如 `base_url = http://127.0.0.1:PORT/nodes/<encodedNodeId>/onlyne/v3`）：
         当 `base_url` 为 `http://127.0.0.1:PORT/nodes/<id>/onlyne/v3` 时：
         - `new URL('./keys', base.href + '/')` 生成的 URL 是 `http://127.0.0.1:PORT/nodes/<id>/onlyne/v3/keys`（无 query、无 hash，完全通过 `keys.ts:215` 的 `address()` 校验！）；
         - `FetchTransport` 发送请求时，`new URL(path, base)` 也会拼出 `http://127.0.0.1:PORT/nodes/<id>/onlyne/v3/threads/...`；
         - 发送方签名时，`url.pathname` 是 `/nodes/<id>/onlyne/v3/threads/...`；
         - 接收方共享 Listener 收到请求后，按 `/nodes/<id>/onlyne/v3/...` 路由到对应 `node.fetch(request)`（**原封不动传入原始 `request`，不篡改 URL**）；
         - `@onlyne/lib` 的 `http.ts` 解析路径时，若 `url.pathname` 包含 `/onlyne/v3/`（匹配 `base_url` 的路径前缀或 آخر `/onlyne/v3/`），从 `/onlyne/v3` 之后切分出路由段 `path`，而验签（`authenticate`）依然读取原始 `request.url` 的完整 `url.pathname + url.search`！
         - 同时，如果请求直接打在根路径 `/onlyne/v3/...`（无 `/nodes/<id>` 前缀），共享 Listener 可以按 Header / Body `target` / 默认主节点路由！
  - **结论**：只需在 `@onlyne/lib/src/http.ts` 放宽 3 行代码（允许 `url.pathname` 带有 `ctx.baseUrl` 的路径前缀或 `/onlyne/v3/` 子路径），单端口多节点复用就能做到 **100% 协议原生、签名零篡改、公开路由（`/keys`、`/introductions`）与认证路由全兼容**！

### 风险 4：稳定身份与多会话/子代理（Subagent）共存
* **原因**：Issue 指出 DSH 每次任务可能并发 50 个 subagent。如果每个 subagent 都调用 `createBotNode` 建独立节点，即使共享端口，也会建 50 个节点实例。而 Issue 建议修法明确指出：
  1. 节点地址使用稳定身份（如配置或 agent 名字 `urn:onlyne:agent:<name>`，默认 `urn:onlyne:agent:dsh` 或按稳定 profile/bot 名），会话与 subagent 作为同一身份下的多个 `context`，或同身份节点在 `NodeTable` 中复用同一个 `BotNode` 实例！
  2. 同时，对于通过 `createOnlyneService().createNode()` 显式创建的不同 `nodeId`（如 `urn:onlyne:agent:issue-runner`、`urn:onlyne:agent:compute`），它们共享同一个进程级 `SharedHttpListener`（只占 1 个 OS 端口），因此无论起多少个会话或 50 个 subagent，全进程始终只有 **1 个监听端口**，且启动握手采用按需单飞 `#pair`（$O(n)$ 而非 $O(n^2)$）！

### 风险 5：持久化存储（SqliteStorage / 文件存储）与跨重启 Grant 恢复
* **原因**：`omd-onlyne` 之前用 `MemoryStorage`，密钥却落盘在 `<dshHome>/data/omd-onlyne/key.json`，导致重启后远端认为已交换信任并已发过 grant，而本地表全空。
* **对策**：
  - 在 `omd-onlyne` 中提供基于 SQLite（Node 22.5+ 内置 `node:sqlite` `DatabaseSync` 或 `bun:sqlite` 双兼容封装，或与 `key.json` 同目录的原子持久化存储/SQLite 存储 `<dshHome>/data/omd-onlyne/storage.sqlite`）作为默认 `Storage`（`guarantee: 'durable'`）；
  - 节点启动时，`OnlyneClient.maintain()` 内部已有 `#resyncGrants`（会向所有 `exchanged` 的信任对端调用 `GET /grants/issued` 找回缺失的 `known` 与 `grant` 行），配合持久化存储与启动时的 `wakeLoop()` / 对账，重启后无需重新交换信任即可续用原线程！

### 风险 6：`senderFor` 无会话回退导致人类浏览器操作冒用随机 Bot 身份
* **原因**：`onlyne://` 的 `read` 应当是进程级的（`?session=` 未命中或未传时回退 `nodes.first()` 并在返回体标明 `node: bot.nodeId`），但 `onlyne://` 的 `POST`（发送消息）绝不能静默回退到 `nodes.first()`！
* **对策**：`senderFor` 严格要求 `exec.agent`、`exec.sessionId`（`?session=`）或 `body.meta?.from` 显式指定发送节点/会话；缺失或未命中时抛出明确错误并列出当前可用的 session / node 列表。

### 风险 7：`omd-issue-runner` 机器名与 Worker ID 解耦后的孤儿回收
* **原因**：之前 `reclaimOrphan` 从 `onlyne://runner/issue/<id>` 正则提取 `<id>` 拼出机器名。解耦后，worker 地址仍为 `onlyne://runner/issue/<workerId>`（稳定可枚举），runner 内部维护 `workerId -> { jobId, machine, workUnit, ... }` 映射（并持久化或同步到 `onlyne://runner/workers` 目录与 `ctx.onlyne` 注册表视图）。当按 `meta.work.unit` 重复投递 `issue` 时，直接命中已有活跃 job 并返回同一个 worker 地址，绝不开第二台 VM。

---

## 四、 并行派工切分与文件所有权边界（零冲突矩阵）

为确保多个 `engineer` 子代理并行修改时不发生文件踩踏，按目录与职责切分为 **4 个严格正交的并行工作包（Slices）**：

| 工作包 | 负责代号 | 独占文件范围 | 核心交付与验收 |
|---|---|---|---|
| **Slice A：协议核心与全包构建导出** | `SliceLibAndBuild` | `packages/*/package.json`<br>`packages/*/tsconfig.json`<br>`packages/lib/src/**`<br>`packages/lib/tests/**`<br>`package.json`（根构建脚本） | 1. `@onlyne/lib` 实现 `meta.work`（`types.ts`、`routes/shared.ts`、`delivery.ts`、`client.ts`）与 `http.ts` 子路径前缀挂载支持；<br>2. 全仓 `packages/*` 配置 `tsc` 构建输出到 `dist/`（启用 `rewriteRelativeImportExtensions` + `declaration`），更新 `main`/`types`/`exports`/`files`/`prepack`；<br>3. 验证 `node -e "import('@onlyne/lib')"` 与 `bun run check && bun run test` 全绿。 |
| **Slice B：`omd-onlyne` 宿主身份、单端口、存储、唤醒与路径** | `SliceOmdOnlyne` | `external-integrations/dsh-onlyne/omd-onlyne/**` | 解决 3 张单（`path-session-coupling`、`runtime-await-wakeloop`、`node-identity-listener-storage`）：<br>1. `supports` 诚实拒绝 `await`；`notify` 触发 `wakeLoop()`；<br>2. 稳定节点身份 + 进程级单 HTTP Listener 端口复用 + 按需单飞握手 + 默认 SQLite/持久化存储 + `seeds` 配置与 `discovery` 解析；<br>3. `onlyne://` 进程级读取回退、`onlyne://neighbors` 与 `plugins.neighbors` 实现、三层合并 `onlyne://directory`（`local`/`trusted`/`discovered`）、`senderFor` 强制显式会话校验；<br>4. 浏览器抽屉 `client/index.js` 显示 `neighbors` 错误而非吞掉。 |
| **Slice C：`omd-compute` OECP v1 报文、所有权与容量边界** | `SliceOmdCompute` | `external-integrations/dsh-onlyne/omd-compute/**` | 解决 `omd-compute-oecp-v1-divergence-and-lease-ownership`：<br>1. 顶层 `meta.*` 与 `meta.data.*` 双层解析归并；<br>2. 回执补齐 `lease_id`、`connection`（含 `portal_urls.desktop`）、`resolved_env`、`allocated`；<br>3. 强制 `owner` 校验（非 owner 的同名 acquire / renew / release / destroy 拒绝并返回 `forbidden`）；<br>4. 新增 `maxTtlMs`、`idleTimeoutMs`，超限返回 `capacity_exceeded`（取消 `Math.min` 静默截断）；<br>5. 独立 `renew` 动词、到期 `expired` 通知、`compute://` 动态会话 owner。 |
| **Slice D：`omd-issue-runner` 工作单元幂等、目录发布与租约心跳** | `SliceOmdIssueRunner` | `external-integrations/dsh-onlyne/omd-issue-runner/**` | 解决 `issue-runner-worker-envelope-identity` 及 `omd-compute` 联动项：<br>1. 支持 `meta.work`（`{ unit, parent?, attempt? }`）作为幂等键，重复投递同一 `work.unit` 的 `issue` 复用已有 job/VM 并返回原 worker 地址；<br>2. 发布可枚举的 `onlyne://runner/workers`（含 worker、machine、thread、state、startedAt、workUnit）及在 `ctx.onlyne` / 内部路径暴露只读视图；<br>3. 机器名与 worker id 解耦；<br>4. `compute.ts` 与 `vm-agent.ts` 升级 OECP v1 报文格式，并增加 `ttl / 2` 自动 `renew` 心跳与到期处理。 |
