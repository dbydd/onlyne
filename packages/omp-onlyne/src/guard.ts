import { OnlyneError, type Action, type Json, type NodeId } from "@onlyne/lib";
import type { ContextRegistry } from "@onlyne/node-host/store";
import type { ContextRecord } from "@onlyne/node-host/types";
import { text } from "./decider-text.ts";
import { resourceFields } from "./plan.ts";
import { threadKey } from "./resolve.ts";

/** The five intents node O-1 chooses among. */
export type OutboundIntent = "communicate" | "inspect" | "manage_thread" | "manage_domain" | "operate_resource";

export interface IntentSettlement {
	intent: OutboundIntent;
	/** Set when a judged intent could not execute and a deterministic rule replaced it. */
	override?: { chosen: OutboundIntent; fallback: OutboundIntent; reason: string };
}

/** A call that names a recipient: `meta.to`, or the message it answers. */
function addressed(meta: Record<string, Json> | undefined): boolean {
	if (meta === undefined) return false;
	if (typeof meta.to === "string" && meta.to.length > 0) return true;
	return meta.in_reply_to !== null && typeof meta.in_reply_to === "object" && !Array.isArray(meta.in_reply_to);
}

/**
 * A resource intent is executable only when the fields it needs are present. The O-1 criteria
 * already say a message that merely mentions a file is not a resource operation, but a criterion
 * is prose the model may violate; this is the same rule in code.
 *
 * When a message names an explicit recipient and carries no complete resource action, the
 * resource choice is a misread of a communication: letting it through can only fail on missing
 * fields, and the error would send the caller to add `meta.resource` — turning a legitimate
 * message into an operation on somebody's resource (issue: outbound-intent-resource-false-
 * positive). With no recipient named the resource branch keeps its own hard error, because
 * nothing in the call says the message was meant for anyone.
 */
export function settleIntent(intent: OutboundIntent, meta: Record<string, Json> | undefined): IntentSettlement {
	if (intent !== "operate_resource") return { intent };
	if (resourceFields(meta) !== undefined) return { intent };
	if (!addressed(meta)) return { intent };
	return {
		intent: "communicate",
		override: {
			chosen: "operate_resource",
			fallback: "communicate",
			reason: "the message names a recipient and carries no complete meta.resource",
		},
	};
}

/**
 * A subagent may read only the threads its own context records authorize on this node. The
 * unfiltered node-wide views stay with the main session.
 */
export class ActionGuard {
	constructor(private readonly deps: { contexts: ContextRegistry; nodeId: NodeId }) {}

	async assert(scope: { readOnly?: boolean; agentId: string }, action: Action): Promise<void> {
		if (scope.readOnly !== true) return;
		if (action.kind !== "read") {
			throw new OnlyneError("forbidden", text("error.guard_write"));
		}
		if (action.scope === "resources") throw new OnlyneError("forbidden", text("error.resources_main_only"));
		if (action.scope === "directory" || action.scope === "neighbors" || action.scope === "threads" || action.scope === "inbox") {
			throw new OnlyneError("forbidden", text("error.guard_node_listing"));
		}
		const records = await this.authorized(scope.agentId);
		if (action.scope === "evidence") {
			const ref = action.message;
			const tied = ref !== undefined && records.some(record =>
				record.source !== undefined && record.source.sender === ref.sender && record.source.id === ref.id,
			);
			if (!tied) throw new OnlyneError("forbidden", text("error.guard_evidence"));
			return;
		}
		// `{sender,id}` names no thread, so the record that received that message is the
		// authorization — the same tie evidence uses.
		if (action.scope === "message" && action.thread === undefined && action.message !== undefined) {
			const ref = action.message;
			const tied = records.some(record =>
				record.source !== undefined && record.source.sender === ref.sender && record.source.id === ref.id,
			);
			if (!tied) throw new OnlyneError("forbidden", text("error.guard_message"));
			return;
		}
		if (action.thread === undefined) {
			throw new OnlyneError("forbidden", text("error.guard_thread_required"));
		}
		const key = threadKey(action.thread);
		if (!records.some(record => record.thread_ref !== undefined && threadKey(record.thread_ref) === key)) {
			throw new OnlyneError("forbidden", text("error.guard_no_slice", { thread: key }));
		}
	}

	private async authorized(agentId: string): Promise<ContextRecord[]> {
		let records: ContextRecord[];
		try {
			records = await this.deps.contexts.list();
		} catch (error) {
			throw new OnlyneError("unavailable", text("error.registry_unreadable", {
				error: error instanceof Error ? error.message : String(error),
			}));
		}
		return records.filter(record =>
			record.node_id === this.deps.nodeId && record.agent_id === agentId && record.thread_ref !== undefined,
		);
	}
}
