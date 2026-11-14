import { formatVars, getLocale, pickText } from "../i18n.ts";

const MESSAGES = {
	"judge.unavailable": [
		"SystemOne judge could not run ({reason}). Configure [judge] in .onlyne/config.toml or pass explicit meta fields (to, relation, thread, response, execution).",
		"SystemOne 判定器无法运行（{reason}）。请在 .onlyne/config.toml 中配置 [judge]，或在 meta 中显式传入字段（to、relation、thread、response、execution）。",
	],
	"error.relation_needs_reference": [
		"relation ('reply' or 'followup') requires meta.in_reply_to.",
		"relation（'reply' 或 'followup'）需要同时提供 meta.in_reply_to。",
	],
	"error.reply_authority": [
		"Reply authority is {authority}, which conflicts with meta.to={target}.",
		"回复权威节点是 {authority}，与 meta.to={target} 冲突。",
	],
	"error.delivery_target": [
		"Could not determine delivery target; pass meta.to with a reachable peer id or alias.",
		"无法确定投递目标，请在 meta.to 中指定可达的对端节点 ID 或别名。",
	],
	"error.execution_context_missing": [
		"Target '{to}' has no matching existing execution context; pass meta.context or set meta.execution='new'.",
		"目标 '{to}' 没有匹配的既有执行上下文；请传入 meta.context 或改用 meta.execution='new'。",
	],
	"error.reply_relation": [
		"SystemOne could not resolve reply/followup relation.",
		"SystemOne 未能判定 reply/followup 关系。",
	],
	"error.no_receiver": [
		"No trusted or discovered peer receivers are available yet.",
		"当前没有可用的已信任或已发现对端节点。",
	],
	"error.target_not_published": [
		"Judge picked unknown target '{chosen}'.",
		"判定器选择了未知目标 '{chosen}'。",
	],
	"error.contexts_unreadable": [
		"Target contexts are unreadable ({reason}); provide meta.context explicitly.",
		"目标节点上下文列表不可读（{reason}）；请显式传入 meta.context。",
	],
	"error.context_not_published": [
		"Context '{alias}' is not published or active on the target node.",
		"上下文 '{alias}' 未在目标节点发布或处于非活跃状态。",
	],
	"error.thread_op": [
		"SystemOne could not select a thread operation.",
		"SystemOne 未能判定话题操作。",
	],
	"error.thread_op_target": [
		"Thread operation '{op}' requires meta.to.",
		"话题操作 '{op}' 需要 meta.to。",
	],
	"error.thread_title_required": [
		"Thread creation requires meta.title or msg.",
		"创建话题需要 meta.title 或消息标题。",
	],
	"error.domain_op": [
		"SystemOne could not select a domain operation.",
		"SystemOne 未能判定域操作。",
	],
	"error.domain_id": [
		"Domain operation '{op}' requires meta.domain_id.",
		"域操作 '{op}' 需要 meta.domain_id。",
	],
	"error.local_thread_choice": [
		"SystemOne could not choose a local thread; pass meta.thread explicitly.",
		"SystemOne 未能选定本地话题，请显式传入 meta.thread。",
	],
	"error.read_scope": [
		"SystemOne could not select a read scope.",
		"SystemOne 未能选定读取范围。",
	],
	"error.message_not_found": [
		"Message {thread}#{seq} was not found.",
		"未找到消息 {thread}#{seq}。",
	],
	"error.in_reply_to_object": [
		"meta.in_reply_to must be an object.",
		"meta.in_reply_to 必须是对象。",
	],
	"error.in_reply_to_shape": [
		"meta.in_reply_to must carry { sender, id } or { thread, seq }.",
		"meta.in_reply_to 必须包含 { sender, id } 或 { thread, seq }。",
	],
	"error.in_reply_to_seq": [
		"meta.in_reply_to.seq must be a non-negative integer string.",
		"meta.in_reply_to.seq 必须是非负整数字符串。",
	],
	"error.reply_reference_conflict": [
		"Message belongs to {actual}, not {named}.",
		"该消息属于 {actual}，而非 {named}。",
	],
	"error.message_thread_conflict": [
		"meta.thread ({thread}) conflicts with meta.message.thread ({embedded}).",
		"meta.thread（{thread}）与 meta.message.thread（{embedded}）冲突。",
	],
	"error.thread_ref_invalid": [
		"Invalid thread reference: {raw}.",
		"无效的话题引用：{raw}。",
	],
	"error.thread_not_found": [
		"Thread '{alias}' was not found.",
		"找不到话题 '{alias}'。",
	],
	"error.peer_ambiguous": [
		"Peer alias '{raw}' matches multiple nodes: {matches}.",
		"节点别名 '{raw}' 匹配多个节点：{matches}。",
	],
	"error.peer_not_found": [
		"Unknown peer '{raw}'.",
		"未知对端节点 '{raw}'。",
	],
	"error.guard_write": [
		"Subagent contexts are restricted to reading their authorized slice.",
		"子对话上下文仅限读取已授权的消息切片，禁止写操作。",
	],
	"error.resources_main_only": [
		"Resource reads belong to the main session only.",
		"资源读取仅限主会话执行。",
	],
	"error.guard_node_listing": [
		"Node-wide listings belong to the main session only.",
		"全节点列表视图仅限主会话访问。",
	],
	"error.guard_evidence": [
		"Evidence read is outside this subagent context's source message.",
		"证据读取超出该子对话上下文对应的源消息范围。",
	],
	"error.guard_message": [
		"Message read is outside this subagent context's authorized slice.",
		"消息读取超出该子对话上下文的授权切片。",
	],
	"error.guard_thread_required": [
		"Subagent thread read requires an authorized thread reference.",
		"子对话读取必须限定在已授权的话题引用内。",
	],
	"error.guard_no_slice": [
		"Subagent context has no authorization for thread '{thread}'.",
		"子对话上下文无权访问话题 '{thread}'。",
	],
	"error.registry_unreadable": [
		"Context registry could not be read: {error}",
		"上下文注册表不可读：{error}",
	],
	"error.todo_associate": [
		"SystemOne could not pick a matching todo item to associate.",
		"SystemOne 未能匹配可关联的待办项。",
	],
	"error.inbound_context": [
		"SystemOne could not choose a valid execution context for inbound delivery.",
		"SystemOne 未能为入站消息选定有效执行上下文。",
	],
	"error.intent_unknown": [
		"SystemOne could not classify outbound intent.",
		"SystemOne 未能判定出站意图。",
	],
	"error.intent_unavailable": [
		"SystemOne could not resolve explicit or judged fields for outbound delivery ({reason}).",
		"SystemOne 无法解析出站消息目标或意图（{reason}）。",
	],
	"judge.o1.instructions": [
		"Classify what the caller wants Onlyne to do right now.",
		"判定调用方此刻希望 Onlyne 执行的操作类型。",
	],
	"judge.o1.communicate": [
		"Send a message, question, answer, handoff, or notification to a peer agent.",
		"向对端节点发送消息、提问、答复、交接或通知。",
	],
	"judge.o1.inspect": [
		"Read local or remote Onlyne state (neighbors, threads, messages, inbox, evidence, directory).",
		"读取本地或远端 Onlyne 状态（邻居、话题、消息、收件箱、送达证据、目录）。",
	],
	"judge.o1.manage_thread": [
		"Create, close, list, describe, or update membership of a thread.",
		"创建、关闭、列出、查询详情或调整话题成员。",
	],
	"judge.o1.manage_domain": [
		"Create, join, leave, list, describe, or discover a routing domain.",
		"创建、加入、退出、列出、查看详情或发现域。",
	],
	"judge.o1.operate_resource": [
		"Invoke an operation directly on a declared resource (IM gateway, compute service, etc.) using meta.resource and meta.operation.",
		"通过 meta.resource 与 meta.operation 直接操作已声明的资源（如 IM 网关、算力服务等）。",
	],
	"judge.o2a.original.instructions": [
		"Decide whether this outbound message directly answers one of the listed unanswered inbound entries, or starts a new message.",
		"判定该出站消息是在直接回复列表中的哪条未答复入站消息，还是发起新消息。",
	],
	"judge.o2a.original.new_message": [
		"Starts a new message or topic rather than directly answering an unanswered inbound entry.",
		"发起新消息或新沟通，而非直接回复列表中的未答复条目。",
	],
	"judge.o2a.original.candidate": [
		"Directly answers this inbound entry.",
		"直接回复这条入站消息。",
	],
	"judge.o2a.relation.instructions": [
		"Choose whether the message answers the referenced message ('reply') or opens a related follow-up ('followup').",
		"选择该消息是对引用消息的直接回答（'reply'），还是相关跟进（'followup'）。",
	],
	"judge.o2a.relation.reply": [
		"Direct reply fulfilling the referenced message.",
		"直接回答或完成所引用的消息。",
	],
	"judge.o2a.relation.followup": [
		"Follow-up referencing the earlier message without consuming a single answer obligation.",
		"作为关联跟进引用前序消息。",
	],
	"judge.target.instructions": [
		"Select which known peer node should receive this message.",
		"从已知对端节点中选择本次投递的目标。",
	],
	"judge.target.candidate": [
		"Deliver to this peer node.",
		"投递到此对端节点。",
	],
	"judge.target.none": [
		"None of the listed peers fits unambiguously.",
		"列表中没有明确匹配的目标节点。",
	],
	"judge.plan.response.instructions": [
		"Pick response expectation: 'await' for synchronous blocking wait when immediate answer is required, 'async' to ask the peer to reply later, or 'none' for notifications.",
		"选择响应期待：需要立即阻塞等待答复时选 'await'，允许对端稍后回复选 'async'，纯通知选 'none'。",
	],
	"judge.plan.response.await": [
		"Caller waits synchronously right here for the peer's answer.",
		"调用方需要在此同步阻塞等待对端答复。",
	],
	"judge.plan.response.async": [
		"Peer handles asynchronously and notifies back when ready.",
		"对端异步处理并在完成后回复通知。",
	],
	"judge.plan.response.none": [
		"One-way notification or FYI; no reply expected.",
		"单向通知或知会，不期待回复。",
	],
	"judge.plan.execution.instructions": [
		"Pick whether the receiver should handle this in an existing conversation context or start a fresh context.",
		"选择接收方应复用已有对话上下文（'existing'），还是开启全新上下文（'new'）。",
	],
	"judge.plan.execution.existing": [
		"Continue inside the matching existing context.",
		"在匹配的既有上下文中继续处理。",
	],
	"judge.plan.execution.new": [
		"Start an isolated new task/conversation context.",
		"开启一个独立的新任务/对话上下文。",
	],
	"judge.context.instructions": [
		"Select which published active context on the target node should receive this delivery.",
		"从目标节点发布的活跃上下文中选择接收目标。",
	],
	"judge.context.candidate": [
		"Route to this active receiver context.",
		"路由到此活跃上下文。",
	],
	"judge.thread.instructions": [
		"Select an existing Onlyne thread when continuing an established topic, or 'new' to open a thread.",
		"选择要继续的已有话题，或选 'new' 创建新话题。",
	],
	"judge.thread.candidate": [
		"Continue inside this existing thread.",
		"在此已有话题中继续。",
	],
	"judge.thread.new": [
		"Open a fresh thread.",
		"开启一个新话题。",
	],
	"judge.read.instructions": [
		"Select what Onlyne view or state scope to inspect.",
		"选择要读取检查的 Onlyne 视图或范围。",
	],
	"judge.read.directory": [
		"Read reachable discovery directory and known endpoints.",
		"读取自动发现目录与已知节点地址。",
	],
	"judge.read.neighbors": [
		"Read trusted peers, pending key requests, and reachability.",
		"读取邻居对端列表、待处理信任与可达性。",
	],
	"judge.read.threads": [
		"List owned and joined threads.",
		"列出本地拥有与加入的话题。",
	],
	"judge.read.inbox": [
		"Read pending and recent inbound inbox obligations.",
		"读取待处理及近期入站收件箱消息。",
	],
	"judge.read.messages": [
		"Read messages inside a thread.",
		"读取指定话题下的消息列表。",
	],
	"judge.read.message": [
		"Inspect one specific message.",
		"检查单条指定消息。",
	],
	"judge.read.evidence": [
		"Inspect outbound delivery receipt and settlement evidence.",
		"检查出站送达回执与结算证据。",
	],
	"judge.read.resources": [
		"Read declared local/remote resource inventory, logs, or operations.",
		"读取已声明资源的目录、事件日志或操作状态。",
	],
	"judge.thread_op.instructions": [
		"Pick the thread operation to execute.",
		"选择要执行的话题管理操作。",
	],
	"judge.domain_op.instructions": [
		"Pick the domain topology operation to execute.",
		"选择要执行的域管理操作。",
	],
	"judge.inbound.route.instructions": [
		"Decide which local execution context should receive this inbound Onlyne message.",
		"判定此入站 Onlyne 消息应投递到哪个本地执行上下文。",
	],
	"judge.inbound.route.main": [
		"Route to the main active Antigravity conversation.",
		"投递到主 Antigravity 会话。",
	],
	"judge.inbound.route.new_context": [
		"Spawn a new isolated task/subagent context for this inbound work.",
		"为该入站任务创建新的独立子上下文。",
	],
	"judge.inbound.route.existing": [
		"Continue inside this bound context.",
		"继续投递到该已绑定的上下文。",
	],
	"judge.todo.action.instructions": [
		"Decide whether this inbound message requires tracking a new Onlyne todo, updating an existing one, or none.",
		"判定该入站消息是否需要新增一条 Onlyne 待办、关联已有待办，或无需记录。",
	],
	"judge.todo.action.none": [
		"Informational, acknowledgment, or already complete; no todo change needed.",
		"仅知会、确认或已完成，无需记录待办。",
	],
	"judge.todo.action.associate": [
		"Relates to an existing open todo item.",
		"与现有待办事项相关。",
	],
	"judge.todo.action.append": [
		"Introduces new actionable work to track.",
		"引入了需要跟进的新任务事项。",
	],
	"judge.todo.associate.instructions": [
		"Pick the existing todo item this message relates to.",
		"选择该消息对应的现有待办项。",
	],
	"judge.todo.associate.item": [
		"Associate with this existing todo item.",
		"关联到该待办项。",
	],
	"judge.todo.title.instructions": [
		"Pick or synthesize a concise actionable todo title from the message.",
		"从消息内容中选择或概括简短的待办标题。",
	],
} as const;

export type DeciderTextKey = keyof typeof MESSAGES;

export function text(key: DeciderTextKey, vars?: Record<string, string | number>): string {
	const [en, zh] = MESSAGES[key];
	return formatVars(pickText(en, zh, getLocale()), vars);
}
