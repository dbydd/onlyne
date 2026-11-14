import { OnlyneError, type Action, type DeliveryPlan, type Json, type Message, type NodeId, type OnlyneNode, type SendOutcome, type ThreadRef } from "@onlyne/lib";
import { toolText as t } from "./tool-text.ts";

export interface Draft {
	draft_id: string;
	to: NodeId;
	delivery_kind: string;
	msg: string;
	thread?: ThreadRef;
	plan: DeliveryPlan;
	in_reply_to?: { sender: NodeId; id: string };
	relation?: "reply" | "followup";
	context?: string;
	mentions?: NodeId[];
	created_at: string;
	sent?: boolean;
}

const PREFIX = "agy/draft/";

export function draftKey(id: string): string {
	return `${PREFIX}${encodeURIComponent(id)}`;
}

export async function createDraft(
	_node: OnlyneNode,
	storage: { transaction<T>(work: (tx: { get<V>(k: string): Promise<V | undefined>; put<V>(k: string, v: V): Promise<void> }) => Promise<T>): Promise<T> },
	action: Extract<Action, { kind: "send" }>,
	input: { delivery_kind: string; msg: string },
	now = new Date().toISOString(),
): Promise<Draft> {
	const draft: Draft = {
		draft_id: `drf-${crypto.randomUUID().slice(0, 8)}`,
		to: action.to,
		delivery_kind: input.delivery_kind,
		msg: input.msg,
		...(action.thread === undefined ? {} : { thread: action.thread }),
		plan: {
			response: action.response,
			execution: action.execution,
			...(action.context === undefined ? {} : { context: action.context }),
		},
		...(action.in_reply_to === undefined ? {} : { in_reply_to: action.in_reply_to }),
		...(action.relation === undefined ? {} : { relation: action.relation }),
		...(action.context === undefined ? {} : { context: action.context }),
		...(action.mentions === undefined ? {} : { mentions: action.mentions }),
		created_at: now,
	};
	await storage.transaction(tx => tx.put(draftKey(draft.draft_id), draft));
	return draft;
}

export async function sendDraft(
	node: OnlyneNode,
	storage: { transaction<T>(work: (tx: { get<V>(k: string): Promise<V | undefined>; put<V>(k: string, v: V): Promise<void>; delete(k: string): Promise<void> }) => Promise<T>): Promise<T> },
	draftId: string,
	overrides?: { msg?: string; delivery_kind?: string; meta?: Record<string, Json>; context?: string },
): Promise<{ draft: Draft; outcome: SendOutcome }> {
	const draft = await storage.transaction(tx => tx.get<Draft>(draftKey(draftId)));
	if (draft === undefined || draft.sent === true) {
		throw new OnlyneError("not_found", t("draftNotFound", { id: draftId }));
	}
	const message: Message = {
		id: `msg-${crypto.randomUUID()}`,
		sender: node.auth.actor,
		delivery_kind: overrides?.delivery_kind ?? draft.delivery_kind,
		msg: overrides?.msg ?? draft.msg,
		meta: {
			...(draft.thread === undefined ? {} : { thread: draft.thread }),
			...(draft.in_reply_to === undefined ? {} : { in_reply_to: draft.in_reply_to }),
			...(draft.relation === undefined ? {} : { relation: draft.relation }),
			...(draft.mentions === undefined ? {} : { mentions: draft.mentions }),
		},
	};
	const outcome = await node.client.send(draft.to, message, draft.plan, {
		...(overrides?.context === undefined ? {} : { context: overrides.context }),
	});
	await storage.transaction(tx => tx.delete(draftKey(draftId)));
	return { draft, outcome };
}

export async function listDrafts(
	storage: { transaction<T>(work: (tx: { scan<V>(prefix: string): Promise<Array<{ key: string; value: V }>> }) => Promise<T>): Promise<T> },
): Promise<Draft[]> {
	const rows = await storage.transaction(tx => tx.scan<Draft>(PREFIX));
	return rows.map(r => r.value).sort((a, b) => b.created_at.localeCompare(a.created_at));
}
