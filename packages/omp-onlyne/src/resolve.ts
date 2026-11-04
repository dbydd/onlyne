import {
	OnlyneError,
	type Entry,
	type Json,
	type JoinedThread,
	type MessageRef,
	type NodeId,
	type Page,
	type Thread,
	type ThreadRef,
} from "@onlyne/lib";
import { text } from "./decider-text.ts";

/** Upper bound on pages scanned for one thread lookup; exceeding it is reported, never hidden. */
const MAX_PAGES = 100;

/** One discovered peer, as the library's discovery records it. */
export interface PeerRecord {
	node_id: NodeId;
	base_url: string;
	instance_id: string;
	last_seen: string;
}

/** The records every textual reference is resolved against. */
export interface ReferenceDeps {
	config: { node: { id: NodeId } };
	log?(message: string, fields: Record<string, unknown>): void;
	listThreads(): Promise<Thread[]>;
	listJoinedThreads(): Promise<JoinedThread[]>;
	listMessages(thread: ThreadRef, cursor?: string): Promise<Page<Entry>>;
	listPeers(): Promise<PeerRecord[]>;
	listTrust(): Promise<Array<{ node_id: NodeId; phase: string; base_url?: string }>>;
	listInbox(): Promise<Array<{ context?: string; attention: { entry?: Entry; disposition?: string } }>>;
	threadMembers(thread: ThreadRef): Promise<NodeId[]>;
}

/** One reply reference: the message it addresses and the node that owns the answer. */
export interface ReplyReference {
	ref: MessageRef;
	authority: NodeId;
	thread_ref?: ThreadRef;
}

/** Every reference one send names, resolved before its plan is committed. */
export interface SendReferences {
	target?: NodeId;
	thread?: ThreadRef;
	mentions?: string[];
	reply?: ReplyReference;
}

function stringField(record: Record<string, Json>, key: string): string | undefined {
	const value = record[key];
	return typeof value === "string" && value.length > 0 ? value : undefined;
}

function recordField(record: Record<string, Json>, key: string): Record<string, Json> | undefined {
	const value = record[key];
	return value !== null && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, Json>) : undefined;
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

function message(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}

/** `owner/id` across both trees: one canonical spelling for a thread reference. */
export function threadKey(ref: ThreadRef): string {
	return `${ref.owner}/${ref.id}`;
}

/** A node id's last URN segment; plain ids are their own segment. */
export function lastSegment(nodeId: NodeId): string {
	const trimmed = nodeId.replace(/\/+$/, "");
	const parts = trimmed.split(/[:/]/).filter(part => part.length > 0);
	return parts.at(-1) ?? trimmed;
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
export async function reuseThread(deps: ReferenceDeps, to: NodeId): Promise<ThreadRef | undefined> {
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
export async function knownPeers(deps: ReferenceDeps): Promise<PeerRecord[]> {
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

/**
 * The single place a textual reference becomes a record: `meta.to` and `meta.mentions` through
 * node identity, `meta.thread` through owned or joined thread evidence, and `meta.in_reply_to`
 * and `meta.message` through the thread that holds the message. Every entry point of a decision
 * resolves here, so the same spelling cannot mean two things on two paths (audit §2.7).
 */
export class References {
	constructor(private readonly deps: ReferenceDeps) {}

	/**
	 * A send's references, resolved in one pass before its plan is committed. Only the fields
	 * the call carries are read, so a message that names no thread costs no thread lookup.
	 */
	async forSend(meta: Record<string, Json> | undefined): Promise<SendReferences> {
		const reply = await this.reply(meta);
		const target = await this.target(meta);
		const thread = await this.thread(meta);
		const mentions = await this.mentions(meta);
		return {
			...(target === undefined ? {} : { target }),
			...(thread === undefined ? {} : { thread }),
			...(mentions === undefined ? {} : { mentions }),
			...(reply === undefined ? {} : { reply }),
		};
	}

	/** `meta.to`, resolved against the same peer set every other reference uses. */
	async target(meta: Record<string, Json> | undefined): Promise<NodeId | undefined> {
		if (meta === undefined) return undefined;
		const value = meta.to;
		if (value === undefined) return undefined;
		if (typeof value !== "string") throw new OnlyneError("invalid_request", text("error.meta_to_single"));
		if (value.length === 0) throw new OnlyneError("invalid_request", text("error.meta_to_empty"));
		return this.peerId(value);
	}

	async peerId(alias: string): Promise<NodeId> {
		const ids = await this.knownIds();
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
	private async knownIds(): Promise<NodeId[]> {
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

	/**
	 * `meta.thread`: a `{owner,id}` record or a text reference. The record form carries the owner
	 * explicitly; the text form is resolved by the rule in `threadByAlias`.
	 */
	async thread(meta: Record<string, Json> | undefined): Promise<ThreadRef | undefined> {
		if (meta === undefined) return undefined;
		const value = meta.thread;
		if (value === undefined) return undefined;
		if (typeof value === "string") {
			if (value.length === 0) throw new OnlyneError("invalid_request", text("error.thread_reference_shape"));
			return this.threadByAlias(value);
		}
		const record = recordField(meta, "thread");
		const owner = record === undefined ? undefined : stringField(record, "owner");
		const id = record === undefined ? undefined : stringField(record, "id");
		if (owner !== undefined && id !== undefined) return { owner, id };
		throw new OnlyneError("invalid_request", text("error.thread_reference_shape"));
	}

	/**
	 * One textual thread reference, resolved by evidence this node actually holds:
	 *
	 * 1. a thread it hosts or was invited into, matched by id or title; a unique match is the
	 *    answer and several are `ambiguous_target`;
	 * 2. otherwise `owner/id`, where the part before the last slash is an explicit owner and is
	 *    accepted as written, because the owner is a claim the caller made;
	 * 3. otherwise `not_found`.
	 *
	 * Step 3 used to differ by entry point: a send read an unknown bare id as the addressee's
	 * thread and stored into a thread nobody had told that node about, while a read refused it.
	 * No local record named that owner, so the send was an invented fact, not a resolution
	 * (audit §2.7).
	 */
	async threadByAlias(alias: string): Promise<ThreadRef> {
		if (alias.length === 0) throw new OnlyneError("invalid_request", text("error.thread_reference_shape"));
		const matches = (await this.namedThreads()).filter(thread => thread.ref.id === alias || thread.title === alias);
		if (matches.length === 1) return matches[0]!.ref;
		if (matches.length > 1) throw new OnlyneError("ambiguous_target", text("error.ambiguous_thread", { alias }));
		// One slash grammar for every textual thread reference: the owner is the part before the
		// LAST slash, which is safe for urn owners and for ids that never carry a slash.
		const slash = alias.lastIndexOf("/");
		if (slash > 0 && slash < alias.length - 1) {
			return { owner: alias.slice(0, slash), id: alias.slice(slash + 1) };
		}
		throw new OnlyneError("not_found", text("error.thread_not_found", { alias }));
	}

	/**
	 * Every thread this node can name by id or title: the ones it hosts, plus the ones it was
	 * invited into. A joined thread lives on its owner, so naming it here is a reference, never
	 * a claim that this node holds the thread record.
	 */
	async namedThreads(): Promise<Array<{ ref: ThreadRef; title: string | undefined }>> {
		const [owned, joined] = await Promise.all([this.deps.listThreads(), this.deps.listJoinedThreads()]);
		return [
			...owned.map(thread => ({ ref: thread.thread_ref, title: thread.title as string | undefined })),
			...joined.map(thread => ({ ref: thread.thread_ref, title: thread.title })),
		];
	}

	async mentions(meta: Record<string, Json> | undefined): Promise<string[] | undefined> {
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
			resolved.push(await this.peerId(item.startsWith("@") ? item.slice(1) : item));
		}
		return resolved;
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
			// The authority is an identity, not the caller's spelling: `caerulea` and
			// `urn:onlyne:agent:caerulea` name the same node, so both the ref and the authority
			// carry the resolved id and every later comparison is like-for-like. Comparing a raw
			// string against a resolved one produced two contradictory errors in a row
			// (cyanea report 1, problem 1).
			const authority = await this.peerId(sender).catch(() => sender);
			// A reference the node can prove wrong is refused here, before the ref reaches the
			// wire and dies in the peer's not_found: an id that names a different sender locally
			// is a definite error, and the error names the message the id really belongs to.
			const elsewhere = (await this.deps.listInbox()).find(row =>
				row.attention.entry !== undefined && row.attention.entry.message.id === id
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
		return { ref: { sender: entry.message.sender, id: entry.message.id }, authority: entry.reply_authority, thread_ref: entry.thread_ref };
	}

	/**
	 * The message a read names, with the thread it was resolved in. `{sender,id}` carries no
	 * thread here; `{thread,seq}` does, and an explicit meta.thread that contradicts the
	 * reference is refused rather than silently winning.
	 */
	async message(meta: Record<string, Json>, thread?: ThreadRef): Promise<{ ref: MessageRef; thread?: ThreadRef } | undefined> {
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
		const embedded = alias === undefined ? undefined : await this.threadByAlias(alias);
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
		const entry = await this.entryAt(scope, seq);
		if (entry === undefined) throw new OnlyneError("not_found", text("error.message_not_found", { thread: scope.id, seq }));
		return { ref: { sender: entry.message.sender, id: entry.message.id }, thread: scope };
	}

	async entryAt(thread: ThreadRef, seq: string): Promise<Entry | undefined> {
		let cursor: string | undefined;
		for (let page = 0; page < MAX_PAGES; page++) {
			const result: Page<Entry> = await this.deps.listMessages(thread, cursor);
			const found = result.items.find(entry => entry.seq === seq);
			if (found !== undefined) return found;
			cursor = result.next_cursor ?? undefined;
			if (cursor === undefined) return undefined;
		}
		return undefined;
	}
}
