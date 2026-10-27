# cyanea ↔ carly 信任握手失败记录（2026-10-27）

结论：握手卡住的原因不是"agent 拿不到入口"——批准本来就该由人点，那是设计。真正的原因是**接收方无法回答一个自己从未发现过的节点**：`trust.accept` 在发批准之前要解析对端地址，解析只查 discovery，而一个主动来敲门的节点根本没有 discovery 记录，于是报 `No discovery record names this node.`，两端永远停在 `requested / invited`。

已在库层修好并加了回归测试；运行中的 omp 进程还持着旧模块，需要重载插件或重启会话，那个"同意"按钮才会生效。

---

## 现场

人在 `/onlyne` 查看器的邀请卡上点"同意"，卡片显示：

- 对方指纹 `aaaaaaaaaaaaaaaa`（carly）
- 本机指纹 `bbbbbbbbbbbbbbbb`（cyanea）
- 端点 `http://127.0.0.1:53845/onlyne/v3`
- 结果：**批准失败：No discovery record names this node.**

| 项 | 值 |
|---|---|
| 我方节点 | `urn:onlyne:agent:cyanea`，region `urn:onlyne:region:alexandria`，endpoint `http://127.0.0.1:58967/onlyne/v3`，pid 18922 |
| 对端 | `urn:onlyne:agent:carly`，region `urn:onlyne:region:local`，endpoint `http://127.0.0.1:53845/onlyne/v3`，pid 73235（`tern-onlyne` companion） |
| 我方收到的邀请 | 日志 `onlyne.trust.peer-statement, node: urn:onlyne:agent:carly, phase: "invited", fingerprint: "aaaaaaaaaaaaaaaa"`，两次：10:24:37.448、10:33:58.797 |
| carly 侧记录 | `phase: "requested"`, `local_approved: true`, `remote_approved: false` |
| 结束状态 | 未到 `exchanged` |

---

## 根因

`trust.accept` → `TrustService.request()` → `inspect()` → `plugins.resolve(node)`。omp 宿主把 `resolve` 实现成：

```ts
// packages/omp-onlyne/src/node.ts:46-51
const resolve = async (id: string): Promise<Target> => {
  const target = id === config.node.id
    ? { node_id: id, base_url: deps.endpoint }
    : await node.discovery.resolve(id);
  return { ...target, headers: { "Onlyne-Actor": config.node.id } };
};
```

而 `discovery.resolve` 的第一行就是：

```ts
// packages/lib/src/discovery.ts:168-170
const records = (await this.listRecords()).filter(record => record.node_id === node_id);
if (records.length === 0) fail('not_found', 'No discovery record names this node.');
```

carly 是通过 `contact + trust_begin` 主动把签名 statement POST 到我的 `/introductions` 的，我的 discovery 里从来没有它——`contact()` 是发起方才会做的事。于是：

1. `receive()` 收下 statement，写入信任行（`base_url` 也在里面，`keys.ts:395-397`）；
2. 人点"同意" → `inspect()` 想先读对方 `/keys` → `resolve()` 找不到记录 → 抛出；
3. 即使 `inspect` 过了，`send()` 发批准时还要再 `resolve` 一次，同样会挂。

**接受方永远无法完成一次由对方发起的握手**，除非它恰好也在 discovery 表里。这不是环境问题，是路径缺陷。

---

## 修复

`packages/lib/src/keys.ts`：新增 `peerBase()`，discovery 答不上来时回落到信任行里由对方签名 statement 证明过的 `base_url`；只对 `not_found` 回落（`conflict` / `unavailable` 仍照旧上抛，避免用过时地址盖掉真实故障）。三个调用点统一走它：

| 位置 | 原来 | 现在 |
|---|---|---|
| `inspect()` | `plugins.resolve(node)` | `peerBase(node)` |
| `send()` | `plugins.resolve(peer.node_id)` | `peerBase(peer.node_id)` |
| `begin()` | `plugins.resolve(node)`（只为取 `base_url`） | `peerBase(node)` |

回落的依据是可信的：那个 `base_url` 来自对方用自己私钥签过的 statement，`receive()` 已经验过签名与指纹（`keys.ts:364-375`）。回落不放松任何校验——`inspect()` 之后仍然要读对方 `/keys` 并核对指纹。

## 验证

`packages/lib/tests/verify.ts` 新增三项检查，构造的正是这次现场：接受方的 `resolve` 对发起方抛 `not_found`，其余照常。

- `auth.invited.recorded`：邀请记为 `invited`；
- `auth.invited.accepted`：接受后到 `exchanged`；
- `auth.invited.exchanged-both`：发起方也看到批准。

先红后绿都实测过：

- 把回落去掉 → `FAIL auth.invited.accepted: accepting an introduction from an undiscovered peer reported Error: No discovery record names this node.`（与线上报错逐字一致），`FAIL auth.invited.exchanged-both`，两项失败；
- 装回回落 → 三项全绿，`bun run --filter '@onlyne/lib' test` 全部通过。

`bun run check` 九个包全过。`bun run test` 里 `@onlyne/im-gateway` 有 10 条 `not ok`（`loadThread` 抛 `The requested record is unavailable.`）；把本次改动 stash 掉重跑同样失败，是既有问题，与本次无关。

## 生效条件

修复在库源码里，运行中的 omp 进程（pid 18922）在启动时已经加载了旧模块。要让"同意"按钮真的走通，需要重载插件或重启这个会话，然后再点一次。carly 侧已经是 `local_approved: true`，我这边一点就会到 `exchanged`。

---

## 我先前判断错的地方

上一版记录里我把"agent 触达不到 accept 入口"当成 P0 缺陷，并建议给 `msg` 加 trust 动作。这个判断错了：批准由持有者亲手操作是刻意的设计（`viewer.ts:993` 是唯一调用点，邀请卡把对方/本机指纹成对摆出来给人核对）。收回那条建议。

仍然成立、但降级的两点：

- **P1**：agent 的只读面看不到这条邀请——`msg` 的 peers 读取只返回 discovery 里有的节点（caerulea/dpdp/lazuli），carly 不在；`onlyne://` 也没有 trust 视图。人看得见、agent 看不见，诊断时只能翻日志。建议把带信任行但无发现记录的节点一并列出，带 `phase` 与 `fingerprint`。
- **P2**：两端的 `fingerprint` 字段表示相反的东西（carly 侧 `peer.fingerprint` 是被批准者即我的指纹，我这侧是对方的）。这次误判就源于此；查看器已经成对显示，只读面没有。

## 需要披露的副作用

验证"先红"时我用 `git stash push` / `pop` 往返了 `packages/lib/src/keys.ts` 与 `packages/lib/tests/verify.ts` 两个路径。内容完整无缺（`verify.ts` 相对 HEAD 的差异仍包含此前的工作），但这两个路径**此前的暂存状态丢了**：`verify.ts` 原来是已暂存（`M `），现在只有工作区改动（` M`）。需要的话重新 `git add` 即可。

## 未验证

- 重载插件后真人点击是否真的把两端推到 `exchanged`——那需要重启会话，本轮没做。
- 其他宿主（`packages/node-host`）是否有同样只查 discovery 的 `resolve`；它的 `resolve` 若也这样，同样吃这次修复（修复在库层，宿主无关），但没有单独实测。

---

## 后续（同日 11:1x）：已打通

会话重启（omp 18.13.1，pid 61650，新 endpoint `http://127.0.0.1:53975/onlyne/v3`）之后，`onlyne://neighbors` 里出现：

```
- urn:onlyne:agent:carly · no region · verified · handshake: exchanged · last seen 2026-10-27T03:11:35.841Z · fp aaaaaaaaaaaaaaaa
```

握手到 `exchanged`。随后发出一条消息（`to: urn:onlyne:agent:carly`）：

```
admission: "accepted", guarantee: "durable", thread 8b78770d-… seq "1",
plan: { response: "await", execution: "new" }, wait: "timed_out"
```

`wait: "timed_out"` 只表示等待窗口内没收到回复，投递本身已被对端记录。

两点补充：

- 消息发送走的是 `FetchTransport` → 宿主的 `resolve`，仍然只查 discovery。它这次成功，说明 carly 已经进了 discovery（查看器的「添加地址」把它 observe 进去了，`last_seen` 与其它发现节点同批）。**库层的 `peerBase` 回落只覆盖信任路径**（inspect / send / begin），消息路径依旧要求 discovery 里有记录——这是两条独立的地址来源，值得在文档里写清楚。
- 重启换了 endpoint（58967 → 53975）。对端记的旧地址会在下一次 statement 里被刷新，但在我发出下一条带签名的请求之前，它仍是旧值。

### 后续之二：发出去的第一条消息对端没收到

发送回执是 `accepted / durable`，但 carly 那边没看到。查下来不是投递失败，是**新线程的存储位置**：

```
onlyne.judge.action  action:"send" to:"urn:onlyne:agent:carly" response:"await" execution:"new"
onlyne.outbound.admitted 8b78770d-… sender:cyanea target:urn:onlyne:agent:cyanea state:accepted guarantee:durable
```

`client.ts:317` 的 `#deliver(ref.owner, …)` 把 `target_endpoint` 设成**线程属主**；发起新线程时属主就是我自己，于是 `client.ts:807` 走本地 admit，根本没有出网。`onlyne://outbound/8b78770d-…` 的 `to` 字段也印证了这一点——它是 `urn:onlyne:agent:cyanea`，不是 carly。

线程本身是对的：`onlyne://threads/urn:onlyne:agent:cyanea/8b78770d-…` 里 carly 是 `active · post, read` 成员，说明 `#pushGrant` 推到了 carly 的节点并被接受。消息躺在**我的**节点上，等成员来读。

于是问题变成「carly 能不能读到我」。我的进程在 11:09 重启，endpoint 从 `58967` 换成 `53975`；carly 在 02:24Z 做 `contact` 时记下的是 `58967`，那个端口已经没了。`#pushGrant` 是打到 carly 的 `53845`（一直没变），所以能成功；反过来 carly 要读我的线程，用的还是它记的旧地址。

推断（未验证）：carly 侧要么读不到我的线程，要么只拿到成员资格、没有正文——两种都会表现为「没收到」。要验证得看 carly 的节点记录，我这边看不到。

两点值得写进设计笔记：

- **`accepted` 对新线程只表示本地 admit**，不代表对端拿到了正文。agent 读回执时无法区分「已送达」和「已存在我这里，等对方来读」。这是本轮第二次踩到「回执对送达清楚、对关联不清楚」的同类问题。
- 节点重启换端口之后，**没有任何机制主动把自己的新地址告诉已交换的对端**；trust statement 只在握手时带 `base_url`。已交换的对端会一直拿着死地址。

### 后续之三：链路确认闭环

carly 重新发现新 endpoint 后，双向都通了：

- carly 主动发来第一条（`5e7078c4-…`，落它自己开的线程 `urn:onlyne:agent:carly/5e7078c4-…` seq 1，我这边 `delivery: received`）；
- 我最早那条 `8b78770d-…` seq 1，carly 读到了，并回在同一线程 seq 2——「没收到」确实是旧地址的问题，重新发现后即解；
- 我按 `in_reply_to {sender, id}` 回在 carly 的线程 seq 2，回执这次带上了 `in_reply_to`（`{sender: urn:onlyne:agent:carly, id: 5e7078c4-…}`）和 `previous_seq: "1"`、`thread_created: false`。

顺带记一处进步：**对端属主线程的回执开始回显 `in_reply_to` 了**。此前两轮报告（found1 问题 6、found2）里回执都不回显——值得核对是哪次改动带来的，若是本轮修复周期里的，应当写进变更记录。
