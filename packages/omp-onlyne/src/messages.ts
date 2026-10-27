import { OnlyneError, type DeliveryPlan, type Entry, type Message, type NotifyEvent, type Runtime, type TraceEvent } from "@onlyne/lib";
import type { ExtensionAPI, ExtensionContext } from "@oh-my-pi/pi-coding-agent";
import { AgentRegistry } from "@oh-my-pi/pi-coding-agent/registry/agent-registry";
import { refreshMailCards, renderMailMessage, type OnlyneMessageDetails } from "./ui/mail-message.ts";
import type { InboundDecision, JudgeDecider } from "./decider.ts";
import type { ContextRegistry } from "./store.ts";
import type { ContextRecord } from "./types.ts";
import { AWAIT_CONSUMED, pendingAwaitResults } from "./tool.ts";
import { applyMessageTodo, type TodoPlanner } from "./todo.ts";
import { getRuntime } from "./runtime.ts";
import { handshakeFacts } from "./handshake.ts";
import { bridgeText as t } from "./bridge-text.ts";

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
 * maintenance tick into a model poll (RFC 0005 §1: one message gets one action). A
 * failed judgment is recorded too: the entry stays owed, and the tick does not re-ask.
 */
interface InboundAttempt {
	decision?: InboundDecision;
	failure?: OnlyneError;
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
		/**
		 * Admission is the last cheap place to refuse a plan this host cannot honor (RFC 0005
		 * §1): a `new` context needs the task tool. It runs inside the library's admission
		 * transaction, so it may not read the context registry — that opens a second transaction
		 * on the same store and waits on itself. A plan that names a lost context is refused by
		 * `decideInbound` instead, which runs outside any transaction.
		 */
		async supports(plan: DeliveryPlan, _message?: Message): Promise<boolean> {
			return plan.execution !== "new" || taskTool(deps.main) !== undefined;
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
			const key = inboundKey(entry);
			let attempt = inboundAttempts.get(key);
			if (attempt === undefined) {
				const started = Date.now();
				let decision: InboundDecision;
				try {
					decision = await deps.decider.decideInbound(deps.main, entry, plan);
				} catch (error) {
					const failure = error instanceof OnlyneError
						? error
						: new OnlyneError("decision_unavailable", error instanceof Error ? error.message : String(error));
					deps.pi.logger.warn("onlyne.judge.failed", {
						entry: key, ms: Date.now() - started, error: failure.message,
					});
					inboundAttempts.set(key, { failure });
					throw failure;
				}
				attempt = { decision };
				inboundAttempts.set(key, attempt);
				deps.pi.logger.info("onlyne.judge.answered", {
					entry: key, action: decision.action, ms: Date.now() - started,
					...(decision.context === undefined ? {} : { context: decision.context }),
				});
			}
			if (attempt.failure !== undefined) {
				deps.pi.logger.debug("onlyne.delivery.held", { entry: key, reason: "judge-failed", error: attempt.failure.message });
				throw attempt.failure;
			}
			const decision = attempt.decision;
			if (decision === undefined) throw new OnlyneError("decision_unavailable", t("noDecision"));
			// The body is already where the decision put it; re-sending it would append a
			// second copy that the cursor can only confirm once.
			if (attempt.landed !== undefined) {
			// RFC 0008 §5: a delivery is landed only while the context holding it can still
			// confirm it. A subagent session that ended leaves the entry unconfirmed forever,
			// and reporting it delivered on every maintenance pass is a retry storm, not a
			// delivery. Mark the dead binding lost and let the next pass decide again.
			const holder = (await deps.contexts.list()).find(record => record.agent_id === attempt.landed);
			if (holder !== undefined && holder.state !== "idle" && holder.state !== "running") {
				await deps.contexts.updateState(holder.alias, "lost").catch(() => undefined);
				inboundAttempts.delete(key);
				throw new OnlyneError("context_lost", t("contextLost", { alias: holder.alias }), { context: holder.alias });
			}
				deps.pi.logger.debug("onlyne.delivery.held", { entry: key, reason: "already-delivered", context: attempt.landed });
				return { context: attempt.landed, consumed: false, outcome: "delivered" as const };
			}
			if (decision.action === "new_subagent") {
				attempt.landed = await startSubagent(deps, entry);
				// A spawn is a submission, not a read: the child's prompt carries the token and its
				// first provider request is the proof, so nothing is consumed here.
				return { context: attempt.landed, consumed: false, outcome: "delivered" as const };
			}
			if (decision.action === "context") {
				const alias = decision.context;
				if (alias === undefined) throw new OnlyneError("decision_unavailable", t("noContextNamed"));
				const record = await deps.contexts.find(alias);
				if (record?.agent_id === undefined || record.state === "lost" || record.state === "unbound") {
					// The binding went away between the decision and the delivery: the entry stays
					// owed, and an explicit new decision is the only way to land it (RFC 0005 §1).
					throw new OnlyneError("context_lost", t("contextLost", { alias }), { context: alias });
				}
				const token = registerPending(entry, record.agent_id, "fragment");
				try {
					await deliverToContext(record.agent_id, payload(entry, SUPPLEMENT, token), record.state === "running");
				} catch (error) {
					// The session behind this context is gone. Mark the binding lost so no decision
					// offers it again, and keep the entry owed: a silent delivery into a disposed
					// session is how a reply disappears (caerulea report 3, problem 1).
					await deps.contexts.updateState(alias, "lost").catch(() => undefined);
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
			// RFC 0018 §3: a handshake status update is delivered for the record, not as work.
			// Only an entry that owes a decision takes the todo decision path.
			if (handshakeFacts(entry.message.meta)?.pending === false) {
				refreshMailCards();
				return;
			}
			const planner: TodoPlanner = {
				decideTodo: (incoming, current) => deps.decider.decideTodo(deps.main, incoming, current),
			};
			await applyMessageTodo(entry, context, planner);
			// The card reports the consumption this hook just served.
			refreshMailCards();
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

function registerPending(entry: Entry, context: string, kind: PendingObservation["kind"], token = `onlyne-${crypto.randomUUID()}`): string {
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
				// A confirmed consumption changes what the delivery card reports.
				refreshMailCards();
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
	if (session === undefined) throw new OnlyneError("not_found", t("noLiveSession", { context: agentId }));
	await session.sendCustomMessage(payload, running ? { deliverAs: "steer" } : { triggerTurn: true });
}
/**
 * One source message owns one context, for the process lifetime: a replay of the same
 * inbox record (the maintain loop's every-2s pass) must never spawn a second task. A
 * record bound to a live agent is returned as is; an unbound one from a failed earlier
 * spawn is reused under its own alias. The task text is the child's first prompt and it
 * carries the consumption token, so the child's first provider request is what proves the
 * model read the delivery; the spawn itself consumes nothing.
 */
async function startSubagent(deps: BridgeDeps, entry: Entry): Promise<string> {
	const existing = await findContextBySource(deps.contexts, entry);
	if (existing?.agent_id !== undefined) return existing.agent_id;
	const tool = taskTool(deps.main);
	if (!tool) throw new OnlyneError("unsupported_delivery", t("noTaskTool"));
	// Both references the child can actually use: the message id for `{sender,id}` replies and
	// the thread/seq pair for reads. A bare `thread/seq` string fits neither (cyanea report 1).
	const source = `${entry.thread_ref.owner}/${entry.thread_ref.id}/${entry.seq}`;
	const references = t("deliveryReferences", {
		message: entry.message.id,
		thread: `${entry.thread_ref.owner}/${entry.thread_ref.id}`,
		seq: entry.seq,
	});
	const record = existing ?? await deps.contexts.allocate(entry.thread_ref, `onlyne ${source}`, entry.message);
	const name = `onlyne-${record.alias}`;
	const token = `onlyne-${crypto.randomUUID()}`;
	const result = await tool.execute(`onlyne-${crypto.randomUUID()}`, {
		// The child cannot send: its task text says so and names the one structured return that
		// does work, because a session told to "handle" a delivery with no send right reads the
		// refusal as its own failure (dpdp report 4).
		task: [
			t("subagentPreamble", { author: entry.authenticated_author, references }),
			t("subagentCannotSend"),
			t("subagentDraftCall", {
				to: JSON.stringify(entry.reply_authority),
				sender: JSON.stringify(entry.message.sender),
				id: JSON.stringify(entry.message.id),
			}),
			t("subagentNoRetry"),
			`[[${token}]]`,
			"",
			entry.message.msg,
		].join("\n"),
		solutionSpace: t("subagentSolutionSpace"),
		name,
	});
	const details = result.details as unknown as { progress?: Array<{ id?: string; index?: number }>; results?: Array<{ id?: string; index?: number }> } | undefined;
	const child = [...(details?.progress ?? []), ...(details?.results ?? [])].find(item => typeof item.id === "string" && item.id.length > 0)?.id;
	if (child === undefined) throw new OnlyneError("unavailable", t("noChildIdentity"));
	const parentId = AgentRegistry.global().get(child)?.parentId;
	await deps.contexts.bind(record.alias, { agent_id: child, ...(parentId === undefined ? {} : { parent_id: parentId }) });
	// The token is already in the child's prompt, so it can only be registered once the child's
	// identity is known; until then there is no provider boundary to observe it at.
	// It is registered as the token the prompt carries: a fresh one would never appear in any
	// provider payload, and the delivery could never be confirmed.
	registerPending(entry, child, "fragment", token);
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
			message_ref: { sender: entry.message.sender, id: entry.message.id },
			// This card exists because the entry reached this node's inbox; whether the model has
			// consumed it is read from history, never assumed here.
			delivery: "received", consumption: "pending",
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
	const label = customType === SUPPLEMENT ? t("labelSupplement") : t("labelInbound");
	const relation = entry.message.meta?.relation === undefined ? "" : ` · ${entry.message.meta.relation}`;
	return `${entry.thread_ref.id}/${entry.seq} · ${label} · ${entry.authenticated_author}${relation}\n${entry.message.msg.split(/\r?\n/).map(line => `  > ${line}`).join("\n")}`;
}
