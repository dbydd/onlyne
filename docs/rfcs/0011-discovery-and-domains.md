# RFC 0011：Region、网络自动发现与节点目录

状态：Draft。依赖：RFC 0001、0002、0007、0009、0010。本文规定 region 与网络发现的目标契约。`packages/lib` 与 OMP 插件已接入公开 region 声明、种子与 HTTP 发现目录、SQLite 候选和动态地址；真实多进程与跨主机验收仍未完成。

## 1. 域的含义

Region（域）是一组节点的协议归属。本文中的 region 与现有库接口中的 domain 表示同一对象，线上成员字段继续使用 `domain_id`，宿主配置使用 `region`。region 标识使用稳定的绝对 URI，例如 `urn:onlyne:region:research-chamber`。它没有 owner、管理员、中心成员表或全局 revision。创建标识的节点与其他节点具有相同地位。

OMP 节点在 bootstrap 中 MUST 确定 region，启动消息服务前提交自己的 active 成员关系。节点身份 `node_id` 由用户填写，初始输入为空。region 单独配置，MUST NOT 拼进 node_id；移动 region 后原 MessageRef、ThreadRef 和 ResourceRef 保持有效。目录同时展示节点身份与当前 region。

一个节点同时只属于一个域，加入、退出和切换保持 `node_id`、密钥、消息引用与业务记录。切换域必须先退出当前域，再加入新域；处于 active 状态时加入另一个域返回 `conflict`。域可以跨设备和网络，同一设备也可以运行属于不同域的节点。域不由 IP、端口、主机名、cwd 或终端位置推断。

域不提供跨域连通能力。thread 可以跨域：owner 属于一个域，成员可以来自其他域，投递和读取按成员表执行。资源的跨域访问仍经资源 owner 的 endpoint 或既有的 ingress 委托，连接、路由和保证等级保持原状；域成员关系不新增入口，也不改变可达性。

每个节点只提交自己的成员关系。节点 A 不能替 B 加入或退出域。宿主插件可以在 A 的本地授权下调用 A 的接口；它不因此获得修改 B 的权限。域没有统一关闭操作：各节点分别退出，之后继续加入同一 domain_id 仍表示加入同一节点组。需要全新协作时使用新的 domain_id。

加入域只表示节点声明自己属于该组。发现某节点、共同加入某域，都不授予 thread 的 post/read、回复许可或资源控制权。封闭协作的访问控制由节点的认证插件、thread 成员表与资源授权分别执行。

## 2. 本地成员记录与权威

```json
{
  "domain_id": "urn:onlyne:region:research",
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

成员修改需要本节点授权，只有主体等于当前 `node_id` 的修改请求可以提交。节点自己的 region 声明可通过公开服务描述读取，消息、历史与资源访问继续执行各自的授权检查。域标识本身不是秘密凭据，也不证明任何业务权限。

## 3. 公开库接口

`OnlyneNode.domains` 提供以下接口，`auth` 与其他嵌入服务共用主体模型：

| 接口 | 效果 |
|---|---|
| `create(auth, mutation)` | 生成 `urn:onlyne:region:<id>`，并在同一事务里加入本节点；返回本节点成员记录 |
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

上述成员接口需要认证。修改接口只接受本节点主体，路径不包含可供调用方替换的 node_id。公开服务描述提供自动发现所需的当前成员声明。HTTP 与进程内调用执行同一服务逻辑、事务与幂等规则。

`GET /description` 的 `features` 包含 `domains` 与 `discovery`。公开响应携带 `domain: string | null` 以及 `membership: DomainMembership | null`，两者指向本节点同一条 active 记录；退出后两者均为 null。`membership` 含 domain_id、node_id、state 和 revision，供发现方核对。域级 `ingress` 字段不属于本协议。服务描述不宣称整个域的成员全集。

## 5. 发现：候选来源与节点声明

发现分为候选取得、身份地址解析、成员关系确认三个步骤。

1. 引导接受一个可达种子地址；已联系节点通过 `GET /discovery` 交换候选；同链路 DNS-SD 可以作为可选来源。所有来源的候选写入节点自己的 SQLite 发现目录。`domainCandidates(domain_id)` 从该目录返回候选 NodeId。
2. 发现目录按 NodeId 保留所有候选地址及实例观察，逐一直接核对后处理冲突。`domainCandidates` 对身份去重，`resolve(node_id)` 必须先完成该身份的实例冲突检查，再返回当前可用地址及绑定所需的认证信息。内置 Fetch transport 检查解析身份一致性，跨主机使用 HTTPS。
3. 发现方直接请求候选的 `GET /description`，核对 protocol、node_id、base_url 和 membership，并记录本次返回的 instance_id。候选 instance_id 属于旧观察；同一地址上的新响应可更新它，不能仅凭实例不同拒绝重启后的节点。节点身份仍按网络绑定核对。只有节点自己返回且成员字段一致的 active 声明进入 members。受保护的成员接口仍可用于认证后的进一步核对。

### 5.1 同一网络链路的自动发现（可选）

HTTP 候选交换是发现的必需路径。同一链路上的 DNS-SD 是可选的加速器：实现 MAY 发布并浏览 `_onlyne._tcp.local.`，缺少多播、缺少 DNS-SD 库或发布失败都不影响节点启动、消息与 region。SRV 记录提供实际主机与监听端口；TXT 记录提供 `protocol=onlyne/3`、`path=/onlyne/v3`、`scheme=http|https` 与 `node_id`。服务实例名标识本次进程实例，不作为 NodeId。region 归属由节点 HTTP 声明确认，浏览时保留其他 region 的候选，以便跨 region 通信。

启用 DNS-SD 时，监听成功后再发布服务，退出时撤销本次发布。每次启动向操作系统申请可用端口；重启后身份保持原样，地址可以改变。多播记录到期、goodbye 或请求失败只改变可达性观察，不修改节点成员关系或业务结果。

bind 为 `127.0.0.1` 时，只接受本机发布者的发现结果。DNS-SD 适配器必须依据平台提供的本地发布者信息，或将应答来源地址与本机接口地址核对；无法确定来源的回环候选不得使用。回环地址不进入 HTTP 候选交换。bind 为通配地址时从对应接口取得可发布的具体地址，服务描述不得把 `0.0.0.0` 或 `::` 当成可连接地址。普通终端与 Tern 采用同一网络路径。

### 5.2 HTTP 候选交换与跨链路引导

公开接口 `GET /onlyne/v3/discovery?cursor=...&page_size=...` 返回以下 JSON。page_size 默认 50，上限 100；next_cursor 为 null 表示结束。cursor 绑定本服务的候选查询及位置，公开查询不绑定认证主体。候选排序按 node_id、instance_id 和 base_url 确定；cursor 失效返回 cursor_expired。

```json
{
  "items": [
    {
      "node_id": "urn:onlyne:agent:reviewer",
      "instance_id": "process-unique-id",
      "base_url": "https://reviewer.example:49152/onlyne/v3",
      "last_seen": "2026-10-24T08:00:00Z"
    }
  ],
  "next_cursor": null
}
```

接口只交换可跨主机使用的具体地址，排除回环地址、通配地址与缺少有效作用域的链路本地地址。字段 last_seen 表示提供目录的节点最后观察该候选的时间，读取方另外保存自己的 received_at。接口提供地址线索，不返回凭据、消息、执行上下文或成员写入权。读取候选后仍需直接核对目标的服务描述。

已联系的节点通过此接口交换候选，接受者将候选写入自己的 SQLite 并独立核对。这个路径是跨子网、禁用多播和网络隔离时的必需入口。bootstrap 必须允许用户提供一个可达节点地址；该地址作为发现记录保存到 SQLite，无需预写 peers 表。尚无可达入口时，界面报告发现范围与具体网络错误；网络恢复后继续发现。

### 5.3 发现记录与冲突

每个进程启动时生成新的 instance_id，所有地址的服务描述返回同一标识。SQLite 记录至少包含候选 node_id、地址、来源、instance_id（直接核对前可缺失）、last_seen、received_at、有效期、最后直接核对结果与成员 revision。秘密凭据不进入公开候选。TOML 只保存本节点配置，不保存动态 peers；用户级共享文件注册表不承担网络发现职责。

第三方目录与 DNS-SD 结果是候选证据，节点自己的 HTTP 响应是直接声明。已认证绑定额外提供身份验证结果；未认证 E2E 绑定必须标明声明未经身份验证。同一 NodeId 同时出现多个直接核对成功且 instance_id 不同的实例时，目录记录冲突，投递返回 `conflict`，不得按最后收到的广播覆盖。同一实例的多个地址可以合并。旧地址不可达且新实例直接核对成功时，可以更新解析记录。目录恢复后重新核对缓存地址，过期记录保留历史来源，不冒充当前可达节点。

```json
{
  "domain_id": "urn:onlyne:region:research",
  "members": [
    {
      "domain_id": "urn:onlyne:region:research",
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

候选目录没有成员写入权。第三方声称“B 在域 D”只触发向 B 的核对，不能替 B 提交记录。查询使用 resolve 返回的连接并保留该绑定的身份验证等级。认证插件与信任配置负责拒绝冒充端点；未认证 E2E 的限制由 RFC 0001 明确声明。

域只扩大候选来源，不扩大信任。与一个已认证节点握手后经候选交换取得的成员仍是线索级候选；节点间信任按 [RFC 0017](0017-node-auth.md) 逐对建立，共同加入域不产生任何自动信任或自动连接。批量信任动作只是界面合批，每对关系保留独立记录与撤销。

## 6. 身份、地址与入口分别配置

节点身份是稳定 NodeId，base_url 是可变地址。域的 domain_id 与两者分开。切换到另一个域时先退出当前域，节点不更换身份，也无需搬动服务、历史或工作区。

| 来源 | 负责提供的内容 |
|---|---|
| Bootstrap | 用户确认本节点身份、region 与监听 host；可选的跨链路初始联系地址 |
| 网络发现目录 | 经网络取得并核对的 NodeId、当前地址与观察证据，存入 SQLite |
|| DID 绑定 | 从该 scheme 的文档解析地址并验证绑定声明 |
|| 域候选插件 | 给定 domain_id 下的候选 NodeId |
|| 节点域接口 | 该节点自己当前的成员关系 |
|| Bonjour 等服务浏览 | 当前链路上的候选地址，不证明身份或归组 |
|| 信任表（RFC 0017） | 已验证的对端公钥与信任状态，存 SQLite |

配置与凭据由宿主加载，库通过网络绑定执行发现。用户无需逐节点配置地址；端口由进程本次监听产生。未登记的标识 scheme 不猜地址。对端目录可提供候选，库通过 resolve/transport 请求目标身份。DID、AgentCard 验签和证书固定属于具体绑定的职责，不由域名字符串代替。

共用网络入口是独立的部署能力。节点可以把服务委托给一个已认证的代理节点，委托列明接收消息、服务 thread 等范围并可撤销。加入域不自动委托，入口不代表域拥有者。库当前只服务自身 owner 的 thread/resource，入口代行需要对应绑定实现明确的委托检查。

跨域发送仍按完整 NodeId、ThreadRef 和资源引用执行，连通性沿用原有地址解析与 ingress 委托。域不会重铸 MessageRef、复制 thread、改写回复权威或提升 durable 保证，也不改变资源 endpoint 的入口。共同成员关系不能绕过 thread 的成员资格；不同域也不会自动使已授权投递被拒。

资源跨域读取、发送或交接使用资源 owner 声明的操作。目标资源属于另一个域时，调用方仍通过该资源 owner 的 endpoint 或其有效 ingress 委托到达，事件归属、积压、revision 检查与交接规则不因域成员关系改变。

## 7. SystemOne 与宿主接入

模型保持一个 msg 入口。SystemOne 可以返回 `kind: domain` 的 create、join、leave、list、describe、discover 动作，库将动作映射到本节点域服务。模型无需生成幂等键或网络期限；join/leave 的条件版本由明确参数和现有成员记录决定。

域目录查询结果可以由宿主提供给判定器，SystemOne 不得把 unavailable 节点说成已退出或把候选当成已验证成员。同名别名存在多个候选时必须显式消歧；成员资格不自动映射为 mentions、thread 成员或运行时 context。

OMP bootstrap 为 region 提供独立输入。有 Tern 时可用当前 session 名作为建议值，用户确认后提交成员关系；没有 Tern 时同样提供完整引导与网络发现。Tern session、cwd 与环境变量均不直接构成协议归属。每个角色是一个 OMP 进程和一个节点；多个角色可属于同一 region，thread 按实际协作动态创建。

## 8. 验收

- 创建者不能替其他节点修改归组，不存在域级 owner 字段或管理员权限。
- 一个节点最多有一个 active 域；加入第二个域返回 `conflict`，退出后可加入新域并保留原身份。多台设备的节点可以使用同一 domain_id。
- 本节点主体才能 join/leave，其他已认证节点的修改被拒。
- stale expected_revision 和迟到 join 不覆盖 left 记录；同键重投返回历史结果且不改变当前状态。
- 同一存储插件重建节点后 active/left 关系恢复。
- 本地列表至多返回一条 active 关系；存储里出现多条 active 时读接口报 conflict，不静默取舍。
- 第三方候选、重复候选、已退出节点都不能产生虚假成员；错误响应与网络失败明确列入 unavailable。
- 无候选插件和中止发现均有明确结局，不改变成员记录。
- 公开服务描述返回本节点当前 region 声明，退出后清空；公开候选不泄露凭据与消息。
- 多个空工作区经 bootstrap 启动后，用种子地址和 HTTP 候选交换发现彼此，无需 peers TOML 或共享文件注册表。取消引导不写配置；节点身份输入初始为空。
- 重启改变端口后，通过新发布更新地址；原 node_id 与消息引用保持有效。
- DNS-SD 与第三方目录中的伪造或过期候选必须直接核对；重复 NodeId 冲突不得被最后一条广播掩盖。
- DNS-SD 缺失或不可用时，节点启动、消息和 region 不受影响；用户通过引导联系一个已有节点，再经 HTTP 目录发现其他候选。
- 没有 Tern 时能完成相同的引导、发现、消息往返和 region 成员操作。
- 共同加入域后，非 thread 成员的投递仍返回 not_a_member。
- thread 跨越两个域时，成员表与回复许可照原规则判定；跨域资源操作仍经 owner endpoint 或 ingress，连通性与保证等级不变。
- 加入同一域后，节点间不产生自动信任；信任表中无对应记录时，认证按未信任处理。批量动作建立的每对信任可独立撤销。
