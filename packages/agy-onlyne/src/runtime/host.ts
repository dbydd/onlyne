import { AsyncLocalStorage } from "node:async_hooks";
import { existsSync } from "node:fs";
import { mkdir } from "node:fs/promises";
import {
	OnlyneError,
	OnlyneNode,
	keyFingerprint,
	trustPhase,
	type Action,
	type Auth,
	type KeyHook,
	type KeyMaterial,
	type MaintenanceClock,
	type MsgInput,
	type NodeId,
	type NotifyEvent,
	type Outbound,
	type Page,
	type Runtime,
	type Thread,
	type ThreadRef,
	type TraceEvent,
	type TrustEntry,
} from "@onlyne/lib";
import { DEFAULT_ACQUISITION, type AcquisitionPolicy } from "@onlyne/node-host/acquire";
import { advertisedHost, createOnlyneNode } from "@onlyne/node-host/node";
import { ensureKeyDir, loadOrCreateKey, writeKeyMaterial } from "@onlyne/node-host/keys";
import { NetworkDiscovery } from "@onlyne/node-host/network";
import { ContextRegistry, SqliteStorage } from "@onlyne/node-host/store";
import {
	configDir,
	dbPath,
	DEFAULT_BIND,
	DEFAULT_PAGE_SIZE,
	loadConfig,
	normalizeNodeId,
	normalizeRegion,
	normalizeServiceUrl,
	saveConfig,
	shortNodeId,
	type AgyOnlyneConfig,
} from "../config.ts";
import { resolveLocale, setLocale } from "../i18n.ts";
import { JudgeDecider, type CallerContext, type JudgeFn } from "../core/decider.ts";
import { executeMsg, type MsgToolParams } from "../core/msg-service.ts";
import { threadKey } from "../core/resolve.ts";
import { readOnlyneUrl, enumerateOnlyneUris, type Caller, type RenderedUrl } from "../core/urls.ts";
import type {
	OnlyneStatus,
	Snapshot,
	TrustSnapshotEntry,
	ViewerContact,
	ViewerDiscovery,
	ViewerMessage,
	ViewerThread,
} from "../core/types.ts";
import { AntigravityBridge } from "./antigravity.ts";
import { DeliveryManager } from "./delivery.ts";
import { AcquisitionLog, appendHandshakeThread, beginPeer, judgeAcquisition } from "./handshake.ts";
import { createAntigravityJudge, type McpSamplingTarget } from "./judge-backend.ts";

export interface AgyRuntimeOptions {
	cwd: string;
	config?: AgyOnlyneConfig;
	inMemory?: boolean;
	discovery?: boolean;
	bindHttp?: boolean;
	customJudge?: JudgeFn;
	sampling?: McpSamplingTarget;
	bridge?: AntigravityBridge;
	log?(event: string, fields?: Record<string, unknown>): void;
}

interface ScopedCall {
	caller: CallerContext;
	capturedAction?: Action;
}

const REQUEST_IP_ALS = new AsyncLocalStorage<string | undefined>();
const CALLER_ALS = new AsyncLocalStorage<ScopedCall>();

function nodeClock(): MaintenanceClock {
	return {
		setTimer: (ms, run) => setTimeout(run, ms),
		clearTimer: handle => clearTimeout(handle as ReturnType<typeof setTimeout>),
	};
}

function fallbackConfig(cwd: string): AgyOnlyneConfig {
	return {
		path: `${configDir(cwd)}/config.toml`,
		language: "en",
		region: "urn:onlyne:region:local",
		node: { id: "urn:onlyne:agent:agy-local", key_scope: "none" },
		server: { bind: DEFAULT_BIND, port: 0 },
		threads: { page_size: DEFAULT_PAGE_SIZE },
		acquisition: { ...DEFAULT_ACQUISITION },
	};
}

export class AgyRuntime {
	readonly cwd: string;
	readonly mainAgentId: string;
	readonly storage: SqliteStorage;
	readonly contexts: ContextRegistry;
	readonly acquisitionLog: AcquisitionLog;
	readonly node: OnlyneNode;
	readonly decider: JudgeDecider;
	readonly delivery: DeliveryManager;
	readonly bridge?: AntigravityBridge;

	private _config: AgyOnlyneConfig;
	private _configured: boolean;
	private _endpoint: string;
	private _fingerprint: string | undefined;
	private _server: ReturnType<typeof Bun.serve> | undefined;
	private _network: NetworkDiscovery | undefined;
	private _clock = nodeClock();
	private _listeners = new Set<(event?: NotifyEvent) => void>();
	private _closed = false;
	private _sampling: McpSamplingTarget | undefined;
	private _log: (event: string, fields?: Record<string, unknown>) => void;

	private constructor(opts: {
		cwd: string;
		config: AgyOnlyneConfig;
		configured: boolean;
		storage: SqliteStorage;
		contexts: ContextRegistry;
		acquisitionLog: AcquisitionLog;
		node: OnlyneNode;
		decider: JudgeDecider;
		delivery: DeliveryManager;
		endpoint: string;
		fingerprint?: string;
		bridge?: AntigravityBridge;
		sampling?: McpSamplingTarget;
		log: (event: string, fields?: Record<string, unknown>) => void;
	}) {
		this.cwd = opts.cwd;
		this.mainAgentId = "antigravity-main";
		this._config = opts.config;
		this._configured = opts.configured;
		this.storage = opts.storage;
		this.contexts = opts.contexts;
		this.acquisitionLog = opts.acquisitionLog;
		this.node = opts.node;
		this.decider = opts.decider;
		this.delivery = opts.delivery;
		this._endpoint = opts.endpoint;
		this._fingerprint = opts.fingerprint;
		this.bridge = opts.bridge;
		this._sampling = opts.sampling;
		this._log = opts.log;
	}

	get config(): AgyOnlyneConfig {
		return this._config;
	}

	get configured(): boolean {
		return this._configured;
	}

	get endpoint(): string {
		return this._endpoint;
	}

	get fingerprint(): string | undefined {
		return this._fingerprint;
	}

	setSamplingTarget(sampling?: McpSamplingTarget): void {
		this._sampling = sampling;
	}

	onChange(listener: (event?: NotifyEvent) => void): () => void {
		this._listeners.add(listener);
		return () => {
			this._listeners.delete(listener);
		};
	}

	private emitChange(event?: NotifyEvent): void {
		for (const listener of this._listeners) {
			try {
				listener(event);
			} catch {
				// Listener errors never break node lifecycle.
			}
		}
	}

	static async start(options: AgyRuntimeOptions): Promise<AgyRuntime> {
		const log = options.log ?? (() => {});
		const loaded = options.config ?? await loadConfig(options.cwd);
		const configured = loaded !== undefined;
		let config = loaded ?? fallbackConfig(options.cwd);
		if (config.language !== undefined) setLocale(config.language);

		await mkdir(configDir(options.cwd), { recursive: true });
		const storage = new SqliteStorage(options.inMemory ? ":memory:" : dbPath(options.cwd));
		const contexts = new ContextRegistry(storage, config.node.id, "antigravity-main");
		await contexts.ensureMain();
		const acquisitionLog = new AcquisitionLog(storage);
		const bridge = options.bridge ?? AntigravityBridge.fromEnv();

		let keyDir: string | undefined;
		let keys: KeyHook | undefined;
		let fingerprint: string | undefined;
		const keyScope = config.node.key_scope ?? "project";
		if (keyScope !== "none") {
			keyDir = await ensureKeyDir(options.cwd, config.node.id, keyScope);
			const initialMaterial = await loadOrCreateKey(keyDir, config.node.id);
			fingerprint = await keyFingerprint(initialMaterial.public_key);
			keys = Object.assign(() => loadOrCreateKey(keyDir!, config.node.id), {
				replace: (material: KeyMaterial) => writeKeyMaterial(keyDir!, material, { replace: true }),
			});
		}

		const instanceId = `agy-${crypto.randomUUID().slice(0, 8)}`;
		const bindHttp = options.bindHttp ?? true;
		let httpServer: ReturnType<typeof Bun.serve> | undefined;
		let activeHandler: (request: Request) => Promise<Response> = async () => new Response("Starting Onlyne", { status: 503 });

		if (bindHttp) {
			httpServer = Bun.serve({
				hostname: config.server.bind,
				port: config.server.port ?? 0,
				fetch: (request, server) => {
					const ip = server.requestIP(request)?.address;
					return REQUEST_IP_ALS.run(ip, () => activeHandler(request));
				},
			});
			const boundPort = httpServer.port ?? config.server.port ?? 0;
			if (configured && config.server.port === undefined && boundPort > 0) {
				config = {
					...config,
					server: { ...config.server, port: boundPort },
				};
				await saveConfig(config).catch(() => undefined);
			}
		}

		const hostPart = advertisedHost(config.server.bind);
		const assignedPort = httpServer?.port ?? config.server.port ?? 0;
		const endpoint = config.server.public_url?.replace(/\/+$/, "")
			?? `http://${hostPart}:${assignedPort}/onlyne/v3`;

		let runtimeRef!: AgyRuntime;
		const runtimeHook: Runtime = {
			supports: async () => true,
			inject: async (entry, context, plan) => runtimeRef.delivery.inject(entry, context, plan),
			todo: async entry => runtimeRef.delivery.recordTodo(entry),
			notify: event => {
				void runtimeRef.handleNotify(event);
			},
			trace: (traceEvent: TraceEvent) => {
				log("onlyne.trace", traceEvent as unknown as Record<string, unknown>);
			},
		};

		const clock = nodeClock();
		const node = createOnlyneNode({
			config,
			endpoint,
			instanceId,
			storage,
			contexts,
			decide: async (input: MsgInput) => runtimeRef.decideFromScope(input),
			runtime: runtimeHook,
			clock,
			...(keyDir !== undefined ? { keyDir } : {}),
			...(keys !== undefined ? { keys } : {}),
		});

		activeHandler = async request => {
			if (new URL(request.url).pathname === "/health") {
				return Response.json({ ok: true, node_id: config.node.id, region: config.region, endpoint });
			}
			return node.fetch(request);
		};

		const judgeBackend = createAntigravityJudge({
			config: () => runtimeRef.config,
			customJudge: options.customJudge,
			sampling: {
				supportsSampling: () => runtimeRef._sampling?.supportsSampling() ?? false,
				createMessage: (params, signal) => runtimeRef._sampling!.createMessage(params, signal),
			},
			log,
		});

		const decider = new JudgeDecider({
			config,
			mainAgentId: "antigravity-main",
			node,
			contexts,
			listThreads: cursor => node.threads.list(node.auth, { page_size: config.threads.page_size, ...(cursor ? { cursor } : {}) }),
			listJoined: cursor => node.threads.joined(node.auth, { page_size: config.threads.page_size, ...(cursor ? { cursor } : {}) }),
			listInbox: async () => (await node.messages.inbox(node.auth, { page_size: 100 })).items,
			listMessages: (thread, cursor) => node.client.threadEntries(thread, {
				attention: "thread",
				page_size: 100,
				...(cursor ? { cursor } : {}),
			}),
			listDiscovery: () => node.discovery.listRecords(),
			listTrust: () => node.trust.list(node.auth),
			peerBase: async nodeId => (await node.trust.entry(nodeId))?.base_url,
			targetContexts: async nodeId => {
				if (nodeId === config.node.id) {
					return { available: true, value: await contexts.list() };
				}
				try {
					const target = await node.discovery.resolve(nodeId);
					const res = await fetch(`${target.base_url.replace(/\/onlyne\/v3$/, "")}/onlyne/v3/contexts`, {
						headers: target.headers,
					});
					if (!res.ok) return { available: true, value: [{ alias: "main", state: "idle" }] };
					const data = await res.json() as { items?: Array<{ alias: string; state?: string; summary?: string }> };
					return { available: true, value: data.items ?? [{ alias: "main", state: "idle" }] };
				} catch {
					return { available: true, value: [{ alias: "main", state: "idle" }] };
				}
			},
			judge: judgeBackend,
			log,
		});

		const delivery = new DeliveryManager({
			config: () => runtimeRef.config,
			node,
			storage,
			contexts,
			decider,
			bridge,
			onChanged: () => runtimeRef.emitChange(),
			log,
		});

		runtimeRef = new AgyRuntime({
			cwd: options.cwd,
			config,
			configured,
			storage,
			contexts,
			acquisitionLog,
			node,
			decider,
			delivery,
			endpoint,
			fingerprint,
			bridge,
			sampling: options.sampling,
			log,
		});
		runtimeRef._server = httpServer;
		runtimeRef._clock = clock;

		// Seed trust anchors & configured discovery seeds before maintenance starts.
		for (const seed of config.trust ?? []) {
			await acquisitionLog.recordAudit({
				at: new Date().toISOString(),
				actor: "seed",
				action: "trust_seed_configured",
				node_id: seed.node_id,
				fingerprint: seed.fingerprint,
				allowed: true,
				override: true,
				source: config.path,
			}).catch(() => undefined);
		}
		for (const rawSeed of config.discovery?.seeds ?? []) {
			await node.discovery.observe(normalizeServiceUrl(rawSeed), "bootstrap").catch(() => undefined);
		}

		node.client.runMaintenance();

		if (options.discovery ?? false) {
			const network = new NetworkDiscovery({
				node_id: config.node.id,
				instance_id: instanceId,
				endpoint,
				bind: config.server.bind,
				onCandidate: async (url, source, expectedNode) => {
					if (expectedNode === config.node.id) return;
					await node.discovery.observe(url, source, expectedNode);
					runtimeRef.emitChange();
				},
				onError: err => log("onlyne.discovery.error", { error: String(err) }),
			});
			try {
				await network.start();
				runtimeRef._network = network;
			} catch (err) {
				log("onlyne.discovery.unavailable", { error: String(err) });
			}
		}

		return runtimeRef;
	}

	async runScopedDecision<T>(
		caller: CallerContext,
		work: (captureAction: (action: Action) => void) => Promise<T>,
	): Promise<{ value: T; action?: Action }> {
		const scoped: ScopedCall = { caller };
		const value = await CALLER_ALS.run(scoped, () => work(action => {
			scoped.capturedAction = action;
		}));
		return { value, ...(scoped.capturedAction === undefined ? {} : { action: scoped.capturedAction }) };
	}

	private async decideFromScope(input: { delivery_kind: string; msg: string; meta?: Record<string, import("@onlyne/lib").Json> }): Promise<Action> {
		const scoped = CALLER_ALS.getStore();
		const caller: CallerContext = scoped?.caller ?? {
			agent_id: this.mainAgentId,
			main: true,
			read_only: false,
		};
		const action = await this.decider.decide(caller, input);
		if (scoped !== undefined) scoped.capturedAction = action;
		return action;
	}

	private async handleNotify(event: NotifyEvent): Promise<void> {
		const now = new Date().toISOString();
		if (event.event === "trust") {
			const { entry, phase, cause } = event;
			const observedIp = REQUEST_IP_ALS.getStore();
			if (cause === "peer-statement" && observedIp !== undefined) {
				await this.acquisitionLog.observe(entry.node_id, entry.fingerprint, {
					observed: observedIp,
					at: now,
					cause,
				});
			} else {
				await this.acquisitionLog.transition(entry.node_id, entry.fingerprint, now, cause);
			}

			// Automatic acquisition policy check on incoming invited peer statement.
			if (cause === "peer-statement" && phase === "invited") {
				const approvalDeps = this.approvalDeps();
				const { verdict, input } = await judgeAcquisition(approvalDeps, entry.node_id, entry.fingerprint, entry.base_url);
				const delivered = await this.acquisitionLog.delivered(entry.node_id, entry.fingerprint, phase);
				if (verdict.allowed) {
					const accepted = await this.node.trust.accept(this.node.auth, entry.node_id, entry.fingerprint);
					await this.acquisitionLog.recordAudit({
						at: now,
						actor: "policy",
						action: "trust_auto_accept",
						node_id: entry.node_id,
						fingerprint: entry.fingerprint,
						observed: input.observed,
						declared: input.declared,
						policy: approvalDeps.policy.policy,
						allowed: true,
						reason: verdict.reason,
					});
					await appendHandshakeThread(
						this.node,
						accepted,
						"auto-accepted",
						`Auto-accepted trust with ${entry.node_id} (fp ${entry.fingerprint}, reason=${verdict.reason}).`,
					);
				} else if (!delivered) {
					await this.acquisitionLog.markDelivered(entry.node_id, entry.fingerprint, phase, now);
					await this.acquisitionLog.recordAudit({
						at: now,
						actor: "policy",
						action: "trust_pending_human",
						node_id: entry.node_id,
						fingerprint: entry.fingerprint,
						observed: input.observed,
						declared: input.declared,
						policy: approvalDeps.policy.policy,
						allowed: false,
						reason: verdict.reason,
					});
					await appendHandshakeThread(
						this.node,
						entry,
						cause,
						`Incoming trust invitation from ${entry.node_id} (fp ${entry.fingerprint}) awaits human approval (${verdict.reason}).`,
					);
				}
			} else if (phase === "exchanged" || phase === "revoked") {
				await appendHandshakeThread(
					this.node,
					entry,
					cause,
					`Trust transition for ${entry.node_id}: ${phase} (${cause}).`,
				);
			}
		}
		this.emitChange(event);
	}

	private approvalDeps() {
		return {
			node: this.node,
			policy: this._config.acquisition ?? DEFAULT_ACQUISITION,
			log: this.acquisitionLog,
			recordedAddresses: async (nodeId: NodeId) => {
				const records = await this.node.discovery.listRecords().catch(() => []);
				return records.filter(r => r.node_id === nodeId).map(r => r.base_url);
			},
		};
	}

	async listOutbound(): Promise<Outbound[]> {
		const rows = await this.storage.transaction(tx => tx.scan<Outbound>("outbound/"));
		return rows.map(r => r.value);
	}

	async executeMsgTool(caller: CallerContext, params: MsgToolParams, signal?: AbortSignal): Promise<{ text: string; details: Record<string, unknown> }> {
		const res = await executeMsg(this, caller, params, this.storage, signal);
		this.emitChange();
		return res;
	}

	async readUrl(caller: Caller, href: string): Promise<RenderedUrl> {
		return readOnlyneUrl({
			config: this._config,
			endpoint: this._endpoint,
			mainAgentId: this.mainAgentId,
			node: this.node,
			contexts: this.contexts,
			listOutbound: () => this.listOutbound(),
			listTodos: () => this.delivery.listTodos(),
		}, caller, href);
	}

	async listUris(): Promise<string[]> {
		return enumerateOnlyneUris({
			config: this._config,
			endpoint: this._endpoint,
			mainAgentId: this.mainAgentId,
			node: this.node,
			contexts: this.contexts,
		});
	}

	async listThreads(cursor?: string): Promise<Page<Thread>> {
		return this.node.threads.list(this.node.auth, { page_size: this._config.threads.page_size, ...(cursor ? { cursor } : {}) });
	}

	async connectSeed(rawUrl: string): Promise<ViewerDiscovery> {
		const url = normalizeServiceUrl(rawUrl);
		const record = await this.node.discovery.observe(url, "manual");
		if (!this._config.discovery?.seeds?.includes(url) && this._configured) {
			const nextSeeds = [...(this._config.discovery?.seeds ?? []), url];
			await this.updateConfig({
				discovery: { seeds: nextSeeds },
			});
		}
		this.emitChange();
		return {
			node_id: record.node_id,
			instance_id: record.instance_id,
			base_url: record.base_url,
			source: record.source,
			...(record.membership?.domain_id ? { region: record.membership.domain_id } : {}),
			...(record.membership?.state ? { membership_state: record.membership.state } : {}),
			...(record.verified_at ? { verified_at: record.verified_at } : {}),
			expires_at: record.expires_at,
			last_seen: record.last_seen,
			...(record.error ? { error: record.error.message } : {}),
		};
	}

	async refreshDiscovery(): Promise<void> {
		await this.node.discovery.refresh();
		this.emitChange();
	}

	async trustBegin(nodeIdRaw: string): Promise<{ node_id: NodeId; fingerprint: string; base_url: string }> {
		const nodeId = normalizeNodeId(nodeIdRaw);
		const exchange = await this.node.trust.begin(this.node.auth, nodeId);
		return { node_id: exchange.node_id, fingerprint: exchange.fingerprint, base_url: exchange.base_url };
	}

	async trustConfirm(nodeIdRaw: string, fingerprint: string): Promise<TrustEntry> {
		const nodeId = normalizeNodeId(nodeIdRaw);
		const exchange = await this.node.trust.begin(this.node.auth, nodeId);
		const committed = await exchange.confirm(fingerprint);
		await this.acquisitionLog.recordAudit({
			at: new Date().toISOString(),
			actor: "human",
			action: "trust_confirm_fingerprint",
			node_id: nodeId,
			fingerprint: committed.fingerprint,
			allowed: true,
		});
		this.emitChange();
		return committed;
	}

	async trustBeginOrApprove(rawTarget: string, actor: "human" | "agent" = "human") {
		const nodeId = normalizeNodeId(rawTarget);
		if (actor === "human") {
			const res = await beginPeer(this.approvalDeps(), nodeId, "human");
			const entry = res.confirmed!;
			const phase = trustPhase(entry);
			this.emitChange();
			return {
				status: phase === "exchanged" ? ("exchanged" as const) : ("requested" as const),
				node_id: entry.node_id,
				fingerprint: entry.fingerprint,
				verdict: res.verdict,
			};
		}
		const exchange = await this.node.trust.begin(this.node.auth, nodeId);
		const { verdict } = await judgeAcquisition(this.approvalDeps(), exchange.node_id, exchange.fingerprint, exchange.base_url);
		if (verdict.allowed) {
			const confirmed = await exchange.confirm(exchange.fingerprint);
			this.emitChange();
			return {
				status: trustPhase(confirmed) === "exchanged" ? ("exchanged" as const) : ("requested" as const),
				node_id: confirmed.node_id,
				fingerprint: confirmed.fingerprint,
				verdict,
			};
		}
		await this.acquisitionLog.recordAudit({
			at: new Date().toISOString(),
			actor: "agent",
			action: "trust_begin_await_human",
			node_id: exchange.node_id,
			fingerprint: exchange.fingerprint,
			allowed: false,
			reason: verdict.reason,
		});
		return {
			status: "pending_human" as const,
			node_id: exchange.node_id,
			fingerprint: exchange.fingerprint,
			verdict,
		};
	}

	async trustAccept(nodeIdRaw: string): Promise<TrustEntry> {
		const nodeId = normalizeNodeId(nodeIdRaw);
		const existing = await this.node.trust.entry(nodeId);
		if (existing === undefined) {
			const ex = await this.node.trust.begin(this.node.auth, nodeId);
			const entry = await ex.confirm(ex.fingerprint);
			this.emitChange();
			return entry;
		}
		const entry = await this.node.trust.accept(this.node.auth, nodeId, existing.fingerprint);
		await this.acquisitionLog.recordAudit({
			at: new Date().toISOString(),
			actor: "human",
			action: "trust_accept",
			node_id: nodeId,
			fingerprint: entry.fingerprint,
			allowed: true,
		});
		this.emitChange();
		return entry;
	}

	async trustRevoke(nodeIdRaw: string): Promise<TrustEntry> {
		const nodeId = normalizeNodeId(nodeIdRaw);
		await this.node.trust.revoke(this.node.auth, nodeId);
		await this.acquisitionLog.settle(nodeId);
		await this.acquisitionLog.recordAudit({
			at: new Date().toISOString(),
			actor: "human",
			action: "trust_revoke",
			node_id: nodeId,
			allowed: false,
		});
		const after = await this.node.trust.entry(nodeId);
		this.emitChange();
		if (after === undefined) {
			throw new OnlyneError("not_found", `Trust entry for ${nodeId} not found`);
		}
		return after;
	}

	async trustRotate(): Promise<string> {
		const identity = await this.node.trust.rotate(this.node.auth);
		this._fingerprint = identity.fingerprint;
		this.emitChange();
		return identity.fingerprint;
	}

	async setAcquisition(policy: AcquisitionPolicy): Promise<void> {
		await this.updateConfig({ acquisition: policy });
	}

	async updateConfig(patch: Partial<AgyOnlyneConfig>): Promise<AgyOnlyneConfig> {
		const next: AgyOnlyneConfig = {
			...this._config,
			...patch,
			path: `${configDir(this.cwd)}/config.toml`,
			node: { ...this._config.node, ...(patch.node ?? {}) },
			server: { ...this._config.server, ...(patch.server ?? {}) },
			threads: { ...this._config.threads, ...(patch.threads ?? {}) },
		};
		if (patch.region) next.region = normalizeRegion(patch.region);
		if (patch.node?.id) next.node.id = normalizeNodeId(patch.node.id);
		if (next.language) setLocale(next.language);
		await saveConfig(next);
		this._config = next;
		this._configured = true;
		this.emitChange();
		return next;
	}

	async status(): Promise<OnlyneStatus> {
		const auth: Auth = this.node.auth;
		const [domains, trust, discovery, inbox, contexts, pendingDeliveries] = await Promise.all([
			this.node.domains.list(auth, { page_size: 50 }).catch(() => ({ items: [] })),
			this.node.trust.list(auth).catch(() => [] as TrustEntry[]),
			this.node.discovery.listRecords().catch(() => []),
			this.node.messages.inbox(auth, { page_size: 100 }).catch(() => ({ items: [] })),
			this.contexts.list().catch(() => []),
			this.delivery.listPending().catch(() => []),
		]);
		const outbound = await this.listOutbound().catch(() => []);
		const reachable = discovery.filter(d => d.error === undefined && d.membership?.state !== "left" && d.node_id !== this._config.node.id);
		const trusted = trust.filter(tr => trustPhase(tr) === "exchanged").length;
		const pendingTrust = trust.filter(tr => trustPhase(tr) === "invited" || trustPhase(tr) === "requested").length;
		const unread = inbox.items.filter(item => !item.injected && item.attention.disposition !== "expired").length;
		return {
			node_id: this._config.node.id,
			region: this._config.region,
			endpoint: this._endpoint,
			domains: domains.items.filter(d => d.state === "active").map(d => d.domain_id),
			peers_total: new Set([...trust.map(t => t.node_id), ...discovery.map(d => d.node_id)].filter(id => id !== this._config.node.id)).size,
			peers_reachable: reachable.length,
			trusted,
			pending_trust: pendingTrust,
			pending_keys: trust.filter(tr => trustPhase(tr) === "requested").length,
			pending_approvals: trust.filter(tr => trustPhase(tr) === "invited").length,
			unread,
			inbox_count: inbox.items.length,
			outbound_pending: outbound.filter(o => o.state === "pending").length,
			contexts_active: contexts.filter(c => c.state !== "lost").length,
			decisions_pending: pendingDeliveries.length,
		};
	}

	async snapshot(): Promise<Snapshot> {
		const auth = this.node.auth;
		const [
			status,
			ownedThreads,
			joinedThreads,
			inbox,
			trustRows,
			discoveryRows,
			domainRows,
			contexts,
			audit,
			todos,
			pendingDeliveries,
		] = await Promise.all([
			this.status(),
			this.node.threads.list(auth, { page_size: this._config.threads.page_size }).catch(() => ({ items: [] })),
			this.node.threads.joined(auth, { page_size: this._config.threads.page_size }).catch(() => ({ items: [] })),
			this.node.messages.inbox(auth, { page_size: 100 }).catch(() => ({ items: [] })),
			this.node.trust.list(auth).catch(() => [] as TrustEntry[]),
			this.node.discovery.listRecords().catch(() => []),
			this.node.domains.list(auth, { page_size: 50 }).catch(() => ({ items: [] })),
			this.contexts.list().catch(() => []),
			this.acquisitionLog.listAudit(50).catch(() => []),
			this.delivery.listTodos().catch(() => []),
			this.delivery.listPending().catch(() => []),
		]);

		const unreadMap = new Map<string, number>();
		for (const item of inbox.items) {
			if (item.injected || item.attention.disposition === "expired") continue;
			const k = threadKey(item.attention.thread_ref);
			unreadMap.set(k, (unreadMap.get(k) ?? 0) + 1);
		}

		const buildMessages = async (ref: ThreadRef): Promise<ViewerMessage[]> => {
			const page = await this.node.client.threadEntries(ref, { attention: "thread", page_size: 80 }).catch(() => ({ items: [] }));
			return page.items.map(entry => ({
				ref: { sender: entry.message.sender, id: entry.message.id },
				seq: entry.seq,
				direction: entry.message.sender === this._config.node.id ? "out" : "in",
				author: shortNodeId(entry.message.sender),
				at: entry.at,
				text: entry.message.msg,
				...(entry.message.meta?.relation ? { relation: entry.message.meta.relation } : {}),
				...(entry.message.meta?.work ? { work: entry.message.meta.work } : {}),
			}));
		};

		const threads: ViewerThread[] = [];
		for (const t of ownedThreads.items) {
			if (t.thread_ref.id === "trust") continue;
			const messages = await buildMessages(t.thread_ref);
			threads.push({
				ref: t.thread_ref,
				alias: threadKey(t.thread_ref),
				title: t.title || t.thread_ref.id,
				state: t.state,
				unread: unreadMap.get(threadKey(t.thread_ref)) ?? 0,
				last_seq: t.next_seq,
				messages,
			});
		}
		for (const j of joinedThreads.items) {
			const messages = await buildMessages(j.thread_ref);
			threads.push({
				ref: j.thread_ref,
				alias: threadKey(j.thread_ref),
				title: j.title ?? j.thread_ref.id,
				state: j.state ?? "open",
				unread: unreadMap.get(threadKey(j.thread_ref)) ?? 0,
				last_seq: j.last_seq,
				messages,
				read_only: true,
			});
		}

		const trust: TrustSnapshotEntry[] = trustRows.map(tr => ({
			node_id: tr.node_id,
			phase: trustPhase(tr),
			fingerprint: tr.fingerprint,
			...(this._fingerprint ? { local_fingerprint: this._fingerprint } : {}),
			url: tr.base_url,
			verified: tr.local_approved && tr.remote_approved,
			updated_at: tr.last_checked,
			last_seen: tr.last_checked,
			peer_confirmed: tr.remote_approved,
			local_confirmed: tr.local_approved,
		}));

		const discovery: ViewerDiscovery[] = discoveryRows
			.filter(d => d.node_id !== this._config.node.id)
			.map(d => ({
				node_id: d.node_id,
				instance_id: d.instance_id,
				base_url: d.base_url,
				source: d.source,
				...(d.membership?.domain_id ? { region: d.membership.domain_id } : {}),
				...(d.membership?.state ? { membership_state: d.membership.state } : {}),
				...(d.verified_at ? { verified_at: d.verified_at } : {}),
				expires_at: d.expires_at,
				last_seen: d.last_seen,
				...(d.error ? { error: d.error.message } : {}),
			}));

		const contacts: ViewerContact[] = discovery
			.filter(d => d.error === undefined && d.membership_state !== "left")
			.map(d => ({
				node_id: d.node_id,
				instance_id: d.instance_id,
				base_url: d.base_url,
				...(d.region ? { region: d.region } : {}),
				last_seen: d.last_seen,
			}));

		const queue = [
			...inbox.items
				.filter(row => !row.injected)
				.map(row => ({
					id: `${threadKey(row.attention.thread_ref)}#${row.attention.seq}`,
					kind: "inbound" as const,
					label: `${shortNodeId(row.attention.authenticated_author)} · ${row.attention.thread_ref.id}#${row.attention.seq}`,
					detail: row.attention.entry?.message.msg.replace(/\s+/g, " ").trim().slice(0, 140) ?? "",
					at: row.attention.entry?.at,
				})),
			...pendingDeliveries.map(d => ({
				id: d.id,
				kind: "decision" as const,
				label: `Pending -> ${d.context_alias}`,
				detail: `${shortNodeId(d.sender)}: ${d.text.replace(/\s+/g, " ").trim().slice(0, 120)}`,
				at: d.created_at,
			})),
		];

		return {
			at: new Date().toISOString(),
			workspace: this.cwd,
			configured: this._configured,
			language: resolveLocale(this._config.language),
			status,
			...(this._fingerprint ? { fingerprint: this._fingerprint } : {}),
			acquisition: {
				...(this._config.acquisition ?? DEFAULT_ACQUISITION),
				allow_loopback: true,
			},
			audit,
			threads,
			trust,
			domains: domainRows.items.map(d => ({
				domain_id: d.domain_id,
				state: d.state,
				revision: d.revision,
				members: [d.node_id],
				updated_at: new Date().toISOString(),
			})),
			discovery,
			contacts,
			contexts,
			queue,
			todos,
			pending_deliveries: pendingDeliveries,
			judge_mode: this._config.judge?.mode ?? "auto",
		};
	}

	async close(): Promise<void> {
		if (this._closed) return;
		this._closed = true;
		this.delivery.close();
		await this._network?.close().catch(() => undefined);
		this._server?.stop(true);
		this._clock;
		this.storage.close();
	}
}
