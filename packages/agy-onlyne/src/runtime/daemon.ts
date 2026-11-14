import { timingSafeEqual } from "node:crypto";
import { existsSync } from "node:fs";
import { chmod, mkdir, readFile, rename, rm, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import type { Json } from "@onlyne/lib";
import { parseAllowBlock, type AcquirePolicy } from "@onlyne/node-host/acquire";
import {
	configDir,
	configPath,
	daemonFilePath,
	normalizeNodeId,
	normalizeRegion,
	normalizeServiceUrl,
	saveConfig,
	type AgyOnlyneConfig,
} from "../config.ts";
import { setLocale } from "../i18n.ts";
import type { CallerContext, JudgeFn } from "../core/decider.ts";
import { listDrafts } from "../core/drafts.ts";
import type { MsgToolParams } from "../core/msg-service.ts";
import type { Caller } from "../core/urls.ts";
import type { HookEventInput } from "./delivery.ts";
import { AgyRuntime, type AgyRuntimeOptions } from "./host.ts";

export interface DaemonInfo {
	pid: number;
	cwd: string;
	ctrlUrl: string;
	token: string;
	nodeId: string;
	endpoint: string;
	startedAt: string;
}

export interface DaemonServerOptions {
	cwd: string;
	token?: string;
	runtimeOptions?: Omit<AgyRuntimeOptions, "cwd">;
	renderGuiHtml?: () => Promise<string> | string;
}

function safeEqual(a: string, b: string): boolean {
	const ab = Buffer.from(a);
	const bb = Buffer.from(b);
	if (ab.length !== bb.length) return false;
	return timingSafeEqual(ab, bb);
}

function bearerToken(request: Request, url: URL): string | undefined {
	const auth = request.headers.get("authorization");
	if (auth && auth.toLowerCase().startsWith("bearer ")) {
		return auth.slice("bearer ".length).trim();
	}
	return url.searchParams.get("token") ?? undefined;
}

async function readJsonBody<T>(request: Request): Promise<T> {
	const raw = await request.text();
	if (raw.trim().length === 0) return {} as T;
	return JSON.parse(raw) as T;
}

function serializeError(error: unknown): { status: number; body: Record<string, unknown> } {
	if (error !== null && typeof error === "object" && "code" in error) {
		const err = error as { code?: string; message?: string; details?: unknown };
		const status = err.code === "forbidden"
			? 403
			: err.code === "not_found"
				? 404
				: err.code === "invalid_request"
					? 400
					: 500;
		return {
			status,
			body: {
				error: err.message ?? String(error),
				code: err.code,
				...(err.details === undefined ? {} : { details: err.details }),
			},
		};
	}
	return {
		status: 500,
		body: {
			error: error instanceof Error ? error.message : String(error),
			code: "internal",
		},
	};
}

export class DaemonServer {
	readonly cwd: string;
	readonly token: string;
	readonly infoPath: string;
	private runtime: AgyRuntime;
	private readonly server: ReturnType<typeof Bun.serve>;
	private readonly sseControllers = new Set<ReadableStreamDefaultController<Uint8Array>>();
	private readonly renderGuiHtml?: () => Promise<string> | string;
	private unsubscribeRuntime: () => void;
	private runtimeOptions: Omit<AgyRuntimeOptions, "cwd">;
	private closed = false;

	private constructor(opts: {
		cwd: string;
		token: string;
		infoPath: string;
		runtime: AgyRuntime;
		server: ReturnType<typeof Bun.serve>;
		runtimeOptions: Omit<AgyRuntimeOptions, "cwd">;
		renderGuiHtml?: () => Promise<string> | string;
	}) {
		this.cwd = opts.cwd;
		this.token = opts.token;
		this.infoPath = opts.infoPath;
		this.runtime = opts.runtime;
		this.server = opts.server;
		this.runtimeOptions = opts.runtimeOptions;
		this.renderGuiHtml = opts.renderGuiHtml;
		this.unsubscribeRuntime = this.runtime.onChange(() => this.broadcast("change"));
	}

	get ctrlUrl(): string {
		return `http://127.0.0.1:${this.server.port ?? 0}`;
	}

	get activeRuntime(): AgyRuntime {
		return this.runtime;
	}

	info(): DaemonInfo {
		return {
			pid: process.pid,
			cwd: this.cwd,
			ctrlUrl: this.ctrlUrl,
			token: this.token,
			nodeId: this.runtime.config.node.id,
			endpoint: this.runtime.endpoint,
			startedAt: new Date().toISOString(),
		};
	}

	static async start(options: DaemonServerOptions): Promise<DaemonServer> {
		const token = options.token ?? Buffer.from(crypto.getRandomValues(new Uint8Array(24))).toString("hex");
		const runtimeOptions: Omit<AgyRuntimeOptions, "cwd"> = options.runtimeOptions ?? {};
		const runtime = await AgyRuntime.start({ cwd: options.cwd, ...runtimeOptions });
		const infoPath = daemonFilePath(options.cwd);

		let daemon!: DaemonServer;
		const server = Bun.serve({
			hostname: "127.0.0.1",
			port: 0,
			idleTimeout: 120,
			fetch: request => daemon.handleRequest(request),
		});

		daemon = new DaemonServer({
			cwd: options.cwd,
			token,
			infoPath,
			runtime,
			server,
			runtimeOptions,
			renderGuiHtml: options.renderGuiHtml,
		});

		if (!runtimeOptions.inMemory) {
			await daemon.writeInfoFile();
		}
		return daemon;
	}

	private async writeInfoFile(): Promise<void> {
		const dir = dirname(this.infoPath);
		await mkdir(dir, { recursive: true });
		const tmp = join(dir, `.agy-daemon.${process.pid}.tmp`);
		await writeFile(tmp, `${JSON.stringify(this.info(), null, 2)}\n`, { mode: 0o600 });
		await chmod(tmp, 0o600);
		await rename(tmp, this.infoPath);
	}

	private broadcast(event: string): void {
		const payload = new TextEncoder().encode(`event: ${event}\ndata: ${JSON.stringify({ at: new Date().toISOString() })}\n\n`);
		for (const controller of this.sseControllers) {
			try {
				controller.enqueue(payload);
			} catch {
				this.sseControllers.delete(controller);
			}
		}
	}

	async reconfigure(config: AgyOnlyneConfig): Promise<AgyOnlyneConfig> {
		await saveConfig({ ...config, path: configPath(this.cwd) });
		this.unsubscribeRuntime();
		await this.runtime.close();
		this.runtime = await AgyRuntime.start({
			cwd: this.cwd,
			...this.runtimeOptions,
			config: undefined,
		});
		this.unsubscribeRuntime = this.runtime.onChange(() => this.broadcast("change"));
		if (!this.runtimeOptions.inMemory) {
			await this.writeInfoFile();
		}
		this.broadcast("change");
		return this.runtime.config;
	}

	private async handleRequest(request: Request): Promise<Response> {
		const url = new URL(request.url);
		if (request.method === "GET" && url.pathname === "/health") {
			const token = bearerToken(request, url);
			if (!token || !safeEqual(token, this.token)) {
				return Response.json({ ok: false, error: "unauthorized" }, { status: 401 });
			}
			return Response.json({
				ok: true,
				pid: process.pid,
				cwd: this.cwd,
				nodeId: this.runtime.config.node.id,
				endpoint: this.runtime.endpoint,
				configured: this.runtime.configured,
			});
		}

		const token = bearerToken(request, url);
		if (!token || !safeEqual(token, this.token)) {
			return Response.json({ error: "Unauthorized" }, { status: 401 });
		}

		try {
			if (request.method === "GET" && (url.pathname === "/" || url.pathname === "/ui")) {
				if (this.renderGuiHtml === undefined) {
					return new Response("GUI renderer unavailable", { status: 404 });
				}
				const rawHtml = await this.renderGuiHtml();
				const bootScript = `<script>window.__ONLYNE_BOOTSTRAP__=${JSON.stringify({
					mode: "http",
					apiBase: this.ctrlUrl,
					token: this.token,
				})};</script>`;
				const html = rawHtml.includes("</head>")
					? rawHtml.replace("</head>", `${bootScript}\n</head>`)
					: `${bootScript}\n${rawHtml}`;
				return new Response(html, {
					headers: { "content-type": "text/html; charset=utf-8", "cache-control": "no-store" },
				});
			}

			if (request.method === "GET" && url.pathname === "/events") {
				const encoder = new TextEncoder();
				let ref!: ReadableStreamDefaultController<Uint8Array>;
				const stream = new ReadableStream<Uint8Array>({
					start: controller => {
						ref = controller;
						this.sseControllers.add(controller);
						controller.enqueue(encoder.encode(`event: ready\ndata: ${JSON.stringify({ at: new Date().toISOString() })}\n\n`));
					},
					cancel: () => {
						this.sseControllers.delete(ref);
					},
				});
				return new Response(stream, {
					headers: {
						"content-type": "text/event-stream",
						"cache-control": "no-cache",
						connection: "keep-alive",
					},
				});
			}

			if (request.method === "GET" && url.pathname === "/api/snapshot") {
				return Response.json(await this.runtime.snapshot());
			}

			if (request.method === "GET" && url.pathname === "/api/drafts") {
				return Response.json({ items: await listDrafts(this.runtime.storage) });
			}

			if (request.method === "GET" && url.pathname === "/api/uris") {
				return Response.json({ uris: await this.runtime.listUris() });
			}

			if (request.method === "POST" && url.pathname === "/api/msg") {
				const body = await readJsonBody<{
					caller?: CallerContext;
					conversationId?: string;
					params: MsgToolParams;
					samplingCallback?: { url: string; token: string };
				}>(request);
				if (body.samplingCallback) {
					this.runtime.setSamplingTarget({
						supportsSampling: () => true,
						createMessage: async (params, signal) => {
							const res = await fetch(body.samplingCallback!.url, {
								method: "POST",
								headers: {
									"content-type": "application/json",
									authorization: `Bearer ${body.samplingCallback!.token}`,
								},
								body: JSON.stringify(params),
								signal,
							});
							if (!res.ok) throw new Error(`Sampling callback HTTP ${res.status}`);
							return await res.json() as { content: { type: string; text?: string } };
						},
					});
				}
				try {
					const caller: CallerContext = body.caller
						?? await this.resolveCallerForConversation(body.conversationId);
					const res = await this.runtime.executeMsgTool(caller, body.params);
					return Response.json(res);
				} finally {
					if (body.samplingCallback) this.runtime.setSamplingTarget(undefined);
				}
			}

			if (request.method === "POST" && url.pathname === "/api/read") {
				const body = await readJsonBody<{ caller?: Caller; conversationId?: string; url: string }>(request);
				const resolved = body.caller ?? await (async () => {
					const c = await this.resolveCallerForConversation(body.conversationId);
					return { agent_id: c.agent_id, main: c.main };
				})();
				return Response.json(await this.runtime.readUrl(resolved, body.url));
			}

			if (request.method === "POST" && url.pathname === "/api/hooks") {
				const body = await readJsonBody<HookEventInput>(request);
				return Response.json(await this.runtime.delivery.processHookEvent(body));
			}

			if (request.method === "POST" && url.pathname === "/api/actions/connect") {
				const body = await readJsonBody<{ url: string }>(request);
				const record = await this.runtime.connectSeed(body.url);
				return Response.json({ ok: true, record });
			}

			if (request.method === "POST" && url.pathname === "/api/actions/refresh") {
				await this.runtime.refreshDiscovery();
				return Response.json({ ok: true });
			}

			if (request.method === "POST" && url.pathname === "/api/actions/trust/begin") {
				const body = await readJsonBody<{ node_id: string }>(request);
				return Response.json(await this.runtime.trustBegin(body.node_id));
			}

			if (request.method === "POST" && url.pathname === "/api/actions/trust/confirm") {
				const body = await readJsonBody<{ node_id: string; fingerprint: string }>(request);
				const entry = await this.runtime.trustConfirm(body.node_id, body.fingerprint);
				return Response.json({ ok: true, entry });
			}

			if (request.method === "POST" && url.pathname === "/api/actions/trust/accept") {
				const body = await readJsonBody<{ node_id: string }>(request);
				const entry = await this.runtime.trustAccept(body.node_id);
				return Response.json({ ok: true, entry });
			}

			if (request.method === "POST" && url.pathname === "/api/actions/trust/revoke") {
				const body = await readJsonBody<{ node_id: string }>(request);
				const entry = await this.runtime.trustRevoke(body.node_id);
				return Response.json({ ok: true, entry });
			}

			if (request.method === "POST" && url.pathname === "/api/actions/trust/rotate") {
				const fingerprint = await this.runtime.trustRotate();
				return Response.json({ ok: true, fingerprint });
			}

			if (request.method === "POST" && url.pathname === "/api/actions/acquisition") {
				const body = await readJsonBody<{ policy: AcquirePolicy; allow?: string[] }>(request);
				const allow = (body.allow ?? []).map(s => s.trim()).filter(Boolean);
				for (const item of allow) {
					if (parseAllowBlock(item) === undefined) {
						return Response.json({ error: `Invalid allow block: ${item}`, code: "invalid_request" }, { status: 400 });
					}
				}
				await this.runtime.setAcquisition({ policy: body.policy, allow });
				return Response.json({ ok: true });
			}

			if (request.method === "POST" && url.pathname === "/api/actions/language") {
				const body = await readJsonBody<{ language: "en" | "zh-CN" }>(request);
				setLocale(body.language);
				if (this.runtime.configured) {
					await this.runtime.updateConfig({ language: body.language });
				}
				return Response.json({ ok: true, language: body.language });
			}

			if (request.method === "POST" && url.pathname === "/api/actions/config") {
				const body = await readJsonBody<{
					region: string;
					node_id: string;
					bind?: string;
					port?: number;
					key_scope?: "project" | "global" | "none";
					page_size?: number;
					seeds?: string[];
					policy?: AcquirePolicy;
					allow?: string[];
					judge_mode?: "auto" | "sampling" | "openai" | "gemini" | "agy-cli" | "none";
					judge_model?: string;
					judge_base_url?: string;
					language?: "en" | "zh-CN";
				}>(request);
				const next: AgyOnlyneConfig = {
					path: configPath(this.cwd),
					language: body.language ?? this.runtime.config.language ?? "en",
					region: normalizeRegion(body.region),
					node: {
						id: normalizeNodeId(body.node_id),
						key_scope: body.key_scope ?? "project",
					},
					server: {
						bind: body.bind?.trim() || this.runtime.config.server.bind || "127.0.0.1",
						...(body.port !== undefined && body.port > 0 ? { port: body.port } : {}),
					},
					threads: {
						page_size: body.page_size && body.page_size > 0 ? body.page_size : 50,
					},
					...(body.seeds && body.seeds.length > 0 ? { discovery: { seeds: body.seeds.map(normalizeServiceUrl) } } : {}),
					acquisition: {
						policy: body.policy ?? "segment",
						allow: (body.allow ?? ["127.0.0.0/8", "::1/128"]).map(s => s.trim()).filter(Boolean),
					},
					...(body.judge_mode ? {
						judge: {
							mode: body.judge_mode,
							...(body.judge_model ? { model: body.judge_model } : {}),
							...(body.judge_base_url ? { base_url: body.judge_base_url } : {}),
						},
					} : {}),
					...(this.runtime.config.trust ? { trust: this.runtime.config.trust } : {}),
				};
				await this.reconfigure(next);
				return Response.json({ ok: true, snapshot: await this.runtime.snapshot() });
			}

			if (request.method === "POST" && url.pathname === "/api/actions/send") {
				const body = await readJsonBody<{
					delivery_kind?: string;
					msg: string;
					to?: string;
					thread?: { owner: string; id: string } | string;
					in_reply_to?: { sender: string; id: string };
					relation?: "reply" | "followup";
					response?: "await" | "async" | "none";
					execution?: "existing" | "new";
				}>(request);
				const meta: Record<string, Json> = {
					...(body.to ? { to: body.to } : {}),
					...(body.thread ? { thread: body.thread as unknown as Json } : {}),
					...(body.in_reply_to ? { in_reply_to: body.in_reply_to as unknown as Json } : {}),
					...(body.relation ? { relation: body.relation } : {}),
					response: body.response ?? "async",
					execution: body.execution ?? "existing",
				};
				const res = await this.runtime.executeMsgTool(
					{ agent_id: this.runtime.mainAgentId, main: true, read_only: false },
					{ delivery_kind: body.delivery_kind ?? "note", msg: body.msg, meta },
				);
				return Response.json(res);
			}

			if (request.method === "POST" && url.pathname === "/api/actions/domain") {
				const body = await readJsonBody<{ op: "join" | "leave"; domain_id: string }>(request);
				const res = await this.runtime.executeMsgTool(
					{ agent_id: this.runtime.mainAgentId, main: true, read_only: false },
					{
						delivery_kind: "domain",
						msg: `${body.op} ${body.domain_id}`,
						meta: { op: body.op, domain_id: body.domain_id },
					},
				);
				return Response.json(res);
			}

			if (request.method === "POST" && url.pathname === "/api/actions/thread/create") {
				const body = await readJsonBody<{ title: string; members?: string[] }>(request);
				const res = await this.runtime.executeMsgTool(
					{ agent_id: this.runtime.mainAgentId, main: true, read_only: false },
					{
						delivery_kind: "thread",
						msg: body.title,
						meta: { op: "create", title: body.title },
					},
				);
				return Response.json(res);
			}

			if (request.method === "POST" && url.pathname === "/api/actions/todo") {
				const body = await readJsonBody<{ id: string; status: "pending" | "in_progress" | "completed" | "blocked" }>(request);
				const row = await this.runtime.delivery.updateTodoStatus(body.id, body.status);
				return Response.json({ ok: row !== undefined, row });
			}

			if (request.method === "POST" && url.pathname === "/api/actions/shutdown") {
				queueMicrotask(() => {
					void this.close();
				});
				return Response.json({ ok: true });
			}

			return Response.json({ error: `Unknown route: ${request.method} ${url.pathname}` }, { status: 404 });
		} catch (error) {
			const formatted = serializeError(error);
			return Response.json(formatted.body, { status: formatted.status });
		}
	}

	async resolveCallerForConversation(conversationId?: string): Promise<CallerContext> {
		const cid = conversationId?.trim();
		if (!cid) {
			return { agent_id: this.runtime.mainAgentId, main: true, read_only: false };
		}
		let isSubagent = false;
		const existingConvs = await this.runtime.delivery.listConversations();
		const known = existingConvs.find(c => c.conversation_id === cid);
		if (known !== undefined) {
			isSubagent = known.is_subagent;
		} else if (this.runtime.bridge !== undefined) {
			const meta = await this.runtime.bridge.getConversationMetadata(cid).catch(() => undefined);
			if (meta !== undefined) isSubagent = meta.isSubagent;
		}
		if (!isSubagent) {
			return { agent_id: this.runtime.mainAgentId, main: true, read_only: false };
		}
		return { agent_id: cid, main: false, read_only: true };
	}

	setCustomJudge(judge?: JudgeFn): void {
		this.runtimeOptions = { ...this.runtimeOptions, customJudge: judge };
	}

	async close(): Promise<void> {
		if (this.closed) return;
		this.closed = true;
		for (const ctrl of this.sseControllers) {
			try {
				ctrl.close();
			} catch {
				// Ignore.
			}
		}
		this.sseControllers.clear();
		this.unsubscribeRuntime();
		this.server.stop(true);
		await this.runtime.close();
		if (!this.runtimeOptions.inMemory && existsSync(this.infoPath)) {
			try {
				const existing = JSON.parse(await readFile(this.infoPath, "utf8")) as Partial<DaemonInfo>;
				if (existing.pid === process.pid) {
					await rm(this.infoPath, { force: true });
				}
			} catch {
				// Ignore.
			}
		}
	}
}
