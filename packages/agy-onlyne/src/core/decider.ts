import {
	OnlyneError,
	trustPhase,
	type Action,
	type DeliveryPlan,
	type DiscoveryRecord,
	type Entry,
	type JoinedThread,
	type Json,
	type LocalAttention,
	type MessageRef,
	type MsgInput,
	type NodeId,
	type Page,
	type ResourceReadQuery,
	type Thread,
	type ThreadRef,
	type TrustEntry,
} from "@onlyne/lib";
import type { ContextRegistry } from "@onlyne/node-host/store";
import type { ContextRecord } from "@onlyne/node-host/types";
import type { AgyOnlyneConfig } from "../config.ts";
import { text } from "./decider-text.ts";
import { ActionGuard, settleIntent, type OutboundIntent } from "./guard.ts";
import { continueThread, inheritedModes, resolvedModes, resourceAction, resourceFields, sendAction } from "./plan.ts";
import {
	contextQuestions,
	domainOpQuestions,
	inboundContextQuestions,
	intentQuestions,
	originalQuestions,
	planQuestions,
	readScopeQuestions,
	relationQuestions,
	targetQuestions,
	threadOpQuestions,
	threadQuestions,
	type Answers,
	type Questions,
} from "./questions.ts";
import { References, reuseThread, stringField, threadKey, type PeerRecord, type ReplyReference } from "./resolve.ts";
import { resourceQuery } from "./resources.ts";

export { reuseThread, threadKey, type PeerRecord, type ReplyReference };

export const STATE_BUDGET_CHARS = 40_000;
const MAX_BODY_SNIPPET = 280;
const MAX_TAIL_PAGES = 10;
const REPLY_CANDIDATES = 8;

export interface CallerContext {
	agent_id: string;
	main: boolean;
	read_only?: boolean;
	recent_user_prompt?: string;
	last_thought?: string;
}

export interface DecisionRecord {
	at: string;
	node: string;
	input: Json;
	answers: Answers;
	duration_ms: number;
}

export type JudgeFn = (state: Json, questions: Questions, signal?: AbortSignal) => Promise<Answers>;

export interface DeciderDeps {
	config: AgyOnlyneConfig;
	mainAgentId: string;
	node: import("@onlyne/lib").OnlyneNode;
	contexts: ContextRegistry;
	listThreads(cursor?: string): Promise<Page<Thread>>;
	listJoined(cursor?: string): Promise<Page<JoinedThread>>;
	listInbox(): Promise<LocalAttention[]>;
	listMessages(thread: ThreadRef, cursor?: string): Promise<Page<Entry>>;
	listDiscovery(): Promise<DiscoveryRecord[]>;
	listTrust(): Promise<TrustEntry[]>;
	peerBase(nodeId: NodeId): Promise<string | undefined>;
	targetContexts(nodeId: NodeId): Promise<{ available: boolean; reason?: string; value: Array<{ alias: string; state?: string; summary?: string }> }>;
	judge?: JudgeFn;
	log?(event: string, fields?: Record<string, unknown>): void;
}

export interface InboundDecision {
	action: "deliver";
	context: string;
	execution: "existing" | "new";
	summary?: string;
}

export interface TodoDecision {
	action: "none" | "associate" | "append";
	phase?: string;
	title?: string;
	explanation?: string;
}

interface DecisionScope {
	caller: CallerContext;
	input: MsgInput;
	signal?: AbortSignal;
}

function asRecord(value: unknown): Record<string, Json> | undefined {
	return value !== null && typeof value === "object" && !Array.isArray(value)
		? value as Record<string, Json>
		: undefined;
}

function boundText(value: string, max = MAX_BODY_SNIPPET): string {
	const flat = value.replace(/\s+/g, " ").trim();
	return flat.length <= max ? flat : `${flat.slice(0, max - 1)}…`;
}

function lastSegment(nodeId: string): string {
	const raw = nodeId.split(":").at(-1) ?? nodeId;
	try {
		return decodeURIComponent(raw);
	} catch {
		return raw;
	}
}

export function fitState(state: Record<string, unknown>, budget = STATE_BUDGET_CHARS): Json {
	const clone = JSON.parse(JSON.stringify(state)) as Record<string, Json>;
	let raw = JSON.stringify(clone);
	if (raw.length <= budget) return clone;
	for (const key of ["candidates", "candidate_peers", "threads", "target_published_contexts", "current_contexts"]) {
		const arr = clone[key];
		if (!Array.isArray(arr)) continue;
		while (arr.length > 2 && JSON.stringify(clone).length > budget) {
			arr.pop();
		}
	}
	raw = JSON.stringify(clone);
	if (raw.length <= budget) return clone;
	return { truncated: true, focus: clone.focus ?? null };
}

function choice(answers: Answers, key: string): string | undefined {
	const raw = answers[key]?.choice;
	return typeof raw === "string" && raw.trim().length > 0 ? raw.trim() : undefined;
}

function isReadScope(value: string): value is "directory" | "neighbors" | "threads" | "inbox" | "messages" | "message" | "evidence" | "resources" {
	return ["directory", "neighbors", "threads", "inbox", "messages", "message", "evidence", "resources"].includes(value);
}

function isThreadOp(value: string): value is "create" | "close" | "list" | "describe" | "member_add" | "member_remove" {
	return ["create", "close", "list", "describe", "member_add", "member_remove"].includes(value);
}

function isDomainOp(value: string): value is "create" | "join" | "leave" | "list" | "describe" | "discover" {
	return ["create", "join", "leave", "list", "describe", "discover"].includes(value);
}

export class JudgeDecider {
	readonly refs: References;
	readonly guard: ActionGuard;
	readonly history: DecisionRecord[] = [];

	constructor(private readonly deps: DeciderDeps) {
		this.refs = new References({
			config: deps.config,
			listThreads: deps.listThreads,
			listJoined: deps.listJoined,
			listInbox: deps.listInbox,
			listMessages: deps.listMessages,
			listDiscovery: deps.listDiscovery,
			listTrust: deps.listTrust,
			peerBase: deps.peerBase,
		});
		this.guard = new ActionGuard({ contexts: deps.contexts, nodeId: deps.config.node.id });
	}

	recentDecisions(limit = 30): DecisionRecord[] {
		return this.history.slice(-limit);
	}

	async decide(caller: CallerContext, input: MsgInput, signal?: AbortSignal): Promise<Action> {
		const scope: DecisionScope = { caller, input, signal };
		const meta = asRecord(input.meta);
		const action = await this.#explicit(scope, meta) ?? await this.#decideWithJudge(scope, meta);
		await this.guard.assert({ readOnly: caller.read_only, agentId: caller.agent_id }, action);
		return action;
	}

	async decideInbound(entry: Entry, preferredContext?: string): Promise<InboundDecision> {
		const records = (await this.deps.contexts.list()).filter(rec => rec.node_id === this.deps.config.node.id && rec.state !== "lost");
		const plan = undefined as { execution?: string } | undefined;
		void plan;

		if (preferredContext !== undefined && preferredContext !== "main") {
			const match = records.find(rec => rec.alias === preferredContext);
			if (match !== undefined) {
				return { action: "deliver", context: match.alias, execution: "existing", summary: match.summary };
			}
			return {
				action: "deliver",
				context: preferredContext,
				execution: "new",
				summary: boundText(entry.message.msg, 120),
			};
		}

		// Follow reply lineage locally if this message replies to a thread bound to a subagent context.
		const threadMatched = records.find(rec =>
			rec.alias !== "main"
			&& rec.thread_ref !== undefined
			&& threadKey(rec.thread_ref) === threadKey(entry.thread_ref));
		if (threadMatched !== undefined) {
			return { action: "deliver", context: threadMatched.alias, execution: "existing", summary: threadMatched.summary };
		}

		if (records.length <= 1 || this.deps.judge === undefined) {
			return { action: "deliver", context: "main", execution: "existing" };
		}

		const started = Date.now();
		try {
			const answers = await this.deps.judge(
				fitState({
					node: this.deps.config.node.id,
					incoming: {
						from: entry.message.sender,
						kind: entry.message.delivery_kind,
						thread: threadKey(entry.thread_ref),
						seq: entry.seq,
						body: boundText(entry.message.msg),
					},
					current_contexts: records.map(r => ({
						alias: r.alias,
						state: r.state,
						thread: r.thread_ref ? threadKey(r.thread_ref) : null,
						summary: r.summary,
					})),
				}),
				inboundContextQuestions(records, true),
			);
			this.#record("inbound.i1", { sender: entry.message.sender, id: entry.message.id }, answers, Date.now() - started);
			const chosen = choice(answers, "route");
			if (chosen === "new_context") {
				return {
					action: "deliver",
					context: `ctx-${crypto.randomUUID().slice(0, 6)}`,
					execution: "new",
					summary: boundText(entry.message.msg, 120),
				};
			}
			if (chosen !== undefined && records.some(r => r.alias === chosen)) {
				return { action: "deliver", context: chosen, execution: "existing" };
			}
		} catch (error) {
			this.deps.log?.("onlyne.inbound.judge_fallback", {
				error: error instanceof Error ? error.message : String(error),
			});
		}
		return { action: "deliver", context: "main", execution: "existing" };
	}

	async decideTodo(entry: Entry, existingTitles: readonly string[]): Promise<TodoDecision> {
		if (entry.message.sender === this.deps.config.node.id) return { action: "none" };
		if (entry.thread_ref.id === "trust") return { action: "none" };
		const textBody = boundText(entry.message.msg, 160);
		if (textBody.length === 0) return { action: "none" };
		const kind = entry.message.delivery_kind.toLowerCase();
		if (["ack", "pong", "fyi", "notify"].includes(kind)) {
			return { action: "none" };
		}
		if (this.deps.judge === undefined) {
			if (["question", "request", "handoff", "task", "todo"].includes(kind)) {
				return {
					action: "append",
					phase: "Inbound",
					title: `${lastSegment(entry.message.sender)}: ${boundText(entry.message.msg, 72)}`,
					explanation: `thread=${threadKey(entry.thread_ref)}#${entry.seq}`,
				};
			}
			return { action: "none" };
		}
		const criteria: Record<string, string> = {
			none: text("judge.todo.action.none"),
			append: text("judge.todo.action.append"),
			...(existingTitles.length > 0 ? { associate: text("judge.todo.action.associate") } : {}),
		};
		try {
			const started = Date.now();
			const answers = await this.deps.judge(
				fitState({
					incoming: {
						from: entry.message.sender,
						kind: entry.message.delivery_kind,
						thread: threadKey(entry.thread_ref),
						seq: entry.seq,
						body: textBody,
					},
					existing_todos: existingTitles,
				}),
				{
					action: {
						type: "choice",
						instructions: text("judge.todo.action.instructions"),
						criteria,
					},
				},
			);
			this.#record("todo.t1", { sender: entry.message.sender, id: entry.message.id }, answers, Date.now() - started);
			const chosen = choice(answers, "action");
			if (chosen === "append") {
				return {
					action: "append",
					phase: "Inbound",
					title: `${lastSegment(entry.message.sender)}: ${boundText(entry.message.msg, 72)}`,
					explanation: `thread=${threadKey(entry.thread_ref)}#${entry.seq}`,
				};
			}
			if (chosen === "associate" && existingTitles.length > 0) {
				if (existingTitles.length === 1) return { action: "associate", title: existingTitles[0] };
				const assocAnswers = await this.deps.judge(
					fitState({ incoming: { body: textBody }, existing_todos: existingTitles }),
					{
						task: {
							type: "choice",
							instructions: text("judge.todo.associate.instructions"),
							criteria: Object.fromEntries(existingTitles.map(item => [item, text("judge.todo.associate.item")])),
						},
					},
				);
				const picked = choice(assocAnswers, "task");
				if (picked !== undefined && existingTitles.includes(picked)) {
					return { action: "associate", title: picked };
				}
			}
			return { action: "none" };
		} catch {
			return { action: "none" };
		}
	}

	async #explicit(scope: DecisionScope, meta: Record<string, Json> | undefined): Promise<Action | undefined> {
		const resAct = resourceAction(meta);
		if (resAct !== undefined) return resAct;
		if (meta === undefined) return undefined;

		const readScope = stringField(meta, "scope");
		if (readScope !== undefined && isReadScope(readScope)) {
			return this.#readActionFromScope(scope, readScope, meta);
		}
		const op = stringField(meta, "op");
		if (op !== undefined) {
			if (meta.domain_id !== undefined && isDomainOp(op)) return this.#buildDomainAction(op, meta);
			if ((meta.thread !== undefined || meta.title !== undefined || scope.input.delivery_kind === "thread") && isThreadOp(op)) {
				return this.#buildThreadAction(scope, op, meta);
			}
		}
		if (scope.input.delivery_kind === "read" && readScope === undefined) {
			return this.#readAction(scope, meta);
		}
		return undefined;
	}

	async #decideWithJudge(scope: DecisionScope, meta: Record<string, Json> | undefined): Promise<Action> {
		let intent: OutboundIntent;
		if (meta !== undefined && (typeof meta.to === "string" || meta.in_reply_to !== undefined)) {
			intent = "communicate";
		} else {
			const started = Date.now();
			const answers = await this.#judge(
				scope.caller,
				{ focus: this.#focus(scope), trigger: this.#trigger(scope.caller) },
				intentQuestions(),
				scope.signal,
			);
			this.#record("outbound.o1", scope.input, answers, Date.now() - started);
			const chosen = choice(answers, "intent");
			if (chosen !== "communicate" && chosen !== "inspect" && chosen !== "manage_thread"
				&& chosen !== "manage_domain" && chosen !== "operate_resource") {
				throw new OnlyneError("decision_unavailable", text("error.intent_unknown"));
			}
			intent = settleIntent(chosen, meta).intent;
		}

		if (intent === "operate_resource") {
			const act = resourceAction(meta);
			if (act !== undefined) return act;
			throw new OnlyneError("invalid_request", text("error.intent_unavailable", {
				reason: "operate_resource requires meta.resource and meta.operation",
			}));
		}
		if (intent === "inspect") return this.#readAction(scope, meta ?? {});
		if (intent === "manage_thread") return this.#threadAction(scope, meta ?? {});
		if (intent === "manage_domain") return this.#domainAction(scope, meta ?? {});
		return this.#communicateAction(scope, meta ?? {});
	}

	async #communicateAction(scope: DecisionScope, meta: Record<string, Json>): Promise<Action> {
		const explicitRelation = stringField(meta, "relation");
		if (explicitRelation !== undefined && explicitRelation !== "reply" && explicitRelation !== "followup") {
			throw new OnlyneError("invalid_request", text("error.reply_relation"));
		}
		const refs = {
			target: await this.refs.target(meta),
			thread: await this.refs.thread(meta),
			mentions: await this.refs.mentions(meta),
			reply: await this.refs.reply(meta),
		};

		let reply = refs.reply;
		if (reply === undefined && explicitRelation === undefined) {
			reply = await this.#originalQuestion(scope, refs.target);
		}
		if (reply === undefined && explicitRelation !== undefined) {
			throw new OnlyneError("invalid_request", text("error.relation_needs_reference"));
		}

		let lockedRelation: "reply" | "followup" | undefined = explicitRelation as "reply" | "followup" | undefined;
		if (reply !== undefined && lockedRelation === undefined) {
			lockedRelation = await this.#relationQuestion(scope, meta);
		}

		let to: NodeId;
		if (reply !== undefined) {
			if (refs.target !== undefined && refs.target !== reply.authority) {
				throw new OnlyneError("invalid_request", text("error.reply_authority", {
					authority: reply.authority,
					target: refs.target,
				}));
			}
			to = reply.authority;
		} else {
			to = refs.target ?? await this.#targetQuestion(scope, meta);
			if (to.length === 0) throw new OnlyneError("ambiguous_target", text("error.delivery_target"));
		}

		const plan = await this.#deliveryPlan(scope, meta, lockedRelation, reply !== undefined);
		const context = plan.execution === "new" ? undefined : await this.#receiverContext(scope, to, meta);
		if (plan.execution === "existing" && context === undefined) {
			throw new OnlyneError("ambiguous_target", text("error.execution_context_missing", { to }));
		}

		let thread = continueThread(refs.thread, reply?.thread_ref);
		if (thread === undefined && reply === undefined && plan.execution === "existing") {
			const reused = await reuseThread(this.deps, to);
			if (reused !== undefined) thread = reused;
		}

		return sendAction({
			to,
			...(thread === undefined ? {} : { thread }),
			...(context === undefined ? {} : { context }),
			...(reply === undefined ? {} : { reply }),
			...(lockedRelation === undefined ? {} : { relation: lockedRelation }),
			...(refs.mentions === undefined ? {} : { mentions: refs.mentions }),
			response: plan.response,
			execution: plan.execution,
		});
	}

	async #originalQuestion(scope: DecisionScope, explicitTarget?: NodeId): Promise<ReplyReference | undefined> {
		const candidates = (await this.#replyCandidates(scope))
			.filter(entry => explicitTarget === undefined || entry.reply_authority === explicitTarget);
		if (candidates.length === 0) return undefined;
		const started = Date.now();
		const facts = candidates.map((entry, index) => ({
			key: `answer_${index + 1}`,
			from: entry.message.sender,
			kind: entry.message.delivery_kind,
			thread: threadKey(entry.thread_ref),
			seq: entry.seq,
			body: boundText(entry.message.msg),
		}));
		const answers = await this.#judge(
			scope.caller,
			{ focus: this.#focus(scope), trigger: this.#trigger(scope.caller), candidates: facts },
			originalQuestions(facts),
			scope.signal,
		);
		this.#record("outbound.o2a_original", scope.input, answers, Date.now() - started);
		const picked = choice(answers, "original");
		if (picked === undefined || picked === "new_message") return undefined;
		const entry = candidates[Number(picked.slice("answer_".length)) - 1];
		if (entry === undefined) throw new OnlyneError("decision_unavailable", text("error.reply_relation"));
		return {
			ref: { sender: entry.message.sender, id: entry.message.id },
			authority: entry.reply_authority,
			thread_ref: entry.thread_ref,
		};
	}

	async #replyCandidates(scope: DecisionScope): Promise<Entry[]> {
		if (scope.caller.read_only === true) return [];
		try {
			const here = await this.#decisionContext(scope.caller);
			if (here === undefined) return [];
			const rows = await this.deps.listInbox();
			const candidates: Entry[] = [];
			for (const row of rows) {
				const entry = row.attention.entry;
				if (entry === undefined || row.attention.disposition === "expired") continue;
				if (entry.message.sender === this.deps.config.node.id) continue;
				if (entry.thread_ref.owner === this.deps.config.node.id && entry.thread_ref.id === "trust") continue;
				const deliveredHere = here.main
					? row.context === undefined || row.context === "main"
					: row.context === here.alias;
				if (!deliveredHere) continue;
				if (await this.#answered(entry)) continue;
				candidates.push(entry);
				if (candidates.length >= REPLY_CANDIDATES) break;
			}
			return candidates;
		} catch {
			return [];
		}
	}

	async #decisionContext(caller: CallerContext): Promise<{ alias?: string; main: boolean } | undefined> {
		if (caller.main || caller.agent_id === this.deps.mainAgentId) return { main: true };
		const rec = (await this.deps.contexts.list()).find(row => row.agent_id === caller.agent_id);
		return rec === undefined ? undefined : { alias: rec.alias, main: false };
	}

	async #answered(entry: Entry): Promise<boolean> {
		let cursor: string | undefined;
		for (let i = 0; i < MAX_TAIL_PAGES; i++) {
			const page = await this.deps.listMessages(entry.thread_ref, cursor);
			for (const row of page.items) {
				const ref = row.message.meta?.in_reply_to;
				if (row.message.sender === this.deps.config.node.id && ref !== undefined
					&& ref.sender === entry.message.sender && ref.id === entry.message.id) {
					return true;
				}
			}
			cursor = page.next_cursor ?? undefined;
			if (cursor === undefined) return false;
		}
		return false;
	}

	async #relationQuestion(scope: DecisionScope, meta: Record<string, Json>): Promise<"reply" | "followup"> {
		const started = Date.now();
		const answers = await this.#judge(
			scope.caller,
			{ focus: this.#focus(scope), in_reply_to: meta.in_reply_to ?? null },
			relationQuestions(),
			scope.signal,
		);
		this.#record("outbound.o2a", scope.input, answers, Date.now() - started);
		const relation = choice(answers, "relation");
		if (relation !== "reply" && relation !== "followup") {
			throw new OnlyneError("decision_unavailable", text("error.reply_relation"));
		}
		return relation;
	}

	async #targetQuestion(scope: DecisionScope, meta: Record<string, Json>): Promise<NodeId> {
		const peers = await this.#knownPeers();
		if (peers.length === 0) throw new OnlyneError("not_found", text("error.no_receiver"));
		if (peers.length === 1) return peers[0]!.node_id;
		const started = Date.now();
		const answers = await this.#judge(
			scope.caller,
			{
				focus: { ...this.#focus(scope), meta_to: meta.to ?? null },
				candidate_peers: peers.map(peer => ({
					node_id: peer.node_id,
					alias: lastSegment(peer.node_id),
					last_seen: peer.last_seen,
				})),
			},
			targetQuestions(peers),
			scope.signal,
		);
		this.#record("outbound.o2a_target", scope.input, answers, Date.now() - started);
		const chosen = choice(answers, "target");
		if (chosen === undefined || chosen === "none") throw new OnlyneError("ambiguous_target", text("error.delivery_target"));
		if (!peers.some(peer => peer.node_id === chosen)) {
			throw new OnlyneError("decision_unavailable", text("error.target_not_published", { chosen }));
		}
		return chosen;
	}

	async #deliveryPlan(
		scope: DecisionScope,
		meta: Record<string, Json>,
		relation: "reply" | "followup" | undefined,
		isReply: boolean,
	): Promise<{ response: "await" | "async" | "none"; execution: "existing" | "new" }> {
		const inherited = inheritedModes(meta);
		if (inherited.response !== undefined && inherited.execution !== undefined) {
			return resolvedModes(inherited, {});
		}
		const started = Date.now();
		const answers = await this.#judge(
			scope.caller,
			{ focus: this.#focus(scope), relation: relation ?? "new" },
			planQuestions({
				response: inherited.response === undefined,
				execution: inherited.execution === undefined,
				reply: isReply,
			}),
			scope.signal,
		);
		this.#record("outbound.o3a", scope.input, answers, Date.now() - started);
		return resolvedModes(inherited, {
			response: choice(answers, "response"),
			execution: choice(answers, "execution"),
		});
	}

	async #receiverContext(scope: DecisionScope, to: NodeId, meta: Record<string, Json>): Promise<string | undefined> {
		const explicit = stringField(meta, "context");
		const receiver = await this.deps.targetContexts(to);
		if (!receiver.available) {
			if (explicit !== undefined) return explicit;
			throw new OnlyneError("unavailable", text("error.contexts_unreadable", { reason: receiver.reason ?? "unavailable" }));
		}
		const published = receiver.value.filter(ctx => ctx.state !== "lost" && ctx.state !== "unbound");
		if (explicit !== undefined) {
			if (!published.some(ctx => ctx.alias === explicit)) {
				throw new OnlyneError("invalid_request", text("error.context_not_published", { alias: explicit }));
			}
			return explicit;
		}
		if (published.length === 0) return undefined;
		if (published.length === 1) return published[0]!.alias;
		const started = Date.now();
		const answers = await this.#judge(
			scope.caller,
			{ focus: this.#focus(scope), target_published_contexts: published },
			contextQuestions(published),
			scope.signal,
		);
		this.#record("outbound.o3a_ctx", scope.input, answers, Date.now() - started);
		const picked = choice(answers, "context");
		if (picked !== undefined && published.some(ctx => ctx.alias === picked)) return picked;
		return published.find(ctx => ctx.alias === "main")?.alias ?? published[0]!.alias;
	}

	async #readAction(scope: DecisionScope, meta: Record<string, Json>): Promise<Action> {
		const explicitScope = stringField(meta, "scope");
		if (explicitScope !== undefined && isReadScope(explicitScope)) {
			return this.#readActionFromScope(scope, explicitScope, meta);
		}
		const started = Date.now();
		const answers = await this.#judge(scope.caller, { focus: this.#focus(scope) }, readScopeQuestions(), scope.signal);
		this.#record("outbound.o2b", scope.input, answers, Date.now() - started);
		const chosen = choice(answers, "scope");
		if (chosen === undefined || !isReadScope(chosen)) {
			throw new OnlyneError("decision_unavailable", text("error.read_scope"));
		}
		return this.#readActionFromScope(scope, chosen, meta);
	}

	async #readActionFromScope(
		scope: DecisionScope,
		readScope: "directory" | "neighbors" | "threads" | "inbox" | "messages" | "message" | "evidence" | "resources",
		meta: Record<string, Json>,
	): Promise<Action> {
		if (readScope === "resources") {
			const query: ResourceReadQuery = await resourceQuery(this.deps.node, meta);
			return { kind: "read", scope: "resources", query };
		}
		let thread = await this.refs.thread(meta);
		const messageRef = (await this.refs.message(meta, thread));
		if (messageRef?.thread !== undefined && thread === undefined) thread = messageRef.thread;
		if (readScope === "messages" && thread === undefined) {
			thread = await this.#threadQuestion(scope, "outbound.o2b-thread");
		}
		const cursor = stringField(meta, "cursor");
		const page_size = typeof meta.page_size === "number" ? meta.page_size : undefined;
		return {
			kind: "read",
			scope: readScope,
			...(thread === undefined ? {} : { thread }),
			...(messageRef === undefined ? {} : { message: messageRef.ref }),
			...(cursor === undefined ? {} : { cursor }),
			...(page_size === undefined ? {} : { page_size }),
		};
	}

	async #threadAction(scope: DecisionScope, meta: Record<string, Json>): Promise<Action> {
		const started = Date.now();
		const answers = await this.#judge(scope.caller, { focus: this.#focus(scope) }, threadOpQuestions(), scope.signal);
		this.#record("outbound.o2c", scope.input, answers, Date.now() - started);
		const op = choice(answers, "thread_op");
		if (op === undefined || !isThreadOp(op)) throw new OnlyneError("decision_unavailable", text("error.thread_op"));
		return this.#buildThreadAction(scope, op, meta);
	}

	async #buildThreadAction(
		scope: DecisionScope,
		op: "create" | "close" | "list" | "describe" | "member_add" | "member_remove",
		meta: Record<string, Json>,
	): Promise<Action> {
		const thread = op === "create" || op === "list"
			? await this.refs.thread(meta)
			: await this.refs.thread(meta) ?? await this.#threadQuestion(scope, "outbound.o2c-thread");
		const to = await this.refs.target(meta);
		const title = stringField(meta, "title") ?? (op === "create" && scope.input.msg.trim().length > 0 ? boundText(scope.input.msg, 80) : undefined);
		if ((op === "member_add" || op === "member_remove") && to === undefined) {
			throw new OnlyneError("decision_unavailable", text("error.thread_op_target", { op }));
		}
		if (op === "create" && title === undefined) {
			throw new OnlyneError("invalid_request", text("error.thread_title_required"));
		}
		return {
			kind: "thread",
			op,
			...(thread === undefined ? {} : { thread }),
			...(to === undefined ? {} : { to }),
			...(title === undefined ? {} : { title }),
		};
	}

	async #domainAction(scope: DecisionScope, meta: Record<string, Json>): Promise<Action> {
		const started = Date.now();
		const answers = await this.#judge(scope.caller, { focus: this.#focus(scope) }, domainOpQuestions(), scope.signal);
		this.#record("outbound.o2d", scope.input, answers, Date.now() - started);
		const op = choice(answers, "domain_op");
		if (op === undefined || !isDomainOp(op)) throw new OnlyneError("decision_unavailable", text("error.domain_op"));
		return this.#buildDomainAction(op, meta);
	}

	#buildDomainAction(
		op: "create" | "join" | "leave" | "list" | "describe" | "discover",
		meta: Record<string, Json>,
	): Action {
		const domain_id = stringField(meta, "domain_id");
		if (op !== "create" && op !== "list" && domain_id === undefined) {
			throw new OnlyneError("invalid_request", text("error.domain_id", { op }));
		}
		return {
			kind: "domain",
			op,
			...(domain_id === undefined ? {} : { domain_id }),
			...(typeof meta.expected_revision === "string" ? { expected_revision: meta.expected_revision } : {}),
		};
	}

	async #threadQuestion(scope: DecisionScope, nodeName: string): Promise<ThreadRef> {
		const threads = await this.refs.allThreads();
		if (threads.length === 0) throw new OnlyneError("decision_unavailable", text("error.local_thread_choice"));
		if (threads.length === 1) return threads[0]!.thread_ref;
		const started = Date.now();
		const answers = await this.#judge(
			scope.caller,
			{
				focus: this.#focus(scope),
				threads: threads.map(item => ({
					key: threadKey(item.thread_ref),
					title: item.title,
					owner: item.thread_ref.owner,
					state: item.state,
					next_seq: item.next_seq,
				})),
			},
			threadQuestions(threads.map(item => ({ ref: item.thread_ref, title: item.title, state: item.state }))),
			scope.signal,
		);
		this.#record(nodeName, scope.input, answers, Date.now() - started);
		const alias = choice(answers, "thread");
		if (alias === undefined || alias === "none" || alias === "new") {
			throw new OnlyneError("decision_unavailable", text("error.local_thread_choice"));
		}
		return this.refs.threadByAlias(alias);
	}

	async #knownPeers(): Promise<PeerRecord[]> {
		const byId = new Map<NodeId, PeerRecord>();
		const [discovery, trust] = await Promise.all([
			this.deps.listDiscovery().catch(() => [] as DiscoveryRecord[]),
			this.deps.listTrust().catch(() => [] as TrustEntry[]),
		]);
		for (const d of discovery) {
			if (d.node_id === this.deps.config.node.id || d.membership?.state === "left") continue;
			byId.set(d.node_id, { node_id: d.node_id, base_url: d.base_url, last_seen: d.last_seen });
		}
		for (const tr of trust) {
			if (tr.node_id === this.deps.config.node.id || trustPhase(tr) === "revoked") continue;
			const cur = byId.get(tr.node_id);
			byId.set(tr.node_id, {
				node_id: tr.node_id,
				base_url: cur?.base_url ?? tr.base_url ?? await this.deps.peerBase(tr.node_id),
				last_seen: cur?.last_seen ?? tr.last_checked ?? tr.first_seen,
			});
		}
		return [...byId.values()].sort((a, b) => a.node_id.localeCompare(b.node_id));
	}

	#focus(scope: DecisionScope): Record<string, Json> {
		return {
			delivery_kind: scope.input.delivery_kind,
			msg: scope.input.msg,
			meta_keys: scope.input.meta === undefined ? [] : Object.keys(scope.input.meta).sort(),
		};
	}

	#trigger(caller: CallerContext): Record<string, Json> {
		return {
			recent_user_prompt: caller.recent_user_prompt ?? null,
			last_thought: caller.last_thought ?? null,
		};
	}

	async #judge(
		caller: CallerContext,
		state: Record<string, unknown>,
		questions: Questions,
		signal?: AbortSignal,
	): Promise<Answers> {
		void caller;
		if (this.deps.judge === undefined) {
			throw new OnlyneError("decision_unavailable", text("judge.unavailable", {
				reason: "no judge backend is active",
			}));
		}
		return this.deps.judge(fitState(state), questions, signal);
	}

	#record(nodeName: string, input: unknown, answers: Answers, duration_ms: number): void {
		this.history.push({
			at: new Date().toISOString(),
			node: nodeName,
			input: input as Json,
			answers,
			duration_ms,
		});
		if (this.history.length > 100) this.history.shift();
	}
}
