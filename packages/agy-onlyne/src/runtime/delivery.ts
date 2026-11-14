import { existsSync } from "node:fs";
import { open, stat } from "node:fs/promises";
import type { DeliveryPlan, Entry, NodeId, OnlyneNode, Storage, ThreadRef } from "@onlyne/lib";
import type { ContextRegistry } from "@onlyne/node-host/store";
import type { AgyOnlyneConfig } from "../config.ts";
import { threadKey } from "../core/resolve.ts";
import type { DeliveryPendingEntry, OnlyneTodoEntry } from "../core/types.ts";
import type { JudgeDecider } from "../core/decider.ts";
import { AntigravityBridge } from "./antigravity.ts";

const DELIVERY_PREFIX = "agy/delivery/pending/";
const CONSUMED_TRANSCRIPT_PREFIX = "agy/delivery/seen/";
const TODO_PREFIX = "agy/todo/";
const CONV_PREFIX = "agy/conversation/";
const DEFAULT_ESCALATE_MS = 60_000;
const TRANSCRIPT_TAIL_BYTES = 262_144;

export interface ActiveConversation {
	conversation_id: string;
	context_alias: string;
	is_main: boolean;
	is_subagent: boolean;
	transcript_path?: string;
	workspace?: string;
	last_seen_at: string;
}

export interface HookEventInput {
	hookEventName: string;
	conversationId?: string;
	workspacePaths?: string[];
	transcriptPath?: string;
	modelName?: string;
	lastUserInput?: string;
	cwd?: string;
}

export interface HookResultPayload {
	injectSteps?: Array<{ systemMessage: { systemMessage: string } }>;
	notes?: string[];
}

function slotKey(sender: NodeId, threadRef: ThreadRef, seq: string, context: string): string {
	return `${DELIVERY_PREFIX}${encodeURIComponent(context)}/${encodeURIComponent(threadKey(threadRef))}/${seq}/${encodeURIComponent(sender)}`;
}

function tokenTag(token: string): string {
	return `[[onlyne:${token}]]`;
}

async function readTailUtf8(filePath: string, maxBytes = TRANSCRIPT_TAIL_BYTES): Promise<string> {
	if (!existsSync(filePath)) return "";
	const info = await stat(filePath).catch(() => undefined);
	if (info === undefined || info.size === 0) return "";
	const handle = await open(filePath, "r");
	try {
		const start = Math.max(0, info.size - maxBytes);
		const length = info.size - start;
		const buf = Buffer.alloc(length);
		await handle.read(buf, 0, length, start);
		return buf.toString("utf8");
	} finally {
		await handle.close();
	}
}

export function formatInboundPrompt(entry: Entry, token: string, contextAlias: string): string {
	const thread = threadKey(entry.thread_ref);
	const body = entry.message.msg.trim();
	return [
		`${tokenTag(token)} Onlyne inbound message (context=${contextAlias})`,
		`- from: ${entry.message.sender}`,
		`- thread: ${thread} (seq ${entry.seq})`,
		`- delivery_kind: ${entry.message.delivery_kind}`,
		`- message_id: ${entry.message.id}`,
		"",
		body,
		"",
		`To reply in this thread, call \`msg\` with \`meta: { in_reply_to: { thread: "${thread}", seq: "${entry.seq}" }, relation: "reply" }\`.`,
	].join("\n");
}

export class DeliveryManager {
	private readonly escalateTimers = new Map<string, ReturnType<typeof setTimeout>>();

	constructor(private readonly deps: {
		config: () => AgyOnlyneConfig;
		node: OnlyneNode;
		storage: Storage;
		contexts: ContextRegistry;
		decider: JudgeDecider;
		bridge?: AntigravityBridge;
		onChanged(): void;
		log?(event: string, fields?: Record<string, unknown>): void;
	}) {}

	async listPending(): Promise<DeliveryPendingEntry[]> {
		const rows = await this.deps.storage.transaction(tx => tx.scan<DeliveryPendingEntry>(DELIVERY_PREFIX));
		return rows.map(r => r.value).sort((a, b) => a.created_at.localeCompare(b.created_at));
	}

	async listTodos(): Promise<OnlyneTodoEntry[]> {
		const rows = await this.deps.storage.transaction(tx => tx.scan<OnlyneTodoEntry>(TODO_PREFIX));
		return rows.map(r => r.value).sort((a, b) => b.updated_at.localeCompare(a.updated_at));
	}

	async updateTodoStatus(id: string, status: OnlyneTodoEntry["status"]): Promise<OnlyneTodoEntry | undefined> {
		const updated = await this.deps.storage.transaction(async tx => {
			const row = await tx.get<OnlyneTodoEntry>(`${TODO_PREFIX}${encodeURIComponent(id)}`);
			if (row === undefined) return undefined;
			const next: OnlyneTodoEntry = { ...row, status, updated_at: new Date().toISOString() };
			await tx.put(`${TODO_PREFIX}${encodeURIComponent(id)}`, next);
			return next;
		});
		if (updated !== undefined) this.deps.onChanged();
		return updated;
	}

	async listConversations(): Promise<ActiveConversation[]> {
		const rows = await this.deps.storage.transaction(tx => tx.scan<ActiveConversation>(CONV_PREFIX));
		return rows.map(r => r.value).sort((a, b) => b.last_seen_at.localeCompare(a.last_seen_at));
	}

	async registerConversation(input: {
		conversationId: string;
		transcriptPath?: string;
		workspace?: string;
	}): Promise<ActiveConversation> {
		const cid = input.conversationId.trim();
		const now = new Date().toISOString();
		const existing = await this.deps.storage.transaction(tx =>
			tx.get<ActiveConversation>(`${CONV_PREFIX}${encodeURIComponent(cid)}`));
		let isSubagent = existing?.is_subagent ?? false;
		let contextAlias = existing?.context_alias ?? "main";
		if (existing === undefined && this.deps.bridge !== undefined) {
			const meta = await this.deps.bridge.getConversationMetadata(cid).catch(() => undefined);
			if (meta !== undefined) {
				isSubagent = meta.isSubagent;
			}
		}
		if (!isSubagent) {
			await this.deps.contexts.ensureMain();
			contextAlias = "main";
		} else {
			const matchedCtx = (await this.deps.contexts.list()).find(c => c.agent_id === cid);
			if (matchedCtx !== undefined) contextAlias = matchedCtx.alias;
		}
		const record: ActiveConversation = {
			conversation_id: cid,
			context_alias: contextAlias,
			is_main: !isSubagent,
			is_subagent: isSubagent,
			...(input.transcriptPath ? { transcript_path: input.transcriptPath } : existing?.transcript_path ? { transcript_path: existing.transcript_path } : {}),
			...(input.workspace ? { workspace: input.workspace } : existing?.workspace ? { workspace: existing.workspace } : {}),
			last_seen_at: now,
		};
		await this.deps.storage.transaction(tx => tx.put(`${CONV_PREFIX}${encodeURIComponent(cid)}`, record));
		return record;
	}

	async recordTodo(entry: Entry): Promise<void> {
		const existing = await this.listTodos();
		const decision = await this.deps.decider.decideTodo(entry, existing.map(t => t.title));
		if (decision.action === "none") return;
		const now = new Date().toISOString();
		if (decision.action === "associate" && decision.title) {
			const hit = existing.find(item => item.title === decision.title);
			if (hit !== undefined) {
				const next: OnlyneTodoEntry = {
					...hit,
					explanation: `${hit.explanation ? `${hit.explanation}; ` : ""}followed by ${threadKey(entry.thread_ref)}#${entry.seq}`,
					updated_at: now,
				};
				await this.deps.storage.transaction(tx => tx.put(`${TODO_PREFIX}${encodeURIComponent(hit.id)}`, next));
				this.deps.onChanged();
			}
			return;
		}
		if (decision.action === "append" && decision.title) {
			const todoId = `td-${crypto.randomUUID().slice(0, 8)}`;
			const row: OnlyneTodoEntry = {
				id: todoId,
				phase: decision.phase ?? "Inbound",
				title: decision.title,
				status: "pending",
				source_sender: entry.message.sender,
				source_id: entry.message.id,
				thread_key: threadKey(entry.thread_ref),
				...(decision.explanation ? { explanation: decision.explanation } : {}),
				updated_at: now,
			};
			await this.deps.storage.transaction(tx => tx.put(`${TODO_PREFIX}${encodeURIComponent(todoId)}`, row));
			this.deps.onChanged();
		}
	}

	async inject(
		entry: Entry,
		explicitContext?: string,
		plan?: DeliveryPlan,
	): Promise<{ context: string; consumed: boolean; outcome: "delivered" | "queued" }> {
		const decision = await this.deps.decider.decideInbound(entry, explicitContext);
		let contextAlias = decision.context;
		let ctxRecord = await this.deps.contexts.find(contextAlias);
		if (contextAlias !== "main" && ctxRecord === undefined) {
			ctxRecord = await this.deps.contexts.allocate(
				entry.thread_ref,
				decision.summary ?? entry.message.msg.slice(0, 80),
				entry.message,
			);
			contextAlias = ctxRecord.alias;
		} else if (contextAlias === "main") {
			ctxRecord = await this.deps.contexts.ensureMain();
		}

		const key = slotKey(entry.message.sender, entry.thread_ref, entry.seq, contextAlias);
		const already = await this.deps.storage.transaction(tx => tx.get<DeliveryPendingEntry>(key));
		if (already !== undefined) {
			return { context: contextAlias, consumed: false, outcome: "queued" };
		}

		const token = crypto.randomUUID().replace(/-/g, "").slice(0, 12);
		const now = new Date().toISOString();
		const pending: DeliveryPendingEntry = {
			id: key,
			token,
			context_alias: contextAlias,
			sender: entry.message.sender,
			message_id: entry.message.id,
			thread_ref: entry.thread_ref,
			delivery_kind: entry.message.delivery_kind,
			text: entry.message.msg,
			plan_response: plan?.response ?? "async",
			plan_execution: plan?.execution ?? decision.execution,
			attempt: 1,
			created_at: now,
		};

		const wakeMode = this.deps.config().wake?.mode ?? "auto";
		const conversations = await this.listConversations();
		let targetConv = conversations.find(c => c.context_alias === contextAlias)
			?? (contextAlias === "main" ? conversations.find(c => c.is_main) : undefined);

		let woke = false;
		if (wakeMode !== "hook" && this.deps.bridge !== undefined) {
			const promptText = formatInboundPrompt(entry, token, contextAlias);
			if (decision.execution === "new" && targetConv === undefined) {
				const spawned = await this.deps.bridge.newConversation(promptText, {
					title: `Onlyne: ${entry.message.sender.split(":").at(-1)}`,
				}).catch(() => ({ ok: false as const, conversationId: undefined }));
				if (spawned.ok && spawned.conversationId) {
					woke = true;
					pending.conversation_id = spawned.conversationId;
					pending.woke_at = now;
					await this.deps.contexts.bind(contextAlias, { agent_id: spawned.conversationId });
					await this.deps.storage.transaction(tx => tx.put(`${CONV_PREFIX}${encodeURIComponent(spawned.conversationId!)}`, {
						conversation_id: spawned.conversationId!,
						context_alias: contextAlias,
						is_main: false,
						is_subagent: true,
						last_seen_at: now,
					} satisfies ActiveConversation));
				}
			} else if (targetConv !== undefined) {
				pending.conversation_id = targetConv.conversation_id;
				const ok = await this.deps.bridge.sendMessage(targetConv.conversation_id, promptText).catch(() => false);
				if (ok) {
					woke = true;
					pending.woke_at = now;
				}
			}
		}

		await this.deps.storage.transaction(tx => tx.put(key, pending));
		this.armEscalation(pending, entry);
		this.deps.onChanged();
		return {
			context: contextAlias,
			consumed: false,
			outcome: woke ? "delivered" : "queued",
		};
	}

	private armEscalation(pending: DeliveryPendingEntry, entry: Entry): void {
		if (pending.context_alias === "main") return;
		const existing = this.escalateTimers.get(pending.id);
		if (existing !== undefined) clearTimeout(existing);
		const delay = this.deps.config().wake?.escalate_ms ?? DEFAULT_ESCALATE_MS;
		const timer = setTimeout(() => {
			this.escalateTimers.delete(pending.id);
			void this.escalateToMain(pending, entry).catch(() => undefined);
		}, delay);
		this.escalateTimers.set(pending.id, timer);
	}

	private async escalateToMain(pending: DeliveryPendingEntry, entry: Entry): Promise<void> {
		const current = await this.deps.storage.transaction(tx => tx.get<DeliveryPendingEntry>(pending.id));
		if (current === undefined) return;
		const mainSlot = slotKey(current.sender, current.thread_ref, entry.seq, "main");
		const escalated: DeliveryPendingEntry = {
			...current,
			id: mainSlot,
			context_alias: "main",
			plan_execution: "existing",
			attempt: current.attempt + 1,
		};
		await this.deps.storage.transaction(async tx => {
			await tx.delete(pending.id);
			await tx.put(mainSlot, escalated);
		});
		if (this.deps.bridge !== undefined && (this.deps.config().wake?.mode ?? "auto") !== "hook") {
			const convs = await this.listConversations();
			const mainConv = convs.find(c => c.is_main);
			if (mainConv !== undefined) {
				await this.deps.bridge.sendMessage(
					mainConv.conversation_id,
					formatInboundPrompt(entry, escalated.token, "main"),
				).catch(() => undefined);
			}
		}
		this.deps.log?.("onlyne.inbound.escalated_to_main", {
			thread: threadKey(entry.thread_ref),
			seq: entry.seq,
		});
		this.deps.onChanged();
	}

	async confirmPendingByToken(token: string): Promise<boolean> {
		const pending = await this.listPending();
		const hit = pending.find(item => item.token === token);
		if (hit === undefined) return false;
		return this.confirmEntry(hit);
	}

	private async resolveEntryForPending(pending: DeliveryPendingEntry): Promise<Entry | undefined> {
		const inbox = await this.deps.node.messages.inbox(this.deps.node.auth, { page_size: 200 }).catch(() => undefined);
		const fromInbox = inbox?.items.find(
			row => row.attention.entry?.message.sender === pending.sender && row.attention.entry?.message.id === pending.message_id,
		)?.attention.entry;
		if (fromInbox !== undefined) return fromInbox;
		const page = await this.deps.node.client.threadEntries(pending.thread_ref, {
			attention: "thread",
			page_size: 100,
		}).catch(() => undefined);
		return page?.items.find(
			item => item.message.sender === pending.sender && item.message.id === pending.message_id,
		);
	}

	private async confirmEntry(pending: DeliveryPendingEntry): Promise<boolean> {
		const timer = this.escalateTimers.get(pending.id);
		if (timer !== undefined) {
			clearTimeout(timer);
			this.escalateTimers.delete(pending.id);
		}
		const entry = await this.resolveEntryForPending(pending);
		await this.deps.storage.transaction(tx => tx.delete(pending.id));
		if (entry !== undefined) {
			await this.deps.node.client.confirmConsumption(entry, pending.context_alias, "fragment");
		}
		this.deps.onChanged();
		return true;
	}

	async processHookEvent(input: HookEventInput): Promise<HookResultPayload> {
		const cid = input.conversationId?.trim();
		const conv = cid
			? await this.registerConversation({
					conversationId: cid,
					transcriptPath: input.transcriptPath,
					workspace: input.workspacePaths?.[0] ?? input.cwd,
				})
			: undefined;

		const pending = await this.listPending();
		const transcriptText = input.transcriptPath ? await readTailUtf8(input.transcriptPath) : "";

		// Step A: Verify consumption when token already appeared inside completed transcript steps.
		if (transcriptText.length > 0) {
			for (const item of pending) {
				const tag = tokenTag(item.token);
				if (!transcriptText.includes(tag)) continue;
				const seenKey = `${CONSUMED_TRANSCRIPT_PREFIX}${item.token}`;
				const firstSeen = await this.deps.storage.transaction(tx => tx.get<{ count: number }>(seenKey));
				if (input.hookEventName === "Stop" || input.hookEventName === "PreInvocation" || (firstSeen !== undefined && firstSeen.count >= 1)) {
					await this.confirmEntry(item);
					await this.deps.storage.transaction(tx => tx.delete(seenKey));
				} else {
					await this.deps.storage.transaction(tx => tx.put(seenKey, { count: 1 }));
				}
			}
		}

		// Step B: Inject remaining deliveries targeting this conversation's context (not yet injected into this transcript).
		if (input.hookEventName === "PostToolUse" || input.hookEventName === "Stop") {
			return {};
		}

		const activeAlias = conv?.context_alias ?? "main";
		const refreshedPending = await this.listPending();
		const relevant = refreshedPending.filter(item => {
			if (item.context_alias !== activeAlias && !(conv?.is_main && item.context_alias === "main")) return false;
			if (transcriptText.includes(tokenTag(item.token))) return false;
			return true;
		});

		const injectSteps: Array<{ systemMessage: { systemMessage: string } }> = [];
		for (const item of relevant) {
			const entry = await this.resolveEntryForPending(item);
			const body = entry
				? formatInboundPrompt(entry, item.token, item.context_alias)
				: `${tokenTag(item.token)} Onlyne inbound message from ${item.sender} in ${threadKey(item.thread_ref)}: ${item.text}`;
			injectSteps.push({ systemMessage: { systemMessage: body } });
		}

		if (injectSteps.length > 0) {
			return { injectSteps };
		}
		return {};
	}

	close(): void {
		for (const timer of this.escalateTimers.values()) clearTimeout(timer);
		this.escalateTimers.clear();
	}
}
