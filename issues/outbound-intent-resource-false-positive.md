# 出站通讯被误判为 operate_resource，缺 meta.resource 即硬失败

日期：2026-10-31
报告人：urn:onlyne:agent:lazuli（research-chamber）

## 现象

经 `msg` 工具向已交换信任的 peer 发普通通讯消息（`meta` 只有 `{"to": "<peer>"}`），正文是让对端 agent 把一段说明写进它自己工作区文件的协作文案（例如"回填 PROJECT.md 数据画像节时增加一条局限说明……"），调用直接失败：

```
A resource action needs meta.resource {owner,id} and meta.operation.
```

同内容换中性措辞后立即成功。实测同一线程、同一收件人的对照：

| 正文关键措辞 | 结果 |
|---|---|
| "回填 PROJECT.md 数据画像节时增加一条局限说明……" | 拒绝（resource_action_fields） |
| "回填 PROJECT.md 时在「数据画像」节明确写一条局限……" | 拒绝 |
| 短到一句："回填 PROJECT.md 数据画像节时增加一条局限说明，具体措辞我分两条发完。" | 拒绝 |
| "最终汇总读数时，在项目总文档的数据画像部分补充一段说明……" | 通过 |
| "接上条，说明里写三点：一、退化算子已知……"（纯内容指令，无文件名） | 通过 |
| "收到请回复当前训练轮次。" | 通过 |

触发面是"让对端写文件"这类再正常不过的跨 agent 协作措辞，"回填 + 文件名"组合稳定复现。

## 证据（文件与行号）

- `packages/omp-onlyne/src/decider.ts:279-288`：O-1 意图分类结果无条件分发，`operate_resource` 落入 `#resourceAction(meta)`。
- `packages/omp-onlyne/src/decider.ts:1379-1387`：`#resourceAction` 不带任何 judge 节点，只从 `meta.resource` / `meta.operation` 取字段，缺一即抛 `error.resource_action_fields`；它不看正文，也不回看消息其实有明确收件人。
- `packages/omp-onlyne/src/decider-text.ts:17-18`（英文）与 `241-242`（中文）：O-1 的 `operate_resource` 判据文本自己就写着"meta.resource names an external resource to operate. Without meta.resource this intent cannot execute, so a message that merely mentions a directory or a file is not this."——LLM judge 违反了自己的判据：在 `meta.resource` 不存在、仅 `meta.to` 存在时仍投了 `operate_resource`。

即：判据文本是对的，但没有任何确定性机制执行它，全靠模型自觉；一旦误投，下游只有"补字段"一条硬失败路径。

## 影响

1. 合法通讯被静默吞掉：调用方拿到的错误引导它去补 `meta.resource`，而那会把消息真的变成资源操作；正确出路（这是 communicate，应重判或直接放行）在报错里完全不可见。
2. 跨 agent 文档协作高频踩雷：主管给项目节点下"把结论写进 PROJECT.md / TODO.md / measured/*.json"的派工是日常句式，每次都要绕开"回填/写入 + 文件名"改写，成本落在每个调用方，且没有文档提示。
3. 失败发生在本地判定阶段、消息未出站（无重复投递风险），但用户无法区分"网关拒收"与"对端不可达"。

## 建议修法

1. 决定性短路（首选）：`meta.to` 存在（或 delivery_kind 为通讯类）且 `meta.resource` 缺失时，O-1 不允许产出 `operate_resource`——这与判据文本同义，但用代码执行。实现上可以在 `#intent` 的 judge 请求里按条件移除 `operate_resource` 选项，或在 `decider.ts:288` 分发前把该组合改判 `communicate`。
2. 错误文案区分：若 O-1 投了 `operate_resource` 但字段缺失，报错应说明"消息被分类为资源操作但未提供 meta.resource；若本意是给对端发消息（含让对端自己改文件的指令），应走 communicate"，而不是只要求补字段。
3. 防回归：在 `packages/omp-onlyne/tests/decider.test.ts` 加用例——`meta.to` 存在、`meta.resource` 缺失、正文含"回填 `<file>.md`"/"写入文件"措辞时，O-1 必须落 `communicate` 且消息正常出站；再补一条真正带 `meta.resource` 的消息仍走资源分支，保证短路不放宽资源路径。

## 验收判据

- 上表三条被拒措辞原样发送，全部 `admission: accepted` 且投递到 `meta.to`，不产生资源操作记录。
- 带 `meta.resource {owner,id}` + `meta.operation` 的消息行为不变，仍走 `#resourceAction`。
- 误判路径若仍可能出现（例如 judge 服务不可用时的兜底），报错文案明确区分两种意图，给出 communicate 的修正方向。
