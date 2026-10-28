# inject 不确认消费：投进已结束子代理上下文的条目每两秒重投一次

2026-10-27 21:00:42 至 22:47:32。本节点 `urn:onlyne:agent:onlyne-dev`，条目 `urn:onlyne:agent:carly/f4d18cb7-a669-47d6-9f47-cd1cd420f38e` seq 1。

## 现场实录

1. 21:00:42，该条目被判定投给新建的子代理上下文，`onlyne.inbound.inject` 开始以固定两秒节奏出现在日志里：

   ```
   21:00:42.005  onlyne.inbound.inject  thread=…carly/f4d18cb7…  seq=1  context=onlyne-c-1  outcome=delivered
   21:00:44.007  onlyne.inbound.inject  thread=…carly/f4d18cb7…  seq=1  context=onlyne-c-1  outcome=delivered
   …
   22:34:13.299  onlyne.inbound.inject  thread=…carly/f4d18cb7…  seq=1  context=onlyne-c-1  outcome=delivered
   22:47:32.856  onlyne.inbound.inject  thread=…carly/f4d18cb7…  seq=1  context=onlyne-c-1  outcome=delivered
   ```

   约 1 小时 47 分钟，三千余次，全部 `outcome=delivered`。

2. 同期其余七个条目都是 `acked: 1, injected: 1`，只有这一条是 `acked: 1, injected: 0`。

3. 收条人对该条目的结算消息（seq 2，`in_reply_to` 指向它）到达后，循环没有停止。

4. 把该条目所在的上下文标记为 `lost` 之后，循环仍在跑：本次运行的插件没有被重建（日志里 `onlyne.listening` 只有 20:25:28 一条，重载后没有第二条），新代码里的存活检查从未执行。

5. 22:47 把该 inbox 行标成 `injected: true` 之后，重投立即停止；22:47:32 是最后一次，之后日志里再无该条目。

## 根因

0. **登记进 `pending` 的 token 从来不在子代理的载荷里**（这一层比下面三条都靠前）。`packages/omp-onlyne/src/messages.ts` 的 `startSubagent` 先在任务文本里写 `[[onlyne-<uuid>]]`（`messages.ts:289`），再调 `registerPending(entry, child, "fragment")`（`messages.ts:304`），而 `registerPending`（`messages.ts:192`）内部又生成一个新的 token。于是 `pending` 里登的那个 token 永远不会出现在任何 provider 载荷里，子代理投递的消费在结构上无法确认——`registerPending` 现已接受外部 token，`startSubagent` 传它自己写进文本的那个。

1. **投递短路无条件报成功。** `packages/omp-onlyne/src/messages.ts` 的 `inject` 里，`attempt.landed` 一旦写入就直接返回 `{consumed: false, outcome: "delivered"}`，不检查落点上下文是否还能确认。投给主会话时这条短路会在模型读到消息后由 `consumption.confirmed` 终止；投给子代理时子代理结束后再没有任何人能确认，短路就变成了永久重投。

   补充：`new_subagent` 分支原本排在存活检查之前（`messages.ts:96`，检查在 `:104`），所以即使加了检查，子代理路径也绕过它。两者现在合并成先检查再分派。

2. **确认与投递没有生命周期边界。** `#consume`（`packages/lib/src/client.ts:1164`）只在 `record.injected` 为真时跳过。一条投递失败或落在已消失上下文里的条目，`injected` 永远是假，于是每个维护周期都重新调用一次宿主的 `inject`。

3. **上下文消失没有传导到投递状态。** 子代理结束不会改写它名下条目的状态；`context/c-1` 被标记 `lost` 也只影响后续判定，不影响已经落在内存 `inboundAttempts` 里的那一次投递。

4. **插件重载不重建运行时。** `packages/omp-onlyne/src/runtime.ts:52` 的 `startRuntime` 在配置未变时直接返回已存在的实例。所以即使修复代码已经落盘，重载也不会加载它——这一条单独值得记，因为它让「改了代码、重载一下」这个直觉失效。

## 已做的修复

- `3235f2f`：落点上下文不再是 `idle`/`running` 时标记 `lost`、丢弃内存中的 attempt、抛 `context_lost` 让下一轮重新判定。这需要进程重启才生效，重载不够。
- `f23ffe6`：`startSubagent` 现在把写进子代理任务文本的那个 token 交给 `registerPending`，而不是让它另生成一个；`new_subagent` 分支也移到存活检查之后，不再绕过它。
- 现场处置：把该行标为 `injected: true`，与库自己的语义一致（「这个上下文体已经拿到」），重投立即停止。
- `b4aee3e`：反方向的同一族缺陷——`fragment` 的定义是不开回合，而 idle 会话永远不会走到下一次上下文装配，于是载荷一直排在队列里、inbox 行一直是 `injected: false`：消息被接受，但永远没人看见。现场实证是 `97a7bfe7` seq 4（23:47 的来信），同批另一条 `injected: true, context: Main` 的早就到了。现在 idle 主会话直接按触发投递；连续若干次维护仍未被确认的投递强制投递一次，之后按 `onlyne.delivery.unconfirmed` 报出来，不再静默重报 delivered。

## 期望

- 投递进入一个上下文时 MUST 同时登记一次可回收的投递状态；该上下文消失后，这一次投递 MUST 被判定为不可确认，而不是当成已送达反复重报。
- 「已入队但未注入」MUST 是维护环会重新尝试的状态，不是终态：每次尝试失败 MUST 计数并留下可观察记录，MUST NOT 只写一次 `injected: false` 就什么都不做。
- 子代理结束 MUST 让它名下未确认的条目回到「待重新判定」，或者判定为不可投递，不得留在「已投递未确认」这个没有人负责的状态里。
- 宿主代码变更后 MUST 有一条真实重建运行时的路径；「配置未变则复用实例」不得成为代码变更不生效的原因。

## 影响范围

所有把来信路由到子代理或其它短生命周期执行上下文的部署。表现为日志与维护环被一条永不确定的条目占满，同时该条目之后再无任何来信被处理——因为维护环始终认为还有工作未完成。
