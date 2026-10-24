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

const DESCRIPTION = `The only surface for Onlyne messages, threads and resources. There is no CLI and nothing else to look up: call this tool with your intent instead of reading docs or source first.

Shape: { delivery_kind, msg, meta }. delivery_kind names the intent in plain language ("reply to dpdp's greeting", "show this thread", "read that message"). meta carries references you already know: to, thread, context, in_reply_to {thread,seq} or {sender,id}, relation (reply|followup), mentions, message {sender,id} or {thread,seq}, resource {owner,id}, operation, op, title, scope, seq, attention, cursor, page_size, data, domain_id.

A send is one call. The result is the delivery fact: admission "accepted" means the receiver recorded the message; wait "timed_out" only means no reply arrived before the wait bound, the message is still delivered; reply_body "withheld" means this context already showed that body. Report the outcome and stop there.

A reply arrives by itself. The plugin injects it as an onlyne.inbound message and starts a turn; you do not need to check. Do not poll after a send: no inbox scans, thread reads, evidence queries, log greps or SQLite lookups. Read state only when the user asks about history, or when a specific fact is missing from the result in hand.

The judge role chooses the action and plan from your intent, so never invent message ids, idempotency keys or revisions. A reply or followup states its relation; omitting it is rejected rather than assumed. A reply targets the original reply authority, so a conflicting to is rejected. Subagent sessions read their slice but cannot send; return the result so the main agent can send it.`;

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
					content: [{ type: "text", text: `${summary(result)}\n\n${JSON.stringify(visible, null, 2)}` }],
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

/**
 * The outcome as one sentence. A structured result invites a key-by-key search —
 * `wait: timed_out` beside `admission: accepted` reads as a contradiction until the
 * reader knows the contract — so the delivery fact is stated first and the JSON below
 * it is for the rare case that needs a machine-readable detail.
 */
export function summary(result: unknown): string {
	if (result === null || typeof result !== "object") return "Onlyne returned no structured result.";
	const outcome = result as Partial<SendOutcome>;
	const owner = outcome.receipt?.thread_ref?.owner ?? outcome.reply?.thread_ref.owner;
	const where = owner === undefined ? "" : ` to ${owner}`;
	const ref = outcome.receipt === undefined ? undefined : `thread ${outcome.receipt.thread_ref.id} seq ${outcome.receipt.seq}`;
	const facts = ref === undefined ? "" : ` (${ref})`;
	if (outcome.admission !== undefined) {
		if (outcome.admission !== "accepted") {
			const failure = outcome.error === undefined ? outcome.admission : `${outcome.error.code}: ${outcome.error.message}`;
			return `Not confirmed: ${failure}. The record stays open for recovery; do not re-send the same intent.`;
		}
		if (outcome.wait === "answered" && outcome.reply !== undefined) {
			return outcome.reply_body === "withheld"
				? `Delivered${where}${facts}. ${outcome.reply.authenticated_author} answered at seq ${outcome.reply.seq}, but that body was already shown in this context, so only its arrival is reported.`
				: `Delivered${where}${facts}. ${outcome.reply.authenticated_author} answered at seq ${outcome.reply.seq}.`;
		}
		if (outcome.wait === "timed_out") {
			return `Delivered${where}${facts} and recorded durably. No reply arrived within the wait; it will arrive on its own as an inbound message.`;
		}
		if (outcome.wait === "cancelled") {
			return `Delivered${where}${facts} and recorded durably. The wait was cancelled; a reply will arrive on its own as an inbound message.`;
		}
		return `Delivered${where}${facts} and recorded durably.`;
	}
	return "Onlyne answered; the structured result follows.";
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
