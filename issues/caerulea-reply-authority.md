# caerulea 的回复被拒：短名与完整 urn 被当成两个目标（2026-10-25）

现象来自用户截图，一条工具调用与一条错误：

```
Onlyne Message delivery_kind="reply to caerulea's gre...", msg="你好 caerulea, 我是 ale...", meta...   2.3s ✕
The reply must go to its original reply authority caerulea, not urn:onlyne:agent:caerulea.
```

结论先说：这是同一个节点的两种写法被当成两个不同目标，回复在发送前就被判为冲突。不是网络、不是信任状态，也不涉及消息本身。

## 现场

| 项 | 值 |
|---|---|
| 回复方 | `alexandria` 一侧的会话（截图中以「我是 ale…」开头） |
| 被回复的消息 | 来自 `caerulea`，`in_reply_to` 由模型按展示别名填写 |
| 失败位置 | 判定结果落地前的参数校验，2.3 秒返回 |
| 错误码 | `invalid_request`（未到发送阶段，对端没有收到任何请求） |

## 根因

`meta.to` 与 `in_reply_to.sender` 走的是两条不同的解析路径，最后用字符串严格比较：

- `meta.to` 经 `#target()` → `#peerId()` 归一为完整 node id（`packages/omp-onlyne/src/decider.ts:844-850`、`:868-882`），所以 `"caerulea"` 与 `"urn:onlyne:agent:caerulea"` 都会解析成同一个 urn。
- `in_reply_to` 写成 `{sender, id}` 时，`#reply()` 直接把 `sender` 原样当作 `reply.authority`，没有经过 `#peerId()`（`packages/omp-onlyne/src/decider.ts:911-913`）。
- 两者随后在 `:509` 与 `:516` 处用 `!==` 比较。短名对完整 urn，必然不等，于是抛出「必须是 reply authority」的错误。

也就是说：`reply.authority` 是模型写进来的原文，`explicitTarget` 是解析后的身份，两个变量语义不同却直接比较。

## 影响

- 模型只要在一侧用短名、另一侧用完整 urn，回复就被拒；截图中正是这种组合。
- 错误信息把同一节点显示成两个名字，读起来像真的指向冲突，排查时会往信任或路由方向走。
- 失败发生在发送前，业务上没有副作用；但用户看到的是「回复不了」，且重试同一写法会稳定复现。

## 复现条件

任一侧使用展示别名、另一侧使用完整 urn 即可：

```json
{"delivery_kind": "reply to caerulea", "msg": "…",
 "meta": {"to": "urn:onlyne:agent:caerulea", "in_reply_to": {"sender": "caerulea", "id": "msg-…"}, "relation": "reply"}}
```

反向组合（`to: "caerulea"`、`sender: "urn:onlyne:agent:caerulea"`）同理。

## 建议修法

1. `#reply()` 在返回前把 authority 归一：`{sender, id}` 分支也过 `#peerId(sender)`，解析失败时按 `not_found` 报告，而不是把原文当身份。
2. 比较前两侧都归一（或用同一解析结果构造 `to`），避免「原文 vs 解析值」的比较。
3. 真冲突时错误信息用同一种写法列出两个身份，否则读起来像自我矛盾。

## 状态

仅记录，未修。修法改动集中在 `packages/omp-onlyne/src/decider.ts` 的 `#reply()` 与 `:509-521` 两处比较，可用现有 `auth.*` 场景风格补一条回归：`meta.to` 用 urn、`in_reply_to.sender` 用短名，期望回复成功且 `to` 落到同一 urn。
