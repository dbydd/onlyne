# RFC 0005：投递决策、库等待与运行时

状态：Draft。依赖：RFC 0001～0004。

## 1. SystemOne 做什么

模型只调用 `msg({ delivery_kind, msg, meta })`。`delivery_kind` 是模型写下的自然语言意图。SystemOne 把它翻译成可执行的投递参数。

SystemOne 只从当前 endpoint 声明的能力、目标位置和运行时可执行操作中选择。它不决定身份认证、权限、去重、回执事实或交接生效。这些由确定逻辑处理。

```json
{
  "response": "none",
  "execution": "new"
}
```

- `response` 取 `none`、`async`、`await`。它表达发送方是否期待答复，以及如何等待。
- `execution` 取 `existing` 或 `new`。它表达接收方如何安排处理上下文。

两个维度独立。`response` 由发送方解释自身意图后确定。`execution` 由接收方确认能否履行。两端通过消息字段交换，实际计划记录在回执中。

`response` 和 `execution` 只描述一次投递。模型入口还承载本地读取和资源操作，因此库的执行计划是一个带类型的动作：

```json
{
  "action": {"kind": "send", "to": "urn:onlyne:agent:reviewer", "thread": null, "context": null, "in_reply_to": null, "relation": null},
  "response": "async",
  "execution": "existing"
}
```

动作按类型分别执行：

| kind | 必填字段 | 可选字段 | 含义 |
|---|---|---|---|
| `send` | `to` | `thread`、`context`、`in_reply_to`、`relation`、`mentions` | 向一个 endpoint 投递消息 |
| `read` | `scope` | `to`、`thread`、`message`、`cursor`、`page_size` | 本地目录、thread 列表、消息列表、单条消息或出站证据 |
| `resource` | `resource`、`operation` | `data` | 调用资源声明的操作 |
| `thread` | `op` | `title`、`thread`、`to` | 创建、关闭、列举、描述本地 thread，或变更其成员 |
| `domain` | `op` | `domain_id`、`expected_revision` | 创建、列举、查询、加入、退出本节点的域关系，或核对域成员 |

`read.scope` 取 `directory`、`threads`、`inbox`、`messages`、`message`、`evidence`。`directory` 不需要其他字段；`threads` 需要 `to`；`inbox` 返回当前主体的关注条目，可按 `thread` 过滤；`messages` 需要 `thread`；`message` 需要 `thread` 和 `message`；`evidence` 需要 `message`。`cursor` 只用于分页读取。`threads` 只列本节点的 thread，不列出对端 thread。

`mentions` 只对 `send` 有意义，取 `@all` 或特定 endpoint。它决定谁收到主动关注，不改变消息归属的 thread。`read` 的 `attention` 字段在 `inbox` 和 `messages` 两个 scope 上取 `inbox` 或 `thread`，默认 `inbox`。

`thread.op` 取 `create`、`close`、`list`、`describe`、`member_add`、`member_remove`。`create` 需要 `title`，返回新 thread 的本地别名。`close`、`describe`、`member_add`、`member_remove` 需要 `thread`。`member_add` 和 `member_remove` 还需要 `to`。这些操作只作用于本节点自己的 thread，规则见 RFC 0002；调用方不是 owner 或未被授权代表时返回 `forbidden`。

`domain.op` 取 `create`、`list`、`describe`、`join`、`leave`、`discover`。`create` 与 `list` 不需要 `domain_id`；其余操作需要。加入和退出只能作用于本节点自己的关系，`expected_revision` 用于条件修改。

`response` 和 `execution` 只对 `send` 有意义。`read`、`thread` 与 `domain` 不使用这两个字段。`resource` 的 `response` 由资源操作声明决定，`execution` 不使用，操作结果另行记录。一次调用只产生一个动作。意图同时匹配多个动作或多个目标时返回 `ambiguous_target`，不挑一个执行。

- 没有明确目标上下文时，`existing` MUST 消歧或拒绝。
- `new` 不默认复制发送方历史。是否带历史、使用哪个模型和工具由接收方策略决定。
- 接收方不支持提案时，返回 `unsupported_delivery` 或明确给出改选方案。改选方案 MUST 在提交前呈现给发送方。

`delivery_kind` 属于消息身份的一部分（RFC 0003 §3）。已接纳消息的重投复用原决定，不重新判定。

## 2. 判定失败

SystemOne 不可用或输出无法执行时，请求返回结构化错误：

```json
{"error": {"code": "decision_unavailable", "message": "No usable delivery plan.", "retryable": true}}
```

- 未形成可执行计划时，库 MUST NOT 自行挑选默认投递方式。
- 判定输入、输出和失败原因 MUST 保留在本地记录中，供查询。
- 模型可以看到实际使用的计划。模型看到的与发生的一致。

## 3. 库的等待

库支持真正的等待：发送方调用注册一个等待者，答复由消息接收路径直接匹配后解除。

- 等待者匹配预期的认证对端、原 MessageRef 和合法关系类型 `reply`。
- 建立或恢复等待者时，MUST 先检查本地已经持久保存的合法 reply 证据。已有证据时立即以它解除等待，不重新等一次。
- `receipt` 和 `followup` 不解除等待。
- 等待有期限。期限由库按对端策略填充，模型不设置。
- 等待超时只结束这一次等待。它不把任何一端标成失败，不产生工作状态。
- 取消等待、发送取消请求、对端确认停止是三个不同事实，分别记录。

网络接收、资源事件和回执处理在等待期间继续运行。等待不阻塞这些路径。

合法 reply 经 `await` 工具结果解除等待并被上下文确认消费后，与该上下文的消费记录（注入游标）共用同一条记录，该条目不再作为未消费关注重复推入自动片段；若该 reply 已经由自动关注片段先行注入并确认消费，随后解除的等待工具结果只返回到达事实与原消息引用，不重复展开正文。工具结果被提交本身不推进记录；只有宿主确认模型消费了该结果才推进。

同一执行上下文直接等待一条必须由自己消费的消息时，库 MUST 拒绝。循环依赖不因为换成 Promise 而消失。

## 4. 运行时 todo

Onlyne 不保存任务状态。一条消息要求持续工作时，由运行时把它写进自己的 todo 原语。

```json
{
  "todo": {
    "message_ref": {"sender": "urn:onlyne:agent:dpdp", "id": "msg-01"},
    "thread_ref": {"owner": "urn:onlyne:agent:lazuli", "id": "thr-research"},
    "summary": "核对实验结论并回复 dpdp"
  }
}
```

- todo 数据归运行时所有。Onlyne 核心不读写、不投影、不同步它，接入层通过运行时 API 建立本地关联。
- 运行时按自己的节奏提醒模型。Onlyne 不代替模型判断一件事是否完成。
- OMP 首版由接入层把来信连同来源引用、消息 ID 和本地上下文映射写进运行时 todo，再唤醒或安排上下文。todo 快照与消息引用到待办项的稳定关联 MUST 由同一可恢复的持久提交记录确定；宿主投影与本地索引在该记录成功后派生。恢复 MUST 先重放该提交记录并重建关联，再决定是否为来信新建待办，不得仅因派生索引缺失而再次追加。
- 资源交接时，Onlyne 交接交接截点之前的事件引用，不交接 A 的 todo。B 是否建立待办由 B 决定。

库可以把自己的事实呈现给运行时：未答复的消息、等待超时、交接待决。这些呈现是本地事实查询，不是工作状态。

## 5. 模型可见列表

消息列表对模型可见（RFC 0004）。OMP 接入需要分别验证：

| 能力 | 需要证明的事情 |
|---|---|
| 工具调用 | 调用库并呈现实际计划、已知接纳证据与答复 |
| 等待及中断 | 等待期间答复能解除等待，用户中止能释放本地等待 |
| 入站注入 | 忙时按宿主规则安排，闲时能唤醒，plan 模式有明确队列状态 |
| 独立执行 | 新上下文具有自己的身份、权限与消息关联 |
| 恢复 | 从本地记录恢复未决消息与上下文映射 |
| todo 写入 | 来信可写进运行时待办；快照与消息关联同一次可恢复提交，恢复不重复建项；完成与提醒由运行时负责 |

某个宿主接口无法表达等待语义时，接入可以选择 SDK 接线或补宿主能力。OMP 首版在发送前拒绝无法兑现的 `await`，不把库的等待改成让模型结束回合等通知。

异步 handle 是另一种能力，不是 `await` 的降级实现。接入提供 handle 模式时 MUST 单独协商，并给出查询结果与恢复上下文的操作。两种模式不得在同一能力名下混用。

## 6. 验收

- 同一意图文本在不同时刻可以给出不同计划，计划和失败原因可查。
- `await` 只被合法 `reply` 解除，超时不产生工作状态。
- 答复先到、等待后注册时，注册读到已有证据并立即解除等待。
- 等待期间网络接收和回执处理继续运行。
- 消息进入运行时 todo，完成与提醒由运行时负责，Onlyne 无 complete 操作。
- 模型侧只看到一个消息入口。thread 创建、关闭、成员变更和资源操作都经这个入口表达，不作为第二套模型工具。
- todo 快照与消息引用的关联在崩溃后仍可恢复，重放同一来信不产生第二条待办。
