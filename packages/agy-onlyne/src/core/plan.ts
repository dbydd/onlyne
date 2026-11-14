import { OnlyneError, type Action, type DeliveryPlan, type Json, type MessageRef, type NodeId, type ResourceRef, type ThreadRef } from "@onlyne/lib";
import { toolText as t } from "./tool-text.ts";

export type ResourceAction = Extract<Action, { kind: "resource" }>;

export interface DeliveryModes {
	response?: DeliveryPlan["response"];
	execution?: DeliveryPlan["execution"];
}

export interface SendSpec {
	to: NodeId;
	thread?: ThreadRef;
	context?: string;
	reply?: { ref: MessageRef };
	relation?: "reply" | "followup";
	mentions?: NodeId[];
	response: DeliveryPlan["response"];
	execution: DeliveryPlan["execution"];
}

export function continueThread(existing: ThreadRef | undefined, picked: ThreadRef | undefined): ThreadRef | undefined {
	if (existing !== undefined) return existing;
	return picked;
}

export function inheritedModes(meta: Record<string, Json> | undefined): DeliveryModes {
	if (meta === undefined) return {};
	let response: DeliveryPlan["response"] | undefined;
	if (meta.response !== undefined) {
		if (meta.response !== "await" && meta.response !== "async" && meta.response !== "none") {
			throw new OnlyneError("invalid_request", t("planResponseInvalid"));
		}
		response = meta.response;
	}
	let execution: DeliveryPlan["execution"] | undefined;
	if (meta.execution !== undefined) {
		if (meta.execution !== "existing" && meta.execution !== "new") {
			throw new OnlyneError("invalid_request", t("planExecutionInvalid"));
		}
		execution = meta.execution;
	}
	return { response, execution };
}

export function resolvedModes(inherited: DeliveryModes, chosen: Record<string, string | undefined>): {
	response: DeliveryPlan["response"];
	execution: DeliveryPlan["execution"];
} {
	const response = inherited.response
		?? (chosen.response === "await" || chosen.response === "async" || chosen.response === "none" ? chosen.response : "async");
	const execution = inherited.execution
		?? (chosen.execution === "existing" || chosen.execution === "new" ? chosen.execution : "existing");
	return { response, execution };
}

export function resourceFields(meta: Record<string, Json> | undefined): {
	resource: ResourceRef;
	operation: string;
	data?: Json;
	expected_revision?: string;
} | undefined {
	if (meta === undefined) return undefined;
	const res = meta.resource;
	const op = meta.operation;
	if (res === undefined && op === undefined) return undefined;
	if (res === null || typeof res !== "object" || Array.isArray(res)) {
		throw new OnlyneError("invalid_request", t("resourceRefInvalid"));
	}
	const owner = (res as Record<string, Json>).owner;
	const id = (res as Record<string, Json>).id;
	if (typeof owner !== "string" || owner.length === 0 || typeof id !== "string" || id.length === 0) {
		throw new OnlyneError("invalid_request", t("resourceRefInvalid"));
	}
	if (typeof op !== "string" || op.trim().length === 0) {
		throw new OnlyneError("invalid_request", t("resourceOpInvalid"));
	}
	if (meta.expected_revision !== undefined && typeof meta.expected_revision !== "string") {
		throw new OnlyneError("invalid_request", t("resourceRevisionInvalid"));
	}
	return {
		resource: { owner, id },
		operation: op.trim(),
		...(meta.data === undefined ? {} : { data: meta.data }),
		...(typeof meta.expected_revision === "string" ? { expected_revision: meta.expected_revision } : {}),
	};
}

export function resourceAction(meta: Record<string, Json> | undefined): ResourceAction | undefined {
	const fields = resourceFields(meta);
	if (fields === undefined) return undefined;
	return {
		kind: "resource",
		resource: fields.resource,
		operation: fields.operation,
		...(fields.data === undefined ? {} : { data: fields.data }),
		...(fields.expected_revision === undefined ? {} : { expected_revision: fields.expected_revision }),
	};
}

export function sendAction(spec: SendSpec): Action {
	return {
		kind: "send",
		to: spec.to,
		...(spec.thread === undefined ? {} : { thread: spec.thread }),
		...(spec.context === undefined ? {} : { context: spec.context }),
		...(spec.reply === undefined ? {} : { in_reply_to: spec.reply.ref }),
		...(spec.relation === undefined ? {} : { relation: spec.relation }),
		...(spec.mentions === undefined ? {} : { mentions: spec.mentions }),
		response: spec.response,
		execution: spec.execution,
	};
}
