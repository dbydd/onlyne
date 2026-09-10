import { parseSwarmHeader } from "./swarm.js";
import { resolveSwarmPrompt } from "./swarm-prompt.js";

export type SwarmHandleResult = "claimed" | "yielded" | "not-swarm";

export interface SwarmPi {
	sendMessage: (message: { customType: string; content: string; display: boolean; details?: unknown }, opts: { triggerTurn: true; deliverAs: "followUp" }) => void;
}

/**
 * Single atomic task slot for swarm mode, extracted for unit testing.
 * index.ts delegates its state transitions here; the only coupling is the
 * followUp injection callback.
 */
export class SwarmSlot {
	private taskId?: string;
	private from = ".";
	private transfer = "";
	private attempt = 1;
	private sentChildIds: string[] = [];

	handle(pi: SwarmPi, text: string, preferredTaskId?: string, workspaceRoot?: string): SwarmHandleResult {
		const parsed = parseSwarmHeader(text);
		if (!parsed) return "not-swarm";
		const { header, delivery, payload } = parsed;
		// Only the scheduler's second delivery is claimable. A worker's raw
		// swarm_send relay and every out wire omit this structured field, so
		// history catchup cannot claim either one as an executable task.
		if (delivery !== "scheduler") return "not-swarm";
		if (!this.taskId) {
			this.taskId = header.task_id;
			this.from = header.from;
			this.transfer = header.transfer_send_to;
			this.attempt = header.attempt;
			this.sentChildIds = [];
			// Cold start needs triggerTurn: followUp alone only queues behind an
			// active turn, and a fresh session has none. sendMessage with
			// triggerTurn starts the model turn immediately.
			pi.sendMessage(
				{ customType: "onlyne-swarm-task", content: resolveSwarmPrompt(workspaceRoot ?? process.cwd(), header, payload) ?? payload, display: true },
				{ triggerTurn: true, deliverAs: "followUp" },
			);
			return "claimed";
		}
		// Env-task preemption: the scheduler injected ONLYNE_SWARM_TASK for
		// this terminal. If the slot holds a stale claim (history catchup
		// grabbed an already-done task) and the env task arrives, yield the
		// slot silently — the stale claim never did work, so nothing is lost.
		if (preferredTaskId && header.task_id === preferredTaskId && this.taskId !== preferredTaskId) {
			this.taskId = header.task_id;
			this.from = header.from;
			this.transfer = header.transfer_send_to;
			this.attempt = header.attempt;
			this.sentChildIds = [];
			pi.sendMessage(
				{ customType: "onlyne-swarm-task", content: resolveSwarmPrompt(workspaceRoot ?? process.cwd(), header, payload) ?? payload, display: true },
				{ triggerTurn: true, deliverAs: "followUp" },
			);
			return "yielded";
		}
		return "not-swarm";
	}

	task(): { taskId?: string; from: string; transferSendTo: string; attempt: number; sentChildIds: string[] } {
		return { taskId: this.taskId, from: this.from, transferSendTo: this.transfer, attempt: this.attempt, sentChildIds: [...this.sentChildIds] };
	}

	noteSpawned(childId: string): void {
		this.sentChildIds.push(childId);
	}

	taskIdOf(): string | undefined {
		return this.taskId;
	}

	clear(): void {
		this.taskId = undefined;
		this.from = ".";
		this.transfer = "";
		this.attempt = 1;
		this.sentChildIds = [];
	}
}

/** Test seam: fresh slot without touching module-global pi-onlyne state. */
export function __swarmSlotForTest(): {
	handle: (pi: SwarmPi, text: string, preferredTaskId?: string, workspaceRoot?: string) => SwarmHandleResult;
	task: () => { taskId?: string; from: string; transferSendTo: string; attempt: number; sentChildIds: string[] };
	taskId: () => string | undefined;
	noteSpawned: (childId: string) => void;
	clear: () => void;
} {
	const slot = new SwarmSlot();
	return {
		handle: (pi, text, preferredTaskId, workspaceRoot) => slot.handle(pi, text, preferredTaskId, workspaceRoot),
		task: () => slot.task(),
		taskId: () => slot.taskIdOf(),
		noteSpawned: (childId) => slot.noteSpawned(childId),
		clear: () => slot.clear(),
	};
}
