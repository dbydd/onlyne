# 审计与重构:解析与目标投递(2026-10-28)

依据:三份并行代码审计(lib / omp-onlyne / tern-onlyne)、5 张未修缺陷单、tern 侧 4 张 issue、近期提交序列。行号为审计时点,后续会漂移,但结论由结构本身支撑。

## 实施状态(2026-10-28 当日)

第 1-5 步已落地,第 6 步(还债)未动:

| 步 | 状态 | 落点 |
|---|---|---|
| 1 resolve + 回执 | ✅ | `packages/lib/src/client.ts`(send() 信任行预检、`#ensureMember` 返回成员行、`#pushGrant` 按 revision 直读 notice、出站记录 `addressee`/`requested`、outcome `resolved`);`packages/lib/tests/verify.ts` phaseAddressee(跨节点 peer-inbox 断言)+ phaseInitiateTrust |
| 2 归一化统一 | ✅ | omp `#reply()` 的 ref 与 authority 都带解析后 id;tern `#replyRef()`/`#peerId()` 同语义 |
| 3 O-2A + 引用写法 | ✅ | `DecisionScope` 经 `#replyCandidates` 提供本上下文可见候选,judge 的 `original` 问题选原消息,插件复制 `entry.reply_authority`;`#reply` 已知冲突预检(`error.reply_reference_conflict`);tool-text 示例统一 `{thread,seq}`(en/zh) |
| 4 网关上下文目录 | ✅ | `createImGateway` 发布 `executionContexts`,配置键 `contexts`,默认 `["main"]`;im-gateway tests 5 条回归 |
| 5 地址权威收敛 | ✅ | `packages/lib/src/resolve.ts` `composeResolver`:discovery 先答,not_found/unavailable 时信任行 `base_url` 兜底并经 `observe` 写回发现目录(source=`trust-row`);node.ts 组装;四值错误码保持 |
| 6 还债 | ⬜ | decider 拆 resolve/questions/plan/guard、client/http 拆文件、tern vendor→共享包:未动 |

新增回归(全部通过):`client.msg.peer-inbox`(主动发起真的到对端 inbox)、`client.msg.named`(resolved=对端,即便拼写一致)、`initiate.trust.paired`、`initiate.trust.deleted`(not_found)、`initiate.trust.deleted.no-retry`(无重试记录)、`address.trust-fallback`(+`.written`,信任行兜底并写回);omp 侧 O-2A 四条、引用归一与冲突预检两条。五张缺陷单已按 issues 规则删除,历史在 git。

防复发两条规则(见文末)自本日起生效。

## 结论

判断成立:近期补丁在堆叠。13 条近期提交里 9 条落在投递/解析链,每条修一种「形态」(idle 会话、busy 会话、trigger、lost context、handover、owner 改名、回执措辞……)。形态是枚举出来的,不是从不变量推出来的,所以组合空间没有边界——每出现一种新组合就再加一个 if。

根因只有一个:**「发给谁、落在哪、怎么送」这三个问题在代码里没有各自的唯一权威。**身份有三个来源(`meta.to`、线程 owner、decider 的 plan),地址有三套来源(trust 行 `base_url`、discovery 记录、宿主 config),落点有四条平行路径(permit / chainLanding / default / inboundThread)。它们散在 lib、宿主 decider、接收端三处,互相覆盖且不报错。5 张未修单全部是这个根因的不同投影。

## 一、证据

### 1.1 「发给谁」的三个权威源互相覆盖

| 权威源 | 位置 | 行为 |
|---|---|---|
| decider 的 plan/`action.to` | `packages/omp-onlyne/src/decider.ts:259,339` | 判定「回复什么/发给谁/怎么发」,产出 action.to |
| 线程 owner | `packages/lib/src/client.ts:297,305,332` | `send()` 里 `to` 被 `ref.owner` **静默覆盖**为 wire target |
| 接收端落点 | `packages/lib/src/messages.ts:94-121` | permit / chainLanding / default / inboundThread 四路平行选择 |

主动发起(自有线程 + `meta.to`)时线程 owner 是发送方自己,于是 wire target = 自己,`#transmit` 走本地 admit(`client.ts:889`),对端零记录——这就是 `issues/initiating-delivery-resolves-to-self.md`。回执还报请求值而非解析落点(`client.ts:899-911`),`accepted` 掩盖了「没出门」。

### 1.2 身份归一化有两条路径

`meta.to` 经 `#target`→`#peerId` 归一为 urn(`decider.ts:1365,1391`);`in_reply_to.sender` 走 `#reply()` 原样当 authority,`.catch(()=>sender)` 回落原文(`decider.ts:1439-1456`)。两条路径的产物用 `!==` 严格比较,短名与完整 urn 被判为两个目标——`issues/caerulea-reply-authority.md`。窄修已落一处(`decider.test.ts:241` 有回归),但 `.catch` 回落仍在,且 tern 侧 `#target` 原样返回 `reply.sender` 与 `meta.to`(`tern decider.ts:230-233`),同缺陷完整复现。

### 1.3 地址有三套来源、失败语义塌缩

- 投递路径只认 `plugins.resolve`(`transport.ts:122`),lib 不自带回退;
- omp 宿主 resolver 接 `discovery.resolve`(`discovery.ts:170-188`):只按名下已有记录逐个探测,记录 TTL 120s(`discovery.ts:8`),对端换地址后旧记录探测不通、新记录无写入口;
- `trust.base_url` 的回退只存在于交换路径 `keys.peerBase`(`keys.ts:243-245`),投递路径没有。

于是「信任行在、发现记录过期」→ `not_found`,可达性问题被报成「不存在」——`issues/trusted-peer-unaddressable.md` 根因 2、3 仍未解,也与 RFC 0011 §4、RFC 0018 §5.1「发现不决定信任」相悖。错误码 `ambiguous_target` 定义了四处(`types.ts:26`、`core.ts:9`、`client.ts:82`、`messages.ts:14`)却全库从未抛出,是死码。

### 1.4 形态枚举的规模

omp `decider.ts` 2103 行、43 个私有方法、7 种职责(判定、身份解析、引用解析、线程归属、权限守卫、证据装配、审计)挤在一个类里。按 `response × execution × session state × thread ownership` 枚举的分支点约 **45 处**(审计逐点清点),全部是 if,没有一处从不变量推导。`#threadOf`(`client.ts:452-465`)与 `chainLanding`(`messages.ts:219-244`)是同一问题(找原消息所在线程)的两套语义;斜杠口径三种(`#sendThread` indexOf、`#threadByAlias` split、`drafts.ts` lastIndexOf);分页循环三份;URL/回环校验三份;「唯一候选免问」重复五处。

### 1.5 测试把 bug 固化了

`packages/lib/tests/verify.ts:1697-1701` 的 phaseAddressee **断言** `receipt.thread_ref.owner === 发送方` 且不查对端 inbox——把「解析到自己」固化为预期行为。`packages/omp-onlyne/tests/convergence.test.ts:1-5` 自述「四种投递形态各跑一遍」,一形态一测试(`messages.test.ts:46-103`),测的是枚举表不是不变量。

### 1.6 tern-onlyne:平行实现必然分叉

tern 有自己的 CarlyDecider(403 行)走 HTTP SystemOne,omp 有 JudgeDecider(2103 行)走内嵌 judge。同一条 msg 两宿主可得到不同 target/plan。`server/vendor/omp-host/` 五个文件里 3 个已落后 omp 源(types 缺 `AcquisitionPolicy`/`port?`,bridge-text 缺 18 条 trust 文案),拷贝无来源版本注记。omp 侧修的归一化缺陷到不了 tern,tern 侧的现场缺陷(carly 两张单)也证明了这一点。

### 1.7 健康度

`bun run check` 全绿。类型与测试基建是好的——问题不在工程化,在结构。

## 二、设计:把形态空间还原成树

目标:解析与投递是一棵**有限、有层次**的树。每一层只回答一个问题、只有一个权威、只有一组封闭的错误码。新增「形态」不再是新 if,而是树里已有的一格。

```
L0 输入归一   normalize(meta)
              短名→urn 在此一步完成,后续所有层只见规范形
L1 身份层     resolveWho「发给谁」
              权威:解析后的唯一 node id
              ├ in_reply_to   → reply authority(同一归一路径)
              ├ meta.to       → 同一归一路径
              ├ thread.owner  → 只定存储归属,永不作收件人
              └ 不一致        → ambiguous_target(启用现有死码,报错不静默)
L2 落点层     resolveLanding「落在哪个线程」
              权威:单一 landing 解析(发送端 #threadOf 与接收端 chainLanding 合并)
              ├ 引用线程          → 该线程
              ├ 自有线程          → 成员+grant,wire = L1 身份(不是 owner)
              ├ 引用本地不可见    → untracked,接收方 chainLanding 判定
              └ 无线程            → startThread(owner=自己),wire = L1 身份
L3 上下文层   resolveContext「接收方在哪个执行上下文处理」
              ├ meta.context 显式
              ├ 目标发布的目录(网关必须发布,见 im-gateway-draft-context)
              └ 线程/绑定回退
              错误码:contexts_unreadable / unsupported_delivery
L4 地址层     resolveAddress「送到哪个 URL」
              权威:trust.base_url;discovery 只是 120s 缓存
              ├ 有信任行,可达   → base_url
              ├ 有信任行,不可达 → unavailable(可重试,不产生 24h 盲重试)
              ├ 无信任行         → not_found
              ├ 策略拒绝         → policy_denied;多实例 → conflict
              └ 对端换地址       → 信任行携带/陈述更新写回 discovery,对模型可见
L5 传输与回执 transmit
              accepted 仅在线路送达后;回执并列 requested(身份)与
              resolved(身份+落点+地址),digest.target_endpoint=resolved
```

形态空间 = 引用(有/无) × 线程归属(自有/对方/无) × execution(existing/new/默认),叶子 ≤ 18,全部走同一棵树。四个维度不再各自成 if 森林。

## 三、lib 层重构(packages/lib)

1. **新增 `src/resolve.ts`**:纯函数 `resolveTarget({to, in_reply_to, thread, plan, trust, discovery}) → {addressee, landing, wire_target}`。`client.ts` 的 `send()` 只此一处调用;三源不一致抛 `ambiguous_target`。`#threadOf` 与接收端 `chainLanding` 合并为一个 landing 模块(同一份语义,两侧共用)。
2. **回执改口径**:`#deliver`/`#transmit` 的 receipt 与 trace 并列 `requested` / `resolved`;`accepted` 只在线路送达后产生。删除「回执报线程持有者」的旧断言,替换为跨节点断言:发送后对端 inbox 必有条目(`tests/verify.ts` 补一组双端用例)。
3. **地址收敛**:`trust.base_url` 为权威地址,discovery 记录降级为可失效缓存;`transport.ts` 增加 trust 回退(与 `keys.peerBase` 合并为一条路径);失败码固定为 not_found / unavailable / policy_denied / conflict 四值,全库不得把可达性报成 not_found。对端换地址:信任陈述更新写回 discovery 的路径补上(RFC 0011 §4 本就要求)。
4. **错误码清理**:启用 `ambiguous_target`;`messages.ts` 的「No default thread」与「原史缺失」区分 not_found 与 context_lost;`/contexts` 缺目录维持 unsupported_delivery,但宿主侧(下条)保证网关发布目录。
5. **过载文件顺手拆**:`client.ts`(1712 行)按 send/receive/maintain 三面拆;`http.ts`(852 行)路由表按资源域拆。此项还债,不阻塞 1-4。

## 四、宿主层重构(packages/omp-onlyne)

1. **decider.ts 拆四个模块**(目标形态,不是一步到位):
   - `resolve/`:`peerId/target/reply/messageRef/thread` 统一为「别名→稳定身份」一条路,一种斜杠口径,一种 not_found 语义;
   - `questions/`:RFC 0016 的每个 O/I 判定点一个问题模板 + 候选构造;「唯一候选免问」与「meta 继承」收敛为一个公共 preflight;
   - `plan/`:`#deliveryPlan + #sendThread + reuseThread` 合成单一 `Decision` 数据类型(response × execution × 线程归属是它的三个字段,不是 45 个 if);
   - `guard + record`:守卫与审计。
2. **DecisionScope 绑定本次入站 MessageRef**:由 `messages.ts:409` 的 `details.message_ref` 注入 `DecisionScope.inboundRef`,实现 O-2A 原消息选择(候选仅限本次调用可见的来信;证据不充分仍提问,不自动绑定——遵循 RFC 0016 §7)。这同时消解 `decider-auto-in-reply-to.md`:模型不再需要手填 UUID。
3. **引用写法统一到 `{thread,seq}`**:以 `contracts/omp.md` 为准修正 `tool-text.ts` 的示例;`{sender,id}` 分支补已知冲突预检(在 outbound 写入与签名之前,失败返回候选而非静默改写;本地不可见时保留回复权威路径照常发送——不设硬拒)。
4. `urls.ts`(1488 行)与 `ui/`(约 2900 行)独立于判定链,只做边界整理,不在本次范围。

## 五、tern-onlyne 收敛(external-integrations/tern-onlyne)

1. 短期:vendor 文件加头注「拷贝自 omp-onlyne@<提交>」,加一致性检查脚本(diff 不一致即 CI 失败)。
2. 中期:把 omp-host 层(`node/store/types/host-text`)抽成可发布包(如 `@onlyne/host-bun`),omp 与 tern 同源依赖;`AddressJudge`/`evaluateInbound` 等纯函数上移共享(omp `acquire.ts` 与 tern `acquisition.ts` 已是双胞胎)。
3. 长期:确定性解析(landing、归一、地址回退)全部下沉 lib;各宿主只保留语义判定(judge)。否则 lib 每修一次解析类缺陷都要在 tern 复现一次——本次 carly 两张现场单就是代价实例。

## 六、实施顺序(每步可独立合并,每步关闭对应缺陷单)

| 步 | 内容 | 消解 |
|---|---|---|
| 1 | lib `resolve.ts` + 回执 requested/resolved + 跨节点断言 | initiating-delivery-resolves-to-self 全部;trusted-peer 的「回执误导」面 |
| 2 | 归一化统一一条路径 + 启用 ambiguous_target(lib+omp+tern) | caerulea-reply-authority |
| 3 | DecisionScope.inboundRef + O-2A + tool-text 统一 {thread,seq} | decider-auto-in-reply-to |
| 4 | 网关发布上下文目录 + 草稿发送回退 | im-gateway-draft-context |
| 5 | 地址权威收敛(trust 权威、discovery 缓存、四值错误码、换地址写回) | trusted-peer-unaddressable 剩余两条 |
| 6 | 还债:decider 拆模块、client/http 拆文件、vendor→共享包 | 结构性,防复发 |

每步验收:`bun run check` + `bun run test` 全绿 + 对应缺陷单的「验收判据」逐条通过 + 删除该单。

## 七、防复发的两条规则

1. **投递链上禁止新的形态 if**:凡是要按 response/execution/归属/session state 分支的改动,必须先把它表达为 resolve.ts 树上的一格(或 Decision 数据类型的新字段),review 时以「这格在树里吗」为准。
2. **回执与测试必须穿端到端**:回执只报 resolved 落点;新增投递行为的回归必须断言对端可见状态(inbox/attention),不许只断言本端回执字段。

## 八、judge 判定面的实测与调优(2026-10-28 当日,晚于第 1-5 步)

做法:真 judge 样例集。omp 侧经 `DeciderDeps.judge` 缝隙把 SystemOne(`http://127.0.0.1:3425/v1/systemone`,model `group/decider`)接进 `JudgeDecider`——插件构造真实问题、真模型作答,除传输层外全路径为真;tern 侧 `CarlyDecider` 原生走同一端点。脚本:`scripts/judge-samples.ts`(omp,36 场景)与 `external-integrations/tern-onlyne/scripts/judge-samples.ts`(23 场景),结果 JSON 在 `/tmp/judge-samples/`。

调优前→后:

| 面 | 调优前 | 调优后 |
|---|---|---|
| omp 全部(36) | 波动 26→31 pass,inspect 面翻转、routing 偏置 | **31 pass / 0 fail,两轮一致** |
| tern 全部(23) | 18 pass / 2 mismatch / 1 error | **20 pass / 0 mismatch**(1 条 record-only 边界) |
| tern 紧急修正 needsAnswer | 0.506(刀锋) | **0.874** |
| tern scope-op | 50% | **100%** |

改动的判据(全部「一句话判据」式,无代码分支):omp `judge.o1.inspect` 补邻居列表措辞(与 manage_domain 的翻转源)、`judge.o1.communicate` 补「开新任务/物理动词也是 communicate」、`judge.o1.manage_thread` 补排除句、I-0 `needs_answer` 补「明说不用回一律 no」、I-1 routing 补「无需答复且非自成一体工作项一律 main_agent」、relation.followup 补致谢确认的归属;omp+tern 的 intent 同步。tern 另修 `#op` 的 state 硬编码 `meta: null`(judge 看不到消息,近均匀分布的根因)+ 9 条 op 判据 + `NoulQuestion.criteria` 双键收紧(服务端契约)。omp `#targetQuestion` 发布 `none` 弃权选项(处理分支本已存在),无指向的目标浮现为 `ambiguous_target` 而非低置信度随机投递。

遗留观察(记录,未做):判定低置信度(confidence 0.03-0.5)照常生效,可考虑阈值转 `decision_unavailable`;zh 提示词 A/B 未测;I-1「no 且无匹配上下文机械落 main」的机械回退候选。
