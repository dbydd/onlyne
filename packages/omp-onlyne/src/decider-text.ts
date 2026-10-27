import { createTranslator } from "@onlyne/node-host/i18n";

/**
 * Every string the judge model or the calling model reads from the decider. Choice keys,
 * question ids and cue lists are identifiers, not prose, and stay untranslated.
 */
const en = {
	"judge.o1.instructions":
		"Choose the one action the model's message asks for. communicate: deliver a message, reply or progress note to another agent. inspect: look up local or network state. manage_thread: create, close, describe or change members of a local thread. manage_domain: manage this node's domain membership. operate_resource: operate a resource the message names explicitly.",
	"judge.o1.communicate": "The message is meant for another agent or node, including asking a peer something.",
	"judge.o1.inspect":
		"The message asks to see node-level state: who is online, the inbox, a thread's messages, one message, or delivery evidence. The thread registry and a thread's members are not this.",
	"judge.o1.manage_thread":
		"The message works on threads themselves: list the threads, describe one thread's members, create, close, add or remove a member. Reading a thread's messages is inspect.",
	"judge.o1.manage_domain":
		"The message changes or reads this node's domain membership: join, leave, list or describe the membership, or discover a domain's members.",
	"judge.o1.operate_resource":
		"meta.resource names an external resource to operate. Without meta.resource this intent cannot execute, so a message that merely mentions a directory or a file is not this.",

	"judge.relation.instructions":
		"The message references an earlier message. Is it a final reply or a progress supplement?",
	"judge.relation.reply": "A final answer that may release the original sender's wait.",
	"judge.relation.followup": "A supplement or progress note that does not release a wait.",

	"judge.target.instructions":
		"Choose the node that should receive this message. Each candidate is named by its node id; the alias is its short name.",
	"judge.target.peer":
		"Discovered peer {node} at {base_url} (instance {instance}, last seen {last_seen}).",

	"judge.response.instructions":
		"Two facts decide: does the receiver have to act, and can the sender continue without the answer?",
	"judge.response.none": "The receiver need not act at all: a notice, a status update, '不用回'.",
	"judge.response.async":
		"The receiver must act, but the sender keeps working and picks the answer up later: a background dispatch, a supplement to running work, '有空再做', '结果稍后贴我'.",
	"judge.response.await":
		"The receiver must act and the sender's next step cannot start without the answer: it is blocked on a question, a number it must have, or a conclusion it ships (等你回、数完回我、我要据此发版).",

	"judge.execution.instructions":
		"Should the receiver handle this in one of its published execution contexts or in a new one?",
	"judge.execution.existing.reply":
		"The message continues work the receiver already knows from the referenced message.",
	"judge.execution.existing.new":
		"The message belongs to work already running in one of the target's published contexts.",
	"judge.execution.new": "The message starts independent work and needs a new context the receiver creates.",

	"judge.context.instructions":
		"Choose the execution context the target should continue in, or none when this message does not continue one.",
	"judge.context.published": "{summary} ({state})",
	"judge.context.none":
		"The message does not continue any published context and starts fresh work instead.",

	"judge.inbound.instructions":
		"Choose where this inbound message is handled. main_agent: the main session takes it. A context alias: the named execution context continues the work it was created for. new_subagent: fresh independent work in a new subagent.",
	"judge.inbound.main_agent": "The message needs an answer, or no registered context's work matches it.",
	"judge.inbound.context": "{summary} (state: {state}, agent: {agent})",
	"judge.inbound.context_same_thread":
		"{summary} (state: {state}, agent: {agent}, same thread as this message)",
	"judge.inbound.new_subagent":
		"The message is a self-contained work item with its own background, and needs no answer. A status note, greeting, relay or summary is not this.",

	"judge.needs_answer.instructions":
		"Does the sender expect this node to answer? A reference to an earlier message does not by itself create that expectation.",
	"judge.needs_answer.yes":
		"The sender asks a question, greets and waits for an answer, requests work whose result must be reported back, or otherwise needs a reply from this node. plan.response await or async is consistent with this.",
	"judge.needs_answer.no":
		"The sender expects nothing back: it completes a reply this node asked for, reports status, records a fact, or says no answer is needed. plan.response none is consistent with this.",

	"judge.insertion.instructions":
		"The message goes to the main session. Choose when it should enter the agent's work.",
	"judge.insertion.steer":
		"Inject immediately after the current tool call: an urgent correction the running work must follow before it continues.",
	"judge.insertion.follow_up":
		"Add when the current turn finishes and start a new turn: the session is expected to act on it, answer it, or use its result.",
	"judge.insertion.fragment":
		"Record without starting a turn and expect nothing back: status syncs, log asides, version records and environment facts that say no action is needed (状态同步、日志旁路、版本记录、暂时不用管).",

	"judge.todo.action.instructions": "Decide what this message should do to the session's todo list.",
	"judge.todo.action.none":
		"The message needs no persistent work item: it is informational, duplicate, or already covered.",
	"judge.todo.action.associate": "The message continues or supplements an existing item.",
	"judge.todo.action.append":
		"The message needs a new persistent work item that no existing item covers.",
	"judge.todo.associate.instructions": "Choose the existing todo item this message continues.",
	"judge.todo.associate.item": "An existing todo item.",
	"judge.todo.title.instructions": "Choose the title for the new item from the message's own text.",
	"judge.todo.title.item": "A title taken from the message.",
	"judge.todo.explanation.instructions":
		"Choose the explanation for the new item from the message's own text, or none to leave it unwritten.",
	"judge.todo.explanation.item": "An explanation taken from the message.",
	"judge.todo.explanation.none": "Leave the explanation unwritten.",

	"judge.read_scope.instructions": "Choose what this message asks to read.",
	"judge.read_scope.peers":
		"The nodes that are online and reachable: 'who is online', 'which nodes can I reach', 'list the nodes', 'show the participants', the contact list, or the neighbour/adjacency view.",
	"judge.read_scope.inbox": "The attention items waiting to be processed.",
	"judge.read_scope.messages": "The message history inside one thread.",
	"judge.read_scope.evidence": "This node's delivery and receipt evidence for one outbound message.",
	"judge.read_scope.resources":
		"Resource inventory, binding, description, event log, events, operation history or operation status. meta.op names the query, default list.",
	"judge.read_scope.none": "The message asks to read none of these channels.",

	"judge.thread.instructions": "Choose the local thread the message asks about.",
	"judge.thread.item": "{title} ({owner}/{id}, {state})",

	"judge.thread_op.instructions": "Choose the local thread operation the message asks for.",
	"judge.thread_op.create": "Create a new local thread.",
	"judge.thread_op.close": "Close an existing local thread.",
	"judge.thread_op.list": "List the local threads.",
	"judge.thread_op.describe": "Describe one local thread.",
	"judge.thread_op.member_add": "Add a member to a local thread.",
	"judge.thread_op.member_remove": "Remove a member from a local thread.",

	"judge.domain_op.instructions": "Choose the domain operation the message asks for.",
	"judge.domain_op.create": "Create this node's domain membership.",
	"judge.domain_op.join": "Join a domain.",
	"judge.domain_op.leave": "Leave the current domain.",
	"judge.domain_op.list": "List this node's domain membership.",
	"judge.domain_op.describe": "Describe this node's membership.",
	"judge.domain_op.discover": "Discover a domain's declared members.",

	"error.outbound_intent": "The decision did not choose an outbound intent.",
	"error.relation_needs_reference": "A relation needs an in_reply_to reference.",
	"error.reply_authority":
		"The reply must go to its original reply authority {authority}, not {target}.",
	"error.delivery_target": "The decision did not choose a delivery target.",
	"error.execution_context_missing":
		'{to} publishes no execution context this message continues. Send it with execution "new" to start one.',
	"error.reply_relation": "The decision did not choose a reply relation.",
	"error.target_unresolved": "The target cannot be resolved: {reason}",
	"error.no_receiver": "No discovered node can receive this message.",
	"error.target_not_published":
		"The decision named {chosen}, which discovery does not publish.",
	"error.response_mode": "The judge did not choose a valid response mode.",
	"error.execution_mode": "The judge did not choose a valid execution mode.",
	"error.meta_mode": "meta.{key} must be await, async, none, existing or new.",
	"error.contexts_unreadable": "The target's published contexts cannot be read: {reason}",
	"error.context_not_published": "The target does not publish an execution context named {alias}.",
	"error.thread_reference_shape": "meta.thread must be a thread alias or {owner,id}.",
	"error.ambiguous_local_thread": "More than one local thread is named {value}.",
	"error.routing_target": "The decision did not choose a routing target.",
	"error.needs_answer": "The decision did not say whether this message needs an answer.",
	"error.registry_unreadable": "The context registry could not be read: {error}",
	"error.context_lost": "{alias} is no longer bound; it needs an explicit new decision",
	"error.insertion_mode": "The decision did not choose an insertion mode.",
	"error.todo_associate":
		"The decision chose to associate without naming an existing todo item.",
	"error.todo_append":
		"The decision chose to append without naming one of the message's title candidates.",
	"error.todo_action": "The decision did not choose a todo action.",
	"error.state_budget":
		"The decision state needs {chars} characters; the judge prompt is capped near {cap}.",
	"error.judge_failed":
		"The judge role could not decide this Onlyne message: {error}",
	"error.thread_title_required": "thread create needs a title.",
	"error.thread_reference_required": "thread {op} needs a thread reference.",
	"error.thread_target_required": "thread {op} needs a target.",
	"error.resource_fields": "resource needs owner, id and operation.",
	"error.read_thread_required": "read {name} needs a thread reference.",
	"error.read_message_required":
		"read {name} needs a message reference: meta.message {sender,id} or {thread,seq}.",
	"error.resources_main_only": "Resource reads belong to the main OMP session.",
	"error.resource_host_unavailable": "The resource query host is unavailable.",
	"error.read_scope_missing":
		"The decision named no read scope; the readable channels are peers, inbox, messages and evidence.",
	"error.read_scope_unknown":
		"No readable channel exists for {scope}; the readable channels are peers, inbox, messages and evidence.",
	"error.no_local_thread": "This node has no local thread to name.",
	"error.local_thread_choice": "The decision did not choose a local thread.",
	"error.thread_op": "The judge did not choose a thread operation.",
	"error.thread_op_target": "The judge chose thread {op} without a target.",
	"error.domain_op": "The judge did not choose a domain operation.",
	"error.domain_id": "domain {op} needs domain_id.",
	"error.resource_action_fields":
		"A resource action needs meta.resource {owner,id} and meta.operation.",
	"error.meta_to_single":
		"meta.to names one target; pass an array to the tool for several, or send one call per peer.",
	"error.meta_to_empty": "meta.to must be a non-empty string.",
	"error.alias_ambiguous": "The alias {alias} names more than one node: {matches}.",
	"error.alias_not_found": "No discovered node is named {alias}.",
	"error.mentions_array": "meta.mentions must be an array.",
	"error.mention_empty": "Each mention must be a non-empty string.",
	"error.in_reply_to_object": "meta.in_reply_to must be an object.",
	"error.in_reply_to_shape": "meta.in_reply_to needs {thread,seq} or {sender,id}.",
	"error.in_reply_to_seq": "meta.in_reply_to.seq must be a string or a number.",
	"error.message_not_found": "No message at {thread}/{seq}.",
	"error.message_thread_conflict":
		"meta.thread names {thread} but meta.message names {embedded}; a message read names one thread.",
	"error.guard_write":
		"A subagent session reads Onlyne state, it does not write it. Read your own slice through onlyne://threads, onlyne://inbox and onlyne://contexts, then return the result so the main agent can send it.",
	"error.guard_node_listing":
		"A subagent reads its own slice through onlyne://threads and onlyne://inbox; the whole node listing belongs to the main session.",
	"error.guard_evidence":
		"A subagent may read outbound evidence only for a message its own context received.",
	"error.guard_message": "A subagent may read one message only when its own context received it.",
	"error.guard_thread_required":
		"A subagent may read only a thread its own context record authorizes.",
	"error.guard_no_slice": "This subagent holds no read slice for thread {thread}.",
	"error.ambiguous_thread": "More than one thread is named {alias}.",
	"error.thread_not_found": "No local thread is named {alias}.",
	"error.page_size": "meta.page_size must be an integer from 1 to 100.",
	"error.attention": "meta.attention must be inbox or thread.",
	"error.relation_value": "meta.relation must be reply or followup.",

	"reason.registry_unreadable": "The context registry could not be read: {error}",
	"reason.discovery_unreadable": "Discovery records could not be read: {error}",
	"reason.receiver_contexts_unreadable":
		"The target's published contexts could not be read: {error}",
	"reason.todo_no_data": "The newest todo-more snapshot has no readable data.",
	"reason.todo_no_phases": "The newest todo-more snapshot has no phase list.",
	"reason.todo_absent": "No todo-more snapshot is on the active branch.",
	"reason.todo_missing": "No todo snapshot was provided.",
	"reason.todo_not_object": "The todo snapshot is not an object.",
	"reason.todo_source_absent": "The todo source is absent.",
	"reason.todo_phases_missing": "The todo snapshot has no phase list.",
	"reason.todo_phase_invalid": "A todo phase has no readable name or item list.",
	"reason.todo_item_title_missing": "A todo item has no readable title.",

	"prose.cut": "… [{more} more characters]",
	"prose.image_omitted": "omitted: image bytes are not decision input",
};

const zh: Record<keyof typeof en, string> = {
	"judge.o1.instructions":
		"选择模型这条消息要求执行的那一个动作。communicate：向另一个 agent 投递消息、回复或进度说明。inspect：查询本地或网络状态。manage_thread：创建、关闭、描述本地 thread 或变更其成员。manage_domain：管理本节点的域成员身份。operate_resource：操作消息明确点名的资源。",
	"judge.o1.communicate": "这条消息是发给另一个 agent 或节点的，包括向对端提问。",
	"judge.o1.inspect":
		"这条消息要求查看节点级状态：谁在线、inbox、某个 thread 的消息、单条消息或投递凭据。thread 注册表和某个 thread 的成员不属于此。",
	"judge.o1.manage_thread":
		"这条消息作用于 thread 本身：列出 thread、描述某个 thread 的成员、创建、关闭、添加或移除成员。读取 thread 内的消息属于 inspect。",
	"judge.o1.manage_domain":
		"这条消息改变或读取本节点的域成员身份：加入、离开、列出或描述成员身份，或发现某个域的成员。",
	"judge.o1.operate_resource":
		"meta.resource 点名了一个要操作的外部资源。没有 meta.resource 时该意图无法执行，因此只是提到某个目录或文件的消息不属于此。",

	"judge.relation.instructions": "这条消息引用了更早的一条消息。它是最终回复还是进度补充？",
	"judge.relation.reply": "一个最终答复，可以解除原发送方的等待。",
	"judge.relation.followup": "一个补充或进度说明，不解除任何等待。",

	"judge.target.instructions":
		"选择应当接收这条消息的节点。每个候选以其 node id 命名，alias 是它的短名。",
	"judge.target.peer":
		"已发现的 peer {node}，位于 {base_url}（实例 {instance}，最后出现于 {last_seen}）。",

	"judge.response.instructions":
		"两个事实决定：接收方是否必须动手，以及发送方能否在没有答复的情况下继续。",
	"judge.response.none": "接收方完全不必动手：通知、状态更新、'不用回'。",
	"judge.response.async":
		"接收方必须动手，但发送方继续工作、稍后再取答复：后台派活、对进行中工作的补充、'有空再做'、'结果稍后贴我'。",
	"judge.response.await":
		"接收方必须动手，且没有答复发送方的下一步就无法开始：卡在一个问题上、必须拿到的数字上，或要据此交付的结论上（等你回、数完回我、我要据此发版）。",

	"judge.execution.instructions":
		"接收方应当在其已发布的某个执行上下文中处理这条消息，还是新建一个？",
	"judge.execution.existing.reply": "这条消息延续接收方从被引用消息中已经知道的工作。",
	"judge.execution.existing.new": "这条消息属于目标已发布的某个上下文中正在进行的工作。",
	"judge.execution.new": "这条消息开启独立工作，需要一个由接收方新建的上下文。",

	"judge.context.instructions":
		"选择目标应当延续的执行上下文；当这条消息不延续任何上下文时选 none。",
	"judge.context.published": "{summary}（{state}）",
	"judge.context.none": "这条消息不延续任何已发布的上下文，而是开启新工作。",

	"judge.inbound.instructions":
		"选择这条入站消息在哪里处理。main_agent：由主会话处理。上下文别名：由该执行上下文延续它当初被创建来处理的工作。new_subagent：在新的 subagent 中处理独立的新工作。",
	"judge.inbound.main_agent": "这条消息需要答复，或没有任何已注册上下文的工作与之匹配。",
	"judge.inbound.context": "{summary}（state: {state}, agent: {agent}）",
	"judge.inbound.context_same_thread":
		"{summary}（state: {state}, agent: {agent}，与本消息同一 thread）",
	"judge.inbound.new_subagent":
		"这条消息是一个自带背景、自成一体的工作项，且无需答复。状态说明、问候、转达或汇总不属于此。",

	"judge.needs_answer.instructions":
		"发送方是否期待本节点答复？仅引用更早的消息本身并不构成这种期待。",
	"judge.needs_answer.yes":
		"发送方提出问题、问候并等待答复、请求必须回报结果的工作，或以其他方式需要本节点回复。plan.response 为 await 或 async 与此一致。",
	"judge.needs_answer.no":
		"发送方不期待任何回应：它在完成本节点先前请求的回复、报告状态、记录事实，或明说不必回复。plan.response 为 none 与此一致。",

	"judge.insertion.instructions":
		"这条消息要进入主会话。选择它应当在何时进入 agent 的工作。",
	"judge.insertion.steer": "在当前工具调用之后立即注入：运行中的工作必须先遵循的紧急修正。",
	"judge.insertion.follow_up":
		"在当前轮次结束后加入并开启新一轮：会话预期要据此行动、答复或使用其结果。",
	"judge.insertion.fragment":
		"只记录、不开启轮次，也不期待任何回应：状态同步、日志旁路、版本记录和无需动作的环境事实（如状态同步、日志旁路、版本记录、暂时不用管）。",

	"judge.todo.action.instructions": "判断这条消息应当对会话的 todo 列表做什么。",
	"judge.todo.action.none": "这条消息不需要持久工作项：它只是信息、重复内容，或已被覆盖。",
	"judge.todo.action.associate": "这条消息延续或补充一个已有的工作项。",
	"judge.todo.action.append": "这条消息需要一个现有工作项都未覆盖的新持久工作项。",
	"judge.todo.associate.instructions": "选择这条消息延续的那个已有 todo 项。",
	"judge.todo.associate.item": "一个已有的 todo 项。",
	"judge.todo.title.instructions": "从消息自身的文本中选择新项的标题。",
	"judge.todo.title.item": "取自消息的标题。",
	"judge.todo.explanation.instructions":
		"从消息自身的文本中选择新项的说明，或选 none 不写说明。",
	"judge.todo.explanation.item": "取自消息的说明。",
	"judge.todo.explanation.none": "不写说明。",

	"judge.read_scope.instructions": "选择这条消息要求读取的内容。",
	"judge.read_scope.peers":
		"在线且可达的节点：'谁在线'、'我能连到哪些节点'、'列出节点'、'显示参与者'、联系人列表，或邻居/邻接视图。",
	"judge.read_scope.inbox": "等待处理的注意项。",
	"judge.read_scope.messages": "某个 thread 内的消息历史。",
	"judge.read_scope.evidence": "本节点对某条出站消息的投递与回执凭据。",
	"judge.read_scope.resources":
		"资源清单、绑定、描述、事件日志、事件、操作历史或操作状态。meta.op 指定查询，默认 list。",
	"judge.read_scope.none": "这条消息不要求读取以上任何通道。",

	"judge.thread.instructions": "选择消息所指的本地 thread。",
	"judge.thread.item": "{title}（{owner}/{id}，{state}）",

	"judge.thread_op.instructions": "选择消息要求的本地 thread 操作。",
	"judge.thread_op.create": "创建一个新的本地 thread。",
	"judge.thread_op.close": "关闭一个已有的本地 thread。",
	"judge.thread_op.list": "列出本地 thread。",
	"judge.thread_op.describe": "描述一个本地 thread。",
	"judge.thread_op.member_add": "向本地 thread 添加成员。",
	"judge.thread_op.member_remove": "从本地 thread 移除成员。",

	"judge.domain_op.instructions": "选择消息要求的域操作。",
	"judge.domain_op.create": "创建本节点的域成员身份。",
	"judge.domain_op.join": "加入一个域。",
	"judge.domain_op.leave": "离开当前域。",
	"judge.domain_op.list": "列出本节点的域成员身份。",
	"judge.domain_op.describe": "描述本节点的成员身份。",
	"judge.domain_op.discover": "发现某个域声明的成员。",

	"error.outbound_intent": "决策没有选择出站意图。",
	"error.relation_needs_reference": "relation 需要 in_reply_to 引用。",
	"error.reply_authority": "回复必须发往其原始回复权威 {authority}，而不是 {target}。",
	"error.delivery_target": "决策没有选择投递目标。",
	"error.execution_context_missing":
		'{to} 未发布这条消息所延续的执行上下文。要以 execution "new" 发送以新建一个。',
	"error.reply_relation": "决策没有选择回复关系。",
	"error.target_unresolved": "无法解析目标：{reason}",
	"error.no_receiver": "没有已发现的节点可以接收这条消息。",
	"error.target_not_published": "决策指定了 {chosen}，但发现记录中没有发布它。",
	"error.response_mode": "judge 没有选择有效的 response 模式。",
	"error.execution_mode": "judge 没有选择有效的 execution 模式。",
	"error.meta_mode": "meta.{key} 必须是 await、async、none、existing 或 new。",
	"error.contexts_unreadable": "无法读取目标已发布的上下文：{reason}",
	"error.context_not_published": "目标未发布名为 {alias} 的执行上下文。",
	"error.thread_reference_shape": "meta.thread 必须是 thread 别名或 {owner,id}。",
	"error.ambiguous_local_thread": "有多个本地 thread 名为 {value}。",
	"error.routing_target": "决策没有选择路由目标。",
	"error.needs_answer": "决策没有说明这条消息是否需要答复。",
	"error.registry_unreadable": "无法读取上下文注册表：{error}",
	"error.context_lost": "{alias} 已不再绑定；需要一次显式的新决策",
	"error.insertion_mode": "决策没有选择插入模式。",
	"error.todo_associate": "决策选择了关联，却没有点名已有的 todo 项。",
	"error.todo_append": "决策选择了追加，却没有从消息的标题候选中点名一个。",
	"error.todo_action": "决策没有选择 todo 动作。",
	"error.state_budget": "决策状态需要 {chars} 个字符；judge prompt 的上限约为 {cap}。",
	"error.judge_failed": "judge 角色无法裁决这条 Onlyne 消息：{error}",
	"error.thread_title_required": "thread create 需要一个 title。",
	"error.thread_reference_required": "thread {op} 需要一个 thread 引用。",
	"error.thread_target_required": "thread {op} 需要一个目标。",
	"error.resource_fields": "resource 需要 owner、id 和 operation。",
	"error.read_thread_required": "read {name} 需要一个 thread 引用。",
	"error.read_message_required":
		"read {name} 需要一条消息引用：meta.message {sender,id} 或 {thread,seq}。",
	"error.resources_main_only": "资源读取属于主 OMP 会话。",
	"error.resource_host_unavailable": "资源查询宿主不可用。",
	"error.read_scope_missing":
		"决策没有点名读取范围；可读通道为 peers、inbox、messages 和 evidence。",
	"error.read_scope_unknown":
		"不存在 {scope} 对应的可读通道；可读通道为 peers、inbox、messages 和 evidence。",
	"error.no_local_thread": "本节点没有可点名的本地 thread。",
	"error.local_thread_choice": "决策没有选择本地 thread。",
	"error.thread_op": "judge 没有选择 thread 操作。",
	"error.thread_op_target": "judge 选择了 thread {op}，但没有目标。",
	"error.domain_op": "judge 没有选择域操作。",
	"error.domain_id": "domain {op} 需要 domain_id。",
	"error.resource_action_fields": "资源动作需要 meta.resource {owner,id} 和 meta.operation。",
	"error.meta_to_single":
		"meta.to 只能点名一个目标；要发给多个，请向工具传数组，或每个 peer 发一次调用。",
	"error.meta_to_empty": "meta.to 必须是非空字符串。",
	"error.alias_ambiguous": "别名 {alias} 指向多个节点：{matches}。",
	"error.alias_not_found": "没有已发现的节点名为 {alias}。",
	"error.mentions_array": "meta.mentions 必须是数组。",
	"error.mention_empty": "每个 mention 必须是非空字符串。",
	"error.in_reply_to_object": "meta.in_reply_to 必须是对象。",
	"error.in_reply_to_shape": "meta.in_reply_to 需要 {thread,seq} 或 {sender,id}。",
	"error.in_reply_to_seq": "meta.in_reply_to.seq 必须是字符串或数字。",
	"error.message_not_found": "{thread}/{seq} 处没有消息。",
	"error.message_thread_conflict":
		"meta.thread 指定 {thread}，而 meta.message 指定 {embedded}；一次消息读取只能指定一个 thread。",
	"error.guard_write":
		"subagent 会话只读取 Onlyne 状态，不写入。请通过 onlyne://threads、onlyne://inbox 和 onlyne://contexts 读取你自己的分片，然后返回结果由主 agent 发送。",
	"error.guard_node_listing":
		"subagent 通过 onlyne://threads 和 onlyne://inbox 读取自己的分片；整节点的列表属于主会话。",
	"error.guard_evidence": "subagent 只能读取其自身上下文收到的消息的出站凭据。",
	"error.guard_message": "只有自身上下文收到过某条消息时，subagent 才能读取该消息。",
	"error.guard_thread_required": "subagent 只能读取其自身上下文记录授权的 thread。",
	"error.guard_no_slice": "该 subagent 没有 thread {thread} 的读取分片。",
	"error.ambiguous_thread": "有多个 thread 名为 {alias}。",
	"error.thread_not_found": "没有名为 {alias} 的本地 thread。",
	"error.page_size": "meta.page_size 必须是 1 到 100 的整数。",
	"error.attention": "meta.attention 必须是 inbox 或 thread。",
	"error.relation_value": "meta.relation 必须是 reply 或 followup。",

	"reason.registry_unreadable": "无法读取上下文注册表：{error}",
	"reason.discovery_unreadable": "无法读取发现记录：{error}",
	"reason.receiver_contexts_unreadable": "无法读取目标已发布的上下文：{error}",
	"reason.todo_no_data": "最新的 todo-more 快照没有可读数据。",
	"reason.todo_no_phases": "最新的 todo-more 快照没有阶段列表。",
	"reason.todo_absent": "活动分支上没有 todo-more 快照。",
	"reason.todo_missing": "没有提供 todo 快照。",
	"reason.todo_not_object": "todo 快照不是对象。",
	"reason.todo_source_absent": "todo 来源不存在。",
	"reason.todo_phases_missing": "todo 快照没有阶段列表。",
	"reason.todo_phase_invalid": "某个 todo 阶段没有可读的名称或条目列表。",
	"reason.todo_item_title_missing": "某个 todo 条目没有可读的标题。",

	"prose.cut": "… [另有 {more} 个字符]",
	"prose.image_omitted": "已省略：图像字节不是决策输入",
};

export const text = createTranslator({ en, "zh-CN": zh });
