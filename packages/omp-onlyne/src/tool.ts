import type { AgentToolResult } from "@oh-my-pi/pi-agent-core";
import { type ExtensionAPI, type ExtensionContext, type SessionEntry, z } from "@oh-my-pi/pi-coding-agent";
import { OnlyneError, trustPhase, type Json, type MsgInput, type SendOutcome } from "@onlyne/lib";
import { runDecision } from "./decider.ts";
import { createDraft, sendDraft } from "./drafts.ts";
import { observeToolResult } from "./messages.ts";
import { getRuntime, type PluginRuntime } from "./runtime.ts";
import { toolText } from "./tool-text.ts";

/** Custom entry naming a reply an `await` returned, left pending for the host bridge to confirm. */
export const AWAIT_RESULT = "onlyne.await-result";
/** The custom entry type naming an await result the model has actually read. */
export const AWAIT_CONSUMED = "onlyne.await-consumed";

export function registerMsgTool(pi: ExtensionAPI): void {
	const parameters = z.object({
		delivery_kind: z.string().min(1).describe(toolText("param.delivery_kind")),
		msg: z.string().describe(toolText("param.msg")),
		meta: z.record(z.string(), z.unknown()).optional().describe(toolText("param.meta")),
	});
	pi.registerTool({
		name: "msg",
		label: toolText("label"),
		description: toolText("description"),
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
			if (!parsed.success) return failure(toolText("failure.invalid_args", { message: parsed.error.message }));
			const runtime = getRuntime();
			if (runtime === undefined) return failure(toolText("failure.not_configured"));
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
				const op = parsed.data.meta?.op;
				if (op === "draft") {
					const result = await createDraft(runtime, ctx, input, signal);
					pi.logger.info("onlyne.draft.created", { ms: Date.now() - started });
					// createDraft is typed unknown but always returns the draft record it just stored.
					const draft = result as { draft_id: string };
					return { content: [{ type: "text", text: toolText("draft.ready", { draft_id: draft.draft_id }) }], details: { result } };
				}
				// RFC 0018 §3.1: a trust action is mechanical — no judging, and no action kind
				// invented for it, because the library executes exactly five and a trust action is
				// not one of them (client.ts admits send, read, thread, resource and domain).
				if (op === "trust_accept" || op === "trust_revoke" || op === "trust_begin") {
					if (ctx.agent.kind === "sub") throw new OnlyneError("forbidden", toolText("failure.trust_main_only"));
					const node = await trustNode(runtime, op, parsed.data.meta);
					if (op === "trust_revoke") {
						await runtime.acquireRevoke(node);
						return { content: [{ type: "text", text: toolText("trust.revoked", { node }) }], details: { result: { op, node_id: node } } };
					}
					if (op === "trust_begin") {
						const entry = await runtime.acquireBegin(node);
						return {
							content: [{ type: "text", text: toolText("trust.begun", { node, fingerprint: entry.fingerprint, phase: trustPhase(entry) }) }],
							details: { result: entry },
						};
					}
					const entry = await runtime.acquireAccept(node, trustField(parsed.data.meta, "fingerprint"));
					return {
						content: [{ type: "text", text: toolText("trust.accepted", { node, fingerprint: entry.fingerprint, phase: trustPhase(entry) }) }],
						details: { result: entry },
					};
				}
				if (typeof parsed.data.meta?.draft_id === "string") {
					const result = await sendDraft(runtime, ctx, parsed.data.meta.draft_id, signal);
					recordAwaitResult(pi, toolCallId, result, ctx);
					return { content: [{ type: "text", text: `${summary(result, spelling(parsed.data.meta?.to))}\n\n${JSON.stringify(visibleResult(result), null, 2)}` }], details: { result: visibleResult(result) } };
				}
				const targets = parsed.data.meta?.to;
				if (Array.isArray(targets)) {
					if (targets.length === 0) return failure(toolText("failure.no_target"));
					const rows: Array<{ to: unknown; outcome?: unknown; error?: string }> = [];
					for (const target of targets) {
						const one: MsgInput = { ...input, meta: { ...input.meta, to: target as Json } };
						const oneScope = { ...scope, input: one };
						try {
							rows.push({ to: target, outcome: await runDecision(oneScope, () => runtime.node.client.msg(one, {
								...(signal === undefined ? {} : { signal }), context: ctx.agent.id,
							})) });
						} catch (error) {
							rows.push({ to: target, error: error instanceof Error ? error.message : String(error) });
						}
					}
					const text = rows.map(row => row.error === undefined
						? toolText("multi.delivered", { to: String(row.to), summary: summary(row.outcome, spelling(row.to)) })
						: toolText("multi.failed", { to: String(row.to), error: row.error })).join("\n");
					pi.logger.info("onlyne.msg.multi", { targets: rows.length, failed: rows.filter(row => row.error !== undefined).length });
					return { content: [{ type: "text", text: `${text}\n\n${JSON.stringify(rows, null, 2)}` }], details: { result: rows } };
				}
				const result = await runDecision(scope, () =>
					runtime.node.client.msg(input, {
						...(signal === undefined ? {} : { signal }),
						context: ctx.agent.id,
					}),
				);
				recordAwaitResult(pi, toolCallId, result, ctx);
				const visible = visibleResult(result);
				pi.logger.info("onlyne.msg.result", { kind: input.delivery_kind, ms: Date.now() - started, ...outcomeFacts(result) });
				const text = op === "member_add" || op === "member_remove"
					? membershipText(op, visible, parsed.data.meta)
					: summary(result, spelling(parsed.data.meta?.to));
				return {
					content: [{ type: "text", text: `${text}\n\n${JSON.stringify(visible, null, 2)}` }],
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
 *
 * `addressed` is the target as the caller wrote it. A receipt names the thread's owner, which
 * is this node whenever it started the conversation, so leading with it made a message to a
 * peer read as a message to itself; the addressee comes first and the storage second.
 */
export function summary(result: unknown, addressed?: string): string {
	if (result === null || typeof result !== "object") return toolText("summary.no_result");
	const outcome = result as Partial<SendOutcome>;
	const receipt = outcome.receipt;
	// The message's own sender is this node: `send` refuses a message another node authors.
	const self = receipt?.message_ref.sender;
	const to = outcome.resolved ?? outcome.requested ?? addressed;
	// The answer is part of the delivery fact: a reply the receipt cannot attribute is one the
	// sender cannot prove landed on the message it meant (cyanea report 2, problem 2).
	const answers = receipt?.in_reply_to;
	const answering = answers === undefined ? "" : toolText("summary.answering", { sender: answers.sender, id: answers.id });
	// The receipt names the thread owner, which is this node when it started the conversation;
	// a peer's thread is stored at the peer, so the two cases are stated apart.
	const storedHere = receipt !== undefined && receipt.thread_ref.owner === self;
	const storage = receipt === undefined ? "" : toolText(storedHere ? "summary.stored_here" : "summary.stored_there", {
		owner: receipt.thread_ref.owner, thread: receipt.thread_ref.id, seq: receipt.seq, answering,
	});
	const facts = [
		to === undefined ? "" : toolText("summary.addressee", { to }),
		storage,
		// The resolved addressee is what keeps an alias send from reading as a self-send.
		outcome.resolved === undefined || outcome.resolved === outcome.requested ? "" : toolText("summary.resolved", { node: outcome.resolved }),
		// The receipt names a thread the receiver owns, which is not a local object; the outbound
		// record is. Point the reader at the handle that opens (dpdp report, problem 1).
		receipt === undefined ? "" : toolText("summary.outbound", { id: receipt.message_ref.id }),
	].filter(part => part.length > 0).join(toolText("summary.sep"));
	// A thread this node holds and a peer it addressed: the entry is here, and the peer reaches
	// it through the grant notice rather than a copy of the body.
	const mechanism = storedHere && to !== undefined && to !== self
		? toolText("summary.grant_notice")
		: "";
	// With no addressee the sentence opens on the storage fact, so it is capitalized like a lead.
	const body = to === undefined && facts.length > 0 ? facts.charAt(0).toUpperCase() + facts.slice(1) : facts;
	if (outcome.admission !== undefined) {
		if (outcome.admission !== "accepted") {
			const error = outcome.error;
			const failure = error === undefined ? outcome.admission : `${error.code}: ${error.message}`;
			// `retryable` belongs to the runtime, not to the caller: the record stays open and the
			// runtime drives it again, while a second send commits a second intent.
			const next = error?.retryable === true
				? toolText("summary.retry")
				: toolText("summary.recover");
			return toolText("summary.not_confirmed", { failure, next });
		}
		// No receipt means no addressee and no storage to state; the JSON below carries the rest.
		if (body.length === 0) return toolText("summary.plain");
		const vars = { facts: body, mechanism };
		if (outcome.wait === "answered" && outcome.reply !== undefined) {
			const answer = { ...vars, author: outcome.reply.authenticated_author, seq: outcome.reply.seq };
			return outcome.reply_body === "withheld"
				? toolText("summary.delivered_withheld", answer)
				: toolText("summary.delivered_answered", answer);
		}
		if (outcome.wait === "timed_out") {
			return toolText("summary.delivered_timed_out", vars);
		}
		if (outcome.wait === "cancelled") {
			return toolText("summary.delivered_cancelled", vars);
		}
		return toolText("summary.delivered", vars);
	}
	return toolText("summary.plain");
}

/** The caller's spelling of one target, when it is a plain name rather than a structured ref. */
function spelling(value: unknown): string | undefined {
	return typeof value === "string" && value.length > 0 ? value : undefined;
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
		...(outcome.receipt?.in_reply_to === undefined ? {} : { reply_to: `${outcome.receipt.in_reply_to.sender}/${outcome.receipt.in_reply_to.id}` }),
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

/**
 * RFC 0018 §3.1: a trust op names its peer mechanically. The fields are read before any
 * judgement or storage write, so a malformed call is a caller error, never a policy answer.
 */
function trustField(meta: Record<string, unknown> | undefined, field: string): string {
	const value = meta?.[field];
	if (typeof value !== "string" || value.length === 0) throw new OnlyneError("invalid_request", toolText("failure.trust_fields", { field }));
	return value;
}

/**
 * RFC 0018 §3.1: the peer a trust op names, resolved through the same peer set a send resolves
 * `meta.to` from. An ambiguous alias names its matches; an unknown one is `not_found` and says
 * which action asked, because the same alias may be valid for another one.
 */
async function trustNode(runtime: PluginRuntime, op: string, meta: Record<string, unknown> | undefined): Promise<string> {
	const node = trustField(meta, "node_id");
	try {
		return await runtime.decider.peerId(node);
	} catch (error) {
		if (error instanceof OnlyneError && error.error.code === "not_found") {
			throw new OnlyneError("not_found", toolText("failure.trust_node_unknown", { action: op, node }), error.error.details);
		}
		throw error;
	}
}

/** The sentence one membership op earns: the member, the thread, and where a later array send lands. */
function membershipText(op: "member_add" | "member_remove", result: unknown, meta: Record<string, unknown> | undefined): string {
	const named = result !== null && typeof result === "object" && "peer" in result && typeof result.peer === "string"
		? result.peer
		: threadLabel(meta?.to);
	return toolText(op === "member_add" ? "thread.member_added" : "thread.member_removed", {
		member: named,
		thread: threadLabel(meta?.thread),
	});
}

/** A reference as the caller wrote it: one name, or `owner/id` for the record form. */
function threadLabel(value: unknown): string {
	if (typeof value === "string") return value;
	if (value === null || typeof value !== "object" || Array.isArray(value)) return "";
	const owner = "owner" in value ? value.owner : undefined;
	const id = "id" in value ? value.id : undefined;
	return typeof owner === "string" && typeof id === "string" ? `${owner}/${id}` : "";
}
