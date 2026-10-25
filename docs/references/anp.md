# ANP 上游规范参考

状态：参考材料，非规范。本文记录 ANP 1.2 的上游事实，以及 Onlyne 实现网络绑定和身份互操作时的核对依据。本文不是 ANP 规范的替代文本，也不规定 Onlyne 的行为。

来源：2026-10-25 从 `docs/rfcs/0010-anp-reference.md` 迁出，编号 0010 保留，在 [RFC 0000 总览](../rfcs/0000-overview.md) 中指向本文件。上游事实与 Onlyne 侧约束分开记录：§1–§8 只记录上游事实；§9–§10 是从这些事实推到 Onlyne 约束的细节，规范性映射与验收以 [RFC 0007 §2、§6](../rfcs/0007-bindings.md) 为准。

## 1. 版本、来源与成熟度

| 项目 | 值 |
|---|---|
| 规范站点 | `https://agent-network-protocol.com/specs/1.2/` |
| 文档集 | ANP 1.2 |
| 代码仓库 | `agent-network-protocol/AgentNetworkProtocol` |
| 引用提交 | `0837cfdfaf4ddcf834f0890ddaf34fc4c824e066` |
| 发布 | v1.2，2026-09-24 |
| 本地快照 | `Alexandria/raw/2026-10/agent-protocol/anp/`，访问日期 2026-10-23 |

固定来源：[版本与状态目录](https://github.com/agent-network-protocol/AgentNetworkProtocol/blob/0837cfdfaf4ddcf834f0890ddaf34fc4c824e066/README.md)、[P1 公共绑定](https://github.com/agent-network-protocol/AgentNetworkProtocol/blob/0837cfdfaf4ddcf834f0890ddaf34fc4c824e066/message/01-core-binding.md)、[P2 身份与发现](https://github.com/agent-network-protocol/AgentNetworkProtocol/blob/0837cfdfaf4ddcf834f0890ddaf34fc4c824e066/message/02-identity-and-discovery.md)、[P3 直接消息](https://github.com/agent-network-protocol/AgentNetworkProtocol/blob/0837cfdfaf4ddcf834f0890ddaf34fc4c824e066/message/03-direct-messaging-base-semantics.md)、[P8 联邦](https://github.com/agent-network-protocol/AgentNetworkProtocol/blob/0837cfdfaf4ddcf834f0890ddaf34fc4c824e066/message/08-federation-and-cross-domain.md)。

各组件成熟度不同，引用时必须分开：

| 文档 | 状态 |
|---|---|
| ANP-01 白皮书 | 信息性 |
| ANP-02 身份认证 | 已发布 |
| ANP-03 DID:WBA 方法 | 已发布 |
| ANP-04 WBA 命名空间 | 已发布 |
| P2 身份与发现 | 已发布 |
| ANP-07 Agent 描述 | 已发布 |
| ANP-08 Agent 发现 | 已发布 |
| ANP-09 即时消息 | 已发布，其中 P6 群组端到端加密为候选 |
| ANP-05 授权 | 草案，未发布 |
| ANP-06 元协议 | 草案 |
| AP2 支付适配 | 草案 |

1.2 是文档发布版本。官方 README 不声明实现互通，SDK 支持放在独立的 AgentConnect 仓库。Onlyne 只能声明「按某次提交的规范实现」，不能声明「与 ANP 生态互通已验证」。

## 2. 分层

ANP 由三层组成（`white-paper` §3.1）：

| 层 | 内容 |
|---|---|
| 身份与安全通信 | DID、WBA 命名空间、认证、密钥分发、端到端加密 |
| 元协议 | 通信协议之间的协商与组合；ANP-06 仍是草案 |
| 应用协议 | Agent 描述（ADP）、Agent 发现（ADSP） |
| 消息 Profile | 直接消息、群组消息、加密覆盖、附件、联邦 |

消息 Profile 是分开的文档，不是一个大而全的消息对象。Onlyne 只需要实现与自身范围相关的 Profile，未实现的 Profile 必须显式拒绝。

## 3. 身份

ANP 把身份分成三层，三者不得自动等同（P2 §3.5）：

| 身份 | 载体 | 作用 |
|---|---|---|
| 业务来源 DID | `meta.sender_did` | 业务发起者，参与业务语义 |
| 逻辑发行者 DID | `group_did`、回执或治理对象的发行者 | 对象发行与见证 |
| 服务 DID | `ANPMessageService.serviceDid` | 一跳服务到服务的调用身份 |

- Agent 用 `agent_did` 标识。能收发 ANP 消息的主体必须有 agent DID（P2 §3.2）。
- 群组用 `group_did`。它是群组的全局应用层标识和发现入口（P2 §3.3）。
- `serviceDid` 只说明哪一跳服务在执行调用，不能替代业务 DID，也不能参与群组角色判定。
- 设备选择器 `device_id` 只属于声明了设备寻址加密的 Profile；基础消息不得携带。

`did:wba` 和原生 `did:web` 共用 ANP-02 的请求认证流程，身份材料按各自的 DID 方法校验。其他 DID 方法通过方法绑定扩展接入（ANP-02 §2、附录 A、附录 B）。

认证与授权是两件事。DID 文档里的 `authentication` 关系授权请求签名；它不包含人类授权关系。高风险操作仍需产品层的人类确认（白皮书 §4.1）。

[ANP-02 认证规范](https://github.com/agent-network-protocol/AgentNetworkProtocol/blob/0837cfdfaf4ddcf834f0890ddaf34fc4c824e066/02-anp-did-authentication-protocol-specification.md) 使用 RFC 9421 的 `Signature-Input` 和 `Signature`。带正文时还使用 RFC 9530 的 `Content-Digest`。签名至少覆盖 `@method`、`@target-uri` 和存在时的 content-digest。keyid 指向 DID 文档授权的 authentication 密钥，created 必填。服务验证签名、时间窗口与重放缓存，challenge 给出 nonce 时按一次性 nonce 校验。认证失败返回 401 与 challenge，认证后无权限返回 403。签名重放检测与业务操作去重分别处理。

## 4. 发现

发现有三个入口：

1. **DID 文档**。支持主动交互的 Agent DID 文档至少包含一个 `ANPMessageService`。直接消息入口、能力协商、密钥材料访问和对象控制都通过这一个统一服务入口暴露（P2 §5.4）。
2. **Agent 描述文档**。ADP 描述 Agent 的名称、所属实体、能力、产品、服务、交互 API。文档可用 `Proof` 防篡改（ANP-07）。
3. **域级发现**。`https://{domain}/.well-known/agent-descriptions` 返回 JSON-LD `CollectionPage`，`items` 列出该域公开的 Agent 描述文档，`next` 分页（ANP-08）。

DID 文档只保存低频、低敏感、与身份直接相关的信息。实时在线状态、群成员表和高频轮换日志不得写进 DID 文档（P2 §4.1、§4.3）。

Onlyne 的 `node_id` 可以是 agent DID，但 `base_url` 不能从 DID 推断。服务地址来自 DID 文档的 `ANPMessageService` 或部署配置。

## 5. 外部绑定

ANP Core Binding（P1）规定所有消息使用 JSON-RPC 2.0，并要求：

- `params` 必须是对象，禁止数组形式（P1 §5.1）。
- 请求 `id` 必须是非空字符串（P1 §5.2）。
- 禁止批量调用（P1 §5.4）。
- 除通知外，每个请求必须返回响应；成功响应只含 `result`，失败只含 `error`（P1 §5.6）。
- 所有绑定必须运行在经验证的安全传输上；HTTP(S) 与 WSS 是推荐绑定（P1 §4.2、§4.3）。

`params` 的通用结构包含 `meta`、可选的 `auth` 和 `body`。`auth` 只在具体 Profile 要求时出现；直接消息的 `transport-protected` 模式要求它（P1 §6.1、P3 §6.4）。下面是一个 P3 请求示例：

```json
{
  "meta": {
    "profile": "anp.direct.base.v1",
    "security_profile": "transport-protected",
    "sender_did": "did:example:agent-a",
    "target": {"kind": "agent", "did": "did:example:agent-b"},
    "operation_id": "msg-10001",
    "message_id": "msg-10001",
    "created_at": "2026-10-23T02:05:00Z",
    "content_type": "text/plain"
  },
  "auth": {
    "scheme": "anp-rfc9421-origin-proof-v1",
    "origin_proof": {
      "contentDigest": "sha-256=:BASE64_DIGEST:",
      "signatureInput": "sig1=(...);created=...;expires=...;nonce=...;keyid=...",
      "signature": "sig1=:BASE64_SIGNATURE:"
    }
  },
  "body": {"text": "请核对第二组数据。"}
}
```

`meta` 的关键字段（P1 §6.2）：

| 字段 | 规则 |
|---|---|
| `profile` | 必填，唯一决定 `body`、结果和错误如何解释，不得表示多个 Profile 的并集 |
| `security_profile` | 必填，取 `transport-protected`、`direct-e2ee`、`group-e2ee` |
| `sender_did` | 业务发送者；不得自动等同于外层链接的一跳调用者 |
| `target` | `kind` 取 `agent`、`group`、`service`；`did` 不携带传输地址 |
| `operation_id` | 所有改变状态的操作必填；重投保持不变 |
| `message_id` | 带消息的操作必填；重投保持不变 |
| `content_type` | 带消息的操作必填，描述业务负载类型 |
| `created_at` | 建议填写，不得作为唯一防重放手段 |

`body` 由 Profile 定义。直接消息的 `text`、`payload`、`payload_b64u` 必须恰好出现一个，出现多个或全缺都拒绝（P3 §6.3）。

## 6. 直接消息

`direct.send` 是直接消息的最小互操作能力（P3 §7.1）。

- 目标必须是单个 Agent，`meta.target.kind` 为 `agent`。
- 一个请求只有一个发送者和一个接收者，不能广播。
- `auth.origin_proof` 绑定规范化后的请求对象，签名跨转发链保持不变。
- 成功结果至少包含 `accepted: true`、`message_id`、`operation_id`、`target_did`、`accepted_at`。

`direct.incoming` 是必需的推送通知，由目标入口服务把已接纳的消息推给目标 Agent。它不返回响应，字段必须与原消息一致（P3 §7.2）。

**成功只表示目标入口服务接纳了消息，并把它放进了协议处理边界。** 它不表示业务负载已处理、已展示、已持久化或已送达用户（P3 §3.3、§8.1）。

顺序与重投：

- 不提供全局严格顺序。同一 `conversation_id` 内可以做尽力而为的顺序处理，跨域层不得假设 FIFO（P3 §8.3）。
- 幂等判断的最小键是 `(sender_did, target.did, method, operation_id)`；消息级重复识别建议再加 `message_id`（P3 §8.2）。
- 重投时 `operation_id`、`message_id` 保持不变，业务负载语义等价（P3 §8.4）。
- 自动重投的次数和退避由本地策略决定，Profile 不作要求。

`conversation_id` 是应用层上下文提示，不是安全上下文，也不是防重放标识（P3 §5.3）。

## 7. 能力协商、幂等与错误

能力协商是强制接口 `anp.get_capabilities`。DID 文档里的静态扩展字段只是缓存提示，运行时以协商结果为准（P1 §8）。

幂等作用域是 `(sender_did, target_scope, method, operation_id)`。语义等价时返回相同或等价结果；语义冲突返回 `anp.idempotency_conflict`（P1 §9.1）。服务器必须先查幂等命中，再做版本前置条件检查。

错误使用 JSON-RPC 的 `error` 对象。ANP 保留标准错误码，并定义 1000 以上的业务错误码，机器可读值放在 `error.data.anp_code`（P1 §10.1、§10.2）。

| 错误码 | `anp_code` | 含义 |
|---|---|---|
| 1005 | `anp.unauthorized` | 未通过认证 |
| 1006 | `anp.forbidden` | 已认证但无权限 |
| 1007 | `anp.target_not_found` | 目标 DID 不存在或不可达 |
| 1008 | `anp.idempotency_conflict` | 幂等键冲突 |
| 1009 | `anp.unsupported_content_type` | 内容类型不支持 |
| 1010 | `anp.delivery_rejected` | 目标服务拒绝接收 |
| 1012 | `anp.temporarily_unavailable` | 服务暂不可用 |
| 2000 | `direct.recipient_unreachable` | 目标 Agent 暂不可达 |
| 2001 | `direct.policy_violation` | 目标策略拒绝 |
| 2005 | `direct.invalid_origin_proof` | 来源证明无效 |
| 2007 | `direct.origin_proof_replayed` | 来源证明疑似重放 |

完整表见 P1 §10.3 与 P3 §11。`data.retryable` 应表达调用方能否重试（P1 §10.4）。

## 8. 联邦与跨域

跨域直接消息经过发送域服务、目标域入口服务和目标 Agent（P3 §3.3）。两类认证必须分开：

- **来源证明**由 `sender_did` 的密钥生成，跨转发链保持不变，目标域必须独立验证。
- **一跳认证**证明当前连接由哪个服务或 Agent 提交，可以逐跳变化。
- 接收方不得仅凭上游服务身份推定业务来源（P3 §3.5、§9.4）。

跨域成功语义是目标入口接纳。群组消息的成功语义是群组 Host 接纳并排序（ANP-09 §4.5）。两者都不等于业务完成。

P8 根据目标种类选择最终服务：agent 解析 Agent DID 的 ANPMessageService，group 解析 Group DID 对应的 Group Host，service 解析目标 serviceDid。跨域直接使用原方法，如 `direct.send`、`group.send`，不再包装一个私有 relay 方法。群组序号由 Group Host 分配。附件控制方法经 ANPMessageService，文件字节经独立 HTTPS 通道下载，不进入联邦转发链。

## 9. 映射到 Onlyne

| Onlyne 事实 | ANP 对应 | 保留的区别 |
|---|---|---|
| 节点身份 | Agent DID | DID 是业务身份；`serviceDid` 是一跳服务身份 |
| 目标 endpoint | `meta.target` | 目标 DID 不携带 URL；地址经 DID 文档发现 |
| 本地 thread 条目 | `message_id`、`conversation_id` | `conversation_id` 不是 thread，也不是安全上下文 |
| 回复关系 | `body.reply_to_message_id` | ANP 只在 Profile 内定义；Onlyne 需按 Profile 能力协商 |
| 持久接纳 | 目标入口接纳 | 入口接纳不证明持久化；Onlyne 只能报告 binding-defined |
| 发送方等待 | 无对应操作 | ANP 不定义业务答复等待；Onlyne 的 await 不能映射成 JSON-RPC 调用等待 |
| 任务状态 | 无 | ANP 不定义任务对象；Onlyne 也不引入任务投影 |
| 资源操作 | ANP 核心未定义 | 由应用 Profile 或资源服务提供 |
| 人类授权 | ANP-05 草案 | 不得把草案当作已发布保证 |

绑定规则：

1. 身份映射必须区分业务来源 DID、逻辑发行者 DID 和一跳服务 DID。三者任一不得被提升为另一者。
2. `operation_id` 和 `message_id` 由发送方生成，重投保持不变。Onlyne 的幂等键可以取 `operation_id`。
3. `meta.profile` 必须显式选择，不能由 Onlyne 猜测。未实现的 Profile 在 ANP 线上返回 `code: 1001` 和 `error.data.anp_code: "anp.unsupported_profile"`；Onlyne 内部把它映射为 `unsupported_delivery`，并说明支持范围。
4. `security_profile` 只能显式协商。不得从高安全等级静默降级到 `transport-protected`。
5. `direct.send` 返回 `accepted: true` 时，Onlyne 记录为“目标入口已接纳”，保证等级为 binding-defined。不能写成 durable。
6. `direct.incoming` 作为事件送达运行时。ANP 不提供历史拉取，也不要求重发；目标入口接纳而本地从未收到的消息无法由协议补齐。接入只能按自己的本地记录处理，或依赖双方另行约定的查询与重放扩展。不能假设推送可靠。
7. ANP 不定义 thread 归属和成员读取范围。Onlyne 的 thread 与许可由 Onlyne 自己持有。
8. 资源交接使用 RFC 0006。ANP 的 `serviceDid` 变更不等于资源接手。

## 10. 绑定验收

- DID 文档校验失败、`keyid` 与 `sender_did` 不一致时，消息被拒绝并保留原因。
- `meta.profile` 缺失或多个 Profile 混用时，请求被拒绝。
- `target.kind` 与方法声明的寻址模式不一致时，返回 `anp.invalid_target_binding`。
- 重投同一 `operation_id` 返回原结果；同键不同内容返回 `anp.idempotency_conflict`。
- `direct.send` 成功只报告入口接纳，不报告模型已读、用户已看或业务完成。
- `security_profile` 降级被显式拒绝，不静默接受。
- 推送通知丢失后，接入只按本地已存记录恢复；无法取得的通知明确报告为缺失，不假装已补齐，也不把缺失当成对端失败。
- 未发布的 ANP-05 授权能力不被当作可用能力。
