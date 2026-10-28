import { Settings } from "@oh-my-pi/pi-coding-agent/config/settings";
import type { ExtensionContext } from "@oh-my-pi/pi-coding-agent";
import { journalJudgmentUsage, resolveJudge } from "@oh-my-pi/pi-coding-agent/judgment";
import { AgentRegistry } from "@oh-my-pi/pi-coding-agent/registry/agent-registry";
import type { SessionEntry } from "@oh-my-pi/pi-coding-agent/session/session-entries";
import { AsyncLocalStorage } from "node:async_hooks";
import {
	type Action,
	type DeliveryPlan,
	type Entry,
	type Json,
	type MessageRef,
	type MsgInput,
	type NodeId,
	type Page,
	type ResourceReadQuery,
	type Thread,
	type ThreadRef,
	OnlyneError,
} from "@onlyne/lib";
import { getLocale } from "@onlyne/node-host/i18n";
import type { ContextRecord, OnlyneConfig } from "./types.ts";
import { canAnswer, type ContextRegistry } from "./store.ts";
import { text } from "./decider-text.ts";

type Row = Record<string, unknown>;

interface ChoiceQuestion {
	type: "choice";
	instructions: string;
	criteria: Record<string, string>;
}

type Questions = Record<string, ChoiceQuestion>;

interface ChoiceAnswer {
	type?: string;
	choice?: string;
}

type Answers = Record<string, ChoiceAnswer | undefined>;

/** A decision input that either carries its records or states why it has none. */
type Source<T> = { available: true; value: T } | { available: false; reason: string };

function ok<T>(value: T): Source<T> {
	return { available: true, value };
}

function none<T>(reason: string): Source<T> {
	return { available: false, reason };
}

function valueOf<T>(source: unknown): T | undefined {
	const row = asRecord(source);
	if (row?.available !== true) return undefined;
	return row.value as T;
}

/** One discovered peer, as the library's discovery records it. */
export interface PeerRecord {
	node_id: NodeId;
	base_url: string;
	instance_id: string;
	last_seen: string;
}

/** One execution context a receiving node publishes for a target-qualified delivery. */
export interface ReceiverContext {
	alias: string;
	summary: string;
	state: string;
}

/** The OMP session a decision is made for. */
export interface DecisionScope {
	ctx: ExtensionContext;
	input: MsgInput;
	signal?: AbortSignal;
	/** Subagent sessions may inspect their own slice but must not write to the network. */
	readOnly?: boolean;
}

export interface InboundDecision {
	action: "fragment" | "wake" | "context" | "new_subagent";
	context?: string;
	/** Node I-2's insertion mode for a main-session delivery: steer interrupts the running turn, follow_up waits for it to end. */
	insertion?: "steer" | "follow_up";
}

export interface TodoDecision {
	action: "append" | "associate" | "none";
	title?: string;
	explanation?: string;
}

const decisionScope = new AsyncLocalStorage<DecisionScope>();

/** Bind one tool call to the session whose judge role answers its decision. */
export function runDecision<T>(scope: DecisionScope, work: () => Promise<T>): Promise<T> {
	return decisionScope.run(scope, work);
}

/** The scope of the `msg` call currently being decided, if any. */
export function currentDecision(): DecisionScope | undefined {
	return decisionScope.getStore();
}

/**
 * The thread a send continues: the most recently opened open thread this node owns whose members
 * already include the addressee. Derived from local records alone (RFC 0016 §5), so it costs no
 * judgement. A node may hold several open threads with one peer; the newest is the conversation
 * still in use, and a caller that wants a fresh topic names one or plans `execution: new`.
 *
 * Only a thread holding this node and the addressee continues: a thread with a third member is a
 * group conversation, and folding a private message into it would put a body in front of every
 * member. A group is addressed by naming it, which is why `meta.thread` wins over this lookup.
 */
export async function reuseThread(deps: DeciderDeps, to: NodeId): Promise<ThreadRef | undefined> {
	const self = deps.config.node.id;
	const threads = await deps.listThreads();
	const candidates = threads
		.filter(thread => thread.thread_ref.owner === self && thread.state === "open")
		.sort((left, right) => right.created_at.localeCompare(left.created_at));
	for (const thread of candidates) {
		let members: NodeId[];
		try {
			members = await deps.threadMembers(thread.thread_ref);
		} catch (error) {
			// An unreadable member list is a reason to try the next thread, not to open a new one
			// silently, so it is reported and skipped.
			deps.log?.("onlyne.thread.members_unreadable", { thread: threadKey(thread.thread_ref), error: message(error) });
			continue;
		}
		if (members.includes(to) && members.every(member => member === self || member === to)) return thread.thread_ref;
	}
	return undefined;
}

/**
 * Every peer this node can name, from both sources that know one.
 *
 * A trust row is identity: the peer exists whether or not anyone answered a probe recently. A
 * discovery record is an address, and a live one wins over the address a statement declared.
 * Callers that ask "who is this" and callers that ask "who may receive this" must not diverge,
 * or the same peer resolves for `meta.to` and vanishes from the target question — the shape of
 * the reported `No discovered node is named …` failure.
 */
export async function knownPeers(deps: DeciderDeps): Promise<PeerRecord[]> {
	const byId = new Map<NodeId, PeerRecord>();
	let readable = false;
	let failure = "";
	try {
		for (const row of await deps.listTrust()) {
			readable = true;
			if (row.phase === "revoked") continue;
			byId.set(row.node_id, { node_id: row.node_id, base_url: row.base_url ?? "", instance_id: "trust", last_seen: "" });
		}
	} catch (error) {
		failure = message(error);
		deps.log?.("onlyne.judge.trust_unreadable", { error: failure });
	}
	try {
		for (const peer of await deps.listPeers()) {
			readable = true;
			byId.set(peer.node_id, peer);
		}
	} catch (error) {
		failure = message(error);
		deps.log?.("onlyne.judge.peers_unreadable", { error: failure });
	}
	if (!readable) throw new OnlyneError("unavailable", text("reason.discovery_unreadable", { error: failure }));
	return [...byId.values()].filter(peer => peer.node_id !== deps.config.node.id);
}

export interface DeciderDeps {
	config: OnlyneConfig;
	/** Registry id of this node's main session; children are bound under it. */
	mainAgentId: string;
	contexts: ContextRegistry;
	log?(message: string, fields: Record<string, unknown>): void;
	listThreads(): Promise<Thread[]>;
	listMessages(thread: ThreadRef, cursor?: string): Promise<Page<Entry>>;
	/** The peers one thread already lists as members; a send reuses the thread a peer belongs to. */
	threadMembers(thread: ThreadRef): Promise<NodeId[]>;
	/** Peers from the library's live discovery records, never a static configuration. */
	listPeers(): Promise<PeerRecord[]>;
	/** Every trust row this node holds, in any phase. Identity outlives a discovery record. */
	listTrust(): Promise<Array<{ node_id: NodeId; phase: string; base_url?: string }>>;
	/** Contexts the target node publishes for `execution: existing`. */
	listReceiverContexts(nodeId: NodeId): Promise<ReceiverContext[]>;
	resourceQuery?(meta: Record<string, Json>): Promise<ResourceReadQuery>;
}

const TODO_ENTRY = "mouriya.omp.todo-more";
/** Upper bound on pages scanned for one thread tail; exceeding it is reported, never hidden. */
const MAX_TAIL_PAGES = 100;
/**
 * One decision prompt is capped at 16384 tokens by the Clef backend, which serializes the
 * state with sorted keys and keeps only the leading characters. A long `history` therefore
 * spends the budget first and the current `request` is dropped, so the answer describes an
 * input the model never saw. Keep the state under a conservative character budget by
 * discarding whole background sections, newest rows last; the request and the questions
 * are never cut, and a state that still does not fit fails instead of being truncated.
 */
const STATE_BUDGET_CHARS = 40_000;
const TRIM_ORDER = ["history", "task_dispatches", "context_note"] as const;
/** Registered contexts offered to I-1; the option count stays within the design's bound. */
const INBOUND_CONTEXT_CAP = 3;
/** Threads and entries a target question scans for recent unanswered inbound messages. */
const RECENT_INBOUND_THREADS = 3;
const RECENT_INBOUND_PER_THREAD = 3;
/** Newest branch rows that may reach the trigger derivation; older work is history, not decision material. */
const HISTORY_ROWS = 40;
/** One row's text cap: a pasted terminal dump must not crowd out the rest of the state. */
const TEXT_CHARS = 4_000;
/** How many characters of one branch message reach a decision prompt. */
const TRIGGER_CHARS = 800;
/** Sender wording that establishes a reply obligation (design §5.1 condition B). */
const REPLY_CUES = ["你好", "您好", "在吗", "请问", "能否", "期待回复", "期待你的回复"];
/** Sender wording that signals an urgent correction, for I-2's urgency signal. */
const URGENCY_CUES = [
	"停下", "停止", "中止", "回滚", "不要", "改用", "改成", "改方案", "紧急", "立刻", "立即",
	"stop", "abort", "revert", "instead", "interrupt", "urgent", "immediately", "do not", "don't",
];

function stateSize(state: Row): number {
	return JSON.stringify(state).length;
}

/** Applies the budget above; the caller fails the decision when `chars` still exceeds it. */
export function fitState(state: Row): { fitted: Row; dropped: string[]; chars: number } {
	let fitted = state;
	const dropped: string[] = [];
	for (const key of TRIM_ORDER) {
		if (stateSize(fitted) <= STATE_BUDGET_CHARS) break;
		const value = fitted[key];
		if (value === undefined) continue;
		if (Array.isArray(value)) {
			// Rows are oldest first, so dropping the front keeps the most recent evidence.
			let kept: unknown[] = value;
			while (kept.length > 0 && stateSize({ ...fitted, [key]: kept }) > STATE_BUDGET_CHARS) kept = kept.slice(Math.ceil(kept.length / 2));
			fitted = { ...fitted, [key]: kept };
		} else {
			fitted = { ...fitted, [key]: null };
		}
		dropped.push(key);
	}
	return { fitted, dropped, chars: stateSize(fitted) };
}

/** Decisions go through OMP's `judge` role; the plugin never picks a target or a plan itself. */
export class JudgeDecider {
	constructor(private readonly deps: DeciderDeps) {}

	async decide(scope: DecisionScope): Promise<Action> {
		const explicit = await this.#explicit(scope);
		if (explicit !== undefined) {
			const filled = await this.#threaded(explicit);
			await this.#guard(scope, filled);
			return filled;
		}

		// A subagent classifies exactly as the main session does and `#guard` decides what may run.
		// Refusing before classification closed the reads as well as the writes, so a subagent that
		// asked for its own slice was told it had no rights at all (caerulea report 4, problem 1).
		// Classification costs one local judge call; a write is refused either way.
		const meta = scope.input.meta ?? {};
		const intent = await this.#intent(scope);
		const action = intent === "communicate"
			? await this.#communicate(scope, meta)
			: intent === "inspect"
				? await this.#inspect(scope, meta)
				: intent === "manage_thread"
					? await this.#threadAction(scope, meta)
					: intent === "manage_domain"
						? await this.#domainAction(scope, meta)
						: await this.#resourceAction(meta);
		const filled = await this.#threaded(action);
		await this.#guard(scope, filled);
		this.deps.log?.("onlyne.judge.action", { direction: "outbound", ...actionFacts(filled) });
		return filled;
	}

	/**
	 * The thread a send belongs to, decided once for every path that produces one.
	 *
	 * A reply lands on the thread that holds its original (the client resolves it), and a plan
	 * that says `new` opens a topic. Otherwise the message continues the conversation this node
	 * already has with its addressee: leaving it unnamed makes the client open a thread per
	 * message, and asking the model for an alias it cannot know is the mistake the design already
	 * recorded. This is a lookup over local records, not a judgement (RFC 0016 §5), so it sits
	 * after both the explicit and the judged paths rather than inside either.
	 */
	async #threaded(action: Action): Promise<Action> {
		if (action.kind !== "send" || action.thread !== undefined) return action;
		if (action.in_reply_to !== undefined || action.execution === "new") return action;
		const thread = await reuseThread(this.deps, action.to);
		return thread === undefined ? action : { ...action, thread };
	}

	/** Node O-1: one question, five options, no lists of peers, threads, or history. */
	async #intent(scope: DecisionScope): Promise<"communicate" | "inspect" | "manage_thread" | "manage_domain" | "operate_resource"> {
		const started = Date.now();
		const answers = await this.#judge(
			scope.ctx,
			{
				focus: this.#focus(scope),
				trigger: this.#trigger(scope.ctx),
				host_env: { session_type: scope.ctx.agent.kind, read_only: scope.readOnly === true },
			},
			{
				intent: {
					type: "choice",
					instructions: text("judge.o1.instructions"),
					criteria: {
						communicate: text("judge.o1.communicate"),
						inspect: text("judge.o1.inspect"),
						manage_thread: text("judge.o1.manage_thread"),
						manage_domain: text("judge.o1.manage_domain"),
						operate_resource: text("judge.o1.operate_resource"),
					},
				},
			},
			scope.signal,
		);
		const intent = choice(answers, "intent");
		this.#record("outbound.o1", scope.input, answers, Date.now() - started);
		if (intent !== "communicate" && intent !== "inspect" && intent !== "manage_thread" && intent !== "manage_domain" && intent !== "operate_resource") {
			throw new OnlyneError("decision_unavailable", text("error.outbound_intent"));
		}
		return intent;
	}

	/** Nodes O-2A / O-3A / O-3A-Ctx for a send, reply or followup. */
	async #communicate(scope: DecisionScope, meta: Record<string, Json>): Promise<Action> {
		const reply = await this.#reply(meta);
		const explicitRelation = relation(meta);
		const lockedRelation = reply !== undefined
			? explicitRelation ?? await this.#relationQuestion(scope, meta)
			: undefined;
		if (reply === undefined && (explicitRelation !== undefined || lockedRelation !== undefined)) {
			throw new OnlyneError("invalid_request", text("error.relation_needs_reference"));
		}
		let to: NodeId;
		if (reply !== undefined) {
			// O-2A pre-pruning: the reply authority is the only target; meta.to and the judge's
			// answer must not move a reply to another node (caerulea report 3).
			const explicitTarget = await this.#target(meta);
			if (explicitTarget !== undefined && explicitTarget !== reply.authority) {
				throw new OnlyneError(
					"invalid_request",
					text("error.reply_authority", { authority: reply.authority, target: explicitTarget }),
				);
			}
			to = reply.authority;
		} else {
			to = await this.#target(meta) ?? await this.#targetQuestion(scope, meta);
			if (to.length === 0) {
				throw new OnlyneError("ambiguous_target", text("error.delivery_target"));
			}
		}

		const plan = await this.#deliveryPlan(scope, meta, lockedRelation, reply !== undefined);
		const thread = await this.#sendThread(meta, to);
		const context = plan.execution === "new" ? undefined : await this.#receiverContext(scope, to, meta);
		if (plan.execution === "existing" && context === undefined) {
			// RFC 0005 §1: `existing` disambiguates or refuses. Quietly opening a new context made
			// the sender's own plan a lie: the receipt said existing and the work started fresh.
			throw new OnlyneError(
				"ambiguous_target",
				text("error.execution_context_missing", { to }),
			);
		}
		const mentions = await this.#mentions(meta);
		return {
			kind: "send",
			to,
			...(thread === undefined ? {} : { thread }),
			...(context === undefined ? {} : { context }),
			...(reply === undefined ? {} : { in_reply_to: reply.ref }),
			...(lockedRelation === undefined ? {} : { relation: lockedRelation }),
			...(mentions === undefined ? {} : { mentions }),
			response: plan.response,
			execution: plan.execution,
		};
	}

	/** Node O-2A question 1: the reply/followup/new relation, asked only when meta left it open. */
	async #relationQuestion(scope: DecisionScope, meta: Record<string, Json>): Promise<"reply" | "followup"> {
		const started = Date.now();
		const answers = await this.#judge(
			scope.ctx,
			{ focus: this.#focus(scope), in_reply_to: meta.in_reply_to ?? null },
			{
				relation: {
					type: "choice",
					instructions: text("judge.relation.instructions"),
					criteria: {
						reply: text("judge.relation.reply"),
						followup: text("judge.relation.followup"),
					},
				},
			},
			scope.signal,
		);
		this.#record("outbound.o2a", scope.input, answers, Date.now() - started);
		const relation = choice(answers, "relation");
		if (relation !== "reply" && relation !== "followup") {
			throw new OnlyneError("decision_unavailable", text("error.reply_relation"));
		}
		return relation;
	}

	/** Node O-2A question 2: the delivery target among live peers, asked only when meta.to is absent. */
	async #targetQuestion(scope: DecisionScope, meta: Record<string, Json>): Promise<NodeId> {
		// The same set `#peerId` resolves against: a peer this node holds a trust row with is a
		// candidate even when discovery has nothing fresh to say about its address.
		const peers = await knownPeers(this.deps);
		// One known peer is the answer; none means the caller must name a target.
		if (peers.length === 0) throw new OnlyneError("not_found", text("error.no_receiver"));
		if (peers.length === 1) return peers[0]!.node_id;
		const started = Date.now();
		const answers = await this.#judge(
			scope.ctx,
			{
				focus: { ...this.#focus(scope), meta_to: meta.to ?? null },
				candidate_peers: peers.map(peer => ({
					node_id: peer.node_id,
					alias: lastSegment(peer.node_id),
					last_seen: peer.last_seen,
				})),
				recent_inbound_unanswered: await this.#unanswered(scope.ctx),
			},
			{
				target: {
					type: "choice",
					instructions: text("judge.target.instructions"),
					criteria: Object.fromEntries(peers.map(peer => [
						peer.node_id,
						text("judge.target.peer", {
							node: peer.node_id,
							base_url: peer.base_url,
							instance: peer.instance_id,
							last_seen: peer.last_seen,
						}),
					])),
				},
			},
			scope.signal,
		);
		this.#record("outbound.o2a", scope.input, answers, Date.now() - started);
		const chosen = choice(answers, "target");
		if (chosen === undefined || chosen === "none") {
			throw new OnlyneError("ambiguous_target", text("error.delivery_target"));
		}
		if (!peers.some(peer => peer.node_id === chosen)) {
			throw new OnlyneError("decision_unavailable", text("error.target_not_published", { chosen }));
		}
		return chosen;
	}

	/** Node O-3A: response and execution, inheriting any meta value the caller already fixed. */
	async #deliveryPlan(
		scope: DecisionScope,
		meta: Record<string, Json>,
		relation: "reply" | "followup" | undefined,
		isReply: boolean,
	): Promise<{ response: "await" | "async" | "none"; execution: "existing" | "new" }> {
		const response = this.#inherited(meta, "response");
		const execution = this.#inherited(meta, "execution");
		if (response !== undefined && execution !== undefined) {
			return { response: response as "await" | "async" | "none", execution: execution as "existing" | "new" };
		}
		const started = Date.now();
		const answers = await this.#judge(
			scope.ctx,
			{ focus: this.#focus(scope), relation: relation ?? "new" },
			{
				...(response === undefined
					? {
							response: {
								type: "choice",
								instructions: text("judge.response.instructions"),
								criteria: {
									none: text("judge.response.none"),
									async: text("judge.response.async"),
									await: text("judge.response.await"),
								},
							},
						}
					: {}),
				...(execution === undefined
					? {
							execution: {
								type: "choice",
								instructions: text("judge.execution.instructions"),
								criteria: {
									existing: isReply
										? text("judge.execution.existing.reply")
										: text("judge.execution.existing.new"),
									new: text("judge.execution.new"),
								},
							},
						}
					: {}),
			},
			scope.signal,
		);
		this.#record("outbound.o3a", scope.input, answers, Date.now() - started);
		const chosenResponse = response ?? choice(answers, "response");
		const chosenExecution = execution ?? choice(answers, "execution");
		if (chosenResponse !== "none" && chosenResponse !== "async" && chosenResponse !== "await") {
			throw new OnlyneError("decision_unavailable", text("error.response_mode"));
		}
		if (chosenExecution !== "existing" && chosenExecution !== "new") {
			throw new OnlyneError("decision_unavailable", text("error.execution_mode"));
		}
		return { response: chosenResponse, execution: chosenExecution };
	}

	#inherited(meta: Record<string, Json>, key: string): "await" | "async" | "none" | "existing" | "new" | undefined {
		const value = meta[key];
		if (value === "await" || value === "async" || value === "none" || value === "existing" || value === "new") return value;
		if (value !== undefined) throw new OnlyneError("invalid_request", text("error.meta_mode", { key }));
		return undefined;
	}

	/**
	 * Node O-3A-Ctx: pick among the target's published contexts. A lost or never-bound record is
	 * not a candidate, the target's own main session is the fallback every host publishes, and
	 * an unreadable list is reported rather than read as "no context" (RFC 0005 §1).
	 */
	async #receiverContext(scope: DecisionScope, to: NodeId, meta: Record<string, Json>): Promise<string | undefined> {
		const receiver = await this.#receiverContexts(to);
		const explicit = stringField(meta, "context");
		if (!receiver.available) {
			if (explicit !== undefined) return explicit;
			throw new OnlyneError("unavailable", text("error.contexts_unreadable", { reason: receiver.reason }));
		}
		const published = receiver.value.filter(context => context.state !== "lost" && context.state !== "unbound");
		if (explicit !== undefined) {
			if (!published.some(context => context.alias === explicit)) {
				throw new OnlyneError("invalid_request", text("error.context_not_published", { alias: explicit }));
			}
			return explicit;
		}
		if (published.length === 0) return undefined;
		// The receiver's main session is the one context every host publishes: continuing work
		// there needs no disambiguation.
		const main = published.find(context => context.alias === "main")?.alias;
		if (published.length === 1) return published[0]!.alias;
		const started = Date.now();
		const answers = await this.#judge(
			scope.ctx,
			{ focus: this.#focus(scope), target_published_contexts: published },
			{
				context: {
					type: "choice",
					instructions: text("judge.context.instructions"),
					criteria: {
						...Object.fromEntries(published.map(context => [
							context.alias,
							text("judge.context.published", { summary: context.summary, state: context.state }),
						])),
						none: text("judge.context.none"),
					},
				},
			},
			scope.signal,
		);
		this.#record("outbound.o3a-ctx", scope.input, answers, Date.now() - started);
		const alias = choice(answers, "context");
		// The plan already says `existing`: with no context named, the target's own main session
		// is where work continues, and the caller refuses only when there is no such context.
		if (alias === undefined || alias === "none" || !published.some(context => context.alias === alias)) return main;
		return alias;
	}

	/**
	 * The thread a send names. The thread decides where the message is stored; `to` decides who
	 * is addressed. An initiator owns its thread for the whole conversation, so every member
	 * writes into it and the client routes each message to that owner. A bare id that names no
	 * local thread is the target's thread id, as a receipt reports it.
	 */
	async #sendThread(meta: Record<string, Json> | undefined, to: NodeId): Promise<ThreadRef | undefined> {
		if (meta?.thread === undefined) return undefined;
		const value = meta.thread;
		if (value !== null && typeof value === "object" && !Array.isArray(value)) {
			return this.#thread(meta);
		}
		if (typeof value !== "string" || value.length === 0) {
			throw new OnlyneError("invalid_request", text("error.thread_reference_shape"));
		}
		const matches = (await this.deps.listThreads()).filter(thread => thread.thread_ref.id === value || thread.title === value);
		if (matches.length > 1) throw new OnlyneError("ambiguous_target", text("error.ambiguous_local_thread", { value }));
		if (matches.length === 1) return matches[0]!.thread_ref;
		const slash = value.indexOf("/");
		if (slash > 0 && slash < value.length - 1) {
			return { owner: value.slice(0, slash), id: value.slice(slash + 1) };
		}
		return { owner: to, id: value };
	}

	/**
	 * One inbound entry's route. `plan` is the sender's delivery plan when the delivery named
	 * one: an explicit `plan.context` is binding, `execution: new` is only a preference.
	 */
	async decideInbound(scope: ExtensionContext, entry: Entry, plan?: DeliveryPlan): Promise<InboundDecision> {
		// The binding is checked first: a plan that names a context needs no obligation question,
		// and a lost one must fail without spending a judge call on every maintenance pass.
		const bound = await this.#boundContext(plan);
		if (bound !== undefined) return bound;
		const needsAnswer = await this.#needsAnswer(scope, entry, plan);
		// The sender asked for a fresh context and expects nothing back: that is exactly what a
		// subagent is, so no routing question can improve on it.
		if (plan?.execution === "new" && !needsAnswer) return { action: "new_subagent" };
		if (plan?.execution === "new") {
			// A new-context preference cannot hold an obligation: a subagent has no send right, so
			// an answer owed here would never leave this node (dpdp report 4, host shape).
			this.deps.log?.("onlyne.judge.overridden", {
				direction: "inbound",
				chosen: "new_subagent",
				fallback: "main_agent",
				reason: "a new-context request cannot hold a message that needs an answer",
			});
			const insertion = await this.#insertionMode(scope, entry, needsAnswer);
			// A message that needs an answer never gets the silent mode; the option is withheld
			// above, and this keeps the decision type honest if that ever changes.
			return { action: "wake", insertion: insertion === "fragment" ? "follow_up" : insertion };
		}
		// Deterministic prefilter (design §5.1): a disposed or never-bound session is a delivery
		// black hole, and a message that needs an answer can only land where a reply can be sent.
		// Pruned candidates never reach the prompt, so the question only offers positive options.
		const registry = valueOf<Row[]>(await this.#contextRows()) ?? [];
		const valid = registry
			.filter(row =>
				row.agent_id !== null && row.agent_id !== undefined && row.live === true
				&& (!needsAnswer || row.can_answer === true)
				// The main session is its own I-1 option; its registry row is not a context candidate.
				&& row.alias !== "main")
			.slice(0, INBOUND_CONTEXT_CAP);
		const started = Date.now();
		const sameThread = (ref: unknown): boolean => {
			const thread = ref as ThreadRef | null | undefined;
			return thread !== null && thread !== undefined
				&& thread.owner === entry.thread_ref.owner && thread.id === entry.thread_ref.id;
		};
		const criteria: Record<string, string> = {
			main_agent: text("judge.inbound.main_agent"),
			...Object.fromEntries(valid.map(row => [
				String(row.alias),
				text(sameThread(row.thread_ref) ? "judge.inbound.context_same_thread" : "judge.inbound.context", {
					summary: String(row.summary),
					state: String(row.state),
					agent: String(row.agent_id),
				}),
			])),
			...(needsAnswer ? {} : { new_subagent: text("judge.inbound.new_subagent") }),
		};
		// A pruned candidate set can leave one route; asking would be a question with one answer.
		const forced = Object.keys(criteria);
		const answers: Answers = forced.length === 1
			? { routing_target: { choice: forced[0] } }
			: await this.#judge(
					scope,
					{
						focus: { node: this.deps.config.node.id, incoming: entryFacts(entry) },
						needs_answer: needsAnswer,
						plan: plan ?? null,
						valid_contexts: valid.map(row => ({
							alias: row.alias,
							summary: row.summary,
							state: row.state,
							agent_id: row.agent_id,
							can_answer: row.can_answer,
							thread_ref: row.thread_ref ?? null,
						})),
						thread_dialogue: await this.#threadEvidenceFor([entry.thread_ref]),
					},
					{
						routing_target: {
							type: "choice",
							instructions: text("judge.inbound.instructions"),
							criteria,
						},
					},
				);
		this.#record("inbound.i1", { delivery_kind: entry.message.delivery_kind, msg: entry.message.msg, meta: entry.message.meta }, answers, Date.now() - started);
		const override = (chosen: string, reason: string): InboundDecision => {
			// The judge is offered only viable targets; when it names another one anyway, the
			// delivery goes to the main session and the override is logged, so judge tuning has
			// evidence instead of a silent rewrite (caerulea report 3, problem 1).
			this.deps.log?.("onlyne.judge.overridden", { direction: "inbound", chosen, fallback: "main_agent", reason });
			return { action: "wake", insertion: "follow_up" };
		};
		const chosen = choice(answers, "routing_target");
		if (chosen === undefined) throw new OnlyneError("decision_unavailable", text("error.routing_target"));
		if (chosen === "new_subagent") {
			if (needsAnswer) return override(chosen, "a subagent cannot answer a message that needs a reply");
			return { action: "new_subagent" };
		}
		if (chosen !== "main_agent") {
			const row = valid.find(candidate => candidate.alias === chosen);
			if (row === undefined) return override(chosen, "the decision named a context the prefilter did not offer");
			return { action: "context", context: chosen };
		}
		const insertion = await this.#insertionMode(scope, entry, needsAnswer);
		if (insertion === "fragment") return { action: "fragment" };
		return { action: "wake", insertion };
	}

	/**
	 * Node I-0: whether this node owes the sender an answer. Routing used to infer it from
	 * wording, so a completed reply read as a question and a greeting read as work; the
	 * obligation is a semantic question now, and a failed judge is a failure, never a guess.
	 */
	async #needsAnswer(scope: ExtensionContext, entry: Entry, plan?: DeliveryPlan): Promise<boolean> {
		const started = Date.now();
		const answers = await this.#judge(
			scope,
			{
				focus: { node: this.deps.config.node.id, incoming: entryFacts(entry) },
				plan: plan ?? null,
				relation: entry.message.meta?.relation ?? null,
				in_reply_to: entry.message.meta?.in_reply_to ?? null,
				// Wording is evidence, not the decision: it names the cases a judge may weigh.
				wording_hint: needsReplyOf(entry),
				current_work: this.#trigger(scope),
				thread_dialogue: await this.#threadEvidenceFor([entry.thread_ref]),
			},
			{
				needs_answer: {
					type: "choice",
					instructions: text("judge.needs_answer.instructions"),
					criteria: {
						yes: text("judge.needs_answer.yes"),
						no: text("judge.needs_answer.no"),
					},
				},
			},
			undefined,
		);
		this.#record("inbound.i0", { delivery_kind: entry.message.delivery_kind, msg: entry.message.msg, meta: entry.message.meta }, answers, Date.now() - started);
		const answer = choice(answers, "needs_answer");
		if (answer !== "yes" && answer !== "no") {
			throw new OnlyneError("decision_unavailable", text("error.needs_answer"));
		}
		return answer === "yes";
	}

	/**
	 * RFC 0005 §1: a plan that names a context is binding. A record that is missing, never
	 * bound, or no longer bound is `context_lost`: the delivery neither opens a new context
	 * nor reroutes, and the entry stays owed for an explicit new decision.
	 */
	async #boundContext(plan: DeliveryPlan | undefined): Promise<InboundDecision | undefined> {
		const alias = plan?.context;
		if (alias === undefined) return undefined;
		let record: ContextRecord | undefined;
		try {
			record = await this.deps.contexts.find(alias);
		} catch (error) {
			throw new OnlyneError("unavailable", text("error.registry_unreadable", { error: message(error) }));
		}
		const dead = record === undefined || record.agent_id === undefined
			|| record.state === "lost" || record.state === "unbound";
		if (!dead) return { action: "context", context: alias };
		// A record that once held a binding has lost it; one that never bound stays unbound.
		if (record !== undefined && record.agent_id !== undefined && record.bound_at !== undefined && record.state !== "lost") {
			await this.deps.contexts.updateState(alias, "lost").catch(() => undefined);
		}
		throw new OnlyneError("context_lost", text("error.context_lost", { alias }), { context: alias });
	}

	/** Node I-2: when a main-session delivery enters the agent's work. */
	async #insertionMode(scope: ExtensionContext, entry: Entry, needsAnswer: boolean): Promise<"steer" | "follow_up" | "fragment"> {
		const body = entry.message.msg.toLowerCase();
		const started = Date.now();
		// `fragment` never starts a turn, so a message that needs an answer can never be one;
		// offering it would be an option whose choice guarantees the obligation is missed.
		const criteria: Record<string, string> = {
			steer: text("judge.insertion.steer"),
			follow_up: text("judge.insertion.follow_up"),
			...(needsAnswer ? {} : { fragment: text("judge.insertion.fragment") }),
		};
		const answers = await this.#judge(
			scope,
			{
				focus: { incoming: entryFacts(entry) },
				host_running_state: { is_turn_active: !scope.isIdle(), plan_mode: this.#planActive(scope) },
				urgency_signals: { has_stop_or_change: URGENCY_CUES.some(cue => body.includes(cue)) },
			},
			{
				insertion_mode: {
					type: "choice",
					instructions: text("judge.insertion.instructions"),
					criteria,
				},
			},
		);
		this.#record("inbound.i2", { delivery_kind: entry.message.delivery_kind, msg: entry.message.msg, meta: entry.message.meta }, answers, Date.now() - started);
		const mode = choice(answers, "insertion_mode");
		if (mode === "fragment" && needsAnswer) {
			this.deps.log?.("onlyne.judge.overridden", { direction: "inbound", chosen: "fragment", fallback: "follow_up", reason: "a silent fragment cannot answer a message that needs a reply" });
			return "follow_up";
		}
		if (mode !== "steer" && mode !== "follow_up" && mode !== "fragment") {
			throw new OnlyneError("decision_unavailable", text("error.insertion_mode"));
		}
		return mode;
	}

	#planActive(ctx: ExtensionContext): boolean {
		const ref = AgentRegistry.global().get(ctx.agent?.id ?? "");
		return ref?.session?.getPlanModeState?.()?.enabled === true;
	}

	async decideTodo(ctx: ExtensionContext, entry: Entry, current: unknown): Promise<TodoDecision> {
		const snapshot = current === undefined || current === null ? this.#todo(ctx) : normalizeTodo(current);
		const candidates = todoCandidates(entry);
		const existing = todoTitles(snapshot);
		const focus = { node: this.deps.config.node.id, incoming: entryFacts(entry) };
		const input = { delivery_kind: entry.message.delivery_kind, msg: entry.message.msg, meta: entry.message.meta };
		// Node T-1: what this message does to the list. Only viable actions are offered: an
		// empty list has nothing to associate with, and an empty body has nothing to append.
		const actionCriteria: Record<string, string> = {
			none: text("judge.todo.action.none"),
		};
		if (existing.length > 0) actionCriteria.associate = text("judge.todo.action.associate");
		if (candidates.titles.length > 0) actionCriteria.append = text("judge.todo.action.append");
		const t1 = Date.now();
		// With nothing to associate and nothing to append, `none` is the only answer.
		const forcedActions = Object.keys(actionCriteria);
		const answers: Answers = forcedActions.length === 1
			? { action: { choice: forcedActions[0] } }
			: await this.#judge(
					ctx,
					{ focus, current_todo_summary: todoSummary(snapshot) },
					{
						action: {
							type: "choice",
							instructions: text("judge.todo.action.instructions"),
							criteria: actionCriteria,
						},
					},
				);
		this.#record("todo.t1", input, answers, Date.now() - t1);
		const action = choice(answers, "action");
		if (action === "none") return { action: "none" };
		if (action === "associate") {
			// Node T-2 (associate): which existing item this message continues.
			const t2 = Date.now();
			const follow: Answers = existing.length === 1
				? { task: { choice: existing[0] } }
				: await this.#judge(
						ctx,
						{ focus, current_todo_items: existing },
						{
							task: {
								type: "choice",
								instructions: text("judge.todo.associate.instructions"),
								criteria: Object.fromEntries(existing.map(title => [title, text("judge.todo.associate.item")])),
							},
						},
					);
			this.#record("todo.t2", input, follow, Date.now() - t2);
			const title = choice(follow, "task");
			if (title === undefined || title === "none" || !existing.includes(title)) {
				throw new OnlyneError("decision_unavailable", text("error.todo_associate"));
			}
			return { action: "associate", title };
		}
		if (action === "append") {
			// Node T-2 (append): the title and explanation, both taken from the message itself.
			const t2 = Date.now();
			// A one-candidate title is the answer; only a real choice is asked.
			const questions: Questions = {
				...(candidates.titles.length === 1
					? {}
					: {
							title: {
								type: "choice" as const,
								instructions: text("judge.todo.title.instructions"),
								criteria: Object.fromEntries(candidates.titles.map(title => [title, text("judge.todo.title.item")])),
							},
						}),
				...(candidates.explanations.length === 0
					? {}
					: {
							explanation: {
								type: "choice" as const,
								instructions: text("judge.todo.explanation.instructions"),
								criteria: {
									...Object.fromEntries(candidates.explanations.map(explanation => [explanation, text("judge.todo.explanation.item")])),
									none: text("judge.todo.explanation.none"),
								},
							},
						}),
			};
			const follow: Answers = Object.keys(questions).length === 0
				? {}
				: await this.#judge(ctx, { focus, title_candidates: candidates.titles, explanation_candidates: candidates.explanations }, questions);
			this.#record("todo.t2", input, follow, Date.now() - t2);
			const title = candidates.titles.length === 1 ? candidates.titles[0] : choice(follow, "title");
			if (title === undefined || title === "none" || !candidates.titles.includes(title)) {
				throw new OnlyneError("decision_unavailable", text("error.todo_append"));
			}
			const explanation = choice(follow, "explanation");
			const chosen = explanation !== undefined && explanation !== "none" && candidates.explanations.includes(explanation)
				? explanation
				: undefined;
			return { action: "append", title, ...(chosen === undefined ? {} : { explanation: chosen }) };
		}
		throw new OnlyneError("decision_unavailable", text("error.todo_action"));
	}

	async #judge(scope: ExtensionContext, state: unknown, questions: Questions, signal?: AbortSignal): Promise<Answers> {
		try {
			const { fitted, dropped, chars } = fitState(state as Row);
			if (chars > STATE_BUDGET_CHARS) {
				throw new OnlyneError("decision_unavailable", text("error.state_budget", { chars, cap: STATE_BUDGET_CHARS }));
			}
			if (dropped.length > 0) this.deps.log?.("onlyne.judge.trimmed", { dropped, chars });
			const settings = (await Settings.current) ?? (await Settings.loadReadOnly({ cwd: scope.cwd }));
			const judge = resolveJudge({
				settings,
				registry: scope.modelRegistry,
				sessionModel: scope.model,
				sessionId: scope.sessionManager.getSessionId(),
				purpose: "onlyne",
				onUsage: journalJudgmentUsage(scope.sessionManager),
			});
			const result = await judge.judge({ state: jsonState(fitted), questions }, signal === undefined ? {} : { signal });
			return result.answers as Answers;
		} catch (error) {
			throw new OnlyneError(
				"decision_unavailable",
				text("error.judge_failed", { error: error instanceof Error ? error.message : String(error) }),
			);
		}
	}

	/**
	 * One line per judged message, at info: the intent, every answer the judge gave, and the
	 * time it took. Tuning the judge needs this record; without it a misrouted send can only be
	 * reconstructed from its receipt (issue report, problem 5).
	 */
	#record(direction: string, input: { delivery_kind: string; msg: string; meta?: unknown }, answers: Answers, ms: number): void {
		this.deps.log?.("onlyne.judge.decided", judgeRecord(direction, input, answers, ms));
	}

	/**
	 * Only stable, explicit fields route without a decision. Everything semantic,
	 * including a bare thread reference, goes to the judge.
	 */
	async #explicit(scope: DecisionScope): Promise<Action | undefined> {
		const meta = scope.input.meta;
		if (meta === undefined) return undefined;
		if (meta.scope === "resources") return this.#read(scope, "resources");
		const op = stringField(meta, "op");
		if (op !== undefined && isThreadOp(op)) {
			const thread = await this.#thread(meta);
			const to = await this.#target(meta);
			const title = stringField(meta, "title");
			if (op === "create" && title === undefined) throw new OnlyneError("invalid_request", text("error.thread_title_required"));
			if (op !== "create" && op !== "list" && thread === undefined) {
				throw new OnlyneError("invalid_request", text("error.thread_reference_required", { op }));
			}
			if ((op === "member_add" || op === "member_remove") && to === undefined) {
				throw new OnlyneError("invalid_request", text("error.thread_target_required", { op }));
			}
			return {
				kind: "thread",
				op,
				...(thread === undefined ? {} : { thread }),
				...(to === undefined ? {} : { to }),
				...(title === undefined ? {} : { title }),
			};
		}

		const resource = recordField(meta, "resource");
		if (resource !== undefined) {
			const owner = stringField(resource, "owner");
			const id = stringField(resource, "id");
			const operation = stringField(meta, "operation");
			if (owner === undefined || id === undefined || operation === undefined) {
				throw new OnlyneError("invalid_request", text("error.resource_fields"));
			}
			return {
				kind: "resource",
				resource: { owner, id },
				operation,
				...(meta.data === undefined ? {} : { data: meta.data }),
				...(typeof meta.expected_revision === "string" ? { expected_revision: meta.expected_revision } : {}),
			};
		}

		const scopeName = stringField(meta, "scope");
		if (scopeName !== undefined) {
			// `peers` is the name a model reaches for; the library's scope is `neighbors`, and
			// the alias keeps one word from meaning two channels (dpdp report 4, navigation).
			const name = scopeName === "peers" ? "neighbors" : scopeName;
			if (isReadScope(name)) return this.#read(scope, name);
		}

		return undefined;
	}

	async #read(scope: DecisionScope, name: ActionReadScope): Promise<Action> {
		const meta = scope.input.meta ?? {};
		if (name === "resources") {
			if (scope.readOnly === true) throw new OnlyneError("forbidden", text("error.resources_main_only"));
			if (this.deps.resourceQuery === undefined) throw new OnlyneError("unavailable", text("error.resource_host_unavailable"));
			return { kind: "read", scope: name, query: await this.deps.resourceQuery(meta) };
		}
		const thread = await this.#thread(meta);
		const page_size = pageSize(meta);
		const cursor = stringField(meta, "cursor");
		const seq = stringField(meta, "seq");
		if (name === "directory") return { kind: "read", scope: name };
		if (name === "neighbors") return { kind: "read", scope: name };
		if (name === "threads") return { kind: "read", scope: name };
		if (name === "inbox") {
			return {
				kind: "read",
				scope: name,
				...(thread === undefined ? {} : { thread }),
				...(cursor === undefined ? {} : { cursor }),
				...(page_size === undefined ? {} : { page_size }),
			};
		}
		if (thread === undefined && name !== "message" && name !== "evidence") {
			throw new OnlyneError("invalid_request", text("error.read_thread_required", { name }));
		}
		if (name === "messages") {
			return {
				kind: "read",
				scope: name,
				thread,
				...(seq === undefined ? {} : { seq }),
				...(cursor === undefined ? {} : { cursor }),
				...(page_size === undefined ? {} : { page_size }),
				attention: attention(meta),
			};
		}
		const resolved = await this.#messageRef(meta, thread);
		if (resolved === undefined) {
			throw new OnlyneError("invalid_request", text("error.read_message_required", { name }));
		}
		// The thread the message was read in travels with the action: a subagent's guard
		// authorizes a thread slice, and `{thread,seq}` already names that slice.
		const owner = resolved.thread ?? thread;
		return {
			kind: "read",
			scope: name,
			...(owner === undefined ? {} : { thread: owner }),
			message: resolved.ref,
		};
	}

	/** Node O-2B: which readable channel the message asks about; O-3B-Th joins when messages has no thread. */
	async #inspect(scope: DecisionScope, meta: Record<string, Json>): Promise<Action> {
		const started = Date.now();
		const answers = await this.#judge(
			scope.ctx,
			{ focus: { delivery_kind: scope.input.delivery_kind, msg: scope.input.msg, meta: meta ?? null } },
			{
				read_scope: {
					type: "choice",
					instructions: text("judge.read_scope.instructions"),
					criteria: {
						peers: text("judge.read_scope.peers"),
						inbox: text("judge.read_scope.inbox"),
						messages: text("judge.read_scope.messages"),
						evidence: text("judge.read_scope.evidence"),
						resources: text("judge.read_scope.resources"),
						none: text("judge.read_scope.none"),
					},
				},
			},
			scope.signal,
		);
		this.#record("outbound.o2b", scope.input, answers, Date.now() - started);
		const scopeName = choice(answers, "read_scope");
		if (scopeName === undefined || scopeName === "none") {
			throw new OnlyneError("decision_unavailable", text("error.read_scope_missing"));
		}
		// The design's `peers` is the library's `neighbors`: the nodes this one can address, the
		// same rows onlyne://neighbors renders. A caller that names a library channel explicitly
		// in meta.scope still gets it verbatim.
		if (scopeName === "resources") return this.#read(scope, "resources");
		if (scopeName !== "peers" && scopeName !== "inbox" && scopeName !== "messages" && scopeName !== "evidence") {
			throw new OnlyneError("decision_unavailable", text("error.read_scope_unknown", { scope: scopeName }));
		}
		const name: ActionReadScope = scopeName === "peers" ? "neighbors" : scopeName;
		const thread = await this.#thread(meta);
		if (name === "messages") {
			const chosen = thread ?? await this.#threadQuestion(scope, "outbound.o3b-th");
			return {
				kind: "read",
				scope: name,
				thread: chosen,
				...(seqField(meta, "seq") === undefined ? {} : { seq: seqField(meta, "seq") }),
				...(stringField(meta, "cursor") === undefined ? {} : { cursor: stringField(meta, "cursor") }),
				...(pageSize(meta) === undefined ? {} : { page_size: pageSize(meta) }),
				attention: attention(meta),
			};
		}
		if (name === "evidence") {
			const resolved = await this.#messageRef(meta, thread);
			if (resolved === undefined) {
				throw new OnlyneError("invalid_request", text("error.read_message_required", { name }));
			}
			const owner = resolved.thread ?? thread;
			return { kind: "read", scope: name, ...(owner === undefined ? {} : { thread: owner }), message: resolved.ref };
		}
		return {
			kind: "read",
			scope: name,
			...(name === "inbox" && thread !== undefined ? { thread } : {}),
			...(stringField(meta, "cursor") === undefined ? {} : { cursor: stringField(meta, "cursor") }),
			...(name === "inbox" && pageSize(meta) !== undefined ? { page_size: pageSize(meta) } : {}),
		};
	}

	/** Node O-3B-Th: pick the local thread a read or thread operation names. */
	async #threadQuestion(scope: DecisionScope, node: string): Promise<ThreadRef> {
		const threads = await this.deps.listThreads();
		// A question needs a real choice: one local thread is the answer, none is an error.
		if (threads.length === 0) throw new OnlyneError("not_found", text("error.no_local_thread"));
		if (threads.length === 1) return threads[0]!.thread_ref;
		const started = Date.now();
		const answers = await this.#judge(
			scope.ctx,
			{
				focus: this.#focus(scope),
				local_threads: threads.map(thread => ({
					id: thread.thread_ref.id,
					title: thread.title,
					owner: thread.thread_ref.owner,
					state: thread.state,
					next_seq: thread.next_seq,
				})),
			},
			{
				thread: {
					type: "choice",
					instructions: text("judge.thread.instructions"),
					criteria: Object.fromEntries(threads.map(thread => [
						thread.thread_ref.id,
						text("judge.thread.item", {
							title: thread.title,
							owner: thread.thread_ref.owner,
							id: thread.thread_ref.id,
							state: thread.state,
						}),
					])),
				},
			},
			scope.signal,
		);
		this.#record(node, scope.input, answers, Date.now() - started);
		const alias = choice(answers, "thread");
		if (alias === undefined || alias === "none") {
			throw new OnlyneError("decision_unavailable", text("error.local_thread_choice"));
		}
		return this.#threadByAlias(alias);
	}

	/** Node O-2C: the local thread operation. */
	async #threadAction(scope: DecisionScope, meta: Record<string, Json>): Promise<Action> {
		const started = Date.now();
		const answers = await this.#judge(
			scope.ctx,
			{ focus: this.#focus(scope) },
			{
				thread_op: {
					type: "choice",
					instructions: text("judge.thread_op.instructions"),
					criteria: {
						create: text("judge.thread_op.create"),
						close: text("judge.thread_op.close"),
						list: text("judge.thread_op.list"),
						describe: text("judge.thread_op.describe"),
						member_add: text("judge.thread_op.member_add"),
						member_remove: text("judge.thread_op.member_remove"),
					},
				},
			},
			scope.signal,
		);
		this.#record("outbound.o2c", scope.input, answers, Date.now() - started);
		const op = choice(answers, "thread_op");
		if (op === undefined || !isThreadOp(op)) {
			throw new OnlyneError("decision_unavailable", text("error.thread_op"));
		}
		// A dependent second call: which thread the operation names is only decidable once the
		// operation is known, and meta.thread settles it without a call (design §4, stepwise).
		const thread = op === "create" || op === "list"
			? await this.#thread(meta)
			: await this.#thread(meta) ?? await this.#threadQuestion(scope, "outbound.o2c-thread");
		const to = await this.#target(meta);
		const title = stringField(meta, "title");
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

	/** Node O-2D: the domain topology operation. */
	async #domainAction(scope: DecisionScope, meta: Record<string, Json>): Promise<Action> {
		const started = Date.now();
		const answers = await this.#judge(
			scope.ctx,
			{ focus: this.#focus(scope) },
			{
				domain_op: {
					type: "choice",
					instructions: text("judge.domain_op.instructions"),
					criteria: {
						create: text("judge.domain_op.create"),
						join: text("judge.domain_op.join"),
						leave: text("judge.domain_op.leave"),
						list: text("judge.domain_op.list"),
						describe: text("judge.domain_op.describe"),
						discover: text("judge.domain_op.discover"),
					},
				},
			},
			scope.signal,
		);
		this.#record("outbound.o2d", scope.input, answers, Date.now() - started);
		const op = choice(answers, "domain_op");
		if (op === undefined || !isDomainOp(op)) {
			throw new OnlyneError("decision_unavailable", text("error.domain_op"));
		}
		const domainId = stringField(meta, "domain_id");
		if (op !== "create" && op !== "list" && domainId === undefined) {
			throw new OnlyneError("invalid_request", text("error.domain_id", { op }));
		}
		return {
			kind: "domain",
			op,
			...(domainId === undefined ? {} : { domain_id: domainId }),
			...(typeof meta.expected_revision === "string" ? { expected_revision: meta.expected_revision } : {}),
		};
	}

	/** The resource branch carries no judge node: meta names every field. */
	async #resourceAction(meta: Record<string, Json>): Promise<Action> {
		const resource = recordField(meta, "resource");
		const owner = resource === undefined ? undefined : stringField(resource, "owner");
		const id = resource === undefined ? undefined : stringField(resource, "id");
		const operation = stringField(meta, "operation");
		if (owner === undefined || id === undefined || operation === undefined) {
			throw new OnlyneError("invalid_request", text("error.resource_action_fields"));
		}
		return {
			kind: "resource",
			resource: { owner, id },
			operation,
			...(meta.data === undefined ? {} : { data: meta.data }),
			...(typeof meta.expected_revision === "string" ? { expected_revision: meta.expected_revision } : {}),
		};
	}

	/** The golden-triage head of every node state: what is being asked, verbatim. */
	#focus(scope: DecisionScope): Row {
		return {
			delivery_kind: scope.input.delivery_kind,
			msg: scope.input.msg,
			meta_keys: scope.input.meta === undefined ? [] : Object.keys(scope.input.meta).sort(),
		};
	}

	/** The user's latest direction behind the call, the one thing a bare msg cannot carry. */
	#trigger(ctx: ExtensionContext): Row {
		const rows = this.#history(ctx);
		const role = (row: Row): string | undefined => asRecord(row.message)?.role as string | undefined;
		const latest = (kind: string): Row | undefined => rows.filter(row => row.type === "message" && role(row) === kind).at(-1);
		const prompt = latest("user");
		const thought = latest("assistant");
		return {
			recent_user_prompt: prompt === undefined ? null : snippet(asRecord(prompt.message)?.content),
			last_thought: thought === undefined ? null : snippet(asRecord(thought.message)?.content),
		};
	}

	/** Inbound messages this node has not answered yet, newest first (O-2A's reply hint). */
	async #unanswered(ctx: ExtensionContext): Promise<Row[]> {
		const rows: Row[] = [];
		try {
			const threads = (await this.deps.listThreads()).slice(-RECENT_INBOUND_THREADS);
			for (const thread of threads) {
				const page = await this.deps.listMessages(thread.thread_ref, undefined);
				for (const entry of page.items.slice(-RECENT_INBOUND_PER_THREAD)) {
					if (entry.message.sender === this.deps.config.node.id) continue;
					if (entry.message.meta?.in_reply_to !== undefined) continue;
					rows.push({
						thread_ref: entry.thread_ref,
						seq: entry.seq,
						from: entry.message.sender,
						delivery_kind: entry.message.delivery_kind,
						msg: boundText(entry.message.msg),
					});
				}
			}
		} catch (error) {
			// The hint is optional context; a failed read narrows the question, not the decision.
			this.deps.log?.("onlyne.judge.hint_unavailable", { what: "recent_inbound_unanswered", error: message(error) });
		}
		return rows.slice(-RECENT_INBOUND_THREADS * RECENT_INBOUND_PER_THREAD).reverse();
	}


	async #thread(meta: Record<string, Json> | undefined): Promise<ThreadRef | undefined> {
		if (meta === undefined) return undefined;
		const value = meta.thread;
		if (value === undefined) return undefined;
		if (typeof value === "string") return this.#threadByAlias(value);
		const record = recordField(meta, "thread");
		const owner = record === undefined ? undefined : stringField(record, "owner");
		const id = record === undefined ? undefined : stringField(record, "id");
		if (owner !== undefined && id !== undefined) return { owner, id };
		throw new OnlyneError("invalid_request", text("error.thread_reference_shape"));
	}

	async #threadByAlias(alias: string): Promise<ThreadRef> {
		const threads = await this.deps.listThreads();
		const matches = threads.filter(thread => thread.thread_ref.id === alias || thread.title === alias);
		if (matches.length === 1) return matches[0]!.thread_ref;
		if (matches.length > 1) throw new OnlyneError("ambiguous_target", text("error.ambiguous_thread", { alias }));
		if (alias.includes("/")) {
			const [owner, id] = alias.split("/", 2);
			if (owner !== undefined && id !== undefined && owner.length > 0 && id.length > 0) return { owner, id };
		}
		throw new OnlyneError("not_found", text("error.thread_not_found", { alias }));
	}

	async #target(meta: Record<string, Json> | undefined): Promise<NodeId | undefined> {
		if (meta === undefined) return undefined;
		const value = meta.to;
		if (value === undefined) return undefined;
		if (typeof value !== "string") {
			throw new OnlyneError("invalid_request", text("error.meta_to_single"));
		}
		if (value.length === 0) throw new OnlyneError("invalid_request", text("error.meta_to_empty"));
		return this.#peerId(value);
	}

	/**
	 * RFC 0018 §3.1: the node a mechanical `op` names, resolved against the same peer set a send
	 * resolves `meta.to` from, so a name that addresses a peer once addresses it everywhere.
	 */
	async peerId(alias: string): Promise<NodeId> {
		return this.#peerId(alias);
	}

	/**
	 * Resolve an explicit target to a node id. Identity comes from the trust table and the
	 * discovery catalog, never from liveness: a peer whose exchange is done is still that peer
	 * when its discovery record goes stale or is deleted. Whether we can reach it is a delivery
	 * question, reported as `unavailable` by the transport, not a reason to claim no such node
	 * exists. A bare alias is accepted only when exactly one known node's last segment names it.
	 */
	async #peerId(alias: string): Promise<NodeId> {
		const ids = await this.#knownIds();
		if (alias === "self") return this.deps.config.node.id;
		if (ids.includes(alias)) return alias;
		const matches = ids.filter(id => lastSegment(id) === alias);
		if (matches.length === 1) return matches[0]!;
		if (matches.length > 1) {
			throw new OnlyneError("ambiguous_target", text("error.alias_ambiguous", { alias, matches: matches.join(", ") }));
		}
		throw new OnlyneError("not_found", text("error.alias_not_found", { alias }));
	}

	/**
	 * Every node this one can name: itself, each peer it holds a live trust row with, and each
	 * peer discovery currently reports. The trust rows come first because they are the ones that
	 * survive a peer moving, restarting or dropping out of the catalog for a refresh window.
	 */
	async #knownIds(): Promise<NodeId[]> {
		const ids = new Set<NodeId>([this.deps.config.node.id]);
		try {
			for (const peer of await knownPeers(this.deps)) ids.add(peer.node_id);
		} catch (error) {
			// Both sources failing leaves only this node; an alias then resolves to `not_found`,
			// which is the honest answer when nothing can be read at all.
			this.deps.log?.("onlyne.judge.peers_unreadable", { error: message(error) });
		}
		return [...ids];
	}

	async #mentions(meta: Record<string, Json> | undefined): Promise<string[] | undefined> {
		if (meta === undefined) return undefined;
		const value = meta.mentions;
		if (value === undefined) return undefined;
		if (!Array.isArray(value)) throw new OnlyneError("invalid_request", text("error.mentions_array"));
		const resolved: string[] = [];
		for (const item of value) {
			if (typeof item !== "string" || item.length === 0) {
				throw new OnlyneError("invalid_request", text("error.mention_empty"));
			}
			if (item === "@all") {
				resolved.push(item);
				continue;
			}
			resolved.push(await this.#peerId(item.startsWith("@") ? item.slice(1) : item));
		}
		return resolved;
	}

	async #reply(meta: Record<string, Json> | undefined): Promise<{ ref: MessageRef; authority: NodeId; thread_ref?: ThreadRef } | undefined> {
		if (meta === undefined) return undefined;
		const value = meta.in_reply_to;
		if (value === undefined) return undefined;
		if (value === null || typeof value !== "object" || Array.isArray(value)) {
			throw new OnlyneError("invalid_request", text("error.in_reply_to_object"));
		}
		const record = value as Record<string, Json>;
		const sender = stringField(record, "sender");
		const id = stringField(record, "id");
		if (sender !== undefined && id !== undefined) {
			// The authority is an identity, not the caller's spelling: `caerulea` and
			// `urn:onlyne:agent:caerulea` name the same node, and the delivery target is always
			// resolved. Comparing a raw string against a resolved id produced two contradictory
			// errors in a row (cyanea report 1, problem 1).
			const authority = await this.#peerId(sender).catch(() => sender);
			return { ref: { sender, id }, authority };
		}
		const threadAlias = stringField(record, "thread");
		const seq = seqField(record, "seq");
		if (threadAlias === undefined) {
			throw new OnlyneError("invalid_request", text("error.in_reply_to_shape"));
		}
		if (seq === undefined) {
			throw new OnlyneError("invalid_request", text("error.in_reply_to_seq"));
		}
		const thread = await this.#threadByAlias(threadAlias);
		const entry = await this.#entryAt(thread, seq);
		if (entry === undefined) throw new OnlyneError("not_found", text("error.message_not_found", { thread: threadAlias, seq }));
		return {
			ref: { sender: entry.message.sender, id: entry.message.id },
			authority: entry.reply_authority,
			thread_ref: entry.thread_ref,
		};
	}

	/**
	 * The message a read names, with the thread it was resolved in. `{sender,id}` carries no
	 * thread here; `{thread,seq}` does, and an explicit meta.thread that contradicts the
	 * reference is refused rather than silently winning.
	 */
	async #messageRef(meta: Record<string, Json>, thread?: ThreadRef): Promise<{ ref: MessageRef; thread?: ThreadRef } | undefined> {
		const value = meta.message;
		const record = value !== null && typeof value === "object" && !Array.isArray(value)
			? (value as Record<string, Json>)
			: undefined;
		const sender = record === undefined ? undefined : stringField(record, "sender");
		const id = record === undefined ? undefined : stringField(record, "id");
		if (sender !== undefined && id !== undefined) {
			return { ref: { sender, id }, ...(thread === undefined ? {} : { thread }) };
		}
		// meta.message takes the same two shapes as meta.in_reply_to, including its own thread
		// reference: `{thread, seq}` is the form a reader copies out of another reference, and
		// refusing it while accepting `{sender, id}` made the same fact two spellings (caerulea 4).
		const alias = record === undefined ? undefined : stringField(record, "thread");
		const embedded = alias === undefined ? undefined : await this.#threadByAlias(alias);
		if (thread !== undefined && embedded !== undefined && threadKey(thread) !== threadKey(embedded)) {
			throw new OnlyneError(
				"invalid_request",
				text("error.message_thread_conflict", { thread: threadKey(thread), embedded: threadKey(embedded) }),
			);
		}
		const scope = thread ?? embedded;
		if (scope === undefined) return undefined;
		const seq = (record === undefined ? undefined : seqField(record, "seq")) ?? seqField(meta, "seq");
		if (seq === undefined) return undefined;
		const entry = await this.#entryAt(scope, seq);
		if (entry === undefined) throw new OnlyneError("not_found", text("error.message_not_found", { thread: scope.id, seq }));
		return { ref: { sender: entry.message.sender, id: entry.message.id }, thread: scope };
	}

	async #entryAt(thread: ThreadRef, seq: string): Promise<Entry | undefined> {
		let cursor: string | undefined;
		for (let page = 0; page < MAX_TAIL_PAGES; page++) {
			const result: Page<Entry> = await this.deps.listMessages(thread, cursor);
			const found = result.items.find(entry => entry.seq === seq);
			if (found !== undefined) return found;
			cursor = result.next_cursor ?? undefined;
			if (cursor === undefined) return undefined;
		}
		return undefined;
	}

	/**
	 * A subagent may read only the threads its own context records authorize on
	 * this node. The unfiltered node-wide views stay with the main session.
	 */
	async #guard(scope: DecisionScope, action: Action): Promise<void> {
		if (scope.readOnly !== true) return;
		if (action.kind !== "read") {
			throw new OnlyneError(
				"forbidden",
				text("error.guard_write"),
			);
		}
		if (action.scope === "resources") throw new OnlyneError("forbidden", text("error.resources_main_only"));
		if (action.scope === "directory" || action.scope === "neighbors" || action.scope === "threads" || action.scope === "inbox") {
			throw new OnlyneError(
				"forbidden",
				text("error.guard_node_listing"),
			);
		}
		const records = await this.#authorizedRecords(scope.ctx.agent.id);
		if (action.scope === "evidence") {
			const ref = action.message;
			const tied = ref !== undefined && records.some(record =>
				record.source !== undefined && record.source.sender === ref.sender && record.source.id === ref.id,
			);
			if (!tied) {
				throw new OnlyneError("forbidden", text("error.guard_evidence"));
			}
			return;
		}
		// `{sender,id}` names no thread, so the record that received that message is the
		// authorization — the same tie evidence uses.
		if (action.scope === "message" && action.thread === undefined && action.message !== undefined) {
			const ref = action.message;
			const tied = records.some(record =>
				record.source !== undefined && record.source.sender === ref.sender && record.source.id === ref.id,
			);
			if (!tied) {
				throw new OnlyneError("forbidden", text("error.guard_message"));
			}
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

	async #authorizedRecords(agentId: string): Promise<ContextRecord[]> {
		let records: ContextRecord[];
		try {
			records = await this.deps.contexts.list();
		} catch (error) {
			throw new OnlyneError("unavailable", text("error.registry_unreadable", { error: message(error) }));
		}
		return records.filter(record =>
			record.node_id === this.deps.config.node.id && record.agent_id === agentId && record.thread_ref !== undefined,
		);
	}

	async #contextRows(): Promise<Source<Row[]>> {
		try {
			const records = (await this.deps.contexts.list()).filter(record => record.node_id === this.deps.config.node.id);
			return ok(records.map(record => {
				// A subagent session cannot send, so a reply routed there can never be answered.
				// The judge sees that fact instead of a bare agent id (caerulea report 3, problem 1).
				const isMain = canAnswer(record);
				return {
					alias: record.alias,
					summary: record.summary,
					state: record.state,
					kind: isMain ? "main" : "subagent",
					can_answer: isMain,
					// A lost or never-bound record cannot receive: offering it guarantees a failure.
					live: record.state !== "lost" && record.state !== "unbound",
					agent_id: record.agent_id ?? null,
					main_agent_id: record.main_agent_id,
					node_id: record.node_id,
					thread_ref: record.thread_ref ?? null,
					source: record.source ?? null,
					created_at: record.created_at,
					bound_at: record.bound_at ?? null,
				};
			}));
		} catch (error) {
			return none(text("reason.registry_unreadable", { error: message(error) }));
		}
	}


	async #receiverContexts(nodeId: NodeId): Promise<Source<ReceiverContext[]>> {
		try {
			return ok(await this.deps.listReceiverContexts(nodeId));
		} catch (error) {
			return none(text("reason.receiver_contexts_unreadable", { error: message(error) }));
		}
	}

	/** The active branch as ordered, id-bearing rows: the newest rows, each text bounded. */
	#history(ctx: ExtensionContext): Row[] {
		const rows: Row[] = [];
		for (const entry of branchOf(ctx)) {
			const row = entryRow(entry);
			if (row !== undefined) rows.push(row);
		}
		return rows.slice(-HISTORY_ROWS);
	}

	/**
	 * The todo-more snapshot on the active branch. It survives a reset boundary
	 * (RFC 0016 §2.2); an empty snapshot ends the search so older lists never
	 * resurrect, and a malformed newest snapshot is reported instead of skipped.
	 */
	#todo(ctx: ExtensionContext): unknown {
		const branch = branchOf(ctx);
		for (let index = branch.length - 1; index >= 0; index--) {
			const row = asRecord(branch[index]);
			if (row?.type !== "custom" || row.customType !== TODO_ENTRY) continue;
			const data = asRecord(row.data);
			if (data === undefined) return none(text("reason.todo_no_data"));
			const phases = data.phases;
			if (!Array.isArray(phases)) return none(text("reason.todo_no_phases"));
			return normalizeTodo({ goal: data.goal ?? null, phases });
		}
		return { present: false, reason: text("reason.todo_absent") };
	}

	async #threadEvidenceFor(refs: ThreadRef[]): Promise<Row[]> {
		const seen = new Map<string, ThreadRef>();
		for (const ref of refs) {
			const key = threadKey(ref);
			if (!seen.has(key)) seen.set(key, ref);
		}
		const rows: Row[] = [];
		for (const ref of seen.values()) rows.push(await this.#threadTail(ref));
		return rows;
	}

	async #threadTail(ref: ThreadRef): Promise<Row> {
		try {
			let cursor: string | undefined;
			let last: Entry[] = [];
			let scanned = 0;
			let truncated = false;
			for (;;) {
				const page = await this.deps.listMessages(ref, cursor);
				last = page.items;
				scanned += 1;
				const next = page.next_cursor ?? undefined;
				if (next === undefined) break;
				cursor = next;
				if (scanned >= MAX_TAIL_PAGES) {
					truncated = true;
					break;
				}
			}
			return {
				thread_ref: ref,
				entries: last.map(entryRowOf),
				window: "tail",
				pages_scanned: scanned,
				truncated,
				next_cursor: truncated ? cursor ?? null : null,
			};
		} catch (error) {
			return { thread_ref: ref, unavailable: message(error) };
		}
	}

}

type ActionReadScope = Extract<Action, { kind: "read" }>["scope"];

function isReadScope(value: string): value is ActionReadScope {
	return value === "directory" || value === "neighbors" || value === "threads" || value === "inbox" || value === "messages" || value === "message" || value === "evidence" || value === "resources";
}

function isThreadOp(value: string): value is Extract<Action, { kind: "thread" }>["op"] {
	return value === "create" || value === "close" || value === "list" || value === "describe" || value === "member_add" || value === "member_remove";
}

function isDomainOp(value: string): value is Extract<Action, { kind: "domain" }>["op"] {
	return value === "create" || value === "join" || value === "leave" || value === "list" || value === "describe" || value === "discover";
}

/** The fields of one judged message: intent, every answer the judge gave, and the time. */
export function judgeRecord(
	direction: string,
	input: { delivery_kind: string; msg: string; meta?: unknown },
	answers: Answers,
	ms: number,
): Record<string, unknown> {
	const picked: Record<string, string> = {};
	for (const [key, value] of Object.entries(answers)) {
		const answer = value !== null && typeof value === "object" && "choice" in value ? (value as { choice?: unknown }).choice : undefined;
		if (typeof answer === "string" && answer.length > 0) picked[key] = answer;
	}
	return {
		direction,
		intent: input.delivery_kind,
		msg: input.msg.length > 120 ? `${input.msg.slice(0, 120)}…` : input.msg,
		answers: picked,
		language: getLocale(),
		ms,
	};
}

/** The fields of a decided action worth one log line. */
function actionFacts(action: Action): Record<string, string> {
	const facts: Record<string, string> = { action: action.kind };
	const record = action as unknown as Record<string, unknown>;
	for (const key of ["to", "context", "response", "execution", "relation", "scope", "op", "operation"]) {
		const value = record[key];
		if (typeof value === "string" && value.length > 0) facts[key] = value;
	}
	const thread = record.thread as { owner?: unknown; id?: unknown } | undefined;
	if (thread !== undefined && typeof thread.owner === "string" && typeof thread.id === "string") facts.thread = `${thread.owner}/${thread.id}`;
	const resource = record.resource as { owner?: unknown; id?: unknown } | undefined;
	if (resource !== undefined && typeof resource.owner === "string" && typeof resource.id === "string") facts.resource = `${resource.owner}/${resource.id}`;
	return facts;
}

function choice(answers: Answers, key: string): string | undefined {
	const answer = answers[key];
	if (answer === undefined || answer === null || typeof answer !== "object") return undefined;
	return typeof answer.choice === "string" ? answer.choice : undefined;
}

function stringField(record: Record<string, Json>, key: string): string | undefined {
	const value = record[key];
	return typeof value === "string" && value.length > 0 ? value : undefined;
}

/**
 * A sequence number as the model writes it: `"1"` and `1` both mean the same seq. The judge
 * schema says `record(string, unknown)`, so nothing tells the caller to quote it (caerulea
 * report 3, problem 2).
 */
function seqField(record: Record<string, Json>, key: string): string | undefined {
	const value = record[key];
	if (typeof value === "string" && value.length > 0) return value;
	if (typeof value === "number" && Number.isSafeInteger(value) && value >= 0) return String(value);
	return undefined;
}

function recordField(record: Record<string, Json>, key: string): Record<string, Json> | undefined {
	const value = record[key];
	return value !== null && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, Json>) : undefined;
}

function pageSize(meta: Record<string, Json>): number | undefined {
	const value = meta.page_size;
	if (value === undefined) return undefined;
	if (typeof value !== "number" || !Number.isInteger(value) || value < 1 || value > 100) {
		throw new OnlyneError("invalid_request", text("error.page_size"));
	}
	return value;
}

function attention(meta: Record<string, Json>): "inbox" | "thread" {
	const value = meta.attention;
	// RFC 0005 §1: reads default to the attention list; `thread` is the switch that widens one.
	if (value === undefined) return "inbox";
	if (value === "inbox" || value === "thread") return value;
	throw new OnlyneError("invalid_request", text("error.attention"));
}

function relation(meta: Record<string, Json> | undefined): "reply" | "followup" | undefined {
	if (meta === undefined) return undefined;
	const value = meta.relation;
	if (value === undefined) return undefined;
	if (value === "reply" || value === "followup") return value;
	throw new OnlyneError("invalid_request", text("error.relation_value"));
}

function asRecord(value: unknown): Row | undefined {
	return value !== null && typeof value === "object" && !Array.isArray(value) ? (value as Row) : undefined;
}

function branchOf(ctx: ExtensionContext): readonly SessionEntry[] {
	return ctx.sessionManager.getBranch();
}

function threadKey(ref: ThreadRef): string {
	return `${ref.owner}/${ref.id}`;
}

/**
 * A wording hint about whether the sender expects an answer: metadata says reply, or the body
 * greets or asks. It is evidence for node I-0, never the decision — a completed reply also
 * mentions a greeting, and a status note also asks nothing (dpdp report 4, host shape).
 */
export function needsReplyOf(entry: Entry): boolean {
	if (entry.message.meta?.in_reply_to !== undefined) return true;
	if (entry.message.meta?.relation === "reply") return true;
	const body = entry.message.msg.toLowerCase();
	return REPLY_CUES.some(cue => body.includes(cue.toLowerCase())) || /\?|？|\bplease\b|期待/.test(entry.message.msg);
}

/** A node id's last URN segment; plain ids are their own segment. */
function lastSegment(nodeId: NodeId): string {
	const trimmed = nodeId.replace(/\/+$/, "");
	const parts = trimmed.split(/[:/]/).filter(part => part.length > 0);
	return parts.at(-1) ?? trimmed;
}

function entryFacts(entry: Entry): Row {
	return {
		thread_ref: entry.thread_ref,
		seq: entry.seq,
		authenticated_author: entry.authenticated_author,
		reply_authority: entry.reply_authority,
		at: entry.at,
		message: {
			id: entry.message.id,
			sender: entry.message.sender,
			delivery_kind: entry.message.delivery_kind,
			msg: entry.message.msg,
			meta: entry.message.meta ?? null,
		},
	};
}

function entryRowOf(entry: Entry): Row {
	return {
		seq: entry.seq,
		at: entry.at,
		authenticated_author: entry.authenticated_author,
		reply_authority: entry.reply_authority,
		message: {
			id: entry.message.id,
			sender: entry.message.sender,
			delivery_kind: entry.message.delivery_kind,
			msg: entry.message.msg,
			meta: entry.message.meta ?? null,
		},
	};
}

function entryRow(entry: SessionEntry): Row | undefined {
	const row = asRecord(entry);
	if (row === undefined) return undefined;
	const id = typeof row.id === "string" ? row.id : undefined;
	const type = typeof row.type === "string" ? row.type : "unknown";
	const parent = typeof row.parentId === "string" ? row.parentId : null;
	const at = typeof row.timestamp === "string" ? row.timestamp : undefined;
	switch (type) {
		case "message": {
			const message = asRecord(row.message);
			if (message === undefined) return { id, type, parent_id: parent, at };
			const role = typeof message.role === "string" ? message.role : "unknown";
			const out: Row = {
				id,
				type,
				parent_id: parent,
				at,
				message: { role, content: contentOf(message.content) },
			};
			const nested = out.message as Row;
			if (typeof message.toolName === "string") nested.tool_name = message.toolName;
			if (typeof message.toolCallId === "string") nested.tool_call_id = message.toolCallId;
			if (typeof message.isError === "boolean") nested.is_error = message.isError;
			if (typeof message.stopReason === "string") nested.stop_reason = message.stopReason;
			return out;
		}
		case "custom_message":
			return {
				id,
				type,
				parent_id: parent,
				at,
				custom_type: row.customType ?? null,
				content: contentOf(row.content),
				display: row.display ?? null,
				attribution: row.attribution ?? null,
			};
		case "custom":
			return { id, type, parent_id: parent, at, custom_type: row.customType ?? null, data: row.data ?? null };
		case "compaction":
			return {
				id,
				type,
				parent_id: parent,
				at,
				summary: row.summary ?? null,
				first_kept_entry_id: row.firstKeptEntryId ?? null,
				tokens_before: row.tokensBefore ?? null,
			};
		case "branch_summary":
			return { id, type, parent_id: parent, at, summary: row.summary ?? null, from_id: row.fromId ?? null };
		case "mode_change":
			return { id, type, parent_id: parent, at, mode: row.mode ?? null, data: row.data ?? null };
		case "session_init":
			return { id, type, parent_id: parent, at, agent: row.agent ?? null, task: row.task ?? null };
		case "reset_boundary":
			return { id, type, parent_id: parent, at };
		case "model_usage":
			return undefined;
		default:
			return { id, type, parent_id: parent, at };
	}
}

/** One pasted terminal dump must not become the whole decision prompt. */
function boundText(value: string): string {
	return cut(value, TEXT_CHARS);
}

function cut(value: string, limit: number): string {
	return value.length <= limit ? value : `${value.slice(0, limit)}${text("prose.cut", { more: value.length - limit })}`;
}

/** One branch message as trigger material: its text, cut to the trigger budget. */
function snippet(content: unknown): string | null {
	if (typeof content === "string") return cut(content, TRIGGER_CHARS);
	if (!Array.isArray(content)) return null;
	const joined = content
		.map(part => asRecord(part))
		.filter(part => typeof part?.text === "string")
		.map(part => String(part!.text))
		.join("\n");
	return joined.length === 0 ? null : cut(joined, TRIGGER_CHARS);
}

/** Content parts keep their text; image bytes are not decision material and are named, not embedded. */
function contentOf(value: unknown): unknown {
	if (typeof value === "string") return boundText(value);
	if (!Array.isArray(value)) return value ?? null;
	return value.map(part => {
		const row = asRecord(part);
		if (row === undefined) return part;
		if (row.type === "image") {
			return {
				type: "image",
				mime_type: row.mimeType ?? row.mime_type ?? null,
				bytes: text("prose.image_omitted"),
			};
		}
		if (typeof row.text === "string") return { ...row, text: boundText(row.text) };
		return row;
	});
}

interface TodoView {
	goal: unknown;
	phases: Array<{ name: string; items: Array<{ title: string; status?: string; explanation?: string }> }>;
}

/**
 * A todo snapshot as the judge sees it. Raw `mouriya.omp.todo-more` snapshots and
 * omp-todo-more's `readTodoMore` state are both accepted; anything unrecognized is
 * reported, never read as an empty list.
 */
function normalizeTodo(current: unknown): Source<TodoView> | { present: false; reason: string } {
	if (current === undefined) return none(text("reason.todo_missing"));
	const row = asRecord(current);
	if (row === undefined) return none(text("reason.todo_not_object"));
	if (row.present === false) {
		return { present: false, reason: typeof row.reason === "string" ? row.reason : text("reason.todo_source_absent") };
	}
	if (row.kind === "empty") return ok({ goal: null, phases: [] });
	const phases = row.phases;
	if (!Array.isArray(phases)) return none(text("reason.todo_phases_missing"));
	const goal = asRecord(row.goal);
	const goalValue = goal?.kind === "set"
		? goal.goal ?? null
		: goal?.kind === "missing"
			? null
			: row.goal ?? null;
	const normalized: TodoView["phases"] = [];
	for (const phase of phases) {
		const phaseRow = asRecord(phase);
		const name = typeof phaseRow?.name === "string" ? phaseRow.name : undefined;
		const items = phaseRow?.items;
		if (name === undefined || !Array.isArray(items)) return none(text("reason.todo_phase_invalid"));
		const rows: TodoView["phases"][number]["items"] = [];
		for (const item of items) {
			const itemRow = asRecord(item);
			const title = typeof itemRow?.title === "string" ? itemRow.title : undefined;
			if (title === undefined) return none(text("reason.todo_item_title_missing"));
			const explanationRow = asRecord(itemRow?.explanation);
			const explanation = typeof itemRow?.explanation === "string"
				? itemRow.explanation
				: explanationRow?.kind === "explained" && typeof explanationRow.text === "string"
					? explanationRow.text
					: undefined;
			rows.push({
				title,
				...(typeof itemRow?.status === "string" ? { status: itemRow.status } : {}),
				...(explanation === undefined ? {} : { explanation }),
			});
		}
		normalized.push({ name, items: rows });
	}
	return ok({ goal: goalValue, phases: normalized });
}

function todoTitles(source: unknown): string[] {
	const view = valueOf<TodoView>(source);
	if (view !== undefined) return view.phases.flatMap(phase => phase.items.map(item => item.title));
	const raw = asRecord(source);
	if (raw === undefined) return [];
	const phases = raw.phases;
	if (!Array.isArray(phases)) return [];
	return phases.flatMap(phase => {
		const items = asRecord(phase)?.items;
		if (!Array.isArray(items)) return [];
		return items.flatMap(item => {
			const title = asRecord(item)?.title;
			return typeof title === "string" ? [title] : [];
		});
	});
}

/** The T-1 view of the todo list: titles and statuses only, no bodies. */
function todoSummary(source: unknown): Array<{ title: string; status: string }> {
	const view = valueOf<TodoView>(source);
	if (view !== undefined) {
		return view.phases.flatMap(phase => phase.items.map(item => ({ title: item.title, status: item.status ?? "open" })));
	}
	const raw = asRecord(source);
	if (raw === undefined) return [];
	const phases = raw.phases;
	if (!Array.isArray(phases)) return [];
	return phases.flatMap(phase => {
		const items = asRecord(phase)?.items;
		if (!Array.isArray(items)) return [];
		return items.flatMap(item => {
			const row = asRecord(item);
			return typeof row?.title === "string" ? [{ title: row.title, status: typeof row.status === "string" ? row.status : "open" }] : [];
		});
	});
}

/** Titles and explanations offered to the judge are the message's own text, never invented. */
function todoCandidates(entry: Entry): { titles: string[]; explanations: string[] } {
	const body = entry.message.msg;
	const delivery = entry.message.delivery_kind.trim();
	const lines = body.split(/\r?\n/).map(line => line.trim()).filter(line => line.length > 0);
	const first = lines[0];
	const paragraph = body.split(/\r?\n\s*\r?\n/)[0]?.trim();
	const sentence = first === undefined ? undefined : (/^(.+?[.!?。！？])(\s|$)/.exec(first)?.[1] ?? first);
	const titles = unique([
		first,
		sentence,
		delivery,
		first === undefined || delivery.length === 0 ? undefined : `${delivery}: ${first}`,
	]);
	const explanations = unique([
		paragraph,
		body.trim(),
		first === undefined || delivery.length === 0 ? undefined : `${delivery} — ${first}`,
	]);
	return { titles, explanations };
}

function unique(values: Array<string | undefined>): string[] {
	const seen = new Set<string>();
	const out: string[] = [];
	for (const value of values) {
		if (value === undefined || value.length === 0 || value === "none" || seen.has(value)) continue;
		seen.add(value);
		out.push(value);
	}
	return out;
}


type JudgeJson = string | number | boolean | null | JudgeJson[] | { [key: string]: JudgeJson };
type JudgeState = { [key: string]: JudgeJson };

/** The judge state must be plain JSON: strip undefined and non-finite values. */
function jsonState(value: unknown): JudgeState {
	return JSON.parse(JSON.stringify(value)) as JudgeState;
}

function message(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}
