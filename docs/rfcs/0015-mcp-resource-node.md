# RFC 0015：MCP 资源节点

状态：Draft。依赖：RFC 0001、0006、0007。

## 1. 适用范围

本文定义把 MCP server 作为 Onlyne 资源节点接入的形状。MCP server 是无状态工具提供者，自己不持有平台事件，也不维护 assignee 关系。

适用场景：搜索 API、代码执行、数据查询、格式转换。不适用的场景：IM 会话（RFC 0012）、长时作业（RFC 0013）、可订阅数据源（RFC 0014）。这些类别有状态和归属，MCP 的工具调用装不下。

## 2. MCP 原语到 Onlyne 的映射

| MCP 原语 | Onlyne 资源 | 说明 |
|---|---|---|
| `tools/list` | 资源目录与操作清单 | 分页返回，`nextCursor` 对应本地 cursor |
| `tools/call` | 资源操作 | `name` 是 `operation`，`arguments` 是 `data` |
| `resources/list` | 资源数据目录 | 数据项，不是可操作对象 |
| `resources/read` | 资源数据读取 | 只读，不产生业务副作用 |
| `resources/subscribe` | 资源事件订阅 | 通知只报变更，不带内容 |
| `notifications/resources/updated` | 资源事件通知 | 收到后需再读正文 |
| `prompts/list`、`prompts/get` | 提示词模板 | 归宿主使用，不进 Onlyne 资源操作 |
| `initialize` 能力协商 | 服务描述的 `features` | 声明的能力才可调用 |
| `notifications/tools/list_changed` | 目录变更通知 | 收到后重新拉取操作清单 |
| `notifications/resources/list_changed` | 数据目录变更 | 收到后重新拉取数据目录 |

`tools/call` 的调用形状：

```json
{
  "operation_id": "op-01",
  "retry_until": "2026-10-23T04:00:00Z",
  "operation": "get_weather",
  "data": {"location": "New York"}
}
```

`operation` 必须出现在该 MCP server 的 `tools/list` 结果里。清单外的操作返回 `unsupported_delivery`，不发送到对端。`data` 必须匹配该工具的 `inputSchema`，不匹配返回 `invalid_request`。

`prompts` 不映射为资源操作。提示词模板由宿主决定如何使用，Onlyne 不为它建立资源身份、授权或事件。

## 3. 结果与错误

MCP 的结果分成功和 `isError: true` 两种。Onlyne 必须区分这两类失败：

| MCP 返回 | Onlyne 记录 |
|---|---|
| `result.content` 且 `isError` 为 false 或缺省 | `committed`，正文进 `data` |
| `isError: true` 且正文描述业务失败 | `aborted`，`error.code` 取 `invalid_request` 或 `unsupported_delivery`，原文进 `details.source` |
| JSON-RPC 协议错误（`-32602` 等） | `aborted`，按 RFC 0001 §4 映射 |
| 传输层失败、超时、连接断开 | `unknown`，保留 `operation_id` 可查回 |

`isError: true` 表示工具执行器报告了失败。它不区分参数错误、权限拒绝和资源内部故障。Onlyne 按正文和错误码推断，推断不出时用 `unavailable` 并保留原文，不猜一个具体的码。

`aborted` 只表示这次调用被明确拒绝或中止。它不表示远端没有执行过任何副作用。调用方需要区分「没有发生」与「不知道是否发生」时，以 `unknown` 为准。

`resources/read` 的返回是 `contents` 数组，每项带 `uri` 和 `mimeType`。Onlyne 把它作为资源数据保存，不升级为消息，也不写入任何 thread。

## 4. 授权

MCP 的授权在 `initialize` 阶段和能力声明上，没有 per-call 的权限模型。Onlyne 补一层：

| 事实 | 持有者 |
|---|---|
| 这个 agent 能调哪些 MCP server | 本地引导配置 |
| 这个 MCP server 暴露哪些工具 | MCP server 自己 |
| 这次调用是否放行 | 本地策略，基于 server 身份和工具名 |

本地策略 MUST 在调用前检查。未授权的 server 或工具返回 `forbidden`，不发送请求。策略允许的工具仍需通过 `inputSchema` 校验。

MCP 的 `resources/subscribe` 不携带调用方身份授权。订阅本节点的资源是本地动作，不代表资源 owner 授予了读取权。

## 5. 恢复

MCP 是无状态协议，恢复集中在连接、订阅和调用三处。

### 连接恢复

`initialize` 协商能力和协议版本。版本不一致时对端返回 `-32602` 并在 `data.supported` 列出可用版本。Onlyne 记录实际协商的版本，不静默降级。

连接断开后重连必须重新 `initialize`，不复用旧会话的能力。重连失败按 RFC 0001 的错误返回，连接状态不进任何业务记录。

### 订阅恢复

MCP 的订阅不保证重放。断开期间发生的 `notifications/resources/updated` 会丢失，重连后不会补发。

- 重连后 Onlyne MUST 重新订阅，并把断开期间标记为未知。
- 恢复手段是重新 `resources/list` 加 `resources/read`，比较本地已见内容。
- 无法确认断开期间是否发生变化时，如实报告 `unknown`。MCP 没有游标，协议不提供精确补齐。

### 调用恢复

MCP 请求没有幂等键。`tools/call` 的重发可能产生第二次副作用。

- Onlyne 为每次调用生成 `operation_id`，在本地记录中查重。
- 响应超时或连接断开时，本地状态为 `unknown`。
- 重试策略分两种：只读工具可以重发；有副作用的工具 MUST 由本地策略决定是否重发，协议不承诺恰好一次。
- 工具自身幂等时，重发安全。工具不幂等时，Onlyne 只能报告 `unknown` 并让人决定。

### 超时与取消

MCP 用 `notifications/cancelled` 取消请求，用 progress 通知表示仍在工作。

- Onlyne 为每次调用设置超时。超时后发送取消通知并停止等待。
- 收到 progress 通知不重置最大超时上限。
- 取消只表示本地不再等待，不取消对端的执行。对端是否停下由 MCP server 决定。

## 6. 接入边界

MCP server 与 Onlyne 节点的关系是「一个 endpoint 提供一组工具」。

- 一个 Onlyne 节点可以接入多个 MCP server，各有权限和超时配置。
- 一个 MCP server 可以被多个 Onlyne 节点接入，MCP 不维护接入者关系。
- MCP server 不持有 assignee，不需要 RFC 0006 的交接流程。
- 平台凭据在 MCP server 侧，Onlyne 不传递令牌。

## 7. 验收

- `tools/list` 之外的工具被本地拒绝，请求不离开本节点。
- `inputSchema` 校验失败返回 `invalid_request`，不发送到 MCP server。
- `isError: true` 与 JSON-RPC 错误分别映射，保留原生原文。
- 传输超时的调用记录为 `unknown`，不写成失败。
- 重连后重新 `initialize` 并重新订阅，断开期间的变化标记为未知。
- 有副作用的工具重发前经过本地策略，协议不承诺恰好一次。
- MCP 资源数据不写入任何 thread，不产生消息。
