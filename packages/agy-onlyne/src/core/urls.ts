import {
	OnlyneError,
	trustPhase,
	type Auth,
	type Entry,
	type OnlyneNode,
	type Outbound,
	type ThreadRef,
} from "@onlyne/lib";
import type { ContextRegistry } from "@onlyne/node-host/store";
import type { ContextRecord } from "@onlyne/node-host/types";
import type { AgyOnlyneConfig } from "../config.ts";
import { ActionGuard } from "./guard.ts";
import { threadKey } from "./resolve.ts";
import { resourceQuery } from "./resources.ts";
import type { OnlyneTodoEntry } from "./types.ts";
import { urlsText as t } from "./urls-text.ts";

export interface UrlsRuntime {
	config: AgyOnlyneConfig;
	endpoint: string;
	mainAgentId: string;
	node: OnlyneNode;
	contexts: ContextRegistry;
	listOutbound?(): Promise<Outbound[]>;
	listTodos?(): Promise<OnlyneTodoEntry[]>;
}

export interface Caller {
	agent_id?: string;
	main?: boolean;
}

export interface RenderedUrl {
	content: string;
	contentType: "text/markdown" | "application/json";
}

const PAGE = 50;

interface ParsedView {
	host: string;
	parts: string[];
	params: URLSearchParams;
}

export function isOnlyneUrl(raw: string): boolean {
	return raw.trim().toLowerCase().startsWith("onlyne://");
}

function parseUrl(raw: string): ParsedView {
	const trimmed = raw.trim();
	if (!isOnlyneUrl(trimmed)) throw new OnlyneError("invalid_request", t("schemeRequired", { url: raw }));
	const withoutScheme = trimmed.slice("onlyne://".length);
	const qIdx = withoutScheme.indexOf("?");
	const pathPart = qIdx >= 0 ? withoutScheme.slice(0, qIdx) : withoutScheme;
	const search = qIdx >= 0 ? withoutScheme.slice(qIdx + 1) : "";
	const slashIdx = pathPart.indexOf("/");
	const rawHost = slashIdx >= 0 ? pathPart.slice(0, slashIdx) : pathPart;
	const rawRest = slashIdx >= 0 ? pathPart.slice(slashIdx + 1) : "";
	let host: string;
	try {
		host = decodeURIComponent(rawHost);
	} catch {
		throw new OnlyneError("invalid_request", t("malformedEncoding", { path: raw }));
	}
	const parts = rawRest
		.split("/")
		.filter(Boolean)
		.map(seg => {
			try {
				return decodeURIComponent(seg);
			} catch {
				throw new OnlyneError("invalid_request", t("malformedEncoding", { path: raw }));
			}
		});
	return { host: host === "root" ? "" : host, parts, params: new URLSearchParams(search) };
}

function markdown(content: string): RenderedUrl {
	return { content, contentType: "text/markdown" };
}

function json(value: unknown): RenderedUrl {
	return { content: `${JSON.stringify(value, null, 2)}\n`, contentType: "application/json" };
}

function requireSession(caller: Caller, view: string): asserts caller is { agent_id: string; main?: boolean } {
	if (caller.agent_id !== undefined && caller.agent_id.length > 0) return;
	throw new OnlyneError("forbidden", t("privateSession", { view }));
}

function requireMain(caller: Caller, view: string): void {
	if (caller.main === true) return;
	throw new OnlyneError("forbidden", t("mainSessionOnly", { view }));
}

function threadRefFromParts(parts: string[], shape: string): ThreadRef {
	if (parts.length !== 2 || !parts[0] || !parts[1]) {
		throw new OnlyneError("invalid_request", t("routeShape", { shape }));
	}
	return { owner: parts[0], id: parts[1] };
}

async function authorizeThread(runtime: UrlsRuntime, caller: Caller, ref: ThreadRef): Promise<void> {
	if (caller.main === true) return;
	const guard = new ActionGuard({ contexts: runtime.contexts, nodeId: runtime.config.node.id });
	await guard.assert({ readOnly: true, agentId: caller.agent_id ?? "" }, {
		kind: "read",
		scope: "messages",
		thread: ref,
	});
}

async function authorizeMessage(runtime: UrlsRuntime, caller: Caller, sender: string, id: string): Promise<void> {
	if (caller.main === true) return;
	const guard = new ActionGuard({ contexts: runtime.contexts, nodeId: runtime.config.node.id });
	await guard.assert({ readOnly: true, agentId: caller.agent_id ?? "" }, {
		kind: "read",
		scope: "message",
		message: { sender, id },
	});
}

async function visibleContexts(runtime: UrlsRuntime, caller: Caller): Promise<ContextRecord[]> {
	const all = (await runtime.contexts.list()).filter(c => c.node_id === runtime.config.node.id);
	if (caller.main === true) return all;
	return all.filter(c => c.agent_id === caller.agent_id);
}

export async function readOnlyneUrl(runtime: UrlsRuntime, caller: Caller, href: string): Promise<RenderedUrl> {
	const view = parseUrl(href);
	const auth: Auth = runtime.node.auth;

	if (view.host === "") {
		return renderRoot(runtime, caller, auth);
	}
	if (view.host === "neighbors" || view.host === "peers") {
		requireMain(caller, "neighbors");
		const [discovery, trust] = await Promise.all([
			runtime.node.discovery.listRecords().catch(() => []),
			runtime.node.trust.list(auth).catch(() => []),
		]);
		if (view.parts.length === 1) {
			const target = view.parts[0]!;
			return json({
				node_id: target,
				discovery: discovery.filter(d => d.node_id === target),
				trust: trust.filter(tr => tr.node_id === target).map(tr => ({ ...tr, phase: trustPhase(tr) })),
			});
		}
		if (view.parts.length > 1) throw new OnlyneError("invalid_request", t("routeShape", { shape: "onlyne://neighbors[/<node_id>]" }));
		const lines = ["# Onlyne Neighbors", ""];
		for (const tr of trust) {
			lines.push(`- \`${tr.node_id}\` · trust=\`${trustPhase(tr)}\` · fp=\`${tr.fingerprint.slice(0, 16)}\` · url=\`${tr.base_url}\``);
		}
		for (const d of discovery) {
			if (d.node_id === runtime.config.node.id || trust.some(tr => tr.node_id === d.node_id)) continue;
			lines.push(`- \`${d.node_id}\` · discovered (${d.source}) · url=\`${d.base_url}\``);
		}
		if (lines.length === 2) lines.push("(none)");
		return markdown(`${lines.join("\n")}\n`);
	}
	if (view.host === "threads") {
		requireSession(caller, "threads");
		if (view.parts.length === 0) {
			requireMain(caller, "threads");
			const [owned, joined] = await Promise.all([
				runtime.node.threads.list(auth, { page_size: PAGE }),
				runtime.node.threads.joined(auth, { page_size: PAGE }),
			]);
			const lines = ["# Onlyne Threads", ""];
			for (const item of owned.items) {
				lines.push(`- \`onlyne://threads/${encodeURIComponent(item.thread_ref.owner)}/${encodeURIComponent(item.thread_ref.id)}\` — **${item.title}** (${item.state}, next_seq=${item.next_seq})`);
			}
			for (const item of joined.items) {
				lines.push(`- \`onlyne://threads/${encodeURIComponent(item.thread_ref.owner)}/${encodeURIComponent(item.thread_ref.id)}\` — ${item.title ?? item.thread_ref.id} (joined, last_seq=${item.last_seq})`);
			}
			return markdown(`${lines.join("\n")}\n`);
		}
		const ref = threadRefFromParts(view.parts.slice(0, 2), "onlyne://threads/<owner>/<id>[/messages[/<seq>]]");
		await authorizeThread(runtime, caller, ref);
		if (view.parts.length === 2) {
			const desc = await runtime.node.client.describeThread(ref);
			return json(desc);
		}
		if (view.parts.length === 3 && view.parts[2] === "messages") {
			const cursor = view.params.get("cursor") ?? undefined;
			const page = await runtime.node.client.threadEntries(ref, {
				attention: "thread",
				page_size: PAGE,
				...(cursor ? { cursor } : {}),
			});
			return renderMessages(ref, page.items, page.next_cursor);
		}
		if (view.parts.length === 4 && view.parts[2] === "messages") {
			const page = await runtime.node.client.threadEntries(ref, { attention: "thread", page_size: 100 });
			const found = page.items.find(item => item.seq === view.parts[3]);
			if (found === undefined) throw new OnlyneError("not_found", `Message ${threadKey(ref)}#${view.parts[3]} not found`);
			return json(found);
		}
		throw new OnlyneError("invalid_request", t("routeShape", { shape: "onlyne://threads/<owner>/<id>[/messages[/<seq>]]" }));
	}
	if (view.host === "inbox") {
		requireMain(caller, "inbox");
		const rows = await runtime.node.messages.inbox(auth, { page_size: PAGE });
		if (view.params.get("format") === "json") return json(rows.items);
		const lines = ["# Onlyne Inbox", ""];
		for (const row of rows.items) {
			const e = row.attention.entry;
			const body = e?.message.msg.replace(/\s+/g, " ").trim().slice(0, 120) ?? "";
			lines.push(`- \`${threadKey(row.attention.thread_ref)}#${row.attention.seq}\` from \`${row.attention.authenticated_author}\` (${row.injected ? "consumed/injected" : "pending"}, context=\`${row.context ?? "main"}\`): ${body}`);
		}
		if (rows.items.length === 0) lines.push("(empty)");
		return markdown(`${lines.join("\n")}\n`);
	}
	if (view.host === "outbound") {
		requireMain(caller, "outbound");
		const rows = await (runtime.listOutbound?.() ?? Promise.resolve([]));
		if (view.parts.length === 0) return json({ items: rows });
		if (view.parts.length === 2) {
			const match = rows.find(row => row.request.message.sender === view.parts[0] && row.request.message.id === view.parts[1]);
			if (match === undefined) throw new OnlyneError("not_found", "Outbound entry not found");
			return json(match);
		}
		throw new OnlyneError("invalid_request", t("routeShape", { shape: "onlyne://outbound[/<sender>/<id>]" }));
	}
	if (view.host === "conversations") {
		requireMain(caller, "conversations");
		if (view.parts.length === 0) return json(await runtime.node.history.list(auth, { page_size: PAGE }));
		if (view.parts.length === 2) return json(await runtime.node.history.conversation(auth, { sender: view.parts[0]!, id: view.parts[1]! }));
		throw new OnlyneError("invalid_request", t("routeShape", { shape: "onlyne://conversations[/<sender>/<id>]" }));
	}
	if (view.host === "messages") {
		requireSession(caller, "messages");
		if (view.parts.length !== 2) throw new OnlyneError("invalid_request", t("routeShape", { shape: "onlyne://messages/<sender>/<id>" }));
		await authorizeMessage(runtime, caller, view.parts[0]!, view.parts[1]!);
		return json(await runtime.node.history.message(auth, { sender: view.parts[0]!, id: view.parts[1]! }));
	}
	if (view.host === "contexts") {
		requireSession(caller, "contexts");
		return json(await visibleContexts(runtime, caller));
	}
	if (view.host === "todo") {
		requireSession(caller, "todo");
		return json(await (runtime.listTodos?.() ?? Promise.resolve([])));
	}
	if (view.host === "resources") {
		requireMain(caller, "resources");
		if (view.parts.length === 0) {
			return json(await runtime.node.client.readResources(await resourceQuery(runtime.node, { op: "list", page_size: PAGE })));
		}
		if (view.parts.length >= 2) {
			const ref = { owner: view.parts[0]!, id: view.parts[1]! };
			if (view.parts.length === 2) {
				return json(await runtime.node.client.readResources(await resourceQuery(runtime.node, { op: "describe", resource: ref })));
			}
			if (view.parts[2] === "log") {
				return json(await runtime.node.client.readResources(await resourceQuery(runtime.node, { op: "log", resource: ref, page_size: PAGE })));
			}
			if (view.parts[2] === "operations") {
				return json(await runtime.node.client.readResources(await resourceQuery(runtime.node, { op: "operations", resource: ref, page_size: PAGE })));
			}
		}
		throw new OnlyneError("invalid_request", t("routeShape", { shape: "onlyne://resources[/<owner>/<id>[/log|/operations]]" }));
	}
	throw new OnlyneError("invalid_request", t("unknownRoute", { host: view.host }));
}

async function renderRoot(runtime: UrlsRuntime, caller: Caller, auth: Auth): Promise<RenderedUrl> {
	const lines = [
		t("rootTitle", { node: runtime.config.node.id }),
		"",
		t("rootRegion", { region: runtime.config.region }),
		t("rootEndpoint", { endpoint: runtime.endpoint }),
	];
	try {
		const domains = await runtime.node.domains.list(auth, { page_size: PAGE });
		const active = domains.items.filter(d => d.state === "active");
		lines.push(t("rootDomain", {
			domains: active.length === 0 ? t("rootDomainNone") : active.map(d => d.domain_id).join(", "),
		}));
	} catch (error) {
		lines.push(t("rootDomainUnavailable", { message: error instanceof Error ? error.message : String(error) }));
	}
	lines.push("", t("rootViews"));
	const ctxs = await visibleContexts(runtime, caller).catch(() => []);
	if (ctxs.length > 0) {
		lines.push("", "## Contexts", ...ctxs.map(c => `- \`${c.alias}\` [${c.state}] ${c.summary}`));
	}
	return markdown(`${lines.join("\n")}\n`);
}

function renderMessages(ref: ThreadRef, items: Entry[], nextCursor: string | null): RenderedUrl {
	const lines = [`# Thread ${threadKey(ref)}`, ""];
	for (const e of items) {
		lines.push(`- **#${e.seq}** \`${e.message.sender}\` [${e.message.delivery_kind}] (${e.at}): ${e.message.msg}`);
	}
	if (nextCursor !== null) lines.push("", `Next cursor: \`${nextCursor}\``);
	return markdown(`${lines.join("\n")}\n`);
}

export async function enumerateOnlyneUris(runtime: UrlsRuntime): Promise<string[]> {
	const auth = runtime.node.auth;
	const uris = [
		"onlyne://",
		"onlyne://neighbors",
		"onlyne://threads",
		"onlyne://inbox",
		"onlyne://outbound",
		"onlyne://conversations",
		"onlyne://contexts",
		"onlyne://resources",
		"onlyne://todo",
	];
	const [owned, joined] = await Promise.all([
		runtime.node.threads.list(auth, { page_size: 20 }).catch(() => ({ items: [] })),
		runtime.node.threads.joined(auth, { page_size: 20 }).catch(() => ({ items: [] })),
	]);
	for (const row of [...owned.items, ...joined.items]) {
		const base = `onlyne://threads/${encodeURIComponent(row.thread_ref.owner)}/${encodeURIComponent(row.thread_ref.id)}`;
		uris.push(base, `${base}/messages`);
	}
	return [...new Set(uris)];
}
