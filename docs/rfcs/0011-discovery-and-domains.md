# RFC 0011：节点发现与无 owner 的域

状态：Draft。依赖：RFC 0001、0002、0007、0009、0010。`packages/lib` 的域接口按本文实现；DID、签名发现和入口委托由各自网络绑定提供，本文不声明它们已经实现。

## 1. 域的含义

域是一组节点的逻辑划分。域使用稳定的绝对 URI `domain_id`，例如 `urn:onlyne:domain:research`。域没有 owner、管理员、中心成员表或全局 revision。创建标识的节点与其他节点具有相同地位。

一个节点同时只属于一个域，加入、退出和切换保持 `node_id`、密钥、消息引用与业务记录。切换域必须先退出当前域，再加入新域；处于 active 状态时加入另一个域返回 `conflict`。域可以跨设备和网络，同一设备也可以运行属于不同域的节点。域不由 IP、端口、主机名、cwd 或终端位置推断。

域不提供跨域连通能力。thread 可以跨域：owner 属于一个域，成员可以来自其他域，投递和读取按成员表执行。资源的跨域访问仍经资源 owner 的 endpoint 或既有的 ingress 委托，连接、路由和保证等级保持原状；域成员关系不新增入口，也不改变可达性。

每个节点只提交自己的成员关系。节点 A 不能替 B 加入或退出域。宿主插件可以在 A 的本地授权下调用 A 的接口；它不因此获得修改 B 的权限。域没有统一关闭操作：各节点分别退出，之后继续加入同一 domain_id 仍表示加入同一节点组。需要全新协作时使用新的 domain_id。

加入域只表示节点声明自己属于该组。发现某节点、共同加入某域，都不授予 thread 的 post/read、回复许可或资源控制权。封闭协作的访问控制由节点的认证插件、thread 成员表与资源授权分别执行。

## 2. 本地成员记录与权威

```json
{
  "domain_id": "urn:onlyne:domain:research",
  "node_id": "urn:onlyne:agent:reviewer",
  "state": "active",
  "revision": "membership-rev-1"
}
```

| 字段 | 规则 |
|---|---|
| `domain_id` | 绝对 URI，精确比较，不解析其中的主机或节点前缀 |
| `node_id` | 声明该关系的节点身份，必须是当前服务节点 |
| `state` | `active` 或 `left` |
| `revision` | 由该节点生成，只用于该节点与该域的关系，不跨节点比较 |

同一节点与同一域只有一条当前记录，同一节点最多有一条 active 记录。节点以自己的存储事务提交关系及幂等结果。存储中出现多条 active 记录时，读接口返回 `conflict`，不静默选择其中一条。退出保留 `left` 记录与 revision，防止迟到的加入覆盖退出。节点重建时从原存储插件恢复，不凭断链推断退出。

查询和修改均先认证。只有认证主体等于当前 `node_id` 的修改请求可以提交。已认证对端可以读取该节点的域声明，具体凭据由宿主认证插件验证。未认证的公开服务描述不列域。域标识本身不是秘密凭据，也不证明任何业务权限。

## 3. 公开库接口

`OnlyneNode.domains` 提供以下接口，`auth` 与其他嵌入服务共用主体模型：

| 接口 | 效果 |
|---|---|
| `create(auth, mutation)` | 生成 `urn:onlyne:domain:<id>`，并在同一事务里加入本节点；返回本节点成员记录 |
| `join(auth, domain_id, mutation)` | 本节点加入该域；已有其他 active 域时返回 `conflict` |
| `leave(auth, domain_id, mutation)` | 本节点退出，保留关系墓碑 |
| `list(auth, options?)` | 返回本节点的 active 关系，至多一条 |
| `describe(auth, domain_id)` | 返回本节点的当前关系，包括 left；从未记录时 not_found |
| `discover(auth, domain_id, signal?)` | 核对候选节点的当前声明，返回已观察成员与查询失败节点 |

`mutation` 携带 `idempotency_key`、`retry_until`；`join/leave` 还接受 `expected_revision`。首次加入省略 expected_revision。已有关系的修改必须匹配当前 revision，缺失或不匹配时返回 `revision_conflict`。同状态、匹配版本的请求返回当前记录，不增加版本。

同键同内容重投返回原提交结果，包括退出之后重投原加入请求的历史成功结果。历史结果只证明原操作提交过，当前成员状态以 describe 为准。重投不复活关系。使用新幂等键但旧版本的迟到请求必须拒绝。离开未知域返回 not_found。

`create` 只分配组标识，没有创建者权限、域级元数据写入权或他人成员管理权。宿主可以预先分配绝对 URI 并分别调用各节点 join，不需要先运行一个建域服务。

## 4. HTTP 接口与描述

以下路径相对 `/onlyne/v3`，路径中的 domain_id 编码为单个 URI segment：

| 路径 | 调用与结果 |
|---|---|
| `POST /domains` | 创建标识并加入本节点；已有 active 域时返回 `conflict` |
| `GET /domains` | 返回本节点 active 关系，至多一条 |
| `GET /domains/{domain_id}` | 查询本节点当前关系 |
| `POST /domains/{domain_id}/join` | 加入本节点，body 为修改信封及可选 expected_revision |
| `POST /domains/{domain_id}/leave` | 退出本节点，body 为修改信封及 expected_revision |
| `GET /domains/{domain_id}/members` | 从本节点视角发现该域成员 |

所有接口需要认证。修改接口只接受本节点主体，路径不包含可供调用方替换的 node_id。HTTP 与进程内调用执行同一服务逻辑、事务与幂等规则。

`GET /description` 的 `features` 包含 `domains`。认证后返回 `domain: string | null`，为当前节点唯一的 active 域；未认证时省略该字段。域级 `ingress` 字段不属于本协议。服务描述不宣称整个域的成员全集。

## 5. 发现：候选来源与节点声明

发现分为候选取得、身份地址解析、成员关系确认三个步骤。

1. 宿主的 `domainCandidates(domain_id)` 插件返回可能属于该域的 NodeId 列表。来源可以是显式配置、进程内注册、网络发现服务或未来的 Tern 插件。
2. 库去重候选并加入自身。`resolve(node_id)` 把每个远端候选映射到 URL 与认证请求头，内置 Fetch transport 检查解析身份一致性，跨主机使用 HTTPS。
3. 库向候选的 `GET /domains/{domain_id}` 查询。响应的 domain_id、node_id、state、revision 均需校验。只有该候选自己的 active 声明进入 members。

```json
{
  "domain_id": "urn:onlyne:domain:research",
  "members": [
    {
      "domain_id": "urn:onlyne:domain:research",
      "node_id": "urn:onlyne:agent:reviewer",
      "state": "active",
      "revision": "membership-rev-1"
    }
  ],
  "unavailable": ["urn:onlyne:agent:offline"]
}
```

远端 left 或 not_found 不进入成员结果。超时、认证失败、不可达或响应字段不匹配的节点进入 unavailable；不能把这些失败解释为已退出。成员结果只表示本次核对的观察，不是全局一致快照，也不代表所有成员已被枚举。候选插件遗漏的节点可能仍属于该域。

未提供候选插件时 discover 返回 `unsupported_delivery`，不把只有自身的列表冒充已完成发现。本节点未加入时拒绝发现；已退出返回 not_a_member，从未记录返回 not_found。远端查询按节点的 wait_ms 有界等待，AbortSignal 取消本次发现；取消不变更成员关系。

候选目录没有成员写入权。第三方声称“B 在域 D”只触发向 B 的核对，不能替 B 提交记录。查询使用 resolve 返回的认证连接；自定义 transport 必须保留该身份绑定。认证插件与信任配置仍负责拒绝冒充端点。

## 6. 身份、地址与入口分别配置

节点身份是稳定 NodeId，base_url 是可变地址。域的 domain_id 与两者分开。切换到另一个域时先退出当前域，节点不更换身份，也无需搬动服务、历史或工作区。

| 来源 | 负责提供的内容 |
|---|---|
| 本地引导 | NodeId 到 URL、认证方式与信任材料的明确映射 |
| DID 绑定 | 从该 scheme 的文档解析地址并验证绑定声明 |
| 域候选插件 | 给定 domain_id 下的候选 NodeId |
| 节点域接口 | 该节点自己当前的成员关系 |
| Bonjour 等服务浏览 | 当前链路上的候选地址，不证明身份或归组 |

本地引导和凭据由宿主加载，库不主动读文件或扫描设备。未登记的标识 scheme 不猜地址。对端目录可提供候选，库只通过受信任的 resolve/transport 请求目标身份。DID、AgentCard 验签和证书固定属于具体绑定的职责，不由域名字符串代替。

共用网络入口是独立的部署能力。节点可以把服务委托给一个已认证的代理节点，委托列明接收消息、服务 thread 等范围并可撤销。加入域不自动委托，入口不代表域拥有者。库当前只服务自身 owner 的 thread/resource，入口代行需要对应绑定实现明确的委托检查。

跨域发送仍按完整 NodeId、ThreadRef 和资源引用执行，连通性沿用原有地址解析与 ingress 委托。域不会重铸 MessageRef、复制 thread、改写回复权威或提升 durable 保证，也不改变资源 endpoint 的入口。共同成员关系不能绕过 thread 的成员资格；不同域也不会自动使已授权投递被拒。

资源跨域读取、发送或交接使用资源 owner 声明的操作。目标资源属于另一个域时，调用方仍通过该资源 owner 的 endpoint 或其有效 ingress 委托到达，事件归属、积压、revision 检查与交接规则不因域成员关系改变。

## 7. SystemOne 与宿主接入

模型保持一个 msg 入口。SystemOne 可以返回 `kind: domain` 的 create、join、leave、list、describe、discover 动作，库将动作映射到本节点域服务。模型无需生成幂等键或网络期限；join/leave 的条件版本由明确参数和现有成员记录决定。

域目录查询结果可以由宿主提供给判定器，SystemOne 不得把 unavailable 节点说成已退出或把候选当成已验证成员。同名别名存在多个候选时必须显式消歧；成员资格不自动映射为 mentions、thread 成员或运行时 context。

Tern 之后由插件维护 `Tern session 稳定标识 → domain_id` 的关联，向各节点调用 join/leave，并通过 domainCandidates 提供候选。库暴露相同的公开接口，承担持久记录与校验；它不读取 Tern 状态、不根据 cwd 或环境变量自动选择域。物理网络位置可以作为插件的归组策略，协议同时允许项目、会话等逻辑分组。

## 8. 验收

- 创建者不能替其他节点修改归组，不存在域级 owner 字段或管理员权限。
- 一个节点最多有一个 active 域；加入第二个域返回 `conflict`，退出后可加入新域并保留原身份。多台设备的节点可以使用同一 domain_id。
- 本节点主体才能 join/leave，其他已认证节点的修改被拒。
- stale expected_revision 和迟到 join 不覆盖 left 记录；同键重投返回历史结果且不改变当前状态。
- 同一存储插件重建节点后 active/left 关系恢复。
- 本地列表至多返回一条 active 关系；存储里出现多条 active 时读接口报 conflict，不静默取舍。
- 第三方候选、重复候选、已退出节点都不能产生虚假成员；错误响应与网络失败明确列入 unavailable。
- 无候选插件和中止发现均有明确结局，不改变成员记录。
- 未认证服务描述不列域，认证描述按当前 active 关系更新。
- 共同加入域后，非 thread 成员的投递仍返回 not_a_member。
- thread 跨越两个域时，成员表与回复许可照原规则判定；跨域资源操作仍经 owner endpoint 或 ingress，连通性与保证等级不变。
