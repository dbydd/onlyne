import { dirname, join } from "node:path";
import { FetchTransport, OnlyneError, type Entry, type LocalAttention, type OnlyneNode, type Outbound, type Page, type Thread, type DiscoveryCandidate } from "@onlyne/lib";
import type { ExtensionAPI, ExtensionContext } from "@oh-my-pi/pi-coding-agent";
import { currentDecision, JudgeDecider } from "./decider.ts";
import { ConfigError, loadConfig, saveConfig } from "./config.ts";
import { createRuntimeBridge } from "./messages.ts";
import { advertisedHost, createOnlyneNode } from "./node.ts";
import { NetworkDiscovery } from "./network.ts";
import { ContextRegistry, SqliteStorage } from "./store.ts";
import type { OnlyneConfig, QueueItem, ViewerMessage, ViewerSnapshot, ViewerThread } from "./types.ts";

export interface PluginRuntime {
	readonly config: OnlyneConfig;
	readonly endpoint: string;
	readonly mainAgentId: string;
	readonly main: ExtensionContext;
	readonly pi: ExtensionAPI;
	readonly contexts: ContextRegistry;
	readonly node: OnlyneNode;
	readonly storage: SqliteStorage;
	readonly decider: JudgeDecider;
	snapshot(): Promise<ViewerSnapshot>;
	onChange(listener: () => void): () => void;
	contact(endpoint: string): Promise<void>;
	updateConfig(next: OnlyneConfig): Promise<void>;
	close(): Promise<void>;
}
let active: PluginRuntime | undefined;
export function getRuntime(): PluginRuntime | undefined { return active; }
export function setRuntime(runtime: PluginRuntime | undefined): void { active = runtime; }
export async function startRuntime(pi: ExtensionAPI, ctx: ExtensionContext): Promise<PluginRuntime> {
	const config = await loadConfig(ctx.cwd);
	if (active && active.config.path === config.path && JSON.stringify(active.config) === JSON.stringify(config)) return active;
	return restartRuntime(config, pi, ctx);
}
export async function restartRuntime(config: OnlyneConfig, pi: ExtensionAPI, ctx: ExtensionContext): Promise<PluginRuntime> {
	await active?.close();
	const runtime = new OmpRuntime(config, pi, ctx);
	try { await runtime.start(); } catch (error) { await runtime.close(); throw error; }
	active = runtime;
	return runtime;
}
export async function stopRuntime(): Promise<void> { await active?.close(); }

class OmpRuntime implements PluginRuntime {
	readonly contexts: ContextRegistry;
	readonly storage: SqliteStorage;
	readonly mainAgentId: string;
	node!: OnlyneNode;
	decider!: JudgeDecider;
	endpoint = "";
	private server?: ReturnType<typeof Bun.serve>;
	private network?: NetworkDiscovery;
	private timer?: ReturnType<ExtensionContext["setInterval"]>;
	private maintenance?: Promise<void>;
	private controller = new AbortController();
	private listeners = new Set<() => void>();
	private closed = false;
	constructor(readonly config: OnlyneConfig, readonly pi: ExtensionAPI, readonly main: ExtensionContext) {
		this.mainAgentId = main.agent.id;
		this.storage = new SqliteStorage(join(dirname(config.path), "omp.db"));
		this.contexts = new ContextRegistry(this.storage, config.node.id, main.agent.id);
	}
	async start(): Promise<void> {
		let handler: (request: Request) => Promise<Response> = async () => new Response("Onlyne is initializing", { status: 503 });
		this.server = Bun.serve({ hostname: this.config.server.bind, port: 0, fetch: request => handler(request) });
		const host = advertisedHost(this.config.server.bind);
		this.endpoint = `http://${host.includes(":") ? `[${host}]` : host}:${this.server.port}/onlyne/v3`;
		const instanceId = crypto.randomUUID();
		const transport = new FetchTransport(async id => {
			const target = await this.node.discovery.resolve(id);
			return { ...target, headers: { "Onlyne-Actor": this.config.node.id } };
		});
		this.decider = new JudgeDecider({
			config: this.config, contexts: this.contexts, mainAgentId: this.mainAgentId,
			listPeers: async () => (await this.node.discovery.listRecords()).filter(record => record.verified_at !== undefined && Date.parse(record.expires_at) > Date.now()),
			listReceiverContexts: async id => id === this.config.node.id
				? [{ alias: "main", summary: "Main OMP session", state: this.main.isIdle() ? "idle" : "running" }, ...await this.contexts.list()]
				: transport.request(id, "GET", "/contexts", undefined, this.controller.signal),
			listThreads: () => allPages(cursor => this.node.threads.list(this.node.auth, { page_size: 100, cursor })),
			listMessages: (thread, cursor) => this.node.messages.list(this.node.auth, thread, { attention: "thread", page_size: 100, cursor }),
		});
		const bridge = createRuntimeBridge({ pi: this.pi, main: this.main, decider: this.decider, contexts: this.contexts, nodeId: this.config.node.id });
		this.node = createOnlyneNode({ config: this.config, endpoint: this.endpoint, instanceId, storage: this.storage, contexts: this.contexts, runtime: bridge,
			decide: input => {
				const scope = currentDecision();
				if (!scope) throw new OnlyneError("decision_unavailable", "No OMP session is bound to this decision.");
				scope.input = input;
				return this.decider.decide(scope);
			},
		});
		const memberships = (await this.node.domains.list(this.node.auth)).items;
		const current = memberships[0];
		const mutation = () => ({ idempotency_key: crypto.randomUUID(), retry_until: new Date(Date.now() + 60_000).toISOString() });
		if (current && current.domain_id !== this.config.region) await this.node.domains.leave(this.node.auth, current.domain_id, { ...mutation(), expected_revision: current.revision });
		if (!current || current.domain_id !== this.config.region) {
			let expected_revision: string | undefined;
			try { expected_revision = (await this.node.domains.describe(this.node.auth, this.config.region)).revision; }
			catch (error) { if (!(error instanceof OnlyneError && error.error.code === "not_found")) throw error; }
			await this.node.domains.join(this.node.auth, this.config.region, { ...mutation(), expected_revision });
		}
		handler = request => this.node.fetch(request);
		this.network = new NetworkDiscovery({ node_id: this.config.node.id, instance_id: instanceId, endpoint: this.endpoint, bind: this.config.server.bind,
			onCandidate: async (url, source, expectedNode) => { if (expectedNode === this.config.node.id) return; await this.node.discovery.observe(url, source, expectedNode, this.controller.signal); this.changed(); },
			onError: error => this.pi.logger.warn("onlyne discovery failed", { error: errorText(error) }),
		});
		try { await this.network.start(); } catch (error) { this.pi.logger.warn("onlyne DNS-SD discovery is unavailable", { error: errorText(error) }); }
		await this.contexts.ensureMain();
		await this.node.client.recover(this.controller.signal);
		this.timer = this.main.setInterval(() => this.maintain(), 2_000);
		this.pi.logger.info("onlyne listening", { endpoint: this.endpoint, node: this.config.node.id, region: this.config.region });
	}
	private maintain(): void {
		if (this.closed || this.maintenance) return;
		const pass = (async () => {
			for (const work of [() => this.node.client.maintain(this.controller.signal), () => this.node.client.recover(this.controller.signal)]) {
				try { await work(); } catch (error) { if (!this.closed) this.pi.logger.warn("onlyne maintenance failed", { error: errorText(error) }); }
			}
			this.changed();
		})();
		this.maintenance = pass.finally(() => { this.maintenance = undefined; });
	}
	onChange(listener: () => void): () => void { this.listeners.add(listener); return () => this.listeners.delete(listener); }
	private changed(): void { for (const listener of this.listeners) listener(); }
	async contact(endpoint: string): Promise<void> {
		await this.node.discovery.observe(endpoint, "bootstrap", undefined, this.controller.signal);
		const url = new URL(endpoint.replace(/\/$/, "") + "/discovery");
		let cursor: string | undefined;
		do {
			url.search = "";
			if (cursor) url.searchParams.set("cursor", cursor);
			const response = await fetch(url, { signal: this.controller.signal });
			if (!response.ok) throw new OnlyneError("unavailable", `Discovery directory answered HTTP ${response.status}.`);
			const page = await response.json() as Page<DiscoveryCandidate>;
			if (!Array.isArray(page.items)) throw new OnlyneError("invalid_request", "The discovery page has no items.");
			for (const candidate of page.items) if (candidate.node_id !== this.config.node.id) await this.node.discovery.observe(candidate.base_url, endpoint, candidate.node_id, this.controller.signal);
			cursor = page.next_cursor ?? undefined;
		} while (cursor);
		this.changed();
	}
	async snapshot(): Promise<ViewerSnapshot> {
		const contexts = await this.contexts.list();
		const inbox = await allPages(cursor => this.node.messages.inbox(this.node.auth, { page_size: 100, cursor }));
		const threads = await allPages(cursor => this.node.threads.list(this.node.auth, { page_size: 100, cursor }));
		const views: ViewerThread[] = [];
		for (const thread of threads) {
			const entries = await allPages(cursor => this.node.messages.list(this.node.auth, thread.thread_ref, { attention: "thread", page_size: 100, cursor }));
			views.push({ alias: thread.thread_ref.id, title: thread.title, state: thread.state,
				unread: inbox.filter(item => !item.injected && item.attention.thread_ref.owner === thread.thread_ref.owner && item.attention.thread_ref.id === thread.thread_ref.id).length,
				last_seq: entries.at(-1)?.seq ?? "0", messages: entries.map(entry => ({ seq: entry.seq, direction: entry.message.sender === this.config.node.id ? "out" : "in", author: entry.authenticated_author, at: entry.at, text: entry.message.msg, relation: entry.message.meta?.relation })),
			});
		}
		const outbound = await this.storage.transaction(tx => tx.scan<Outbound>("outbound/"));
		const queue: QueueItem[] = inbox.filter(item => !item.injected).map(item => ({ id: `inbound:${item.attention.obligation_id}`, kind: "inbound", label: `${item.attention.thread_ref.id}/${item.attention.seq}`, detail: `${item.attention.authenticated_author} · pending consumption`, at: item.attention.entry?.at }));
		queue.push(...contexts.filter(record => record.state === "running" || record.state === "unbound").map(record => ({ id: `context:${record.alias}`, kind: "context" as const, label: record.alias, detail: `${record.summary} · ${record.state}`, at: record.created_at })));
		queue.push(...outbound.filter(row => row.value.state === "unknown" || row.value.state === "pending").map(row => ({ id: row.key, kind: "outbound" as const, label: row.value.request.target_endpoint, detail: row.value.state })));
		return { region: this.config.region, endpoint: this.endpoint, contexts, queue, threads: views };
	}
	async updateConfig(next: OnlyneConfig): Promise<void> { await saveConfig(next); await restartRuntime(next, this.pi, this.main); }
	async close(): Promise<void> {
		if (this.closed) return;
		this.closed = true;
		this.controller.abort();
		if (this.timer) this.main.clearTimer(this.timer);
		await this.network?.close();
		this.server?.stop(true);
		await this.maintenance;
		this.storage.close();
		this.listeners.clear();
		if (active === this) active = undefined;
	}
}
export async function allPages<T>(read: (cursor?: string) => Promise<Page<T>>): Promise<T[]> {
	const items: T[] = [];
	let cursor: string | undefined;
	do { const page = await read(cursor); items.push(...page.items); cursor = page.next_cursor ?? undefined; } while (cursor);
	return items;
}
function errorText(error: unknown): string { return error instanceof Error ? error.message : String(error); }
