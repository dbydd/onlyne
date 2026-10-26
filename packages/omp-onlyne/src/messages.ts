import { OnlyneError, type DeliveryPlan, type Entry, type NotifyEvent, type Runtime, type TraceEvent } from "@onlyne/lib";
import type { ExtensionAPI, ExtensionContext } from "@oh-my-pi/pi-coding-agent";
import { AgentRegistry } from "@oh-my-pi/pi-coding-agent/registry/agent-registry";
import { renderMailMessage, type OnlyneMessageDetails } from "./ui/mail-message.ts";
import type { InboundDecision, JudgeDecider } from "./decider.ts";
import type { ContextRegistry } from "./store.ts";
import type { ContextRecord } from "./types.ts";
import { AWAIT_CONSUMED, pendingAwaitResults } from "./tool.ts";
import { applyMessageTodo, type TodoPlanner } from "./todo.ts";
import { getRuntime } from "./runtime.ts";

export const INBOUND = "onlyne.inbound";
export const ROUTING = "onlyne.routing";
export const SUPPLEMENT = "onlyne.supplement";

export interface BridgeDeps {
	pi: ExtensionAPI;
	main: ExtensionContext;
	decider: JudgeDecider;
	contexts: ContextRegistry;
	nodeId: string;
}
/**
 * One inbound entry is judged once per process. A retry replays the recorded decision
 * and, at most, the delivery it names; asking the judge again would turn the
 * maintenance tick into a model poll (RFC 0005 §1: one message gets one action).
 */
interface InboundAttempt {
	decision: InboundDecision;
	landed?: string;
}
const inboundAttempts = new Map<string, InboundAttempt>();

function inboundKey(entry: Entry): string {
	return `${entry.thread_ref.owner}/${entry.thread_ref.id}/${entry.seq}`;
}
/** Transitions that repeat every maintenance pass once a record is decided. */
const TRACE_DEBUG = new Set(["inbound.inject", "inbound.duplicate"]);

export function createRuntimeBridge(deps: BridgeDeps): Runtime {
	return {
		async supports(plan: DeliveryPlan): Promise<boolean> {
			if (plan.execution !== "new") return true;
			return taskTool(deps.main) !== undefined;
		},
		trace: (event: TraceEvent) => {
			const fields = { ...event } as Record<string, unknown>;
			const name = String(fields.event);
			delete fields.event;
			const thread = fields.thread as { owner?: unknown; id?: unknown } | undefined;
			if (thread !== undefined) fields.thread = `${String(thread.owner)}/${String(thread.id)}`;
			if (TRACE_DEBUG.has(name)) deps.pi.logger.debug(`onlyne.${name}`, fields);
			else deps.pi.logger.info(`onlyne.${name}`, fields);
		},
		async inject(entry: Entry, context?: string, plan?: DeliveryPlan) {
			if (plan?.execution === "new") return { context: await startSubagent(deps, entry), consumed: true, outcome: "consumed" as const };
			const key = inboundKey(entry);
			let attempt = inboundAttempts.get(key);
			if (attempt === undefined) {
				const started = Date.now();
				let decision: InboundDecision;
				try {
					decision = await deps.decider.decideInbound(deps.main, entry);
				} catch (error) {
					deps.pi.logger.warn("onlyne.judge.failed", {
						entry: key, ms: Date.now() - started, error: error instanceof Error ? error.message : String(error),
					});
					throw error;
				}
				attempt = { decision };
				inboundAttempts.set(key, attempt);
				deps.pi.logger.info("onlyne.judge.answered", {
					entry: key, action: decision.action, ms: Date.now() - started,
					...(decision.context === undefined ? {} : { context: decision.context }),
				});
			}
			const { decision } = attempt;
			if (decision.action === "new_subagent") {
				attempt.landed ??= await startSubagent(deps, entry);
				return { context: attempt.landed, consumed: true, outcome: "consumed" as const };
			}
			// The body is already where the decision put it; re-sending it would append a
			// second copy that the cursor can only confirm once.
			if (attempt.landed !== undefined) {
				deps.pi.logger.debug("onlyne.delivery.held", { entry: key, reason: "already-delivered", context: attempt.landed });
				return { context: attempt.landed, consumed: false, outcome: "delivered" as const };
			}
			if (decision.action === "context") {
				const alias = decision.context;
				if (alias === undefined) throw new OnlyneError("decision_unavailable", "The inbound decision named no context.");
				const record = await deps.contexts.find(alias);
				if (record?.agent_id === undefined) throw new OnlyneError("not_found", `No live context is registered as ${alias}.`);
				const token = registerPending(entry, record.agent_id, "fragment");
				try {
					await deliverToContext(record.agent_id, payload(entry, SUPPLEMENT, token), record.state === "running");
				} catch (error) {
					// The session behind this context is gone. Mark it unbound so the judge stops
					// offering it, and keep the entry owed: a silent delivery into a disposed
					// session is how a reply disappears (caerulea report 3, problem 1).
					await deps.contexts.updateState(alias, "unbound").catch(() => undefined);
					throw error;
				}
				attempt.landed = record.agent_id;
				deps.pi.logger.info("onlyne.delivery.sent", { entry: key, action: "context", target: record.agent_id, running: record.state === "running" });
				return { context: record.agent_id, consumed: false, outcome: "delivered" as const };
			}
			const token = registerPending(entry, deps.main.agent.id, "fragment");
			if (decision.action === "wake") {
				// An explicit deliverAs never starts a turn on an idle session, so an idle main
				// session gets a plain trigger whatever the insertion mode says.
				const idle = deps.main.isIdle();
				const mode = idle ? undefined : decision.insertion;
				deps.pi.sendMessage(
					payload(entry, INBOUND, token),
					mode === undefined ? { triggerTurn: true } : { deliverAs: mode === "steer" ? "steer" : "followUp" },
				);
			} else {
				deps.pi.sendMessage(payload(entry, INBOUND, token), { deliverAs: "nextTurn" });
			}
			attempt.landed = deps.main.agent.id;
			deps.pi.logger.info("onlyne.delivery.sent", {
				entry: key, action: decision.action, target: deps.main.agent.id, turn: decision.action === "wake",
				...(decision.insertion === undefined ? {} : { insertion: decision.insertion }),
			});
			return { context: deps.main.agent.id, consumed: false, outcome: "delivered" as const };
		},
		todo: async (entry, context) => {
			inboundAttempts.delete(inboundKey(entry));
			const planner: TodoPlanner = {
				decideTodo: (incoming, current) => deps.decider.decideTodo(deps.main, incoming, current),
			};
			await applyMessageTodo(entry, context, planner);
		},
		notify: (event: NotifyEvent) => {
			getRuntime()?.notify(event);
		},
	};
}

/** A provider request contains the payload; submission only schedules it. */
interface PendingObservation {
	entry: Entry;
	context: string;
	kind: "fragment" | "tool_result";
}
const pending = new Map<string, PendingObservation>();
const toolResults = new Map<string, PendingObservation>();
const observed = new Map<string, string[]>();

function registerPending(entry: Entry, context: string, kind: PendingObservation["kind"]): string {
	const token = `onlyne-${crypto.randomUUID()}`;
	pending.set(token, { entry, context, kind });
	return token;
}
export function observeToolResult(toolCallId: string, entry: Entry, context: string): void {
	toolResults.set(toolCallId, { entry, context, kind: "tool_result" });
}
export function registerProviderBoundary(pi: ExtensionAPI): void {
	pi.on("before_provider_request", (event, ctx) => {
		for (const result of pendingAwaitResults(ctx.sessionManager.getBranch())) {
			if (result.context === ctx.agent.id) observeToolResult(result.toolCallId, result.entry, result.context);
		}
		const text = typeof event.payload === "string" ? event.payload : JSON.stringify(event.payload);
		const found: string[] = [];
		for (const [token, record] of pending) if (record.context === ctx.agent.id && text.includes(token)) found.push(token);
		for (const [toolCallId, record] of toolResults) {
			if (record.context !== ctx.agent.id || !text.includes(toolCallId)) continue;
			pending.set(`tool:${toolCallId}`, record);
			toolResults.delete(toolCallId);
			found.push(`tool:${toolCallId}`);
		}
		observed.set(ctx.agent.id, found);
	});
	pi.on("after_provider_response", async (event, ctx) => {
		const tokens = observed.get(ctx.agent.id) ?? [];
		observed.delete(ctx.agent.id);
		if (event.status >= 400) return;
		const runtime = getRuntime();
		if (!runtime) return;
		for (const token of tokens) {
			const record = pending.get(token);
			if (!record) continue;
			try {
				await runtime.node.client.confirmConsumption(record.entry, record.context, record.kind);
				if (record.kind === "tool_result") pi.appendEntry(AWAIT_CONSUMED, {
					tool_call_id: token.slice("tool:".length), context: record.context,
					ref: { sender: record.entry.message.sender, id: record.entry.message.id },
					thread_ref: record.entry.thread_ref, seq: record.entry.seq, at: new Date().toISOString(),
				});
				pending.delete(token);
			} catch (error) {
				pi.logger.warn("onlyne.consumption.failed", {
					entry: inboundKey(record.entry), context: record.context, via: record.kind,
					error: error instanceof Error ? error.message : String(error),
				});
			}
		}
	});
}

async function deliverToContext(agentId: string, payload: MessagePayload, running: boolean): Promise<void> {
	// Never revive a session for an inbound delivery: a disposed subagent accepts the message
	// where nobody reads it, and the library records the injection as delivered.
	const session = AgentRegistry.global().get(agentId)?.session ?? undefined;
	if (session === undefined) throw new OnlyneError("not_found", `The execution context ${agentId} has no live session; it must be bound again before it can receive messages.`);
	await session.sendCustomMessage(payload, running ? { deliverAs: "steer" } : { triggerTurn: true });
}
/**
 * One source message owns one context, for the process lifetime: a replay of the same
 * inbox record (the maintain loop's every-2s pass) must never spawn a second task. A
 * record bound to a live agent is returned as is; an unbound one from a failed earlier
 * spawn is reused under its own alias. The task text is the child's first prompt, so a
 * finished spawn has already consumed the message — the caller reports it as consumed.
 */
async function startSubagent(deps: BridgeDeps, entry: Entry): Promise<string> {
	const existing = await findContextBySource(deps.contexts, entry);
	if (existing?.agent_id !== undefined) return existing.agent_id;
	const tool = taskTool(deps.main);
	if (!tool) throw new OnlyneError("unsupported_delivery", "This OMP session has no native task tool.");
	// Both references the child can actually use: the message id for `{sender,id}` replies and
	// the thread/seq pair for reads. A bare `thread/seq` string fits neither (cyanea report 1).
	const source = `${entry.thread_ref.owner}/${entry.thread_ref.id}/${entry.seq}`;
	const references = `message ${entry.message.id}, thread ${entry.thread_ref.owner}/${entry.thread_ref.id} seq ${entry.seq}`;
	const record = existing ?? await deps.contexts.allocate(entry.thread_ref, `onlyne ${source}`, entry.message);
	const name = `onlyne-${record.alias}`;
	const result = await tool.execute(`onlyne-${crypto.randomUUID()}`, {
		// The child cannot send: its task text says so, because a session told to "handle" a
		// delivery with no send right reads the refusal as its own failure (dpdp report 4).
		task: `Read this onlyne delivery from ${entry.authenticated_author} (${references}) and return what the main session should send back; this session cannot send.\n\n${entry.message.msg}`,
		solutionSpace: "The delivery decides the work; use the message as the task.",
		name,
	});
	const details = result.details as unknown as { progress?: Array<{ id?: string; index?: number }>; results?: Array<{ id?: string; index?: number }> } | undefined;
	const child = [...(details?.progress ?? []), ...(details?.results ?? [])].find(item => typeof item.id === "string" && item.id.length > 0)?.id;
	if (child === undefined) throw new OnlyneError("unavailable", "The native task tool returned no child identity.");
	const parentId = AgentRegistry.global().get(child)?.parentId;
	await deps.contexts.bind(record.alias, { agent_id: child, ...(parentId === undefined ? {} : { parent_id: parentId }) });
	return child;
}
async function findContextBySource(contexts: ContextRegistry, entry: Entry): Promise<ContextRecord | undefined> {
	const records = await contexts.list();
	return records.find(record =>
		record.source?.sender === entry.message.sender && record.source?.id === entry.message.id
		&& record.thread_ref?.owner === entry.thread_ref.owner && record.thread_ref?.id === entry.thread_ref.id);
}
function taskTool(ctx: ExtensionContext) {
	return AgentRegistry.global().get(ctx.agent.id)?.session?.getToolByName("task");
}
interface MessagePayload {
	customType: string;
	content: string;
	display: true;
	attribution: "agent";
	details: OnlyneMessageDetails;
}
function payload(entry: Entry, customType: string, token: string): MessagePayload {
	return { customType, content: `${renderEntry(entry, customType)}\n\n[[${token}]]`, display: true, attribution: "agent" as const,
		details: {
			thread_ref: entry.thread_ref, seq: entry.seq,
			message_ref: { sender: entry.message.sender, id: entry.message.id }, consumption: "pending",
			mail: {
				from: entry.authenticated_author, at: entry.at, subject: entry.message.delivery_kind, body: entry.message.msg,
				...(entry.message.meta?.relation === undefined ? {} : { relation: entry.message.meta.relation }),
				...(entry.message.meta?.in_reply_to === undefined ? {} : { in_reply_to: entry.message.meta.in_reply_to }),
			},
		} };
}
export function registerMessageRenderers(pi: ExtensionAPI): void {
	for (const type of [INBOUND, ROUTING, SUPPLEMENT]) pi.registerMessageRenderer(type, renderMailMessage);
}
function renderEntry(entry: Entry, customType: string): string {
	const label = customType === SUPPLEMENT ? "supplement" : "inbound";
	const relation = entry.message.meta?.relation === undefined ? "" : ` · ${entry.message.meta.relation}`;
	return `${entry.thread_ref.id}/${entry.seq} · ${label} · ${entry.authenticated_author}${relation}\n${entry.message.msg.split(/\r?\n/).map(line => `  > ${line}`).join("\n")}`;
}
