# RFC 0002：Thread 与成员

状态：Draft。依赖：RFC 0001。

## 1. Thread

Thread 是一个 agent 拥有的消息列表。每条消息归属一个明确的 thread，往来双方的消息都记在其中。Thread 记录消息的顺序、成员访问和生命周期，不携带工作状态、进度或终态。

Thread 属于单一 owner。同一 ThreadRef 只有一个持有者。完整线程由最初发起该会话的节点持有：
- 对于 IM 会话，IM gateway 是外部会话在 Onlyne 中的发起者和 thread owner；
- 对于 agent 协作或对话，最初发起消息的 agent 是该 thread 的 owner；
- 参与者（包括被联系的 peer 及后续加入的成员）均为该 thread 的成员，持有该 thread 的访问和投递许可，不持有独立的消息集合。

Thread 是消息的唯一存盘容器。投递、@ 关注和引用回复都不改变存盘位置：一条消息进一个 thread，分配一个 seq。@ 只决定谁收到主动关注，不产生第二份记录。

Thread 可以跨域。域成员关系不决定谁持有或进入这份列表。成员可以是其他域的节点，投递、读取和回复都按成员表判断，与成员所属域无关。协议不存在「thread 只在域内有效」的规则。跨域资源的读写和交接仍走资源 owner 的 endpoint 或既有 ingress，连通性保持原状。

一条消息只归属一个 thread。双向往来都记在这个 thread 里，按 owner 分配的 seq 排。其他节点不持有这份列表，也没有第二份 thread 条目。

Thread 可以显式创建，或按 owner 策略自动创建。默认 thread 是一个不依赖调用方提供 id 的位置。

### Thread 状态

```json
{
  "id": "thr-research",
  "owner": "urn:onlyne:agent:lazuli",
  "title": "sea ice review",
  "state": "open",
  "closed_at": null,
  "next_seq": "103",
  "retention": {"messages_until": "2027-01-01T00:00:00Z"},
  "created_at": "2026-10-23T02:00:00Z"
}
```

`state` 取 `open` 或 `closed`。closed thread 不接收新消息，历史条目保留可读。关闭 thread 不取消工作，不产生任何工作事件。关闭后，已提交且未确认的关注义务继续出现在成员的关注清单里（接口见 RFC 0004 §1），直到成员确认或 owner 按保留策略给出明确处置。

Thread 元数据可由 owner 更新。`next_seq` 是只读投影，不是可以客户端写入的序号。

`seq` 在 thread 内严格递增，重启后保持已分配的值。清除历史后，已分配的 `seq` 不重新分配给新消息。`seq` 是十进制字符串（RFC 0001 §3）。

## 2. 成员

Thread 的成员表决定谁可以投递、谁可以读取。进入 thread 的成员默认获得 `post` 和 `read` 两项权限。禁言和限制读取范围是主持人后续配置的动作。

```json
{
  "thread_ref": {"owner": "urn:onlyne:agent:lazuli", "id": "thr-research"},
  "peer": "urn:onlyne:agent:dpdp",
  "access": ["post", "read"],
  "state": "active",
  "since": "2026-10-23T02:01:00Z",
  "read_scope": {}
}
```

`post` 允许投递消息并接收回复。`read` 允许在读取范围内查看列表。两项默认同时授予。

读取范围由读取授权 `read_scope` 表达，它是机器可读的选择条件：

```json
{
  "read_scope": {
    "peers": ["urn:onlyne:agent:lazuli"],
    "since_seq": "100",
    "related_to": {"sender": "urn:onlyne:agent:lazuli", "id": "msg-01"}
  }
}
```

`access` 是数组。`post` 授予投递，`read` 授予按 `read_scope` 查看。

默认 `read_scope` 为空对象，表示读取全部历史。owner 读取自己的 thread 使用全部范围。

- 加入 thread 时 MUST 同时授予 `post` 和 `read`。只授予一项的成员表由 owner 显式配置，加入请求不产生单项权限。
- `post` 不授予其他 thread 的任何权限。投递权只在授予它的那个 thread 内有效。
- 主持人可以收窄 `read_scope` 或撤销 `read`，把成员限制为只能投递。收窄是明确的 owner 动作，不是加入时的默认值。
- 读取授权应用于列表、单条读取、关系元数据、分页和事件流。
- `read_scope` 的每个键都可选。省略的键表示该维度不限制。

成员看不到自己 `read_scope` 之外的来源消息。收窄范围时，历史条目保留，该成员的分页和事件按当前范围判断。

移除成员 MUST 同时结束该成员的投递许可和读取授权。已经取得的副本由接收方自己保管，协议不负责收回。后续分页和事件 MUST 按当前授权判断，不把旧 cursor 或已建立的流当作持续授权。

关闭 thread 由 owner 决定，对全部成员同时生效。工作是否结束由主持人判断，协议不提供成员自行结算的路径。

投递到 thread 时，owner MUST 校验调用者是 `post` 成员。

### 成员状态

| 状态 | 含义 |
|---|---|
| `requested` | 来源已请求加入，等待 owner 决定 |
| `active` | 当前有效 |
| `revoked` | 已停止新投递许可 |
| `denied` | owner 拒绝加入 |

状态变化不改变历史条目，不产生消息。owner 按自己的策略决定 requested 是否自动转为 active。

### 服务必须验证 owner

路径与 ThreadRef 中的 owner 是同一身份。权威服务 MUST 校验 `thread_ref.owner` 是自己服务的身份，或是一个已把服务权委托给它、委托仍有效的节点。两者都不满足时返回 `invalid_request` 或 `forbidden`，不做本地查找、不回退默认 thread、不代理转发该请求。委托规则见 [节点发现与域](0011-discovery-and-domains.md)。

thread id 在 owner 内唯一。关闭或清除历史后，该 id MUST NOT 重新分配给另一个 thread。owner 身份改变时，旧 ThreadRef 不继承新身份的权限。

### 加入请求

`POST /threads/{owner}/{thread_id}/memberships`

```json
{
  "retry_until": "2026-10-23T03:00:00Z",
  "peer": "urn:onlyne:agent:dpdp",
  "access": ["post"]
}
```

owner 返回成员状态。owner 也可以主动添加成员，来源无需请求。授权时 MUST 校验加入该 thread 的许可，不因对方是可信主体就默认开放。

owner 主动添加成员时，MUST 在同一持久事务里写入成员记录和该成员的持久授权通知。通知带稳定标识、`thread_ref`、接收方身份和成员版本。事务提交之前，成员记录不算生效。owner 重试投递，直到成员确认已经持久记录这个 owner 与 thread。成员在确认之前不承担该 thread 的关注义务，owner 在此期间保留通知。

来源自己请求加入时，`peer` MUST 等于认证主体 `actor`。peer 与 actor 不一致时返回 `forbidden`，避免一个来源冒名替另一个来源制造成员资格。

owner 或明确授权的代表可以添加、激活、拒绝、修改 access 或移除其他 peer。`memberships/self` MUST 从认证主体解析，不接受调用方指定 peer。

投递许可要求成员状态为 `active` 且 access 授予 post。持有 `requested`、`revoked`、`denied` 记录都不足以投递。

### 退出与移除

退出由来源发起：`DELETE /threads/{owner}/{thread_id}/memberships/self`。移除由 owner 发起：`DELETE /threads/{owner}/{thread_id}/memberships/{peer}`。

退出和移除都只停止新投递许可。它们 MUST NOT 撤回已接纳的消息，MUST NOT 取消在途等待，MUST NOT 删除历史条目。

成员已退出或移除时，消息不能投递到该 thread。接收方返回 `not_a_member`，并保留该拒绝记录。

回复的默认目标位置是原消息所在的 thread。持有 post 成员资格的回复直接投递至该 thread 的 owner（`POST /threads/{owner}/{thread_id}/messages`），由 owner 在该 thread 追加条目并分配递增 seq，双向往来及多方发言均记录在同一个 thread 里。这是常规路径，不需要额外许可。

回复的投递目标：线程内消息的回复统一投递至该 thread 的 owner。owner 在持久接纳该回复后，在同一事务内为被引用的原消息作者（回复权威节点）及被 @ 的成员写入关注义务。等待方（await）从自身关注清单获取该答复并解除等待。

三种例外使用窄回复许可：
- 来源被禁言或移除，不能向原 thread 投递。
- 原 thread 已关闭。
- 对方只想把结论送回发送方节点，不加入该 thread。

在这三种例外下，回复投递至回复权威节点，按保存的本地落点绑定记录。常规回复均走统一 thread owner。

## 3. 默认 thread

每个 agent 有一个默认 thread。owner 返回其 id 和自身身份。

```json
{"id": "thr-default", "owner": "urn:onlyne:agent:dpdp"}
```

- 默认位置解析 MUST 是 owner 本地动作，并在接纳事务内完成一次。提交结果 MUST 记录解析出的 ThreadRef。重投返回该已记录结果，不重新解析。
- 首次接触投递需要 owner 的显式入站策略授予投递许可。默认位置被发现不授予许可。
- 关闭当前默认 thread 时，省略目标的新投递 MUST 被拒绝，或由 owner 原子指定另一个 open 默认 thread。协议 MUST 明确所选行为，不允许静默切换。
- ThreadRef 指向不存在、或调用方没有读取权的 thread 时返回 `not_found`，已关闭时返回 `thread_closed`。读取权指成员表授予的 read 权限，或一条针对调用方的有效回复许可。调用方有读取权但缺少投递成员资格时返回 `not_a_member`，其他权限拒绝返回 `forbidden`。MUST NOT 回退到默认 thread。
- owner 可以拒绝默认 thread 投递，此时返回明确错误。

请求没有明确 ThreadRef 时，消息落在 owner 的默认 thread，投递许可仍按成员表判断。

默认 thread 只解决位置选择，不授予任何额外权限。

## 4. 创建与关闭

创建：`POST /threads`

```json
{"retry_until": "2026-10-23T03:00:00Z", "title": "sea ice review"}
```

关闭：`POST /threads/{owner}/{thread_id}/close`

```json
{"retry_until": "2026-10-23T03:00:00Z"}
```

关闭幂等。已关闭的 thread 返回 `thread_closed`。关闭不删除消息，不通知模型完成工作。

只有 owner 或明确授权的代表可以创建或关闭自己服务的 thread。调用方不能替其他身份执行这两个操作。

投递授权、thread 打开检查、消息追加和持久接纳结果 MUST 与关闭、移除操作共用同一原子顺序。顺序在变更前完成的接纳保持已接纳。顺序在变更后到达的投递失败。重投一个已提交操作返回原结果，不再次追加。

迟到的合法回复遇到已关闭的本地 thread 时，MUST 有明确授权的替代落点或明确返回 `thread_closed`。协议不允许静默改投其他 thread。

## 5. Thread 操作与会话的关系

Thread 属于消息层。OMP 会话、ACP 会话和 IM 会话是运行时上下文，不由 ThreadRef 推断，也不随线程变化。一个运行时会话可以读取多个 thread，一个 thread 的消息可以进入多个运行时会话，迁移会话时 thread 不转移。宿主侧映射规则见 [《OMP 宿主契约》](../contracts/omp.md) §9。

加入 thread 的成员默认持有 read 授权，可以直接读双向往来。被收窄范围的成员只看自己范围内的条目，其余按出站记录（RFC 0003 §3）处理。

ThreadRef 不授权读取其他 thread，也不授予任何资源凭据。

## 6. 验收

- 同一来源在两个 thread 投递，只进入指定 thread，不产生广播。
- 非成员投递被拒并保留拒绝记录。
- 加入 thread 的成员同时获得 post 和 read，可以直接读双向往来。
- 被收窄 read_scope 的成员只看到范围内条目，投递仍按 post 权限判断。
- 成员退出后重投原消息不重新接纳；合法 reply 仍可送达回复权威。
- 关闭 thread 阻止新消息，历史仍可读，不产生工作状态。
- 未知或无权 thread 显式报错，不回退默认 thread。
- ThreadRef 的 owner 不是本服务身份时，拒绝而不是本地查找。
- 来源请求加入时 peer 与 actor 不一致被拒。
- 两个来源持有互不相交的 read_scope，彼此看不到对方消息。
- 移除成员或撤销 read 后，其后续分页和事件流都拿不到新内容。
- 默认 thread 变更后，已提交消息的重投仍返回原落点。
- 关闭、移除与投递并发时，只存在一种提交结果。
- 迟到的合法回复遇到已关闭 thread 时，返回 thread_closed 或使用已授权替代落点。
- 关闭 thread 后，已提交未确认的关注义务仍可读取并确认。
- owner 添加离线成员后，成员上线能凭授权通知找到 owner，不依赖此前是否知道该 thread。
