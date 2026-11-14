import { formatVars, getLocale, pickText } from "../i18n.ts";

const MESSAGES = {
	description: [
		"Send an Onlyne message, reply/followup in a thread, manage threads/domains/resources, or inspect Onlyne state.",
		"发送 Onlyne 消息、在话题中回复/跟进、管理话题/域/资源，或检查 Onlyne 状态。",
	],
	paramDeliveryKind: [
		"Communicative intent tag such as 'question', 'answer', 'notify', 'handoff', or 'read'. SystemOne resolves routing and execution.",
		"通信意图标签，如 'question'、'answer'、'notify'、'handoff' 或 'read'。SystemOne 会解析路由与执行计划。",
	],
	paramMsg: [
		"Message body or structured instruction text.",
		"消息正文或结构化指令文本。",
	],
	paramMeta: [
		"Optional structured metadata: to, in_reply_to, relation, thread, context, response ('await'|'async'|'none'), execution ('existing'|'new'), mentions, work, op, etc.",
		"可选结构化元数据：to、in_reply_to、relation、thread、context、response（'await'|'async'|'none'）、execution（'existing'|'new'）、mentions、work、op 等。",
	],
	metaOpInvalid: [
		"meta.op must be a non-empty string.",
		"meta.op 必须是非空字符串。",
	],
	metaOpUnsupported: [
		"Unsupported meta.op '{op}'.",
		"不支持的 meta.op '{op}'。",
	],
	metaToItemInvalid: [
		"Every entry in meta.to must be a non-empty node id or alias.",
		"meta.to 中的每一项都必须是非空节点 ID 或别名。",
	],
	metaThreadRequired: [
		"Multi-recipient sends require an explicit meta.thread or an automatically created shared thread.",
		"向多个接收方发送需要显式 meta.thread 或新建共享话题。",
	],
	draftIdRequired: [
		"meta.draft_id must be a non-empty string.",
		"meta.draft_id 必须是非空字符串。",
	],
	draftNotFound: [
		"Draft '{id}' was not found or already sent.",
		"找不到草稿 '{id}'，或草稿已发送。",
	],
	draftCreated: [
		"Draft {id} created for {to}. Review or send with meta.op='send_draft'.",
		"已为 {to} 创建草稿 {id}。可通过 meta.op='send_draft' 检查或发送。",
	],
	trustTargetRequired: [
		"trust_{op} requires meta.to (peer node id or alias).",
		"trust_{op} 需要 meta.to（对端节点 ID 或别名）。",
	],
	trustRotateConfirmed: [
		"Rotated local identity key for {node}. New fingerprint: {fingerprint}.",
		"已轮换 {node} 的本地身份密钥。新指纹：{fingerprint}。",
	],
	trustAccepted: [
		"Accepted trust for {node} (phase: {phase}, fingerprint: {fingerprint}).",
		"已接受 {node} 的信任（阶段：{phase}，指纹：{fingerprint}）。",
	],
	trustRevoked: [
		"Revoked trust for {node} (phase: {phase}).",
		"已撤销 {node} 的信任（阶段：{phase}）。",
	],
	trustBegan: [
		"Trust handshake with {node} completed local confirmation (phase: {phase}, fingerprint: {fingerprint}).",
		"与 {node} 的信任握手已完成本地确认（阶段：{phase}，指纹：{fingerprint}）。",
	],
	trustPendingHuman: [
		"Key exchange initialized for {node} (fingerprint: {fingerprint}), waiting for human verification in Onlyne GUI/TUI.",
		"已初始化与 {node} 的密钥交换（指纹：{fingerprint}），等待人工在 Onlyne GUI/TUI 中核对指纹确认。",
	],
	planResponseInvalid: [
		"meta.response must be 'await', 'async', or 'none'.",
		"meta.response 必须是 'await'、'async' 或 'none'。",
	],
	planExecutionInvalid: [
		"meta.execution must be 'existing' or 'new'.",
		"meta.execution 必须是 'existing' 或 'new'。",
	],
	planAwaitNewInvalid: [
		"meta.response='await' cannot be combined with meta.execution='new' without an established thread.",
		"meta.response='await' 不能在无既存上下文的情况下与 meta.execution='new' 同用。",
	],
	resourceRefInvalid: [
		"meta.resource must be { owner, id } with non-empty strings.",
		"meta.resource 必须包含非空的 { owner, id }。",
	],
	resourceOpInvalid: [
		"meta.operation must be a non-empty string.",
		"meta.operation 必须是非空字符串。",
	],
	resourceRevisionInvalid: [
		"meta.expected_revision must be a string.",
		"meta.expected_revision 必须是字符串。",
	],
	summarySent: [
		"Sent ({kind}) -> {to} in thread {thread} (seq {seq}, state: {state}, plan: {response}/{execution}).",
		"已发送（{kind}）-> {to}，话题 {thread}（seq {seq}，状态：{state}，计划：{response}/{execution}）。",
	],
	summarySentAwaitReply: [
		"Awaited reply arrived from {from} in {thread} (seq {seq}):\n{reply}",
		"收到来自 {from} 的等待回复（话题 {thread}，seq {seq}）：\n{reply}",
	],
	summaryAwaitPending: [
		"Sent ({kind}) -> {to} in {thread} (seq {seq}); awaiting reply via asynchronous notification (state: {state}).",
		"已发送（{kind}）-> {to}，话题 {thread}（seq {seq}）；同步等待未直接返回，后续回复将异步送达（状态：{state}）。",
	],
	summaryRead: [
		"Read {scope} completed ({count} items).",
		"读取 {scope} 完成（共 {count} 项）。",
	],
	summaryThread: [
		"Thread {op} completed ({thread}).",
		"话题操作 {op} 已完成（{thread}）。",
	],
	summaryDomain: [
		"Domain {op} completed ({domain}).",
		"域操作 {op} 已完成（{domain}）。",
	],
	summaryResource: [
		"Resource operation {operation} on {resource} -> {state}.",
		"资源 {resource} 操作 {operation} -> {state}。",
	],
	subagentDenied: [
		"This Antigravity subagent conversation is read-only for its assigned Onlyne slice and cannot execute '{intent}'.",
		"当前 Antigravity 子对话对其分配的 Onlyne 切片只读，不能执行 '{intent}'。",
	],
} as const;

export type ToolTextKey = keyof typeof MESSAGES;

export function toolText(key: ToolTextKey, vars?: Record<string, string | number>): string {
	const [en, zh] = MESSAGES[key];
	return formatVars(pickText(en, zh, getLocale()), vars);
}
