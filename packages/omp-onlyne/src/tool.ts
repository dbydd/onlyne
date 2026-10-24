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
			try {
				const result = await runDecision(scope, () =>
					runtime.node.client.msg(input, {
						...(signal === undefined ? {} : { signal }),
						context: ctx.agent.id,
					}),
				);
				recordAwaitResult(pi, toolCallId, result, ctx);
				return {
					content: [{ type: "text", text: JSON.stringify({ result }, null, 2) }],
					details: { result },
				};
			} catch (error) {
				return failure(error instanceof Error ? error.message : String(error));
			}
		},
	});
}

/**
 * An `await` that returned a reply is recorded as a pending custom entry. The
 * host bridge confirms consumption when the model actually reads the result;
 * submitting the tool result proves nothing (RFC 0008 §5).
 */
function recordAwaitResult(pi: ExtensionAPI, toolCallId: string, result: unknown, ctx: ExtensionContext): void {
	if (result === null || typeof result !== "object") return;
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

/**
 * Marks every pending await result on the active branch as consumed. The
 * `before_provider_request` scan only sees entries still in `pending`, so a
 * consumed one costs nothing and cannot fire twice. The marker is durable:
 * after a restart the pending entry is re-armed only if no marker names it.
 */
export function markAwaitConsumed(pi: ExtensionAPI, branch: readonly SessionEntry[]): void {
	const pendingResults = branch.filter((entry): entry is SessionEntry & { customType: typeof AWAIT_RESULT; data: Record<string, unknown> } =>
		entry.type === "custom" && entry.customType === AWAIT_RESULT
		&& (entry.data as { state?: unknown } | undefined)?.state === "pending");
	for (const entry of pendingResults) {
		const data = entry.data as { tool_call_id?: unknown };
		if (typeof data.tool_call_id !== "string") continue;
		pi.appendEntry(AWAIT_CONSUMED, {
			tool_call_id: data.tool_call_id,
			ref: (entry.data as { ref?: unknown }).ref,
			thread_ref: (entry.data as { thread_ref?: unknown }).thread_ref,
			seq: (entry.data as { seq?: unknown }).seq,
			context: (entry.data as { context?: unknown }).context,
			at: new Date().toISOString(),
		});
	}
}

function failure(text: string): AgentToolResult<{ result: unknown }> {
	return {
		content: [{ type: "text", text }],
		details: { result: undefined },
		isError: true,
	};
}
