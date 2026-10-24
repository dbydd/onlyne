import type { SessionEntry } from "@oh-my-pi/pi-coding-agent";
import { USER_TODO_EDIT_CUSTOM_TYPE } from "@oh-my-pi/pi-coding-agent/tools/todo";
import type { TodoState, TodoStateGoal, TodoStateItem, TodoStatePhase, TodoStatus } from "./todo.ts";

/**
 * Branch records the structural reader understands. Kept as literals so this
 * module never imports omp-todo-more; the rich adapter checks the installed
 * module against them before it writes anything.
 */
export const TODO_MORE_ENTRY = "mouriya.omp.todo-more";
export const TODO_MORE_RESULT_MARK = "todoMore";

/** What `session.setTodoPhases` and the native HUD take. */
export interface HostTodoTask {
	content: string;
	status: TodoStatus;
	blocker?: string;
	details?: string;
	notes?: string[];
}

export interface HostTodoPhase {
	name: string;
	tasks: HostTodoTask[];
}

const TODO_STATUS: Record<TodoStatus, true> = {
	pending: true,
	in_progress: true,
	completed: true,
	abandoned: true,
	blocked: true,
};

export function isTodoStatus(value: unknown): value is TodoStatus {
	return typeof value === "string" && Object.hasOwn(TODO_STATUS, value);
}

interface NativeTask {
	content: string;
	status: TodoStatus;
	blocker?: string;
	details?: string;
	notes?: string[];
}

interface NativePhase {
	name: string;
	tasks: NativeTask[];
}

/**
 * The host's own todo list on a branch (oldest first): the newest native edit,
 * joined onto the nearest earlier rich snapshot by title. Rich snapshots only
 * contribute explanations; statuses, blockers and ordering are native.
 */
export function readNativeState(entries: readonly SessionEntry[]): TodoState {
	const pending: NativePhase[][] = [];
	for (let index = entries.length - 1; index >= 0; index--) {
		const entry = entries[index];
		if (entry === undefined) continue;
		const rich = richState(entry);
		if (rich !== undefined) return joinNative(rich, pending);
		const native = nativeRecord(entry);
		if (native === undefined) continue;
		pending.push(native);
		// An empty native edit clears the list; older records must not resurrect it.
		if (native.every(phase => phase.tasks.length === 0)) break;
	}
	return joinNative({ goal: null, phases: [] }, pending);
}

/** The titles one native record defines, for pruning Onlyne's origin roster. */
export function nativeTitles(entry: SessionEntry): Set<string> | undefined {
	const phases = nativeRecord(entry);
	if (phases === undefined) return undefined;
	const titles = new Set<string>();
	for (const phase of phases) {
		for (const task of phase.tasks) titles.add(`${phase.name}\u0000${task.content}`);
	}
	return titles;
}

/** The host projection of a state: what `setTodoPhases` persists. */
export function nativePhases(state: TodoState): HostTodoPhase[] {
	return state.phases.map(phase => ({
		name: phase.name,
		tasks: phase.items.map(item => ({
			content: item.title,
			status: item.status,
			...(item.blocker === undefined ? {} : { blocker: item.blocker }),
			...(item.details === undefined ? {} : { details: item.details }),
			...(item.notes === undefined ? {} : { notes: item.notes }),
		})),
	}));
}

function joinNative(base: TodoState, pendingNewestFirst: NativePhase[][]): TodoState {
	return pendingNewestFirst.reduceRight((state, phases) => {
		const known = new Map<string, TodoStateItem>();
		for (const phase of state.phases) {
			for (const item of phase.items) known.set(item.title, item);
		}
		const joined: TodoStatePhase[] = phases.map(phase => ({
			name: phase.name,
			items: phase.tasks.map(task => {
				const previous = known.get(task.content);
				const details = task.details ?? previous?.details;
				const notes = task.notes ?? previous?.notes;
				return {
					title: task.content,
					status: task.status,
					...(previous?.explanation === undefined ? {} : { explanation: previous.explanation }),
					...(task.blocker === undefined ? {} : { blocker: task.blocker }),
					...(details === undefined ? {} : { details }),
					...(notes === undefined ? {} : { notes }),
				};
			}),
		}));
		let count = 0;
		for (const phase of joined) count += phase.items.length;
		return count === 0 ? { goal: null, phases: [] } : { goal: state.goal, phases: joined };
	}, base);
}

function richState(entry: SessionEntry): TodoState | undefined {
	if (entry.type !== "custom" || entry.customType !== TODO_MORE_ENTRY) return undefined;
	const phases = recordField(entry.data, "phases");
	if (!Array.isArray(phases)) return undefined;
	const parsed: TodoStatePhase[] = [];
	for (const phase of phases) {
		const name = stringField(phase, "name");
		const items = recordField(phase, "items");
		if (name === undefined || !Array.isArray(items)) return undefined;
		const parsedItems: TodoStateItem[] = [];
		for (const item of items) {
			const title = stringField(item, "title");
			const status = recordField(item, "status");
			if (title === undefined || !isTodoStatus(status)) return undefined;
			const explanation = stringField(item, "explanation");
			const blocker = stringField(item, "blocker");
			parsedItems.push({
				title,
				status,
				...(explanation === undefined ? {} : { explanation }),
				...(blocker === undefined ? {} : { blocker }),
			});
		}
		parsed.push({ name, items: parsedItems });
	}
	const goal = richGoal(recordField(entry.data, "goal"));
	if (goal === undefined) return undefined;
	let count = 0;
	for (const phase of parsed) count += phase.items.length;
	return count === 0 ? { goal: null, phases: [] } : { goal, phases: parsed };
}

/** `null` means the snapshot carries no goal; `undefined` means it is malformed. */
function richGoal(value: unknown): TodoStateGoal | null | undefined {
	if (value === null || value === undefined) return null;
	const objective = stringField(value, "objective");
	const measure = stringField(value, "measure");
	const doneWhen = stringField(value, "doneWhen");
	if (objective === undefined || measure === undefined || doneWhen === undefined) return undefined;
	if (objective.trim().length === 0 || measure.trim().length === 0 || doneWhen.trim().length === 0) return undefined;
	return { objective, measure, doneWhen };
}

function nativeRecord(entry: SessionEntry): NativePhase[] | undefined {
	if (entry.type === "custom") {
		if (entry.customType !== USER_TODO_EDIT_CUSTOM_TYPE) return undefined;
		return parseNativePhases(recordField(entry.data, "phases"));
	}
	if (entry.type !== "message") return undefined;
	const message = entry.message as unknown as {
		role?: unknown;
		toolName?: unknown;
		isError?: unknown;
		details?: { op?: unknown; phases?: unknown; [TODO_MORE_RESULT_MARK]?: unknown };
	};
	if (message.role !== "toolResult" || message.toolName !== "todo" || message.isError === true) return undefined;
	if (message.details?.op === "view") return undefined;
	// todo-more's own results follow the entry that records them.
	if (message.details?.[TODO_MORE_RESULT_MARK] === true) return undefined;
	return parseNativePhases(message.details?.phases);
}

function parseNativePhases(value: unknown): NativePhase[] | undefined {
	if (!Array.isArray(value)) return undefined;
	const phases: NativePhase[] = [];
	for (const phase of value) {
		const name = stringField(phase, "name");
		const tasks = recordField(phase, "tasks");
		if (name === undefined || !Array.isArray(tasks)) return undefined;
		const parsed: NativeTask[] = [];
		for (const task of tasks) {
			const content = stringField(task, "content");
			const status = recordField(task, "status");
			if (content === undefined || !isTodoStatus(status)) return undefined;
			const blocker = stringField(task, "blocker");
			const details = stringField(task, "details");
			const notes = stringListField(task, "notes");
			parsed.push({
				content,
				status,
				...(blocker === undefined ? {} : { blocker }),
				...(details === undefined ? {} : { details }),
				...(notes === undefined ? {} : { notes }),
			});
		}
		phases.push({ name, tasks: parsed });
	}
	return phases;
}

function stringListField(value: unknown, field: string): string[] | undefined {
	const list = recordField(value, field);
	if (!Array.isArray(list)) return undefined;
	return list.every(entry => typeof entry === "string") ? (list as string[]) : undefined;
}

export function recordField(value: unknown, field: string): unknown {
	if (value === null || typeof value !== "object") return undefined;
	return (value as Record<string, unknown>)[field];
}

export function stringField(value: unknown, field: string): string | undefined {
	const text = recordField(value, field);
	return typeof text === "string" ? text : undefined;
}
