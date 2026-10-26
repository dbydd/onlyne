import { Settings } from "@oh-my-pi/pi-coding-agent/config/settings";
import type { ExtensionContext } from "@oh-my-pi/pi-coding-agent";
import { journalJudgmentUsage, resolveJudge } from "@oh-my-pi/pi-coding-agent/judgment";
import { AgentRegistry } from "@oh-my-pi/pi-coding-agent/registry/agent-registry";
import type { SessionEntry } from "@oh-my-pi/pi-coding-agent/session/session-entries";
import { AsyncLocalStorage } from "node:async_hooks";
import {
	type Action,
	type Entry,
	type Json,
	type MessageRef,
	type MsgInput,
	type NodeId,
	type Page,
	type Thread,
	type ThreadRef,
	OnlyneError,
} from "@onlyne/lib";
import type { ContextRecord, OnlyneConfig } from "./types.ts";
import type { ContextRegistry } from "./store.ts";

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

export interface DeciderDeps {
	config: OnlyneConfig;
	/** Registry id of this node's main session; children are bound under it. */
	mainAgentId: string;
	contexts: ContextRegistry;
	log?(message: string, fields: Record<string, unknown>): void;
	listThreads(): Promise<Thread[]>;
	listMessages(thread: ThreadRef, cursor?: string): Promise<Page<Entry>>;
	/** Peers from the library's live discovery records, never a static configuration. */
	listPeers(): Promise<PeerRecord[]>;
	/** Contexts the target node publishes for `execution: existing`. */
	listReceiverContexts(nodeId: NodeId): Promise<ReceiverContext[]>;
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
			await this.#guard(scope, explicit);
			return explicit;
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
		await this.#guard(scope, action);
		this.deps.log?.("onlyne.judge.action", { direction: "outbound", ...actionFacts(action) });
		return action;
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
					instructions:
						"Choose the one action the model's message asks for. communicate: deliver a message, reply or progress note to another agent. inspect: look up local or network state. manage_thread: create, close, describe or change members of a local thread. manage_domain: manage this node's domain membership. operate_resource: operate a resource the message names explicitly.",
					criteria: {
						communicate: "The message is meant for another agent or node, including asking a peer something.",
						inspect: "The message asks to see node-level state: who is online, the inbox, a thread's messages, one message, or delivery evidence. The thread registry and a thread's members are not this.",
						manage_thread: "The message works on threads themselves: list the threads, describe one thread's members, create, close, add or remove a member. Reading a thread's messages is inspect.",
						manage_domain: "The message changes or reads this node's domain membership: join, leave, list or describe the membership, or discover a domain's members.",
						operate_resource: "meta.resource names an external resource to operate. Without meta.resource this intent cannot execute, so a message that merely mentions a directory or a file is not this.",
					},
				},
			},
			scope.signal,
		);
		const intent = choice(answers, "intent");
		this.#record("outbound.o1", scope.input, answers, Date.now() - started);
		if (intent !== "communicate" && intent !== "inspect" && intent !== "manage_thread" && intent !== "manage_domain" && intent !== "operate_resource") {
			throw new OnlyneError("decision_unavailable", "The decision did not choose an outbound intent.");
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
			throw new OnlyneError("invalid_request", "A relation needs an in_reply_to reference.");
		}
		let to: NodeId;
		if (reply !== undefined) {
			// O-2A pre-pruning: the reply authority is the only target; meta.to and the judge's
			// answer must not move a reply to another node (caerulea report 3).
			const explicitTarget = await this.#target(meta);
			if (explicitTarget !== undefined && explicitTarget !== reply.authority) {
				throw new OnlyneError(
					"invalid_request",
					`The reply must go to its original reply authority ${reply.authority}, not ${explicitTarget}.`,
				);
			}
			to = reply.authority;
		} else {
			to = await this.#target(meta) ?? await this.#targetQuestion(scope, meta);
			if (to.length === 0) {
				throw new OnlyneError("ambiguous_target", "The decision did not choose a delivery target.");
			}
		}

		const plan = await this.#deliveryPlan(scope, meta, lockedRelation, reply !== undefined);
		const thread = await this.#sendThread(meta, to, reply !== undefined);
		const context = plan.execution === "new" ? undefined : await this.#receiverContext(scope, to, meta);
		// `existing` names where the work lands, not a reason to fail the delivery: with no
		// published context to continue, the receiver starts a new one (report 2, problem 2).
		const execution = plan.execution === "existing" && context === undefined ? "new" : plan.execution;
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
			execution,
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
					instructions:
						"The message references an earlier message. Is it a final reply or a progress supplement?",
					criteria: {
						reply: "A final answer that may release the original sender's wait.",
						followup: "A supplement or progress note that does not release a wait.",
					},
				},
			},
			scope.signal,
		);
		this.#record("outbound.o2a", scope.input, answers, Date.now() - started);
		const relation = choice(answers, "relation");
		if (relation !== "reply" && relation !== "followup") {
			throw new OnlyneError("decision_unavailable", "The decision did not choose a reply relation.");
		}
		return relation;
	}

	/** Node O-2A question 2: the delivery target among live peers, asked only when meta.to is absent. */
	async #targetQuestion(scope: DecisionScope, meta: Record<string, Json>): Promise<NodeId> {
		const peers = await this.#peers();
		if (!peers.available) {
			throw new OnlyneError("unavailable", `The target cannot be resolved: ${peers.reason}`);
		}
		// One discovered peer is the answer; none means the caller must name a target.
		if (peers.value.length === 0) throw new OnlyneError("not_found", "No discovered node can receive this message.");
		if (peers.value.length === 1) return peers.value[0]!.node_id;
		const started = Date.now();
		const answers = await this.#judge(
			scope.ctx,
			{
				focus: { ...this.#focus(scope), meta_to: meta.to ?? null },
				candidate_peers: peers.value.map(peer => ({
					node_id: peer.node_id,
					alias: lastSegment(peer.node_id),
					last_seen: peer.last_seen,
				})),
				recent_inbound_unanswered: await this.#unanswered(scope.ctx),
			},
			{
				target: {
					type: "choice",
					instructions:
						"Choose the node that should receive this message. Each candidate is named by its node id; the alias is its short name.",
					criteria: Object.fromEntries(peers.value.map(peer => [
						peer.node_id,
						`Discovered peer ${peer.node_id} at ${peer.base_url} (instance ${peer.instance_id}, last seen ${peer.last_seen}).`,
					])),
				},
			},
			scope.signal,
		);
		this.#record("outbound.o2a", scope.input, answers, Date.now() - started);
		const chosen = choice(answers, "target");
		if (chosen === undefined || chosen === "none") {
			throw new OnlyneError("ambiguous_target", "The decision did not choose a delivery target.");
		}
		if (!peers.value.some(peer => peer.node_id === chosen)) {
			throw new OnlyneError("decision_unavailable", `The decision named ${chosen}, which discovery does not publish.`);
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
								instructions:
									"Two facts decide: does the receiver have to act, and can the sender continue without the answer?",
								criteria: {
									none: "The receiver need not act at all: a notice, a status update, '不用回'.",
									async: "The receiver must act, but the sender keeps working and picks the answer up later: a background dispatch, a supplement to running work, '有空再做', '结果稍后贴我'.",
									await: "The receiver must act and the sender's next step cannot start without the answer: it is blocked on a question, a number it must have, or a conclusion it ships (等你回、数完回我、我要据此发版).",
								},
							},
						}
					: {}),
				...(execution === undefined
					? {
							execution: {
								type: "choice",
								instructions: "Should the receiver handle this in one of its published execution contexts or in a new one?",
								criteria: {
									existing: isReply
										? "The message continues work the receiver already knows from the referenced message."
										: "The message belongs to work already running in one of the target's published contexts.",
									new: "The message starts independent work and needs a new context the receiver creates.",
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
			throw new OnlyneError("decision_unavailable", "The judge did not choose a valid response mode.");
		}
		if (chosenExecution !== "existing" && chosenExecution !== "new") {
			throw new OnlyneError("decision_unavailable", "The judge did not choose a valid execution mode.");
		}
		return { response: chosenResponse, execution: chosenExecution };
	}

	#inherited(meta: Record<string, Json>, key: string): "await" | "async" | "none" | "existing" | "new" | undefined {
		const value = meta[key];
		if (value === "await" || value === "async" || value === "none" || value === "existing" || value === "new") return value;
		if (value !== undefined) throw new OnlyneError("invalid_request", `meta.${key} must be await, async, none, existing or new.`);
		return undefined;
	}

	/** Node O-3A-Ctx: pick among the target's published contexts; empty or unreadable degrades silently to new. */
	async #receiverContext(scope: DecisionScope, to: NodeId, meta: Record<string, Json>): Promise<string | undefined> {
		const receiver = await this.#receiverContexts(to);
		const explicit = stringField(meta, "context");
		if (!receiver.available) {
			if (explicit !== undefined) return explicit;
			return undefined;
		}
		if (explicit !== undefined) {
			if (!receiver.value.map(context => context.alias).includes(explicit)) {
				throw new OnlyneError("invalid_request", `The target does not publish an execution context named ${explicit}.`);
			}
			return explicit;
		}
		if (receiver.value.length === 0) return undefined;
		const started = Date.now();
		const answers = await this.#judge(
			scope.ctx,
			{ focus: this.#focus(scope), target_published_contexts: receiver.value },
			{
				context: {
					type: "choice",
					instructions: "Choose the execution context the target should continue in, or none when this message does not continue one.",
					criteria: {
						...Object.fromEntries(receiver.value.map(context => [context.alias, `${context.summary} (${context.state})`])),
						none: "The message does not continue any published context and starts fresh work instead.",
					},
				},
			},
			scope.signal,
		);
		this.#record("outbound.o3a-ctx", scope.input, answers, Date.now() - started);
		const alias = choice(answers, "context");
		// `existing` names where the work lands, not a reason to fail the delivery: none or an
		// unknown alias falls back to a new context (report 2, problem 2).
		if (alias === undefined || alias === "none" || !receiver.value.some(context => context.alias === alias)) {
			return undefined;
		}
		return alias;
	}

	/**
	 * The landing a send names. A reply's landing is the receiver's decision (RFC 0003 §6: the
	 * thread holding the original message), so naming one of this node's own threads would be
	 * accepted and then land elsewhere — the failure dpdp report 4 could not detect. A bare id
	 * that names no local thread is the target's thread id, as a receipt reports it.
	 */
	async #sendThread(meta: Record<string, Json> | undefined, to: NodeId, reply: boolean): Promise<ThreadRef | undefined> {
		if (meta?.thread === undefined) return undefined;
		const value = meta.thread;
		if (value !== null && typeof value === "object" && !Array.isArray(value)) {
			const thread = await this.#thread(meta);
			if (thread === undefined) return undefined;
			return this.#landing(thread, to, reply);
		}
		if (typeof value !== "string" || value.length === 0) {
			throw new OnlyneError("invalid_request", "meta.thread must be a thread alias or {owner,id}.");
		}
		const matches = (await this.deps.listThreads()).filter(thread => thread.thread_ref.id === value || thread.title === value);
		if (matches.length > 1) throw new OnlyneError("ambiguous_target", `More than one local thread is named ${value}.`);
		if (matches.length === 1) return this.#landing(matches[0]!.thread_ref, to, reply);
		const slash = value.indexOf("/");
		if (slash > 0 && slash < value.length - 1) {
			return this.#landing({ owner: value.slice(0, slash), id: value.slice(slash + 1) }, to, reply);
		}
		return { owner: to, id: value };
	}

	#landing(thread: ThreadRef, to: NodeId, reply: boolean): ThreadRef {
		if (thread.owner === to) return thread;
		if (thread.owner === this.deps.config.node.id) {
			throw new OnlyneError(
				"invalid_request",
				reply
					? `meta.thread names ${thread.id}, a thread this node owns. A reply lands in the thread that holds the original message, and ${to} decides it; drop meta.thread and read the receipt for the landing.`
					: `meta.thread names ${thread.id}, a thread this node owns, but the message targets ${to}. A delivery lands in a thread the target owns.`,
			);
		}
		throw new OnlyneError("invalid_request", `The named thread belongs to ${thread.owner}, but the message targets ${to}.`);
	}

	async decideInbound(scope: ExtensionContext, entry: Entry): Promise<InboundDecision> {
		const needsReply = needsReplyOf(entry);
		// Deterministic prefilter (design §5.1): a disposed or never-bound session is a delivery
		// black hole, and a message that needs an answer can only land where a reply can be sent.
		// Pruned candidates never reach the prompt, so the question only offers positive options.
		const registry = valueOf<Row[]>(await this.#contextRows()) ?? [];
		const valid = registry
			.filter(row =>
				row.agent_id !== null && row.agent_id !== undefined
				&& (!needsReply || row.can_answer === true)
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
			main_agent: "The message needs an answer, or no registered context's work matches it.",
			...Object.fromEntries(valid.map(row => [
				String(row.alias),
				`${String(row.summary)} (state: ${String(row.state)}, agent: ${String(row.agent_id)}${sameThread(row.thread_ref) ? ", same thread as this message" : ""})`,
			])),
			...(needsReply ? {} : { new_subagent: "The message is a self-contained work item with its own background, and needs no answer. A status note, greeting, relay or summary is not this." }),
		};
		// A pruned candidate set can leave one route; asking would be a question with one answer.
		const forced = Object.keys(criteria);
		const answers: Answers = forced.length === 1
			? { routing_target: { choice: forced[0] } }
			: await this.#judge(
					scope,
					{
						focus: { node: this.deps.config.node.id, incoming: entryFacts(entry) },
						needs_reply: needsReply,
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
							instructions:
								"Choose where this inbound message is handled. main_agent: the main session takes it. A context alias: the named execution context continues the work it was created for. new_subagent: fresh independent work in a new subagent.",
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
		if (chosen === undefined) throw new OnlyneError("decision_unavailable", "The decision did not choose a routing target.");
		if (chosen === "new_subagent") {
			if (needsReply) return override(chosen, "a subagent cannot answer a message that needs a reply");
			return { action: "new_subagent" };
		}
		if (chosen !== "main_agent") {
			const row = valid.find(candidate => candidate.alias === chosen);
			if (row === undefined) return override(chosen, "the decision named a context the prefilter did not offer");
			return { action: "context", context: chosen };
		}
		const insertion = await this.#insertionMode(scope, entry, needsReply);
		if (insertion === "fragment") return { action: "fragment" };
		return { action: "wake", insertion };
	}

	/** Node I-2: when a main-session delivery enters the agent's work. */
	async #insertionMode(scope: ExtensionContext, entry: Entry, needsReply: boolean): Promise<"steer" | "follow_up" | "fragment"> {
		const body = entry.message.msg.toLowerCase();
		const started = Date.now();
		// `fragment` never starts a turn, so a message that needs an answer can never be one;
		// offering it would be an option whose choice guarantees the obligation is missed.
		const criteria: Record<string, string> = {
			steer: "Inject immediately after the current tool call: an urgent correction the running work must follow before it continues.",
			follow_up: "Add when the current turn finishes and start a new turn: the session is expected to act on it, answer it, or use its result.",
			...(needsReply ? {} : { fragment: "Record without starting a turn and expect nothing back: status syncs, log asides, version records and environment facts that say no action is needed (状态同步、日志旁路、版本记录、暂时不用管)." }),
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
					instructions: "The message goes to the main session. Choose when it should enter the agent's work.",
					criteria,
				},
			},
		);
		this.#record("inbound.i2", { delivery_kind: entry.message.delivery_kind, msg: entry.message.msg, meta: entry.message.meta }, answers, Date.now() - started);
		const mode = choice(answers, "insertion_mode");
		if (mode === "fragment" && needsReply) {
			this.deps.log?.("onlyne.judge.overridden", { direction: "inbound", chosen: "fragment", fallback: "follow_up", reason: "a silent fragment cannot answer a message that needs a reply" });
			return "follow_up";
		}
		if (mode !== "steer" && mode !== "follow_up" && mode !== "fragment") {
			throw new OnlyneError("decision_unavailable", "The decision did not choose an insertion mode.");
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
			none: "The message needs no persistent work item: it is informational, duplicate, or already covered.",
		};
		if (existing.length > 0) actionCriteria.associate = "The message continues or supplements an existing item.";
		if (candidates.titles.length > 0) actionCriteria.append = "The message needs a new persistent work item that no existing item covers.";
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
							instructions: "Decide what this message should do to the session's todo list.",
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
								instructions: "Choose the existing todo item this message continues.",
								criteria: Object.fromEntries(existing.map(title => [title, "An existing todo item."])),
							},
						},
					);
			this.#record("todo.t2", input, follow, Date.now() - t2);
			const title = choice(follow, "task");
			if (title === undefined || title === "none" || !existing.includes(title)) {
				throw new OnlyneError("decision_unavailable", "The decision chose to associate without naming an existing todo item.");
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
								instructions: "Choose the title for the new item from the message's own text.",
								criteria: Object.fromEntries(candidates.titles.map(title => [title, "A title taken from the message."])),
							},
						}),
				...(candidates.explanations.length === 0
					? {}
					: {
							explanation: {
								type: "choice" as const,
								instructions: "Choose the explanation for the new item from the message's own text, or none to leave it unwritten.",
								criteria: {
									...Object.fromEntries(candidates.explanations.map(text => [text, "An explanation taken from the message."])),
									none: "Leave the explanation unwritten.",
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
				throw new OnlyneError("decision_unavailable", "The decision chose to append without naming one of the message's title candidates.");
			}
			const explanation = choice(follow, "explanation");
			const chosen = explanation !== undefined && explanation !== "none" && candidates.explanations.includes(explanation)
				? explanation
				: undefined;
			return { action: "append", title, ...(chosen === undefined ? {} : { explanation: chosen }) };
		}
		throw new OnlyneError("decision_unavailable", "The decision did not choose a todo action.");
	}

	async #judge(scope: ExtensionContext, state: unknown, questions: Questions, signal?: AbortSignal): Promise<Answers> {
		try {
			const { fitted, dropped, chars } = fitState(state as Row);
			if (chars > STATE_BUDGET_CHARS) {
				throw new OnlyneError("decision_unavailable", `The decision state needs ${String(chars)} characters; the judge prompt is capped near ${String(STATE_BUDGET_CHARS)}.`);
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
				`The judge role could not decide this Onlyne message: ${error instanceof Error ? error.message : String(error)}`,
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
		const op = stringField(meta, "op");
		if (op !== undefined && isThreadOp(op)) {
			const thread = await this.#thread(meta);
			const to = await this.#target(meta);
			const title = stringField(meta, "title");
			if (op === "create" && title === undefined) throw new OnlyneError("invalid_request", "thread create needs a title.");
			if (op !== "create" && op !== "list" && thread === undefined) {
				throw new OnlyneError("invalid_request", `thread ${op} needs a thread reference.`);
			}
			if ((op === "member_add" || op === "member_remove") && to === undefined) {
				throw new OnlyneError("invalid_request", `thread ${op} needs a target.`);
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
				throw new OnlyneError("invalid_request", "resource needs owner, id and operation.");
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
		if (scopeName !== undefined && isReadScope(scopeName)) {
			return this.#read(scope, scopeName);
		}

		return undefined;
	}

	async #read(scope: DecisionScope, name: ActionReadScope): Promise<Action> {
		const meta = scope.input.meta ?? {};
		const thread = await this.#thread(meta);
		const page_size = pageSize(meta);
		const cursor = stringField(meta, "cursor");
		const seq = stringField(meta, "seq");
		if (name === "directory") return { kind: "read", scope: name };
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
			throw new OnlyneError("invalid_request", `read ${name} needs a thread reference.`);
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
		const message = await this.#messageRef(meta, thread);
		if (message === undefined) {
			throw new OnlyneError("invalid_request", `read ${name} needs a message reference: meta.message {sender,id} or {thread,seq}.`);
		}
		return { kind: "read", scope: name, thread, message };
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
					instructions: "Choose what this message asks to read.",
					criteria: {
						peers: "The nodes that are online and reachable: 'who is online', 'which nodes can I reach', 'list the nodes', 'show the participants', the contact list, or the neighbour/adjacency view.",
						inbox: "The attention items waiting to be processed.",
						messages: "The message history inside one thread.",
						evidence: "This node's delivery and receipt evidence for one outbound message.",
						none: "The message asks to read none of these channels.",
					},
				},
			},
			scope.signal,
		);
		this.#record("outbound.o2b", scope.input, answers, Date.now() - started);
		const scopeName = choice(answers, "read_scope");
		if (scopeName === undefined || scopeName === "none") {
			throw new OnlyneError("decision_unavailable", "The decision named no read scope; the readable channels are peers, inbox, messages and evidence.");
		}
		// The design's names map onto the library's: `peers` is the directory listing. A caller
		// that names a library channel explicitly in meta.scope still gets it verbatim.
		if (scopeName !== "peers" && scopeName !== "inbox" && scopeName !== "messages" && scopeName !== "evidence") {
			throw new OnlyneError("decision_unavailable", `No readable channel exists for ${scopeName}; the readable channels are peers, inbox, messages and evidence.`);
		}
		const name: ActionReadScope = scopeName === "peers" ? "directory" : scopeName;
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
			const message = await this.#messageRef(meta, thread);
			if (message === undefined) {
				throw new OnlyneError("invalid_request", `read ${name} needs a message reference: meta.message {sender,id} or {thread,seq}.`);
			}
			return { kind: "read", scope: name, ...(thread === undefined ? {} : { thread }), message };
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
		if (threads.length === 0) throw new OnlyneError("not_found", "This node has no local thread to name.");
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
					instructions: "Choose the local thread the message asks about.",
					criteria: Object.fromEntries(threads.map(thread => [
						thread.thread_ref.id,
						`${thread.title} (${thread.thread_ref.owner}/${thread.thread_ref.id}, ${thread.state})`,
					])),
				},
			},
			scope.signal,
		);
		this.#record(node, scope.input, answers, Date.now() - started);
		const alias = choice(answers, "thread");
		if (alias === undefined || alias === "none") {
			throw new OnlyneError("decision_unavailable", "The decision did not choose a local thread.");
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
					instructions: "Choose the local thread operation the message asks for.",
					criteria: {
						create: "Create a new local thread.",
						close: "Close an existing local thread.",
						list: "List the local threads.",
						describe: "Describe one local thread.",
						member_add: "Add a member to a local thread.",
						member_remove: "Remove a member from a local thread.",
					},
				},
			},
			scope.signal,
		);
		this.#record("outbound.o2c", scope.input, answers, Date.now() - started);
		const op = choice(answers, "thread_op");
		if (op === undefined || !isThreadOp(op)) {
			throw new OnlyneError("decision_unavailable", "The judge did not choose a thread operation.");
		}
		// A dependent second call: which thread the operation names is only decidable once the
		// operation is known, and meta.thread settles it without a call (design §4, stepwise).
		const thread = op === "create" || op === "list"
			? await this.#thread(meta)
			: await this.#thread(meta) ?? await this.#threadQuestion(scope, "outbound.o2c-thread");
		const to = await this.#target(meta);
		const title = stringField(meta, "title");
		if ((op === "member_add" || op === "member_remove") && to === undefined) {
			throw new OnlyneError("decision_unavailable", `The judge chose thread ${op} without a target.`);
		}
		if (op === "create" && title === undefined) {
			throw new OnlyneError("invalid_request", "thread create needs a title.");
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
					instructions: "Choose the domain operation the message asks for.",
					criteria: {
						create: "Create this node's domain membership.",
						join: "Join a domain.",
						leave: "Leave the current domain.",
						list: "List this node's domain membership.",
						describe: "Describe this node's membership.",
						discover: "Discover a domain's declared members.",
					},
				},
			},
			scope.signal,
		);
		this.#record("outbound.o2d", scope.input, answers, Date.now() - started);
		const op = choice(answers, "domain_op");
		if (op === undefined || !isDomainOp(op)) {
			throw new OnlyneError("decision_unavailable", "The judge did not choose a domain operation.");
		}
		const domainId = stringField(meta, "domain_id");
		if (op !== "create" && op !== "list" && domainId === undefined) {
			throw new OnlyneError("invalid_request", `domain ${op} needs domain_id.`);
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
			throw new OnlyneError("invalid_request", "A resource action needs meta.resource {owner,id} and meta.operation.");
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
		throw new OnlyneError("invalid_request", "meta.thread must be a thread alias or {owner,id}.");
	}

	async #threadByAlias(alias: string): Promise<ThreadRef> {
		const threads = await this.deps.listThreads();
		const matches = threads.filter(thread => thread.thread_ref.id === alias || thread.title === alias);
		if (matches.length === 1) return matches[0]!.thread_ref;
		if (matches.length > 1) throw new OnlyneError("ambiguous_target", `More than one thread is named ${alias}.`);
		if (alias.includes("/")) {
			const [owner, id] = alias.split("/", 2);
			if (owner !== undefined && id !== undefined && owner.length > 0 && id.length > 0) return { owner, id };
		}
		throw new OnlyneError("not_found", `No local thread is named ${alias}.`);
	}

	async #target(meta: Record<string, Json> | undefined): Promise<NodeId | undefined> {
		if (meta === undefined) return undefined;
		const value = meta.to;
		if (value === undefined) return undefined;
		if (typeof value !== "string" || value.length === 0) throw new OnlyneError("invalid_request", "meta.to must be a non-empty string.");
		return this.#peerId(value);
	}

	/**
	 * Resolve an explicit target against live discovery. A bare alias is accepted
	 * only when exactly one known node's last URN segment names it.
	 */
	async #peerId(alias: string): Promise<NodeId> {
		const peers = await this.#peers();
		if (!peers.available) {
			throw new OnlyneError("unavailable", `The target cannot be resolved: ${peers.reason}`);
		}
		const ids = [this.deps.config.node.id, ...peers.value.map(peer => peer.node_id)];
		if (alias === "self") return this.deps.config.node.id;
		if (ids.includes(alias)) return alias;
		const matches = ids.filter(id => lastSegment(id) === alias);
		if (matches.length === 1) return matches[0]!;
		if (matches.length > 1) {
			throw new OnlyneError("ambiguous_target", `The alias ${alias} names more than one node: ${matches.join(", ")}.`);
		}
		throw new OnlyneError("not_found", `No discovered node is named ${alias}.`);
	}

	async #mentions(meta: Record<string, Json> | undefined): Promise<string[] | undefined> {
		if (meta === undefined) return undefined;
		const value = meta.mentions;
		if (value === undefined) return undefined;
		if (!Array.isArray(value)) throw new OnlyneError("invalid_request", "meta.mentions must be an array.");
		const resolved: string[] = [];
		for (const item of value) {
			if (typeof item !== "string" || item.length === 0) {
				throw new OnlyneError("invalid_request", "Each mention must be a non-empty string.");
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
			throw new OnlyneError("invalid_request", "meta.in_reply_to must be an object.");
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
			throw new OnlyneError("invalid_request", "meta.in_reply_to needs {thread,seq} or {sender,id}.");
		}
		if (seq === undefined) {
			throw new OnlyneError("invalid_request", "meta.in_reply_to.seq must be a string or a number.");
		}
		const thread = await this.#threadByAlias(threadAlias);
		const entry = await this.#entryAt(thread, seq);
		if (entry === undefined) throw new OnlyneError("not_found", `No message at ${threadAlias}/${seq}.`);
		return {
			ref: { sender: entry.message.sender, id: entry.message.id },
			authority: entry.reply_authority,
			thread_ref: entry.thread_ref,
		};
	}

	async #messageRef(meta: Record<string, Json>, thread?: ThreadRef): Promise<MessageRef | undefined> {
		const value = meta.message;
		const record = value !== null && typeof value === "object" && !Array.isArray(value)
			? (value as Record<string, Json>)
			: undefined;
		const sender = record === undefined ? undefined : stringField(record, "sender");
		const id = record === undefined ? undefined : stringField(record, "id");
		if (sender !== undefined && id !== undefined) return { sender, id };
		// meta.message takes the same two shapes as meta.in_reply_to, including its own thread
		// reference: `{thread, seq}` is the form a reader copies out of another reference, and
		// refusing it while accepting `{sender, id}` made the same fact two spellings (caerulea 4).
		const alias = record === undefined ? undefined : stringField(record, "thread");
		const scope = thread ?? (alias === undefined ? undefined : await this.#threadByAlias(alias));
		if (scope === undefined) return undefined;
		const seq = (record === undefined ? undefined : seqField(record, "seq")) ?? seqField(meta, "seq");
		if (seq === undefined) return undefined;
		const entry = await this.#entryAt(scope, seq);
		if (entry === undefined) throw new OnlyneError("not_found", `No message at ${scope.id}/${seq}.`);
		return { sender: entry.message.sender, id: entry.message.id };
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
				"A subagent session reads Onlyne state, it does not write it. Read your own slice through onlyne://threads, onlyne://inbox and onlyne://contexts, then return the result so the main agent can send it.",
			);
		}
		if (action.scope === "directory" || action.scope === "threads" || action.scope === "inbox") {
			throw new OnlyneError(
				"forbidden",
				"A subagent reads its own slice through onlyne://threads and onlyne://inbox; the whole node listing belongs to the main session.",
			);
		}
		const records = await this.#authorizedRecords(scope.ctx.agent.id);
		if (action.scope === "evidence") {
			const ref = action.message;
			const tied = ref !== undefined && records.some(record =>
				record.source !== undefined && record.source.sender === ref.sender && record.source.id === ref.id,
			);
			if (!tied) {
				throw new OnlyneError("forbidden", "A subagent may read outbound evidence only for a message its own context received.");
			}
			return;
		}
		if (action.thread === undefined) {
			throw new OnlyneError("forbidden", "A subagent may read only a thread its own context record authorizes.");
		}
		const key = threadKey(action.thread);
		if (!records.some(record => record.thread_ref !== undefined && threadKey(record.thread_ref) === key)) {
			throw new OnlyneError("forbidden", `This subagent holds no read slice for thread ${key}.`);
		}
	}

	async #authorizedRecords(agentId: string): Promise<ContextRecord[]> {
		let records: ContextRecord[];
		try {
			records = await this.deps.contexts.list();
		} catch (error) {
			throw new OnlyneError("unavailable", `The context registry could not be read: ${message(error)}`);
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
				const isMain = record.agent_id !== undefined && record.agent_id === record.main_agent_id;
				return {
					alias: record.alias,
					summary: record.summary,
					state: record.state,
					kind: isMain ? "main" : "subagent",
					can_answer: isMain,
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
			return none(`The context registry could not be read: ${message(error)}`);
		}
	}


	async #peers(): Promise<Source<PeerRecord[]>> {
		try {
			return ok(await this.deps.listPeers());
		} catch (error) {
			return none(`Discovery records could not be read: ${message(error)}`);
		}
	}

	async #receiverContexts(nodeId: NodeId): Promise<Source<ReceiverContext[]>> {
		try {
			return ok(await this.deps.listReceiverContexts(nodeId));
		} catch (error) {
			return none(`The target's published contexts could not be read: ${message(error)}`);
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
			if (data === undefined) return none("The newest todo-more snapshot has no readable data.");
			const phases = data.phases;
			if (!Array.isArray(phases)) return none("The newest todo-more snapshot has no phase list.");
			return normalizeTodo({ goal: data.goal ?? null, phases });
		}
		return { present: false, reason: "No todo-more snapshot is on the active branch." };
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
	return value === "directory" || value === "neighbors" || value === "threads" || value === "inbox" || value === "messages" || value === "message" || value === "evidence";
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
		throw new OnlyneError("invalid_request", "meta.page_size must be an integer from 1 to 100.");
	}
	return value;
}

function attention(meta: Record<string, Json>): "inbox" | "thread" {
	const value = meta.attention;
	if (value === undefined) return "thread";
	if (value === "inbox" || value === "thread") return value;
	throw new OnlyneError("invalid_request", "meta.attention must be inbox or thread.");
}

function relation(meta: Record<string, Json> | undefined): "reply" | "followup" | undefined {
	if (meta === undefined) return undefined;
	const value = meta.relation;
	if (value === undefined) return undefined;
	if (value === "reply" || value === "followup") return value;
	throw new OnlyneError("invalid_request", "meta.relation must be reply or followup.");
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
 * Whether the sender expects an answer (design §5.1): metadata says reply, or the body
 * greets or asks. A reply routed where no reply can be sent is the field reports' deadlock.
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
function boundText(text: string): string {
	return cut(text, TEXT_CHARS);
}

function cut(text: string, limit: number): string {
	return text.length <= limit ? text : `${text.slice(0, limit)}… [${String(text.length - limit)} more characters]`;
}

/** One branch message as trigger material: its text, cut to the trigger budget. */
function snippet(content: unknown): string | null {
	if (typeof content === "string") return cut(content, TRIGGER_CHARS);
	if (!Array.isArray(content)) return null;
	const text = content
		.map(part => asRecord(part))
		.filter(part => typeof part?.text === "string")
		.map(part => String(part!.text))
		.join("\n");
	return text.length === 0 ? null : cut(text, TRIGGER_CHARS);
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
				bytes: "omitted: image bytes are not decision input",
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
	if (current === undefined) return none("No todo snapshot was provided.");
	const row = asRecord(current);
	if (row === undefined) return none("The todo snapshot is not an object.");
	if (row.present === false) {
		return { present: false, reason: typeof row.reason === "string" ? row.reason : "The todo source is absent." };
	}
	if (row.kind === "empty") return ok({ goal: null, phases: [] });
	const phases = row.phases;
	if (!Array.isArray(phases)) return none("The todo snapshot has no phase list.");
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
		if (name === undefined || !Array.isArray(items)) return none("A todo phase has no readable name or item list.");
		const rows: TodoView["phases"][number]["items"] = [];
		for (const item of items) {
			const itemRow = asRecord(item);
			const title = typeof itemRow?.title === "string" ? itemRow.title : undefined;
			if (title === undefined) return none("A todo item has no readable title.");
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
