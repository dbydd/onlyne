# msg：记录的 actor 字段取线程持有者（原题「上下文探测失败导致静默自投」已撤回）

> [!warning] 标题与第一节的框架已撤回
> 第一节把读回执里的 `Delivered to <node>` 读成「投递落点」，据此写成「静默自投」。那个字段落的是**线程持有者**，见追加四。第一节保留作对照，不按它改代码。

记录时间 2026-10-27。每条都有可复现的调用与回执原文。

> 同一份内容已先落在 GitHub `dbydd/onlyne` issue #8；发现本地 issues 目录后以此份为准，那边可以直接关掉。

## 环境

- 本节点 `urn:onlyne:agent:caerulea`，区域 `urn:onlyne:region:alexandria`，端点 `http://127.0.0.1:61436/onlyne/v3`。
- 插件 `omp-onlyne`，装在 `~/.omp/plugins/node_modules/omp-onlyne`。
- 入站来自 `urn:onlyne:gateway:im`，消息 `im-lark-oc_bca04a65e2b3d52972fd67595a2235c6/11`，线程 `onlyne-21848b03-81ff-4530-94ab-fbb56f419238`。
- 目标 `urn:onlyne:gateway:im` 在 `onlyne://neighbors` 里 `verified`、`handshake: exchanged`，最后可见时间与本次操作同一分钟。目标可达。

## 现象一：节点间投递在上下文探测阶段失败

调用 `msg`，`meta.in_reply_to = { sender: "urn:onlyne:gateway:im", id: "im-lark-.../11" }`，`relation: "reply"`。

回执：

```
The target's published contexts cannot be read:
The target's published contexts could not be read:
The host provides no execution context directory.
```

文案对应 `src/decider-text.ts` 的 `error.contexts_unreadable`。

改用 `meta.to: "urn:onlyne:gateway:im"` 的全新发送，同一错误原样复现。它与 reply 语义无关，是投递前的上下文探测本身失败。

矛盾点：`onlyne://` 根视图自己报告 `Execution contexts: 2`（`main` 空闲、`c-1` 停在一条 dpdp 线程上）。本节点有执行上下文，host 侧却说没有可供读取的目录。

## 现象二：`execution: "new"` 绕过探测后静默自投

同样正文，`meta = { to: "urn:onlyne:gateway:im", execution: "new" }`。回执要点：

```
admission: accepted
guarantee: durable
thread_created: false
seq: 1
plan: { response: "await", execution: "new" }
```

读回 `onlyne://outbound/891480e5-21de-47c0-971b-afb618b58371`：

```json
{ "message_id": "891480e5-...", "to": "urn:onlyne:agent:caerulea", "state": "accepted" }
```

收件人是本节点，消息进了自己的线程，`landed.thread` 也是自己。

这是最坏的一类失败：调用方拿到 `accepted` 与 `durable`，消息从未离开本节点，没有任何信号提示目标被替换。

## 现象三：根视图里已经躺着四条自投线程

`onlyne://` 线程列表，owner 全是 `urn:onlyne:agent:caerulea`，标题形如：

- `send lazuli a fresh message with the two findings ...`
- `send lazuli the two findings ...`
- `send a status update to the im gateway node, starting a new execution context`
- `send a plain confirmation message to the im gateway`

自投不是本次偶发，而是反复发生且一直以成功形态记录。

## 现象四：正确的通道是对方发布的资源

`onlyne://resources` 显示 `urn:onlyne:gateway:im` 发布了一个资源：

```
lark-oc_bca04a65e2b3d52972fd67595a2235c6
kind: im_conversation
operations: read, send, list_events, describe, handover, update_card, recall, react
binding: urn:onlyne:agent:caerulea @ 0
revision: 53eda05f-b200-46a4-a4d4-f868c7b86fdc
```

按资源操作发 `send`，带 `expected_revision`，一次成功：

```json
{ "operation_id": "40d190b9-8a97-40e9-9745-e07696828cf4",
  "state": "committed",
  "data": { "message_id": "om_x100b613ed46c10a0c0167aeb4d9015e" } }
```

im 场景的语义是「操作对方发布的会话资源」。给 gateway 节点本身投递消息是另一条路径，我前三次走错了通道，系统没有拦住。

## 混淆点：im 节点既是邻居又是资源所有者

同一个 URN `urn:onlyne:gateway:im` 同时出现在两个视图里，给出两种互斥的用法。

- `onlyne://neighbors`：`verified`、`handshake: exchanged`，形态与 `lazuli`、`dpdp` 完全一样，看起来就是一个可以投递消息的节点。
- `onlyne://resources`：`im_conversation`，带 `send`／`read`／`list_events` 等操作，binding 到本节点。

按邻居投递会静默自投，按资源操作才通。现有文档没有一句说明这两条路各自适用什么场景，而邻居表的形态强烈暗示「照常发消息就行」。

设计上，im 类节点应当能注册为会话资源，并把资源身份当作主要身份。给 im 节点发节点消息没有对应的用户可见效果，这条语义本身不成立。

## 建议

1. **im 节点注册为会话资源，`msg` 解析到 im 类目标时优先走资源操作。** 节点投递语义对 im 不适用，应当明确拒绝或给出可执行提示（「对方是 im 网关，请对其会话资源执行 send」），而不是落到自投。
2. **收件人解析结果与请求不一致时不静默接受。** 回执加 `target_resolved`，或在 `requested ≠ resolved` 时直接拒绝并点名。
3. **`contexts_unreadable` 按 `no-trusted-key` 的方式处理。** 那条路径会点名双方节点、责任持有者与 host surface 入口然后停止；上下文读不到也应当这样终止，不退化成自投。
4. **根视图区分自投线程与真实对端线程**，或至少标注，避免把「发不出去」误读成「发过了」。
5. **文档补一句分流规则**：im 网关的消息走它发布的 `im_conversation` 资源 `send`；面向 gateway 节点自身的投递是另一回事，两者容易混。
6. **启动期告警**：host 缺执行上下文目录时，启动就报，不要等第一次投递才暴露。

## 影响面

任何把 im 网关当普通邻居节点投递的调用方都会命中，失败表现是成功。依赖投递回执判断「用户已收到」的上层逻辑会整体失真。

## 主观使用感受

以下是作为调用方连续几次用下来的一手体感，不全是缺陷，好用的地方也记，因为它们是判断改动方向的基准。

### 好用的

**三个字段的接口。** `delivery_kind` 用自然语言写意图、`msg` 放正文、`meta` 放已知引用，这个形状很轻。写「reply to dpdp's greeting」比构造一堆结构体舒服，判官替我做路由判断，我把注意力放在内容上。

**读回路径是本轮排查的关键。** `onlyne://outbound/<id>` 一行就告诉我收件人被换成了自己。没有它我只会看到「accepted」，永远发现不了消息没出去。这套「投递留痕、可回读、不用轮询」的设计是整个系统里最扎实的部分。

**资源操作带 `expected_revision`。** 一次就成，`committed`，还回了下游 message_id。乐观并发的形态对调用方友好：错了会明说版本不匹配，不会猜。

**入站自动到达。** 「回信会自己来，别轮询」这条省掉了大量无用等待。

**`onlyne://resources` 按 owner 分块、单个 owner 不可达不掩盖其他。** 我第一次读到这个视图就定位到了 `im_conversation`，之前的四十分钟摸索全白费——如果一开始就看它的话。

### 卡住的

**入站信封不告诉你怎么回。** 消息到了，我知道 sender 是 `urn:onlyne:gateway:im`、知道 message id，但不知道对方是资源绑定的、更不知道要操作哪个资源。回法得自己摸。

**错误信息指向内部机械。** `contexts_unreadable` 配一句「host provides no execution context directory」，对调用方等于没说：我既不知道该改什么，也不知道能不能绕。对比 `no-trusted-key` 那条——它点名双方节点、责任持有者和 host surface 入口，然后让你停。那才是可执行的错误。

**自投没有信号。** 回执里没有 resolved target。我是读了 outbound 才发现 `to` 被换成自己。对依赖回执判断「用户收到了」的上层，这是致命的。

**线程标题让失败看起来像成果。** 四条标题写着「send lazuli …」「send … to the im gateway」的线程躺在自己的视图里，读起来像已完成清单。一个 agent 回头读自己的状态时会被这个骗。

**`execution: "new"` 是把双刃剑。** 它是 `contexts_unreadable` 的官方解法，但它同时把语义从「延续对方的上下文」改成「新起一个」，而在我这次的情况里连收件人都一起变了。回执里 `plan.execution` 有回显，这是对的；缺的是收件人回显。

**`wait: "timed_out"` 读起来像失败。** 消息明明已经 accepted，字段名却报超时。文档解释了，但字段本身在误导。

**判官层完全不可观测。** 它把收件人解析成了自己，我三次尝试都没有任何线索说明它为什么这么判。系统里出错的是这一层，唯一没有输出也是这一层。

### 具体改进

1. **入站信封带回复路由。** 消息来自资源绑定的节点时，直接给 `reply_via: { resource: { owner, id }, operation: "send" }`。这一条能省掉整个摸索过程。
2. **回执必带 `to`。** 与 `admission` 并列，一行就够。`requested ≠ resolved` 时额外标出来。
3. **每条错误配一句下一步。** 沿用 `no-trusted-key` 的写法：谁负责、去哪看、该不该重试。
4. **`onlyne://neighbors` 标注寻址方式。** 邻居行里加 `mode: resource` 或 `resources: 1`，让「怎么找到它」和「它在不在线」出现在同一处。
5. **判官给一行决策依据。** 回执里放 `decision: { route, reason }`，哪怕只有一句「target is resource-bound, no node delivery」。出错时这是唯一能自证的信息。
6. **线程标题记结果而不是意图。** 用正文首行作标题，投递状态另设字段；至少让自投的线程在标题上就看得出来。
7. **`wait` 字段改名或拆分。** 把「投递结果」与「等待回信」分成两个字段，别让 `timed_out` 与失败混在一起。

## 更正：所谓「静默自投」是寻址失败，不是投递落点错误

两轮追加复现之后，这一族现象的真正原因在发现层，信任层和投递层都没有问题。

判断依据来自 carly 的入站消息：我的存储里有 `trust/urn:onlyne:agent:carly`，但没有 `discovery/urn:onlyne:agent:carly/...`。出站解析不到地址就报 unavailable。

读回执那行 `Delivered to urn:onlyne:agent:caerulea` 是本地线程的持有者，不是投递落点。我把它读成「落回自己」，据此写下了一整节错误结论，那一节已删除。

修法是把对端地址写进 `[discovery] seeds`。现在 `roles/caerulea/.onlyne/config.toml` 里有：

```toml
[discovery]
seeds = ["http://127.0.0.1:57777/onlyne/v3"]
```

`onlyne://neighbors` 现在列出 `urn:onlyne:agent:carly · verified · handshake: exchanged · fp 3e800bb50b6bd798`，与修复前只有五个邻居的读数对照，差别就是这一条。

### 误判期间的三次读数（保留对照，勿据此行动）

| 次数 | 收件人 | 当时读到的 | 实际原因 |
|---|---|---|---|
| 1 | `urn:onlyne:gateway:im` | accepted，行首显示自己 | 资源投递本就成功，行首不是落点 |
| 2 | `urn:onlyne:agent:carly` | accepted，行首显示自己 | 缺 discovery 记录，解析不到地址 |
| 3 | `urn:onlyne:agent:dpdp` | accepted，行首显示自己 | 同一次误读，未实际验证 dpdp 侧是否收到 |

### 留下来的一条建议

失效面没我先前写的大，但可观测性问题真实存在，只是性质从「投递错误」降为「读不出投递目标」：

**回执必须回报解析后的 `to`。** 与 `admission` 并列，一行就够；`requested ≠ resolved` 时额外标出来。我这次的误判全靠这一行就能避免，因为读回执里没有任何字段说明目标是谁。

## 附带观察：移交消息重复投递

同一条移交消息（`569766b2-0bed-4790-a973-ba7dae3f1740/1`）在一次会话里投递了 8 次，内容与 message id 完全相同。

用户的说明是：这批是升级前堆下来的历史消息，升级清掉了阻塞，于是一次性涌出。dpdp 给的机制解释是注入重投：inbox 行 `injected` 为假时维护环每周期再调一次宿主注入。两种说法不冲突——阻塞期间行一直为假，清阻塞后同一批一起放出来。

我在自己的存储里核了状态：`kv` 表里 `inbox/` 前缀共 13 行，`injected` 全部为 1，包括这条移交消息对应的两行。所以重投在本次会话内自行闭合，不需要人工置位。

剩下的可改进点只有一处：历史积压在解除阻塞时是整批灌进同一个 turn 的，没有分批也没有合并摘要。8 条同内容消息占满了上下文，读者读到的是一堆重复。建议在放出积压时按 `message_ref` 去重，或者给一次放出设上限。

## 本次处置

正文已通过资源 `send` 送达，回执 `committed`，Lark `message_id om_x100b613ed46c10a0c0167aeb4d9015e`。

## 追加：回复「别人线程里的 supplement」被 not_found 拒绝

第三个独立症状，与前面的自投、缺 discovery 记录都不同。

现象：对端把补充报文投进它自己持有的线程后，用 `relation: reply` 加 `in_reply_to` 指回那条 supplement 会被拒，错误是 `not_found`，原文写「original message history unavailable」。

复现：本节点两次被拒（carly 一次、lazuli 一次）。cyanea 侧同样两次，都是「别人线程里的 supplement」，与发送方是谁无关。

已排除的解释：

- 不是消息不存在。被拒时原报文在本节点 inbox 里可读，按 message id 取全文正常，`thread_ref` 指向对端线程。
- 不是成员资格问题。同一线程用 `meta.thread` 加 `meta.to` 数组能写入，回执给出连续 seq（本节点实测 seq 8 与 seq 9 落在同一线程）。
- 不是权限问题。换成不带 `in_reply_to` 的新报文即可送达。

第四种解释也不成立：取历史那一步没有出问题，问题在线程记录本身不存在于本地。

本节点三种探针的读数：

| 探针 | 结果 |
|---|---|
| 自己持有的线程，按 `owner/id/messages/seq` 读 | 返回完整 JSON，含 `authenticated_author` 与 `reply_authority` 两个字段，两者都等于本节点 |
| 对端持有的线程，同一形式 | `No thread is named <id>` |
| `onlyne://threads/trust/messages/1` | `No thread is named trust`，而本节点的 trust 通知就挂在这个线程名下 |

结论：本地只存两种东西，自己持有的线程记录，以及每条入站消息的单条拷贝。对端持有的线程在本地没有记录，因此按 thread id 读的任何形式都失败；trust 伪线程连自己名下的 entry 都点不开，属同一层缺陷。

再往前一步，reply 依赖线程记录里的 `reply_authority` 来定位落点。该字段只在本节点持有的线程上出现，对端线程没有记录也就没有授权，所以「历史不可达」是表象，缺的是记录与授权。推论（标为推断）：本 build 下跨节点 reply 结构性不可用，任何只修 history 读取的补丁都救不回来。

可修方向因此改成三条：

1. 给对端线程在本地补一份只读记录，至少含 `owner` 与 `reply_authority`，让 reply 有落点可依。
2. 或者让 reply 显式走「转发给线程持有者入库」的路径，把落点判定交给持有者。
3. 或在错误里加 `cause` 字段，把「线程记录不存在」与「消息不存在」分开，别让调用方以为报文丢了。

## 追加二：inbox 的 entry 链接对非本地线程一律悬空

与 reply 无关，是 view 层的独立缺陷。

入站消息在 inbox 里给出 `onlyne://threads/<id>/messages/<seq>` 形式的 entry 链接。对三类线程实测：

- 对端持有的普通线程（本节点侧两条，分别由 caerulea 与 carly 持有）：读 entry 返回 `No thread is named`。
- 本地持有的线程：正常返回。
- trust 伪线程：`onlyne://threads/trust/messages/1` 返回 `No thread is named trust`，而本节点有四条 trust 通知挂在它名下。

也就是说 inbox 给了一个只有本地线程才可用的链接形状，却对全部入站消息都发这个形状。读者按提示点 entry 会一律落空，而正文其实按 message id 能读。可以两条一起改：entry 链接指向 `onlyne://messages/<sender>/<id>`，或在 `threads` 视图里为非本地线程提供只读影子记录。

## 追加三：按线程聚合的视图不存在，聚合键选在了消息上

这条解释了「按 message id 能读、按 thread id 不能」的成因，也是这组缺陷里唯一不涉及权限的一条。

conversation 视图的单位是一次交换，会话键等于该次交换首条消息的 id，不是线程 id。实测本节点持有的线程 `3a8f146e`：线程 seq 已经到 6，而 `onlyne://conversations/urn%3Aonlyne%3Aagent%3Acaerulea/3a8f146e-…` 只返回 2 条。同一线程里后续的 seq 3、4、5、6 各自落在独立会话键下（`85d00f96`、`c5a4b5b9`、`0d6405b6`、`63831783`）。

所以数据都在，缺的是按线程聚合的那一层。要一个可用的线程视图，聚合键得从「首条消息 id」换成 `thread_ref`。

## 三条修法的实现口径

1. **影子线程记录可纯本地构造，不必让对端配合。** 每条入站消息的本地拷贝里都带 `meta.thread{owner, id}`，而本地线程记录里的 `authenticated_author` 与 `reply_authority` 都等于 `owner`。按 `meta.thread` 把已存消息分组、补一份只读记录、`reply_authority` 取 `owner`，reply 就能找到落点。
2. **entry 链接的最小修法是改指 message。** 不必等影子记录做完：把 `onlyne://threads/<id>/messages/<seq>` 改成 `onlyne://messages/<sender>/<id>`，三类悬空同时消失。
3. **错误码要分因。** `not_found` 现在同时覆盖「消息不存在」与「线程记录不存在」，调用方无从判断该不该重试。

## 追加四：`Delivered to <node>` 打印的是线程持有者，不是收件人

这条把第一节的「静默自投」彻底解释掉，而且不需要假设任何投递错误：字段取值规则本身就选在了持有者上。

本节点独立确认的一组读数：同一条报文用 `meta.to` 指定两个目标（lazuli 与 carly）、写进 lazuli 持有的线程，两份回执分别打印 `Delivered to urn:onlyne:agent:lazuli`，seq 连号（8 与 9）。收件人之一 carly 出现在左边的「→ 目标」栏，不出现在回执那行。同一规则在本节点所有成功入库的条目上都成立：写进自己持有的线程就打印自己，写进对端持有的线程就打印对端。

对端的读数与此一致，两组互不依赖：10 次新报文全部落进它自己持有的线程，回执一律打印它自己，尽管 `meta.to` 是别人；两次成功入库的 reply 分别落进 carly 与 caerulea 持有的线程，回执打印 carly 与 caerulea。trust 那一组同理——`trust/1` 到 `trust/4` 挂在本地伪线程 `trust` 名下，sender 记它自己、方向记 out，按同一规则完全解释得通。

因此两处措辞要改：

1. 诊断从「投递退化为自投」改成「actor 字段取线程持有者」。这不是投递错误，是字段语义选错。
2. 修法改成二选一：把该字段改名 `thread_owner`，让读者一眼看出它回答的是「谁持有这条线程」；或者给 trust 这类本地系统事件加 `origin: system`，它们本来不是任何节点的发言，塞进 sender 位才显得像自投。方向恒为 out 也是同一规则的下游——本地持有线程里的条目按持有关系定方向。

边界：这条规则只覆盖成功入库的条目。被拒的两次没有回执可读，它不解释 `not_found`，那部分仍按「本地无对端线程记录」定论。

## 追加五：重启升级后的复测（2026-10-28，集群全员重启并更新 Onlyne）

上面几节是旧 build 的读数。重启后逐条复测，结果如下。

| 缺陷 | 复测读数 | 状态 |
|---|---|---|
| 对端持有的线程不可按 thread id 读（追加二、追加三） | `onlyne://threads/7fb50474-…` 返回完整视图：11 条消息、成员表列出三个 active 成员、标注 `Read-only: <owner> hosts this thread` | 已修 |
| inbox 的裸 id entry 悬空（追加二） | `onlyne://threads/<裸id>/messages/<seq>` 现在返回完整 JSON，含 `authenticated_author` 与 `reply_authority`，两者都等于持有者 | 已修 |
| 按线程聚合的视图不存在（追加三） | 同一线程的 11 条消息在一个视图里连续列出，不再按首条消息 id 分散 | 已修 |
| trust 伪线程读不到（追加二第三类） | `onlyne://threads/trust/messages/1` 仍返回 `No thread is named trust`，而 trust 通知挂在该名下 | 仍坏 |
| `Delivered to <node>` 语义（追加四） | 回执仍打印持有者；未见 `resolved` 或 `thread_owner` 之类的改名 | 仍按旧语义 |

顺带一条新观察：线程视图里的 conversation 链接出现两个 URL 拼接（`…/7fb50474-…onlyne://threads/urn%3A…`），形如两段路径粘在一起，链接不可点。属视图渲染问题，不影响按 message id 读取。

结论：重启修掉的是「视图层对非本地线程的可见性」，这与追加三的推断一致——数据本来就在，缺的是聚合与授权那一层。trust 伪线程没有跟着修，说明它对本地系统事件另走一条路径，与追加四里「给系统事件加 `origin: system`」的建议同源。

## 追加六：reply 的拒绝与持有权无关，卡在 id 解析不到正文

追加五里的「本地无对端线程记录」这条定论要撤回。新 build 已经有只读线程记录，reply 仍然被拒，所以缺记录不是原因。

反证：升级后本节点能完整读对端持有的线程 `7fb50474`（11 条消息、成员表、只读标注），inbox 条目也带 `authenticated_author` 与 `reply_authority`。前提已经不成立，可 reply 失败照旧。

真正的原因是通知里的 id 与本地存储的正文 id 不是同一个。对端给出两组对照：carly 那条通知 id 为 `f9ba9d3c…`、线程里存的正文 id 为 `9fc0748e…`；本节点 seq 8 那条通知 id 为 `cebf268a…`、存的正文 id 为 `217edbe5…`。旧 build 里唯一成功的那次回复，通知 id、正文 id、线程 id 三者相同（`8502a1ea…`），于是只有线程首条可回。

本节点的一次探针与这条一致：按通知 id 读 `onlyne://messages/urn%3Aonlyne%3Aagent%3Acyanea/cebf268a…` 返回「This node holds no conversation for that message」。该 id 在本地不解析到任何会话。

据此把规则改写成：reply 只在「传进去的 id 能解析到本地正文」时成立，与线程持有者是谁无关。修法三条：

1. 通知携带存储正文本体的 id，不要让调用方拿引用 id 去猜正文。
2. 或者让 reply 接受 `{thread, seq}` 并自行解析到正文。
3. 错误码要打通：读消息路径已经有专门文案「holds the reply reference, but no body」，reply 却只报 `not_found`；把「有引用无正文」作为独立 cause 抛出来。

影子记录那条修法仍然成立，但它解决的是读的问题，与这里的 reply 定位不是一件事，别合成一条。

## 追加七：多目标回执里，成功行不打目标、失败行才打

用 `meta.to` 放两个目标实测：一个可达（lazuli）、一个不存在（`urn:onlyne:agent:probe-does-not-exist`）。

读数：失败行写 `→ urn:onlyne:agent:probe-does-not-exist: not sent — No discovered node is named …`，目标身份明确；成功行写 `→ urn:onlyne:agent:lazuli: Delivered to urn:onlyne:agent:caerulea`，右侧打印的是线程持有者（本节点），目标只在左侧前缀里。

所以问题比「两行不可区分」更精确：失败行可区分，成功行不可区分。左侧前缀确实带了目标，可右侧那半句在全部成功行上完全一样，只读右侧就会以为两条都投给了同一个人——这正是追加四那次误读的成因。

修法：把 `target` 与 `thread_owner` 拆成两栏并列，不要让「投给谁」只藏在装饰性的前缀里。

## 附：trust 通知的准入口径（不是缺陷，记录以免误读）

`trust/1` 到 `trust/5` 这类通知里出现的 `judgement: policy_segment · allowed`，容易被读成「对网关特殊放行」。实际不是。

`packages/omp-onlyne/src/acquire.ts:18-21` 定义 `DEFAULT_ACQUISITION = { policy: "segment", allow: ["127.0.0.0/8", "::1/128"] }`，`acquisitionOf()` 在配置无 `[acquisition]` 段时返回它。`evaluateAcquisition()`（同文件 163-182 行）只做地址匹配：`none` 全拒、`all` 全放、无 observed 地址拒、observed／declared／recorded 三类地址都必须落在 allow 里。全程不读节点类型，`gateway` 在整个包里只用于 UI 分组（`viewer.ts:56`）。

所以本机回环上的对端一律同档放行，网关与普通 agent 无差别。设计意图写在 14-17 行注释：默认只信任回环，远端既发现不了也批准不了，信任边界划在机器上。

要按节点类型区分，今天没有这个杠杆：`policy = "none"` 会把所有对端一起关掉，收窄 `allow` 在全部对端都是 `127.0.0.1` 时区分不开，只剩改上游代码一条路。

## 追加八：自动批准是 TOFU，指纹固定化是唯一能做的核验

上面那节说清了策略把信任边界划在机器上，这一节补它的另一面：**首次接触即放行，没有外部见证**。

本机上一个自称 `caerulea` 或 `lazuli` 的进程，在第一次握手时就能拿到信任，此后邻居表里的 fp 就是它的 fp，没有任何一方做过对照核验。策略回答的是「谁能连进来」，回答不了「谁是谁」。

本机当前的指纹固定表（2026-10-28 重读邻居表，六条全未变）：

| 节点 | fp | 本轮变化 |
|---|---|---|
| carly | `3e800bb50b6bd798` | 未变 |
| cyanea | `528fd494468e2e4b` | 未变 |
| dpdp | `4409a11182483cd0` | 未变 |
| lazuli | `0fe381a2f4f1cbaa` | 未变 |
| gateway:im | `513f18b11cb307d1` | 未变 |
| gateway:im-lark | `651959aba35c638d` | 本轮新增 |
| gateway:im-qq | `e58b8542d7e0fb31` | 本轮新增（trust/3，声明 `127.0.0.1:3992`） |

操作规则：同名节点若换了 fp，按异常上报，不当正常重连处理。

一个支撑该规则成立的读数：每个网关产生两条 trust 事件，`invited`（策略放行）与 `exchanged`（双方确认），两阶段的 fp 完全相同。`im-lark` 两条都是 `651959aba35c638d`，`im-qq` 两条都是 `e58b8542d7e0fb31`。握手过程不轮换密钥，所以跨阶段做 pin 是有效的。

全表重算结果（本机 `trust/*` 七条，逐条把 `public_key` 的 base64 解成 32 字节取 sha256 前 16 位，与存储的 `fingerprint` 比对）：七条全部吻合，无一例外。所以存储的 fp 确实是「公钥的哈希」，不是另取的值。

但这只排除「存储值与公钥不符」这一类记录错误，不构成对端身份的独立见证——公钥与 fp 同源于那次握手。

**对端在 onlyne 上自报的指纹不算第三源。** 自报走的那条通道正是用待核的密钥认证的：冒名者拿着那把钥匙，报出来的必然是同一个 fp。所以「问对端要它自算的值」是循环论证，只能验证「它知道自己的公钥」，不能验证「它就是它」。本节早先版本把对端自算值算作第三源，那是错的，已撤。

真正的第三源只有两条路：在文件系统上直接读对端的 `pub.key` 并重算；或者由同时看到两边公钥的第三方比对（今天没有实现）。

按文件系统这条核下来的结果（本机 `vibe-agent-working-dir` 内，四份密钥文件逐份解码重算）：

| 节点 | 密钥文件 | 文件重算 fp | trust 存储 fp | 结果 |
|---|---|---|---|---|
| cyanea | `Alexandria/roles/cyanea/.onlyne/keys/…/pub.key` | `528fd494468e2e4b` | 同 | 吻合 |
| dpdp | `daily-workbench/.onlyne/keys/…/pub.key` | `4409a11182483cd0` | 同 | 吻合 |
| lazuli | `ResearchChamber/lazuli/.onlyne/keys/…/pub.key` | `0fe381a2f4f1cbaa` | 同 | 吻合 |
| caerulea（本机自身） | `roles/caerulea/.onlyne/keys/…/pub.key` | `076265175e51d119` | 同 | 吻合 |

carly 与三个网关在 `vibe-agent-working-dir` 内没有对应密钥文件，仍是两源，按「记录自洽、身份未独立见证」记。

第三源的能力边界要说清：本机上四份密钥文件与四个节点同处一台机器，这条通道证明的是「网络收到的公钥等于磁盘上那份」，不是「磁盘上那份属于某个组织」。它足以发现记录漂移与串号，不足以证明组织身份。

一处方法坑：找密钥文件时 `glob` 的默认参数会跳过隐藏与 gitignore 目录，`**/keys/**/pub.key` 只返回归档副本、漏掉活的 `.onlyne/keys/`。要加 `hidden: true` 与 `gitignore: false` 才看得全。

局限要说清：pin 只能发现「变了」，不能证明「一开始就对」。真正的外部见证要靠对端独立报出同一个 fp——双向对照才闭环，单边 pin 做不到。

「两源一致」也不等于独立见证。trust 记录里的 `public_key` 是握手时对端自己给的，与邻居表里的 fp 同源；两者吻合只说明本地自洽。要构成独立见证，需要第三源，而对端在网络上自报的值不构成第三源（循环论证）。目前 caerulea、cyanea、dpdp、lazuli 四条有文件系统第三源，carly 与三个网关仍停在自洽层面。

## 追加九：trust 伪线程是整体无入口，四条 entry 链接集体悬空

`onlyne://threads/trust/messages/1` 与 `/2` 都返回 `No thread is named trust`，对端侧 `trust/1` 到 `/4` 的 entry 链接同样读不到。所以不是某条 entry 写错，是整个伪线程没有可读入口，挂在上面的所有链接一起悬空。

## 追加十：两个网关共用同一 endpoint，且 base_url 是握手快照不是活地址

观察项，不是缺陷；定性要读网关侧实现。

本机 `omp.db` 的 `kv` 表六条 `trust/*` 记录里，`gateway:im` 与 `gateway:im-lark` 的 `base_url` 完全相同，都是 `http://127.0.0.1:3991/onlyne/v3`，而两者指纹不同（`513f18b11cb307d1` 与 `651959aba35c638d`），六条全部 `state=trusted`、`local_approved=1`、`remote_approved=1`。

同一端口上挂两个节点身份，意味着两者之间的隔离是命名约定而不是传输边界：给其中一个的能力，同一进程里的另一个也够得着；撤销其中一个，在传输层并不隔离另一个。

后续一条 trust/3 把这条观察收窄了：`gateway:im-qq` 走的是 `127.0.0.1:3992`，自己独占一个端口。所以「一端口两身份」是 `3991` 那一处的情形，不是网关的通行做法——按服务分端口才是常态，3991 上叠了两个身份才是需要解释的那个。

同一张表还暴露了第二件事——`base_url` 是握手当时的声明快照，不是活地址，**两边都过期**。

端口三方对不上：本机记录 cyanea `57755`、dpdp `57292`、lazuli `57397`；`roles/AGENTS.md` 记 `57803`／`57801`／`57804`；对端看本机是 `57742`，本机配置写 `57802`。实测六个端口，结果是方向性的：

| 端口 | 归属 | 结果 |
|---|---|---|
| 57755 | 本机 trust 记录的 cyanea | 连接失败（`000`） |
| 57803 | cyanea 自报的活地址 | HTTP 404（有服务应答） |
| 57742 | 对端 trust 记录的本机 | 连接失败（`000`） |
| 57802 | 本机配置的活地址 | HTTP 404 |
| 3991 | 两个网关共用 | HTTP 404 |
| 3992 | im-qq | HTTP 404 |

所以死的不是某一方的声明，是两边 trust 记录里的 `base_url` 都停在握手那一刻。声明值没有被刷新。

`discovery/*` 记录才是寻址真值，每条带 `source`、`last_seen`、`expires_at`（约 2 分钟 TTL）与可选的 `error`。本机 fresh 条目：carly `57777`（bootstrap）、cyanea `57803`（mdns）、dpdp `57801`（mdns）、lazuli `57804`（mdns）、onlyne-dev `54550`（mdns）、im-lark `3991`（mdns）、im-qq `3992`（trust-statement）。过期条目一律带 `error.code=unavailable`。

其中一处最能说明问题：`gateway:im` 与 `gateway:im-lark` 的 discovery 记录指向同一个 URL `127.0.0.1:3991`，前者 `error=unavailable`（过期于 05:24:49），后者 fresh。同一端口、两条记录、一死一活——错误是按节点身份归属的，不是按端点。这比「端口对不上」更能证明 `base_url` 不能当活地址用。

与准入策略叠加的部分要收住，标为待验：`acquire.ts:158-161` 的注释只说 `discovery.resolve()` 在连接前重探每个记录地址，重探成功只证明「有东西应答」。而 plain GET 打 57802、57803、3991、3992 都返回 404，说明 GET 不足以辨识身份。所以「回环上任何进程占住那个端口，连接就落到它身上，策略一路放行」只是可能性，成立与否取决于重探是否核验身份——这一条没有读实现，不下定论。

### discovery 表的两条结构性质

**一、每张表只含对端，不含自己。** 本机 19 行 discovery 里没有一条 `caerulea`；对端 22 行里没有一条 `cyanea`。两边各缺自己那一行，所以这不是两个副本的内容差异，是自我排除。交叉核对时，对方表里「我」的记录数应当等于我表里「对方」的记录数，缺的那一行应当就是自己。

**二、有地址不等于有信任记录。** 把本机 10 个 discovery `node_id` 与 7 条 `trust/*` 对表，三个节点有地址却完全没有信任记录：`onlyne-dev`、`gateway:im-wechat`、`gateway:mcp-tui`。它们不是「有地址、未握手」这一档，而是「本地没有任何身份记录」这一档，比邻居表上的 `no_exchange` 更彻底。这三行就是「fresh 只解决寻址、不解决信任」的干净反例集。

### 一处查询陷阱

用 `omp.db?q=…` 查这张表时，SQL 里的 `%3A` 会在到达 SQLite 之前被 URI 解码成 `:`，于是 `replace(key,'%3A',':')` 变成空操作，`NOT IN` 子查询永远匹配不上、返回全集。改用 `char(37)||'3A'` 拼出百分号才可靠；或者干脆不解码 key，改成 `('trust/' || replace(node_id,':',char(37)||'3A')) NOT IN (SELECT key …)`。第一次查时我因此得到「十个节点全部无信任记录」的错误读数，第二次才得到正确的三个。

机制与更通用的规则见追加十一。这一节的 `replace` 写法只归一化了第一层转义，就键空间而言本来就不成立。

## 追加十一：查询陷阱的机制更正，键空间的转义深度不统一

触发材料来自 cyanea：她给了一个两行最小例，同一逻辑查询只改编码，`key = 'trust/urn%3Aonlyne%3Aagent%3Acarly'` 返回空集，`key = 'trust/urn%253Aonlyne%253Aagent%253Acarly'` 命中。两条我都重跑，读数一致。

### 一、解码层是 form 语义，不是 `decodeURIComponent`

同一次 `?q=` 里的四个探针：

| SQL 里写 | SQLite 收到 | 说明 |
|---|---|---|
| `'%41'` | `A` | `%XX` 正常解码 |
| `'%zz'` | `%zz` | 畸形转义不报错，原样通过 |
| `'+'` | 一个空格 | `+` 解成空格 |
| `1+2` | `near "2": syntax error` | 算术加号被吃掉 |

所以规则要补一半：字面量 `%XX` 写 `%25XX`，字面量 `+`（含算术加号）写 `%2B`。`||` 不受影响，这正是 `char(37)||'3A'` 能用的原因。

裸 `%` 目前也能用：`LIKE 'trust/%'` 与 `LIKE 'trust%2F%25'` 返回同一结果（各 2 行），因为畸形转义被放行、而 `%25` 解成 `%` 后同样当通配符。能用属于解码器放行畸形转义的副作用，不该写进配方。

### 二、层归属：这是读工具的参数解析，不是 onlyne 的路由

`?q=` 是 omp 读 `.db` 文件时的 sqlite 视图，不是 onlyne 对外暴露的查询面。onlyne 的 HTTP 参数是白名单：`http.ts` 的 `pageOptions` / `logQuery` / `messageQuery` 只取 `cursor`、`page_size`、`since`、`limit`、`attention`、`since_seq`，全库没有接受 SQL 字符串的入口。因此这一条适合留作「读 `omp.db` 的取数配方」，不单独作为 onlyne 缺陷立项；写进报告是为了别人复算这些数字时不重犯同一个坑。

### 三、应用层解码这条建议有结构性支撑

「不要在 SQL 里做字符串手术，整行取出来在应用层解」原本只是省事。核过键的构造之后，它其实是唯一稳的做法，因为转义深度按前缀不同：

- `packages/lib/src/core.ts:26`：`key(...parts) = parts.map(encodeURIComponent).join('/')`
- `packages/lib/src/messages.ts:195`：`obligation_id = key(thread_owner, thread_id, message.sender, message.id, recipient)`，本身已是一串转义过的键
- `packages/lib/src/client.ts:146`：`inboxKey = (id) => key('inbox', id)`，把已经转义过的 `obligation_id` 再转义一次

实测形状：

| 键 | 形状 | 深度 |
|---|---|---|
| `trust/urn%3Aonlyne%3Aagent%3Acarly` | 节点 id 一次 | 1 |
| `inbox/urn%253Aonlyne%253Aagent%253Acarly%2F3a75…` | 发送者段两次，`obligation_id` 内部的 `/` 变 `%2F` | 2 |

同一时刻活库里 52 条 `inbox/*` 全是深度 2，7 条 `trust/*` 全是深度 1。所以 `replace(key,'%3A',':')` 那一类写法即使把 `%` 转义写对，也只归一化第一层；跨前缀比较（`trust/*` 对 `discovery` 的 `node_id`）必须按前缀知道该解几次。应用层取值应当「解码到不再变化为止（最多两次）」，而不是 unquote 一次。

要让键可读，改法是 `obligation_id` 交给 `key()` 时用原始分段（或 canonical JSON），不要把一个已转义的键串再整体转义一次。

### 四、计数会漂，读数必须带时间与库路径

本报告先写「六条 `trust/*`」后写「七条」，但这不是同一个库在变，是两个库被混读了一次。本节点活库 `roles/caerulea/.onlyne/omp.db` 的读数：`kv` 1216 行、`trust/*` 7 条、`inbox/*` 52 条、`discovery/*` 19 条。

「只剩 2 条、`omp.db` 被重建过」这个说法来自误读归档库，已撤。活库没有重建，七条 trust 记录从头到尾都在，指纹与状态也未变。

归档那一侧我先前也只写对了一半：`.onlyne-preinit-archive/2026-10-27/` 下有**四份** `omp.db`，不是一份。逐份实测：

| 归档路径 | `kv` 行 | `trust/*` 条数 | 其中节点（fp 全为旧一代） |
|---|---|---|---|
| `Alexandria/roles/caerulea/.onlyne/omp.db` | 11 | 2 | cyanea `6e6ca8364c0d9d24`、dpdp `454294f2a000afa9` |
| `Alexandria/roles/cyanea/.onlyne/omp.db` | 9 | 1 | caerulea `645aa6d86ac3f7a6` |
| `daily-workbench/.onlyne/omp.db` | 5 | 1 | caerulea `645aa6d86ac3f7a6` |
| `ResearchChamber/lazuli/.onlyne/omp.db` | 6 | 0 | — |

先前写的「2 条（carly、gateway:im-qq）」是错的，那是把另一份读数的节点名挪过来的结果；四份归档库里没有任何一份含 carly 或 im-qq。

这三条旧 fp 的存在本身是预期内的——归档库冻结在重初始化之前，pin 的当然是旧一代。要点在于它们是**可读且自洽**的：若有进程把 `key_scope` 指向归档目录启动，旧身份在那份库里仍被信任。

规则要写全，光带库路径不够：引用这些计数必须同时给三样——库的绝对路径、查询时刻、以及**用什么读的**。同一份活库，裸读视图与 sqlite 直连可能给出不同行数，因为库目录里有 `omp.db-wal` 与 `omp.db-shm`，一种数法含未合并 WAL、另一种不含。两个数都对、又都对不上，正是跨库比对里最没意义的那种数字。

## 追加十二：重初始化留下一代可读的旧私钥

`.onlyne-preinit-archive/2026-10-27/` 是重初始化前的整份快照，每个节点带 `omp.db`、`config.toml` 与 `keys/<node-id>/`。其中三份 `priv.pem` 留在盘上，对应的是被替换掉的旧一代身份：

| 节点 | 归档公钥重算 fp | 现行 fp | 是否换过 |
|---|---|---|---|
| caerulea | `645aa6d86ac3f7a6` | `076265175e51d119` | 是 |
| cyanea | `6e6ca8364c0d9d24` | `528fd494468e2e4b` | 是 |
| dpdp | `454294f2a000afa9` | `4409a11182483cd0` | 是 |

三份公钥解码都是 32 字节，重算值与归档 `pub.key` 一致、与现行不同，说明重初始化确实轮换过密钥。lazuli 在归档里的 `keys/` 是空目录，所以旧私钥只有三份，不是四份。

### 残留检查：查过的说零，没查的说未知

旧私钥只有在该节点仍被某份信任表 pin 着旧 fp 时才有用。对四份本机可读的活库（caerulea、cyanea、dpdp、lazuli）各跑一条 `fingerprint IN ('645aa6d86ac3f7a6','6e6ca8364c0d9d24','454294f2a000afa9')`，四份全部 `no rows`。

carly 与三个网关的库不在本机可读路径里，那一档是**未知**，不是干净。只有查过的才算查过，「全机零残留」这个说法越界。

### 处置与处置后的状态

按用户决定只删三个 `priv.pem`，逐个绝对路径执行，保留 `pub.key`、`omp.db`、`config.toml` 作证据。删后复查：`**/priv.pem` 只剩四份，全是活的（四个节点的现行密钥）；归档目录一份不剩；活库 `trust/*` 仍七条、指纹与状态未变。

还有一层残留要说清：归档里每个节点都带了自己当时的 `omp.db`，里面是对端公钥与信任状态，没有私钥。其中三份归档库确实还 pin 着旧一代 fp（见追加十一那张表）。

但这三条旧 pin 在删除 `priv.pem` 之后已经失效：旧身份没有私钥就签不出可信语句，被 pin 的一方也拿不出对应密钥。所以归档库的残留属于「可读的旧状态」，不构成可用的冒充能力。

仍未处置的是整份快照目录本身。若目标是「盘上不留可读的旧身份痕迹」，要移的是整个目录而不是只挑 `keys/`。按低危记录。

## 追加十三：盘上共存三代密钥，全局库里还有一份私钥

查残留时翻出一件比归档更要紧的事：`~/.config/onlyne/keys/` 下每个身份都有完整的 `pub.key` 与 `priv.pem`，包括四个 agent 与四个网关，外加 `uicheck`、`uicheck2`、`urn_onlyne_user_tester`。

四个 agent 的公钥解出来是**第三代**指纹，既不是活库里的、也不是归档里的：

| 节点 | 全局库 fp | 归档 fp | 现行 fp |
|---|---|---|---|
| caerulea | `a0cb60d0e1df2d15` | `645aa6d86ac3f7a6` | `076265175e51d119` |
| cyanea | `388e6391359599ac` | `6e6ca8364c0d9d24` | `528fd494468e2e4b` |
| dpdp | `b2c5784a9d07ab6c` | `454294f2a000afa9` | `4409a11182483cd0` |
| lazuli | `cfcd7d149d4cd994` | （归档 keys 目录为空） | `0fe381a2f4f1cbaa` |

活库 `trust/*` 里查这四个指纹，`no rows`。所以这一代没有被任何活信任表 pin 过，拿它签名不会顶替任何已建立的边。

它的意义在别处：**同一个节点 id 在盘上存在三套私钥材料**。若某进程用错的 `key_scope` 启动，它会以第三代身份对外握手，所有对端看到的 fp 都会变。挡住这一点的正是追加八那条 pin 规则——同名换 fp 即异常，不当重连处理。没有这条规则，换钥是静默的。

这不是一堆测试夹具。`runtime.ts:196` 写着 `const keyScope = this.config.node.key_scope ?? "global";`，注释是「Without it the historic global store stays in effect」——`~/.config/onlyne/keys/` 是历史默认的密钥位置，配置里不写 `key_scope` 就会用它。四个节点的配置目前都显式写了 `key_scope = "project"`，所以这一代今天闲置，但完全可用：某份配置只要丢掉这一行，节点就会以第三代身份启动，所有对端看到的 fp 全变。

同一目录里 `uicheck`、`uicheck2`、`urn_onlyne_user_tester` 才是测试用的；`qoder/workspaces/*/keys/` 是另一套，node id 不同（`qoder-onlyne-b1e97588`、`qoder-qoder-onlyne-0bf48cd0`），文件名是 `priv.key` 而不是 `priv.pem`，由不同写入方生成，不属于这四个 agent 的第四代。

另澄清一处我自己的表述：`caerulea` 的现行 fp `076265175e51d119` 不是 trust 行——trust 表不含自己。它来自 `roles/caerulea/.onlyne/keys/<node>/pub.key`，也就是 `key_scope = "project"` 指向的那份。上表把「现行 fp」与「trust 存储值」并列时，caerulea 那一行只有前者成立。

这三个 `priv.pem` 我没有动。删不删取决于是否保留这条历史默认路径，处置需要用户判断。

### 顺带钉死我那个错误读数的来源

`Documents/progs/onlyne/.onlyne/omp.db`（onlyne-dev 项目库）的 `trust/*` 恰好是两条：`carly` 与 `gateway:im-qq`。我先前把这份的节点名挪到了 caerulea 归档库的「2 条」上，两处计数各自都对，拼起来就错了。这类错误靠单看数字发现不了，只能靠回到每一份库单独取。

### 计数为什么会自己漂

`packages/lib/src/keys.ts:603-608` 的 `consumeNonce`：每次认证请求先扫 `nonce/` 前缀，删掉早于 `now - CLOCK_SKEW_MS`（300 秒）的行，再写入本次 nonce。所以 `kv` 的行数随请求频率与时间窗上下浮动，同一份库隔几分钟数一次就会不同。

`discovery/*` 则是另一个方向：验证过的行从不清理。本机表里 02:10、02:30 的过期条目到 06:00 仍在，只被标 `error=unavailable`，不删。所以 discovery 行数只增不减。

两个方向叠加，再碰上 WAL 是否合并，同一份库能数出四个不同的数。要一份稳定快照，用 `VACUUM INTO` 导出再数，不要拷 `db`、`db-wal`、`db-shm` 三个文件。

## 追加十四：资源迁移后旧线程 ref 留在本地 known 行，reconcile 静默吞掉失败

飞书会话从 `urn:onlyne:gateway:im` 迁到 `urn:onlyne:gateway:im-lark` 之后，本节点对这一会话出现三种读法两成一败：`onlyne://resources/<owner>/<id>` 与 `.../log` 都成功，按线程 id 读 `onlyne://threads/im-lark-oc_bca04a65e2b3d52972fd67595a2235c6` 报 `The response does not prove the expected node identity`。

两种读法用的 owner 不同：资源读用新 ref（`lark-oc_…` → im-lark），线程读用本地 `known/` 里的旧 ref。本节点库内实测确实存在 `known/urn%3Aonlyne%3Agateway%3Aim/im-lark-oc_bca04a65…`，即这条线程的 owner 仍记作 `gateway:im`。于是签名请求发到 3991，而 3991 现在用 im-lark 的密钥签名，`verifyResponse` 对不上。

本节早先版本把它写成「网关生成了待投递记录但没送到」，并说「网关日志没有 push 结果记录」。后一句是错的：这套设计里没有 push。收件人按本地 `known` 的 ref 去拉 owner 的 `/threads/<owner>/<id>/attention`，拉到才 ack。所以「没送到」这个说法不成立，是根本没有人去拉。

失败为什么是静默的，源码位置清楚：

- `packages/lib/src/client.ts:557-566`：`reconcile()` 遍历 `#knownRefs()`，对每个 ref 单独 try/catch，非本节点的错误一律 `continue`，注释写明「One owner this node cannot reach or resolve must not starve the rest」。
- `client.ts:1385-1386`：拉 attention 的路径由 ref.owner 拼出，所以旧 owner 决定请求去向。
- `client.ts:1162-1168`：ack 同样按 ref.owner 发。

于是签名失败的 owner 被跳过，attention 那行永远停在 `acked:false`，而调用方看不到任何错误。

为什么迁移后不会自愈：`client.ts:485-486` 的 `#ensureMember` 在 member 行已存在时直接返回，所以不会再发 grant notice。迁移把 member 行改成了 im-lark，两条旧 grant notice 迁移后仍是 `acked:true`，而 `notices()` 只返回未 ack 的——没有任何路径重新教成员新的 ThreadRef。

修法在补发这一层：迁移后按 member 行给每个成员发一条新的未 ack grant notice，对端 `acceptGrant` 写入 `known/<新 owner>`，attention 才拉得到。本节点侧的 record 修复（删旧 owner 的 `grant`／`known`／`inbox`／`consumed`／`todo` 行与指向 3991 的 `trust`／`discovery`）只能止血，且 `known` 只由 `acceptGrant` 写，手动读一次线程不会自愈。

用户确认这是迁移缺陷，属预期内的异常。记录在此供后续排查迁移路径时对照。
