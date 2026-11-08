# issues

本目录只放**未修的缺陷单**，一份一个文件：现象、证据（文件与行号）、影响、建议修法、验收判据。修完就删，历史在 git 里。

`archive/` 放 2026-10-25 到 10-27 的轮次报告（14 份现场证据）。它们是当时能复现、后来未必还在的观察，不是工单；需要考古时再读，平时不要拿它当现状。

## 未修（0）

（当前无未修工单）

## 已修归档（移入 archive/）

- `archive/2026-11-07-dsh-onlyne-path-session-coupling-and-directory-scope.md` —— `onlyne://` 读取改为进程级回退并附带节点标识；补齐 `onlyne://neighbors` 与 `plugins.neighbors`；`directory` 合并 `local`/`trusted`/`discovered` 三层来源；前端抽屉展示 `neighbors` 错误与来源标签。
- `archive/2026-11-07-dsh-runtime-await-violation-and-inbound-wakeloop.md` —— `omd-onlyne` 的 `runtime.supports` 对 `response: "await"` 与 `execution: "new"` 返回 `false`；`node.ts` 统一将 `notify` 接到 `wakeLoop()` 及串行立即维护轮次，HTTP 入站毫秒级进回合。
- `archive/2026-11-07-dsh-node-identity-listener-topology-and-ephemeral-storage.md` —— 节点身份稳定化并与会话解耦；全进程共享单 HTTP listener（配合 `@onlyne/lib` 子路径挂载与签名路由）；默认启用 `node:sqlite`/原子落盘存储；按需单飞配对与外部 `seeds` 观测；`onlyne://` POST 禁止静默冒用首节点。
- `archive/2026-11-07-omd-compute-oecp-v1-divergence-and-lease-ownership.md` —— `omd-compute` 支持顶层与 `meta.data` 双层 OECP v1 解析，回执补齐 `lease_id`、`connection.portal_urls.desktop`、`resolved_env`、`allocated`；强制校验 `owner`（非所有者返回 `forbidden`）；新增 `maxTtlMs` 硬上限（超限返回 `capacity_exceeded`）与到期 `expired` 通知；`omd-issue-runner` 增加 `ttl/2` 自动续期心跳。
- `archive/2026-11-07-issue-runner-worker-envelope-identity-and-catalog-visibility.md` —— RFC 0003/0005 与 `@onlyne/lib` 新增一级工作单元字段 `meta.work`；`omd-issue-runner` 按 `work` 幂等去重、解耦 `workerId` 与物理机器名、支持跨线程 `review`（`approve`/`reject`）并发布 `onlyne://runner/workers` 目录。
- `archive/2026-11-07-onlyne-lib-ts-source-exports-break-node-modules-and-asar.md` —— 全部 14 个 `@onlyne/*` 包通过 TypeScript `rewriteRelativeImportExtensions` 编译至 `./dist/`（含 `.js` 与 `.d.ts`），`main`/`types`/`exports`/`files`/`bin` 指向 `dist`，纯 Node 22+ 与 Electron ASAR 可直接从 `node_modules` 加载。
- `archive/bootstrap-init-start-failed-record-unavailable.md` —— `/onlyne init` 半初始化死锁与启动阶段 not_found 穿透；已通过 `isNotFoundError` 结构化判断、`saveConfig` 前预检 `ensureKeyDir`、以及放宽未运行节点的 `/onlyne init` 门禁修复。
- `archive/gateway-thread-row-lost-push-refused-forever.md` —— 网关 store 丢失 thread 行后启动自愈（`8337b1a` `ensureConversationThread`）并清理瞬时拒绝墓碑（`504e1fd` `TRANSIENT_REFUSALS`）。
- `archive/outbound-intent-resource-false-positive.md` —— 出站通讯被误判为操作资源；已由 `c7d74be` 的 `settleIntent` 确定性回退与问题项动态扣除修复。
- `archive/2026-11-01-owner-grant-not-replayed-to-rebuilt-peer-store.md` —— 对端 store 重建后丢失 grant；已由 `504e1fd` 的 `threads.issued` + client 维护循环反向 pull 修复。
- `archive/2026-11-01-sqlitestorage-tail-deadlock-description-timeout.md` —— SqliteStorage `#tail` 链排队死锁；已由 `504e1fd`/`c7d74be` 的 `AsyncLocalStorage` 重入拒绝、5s 队列超时与 `domains.declared` 内存缓存修复。
- `archive/menugui-thread-has-no-scrollbar.md` —— TUI 菜单线程列表无滚动支持及子会话并发事务；已由 `c7d74be` 的 viewer 原生滚动结构及 subagent 读写门禁修复。
- `archive/reconcile-unreachable-no-backoff.md` —— reconcile 无退避风暴；已由 `504e1fd` 的指数退避调度、`MAX_ATTEMPTS` 上限与时间窗口抑制修复。

2026-10-28 的五张单（主动发起解析到自己、短名与 urn 双写被拒、手填 UUID、网关上下文目录、信任对端不可寻址）已随「解析与投递收敛」一次修掉，验收判据落在 `packages/lib/tests/verify.ts`（phaseAddressee 跨节点断言、phaseInitiateTrust）与 `packages/omp-onlyne/tests/decider.test.ts`（O-2A 原消息选择、引用归一与冲突预检）的回归里；实施记录见 `docs/2026-10-28-audit-and-restructure.md`。遗留的结构性还债（decider 拆模块、client/http 拆文件、tern vendor→共享包）不是缺陷，走该文档第 6 步。

## 已修删除（留档于 git 之外的备份）

`injection-not-consumed.md`、`cyanea-trust-carly.md`、`migration-owner-remap-grant-not-reissued.md` —— 分别由 `3235f2f`/`f23ffe6`/`b4aee3e`、`6f6bc40`、`b4de14b`/`503785d` 修掉。
