# RFC 0003：消息、接纳与回执

状态：Draft。依赖：RFC 0001、RFC 0002。

## 1. 消息是唯一事实

消息包含一个发送者定义的稳定 id、不可变正文和显式关系。消息没有任务状态。completed、pending、failed、blocked 这些词不描述消息，也不描述 thread；投递记录的 `admission.state` 取值 `pending` 属于投递在途，不是任务状态。

一条消息具有发送方声明的目标 endpoint 和目标 ThreadRef。消息的 `id` 由发送方在第一个字节之前确定，跨节点保持不变。一条消息只归属一个 thread。

```json
{
  "id": "msg-01",
  "sender": "urn:onlyne:agent:dpdp",
  "delivery_kind": "等你看完给我结论",
  "msg": "请核对这次实验的结论。",
  "meta": {
    "in_reply_to": null,
    "thread": {"owner": "urn:onlyne:agent:lazuli", "id": "thr-research"},
    "mentions": ["@all"],
    "parts": [],
    "data": {}
  }
}
```

`in_reply_to` 是 MessageRef。库在发送前把本地来信句柄翻译成该 MessageRef，网络只传可验证的引用。接收方不解析对方的本地句柄。

`mentions` 是字符串数组，表达该消息希望谁主动关注。取值为 `@all`（全体成员关注）或特定的 endpoint 别名 / `node_id`（单发关注）。省略或为空表示普通通信。

### 消息存盘与投送关注分离

协议严格区分**消息在 thread 中的存盘记录**与**向成员的主动投送关注**：

| 通信方式 | 标记形式 | thread 存盘 | 主动投送与提醒 |
|---|---|---|---|
| 单发关注 | `mentions: ["@reviewer"]` | 正常追加并分配 seq | 仅向被 @ 的成员发送主动提醒/唤醒 |
| 群发关注 | `mentions: ["@all"]` | 正常追加并分配 seq | 向该 thread 全体成员发送主动提醒/唤醒 |
| 引用回复 | `in_reply_to: <MessageRef>` | 正常追加并分配 seq | 主动提醒原消息作者，若对方处于 await 则解除等待 |
| 普通通信 | 无 mention 且无特定引用 | 正常追加并分配 seq | **不主动打扰任何人**，成员闲时主动向主持人拉取 |

无论是哪种通信方式，只要被 thread 接纳，消息都会分配全局唯一的递增 `seq` 存盘。成员节点直接看到的提示默认仅收录需要自己关注的消息，完整 thread 历史由 agent 主动向主持人查询。

## 2. 三个不同的事实

| 事实 | 谁确认 | 含义 |
|---|---|---|
| 持久接纳 | 接收节点 | 消息已进入接收节点的持久队列，等待安排处理 |
| 模型收到 | 运行时 | 消息文本进入模型可见上下文 |
| 业务答复 | 回复方 | 一条显式关联原消息的新消息 |

三者独立。接收节点确认接纳不代表模型已经看到。模型看到不代表业务完成。

持久接纳的证据是回执。回执不进入模型可见的消息列表，运行时可以单独查询。

### 关注义务

接纳一条消息时，owner 在提交消息条目、回执与待处理项的同一事务里，为每个应主动关注的接收方提交一条关注义务。关注义务的标识是 `(thread_ref, message_ref, recipient)`，携带该条目的 `seq`。义务只由 owner 写入；成员持有的副本是观察值。

义务在 thread 关闭后继续存在。关闭停止新投递，不处置已经提交的义务。

成员通过 owner 的认证关注清单读取义务（接口见 RFC 0004 §1）。清单按不透明游标分页，条目携带 owner 证明的接纳事实：`thread_ref`、`seq`、`message_ref`、认证作者、`relation`、`in_reply_to` 的完整引用，以及 owner 在接纳时已校验作者身份这一事实。通知和事件流是提示，清单是恢复依据。

确认（`ack`）表示成员已经在本节点持久保存该条目的正文或可恢复引用。`ack` 按义务标识幂等，不能确认其他成员或其他条目的义务。成员在持久保存之前不得确认。内容已按保留策略过期的义务，成员在持久记录该过期结局之后可以确认；这次确认表示成员记录了结局，不表示成员仍持有正文，也不重新打开义务。

成员必须持续对账未确认义务：重启、长连接中断、提示丢失，或本地读完一轮之后，都要重新枚举已知 owner 的清单。对账由库的维护环在每轮维护中执行，与消息维护同一节奏；网络发现的调度属于宿主接入的下层网络协议，库只暴露发现目录接口与插件钩子，由插件按实际网络形态驱动。具体职责与场景见 [《lib 宿主契约》](../contracts/lib-host.md) §10。已知 owner 的集合来自成员加入 thread 时 owner 发出的持久授权通知；该通知带稳定标识、`thread_ref`、接收方身份和成员版本，owner 重试到成员确认持久记录为止。成员在通知到达之前不知道这个 owner，因此 owner 不得把义务只留在一次通知里。

## 3. 发送

发送者先落盘自己的出站记录，再发送。出站记录不是 thread 条目：它没有 seq、没有成员、没有生命周期，只保存这条消息投递到哪里、证据是什么。

```json
{
  "outbound": {
    "message_ref": {"sender": "urn:onlyne:agent:dpdp", "id": "msg-01"},
    "target_endpoint": "urn:onlyne:agent:lazuli",
    "thread_ref": {"owner": "urn:onlyne:agent:lazuli", "id": "thr-research"},
    "relation": null,
    "admission": {"state": "pending"},
    "reply_authority": "urn:onlyne:agent:lazuli",
    "local_landing": null,
    "retry_until": "2026-10-23T03:00:00Z"
  }
}
```

`admission.state` 取 `pending`、`accepted`、`refused` 或 `unknown`。`refused` 保存对端错误码。`unknown` 在期限到达后仍是终态，不改成 `undelivered`。

`local_landing` 仅在窄回复许可或跨线程引用例外下使用。常规对话中，完整线程由最初发起者（agent 或 IM gateway）持有，所有往来及群回复统一存入发起者持有的 thread。

```json
{
  "retry_until": "2026-10-23T03:00:00Z",
  "message": {
    "id": "msg-01",
    "sender": "urn:onlyne:agent:dpdp",
    "delivery_kind": "等你看完给我结论",
    "msg": "请核对这次实验的结论。",
    "meta": {"thread": {"owner": "urn:onlyne:agent:lazuli", "id": "thr-research"}}
  }
}
```

`POST /threads/{owner}/{thread_id}/messages`

这些路径相对 `/onlyne/v3`。发送时用 `Idempotency-Key` 请求头承载幂等键。省略目标 thread 使用 `POST /messages`，由认证目标节点解析默认落点。路径与消息中的显式 ThreadRef 必须一致。

顺序固定：

1. 认证调用方并校验其有权代表 `sender`。普通直连发送要求 sender 等于 actor。绑定转发必须有单独授权，授权记录同时指定该消息的回复权威。
2. 按 `(sender, id)` 查找接收方已存的接纳结果。结果包括已接纳和终态拒绝。命中且内容相同返回原结果。命中且内容不同返回 `id_conflict`。
3. 内容相同由身份摘要判定。摘要覆盖 `sender`、目标 endpoint、ThreadRef、`in_reply_to` 的关系目标、relation、`mentions`、`delivery_kind` 原文、`msg` 正文、`meta.parts`、`meta.data`。`retry_until` 不属于身份。
4. 评估成员和 thread 状态。非 post 成员或 thread 关闭返回 not_a_member 或 thread_closed，并把该终态拒绝与消息身份一起记录。判定服务暂时不可用等可重试失败不写终态拒绝。
5. 提交消息条目、回执和待处理项，再返回 201。

消息身份摘要 MUST 覆盖 `delivery_kind`。发送方修改意图文本时，消息就是不同的消息。把新意图映射到旧计划会静默改写发送方意图。

身份摘要 MUST NOT 归一化空白。空白不同的两条消息是不同的消息。

消息携带的 ThreadRef 是发送方声明的目标位置，属于不可变身份的一部分。消息只归属这一个 thread，双向往来都记在其中。

目标位置省略时，身份摘要保留“省略”这个值。接收方在接纳事务内解析默认 thread，并把解析结果写入提交记录。重投返回该已记录的 ThreadRef，不重新解析。默认 thread 变更不改变已提交消息的身份。发送方不得把接纳结果中的 ThreadRef 回填进原消息后重投。

绑定转发时，接纳记录 MUST 保存回复权威。回复权威是原发送节点，或由转发授权明确指定的节点。普通直连的回复权威就是认证来源节点。接收方把回复送到该节点，不根据外层连接的最后一跳推断。

## 4. 接纳的保证等级

```json
{
  "message_ref": {"sender": "urn:onlyne:agent:dpdp", "id": "msg-01"},
  "receipt_id": "rcpt-01",
  "guarantee": "durable",
  "thread_ref": {"owner": "urn:onlyne:agent:lazuli", "id": "thr-research"},
  "seq": "103",
  "plan": {"response": "await", "execution": "new"}
}
```

- `durable`：消息与待处理项在同一可靠提交中保存后，才返回该回执。
- `binding-defined`：由绑定声明的其他保证。原始 A2A endpoint 未声明持久接纳扩展时，只能给出绑定声明的范围。

HTTP 200 或对端返回 Task 不等于 durable 接纳。库必须报告实际保证等级。

`seq` 是接收方 thread 内的本地序号。它用于本地展示，不是跨节点身份。`message_ref` 才是身份。

## 5. 拒绝与拒答

- 拒绝在接收前发生。返回结构化错误，接收方保存这次拒绝。拒绝保留在发送方的出站记录中，不占对方 thread 的 seq。
- 拒答是一条显式关联原消息的业务消息，关系为 `reply`。它把发送方的等待转入已回答状态。

拒绝与拒答的后续动作不同。拒绝后发送方可以用新 id 重发。拒答表示对方受理后交还，重写消息本身不解决问题。

## 6. 回复

回复使用同一发送接口。

```json
{
  "retry_until": "2026-10-23T04:00:00Z",
  "message": {
    "id": "msg-02",
    "sender": "urn:onlyne:agent:lazuli",
    "delivery_kind": "答复这条请求，结论如下",
    "msg": "第二组数据需要重算。",
    "meta": {"in_reply_to": {"sender": "urn:onlyne:agent:dpdp", "id": "msg-01"}}
  }
}
```

关系必须指向原消息。等待答复的调用只被 `reply` 解除。`receipt` 和 `followup` 不解除等待。

回复默认走原消息所在 thread 的 `POST /threads/{owner}/{thread_id}/messages`，统一投递到该 thread 的 owner，`in_reply_to` 指向原消息。发送方持有该 thread 的 post 成员资格时走这条路径。双向往来均存盘在发起者的同一 thread 内。

回复的投递目标是该 thread 的 owner 节点，由 owner 统一追加并分配递增 seq。owner 追加 reply 时，在同一事务里为被回复消息的作者（回复权威节点）及 `mentions` 指向的接收方提交关注义务。回复权威节点持有该义务，因此等待方可以从自己的关注清单取回这条答复并解除等待。作者身份由 owner 在接纳时认证并写入清单条目；成员不得从消息正文或 `meta` 的自我声明推断作者。

来源没有 post 成员资格时（如原 thread 已关闭、成员被撤销），使用窄回复许可走 `POST /messages` 到回复权威节点，按保存的本地落点绑定记录这条回复。

合法的 `reply` 证明预期对端收到并答复了原消息。答复先于回执到达时，发送方记录这项证据并解除等待。durable 保证仍需对端的持久接纳回执或协商契约，答复本身不提升保证等级。随后的回执补齐证据，不产生第二次接纳。

## 7. 回复许可

原发送方可以授予对方一个窄许可：只允许回复指定消息，且可撤销。

```json
{
  "reply_permit": {
    "message_ref": {"sender": "urn:onlyne:agent:dpdp", "id": "msg-01"},
    "grantee": "urn:onlyne:agent:reviewer",
    "expires_at": "2026-10-23T05:00:00Z",
    "revoked_at": null
  }
}
```

- 许可只包含对指定消息的回复权。MUST NOT 读取其他消息或其他 thread。
- owner 可以撤销。撤销后新的回复请求被拒，已接纳的消息不受影响。
- 许可用于三种例外：来源被禁言或移除、原 thread 已关闭、对方不加入该 thread 只把结论送回。常规回复走成员资格，不需要许可。

许可 MUST 绑定原消息 MessageRef、被许可的认证回复方、接收节点和允许的回复操作。调用方提供的 `in_reply_to`、`sender`、地址或 ThreadRef 不创建许可。

许可 MUST NOT 授予历史读取、成员管理、无关消息或资源操作。来源成员资格被移除不自动取消已签发的回复许可；接收节点仍保留对应的本地落点绑定。

加入 thread 的成员默认持有该 thread 的 read 授权，可以直接读双向往来（RFC 0002 §2）。read_scope 收窄时，成员只看范围内条目。

许可的回复次数和有效期由 owner 策略设定。协议不提供无限次回复权。回复次数用完或过期后，新的回复请求被拒。

需要窄回复许可时，接收节点 MUST 保存该消息的本地落点绑定，并把绑定告知发送方。回复落到该绑定位置。来源侧的默认 thread 变更不改变这个绑定。

持有 post 成员资格的回复走默认路径：进入原消息所在的 thread。发送方默认持有 read 授权，因此可以直接看到对方的答复。

`followup` 是补充和进度。它进入哪个执行上下文由 SystemOne 结合原消息与工作历史判定，判定输入与落点规则见 [RFC 0016](0016-systemone-context.md) 与 [《OMP 宿主契约》](../contracts/omp.md) §9。目标不可用时保留原安排并报告执行失败，插件不改成普通消息或新建任务。它不解除等待，投递仍需 active post 成员资格或明确授予 followup 的窄许可。关系字段本身不产生访问权。

## 8. 消息 ID 的重投窗口

发送方在请求中声明绝对 UTC 期限 `retry_until`。接收方按自己的接收时钟校验，并把已接纳消息的去重记录至少保留到该期限。幂等键和 MessageRef 去重规则共同生效。

- 新请求的 retry_until 超过接收时刻加服务声明的最大期限时，接收方返回 `invalid_request` 并报出支持的最大值。
- 已有同内容接纳结果时返回原结果，包括终态拒绝。无可重放结果且请求期限已过时返回 `retry_window_expired`，不重新接纳。
- 发送方到期停止自动重投。缺少接纳证据的结果继续显示 `unknown`，可查询原结果。期限不能证明对方没有接纳。
- 去重行在窗口到达前不得驱逐。明细清除后，接收方 MUST 在该节点身份的有效期内保留 MessageRef 和身份摘要的墓碑。墓碑命中且原结果不可得时返回 `retry_window_expired`，不得用新幂等键或延长 retry_until 再执行。节点清除墓碑后继续服务时必须使用新 node_id，旧身份不得继续接纳。

中继绑定按发送方 id 和窗口去重。中继返回 202 不改变下游的接纳事实。

## 9. 验收

- 响应丢失后重投同一 id 不产生第二次接纳。
- 同 id 不同 `delivery_kind` 或正文返回 id_conflict。
- 成员退出后重投原消息返回原结果或原拒绝，不重新接纳。
- 合法 reply 先于 receipt 到达时保留答复并解除等待，durable 保证按实际回执记录。
- receipt 不进入模型消息列表，不占 thread 序号。
- 回复许可撤销后新的回复被拒，已接纳消息不受影响。
- 窗口过长由接收方拒绝并报出上限，过期的新请求不接纳，未知结果不会被改称未送达。
- 关闭 thread 后，已提交且未确认的关注义务仍可通过关注清单读取并确认。
- owner 添加一个离线的成员时，授权通知在成员确认持久记录之前一直保留。
- 关注清单确认不能用于其他接收方或其他条目的义务。
- 提示丢失但连接正常时，成员仍会在下一次对账读到未确认义务。
