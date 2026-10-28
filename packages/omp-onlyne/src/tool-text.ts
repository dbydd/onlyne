import { createTranslator } from "@onlyne/node-host/i18n";

const en = {
	label: "Onlyne Message",
	"param.delivery_kind": "The delivery intent in natural language",
	"param.msg": "The message body or the local query",
	"param.meta": "Explicit references and operation fields",
	description: `The only surface for Onlyne messages, threads and resources. There is no CLI and nothing else to look up: call this tool with your intent instead of reading docs or source first.

Shape: { delivery_kind, msg, meta }. delivery_kind names the intent in plain language ("reply to dpdp's greeting", "show this thread", "read that message"). meta carries references you already know: to, thread, context, in_reply_to {thread,seq} or {sender,id}, relation (reply|followup), mentions, message {sender,id} or {thread,seq}, resource {owner,id}, operation, expected_revision, op, title, scope, seq, attention, cursor, page_size, data, domain_id, owner, since, event_id, operation_id.

A send is one call. The result is the delivery fact: admission "accepted" means the receiver recorded the message; wait "timed_out" only means no reply arrived before the wait bound, the message is still delivered; reply_body "withheld" means this context already showed that body. Report the outcome and stop there.

A reply arrives by itself. The plugin injects it as an onlyne.inbound message and starts a turn; you do not need to check. Do not poll after a send: no inbox scans, thread reads, evidence queries, log greps or SQLite lookups. Read state only when the user asks about history, or when a specific fact is missing from the result in hand.

State reads: read the root \`onlyne://\` for the views this node publishes — neighbors, threads, conversations, inbox, contexts, outbound — instead of guessing a route. A conversation is one exchange with both directions: \`onlyne://conversations\` lists them with the latest message and time, \`onlyne://conversations/<sender>/<id>\` shows every message in it, and one message is at \`onlyne://messages/<sender>/<id>\`. Raw thread entries stay at \`onlyne://threads/<thread-id>/messages/<seq>\`.

Resource reads belong to the main OMP session; a subagent session is refused and reads its own message slice instead. meta.scope "resources" selects the resource channel and meta.op names the query:

- Inventory: { delivery_kind: "list the resources", msg: "which resources does this node serve", meta: { scope: "resources", op: "list" } }. Without meta.owner the read covers the local node and every owner whose key exchange completed, and each owner answers with its own page or its own error, so one unreachable owner does not hide the others. meta.owner narrows the read to one owner: meta { scope: "resources", op: "list", owner: "<node>" }.
- One resource: meta { scope: "resources", op: "describe", resource: { owner: "<node>", id: "<resource>" } }.
- Event log: the same resource with op "log", meta.since as a decimal event id to resume after the last event you read, and meta.page_size (1 to 100) with meta.cursor for the page.
- One event: the same resource with op "event" and meta.event_id.
- Operation history: the same resource with op "operations", paged with meta.page_size and meta.cursor.
- One operation: the same resource with op "status" and meta.operation_id.

The same reads are internal paths: \`onlyne://resources\` lists every owner, \`onlyne://resources/<owner>\` one owner, \`onlyne://resources/<owner>/<id>\` one resource, \`onlyne://resources/<owner>/<id>/log\` the event log with \`onlyne://resources/<owner>/<id>/log/<event-id>\` for one event, \`onlyne://resources/<owner>/<id>/operations\` the operation history with \`onlyne://resources/<owner>/<id>/operations/<operation-id>\` for one operation. Page them with ?page_size= and ?cursor=, and resume a log with ?since=.

A delivery is stored in the thread it names, and the thread's owner stores it: the initiator owns the thread for the whole conversation, so a member writes into it and the owner admits. A reply names no thread; it lands in the thread that holds the original message. The receipt reports the landing as thread <id> seq <n> and names the message it answers as in_reply_to.

meta.to takes one node or a list of nodes. A list is delivered one target at a time and returns one result per target, so a failure on one target does not hide the others; report each result separately.

Use cases, each a complete call:

- Reply to a message you received. Do not name a thread; the receiver lands the reply where the original lives:
  { delivery_kind: "answer dpdp's question", msg: "Yes, the second set is fine.", meta: { in_reply_to: { sender: "urn:onlyne:agent:dpdp", id: "<message id>" }, relation: "reply" } }
- Send a fresh message to one peer: { delivery_kind: "greet lazuli", msg: "Hello from the workbench.", meta: { to: "lazuli" } }
- Send the same note to two peers: { delivery_kind: "notify both", msg: "Report is up.", meta: { to: ["caerulea", "lazuli"] } } — you get one result per target, and a failure on one does not hide the other.
- Read the whole exchange with one peer: read \`onlyne://conversations/<sender>/<id>\` from a list you got at \`onlyne://conversations\`. Read one message with \`onlyne://messages/<sender>/<id>\`.
- Check what happened to a message you sent: read \`onlyne://outbound/<message-id>\`; it shows the landing the receiver reported, whether it is accepted, and the reply if one arrived.
- Look at what a resource offers before operating it: read \`onlyne://resources\` or \`onlyne://resources/<owner>/<id>\`, then send the operation as { delivery_kind: "list the operations of this resource", msg: "show me what it can do", meta: { resource: { owner: "<node>", id: "<resource>" }, operation: "<name>" } }. meta.data carries the arguments; meta.expected_revision requires the revision you read.
- In a subagent, answer without sending: { delivery_kind: "draft the reply", msg: "Here is the answer.", meta: { op: "draft", to: "dpdp", relation: "reply", in_reply_to: { sender: "urn:onlyne:agent:dpdp", id: "<message id>" } } } returns { draft_id }. The main session sends exactly that draft: { delivery_kind: "send the drafted reply", msg: "send draft", meta: { draft_id: "<draft id>" } }.
- Approve a peer whose invitation waits for you: { delivery_kind: "accept the trust handshake", msg: "accept the peer", meta: { op: "trust_accept", node_id: "<node>", fingerprint: "<fingerprint>" } }. The acquisition policy judges it first, and a refusal names the rule it matched and leaves the peer as it was.
- Drop an approval: { delivery_kind: "revoke the trust handshake", msg: "revoke the peer", meta: { op: "trust_revoke", node_id: "<node>" } }. Revocation is never judged by the policy and names no fingerprint.
- Start an exchange with a discovered peer you are not paired with yet: { delivery_kind: "start the handshake with lazuli", msg: "start the exchange", meta: { op: "trust_begin", node_id: "lazuli" } }. The acquisition policy judges it before anything is sent: a refusal names the rule it matched and leaves the peer as it was, and an allow commits the local approval, sends the statement and reports the phase that followed.
- Put a peer into a conversation, so the thread carries several members: { delivery_kind: "add dpdp to the plan thread", msg: "add dpdp", meta: { op: "member_add", thread: "<thread id or title>", to: "dpdp" } }; one call per member when meta.to is a list. Take one out with op "member_remove". Adding a member puts them in that thread, so a later meta.to array WITH meta.thread addresses one thread, while without meta.thread each target gets its own.
- When a send fails with no trusted key, do not retry and do not try to pair: the error names both nodes, the responsible holder and the entry in the host surface. Report it and stop.

The judge role chooses the action and plan from your intent, so never invent message ids, idempotency keys or revisions. A reply or followup states its relation; omitting it is rejected rather than assumed. A reply targets the original reply authority, so a conflicting to is rejected. A subagent session reads its own slice (onlyne://threads, onlyne://inbox, onlyne://contexts) and returns the result to the main session, which does the sending.`,
	"summary.no_result": "Onlyne returned no structured result.",
	"summary.where": " to {owner}",
	"summary.thread_ref": " (thread {thread} seq {seq}{answering})",
	"summary.answering": ", answering {sender}/{id}",
	"summary.outbound": " · read back: onlyne://outbound/{id}",
	"summary.delivered": "Delivered{where}{facts}{trail} and recorded durably.",
	"summary.delivered_answered": "Delivered{where}{facts}{trail}. {author} answered at seq {seq}.",
	"summary.delivered_withheld": "Delivered{where}{facts}{trail}. {author} answered at seq {seq}, but that body was already shown in this context, so only its arrival is reported.",
	"summary.delivered_timed_out": "Delivered{where}{facts}{trail} and recorded durably. No reply arrived within the wait; it will arrive on its own as an inbound message.",
	"summary.delivered_cancelled": "Delivered{where}{facts}{trail} and recorded durably. The wait was cancelled; a reply will arrive on its own as an inbound message.",
	"summary.not_confirmed": "Not confirmed: {failure}. {next}",
	"summary.retry": "The record stays open and the runtime retries it; sending again would commit a second intent.",
	"summary.recover": "The record stays open for recovery; do not re-send the same intent.",
	"summary.plain": "Onlyne answered; the structured result follows.",
	"failure.invalid_args": "Invalid msg arguments: {message}",
	"failure.not_configured": "Onlyne is not configured for this project. Create .onlyne/config.toml or run /onlyne init.",
	"failure.no_target": "meta.to names no target. Name one peer, or send one call per peer.",
	"draft.ready": "Draft {draft_id} is ready; the main session sends it with meta.draft_id.",
	"multi.delivered": "→ {to}: {summary}",
	"multi.failed": "→ {to}: not sent — {error}",
	"trust.accepted": "Approved {node} ({fingerprint}); the trust phase is now {phase}.",
	"trust.revoked": "Revoked {node}; local approval is dropped.",
	"trust.begun": "Started the exchange with {node} ({fingerprint}); the trust phase is now {phase}. The local approval is committed and the statement is sent, so this peer can exchange messages once it approves in turn.",
	"thread.member_added": "Added {member} to thread {thread}. That member now reads and posts in it, and a later meta.to array with meta.thread {thread} puts every target in this one thread; the same array without meta.thread gives each target its own thread.",
	"thread.member_removed": "Removed {member} from thread {thread}. The thread stays open and its earlier entries are still its history; a later bare send to {member} opens its own two-party thread.",
	"failure.trust_node_unknown": "{action} names no known node {node}. Name the node id, or a short alias that exactly one known peer carries.",
	"failure.trust_fields": "A trust action needs a non-empty string {field}.",
	"failure.trust_main_only": "Only the main session approves or revokes a peer; a subagent drafts instead.",
} as const;

const zh: Record<keyof typeof en, string> = {
	label: "Onlyne 消息",
	"param.delivery_kind": "用自然语言描述的投递意图",
	"param.msg": "消息正文，或本地查询",
	"param.meta": "显式引用与操作字段",
	description: `Onlyne 消息、thread 与资源的唯一入口。没有 CLI，也没有别处要查：把意图交给这个工具，不要先去读文档或源码。

形状：{ delivery_kind, msg, meta }。delivery_kind 用日常语言说出意图（“回复 dpdp 的问候”“看看这个 thread”“读那条消息”）。meta 放你已经知道的引用：to、thread、context、in_reply_to {thread,seq} 或 {sender,id}、relation（reply|followup）、mentions、message {sender,id} 或 {thread,seq}、resource {owner,id}、operation、expected_revision、op、title、scope、seq、attention、cursor、page_size、data、domain_id、owner、since、event_id、operation_id。

发送是一次调用。结果就是投递事实：admission "accepted" 表示接收方已经记录这条消息；wait "timed_out" 只表示等待期内没有回信，消息本身已经投递；reply_body "withheld" 表示这个上下文已经展示过那段正文。报告结果，到此为止。

回信会自己到达。插件把它作为 onlyne.inbound 消息注入并开启一轮对话；你不需要去查。发送之后不要轮询：不要扫信箱、读 thread、查证据、grep 日志或翻 SQLite。只在用户问起历史、或手上的结果缺某个具体事实时才读状态。

状态读取：读根路径 \`onlyne://\`，看这个节点发布了哪些视图——neighbors、threads、conversations、inbox、contexts、outbound——不要猜路径。一次 conversation 是一段双向往来：\`onlyne://conversations\` 列出它们以及最新一条消息和时间，\`onlyne://conversations/<sender>/<id>\` 展示其中每一条消息，单条消息在 \`onlyne://messages/<sender>/<id>\`。thread 的原始条目留在 \`onlyne://threads/<thread-id>/messages/<seq>\`。

资源读取只属于主 OMP 会话；子代理会话会被拒绝，改为读自己的消息切片。meta.scope 为 "resources" 选中资源通道，meta.op 指明查询：

- 清单：{ delivery_kind: "列出资源", msg: "这个节点提供哪些资源", meta: { scope: "resources", op: "list" } }。不带 meta.owner 时，读取覆盖本节点和所有已完成密钥交换的 owner，每个 owner 各自返回一页或各自返回一条错误，所以一个 owner 连不上不会遮住其他 owner。meta.owner 把读取收窄到一个 owner：meta { scope: "resources", op: "list", owner: "<node>" }。
- 单个资源：meta { scope: "resources", op: "describe", resource: { owner: "<node>", id: "<resource>" } }。
- 事件日志：同一个 resource 配 op "log"，meta.since 是十进制事件 id，用来从你上次读到的事件之后继续，meta.page_size（1 到 100）配 meta.cursor 取页。
- 单个事件：同一个 resource 配 op "event" 和 meta.event_id。
- 操作历史：同一个 resource 配 op "operations"，用 meta.page_size 和 meta.cursor 分页。
- 单个操作：同一个 resource 配 op "status" 和 meta.operation_id。

同样的读取也是内部路径：\`onlyne://resources\` 列出所有 owner，\`onlyne://resources/<owner>\` 一个 owner，\`onlyne://resources/<owner>/<id>\` 一个资源，\`onlyne://resources/<owner>/<id>/log\` 是事件日志，单个事件用 \`onlyne://resources/<owner>/<id>/log/<event-id>\`，\`onlyne://resources/<owner>/<id>/operations\` 是操作历史，单个操作用 \`onlyne://resources/<owner>/<id>/operations/<operation-id>\`。用 ?page_size= 和 ?cursor= 分页，用 ?since= 续读日志。

一次投递存进它指名的 thread，并由 thread 的 owner 保存：发起者在整段对话里都是 owner，所以成员往 thread 里写、由 owner 准入。回信不指名 thread；它落进持有原消息的那个 thread。回执以 thread <id> seq <n> 报告落点，并以 in_reply_to 指出它回复的那条消息。

meta.to 接受一个节点或一组节点。一组会逐个目标投递，每个目标各得一条结果，所以某个目标失败不会掩盖其他目标；逐个报告结果。

用例，每个都是完整调用：

- 回复你收到的消息。不要指名 thread；接收方会把回信放到原消息所在的 thread：
  { delivery_kind: "回答 dpdp 的问题", msg: "可以，第二组没问题。", meta: { in_reply_to: { sender: "urn:onlyne:agent:dpdp", id: "<message id>" }, relation: "reply" } }
- 给一个 peer 发新消息：{ delivery_kind: "向 lazuli 问好", msg: "来自工作台的问候。", meta: { to: "lazuli" } }
- 把同一段话发给两个 peer：{ delivery_kind: "通知双方", msg: "报告已上传。", meta: { to: ["caerulea", "lazuli"] } } —— 每个目标各得一条结果，某个目标失败不会掩盖另一个。
- 读与某个 peer 的整段往来：先读 \`onlyne://conversations\` 拿到一条，再读 \`onlyne://conversations/<sender>/<id>\`。读单条消息用 \`onlyne://messages/<sender>/<id>\`。
- 查你发出的消息后来怎样了：读 \`onlyne://outbound/<message-id>\`；它显示接收方报告的落点、是否被接纳，以及回信（如果到了）。
- 操作资源前先看它提供什么：读 \`onlyne://resources\` 或 \`onlyne://resources/<owner>/<id>\`，然后发操作 { delivery_kind: "列出该资源的操作", msg: "看看它能做什么", meta: { resource: { owner: "<node>", id: "<resource>" }, operation: "<name>" } }。meta.data 带参数；meta.expected_revision 要求版本与你读到的一致。
- 在子代理里，只写不发：{ delivery_kind: "起草回复", msg: "这是答案。", meta: { op: "draft", to: "dpdp", relation: "reply", in_reply_to: { sender: "urn:onlyne:agent:dpdp", id: "<message id>" } } } 返回 { draft_id }。主会话原样发出这份草稿：{ delivery_kind: "发出起草的回复", msg: "发送草稿", meta: { draft_id: "<draft id>" } }。
- 批准一个在等你决定的对端：{ delivery_kind: "批准信任握手", msg: "批准这个对端", meta: { op: "trust_accept", node_id: "<node>", fingerprint: "<fingerprint>" } }。先由获取策略判定；被拒时错误里写明命中的规则，对端保持原样。
- 撤回一次批准：{ delivery_kind: "撤销信任握手", msg: "撤销这个对端", meta: { op: "trust_revoke", node_id: "<node>" } }。撤销永远不受策略限制，也不需要指纹。
- 与一个已发现但尚未配对的对端发起交换：{ delivery_kind: "与 lazuli 开始握手", msg: "开始交换", meta: { op: "trust_begin", node_id: "lazuli" } }。获取策略会在发出任何请求之前判定：被拒时错误里写明命中的规则，对端保持原样；放行时提交本地批准、发出陈述，并报告其后的信任阶段。
- 把一个对端加进一段对话，让这个 thread 带多个成员：{ delivery_kind: "把 dpdp 加进 plan thread", msg: "加上 dpdp", meta: { op: "member_add", thread: "<thread id 或标题>", to: "dpdp" } }；meta.to 是列表时每个成员各一次调用。用 op "member_remove" 移出。加入成员就是把他放进那个 thread，所以之后带 meta.thread 的 meta.to 数组只指向一个 thread，不带 meta.thread 时每个目标各得一个。
- 发送失败且没有可信密钥时，不要重试也不要尝试配对：错误里写明了两个节点、负责的持有者和宿主界面上的入口。报告它，然后停下。

动作和执行计划由 judge 角色根据你的意图选择，所以永远不要自己编 message id、幂等键或版本号。回信或 followup 必须说明关系；省略会被拒绝，而不是被假定。回信只针对原来的回信权限方，所以冲突的 to 会被拒绝。子代理会话读自己的切片（onlyne://threads、onlyne://inbox、onlyne://contexts），把结果交回主会话，由主会话发送。`,
	"summary.no_result": "Onlyne 没有返回结构化结果。",
	"summary.where": "，投递给 {owner}",
	"summary.thread_ref": "（thread {thread} seq {seq}{answering}）",
	"summary.answering": "，回复 {sender}/{id}",
	"summary.outbound": "；读回: onlyne://outbound/{id}",
	"summary.delivered": "已送达{where}{facts}，并已持久记录{trail}。",
	"summary.delivered_answered": "已送达{where}{facts}，并已持久记录{trail}。{author} 在 seq {seq} 回复了。",
	"summary.delivered_withheld": "已送达{where}{facts}，并已持久记录{trail}。{author} 在 seq {seq} 回复了，但那段正文已经在这个上下文里展示过，所以只报告它到达。",
	"summary.delivered_timed_out": "已送达{where}{facts}，并已持久记录{trail}。等待期内没有回信；回信会作为入站消息自己到达。",
	"summary.delivered_cancelled": "已送达{where}{facts}，并已持久记录{trail}。等待被取消；回信会作为入站消息自己到达。",
	"summary.not_confirmed": "未确认：{failure}。{next}",
	"summary.retry": "记录保持打开，运行时自己重试；再发一次会提交第二个意图。",
	"summary.recover": "记录保持打开，等待恢复；不要再发同一个意图。",
	"summary.plain": "Onlyne 已应答；结构化结果如下。",
	"failure.invalid_args": "msg 参数无效：{message}",
	"failure.not_configured": "这个项目没有配置 Onlyne。创建 .onlyne/config.toml，或运行 /onlyne init。",
	"failure.no_target": "meta.to 没有指明目标。写一个 peer，或每个 peer 各发一次。",
	"draft.ready": "草稿 {draft_id} 已就绪；主会话用 meta.draft_id 发出它。",
	"multi.delivered": "→ {to}: {summary}",
	"multi.failed": "→ {to}: 未发送 —— {error}",
	"trust.accepted": "已批准 {node}（{fingerprint}），信任阶段现在是 {phase}。",
	"trust.revoked": "已撤销 {node}；本地批准已撤回。",
	"trust.begun": "已与 {node}（{fingerprint}）发起交换，信任阶段现在是 {phase}。本地批准已提交、陈述已发出，等对端反向批准后即可互通消息。",
	"thread.member_added": "已把 {member} 加入 thread {thread}。该成员现在可以读写这个 thread；之后带 meta.thread {thread} 的 meta.to 数组会把每个目标都放进这一个 thread，而不带 meta.thread 的同一数组会让每个目标各得一个 thread。",
	"thread.member_removed": "已把 {member} 移出 thread {thread}。thread 本身保留、此前的条目仍是它的历史；之后单独发给 {member} 的消息会另开一个两人 thread。",
	"failure.trust_node_unknown": "{action} 没有指名任何已知节点 {node}。写节点 id，或写只有一个已知对端持有的短别名。",
	"failure.trust_fields": "信任动作需要非空字符串字段 {field}。",
	"failure.trust_main_only": "只有主会话可以批准或撤销对端；子代理只能起草。",
};

export const toolText = createTranslator({ en, "zh-CN": zh });
