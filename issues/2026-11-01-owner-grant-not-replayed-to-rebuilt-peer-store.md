# owner 线程的 grant 不会向「存储被重建」的参与方补发：carly→caerulea 单向丢消息（notice 已 acked，对端无 grant/known）

日期：2026-11-01
报告人：urn:onlyne:agent:carly（Tern tern-onlyne 集成），现场在 alexandria/caerulea 节点

## 现象

两个互相 `exchanged` 信任的节点 carly（Tern companion，57777）与 caerulea（omp-onlyne，57802，omp 18.14.x），双向信任、签名、HTTP 都正常，但**只有一个方向丢消息**：

- **caerulea 作为线程 owner → carly：正常。** 10-31 / 11-01 她发起的线程（如 `5aeec703-afbd-4565-8e1e-171b0ebedf19`），carly 能收到 grant notice、收到消息、回 ack、沿同一线程回复；caerulea 侧 `entry/seq=2,4`、`inbox/...acked=true` 全部落库。
- **carly 作为线程 owner → caerulea：静默丢失。** carly 名下既有线程 `cb6b56c0-d9aa-4bc3-872a-00d5143c5006`（2026-10-27 建立、当时双方往返过 11 条）上 11-01 新发 seq=12、13，回执都是 `admission=accepted / guarantee=durable / resolved=caerulea`，carly 侧 notice `51bfa4ae-acd7-4e75-af7b-9bef95e5b0cc` 显示 `acked=true`，但 caerulea 的库里**没有任何对应 `grant/` 或 `known/` 行**，她的 joined 列表里没有该线程，`unread=0`，维护循环从不 reconcile 它。新建 owner 线程（`8237bb0a-150a-49b1-b82d-03d068bc94ef`，notice `288cb4f0`，同样 `acked=true`）也一样：caerulea 侧无 grant/known。

外在表现：发送方拿到的是「成功、durable、对端已 ack」，对端却永远看不到消息，没有任何错误日志或重试——失败被 ack 状态完全掩盖。

## 已确认 vs 推测

**已确认（磁盘账本）：**

1. carly 侧 owner notice 为 `acked=true`：Tern companion 库的 `notice/51bfa4ae-…`（线程 cb6b56c0）与 `notice/288cb4f0-…`（线程 8237bb0a）均 `{"recipient":"urn:onlyne:agent:caerulea",...,"acked":true}`。owner 只会在收到参与方 `POST /grants/<id>/ack` 后把 notice 置 acked（`packages/lib/src/threads.ts` 的 `ackGrant`，约 :374-387），说明这些 grant 历史上确实被 caerulea 身份接收并确认过。
2. caerulea 侧当前 store（`~/vibe-agent-working-dir/Alexandria/roles/caerulea/.onlyne/omp.db`）对 carly 名下线程**零记录**：无 `grant/<owner=carly>`、无 `known/<owner=carly>`；她库里现存的 `grant/`、`known/` 只指向 cyanea。trust、discovery、nonce 行里 carly 都在，唯独 grant/known 缺失。
3. 同方向「caerulea owner」的线程 grant/消息完全正常（`5aeec703` 双向 acked、entry 落库、注入触发了她的 turn）。
4. caerulea 的 omp 进程已整机重启过一次（旧 pid 60768 → 新 pid 7389，omp 18.14.2），重启前后现象一致，排除「只是维护循环卡死」。她节点能发消息、能建线程、能把 carly 加为 member（`member/.../carly`、`thread.create`、`thread.member` 幂等行都在），证明节点、密钥、出站、签名链路活着。

**推测（待 dev 判定）：**

caerulea 的 onlyne 本地存储在 10-27 之后某个时点被重建/清过（`.onlyne/omp.db`、`config.toml`、`keys/` 的文件 birth time 都是 2026-10-27 15:34，而信任/发现数据在新库里仍在；现存 grant 只覆盖重建后由 cyanea 发起的线程）。怀疑链路：**owner 侧的 grant notice 在首次 ack 后就不再重推（`client.ts#pushGrant` 读到 `notice.acked` 即直接 return），参与方一旦丢失本地 `grant/known`（store 重建、回滚、整文件覆盖），既不会重新收到旧 grant，也不会主动按 trust/discovery 去 reconcile owner 线程，于是该 owner→该参与方的后续消息永久不可见，而 owner 的 notice 仍是 acked，看起来一切正常。** 触发「store 重建 / grant 行丢失」的确切事件本次没抓到（lib 内未发现删除 owner 线程 grant 的常规路径；唯一显式删除是 `acceptGrant` 在 `previous_thread_ref` owner 身份迁移分支，`threads.ts:355-362`，本次 notice 不带该字段，不适用）。

## 证据（文件、键、行号）

- carly 发送与回执：`~/Library/Application Support/Tern/plugin-data/onlyne/onlyne.db`
  - `member/urn%3Aonlyne%3Aagent%3Acarly/cb6b56c0-…/urn:onlyne:agent:caerulea`（注：诊断中曾手动 `member_remove` 置 `state:revoked, revision:975a1c29-…`，未再恢复；这是诊断动作，非原始状态）
  - `notice/51bfa4ae-acd7-4e75-af7b-9bef95e5b0cc`（cb6b56c0，acked=true）、`notice/288cb4f0-ab39-4d36-b926-d816c078b3dc`（8237bb0a，acked=true）
  - `entry/…/cb6b56c0…/12,13` 与 `message/…/c4d842e9-…`（durable receipt，seq=12/13）
- caerulea 接收：`~/vibe-agent-working-dir/Alexandria/roles/caerulea/.onlyne/omp.db`
  - 无任何 carly-owner 的 `grant/`、`known/`（对照：`grant/…cyanea/966c652c-…`、`known/…cyanea/cd330cb9-…` 存在）
  - caerulea-owner 正常样本：`thread/…/5aeec703-…`、`entry/…/5aeec703/1..5`、`inbox/…/carly/00fbe294…` 与 `…/b2b18850…`（均 acked=true）
- 代码路径（`packages/lib/src/`）：
  - `client.ts` `#pushGrant`（约 :528-531）：`const notice = …tx.get(key('notice', member.revision)); if (notice === undefined || notice.acked) return;` —— acked 的 notice 永不重发。
  - `client.ts` `#deliverGrants`（约 :1509-1531）：只推 `threads.notices()` 列出的未 acked notice。
  - `threads.ts` `acceptGrant`（:335-370）：参与方落 `grant/` + `known/` 的入口；`joined()`（:192-217）靠扫本地 `known/`、`grant/`、inbox、回执决定要 reconcile 哪些 owner 线程。
  - `client.ts` `reconcile` / `#reconcileThread`（:590+、:1450+）：只遍历本地已知线程；没有已知行就永远不会去问 owner。
- 相关前科：`migration-owner-remap-grant-not-reissued`（owner 重映射时 grant 不重发）已由 `b4de14b`/`503785d` 修过，但那条只覆盖 im-gateway 的 owner **身份迁移**；本次是 **owner 身份不变、参与方本地 store 重建**后旧 acked grant 不补发，属另一触发面。

## 复现（最小）

1. A、B 两节点 exchanged。A 建 owner 线程并把 B 加为 member、发消息；确认 B 收到、`/grants/<id>/ack` 已回、A 的 notice=acked、B 库有 `grant/`+`known/`。
2. **只清空 B 的 onlyne 本地存储**（删除其 `omp.db` 或其中 `grant/`、`known/` 行），trust/keys/discovery 保留；重启 B（等价于 B 的 store 被重建）。
3. A 在同一线程再发一条。预期（坏）：A 侧 `accepted/durable`、notice 仍 acked、无重试；B 侧不出现 `grant/known`，joined 列表无该线程，`unread=0`，消息永远不可见。
4. 反向对照：让 B 新建一个自己 owner 的线程发给 A —— A 正常收到（证明不是网络/签名/信任问题）。

## 影响

1. **静默、单向、终态**：发送方拿到 durable + 对端 ack，实际对端永久收不到；无错误、无重试、无告警，只能靠对端发现 unread 异常。
2. **与常见运维操作叠加即触发**：参与方重建/回滚/清空本地 store（换机、重置、文件级恢复、存储迁移）在真实部署里会发生；一次重建会静默切断「所有对方 owner 且 notice 已 acked」的既有线程。
3. **掩盖在 ack 语义下**：acked 本应表示「参与方已持久持有」，但它不随参与方存储丢失而失效，owner 无法区分「对端真有」与「对端曾经有、现在没了」。

## 建议修法（供权衡，不锁死实现）

1. **让参与方能自愈**：B 启动或维护时，除扫本地 `known/grant`，对 trust 内活跃对端做一次轻量 reconcile（owner 侧提供「我把你当 member 的线程清单」，B 拉回并对缺失线程补 `acceptGrant` 再补消息）。覆盖面最大，不依赖 owner 记得重发。
2. **或让 owner 侧可重发**：给 grant notice 引入「参与方存储代次/游标」，参与方以新代次握手或 owner 发现对端 joined 视图缺线程时重新签发/重推 grant（现在 `notice.acked` 直接短路重发）。
3. **ack 语义可失效**：把 acked 与「参与方当前仍持有」解耦，允许 owner 在对端代次变化后把 notice 重新置为待发。
4. **至少加可观测性**：发送侧在 `accepted+durable+acked` 之外，周期核对对端 joined/attention 游标，发现「owner 认为已投递、对端无已知线程」时打 error 并触发补发，避免完全静默。

## 验收判据

- 第 2 步清空 B 的 `grant/known`（保留 trust/keys）并重启后，A 在旧线程再发消息：B 在下一个维护窗口内自动恢复 `grant/known`、joined 出现该线程、消息落 inbox（无需任何手工 member 操作）。
- 恢复不产生重复消息（幂等），也不要求 A 改线程 id 或 owner 身份。
- owner notice 状态能反映对端「当前是否持有」，或存在明确日志/指标暴露「已 acked 但对端 joined 缺失」。
- 反向（B owner → A）回归不受影响；`packages/lib/tests/verify.ts` 既有跨节点 grant/addressee 断言保持通过，并新增「participant store rebuild → owner thread re-syncs」用例。
