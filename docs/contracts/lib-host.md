# lib 宿主契约：SDK 接口与宿主义务

状态：Draft，规范性契约。上游来源：RFC 0003 §2、RFC 0004 §5、RFC 0005 §3、RFC 0011 §3、RFC 0016 §3.7、RFC 0017 §2、§3.5、§6。

本文件规定 `@onlyne/lib`（下称“库”）对宿主公开的接口、宿主必须提供的能力，以及判定符合性的场景。协议字段与线上行为以 RFC 为准；库的内部结构与常量在 [`../design/lib/`](../design/lib/) 另记；实现进度与验收证据单独保存，不改变本文要求。本文的 MUST、MUST NOT、SHOULD 按 RFC 2119 解释。

术语：

- **宿主**：创建 `OnlyneNode`、提供插件、监听 HTTP 的进程或插件。
- **运行时**：宿主中把条目送进模型上下文、维护 todo 的部分。
- **主体**：`Auth.actor`，一次调用的已认证节点身份。
- **维护**：库推进对账、出站重投、信任重驱与待办重放的周期工作。

## 1. 分工与插件清单

库拥有协议状态机与提交：消息接纳、关注义务、回复关联、信任行、域成员记录、幂等与重投记录都在库内完成。宿主提供存储、认证、地址解析、判定、运行时接入和平台能力。

宿主 MUST NOT 复制一份协议状态作为第二权威，MUST NOT 为消息或 thread 生成 complete、pending、failed、blocked 一类工作状态。宿主 MUST NOT 在库之外重实现消息重投、信任重发或对账计时器。

| 插件 | 宿主必须提供的行为 | 缺失时库的行为 |
|---|---|---|
| `storage` | 原子、可串行化事务；事务回调失败时回滚全部写入；声明接纳保证 | 构造契约不成立 |
| `authenticate` | 把入站请求映射为已验证主体及明确授权的转发身份 | 构造契约不成立 |
| `resolve` | 把节点身份解析为服务地址与认证请求头 | 构造契约不成立 |
| `decide` | 把消息意图解释为一个可执行动作 | 构造契约不成立；判定失败时库记录失败并返回 `decision_unavailable` |
| `runtime` | `supports`、`inject`、`notify`，按需提供 `todo`、`trace`、`confirmToolResult` | 不注入来信；`await` 在发送前被拒绝 |
| `keys` | 加载或创建节点身份密钥，可选 `replace` | 需要密钥的操作返回 `unsupported_delivery` |
| `storage` 保密 | 未确认轮换期间，旧私钥以字节保存在 `trust-outbound` 行内；storage 所在目录与数据库文件（含 WAL/SHM）MUST 按私钥同等保密（目录 0700、文件 0600），或由宿主改用引用把旧钥留在密钥钩子里 | 库不检查宿主存储权限 |
| `maintenanceClock` | 提供一次性 `setTimer` 与 `clearTimer` | 库不启动维护环，宿主自行调度 |
| `domainCandidates` | 提供可能声明某域的候选节点 | `domains.discover` 返回 `unsupported_delivery` |
| `transport` | 用其他网络绑定替换内置 Fetch transport | 使用内置 Fetch transport |

资源驱动、接纳策略、目录等其余插件由各自 RFC 与宿主设计定义，不在本契约展开。

只要某项能力缺失，库 MUST 以显式错误拒绝，MUST NOT 用默认值补出语义决定，MUST NOT 退化为明文或未签名请求。

符合性场景：

- 不提供 `keys` 的宿主调用 `trust.begin`：返回 `unsupported_delivery`，没有任何明文介绍信发出。
- 不提供 `runtime` 的宿主发送 `response: "await"` 的计划：返回 `unsupported_delivery`，对端没有收到请求。
- 不提供 `domainCandidates` 的宿主调用 `domains.discover`：返回 `unsupported_delivery`，不返回猜测出来的成员。
- 宿主在库外自行重发介绍信：不符合；重驱只由维护环执行。

## 2. 构造、身份与可信时钟

`OnlyneNode` 的必填项是 `node_id`、`base_url`、`plugins`、`now`、`id`。宿主 MUST 提供这五项；库不提供默认实现。

- `node_id` 是节点身份，`base_url` 是服务地址并包含 `/onlyne/v3`。宿主 MUST 独立提供这两项；库 MUST NOT 从监听地址、主机名、cwd 或终端位置推导它们。
- `now` MUST 返回宿主实际使用的毫秒时间。库用它计算签名时间窗、nonce 过期、重投截止与维护节奏。返回常量、随机值，或回跳到已关闭窗口之前的时钟 MUST 视为不符合。
- `id` MUST 返回不可猜测且在同一宿主内不重复的字符串。库用它生成 nonce、幂等键、域名与成员 revision。宿主无法提供时 MUST 拒绝运行需要密钥的节点，MUST NOT 退回可预测的默认值。
- 重建节点时宿主 MUST 继续使用原 `storage` 与同一 `node_id`；库不迁移宿主存储。
- 一个节点的 `storage` MUST 由单个进程持有。轮换的换钥窗口与旧钥证明依赖这一点；跨进程共用同一存储时，库不保证第二次轮换被拒。

符合性场景：

- 用同一 `storage` 重建节点：信任行、域成员记录、inbox 与消费记录仍在，签名身份不变。
- 连续两次调用 `id`：返回值不同，且不是时间戳、自增计数器等可预测序列。
- 把 `now` 换成常量后与真实对端握手：时间戳校验失败，本节点不扩大时间窗来接受它。
- 缺少 `now` 或 `id` 的构造：类型检查不通过；运行期不得以 `Date.now` 或随机 UUID 静默替代。

## 3. 密钥钩子与密钥生命周期

`plugins.keys` 是库取得节点身份密钥的唯一入口，形状为 `() => Promise<{ private_key, public_key }>`，可附带 `replace(material)`。

- 库 MUST NOT 决定密钥位置、读写密钥文件或生成密钥文件。位置、权限与版本库忽略规则由宿主决定；OMP 侧的具体规则在 [OMP 契约](omp.md)。
- 宿主 MUST 返回匹配的私钥与公钥。库 MUST 校验由私钥派生的公钥与返回值一致，不一致时拒绝使用。
- 宿主 MUST 持久保存密钥，并在重启后返回同一把。已经存在信任行时，宿主 MUST 通过 `trust.rotate` 更换身份，MUST NOT 静默生成新密钥。
- `replace` MUST 在返回前完成持久写入。`trust.rotate` 与尚无信任关系时的密钥重建都经过这个钩子。
- `replace` 拒绝时，宿主 MUST 让旧钥仍然可用：所有可能失败的步骤在换入新私钥之前完成。库在钩子拒绝后 MUST 重新读取钩子，只有钩子仍报旧指纹时才把在途轮换当作失败。
- 密钥钩子缺失或失败时，需要密钥的操作 MUST 失败；库 MUST NOT 使用内存临时密钥，MUST NOT 发送未签名的跨主机请求。

符合性场景：

- 首次启动创建密钥，重启后 `keys.describe()` 返回相同指纹。
- 返回不匹配的私钥与公钥：操作失败，且没有写入信任行。
- `replace` 持久化失败：`trust.rotate` 返回错误，旧密钥仍然可用。
- 钩子每次调用都生成新密钥：不符合；同一节点在信任关系存在期间必须保持同一身份。

## 4. 维护环与维护时钟

提供 `plugins.maintenanceClock` 后，维护环归库所有。时钟提供 `setTimer(ms, run)` 与 `clearTimer(handle)`。`setTimer` 是一次性定时器：到期调用 `run` 一次后即失效，维护环在每轮结束时重新设置下一轮。时钟不带时间源；每处时钟读取都走 `NodeOptions.now`，第二套时间会与签名窗口和重投截止冲突。

- 提供时钟时，宿主 MUST 在构造后调用 `client.runMaintenance()`，在关闭或重建前调用 `client.stopMaintenance()`。重复调用 `runMaintenance` 不产生第二个环。
- 每轮维护 MUST 依次执行 `maintain()` 与 `recover()`，一轮不可重入。`maintain()` 覆盖授权通知投递与确认、待办重放、inbox 重放、对账与信任重驱；`recover()` 覆盖出站重投与过期。信任重驱在 `maintain()` 内，因为无时钟宿主只有这一个调度入口。
- 有未完成工作时库 MUST 使用活跃节奏，持久记录清空后降到空闲节奏。具体间隔属于库设计，宿主 MUST NOT 配置或复制这些常量。
- 宿主 MUST NOT 另行实现消息重投、信任重发或对账计时器，MUST NOT 调用 `trust.redrive()`（它已包含在 `maintain()` 内）。
- 不提供时钟时，库 MUST NOT 自行启动任何后台工作。宿主 MUST 自行按同样职责调用 `client.maintain()` 与 `client.recover()`。
- `stopMaintenance()` 之后，宿主提供的 `clearTimer` MUST 真正取消回调；停止后不得再收到维护回调。
- 宿主 MUST 让 `setTimer` 只触发一次。自动重复触发同一回调，或让 `clearTimer` 无法阻止已安排的调用，都 MUST 视为不符合。
- 一轮结束时库 MUST 清掉已触发的定时器句柄再排下一轮。宿主时钟保留已触发句柄直到 `clearTimer` 时，这一条保证不会每次节奏都多留一个句柄。
- `hasPendingWork()` 是展示用读取。宿主 MUST NOT 用它替代维护节奏。

符合性场景：

- 启动后没有入站事件：维护仍按期重放未注入条目并重投未结出站记录。
- 调用 `stopMaintenance()`：计时器不再触发，进程可以退出。
- 重启后：未确认的 inbox 条目与未执行的 todo 钩子被重放。
- 未提供时钟且宿主未调度 `maintain`/`recover`：未确认条目保持原状，库不声称已经完成维护。
- 宿主把 `clearTimer` 实现为空操作，或把 `setTimer` 实现成周期定时器：不符合；停止后仍有回调、或一轮出现多次回调即违约。

## 5. 通知通道

宿主 MUST 提供 `runtime.notify(event)`。库在提交完成后调用它；通知抛错 MUST NOT 回滚已提交的状态。

三类事件：

| 事件 | 触发 | 携带 |
|---|---|---|
| `work-pending` | 维护结束或本地接纳使 `hasPendingWork` 的答案发生变化 | `pending` 布尔值 |
| `trust` | 每一次提交的信任变化 | `cause`、`phase`、`entry`，变化前有行时带 `previous` |
| `inbound` | 一个条目被本节点接纳 | `thread`、`seq`、`sender` |

- `cause` 取 `local-request`（本节点发起交换）、`local-accept`（本节点接受对端邀请）、`local-revoke`（撤销）、`peer-statement`（对端陈述到达）、`rotation`（本节点轮换密钥）。
- `inbound` MUST 在每条被接纳的条目上发出，包括本地 thread 追加与经 `client.receive` 进入本地 inbox 的远端来信。它只证明接纳，不证明模型已经看到。
- 事件是提示，不是事实来源。宿主 MUST NOT 把事件当作唯一持久记录，MUST NOT 因为漏掉一个事件推断关系消失；下一次维护会收敛。
- `notify` 只携带标识、阶段和计数，MUST NOT 携带消息正文或凭据。正文经注入路径取得。

符合性场景：

- 一次 `confirm` 产生 `local-request` 事件，事件的 `phase` 与 `entry` 和存储中的行一致。
- 对端陈述到达产生 `peer-statement`；宿主漏掉该事件后，下一轮维护仍能读到 `exchanged`。
- 维护从有工作变为空闲产生 `work-pending: false`，反向变化产生 `true`。
- `notify` 自行抛错：提交仍然成功，下一次维护继续。
- 事件中读出消息正文：不符合。

## 6. 认证会话与阶段投影

接口：`trust.begin(auth, node)`、`exchange.confirm(fingerprint)`、`trust.revoke(auth, node)`、`trust.entry(node)`、`trust.list(auth)`、`trust.rotate(auth)`、`keys.describe()`。

- `begin` MUST 只读取对端实时 `/keys` 并返回展示材料：`node_id`、`fingerprint`、`public_key`、`base_url`、`confirm`。它 MUST NOT 写入信任行或发送介绍信；取消会话 MUST 无副作用。
- 宿主 MUST 在确认界面同时显示双方指纹，MUST 把显示过的对端指纹原样传给 `confirm`，MUST NOT 自动确认或跳过展示。
- `confirm` MUST 把 `begin` 观察到的指纹封存到这次确认上，并在提交前重新核对端点当前公钥。收到的指纹与展示的不同，或端点在此期间换钥，MUST 失败（`unauthenticated`），MUST NOT 落库或发送。
- 指纹一致时，库 MUST 提交本地批准、创建送达义务、发送签名介绍信并返回最新行。
- 对已 `exchanged` 的行重复调用 `confirm` MUST 只刷新时间与地址，MUST NOT 写新义务、MUST NOT 重发。对仍为单侧的 `requested` 行重复确认视为重试，写新义务并重发。宿主 MUST NOT 在界面渲染循环里调用 `confirm`。
- 本地批准与密钥轮换 MUST 创建独立的持久出站义务：把这份签名陈述送达对端，并取得对端签名确认它已持久记录该陈述。对端主动发来的任何陈述（包括 `approved=false` 与轮换陈述）MUST NOT 清除这项义务；它只证明对端自己提交的事实。
- 持久出站义务 MUST 只在两种情况下清除：收到对端对本次介绍信请求的成功响应（HTTP 2xx，其中 `approved` 可以为 `false`——对端已记录陈述但尚未批准），该响应的签名绑定本次 nonce；或本侧撤销该对端。没有响应、响应丢失、验签失败或对端时钟拒绝时义务保留，行不降级。
- `/introductions` 不带 Onlyne 签名，其错误响应因此不可认证。对端报 `forbidden` 或 `id_conflict` 时，库 MUST 让义务退避（间隔属于库设计，不得为零），MUST NOT 删除义务。退避到期后维护环重试；伪造的拒绝只能推迟送达。
- 签名请求的错误响应同样带响应签名。库 MUST 在读取错误码之前校验该签名；验不过时按 `unauthenticated` 处理，不得把未认证的错误当作对端的拒绝。
- 维护环重驱 MUST 使用新的 nonce。对端 nonce 单次消费，重放旧 nonce 必然被拒。已处于 `exchanged` 的行同样适用：批准已经成立但陈述未获确认的，继续重驱。
- 未被退避的送达义务存在时，`hasPendingWork` MUST 为真，维护环 MUST 保持活跃节奏直到义务清除。退避中的义务由空闲节奏照看，退避到期后 MUST 恢复为待办。
- 阶段 MUST 由 `trustPhase(entry)` 得出，取 `invited`、`requested`、`exchanged`、`revoked`。宿主 MUST NOT 用 `local_approved`、`remote_approved`、`state` 自行组合阶段。
- 拒绝一条 `invited` 行 MUST 使用 `revoke`，不使用本地确认。撤销清除两侧批准；之后该对端 MUST 重新获得本节点的显式批准才能交换。
- 密钥轮换 MUST 通过 `trust.rotate` 完成：宿主保存新密钥，已信任对端收到旧密钥签名的轮换陈述，对端验过后更新指纹并把旧指纹记入 `history`。
- 存在未清除的轮换义务时 `trust.rotate` MUST 返回 `conflict`，不得开始第二次轮换。旧私钥只随该义务保留，义务清除后删除。
- 宿主 MUST 用 `trust` 事件刷新界面，MUST NOT 把轮询存储当作唯一状态来源。
- 对端换钥且没有旧钥证明时，库返回 `id_conflict`。宿主 MUST 显示双方指纹与「撤销后重新确认」的步骤；MUST NOT 自动撤销或自动改钉新指纹（RFC 0017 §2）。

符合性场景：

- `begin` 显示指纹；`confirm` 传入错误指纹：失败，且没有信任行。
- 展示后对端换钥：`confirm` 失败并给出新指纹。
- 双方批准：本端进入 `exchanged`；最后一封介绍信丢失、且期间没有发生轮换时，维护环补发，无需再次人工确认（跨轮换见 RFC 0017 §2）。
- 对端已收到陈述但回执丢失：持久送达义务仍在，维护环用新 nonce 重驱；收到对端的签名确认后义务清除。
- 对端以成功响应回 `approved=false`（它已记录陈述、尚未批准）：送达义务清除，本侧停在 `requested`，不继续重驱。
- 对端主动发来 `approved=false` 的陈述（不是对本次投递的应答）：不解除本侧义务。
- 已 `exchanged` 的行在轮换陈述未获确认时继续重驱，行不降级。
- 撤销后发送被拒，撤销事件到达宿主；被撤销的对端不能靠新陈述自行恢复。
- 轮换后对端指纹正确，`history` 保留旧指纹。
- 宿主用两个批准位自行拼出阶段：不符合。

## 7. 域接口

接口：`domains.create(auth, mutation)`、`domains.join(auth, domain_id, mutation)`、`domains.leave(auth, domain_id, mutation)`、`domains.list(auth, options?)`、`domains.describe(auth, domain_id)`、`domains.discover(auth, domain_id, signal?)`。

- 宿主 MUST 用本节点主体调用 `create`、`join`、`leave`。库 MUST 对任何其他主体返回 `forbidden`；宿主 MUST NOT 代其他节点修改成员关系。
- `mutation` MUST 携带 `idempotency_key` 与 `retry_until`。`join`/`leave` 修改已有关系时 MUST 携带当前 `expected_revision`，首次加入可以省略；缺失或不匹配返回 `revision_conflict`。
- `create` MUST 生成合法的绝对 URI 并在同一事务里加入本节点。`list` MUST 至多返回一条 active 关系；存储中出现多条 active 时 MUST 返回 `conflict`，不得静默选择。`describe` MUST 返回本节点当前记录，包括 `left` 墓碑。
- `discover` MUST 只采信候选节点自己的服务描述：协议与身份一致、声明 active 且域标识相同才算成员；查询失败的节点进入 `unavailable`。库 MUST NOT 用缓存、目录清单或共同域推断成员。
- 宿主 MUST 通过 `domainCandidates` 提供候选，通过 `resolve` 提供地址。宿主 MUST NOT 从 IP、端口、主机名、cwd 或终端位置推断 region。

符合性场景：

- `create` 后 `list` 返回一条 active；再 `join` 另一个域返回 `conflict`。
- 用旧 revision 调用 `join`：返回 `revision_conflict`；同键同内容重投返回原结果，不增加版本。
- `discover` 中一个候选不可达：进入 `unavailable`，其余成员照常返回。
- 候选声明另一个域：不出现在成员列表里。
- 宿主用其他节点身份调用 `join`：返回 `forbidden`。

## 8. 等待能力

计划的 `response` 为 `await` 时，库 MUST 在发送前调用 `runtime.supports(plan)`。宿主未提供 `runtime` 或返回 `false` 时，库 MUST 返回 `unsupported_delivery`，MUST NOT 发出消息。

- 提供 `await` 的宿主 MUST 保持工具调用打开，直到答复、超时或取消；MUST NOT 改成“结束回合、等通知”；MUST 提供 `confirmToolResult`，让库在模型消费工具结果后推进消费记录。
- `async` MUST NOT 作为 `await` 的降级实现。`handle` 是另一种能力，MUST 单独协商，MUST 在同一能力名下与 `await` 分开，并提供查询结果与恢复上下文的操作。
- 库的等待者匹配 MUST 限定为：期望的认证对端、原 MessageRef、关系 `reply`。回执与 `followup` MUST NOT 解除等待。
- 超时与取消 MUST 只结束本次本地等待，MUST NOT 改写投递记录；记录仍按重投窗口由恢复流程处理。
- 同一执行上下文直接等待一条只能由自己答复的消息时，库 MUST 拒绝并说明循环依赖，且不发送。
- 等待期间，网络接收、资源事件与回执处理 MUST 继续运行。
- 合法 reply 经工具结果返回后与注入游标共用同一条消费记录，正文是否重复展开由该记录决定（§9）。

符合性场景：

- 无 `runtime.supports` 或返回 `false`：`await` 请求失败，对端没有记录。
- 对端回 `reply`：调用在等待窗口内返回 `answered`；回 `followup` 或回执不解除等待。
- 超时返回 `timed_out`，随后答复到达仍进入 inbox，出站记录保持打开到窗口结束。
- 等待本节点拥有的 thread 的答复：返回 `unsupported_delivery`，没有发出消息。
- 宿主把 `handle` 结果标成 `await`：不符合；两种能力不得混用。

## 9. 消费记录

库维护一条按 `(context, ThreadRef, seq)` 记录的消费记录，只证明指定上下文消费过该条目。自动关注片段与 `await` 工具结果共用这一条记录。

- 写入 MUST 只经 `client.confirmConsumption(entry, context?, 'fragment')` 与 `client.confirmToolResult(entry, context?)`，两者分别以 `fragment` 与 `tool_result` 记录来源。
- 宿主 MUST 在观察到模型实际消费后才确认。工具结果的提交本身 MUST NOT 推进记录。
- `await` 返回正文时，若该上下文已经确认过，库 MUST 只返回到达事实与原 MessageRef（`reply_body: 'withheld'`），不重复正文；否则返回正文并保留占用，直到确认。
- 宿主准备自动关注片段前 MUST 查询 `client.isWaitingReply(entry, context?)`。为真时 MUST 推迟片段，因为工具结果已经或即将呈现同一正文。
- 重复注入同一上下文的折叠 MUST 由接入层执行，只有接入层观察得到模型上下文。
- 恢复同一逻辑上下文时，只有该上下文的历史仍然可用才可沿用记录；创建新上下文时它 MUST 建立自己的记录，只消费明确分配给它的义务。
- 消费提交后，库 MUST 在提供 `runtime.todo` 时调用它。钩子失败 MUST 留下待重放记录；重放 MUST 安全，宿主 MUST 按条目身份去重。

符合性场景：

- `await` 返回正文后未确认：自动片段被推迟；确认后同一正文不再展开。
- 先经片段注入并确认，随后 `await` 解除：工具结果只给到达事实与原引用。
- 重启后同一 context 的记录仍在：条目不重复展开。
- 新 context 收到同一条目：正文正常展开一次，与旧 context 的记录互不影响。
- `todo` 钩子抛错：下一轮维护重放；宿主按 `(context, ThreadRef, seq)` 只建一条待办。
- 宿主在工具结果提交时就调用确认：不符合。

## 10. 对账、恢复与发现调度

- `maintain()` MUST 依次投递并确认授权通知、按 thread 顺序重放 owed todo 与未注入条目、对账关注义务。一轮中一个记录失败 MUST NOT 阻止其余记录；失败的记录保持 owed，下一轮再试。
- 对账 MUST 覆盖所有已知 owner：来自已接受授权通知的 `owner + thread`，加上本节点拥有的 thread。一个 owner 不可达 MUST NOT 阻止其余 owner 完成对账。
- ack MUST 在条目已本地持久保存之后发出。ack 幂等，MUST NOT 确认其他成员或其他条目的义务。
- `recover()` MUST 重投窗口内未结的出站记录；窗口结束后 MUST 记为 `unknown` 并附 `retry_window_expired`，MUST NOT 继续停留在 `pending`。
- 宿主 MUST 在没有来信事件时也运行维护。事件不是对账的触发条件。
- 网络发现的调度 MUST 属于宿主：库 MUST NOT 自行探测网络或定时刷新目录。宿主通过 `resolve` 与 `domainCandidates` 提供地址与候选，并可提供 `directory` 视图；宿主可以调用 `discovery.observe`、`list`、`listRecords`、`resolve`、`candidates`、`refresh` 读写发现目录。
- 发现记录是候选线索，MUST NOT 被当作成员事实或信任事实；成员以对端当前服务描述为准，信任以 §6 的会话为准。

符合性场景：

- 提示丢失但连接正常：下一轮对账读到未确认义务并补 ack。
- 一个 owner 不可达：其他 owner 仍完成对账，失败 owner 的记录保持 owed。
- 出站窗口结束仍未知：记录转 `unknown`，原因 `retry_window_expired`，不再无限重投。
- 目录中存在过期记录：不构成活跃成员，`discover` 仍以对端当前描述为准。
- 宿主用目录记录直接判定成员或信任：不符合。

## 11. 本地记录与 todo 归属

- 库 MUST NOT 保存任务状态，MUST NOT 为消息投影 complete、pending、failed、blocked，MUST NOT 读写宿主 todo。
- 库暴露的可恢复本地记录包括：出站证据（按 MessageRef 读取）、inbox 记录、消费记录、信任行与域成员记录。宿主 MUST NOT 直接改写库的存储键。
- todo 的持久化 MUST 属于宿主运行时：写什么信息、存在哪里、如何提醒由插件决定；库只提供 `runtime.todo(entry, context)` 钩子。
- 宿主 MUST 让 todo 与消息的关联可恢复，并按 `(context, ThreadRef, seq)` 去重。恢复时 MUST 先重放 owed 记录，再决定是否新建条目。
- 回复 MUST NOT 自动完成 todo，todo 完成 MUST NOT 自动发出回复。

符合性场景：

- 一条消息要求持续工作：待办出现在宿主 todo 原语中，库存储里没有任务状态。
- 重启后 owed todo 重放，不产生第二条同源条目。
- 发出 reply 后宿主 todo 仍开放，直到宿主自己决定完成。
- 宿主删除或重建库的存储键：不符合；重建节点必须沿用原存储。

## 12. 版本、证据与变更

- 本契约跟随 RFC 版本。接口增删 MUST 先改上游 RFC，再同步本文件与下游 [OMP 契约](omp.md)。
- 符合性按各节场景判定。实现覆盖与缺口记录在实施证据中，本文不声明已经通过。
- 场景本身可被独立实现复用；实现语言不改变接口语义，只改变符号拼写。
