# omd-compute 报文与 OECP v1 层级不一致，租约没有所有权

日期：2026-11-07
报告人：dsh-onlyne 集成（omd-compute + omd-issue-runner）
路径说明：插件已迁入 `onlyne/external-integrations/dsh-onlyne/`，迁移前位于 `oh-my-dsh/plugins/`。行号以迁入后的树为准。

## 现象

1. 按 `issue/OECP-v1-specification.md` 的 acquire 报文（`meta.operation` 加 `container`/`resources`/`network`/`provider_meta`/`lease` 四组规格）发送，manager 只读到 `meta.data` 里的 operation、name、workspacePath、ttlMs、purpose，其余字段被忽略。
2. 回执只有 `name`、`state`、`workspacePath`、`owner`、`purpose`、`createdAt`、`expiresAt`、`ttlMs`，没有 `lease_id`、`connection`、`resolved_env`、`allocated`。
3. 任何节点都能对别人持有的机器发 `renew`、`release`、`destroy`，manager 只按机器名匹配。
4. `ttl_ms` 超过管理器默认值时被静默截断，不返回 `capacity_exceeded`。
5. 任务运行超过 6 小时后，机器在 sweep 里被停机；调用方（issue-runner）不会续期，也没有收到停机通知。

## 根因分析与证据

1. 报文字段位置不对：`operationOf` 从 `entry.message.meta?.data?.operation` 取（`external-integrations/dsh-onlyne/omd-compute/src/index.ts:119-123`），`nameOf` 从 `meta?.data?.name` 取（:126-130），`readSpec` 只取三个字段（:238-245）。OECP v1 把这些放在 `meta` 顶层（`issue/OECP-v1-specification.md:74-186`，`docs/rfcs/0019-sandbox-compute-resource-node.md:75-159`）。报错文案写成 `meta.name`（`external-integrations/dsh-onlyne/omd-compute/src/index.ts:129`），与实际读取位置不一致。
2. 描述符只有表内字段：`describeLease`（`external-integrations/dsh-onlyne/omd-compute/src/index.ts:89-100`）直接映射 `ComputeLease`，而 `ComputeLease` 没有 `lease_id`、`connection`、`resolved_env`、`allocated`（`src/types.ts:42-57`）。issue-runner 侧的 `ComputeDescriptor` 声明了 `desktopUrl`（`external-integrations/dsh-onlyne/omd-issue-runner/src/types.ts:209-217`），manager 从来不填。
3. 所有权只在写入时存：`acquire(spec, owner)` 记录 owner（`external-integrations/dsh-onlyne/omd-compute/src/manager.ts:110-138`），`release`/`destroy` 只按 name 取（:140-186）。没有 `renew` 动词，续期走的是 acquire 的"同名续期"分支（`src/index.ts:206` 与 :294）。读取时从不比对 owner。
4. `windowOf` 用 `defaultTtlMs` 当上限（`external-integrations/dsh-onlyne/omd-compute/src/manager.ts:73-80`），配置里没有 `max_ttl_ms`、`capacity_exceeded` 这类概念（`src/index.ts:69-86` 的 Config）。请求值超限被 `Math.min` 静默压回。
5. `compute://` POST 把 owner 写死成 `'compute://'`（`external-integrations/dsh-onlyne/omd-compute/src/index.ts:300`），路径调用与 Onlyne 调用共用一个假所有者，无法区分是谁在用。
6. issue-runner 的控制面只有 acquire 与 destroy（`external-integrations/dsh-onlyne/omd-issue-runner/src/types.ts:230-251`，`src/compute.ts:178-210`），没有 renew/heartbeat。`defaultTtlMs` 默认 6 小时（`external-integrations/dsh-onlyne/omd-compute/src/index.ts:74-75`）。

## 影响

1. 按规范实现的调用方（云上 broker、其它 agent 框架）无法对接，字段名与层级都对不上。
2. 租约没有所有权，多 Bot 场景下任意一方可以停掉别人的机器，误操作不可追责。
3. 长任务被中途停机，调用方看到的是命令突然失败，没有 `expired` 语义。
4. 连接信息只能另取 `omd-sandbox-vm` 的 `vm://`（`oh-my-dsh/plugins/omd-context-tavern/src/client/index.js:2542-2546`），OECP 描述符里的直连通道与动态环境变量没有落地。

## 建议修法

1. 报文按 OECP 分层：`meta.operation` 与 `meta.container/resources/network/provider_meta/lease` 读顶层，`meta.data` 只作过渡别名；读到的字段拼成同一个 Spec。补 `lease_id`（可先由 name 派生稳定 id），回执补 `connection`（exec_transport、endpoints、portal_urls）、`resolved_env`、`allocated`。
2. 所有权：lease 的 owner 取认证发送方 node id；acquire 命中他人 lease 返回 `forbidden` 或 `capacity_exceeded`；renew/release/destroy 校验 owner。管理动作（强制回收）单独命名，不挂在同一组动词上。
3. 补 `max_ttl_ms`、`idle_timeout_ms`、`retention_ms`：`windowOf` 区分"请求值超上限"与"合法值"，前者返回 `capacity_exceeded`，后者按请求值生效。
4. `renew` 独立成动词；issue-runner 加心跳（在 `ttl/2` 处续期），或由 manager 在临近过期时向 owner 推送提醒。达到 `max_ttl_ms` 的处置写进规范。
5. `compute://` 的 owner 用调用方 session 的节点 id，不写死。
6. 报错文案改成实际读取位置（`meta.data.name`），并与文档同步。

## 验收判据

- 按 OECP v1 发一条 acquire，回执里有 `lease_id`、`connection.portal_urls.desktop`、`resolved_env`，一个非 DSH 客户端能直接解析并使用。
- 节点 B 对节点 A 的 lease 发 renew/release/destroy 全部被拒并带明确错误码；A 自己发全部成功。
- `ttl_ms` 大于 `max_ttl_ms` 返回 `capacity_exceeded` 且机器不启动；`ttl_ms` 在范围内时 `expires_at - created_at` 等于请求值。
- 一个跨过 `defaultTtlMs` 的 8 小时任务靠心跳持续持有租约，sweep 不停机；心跳中断后按 `max_ttl_ms` 回收并通知 owner。
