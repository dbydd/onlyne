import { OnlyneError, type Action, type Json, type NodeId, type ThreadRef } from "@onlyne/lib";
import { text } from "./decider-text.ts";
import { choice, type Answers } from "./questions.ts";
import { reuseThread, type ReplyReference, type ReferenceDeps } from "./resolve.ts";

export type ResponseMode = "await" | "async" | "none";
export type ExecutionMode = "existing" | "new";

/** The plan fields the caller already fixed in meta. */
export interface InheritedModes {
	response?: ResponseMode;
	execution?: ExecutionMode;
}

export interface ResourceFields {
	owner: NodeId;
	id: string;
	operation: string;
}

function stringField(record: Record<string, Json>, key: string): string | undefined {
	const value = record[key];
	return typeof value === "string" && value.length > 0 ? value : undefined;
}

function recordField(record: Record<string, Json>, key: string): Record<string, Json> | undefined {
	const value = record[key];
	return value !== null && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, Json>) : undefined;
}

/**
 * A plan field the caller stated is binding and is never asked again; a value outside the two
 * vocabularies is a caller error, never a reason to ask the judge. Anything the caller left out
 * is decided by the O-3A question, which is why the omission is returned as `undefined` here
 * rather than filled with a default (audit §2.5).
 */
export function inheritedModes(meta: Record<string, Json> | undefined): InheritedModes {
	if (meta === undefined) return {};
	const response = meta.response;
	const execution = meta.execution;
	if (response !== undefined && response !== "await" && response !== "async" && response !== "none") {
		throw new OnlyneError("invalid_request", text("error.meta_mode", { key: "response" }));
	}
	if (execution !== undefined && execution !== "existing" && execution !== "new") {
		throw new OnlyneError("invalid_request", text("error.meta_mode", { key: "execution" }));
	}
	return {
		...(response === undefined ? {} : { response }),
		...(execution === undefined ? {} : { execution }),
	};
}

/** The complete plan: what the caller fixed, plus what the judge answered for the rest. */
export function resolvedModes(inherited: InheritedModes, answers: Answers): { response: ResponseMode; execution: ExecutionMode } {
	const response = inherited.response ?? choice(answers, "response");
	const execution = inherited.execution ?? choice(answers, "execution");
	if (response !== "none" && response !== "async" && response !== "await") {
		throw new OnlyneError("decision_unavailable", text("error.response_mode"));
	}
	if (execution !== "existing" && execution !== "new") {
		throw new OnlyneError("decision_unavailable", text("error.execution_mode"));
	}
	return { response, execution };
}

/**
 * The fields a resource operation needs, or `undefined` when the call does not name a complete
 * one. A partial `meta.resource` is not a resource action, and it is not a reason to fall back
 * to communication either: the caller named a resource and must finish naming it.
 */
export function resourceFields(meta: Record<string, Json> | undefined): ResourceFields | undefined {
	if (meta === undefined) return undefined;
	const resource = recordField(meta, "resource");
	const owner = resource === undefined ? undefined : stringField(resource, "owner");
	const id = resource === undefined ? undefined : stringField(resource, "id");
	const operation = stringField(meta, "operation");
	return owner === undefined || id === undefined || operation === undefined ? undefined : { owner, id, operation };
}

/** The resource action both the explicit and the judged branch build. */
export function resourceAction(meta: Record<string, Json> | undefined): Action {
	const fields = resourceFields(meta);
	if (fields === undefined) throw new OnlyneError("invalid_request", text("error.resource_action_fields"));
	const data = meta === undefined ? undefined : meta.data;
	const revision = meta === undefined ? undefined : meta.expected_revision;
	return {
		kind: "resource",
		resource: { owner: fields.owner, id: fields.id },
		operation: fields.operation,
		...(data === undefined ? {} : { data }),
		...(typeof revision === "string" ? { expected_revision: revision } : {}),
	};
}

/**
 * The thread a send belongs to when the caller named none.
 *
 * A reply lands on the thread that holds its original (the client resolves it), and a plan
 * that says `new` opens a topic. Otherwise the message continues the conversation this node
 * already has with its addressee: leaving it unnamed makes the client open a thread per
 * message, and asking the model for an alias it cannot know is the mistake the design already
 * recorded. This is a lookup over local records, not a judgement (RFC 0016 §5).
 */
export async function continueThread(deps: ReferenceDeps, action: Action): Promise<Action> {
	if (action.kind !== "send" || action.thread !== undefined) return action;
	if (action.in_reply_to !== undefined || action.execution === "new") return action;
	const thread = await reuseThread(deps, action.to);
	return thread === undefined ? action : { ...action, thread };
}

/** One send action from its resolved references and judged plan. */
export function sendAction(parts: {
	to: NodeId;
	response: "await" | "async" | "none";
	execution: "existing" | "new";
	thread?: ThreadRef;
	context?: string;
	reply?: ReplyReference;
	relation?: "reply" | "followup";
	mentions?: string[];
}): Action {
	return {
		kind: "send",
		to: parts.to,
		...(parts.thread === undefined ? {} : { thread: parts.thread }),
		...(parts.context === undefined ? {} : { context: parts.context }),
		...(parts.reply === undefined ? {} : { in_reply_to: parts.reply.ref }),
		...(parts.relation === undefined ? {} : { relation: parts.relation }),
		...(parts.mentions === undefined ? {} : { mentions: parts.mentions }),
		response: parts.response,
		execution: parts.execution,
	};
}
