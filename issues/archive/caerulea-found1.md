# caerulea 现场问题报告 1（2026-10-25）

本报告记录在 `alexandria/caerulea` 这个 OMP 会话里做两件正常事时抓出来的问题：回 dpdp 的问候、向 alexandria 域内广播一次问好。两件事都碰到了阻断级缺陷，其中「发给 cyanea 的消息被静默改成本地追加、回执照样报成功」这一条最危险，因为它会让模型和用户都相信消息已经发出。

按你的要求，全程只诊断、没有绕过、没有改代码。

---

## 环境与现场

| 节点 | region | 本次观察到的 endpoint |
|---|---|---|
| `urn:onlyne:agent:caerulea`（本节点） | `urn:onlyne:region:alexandria` | `http://127.0.0.1:61409/onlyne/v3` |
| `urn:onlyne:agent:cyanea` | `urn:onlyne:region:alexandria` | `http://127.0.0.1:61577/onlyne/v3` |
| `urn:onlyne:agent:dpdp` | `urn:onlyne:region:daily-workbench` | `http://127.0.0.1:61320/onlyne/v3` |
| `urn:onlyne:agent:lazuli` | `urn:onlyne:region:research-chamber` | `http://127.0.0.1:61386/onlyne/v3` |

endpoint 是 12:37 读到的快照，进程重启后端口会变。四个节点都由 `/onlyne init` 建出 `key_scope = "project"`，因此都带节点密钥、都声明 `authentication: ["signature"]`（`packages/omp-onlyne/src/node.ts:60`）。

`alexandria` 域当前只有 caerulea 与 cyanea 两个成员，所以「域内广播」实际就是向 cyanea 发一条。

**遗留状态**：两次尝试发给 cyanea 的问候（seq 3、seq 4）都落进了我自己的线程 `8e3cd06b-3e32-4392-808b-e6817989a293`，而 dpdp 是这个线程的成员并会 reconcile 它，因此 dpdp 那边会看到两条署名给 cyanea 的消息。修好问题 1 后需要清理，或者由我在 dpdp 线程里解释一句。

---

## 问题 1：发给其他节点的消息被静默改成本地追加，回执仍报成功

### 1. 现象

用 `msg` 向 cyanea 发一条首次联系的消息，`meta.to` 明确写着 `urn:onlyne:agent:cyanea`，返回：

```
Delivered to urn:onlyne:agent:caerulea (thread 8e3cd06b-3e32-4392-808b-e6817989a293 seq 3) and recorded durably.
{"admission":"accepted","receipt":{...,"guarantee":"durable","thread_ref":{"owner":"urn:onlyne:agent:caerulea","id":"8e3cd06b-..."},"seq":"3",...}}
```

`admission: accepted` + `guarantee: durable` 是一句成功宣告，但消息一个字节都没有离开本节点，cyanea 什么也没收到。回执里的 `thread_ref.owner` 是发送方自己（`caerulea`），这是唯一能看出异常的字段。

第二次我把意图写死成 `"send a first message to the peer cyanea; there is no prior thread with it, so do not continue any local thread"`，结果完全相同，seq 变成 4。

### 2. 根因链条

三处叠加，缺一不可：

1. `packages/omp-onlyne/src/decider.ts:675-687`：只要本地存在线程，`thread` 这道题就**无条件**加进问题集，与 action 无关。题目自己的 instructions 写的是「Choose the local thread **a read or thread action** names, or none」——出题人承认这道题对 send 不适用，但答案照样会被拿去用。
2. `packages/omp-onlyne/src/decider.ts:508`：send 分支直接 `const thread = await this.#answerThread(answers, meta);`，不校验线程 owner 与 `to` 是否自洽。`#answerThread`（`:775-781`）只做「meta 显式优先，否则取 judge 答案」。
3. `packages/lib/src/client.ts:492-495`：`action.thread.owner === 本节点` 时 `return this.#admitHere(message, plan)`，`action.to` 被直接丢弃。`#admitHere`（`:282-297`）的注释自己写着 "Appends to a thread this node hosts: no outbound record, no peer, no network."

所以一次「发给对端」的 send，只要 judge 顺手选了一个本地线程，就整条链路降级成往自己的线程里记一笔，全程没有任何冲突检测、告警或降级标记。

### 3. 证据

两次发送的对照（日志 `~/.omp/logs/omp.2026-10-25.5066.log`）：

| 时间 | `meta.to` | 回执 thread owner | seq | `onlyne.outbound.admitted` |
|---|---|---|---|---|
| 12:37:50 | `urn:onlyne:agent:cyanea` | `urn:onlyne:agent:caerulea` | 3 | **无** |
| 12:45:28 | `urn:onlyne:agent:cyanea` | `urn:onlyne:agent:caerulea` | 4 | **无** |

对照组是 dpdp 12:33:20 那次真实投递（日志 `omp.2026-10-25.4889.log` 第 26 行），成功时 `onlyne.outbound.admitted ... state: accepted, guarantee: durable` 与 `onlyne.msg.result` 是成对出现的。缺这一行就是没走网络。

另外两条辅助事实：

- `#transmit` 是唯一真正发 HTTP 的地方（`packages/lib/src/client.ts:585-600`），本地追加路径完全绕过它。
- 工具结果的措辞把线程 owner 当成「投递对象」来陈述，`Delivered to urn:onlyne:agent:caerulea` 对一条发给 cyanea 的消息来说极易被读成成功。`summary()` 的实现（`packages/omp-onlyne/src/tool.ts:97-117`）里没有 `to`、没有「本地追加」标记、也没有 peer 投递事实。

### 4. 修复建议

- `thread` 题只在 action 为 `read` / `thread` 时出；或者在 send 分支要求 `thread.owner === to`，不自洽就按 `invalid_request` 硬拒，绝不允许静默降级。
- `#admitHere` 的返回值在工具结果里显式标成「本地追加」，并把 `to` 一并写进回执，让「发给谁」和「记在哪个线程」两个事实同时可见。
- 若确实需要支持「向自己拥有的线程追加」，让它只能由 `to === self` 或显式 `meta.thread` 触发，而不是由 judge 的自由选择触发。

---

## 问题 2：decider 的 transport 不签名，`execution: existing` 的发送必然失败

### 1. 现象

第一次回 dpdp 的问候，`msg` 直接失败：

```
The target's published contexts could not be read: A node signature is required.
```

我加上 `meta.context` 之后立刻成功，而信任关系其实一直是好的。

### 2. 根因链条

1. `packages/omp-onlyne/src/runtime.ts:93-96` 自己 new 了一个 transport，**没有传第三个参数**：

   ```ts
   const transport = new FetchTransport(async id => {
       const target = await this.node.discovery.resolve(id);
       return { ...target, headers: { "Onlyne-Actor": this.config.node.id } };
   });
   ```

   而 `packages/lib/src/node.ts:34-38` 里 node 自己的 transport 在 keys 存在时是传了 `{ trust: this.trust }` 的。签名逻辑在 `packages/lib/src/transport.ts:63-70`：只有 `signing` 存在且 `trust.entry(target).state === 'trusted'` 才签。这个裸 transport 全程不签名。

2. 它只被一处使用：`runtime.ts:101-103` 的 `listReceiverContexts` → `GET /contexts`。

3. 对端是 key 模式的，入站走 `packages/lib/src/http.ts:379-385`，keyed 节点不允许回退到裸 actor 认证，于是 `packages/lib/src/keys.ts:245` 直接 `A node signature is required.`。

4. `packages/omp-onlyne/src/decider.ts:528-537` 把这个失败包成 `decision_unavailable`，整次 send 失败——尽管 send 本身完全有能力签名。

### 3. 证据

- 失败时从本节点 `.onlyne/omp.db` 读到的信任记录是完整的：`state = trusted`、`local_approved = 1`、`remote_approved = 1`（key `trust/urn%3Aonlyne%3Aagent%3Adpdp`）。信任没问题，是探测请求自己没带签名。
- 加上 `meta.context` 后立刻成功，因为 `decider.ts:530-532` 在 receiver 列表不可用时直接返回显式 context，绕过了那次探测。这反证了 send 路径本身正常。

### 4. 易用性要点

同一句 msg 成不成，取决于 judge 这次选 `existing` 还是 `new`。选 `new` 一切正常；选 `existing` 就报一个听起来像「信任没配好」的错误，而实际信任是好的。错误文本指向错误的方向，排查成本很高。

### 5. 修复建议

给这个 transport 传 `{ trust: this.node.trust }`。构造顺序是障碍：它在 `runtime.ts:93` 创建，`this.node` 要到 `:110` 才存在，所以要么把 transport 的创建挪到 node 之后（decider 的 `listReceiverContexts` 是闭包、只在决策时才调用，可以延后），要么让 `OnlyneNode` 暴露它自己的 transport 供宿主复用，避免同一进程里出现两个签名能力不同的 transport。

---

## 问题 3：首次投递 `unauthenticated` 拒绝为终态，不自愈

### 1. 现象

dpdp 的第一次问候在 12:29:22 被判 `refused`，同一意图在 12:33:20 重发即 `accepted / durable`。中间没有任何配置变更，只是信任交换在这两次之间完成了。

### 2. 根因链条

- `packages/lib/src/client.ts:59-64` 把 `unauthenticated` 列进 `TERMINAL`，`:68-70` 的 `admissionOf` 因此把它记成 `refused`。
- `packages/lib/src/client.ts:245-267` 的 `recover()` 只重投 `pending` / `unknown`，`refused` 是终态，永远不会再试。

于是「首触抢在信任握手完成之前」这一类时序问题，会被永久记成一次拒绝。工具结果既不会提示「去完成信任握手」，也不会提示「重发就行」。

### 3. 证据

日志 `omp.2026-10-25.4889.log` 第 13-14 行（12:29:22，`state: refused, code: unauthenticated`）与第 26-27 行（12:33:20，`state: accepted, guarantee: durable`）。

### 4. 修复建议

- `unauthenticated` 在「本地尚未持有对端 trusted 条目」这一具体前提下不应视为终态：要么在签名门（`transport.ts:63-70`）判定为「无法签名」时直接给一个可重试的 `unavailable`，要么在 `recover()` 里为这类拒绝保留一次窗口内重试。
- 错误文本应当可操作：区分「对端拒绝了我的签名」与「我还没有对端的密钥/信任」，后者直接给出完成握手的路径。

---

## 问题 4：私钥落在 `.onlyne/.onlyne/keys`（低，但会误导排查）

### 1. 现象

四个角色目录下的密钥都在叠了一层的路径里：

```
<role>/.onlyne/.onlyne/keys/urn%3Aonlyne%3Aagent%3A<name>/{priv.pem,pub.key}
```

而对用户承诺的位置是 `.onlyne/keys`：`packages/omp-onlyne/src/keys.ts:78` 的文档注释与 `:85-86` 的报错文案都这么写，`/onlyne init` 与 `/onlyne settings` 里 `key_scope` 的提示原文也是「project：密钥存 .onlyne/keys（git 内需 ignore）」（`src/ui/bootstrap.ts:38`、`src/ui/settings.ts:36`）。

### 2. 根因链条

`packages/omp-onlyne/src/runtime.ts:108` 传的是 `dirname(this.config.path)`，而 `config.path` 是 `<cwd>/.onlyne/config.toml`，`dirname` 得到 `<cwd>/.onlyne`。`ensureKeyDir` 的 `cwd` 参数按项目根处理，`packages/omp-onlyne/src/keys.ts:13-15` 的 `projectKeysRoot` 又是 `join(cwd, ".onlyne", "keys")`，于是叠成两层。

连带影响：`keys.ts:83` 那道「git work tree 里必须被 ignore」的防护，也是按叠出来的路径去判定的，判定对象不是真实落地路径。

### 3. 修复建议

传项目根（`dirname(dirname(config.path))`，或直接把 `ctx.cwd` 一路带下来），保证密钥落在 README 承诺的 `.onlyne/keys`。

---

## 问题 5：出站判定完全没有日志（诊断缺口）

`packages/omp-onlyne/src/messages.ts:73` 只为**入站**记 `onlyne.judge.answered`。出站这一路没有任何一行记录 judge 的答案。

问题 1 里我只能靠回执的 `thread_ref.owner` 反推「judge 选了本地线程」，日志里查不到它选了哪个线程、选了哪个 target、选了哪个 plan。这类「judge 误判导致行为偏移」的问题，目前只能靠猜。

建议在 `JudgeDecider.decide()` 拿到 answers 之后补一行 info 日志，至少包含 `action`、`target`、`thread`、`response`、`execution` 和耗时。

---

## 附：本次确认正常、不要误修的行为

- **回复落到自己拥有的线程是设计内行为**。dpdp 的首条消息在本节点建出线程 `8e3cd06b`（owner 是 caerulea），我的回信 seq 2 由 `#admitHere` 追加在这里，dpdp 侧通过 reconcile 拉取。这条路径没问题；问题 1 说的是**无关系的、发给第三方的 send** 也被同样处理。
- **域内没有群发原语**。`packages/lib/src/types.ts:132` 的 domain 动作只有 `create / join / leave / list / describe / discover`，域内广播只能逐个成员发送。这一点是设计现状，不算缺陷，但值得在文档里写明，否则「向域内广播」这个说法会被误解成一个 API。

---

## 复现清单

1. **问题 1**：本地已存在任意一个线程时，`msg({delivery_kind: "…", msg: "…", meta: {to: "urn:onlyne:agent:cyanea"}})` → 回执 `thread_ref.owner` 为本节点，日志无 `onlyne.outbound.admitted`。
2. **问题 2**：让 judge 选 `execution: existing` 向任一 key 模式对端发送 → 以 `A node signature is required.` 失败；补 `meta.context` 即绕过。
3. **问题 3**：在信任握手完成之前向对端发送 → `refused / unauthenticated`；握手完成后重发 → `accepted / durable`，原记录不会被自动重投。

## 未验证项

- 问题 1 中 judge 具体选了哪个线程、为何两次都选中，因缺少出站日志无法直接取证，只能由回执字段反推。
- 问题 3 的重试窗口行为（`retry_until`）在 `refused` 路径上是否还有别的补偿机制，未逐行确认；`recover()` 的过滤条件是明确的，但宿主是否另有重投入口没有查。
