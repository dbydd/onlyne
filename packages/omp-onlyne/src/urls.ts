import type { Entry, LocalAttention, Outbound, Page, Thread, ThreadRef } from "@onlyne/lib";
import { InternalUrlRouter, type InternalResource, type InternalUrl, type ProtocolHandler, type ResolveContext } from "@oh-my-pi/pi-coding-agent/internal-urls";
import { getRuntime, type PluginRuntime } from "./runtime.ts";
import type { ContextRecord } from "./types.ts";

type Caller = { main: boolean; agent_id?: string };
type View = { host: string; parts: string[] };
type Rendered = { content: string; contentType: InternalResource["contentType"] };
type UrlLike = Pick<URL, "hostname" | "pathname"> & { rawHost?: string; rawPathname?: string };

const PAGE = 100;
/** A page loop that exceeds this is a bug in the source, not a reason to truncate silently. */
const MAX_PAGES = 10_000;

const handler: ProtocolHandler = {
	scheme: "onlyne",
	spec: { backing: "virtual", selectors: "lines", immutable: true },

	async resolve(url: InternalUrl, context?: ResolveContext): Promise<InternalResource> {
		const runtime = getRuntime();
		if (runtime === undefined) throw new Error("Onlyne is not configured for this project.");
		const rendered = await render(runtime, callerOf(runtime, context), viewOf(url));
		return { url: url.href, content: rendered.content, contentType: rendered.contentType, immutable: true };
	},

	/**
	 * Leaf documents for `read`, `grep` and `find`. The root expands into views and
	 * thread documents; a thread expands into its message documents; the inbox
	 * expands into the messages its visible attention items name.
	 */
	async enumerate(url: InternalUrl, context?: ResolveContext): Promise<Array<{ url: string; content: string }>> {
		const runtime = getRuntime();
		if (runtime === undefined) throw new Error("Onlyne is not configured for this project.");
		const caller = callerOf(runtime, context);
		const view = viewOf(url);
		if (view.host === "") {
			const urls = caller.agent_id === undefined
				? []
				: ["onlyne://inbox", "onlyne://contexts", "onlyne://outbound"];
			for (const thread of await visibleThreads(runtime, caller)) {
				urls.push(`onlyne://threads/${encodeURIComponent(thread.thread_ref.id)}`);
			}
			return leafDocuments(runtime, caller, urls);
		}
		if (view.host === "threads" && view.parts.length === 0) {
			const urls = (await visibleThreads(runtime, caller)).map(
				thread => `onlyne://threads/${encodeURIComponent(thread.thread_ref.id)}`,
			);
			return leafDocuments(runtime, caller, urls);
		}
		if (view.host === "threads") {
			const thread = await findThread(runtime, caller, threadAlias(view.parts));
			const entries = await allMessages(runtime, thread.thread_ref);
			return entries.map(entry => ({
				url: `onlyne://threads/${encodeURIComponent(thread.thread_ref.id)}/messages/${entry.seq}`,
				content: JSON.stringify(entry, null, 2),
			}));
		}
		if (view.host === "inbox") {
			return inboxLeaves(runtime, caller);
		}
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
		.map(part => decodeURIComponent(part));
	return { host: host === "root" ? "" : host, parts };
}

function viewOfHref(href: string): View {
	return viewOf(new URL(href));
}

async function render(runtime: PluginRuntime, caller: Caller, view: View): Promise<Rendered> {
	if (view.host === "threads") return renderThreads(runtime, caller, view.parts);
	if (view.host === "contexts" || view.host === "inbox" || view.host === "outbound") {
		requireSession(caller, view.host);
		if (view.parts.length > 0 && view.host !== "outbound") {
			throw new Error(`onlyne://${view.host} has no subroutes.`);
		}
		if (view.host === "contexts") return { content: await renderContexts(runtime, caller), contentType: "text/markdown" };
		if (view.host === "inbox") return { content: await renderInbox(runtime, caller), contentType: "text/markdown" };
		return renderOutbound(runtime, caller, view.parts);
	}
	if (view.host === "") {
		return { content: await renderRoot(runtime, caller), contentType: "text/markdown" };
	}
	throw new Error(`Unknown onlyne:// route ${view.host}.`);
}

function requireSession(caller: Caller, view: string): void {
	if (caller.agent_id !== undefined) return;
	throw new Error(`onlyne://${view} is private and needs a calling OMP session.`);
}

async function renderRoot(runtime: PluginRuntime, caller: Caller): Promise<string> {
	const lines = [
		`# Onlyne · ${runtime.config.node.id}`,
		"",
		`Region: ${runtime.config.region}`,
		`Endpoint: ${runtime.endpoint}`,
	];
	try {
		const domains = await runtime.node.domains.list(runtime.node.auth, { page_size: PAGE });
		const active = domains.items.filter(membership => membership.state === "active");
		lines.push(`Domain: ${active.length === 0 ? "none active" : active.map(membership => membership.domain_id).join(", ")}`);
	} catch (error) {
		lines.push(`Domain: unavailable (${error instanceof Error ? error.message : String(error)})`);
	}
	if (caller.agent_id === undefined) {
		lines.push(
			"",
			"Private views need a calling OMP session: onlyne://threads, onlyne://inbox, onlyne://contexts, onlyne://outbound.",
		);
		return `${lines.join("\n")}\n`;
	}
	const records = await scopedRecords(runtime, caller);
	const threads = await visibleThreads(runtime, caller);
	lines.push("", `${caller.main ? "Execution contexts" : `Contexts held by ${caller.agent_id}`}: ${records.length}`);
	for (const record of records) {
		const thread = record.thread_ref === undefined ? "" : ` · ${record.thread_ref.owner}/${record.thread_ref.id}`;
		lines.push(`- \`${record.alias}\` · ${record.state} · ${record.summary}${thread}`);
	}
	if (records.length === 0) lines.push("No execution contexts.");
	lines.push("", `Threads: ${threads.length}`);
	for (const thread of threads) {
		lines.push(`- **${thread.title}** \`${thread.thread_ref.id}\` · ${thread.state} · next seq ${thread.next_seq}`);
	}
	if (threads.length === 0) lines.push("No threads.");
	lines.push("", "Views: onlyne://threads, onlyne://inbox, onlyne://contexts, onlyne://outbound");
	return `${lines.join("\n")}\n`;
}

async function renderThreads(runtime: PluginRuntime, caller: Caller, parts: string[]): Promise<Rendered> {
	requireSession(caller, "threads");
	if (parts.length === 0) {
		const threads = await visibleThreads(runtime, caller);
		const lines = [`# Threads · ${runtime.config.node.id}`, ""];
		for (const thread of threads) {
			lines.push(
				`- **${thread.title}** \`${thread.thread_ref.id}\` · ${thread.state} · next seq ${thread.next_seq} · ${thread.thread_ref.owner}`,
			);
		}
		if (threads.length === 0) lines.push("No threads.");
		return { content: `${lines.join("\n")}\n`, contentType: "text/markdown" };
	}
	const thread = await findThread(runtime, caller, threadAlias(parts));
	if (parts.length >= 2 && parts[1] === "messages") {
		if (parts.length !== 3) throw new Error("onlyne://threads/<alias>/messages/<seq> needs exactly one seq.");
		return messageView(runtime, thread, parts[2]!);
	}
	if (parts.length > 2) throw new Error("onlyne://threads/<alias> has no further routes.");
	return { content: await renderThread(runtime, thread), contentType: "text/markdown" };
}

/** `<alias>` is one segment; `<owner>/<id>` keeps its two segments. */
function threadAlias(parts: string[]): string {
	return parts.length === 2 && parts[1] !== "messages" ? `${parts[0]}/${parts[1]}` : parts[0]!;
}

async function messageView(runtime: PluginRuntime, thread: Thread, seq: string): Promise<Rendered> {
	const entry = (await allMessages(runtime, thread.thread_ref)).find(item => item.seq === seq);
	if (entry === undefined) throw new Error(`No message at ${thread.thread_ref.id}/${seq}.`);
	return { content: `${JSON.stringify(entry, null, 2)}\n`, contentType: "application/json" };
}

async function renderThread(runtime: PluginRuntime, thread: Thread): Promise<string> {
	const lines = [
		`# ${thread.title}`,
		"",
		`Thread \`${thread.thread_ref.owner}/${thread.thread_ref.id}\` · ${thread.state} · next seq ${thread.next_seq}`,
		"",
		"## Members",
		"",
	];
	try {
		const described = await runtime.node.threads.describe(runtime.node.auth, thread.thread_ref);
		for (const member of described.members) {
			lines.push(`- \`${member.peer}\` · ${member.state} · ${member.access.length === 0 ? "no access" : member.access.join(", ")}`);
		}
		if (described.members.length === 0) lines.push("No membership records.");
	} catch (error) {
		lines.push(`unavailable: ${error instanceof Error ? error.message : String(error)}`);
	}
	lines.push("", "## Messages", "");
	const entries = await allMessages(runtime, thread.thread_ref);
	for (const entry of entries) lines.push(renderEntryLine(entry, runtime.config.node.id));
	if (entries.length === 0) lines.push("No messages.");
	return `${lines.join("\n")}\n`;
}

async function renderContexts(runtime: PluginRuntime, caller: Caller): Promise<string> {
	const records = await scopedRecords(runtime, caller);
	const lines = [`# Execution contexts · ${runtime.config.node.id}`, ""];
	for (const record of records) {
		const thread = record.thread_ref === undefined ? "" : ` · ${record.thread_ref.owner}/${record.thread_ref.id}`;
		const agent = record.agent_id === undefined ? "" : ` · ${record.agent_id}`;
		const source = record.source === undefined ? "" : ` · source ${record.source.sender}/${record.source.id}`;
		lines.push(`- \`${record.alias}\` · ${record.state} · ${record.summary}${thread}${agent}${source}`);
	}
	if (records.length === 0) lines.push("No registered contexts.");
	return `${lines.join("\n")}\n`;
}

async function inboxItems(runtime: PluginRuntime): Promise<LocalAttention[]> {
	return collect(cursor => runtime.node.messages.inbox(runtime.node.auth, { page_size: PAGE, ...(cursor === undefined ? {} : { cursor }) }));
}

async function renderInbox(runtime: PluginRuntime, caller: Caller): Promise<string> {
	const items = await visibleInbox(runtime, caller);
	const lines = [`# Inbox · ${runtime.config.node.id}`, ""];
	for (const item of items) {
		const attention = item.attention;
		lines.push(
			`- \`${attention.thread_ref.id}/${attention.seq}\` · ${attention.authenticated_author} · ${attention.acked ? "acked" : "pending"}${attention.disposition === "expired" ? " · expired" : ""}`,
		);
	}
	if (items.length === 0) lines.push("No attention items.");
	return `${lines.join("\n")}\n`;
}

async function renderOutbound(runtime: PluginRuntime, caller: Caller, parts: string[]): Promise<Rendered> {
	const rows = await outboundRows(runtime, caller);
	if (parts.length === 0) {
		const lines = [`# Outbound · ${runtime.config.node.id}`, ""];
		for (const outbound of rows) {
			const related = outbound.request.message.meta?.in_reply_to;
			const reply = related === undefined ? "" : ` · reply to ${related.sender}/${related.id}`;
			lines.push(
				`- \`${outbound.request.message.id}\` → ${outbound.request.target_endpoint} · ${outbound.state}${reply}${outbound.error === undefined ? "" : ` · ${outbound.error.code}`}`,
			);
		}
		if (rows.length === 0) lines.push("No outbound records.");
		return { content: `${lines.join("\n")}\n`, contentType: "text/markdown" };
	}
	if (parts.length !== 1) throw new Error("onlyne://outbound/<message-id> has no further routes.");
	const found = rows.find(row => row.request.message.id === parts[0]);
	if (found === undefined) throw new Error(`No outbound record is named ${parts[0]}.`);
	return { content: `${JSON.stringify(found, null, 2)}\n`, contentType: "application/json" };
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
		const key = `${attention.thread_ref.owner}/${attention.thread_ref.id}`;
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
				url: `onlyne://threads/${encodeURIComponent(group.ref.id)}/messages/${entry.seq}`,
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
	return items.filter(item => allowed.has(`${item.attention.thread_ref.owner}/${item.attention.thread_ref.id}`));
}

async function visibleThreads(runtime: PluginRuntime, caller: Caller): Promise<Thread[]> {
	const threads = await localThreads(runtime);
	if (caller.main) return threads;
	const allowed = allowedThreads(await scopedRecords(runtime, caller));
	return threads.filter(thread => allowed.has(`${thread.thread_ref.owner}/${thread.thread_ref.id}`));
}

async function findThread(runtime: PluginRuntime, caller: Caller, alias: string): Promise<Thread> {
	const threads = await visibleThreads(runtime, caller);
	const matches = threads.filter(thread =>
		thread.thread_ref.id === alias ||
		thread.title === alias ||
		`${thread.thread_ref.owner}/${thread.thread_ref.id}` === alias,
	);
	if (matches.length === 1) return matches[0]!;
	if (matches.length > 1) throw new Error(`More than one thread is named ${alias}.`);
	if (!caller.main && caller.agent_id !== undefined) throw new Error(`This session holds no read slice for thread ${alias}.`);
	throw new Error(`No thread is named ${alias}.`);
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
		allowed.add(`${record.thread_ref.owner}/${record.thread_ref.id}`);
	}
	return allowed;
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
	const sources = new Set<string>();
	for (const record of await scopedRecords(runtime, caller)) {
		if (record.source === undefined) continue;
		sources.add(`${record.source.sender}/${record.source.id}`);
	}
	if (sources.size === 0) return [];
	return values.filter(outbound => {
		const related = outbound.request.message.meta?.in_reply_to;
		return related !== undefined && sources.has(`${related.sender}/${related.id}`);
	});
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
	throw new Error("onlyne:// pagination did not terminate within the page cap.");
}

function renderEntryLine(entry: Entry, nodeId: string): string {
	const direction = entry.message.sender === nodeId ? "out" : "in";
	const relation = entry.message.meta?.relation;
	const meta = relation === undefined ? "" : ` · ${relation}`;
	const body = entry.message.msg
		.split(/\r?\n/)
		.map(line => `  > ${line}`)
		.join("\n");
	return `[${entry.seq}] ${direction} · ${entry.authenticated_author} · ${entry.at}${meta}\n${body}`;
}

