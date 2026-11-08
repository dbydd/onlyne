# DSH 宿主对 await 一律承诺却没有同步回执，HTTP 入站不唤醒维护循环

日期：2026-11-07
报告人：dsh-onlyne 集成（DSH 进程内节点）
路径说明：插件已迁入 `onlyne/external-integrations/dsh-onlyne/`，迁移前位于 `oh-my-dsh/plugins/`。行号以迁入后的树为准。

## 现象

1. 外部对端向 DSH 节点发 `response: "await"` 的消息，admission 成功，对端一直等到超时。DSH 侧只多了一条排队进会话的用户消息，没有任何拒绝记录。
2. 从外部进程（HTTP）投递的消息落库后，最长 30 秒才进入 agent 回合。同进程投递没有这个延迟。

## 根因分析与证据

1. `createRuntime` 的 `supports` 恒为 true（`external-integrations/dsh-onlyne/omd-onlyne/src/index.ts:130`）。`inject` 只把消息排进 agent inbox 并返回 `{ consumed: true, outcome: 'queued' }`（:131-161），没有同步回执路径。lib 在 admit 阶段先问宿主 `runtime.supports(plan, message)`（`packages/lib/src/messages.ts:58-59`），宿主全 true，于是任何 plan 都被受理；支持与否的拒绝分支在 `messages.ts:80-89`，DSH 永远走不到。
2. RFC 0005 §3 末段要求：宿主接口无法表达等待语义时，接入必须在发送前拒绝无法兑现的 `await`，不得先发送再返回 handle（`docs/rfcs/0005-delivery-and-runtime.md:108`）。同一份文档 §5 还规定 handle 是另一种能力，不得与 `await` 共用能力名（:110）。DSH 两条都没有区分。

同类问题在 `packages/omp-onlyne` 与 `external-integrations/tern-onlyne` 上已由 `issue/application-protocol-multiplexing-and-tern-bot-requirements.md` 的 R4 记录；本文只记 DSH 这一份的落地缺陷，不重复那边的方案讨论。

3. `notify` 是空函数（`external-integrations/dsh-onlyne/omd-onlyne/src/index.ts:163-165`）。lib 把 `notify` 定义成唯一通知通道，节点对每次信任变迁、每条入站、每次维护结果都调用它（`packages/lib/src/types.ts:272-278`）；`messages.admit` 提交后调用两次（`packages/lib/src/messages.ts:98-99`）。DSH 没有把它接到 `client.wakeLoop()`，节点构造后也没有给 message service 绑定唤醒。R5 记录了同一个缺口。
4. 同进程投递之所以快，是 `HybridTransport.#local` 在成功后调用 `endpoint.wake?.()`（`external-integrations/dsh-onlyne/omd-onlyne/src/transport.ts:136`），只有本进程路径享受。HTTP 走 `FetchTransport`，接收方自己落库，唤醒只能等维护 tick。
5. 维护循环的空闲间隔是 30 秒（`packages/lib/src/client.ts:49` 的 `IDLE_INTERVAL_MS`），HTTP 入站的等待上限就是它。
6. `index.ts:162-164` 的注释把"同进程"的事实写成"不需要第二个通知面"；Network 抽屉每 5 秒轮询一次（`external-integrations/dsh-onlyne/omd-onlyne/src/client/index.js:44,491`），只刷新面板，不改变 agent 回合的延迟。

## 影响

1. 机器协议想用 await 拿租约描述符，会白等到超时；调用方拿不到"对端不支持等待"这个可读结论。
2. 人类从浏览器插话（`omd-context-tavern` 的 `review`/`close` POST 走 HTTP 入站）到 runner 进入回合，中间有几十秒空窗。
3. 对端看到"已受理、无回复"，无法区分对端忙与会话不支持等待。
4. 30 秒延迟是空闲态的平均值，节点越忙越难复现，问题容易被当成偶发。

## 建议修法

1. `supports` 按 plan 判定：`response: 'await'` 与 `execution: 'new'` 只在宿主能兑现时返回 true。DSH 当前没有同步回复通道，对 await 返回 false，让 lib 在发送方返回 `unsupported_delivery`。将来给机器协议加了同步 handler，再按 protocol 打开这个能力。
2. `notify` 接到 `node.client.wakeLoop()`，与同进程的 `endpoint.wake` 等价；同时在节点构造完成后给 message service 绑定同一个唤醒（R5 与 RFC 0005 需求 5 的落点）。
3. 注释与 README 改准：同进程走 registry.wake，HTTP 走 notify→wakeLoop，两者都以 admit 提交为触发点。

## 验收判据

- 外部节点向 DSH 发 `{ response: 'await' }`，发送端在提交前拿到 `unsupported_delivery`，DSH 侧不出现排队消息。
- 从外部进程 HTTP 投递一条消息，落库到进入 agent 回合的延迟小于 200 ms，连续三次测量取最大值。
- 同进程投递延迟不退化，现有 transport 测试保持通过。
- 代码里不存在空 `notify`：`notify` 内调用 `wakeLoop`，或由节点在构造时统一绑定。
