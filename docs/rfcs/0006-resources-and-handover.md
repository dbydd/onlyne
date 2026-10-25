# RFC 0006：资源与动态交接

状态：Draft。依赖：RFC 0001～0005。

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
| 上下文 | 消息或事件里带的 `resource_ref` | 接手邀请、事件推送带过来的引用 |

`GET /resources` 只返回调用方有操作权的资源：

```json
{
  "items": [
    {
      "resource_ref": {"owner": "urn:onlyne:gateway:im", "id": "chat-01"},
      "alias": "sea-ice-chat",
      "kind": "im_conversation",
      "operations": ["read", "send", "handover"],
      "binding": {"assignee": "urn:onlyne:agent:lazuli", "revision": "rev-a"}
    }
  ],
  "next_cursor": null
}
```

目录返回当前 assignee 和 revision，agent 缓存仅作展示。实际操作时 endpoint 重读自己的绑定，不信任缓存。

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

查询操作只读不写，`operation` 仍取自清单（`read`、`describe`、`list_events`），结果按当前授权过滤。

调用方对接纳后尚未取得终态的操作，MUST 在完成通知丢失、连接恢复或本地等待到达上限时按 `operation_id` 查询最终记录。查询不可用或结果已过保留期时，明确报告 `unknown` 或 `expired`。远端执行可以继续运行；协议只要求本地等待最终落到已知结果或明确的未知结局。

### 事件入口

资源事件经 SSE 送达运行时，游标由 endpoint 分配：

```text
id: 901
event: resource_event
data: {"resource_ref":{"owner":"urn:onlyne:gateway:im","id":"chat-01"},"event_id":"31","assignee":"urn:onlyne:agent:lazuli","revision":"rev-a"}
```

- `event_id` 是 endpoint 在该资源内分配的十进制递增字符串，不复用。
- 游标绑定节点、主体和 `resource_ref`，跨流使用返回 `invalid_request`。
- 事件只承载引用和归属，正文经 `GET /resources/{owner}/{id}/events/{event_id}` 取。
- 推送丢失时按资源查询补齐。查询不到的事件明确报告缺失，不假装已收到。

IM 平台事件不进任何 thread。agent 侧由接入层决定是否建 todo，资源事件本身不是消息。
协议不规定资源必须表现为会调用模型的 agent。资源可以只暴露自己声明的操作。

权威服务 MUST 校验 `resource_ref.owner` 是自己服务的身份。owner 不匹配时返回 `invalid_request` 或 `forbidden`，不做本地查找、不回退、不代理转发。

IM 会话是独立资源节点。它由 IM endpoint 持有平台事件和路由绑定，agent 通过 `resource` 动作订阅、发送和交接。agent 不为 IM 会话建立 thread，也不把平台消息直接写进自己的消息列表。需要持续处理时，由接入层把它写进运行时 todo。

资源 id 在 owner 内唯一。资源退役后，该 id MUST NOT 重新分配给另一个资源。去重键 `(resource_ref, platform_event_id)` 依赖这条规则。

revision 是不透明、区分大小写的字符串，按字节比较。它在同一资源内不复用，不跨资源共享命名空间。

## 2. 资源记录

每个资源 endpoint 保存自己经手的操作、结果和关系变化。

| 资源 | endpoint 持有的记录 |
|---|---|
| IM 会话 | 平台事件引用、当前绑定、已接受的发送 |
| 计算作业 | 作业 id、提交者、状态、产物引用 |
| 数据流 | 订阅 id、事件游标、接收关系 |

agent 可以保存该资源的本地引用与已见版本。资源 endpoint 验证实际操作时读取自己的当前关系，不信任调用方缓存的旧权限。

版本是并发控制条件，不是访问凭据。资源操作同时验证调用者身份、操作权限和预期版本。

## 3. 计算作业

计算作业的模型、三项独立授权、提交、事件、产物、取消与恢复由 RFC 0013 规定。通用层保留一条跨资源类别的规则：计算服务接收提交不代表作业运行成功，提交回执、作业事件和最终产物分别记录。A 交出自己的结果等待时，库明确返回 `transferred` 和交接引用；`transferred` 是库层的等待转移结果，不是资源操作的四态之一，只增加 B 的观察订阅不改变 A 的等待。

## 4. 路由绑定

资源关系由该资源所有者持有。一个 IM 网关负责自己会话的绑定表，一个计算服务负责自己作业的控制关系。

```json
{
  "resource_ref": {"owner": "urn:onlyne:gateway:im", "id": "chat-01"},
  "assignee": "urn:onlyne:agent:lazuli",
  "revision": "rev-a",
  "updated_at": "2026-10-23T02:00:00Z"
}
```

绑定表是局部状态。协议不建立全局路由表，也不要求全网一致视图。

资源操作可以跨域。assignee 与资源 endpoint 可以在不同域。交接邀请、事件推送和结果通知都按消息路径送达，投递权按成员表和回复许可判断，与域无关。资源 endpoint 不因为 assignee 换了域就重新建立绑定，也不把跨域送达改成新的资源操作。

IM 推送是资源事件，不是 thread 消息。平台事件由 IM endpoint 首次接收并归属，随后作为资源事件推给当前 assignee。assignee 在另一个域时，推送经双方入口送达，事件引用和归属不变。IM endpoint 不把事件写进任何 agent 的 thread；agent 是否建立待办由运行时决定。

平台凭据留在 IM endpoint。跨域推送不传递平台令牌，也不让对端 agent 直接访问平台。

## 5. 交接

交接说明变更范围。`handover` 同时切换指定资源的后续入站处理者和发送权限。平台凭据留在资源 endpoint。只转观察订阅使用不同的资源操作。

交接准备：

1. 发起者表达交接意图。发起者是当前 assignee 或资源明确授权的代表。
2. 库调用资源声明的交接操作，携带操作 ID、当前预期 revision、目标、`decision_deadline` 和 `retry_until`。资源 endpoint 校验发起者的交接权限，以及目标是否获准接手。
3. 资源 endpoint 把接手邀请作为关联该操作 ID 的消息发给目标。目标通过同一消息入口回复接受或拒绝。回复的落点由本地绑定决定。
4. 资源 endpoint 在 `decision_deadline` 前重查发起者和目标权限，比较预期 revision，原子写入新 assignee、新 revision 和交接结果。这个提交是生效点。
5. 资源 endpoint 向双方返回或发布结果。通知丢失时，双方按操作 ID 查回。

- 目标接受后、提交前崩溃，恢复时仍是原 revision。接受不表示交接完成。
- 两个并发交接只有一个能基于同一预期 revision 成功，另一个得到 `revision_conflict` 和当前版本。
- 相同操作 ID 重投返回原结果。修改资源、目标或预期版本返回 `id_conflict`。
- `decision_deadline` 限制这次交接能否提交。到点未提交的准备 MUST 记为 `aborted`，之后不得凭已记录的接受补交。
- `retry_until` 只限制请求重投和接纳。它到期不改变已接纳交接的提交资格，提交资格只由 `decision_deadline` 决定。
- 失败、超时或冲突的准备记为 `aborted`。目标可按操作 ID 查回 committed 或 aborted。
- endpoint 重启后 MUST 接管所有未终结准备。准备记录保存操作标识、发起者、目标、预期 revision、接受事实、`decision_deadline` 与 `retry_until`。恢复本身承担推进责任：期限未到、接受事实仍在、权限与 revision 仍然成立时可以提交；到期仍未提交的准备 MUST 记为 `aborted`。恢复推进不得依赖新的请求重新建立。

## 6. 入站事件归属

每条事件首次持久接收时，资源 endpoint 按 `(resource_ref, platform_event_id)` 去重，记录自己的事件引用、首次归属和首次绑定 revision。

- 平台事件 ID 可能只在一个会话内唯一，不能跨会话去重。
- 首次归属在接收提交时固定，之后不改写。
- 投递义务的标识是 `(resource_ref, event_id)`。确认按这个标识记录。
- 交接与事件接收按确定顺序提交。提交前接收的事件在提交后仍属于它的首次归属。

**有积压时不得交接。** 交接提交要求该资源当前没有积压。积压指 endpoint 已经持久接收、尚无确认记录、也未进入保留期终态的投递义务。存在积压时，提交返回 `conflict`，调用者必须先清空积压再重试。

清空积压只有两种方式：接收者确认接收，或该义务按保留策略给出明确终态。投递确认记录接收证据。接收者确认与保留期过期各自将该投递义务从待清空积压中幂等移除，二者都不重建积压。过期后到达的首次归属者有效确认，endpoint 可追加记录接收证据，但不重复清空积压，也不改变已结束的本地投递责任。积压清空指该事件不再处于待清空集合（即已被确认或已过期），交接前置条件依此判定。保留期到达时，endpoint MUST 为该投递义务写入 `expired` 终态并清除积压；终态结束本地投递责任，内容仍按内容保留策略处理。协议不提供把积压转给另一个节点的操作。

积压为零时，普通交接提交只改变后续事件的归属与发送权限。已经接收的事件保留首次归属与原处理者的读取权。

新事件的接收者取提交后的绑定。首次推送与重放都按事件的首次归属投递。

接手材料至少包含交接截点之前、目标仍需读取的事件引用和交接截点本身。投递义务已在提交前清空，所以这里的引用不表示未确认的积压，只表示目标按授权可以读取的历史范围。目标按授权读取这些事件。提供历史材料不等于重新派发全部历史。

资源事件引用包含 `resource_ref` 和 owner 在该资源内分配的 `event_id`。event_id 是不复用的十进制递增字符串。交接结果中的 `cutoff_event_id` 是提交前已记录的最后一个事件 ID，无事件时为 null。它属于资源事件序列，与 thread 的 SSE 游标分别编号。

`GET /resources/{owner}/{resource_id}/events/{event_id}` 按当前授权返回平台事件引用、首次归属的 assignee 和数据引用。资源保留策略删除内容后返回明确的 `not_found`，不把该 ID 分给其他事件。交接操作和资源事件查询均相对 `/onlyne/v3`。

接手者不可达时，endpoint 在该资源的队列中保留事件并暴露未送达记录。该资源的积压不阻塞其他资源。重新改派须经过一次授权交接。

## 7. 发送围栏

资源发送操作同时校验当前 assignee、已认证调用者和 revision。

- 交接后原 assignee 无法凭旧版本或对方的版本继续发送。
- 当前 assignee 持有旧版本时得到 `revision_conflict` 和当前绑定。刷新绑定不改写已有出站请求的 revision。重新发送该内容需要新的操作 ID。
- revision 检查同时防止 A → B → A 后，第一轮遗留的发送请求在第二轮任期内误执行。

平台发送与交接并发时，endpoint 必须为发送接受和交接提交排出先后顺序。交接前已获接受的平台发送可能在交接后才被平台确认。协议不承诺交接生效后旧内容绝不出现。回执记录该发送使用的 revision 和平台结果。

原 assignee 可以完成旧工作的分析，把结果发给新 assignee，由新 assignee 决定如何回复平台。产品希望原 assignee 仍能回复某条历史消息时，由 endpoint 提供有明确范围的历史回复授权。

## 8. 验收

- 交接提交前崩溃，恢复后仍是原 revision。
- 交接请求的重投窗口到期不阻止已接纳准备在 decision_deadline 前提交；decision_deadline 到期后不再提交。
- 存在未确认投递时，交接提交被拒绝，返回 `conflict`。
- 积压清空后，交接提交成功；已接收事件仍留在原处理者。
- 并发交接只有一个成功，另一个得到当前 revision。
- 交接后新事件进入新 assignee，已接收事件留在原 assignee。
- 原 assignee 的平台发送被拒，新 assignee 可发送。
- A → B → A 后，第一轮的旧发送请求仍被拒。
- 作业转交后提交者、配额和排队位置不变，未指定的权限保持原样。
- 资源积压不阻塞其他资源，重新改派需要授权交接。
