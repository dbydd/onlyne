# RFC 0007：协议绑定

状态：Draft。依赖：RFC 0001～0006。

## 1. 绑定必须声明的保证

每个绑定把 Onlyne 语义映射到具体协议，并声明自己的保证范围。绑定 MUST 报告实际保证，MUST NOT 把传输层的成功提升为 Onlyne 的持久接纳。

| Onlyne 语义 | 绑定必须回答 |
|---|---|
| 消息身份 | 原 MessageRef 如何保留，中继是否重新铸 id |
| 持久接纳 | 是否满足 durable，还是 binding-defined |
| 关系 | reply、followup、receipt 如何区分 |
| 重投 | 幂等键、retry_until 和去重如何传递 |
| 结果未知 | 无响应或超时时如何记录为 `unknown`，保留哪个引用供查回 |
| 授权 | 调用者身份、成员资格和回复许可如何映射 |
| 事件 | 更新如何送达，游标如何续传 |
| 资源操作 | 交接、作业控制如何映射，不支持的操作为何 |

绑定不映射某项能力时 MUST 显式报错。静默降级会让调用方把未发生的保证当作已发生。

### 错误翻译

非 HTTP 绑定把原生错误翻译到 RFC 0001 §4 的错误码。翻译 MUST 保留三件事：`error.code` 属于 RFC 0001 的集合、`retryable` 反映实际可否重试、`details` 只放调用方有权读取的信息。

- A2A 的 `TaskNotFoundError`、`UnsupportedOperationError` 等原生错误映射到对应的 Onlyne 错误码，原生码和原文放进 `details.source`。
- MCP 的协议错误和工具错误分别映射。工具执行的业务失败用工具自己的结果表达，不用 Onlyne 错误码冒充传输失败。
- 绑定无法表达某个 Onlyne 错误码时，返回最近的码并在 `details` 说明差异。MUST NOT 把拒绝说成成功。
- Onlyne 错误进入 A2A 或 MCP 时，同样保留 code、retryable 和必要的 details。
- `unknown` 不是错误码。绑定遇到传输层无响应或超时时，MUST 把它记录为结果 `unknown`，MUST NOT 写成 `refused`、`aborted` 或 `binding-defined` 的成功。

## 2. A2A 映射

A2A 1.0 分三层：数据模型、抽象操作和协议绑定。数据模型包含 AgentCard、Message、Task、Part、Artifact 和 Extension。Send Message 可以返回 Task 或直接 Message，异步更新由流式响应和 push 通知提供。引用提交、操作名和已知上游矛盾见 [A2A 上游规范参考](../references/a2a.md)。

| Onlyne | A2A | 必须保留的区别 |
|---|---|---|
| endpoint 描述 | AgentCard、接口与能力声明 | 描述不替代调用权限 |
| 消息与关联 | Message、messageId、contextId、taskId | context 不自动等于本地 thread 或 OMP 会话 |
| 文本、数据、资源引用 | Part | 资源 URI 不自动授予读取权限 |
| 计算结果与产物 | Task、Artifact 及更新事件 | 远端作业状态由远端负责 |
| 异步答复 | 流式更新、push 通知 | 保留来源与对应的本地消息引用 |
| 持久接纳、执行意图 | 协商的 Extension 或绑定契约 | 不支持时明确标明保证范围 |
| 资源接管 | 资源服务声明的操作或 Extension | A2A 基础规范没有定义 IM 会话所有权转移 |

A2A Task 是对端的工作对象。Onlyne 保存关联和对端报告，不把远端 Task 复制成本地任务，也不维护与它同步的状态机。本地待办由运行时 todo 承载。

- 对端返回 Message 时，Onlyne 记录该消息并按其关系处理。
- 对端返回 Task 时，Onlyne 记录 Task 引用和对端报告。Task 的终态是对端事实，不是 Onlyne 的工作状态。
- A2A 的 push 通知和流式更新送达运行时。Onlyne 不假设它们可靠；事件丢失时按对端提供的任务查询恢复状态，未收到的消息不能保证补齐。

A2A 身份、发现、AgentCard 和传输绑定由该规范定义。Onlyne 不为跨 agent 互操作重新发明身份和传输层。

### ANP 映射

ANP 1.2 使用 DID 身份、JSON-RPC 2.0 外部绑定和按 Profile 分开的消息语义。身份层次、`direct.send` 的接纳边界、幂等键与错误码见 [ANP 上游规范参考](../references/anp.md)。

| Onlyne | ANP | 必须保留的区别 |
|---|---|---|
| 节点身份 | Agent DID | 业务来源、逻辑发行者和一跳服务 DID 不得互相替代 |
| 目标 endpoint | `meta.target` | DID 不是传输地址 |
| 本地 thread 条目 | `message_id`、`conversation_id` | conversation 不是 thread，也不是安全上下文 |
| 回复关系 | `body.reply_to_message_id` | 按 Profile 能力协商，不得假设所有 Profile 支持 |
| 持久接纳 | 入口接纳 | 入口接纳不证明持久化，保证等级为 binding-defined |
| 资源交接 | 核心规范未定义 | 使用 RFC 0006，不借服务 DID 变更代替 |

ANP 的 `security_profile` MUST 显式协商，禁止静默降级。未实现的消息 Profile 返回 `unsupported_delivery`。ANP-05 授权与 ANP-06 元协议仍是草案，不得作为已发布保证引用。

## 3. MCP 映射

MCP 承载工具调用、资源读取和订阅通知。计算服务和受控资源可以通过 MCP 暴露操作。

- MCP 工具调用映射为资源操作。调用结果按 RFC 0006 记录。
- MCP 资源读取映射为资源数据访问。MCP 资源 URI 不自动授予 Onlyne 的 thread 读取权或回复权。
- MCP 订阅通知映射为资源事件。通知丢失时按资源查询补齐。
- MCP 不承载 Onlyne 的消息身份和回执。跨节点的 Onlyne 消息仍走 HTTP 或 A2A 绑定。

## 4. HTTP/JSON 绑定

HTTP/JSON 是首个直接互操作绑定。约束见 RFC 0001 和 RFC 0002～0006 的接口定义。

- 线程、消息、成员、资源操作各有明确路径和状态码。
- SSE 提供可恢复的节点事件。游标与消息序号分开。
- 跨主机使用 HTTPS。回环开发可以使用 HTTP，认证与授权规则不变。
- 凭据不得写入消息正文、日志或公开描述。

### 路由模型

HTTP 绑定没有路由表、跳数预算和中转决策。一次投递的顺序固定：

1. 调用方认证，解析出 `actor`。
2. 权威服务校验 `thread_ref.owner` 是自己或持有有效委托的节点。
3. 校验成员资格、thread 状态和消息身份摘要。
4. 追加条目、回执和待处理项，一次提交。

地址由引导条目或发现记录直接给出，协议不推断路径。ingress 只做委托校验和原样转发，不重新选路。路由决策属于部署配置和 SystemOne，协议层保持可预测。

各绑定的 SDK 选型、可用符号与 sidecar 方案属于实现选择，见 [库设计](../design/lib/) §8。

## 5. OMP 运行时绑定

OMP 是首个运行时接入目标。接入把库操作映射到宿主能力，并把宿主的忙闲、plan 状态与注入时机翻译成投递执行。

接入 MUST 提供下列能力，逐条的可观察结果见 [《OMP 宿主契约》](../contracts/omp.md) §3：

- 单一模型消息入口调用 `msg`，模型没有第二套发信或 thread 管理工具。
- 消息条目按索引渲染给模型，模型能按 `(thread, seq)` 读全文。
- 工具调用能停在库的 Promise 上，合法 reply 解除等待，用户中止只取消本地等待。
- 入站来信由 SystemOne 判定动作后执行，忙时排到宿主边界，plan 模式有明确队列状态。
- 新执行上下文具有自己的身份、权限与消息关联。
- 同一根消息的补充内容进入它已有的执行上下文，上下文可用时不创建第二个。
- 从本地记录恢复未决消息、上下文映射与消费记录。
- 来信可写入运行时待办，快照与消息关联同一次可恢复提交，恢复不重复建项；提醒由待办提供者发出。

宿主接口无法表达等待语义时，接入 MUST 在发送前拒绝 `await`，并在 `details` 指出缺失的能力，不先发送再返回 handle。handle 模式 MUST 单独协商，MUST NOT 作为 `await` 的降级实现。

OMP 的模型界面、工具形状、列表渲染、注入时机与 todo 规则由 [OMP 宿主契约](../contracts/omp.md) 规定；模型界面的组成见 [RFC 0008](0008-omp-surface.md)，宿主 API 证据见 [OMP 扩展接口调查](../references/omp-extension-api.md)。

接入限制 MUST NOT 改写库语义。无法兑现的能力显式拒绝。

## 6. 验收

- A2A 直返 Message、返回 Task、流式产物和错误都保留原保证范围。
- A2A context 不被当成本地 thread 或 OMP 会话。
- MCP 资源 URI 不授予 thread 读取或回复权限。
- 绑定不支持的能力显式报错，不静默降级。
- 传输层无响应时结果为 `unknown`，不写成拒绝或成功。
- HTTP 绑定的事件丢失后可以按消息查询补齐。
