import { dirname, join } from "node:path";
import { OnlyneError, trustPhase, type DiscoveryCandidate, type DiscoveryRecord, type KeyExchange, type MaintenanceClock, type NotifyEvent, type OnlyneNode, type Outbound, type Page } from "@onlyne/lib";
import type { ExtensionAPI, ExtensionContext } from "@oh-my-pi/pi-coding-agent";
import { currentDecision, JudgeDecider } from "./decider.ts";
import { loadConfig, saveConfig, shortNodeId } from "./config.ts";
import { ensureKeyDir } from "./keys.ts";
import { createRuntimeBridge } from "./messages.ts";
import { advertisedHost, createOnlyneNode } from "./node.ts";
import { NetworkDiscovery } from "./network.ts";
import { ContextRegistry, SqliteStorage } from "./store.ts";
import type { OnlyneConfig, OnlyneStatus, QueueItem, ViewerContact, ViewerDiscovery, ViewerMessage, ViewerSnapshot, ViewerThread } from "./types.ts";

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
	status(): Promise<OnlyneStatus>;
	onChange(listener: () => void): () => void;
	/** The library's one notification channel: trust transitions, inbound entries, pending-work flips. */
	notify(event: NotifyEvent): void;
	contact(endpoint: string): Promise<void>;
	/** Stage one of one exchange; the returned session carries the fingerprint the peer proved. */
	trustBegin(node: string): Promise<KeyExchange>;
	trustAccept(node: string, fingerprint: string): Promise<void>;
	trustRevoke(node: string): Promise<void>;
	trustRotate(): Promise<{ fingerprint: string }>;
	refreshDiscovery(): Promise<void>;
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

/** How often the discovery catalog is re-observed; well inside the library's 120 s freshness window. */
const DISCOVERY_REFRESH_MS = 30_000;

class OmpRuntime implements PluginRuntime {
	readonly contexts: ContextRegistry;
	readonly storage: SqliteStorage;
	readonly mainAgentId: string;
	node!: OnlyneNode;
	decider!: JudgeDecider;
	endpoint = "";
	private server?: ReturnType<typeof Bun.serve>;
	private keyDir?: string;
	private network?: NetworkDiscovery;
	/** The host's one periodic job: re-observing discovery. Delivery cadence belongs to the library. */
	private timer?: Timer;
	/** A round that outlives its interval must not overlap the next one. */
	private rediscovering = false;
	private discoveryError?: string;
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
		// The decider probes peers for their execution contexts, so it must use the node's own
		// transport: an unsigned probe is refused by every keyed peer.
		let transport: OnlyneNode["transport"] | undefined;
		this.decider = new JudgeDecider({
			config: this.config, contexts: this.contexts, mainAgentId: this.mainAgentId,
			log: (message, fields) => {
				// Judge decisions and their trims are the tuning record; everything else stays debug.
				if (message.startsWith("onlyne.judge.")) this.pi.logger.info(message, fields);
				else this.pi.logger.debug(message, fields);
			},
			listPeers: async () => (await this.node.discovery.listRecords()).filter(record => record.verified_at !== undefined && Date.parse(record.expires_at) > Date.now()),
			listReceiverContexts: async id => id === this.config.node.id
				? [
						{ alias: "main", summary: "Main OMP session", state: this.main.isIdle() ? "idle" : "running" },
						// A lost or never-bound record cannot receive a continuation; advertising it
						// would invite the sender to pick a context that fails on arrival.
						...await this.contexts.list().then(records => records
							.filter(record => record.state !== "lost" && record.state !== "unbound")
							.map(record => ({ alias: record.alias, summary: record.summary, state: record.state }))),
					]
				: transport!.request(id, "GET", "/contexts", undefined, this.controller.signal),
			listThreads: () => allPages(cursor => this.node.threads.list(this.node.auth, { page_size: 100, cursor })),
			listMessages: (thread, cursor) => this.node.messages.list(this.node.auth, thread, { attention: "thread", page_size: 100, cursor }),
		});
		const bridge = createRuntimeBridge({ pi: this.pi, main: this.main, decider: this.decider, contexts: this.contexts, nodeId: this.config.node.id });
		const projectRoot = dirname(dirname(this.config.path));
		// `node.key_scope` names where the identity lives. Without it the historic global
		// store stays in effect; `none` runs the keyless loopback development binding. A
		// refused project directory (git work tree without an ignore rule) fails the start.
		const keyScope = this.config.node.key_scope ?? "global";
		const keyDir = keyScope === "none" ? undefined : await ensureKeyDir(projectRoot, this.config.node.id, keyScope);
		this.keyDir = keyDir;
		// The library owns the maintenance cadence; the host only lends it one-shot session
		// timers. The loop re-arms after every pass, so a repeating timer would double it.
		const clock: MaintenanceClock = {
			setTimer: (ms, run) => this.main.setTimeout(run, ms),
			clearTimer: handle => this.main.clearTimer(handle as Timer),
		};
		this.node = createOnlyneNode({ config: this.config, endpoint: this.endpoint, instanceId, storage: this.storage, contexts: this.contexts, runtime: bridge, clock,
			...(keyDir === undefined ? {} : { keyDir }),
			decide: input => {
				const scope = currentDecision();
				if (!scope) throw new OnlyneError("decision_unavailable", "No OMP session is bound to this decision.");
				scope.input = input;
				return this.decider.decide(scope);
			},
		});
		transport = this.node.transport;
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
			onCandidate: async (url, source, expectedNode) => {
				if (expectedNode === this.config.node.id) return;
				await this.node.discovery.observe(url, source, expectedNode, this.controller.signal);
				this.discoveryError = undefined;
				this.changed();
			},
			onError: error => { this.discoveryError = errorText(error); this.pi.logger.warn("onlyne.discovery.failed", { error: errorText(error) }); },
		});
		try { await this.network.start(); } catch (error) { this.pi.logger.warn("onlyne.discovery.unavailable", { error: errorText(error) }); }
		await this.contexts.ensureMain();
		await this.node.client.recover(this.controller.signal);
		this.node.client.runMaintenance();
		this.rediscover();
		this.timer = this.main.setInterval(() => this.rediscover(), DISCOVERY_REFRESH_MS);
		this.pi.logger.info("onlyne.listening", { endpoint: this.endpoint, node: this.config.node.id, region: this.config.region });
	}
	/**
	 * RFC 0017 §5: the library reports every committed trust transition, every inbound entry
	 * and every pending-work flip here. The viewer repaints from it; it never polls storage
	 * for these.
	 */
	notify(event: NotifyEvent): void {
		if (event.event === "trust") {
			this.pi.logger.info(`onlyne.trust.${event.cause}`, {
				node: event.entry.node_id, phase: event.phase, fingerprint: event.entry.fingerprint,
			});
		}
		this.changed();
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
	/** Stage one of one exchange: the peer's proved fingerprint, and the session that commits it. */
	async trustBegin(node: string): Promise<KeyExchange> {
		return this.node.trust.begin(this.node.auth, node);
	}
	async trustAccept(node: string, fingerprint: string): Promise<void> {
		await this.node.trust.accept(this.node.auth, node, fingerprint);
	}
	async trustRevoke(node: string): Promise<void> {
		await this.node.trust.revoke(this.node.auth, node);
	}
	async trustRotate(): Promise<{ fingerprint: string }> {
		const identity = await this.node.trust.rotate(this.node.auth);
		return { fingerprint: identity.fingerprint };
	}
	async refreshDiscovery(): Promise<void> {
		await this.node.discovery.refresh(this.controller.signal);
		this.changed();
	}
	/** One discovery round on the host's timer; a failed round keeps the last good catalog. */
	private rediscover(): void {
		if (this.rediscovering) return;
		this.rediscovering = true;
		void this.refreshDiscovery()
			.catch(error => { if (!this.closed) this.pi.logger.warn("onlyne.discovery.failed", { error: errorText(error) }); })
			.finally(() => { this.rediscovering = false; });
	}

	async status(): Promise<OnlyneStatus> {
		const records = await this.node.discovery.listRecords();
		const inbox = await allPages(cursor => this.node.messages.inbox(this.node.auth, { page_size: 100, cursor }));
		const at = Date.now();
		// Reachable now: a fresh direct observation, never an expired or never-verified clue.
		const reachable = new Set(records
			.filter(record => record.verified_at !== undefined && Date.parse(record.expires_at) > at)
			.map(record => record.node_id));
		reachable.add(this.config.node.id);
		// One hop is the set this node can actually address: peers it has verified and, when
		// keys are in use, trusted. A clue from a relay is not a neighbor.
		const peers = new Set(records
			.filter(record => record.node_id !== this.config.node.id && record.verified_at !== undefined && Date.parse(record.expires_at) > at)
			.map(record => record.node_id));
		if (this.node.keys.enabled) {
			const trusted = new Set((await this.node.trust.list(this.node.auth)).filter(entry => trustPhase(entry) === "exchanged").map(entry => entry.node_id));
			for (const node of [...peers]) if (!trusted.has(node)) peers.delete(node);
		}
		// Live attention only: an acknowledged or consumed item is history, not an alert.
		const alerts = inbox.filter(item => !item.attention.acked && !item.injected).length;
		return {
			region: this.config.region, node_id: this.config.node.id, nodes: reachable.size, neighbors: peers.size,
			alerts, unread: inbox.filter(item => !item.injected).length,
			...(this.discoveryError === undefined ? {} : { discovery_error: this.discoveryError }),
		};
	}
	async snapshot(): Promise<ViewerSnapshot> {
		const contexts = await this.contexts.list();
		const records = await this.node.discovery.listRecords();
		const inbox = await allPages(cursor => this.node.messages.inbox(this.node.auth, { page_size: 100, cursor }));
		const views: ViewerThread[] = [];
		// Hosted threads are read locally. A thread this node only belongs to lives on its
		// owner, so it is shown from the attention rows already held here; the viewer never
		// reaches across the network.
		const hosted = new Set<string>();
		for (const thread of await allPages(cursor => this.node.threads.list(this.node.auth, { page_size: 100, cursor }))) {
			const ref = thread.thread_ref;
			hosted.add(`${ref.owner}/${ref.id}`);
			const entries = await allPages(cursor => this.node.messages.list(this.node.auth, ref, { attention: "thread", page_size: 100, cursor }));
			views.push({ ref, alias: `${encodeURIComponent(ref.owner)}/${encodeURIComponent(ref.id)}`, title: thread.title, state: thread.state,
				unread: inbox.filter(item => !item.injected && item.attention.thread_ref.owner === ref.owner && item.attention.thread_ref.id === ref.id).length,
				last_seq: entries.at(-1)?.seq ?? "0", messages: entries.map(entry => ({ ref: { sender: entry.message.sender, id: entry.message.id }, seq: entry.seq, direction: entry.message.sender === this.config.node.id ? "out" : "in", author: entry.authenticated_author, at: entry.at, text: entry.message.msg, relation: entry.message.meta?.relation })),
			});
		}
		// A member's own message never comes back through its inbox, so the message that
		// opened a thread it only takes part in survives only in the outbound record that
		// carried it. Read those here and merge them below the inbox rows, or the member
		// sees the reply and not the question that started it.
		const outbound = await this.storage.transaction(tx => tx.scan<Outbound>("outbound/"));
		const seen = new Map<string, ViewerThread>();
		for (const item of inbox) {
			const ref = item.attention.thread_ref;
			const key = `${ref.owner}/${ref.id}`;
			if (hosted.has(key)) continue;
			const view = seen.get(key) ?? { ref, alias: `${encodeURIComponent(ref.owner)}/${encodeURIComponent(ref.id)}`, title: `来自 ${shortNodeId(ref.owner)} 的线程`, state: "open", unread: 0, last_seq: "0", messages: [] };
			seen.set(key, view);
			if (!item.injected) view.unread++;
			const entry = item.attention.entry;
			if (entry !== undefined) {
				view.messages.push({ ref: { sender: entry.message.sender, id: entry.message.id }, seq: entry.seq, direction: entry.message.sender === this.config.node.id ? "out" : "in", author: entry.authenticated_author, at: entry.at, text: entry.message.msg, relation: entry.message.meta?.relation });
				view.last_seq = entry.seq;
			}
		}
		for (const row of outbound) {
			const receipt = row.value.receipt;
			if (receipt === undefined) continue;
			const ref = receipt.thread_ref;
			const key = `${ref.owner}/${ref.id}`;
			if (hosted.has(key)) continue;
			const view = seen.get(key) ?? { ref, alias: `${encodeURIComponent(ref.owner)}/${encodeURIComponent(ref.id)}`, title: `来自 ${shortNodeId(ref.owner)} 的线程`, state: "open", unread: 0, last_seq: "0", messages: [] };
			seen.set(key, view);
			const message = row.value.request.message;
			view.messages.push({ ref: { sender: message.sender, id: message.id }, seq: receipt.seq, direction: "out", author: message.sender, at: row.value.created_at ?? "", text: message.msg, relation: message.meta?.relation });
			if (Number(receipt.seq) > Number(view.last_seq)) view.last_seq = receipt.seq;
		}
		for (const view of seen.values()) view.messages.sort((left, right) => Number(left.seq) - Number(right.seq));
		views.push(...seen.values());
		const queue: QueueItem[] = inbox.filter(item => !item.injected).map(item => ({ id: `inbound:${item.attention.obligation_id}`, kind: "inbound", label: `${item.attention.thread_ref.id}/${item.attention.seq}`, detail: `${item.attention.authenticated_author} · pending consumption`, at: item.attention.entry?.at }));
		queue.push(...contexts.filter(record => record.state === "running" || record.state === "unbound").map(record => ({ id: `context:${record.alias}`, kind: "context" as const, label: record.alias, detail: `${record.summary} · ${record.state}`, at: record.created_at })));
		queue.push(...outbound.filter(row => row.value.state === "unknown" || row.value.state === "pending").map(row => ({ id: row.key, kind: "outbound" as const, label: row.value.request.target_endpoint, detail: row.value.state })));
		const at = Date.now();
		// One row per node: a node that moved ports would otherwise appear once per address.
		// A verified record beats an unverified one; within each class the newest wins.
		const byNode = new Map<string, DiscoveryRecord>();
		for (const record of records) {
			const kept = byNode.get(record.node_id);
			if (kept === undefined || newerRecord(record, kept)) byNode.set(record.node_id, record);
		}
		const discovery: ViewerDiscovery[] = [...byNode.values()].map(record => ({
			node_id: record.node_id, instance_id: record.instance_id, base_url: record.base_url, source: record.source,
			...(record.membership?.domain_id === undefined ? {} : { region: record.membership.domain_id }),
			...(record.membership?.state === undefined ? {} : { membership_state: record.membership.state }),
			...(record.verified_at === undefined ? {} : { verified_at: record.verified_at }),
			expires_at: record.expires_at, last_seen: record.last_seen,
			...(record.error === undefined ? {} : { error: record.error.message }),
		}));
		const newest = new Map<string, DiscoveryRecord>();
		for (const record of records) {
			if (record.node_id === this.config.node.id || record.verified_at === undefined || Date.parse(record.expires_at) <= at) continue;
			const kept = newest.get(record.node_id);
			if (kept === undefined || newerRecord(record, kept)) newest.set(record.node_id, record);
		}
		const contacts: ViewerContact[] = [...newest.values()].map(record => ({
			node_id: record.node_id, instance_id: record.instance_id, base_url: record.base_url,
			...(record.membership?.domain_id === undefined ? {} : { region: record.membership.domain_id }),
			last_seen: record.last_seen,
		}));
		const trust = this.node.keys.enabled ? await this.node.trust.list(this.node.auth) : [];
		const identity = this.node.keys.enabled ? await this.node.keys.describe() : undefined;
		return { region: this.config.region, endpoint: this.endpoint, contexts, queue, threads: views, discovery, contacts,
			trust: trust.map(row => ({ node_id: row.node_id, fingerprint: row.fingerprint, phase: trustPhase(row), base_url: row.base_url })),
			...(identity === undefined ? {} : { identity: { public_key: identity.public_key, fingerprint: identity.fingerprint } }),
			...(this.discoveryError === undefined ? {} : { discovery_error: this.discoveryError }) };
	}
	async updateConfig(next: OnlyneConfig): Promise<void> { await saveConfig(next); await restartRuntime(next, this.pi, this.main); }
	async close(): Promise<void> {
		if (this.closed) return;
		this.closed = true;
		this.controller.abort();
		if (this.timer) this.main.clearTimer(this.timer);
		// A start that failed before the node existed still comes here to release the server.
		this.node?.client.stopMaintenance();
		await this.network?.close();
		this.server?.stop(true);
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

/** A verified record beats an unverified one; within each class the newest observation wins. */
function newerRecord(candidate: DiscoveryRecord, kept: DiscoveryRecord): boolean {
	const candidateVerified = candidate.verified_at !== undefined;
	const keptVerified = kept.verified_at !== undefined;
	if (candidateVerified !== keptVerified) return candidateVerified;
	return Date.parse(candidate.last_seen) > Date.parse(kept.last_seen);
}
