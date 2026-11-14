import {
	OnlyneError,
	trustPhase,
	type DiscoveryRecord,
	type Entry,
	type JoinedThread,
	type Json,
	type LocalAttention,
	type NodeId,
	type Page,
	type Thread,
	type ThreadRef,
	type TrustEntry,
} from "@onlyne/lib";
import { text } from "./decider-text.ts";

export interface PeerRecord {
	node_id: NodeId;
	base_url?: string;
	last_seen: string;
}

export interface ReplyReference {
	ref: { sender: NodeId; id: string };
	authority: NodeId;
	thread_ref?: ThreadRef;
}

export interface ResolveDeps {
	config: { node: { id: NodeId }; region: string };
	listThreads(cursor?: string): Promise<Page<Thread>>;
	listJoined(cursor?: string): Promise<Page<JoinedThread>>;
	listInbox(): Promise<LocalAttention[]>;
	listMessages(thread: ThreadRef, cursor?: string): Promise<Page<Entry>>;
	listDiscovery(): Promise<DiscoveryRecord[]>;
	listTrust(): Promise<TrustEntry[]>;
	peerBase(nodeId: NodeId): Promise<string | undefined>;
}

const MAX_PAGES = 20;

export function threadKey(ref: ThreadRef): string {
	return `${ref.owner}/${ref.id}`;
}

export function stringField(record: Record<string, Json> | undefined, field: string): string | undefined {
	const value = record?.[field];
	return typeof value === "string" && value.trim().length > 0 ? value.trim() : undefined;
}

export function seqField(record: Record<string, Json> | undefined, field: string): string | undefined {
	const value = record?.[field];
	if (typeof value === "number" && Number.isInteger(value) && value >= 0) return String(value);
	if (typeof value === "string" && /^(0|[1-9][0-9]*)$/.test(value.trim())) return value.trim();
	return undefined;
}

export async function reuseThread(
	deps: Pick<ResolveDeps, "listThreads" | "listJoined">,
	target: NodeId,
): Promise<ThreadRef | undefined> {
	let cursor: string | undefined;
	for (let i = 0; i < MAX_PAGES; i++) {
		const owned = await deps.listThreads(cursor);
		for (const t of owned.items) {
			if (t.state !== "open" || t.thread_ref.id === "trust") continue;
			if (t.thread_ref.owner === target || t.title.includes(target)) return t.thread_ref;
		}
		cursor = owned.next_cursor ?? undefined;
		if (cursor === undefined) break;
	}
	cursor = undefined;
	for (let i = 0; i < MAX_PAGES; i++) {
		const joined = await deps.listJoined(cursor);
		for (const j of joined.items) {
			if (j.state === "closed") continue;
			if (j.thread_ref.owner === target) return j.thread_ref;
		}
		cursor = joined.next_cursor ?? undefined;
		if (cursor === undefined) break;
	}
	return undefined;
}

export async function knownPeers(deps: Pick<ResolveDeps, "config" | "listDiscovery" | "listTrust" | "peerBase">): Promise<PeerRecord[]> {
	const byId = new Map<NodeId, PeerRecord>();
	const [discovery, trust] = await Promise.all([
		deps.listDiscovery().catch(() => [] as DiscoveryRecord[]),
		deps.listTrust().catch(() => [] as TrustEntry[]),
	]);
	for (const row of discovery) {
		if (row.node_id === deps.config.node.id) continue;
		if (row.membership?.state === "left") continue;
		byId.set(row.node_id, {
			node_id: row.node_id,
			base_url: row.base_url,
			last_seen: row.last_seen,
		});
	}
	for (const row of trust) {
		if (row.node_id === deps.config.node.id) continue;
		if (trustPhase(row) === "revoked") continue;
		const existing = byId.get(row.node_id);
		const lastSeen = existing?.last_seen ?? row.last_checked ?? row.first_seen;
		byId.set(row.node_id, {
			node_id: row.node_id,
			base_url: existing?.base_url ?? row.base_url ?? await deps.peerBase(row.node_id),
			last_seen: lastSeen,
		});
	}
	return [...byId.values()].sort((a, b) => a.node_id.localeCompare(b.node_id));
}

export class References {
	constructor(private readonly deps: ResolveDeps) {}

	async allThreads(): Promise<Array<{ thread_ref: ThreadRef; title: string; state: "open" | "closed"; next_seq: string }>> {
		const seen = new Map<string, { thread_ref: ThreadRef; title: string; state: "open" | "closed"; next_seq: string }>();
		let cursor: string | undefined;
		for (let i = 0; i < MAX_PAGES; i++) {
			const page = await this.deps.listThreads(cursor);
			for (const row of page.items) {
				if (row.thread_ref.id === "trust") continue;
				seen.set(threadKey(row.thread_ref), {
					thread_ref: row.thread_ref,
					title: row.title,
					state: row.state,
					next_seq: row.next_seq,
				});
			}
			cursor = page.next_cursor ?? undefined;
			if (cursor === undefined) break;
		}
		cursor = undefined;
		for (let i = 0; i < MAX_PAGES; i++) {
			const page = await this.deps.listJoined(cursor);
			for (const row of page.items) {
				const k = threadKey(row.thread_ref);
				if (seen.has(k)) continue;
				seen.set(k, {
					thread_ref: row.thread_ref,
					title: row.title ?? row.thread_ref.id,
					state: row.state ?? "open",
					next_seq: row.last_seq,
				});
			}
			cursor = page.next_cursor ?? undefined;
			if (cursor === undefined) break;
		}
		return [...seen.values()];
	}

	async peerId(raw: string): Promise<NodeId> {
		const trimmed = raw.trim();
		if (trimmed.length === 0) throw new OnlyneError("invalid_request", text("error.peer_not_found", { raw }));
		const peers = await knownPeers(this.deps);
		const exact = peers.find(peer => peer.node_id === trimmed);
		if (exact !== undefined) return exact.node_id;
		const normalized = trimmed.startsWith("urn:") ? trimmed : `urn:onlyne:agent:${encodeURIComponent(trimmed)}`;
		const normHit = peers.find(peer => peer.node_id === normalized);
		if (normHit !== undefined) return normHit.node_id;
		const aliasMatches = peers.filter(peer => {
			const suffix = peer.node_id.split(":").at(-1);
			return suffix === trimmed || decodeURIComponent(suffix ?? "") === trimmed;
		});
		if (aliasMatches.length === 1) return aliasMatches[0]!.node_id;
		if (aliasMatches.length > 1) {
			throw new OnlyneError("ambiguous_target", text("error.peer_ambiguous", {
				raw: trimmed,
				matches: aliasMatches.map(m => m.node_id).join(", "),
			}));
		}
		if (trimmed.startsWith("urn:")) return trimmed;
		throw new OnlyneError("not_found", text("error.peer_not_found", { raw: trimmed }));
	}

	async target(meta: Record<string, Json> | undefined): Promise<NodeId | undefined> {
		const raw = stringField(meta, "to");
		if (raw === undefined) return undefined;
		return this.peerId(raw);
	}

	async mentions(meta: Record<string, Json> | undefined): Promise<NodeId[] | undefined> {
		const value = meta?.mentions;
		if (!Array.isArray(value)) return undefined;
		const out: NodeId[] = [];
		for (const item of value) {
			if (typeof item !== "string" || item.trim().length === 0) continue;
			out.push(await this.peerId(item).catch(() => item.trim()));
		}
		return out.length > 0 ? [...new Set(out)] : undefined;
	}

	async thread(meta: Record<string, Json> | undefined): Promise<ThreadRef | undefined> {
		const value = meta?.thread;
		if (value === undefined) return undefined;
		if (typeof value === "string") return this.threadByAlias(value);
		if (value !== null && typeof value === "object" && !Array.isArray(value)) {
			const obj = value as Record<string, Json>;
			const owner = stringField(obj, "owner");
			const id = stringField(obj, "id");
			if (owner !== undefined && id !== undefined) return { owner, id };
		}
		throw new OnlyneError("invalid_request", text("error.thread_ref_invalid", { raw: JSON.stringify(value) }));
	}

	async threadByAlias(alias: string): Promise<ThreadRef> {
		const trimmed = alias.trim();
		const slash = trimmed.lastIndexOf("/");
		if (slash > 0) {
			const owner = trimmed.slice(0, slash);
			const id = trimmed.slice(slash + 1);
			if (owner.startsWith("urn:") && id.length > 0) return { owner, id };
		}
		const threads = await this.allThreads();
		const exactKey = threads.find(row => threadKey(row.thread_ref) === trimmed);
		if (exactKey !== undefined) return exactKey.thread_ref;
		const exactId = threads.filter(row => row.thread_ref.id === trimmed || row.title === trimmed);
		if (exactId.length === 1) return exactId[0]!.thread_ref;
		const prefixMatches = threads.filter(row => row.thread_ref.id.startsWith(trimmed));
		if (prefixMatches.length === 1) return prefixMatches[0]!.thread_ref;
		throw new OnlyneError("not_found", text("error.thread_not_found", { alias }));
	}

	async reply(meta: Record<string, Json> | undefined): Promise<ReplyReference | undefined> {
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
			const authority = await this.peerId(sender).catch(() => sender);
			const elsewhere = (await this.deps.listInbox()).find(row =>
				row.attention.entry !== undefined
				&& row.attention.entry.message.id === id
				&& row.attention.entry.message.sender !== authority);
			if (elsewhere !== undefined) {
				const actual = elsewhere.attention.entry!.message.sender;
				throw new OnlyneError("invalid_request", text("error.reply_reference_conflict", { named: sender, actual }));
			}
			return { ref: { sender: authority, id }, authority };
		}
		const threadAlias = stringField(record, "thread");
		const seq = seqField(record, "seq");
		if (threadAlias === undefined) throw new OnlyneError("invalid_request", text("error.in_reply_to_shape"));
		if (seq === undefined) throw new OnlyneError("invalid_request", text("error.in_reply_to_seq"));
		const thread = await this.threadByAlias(threadAlias);
		const entry = await this.entryAt(thread, seq);
		if (entry === undefined) throw new OnlyneError("not_found", text("error.message_not_found", { thread: threadAlias, seq }));
		return {
			ref: { sender: entry.message.sender, id: entry.message.id },
			authority: entry.reply_authority,
			thread_ref: entry.thread_ref,
		};
	}

	async message(meta: Record<string, Json>, thread?: ThreadRef): Promise<{ ref: { sender: NodeId; id: string }; thread?: ThreadRef } | undefined> {
		const value = meta.message;
		const record = value !== null && typeof value === "object" && !Array.isArray(value)
			? value as Record<string, Json>
			: undefined;
		const sender = record === undefined ? undefined : stringField(record, "sender");
		const id = record === undefined ? undefined : stringField(record, "id");
		if (sender !== undefined && id !== undefined) {
			return { ref: { sender, id }, ...(thread === undefined ? {} : { thread }) };
		}
		const alias = record === undefined ? undefined : stringField(record, "thread");
		const embedded = alias === undefined ? undefined : await this.threadByAlias(alias);
		if (thread !== undefined && embedded !== undefined && threadKey(thread) !== threadKey(embedded)) {
			throw new OnlyneError("invalid_request", text("error.message_thread_conflict", {
				thread: threadKey(thread),
				embedded: threadKey(embedded),
			}));
		}
		const scope = thread ?? embedded;
		if (scope === undefined) return undefined;
		const seq = (record === undefined ? undefined : seqField(record, "seq")) ?? seqField(meta, "seq");
		if (seq === undefined) return undefined;
		const entry = await this.entryAt(scope, seq);
		if (entry === undefined) throw new OnlyneError("not_found", text("error.message_not_found", { thread: scope.id, seq }));
		return { ref: { sender: entry.message.sender, id: entry.message.id }, thread: scope };
	}

	async entryAt(thread: ThreadRef, seq: string): Promise<Entry | undefined> {
		let cursor: string | undefined;
		for (let i = 0; i < MAX_PAGES; i++) {
			const page = await this.deps.listMessages(thread, cursor);
			const found = page.items.find(item => item.seq === seq);
			if (found !== undefined) return found;
			cursor = page.next_cursor ?? undefined;
			if (cursor === undefined) return undefined;
		}
		return undefined;
	}
}
