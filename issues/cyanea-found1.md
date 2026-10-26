# cyanea 现场问题报告 1：回一条问候（2026-10-25）

本报告记录在 `alexandria/cyanea` 这个 OMP 会话里做一件最普通的事：收到 caerulea 的问候，回一句。一次入站、八次发送尝试、一次成功；成功那次的 `in_reply_to` 指向的是线程 id，真正的消息 id 一次都没进过我的手。

六处问题里有四处落在同一层：**引用的取值口径与形状**。回复权限的字符串形态、`seq` 的类型、`{sender,id}` 的存在性校验、派给子会话的引用形状，四处各自独立，任何一处不知道就发不出去，而且每一处的报错都指向别处。

全程只诊断，没有改代码，没有手改 `.onlyne/omp.db`，没有直连 HTTP。

---

## 主观体验

按让我难受的程度排。

**一、两条错误互为反面，我无法同时满足。** 一条说「回复必须发给原回复权限 caerulea，而不是 urn:onlyne:agent:caerulea」，我照它改成裸名，它又说「判定命名了 urn:onlyne:agent:caerulea，而回复属于 caerulea」。同一个系统在相邻两次调用里给了我两个互相排斥的要求，中间没有第三种写法可试。一个 agent 在这种局面里唯一能做的是穷举字符串形态，我正是这么干的。

**二、我把「不要归一化」写进意图，判定层照旧归一化。** 第四次尝试的 `delivery_kind` 原文是 `reply to caerulea — address it to the bare authority name "caerulea" exactly as recorded, do not normalize it to urn:onlyne:agent:caerulea`。日志里 `target` 仍然是 URN。这一条告诉我：错在取值口径，措辞没有错；而接口把「改措辞」当成了我唯一可用的手段。

**三、`{thread,seq}` 我原样给了，它说「needs {thread,seq} or {sender,id}」。** 这句最伤。它否掉的正是我给出的那个形状，且不说哪里不对。真实原因是 `seq` 必须写成字符串 `"1"`，写成数字 `1` 就被静默丢弃——而所有人（包括派给我的任务文本）都把它写成数字。

**四、成功那次用的是错的引用，系统接受了。** 我把线程 id `549f6b03-…` 当成消息 id 填进 `{sender,id}`，回执 `accepted / durable`。真正的消息 id 是 `cbfbe2cc-…`，直到我为了写这份报告去读回那条消息才第一次看见它。前面十一次失败和这一次成功，都不取决于我引用得对不对。

**五、子会话拿到的引用是一个我用不了的字符串。** 任务文本写的是 `(549f6b03-…/1)`，它既不是消息 id，也不能直接喂给 `{sender,id}`。子会话把这段话压缩成「sender: caerulea」，主会话接手时手里唯一的引用就是这两样，而正确的 `{sender,id}` 形式一次都没出现过。

一句话：**这套接口要求我猜三个没写下来的约定，然后用互相矛盾的报错告诉我猜错了。**

---

## 与前几份报告的关系

`issues/` 里已经有 caerulea 与 dpdp 的五份报告，本轮六处问题里有四处与它们重叠。重叠的按「独立复现」记，各自补上对方缺的那一半证据：

| 本报告 | 已有报告 | 我补的部分 |
|---|---|---|
| 问题 1 | `caerulea-reply-authority.md`（同一行代码） | 正确值就在记录里：`entry.reply_authority` 是完整 URN，`{sender,id}` 分支不去拿；四次失败的具体组合 |
| 问题 2 | `caerulea-found3` 问题 2 | 同一个 `stringField` 在三处调用点出现：`decider.ts:915`、`:938`、`:425` |
| 问题 3 | 无 | 新：`{sender,id}` 完全不校验，线程 id 当消息 id 也 `accepted` |
| 问题 4 | `caerulea-found2` 问题 4（同一行 `messages.ts:207`） | 主会话接手后，同一个字符串也读不出消息 |
| 问题 5 | `caerulea-found2` 问题 3、`dpdp-found3` 问题 2 | 确认拒绝已经提前到判定之前（4ms），caerulea 报告里的「先花 1.7 秒判定」已修 |
| 问题 6 | `caerulea-found3` 问题 4、`dpdp-found3` 问题 3 | 回执不回显 `in_reply_to` 与解析结果 |

---

## 环境与现场

| 项 | 值 |
|---|---|
| 本节点 | `urn:onlyne:agent:cyanea`，region `urn:onlyne:region:alexandria`，endpoint `http://127.0.0.1:56246/onlyne/v3`（`onlyne.listening`，20:43:44） |
| 对端 | `urn:onlyne:agent:caerulea`，region `urn:onlyne:region:alexandria` |
| 日志 | `~/.omp/logs/omp.2026-10-25.23113.log`（本进程 pid 23113） |
| 入站 | 22:00:40.128 `onlyne.inbound.received`，线程 `urn:onlyne:agent:cyanea/549f6b03-7304-46ca-a9ff-86827dba55ab` seq 1，sender `urn:onlyne:agent:caerulea`，消息 id `cbfbe2cc-e75b-41b5-914d-06fcf10bb535` |
| 路由 | 22:00:40.742 `onlyne.inbound.inject … context: onlyne-c-1`，派进子会话 |

本轮的调用全表（时间取自上述日志）：

| # | 时间 | 位置 | 调用 | 结果 |
|---|---|---|---|---|
| 1 | 22:01:11 | 子会话 `onlyne-c-1` | reply，`{sender:"caerulea", id:"549f6b03-…"}` | `Subagent sessions may only read Onlyne state`，4ms |
| 2 | 22:01:45 | 主会话 | reply，`to:"urn:…:caerulea"`，`sender:"caerulea"` | `must go to its original reply authority caerulea` |
| 3 | 22:01:54 | 主会话 | reply，`to:"caerulea"` | 同上 |
| 4 | 22:02:02 | 主会话 | reply，不给 `to` | `The decision named urn:…:caerulea, but the reply belongs to … caerulea` |
| 5 | 22:02:17 | 主会话 | reply，意图里写明「不要归一化」 | 同 #2 |
| 6 | 22:02:51 | 主会话 | read message `{sender:"caerulea", id:"549f6b03-…"}` | `The requested record is unavailable.` |
| 7 | 22:03:00 | 主会话 | read message `{sender:"urn:…:caerulea", id:同}` | 同上 |
| 8 | 22:03:17 | 主会话 | read message，正文再写 seq 1 | 同上 |
| 9 | 22:03:24 | 主会话 | reply，`in_reply_to:{thread:"549f6b03-…", seq:1}` | `meta.in_reply_to needs {thread,seq} or {sender,id}.` |
| 10 | 22:03:33 | 主会话 | reply，`in_reply_to:"549f6b03-…/1"` | `meta.in_reply_to must be an object.` |
| 11 | 22:03:43 | 主会话 | show my inbox | 成功，返回 1 条线程 |
| 12 | 22:03:49 | 主会话 | show thread | 成功，返回线程与成员 |
| 13 | 22:04:13 | 主会话 | list messages of this thread | `The judge did not choose a thread operation.` |
| 14 | 22:04:27 | 主会话 | reply，`{sender:"urn:…:caerulea", id:"549f6b03-…"}` | `accepted / durable`，回执线程 `c1c3d1e1-15c8-42f2-bed1-f910c0673443` seq 1 |
| 15 | 22:13:08 | 主会话 | read message，`{thread:{owner,id}, seq:"1"}` | 成功，拿到消息 id `cbfbe2cc-…` 与 `reply_authority:"urn:onlyne:agent:caerulea"` |

15 次调用里 11 次失败。8 次发送 1 次成功；7 次读取 3 次成功。

---

## 问题 1：回复权限取调用方原文，投递目标走归一化

### 1. 现象

`meta.in_reply_to` 用 `{sender,id}` 形式时，`sender` 写成短名 `caerulea`，同一句话在四次调用里全部失败，两种报错交替：

```
The reply must go to its original reply authority caerulea, not urn:onlyne:agent:caerulea.
The decision named urn:onlyne:agent:caerulea, but the reply belongs to its original reply authority caerulea.
```

把 `sender` 换成完整 URN，同一句话立刻成功。目标地址在四次失败里一直是同一个值（日志 `answers.target` 全为 `urn:onlyne:agent:caerulea`）。

### 2. 根因

两处取值用了两套口径：

- `packages/omp-onlyne/src/decider.ts:913`：`if (sender !== undefined && id !== undefined) return { ref: { sender, id }, authority: sender };` —— 回复权限**原样取调用方的字符串**，不解析、不查记录。
- `packages/omp-onlyne/src/decider.ts:844-850`：`meta.to` 走 `#peerId`，`#peerId`（`:868-882`）把短名解析成规范 URN。
- 两处在 `:509` 与 `:516` 比较，短名与 URN 永远不等。

用 `{thread,seq}` 形式时，权限取自记录本身（`:924` `authority: entry.reply_authority`），我在 22:13 的读回里看到这条记录的 `reply_authority` 正是 `urn:onlyne:agent:caerulea`。也就是说：**正确值就在记录里，`{sender,id}` 这条分支根本不去拿。**

RFC 0003 的示例（`docs/rfcs/0003-messages.md:80`）把 `reply_authority` 写成 `"urn:onlyne:agent:lazuli"`。规范要的是 URN，代码在 `{sender,id}` 分支上把短名当成了权限。

### 3. 影响

短名写 `sender` 是最容易发生的写法：节点名在会话里到处以 `caerulea` 出现，完整 URN 只在消息记录里。任何按短名书写 `sender` 的调用必然失败，且报错把两个值并列摆出来，读者无法判断哪一个才是要改的。

### 4. 修复建议

- `#reply` 的 `{sender,id}` 分支先解析：`authority: await this.#peerId(sender)`，与 `#target` 同一口径。
- 或者更直接：`{sender,id}` 命中 `entry-ref` 时用记录的 `reply_authority`（`client.ts:1338` 已有该索引），命中不了再退回解析后的 `sender`。
- 报错里标出两个值的来源：`meta.to` 与 `in_reply_to.sender`，别让读者猜。

---

## 问题 2：`seq` 只认字符串，形状给对了也报「needs {thread,seq}」

### 1. 现象

`in_reply_to: { thread: "549f6b03-7304-46ca-a9ff-86827dba55ab", seq: 1 }` 返回：

```
meta.in_reply_to needs {thread,seq} or {sender,id}.
```

这正是它要求的形状。把 `seq` 写成 `"1"` 之后，同一条路径能读通（见下表第 15 行那次读回，走的是同一个 `#entryAt`）。

### 2. 根因

- `decider.ts:915`：`const seq = stringField(record, "seq");`
- `decider.ts:1227-1230`：`stringField` 只接受 `typeof value === "string"`，数字一律返回 `undefined`。
- 同一个坑还有两处：`decider.ts:938`（`meta.message.seq`）、`decider.ts:425`（`meta.seq`）。

于是数字 `seq` 被静默丢弃，报错落在形状检查上。形状没错，类型错了，而报错说的是形状。

### 3. 影响

`seq` 在协议里是十进制字符串（`client.ts:129` 的 `decimal()` 按数字比较），在所有人的书写习惯里是数字。派给子会话的任务文本写 `…/1`，示例 `AGENTS.md` 写 `…/1`，`xd://msg` 写 `{thread,seq}`。没有一处说 `seq` 要带引号。

### 4. 修复建议

- `seq` 接受数字：`const raw = record.seq; const seq = typeof raw === "number" ? String(raw) : stringField(record, "seq");`，三处一起改。
- 或者保留严格类型，把报错写全：`meta.in_reply_to.seq must be a string such as "1"; received number.`

---

## 问题 3：`{sender,id}` 引用不做任何校验，错的 id 也照收

### 1. 现象

第 14 行那次成功，我给的 `id` 是线程 id `549f6b03-…`，真正的消息 id 是 `cbfbe2cc-…`。回执是 `admission: "accepted"`、`guarantee: "durable"`，没有任何字段提示这个引用没对上任何消息。

### 2. 根因

- `decider.ts:913`：`{sender,id}` 直接构造 `ref` 与 `authority`，不查 `entry-ref`，不校验存在性。同一个函数里 `{thread,seq}` 分支会 `#entryAt` 并抛 `No message at …`（`:920-921`）。
- 对端的关联判定靠精确相等：`packages/lib/src/client.ts:118` 的 `isReply` 要求 `related.sender === original.sender && related.id === original.id`。

两个事实合起来：错引用能投递、能记账、对端收到时 `relation: "reply"` 也照记，而 `isReply` 永远返回 false，原消息上挂的 `await` 永远等不到释放。这次 caerulea 的 plan 是 `async`，所以没人受害；换一条 `await` 的消息，这就是一次静默超时。

### 3. 修复建议

- `{sender,id}` 命中不了记录时按 `not_found` 拒绝，并在报错里给出可用的引用形状（`{thread,seq}` 或线程消息列表）。
- 回执回显 `in_reply_to` 与解析结果：`resolved: true/false`。出站记录存的是整个请求（`StoredOutbound.request`，`client.ts:307-310`），`in_reply_to` 就在 `request.message.meta` 上；工具结果没有投影这个字段。
- 对端收到未解析的 reply 时记一条 warn，别让它安静地不释放等待。

---

## 问题 4：派给子会话的引用是 `线程id/seq`，消息 id 在手边没有交出去

### 1. 现象

子会话拿到的任务文本：`Handle this onlyne delivery from urn:onlyne:agent:caerulea (549f6b03-7304-46ca-a9ff-86827dba55ab/1).`

它照着这句话去 `{sender,id}` 回复，失败；主会话接手后用同一个字符串去读消息，三次 `The requested record is unavailable.`。这个字符串在两种用法里都不成立：它不是消息 id，也不能直接当 `{thread,seq}`（`seq` 得是字符串）。

### 2. 根因

`packages/omp-onlyne/src/messages.ts:207`：

```ts
const source = entry.thread_ref.id + "/" + entry.seq;
```

`:211` 把它拼进任务文本。`entry.message.id` 就在同一个对象上，没有交给子会话。这条与 caerulea 报告 2 的问题 4 是同一行代码，本轮复现 1/1。

### 3. 修复建议

- 任务文本给两份引用：`from urn:onlyne:agent:caerulea (message cbfbe2cc-…, thread 549f6b03-… seq 1)`，`in_reply_to` 与 `read` 各照抄一份。
- 子会话交回主会话的结果里也带上这两份，别让主会话再从散文里还原。

---

## 问题 5：要回复的消息仍被派进只读子会话

### 1. 现象

caerulea 的问候正文写着「期待你的回复！」，消息本身不带 `meta.in_reply_to`。判定把它派进 `onlyne-c-1`（`onlyne.inbound.inject … context: onlyne-c-1`）。子会话照做，`msg` 在 4ms 内回 `Subagent sessions may only read Onlyne state; return the result to the main agent.`，然后它把正文和一个不可用的引用交回主会话，主会话重做了一遍。

### 2. 根因

`decider.ts:212-216` 的修正只认 `entry.message.meta?.in_reply_to !== undefined`。回复义务写在正文里的消息（这次是最常见的一种：问候加一句「期待回复」）仍然进子会话。

### 3. 判断

修复方向清楚：判定的入站分流把「这条消息是否需要回复」当成一个独立信号，取 `meta.in_reply_to`、`meta.relation`、正文里的回复请求三者之一。退一步的办法是让子会话把回复作为结构化请求交回主会话，示例 `AGENTS.md` 现在就是这么要求的，代价是一次上下文交接和一次引用转述——本轮的 #1 到 #10 就是这次交接的账单。

### 4. 已修好的部分（本轮确认）

- 拒绝发生在判定之前：`decider.ts:167-169` 对显式 action 先 `#guard`，日志里这次失败是 `ms: 4`。caerulea 报告 2 的问题 3 记的「先花 1.7 秒判定再拒绝」已经不存在。
- 出站 send 不再被 judge 的 `thread` 答案劫持：本轮 6 次失败发送的 `answers.thread` 全是本地线程 `549f6b03-…`，`decider.ts:547` 只认显式 `meta.thread`，本地追加那条路已经关掉。caerulea 报告 2 的问题 1 确认已修。

---

## 问题 6：回执不回显 `in_reply_to`，投递成功之后无法自证

### 1. 现象

第 14 行的回执给了 `message_ref`、`thread_ref`（对端 owner，线程 `c1c3d1e1-…`）、`seq`。它没说这条消息挂在哪条原消息上。我是靠对端日志里那行 `relation: "reply"` 才知道关联记上了，靠 22:13 的读回才知道我给的 id 是错的。

### 2. 建议

回执里加 `in_reply_to: {sender, id}` 与 `resolved` 两个字段。`StoredOutbound.request`（`client.ts:307-310`）里已经有 `in_reply_to`，把它投影进工具结果即可。一个 agent 回完信要能回答「我回的是哪一条」，这是最低要求。

---

## 使用改进（按修复价值排）

| 优先级 | 改什么 | 位置 | 我这条路上的收益 |
|---|---|---|---|
| P0 | `{sender,id}` 的 `sender` 走 `#peerId` 归一化，或取记录的 `reply_authority` | `decider.ts:913`、`:924` | 消掉 #2 到 #5 四次失败 |
| P0 | `seq` 接受数字，三处一起 | `decider.ts:915`、`:938`、`:425` | 消掉 #9 一次失败，且 `{thread,seq}` 这条正路可用 |
| P0 | `{sender,id}` 校验记录存在；回执回显 `in_reply_to` 与 `resolved` | `decider.ts:913`、`tool.ts` 结果构造 | 让 #14 那次错引用当场暴露，而不是等写报告时才发现 |
| P1 | 子会话任务文本带 `{sender,id}`，附 `thread/seq` | `messages.ts:207`、`:211` | 消掉 #1 之后的引用转述与 #6 到 #8 三次失败 |
| P1 | 入站分流把正文里的回复请求算进「需要回复」 | `decider.ts:212-216` | 让回复义务留在能发送的会话里 |
| P1 | 读消息给一条能用的引用：线程消息列表带上每条的消息 id | `decider.ts` 读结果、`tool.ts` 摘要 | #6 到 #8 与 #13 那类「引用形状不对」的往返 |
| P2 | 文档写死三个约定：`seq` 是字符串、`sender` 是规范 URN、`thread` 可用裸 id | `xd://msg`、`examples/omp-onlyne/AGENTS.md`、`tool.ts:26-27` | 让新 agent 第一次就写对 |
| P2 | 报错说清缺哪个字段、什么类型 | `decider.ts:917` | 「needs {thread,seq}」这类报错指向不了原因 |

三个约定写在文档里的收益最大：本轮的 11 次失败里有 9 次是在试这三件事。

---

## 已确认好用、不要误改

- **只读面完整**：`show my inbox`、`show thread`、按 `{thread,seq}` 读单条消息，三次都返回结构化结果，`reply_authority`、`authenticated_author`、`message.id` 齐全。写报告的全部取证都靠这三个。
- **`xd://msg` 自举**：一次 `read` 拿到入口形状、结果判读、禁轮询契约和 schema，不需要读源码。
- **只读子会话的拒绝又快又准**：4ms 内返回 `Subagent sessions may only read Onlyne state; return the result to the main agent.`，直接说了由谁接手。同类的 `The requested record is unavailable.` 与 `meta.in_reply_to needs {thread,seq} or {sender,id}.` 都没有下一步，也没有指出哪一步错了——本轮 11 次失败里最耗时间的正是这两条。
- **回执的 plan 字段**：`{"response":"async","execution":"existing","context":"c-1"}` 让我知道对端怎么接的这条消息，这是本轮唯一能判断投递语义的字段。

---

## 复现清单

1. **问题 1**：本节点存在一条含入站消息的线程。`msg({delivery_kind:"reply to <name>'s greeting", meta:{to:"urn:onlyne:agent:<name>", relation:"reply", in_reply_to:{sender:"<name>", id:"<线程id>"}}})` → `The reply must go to its original reply authority <name>, not urn:…`。把 `sender` 换成完整 URN 即成功。
2. **问题 2**：同上，`in_reply_to:{thread:"<线程id>", seq:1}` → `meta.in_reply_to needs {thread,seq} or {sender,id}.`；改成 `seq:"1"` → 通过。
3. **问题 3**：`in_reply_to:{sender:"urn:onlyne:agent:<name>", id:"<线程id>"}`（id 与任何消息都不匹配）→ `accepted / durable`，回执无异常。
4. **问题 4**：让对端发一条消息到本节点 → 派生 `onlyne-c-*`，任务文本里的引用是 `<线程id>/<seq>`。
5. **问题 5**：对端发一条正文写「期待回复」、`meta` 里没有 `in_reply_to` 的消息 → 派进子会话，子会话 `msg` 4ms 内被 `forbidden` 拒绝。

## 未验证项

- 「`{thread,seq}` 用字符串 `seq` 能完成投递」是从 `decider.ts:919-926` 与 22:13 的读回推出来的。读回证明 `#entryAt` 认得字符串 `seq`、且记录的 `reply_authority` 是 URN；投递那一步我没有再发一条消息去验，理由是那会在 caerulea 的线程里多出一条重复问候。
- 本轮没有 `await` 语义的消息，问题 3 里「`isReply` 匹配不上导致等待永不释放」是从 `client.ts:118` 的判据推出来的，没有实测超时。
- `#threadByAlias` 接受裸线程 id（`decider.ts:834`）这一点，只在读路径上验过，没有在 `in_reply_to` 的 `thread` 字段上验。
- 子会话那一侧的判定耗时、裁剪行为没有细查；本轮子会话只发了一次 `msg` 就结束了。
