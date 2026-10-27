import { OnlyneError, type Entry, type MessageRef, type ThreadRef } from "@onlyne/lib";
import type { AgentSession, ExtensionAPI, ExtensionContext, SessionEntry } from "@oh-my-pi/pi-coding-agent";
import { AgentRegistry } from "@oh-my-pi/pi-coding-agent/registry/agent-registry";
import { USER_TODO_EDIT_CUSTOM_TYPE } from "@oh-my-pi/pi-coding-agent/tools/todo";
import {
	TODO_MORE_ENTRY,
	TODO_MORE_RESULT_MARK,
	nativePhases,
	nativeTitles,
	readNativeState,
	recordField,
	stringField,
	type HostTodoPhase,
} from "./todo-native.ts";
import { bridgeText as t } from "./bridge-text.ts";

export type TodoStatus = "pending" | "in_progress" | "completed" | "abandoned" | "blocked";

/** What the work is, how its result is measured, and how far it must go to count as done. */
export interface TodoStateGoal {
	objective: string;
	measure: string;
	doneWhen: string;
}

export interface TodoStateItem {
	title: string;
	status: TodoStatus;
	/** Present when the branch recorded one; the native host list has no such field. */
	explanation?: string;
	blocker?: string;
	/** Native-only item fields; carried through so a rewrite does not drop them. */
	details?: string;
	notes?: string[];
}

export interface TodoStatePhase {
	name: string;
	items: TodoStateItem[];
}

/**
 * What the planner sees, whichever store backs the list. `goal` is null for a
 * native OMP list: `user_todo_edit` carries phases only.
 */
export interface TodoState {
	goal: TodoStateGoal | null;
	phases: TodoStatePhase[];
}

/**
 * One inbound message's effect on the todo list, as decided by SystemOne.
 * `append` carries the new item; `associate` names an existing item; `none`
 * records that the message was considered and needs no todo change.
 */
export interface TodoDecision {
	action: "none" | "associate" | "append";
	/** append: target phase, default "Tasks"; associate: optional disambiguator. */
	phase?: string;
	/** append: the new item's title; associate: the existing item's title. */
	title?: string;
	/** append: the explanation body after the fixed Source line. */
	explanation?: string;
	/** append into an empty list: the goal the list needs. */
	goal?: { objective: string; measure: string; doneWhen: string };
	/** Model-owned estimate; absent leaves the item unestimated instead of inventing one. */
	minutes?: number;
}

/** The decision surface `applyMessageTodo` consumes. JudgeDecider wiring supplies it. */
export interface TodoPlanner {
	decideTodo(entry: Entry, current: TodoState, context: string): Promise<TodoDecision>;
}

/**
 * A durable item identity. Onlyne records it in branch metadata; the host todo
 * list keeps titles only, so this roster is the only thing that can tell a
 * surviving item from a deleted-then-recreated title.
 */
export interface OnlyneTodoOrigin {
	item_id: string;
	phase: string;
	title: string;
}

export interface OnlyneTodoLink {
	/** Stable association identity; reused by replays and retries. */
	link_id: string;
	message_ref: MessageRef;
	thread_ref: ThreadRef;
	context: string;
	seq: string;
	item_id: string;
	/** Display/back-reference snapshot, never the association key. */
	phase: string;
	title: string;
	at: string;
}

export interface OnlyneTodoDecisionRecord {
	/** context + message identity; the idempotence key. */
	key: string;
	context: string;
	message_ref: MessageRef;
	thread_ref: ThreadRef;
	seq: string;
	action: TodoDecision["action"];
	link_id?: string;
	/** Native fallback: the item explanation the host list cannot carry. */
	explanation?: string;
	at: string;
}

export interface OnlyneTodoMetadata {
	version: 1;
	commit_id: string;
	/** The decision this commit records, when a message caused it. */
	decision?: OnlyneTodoDecisionRecord;
	/** Full origin roster after this commit. */
	origins: OnlyneTodoOrigin[];
	/** Associations established by this commit. Replay accumulates them. */
	links: OnlyneTodoLink[];
	/** Native fallback only: goal fields the host list cannot carry. */
	goal_intent?: TodoStateGoal;
}

/** The branch-derived view the viewer reads; no SQLite index is involved. */
export interface OnlyneTodoBranch {
	state: TodoState;
	origins: readonly OnlyneTodoOrigin[];
	links: readonly OnlyneTodoLink[];
	/** Links whose item origin still exists under the current state. */
	activeLinks: readonly OnlyneTodoLink[];
	decisions: ReadonlyMap<string, OnlyneTodoDecisionRecord>;
}

/**
 * Native fallback's branch record: the decision and its associations, with no
 * rich snapshot. It is the idempotence marker that keeps a message from being
 * judged twice when omp-todo-more is not installed.
 */
export const ONLYNE_TODO_DECISION = "onlyne.todo-decision";

const TODO_MORE_PACKAGE = "omp-todo-more";
const TODO_MORE_STATE_MODULE = `${TODO_MORE_PACKAGE}/src/state.ts`;

type RichGoalState = { readonly kind: "set"; readonly goal: TodoStateGoal } | { readonly kind: "missing" };
type RichExplanation = { readonly kind: "explained"; readonly text: string } | { readonly kind: "missing" };
type RichTime =
	| { readonly kind: "missing" }
	| {
			readonly kind: "estimated";
			readonly budgetId: string;
			readonly minutes: number;
			readonly elapsedMs: number;
			readonly runningSince: number | null;
	  };

interface RichItem {
	readonly title: string;
	readonly explanation: RichExplanation;
	readonly status: TodoStatus;
	readonly blocker?: string;
	readonly time: RichTime;
}

interface RichPhase {
	readonly name: string;
	readonly items: readonly RichItem[];
}

type RichState =
	| { readonly kind: "empty" }
	| { readonly kind: "list"; readonly goal: RichGoalState; readonly phases: readonly RichPhase[] };

interface RichSnapshot {
	goal: TodoStateGoal | null;
	phases: Array<{
		name: string;
		items: Array<{
			title: string;
			explanation: string | null;
			status: TodoStatus;
			blocker?: string;
			time: RichTime;
		}>;
	}>;
}

/** The subset of `omp-todo-more/src/state.ts` the rich adapter drives. */
interface TodoMoreStateModule {
	readonly TODO_MORE_ENTRY_TYPE: string;
	readonly TODO_MORE_RESULT_MARK: string;
	readTodoMore(entries: readonly SessionEntry[]): RichState;
	hostPhases(state: RichState): HostTodoPhase[];
	toSnapshot(state: RichState): RichSnapshot;
	stateOf(goal: RichGoalState, phases: readonly RichPhase[]): RichState;
}

interface TodoBackend {
	read(entries: readonly SessionEntry[]): TodoState;
	/** Runs inside `appendEntriesAtomically`; must not await. */
	commit(session: AgentSession, plan: TodoDecision, entry: Entry, context: string): void;
	project(session: AgentSession, entries: readonly SessionEntry[]): void;
}

interface PreparedRich {
	next: RichState;
	origins: OnlyneTodoOrigin[];
	link?: OnlyneTodoLink;
	decision: OnlyneTodoDecisionRecord;
}

interface PreparedNative {
	/** Undefined for `none` and `associate`: those leave the host list untouched. */
	next?: TodoState;
	origins: OnlyneTodoOrigin[];
	link?: OnlyneTodoLink;
	decision: OnlyneTodoDecisionRecord;
	goalIntent?: TodoStateGoal;
}

let backendPromise: Promise<TodoBackend> | undefined;

/**
 * Record SystemOne's todo decision for one inbound message.
 *
 * With omp-todo-more installed, the rich snapshot, the source association and
 * the decision are one atomic branch commit; the native projection follows so
 * OMP restores its HUD on resume. Without it, the decision marker and the
 * `user_todo_edit` projection are that same atomic commit.
 */
export async function applyMessageTodo(entry: Entry, context: string, planner: TodoPlanner): Promise<void> {
	const scope = context.replace(/\s+/g, " ").trim();
	if (scope.length === 0) throw new OnlyneError("invalid_request", t("todoContextRequired"));

	const session = liveSession();
	const backend = await backendFor();
	const key = decisionKey(scope, entry);
	const replayed = readOnlyneTodo(session.sessionManager.getBranch());
	if (replayed.decisions.has(key)) {
		backend.project(session, session.sessionManager.getBranch());
		return;
	}

	const plan = await planner.decideTodo(entry, backend.read(session.sessionManager.getBranch()), scope);
	if (plan.action !== "none" && plan.action !== "associate" && plan.action !== "append") {
		throw new OnlyneError("decision_unavailable", t("todoUnknownAction", { action: String(plan.action) }));
	}

	await session.sessionManager.appendEntriesAtomically(() => {
		backend.commit(session, plan, entry, scope);
	});

	// Re-read after the commit: a later queued commit may already have landed.
	backend.project(session, session.sessionManager.getBranch());
}

/**
 * Rebuild the host todo projection from the branch after startup, rewind and
 * branch switches. The branch is authoritative; nothing is cached. The
 * structural reader covers both record formats, so recovery never waits for
 * the optional module to load.
 */
export function registerTodoRecovery(pi: ExtensionAPI): void {
	const project = (ctx: ExtensionContext): void => {
		const session = AgentRegistry.global().get(ctx.agent.id)?.session;
		if (session === null || session === undefined) return;
		session.setTodoPhases(nativePhases(readNativeState(session.sessionManager.getBranch())));
	};
	pi.on("session_start", (_event, ctx) => {
		project(ctx);
	});
	pi.on("session_branch", (_event, ctx) => {
		project(ctx);
	});
	pi.on("session_tree", (_event, ctx) => {
		project(ctx);
	});
}

/** Read the todo state and every Onlyne association recorded on an active branch. */
export function readOnlyneTodo(entries: readonly SessionEntry[]): OnlyneTodoBranch {
	let origins = new Map<string, OnlyneTodoOrigin>();
	const links = new Map<string, OnlyneTodoLink>();
	const decisions = new Map<string, OnlyneTodoDecisionRecord>();

	for (const entry of entries) {
		if (entry.type === "custom" && entry.customType === TODO_MORE_ENTRY) {
			const titles = snapshotTitles(entry.data);
			if (titles === undefined) continue;
			const metadata = onlyneMetadata(entry.data, entry.id);
			if (metadata !== undefined) {
				origins = new Map(
					metadata.origins
						.filter(origin => titles.has(`${origin.phase}\u0000${origin.title}`))
						.map(origin => [origin.item_id, origin]),
				);
				for (const link of metadata.links) links.set(link.link_id, link);
				if (metadata.decision !== undefined) decisions.set(metadata.decision.key, metadata.decision);
			} else {
				// A snapshot written by todo-more's own tool: titles keep their
				// origins only while they remain present. A title absent here is
				// retired, so a later same-title item starts a new identity.
				origins = pruneOrigins(origins, titles);
			}
			continue;
		}
		if (entry.type === "custom" && entry.customType === ONLYNE_TODO_DECISION) {
			const metadata = markerMetadata(entry.data, entry.id);
			origins = new Map(metadata.origins.map(origin => [origin.item_id, origin]));
			for (const link of metadata.links) links.set(link.link_id, link);
			if (metadata.decision !== undefined) decisions.set(metadata.decision.key, metadata.decision);
			continue;
		}
		const native = nativeTitles(entry);
		if (native !== undefined) origins = pruneOrigins(origins, native);
	}

	const state = readNativeState(entries);
	const present = stateTitles(state);
	const finalOrigins = [...origins.values()].filter(origin => present.has(`${origin.phase}\u0000${origin.title}`));
	const itemIds = new Set(finalOrigins.map(origin => origin.item_id));
	return {
		state,
		origins: finalOrigins,
		links: [...links.values()],
		activeLinks: [...links.values()].filter(link => itemIds.has(link.item_id)),
		decisions,
	};
}

function backendFor(): Promise<TodoBackend> {
	backendPromise ??= detectBackend();
	return backendPromise;
}

async function detectBackend(): Promise<TodoBackend> {
	const module = await loadTodoMoreState();
	return module === undefined ? nativeBackend : todoMoreBackend(module);
}

async function loadTodoMoreState(): Promise<TodoMoreStateModule | undefined> {
	const specifier = TODO_MORE_STATE_MODULE;
	try {
		// A variable specifier keeps TypeScript and the bundler from resolving the
		// optional peer at build time; absence is decided at runtime.
		return asTodoMoreStateModule(await import(specifier));
	} catch (error) {
		if (isMissingTodoMore(error)) return undefined;
		throw error;
	}
}

function isMissingTodoMore(error: unknown): boolean {
	if (error === null || typeof error !== "object") return false;
	const { code, message } = error as { code?: unknown; message?: unknown };
	if (code !== "MODULE_NOT_FOUND" && code !== "ERR_MODULE_NOT_FOUND") return false;
	if (typeof message !== "string") return false;
	// Absent means the package itself does not resolve. A missing file inside an
	// installed package is a broken install and must surface, not fall back.
	return (
		message.includes(`Cannot find package '${TODO_MORE_PACKAGE}'`) ||
		message.includes(`Cannot find package "${TODO_MORE_PACKAGE}"`) ||
		message.includes(`Cannot find module '${TODO_MORE_PACKAGE}'`) ||
		message.includes(`Cannot find module "${TODO_MORE_PACKAGE}"`)
	);
}

function asTodoMoreStateModule(value: unknown): TodoMoreStateModule {
	const module = value as Partial<TodoMoreStateModule> | null;
	if (
		module === null ||
		typeof module !== "object" ||
		typeof module.readTodoMore !== "function" ||
		typeof module.hostPhases !== "function" ||
		typeof module.toSnapshot !== "function" ||
		typeof module.stateOf !== "function" ||
		typeof module.TODO_MORE_ENTRY_TYPE !== "string" ||
		typeof module.TODO_MORE_RESULT_MARK !== "string"
	) {
		throw new OnlyneError("unavailable", t("todoAdapterApi"));
	}
	if (module.TODO_MORE_ENTRY_TYPE !== TODO_MORE_ENTRY || module.TODO_MORE_RESULT_MARK !== TODO_MORE_RESULT_MARK) {
		throw new OnlyneError("unavailable", t("todoRecordTypes"));
	}
	return module as TodoMoreStateModule;
}

function todoMoreBackend(module: TodoMoreStateModule): TodoBackend {
	return {
		read: entries => stateFromRich(module.readTodoMore(entries)),
		commit(session, plan, entry, context) {
			const branch = session.sessionManager.getBranch();
			const latest = readOnlyneTodo(branch);
			if (latest.decisions.has(decisionKey(context, entry))) return;
			const prepared = prepareRichCommit(module, module.readTodoMore(branch), latest.origins, plan, entry, context);
			const metadata: OnlyneTodoMetadata = {
				version: 1,
				commit_id: crypto.randomUUID(),
				decision: prepared.decision,
				origins: prepared.origins,
				links: prepared.link === undefined ? [] : [prepared.link],
			};
			// Rich snapshot first, host projection second: todo-more joins the native
			// record onto the nearest earlier rich snapshot, and OMP restores the HUD
			// from the native record on resume.
			session.sessionManager.appendCustomEntry(TODO_MORE_ENTRY, {
				...module.toSnapshot(prepared.next),
				onlyne: metadata,
			});
			session.sessionManager.appendCustomEntry(USER_TODO_EDIT_CUSTOM_TYPE, {
				phases: module.hostPhases(prepared.next),
			});
		},
		project(session, entries) {
			session.setTodoPhases(module.hostPhases(module.readTodoMore(entries)));
		},
	};
}

const nativeBackend: TodoBackend = {
	read: readNativeState,
	commit(session, plan, entry, context) {
		const branch = session.sessionManager.getBranch();
		const latest = readOnlyneTodo(branch);
		if (latest.decisions.has(decisionKey(context, entry))) return;
		const prepared = prepareNativeCommit(latest.state, latest.origins, plan, entry, context);
		const metadata: OnlyneTodoMetadata = {
			version: 1,
			commit_id: crypto.randomUUID(),
			decision: prepared.decision,
			origins: prepared.origins,
			links: prepared.link === undefined ? [] : [prepared.link],
			...(prepared.goalIntent === undefined ? {} : { goal_intent: prepared.goalIntent }),
		};
		session.sessionManager.appendCustomEntry(ONLYNE_TODO_DECISION, metadata);
		if (prepared.next !== undefined) {
			session.sessionManager.appendCustomEntry(USER_TODO_EDIT_CUSTOM_TYPE, {
				phases: nativePhases(prepared.next),
			});
		}
	},
	project(session, entries) {
		session.setTodoPhases(nativePhases(readNativeState(entries)));
	},
};

function stateFromRich(state: RichState): TodoState {
	if (state.kind === "empty") return { goal: null, phases: [] };
	return {
		goal: state.goal.kind === "set" ? state.goal.goal : null,
		phases: state.phases.map(phase => ({
			name: phase.name,
			items: phase.items.map(item => ({
				title: item.title,
				status: item.status,
				...(item.explanation.kind === "explained" ? { explanation: item.explanation.text } : {}),
				...(item.blocker === undefined ? {} : { blocker: item.blocker }),
			})),
		})),
	};
}

function prepareRichCommit(
	module: TodoMoreStateModule,
	state: RichState,
	latestOrigins: readonly OnlyneTodoOrigin[],
	plan: TodoDecision,
	entry: Entry,
	context: string,
): PreparedRich {
	const at = new Date().toISOString();
	const decision: OnlyneTodoDecisionRecord = {
		key: decisionKey(context, entry),
		context,
		message_ref: { sender: entry.message.sender, id: entry.message.id },
		thread_ref: entry.thread_ref,
		seq: entry.seq,
		action: plan.action,
		at,
	};
	const origins = new Map(latestOrigins.map(origin => [origin.item_id, origin]));
	const present = richTitles(state);
	for (const [id, origin] of origins) {
		if (!present.has(`${origin.phase}\u0000${origin.title}`)) origins.delete(id);
	}

	switch (plan.action) {
		case "none":
			return { next: state, origins: [...origins.values()], decision };
		case "associate": {
			const title = requiredText(plan.title, "associate", "title");
			const phase = plan.phase?.trim() || undefined;
			if (state.kind === "empty") {
				throw new OnlyneError("decision_unavailable", t("todoEmptyAssociate"));
			}
			const matches = state.phases
				.filter(candidate => phase === undefined || candidate.name === phase)
				.filter(candidate => candidate.items.some(item => item.title === title))
				.map(candidate => candidate.name);
			if (matches.length === 0) {
				throw new OnlyneError("decision_unavailable", t("todoNoSuchItem", { title }));
			}
			if (matches.length > 1) {
				throw new OnlyneError("ambiguous_target", t("todoAmbiguousTitle", { title }));
			}
			const phaseName = matches[0]!;
			const origin = [...origins.values()].find(
				candidate => candidate.phase === phaseName && candidate.title === title,
			) ?? { item_id: crypto.randomUUID(), phase: phaseName, title };
			origins.set(origin.item_id, origin);
			const link = makeLink(entry, context, origin, at);
			decision.link_id = link.link_id;
			return { next: state, origins: [...origins.values()], link, decision };
		}
		case "append": {
			const title = requiredText(plan.title, "append", "title");
			const existing = state.kind === "list"
				? state.phases.flatMap(candidate => candidate.items).some(item => item.title === title)
				: false;
			if (existing) {
				throw new OnlyneError("id_conflict", t("todoDuplicateTitle", { title }));
			}

			const phaseName = plan.phase?.trim() ?? t("todoDefaultPhase");
			if (phaseName.length === 0) throw new OnlyneError("invalid_request", t("todoPhaseRequired"));
			const explanation = appendExplanation(plan, entry, context);
			const item: RichItem = {
				title,
				explanation,
				status: "pending",
				time: itemTime(plan.minutes),
			};

			let goal: RichGoalState;
			let phases: readonly RichPhase[];
			if (state.kind === "empty") {
				goal = { kind: "set", goal: requiredGoal(plan.goal) };
				phases = [{ name: phaseName, items: [item] }];
			} else {
				goal =
					state.goal.kind === "missing" && plan.goal !== undefined
						? { kind: "set", goal: requiredGoal(plan.goal) }
						: state.goal;
				const index = state.phases.findIndex(candidate => candidate.name === phaseName);
				phases =
					index < 0
						? [...state.phases, { name: phaseName, items: [item] }]
						: state.phases.map((candidate, position) =>
								position === index ? { name: candidate.name, items: [...candidate.items, item] } : candidate,
							);
			}

			const next = module.stateOf(goal, phases);
			const origin: OnlyneTodoOrigin = { item_id: crypto.randomUUID(), phase: phaseName, title };
			origins.set(origin.item_id, origin);
			const link = makeLink(entry, context, origin, at);
			decision.link_id = link.link_id;
			return { next, origins: [...origins.values()], link, decision };
		}
		default: {
			const unreachable: never = plan.action;
			throw new Error(t("todoUnexpectedAction", { action: String(unreachable) }));
		}
	}
}

function prepareNativeCommit(
	state: TodoState,
	latestOrigins: readonly OnlyneTodoOrigin[],
	plan: TodoDecision,
	entry: Entry,
	context: string,
): PreparedNative {
	const at = new Date().toISOString();
	const decision: OnlyneTodoDecisionRecord = {
		key: decisionKey(context, entry),
		context,
		message_ref: { sender: entry.message.sender, id: entry.message.id },
		thread_ref: entry.thread_ref,
		seq: entry.seq,
		action: plan.action,
		at,
	};
	const origins = new Map(latestOrigins.map(origin => [origin.item_id, origin]));
	const present = stateTitles(state);
	for (const [id, origin] of origins) {
		if (!present.has(`${origin.phase}\u0000${origin.title}`)) origins.delete(id);
	}

	switch (plan.action) {
		case "none":
			return { origins: [...origins.values()], decision };
		case "associate": {
			const title = requiredText(plan.title, "associate", "title");
			const phase = plan.phase?.trim() || undefined;
			const matches = state.phases
				.filter(candidate => phase === undefined || candidate.name === phase)
				.filter(candidate => candidate.items.some(item => item.title === title))
				.map(candidate => candidate.name);
			if (matches.length === 0) {
				throw new OnlyneError("decision_unavailable", t("todoNoSuchItem", { title }));
			}
			if (matches.length > 1) {
				throw new OnlyneError("ambiguous_target", t("todoAmbiguousTitle", { title }));
			}
			const phaseName = matches[0]!;
			const origin = [...origins.values()].find(
				candidate => candidate.phase === phaseName && candidate.title === title,
			) ?? { item_id: crypto.randomUUID(), phase: phaseName, title };
			origins.set(origin.item_id, origin);
			const link = makeLink(entry, context, origin, at);
			decision.link_id = link.link_id;
			return { origins: [...origins.values()], link, decision };
		}
		case "append": {
			const title = requiredText(plan.title, "append", "title");
			const existing = state.phases.some(candidate => candidate.items.some(item => item.title === title));
			if (existing) {
				throw new OnlyneError("id_conflict", t("todoDuplicateTitle", { title }));
			}
			const phaseName = plan.phase?.trim() ?? t("todoDefaultPhase");
			if (phaseName.length === 0) throw new OnlyneError("invalid_request", t("todoPhaseRequired"));
			const item: TodoStateItem = { title, status: "pending" };
			const index = state.phases.findIndex(candidate => candidate.name === phaseName);
			const phases =
				index < 0
					? [...state.phases, { name: phaseName, items: [item] }]
					: state.phases.map((candidate, position) =>
							position === index ? { name: candidate.name, items: [...candidate.items, item] } : candidate,
						);
			const origin: OnlyneTodoOrigin = { item_id: crypto.randomUUID(), phase: phaseName, title };
			origins.set(origin.item_id, origin);
			const link = makeLink(entry, context, origin, at);
			decision.link_id = link.link_id;
			const explanation = explanationText(plan, entry, context);
			if (explanation !== undefined) decision.explanation = explanation;
			// Native OMP lists have no goal field; the intent survives on the marker
			// instead of being invented into the host list.
			const empty = state.phases.every(candidate => candidate.items.length === 0);
			const goalIntent = empty ? goalIntentOf(plan.goal) : undefined;
			return {
				next: { goal: state.goal, phases },
				origins: [...origins.values()],
				link,
				decision,
				...(goalIntent === undefined ? {} : { goalIntent }),
			};
		}
		default: {
			const unreachable: never = plan.action;
			throw new Error(t("todoUnexpectedAction", { action: String(unreachable) }));
		}
	}
}

function decisionKey(context: string, entry: Entry): string {
	return `${context}\u0000${entry.message.sender}\u0000${entry.message.id}`;
}

function makeLink(entry: Entry, context: string, origin: OnlyneTodoOrigin, at: string): OnlyneTodoLink {
	return {
		link_id: crypto.randomUUID(),
		message_ref: { sender: entry.message.sender, id: entry.message.id },
		thread_ref: entry.thread_ref,
		context,
		seq: entry.seq,
		item_id: origin.item_id,
		phase: origin.phase,
		title: origin.title,
		at,
	};
}

function explanationText(plan: TodoDecision, entry: Entry, context: string): string | undefined {
	const body = plan.explanation?.trim() || entry.message.msg.replace(/\s+/g, " ").trim();
	if (body.length === 0) return undefined;
	return `${t("todoSourceLine", { source: context, seq: entry.seq, author: entry.authenticated_author })}\n\n${body}`;
}

function appendExplanation(plan: TodoDecision, entry: Entry, context: string): RichExplanation {
	const text = explanationText(plan, entry, context);
	if (text === undefined) {
		throw new OnlyneError("invalid_request", t("todoExplanationRequired"));
	}
	return { kind: "explained", text };
}

function itemTime(minutes: number | undefined): RichTime {
	if (minutes === undefined) return { kind: "missing" };
	const max = Math.floor(Number.MAX_SAFE_INTEGER / 60_000);
	if (!Number.isInteger(minutes) || minutes <= 0 || minutes > max) {
		throw new OnlyneError("invalid_request", t("todoEstimateInvalid"));
	}
	return { kind: "estimated", budgetId: crypto.randomUUID(), minutes, elapsedMs: 0, runningSince: null };
}

function requiredText(value: string | undefined, action: string, field: string): string {
	const text = value?.trim();
	if (text === undefined || text.length === 0) {
		throw new OnlyneError("decision_unavailable", t("todoMissingField", { action, field }));
	}
	return text;
}

function requiredGoal(goal: TodoDecision["goal"]): TodoStateGoal {
	const objective = goal?.objective.trim();
	const measure = goal?.measure.trim();
	const doneWhen = goal?.doneWhen.trim();
	if (!objective || !measure || !doneWhen) {
		throw new OnlyneError("decision_unavailable", t("todoGoalRequired"));
	}
	return { objective, measure, doneWhen };
}

/** Native lists cannot carry a goal; record a complete one, ignore a partial one. */
function goalIntentOf(goal: TodoDecision["goal"]): TodoStateGoal | undefined {
	const objective = goal?.objective.trim();
	const measure = goal?.measure.trim();
	const doneWhen = goal?.doneWhen.trim();
	if (!objective || !measure || !doneWhen) return undefined;
	return { objective, measure, doneWhen };
}

function liveSession(): AgentSession {
	const mains = AgentRegistry.global()
		.list()
		.filter(ref => ref.kind === "main" && ref.session !== null);
	if (mains.length === 0) {
		throw new OnlyneError("unavailable", t("todoNoSession"));
	}
	if (mains.length > 1) {
		throw new OnlyneError("unavailable", t("todoMultipleSessions"));
	}
	return mains[0]!.session!;
}

function stateTitles(state: TodoState): Set<string> {
	const titles = new Set<string>();
	for (const phase of state.phases) {
		for (const item of phase.items) titles.add(`${phase.name}\u0000${item.title}`);
	}
	return titles;
}

function richTitles(state: RichState): Set<string> {
	const titles = new Set<string>();
	if (state.kind === "list") {
		for (const phase of state.phases) {
			for (const item of phase.items) titles.add(`${phase.name}\u0000${item.title}`);
		}
	}
	return titles;
}

function pruneOrigins(
	origins: Map<string, OnlyneTodoOrigin>,
	present: ReadonlySet<string>,
): Map<string, OnlyneTodoOrigin> {
	const kept = new Map<string, OnlyneTodoOrigin>();
	for (const origin of origins.values()) {
		if (present.has(`${origin.phase}\u0000${origin.title}`)) kept.set(origin.item_id, origin);
	}
	return kept;
}

function snapshotTitles(data: unknown): Set<string> | undefined {
	const phases = recordField(data, "phases");
	if (!Array.isArray(phases)) return undefined;
	const titles = new Set<string>();
	for (const phase of phases) {
		const name = stringField(phase, "name");
		const items = recordField(phase, "items");
		if (name === undefined || !Array.isArray(items)) return undefined;
		for (const item of items) {
			const title = stringField(item, "title");
			if (title === undefined) return undefined;
			titles.add(`${name}\u0000${title}`);
		}
	}
	return titles;
}

function onlyneMetadata(data: unknown, entryId: string): OnlyneTodoMetadata | undefined {
	const value = recordField(data, "onlyne");
	if (value === undefined) return undefined;
	return parseMetadata(value, entryId);
}

function markerMetadata(data: unknown, entryId: string): OnlyneTodoMetadata {
	const metadata = parseMetadata(data, entryId);
	if (metadata === undefined) {
		throw new Error(t("todoBadMetadataVersion", { entry: entryId }));
	}
	if (metadata.decision === undefined) {
		throw new Error(t("todoNoDecisionRecord", { entry: entryId }));
	}
	return metadata;
}

function parseMetadata(value: unknown, entryId: string): OnlyneTodoMetadata | undefined {
	if (recordField(value, "version") !== 1) return undefined;
	const origins = parseOrigins(recordField(value, "origins"), entryId);
	const links = parseLinks(recordField(value, "links"), entryId);
	const decision = parseDecision(recordField(value, "decision"), entryId);
	const goalIntent = parseGoalIntent(recordField(value, "goal_intent"), entryId);
	return {
		version: 1,
		commit_id: stringField(value, "commit_id") ?? "",
		origins,
		links,
		...(decision === undefined ? {} : { decision }),
		...(goalIntent === undefined ? {} : { goal_intent: goalIntent }),
	};
}

function parseGoalIntent(value: unknown, entryId: string): TodoStateGoal | undefined {
	if (value === undefined) return undefined;
	return {
		objective: requiredString(value, "objective", entryId),
		measure: requiredString(value, "measure", entryId),
		doneWhen: requiredString(value, "doneWhen", entryId),
	};
}

function parseOrigins(value: unknown, entryId: string): OnlyneTodoOrigin[] {
	if (!Array.isArray(value)) {
		throw new Error(t("todoNoOrigins", { entry: entryId }));
	}
	return value.map(origin => ({
		item_id: requiredString(origin, "item_id", entryId),
		phase: requiredString(origin, "phase", entryId),
		title: requiredString(origin, "title", entryId),
	}));
}

function parseLinks(value: unknown, entryId: string): OnlyneTodoLink[] {
	if (!Array.isArray(value)) {
		throw new Error(t("todoNoLinks", { entry: entryId }));
	}
	return value.map(link => ({
		link_id: requiredString(link, "link_id", entryId),
		message_ref: requiredMessageRef(recordField(link, "message_ref"), entryId),
		thread_ref: requiredThreadRef(recordField(link, "thread_ref"), entryId),
		context: requiredString(link, "context", entryId),
		seq: requiredString(link, "seq", entryId),
		item_id: requiredString(link, "item_id", entryId),
		phase: requiredString(link, "phase", entryId),
		title: requiredString(link, "title", entryId),
		at: requiredString(link, "at", entryId),
	}));
}

function parseDecision(value: unknown, entryId: string): OnlyneTodoDecisionRecord | undefined {
	if (value === undefined) return undefined;
	const action = stringField(value, "action");
	if (action !== "none" && action !== "associate" && action !== "append") {
		throw new Error(t("todoUnknownRecordedAction", { entry: entryId }));
	}
	const linkId = stringField(value, "link_id");
	const explanation = stringField(value, "explanation");
	return {
		key: requiredString(value, "key", entryId),
		context: requiredString(value, "context", entryId),
		message_ref: requiredMessageRef(recordField(value, "message_ref"), entryId),
		thread_ref: requiredThreadRef(recordField(value, "thread_ref"), entryId),
		seq: requiredString(value, "seq", entryId),
		action,
		...(linkId === undefined ? {} : { link_id: linkId }),
		...(explanation === undefined ? {} : { explanation }),
		at: requiredString(value, "at", entryId),
	};
}

function requiredMessageRef(value: unknown, entryId: string): MessageRef {
	return {
		sender: requiredString(value, "sender", entryId),
		id: requiredString(value, "id", entryId),
	};
}

function requiredThreadRef(value: unknown, entryId: string): ThreadRef {
	return {
		owner: requiredString(value, "owner", entryId),
		id: requiredString(value, "id", entryId),
	};
}

function requiredString(value: unknown, field: string, entryId: string): string {
	const text = stringField(value, field);
	if (text === undefined) {
		throw new Error(t("todoMissingString", { entry: entryId, field }));
	}
	return text;
}
