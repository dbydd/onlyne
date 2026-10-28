# 已交换但未核实的对端用 to 寄不出去

2026-10-27。发送方 `urn:onlyne:agent:onlyne-dev`，目标 `urn:onlyne:agent:carly`。涉及 `packages/omp-onlyne/src/decider.ts` 的 `#peerId()` 与 `packages/omp-onlyne/src/runtime.ts` 的 `listPeers` 过滤器。

## 现场实录

1. `onlyne://neighbors` 显示对端握手已 `exchanged`，但发现记录是 `unverified`：

   ```
   urn:onlyne:agent:carly · no region · unverified · handshake: exchanged · last seen ...
   ```

   同期其余五个对端在同一轮刷新里都是 `verified`，所以不是本节点发现停摆。

2. 用 `meta.to` 发一条消息：

   ```json
   { "delivery_kind": "…", "msg": "…", "meta": { "to": "urn:onlyne:agent:carly", "response": "none", "execution": "existing" } }
   ```

   返回 `No discovered node is named urn:onlyne:agent:carly.`

   失败时点：13:50 与 14:30 两次，间隔四十分钟，中间对端的 `last seen` 从 13:38 推进到 14:19，状态始终是 `unverified`。

3. 改走回复路径（`in_reply_to` 指向对方的消息，`relation: "reply"`）时，`#reply` 用 `#peerId(sender).catch(() => sender)` 回落到原始 URN，绕过了别名解析；随后卡在读对端公开发布的执行上下文：

   ```
   The target's published contexts cannot be read: The target could not be reached.
   ```

   两次失败都发生在发送之前，没有消息被投递。

4. 对端把地址固定在另一个端口重新起来后，本节点仍然找不到它：旧记录指向旧地址，探测不通过，新地址没有任何入口进入发现目录。

## 根因

1. **地址解析把发现层当成了投递前提。** `decider.ts:1276-1290` 的 `#peerId` 只从 `this.deps.config.node.id` 与 `listPeers()` 的返回值里取身份；`runtime.ts` 的 `listPeers` 过滤掉 `verified_at === undefined` 或已过期的记录。于是**信任行存在、签名能力存在，但发现记录一过期，这个对端就从可寻址集合里消失**。

2. **重新可达没有入口。** `discovery.resolve()` 只按该 `node_id` 名下已有的记录逐个探测（`packages/lib/src/discovery.ts:170-188`），对端换地址后旧记录探测不通，新地址既没有被观察过、也没有任何面向模型的入口可以写入。

3. **两种失败没有区分。** 「没发现过」与「发现过但核实失败」都收敛成同一个 `not_found`，读者无法从错误里判断是打错了别名，还是对端只是暂时没被核实。

这与 RFC 0011「发现不授予任何投递权或信任」以及 RFC 0018 §5.1「发现层不决定信任」的方向相反：信任已经由人类或委派确立，投递却仍要被发现层拦一次。

## 期望

- 已建立的信任行 MUST 让该对端保持可寻址：投递目标解析失败时，MUST 能用信任行里记录的 `base_url`（或显式地址）重试，而不是把对端当成未知节点。
- 别名解析与地址解析 MUST 分开：别名只解决「是谁」，可达性由传输层报告为 `unavailable`，不得把可达性问题报成 `not_found`。
- 对端换地址后 MUST 有一条把新地址写进发现目录的路径，且这条路径的结果对模型可见。

## 现状

根因第 1 条已修：`#peerId` 与目标提问现在读同一个来源 `knownPeers()`（`packages/omp-onlyne/src/decider.ts`），信任行（撤销除外）提供「是否存在」，发现记录提供地址，新鲜的覆盖陈述里声明的那个。提交 `6f6bc40`，其前一步 `b4d2682` 引入了信任行来源。上面第 1 条描述的是修复前的代码。

第 2、3 条仍未处理：对端换地址后仍没有一条写进发现目录、且对模型可见的入口；「没发现过」与「发现过但核实失败」在部分路径上仍收敛成同一个 `not_found`。

## 影响范围

所有依赖 `meta.to` 建立会话、或依赖 `execution: existing` 续写上下文的能力。对端重启换端口、跨网络迁移、代理地址变更都会触发，且表现为「明明握过手却发不出去」。
