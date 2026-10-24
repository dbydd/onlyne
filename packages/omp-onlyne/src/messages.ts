import { OnlyneError, type DeliveryPlan, type Entry, type Runtime } from "@onlyne/lib";
import type { ExtensionAPI, ExtensionContext } from "@oh-my-pi/pi-coding-agent";
import { AgentLifecycleManager } from "@oh-my-pi/pi-coding-agent/registry/agent-lifecycle";
import { AgentRegistry } from "@oh-my-pi/pi-coding-agent/registry/agent-registry";
import { Text } from "@oh-my-pi/pi-tui";
import type { JudgeDecider } from "./decider.ts";
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
export function createRuntimeBridge(deps: BridgeDeps): Runtime {
	return {
		async supports(plan: DeliveryPlan): Promise<boolean> {
			if (plan.execution !== "new") return true;
			return taskTool(deps.main) !== undefined;
		},
		async inject(entry: Entry, context?: string, plan?: DeliveryPlan) {
			const decision = plan?.execution === "new" ? { action: "new_subagent" as const } : await deps.decider.decideInbound(deps.main, entry);
			if (decision.action === "queue") return { context: context ?? deps.main.agent.id, consumed: false };
			if (decision.action === "new_subagent") return { context: await startSubagent(deps, entry), consumed: true };
			if (decision.action === "context") {
				const alias = decision.context;
				if (alias === undefined) throw new OnlyneError("decision_unavailable", "The inbound decision named no context.");
				const record = await deps.contexts.find(alias);
				if (record?.agent_id === undefined) throw new OnlyneError("not_found", `No live context is registered as ${alias}.`);
				const token = registerPending(entry, record.agent_id, "fragment");
				await deliverToContext(record.agent_id, payload(entry, SUPPLEMENT, token), record.state === "running");
				return { context: record.agent_id, consumed: false };
			}
			if (decision.action === "wait_result") return { context: context ?? deps.main.agent.id, consumed: false };
			const token = registerPending(entry, deps.main.agent.id, "fragment");
			if (decision.action === "wake") deps.pi.sendMessage(payload(entry, INBOUND, token), { triggerTurn: true });
			else deps.pi.sendMessage(payload(entry, INBOUND, token), { deliverAs: "nextTurn" });
			return { context: deps.main.agent.id, consumed: false };
		},
		todo: async (entry, context) => {
			const planner: TodoPlanner = {
				decideTodo: (incoming, current) => deps.decider.decideTodo(deps.main, incoming, current),
			};
			await applyMessageTodo(entry, context, planner);
		},
		wake: () => getRuntime()?.wake(),
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
				pi.logger.warn("onlyne consumption confirmation failed", { error: error instanceof Error ? error.message : String(error) });
			}
		}
	});
}

async function deliverToContext(agentId: string, payload: MessagePayload, running: boolean): Promise<void> {
	const ref = AgentRegistry.global().get(agentId);
	const session = ref?.session ?? (await AgentLifecycleManager.global().ensureLive(agentId));
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
	const source = entry.thread_ref.id + "/" + entry.seq;
	const record = existing ?? await deps.contexts.allocate(entry.thread_ref, `onlyne ${source}`, entry.message);
	const name = `onlyne-${record.alias}`;
	const result = await tool.execute(`onlyne-${crypto.randomUUID()}`, {
		task: `Handle this onlyne delivery from ${entry.authenticated_author} (${source}).\n\n${entry.message.msg}`,
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
	details: { thread_ref: Entry["thread_ref"]; seq: string; message_ref: { sender: string; id: string }; consumption: string };
}
function payload(entry: Entry, customType: string, token: string): MessagePayload {
	return { customType, content: `${renderEntry(entry, customType)}\n\n[[${token}]]`, display: true, attribution: "agent" as const,
		details: { thread_ref: entry.thread_ref, seq: entry.seq, message_ref: { sender: entry.message.sender, id: entry.message.id }, consumption: "pending" } };
}
export function registerMessageRenderers(pi: ExtensionAPI): void {
	for (const type of [INBOUND, ROUTING, SUPPLEMENT]) pi.registerMessageRenderer(type, message => new Text(renderCustomMessage(message), 1, 1));
}
function renderCustomMessage(message: unknown): string {
	const record = message as { content?: unknown };
	return typeof record.content === "string" ? record.content : textOf(record.content) ?? "";
}
function renderEntry(entry: Entry, customType: string): string {
	const label = customType === SUPPLEMENT ? "supplement" : "inbound";
	const relation = entry.message.meta?.relation === undefined ? "" : ` · ${entry.message.meta.relation}`;
	return `${entry.thread_ref.id}/${entry.seq} · ${label} · ${entry.authenticated_author}${relation}\n${entry.message.msg.split(/\r?\n/).map(line => `  > ${line}`).join("\n")}`;
}
function textOf(value: unknown): string | undefined {
	if (typeof value === "string") return value;
	if (Array.isArray(value)) return value.map(part => typeof part === "string" ? part : part && typeof part === "object" && "text" in part && typeof part.text === "string" ? part.text : "").join("") || undefined;
	if (value && typeof value === "object" && "text" in value && typeof value.text === "string") return value.text;
	return undefined;
}
