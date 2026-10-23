# RFC 0009：A2A 规范参考

状态：Draft。本文记录 Onlyne 实现绑定时的上游依据。本文不是 A2A 规范的替代文本。

## 1. 版本与来源

| 项目 | 值 |
|---|---|
| 规范站点 | `https://a2a-protocol.org` |
| 站点标注版本 | 1.0.0 |
| 代码仓库 | `a2aproject/A2A` |
| 引用提交 | `3303592588e388e62e0f69f701af531d2f4e3991`（tag `v1.0.1`） |
| 主要文件 | `specification/a2a.proto`、`docs/specification.md` |

固定来源：[数据模型与 RPC](https://github.com/a2aproject/A2A/blob/3303592588e388e62e0f69f701af531d2f4e3991/specification/a2a.proto)、[操作与绑定规范](https://github.com/a2aproject/A2A/blob/3303592588e388e62e0f69f701af531d2f4e3991/docs/specification.md)、[规范站点](https://a2a-protocol.org/v1.0.0/specification/)。本文的文件行号指固定提交。

两处版本号不一致：规范页写 1.0.0，仓库最新发布是 v1.0.1。协议版本只比较 `Major.Minor`，补丁号不参与协商（`docs/specification.md:706-745`）。写实现时以引用提交的 proto 为准，文中注明站点版本。

上游存在两处内部矛盾，绑定实现必须自己选一边并记录：

- Send Message 的概述说操作立即返回，详细执行模式又说默认阻塞到终态或中断态（`specification/a2a.proto:155-160`，`docs/specification.md:565-567`）。
- Subscribe to Task 的 HTTP 描述写 POST，proto 的 HTTP 映射写 GET（`docs/specification.md:1169`、`docs/specification.md:2795` 与 `specification/a2a.proto:74-83`）。

## 2. 分层结构

A2A 把协议分成三层：

| 层 | 内容 |
|---|---|
| 数据模型 | Task、Message、Part、Artifact、AgentCard、Extension |
| 抽象操作 | 发送消息、取任务、列任务、取消任务、订阅、push 配置、取扩展卡 |
| 协议绑定 | JSON-RPC、gRPC、HTTP+JSON |

JSON 线格式使用 camelCase，枚举使用 ProtoJSON 的 SCREAMING_SNAKE_CASE 名称，时间为 UTC ISO 8601（`docs/specification.md:1176-1232`）。Onlyne 绑定跨越这条命名边界时要做显式转换，不在内部类型上二选一。

## 3. 发现与地址

AgentCard 是服务自描述清单，包含 `name`、`description`、`supportedInterfaces`、`version`、`capabilities`、`securitySchemes`、`securityRequirements`、输入输出媒体类型和技能（`specification/a2a.proto:357-398`）。

- AgentCard 通过 `https://{server_domain}/.well-known/agent-card.json`、注册目录或直接配置获得（`docs/specification.md:1974-1980`）。
- `supportedInterfaces` 是有序列表，第一项优先；每项包含 URL、`protocolBinding`、可选 `tenant` 和 `protocolVersion`（`specification/a2a.proto:334-355`）。
- 客户端选中接口后，必须把该接口的 `tenant` 原样放进每个请求（`docs/specification.md:1993-2000`）。
- AgentCard 可用 JWS 签名，签名前用 RFC 8785 JCS 规范化（`docs/specification.md:2002-2013`）。

`tenant` 是不透明路由值，格式和语义由服务决定。Onlyne 不得从 URL 或 tenant 推断身份、权限或资源归属。

## 4. 消息身份

```json
{
  "messageId": "msg-01",
  "contextId": "ctx-01",
  "taskId": "task-01",
  "role": "ROLE_USER",
  "parts": [{"text": "请核对第二组数据。"}],
  "metadata": {},
  "referenceTaskIds": []
}
```

（`specification/a2a.proto:254-277`）

- `messageId` 由消息创建者生成，必填。
- 服务端消息必须带 `contextId`；任务存在时带 `taskId`。
- 客户端可以只带 `taskId`，此时服务端从任务推断 `contextId`。同时提供两者且不一致时，服务端必须拒绝（`docs/specification.md:617-628`）。
- `contextId` 把多条消息和任务归入同一会话上下文，是不透明值。客户端可以复用，也可以由服务端生成；服务端生成后必须随响应返回（`docs/specification.md:584-602`）。
- `taskId` 由服务端生成。客户端不能指定新任务的 id（`docs/specification.md:604-615`）。
- `referenceTaskIds` 只是相关任务提示，不建立新的身份或权限。

## 5. 操作与线格式

| 操作 | JSON-RPC 方法 | HTTP 路径 | 幂等 |
|---|---|---|---|
| 发送消息 | `SendMessage` | `POST /message:send` | 可选，按 messageId 去重 |
| 流式发送 | `SendStreamingMessage` | `POST /message:stream` | 同上 |
| 取任务 | `GetTask` | `GET /tasks/{id}` | 是 |
| 列任务 | `ListTasks` | `GET /tasks` | 是 |
| 取消任务 | `CancelTask` | `POST /tasks/{id}:cancel` | 是 |
| 订阅任务 | `SubscribeToTask` | `GET /tasks/{id}:subscribe`（proto；方法表写 POST） | 是 |
| 创建 push 配置 | `CreateTaskPushNotificationConfig` | `POST /tasks/{id}/pushNotificationConfigs` | 按配置 |
| 读取 push 配置 | `GetTaskPushNotificationConfig` | `GET /tasks/{id}/pushNotificationConfigs/{configId}` | 是 |
| 列出 push 配置 | `ListTaskPushNotificationConfigs` | `GET /tasks/{id}/pushNotificationConfigs` | 是 |
| 删除 push 配置 | `DeleteTaskPushNotificationConfig` | `DELETE /tasks/{id}/pushNotificationConfigs/{configId}` | 按删除语义 |
| 取扩展卡 | `GetExtendedAgentCard` | `GET /extendedAgentCard` | 是 |

JSON-RPC 方法名以引用提交的方法映射表为准（`docs/specification.md:1160-1174`）。v1 使用 PascalCase，实现不得沿用 0.3 的 `message/send` 写法。`{tenant}` 作为可选前缀出现。

发送结果是一个二选一的联合：Task 或 Message（`specification/a2a.proto:778-802`）。流式结果是 Task、Message、状态更新或产物更新之一。

`SendMessageConfiguration.returnImmediately` 为 true 时，创建任务后立即返回；为 false（默认）时，必须等待任务进入终态或中断态（`specification/a2a.proto:143-161`，`docs/specification.md:442-448`）。

该字段对三种情况没有作用（`docs/specification.md:450-454`）：直接返回 Message 的响应、流式操作、push 配置。规范没有为「不支持该字段」定义单独能力或错误，接入不得自行发明一个。

## 6. 任务状态与更新

| 状态 | 类别 |
|---|---|
| `TASK_STATE_SUBMITTED`、`TASK_STATE_WORKING` | 进行中 |
| `TASK_STATE_INPUT_REQUIRED`、`TASK_STATE_AUTH_REQUIRED` | 中断，等待客户端输入 |
| `TASK_STATE_COMPLETED`、`TASK_STATE_FAILED`、`TASK_STATE_CANCELED`、`TASK_STATE_REJECTED` | 终态 |

（`specification/a2a.proto:186-208`）

任务更新有三条通道：轮询 `GetTask`、流式订阅、push webhook（`docs/specification.md:646-677`）。

- 流式事件必须按生成顺序投递，同一任务的多个流收到相同顺序的事件，关闭一个流不影响其他流（`docs/specification.md:679-700`）。
- push 通知使用 HTTP POST，负载与流式响应同形；客户端必须返回 2xx，并应幂等处理重复投递（`docs/specification.md:842-887`）。
- 服务端对每个 webhook 至少尝试投递一次，可以重试，也可以在连续失败后停止（`docs/specification.md:882-887`）。
- 消息不是可靠投递机制；流断开重连后可能缺少部分状态消息（`docs/specification.md:760-764`）。

## 7. 错误

错误响应必须包含机器可读错误码、可读消息和可选结构化详情；详情数组的每项必须带 `@type`（`docs/specification.md:538-549`）。

| A2A 错误 | JSON-RPC | gRPC | HTTP | Onlyne 映射 |
|---|---|---|---|---|
| `TaskNotFoundError` | -32001 | NOT_FOUND | 404 | `not_found` |
| `TaskNotCancelableError` | -32002 | FAILED_PRECONDITION | 400 | `invalid_request` |
| `PushNotificationNotSupportedError` | -32003 | FAILED_PRECONDITION | 400 | `unsupported_delivery` |
| `UnsupportedOperationError` | -32004 | FAILED_PRECONDITION | 400 | `unsupported_delivery` |
| `ContentTypeNotSupportedError` | -32005 | INVALID_ARGUMENT | 400 | `unsupported_media_type` |
| `InvalidAgentResponseError` | -32006 | INTERNAL | 500 | `internal` |
| `ExtendedAgentCardNotConfiguredError` | -32007 | FAILED_PRECONDITION | 400 | `not_found` |
| `ExtensionSupportRequiredError` | -32008 | FAILED_PRECONDITION | 400 | `unsupported_delivery` |
| `VersionNotSupportedError` | -32009 | FAILED_PRECONDITION | 400 | `invalid_request` |

（`docs/specification.md:1176-1194`）

映射进 Onlyne 时保留原生错误名和原文到 `details.source`，并保留 `retryable` 的实际含义。A2A 的鉴权、参数和资源错误按同一原则映射到 RFC 0001 的错误集合。

## 8. 能力校验

Agent 必须按 AgentCard 声明拒绝未协商的能力（`docs/specification.md:569-578`）：

- 未声明 push 支持时，push 配置操作返回 `PushNotificationNotSupportedError`。
- 未声明流式支持时，流式发送和订阅返回 `UnsupportedOperationError`。
- 未声明扩展卡时，取扩展卡返回 `UnsupportedOperationError`；声明支持但未配置时返回 `ExtendedAgentCardNotConfiguredError`。
- 服务端要求 `required: true` 的扩展而客户端未声明时，返回 `ExtensionSupportRequiredError`。

Onlyne 绑定必须在发送前检查这些能力，不能把不支持的操作发送出去再解释失败。

## 9. 映射到 Onlyne

| Onlyne 事实 | A2A 对应 | 保留的区别 |
|---|---|---|
| 目标 endpoint | AgentInterface URL 与 tenant | 地址不授予身份或权限 |
| 本地 thread 条目 | Message 及 contextId | contextId 不自动等于 thread 或 OMP 会话 |
| 消息身份 | messageId | 发送方预先生成；绑定不得重铸 |
| 回复关系 | `referenceTaskIds`、任务内后续 Message | A2A 没有通用 `inReplyTo` 消息引用；Onlyne 的 reply 关系需在扩展或适配层保持 |
| 业务答复 | 一条 Message，或任务产物 | 任务产物不自动成为消息 |
| 持久接纳 | 服务端是否持久保存 Message | A2A 核心未定义持久接纳；只能声明 binding-defined |
| 发送方等待 | `returnImmediately: false` | 等待任务进入终态或中断态，不是 Onlyne 的 reply 关系 |
| 任务状态 | TaskState | 远端事实，不复制成本地任务状态 |
| 资源操作 | 未定义 | 由资源服务或扩展提供 |

绑定规则：

1. 对端返回 Message 时，按普通消息记录并解析关系。
2. 对端返回 Task 时，记录 `taskId`、`contextId`、状态快照和产物引用。终态只作为对端报告保存。
3. Onlyne 的 `await` 只能被合法 `reply` 解除。A2A 的 `returnImmediately: false` 返回任务状态，不等于业务答复。
4. 原始 A2A 服务未声明持久接纳扩展时，Onlyne 报告 `binding-defined`。
5. A2A 的 push 与流式更新按事件送达运行时。丢失后，`GetTask` 或 `ListTasks` 只能恢复任务当前状态和可用产物；规范不保证补回全部事件或历史 Message（`docs/specification.md:760-764`）。
6. A2A 不定义 IM 会话所有权转移。资源交接使用 RFC 0006 的资源操作，不借用 Task。

## 10. 绑定验收

- AgentCard 的接口顺序和 tenant 被正确使用，未声明能力在本地被拒绝。
- 启用幂等扩展时，messageId 在重投中保持不变，接收方按约定去重。基础 A2A 只允许按 messageId 检测重复，不规定内容不同必须返回冲突。
- `contextId` 不被当作本地 thread id 或 OMP 会话 id。
- `returnImmediately` 的两种取值都按上游语义执行，并在工具结果中如实报告等待的对象。
- 流式事件缺失时，客户端能恢复任务当前状态和可用产物，并明确报告无法恢复的事件，不把缺失当成任务失败。
- TaskState 的改变只更新对端报告，不产生 Onlyne 工作状态。
- 每个 A2A 错误映射后保留原生错误名、可读消息和实际可重试性。
