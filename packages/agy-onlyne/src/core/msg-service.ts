import {
	OnlyneError,
	trustPhase,
	type Action,
	type Json,
	type MsgInput,
	type NodeId,
	type OnlyneNode,
	type Page,
	type SendOutcome,
	type ThreadRef,
	type TrustEntry,
} from "@onlyne/lib";
import type { AcquireVerdict } from "@onlyne/node-host/acquire";
import type { ContextRegistry } from "@onlyne/node-host/store";
import type { AgyOnlyneConfig } from "../config.ts";
import type { CallerContext, JudgeDecider } from "./decider.ts";
import { createDraft, sendDraft } from "./drafts.ts";
import { threadKey } from "./resolve.ts";
import { toolText as t } from "./tool-text.ts";

export interface MsgToolParams {
	delivery_kind: string;
	msg: string;
	meta?: Record<string, Json>;
}

export interface MsgRuntimeHost {
	config: AgyOnlyneConfig;
	node: OnlyneNode;
	contexts: ContextRegistry;
	decider: JudgeDecider;
	runScopedDecision<T>(caller: CallerContext, work: (captureAction: (action: Action) => void) => Promise<T>): Promise<{ value: T; action?: Action }>;
	trustAccept(nodeId: NodeId): Promise<TrustEntry>;
	trustRevoke(nodeId: NodeId): Promise<TrustEntry>;
	trustRotate(): Promise<string>;
	trustBeginOrApprove(rawTarget: string, actor?: "human" | "agent"): Promise<{
		status: "exchanged" | "requested" | "pending_human";
		node_id: NodeId;
		fingerprint: string;
		verdict?: AcquireVerdict;
	}>;
	listThreads(cursor?: string): Promise<Page<import("@onlyne/lib").Thread>>;
}

function asRecord(value: unknown): Record<string, Json> | undefined {
	return value !== null && typeof value === "object" && !Array.isArray(value)
		? value as Record<string, Json>
		: undefined;
}

function deliverySummary(
	input: MsgToolParams,
	action: Extract<Action, { kind: "send" }>,
	outcome: SendOutcome,
): string {
	const seq = outcome.receipt?.seq ?? "pending";
	const thread = outcome.receipt?.thread_ref !== undefined
		? threadKey(outcome.receipt.thread_ref)
		: action.thread !== undefined
			? threadKey(action.thread)
			: "default";
	if (outcome.reply !== undefined && outcome.reply_body !== "withheld") {
		return t("summarySentAwaitReply", {
			from: outcome.reply.message.sender,
			thread: threadKey(outcome.reply.thread_ref),
			seq: outcome.reply.seq,
			reply: outcome.reply.message.msg,
		});
	}
	if (action.response === "await" && outcome.wait !== "answered") {
		return t("summaryAwaitPending", {
			kind: input.delivery_kind,
			to: outcome.resolved ?? action.to,
			thread,
			seq,
			state: outcome.admission,
		});
	}
	return t("summarySent", {
		kind: input.delivery_kind,
		to: outcome.resolved ?? action.to,
		thread,
		seq,
		state: outcome.admission,
		response: action.response,
		execution: action.execution,
	});
}

function readSummary(action: Extract<Action, { kind: "read" }>, result: unknown): string {
	const count = Array.isArray(result)
		? result.length
		: result !== null && typeof result === "object" && Array.isArray((result as { items?: unknown }).items)
			? (result as { items: unknown[] }).items.length
			: 1;
	return t("summaryRead", { scope: action.scope, count });
}

export async function executeMsg(
	host: MsgRuntimeHost,
	caller: CallerContext,
	params: MsgToolParams,
	storage: {
		transaction<T>(work: (tx: {
			get<V>(k: string): Promise<V | undefined>;
			put<V>(k: string, v: V): Promise<void>;
			delete(k: string): Promise<void>;
			scan<V>(prefix: string): Promise<Array<{ key: string; value: V }>>;
		}) => Promise<T>): Promise<T>;
	},
	signal?: AbortSignal,
): Promise<{ text: string; details: Record<string, unknown> }> {
	const meta = asRecord(params.meta);
	const op = meta?.op;
	if (op !== undefined && (typeof op !== "string" || op.trim().length === 0)) {
		throw new OnlyneError("invalid_request", t("metaOpInvalid"));
	}

	const callerCtx = (await host.contexts.list()).find(c => c.agent_id === caller.agent_id);
	const localContextAlias = caller.main ? "main" : callerCtx?.alias;

	if (op === "trust_rotate") {
		if (caller.read_only === true) throw new OnlyneError("forbidden", t("subagentDenied", { intent: op }));
		const fingerprint = await host.trustRotate();
		const summary = t("trustRotateConfirmed", { node: host.config.node.id, fingerprint });
		return { text: summary, details: { op, node_id: host.config.node.id, fingerprint } };
	}

	if (op === "trust_accept" || op === "trust_revoke" || op === "trust_begin") {
		if (caller.read_only === true) throw new OnlyneError("forbidden", t("subagentDenied", { intent: op }));
		const to = typeof meta?.to === "string" ? meta.to.trim() : "";
		if (to.length === 0) throw new OnlyneError("invalid_request", t("trustTargetRequired", { op: op.replace("trust_", "") }));
		const nodeId = await host.decider.refs.peerId(to).catch(() =>
			to.startsWith("urn:") ? to : `urn:onlyne:agent:${encodeURIComponent(to)}`);
		if (op === "trust_accept") {
			const entry = await host.trustAccept(nodeId);
			const phase = trustPhase(entry);
			return {
				text: t("trustAccepted", { node: entry.node_id, phase, fingerprint: entry.fingerprint }),
				details: { op, entry, phase },
			};
		}
		if (op === "trust_revoke") {
			const entry = await host.trustRevoke(nodeId);
			const phase = trustPhase(entry);
			return {
				text: t("trustRevoked", { node: entry.node_id, phase }),
				details: { op, entry, phase },
			};
		}
		const res = await host.trustBeginOrApprove(nodeId, "agent");
		if (res.status === "pending_human") {
			return {
				text: t("trustPendingHuman", { node: res.node_id, fingerprint: res.fingerprint }),
				details: { op, ...res },
			};
		}
		return {
			text: t("trustBegan", { node: res.node_id, phase: res.status, fingerprint: res.fingerprint }),
			details: { op, ...res },
		};
	}

	if (op === "send_draft") {
		if (caller.read_only === true) throw new OnlyneError("forbidden", t("subagentDenied", { intent: op }));
		const draftId = typeof meta?.draft_id === "string" ? meta.draft_id.trim() : "";
		if (draftId.length === 0) throw new OnlyneError("invalid_request", t("draftIdRequired"));
		const { draft, outcome } = await sendDraft(host.node, storage, draftId, {
			...(params.msg.trim().length > 0 ? { msg: params.msg } : {}),
			...(params.delivery_kind.trim().length > 0 ? { delivery_kind: params.delivery_kind } : {}),
			...(localContextAlias === undefined ? {} : { context: localContextAlias }),
		});
		const action: Extract<Action, { kind: "send" }> = {
			kind: "send",
			to: draft.to,
			...(draft.thread === undefined ? {} : { thread: draft.thread }),
			...(draft.context === undefined ? {} : { context: draft.context }),
			...(draft.in_reply_to === undefined ? {} : { in_reply_to: draft.in_reply_to }),
			...(draft.relation === undefined ? {} : { relation: draft.relation }),
			response: draft.plan.response,
			execution: draft.plan.execution,
		};
		if (outcome.reply !== undefined && outcome.reply_body === "included") {
			await host.node.client.confirmToolResult(outcome.reply, localContextAlias).catch(() => undefined);
		}
		return {
			text: deliverySummary(params, action, outcome),
			details: { op, draft_id: draftId, action, outcome },
		};
	}

	if (Array.isArray(meta?.to)) {
		const rawTargets = meta.to;
		const recipients: string[] = [];
		for (const item of rawTargets) {
			if (typeof item !== "string" || item.trim().length === 0) {
				throw new OnlyneError("invalid_request", t("metaToItemInvalid"));
			}
			recipients.push(item.trim());
		}
		if (recipients.length === 0) throw new OnlyneError("invalid_request", t("metaToItemInvalid"));

		const sharedThread: ThreadRef | undefined = await host.decider.refs.thread(meta);
		const results: Array<{ to: NodeId; outcome: SendOutcome; summary: string }> = [];
		for (const rawTarget of recipients) {
			const singleMeta: Record<string, Json> = {
				...meta,
				to: rawTarget,
				...(sharedThread === undefined ? {} : { thread: sharedThread as unknown as Json }),
			};
			delete singleMeta.op;
			const { value, action } = await host.runScopedDecision(caller, () =>
				host.node.client.msg(
					{
						delivery_kind: params.delivery_kind,
						msg: params.msg,
						meta: singleMeta,
					},
					{ signal, ...(localContextAlias === undefined ? {} : { context: localContextAlias }) },
				),
			);
			if (action === undefined || action.kind !== "send") {
				throw new OnlyneError("invalid_request", t("metaThreadRequired"));
			}
			const outcome = value as SendOutcome;
			if (sharedThread !== undefined && action.to !== host.config.node.id) {
				await host.node.threads.member(host.node.auth, sharedThread, action.to, { access: ["read", "post"] }, {
					idempotency_key: `add-${sharedThread.id}-${action.to}`,
					retry_until: new Date(Date.now() + 300_000).toISOString(),
				}).catch(() => undefined);
			}
			results.push({ to: action.to, outcome, summary: deliverySummary(params, action, outcome) });
		}
		return {
			text: results.map(r => r.summary).join("\n"),
			details: { multi: true, count: results.length, results },
		};
	}

	const cleanMeta = meta === undefined ? undefined : (() => {
		if (op === "draft") {
			const copy = { ...meta };
			delete copy.op;
			return copy;
		}
		return meta;
	})();

	const input: MsgInput = {
		delivery_kind: params.delivery_kind,
		msg: params.msg,
		...(cleanMeta === undefined ? {} : { meta: cleanMeta }),
	};

	if (op === "draft") {
		if (caller.read_only === true) throw new OnlyneError("forbidden", t("subagentDenied", { intent: "draft" }));
		const action = await host.decider.decide(caller, input, signal);
		if (action.kind !== "send") {
			throw new OnlyneError("invalid_request", t("metaOpUnsupported", { op: "draft" }));
		}
		const draft = await createDraft(host.node, storage, action, input);
		return {
			text: t("draftCreated", { id: draft.draft_id, to: draft.to }),
			details: { op: "draft", draft, action },
		};
	}

	const { value: result, action } = await host.runScopedDecision(caller, () =>
		host.node.client.msg(input, {
			signal,
			...(localContextAlias === undefined ? {} : { context: localContextAlias }),
		}),
	);

	if (action === undefined) {
		return { text: JSON.stringify(result, null, 2), details: { result } };
	}

	if (action.kind === "send") {
		const outcome = result as SendOutcome;
		if (outcome.reply !== undefined && outcome.reply_body === "included") {
			await host.node.client.confirmToolResult(outcome.reply, localContextAlias).catch(() => undefined);
		}
		return {
			text: deliverySummary(params, action, outcome),
			details: { action, outcome },
		};
	}
	if (action.kind === "read") {
		return {
			text: `${readSummary(action, result)}\n${JSON.stringify(result, null, 2)}`,
			details: { action, result },
		};
	}
	if (action.kind === "thread") {
		return {
			text: `${t("summaryThread", { op: action.op, thread: action.thread ? threadKey(action.thread) : (action.title ?? "ok") })}\n${JSON.stringify(result, null, 2)}`,
			details: { action, result },
		};
	}
	if (action.kind === "domain") {
		return {
			text: `${t("summaryDomain", { op: action.op, domain: action.domain_id ?? "ok" })}\n${JSON.stringify(result, null, 2)}`,
			details: { action, result },
		};
	}
	return {
		text: `${t("summaryResource", {
			resource: `${action.resource.owner}/${action.resource.id}`,
			operation: action.operation,
			state: String((result as { state?: string } | undefined)?.state ?? "done"),
		})}\n${JSON.stringify(result, null, 2)}`,
		details: { action, result },
	};
}
