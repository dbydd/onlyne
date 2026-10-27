# RFC 0006：资源与动态交接

状态：Draft。依赖：RFC 0001～0005、RFC 0017。

## 1. 资源是可操作 endpoint

资源是 endpoint 管理的一类对象：IM 会话、计算作业、订阅的数据流。资源有自己的稳定身份、支持的操作和授权要求。

```json
{
  "resource_ref": {"owner": "urn:onlyne:gateway:im", "id": "chat-01"},
  "kind": "im_conversation",
  "operations": ["subscribe", "send", "read", "handover", "describe"],
  "requires": ["im_platform_send"]
}
```

资源描述至少包含稳定引用、操作清单、事件入口和授权要求。具体请求和结果使用结构化数据。SystemOne 不凭一段自然语言生成可执行命令。

### 资源发现

资源有三种发现路径，与节点发现同构（RFC 0011）：

| 路径 | 来源 | 适用 |
|---|---|---|
| 目录 | `GET /resources` 返回本节点可操作的资源 | agent 不知道自己能碰什么 |
| 描述 | `GET /resources/{owner}/{resource_id}` | 已知别名，取操作清单和 schema |
| 上下文 | 消息或事件里带的 `resource_ref` | 握手探测、事件推送带过来的引用 |

`GET /resources` 只返回调用方有操作权的资源：

```json
{
  "items": [
    {
      "resource_ref": {"owner": "urn:onlyne:gateway:im", "id": "chat-01"},
      "alias": "sea-ice-chat",
      "kind": "im_conversation",
      "operations": ["read", "send", "handover"],
      "binding": {"handler": "urn:onlyne:agent:lazuli", "cursor": "104", "revision": "rev-a"}
    }
  ],
  "next_cursor": null
}
```

目录返回当前 handler、游标和 revision，agent 缓存仅作展示。实际操作时 endpoint 重读自己的绑定，不信任缓存。

目录 MUST NOT 返回调用方无操作权的资源。资源存在但无权时，`GET /resources/{owner}/{id}` 返回 `not_found`，不暴露其存在。

### 资源操作

操作是统一的 RPC 形状，一次一个操作：

```json
{
  "operation_id": "op-01",
  "retry_until": "2026-10-23T04:00:00Z",
  "operation": "send",
  "expected_revision": "rev-a",
  "data": {"text": "图已更新，单位是 W/m²。"}
}
```

`POST /resources/{owner}/{resource_id}/operations`

- `operation` 取自该资源描述里的 `operations` 清单。清单外的操作返回 `unsupported_delivery`。
- `data` 必须匹配该操作的参数 schema。不匹配返回 `invalid_request`，不转发到平台再报错。
- `expected_revision` 在需要版本保护的操作上必填。不匹配返回 `revision_conflict` 并带回当前 revision。
- 幂等键用 `Idempotency-Key` 头，`operation_id` 同时进身份摘要。同 ID 重投返回原结果；同 ID 改内容返回 `id_conflict`。
- 操作结果分四态：`committed`（endpoint 已提交）、`pending`（已受理未完成）、`aborted`（endpoint 明确拒绝或本地中止，且已确认未产生外部效果）、`unknown`（调用方未取得确定终态，外部效果可能存在）。无法确认是否已发生外部副作用的失败必须记录为 `unknown`，并在 `details` 中保留平台或工具返回的原始错误信息。异步操作先回 `pending`，完成事件随后到达。超时与连接中断属于 `unknown`。

查询操作只读不写，`operation` 仍取自清单（`read`、`describe`、`list_events`），结果按日志读者授权过滤。

调用方对接纳后尚未取得终态的操作，MUST 在完成通知丢失、连接恢复或本地等待到达上限时按 `operation_id` 查询最终记录。查询不可用或结果已过保留期时，明确报告 `unknown` 或 `expired`。远端执行可以继续运行；协议只要求本地等待最终落到已知结果或明确的未知结局。

## 2. 两类状态：日志与运行时

一个资源只有两类持久状态，它们的生命周期和访问规则不同。

| 状态 | 内容 | 变更 | 谁读 |
|---|---|---|---|
| 事件日志 | 追加写入的事件序列，`event_id` 十进制递增且不复用 | 只追加，内容永不改写 | 当前处理者与日志读者 |
| 运行时绑定 | 当前处理者、游标、revision | 由交接与发送改写 | 当前处理者与 endpoint |

日志是事实，绑定是指针。删掉绑定不改变日志，换掉处理者不改变日志内容，读日志不需要绑定有效。

- 事件内容写入后 MUST NOT 改写。保留策略到期只能写入终态或删除内容，MUST NOT 把该 `event_id` 分配给另一条事件。
- 绑定不记录历史归属。协议没有「首次处理者」这个概念：一条事件属于资源，不属于某个 agent。
- 参与者 MUST 为自己消费过的事件保存本地副本；读自己的历史 MUST NOT 依赖 endpoint 可达。

## 3. 路由绑定

资源 endpoint 持有该资源的绑定表。绑定是局部状态，协议不建立全局路由表，也不要求全网一致视图。

```json
{
  "resource_ref": {"owner": "urn:onlyne:gateway:im", "id": "chat-01"},
  "handler": "urn:onlyne:agent:a",
  "cursor": "104",
  "revision": "rev-a",
  "updated_at": "2026-10-23T02:00:00Z"
}
```

- `handler` 是唯一的当前处理者。同一时刻一个资源只有一个 handler，不存在并行处理。
- `cursor` 是 handler 已确认消费的最高 `event_id`。大于游标的事件待投递。
- `revision` 是不透明、区分大小写的字符串，按字节比较，同一资源内不复用，不跨资源共享命名空间。

绑定不携带读权限、事件订阅或结果收件人。操作授权与日志读者是另外两件事，见 §6 与 §7。

资源类别可以声明额外的操作授权槽（例如计算作业的提交者、取消者与产物读者），它们约束操作，不参与事件路由。事件路由始终只有一条绑定。

版本是并发控制条件，不是访问凭据。资源操作同时验证调用者身份、操作权限和预期版本。

## 4. 交接

交接是资源 endpoint 主动更换 handler 的三段式握手。它不是把授权从 A 转给 B，而是资源 C 换掉自己的服务对端。

```text
阶段 1   a —— c          当前绑定
阶段 2   a → c           发起交接，c 冻结入站投递
阶段 3   a —— c —— b     探测期：c 带元数据与游标连接 b
阶段 4        c —— b     握手成立，原子切换，a 断开
```

### 4.1 发起

```json
{
  "operation_id": "op-42",
  "retry_until": "2026-10-23T04:00:00Z",
  "operation": "handover",
  "expected_revision": "rev-a",
  "data": {
    "target": "urn:onlyne:agent:b",
    "handover_deadline": "2026-10-23T04:00:00Z",
    "metadata": {"reason": "转交技术专家处理"}
  }
}
```

- 发起者 MUST 是当前 handler。其他主体得到 `forbidden`。
- `expected_revision` MUST 匹配当前绑定。不匹配返回 `revision_conflict` 和当前 revision。
- `handover_deadline` 约束整个握手。到点未完成，交接 MUST 记为 `aborted`，绑定保持不变。
- `metadata` 对协议不透明：endpoint MUST 原样带给目标，MUST NOT 解析或改写它。

### 4.2 冻结

受理交接后，endpoint 立即停止向原 handler 投递新事件，并记下冻结时的游标。冻结期内到达的事件照常写入日志，但不投递。

- 冻结 MUST 在受理提交时生效，MUST NOT 等到握手成功才生效。
- 冻结只针对入站投递。冻结期内 endpoint 仍按原 handler 与当前 revision 接受发送操作，避免一次失败的握手变成发送中断。
- 交接失败时，冻结期内的事件从原游标起补投给原 handler。

### 4.3 探测

endpoint 向目标节点发起握手探测：

```json
{
  "operation_id": "op-42",
  "resource": {"owner": "urn:onlyne:gateway:im", "id": "chat-01"},
  "kind": "im_conversation",
  "operations": ["read", "send", "list_events", "handover"],
  "metadata": {"platform": "lark", "conversation": "oc_xxx", "title": "客服专线"},
  "initiator": "urn:onlyne:agent:a",
  "cursor": "104",
  "deadline": "2026-10-23T04:00:00Z"
}
```

`POST /onlyne/v3/handover/probes`

- 探测 MUST 是签名请求。目标按 RFC 0017 验证发起方身份；未信任的对端 MUST 拒绝。
- `metadata` 是目标判断自己能否接手的依据。目标 MUST 能读到它；协议不规定它的结构，也不解释它的字段。
- 目标 MUST 在 `deadline` 前给出答复：

```json
{"answer": "accept", "cursor": "104"}
```

```json
{"answer": "reject", "reason": "capability_mismatch"}
```

- 目标 MUST 按 `operation_id` 持久记录自己的答复，供发起方在崩溃后查回。
- 目标拒绝、不可达或超时，交接 MUST 记为 `aborted`，绑定保持不变。
- endpoint MUST 自己与目标完成密钥交换：探测由 endpoint 签发，不由发起交接的 agent 签发。缺少这段配对时请求发不出去，交接记为 `aborted`，`data.reason` 为 `untrusted_target`，`error` 保留缺失的配对，`details.required_action` 指出该由哪一方发起交换。把它报成 `unreachable` 会让运维去查一个不存在的网络故障。

### 4.4 切换

目标接受后，endpoint 在一个事务里写入新绑定与结果：

- `handler` 换成目标，`cursor` 换成探测确认的游标，`revision` 换成新值。
- 冻结期内写入日志的事件从新游标起投给新 handler。
- 原 handler 的发送因 revision 过期被拒，见 §7。

切换是唯一的生效点。切换前崩溃，恢复后仍是原绑定。

### 4.5 结果与恢复

交接操作只有三个终态：`committed`、`aborted`、`unknown`。协议没有 `pending` 交接：握手在操作内完成，调用方拿到的是结果，不是一张待办。

- 探测超出调用方等待窗口时返回 `unknown`，调用方按 `operation_id` 查回。
- endpoint 崩溃恢复后 MUST 接管未结案的交接：向目标按 `operation_id` 查答复，接受则切换；拒绝、无记录或期限已过则回到原绑定。
- 相同 `operation_id` 重投返回原结果；修改目标、元数据或预期版本返回 `id_conflict`。
- 两个并发交接基于同一 revision 时只有一个能切换，另一个得到 `revision_conflict` 和当前绑定。

### 4.6 跨域

交接可以跨域。探测是普通的签名资源请求，按 RFC 0017 逐对认证，与域无关。资源 endpoint 不因为 handler 换了域就重建绑定，也不把跨域探测改成新的资源操作。

平台凭据留在资源 endpoint。探测不携带任何平台令牌，也不让目标直接访问平台。

## 5. 事件接收与游标

平台事件首次持久接收时，endpoint 按 `(resource_ref, platform_event_id)` 去重，分配下一个 `event_id` 并追加进日志。

- `event_id` 在该资源内十进制递增且不复用。平台事件 ID 可能只在一个会话内唯一，不能跨会话去重。
- 事件只承载引用与数据，不承载归属。协议没有「首次处理者」字段。
- 投递义务是「游标之后的事件」这个位置，不是某个人身上的欠账。endpoint 向 handler 推送 `event_id > cursor` 的事件。
- handler 确认消费后游标前移。确认按 `(resource_ref, event_id)` 记录，重复确认幂等。

因为义务属于位置而不是人，交接不需要清空任何积压：新 handler 从新游标起接管，之前的事件由日志负责。交接前后重叠的事件按 `event_id` 幂等处理，重复投递不产生第二次效果。

推送丢失时按日志补齐。日志读不到的事件明确报告缺失，不假装已收到。

资源事件不是 agent 消息。把它投影成接入层的哪种形状，由接入层自己决定，协议不规定。

权威服务 MUST 校验 `resource_ref.owner` 是自己服务的身份。owner 不匹配时返回 `invalid_request` 或 `forbidden`，不做本地查找、不回退、不代理转发。

资源 id 在 owner 内唯一。资源退役后，该 id MUST NOT 重新分配给另一个资源。去重键 `(resource_ref, platform_event_id)` 依赖这条规则。

## 6. 日志读取

日志读取是静态查询，与运行时绑定无关。

```text
GET /resources/{owner}/{resource_id}/log?since=<event_id>&limit=<n>
GET /resources/{owner}/{resource_id}/log/{event_id}
```

- 授权只看两件事：调用方是当前 handler，或在资源的 `log_readers` 表里。两者都不是时返回 `not_found`，不暴露资源存在。
- `log_readers` 是独立于绑定的表。交接 MUST NOT 改变它：历史访问是一份单独的授权，不是「谁在处理」的副作用。
- 日志读取 MUST 在绑定无效、handler 离线或 endpoint 重启后仍然可用。它不读运行时状态，只读追加日志。
- 保留策略删除内容后返回明确的 `not_found`，不把该 `event_id` 分配给其他事件。

参与者自己保存的本地副本是读历史的默认路径；endpoint 的日志用于对账与补缺，不是唯一来源。

## 7. 发送围栏

资源发送操作同时校验当前 handler、已认证调用者和 revision。

- 交接后原 handler 无法凭旧版本或对方的版本继续发送。
- 当前 handler 持有旧版本时得到 `revision_conflict` 和当前绑定。刷新绑定不改写已有出站请求的 revision。重新发送该内容需要新的操作 ID。
- revision 检查同时防止 A → B → A 后，第一轮遗留的发送请求在第二轮任期内误执行。

平台发送与绑定切换并发时，endpoint 必须为发送接受和切换排出先后顺序。切换前已获接受的平台发送可能在切换后才被平台确认。协议不承诺切换生效后旧内容绝不出现。回执记录该发送使用的 revision 和平台结果。

## 8. 计算作业

计算作业的模型、三项独立授权、提交、事件、产物、取消与恢复由 RFC 0013 规定。通用层保留一条跨资源类别的规则：计算服务接收提交不代表作业运行成功，提交回执、作业事件和最终产物分别记录。A 交出自己的结果等待时，库明确返回 `transferred` 和交接引用；`transferred` 是库层的等待转移结果，不是资源操作的四态之一，只增加 B 的观察订阅不改变 A 的等待。

## 9. 验收

- 握手成功：绑定切到目标，revision 变更，原 handler 的发送被拒，新 handler 可发送。
- 握手失败、拒绝或超时：绑定与 revision 均不变，冻结期内的事件补投给原 handler，一条不丢。
- 探测携带的 `metadata` 原样到达目标；目标可以凭它拒绝。
- 目标拒绝后，资源不存在待决记录：按 `operation_id` 查回得到 `aborted`。
- 探测超出发起方等待窗口返回 `unknown`；恢复后按 `operation_id` 从目标查回答复并结案。
- 切换前崩溃：恢复后仍是原绑定、原 revision。
- 两个并发交接基于同一 revision：只有一个切换成功，另一个得到当前 revision。
- 同一 `operation_id` 重投返回原结果；改目标或改元数据返回 `id_conflict`。
- 冻结期写入的事件在成功后进入新 handler，在失败后回到原 handler。
- 交接不改变 `log_readers`：原 handler 仍在读者表时可读全量历史，被移出后返回 `not_found`。
- 绑定切走且 handler 离线时，日志读取仍然可用。
- 参与者用本地副本读历史，endpoint 不可达时不受影响。
- A → B → A 后，第一轮的旧发送请求仍被拒。
- 作业转交后提交者、配额和排队位置不变，未指定的权限保持原样。
