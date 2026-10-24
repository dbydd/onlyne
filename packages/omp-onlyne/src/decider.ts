import { Settings } from "@oh-my-pi/pi-coding-agent/config/settings";
import type { ExtensionContext } from "@oh-my-pi/pi-coding-agent";
import { journalJudgmentUsage, resolveJudge } from "@oh-my-pi/pi-coding-agent/judgment";
import { AgentRegistry } from "@oh-my-pi/pi-coding-agent/registry/agent-registry";
import type { SessionEntry } from "@oh-my-pi/pi-coding-agent/session/session-entries";
import { getContextNotes } from "@oh-my-pi/pi-coding-agent/session/context-notes";
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
	action: "fragment" | "wake" | "queue" | "context" | "new_subagent" | "wait_result";
	context?: string;
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

/** Decisions go through OMP's `judge` role; the plugin never picks a target or a plan itself. */
export class JudgeDecider {
	constructor(private readonly deps: DeciderDeps) {}

	async decide(scope: DecisionScope): Promise<Action> {
		const explicit = await this.#explicit(scope);
		if (explicit !== undefined) {
			await this.#guard(scope, explicit);
			return explicit;
		}

		const state = await this.#state(scope);
		const questions = this.#outboundQuestions(state);
		const answers = await this.#judge(scope.ctx, state, questions, scope.signal);
		if (scope.readOnly === true && choice(answers, "action") !== "read") {
			throw new OnlyneError("forbidden", "Subagent sessions may only read Onlyne state; return the result to the main agent.");
		}
		const action = await this.#action(scope, answers);
		await this.#guard(scope, action);
		return action;
	}

	async decideInbound(scope: ExtensionContext, entry: Entry): Promise<InboundDecision> {
		const base = this.#base(scope);
		const state: Row = {
			...base,
			incoming: entryFacts(entry),
			contexts: await this.#contextRows(),
			threads: await this.#threadRows(),
			thread_dialogue: await this.#threadEvidenceFor([entry.thread_ref]),
		};
		const questions = this.#inboundQuestions(state);
		const answers = await this.#judge(scope, state, questions);
		const action = choice(answers, "action");
		if (action === "fragment" || action === "wake" || action === "queue" || action === "new_subagent" || action === "wait_result") {
			return { action };
		}
		if (action !== "context") {
			throw new OnlyneError("decision_unavailable", "The decision did not choose a valid inbound action.");
		}
		const alias = choice(answers, "context");
		if (alias === undefined || alias === "none") {
			throw new OnlyneError("decision_unavailable", "The decision chose a context without naming one.");
		}
		const rows = valueOf<Row[]>(state.contexts) ?? [];
		if (!rows.some(row => row.alias === alias)) {
			throw new OnlyneError("decision_unavailable", `The decision named context ${alias}, which is not registered.`);
		}
		return { action: "context", context: alias };
	}

	async decideTodo(ctx: ExtensionContext, entry: Entry, current: unknown): Promise<TodoDecision> {
		const snapshot = current === undefined || current === null ? this.#todo(ctx) : normalizeTodo(current);
		const candidates = todoCandidates(entry);
		const existing = todoTitles(snapshot);
		const base = this.#base(ctx);
		const state: Row = {
			...base,
			todo: snapshot,
			incoming: entryFacts(entry),
			title_candidates: candidates.titles,
			explanation_candidates: candidates.explanations,
			thread_dialogue: await this.#threadEvidenceFor([entry.thread_ref]),
		};
		const answers = await this.#judge(ctx, state, todoQuestions(existing, candidates));
		const action = choice(answers, "action");
		if (action === "none") return { action: "none" };
		if (action === "associate") {
			const title = choice(answers, "task");
			if (title === undefined || title === "none" || !existing.includes(title)) {
				throw new OnlyneError("decision_unavailable", "The decision chose to associate without naming an existing todo item.");
			}
			return { action: "associate", title };
		}
		if (action === "append") {
			const title = choice(answers, "title");
			if (title === undefined || title === "none" || !candidates.titles.includes(title)) {
				throw new OnlyneError("decision_unavailable", "The decision chose to append without naming one of the message's title candidates.");
			}
			const explanation = choice(answers, "explanation");
			const chosen = explanation !== undefined && explanation !== "none" && candidates.explanations.includes(explanation)
				? explanation
				: undefined;
			return { action: "append", title, ...(chosen === undefined ? {} : { explanation: chosen }) };
		}
		throw new OnlyneError("decision_unavailable", "The decision did not choose a todo action.");
	}

	async #judge(scope: ExtensionContext, state: unknown, questions: Questions, signal?: AbortSignal): Promise<Answers> {
		try {
			const settings = (await Settings.current) ?? (await Settings.loadReadOnly({ cwd: scope.cwd }));
			const judge = resolveJudge({
				settings,
				registry: scope.modelRegistry,
				sessionModel: scope.model,
				sessionId: scope.sessionManager.getSessionId(),
				purpose: "onlyne",
				onUsage: journalJudgmentUsage(scope.sessionManager),
			});
			const result = await judge.judge({ state: jsonState(state), questions }, signal === undefined ? {} : { signal });
			return result.answers as Answers;
		} catch (error) {
			throw new OnlyneError(
				"decision_unavailable",
				`The judge role could not decide this Onlyne message: ${error instanceof Error ? error.message : String(error)}`,
			);
		}
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
		if (message === undefined) throw new OnlyneError("invalid_request", `read ${name} needs a message reference.`);
		return { kind: "read", scope: name, thread, message };
	}

	async #action(scope: DecisionScope, answers: Answers): Promise<Action> {
		const kind = choice(answers, "action");
		if (kind === "read") {
			const name = choice(answers, "read_scope");
			if (name === undefined || name === "none" || !isReadScope(name)) {
				throw new OnlyneError("decision_unavailable", "The judge did not choose a readable scope.");
			}
			const thread = await this.#answerThread(answers, scope.input.meta);
			if (name === "messages" && thread === undefined) {
				throw new OnlyneError("decision_unavailable", "The judge chose messages without a thread.");
			}
			const meta = scope.input.meta ?? {};
			const message = name === "message" || name === "evidence"
				? await this.#messageRef(meta, thread)
				: undefined;
			if ((name === "message" || name === "evidence") && message === undefined) {
				throw new OnlyneError("decision_unavailable", `The judge chose ${name} without a message reference.`);
			}
			return {
				kind: "read",
				scope: name,
				...(thread === undefined ? {} : { thread }),
				...(message === undefined ? {} : { message }),
				...(stringField(meta, "seq") === undefined ? {} : { seq: stringField(meta, "seq") }),
				...(stringField(meta, "cursor") === undefined ? {} : { cursor: stringField(meta, "cursor") }),
				...(pageSize(meta) === undefined ? {} : { page_size: pageSize(meta) }),
				attention: attention(meta),
			};
		}
		if (kind === "thread") {
			const op = choice(answers, "thread_op");
			if (op === undefined || op === "none" || !isThreadOp(op)) {
				throw new OnlyneError("decision_unavailable", "The judge did not choose a thread operation.");
			}
			const thread = await this.#answerThread(answers, scope.input.meta);
			if (op !== "create" && op !== "list" && thread === undefined) {
				throw new OnlyneError("decision_unavailable", `The judge chose thread ${op} without a thread.`);
			}
			const to = await this.#target(scope.input.meta);
			const title = stringField(scope.input.meta ?? {}, "title");
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
		if (kind === "domain") {
			const op = choice(answers, "domain_op");
			if (op === undefined || op === "none" || !isDomainOp(op)) {
				throw new OnlyneError("decision_unavailable", "The judge did not choose a domain operation.");
			}
			const domainId = stringField(scope.input.meta ?? {}, "domain_id");
			if (op !== "create" && op !== "list" && domainId === undefined) {
				throw new OnlyneError("invalid_request", `domain ${op} needs domain_id.`);
			}
			return {
				kind: "domain",
				op,
				...(domainId === undefined ? {} : { domain_id: domainId }),
				...(typeof scope.input.meta?.expected_revision === "string"
					? { expected_revision: scope.input.meta.expected_revision }
					: {}),
			};
		}
		if (kind === "resource") {
			const meta = scope.input.meta ?? {};
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
		if (kind !== "send") {
			throw new OnlyneError("decision_unavailable", "The judge did not choose an executable action.");
		}

		const meta = scope.input.meta ?? {};
		const reply = await this.#reply(meta);
		const explicitRelation = relation(meta);
		const relationChoice = choice(answers, "relation");
		let to: NodeId;
		let resolvedRelation: "reply" | "followup" | undefined;
		if (reply !== undefined) {
			resolvedRelation = explicitRelation
				?? (relationChoice === "reply" || relationChoice === "followup" ? relationChoice : undefined);
			if (resolvedRelation === undefined) {
				throw new OnlyneError("invalid_request", "A reply needs relation reply or followup; the decision did not choose one.");
			}
			const explicitTarget = await this.#target(meta);
			if (explicitTarget !== undefined && explicitTarget !== reply.authority) {
				throw new OnlyneError(
					"invalid_request",
					`The reply must go to its original reply authority ${reply.authority}, not ${explicitTarget}.`,
				);
			}
			const chosen = await this.#targetChoice(answers);
			if (chosen !== undefined && chosen !== reply.authority) {
				throw new OnlyneError(
					"invalid_request",
					`The decision named ${chosen}, but the reply belongs to its original reply authority ${reply.authority}.`,
				);
			}
			to = reply.authority;
		} else {
			if (explicitRelation !== undefined || relationChoice === "reply" || relationChoice === "followup") {
				throw new OnlyneError("invalid_request", "A relation needs an in_reply_to reference.");
			}
			const explicitTarget = await this.#target(meta);
			to = explicitTarget ?? (await this.#targetChoice(answers)) ?? "";
			if (to.length === 0) {
				throw new OnlyneError("ambiguous_target", "The decision did not choose a delivery target.");
			}
		}

		const response = choice(answers, "response");
		const execution = choice(answers, "execution");
		if (response !== "none" && response !== "async" && response !== "await") {
			throw new OnlyneError("decision_unavailable", "The judge did not choose a valid response mode.");
		}
		if (execution !== "existing" && execution !== "new") {
			throw new OnlyneError("decision_unavailable", "The judge did not choose a valid execution mode.");
		}

		const thread = await this.#answerThread(answers, meta);
		const context = execution === "new" ? undefined : await this.#existingContext(scope, to, meta);
		const mentions = await this.#mentions(meta);
		return {
			kind: "send",
			to,
			...(thread === undefined ? {} : { thread }),
			...(context === undefined ? {} : { context }),
			...(reply === undefined ? {} : { in_reply_to: reply.ref }),
			...(resolvedRelation === undefined ? {} : { relation: resolvedRelation }),
			...(mentions === undefined ? {} : { mentions }),
			response,
			execution,
		};
	}

	/**
	 * `execution: existing` names a context the target publishes. The sender never
	 * allocates a receiver alias and never validates against its own registry.
	 */
	async #existingContext(scope: DecisionScope, to: NodeId, meta: Record<string, Json>): Promise<string> {
		const receiver = await this.#receiverContexts(to);
		const explicit = stringField(meta, "context");
		if (!receiver.available) {
			if (explicit !== undefined) return explicit;
			throw new OnlyneError(
				"decision_unavailable",
				`The target's published contexts could not be read: ${receiver.reason}`,
			);
		}
		const aliases = receiver.value.map(context => context.alias);
		if (explicit !== undefined) {
			if (!aliases.includes(explicit)) {
				throw new OnlyneError("invalid_request", `The target does not publish an execution context named ${explicit}.`);
			}
			return explicit;
		}
		if (receiver.value.length === 0) {
			throw new OnlyneError("ambiguous_target", "execution existing needs one of the target's published contexts, but it publishes none.");
		}
		const state: Row = {
			...this.#base(scope.ctx),
			request: { delivery_kind: scope.input.delivery_kind, msg: scope.input.msg, meta },
			target: { node_id: to },
			receiver_contexts: receiver.value,
		};
		const criteria: Record<string, string> = {};
		for (const context of receiver.value) criteria[context.alias] = `${context.summary} (${context.state})`;
		criteria.none = "The target does not continue any of its published contexts.";
		const answers = await this.#judge(
			scope.ctx,
			state,
			{
				context: {
					type: "choice",
					instructions: "Choose the execution context the target should continue in, or none when this message does not continue one.",
					criteria,
				},
			},
			scope.signal,
		);
		const alias = choice(answers, "context");
		if (alias === undefined || alias === "none") {
			throw new OnlyneError("ambiguous_target", "The decision did not choose one of the target's execution contexts.");
		}
		if (!aliases.includes(alias)) {
			throw new OnlyneError("decision_unavailable", `The decision named context ${alias}, which the target does not publish.`);
		}
		return alias;
	}

	#base(ctx: ExtensionContext): Row {
		const history = this.#history(ctx);
		return {
			node: this.deps.config.node.id,
			region: this.deps.config.region,
			host: hostFacts(ctx),
			plan: this.#plan(ctx),
			context_note: this.#contextNote(ctx),
			todo: this.#todo(ctx),
			history: ok(history),
			task_dispatches: ok(dispatches(history)),
			children: this.#children(),
		};
	}

	async #state(scope: DecisionScope): Promise<Row> {
		const meta = scope.input.meta ?? {};
		const explicitTarget = await this.#target(meta);
		const threads = await this.#threadRows();
		const contexts = await this.#contextRows();
		const referenced: ThreadRef[] = [];
		const explicitThread = await this.#thread(meta);
		if (explicitThread !== undefined) referenced.push(explicitThread);
		const reply = await this.#reply(meta).catch(() => undefined);
		if (reply?.thread_ref !== undefined) referenced.push(reply.thread_ref);
		for (const record of valueOf<Row[]>(contexts) ?? []) {
			const ref = record.thread_ref as ThreadRef | null;
			if (ref !== null) referenced.push(ref);
		}
		return {
			...this.#base(scope.ctx),
			request: { delivery_kind: scope.input.delivery_kind, msg: scope.input.msg, meta },
			peers: await this.#peers(),
			threads,
			thread_dialogue: await this.#threadEvidenceFor(referenced),
			receiver_contexts: explicitTarget === undefined
				? none("The target is chosen by this decision; the target's contexts are requested once it is known.")
				: await this.#receiverContexts(explicitTarget),
		};
	}

	#outboundQuestions(state: Row): Questions {
		const peers = valueOf<PeerRecord[]>(state.peers) ?? [];
		const threads = valueOf<Row[]>(state.threads) ?? [];
		const targetCriteria: Record<string, string> = {
			self: "This node's own threads and execution contexts.",
		};
		for (const peer of peers) {
			targetCriteria[peer.node_id] =
				`Discovered peer ${peer.node_id} at ${peer.base_url} (instance ${peer.instance_id}, last seen ${peer.last_seen}).`;
		}
		targetCriteria.none = "No peer target.";
		const questions: Questions = {
			action: {
				type: "choice",
				instructions:
					"Choose the one action the model's message asks for. send: deliver a message to a peer. read: inspect a directory, thread, inbox, message or outbound evidence. thread: create, close, list, describe or change members of a local thread. resource: operate a resource the message names explicitly. domain: create, join, leave, list, describe or discover this node's domain.",
				criteria: {
					send: "The message is meant for another agent.",
					read: "The message asks to inspect local Onlyne state.",
					thread: "The message manages a local thread.",
					resource: "The message operates an explicitly named resource.",
					domain: "The message manages this node's domain membership.",
				},
			},
			response: {
				type: "choice",
				instructions: "Does the sender wait for a reply? none: no reply expected. async: delivery only. await: hold this call until a legal reply arrives.",
				criteria: { none: "No reply is expected.", async: "Delivery only; a later reply may arrive.", await: "The call should wait for the reply." },
			},
			execution: {
				type: "choice",
				instructions: "Should the receiver handle this in one of its published execution contexts or in a new one?",
				criteria: {
					existing: "The message belongs to work already running in one of the target's published contexts.",
					new: "The message starts independent work and needs a new context the receiver creates.",
				},
			},
			relation: {
				type: "choice",
				instructions: "If the message references an earlier message, is it a final reply or a progress supplement? Choose none when there is no reference.",
				criteria: {
					reply: "A final answer that may release the original sender's wait.",
					followup: "A supplement or progress note that does not release a wait.",
					none: "The message does not reference an earlier message.",
				},
			},
			target: {
				type: "choice",
				instructions: "Choose the node that should receive a send action, or none when this is not a send.",
				criteria: targetCriteria,
			},
		};
		if (threads.length > 0) {
			const threadCriteria: Record<string, string> = {};
			for (const thread of threads) {
				const ref = thread.ref as ThreadRef;
				threadCriteria[ref.id] = `${String(thread.title)} (${ref.owner}/${ref.id}, ${String(thread.state)})`;
			}
			threadCriteria.none = "No local thread.";
			questions.thread = {
				type: "choice",
				instructions: "Choose the local thread a read or thread action names, or none.",
				criteria: threadCriteria,
			};
		}
		questions.read_scope = {
			type: "choice",
			instructions: "If action is read, choose what to read.",
			criteria: {
				directory: "Peers, resources and capabilities.",
				threads: "This node's threads.",
				inbox: "Attention items.",
				messages: "A thread's message list.",
				message: "One message by reference.",
				evidence: "This node's outbound evidence for a message.",
				none: "Not a read.",
			},
		};
		questions.thread_op = {
			type: "choice",
			instructions: "If action is thread, choose the operation.",
			criteria: {
				create: "Create a local thread.",
				close: "Close a local thread.",
				list: "List local threads.",
				describe: "Describe one local thread.",
				member_add: "Add a member to a local thread.",
				member_remove: "Remove a member from a local thread.",
				none: "Not a thread action.",
			},
		};
		questions.domain_op = {
			type: "choice",
			instructions: "If action is domain, choose the operation.",
			criteria: {
				create: "Create this node's domain membership.",
				join: "Join a domain.",
				leave: "Leave the current domain.",
				list: "List this node's domain membership.",
				describe: "Describe this node's membership.",
				discover: "Discover a domain's declared members.",
				none: "Not a domain action.",
			},
		};
		return questions;
	}

	#inboundQuestions(state: Row): Questions {
		const contexts = valueOf<Row[]>(state.contexts) ?? [];
		const questions: Questions = {
			action: {
				type: "choice",
				instructions:
					"Decide how this inbound Onlyne message should be handled in this OMP session. fragment: add it to the next model context without starting a turn. wake: add it and start a turn now. queue: keep it pending because the session must not handle it yet. context: deliver it into one of the registered execution contexts. new_subagent: start independent new work in a fresh subagent. wait_result: this message is the reply an in-flight await is waiting for.",
				criteria: {
					fragment: "The message belongs in the main session's next context but does not need a turn now.",
					wake: "The message should start work in the main session now.",
					queue: "The session is busy, in plan mode, or otherwise must not receive the message yet.",
					context: "The message is a supplement or continuation for one registered execution context.",
					new_subagent: "The message asks for fresh, independent work that no registered context is already doing.",
					wait_result: "The message answers a message this session is currently awaiting.",
				},
			},
		};
		if (contexts.length > 0) {
			const contextCriteria: Record<string, string> = {};
			for (const context of contexts) {
				contextCriteria[String(context.alias)] =
					`${String(context.summary)} (${String(context.state)}${context.agent_id === null || context.agent_id === undefined ? ", unbound: the host must bind it before delivery" : `, agent ${String(context.agent_id)}`})`;
			}
			contextCriteria.none = "Do not deliver this message into a registered context.";
			questions.context = {
				type: "choice",
				instructions:
					"Choose the execution context that should receive this message when action is context. Choose none otherwise.",
				criteria: contextCriteria,
			};
		}
		return questions;
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

	async #answerThread(answers: Answers, meta: Record<string, Json> | undefined): Promise<ThreadRef | undefined> {
		const explicit = meta === undefined ? undefined : await this.#thread(meta);
		if (explicit !== undefined) return explicit;
		const alias = choice(answers, "thread");
		if (alias === undefined || alias === "none") return undefined;
		return this.#threadByAlias(alias);
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

	async #targetChoice(answers: Answers): Promise<NodeId | undefined> {
		const alias = choice(answers, "target");
		if (alias === undefined || alias === "none") return undefined;
		if (alias === "self") return this.deps.config.node.id;
		const peers = await this.#peers();
		if (!peers.available) {
			throw new OnlyneError("decision_unavailable", `The target cannot be resolved: ${peers.reason}`);
		}
		if (peers.value.some(peer => peer.node_id === alias)) return alias;
		throw new OnlyneError("decision_unavailable", `The decision named ${alias}, which discovery does not publish.`);
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
		if (sender !== undefined && id !== undefined) return { ref: { sender, id }, authority: sender };
		const threadAlias = stringField(record, "thread");
		const seq = stringField(record, "seq");
		if (threadAlias === undefined || seq === undefined) {
			throw new OnlyneError("invalid_request", "meta.in_reply_to needs {thread,seq} or {sender,id}.");
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
		if (thread === undefined) return undefined;
		const seq = (record === undefined ? undefined : stringField(record, "seq")) ?? stringField(meta, "seq");
		if (seq === undefined) return undefined;
		const entry = await this.#entryAt(thread, seq);
		return entry === undefined ? undefined : { sender: entry.message.sender, id: entry.message.id };
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
			throw new OnlyneError("forbidden", "Subagent sessions may only read Onlyne state; return the result to the main agent.");
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
			return ok(records.map(record => ({
				alias: record.alias,
				summary: record.summary,
				state: record.state,
				agent_id: record.agent_id ?? null,
				main_agent_id: record.main_agent_id,
				node_id: record.node_id,
				thread_ref: record.thread_ref ?? null,
				source: record.source ?? null,
				created_at: record.created_at,
				bound_at: record.bound_at ?? null,
			})));
		} catch (error) {
			return none(`The context registry could not be read: ${message(error)}`);
		}
	}

	async #threadRows(): Promise<Source<Row[]>> {
		try {
			const threads = await this.deps.listThreads();
			return ok(threads.map(thread => ({
				ref: thread.thread_ref,
				title: thread.title,
				state: thread.state,
				next_seq: thread.next_seq,
			})));
		} catch (error) {
			return none(`The thread list could not be read: ${message(error)}`);
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

	/** The active branch as ordered, id-bearing rows; no window and no text cut. */
	#history(ctx: ExtensionContext): Row[] {
		const rows: Row[] = [];
		for (const entry of branchOf(ctx)) {
			const row = entryRow(entry);
			if (row !== undefined) rows.push(row);
		}
		return rows;
	}

	#plan(ctx: ExtensionContext): unknown {
		const agentId = ctx.agent?.id;
		if (agentId === undefined) return none("The calling agent identity is unavailable.");
		const ref = AgentRegistry.global().get(agentId);
		if (ref === undefined) return none(`Agent ${agentId} is not registered in this process.`);
		const session = ref.session;
		if (session === null) return none(`The session for ${agentId} is not live.`);
		try {
			return ok(session.getPlanModeState?.() ?? null);
		} catch (error) {
			return none(`Plan state could not be read: ${message(error)}`);
		}
	}

	#contextNote(ctx: ExtensionContext): unknown {
		const branch = branchOf(ctx);
		try {
			const notes = getContextNotes(branch);
			if (notes !== undefined) return { present: true, entry_id: notes.entryId, text: notes.text };
			const reset = [...branch].reverse().map(entry => asRecord(entry)).find(row => row?.type === "reset_boundary");
			return reset === undefined
				? { present: false }
				: { present: false, invalidated_by_reset: typeof reset.id === "string" ? reset.id : true };
		} catch (error) {
			return none(`Context notes could not be read: ${message(error)}`);
		}
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

	#children(): unknown {
		try {
			const refs = AgentRegistry.global().list().filter(ref => ref.kind === "sub" && ref.parentId === this.deps.mainAgentId);
			return ok(refs.map(ref => ({
				agent_id: ref.id,
				display_name: ref.displayName,
				status: ref.status,
				parent_id: ref.parentId ?? null,
				session_file: ref.sessionFile ?? null,
				activity: ref.activity ?? null,
				last_activity: ref.lastActivity,
				created_at: ref.createdAt,
			})));
		} catch (error) {
			return none(`The agent registry could not be read: ${message(error)}`);
		}
	}
}

type ActionReadScope = Extract<Action, { kind: "read" }>["scope"];

function isReadScope(value: string): value is ActionReadScope {
	return value === "directory" || value === "threads" || value === "inbox" || value === "messages" || value === "message" || value === "evidence";
}

function isThreadOp(value: string): value is Extract<Action, { kind: "thread" }>["op"] {
	return value === "create" || value === "close" || value === "list" || value === "describe" || value === "member_add" || value === "member_remove";
}

function isDomainOp(value: string): value is Extract<Action, { kind: "domain" }>["op"] {
	return value === "create" || value === "join" || value === "leave" || value === "list" || value === "describe" || value === "discover";
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

/** A node id's last URN segment; plain ids are their own segment. */
function lastSegment(nodeId: NodeId): string {
	const trimmed = nodeId.replace(/\/+$/, "");
	const parts = trimmed.split(/[:/]/).filter(part => part.length > 0);
	return parts.at(-1) ?? trimmed;
}

function hostFacts(ctx: ExtensionContext): Row {
	return {
		agent: {
			id: ctx.agent?.id ?? null,
			kind: ctx.agent?.kind ?? null,
			name: ctx.agent?.name ?? null,
			depth: ctx.agent?.depth ?? null,
			parent_id: ctx.agent?.parentId ?? null,
		},
		idle: ctx.isIdle(),
		pending_messages: ctx.hasPendingMessages(),
		run_mode: ctx.mode,
		live_waits: none("The library keeps live waiters in process state and does not expose them."),
	};
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

/** Content parts keep their text; image bytes are not decision material and are named, not embedded. */
function contentOf(value: unknown): unknown {
	if (typeof value === "string") return value;
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
		return row;
	});
}

/** Task tool calls paired with the results that reported real agent ids. */
function dispatches(history: Row[]): Row[] {
	const calls = new Map<string, Row>();
	const out: Row[] = [];
	for (const row of history) {
		if (row.type !== "message") continue;
		const message = asRecord(row.message);
		if (message === undefined) continue;
		if (message.role === "assistant" && Array.isArray(message.content)) {
			for (const part of message.content) {
				const call = asRecord(part);
				if (call?.type !== "toolCall" || call.name !== "task" || typeof call.id !== "string") continue;
				const record: Row = { call_id: call.id, arguments: call.arguments ?? null };
				calls.set(call.id, record);
				out.push(record);
			}
		}
		if (message.role === "toolResult" && message.tool_name === "task" && typeof message.tool_call_id === "string") {
			const record = calls.get(message.tool_call_id);
			if (record !== undefined) {
				record.result = message.content ?? null;
				record.is_error = message.is_error ?? false;
			} else {
				out.push({ call_id: message.tool_call_id, result: message.content ?? null, is_error: message.is_error ?? false });
			}
		}
	}
	return out;
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

function todoQuestions(existing: string[], candidates: { titles: string[]; explanations: string[] }): Questions {
	const actionCriteria: Record<string, string> = {
		append: "The message needs a new persistent work item that no existing item covers.",
		none: "The message needs no persistent work item: it is informational, duplicate, or already fully covered.",
	};
	if (existing.length > 0) {
		actionCriteria.associate = "The message continues or supplements an existing item.";
	}
	const questions: Questions = {
		action: {
			type: "choice",
			instructions: "Decide what this message should do to the session's todo list.",
			criteria: actionCriteria,
		},
	};
	if (existing.length > 0) {
		const taskCriteria: Record<string, string> = {};
		for (const title of existing) taskCriteria[title] = "An existing todo item.";
		taskCriteria.none = "No existing item.";
		questions.task = {
			type: "choice",
			instructions: "If action is associate, choose the existing item this message continues; choose none otherwise.",
			criteria: taskCriteria,
		};
	}
	if (candidates.titles.length > 0) {
		const titleCriteria: Record<string, string> = {};
		for (const title of candidates.titles) titleCriteria[title] = "A title taken from the message.";
		titleCriteria.none = "No new item.";
		questions.title = {
			type: "choice",
			instructions: "If action is append, choose the title for the new item from the message's own text; choose none otherwise.",
			criteria: titleCriteria,
		};
	}
	if (candidates.explanations.length > 0) {
		const explanationCriteria: Record<string, string> = {};
		for (const text of candidates.explanations) explanationCriteria[text] = "An explanation taken from the message.";
		explanationCriteria.none = "Leave the explanation unwritten.";
		questions.explanation = {
			type: "choice",
			instructions: "If action is append, choose the explanation for the new item from the message's own text; choose none to leave it unexplained.",
			criteria: explanationCriteria,
		};
	}
	return questions;
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
