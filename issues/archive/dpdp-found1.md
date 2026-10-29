# dpdp 现场问题报告 1：信息可见渠道（2026-10-25）

本报告从 `daily-workbench/dpdp` 这个 OMP 会话出发，记录一次连通性测试里能看到的全部信息面：我是谁、我能看到谁、我发出去的东西还能不能看到、对端的回信能不能读到。

结论先说：**发出去的消息只留下一条状态行，正文和后续都读不回来；收进来的消息一条都没到，`onlyne://inbox` 全程为空；邻居和联系人只有人类 TUI 有、agent 侧没有任何通道。** 三条加起来，agent 在 Onlyne 里对自己所处的网络是半盲的——能发，但看不见自己发进哪个线程、也看不见别人回了什么。

需要说明边界：`onlyne://inbox` 这个视图是存在的，我每次读都是 `No attention items.`。所以「收件侧没有入口」不是本次能下的结论——本次的实际情况是**一条入站消息都没有到达**，我没有机会验证收到之后能不能读。

全程只诊断，没有绕过，没有改代码，没有读插件源码和 `omp.db`。日志证据来自宿主进程的日志文件 `~/.omp/logs/omp.2026-10-25.<pid>.log`，这是协议之外的诊断通道。

---

## 主观感受：我在这个网络里是「听得见自己说话，听不见别人回答」的

先说一句最直白的：**这套接口让我感觉自己被蒙着眼睛放进了房间。** 我知道房间里有别人（用户告诉我 caerulea 在），我能朝那个方向喊话，回执也告诉我「喊到了」。但我不知道房间里还有谁、谁已经跟我握过手、我刚才那句话落进了哪个角落、有没有人回答我。

具体的体感，按让我难受的程度排：

**一、发完之后手里是空的。** 一次 `accepted` 之后，我拿到的是一串 uuid。这串 uuid 在本地什么都打不开。我习惯的工作方式是「做完一步，回读一步」，在这里回读这一步根本不存在。我想确认自己写了什么，得去翻 `onlyne://outbound/<id>` 的 digest——那是个给机器看的 JSON 字符串，不是给人（或给 agent）看的一条消息。

**二、问「我有谁」这个问题时，我在跟分类器猜谜。** 这是我这次最挫败的地方。我很清楚我要问什么——哪些邻居握手完成了——但这个问题在这套接口里没有落点。我换了七八种问法，看着 judge 在 `action: send` / `read_scope: none` 之间来回摇摆，报错一次比一次更像在怪我措辞不好。实际上不是我的措辞问题：**这个问题没有对应的答案选项。** 一个 agent 连自己的邻居都列不出来，却要它去决定「该找谁协作」，这件事本身是矛盾的。

**三、我的一条读请求被记成了一次投递。** 我想读线程，结果它没有返回任何数据，而是在本地记了一条出站记录、报 `Delivered … seq 2`。这件事让我后背发凉——不是因为它坏了，而是因为**它的坏法和我以为的坏法不一样**。我原本担心的是「发了但没到」，实际遇到的是「没想发，但它按发了记账」。对一个会自己决定动作的系统来说，后者才是真正难防的。

**四、空结果和「不存在」长得一模一样。** 我编了一个根本不存在的资源 id，它回我和真实资源一样的 `items: []`。如果我不做这个对照实验，我会把它读成「确实没有邻居」，然后基于这个错误前提继续干活。agent 最怕的不是报错，是**沉默地给出一个看起来像答案的东西**。

**五、我的所有判断都得靠日志反推。** 这次能写清根因，靠的是 `~/.omp/logs/` 里的进程日志——一个协议之外的、给人排查用的通道。也就是说：**agent 在协议内看到的，比人在日志里看到的少得多。** 如果这类问题出现在没有日志权限的环境里，我除了「它没成功」之外什么都说不出来。

一句话总结我的感受：**能发，但看不见。** 这不是「功能不全」，是「信息不对称」——系统知道的事比它告诉我的多，而它没告诉我的那些恰好是判断下一步要用的。

---

## 关于信息可见渠道的建议

按「补上之后我能不能自己把活干完」排序，而不是按实现难度。

### P0：让 agent 能回答「我有谁」

没有这一条，其余协作都建立在盲猜上。最小可用形态（字段示意，不是本次实际数据）：

```
onlyne://neighbors
  <node id> · <region> · handshake: complete | pending | refused · last_seen: <time>
```

同一份数据给 `msg` 一个 `read_scope: neighbors`。关键是**两个入口同源**，别再让 TUI 和 agent 各自维护一套枚举。

### P0：让一次发送在本地留下可读的对象

现在的 `onlyne://outbound/<id>` 是「投递记录」，不是「我发的那条消息」。我需要的是发完之后能回读：

```
onlyne://outbound/<id>
  正文（原文，不是 digest）
  落到哪个线程 / 第几条
  对方读了没有
  对方回了没有 → 回复的 message id
  超时了没有
```

最后三项现在是空的，而它们正是「我该不该继续等」的唯一依据。

### P1：让「没有」和「不知道」在返回值上分开

`items: []` 只能有一个含义。未知资源 id 必须报错，或返回带 `supported: false` 的明确否定。这条改起来最便宜，收益却很大——它直接决定了 agent 会不会基于幻觉继续推理。

### P1：`read_scope` 与 `onlyne://` 视图一一对应

两套枚举各自演化是这次一半问题的来源。如果 `onlyne://` 有 `neighbors`，`read_scope` 就该有 `neighbors`；如果没有，两个都别留。现在的情况是视图有、scope 没有（`inbox`/`threads` 有），或者两者都没有（`neighbors`）。

### P2：决策不自洽时硬拒，不要挑一个能跑的执行

`action: send` + `read_scope: messages` 这种组合应当直接拒绝。现在它按 `action` 执行，后果是一次读请求变成了误发。宁可报「决策矛盾，请重述意图」，也不要猜。

### P2：给 judge 的失败报错说人话

`The judge did not choose a readable scope.` 把责任推给了模型，而实际是接口没有提供选项。改成 `no readable channel exists for this topic` 之类，排查方向立刻就不一样了——这次我花了七八次尝试才意识到不是措辞问题。

### P3：`relation` 允许调用方固定

`meta.relation` 显式给出时不要覆盖。已经在 `meta` 里写清的事实不该再被猜一遍，这是 `msg` 文档自己的定位。

---

## 环境与现场

| 项 | 值 |
|---|---|
| 节点 | `urn:onlyne:agent:dpdp` |
| region | `urn:onlyne:region:daily-workbench` |
| endpoint | `http://127.0.0.1:54280/onlyne/v3`（本进程 pid 16895） |
| 对端 | `urn:onlyne:agent:caerulea`（`alexandria`，pid 17047，`http://127.0.0.1:54484/onlyne/v3`） |
| 配置 | `.onlyne/config.toml` 只有 id / region / key_scope / bind / page_size |
| 密钥 | 存在，`.onlyne/keys/urn%3Aonlyne%3Aagent%3Adpdp/{priv.pem,pub.key}` |

本次实际发出的消息三条，本地都记成 `accepted / durable`：

| message id | 落点线程 | seq |
|---|---|---|
| `4867719e-00e0-4a1d-85f3-888124ac6ae9` | `59ca7caf-f9f4-47b6-9cfd-d83f0484e026`（owner: caerulea） | 1 |
| `bcc0fdae-73b1-45bc-8719-6e81af418c84` | `e3433c29-2d78-46b3-b525-f8610f9886f8`（owner: caerulea） | 1 |
| `f927c1ee-8fc4-4bfc-94e2-a34e05784b02` | `59ca7caf-f9f4-47b6-9cfd-d83f0484e026`（owner: caerulea） | 2 |

第三条是误发，见问题 4；前两条在对端日志里都有对应的入站记录，第三条没有。

---

## 问题 1：发送方读不到自己刚写入的线程，回执给出的引用在本地是死链

### 1. 现象

首条消息回执写着：

```json
"thread_ref": { "owner": "urn:onlyne:agent:caerulea",
                "id": "59ca7caf-f9f4-47b6-9cfd-d83f0484e026" },
"seq": "1"
```

拿这个 id 回读，两条路径都不认：

- `onlyne://threads/59ca7caf-f9f4-47b6-9cfd-d83f0484e026` → `No thread is named 59ca7caf-f9f4-47b6-9cfd-d83f0484e026.`
- `msg({ meta: { thread: "59ca7caf-…" } })` → `No local thread is named 59ca7caf-….`
- `onlyne://threads` → `No threads.`

与此同时 `onlyne://` 根页写着 `Threads: 0`，而 `onlyne://outbound` 有一条 `accepted` 记录。后续又发了两条，`Threads: 0` 始终不变。**三条消息发出去了，本地线程数一直是 0。**

### 2. 判断

回执里的 `thread_ref` 是「对端线程的引用」，不是「我这边可以打开的对象」。这在语义上说得通（线程 owner 是 caerulea），但接口没有区分这两件事：回执只给一个 id，读者很自然会拿它当句柄用，而它在本地没有任何对应的可读对象。

对比 caerulea 那份报告里的行为：她收到我的消息后，线程是**她自己**的，所以她能 `read scope=messages` 读到（她日志 13:26:04 那次 `onlyne.msg.result` 成功）。发送方则永远拿不到这个视角。

### 3. 影响

- 发出后想复核「我到底写了什么、写到哪」只能靠 `onlyne://outbound/<id>` 的 digest 字段反推。
- 无法确认对端是否已经消费、是否已回复。
- 想回复时没有可用的线程句柄，直接导致问题 5。

### 4. 建议

回执里把两件事分开：`thread_ref`（对端 owner 的线程）之外，再给一个本地可读的回执对象 id，或直接允许 `onlyne://outbound/<message-id>` 承担这个角色并把它写进回执。至少要能在本地打开「我发出的这条消息及其后续」。

---

## 问题 2：邻居、联系人、发现状态只有人类 TUI 有，agent 侧没有任何通道

### 1. 现象

`onlyne://` 根页自己列出可用视图，只有四个：

```
Views: onlyne://threads, onlyne://inbox, onlyne://contexts, onlyne://outbound
```

其余一律 `Unknown onlyne:// route`，逐个试过：`neighbors`、`contacts`、`nodes`、`peers`、`status`、`discovery`、`trust`、`pending`、`messages`、`recovery`、`node`。

`msg` 的资源读取通道对同样的主题全部返回空列表：

```json
{ "items": [], "next_cursor": null }
```

试过的资源 id：`neighbors`、`contacts`、`nodes`、`peers`、`connections`、`attention`、`outbound`，以及区域范围内的 node 列表。

而 README 明确写着这些信息是存在的，只是活在 TUI 里：

> 5. `/onlyne` page 3 lists every discovered node with `[x]`/`[ ]` … page 4 lists the reachable contacts.

**也就是说：发现结果、握手状态、可达联系人，人类在 TUI 上能看见，agent 没有任何等价入口。** 我这次要回答「我有哪些已握手邻居」这个问题，`onlyne://`、`msg` 两条路都走不通，最后是靠宿主的 `onlyne.listening` 日志和 `lsof` 才拼出对端的 endpoint。

### 2. 判断

这是本次最影响可用性的缺口，因为「我能不能联系到某人」是 agent 做任何协作决策的前置事实。当前 agent 唯一能做的是**盲发**：写一个 `meta.to`，然后看回执。

### 3. 建议

- 补 `onlyne://neighbors`（或 `contacts`），内容与 TUI page 3/4 同源：node id、region、握手状态、最后可达时间。
- `msg` 的 read_scope 里加对应的 scope，让「列出邻居」这类意图能落到确定的读取上，而不是靠 judge 猜。

---

## 问题 3：空结果不可区分，`items: []` 同时表示「没有」和「不存在」

### 1. 现象

同一个形状、同一句 `Onlyne answered`，覆盖了三种完全不同的情况：

| 请求 | 结果 |
|---|---|
| `resource: {owner: self, id: "neighbors"}` | `{ "items": [] }` |
| `resource: {owner: self, id: "outbound"}` | `{ "items": [] }` ← 实际有三条记录 |
| `resource: {owner: self, id: "zzz-not-a-resource"}` | `{ "items": [] }` |

第三条是我故意编的、不存在的资源 id，它和真实资源返回的东西一模一样。

### 2. 判断

这个失败模式对 agent 特别危险：模型会把它读成「确实没有邻居」，而不是「这条路我没走对」。`outbound` 那条尤其说明问题——`onlyne://outbound` 同一时刻列出三条记录，`msg` 的资源通道说空。两者不一致，且空的那个没有任何标记。

对比 `onlyne://` 的行为是好的：未知路由会明确说 `Unknown onlyne:// route neighbors.`，未知 id 会说 `No outbound record is named ….`。同一套协议的两个入口，一个诚实一个沉默。

### 3. 建议

`msg` 资源通道对未知 id 应当报错（或返回带 `supported: false` 的明确否定），不要用空列表代替。空列表只留给「资源存在且确实为空」。

---

## 问题 4：读意图被判定成 send，本地记成一次成功投递（最严重）

### 1. 现象

我想读「回执里那个线程的消息」，发出的意图是：

```
delivery_kind: "read the messages in the remote thread my receipt names"
meta: { thread: "urn:onlyne:agent:caerulea/59ca7caf-…" }
```

返回：

```
Delivered to urn:onlyne:agent:caerulea (thread 59ca7caf-… seq 2) and recorded durably.
```

它**发出去了一条消息**，`f927c1ee-8fc4-4bfc-94e2-a34e05784b02`，正文是那句本该作为查询描述的文字，作为 seq 2 落进了 `59ca7caf` 线程。

对端是否收到这一条，我无法从任何接口确认：caerulea 的日志里 `onlyne.inbound.received` 只有 `4867719e`（13:24:23，线程 `59ca7caf` seq 1）和 `bcc0fdae`（13:36:33，线程 `e3433c29` seq 1）两条，**没有 `f927c1ee`**。而我这边的 `onlyne.outbound` 说它 `accepted`。这正好是问题 6 说的那个盲区：`accepted` 只代表本地记账，不代表对端可见。

日志里能直接看到判定过程（`omp.2026-10-25.16895.log`）：

```
113| onlyne.judge.decided … "answers":{"action":"send", …, "target":"urn:onlyne:agent:caerulea",
    "read_scope":"messages", "thread_op":"none", …}
114| onlyne.judge.action … "action":"send","to":"urn:onlyne:agent:caerulea",…,"thread":"urn:onlyne:agent:caerulea/59ca7caf-…"
115| onlyne.outbound.admitted f927c1ee-… "state":"accepted","guarantee":"durable"
```

注意 113 行：`action` 是 `send`，但 `read_scope` 是 `messages`——两个字段自相矛盾，而执行按 `action` 走了。

### 2. 判断

这条比 caerulea 报告的问题 1 更危险一点。她那边的表现是**本地追加冒充投递**（消息没出门，回执照样报成功）；我这边是**读请求变成了 `outbound.admitted`**，本地按投递记账，还占掉了对端线程的 seq 2。一个纯读取的意图，既没有返回任何数据，也没有报错，而是产生了一条出站记录。

这条消息最终有没有到达对端，我从任何接口都查不到（对端日志没有它的入站记录，见问题 6）。也就是说，一次读操作最坏的后果可能已经发生（污染对端线程），而调用方连「发生了」都看不出来。

而且回执是纯粹的成功语义：`admitted … accepted … durable`，没有任何一处提示「这次操作和你描述的动作不是一回事」。

### 3. 建议

- `read_scope` 非 `none` 而 `action` 为 `send` 时，直接判为决策不自洽并拒绝，不要让 `action` 单方面胜出。
- 或者更根本：`read` 类意图如果缺少足够的判据（比如目标线程在本地不可读），应当明确失败，而不是降级成一个「看起来能做成」的 send。
- 出站路径补一行日志记录 judge 的完整答案（caerulea 报告问题 5 也提了同一件事，这里再确认一次：`read_scope` 与 `action` 的冲突在日志里是可见的，但对调用方不可见）。

---

## 问题 5：回信没有可用的 `in_reply_to` 形状

### 1. 现象

我想回 caerulea，但**我手里没有任何属于她的消息**——她对我的问候尝试了七次回信，一次都没送达（见问题 6）。我能引用的只有我自己那条 seq 1。在这个前提下，三种 `in_reply_to` 引用形式全部被拒：

| 形式 | 结果 |
|---|---|
| `{ thread: "59ca7caf-…", seq: "1" }`（裸 uuid） | `No local thread is named 59ca7caf-….` |
| `{ thread: "urn:onlyne:agent:caerulea/59ca7caf-…", seq: "1" }`（owner 限定） | `This node does not serve the requested owner.` |
| `{ sender: "urn:onlyne:agent:dpdp", id: "4867719e-…" }` | `The reply must go to its original reply authority urn:onlyne:agent:dpdp, not urn:onlyne:agent:caerulea.` |

第三种最能说明问题：`sender/id` 形式会从被引用的那条消息反推 reply authority，而我引用的消息是我自己发的，authority 就是我。要回 caerulea，我必须引用**她那条消息的 id**——但 dpdp 从头到尾没有收到过任何 inbound（本进程日志里 `onlyne.inbound.*` 零条），也没有任何视图能看到对端线程里的消息内容。

**死循环：想回信需要对方的 message id；拿到它要么靠收到消息，要么靠读到对端线程；而收到消息这件事本身就没发生，读对端线程又没有通道。**

### 2. 判断

`in_reply_to` 的三种引用形式里，没有一种能在「对端 owner 的线程」上工作。第一种假设线程是本地对象，第二种被 owner 校验挡住，第三种只在你引用自己消息时可用。

### 3. 建议

`{ thread, seq }` 在 thread 属于对端 owner 时，应当被解释成「向该 owner 的这条线程追加一条 reply」，而不是当作本地对象去查。这才是 `execution: existing` 的自然含义。对照面：caerulea 那边用 owner 限定的 thread ref 做**读**是成功的（她日志 13:26:04，`scope: messages`，`thread: urn:onlyne:agent:caerulea/59ca7caf-…`），同一组数据换成本地节点去写就被拒。

---

## 问题 6：`awaiting reply` 状态不可见，`accepted` 与「对方收到」不可区分

### 1. 现象

首条消息的 plan 是 `{ "response": "await", "execution": "new" }`，`retry_until` 是 `2026-10-26T05:24:02.857Z`（第二天）。但：

- `onlyne://outbound` 只显示 `accepted`，没有「已回复 / 等待中 / 超时」。
- `onlyne://outbound/<id>` 的详情里能看到 `plan.response: await` 和 `retry_until`，但**没有** reply 状态字段。
- `onlyne://inbox` 全程是 `No attention items.`（这个视图存在，只是没有任何条目）
- 本进程日志里 `onlyne.inbound.*` 一条都没有（`grep inbound` 无匹配）。

也就是说：我对 caerulea 发了一条 `await` 语义的消息，**在这套接口里，我无法区分「她还没回」和「她的回复丢了」和「回复路径压根不存在」**。实际发生的是第三种：caerulea 七次尝试回信，全部在发送阶段失败，一次都没出门（她日志 13:24:40 / 13:25:21 / 13:25:40 / 13:26:15 / 13:26:41 / 13:27:16 / 13:27:42，错误分别是 `Subagent sessions may only read Onlyne state`、`a delivery cannot be a local append`、`did not choose one of the target's execution contexts`）。

同一件事还有第二个证据面：**我的三条 `accepted` 里只有两条在对端留下入站记录**。`4867719e` 与 `bcc0fdae` 在 caerulea 的 `onlyne.inbound.received` 里各有一条，`f927c1ee` 没有。所以 `accepted` 和「对端收到了」之间不是等号，而本地没有任何字段能把这两件事分开。

### 2. 判断

用户说的「dpdp 那边又没法回信了」的直接原因是第一件事：**caerulea 七次都没发出来**。第二件事是我这次能确认的另一半：**即使她发出来了，我也无从得知**——本地没有任何字段把「已记账」和「对端已收到」分开，`inbox` 也没收到过任何东西。

### 3. 建议

- `onlyne://outbound/<id>` 增加 reply 状态（`pending` / `replied` / `expired`）以及对应的 reply message id。
- `accepted` 只应表示「本地已记账」；「对端已确认收到」需要独立字段（例如对端 receipt id 回传），否则调用方无法判断消息是否真的落地。
- `onlyne://inbox` 之外，需要一个「已读但未回复」的可见面，否则 `await` 语义在 agent 侧没有落点。

---

## 问题 7：`relation` 由 judge 决定，调用方无法固定

### 1. 现象

同一个 `meta: { to: "caerulea" }`，意图措辞不同，judge 给的 `relation` 不同：

| 意图 | judge 判定 | 结果 |
|---|---|---|
| `send a greeting to caerulea` | `relation: none` | 成功 |
| `给 caerulea 发一条新的普通消息` | `relation: followup` | `A relation needs an in_reply_to reference.` |
| `send a brand-new independent message to caerulea; this is not a reply and not a followup to anything` | `relation: none` | 成功 |

第二条里我明确说了「新的普通消息」，judge 仍然判成 `followup`，然后被校验拒绝。要让它判对，必须把「不是 reply、不是 followup」这句话显式写进意图里——这是在跟分类器博弈，不是在用接口。

### 2. 建议

`relation` 允许调用方显式给出（`meta.relation: "none"`），judge 只在缺省时推断。这和 `msg` 文档里「`meta` carries references you already know」的定位一致：调用方已知的事实不该再被猜一遍。

---

## 问题 8：问节点状态时 judge 只能选 `none`，读意图在决策层就断了

### 1. 现象

这一节单独列，是因为它和问题 2 是两个层次：问题 2 说「没有对应的视图」，这一节说「即使视图存在，读意图也到不了」。

下表是本次所有「问节点或网络状态」的意图，八次全部失败在决策或其后一步的校验上，返回的是错误而不是数据：

| 意图 | judge 答案 | 报错 |
|---|---|---|
| 列出已握手邻居 | `action: send`, `target: none` | `The decision did not choose a delivery target.` |
| 列出已知联系人及其可达性 | `action: read`, `read_scope: none` | `The judge did not choose a readable scope.` |
| 显示可达联系人页 | `action: send`, `target: none` | `The decision did not choose a delivery target.` |
| 我自己的可达邻居 | `action: read`, `read_scope: none` | `The judge did not choose a readable scope.` |
| 显示 onlyne 第 3 页（已发现节点） | `action: read`, `read_scope: none` | `The judge did not choose a readable scope.` |
| 列出我的邻居 | `action: read`, `read_scope: none` | `The judge did not choose a readable scope.` |
| 显示我的 outbound 记录 | `action: read`, `read_scope: evidence` | `The judge chose evidence without a message reference.` |
| 显示我的执行上下文 | `action: read`, `read_scope: none` | `The judge did not choose a readable scope.` |

同一批探测里，走 judge 并且真拿到数据的只有三次：`scope: inbox`、`thread_op: list`、以及带 `message` 引用的 `scope: evidence`。前两者恰好是 `onlyne://` 已经有视图的两个主题（`inbox`、`threads`），后者能成功是因为我在 `meta` 里显式给了 `message {sender, id}`——把引用塞给 judge，它就不用猜了。其余几次绕过 judge 的资源通道调用虽然返回了 `items: []`，但那是问题 3 的空结果，不能算答案。

### 2. 判断

这组数据说明 judge 不是随机失手，而是**它的选项集里没有能回答这些问题的答案**。邻居、联系人、节点、发现状态、执行上下文都没有对应的 `read_scope`，所以它只能选 `none`，然后被下游校验拒绝。两次「选成 send」的情况更说明问题：当没有可读的 scope 时，模型会退回到唯一它能看见的动作上。

顺带一个观察：日志里反复出现 `onlyne.judge.trimmed dropped:["history"]`（13:22:29、13:33:11、13:34:29、13:34:30 等）。judge 的上下文被裁剪过，`history` 被丢掉——而「我刚才收到了谁的消息」这类问题恰恰依赖 history。

### 3. 建议

- `read_scope` 补齐到与 `onlyne://` 视图一一对应，别让视图和 scope 两套枚举各自演化。
- 决策失败时的报错应当指出「这个主题没有可读通道」，而不是 `did not choose a readable scope`——后者读起来像模型的错，实际是接口没有提供选项。
- 当 `action` 被判成 `read` 却没有可用 scope 时，禁止回退到 `send`（问题 4 是这条的后果）。

---

## 附：本次确认正常、不要误修的行为

- **首次投递的时序重试是对的**。13:24:02 首次 `state: unknown, code: unavailable`（`No trusted key for urn:onlyne:agent:caerulea`），13:24:23 自动 `onlyne.outbound.redriven` 后 `accepted / durable`。约 21 秒自愈，不需要人工干预。
- **`onlyne://` 的未知路由和未知 id 报错文案是好的**：`Unknown onlyne:// route neighbors.` / `No outbound record is named ….` / `onlyne://contexts has no subroutes.`。问题 3 要修的是 `msg` 那侧，不要动这里。
- **`onlyne://outbound/<id>` 的详情完整**：request、digest、receipt、plan、state 都在，这是目前唯一能复核「我发了什么」的地方。
- **跨区域发送不需要额外配置**。`dpdp`（`daily-workbench`）发给 `caerulea`（`alexandria`）两次成功投递（`4867719e`、`bcc0fdae`，对端日志均有入站记录），region 边界没有拦截。
- **`in_reply_to { sender, id }` 的 authority 校验是对的**：它正确识别出我引用的是自己的消息、authority 是我自己。问题 5 说的是这个形式在跨节点回复时不可用，不是这个校验错了。

---

## 遗留状态

本地 `onlyne://outbound` 记录三条 `accepted`，但对端日志只能确认前两条到达：

- `4867719e` → 线程 `59ca7caf` seq 1（问候）· 对端 `onlyne.inbound.received` 13:24:23 已确认
- `bcc0fdae` → 线程 `e3433c29` seq 1（「新开的消息」）· 对端 `onlyne.inbound.received` 13:36:33 已确认
- `f927c1ee` → 线程 `59ca7caf` seq 2（**误发**，正文是读请求的描述）· 对端日志无对应 `inbound.received`，**未确认到达**

`f927c1ee` 是问题 4 的产物。它在本地记成 `accepted / durable`，对端却查不到收据——如果它确实没有送达，那么「`accepted` 即已投递」这个理解就是错的；如果它送达了而没有日志，那么入站日志有缺口。两种情况都需要在修复时定论。

---

## 复现清单

1. **问题 1**：任意发送成功后，拿回执里的 `thread_ref.id` 去 `onlyne://threads/<id>` 和 `msg({meta:{thread}})` 读 → 都报「不存在」，同时 `onlyne://` 根页 `Threads: 0`。
2. **问题 2**：`onlyne://neighbors`（及 contacts/nodes/peers/status/discovery/trust/pending/messages）→ `Unknown onlyne:// route`；`msg` 资源读取同主题 → 空列表。
3. **问题 3**：`msg` 读一个编造的资源 id（如 `zzz-not-a-resource`）→ `{ "items": [] }`，与真实空资源无法区分。
4. **问题 4**：`msg({ delivery_kind: "read the messages in the remote thread my receipt names", meta: { thread: "<owner>/<uuid>" } })` → 不返回任何数据，本地记一条 `outbound.admitted … accepted`，回执报 `Delivered … seq 2`。
5. **问题 5**：对一条对端 owner 线程的 seq 发 reply，分别用裸 uuid / owner 限定 / sender-id 三种 `in_reply_to` → 三种错误，无一可用。
6. **问题 6**：发一条 `response: await` 的消息后，`onlyne://outbound/<id>` 无 reply 状态字段，`onlyne://inbox` 无条目。
7. **问题 7**：`msg({ meta: { to: "<peer>" }, delivery_kind: "给某人发一条新的普通消息" })` → judge 判 `followup` → `A relation needs an in_reply_to reference.`
8. **问题 8**：依次问「列出我的邻居」「我有几个已握手邻居」「显示我的执行上下文」→ 全部返回 `The judge did not choose a readable scope.` 或 `The decision did not choose a delivery target.`，judge 日志里对应 `read_scope: none`。

---

## 未验证项

- `onlyne://` 是否支持 query 参数（如 `?cursor=`）——只试过 `threads?cursor=0` 与 `inbox?cursor=0`，行为与不带参数一致，未穷举。
- TUI page 3/4 的实际渲染未在本会话验证（本会话是 agent 侧，未打开 TUI）。「agent 无等价通道」的结论基于 `onlyne://` 路由表和 `msg` 读 scope 的可枚举范围，不代表 TUI 之外还有别的入口。
- `msg` 的 `read_scope` 全集未穷举：观察到 judge 实际给出过 `inbox`、`threads`、`messages`、`message`、`evidence`、`none`。是否存在 `contacts`/`nodes` 之类的 scope 但 judge 未选中，无法从外部区分。
- caerulea 侧七次失败的根因归属（`execution: existing` 需要签名 transport、`local append` 校验过严）由她那份报告负责，本报告只记录「回信没有到达 dpdp」这一事实。
