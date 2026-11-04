import { OnlyneError, type Json, type MessageRef, type MsgInput, type PreparedSend } from "@onlyne/lib";
import type { ExtensionContext } from "@oh-my-pi/pi-coding-agent";
import { runDecision } from "./decider.ts";
import type { PluginRuntime } from "./runtime.ts";
import { bridgeText as t } from "./bridge-text.ts";

interface Draft {
	id: string;
	created_by: string;
	created_at: string;
	prepared: PreparedSend;
}

/** How a caller names the draft it wants sent: by id, or by the message it answers. */
export interface DraftRequest {
	id?: string;
	in_reply_to?: Json;
}

export async function createDraft(runtime: PluginRuntime, ctx: ExtensionContext, input: MsgInput, signal?: AbortSignal): Promise<unknown> {
	const meta = { ...input.meta };
	delete meta.op;
	if (meta.relation !== "reply" && meta.relation !== "followup") throw new OnlyneError("invalid_request", t("draftNeedsRelation"));
	const original = await sourceReference(runtime, meta.in_reply_to);
	if (ctx.agent.kind === "sub") {
		const authorized = (await runtime.contexts.list()).some(record => record.agent_id === ctx.agent.id
			&& record.source?.sender === original.sender && record.source.id === original.id);
		if (!authorized) throw new OnlyneError("forbidden", t("draftChildScope"));
	}
	meta.in_reply_to = { sender: original.sender, id: original.id };
	const normalized = { ...input, meta };
	const prepared = await runDecision({ ctx, input: normalized, signal, readOnly: false }, () => runtime.node.client.prepare(normalized));
	const draft: Draft = { id: crypto.randomUUID(), created_by: ctx.agent.id, created_at: new Date().toISOString(), prepared };
	await runtime.storage.transaction(tx => tx.put(`draft/${draft.id}`, draft));
	return { draft_id: draft.id, to: prepared.to, relation: prepared.message.meta?.relation,
		in_reply_to: original, body: prepared.message.msg, state: "draft", send: { meta: { draft_id: draft.id } } };
}

/**
 * Sends a prepared reply. The caller may name the draft by id or by the message it answers; a
 * call that names neither continues with the most recent draft this session created, because
 * the draft already carries the one decision that must not be taken twice — its plan.
 */
export async function sendDraft(runtime: PluginRuntime, ctx: ExtensionContext, request: DraftRequest, signal?: AbortSignal): Promise<unknown> {
	if (ctx.agent.kind === "sub") throw new OnlyneError("forbidden", t("draftChildCannotSend"));
	const draft = await findDraft(runtime, ctx, request);
	const { to, message, plan } = draft.prepared;
	return runtime.node.client.send(to, message, plan, { context: ctx.agent.id, signal });
}

/**
 * Draft lookup. The index is (created_by, in_reply_to): a reference names the draft that
 * answered that message, and a session with no reference of its own sends the newest draft it
 * created. Several drafts can answer one message — a child drafted, then the main session
 * answered something else — so the newest wins; the draft's own plan is what makes the choice
 * safe, since no wording is decided again here.
 */
async function findDraft(runtime: PluginRuntime, ctx: ExtensionContext, request: DraftRequest): Promise<Draft> {
	if (request.id !== undefined) {
		const draft = await runtime.storage.transaction(tx => tx.get<Draft>(`draft/${request.id}`));
		if (draft === undefined) throw new OnlyneError("not_found", t("draftNotFound", { id: request.id }));
		return draft;
	}
	// The store is the only index: drafts outlive the session that created them, and a scan of
	// a handful of rows costs less than a second index that could disagree with it.
	const stored = await runtime.storage.transaction(tx => tx.scan<Draft>("draft/"));
	const drafts = stored.map(row => row.value);
	const wanted = request.in_reply_to === undefined ? undefined : await sourceReference(runtime, request.in_reply_to);
	const matching = wanted === undefined
		? drafts
		: drafts.filter(draft => {
				const ref = draft.prepared.message.meta?.in_reply_to;
				return ref !== undefined && ref.sender === wanted.sender && ref.id === wanted.id;
			});
	// This session's own draft is preferred; a child's draft it was just handed still answers,
	// because the reference, not the author, is what the caller named.
	const own = matching.filter(draft => draft.created_by === ctx.agent.id);
	const pool = wanted === undefined ? own : own.length > 0 ? own : matching;
	const newest = pool.sort((left, right) =>
		right.created_at.localeCompare(left.created_at) || right.id.localeCompare(left.id))[0];
	if (newest !== undefined) return newest;
	throw wanted === undefined
		? new OnlyneError("not_found", t("draftNoRecent"))
		: new OnlyneError("not_found", t("draftNoMatch", { sender: wanted.sender, id: wanted.id }));
}

/** The message a draft answers, in the two shapes a caller can copy from a receipt or a read. */
async function sourceReference(runtime: PluginRuntime, value: Json | undefined): Promise<MessageRef> {
	if (value === null || typeof value !== "object" || Array.isArray(value)) {
		throw new OnlyneError("invalid_request", t("draftNeedsReference"));
	}
	const sender = typeof value.sender === "string" && value.sender.length > 0 ? value.sender : undefined;
	const id = typeof value.id === "string" && value.id.length > 0 ? value.id : undefined;
	if (sender !== undefined && id !== undefined) {
		return { sender: await runtime.decider.refs.peerId(sender).catch(() => sender), id };
	}
	const thread = typeof value.thread === "string" && value.thread.length > 0 ? value.thread : undefined;
	const seq = typeof value.seq === "number" ? String(value.seq) : typeof value.seq === "string" && value.seq.length > 0 ? value.seq : undefined;
	if (thread !== undefined && seq !== undefined) {
		const ref = await runtime.decider.refs.threadByAlias(thread);
		const entry = await runtime.decider.refs.entryAt(ref, seq);
		if (entry !== undefined) return { sender: entry.message.sender, id: entry.message.id };
		throw new OnlyneError("not_found", t("draftSourceUnavailable"));
	}
	throw new OnlyneError("invalid_request", t("draftNeedsReference"));
}
