import { AsyncLocalStorage } from "node:async_hooks";
import { dirname, join } from "node:path";
import { OnlyneError, trustPhase, type DiscoveryCandidate, type DiscoveryRecord, type KeyExchange, type KeyHook, type MaintenanceClock, type NotifyEvent, type OnlyneNode, type Outbound, type Page, type TrustEntry } from "@onlyne/lib";
import type { ExtensionAPI, ExtensionContext } from "@oh-my-pi/pi-coding-agent";
import { ensureKeyDir } from "@onlyne/node-host/keys";
import { NetworkDiscovery } from "@onlyne/node-host/network";
import { resolveLocale, setLocale } from "@onlyne/node-host/i18n";
import { acquisitionOf } from "./acquire.ts";
import { AcquisitionLog, approvePeer, handshakeEntry, isTrustThread, judgeApproval, judgementFacts, revokePeer, trustThread, TRUST_THREAD, type ApprovalDeps, type ApprovalJudgement } from "./handshake.ts";
import { commandText as t } from "./ui/command-text.ts";
import { registerMsgTool } from "./tool.ts";
import { currentDecision, JudgeDecider } from "./decider.ts";
import { loadConfig, normalizeServiceUrl, saveConfig, shortNodeId } from "./config.ts";
import { createRuntimeBridge } from "./messages.ts";
import { advertisedHost, createOnlyneNode } from "./node.ts";
import { canAnswer, ContextRegistry, SqliteStorage } from "./store.ts";
import { resourceQuery } from "./resources.ts";
import type { OnlyneConfig, OnlyneStatus, QueueItem, ViewerContact, ViewerDiscovery, ViewerMessage, ViewerSnapshot, ViewerThread } from "./types.ts";

/** The trust transition of RFC 0017 §5, as the host receives it. */
type TrustNotify = Extract<NotifyEvent, { event: "trust" }>;

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
	/** A human's confirmation in the panel: RFC 0018 §2 keeps that gate outside the policy. */
	trustAccept(node: string, fingerprint: string): Promise<void>;
	trustRevoke(node: string): Promise<void>;
	trustRotate(): Promise<{ fingerprint: string }>;
	/** RFC 0018 §2/§3.1: an agent's approval, judged against the acquisition policy first. */
	acquireAccept(node: string, fingerprint: string): Promise<TrustEntry>;
	/** RFC 0018 §2/§3.1: an agent's revocation. The policy never applies to dropping a peer. */
	acquireRevoke(node: string): Promise<void>;
	refreshDiscovery(): Promise<void>;
	updateConfig(next: OnlyneConfig): Promise<void>;
	close(): Promise<void>;
}
let active: PluginRuntime | undefined;
export interface RuntimeHost {
	config?: () => Promise<OnlyneConfig>;
	keys?: KeyHook;
}
export function getRuntime(): PluginRuntime | undefined { return active; }
export function setRuntime(runtime: PluginRuntime | undefined): void { active = runtime; }
export async function startRuntime(pi: ExtensionAPI, ctx: ExtensionContext, host: RuntimeHost = {}): Promise<PluginRuntime> {
	const config = await (host.config === undefined ? loadConfig(ctx.cwd) : host.config());
	setLocale(resolveLocale(config.language));
	if (active && active.config.path === config.path && JSON.stringify(active.config) === JSON.stringify(config)) return active;
	return restartRuntime(config, pi, ctx, host);
}
export async function restartRuntime(config: OnlyneConfig, pi: ExtensionAPI, ctx: ExtensionContext, host: RuntimeHost = {}): Promise<PluginRuntime> {
	setLocale(resolveLocale(config.language));
	await active?.close();
	const runtime = new OmpRuntime(config, pi, ctx, host);
	try { await runtime.start(); } catch (error) { await runtime.close(); throw error; }
	active = runtime;
	registerMsgTool(pi);
	return runtime;
}
export async function stopRuntime(): Promise<void> { await active?.close(); }

/** How often the discovery catalog is re-observed; well inside the library's 120 s freshness window. */
const DISCOVERY_REFRESH_MS = 30_000;

/** The alias the context registry gives this node's main session (`ContextRegistry.ensureMain`). */
const MAIN_CONTEXT = "main";

/** One mutation's identity for the library: a fresh key and a window that outlives this call. */
function mutation(): { idempotency_key: string; retry_until: string } {
	return { idempotency_key: crypto.randomUUID(), retry_until: new Date(Date.now() + 60_000).toISOString() };
}

class OmpRuntime implements PluginRuntime {
	readonly contexts: ContextRegistry;
	readonly storage: SqliteStorage;
	readonly mainAgentId: string;
	node!: OnlyneNode;
	decider!: JudgeDecider;
	endpoint = "";
	/** RFC 0018 §2.1: where the connection being served came from, for the library's own call into `notify`. */
	private readonly connection = new AsyncLocalStorage<string | undefined>();
	private readonly log: AcquisitionLog;
	/** The host side of one approval, built once with the node in `start`. */
	private peers!: ApprovalDeps;
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
	constructor(readonly config: OnlyneConfig, readonly pi: ExtensionAPI, readonly main: ExtensionContext, readonly host: RuntimeHost = {}) {
		this.mainAgentId = main.agent.id;
		this.storage = new SqliteStorage(join(dirname(config.path), "omp.db"));
		this.contexts = new ContextRegistry(this.storage, config.node.id, main.agent.id);
		this.log = new AcquisitionLog(this.storage);
	}
	async start(): Promise<void> {
		let handler: (request: Request) => Promise<Response> = async () => new Response(t("initializing"), { status: 503 });
		// RFC 0018 §2.1: the address a peer statement arrived from is known only here. The
		// library calls `notify` synchronously from inside the request it is serving, so the
		// connection's own store is still in scope when the trust event is handled.
		this.server = Bun.serve({
			hostname: this.config.server.bind, port: 0,
			fetch: (request, server) => this.connection.run(server.requestIP(request)?.address, () => handler(request)),
		});
		const host = advertisedHost(this.config.server.bind);
		this.endpoint = `http://${host.includes(":") ? `[${host}]` : host}:${this.server.port}/onlyne/v3`;
		const instanceId = crypto.randomUUID();
		// The decider probes peers for their execution contexts, so it must use the node's own
		// transport: an unsigned probe is refused by every keyed peer.
		let transport: OnlyneNode["transport"] | undefined;
		this.decider = new JudgeDecider({
			config: this.config, contexts: this.contexts, mainAgentId: this.mainAgentId,
			resourceQuery: meta => resourceQuery(this.node, meta),
			log: (message, fields) => {
				// Judge decisions and their trims are the tuning record; everything else stays debug.
				if (message.startsWith("onlyne.judge.")) this.pi.logger.info(message, fields);
				else this.pi.logger.debug(message, fields);
			},
			listPeers: async () => (await this.node.discovery.listRecords()).filter(record => record.verified_at !== undefined && Date.parse(record.expires_at) > Date.now()),
			listReceiverContexts: async id => id === this.config.node.id
				? [
						{ alias: "main", summary: t("main"), state: this.main.isIdle() ? "idle" : "running" },
						// A lost or never-bound record cannot receive a continuation; advertising it
						// would invite the sender to pick a context that fails on arrival.
						...await this.contexts.list().then(records => records
							.filter(record => record.state !== "lost" && record.state !== "unbound" && canAnswer(record))
							.map(record => ({ alias: record.alias, summary: record.summary, state: record.state }))),
					]
				: transport!.request(id, "GET", "/contexts", undefined, this.controller.signal),
			// RFC 0018 §3.2: the handshake thread is this host's own record, never a candidate
			// for a conversation the agent routes or reads by name.
			listThreads: async () => (await allPages(cursor => this.node.threads.list(this.node.auth, { page_size: 100, cursor })))
				.filter(thread => !isTrustThread(this.config.node.id, thread.thread_ref)),
			listMessages: (thread, cursor) => this.node.messages.list(this.node.auth, thread, { attention: "thread", page_size: 100, cursor }),
		});
		const bridge = createRuntimeBridge({ pi: this.pi, main: this.main, decider: this.decider, contexts: this.contexts, nodeId: this.config.node.id });
		const projectRoot = dirname(dirname(this.config.path));
		// `node.key_scope` names where the identity lives. Without it the historic global
		// store stays in effect; `none` runs the keyless loopback development binding. A
		// refused project directory (git work tree without an ignore rule) fails the start.
		const keyScope = this.config.node.key_scope ?? "global";
		const keyDir = this.host.keys !== undefined || keyScope === "none" ? undefined : await ensureKeyDir(projectRoot, this.config.node.id, keyScope);
		this.keyDir = keyDir;
		// The library owns the maintenance cadence; the host only lends it one-shot session
		// timers. The loop re-arms after every pass, so a repeating timer would double it.
		const clock: MaintenanceClock = {
			setTimer: (ms, run) => this.main.setTimeout(run, ms),
			clearTimer: handle => this.main.clearTimer(handle as Timer),
		};
		this.node = createOnlyneNode({ config: this.config, endpoint: this.endpoint, instanceId, storage: this.storage, contexts: this.contexts, runtime: bridge, clock,
			...(keyDir === undefined ? {} : { keyDir }),
			...(this.host.keys === undefined ? {} : { keys: this.host.keys }),
			decide: input => {
				const scope = currentDecision();
				if (!scope) throw new OnlyneError("decision_unavailable", "No OMP session is bound to this decision.");
				scope.input = input;
				return this.decider.decide(scope);
			},
		});
		transport = this.node.transport;
		this.peers = {
			policy: acquisitionOf(this.config),
			log: this.log,
			observed: async (nodeId, fingerprint) => (await this.log.observation(nodeId, fingerprint))?.observed,
			declared: async nodeId => (await this.node.trust.entry(nodeId))?.base_url,
			recorded: nodeId => this.recorded(nodeId),
			audit: judgement => this.pi.logger.info("onlyne.acquire.decided", judgementFacts(judgement)),
			accept: (nodeId, fingerprint) => this.node.trust.accept(this.node.auth, nodeId, fingerprint),
			revoke: nodeId => this.node.trust.revoke(this.node.auth, nodeId),
			now: () => new Date().toISOString(),
		};
		const memberships = (await this.node.domains.list(this.node.auth)).items;
		const current = memberships[0];
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
		// A saved seed is reached on every start, so a node that moved ports is found again even
		// where mDNS never hears it. One unreachable seed is a logged fact, never a failed start,
		// and the seeds are read together so a dead address costs one wait rather than one each.
		await Promise.all((this.config.discovery?.seeds ?? []).map(async seed => {
			try { await this.follow(seed); }
			catch (error) { this.pi.logger.warn("onlyne.seed.failed", { seed, error: errorText(error) }); }
		}));
		this.changed();
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
			// RFC 0018 §2.1: the address this statement arrived from exists only inside the
			// call the library is making right now, and nothing after it can recover it.
			this.trustEvent(event, this.connection.getStore());
		}
		this.changed();
	}
	onChange(listener: () => void): () => void { this.listeners.add(listener); return () => this.listeners.delete(listener); }
	private changed(): void { for (const listener of this.listeners) listener(); }
	/**
	 * RFC 0018 §2/§3: one trust transition's host work — the observation its statement's
	 * connection left, the judgement of an invitation this host may approve itself, and the
	 * handshake entry the agent reads. Everything is reported by the library's own call, so it
	 * runs beside it; a failure here is logged and is never a reason to doubt the committed row.
	 */
	private trustEvent(event: TrustNotify, observed: string | undefined): void {
		void this.handleTrust(event, observed).catch(error => {
			this.pi.logger.warn("onlyne.trust.handling_failed", { node: event.entry.node_id, phase: event.phase, error: errorText(error) });
		});
	}
	private async handleTrust(event: TrustNotify, observed: string | undefined): Promise<void> {
		const node = event.entry.node_id;
		const fingerprint = event.entry.fingerprint;
		// The address is recorded for every peer statement, before its verdict. Local
		// transitions never replace it with the address of the approval call (which has none),
		// so an approval decided later still reads the statement's original connection.
		const at = new Date().toISOString();
		if (event.cause === "peer-statement") {
			await this.log.observe(node, fingerprint, {
				...(observed === undefined ? {} : { observed }), at, cause: event.cause,
			});
		} else await this.log.transition(node, fingerprint, at, event.cause);
		// The statement's own address is also a discovery candidate here, as the tern host's
		// bridge records it. Awaited, so the verdict below already counts that record (§2.2).
		if (event.entry.base_url) {
			await this.node.discovery.observe(event.entry.base_url, "trust-statement", node).catch(() => undefined);
		}
		// §2.2: only an invitation whose statement covered a connection this host observed, and
		// whose declared and recorded addresses all stay inside the allow set, is committed
		// without a human deciding first.
		const judgement = event.phase === "invited" && event.cause === "peer-statement"
			? await judgeApproval(this.peers, { node_id: node, fingerprint, observed })
			: undefined;
		if (judgement?.verdict.allowed === true) this.approveDeferred(node, fingerprint);
		await this.deliverHandshake(event, observed, judgement);
	}
	/**
	 * RFC 0018 §2: the approval an invitation earned is committed outside the library's call,
	 * so the admission that notified this host is never re-entered.
	 */
	private approveDeferred(node: string, fingerprint: string): void {
		queueMicrotask(() => {
			void this.peers.accept(node, fingerprint)
				.catch(error => this.pi.logger.warn("onlyne.acquire.approve_failed", { node, error: errorText(error) }));
		});
	}
	/**
	 * RFC 0018 §3: one handshake entry per `(node, fingerprint, phase)`, written into this
	 * node's own thread under a plan bound to the main session. The thread has this node as its
	 * only member, so the entry never leaves the node.
	 */
	private async deliverHandshake(event: TrustNotify, observed: string | undefined, judgement: ApprovalJudgement | undefined): Promise<void> {
		const node = event.entry.node_id;
		const fingerprint = event.entry.fingerprint;
		if (await this.log.delivered(node, fingerprint, event.phase)) return;
		const handshake = handshakeEntry({
			node_id: node, fingerprint, phase: event.phase, declared: event.entry.base_url, observed,
			policy: this.peers.policy, judgement, approved: judgement?.verdict.allowed === true,
		});
		await this.ensureTrustThread();
		const outcome = await this.node.client.send(this.config.node.id, {
			id: crypto.randomUUID(),
			sender: this.config.node.id,
			delivery_kind: "trust",
			msg: handshake.msg,
			// The mention is what puts the entry in this node's own inbox: the delivery plan
			// names the main context, and the bridge reads the facts for its todo hook.
			meta: { thread: trustThread(this.config.node.id), mentions: [this.config.node.id], data: { trust: handshake.trust } },
		}, { response: "none", execution: "existing", context: MAIN_CONTEXT });
		if (outcome.admission !== "accepted") {
			this.pi.logger.warn("onlyne.trust.delivery_failed", { node, phase: event.phase, error: outcome.error?.message });
			return;
		}
		await this.log.markDelivered(node, fingerprint, event.phase, new Date().toISOString());
	}
	/** RFC 0018 §3.2: the handshake thread, created on first use. Its id is fixed, so it is one thread for the node's lifetime. */
	private async ensureTrustThread(): Promise<void> {
		const ref = trustThread(this.config.node.id);
		try {
			await this.node.threads.describe(this.node.auth, ref);
			return;
		} catch (error) {
			if (!(error instanceof OnlyneError && error.error.code === "not_found")) throw error;
		}
		await this.node.threads.create(this.node.auth, { title: TRUST_THREAD, id: ref.id, ...mutation() });
	}
	/** RFC 0018 §2.2: every address discovery holds for this node is an outbound candidate the policy must allow. */
	private async recorded(node: string): Promise<string[]> {
		const records = await this.node.discovery.listRecords();
		return [...new Set(records.filter(record => record.node_id === node).map(record => record.base_url))];
	}
	/**
	 * A user-typed address. It is normalized once, read for its own identity and everything it
	 * declares, and only a successful read becomes a seed. The node keeps running: a seed is a
	 * routing hint, not a second binding.
	 */
	async contact(endpoint: string): Promise<void> {
		const seed = normalizeServiceUrl(endpoint);
		await this.follow(seed);
		await this.rememberSeed(seed);
		this.changed();
	}
	/**
	 * Reads one address into the discovery catalog. The peer's own directory is a bonus: a
	 * node that answers `/description` but not `/discovery` is still a usable neighbor.
	 */
	private async follow(seed: string): Promise<void> {
		await this.node.discovery.observe(seed, "bootstrap", undefined, this.controller.signal);
		await this.followDirectory(seed).catch(error => {
			this.pi.logger.debug("onlyne.seed.directory", { seed, error: errorText(error) });
		});
	}
	/** Every peer the address declares, each read on its own: one failure does not stop the rest. */
	private async followDirectory(seed: string): Promise<void> {
		const url = new URL(`${seed}/discovery`);
		let cursor: string | undefined;
		do {
			url.search = "";
			if (cursor) url.searchParams.set("cursor", cursor);
			const response = await fetch(url, { signal: this.controller.signal });
			if (!response.ok) throw new OnlyneError("unavailable", `Discovery directory answered HTTP ${response.status}.`);
			const page = await response.json() as Page<DiscoveryCandidate>;
			if (!Array.isArray(page.items)) throw new OnlyneError("invalid_request", "The discovery page has no items.");
			for (const candidate of page.items) {
				if (candidate.node_id === this.config.node.id) continue;
				await this.node.discovery.observe(candidate.base_url, seed, candidate.node_id, this.controller.signal).catch(() => undefined);
			}
			cursor = page.next_cursor ?? undefined;
		} while (cursor);
	}
	/** The seed list is configuration: one write per new address, and none on a start. */
	private async rememberSeed(seed: string): Promise<void> {
		const seeds = this.config.discovery?.seeds ?? [];
		if (seeds.includes(seed)) return;
		const saved = await saveConfig({ ...this.config, discovery: { seeds: [...seeds, seed] } });
		this.config.discovery = saved.discovery;
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
	/** RFC 0018 §2/§3.1: the agent's approval, refused with `policy_denied` when the policy does not allow it. */
	async acquireAccept(node: string, fingerprint: string): Promise<TrustEntry> {
		return approvePeer(this.peers, node, fingerprint);
	}
	/** RFC 0018 §2/§3.1: the agent's revocation, which no policy judges. */
	async acquireRevoke(node: string): Promise<void> {
		await revokePeer(this.peers, node);
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
			// RFC 0018 §3.2: the handshake entries stay out of the conversation tree. The
			// thread is still recorded above, so its own inbox rows are not re-rendered here.
			if (isTrustThread(this.config.node.id, ref)) continue;
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
			const view = seen.get(key) ?? { ref, alias: `${encodeURIComponent(ref.owner)}/${encodeURIComponent(ref.id)}`, title: t("thread", { node: shortNodeId(ref.owner) }), state: "open", unread: 0, last_seq: "0", messages: [] };
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
			const view = seen.get(key) ?? { ref, alias: `${encodeURIComponent(ref.owner)}/${encodeURIComponent(ref.id)}`, title: t("thread", { node: shortNodeId(ref.owner) }), state: "open", unread: 0, last_seq: "0", messages: [] };
			seen.set(key, view);
			const message = row.value.request.message;
			view.messages.push({ ref: { sender: message.sender, id: message.id }, seq: receipt.seq, direction: "out", author: message.sender, at: row.value.created_at ?? "", text: message.msg, relation: message.meta?.relation });
			if (Number(receipt.seq) > Number(view.last_seq)) view.last_seq = receipt.seq;
		}
		for (const view of seen.values()) view.messages.sort((left, right) => Number(left.seq) - Number(right.seq));
		views.push(...seen.values());
		const queue: QueueItem[] = inbox.filter(item => !item.injected).map(item => ({ id: `inbound:${item.attention.obligation_id}`, kind: "inbound", label: `${item.attention.thread_ref.id}/${item.attention.seq}`, detail: t("pending", { author: item.attention.authenticated_author }), at: item.attention.entry?.at }));
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
	async updateConfig(next: OnlyneConfig): Promise<void> { await saveConfig(next); await restartRuntime(next, this.pi, this.main, this.host); }
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
