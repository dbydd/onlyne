import {
	OnlyneError,
	trustPhase,
	type Conversation,
	type Entry,
	type HistoryMessage,
	type HistoryQuery,
	type LocalAttention,
	type MessageRef,
	type OperationRecord,
	type Outbound,
	type Page,
	type Resource,
	type ResourceEvent,
	type ResourceInventory,
	type ResourceRef,
	type Thread,
	type ThreadRef,
	type TrustPhase,
} from "@onlyne/lib";
import { createTranslator } from "@onlyne/node-host/i18n";
import {
	InternalUrlRouter,
	parseInternalUrl,
	type InternalResource,
	type InternalUrl,
	type ProtocolHandler,
	type ResolveContext,
} from "@oh-my-pi/pi-coding-agent/internal-urls";
import { resourceQuery } from "./resources.ts";
import { getRuntime, type PluginRuntime } from "./runtime.ts";
import type { ContextRecord } from "./types.ts";
import { urlsText } from "./urls-text.ts";

type Caller = { main: boolean; agent_id?: string };
type View = { host: string; parts: string[]; params: URLSearchParams };
type Rendered = { content: string; contentType: InternalResource["contentType"] };
type UrlLike = Pick<URL, "hostname" | "pathname" | "search"> & { rawHost?: string; rawPathname?: string };

const t = createTranslator(urlsText);

/** Scan size for collecting a library page sequence; the visible page size is separate. */
const PAGE = 100;
/** A page loop that exceeds this is a bug in the source, not a reason to truncate silently. */
const MAX_PAGES = 10_000;
const DEFAULT_PAGE = 50;
const MAX_PAGE = 100;
const SNIPPET_CHARS = 96;
const TITLE_CHARS = 80;

const handler: ProtocolHandler = {
	scheme: "onlyne",
	spec: { backing: "virtual", selectors: "lines", immutable: true },

	async resolve(url: InternalUrl, context?: ResolveContext): Promise<InternalResource> {
		const runtime = getRuntime();
		if (runtime === undefined) throw new Error(t("notConfigured"));
		const rendered = await render(runtime, callerOf(runtime, context), viewOf(url));
		return { url: url.href, content: rendered.content, contentType: rendered.contentType, immutable: true };
	},

	/**
	 * Leaf documents for `read`, `grep` and `find`. The root expands into views and
	 * thread documents; a thread expands into its message documents; the inbox
	 * expands into the messages its visible attention items name; a conversation
	 * expands into its messages; a resource collection expands into its readable
	 * detail documents, and a log or operation collection into its entry records.
	 */
	async enumerate(url: InternalUrl, context?: ResolveContext): Promise<Array<{ url: string; content: string }>> {
		const runtime = getRuntime();
		if (runtime === undefined) throw new Error(t("notConfigured"));
		const caller = callerOf(runtime, context);
		const view = viewOf(url);
		if (view.host === "") {
			const urls = caller.agent_id === undefined
				? []
				: ["onlyne://inbox", "onlyne://contexts", "onlyne://outbound", "onlyne://conversations"];
			if (caller.main) urls.push("onlyne://resources");
			for (const thread of await visibleThreads(runtime, caller)) {
				urls.push(threadPath(thread.thread_ref));
			}
			return leafDocuments(runtime, caller, urls);
		}
		if (view.host === "threads" && view.parts.length === 0) {
			const urls = (await visibleThreads(runtime, caller)).map(thread => threadPath(thread.thread_ref));
			return leafDocuments(runtime, caller, urls);
		}
		if (view.host === "threads") {
			const target = threadTarget(view.parts);
			const thread = await findThread(runtime, caller, target.alias);
			if (target.seq !== undefined) {
				const entry = await entryAt(runtime, thread, target.seq);
				return [{ url: entryPath(thread.thread_ref, target.seq), content: `${JSON.stringify(entry, null, 2)}\n` }];
			}
			const entries = await allMessages(runtime, thread.thread_ref);
			return entries.map(entry => ({
				url: entryPath(thread.thread_ref, entry.seq),
				content: JSON.stringify(entry, null, 2),
			}));
		}
		if (view.host === "inbox") {
			return inboxLeaves(runtime, caller);
		}
		if ((view.host === "neighbors" || view.host === "peers") && view.parts.length === 0) {
			const urls = (await neighborRows(runtime)).map(row => `onlyne://neighbors/${encodeURIComponent(row.node_id)}`);
			return leafDocuments(runtime, caller, urls);
		}
		if (view.host === "conversations" && view.parts.length === 0) {
			const conversations = await visibleConversations(runtime, caller, conversationFilter(view).query);
			return leafDocuments(runtime, caller, conversations.map(conversation => conversationPath(conversation.id)));
		}
		if (view.host === "conversations") {
			const ref = messageRefFromParts(view.parts, t("conversationsShape"));
			const conversation = await authorizeConversation(runtime, caller, ref);
			return conversation.messages.map(message => ({
				url: messagePath(message.message_ref),
				content: `${JSON.stringify(message, null, 2)}\n`,
			}));
		}
		if (view.host === "messages") {
			const ref = messageRefFromParts(view.parts, t("messagesShape"));
			return leafDocuments(runtime, caller, [messagePath(ref)]);
		}
		if (view.host === "resources") return resourceLeaves(runtime, caller, view);
		return leafDocuments(runtime, caller, [`onlyne://${view.host}${view.parts.length === 0 ? "" : `/${view.parts.map(encodeURIComponent).join("/")}`}`]);
	},
};

/** Register once per process; another copy of the plugin reuses the existing handler. */
export function registerOnlyneUrls(): void {
	const router = InternalUrlRouter.instance();
	if (!router.canHandle("onlyne://")) router.register(handler);
}

function callerOf(runtime: PluginRuntime, context: ResolveContext | undefined): Caller {
	const agentId = context?.session?.getAgentId?.() ?? undefined;
	return { agent_id: agentId, main: agentId !== undefined && agentId === runtime.mainAgentId };
}

function viewOf(url: UrlLike): View {
	const host = (url.rawHost || url.hostname || "").toLowerCase();
	const raw = url.rawPathname ?? url.pathname;
	const parts = raw
		.split("/")
		.filter(part => part.length > 0)
		.map(part => {
			try {
				return decodeURIComponent(part);
			} catch {
				throw new Error(t("malformedEncoding", { path: raw }));
			}
		});
	return { host: host === "root" ? "" : host, parts, params: new URLSearchParams(url.search ?? "") };
}

function viewOfHref(href: string): View {
	return viewOf(parseInternalUrl(href));
}

function markdown(content: string): Rendered {
	return { content, contentType: "text/markdown" };
}

function json(value: unknown): Rendered {
	return { content: `${JSON.stringify(value, null, 2)}\n`, contentType: "application/json" };
}

function routeShape(shape: string): Error {
	return new Error(t("routeShape", { shape }));
}

async function render(runtime: PluginRuntime, caller: Caller, view: View): Promise<Rendered> {
	if (view.host === "") return markdown(await renderRoot(runtime, caller));
	if (view.host === "threads") return renderThreads(runtime, caller, view);
	if (view.host === "neighbors" || view.host === "peers") {
		requireSession(caller, "neighbors");
		if (view.parts.length > 1) throw routeShape(t("neighborsShape"));
		if (view.parts.length === 1) return json(await neighborView(runtime, view.parts[0]!));
		return markdown(await renderNeighbors(runtime, caller, view));
	}
	if (view.host === "contexts") {
		requireSession(caller, "contexts");
		if (view.parts.length > 0) throw routeShape(t("contextsShape"));
		return markdown(await renderContexts(runtime, caller, view));
	}
	if (view.host === "inbox") {
		requireSession(caller, "inbox");
		if (view.parts.length > 0) throw routeShape(t("inboxShape"));
		return renderInbox(runtime, caller, view);
	}
	if (view.host === "outbound") return renderOutbound(runtime, caller, view);
	if (view.host === "conversations") return renderConversations(runtime, caller, view);
	if (view.host === "messages") {
		requireSession(caller, "messages");
		const ref = messageRefFromParts(view.parts, t("messagesShape"));
		await authorizeConversation(runtime, caller, ref);
		return json(await runtime.node.history.message(runtime.node.auth, ref));
	}
	if (view.host === "resources") return renderResources(runtime, caller, view);
	throw new Error(t("unknownRoute", { host: view.host }));
}

function requireSession(caller: Caller, view: string): void {
	if (caller.agent_id !== undefined) return;
	throw new Error(t("privateSession", { view }));
}

/** Resource reads are the main session's: a subagent's slice never covers a peer's inventory. */
function requireMain(caller: Caller, view: string): void {
	if (caller.main) return;
	throw new Error(t("mainSessionOnly", { view }));
}

async function renderRoot(runtime: PluginRuntime, caller: Caller): Promise<string> {
	const lines = [
		t("rootTitle", { node: runtime.config.node.id }),
		"",
		t("rootRegion", { region: runtime.config.region }),
		t("rootEndpoint", { endpoint: runtime.endpoint }),
	];
	try {
		const domains = await runtime.node.domains.list(runtime.node.auth, { page_size: PAGE });
		const active = domains.items.filter(membership => membership.state === "active");
		lines.push(t("rootDomain", {
			domains: active.length === 0 ? t("rootDomainNone") : active.map(membership => membership.domain_id).join(", "),
		}));
	} catch (error) {
		lines.push(t("rootDomainUnavailable", { message: error instanceof Error ? error.message : String(error) }));
	}
	if (caller.agent_id === undefined) {
		lines.push("", t("rootPrivateViews"), t("rootResourceViews"));
		return `${lines.join("\n")}\n`;
	}
	const records = await scopedRecords(runtime, caller);
	const threads = await visibleThreads(runtime, caller);
	lines.push("", caller.main
		? t("rootContexts", { count: records.length })
		: t("rootContextsHeld", { agent: caller.agent_id ?? "", count: records.length }));
	for (const record of records) {
		const thread = record.thread_ref === undefined ? "" : t("suffixThread", { key: threadKey(record.thread_ref) });
		lines.push(t("rootContextRow", { alias: record.alias, state: record.state, summary: record.summary, thread }));
	}
	if (records.length === 0) lines.push(t("rootNoContexts"));
	lines.push("", t("rootThreads", { count: threads.length }));
	for (const thread of threads) {
		lines.push(t("rootThreadRow", {
			title: thread.title,
			key: threadKey(thread.thread_ref),
			state: thread.state,
			next: thread.next_seq,
		}));
	}
	if (threads.length === 0) lines.push(t("rootNoThreads"));
	lines.push(
		"",
		caller.main ? t("rootViewsResources") : t("rootViews"),
		t("rootConversation"),
		t("rootCollection"),
	);
	return `${lines.join("\n")}\n`;
}

async function renderThreads(runtime: PluginRuntime, caller: Caller, view: View): Promise<Rendered> {
	requireSession(caller, "threads");
	if (view.parts.length === 0) {
		const request = pageRequest(view);
		const threads = (await visibleThreads(runtime, caller))
			.slice()
			.sort((left, right) => threadKey(left.thread_ref).localeCompare(threadKey(right.thread_ref)));
		const index = await historyIndex(runtime, caller);
		const page = localPage(threads, request, cursorScope("threads", caller, ""), thread => threadKey(thread.thread_ref));
		const lines = [t("threadsTitle", { node: runtime.config.node.id }), ""];
		for (const thread of page.items) {
			const rows = index.get(threadKey(thread.thread_ref)) ?? [];
			const first = rows[0];
			const latest = rows.at(-1);
			const title = first === undefined ? thread.title : titleOf(first);
			lines.push(t("threadRow", {
				title,
				key: threadKey(thread.thread_ref),
				state: thread.state,
				next: thread.next_seq,
				count: rows.length,
			}));
			if (latest === undefined) {
				lines.push(t("threadNoHistory", { url: threadPath(thread.thread_ref) }));
			} else {
				lines.push(t("threadLatest", {
					direction: latest.direction,
					at: latest.at ?? t("noTime"),
					snippet: firstLine(latest.message.msg, SNIPPET_CHARS),
				}));
				lines.push(t("threadLinks", {
					thread: threadPath(thread.thread_ref),
					latest: messagePath(latest.message_ref),
					conversation: conversationPath(latest.message_ref),
				}));
			}
		}
		if (page.items.length === 0) lines.push(t("noThreads"));
		lines.push("", nextLink("threads", request, page.next));
		return markdown(`${lines.join("\n")}\n`);
	}
	const target = threadTarget(view.parts);
	const thread = await findThread(runtime, caller, target.alias);
	if (target.seq !== undefined) return messageView(runtime, thread, target.seq);
	return markdown(await renderThread(runtime, thread));
}

interface ThreadTarget {
	alias: string;
	seq?: string;
}

/**
 * A thread alias is one segment or two (`<owner>/<id>`), and the messages tail is
 * optional. The tail is found by name so an unencoded `<owner>/<id>/messages/<seq>`
 * and a percent-encoded `<alias>/messages/<seq>` both resolve.
 */
function threadTarget(parts: string[]): ThreadTarget {
	const tail = parts.indexOf("messages");
	if (tail > 0) {
		if (parts.length !== tail + 2) throw routeShape(t("threadSeqShape"));
		return { alias: parts.slice(0, tail).join("/"), seq: parts[tail + 1]! };
	}
	if (parts.length > 2) throw routeShape(t("threadAliasShape"));
	return { alias: parts.join("/") };
}

async function entryAt(runtime: PluginRuntime, thread: Thread, seq: string): Promise<Entry> {
	const entry = (await allMessages(runtime, thread.thread_ref)).find(item => item.seq === seq);
	if (entry === undefined) throw new Error(t("noMessageAt", { key: threadKey(thread.thread_ref), seq }));
	return entry;
}

async function messageView(runtime: PluginRuntime, thread: Thread, seq: string): Promise<Rendered> {
	return json(await entryAt(runtime, thread, seq));
}

async function renderThread(runtime: PluginRuntime, thread: Thread): Promise<string> {
	const rows = await threadMessages(runtime, thread.thread_ref);
	const title = rows.length === 0 ? thread.title : titleOf(rows[0]!);
	const lines = [
		t("threadTitle", { title }),
		"",
		t("threadMeta", {
			key: threadKey(thread.thread_ref),
			state: thread.state,
			next: thread.next_seq,
			count: rows.length,
		}),
		"",
		t("members"),
		"",
	];
	try {
		const described = await runtime.node.threads.describe(runtime.node.auth, thread.thread_ref);
		for (const member of described.members) {
			lines.push(t("memberRow", {
				peer: member.peer,
				state: member.state,
				access: member.access.length === 0 ? t("noAccess") : member.access.join(", "),
			}));
		}
		if (described.members.length === 0) lines.push(t("noMembers"));
	} catch (error) {
		lines.push(t("unavailable", { message: error instanceof Error ? error.message : String(error) }));
	}
	lines.push("", t("messages"), "");
	for (const row of rows) lines.push(renderHistoryRow(row, runtime.config.node.id));
	if (rows.length === 0) lines.push(t("noMessages"));
	return `${lines.join("\n")}\n`;
}

async function renderContexts(runtime: PluginRuntime, caller: Caller, view: View): Promise<string> {
	const request = pageRequest(view);
	const records = (await scopedRecords(runtime, caller))
		.slice()
		.sort((left, right) => left.alias.localeCompare(right.alias));
	const page = localPage(records, request, cursorScope("contexts", caller, ""), record => record.alias);
	const lines = [t("contextsTitle", { node: runtime.config.node.id }), ""];
	for (const record of page.items) {
		const thread = record.thread_ref === undefined ? "" : t("suffixThread", { key: threadKey(record.thread_ref) });
		const agent = record.agent_id === undefined ? "" : t("suffixAgent", { agent: record.agent_id });
		const source = record.source === undefined
			? ""
			: t("suffixSource", { ref: `${record.source.sender}/${record.source.id}` });
		lines.push(t("contextRow", { alias: record.alias, state: record.state, summary: record.summary, thread, agent, source }));
	}
	if (page.items.length === 0) lines.push(t("noContexts"));
	lines.push("", nextLink("contexts", request, page.next));
	return `${lines.join("\n")}\n`;
}

export interface NeighborRow {
	node_id: string;
	base_url: string;
	region?: string;
	last_seen?: string;
	verified: boolean;
	/** The exchange stage from `trustPhase`, or `no_exchange` when no key exchange has happened. */
	phase: TrustPhase | "no_exchange";
	fingerprint: string | null;
}

/** Every node this process has observed, with its trust state: the viewer and onlyne:// read this. */
export async function neighborRows(runtime: PluginRuntime): Promise<NeighborRow[]> {
	const records = await runtime.node.discovery.listRecords();
	const trust = new Map((await runtime.node.trust.list(runtime.node.auth)).map(entry => [entry.node_id, entry]));
	const at = Date.now();
	const byNode = new Map<string, NeighborRow>();
	for (const record of records) {
		if (record.node_id === runtime.config.node.id) continue;
		const verified = record.verified_at !== undefined && Date.parse(record.expires_at) > at;
		const current = byNode.get(record.node_id);
		if (current !== undefined && (current.verified || !verified)) continue;
		const entry = trust.get(record.node_id);
		byNode.set(record.node_id, {
			node_id: record.node_id,
			base_url: record.base_url,
			...(record.membership?.domain_id === undefined ? {} : { region: record.membership.domain_id }),
			...(record.last_seen === undefined ? {} : { last_seen: record.last_seen }),
			verified,
			phase: entry === undefined ? "no_exchange" : trustPhase(entry),
			fingerprint: entry?.fingerprint ?? null,
		});
	}
	return [...byNode.values()].sort((left, right) => left.node_id.localeCompare(right.node_id));
}

async function renderNeighbors(runtime: PluginRuntime, caller: Caller, view: View): Promise<string> {
	const request = pageRequest(view);
	const rows = await neighborRows(runtime);
	const page = localPage(rows, request, cursorScope("neighbors", caller, ""), row => row.node_id);
	const lines = [t("neighborsTitle", { node: runtime.config.node.id }), ""];
	for (const row of page.items) {
		const region = row.region === undefined ? t("noRegion") : row.region;
		const seen = row.last_seen === undefined ? "" : t("suffixLastSeen", { at: row.last_seen });
		const fp = row.fingerprint === null ? "" : t("suffixFingerprint", { fp: row.fingerprint });
		lines.push(t("neighborRow", {
			node: row.node_id,
			region,
			verified: row.verified ? t("verified") : t("unverified"),
			phase: row.phase,
			seen,
			fp,
		}));
	}
	if (page.items.length === 0) lines.push(t("noNeighbors"));
	lines.push("", nextLink("neighbors", request, page.next));
	return `${lines.join("\n")}\n`;
}

async function neighborView(runtime: PluginRuntime, node: string): Promise<NeighborRow> {
	const rows = await neighborRows(runtime);
	const row = rows.find(entry => entry.node_id === node);
	if (row === undefined) throw new Error(t("noNeighborNamed", { node }));
	return row;
}

async function inboxItems(runtime: PluginRuntime): Promise<LocalAttention[]> {
	return collect(cursor => runtime.node.messages.inbox(runtime.node.auth, { page_size: PAGE, ...(cursor === undefined ? {} : { cursor }) }));
}

async function renderInbox(runtime: PluginRuntime, caller: Caller, view: View): Promise<Rendered> {
	const request = pageRequest(view);
	const rows = (await visibleInbox(runtime, caller))
		.map(item => inboxRow(runtime, item))
		.sort((left, right) => left.position.localeCompare(right.position));
	const page = localPage(rows, request, cursorScope("inbox", caller, ""), row => row.position);
	const lines = [t("inboxTitle", { node: runtime.config.node.id }), ""];
	for (const row of page.items) lines.push(...row.lines);
	if (page.items.length === 0) lines.push(t("noAttention"));
	lines.push("", nextLink("inbox", request, page.next));
	return markdown(`${lines.join("\n")}\n`);
}

function inboxRow(runtime: PluginRuntime, item: LocalAttention): { position: string; lines: string[] } {
	const attention = item.attention;
	const entry = attention.entry;
	const direction = entry === undefined ? "in" : entry.message.sender === runtime.config.node.id ? "out" : "in";
	const at = entry?.at;
	const status = [attention.acked ? t("acked") : t("pending"), attention.disposition === "expired" ? t("expired") : ""]
		.filter(part => part.length > 0)
		.join(" · ");
	const ref = attention.message_ref;
	const position = `${at ?? ""}\u0000${threadKey(attention.thread_ref)}\u0000${attention.seq}`;
	const lines = [
		t("attentionRow", {
			key: threadKey(attention.thread_ref),
			seq: attention.seq,
			direction,
			author: attention.authenticated_author,
			at: at ?? t("noTime"),
			status,
		}),
	];
	if (entry !== undefined) lines.push(`  ${firstLine(entry.message.msg, SNIPPET_CHARS)}`);
	lines.push(t("messageLinks", {
		message: messagePath(ref),
		conversation: conversationPath(ref),
		entry: entryPath(attention.thread_ref, attention.seq),
	}));
	return { position, lines };
}

async function renderOutbound(runtime: PluginRuntime, caller: Caller, view: View): Promise<Rendered> {
	requireSession(caller, "outbound");
	const rows = await outboundRows(runtime, caller);
	if (view.parts.length === 0) {
		const request = pageRequest(view);
		const list = rows
			.map(outbound => outboundRow(outbound))
			.sort((left, right) => left.position.localeCompare(right.position));
		const page = localPage(list, request, cursorScope("outbound", caller, ""), row => row.position);
		const lines = [t("outboundTitle", { node: runtime.config.node.id }), ""];
		for (const row of page.items) lines.push(...row.lines);
		if (page.items.length === 0) lines.push(t("noOutbound"));
		lines.push("", nextLink("outbound", request, page.next));
		return markdown(`${lines.join("\n")}\n`);
	}
	if (view.parts.length !== 1) throw routeShape(t("outboundShape"));
	const found = rows.find(row => row.request.message.id === view.parts[0]);
	if (found === undefined) throw new Error(t("noOutboundNamed", { id: view.parts[0]! }));
	return json(await readableOutbound(runtime, found));
}

function outboundRow(outbound: Outbound): { position: string; lines: string[] } {
	const message = outbound.request.message;
	const ref: MessageRef = { sender: message.sender, id: message.id };
	const related = message.meta?.in_reply_to;
	const at = outbound.created_at ?? null;
	const status = [outbound.state, outbound.error?.code ?? ""].filter(part => part.length > 0).join(" · ");
	const reply = related === undefined ? "" : t("suffixReply", { ref: `${related.sender}/${related.id}` });
	const position = `${at ?? ""}\u0000${refKey(ref)}`;
	const lines = [
		t("outboundRow", {
			id: ref.id,
			endpoint: outbound.request.target_endpoint,
			at: at ?? t("noTime"),
			status,
			reply,
		}),
		`  ${firstLine(message.msg, SNIPPET_CHARS)}`,
		t("outboundLinks", { message: messagePath(ref), conversation: conversationPath(ref) }),
	];
	return { position, lines };
}

/**
 * The outbound record as its sender reads it back: the body itself (not a digest), the local
 * history row for it, and the reply the local conversation holds. Peer-owned threads are never
 * read here; `entry` is the local messages route.
 */
async function readableOutbound(runtime: PluginRuntime, outbound: Outbound): Promise<Record<string, unknown>> {
	const message = outbound.request.message;
	const ref: MessageRef = { sender: message.sender, id: message.id };
	const conversation = await runtime.node.history.conversation(runtime.node.auth, ref);
	const self = conversation.messages.find(item => item.message_ref.sender === ref.sender && item.message_ref.id === ref.id);
	const related = message.meta?.in_reply_to;
	const reply = conversation.messages.find(item => {
		const target = item.message.meta?.in_reply_to;
		return target !== undefined && target.sender === message.sender && target.id === message.id;
	});
	const landed = self?.thread_ref === undefined
		? undefined
		: { thread: threadKey(self.thread_ref), seq: self.seq, guarantee: self.guarantee };
	return {
		message_id: message.id,
		to: outbound.request.target_endpoint,
		body: message.msg,
		state: outbound.state,
		status: self?.delivery ?? outbound.state,
		error: outbound.error === undefined ? undefined : outbound.error.code,
		in_reply_to: related === undefined ? undefined : { sender: related.sender, id: related.id },
		at: self?.at ?? outbound.created_at ?? null,
		entry: messagePath(ref),
		conversation: conversationPath(ref),
		landed,
		reply: reply === undefined ? undefined : {
			by: reply.message.sender,
			message_id: reply.message.id,
			seq: reply.seq,
			direction: reply.direction,
			at: reply.at,
			body: reply.message.msg,
			message: messagePath(reply.message_ref),
			conversation: conversationPath(reply.message_ref),
		},
		retry_until: outbound.request.retry_until,
	};
}

async function renderConversations(runtime: PluginRuntime, caller: Caller, view: View): Promise<Rendered> {
	requireSession(caller, "conversations");
	if (view.parts.length === 0) {
		const request = pageRequest(view);
		const filter = conversationFilter(view);
		const conversations = await visibleConversations(runtime, caller, filter.query);
		const sorted = conversations
			.slice()
			.sort((left, right) => `${left.last_at ?? ""}\u0000${refKey(left.id)}`.localeCompare(`${right.last_at ?? ""}\u0000${refKey(right.id)}`));
		const page = localPage(
			sorted,
			request,
			cursorScope("conversations", caller, filter.key),
			conversation => `${conversation.last_at ?? ""}\u0000${refKey(conversation.id)}`,
		);
		const lines = [t("conversationsTitle", { node: runtime.config.node.id }), ""];
		for (const conversation of page.items) {
			const latest = conversation.messages.at(-1);
			const title = conversation.title.length > 0
				? conversation.title
				: latest === undefined ? refKey(conversation.id) : titleOf(latest);
			lines.push(t("conversationRow", {
				title,
				key: refKey(conversation.id),
				count: conversation.messages.length,
				at: conversation.last_at ?? t("conversationUnknown"),
			}));
			if (latest === undefined) {
				lines.push(t("conversationOnly", { url: conversationPath(conversation.id) }));
			} else {
				lines.push(t("conversationLatest", {
					direction: latest.direction,
					at: latest.at ?? t("noTime"),
					snippet: firstLine(latest.message.msg, SNIPPET_CHARS),
				}));
				lines.push(t("conversationLinks", {
					conversation: conversationPath(conversation.id),
					message: messagePath(latest.message_ref),
				}));
			}
			if (conversation.thread_refs.length > 0) {
				lines.push(t("conversationThreads", { threads: conversation.thread_refs.map(threadPath).join(", ") }));
			}
		}
		if (page.items.length === 0) lines.push(t("noConversations"));
		lines.push("", nextLink("conversations", request, page.next, filter.params));
		return markdown(`${lines.join("\n")}\n`);
	}
	if (view.parts.length !== 2) throw routeShape(t("conversationsShape"));
	const ref: MessageRef = { sender: view.parts[0]!, id: view.parts[1]! };
	const conversation = await authorizeConversation(runtime, caller, ref);
	const rows = conversation.messages;
	const title = conversation.title.length > 0 ? conversation.title : rows.length === 0 ? refKey(ref) : titleOf(rows[0]!);
	const lines = [
		t("threadTitle", { title }),
		"",
		t("conversationMeta", {
			key: refKey(ref),
			count: rows.length,
			at: conversation.last_at ?? t("conversationUnknown"),
		}),
	];
	if (conversation.thread_refs.length > 0) {
		lines.push(t("threadsList", { threads: conversation.thread_refs.map(threadPath).join(", ") }));
	}
	lines.push("", t("messages"), "");
	for (const row of rows) lines.push(renderHistoryRow(row, runtime.config.node.id));
	if (rows.length === 0) lines.push(t("noMessages"));
	return markdown(`${lines.join("\n")}\n`);
}

interface ConversationFilter {
	query: HistoryQuery;
	key: string;
	params: Record<string, string>;
}

function conversationFilter(view: View): ConversationFilter {
	const threadRaw = view.params.get("thread");
	const messageRaw = view.params.get("message");
	const params: Record<string, string> = {};
	let thread: ThreadRef | undefined;
	let message: MessageRef | undefined;
	if (threadRaw !== null) {
		const [owner, id] = parsePair(threadRaw, "thread");
		thread = { owner, id };
		params.thread = threadRaw;
	}
	if (messageRaw !== null) {
		const [sender, id] = parsePair(messageRaw, "message");
		message = { sender, id };
		params.message = messageRaw;
	}
	return {
		query: { ...(thread === undefined ? {} : { thread }), ...(message === undefined ? {} : { message }) },
		key: JSON.stringify([threadRaw, messageRaw]),
		params,
	};
}

function parsePair(raw: string, label: string): [string, string] {
	const parts = raw.split("/");
	if (parts.length !== 2 || parts[0] === undefined || parts[0].length === 0 || parts[1] === undefined || parts[1].length === 0) {
		throw routeShape(t("pairShape", { label }));
	}
	return [parts[0]!, parts[1]!];
}

function messageRefFromParts(parts: string[], shape: string): MessageRef {
	if (parts.length !== 2) throw routeShape(shape);
	return { sender: parts[0]!, id: parts[1]! };
}

async function leafDocuments(
	runtime: PluginRuntime,
	caller: Caller,
	hrefs: string[],
): Promise<Array<{ url: string; content: string }>> {
	const leaves: Array<{ url: string; content: string }> = [];
	for (const href of hrefs) {
		const rendered = await render(runtime, caller, viewOfHref(href));
		leaves.push({ url: href, content: rendered.content });
	}
	return leaves;
}

async function inboxLeaves(runtime: PluginRuntime, caller: Caller): Promise<Array<{ url: string; content: string }>> {
	const items = await visibleInbox(runtime, caller);
	const byThread = new Map<string, { ref: ThreadRef; seqs: Set<string> }>();
	for (const item of items) {
		const attention = item.attention;
		const key = threadKey(attention.thread_ref);
		const seen = byThread.get(key);
		if (seen !== undefined) {
			seen.seqs.add(attention.seq);
			continue;
		}
		byThread.set(key, { ref: attention.thread_ref, seqs: new Set([attention.seq]) });
	}
	const leaves: Array<{ url: string; content: string }> = [];
	for (const group of byThread.values()) {
		const entries = await allMessages(runtime, group.ref);
		for (const entry of entries) {
			if (!group.seqs.has(entry.seq)) continue;
			leaves.push({
				url: entryPath(group.ref, entry.seq),
				content: `${JSON.stringify(entry, null, 2)}\n`,
			});
		}
	}
	return leaves;
}

async function visibleInbox(runtime: PluginRuntime, caller: Caller): Promise<LocalAttention[]> {
	const items = await inboxItems(runtime);
	if (caller.main) return items;
	const allowed = allowedThreads(await scopedRecords(runtime, caller));
	return items.filter(item => allowed.has(threadKey(item.attention.thread_ref)));
}

async function visibleThreads(runtime: PluginRuntime, caller: Caller): Promise<Thread[]> {
	const threads = await localThreads(runtime);
	if (caller.main) return threads;
	const allowed = allowedThreads(await scopedRecords(runtime, caller));
	return threads.filter(thread => allowed.has(threadKey(thread.thread_ref)));
}

async function findThread(runtime: PluginRuntime, caller: Caller, alias: string): Promise<Thread> {
	const threads = await visibleThreads(runtime, caller);
	const matches = threads.filter(thread =>
		thread.thread_ref.id === alias ||
		thread.title === alias ||
		threadKey(thread.thread_ref) === alias,
	);
	if (matches.length === 1) return matches[0]!;
	if (matches.length > 1) throw new Error(t("manyThreadsNamed", { alias }));
	if (!caller.main && caller.agent_id !== undefined) throw new Error(t("noThreadSlice", { alias }));
	throw new Error(t("noThreadNamed", { alias }));
}

/** Contexts this caller may see: every node-local record for the main session, its own records otherwise. */
async function scopedRecords(runtime: PluginRuntime, caller: Caller): Promise<ContextRecord[]> {
	const records = (await runtime.contexts.list()).filter(record => record.node_id === runtime.config.node.id);
	if (caller.main) return records;
	if (caller.agent_id === undefined) return [];
	return records.filter(record => record.agent_id === caller.agent_id);
}

function allowedThreads(records: ContextRecord[]): Set<string> {
	const allowed = new Set<string>();
	for (const record of records) {
		if (record.thread_ref === undefined) continue;
		allowed.add(threadKey(record.thread_ref));
	}
	return allowed;
}

/** The inbound message a subagent was dispatched for; its replies are readable even without a thread link. */
function sourceRefs(records: ContextRecord[]): Set<string> {
	const sources = new Set<string>();
	for (const record of records) {
		if (record.source === undefined) continue;
		sources.add(`${record.source.sender}/${record.source.id}`);
	}
	return sources;
}

function messagePermitted(message: HistoryMessage, allowed: Set<string>, sources: Set<string>): boolean {
	if (sources.has(refKey(message.message_ref))) return true;
	const thread = message.thread_ref ?? message.message.meta?.thread;
	return thread !== undefined && allowed.has(threadKey(thread));
}

function conversationPermitted(conversation: Conversation, allowed: Set<string>, sources: Set<string>): boolean {
	if (sources.has(refKey(conversation.id))) return true;
	if (conversation.thread_refs.some(thread => allowed.has(threadKey(thread)))) return true;
	return conversation.messages.some(message => messagePermitted(message, allowed, sources));
}

async function localThreads(runtime: PluginRuntime): Promise<Thread[]> {
	return collect(cursor =>
		runtime.node.threads.list(runtime.node.auth, {
			page_size: PAGE,
			...(cursor === undefined ? {} : { cursor }),
		}),
	);
}

async function allMessages(runtime: PluginRuntime, ref: Thread["thread_ref"]): Promise<Entry[]> {
	return collect(cursor =>
		runtime.node.messages.list(runtime.node.auth, ref, {
			attention: "thread",
			page_size: PAGE,
			...(cursor === undefined ? {} : { cursor }),
		}),
	);
}

/** The node's own outbound records; a subagent sees only replies tied to a message it received. */
async function outboundRows(runtime: PluginRuntime, caller: Caller): Promise<Outbound[]> {
	const rows = await runtime.storage.transaction(tx => tx.scan<Outbound>("outbound/"));
	const values = rows.map(row => row.value);
	if (caller.main) return values;
	const sources = sourceRefs(await scopedRecords(runtime, caller));
	if (sources.size === 0) return [];
	return values.filter(outbound => {
		const related = outbound.request.message.meta?.in_reply_to;
		return related !== undefined && sources.has(`${related.sender}/${related.id}`);
	});
}

/** Every conversation the caller may read: the node's own history for main, its linked slice otherwise. */
async function visibleConversations(
	runtime: PluginRuntime,
	caller: Caller,
	query: HistoryQuery = {},
): Promise<Conversation[]> {
	const conversations = await collect(cursor =>
		runtime.node.history.list(runtime.node.auth, {
			...query,
			page_size: PAGE,
			...(cursor === undefined ? {} : { cursor }),
		}),
	);
	if (caller.main) return conversations;
	const records = await scopedRecords(runtime, caller);
	const allowed = allowedThreads(records);
	const sources = sourceRefs(records);
	return conversations.filter(conversation => conversationPermitted(conversation, allowed, sources));
}

/**
 * A conversation reached through the thread filter can carry the other node's thread ref on
 * its outbound rows, so the whole chain is shown; dedup is by message reference.
 */
async function threadMessages(runtime: PluginRuntime, ref: ThreadRef): Promise<HistoryMessage[]> {
	const conversations = await collect(cursor =>
		runtime.node.history.list(runtime.node.auth, {
			thread: ref,
			page_size: PAGE,
			...(cursor === undefined ? {} : { cursor }),
		}),
	);
	const rows = new Map<string, HistoryMessage>();
	for (const conversation of conversations) {
		for (const message of conversation.messages) {
			rows.set(refKey(message.message_ref), message);
		}
	}
	return sortRows([...rows.values()]);
}

/** Thread rows for the list view, keyed by thread and deduplicated by message reference. */
async function historyIndex(runtime: PluginRuntime, caller: Caller): Promise<Map<string, HistoryMessage[]>> {
	const conversations = await visibleConversations(runtime, caller);
	const byThread = new Map<string, Map<string, HistoryMessage>>();
	for (const conversation of conversations) {
		for (const message of conversation.messages) {
			const thread = message.thread_ref ?? message.message.meta?.thread;
			if (thread === undefined) continue;
			const key = threadKey(thread);
			const rows = byThread.get(key) ?? new Map<string, HistoryMessage>();
			rows.set(refKey(message.message_ref), message);
			byThread.set(key, rows);
		}
	}
	const index = new Map<string, HistoryMessage[]>();
	for (const [key, rows] of byThread) index.set(key, sortRows([...rows.values()]));
	return index;
}

async function authorizeConversation(runtime: PluginRuntime, caller: Caller, ref: MessageRef): Promise<Conversation> {
	const conversation = await runtime.node.history.conversation(runtime.node.auth, ref);
	if (caller.main) return conversation;
	const records = await scopedRecords(runtime, caller);
	if (!conversationPermitted(conversation, allowedThreads(records), sourceRefs(records))) {
		throw new Error(t("noConversationSlice", { key: refKey(ref) }));
	}
	return conversation;
}

/**
 * Resource views. Reads go through the library client so every peer page is a signed
 * request, and the caller boundary is checked before any query runs: resources are
 * the main session's channel, never a subagent's slice.
 */
async function renderResources(runtime: PluginRuntime, caller: Caller, view: View): Promise<Rendered> {
	requireMain(caller, "resources");
	const parts = view.parts;
	if (parts.length === 0) {
		const request = pageRequest(view);
		if (request.cursor !== undefined) throw routeShape(t("resourceCursorOneOwner"));
		const query = await resourceQuery(runtime.node, { op: "list", page_size: request.size });
		const inventory = (await runtime.node.client.readResources(query)) as ResourceInventory;
		return markdown(renderInventory(runtime, inventory, request));
	}
	const owner = parts[0]!;
	if (parts.length === 1) {
		const request = pageRequest(view);
		const query = await resourceQuery(runtime.node, {
			op: "list",
			owner,
			page_size: request.size,
			...(request.cursor === undefined ? {} : { cursor: request.cursor }),
		});
		const inventory = (await runtime.node.client.readResources(query)) as ResourceInventory;
		return markdown(renderInventory(runtime, inventory, request));
	}
	const ref: ResourceRef = { owner, id: parts[1]! };
	if (parts.length === 2) {
		const query = await resourceQuery(runtime.node, { op: "describe", resource: ref });
		return json(await runtime.node.client.readResources(query));
	}
	const tail = parts[2]!;
	if (tail === "log") {
		if (parts.length === 3) {
			const request = pageRequest(view);
			const since = view.params.get("since") ?? undefined;
			const query = await resourceQuery(runtime.node, {
				op: "log",
				resource: ref,
				page_size: request.size,
				...(request.cursor === undefined ? {} : { cursor: request.cursor }),
				...(since === undefined ? {} : { since }),
			});
			const page = (await runtime.node.client.readResources(query)) as Page<ResourceEvent>;
			return markdown(renderLog(ref, page, request, since));
		}
		if (parts.length === 4) {
			const query = await resourceQuery(runtime.node, { op: "event", resource: ref, event_id: parts[3]! });
			return json(await runtime.node.client.readResources(query));
		}
		throw routeShape(t("resourceLogShape"));
	}
	if (tail === "operations") {
		if (parts.length === 3) {
			const request = pageRequest(view);
			const query = await resourceQuery(runtime.node, {
				op: "operations",
				resource: ref,
				page_size: request.size,
				...(request.cursor === undefined ? {} : { cursor: request.cursor }),
			});
			const page = (await runtime.node.client.readResources(query)) as Page<OperationRecord>;
			return markdown(renderOperations(ref, page, request));
		}
		if (parts.length === 4) {
			// Status is read directly: a former handler keeps its own operation record after
			// a handover, and an extra describe first would refuse the very access this route exists for.
			const query = await resourceQuery(runtime.node, { op: "status", resource: ref, operation_id: parts[3]! });
			return json(await runtime.node.client.readResources(query));
		}
		throw routeShape(t("resourceOperationsShape"));
	}
	throw routeShape(t("resourceShape"));
}

function renderInventory(runtime: PluginRuntime, inventory: ResourceInventory, request: PageRequest): string {
	const lines = [t("resourcesTitle", { node: runtime.config.node.id }), ""];
	for (const entry of inventory.owners) {
		lines.push(`## ${entry.owner}`);
		if ("error" in entry) {
			lines.push(t("resourceOwnerError", { code: entry.error.code, message: entry.error.message }));
			continue;
		}
		if (entry.page.items.length === 0) lines.push(t("noResources"));
		for (const resource of entry.page.items) {
			const ref = resource.resource_ref;
			lines.push(t("resourceRow", {
				id: ref.id,
				kind: resource.kind,
				operations: resource.operations.length === 0 ? t("noOperations") : resource.operations.join(", "),
				handler: resource.binding.handler,
				cursor: resource.binding.cursor,
				revision: resource.revision,
				event_seq: resource.event_seq,
			}));
			lines.push(t("resourceLinks", {
				detail: resourcePath(ref),
				log: resourceLogPath(ref),
				operations: resourceOperationsPath(ref),
			}));
		}
		// A list cursor names one owner, so its continuation leaves the aggregate route.
		if (entry.page.next_cursor !== null) {
			lines.push(nextLink(`resources/${encodeURIComponent(entry.owner)}`, request, entry.page.next_cursor));
		}
	}
	return `${lines.join("\n")}\n`;
}

function renderLog(ref: ResourceRef, page: Page<ResourceEvent>, request: PageRequest, since: string | undefined): string {
	const lines = [t("resourceLogTitle", { resource: `${ref.owner}/${ref.id}` }), ""];
	for (const event of page.items) {
		lines.push(t("eventRow", {
			id: event.event_id,
			source: event.source_id,
			at: event.at,
			expires: event.expires_at,
			expired: event.expired ? t("suffixExpired") : "",
		}));
		lines.push(t("eventLink", { url: resourceEventPath(ref, event.event_id) }));
	}
	if (page.items.length === 0) lines.push(t("noEvents"));
	lines.push("", nextLink(
		`resources/${encodeURIComponent(ref.owner)}/${encodeURIComponent(ref.id)}/log`,
		request,
		page.next_cursor,
		since === undefined ? {} : { since },
	));
	return `${lines.join("\n")}\n`;
}

function renderOperations(ref: ResourceRef, page: Page<OperationRecord>, request: PageRequest): string {
	const lines = [t("resourceOperationsTitle", { resource: `${ref.owner}/${ref.id}` }), ""];
	for (const record of page.items) {
		const id = record.request.operation_id;
		lines.push(t("operationRow", {
			id,
			operation: record.request.operation,
			state: record.result.state,
			actor: record.actor,
			at: record.accepted_at,
		}));
		lines.push(t("statusLink", { url: resourceStatusPath(ref, id) }));
	}
	if (page.items.length === 0) lines.push(t("noOperationRecords"));
	lines.push("", nextLink(
		`resources/${encodeURIComponent(ref.owner)}/${encodeURIComponent(ref.id)}/operations`,
		request,
		page.next_cursor,
	));
	return `${lines.join("\n")}\n`;
}

/**
 * Resource enumeration. A collection expands into the detail documents the caller may
 * read; a log or operation collection expands into its entry records with the same
 * filters the route accepts. The main-session check runs before any query.
 */
async function resourceLeaves(
	runtime: PluginRuntime,
	caller: Caller,
	view: View,
): Promise<Array<{ url: string; content: string }>> {
	requireMain(caller, "resources");
	const parts = view.parts;
	if (parts.length === 0) {
		if (view.params.get("cursor") !== null) throw routeShape(t("resourceCursorOneOwner"));
		return detailLeaves(runtime, await listResources(runtime));
	}
	const owner = parts[0]!;
	if (parts.length === 1) {
		return detailLeaves(runtime, await listResources(runtime, owner));
	}
	const ref: ResourceRef = { owner, id: parts[1]! };
	if (parts.length === 2) return leafDocuments(runtime, caller, [resourcePath(ref)]);
	const tail = parts[2]!;
	if (tail !== "log" && tail !== "operations") throw routeShape(t("resourceShape"));
	if (parts.length === 3) {
		const request = pageRequest(view);
		if (tail === "log") {
			const since = view.params.get("since") ?? undefined;
			const query = await resourceQuery(runtime.node, {
				op: "log",
				resource: ref,
				page_size: request.size,
				...(request.cursor === undefined ? {} : { cursor: request.cursor }),
				...(since === undefined ? {} : { since }),
			});
			const page = (await runtime.node.client.readResources(query)) as Page<ResourceEvent>;
			return page.items.map(event => ({
				url: resourceEventPath(ref, event.event_id),
				content: `${JSON.stringify(event, null, 2)}\n`,
			}));
		}
		const query = await resourceQuery(runtime.node, {
			op: "operations",
			resource: ref,
			page_size: request.size,
			...(request.cursor === undefined ? {} : { cursor: request.cursor }),
		});
		const page = (await runtime.node.client.readResources(query)) as Page<OperationRecord>;
		return page.items.map(record => ({
			url: resourceStatusPath(ref, record.request.operation_id),
			content: `${JSON.stringify(record, null, 2)}\n`,
		}));
	}
	if (parts.length === 4) {
		return leafDocuments(runtime, caller, [
			tail === "log" ? resourceEventPath(ref, parts[3]!) : resourceStatusPath(ref, parts[3]!),
		]);
	}
	throw routeShape(tail === "log" ? t("resourceLogShape") : t("resourceOperationsShape"));
}

/** Every resource in the caller's inventory, across owner pages; enumeration is not a view page. */
async function listResources(runtime: PluginRuntime, owner?: string): Promise<Resource[]> {
	const first = (await runtime.node.client.readResources(await resourceQuery(runtime.node, {
		op: "list",
		page_size: PAGE,
		...(owner === undefined ? {} : { owner }),
	}))) as ResourceInventory;
	const resources: Resource[] = [];
	for (const entry of first.owners) {
		if ("error" in entry) continue;
		resources.push(...entry.page.items);
		let cursor = entry.page.next_cursor;
		for (let page = 0; cursor !== null; page++) {
			if (page >= MAX_PAGES) throw new Error(t("paginationCap"));
			const next = (await runtime.node.client.readResources(await resourceQuery(runtime.node, {
				op: "list",
				owner: entry.owner,
				page_size: PAGE,
				cursor,
			}))) as ResourceInventory;
			const row = next.owners[0];
			if (row === undefined || "error" in row) break;
			resources.push(...row.page.items);
			cursor = row.page.next_cursor;
		}
	}
	return resources;
}

/** One detail document per readable resource; a list row the caller cannot describe is not a leaf. */
async function detailLeaves(
	runtime: PluginRuntime,
	resources: Resource[],
): Promise<Array<{ url: string; content: string }>> {
	const leaves: Array<{ url: string; content: string }> = [];
	for (const resource of resources) {
		const ref = resource.resource_ref;
		try {
			const query = await resourceQuery(runtime.node, { op: "describe", resource: ref });
			const described = await runtime.node.client.readResources(query);
			leaves.push({ url: resourcePath(ref), content: `${JSON.stringify(described, null, 2)}\n` });
		} catch (error) {
			// The listing already filtered readability; a resource that changed between the
			// list and the describe simply has no detail leaf to enumerate.
			if (!(error instanceof OnlyneError)) throw error;
		}
	}
	return leaves;
}

function resourcePath(ref: ResourceRef): string {
	return `onlyne://resources/${encodeURIComponent(ref.owner)}/${encodeURIComponent(ref.id)}`;
}

function resourceLogPath(ref: ResourceRef): string {
	return `${resourcePath(ref)}/log`;
}

function resourceEventPath(ref: ResourceRef, eventId: string): string {
	return `${resourceLogPath(ref)}/${encodeURIComponent(eventId)}`;
}

function resourceOperationsPath(ref: ResourceRef): string {
	return `${resourcePath(ref)}/operations`;
}

function resourceStatusPath(ref: ResourceRef, operationId: string): string {
	return `${resourceOperationsPath(ref)}/${encodeURIComponent(operationId)}`;
}

async function collect<T>(fetch: (cursor?: string) => Promise<Page<T>>): Promise<T[]> {
	const items: T[] = [];
	let cursor: string | undefined;
	for (let page = 0; page < MAX_PAGES; page++) {
		const result = await fetch(cursor);
		items.push(...result.items);
		const next = result.next_cursor ?? undefined;
		if (next === undefined) return items;
		cursor = next;
	}
	throw new Error(t("paginationCap"));
}

interface PageRequest {
	size: number;
	cursor?: string;
}

function pageRequest(view: View): PageRequest {
	const explicit = view.params.get("page_size");
	const alias = view.params.get("limit");
	if (explicit !== null && alias !== null && explicit !== alias) {
		throw routeShape(t("pageSizeDisagree"));
	}
	const raw = explicit ?? alias;
	const size = raw === null ? DEFAULT_PAGE : Number(raw);
	if (!Number.isInteger(size) || size < 1 || size > MAX_PAGE) {
		throw routeShape(t("pageSizeRange", { max: MAX_PAGE }));
	}
	const cursor = view.params.get("cursor");
	return { size, ...(cursor === null || cursor.length === 0 ? {} : { cursor }) };
}

function cursorScope(route: string, caller: Caller, query: string): string {
	return JSON.stringify([route, caller.main ? "main" : `agent:${caller.agent_id ?? "anonymous"}`, query]);
}

function decodeCursor(scope: string, cursor: string): string {
	let parsed: unknown;
	try {
		parsed = JSON.parse(decodeURIComponent(cursor));
	} catch {
		throw routeShape(t("invalidCursor"));
	}
	const record = parsed as { scope?: unknown; after?: unknown } | null;
	if (record === null || typeof record !== "object" || record.scope !== scope || typeof record.after !== "string") {
		throw routeShape(t("foreignCursor"));
	}
	return record.after;
}

function localPage<T>(items: T[], request: PageRequest, scope: string, position: (item: T) => string): { items: T[]; next: string | null } {
	const after = request.cursor === undefined ? undefined : decodeCursor(scope, request.cursor);
	const remaining = after === undefined ? items : items.filter(item => position(item) > after);
	const selected = remaining.slice(0, request.size);
	const last = selected.at(-1);
	const next = remaining.length > selected.length && last !== undefined
		? encodeURIComponent(JSON.stringify({ scope, after: position(last) }))
		: null;
	return { items: selected, next };
}

function nextLink(route: string, request: PageRequest, next: string | null, filters: Record<string, string> = {}): string {
	if (next === null) return t("endOfList");
	const params = new URLSearchParams();
	for (const [key, value] of Object.entries(filters)) params.set(key, value);
	params.set("page_size", String(request.size));
	params.set("cursor", next);
	return t("next", { url: `onlyne://${route}?${params.toString()}` });
}

function refKey(ref: MessageRef): string {
	return `${ref.sender}/${ref.id}`;
}

function threadKey(ref: ThreadRef): string {
	return `${ref.owner}/${ref.id}`;
}

function sortRows(rows: HistoryMessage[]): HistoryMessage[] {
	return rows.sort((left, right) => {
		const at = (left.at ?? "").localeCompare(right.at ?? "");
		if (at !== 0) return at;
		if (left.seq !== undefined && right.seq !== undefined && left.seq !== right.seq) {
			return left.seq.length - right.seq.length || left.seq.localeCompare(right.seq);
		}
		return refKey(left.message_ref).localeCompare(refKey(right.message_ref));
	});
}

function firstLine(text: string, limit: number): string {
	const line = text.split(/\r?\n/).map(part => part.trim()).find(part => part.length > 0) ?? "";
	return line.length > limit ? `${line.slice(0, limit - 1)}…` : line;
}

function titleOf(row: HistoryMessage): string {
	const line = firstLine(row.message.msg, TITLE_CHARS);
	return line.length === 0 ? refKey(row.message_ref) : line;
}

function threadPath(ref: ThreadRef): string {
	return `onlyne://threads/${encodeURIComponent(ref.owner)}/${encodeURIComponent(ref.id)}`;
}

/** The local messages route; a sender reads its own outbound and inbound here alike. */
export function messagePath(ref: MessageRef): string {
	return `onlyne://messages/${encodeURIComponent(ref.sender)}/${encodeURIComponent(ref.id)}`;
}

/** The whole local conversation around a message, from the root message reference. */
export function conversationPath(ref: MessageRef): string {
	return `onlyne://conversations/${encodeURIComponent(ref.sender)}/${encodeURIComponent(ref.id)}`;
}

/** The raw local thread entry route, kept alongside the history projection. */
function entryPath(ref: ThreadRef, seq: string): string {
	return `onlyne://threads/${encodeURIComponent(ref.id)}/messages/${encodeURIComponent(seq)}`;
}

function renderHistoryRow(row: HistoryMessage, nodeId: string): string {
	const ref = row.message_ref;
	const thread = row.thread_ref ?? row.message.meta?.thread;
	const related = row.message.meta?.in_reply_to;
	const status = [
		row.delivery,
		row.consumption === undefined ? "" : t("consumptionSuffix", { value: row.consumption }),
		row.guarantee === undefined ? "" : t("guaranteeSuffix", { value: row.guarantee }),
		row.error === undefined ? "" : t("errorSuffix", { code: row.error.code }),
	].filter(part => part.length > 0).join(" · ");
	const lines = [
		t("historyRow", {
			prefix: row.seq === undefined ? "" : t("historySeqPrefix", { seq: row.seq }),
			direction: row.direction,
			sender: ref.sender,
			at: row.at ?? t("noTime"),
			status,
		}),
	];
	if (related !== undefined) {
		lines.push(t("replyTo", { ref: `${related.sender}/${related.id}`, url: messagePath(related) }));
	}
	const entry = thread === undefined || row.seq === undefined || thread.owner !== nodeId
		? ""
		: t("suffixEntry", { url: entryPath(thread, row.seq) });
	lines.push(t("historyLinks", { message: messagePath(ref), conversation: conversationPath(ref), entry }));
	for (const line of row.message.msg.split(/\r?\n/)) lines.push(`  > ${line}`);
	return lines.join("\n");
}
