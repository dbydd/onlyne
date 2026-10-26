import { OnlyneError, type Json, type MessageRef, type MsgInput, type PreparedSend } from "@onlyne/lib";
import type { ExtensionContext } from "@oh-my-pi/pi-coding-agent";
import { runDecision } from "./decider.ts";
import type { PluginRuntime } from "./runtime.ts";

interface Draft {
	id: string;
	created_by: string;
	created_at: string;
	prepared: PreparedSend;
}

export async function createDraft(runtime: PluginRuntime, ctx: ExtensionContext, input: MsgInput, signal?: AbortSignal): Promise<unknown> {
	const meta = { ...input.meta };
	delete meta.op;
	if (meta.relation !== "reply" && meta.relation !== "followup") throw new OnlyneError("invalid_request", "A reply draft needs relation reply or followup and in_reply_to.");
	const original = await resolveSource(runtime, meta.in_reply_to);
	if (ctx.agent.kind === "sub") {
		const authorized = (await runtime.contexts.list()).some(record => record.agent_id === ctx.agent.id
			&& record.source?.sender === original.sender && record.source.id === original.id);
		if (!authorized) throw new OnlyneError("forbidden", "A child may draft a reply only to the source message assigned to its context.");
	}
	meta.in_reply_to = { sender: original.sender, id: original.id };
	const normalized = { ...input, meta };
	const prepared = await runDecision({ ctx, input: normalized, signal, readOnly: false }, () => runtime.node.client.prepare(normalized));
	const draft: Draft = { id: crypto.randomUUID(), created_by: ctx.agent.id, created_at: new Date().toISOString(), prepared };
	await runtime.storage.transaction(tx => tx.put(`draft/${draft.id}`, draft));
	return { draft_id: draft.id, to: prepared.to, relation: prepared.message.meta?.relation,
		in_reply_to: original, body: prepared.message.msg, state: "draft", send: { meta: { draft_id: draft.id } } };
}

export async function sendDraft(runtime: PluginRuntime, ctx: ExtensionContext, id: string, signal?: AbortSignal): Promise<unknown> {
	if (ctx.agent.kind === "sub") throw new OnlyneError("forbidden", "Return draft_id to the main session; a child cannot send a draft.");
	const draft = await runtime.storage.transaction(tx => tx.get<Draft>(`draft/${id}`));
	if (draft === undefined) throw new OnlyneError("not_found", `No reply draft is named ${id}.`);
	const { to, message, plan } = draft.prepared;
	return runtime.node.client.send(to, message, plan, { context: ctx.agent.id, signal });
}

async function resolveSource(runtime: PluginRuntime, value: Json | undefined): Promise<MessageRef> {
	if (value === null || typeof value !== "object" || Array.isArray(value)) throw new OnlyneError("invalid_request", "A reply draft needs in_reply_to {sender,id} or {thread,seq}.");
	if (typeof value.sender === "string" && typeof value.id === "string") return { sender: value.sender, id: value.id };
	if (typeof value.thread === "string" && (typeof value.seq === "string" || typeof value.seq === "number")) {
		const slash = value.thread.lastIndexOf("/");
		const thread = slash < 0 ? { owner: runtime.config.node.id, id: value.thread }
			: { owner: value.thread.slice(0, slash), id: value.thread.slice(slash + 1) };
		let cursor: string | undefined;
		do {
			const result = await runtime.node.messages.list(runtime.node.auth, thread, { attention: "thread", page_size: 100, cursor });
			const found = result.items.find(row => row.seq === String(value.seq));
			if (found !== undefined) return { sender: found.message.sender, id: found.message.id };
			cursor = result.next_cursor ?? undefined;
		} while (cursor !== undefined);
	}
	throw new OnlyneError("not_found", "The draft source message is unavailable.");
}
