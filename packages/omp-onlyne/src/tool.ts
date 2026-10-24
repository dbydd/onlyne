import type { AgentToolResult } from "@oh-my-pi/pi-agent-core";
import { type ExtensionAPI, type ExtensionContext, type SessionEntry, z } from "@oh-my-pi/pi-coding-agent";
import type { Json, MsgInput, SendOutcome } from "@onlyne/lib";
import { runDecision } from "./decider.ts";
import { observeToolResult } from "./messages.ts";
import { getRuntime } from "./runtime.ts";

/** Custom entry naming a reply an `await` returned, left pending for the host bridge to confirm. */
export const AWAIT_RESULT = "onlyne.await-result";
/** The custom entry type naming an await result the model has actually read. */
export const AWAIT_CONSUMED = "onlyne.await-consumed";

const parameters = z.object({
	delivery_kind: z.string().min(1).describe("The delivery intent in natural language"),
	msg: z.string().describe("The message body or the local query"),
	meta: z.record(z.string(), z.unknown()).optional().describe("Explicit references and operation fields"),
});

const DESCRIPTION = `Send, reply to, read or manage Onlyne threads and resources through one entry.

Use delivery_kind for the intent ("ask reviewer to check the data", "reply to this request", "show the thread"). Put explicit references in meta:
to (a node id from the node directory, or an unambiguous short alias), thread, context, in_reply_to {thread,seq}, relation (reply|followup), mentions, message {sender,id} or {thread,seq}, resource {owner,id}, operation, op (create|close|list|describe|member_add|member_remove), title, scope (directory|threads|inbox|messages|message|evidence), seq, attention (inbox|thread), cursor, page_size, data, domain_id.

The plugin sends the request to OMP's judge role, which chooses the action and delivery plan. The result reports the plan that actually ran. Never guess a message id, idempotency key or revision. A reply or followup must state its relation; omitting it is rejected rather than assumed. A reply's target is the original reply authority, so a conflicting to is rejected. execution existing names a context the target publishes; execution new never allocates one here. Subagent sessions may read their own slice but may not send; return the result to the main agent instead.`;

export function registerMsgTool(pi: ExtensionAPI): void {
	pi.registerTool({
		name: "msg",
		label: "Onlyne Message",
		description: DESCRIPTION,
		parameters,
		approval: "read",
		strict: true,
		execute: async (
			toolCallId,
			raw,
			signal,
			_onUpdate,
			ctx,
		): Promise<AgentToolResult<{ result: unknown }>> => {
			const parsed = parameters.safeParse(raw);
			if (!parsed.success) return failure(`Invalid msg arguments: ${parsed.error.message}`);
			const runtime = getRuntime();
			if (runtime === undefined) return failure("Onlyne is not configured for this project. Create .onlyne/config.toml or run /onlyne init.");
			const input: MsgInput = {
				delivery_kind: parsed.data.delivery_kind,
				msg: parsed.data.msg,
				...(parsed.data.meta === undefined ? {} : { meta: parsed.data.meta as Record<string, Json> }),
			};
			const scope = {
				ctx,
				input,
				...(signal === undefined ? {} : { signal }),
				...(ctx.agent.kind === "sub" ? { readOnly: true } : {}),
			};
			const started = Date.now();
			try {
				const result = await runDecision(scope, () =>
					runtime.node.client.msg(input, {
						...(signal === undefined ? {} : { signal }),
						context: ctx.agent.id,
					}),
				);
				recordAwaitResult(pi, toolCallId, result, ctx);
				const visible = visibleResult(result);
				pi.logger.info("onlyne.msg.result", { kind: input.delivery_kind, ms: Date.now() - started, ...outcomeFacts(result) });
				return {
					content: [{ type: "text", text: JSON.stringify({ result: visible }, null, 2) }],
					details: { result: visible },
				};
			} catch (error) {
				pi.logger.warn("onlyne.msg.failed", {
					kind: input.delivery_kind, ms: Date.now() - started,
					error: error instanceof Error ? error.message : String(error),
				});
				return failure(error instanceof Error ? error.message : String(error));
			}
		},
	});
}

/** The facts one `msg` call produced, without the reply body. */
function outcomeFacts(result: unknown): Record<string, unknown> {
	if (result === null || typeof result !== "object") return {};
	const outcome = result as Partial<SendOutcome>;
	return {
		...(outcome.admission === undefined ? {} : { admission: outcome.admission }),
		...(outcome.wait === undefined ? {} : { wait: outcome.wait }),
		...(outcome.reply_body === undefined ? {} : { withheld: outcome.reply_body === "withheld" }),
		...(outcome.receipt?.seq === undefined ? {} : { seq: outcome.receipt.seq }),
		...(outcome.reply === undefined ? {} : { entry: `${outcome.reply.thread_ref.owner}/${outcome.reply.thread_ref.id}/${outcome.reply.seq}` }),
		...(outcome.error === undefined ? {} : { code: outcome.error.code }),
	};
}

/**
 * An `await` that returned a reply is recorded as a pending custom entry. The
 * host bridge confirms consumption when the model actually reads the result;
 * submitting the tool result proves nothing (RFC 0008 §5).
 */
function recordAwaitResult(pi: ExtensionAPI, toolCallId: string, result: unknown, ctx: ExtensionContext): void {
	if (result === null || typeof result !== "object") return;
	if ((result as Partial<SendOutcome>).reply_body === "withheld") return;
	const reply = (result as Partial<SendOutcome>).reply;
	if (reply === null || reply === undefined || typeof reply !== "object") return;
	if (typeof reply.seq !== "string" || typeof reply.thread_ref?.owner !== "string" || typeof reply.thread_ref.id !== "string") return;
	if (typeof reply.message?.sender !== "string" || typeof reply.message.id !== "string") return;
	observeToolResult(toolCallId, reply, ctx.agent.id);
	pi.appendEntry(AWAIT_RESULT, {
		tool_call_id: toolCallId,
		context: ctx.agent.id,
		ref: { sender: reply.message.sender, id: reply.message.id },
		thread_ref: reply.thread_ref,
		seq: reply.seq,
		state: "pending",
		at: new Date().toISOString(),
	});
}

function visibleResult(result: unknown): unknown {
	if (result === null || typeof result !== "object") return result;
	const outcome = result as Partial<SendOutcome>;
	if (outcome.reply_body !== "withheld" || outcome.reply === undefined) return result;
	const { thread_ref, seq, authenticated_author, reply_authority, at, message } = outcome.reply;
	return {
		...outcome,
		reply: {
			thread_ref, seq, authenticated_author, reply_authority, at,
			message: { sender: message.sender, id: message.id },
		},
	};
}

export function pendingAwaitResults(branch: readonly SessionEntry[]): Array<{ toolCallId: string; context: string; entry: NonNullable<SendOutcome["reply"]> }> {
	const consumed = new Set(branch.flatMap(entry => entry.type === "custom" && entry.customType === AWAIT_CONSUMED
		&& typeof (entry.data as { tool_call_id?: unknown })?.tool_call_id === "string"
		? [(entry.data as { tool_call_id: string }).tool_call_id] : []));
	const results = new Map(branch.flatMap(entry => entry.type === "message" && entry.message.role === "toolResult"
		? [[entry.message.toolCallId, entry.message] as const] : []));
	return branch.flatMap(entry => {
		if (entry.type !== "custom" || entry.customType !== AWAIT_RESULT) return [];
		const data = entry.data as { tool_call_id?: unknown; context?: unknown; ref?: { sender?: unknown; id?: unknown } } | undefined;
		if (typeof data?.tool_call_id !== "string" || typeof data.context !== "string" || consumed.has(data.tool_call_id)) return [];
		const outcome = (results.get(data.tool_call_id)?.details as { result?: Partial<SendOutcome> } | undefined)?.result;
		const reply = outcome?.reply;
		if (outcome?.reply_body === "withheld" || typeof reply?.message?.msg !== "string"
			|| reply.message.sender !== data.ref?.sender || reply.message.id !== data.ref?.id) return [];
		return [{ toolCallId: data.tool_call_id, context: data.context, entry: reply }];
	});
}

function failure(text: string): AgentToolResult<{ result: unknown }> {
	return {
		content: [{ type: "text", text }],
		details: { result: undefined },
		isError: true,
	};
}
