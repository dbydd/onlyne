# RFC 0004：消息视图与事件续传

状态：Draft。依赖：RFC 0001、RFC 0002、RFC 0003。

## 1. 视图是投影

消息视图从消息条目和关系推导。视图不保存第二份消息状态。received 与 replied 是视图的派生结果；已注入是运行时本地视图的派生结果，不出现在对外视图里。

视图有两种，作用不同：

| 视图 | 内容 | 谁持有 |
|---|---|---|
| thread 视图 | 一个 thread 内的全部消息条目，双向往来都在一起 | thread 的 owner |
| 出站证据视图 | 本节点的发送记录和回执 | 发送节点自己 |

一条消息只出现在一个 thread 视图里。发送方被收窄 read_scope 或不持有 read 授权时，它只能看出站证据视图：自己发了什么、对端的接纳结果和错误。它看不到自己范围外的 thread 内容，也不需要第二份消息条目。

```json
{
  "thread_ref": {"owner": "urn:onlyne:agent:lazuli", "id": "thr-research"},
  "entries": [
    {
      "seq": "103",
      "message_ref": {"sender": "urn:onlyne:agent:dpdp", "id": "msg-01"},
      "direction": "in",
      "peer": "urn:onlyne:agent:dpdp",
      "at": "2026-10-23T02:05:00Z",
      "delivery_kind": "等你看完给我结论",
      "body": "请核对这次实验的结论。",
      "relation": null,
      "mentions": ["@all"]
    },
    {
      "seq": "104",
      "message_ref": {"sender": "urn:onlyne:agent:lazuli", "id": "msg-02"},
      "direction": "out",
      "peer": "urn:onlyne:agent:dpdp",
      "at": "2026-10-23T02:07:00Z",
      "delivery_kind": "答复这条请求，结论如下",
      "body": "第二组数据需要重算。",
      "relation": {"type": "reply", "message_ref": {"sender": "urn:onlyne:agent:dpdp", "id": "msg-01"}}
    }
  ],
  "next_cursor": null
}
```

`seq` 是 thread 内的本地序号，十进制字符串，只在这个 thread 内唯一。跨节点引用使用 `message_ref`。

- 回执和拒绝不是消息条目。它们进入出站证据视图，不占 thread 序号。
- received 表示对方 thread 里存在对应条目。看不到该条目时，received 只能由对方的接纳回执或合法 reply 证明。
- replied 表示对方 thread 里存在指向该消息且关系为 reply 的条目，或对方答复落到本节点。
- received 不表示模型已经看到。replied 不表示工作已经完成。

### 关注视图

视图提供两种读取模式，同一份存盘数据的两种投影：

| 模式 | 返回内容 |
|---|---|
| `inbox`（默认） | 当前调用方被主动关注的消息：@ 自己、@ all、@ 自己消息的回复 |
| `thread` | 该 thread 的完整双向历史，按 seq 顺序 |

`inbox` 视图按 thread 聚合，条目的 `mentions` 原样返回。agent 通过 `inbox` 及时看到需要关注的内容，通过 `thread` 浏览完整上下文。

查询参数增加：

| 参数 | 含义 |
|---|---|
| `attention` | `inbox` 或 `thread`，默认 `inbox` |

`inbox` 只返回当前认证主体的关注条目。它由 owner 为该主体提交的关注义务推导。未 @ 且非回复目标的普通通信不进入任何成员的 inbox，仍存盘在 thread 视图里。

### 关注清单与确认

关注清单是关注义务的权威读取面：

```text
GET  /threads/{owner}/{thread_id}/attention?cursor=...&page_size=...
POST /threads/{owner}/{thread_id}/attention/{obligation_id}/ack
```

- 清单按 `seq` 的稳定顺序分页，条目携带 `obligation_id`、`seq`、`message_ref`、认证作者、`relation` 与 `in_reply_to`。
- 游标是 `seq` 上的位置，不按未确认集合的偏移计算。确认只改变该义务的状态，不改变任何已发出游标的含义。
- 确认按 `obligation_id` 幂等。确认其他接收方或其他条目的义务返回 `not_found`。
- 内容已过期时，清单条目保留义务标识并明确返回过期结局；该结局不阻塞后续条目。
- 过期的义务可以被确认：成员在持久记录这个过期结局之后确认它。这次确认表示成员记录了结局，不表示成员仍持有正文。
- 确认对未知义务或他人的义务返回 `not_found`，不因为正文已过期而返回 `not_found`。
- 通知和 SSE 只用于提示。成员在对账时以清单为准。

## 2. 读取

`GET /threads/{owner}/{thread_id}/messages`

查询参数：

| 参数 | 含义 |
|---|---|
| `peer` | 按来源过滤 |
| `since_seq` | 从序号之后开始 |
| `related_to` | 按关系消息过滤 |
| `page_size` | 1～100，默认 50 |
| `cursor` | 上一页返回的 opaque cursor |
| `attention` | `inbox` 或 `thread`，默认 `inbox`，含义见 §1 |

`related_to` 使用 JSON 编码的完整 MessageRef，返回与该消息关联的条目。视图服务保留历史直到自己的保留期限。到期后旧条目可被清除，返回时明确说明截断位置。

MUST NOT 因为省略参数返回全部历史。长列表必须分页。

## 3. 多行消息与索引

索引在信封上，不写入正文。渲染给模型时，每条消息一个头部行，正文逐行缩进。

```text
[103] thr-research dpdp 02:05
  > 请核对这次实验的结论。
[104] thr-research lazuli 02:07 ↪103
  > 第二组数据需要重算。
```

- 多行正文每一行缩进，避免正文里看似索引的行被当成新消息。
- 同一视图出现多个 thread 时，头部带 threadId。`(threadId, seq)` 只在同一 owner 内唯一。
- 视图跨多个 owner 时，threadId 使用 owner 限定的短别名。别名到完整 ThreadRef 的映射由接入层保存，同一上下文中不重新绑定。
- 长正文只渲染头部和首行，标注剩余行数。模型按索引取全文。
- 模型引用消息时使用展示的 thread 别名和 seq。只有当前视图唯一确定 thread 时才可省略别名。库解析到 `message_ref`，不会从正文识别消息 id。

正文中的换行和空白原样保留。渲染层不得 trim 或合并正文行。

## 4. 事件流

节点提供 SSE 事件流，用于把新消息、成员变化和资源操作结果送达运行时。

`GET /threads/{owner}/{thread_id}/events`

```text
id: 900
event: message
data: {"thread_ref":{"owner":"urn:onlyne:agent:lazuli","id":"thr-research"},"seq":"103","message_ref":{"sender":"urn:onlyne:agent:dpdp","id":"msg-01"}}
```

- `id` 是事件游标，与消息序号是两套编号。事件可以对应成员变化或资源结果，不都对应消息。
- 游标在同一 thread 的事件流内严格递增，不复用。重启保留原游标或使旧流明确失效。
- 客户端用 `Last-Event-ID` 续传。游标绑定节点、主体、ThreadRef 和查询范围，跨流使用返回 `invalid_request`。
- 续传时服务重新检查调用方权限。失去权限返回 403，不返回该 thread 的内容。
- 游标过期返回 `cursor_expired`。客户端取得新快照再续传。
- `GET /threads/{owner}/{thread_id}/snapshot` 返回当前可读的 thread 元数据、成员授权、首屏消息、该 thread 引用的资源操作列表和 `resume_cursor`。资源操作列表包含操作 ID、ResourceRef、当前已知状态和最后事件引用。这些数据与游标具有同一提交边界。
- 客户端从 resume_cursor 订阅快照之后的事件，历史消息用快照绑定的分页 cursor 补齐。资源结果按快照或事件里的操作引用查询资源 owner，消息列表不能代替资源结果查询。
- 资源 owner 提供 `GET /resources/{owner}/{resource_id}/operations?cursor=...`，返回该资源在保留窗口内的操作摘要。快照中的引用过期或事件丢失时，客户端用这个查询重建操作集合。
- 事件流提供保留窗口内的重放。客户端按事件游标去重，资源操作按操作 ID 查询最终记录。
- 事件不携带对端存活或工作状态。节点重启后，游标可继续或明确失效。

## 5. 注入游标

运行时维护自己的消费记录（注入游标）：哪些条目已经进入模型上下文。该记录是本地状态，不对外发布，也不写入消息。记录按 `(context, thread, seq)` 记录，只证明指定上下文消费过该条目。

- received 不等于已注入。未注入的条目可以被重新注入。
- 同一上下文重复注入同一条目时，接入层 MUST 折叠为一次展示。经 `await` 工具结果消费的 reply 条目与经自动关注片段进入该上下文的条目共用 `(context, ThreadRef, seq)` 消费记录，已确认消费的条目不得向同一上下文重复展开正文。库提供这条共用记录的读写接口；折叠动作由接入层执行，因为只有接入层能观察模型上下文。
- 恢复同一逻辑上下文时沿用它的游标，前提是该上下文对应的历史仍然可用。历史不可恢复时，游标不能单独证明条目已经可用。
- 创建独立上下文时，它建立自己的消费记录，只消费明确分配给它的关注义务。

## 6. 验收

- received 与 replied 由条目和关系推导，任何一端查不到 Onlyne 维护的工作状态。
- 同一消息的回复链在视图里可见，多个 thread 同屏时索引不冲突。
- 多行正文缩进后不会伪装成新消息。
- 事件续传丢失权限返回 403，游标过期返回 cursor_expired。
- 回执不进入消息视图，不占 thread 序号。
- 关注清单的确认按义务标识幂等，且不改变其他接收方的清单。
- 过期条目给出明确结局，后续条目仍可读取。
