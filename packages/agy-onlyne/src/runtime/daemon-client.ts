import { existsSync } from "node:fs";
import { mkdir, open, readFile, rm, stat } from "node:fs/promises";
import { join } from "node:path";
import type { AcquirePolicy } from "@onlyne/node-host/acquire";
import { configDir, daemonFilePath, type AgyOnlyneConfig } from "../config.ts";
import type { CallerContext } from "../core/decider.ts";
import type { MsgToolParams } from "../core/msg-service.ts";
import type { Snapshot } from "../core/types.ts";
import type { Caller, RenderedUrl } from "../core/urls.ts";
import type { DaemonInfo } from "./daemon.ts";
import type { HookEventInput, HookResultPayload } from "./delivery.ts";

export interface DaemonSpawnCommand {
	command: string[];
	cwd: string;
}

const LOCK_NAME = "agy-daemon.lock";
const LOCK_STALE_MS = 12_000;
const SPAWN_TIMEOUT_MS = 8_000;

function sleep(ms: number): Promise<void> {
	return new Promise(resolve => setTimeout(resolve, ms));
}

export async function readDaemonInfo(cwd: string): Promise<DaemonInfo | undefined> {
	const path = daemonFilePath(cwd);
	if (!existsSync(path)) return undefined;
	try {
		const raw = await readFile(path, "utf8");
		const parsed = JSON.parse(raw) as Partial<DaemonInfo>;
		if (typeof parsed.ctrlUrl === "string" && typeof parsed.token === "string") {
			return parsed as DaemonInfo;
		}
	} catch {
		return undefined;
	}
	return undefined;
}

export async function checkDaemonHealth(info: Pick<DaemonInfo, "ctrlUrl" | "token">): Promise<boolean> {
	try {
		const res = await fetch(`${info.ctrlUrl}/health`, {
			headers: { authorization: `Bearer ${info.token}` },
			signal: AbortSignal.timeout(1_500),
		});
		return res.ok;
	} catch {
		return false;
	}
}

export function defaultDaemonCommand(cliEntryPath: string, workspaceCwd: string): string[] {
	return [process.execPath, cliEntryPath, "daemon", "run", "--cwd", workspaceCwd];
}

export class DaemonClient {
	private constructor(
		readonly cwd: string,
		private info: DaemonInfo,
	) {}

	get ctrlUrl(): string {
		return this.info.ctrlUrl;
	}

	get token(): string {
		return this.info.token;
	}

	get guiUrl(): string {
		return `${this.info.ctrlUrl}/ui?token=${encodeURIComponent(this.info.token)}`;
	}

	static fromInfo(info: DaemonInfo): DaemonClient {
		return new DaemonClient(info.cwd, info);
	}

	static async connectOrSpawn(options: {
		cwd: string;
		spawnCommand?: string[];
		timeoutMs?: number;
	}): Promise<DaemonClient> {
		const existing = await readDaemonInfo(options.cwd);
		if (existing !== undefined && await checkDaemonHealth(existing)) {
			return new DaemonClient(options.cwd, existing);
		}
		if (!options.spawnCommand || options.spawnCommand.length === 0) {
			throw new Error(`Onlyne Antigravity daemon is not running in ${options.cwd}`);
		}

		const dir = configDir(options.cwd);
		await mkdir(dir, { recursive: true });
		const lockPath = join(dir, LOCK_NAME);
		const deadline = Date.now() + (options.timeoutMs ?? SPAWN_TIMEOUT_MS);

		let ownsLock = false;
		while (!ownsLock && Date.now() < deadline) {
			try {
				const handle = await open(lockPath, "wx");
				await handle.writeFile(String(process.pid));
				await handle.close();
				ownsLock = true;
			} catch {
				const info = await readDaemonInfo(options.cwd);
				if (info !== undefined && await checkDaemonHealth(info)) {
					return new DaemonClient(options.cwd, info);
				}
				const infoStat = await stat(lockPath).catch(() => undefined);
				if (infoStat !== undefined && Date.now() - infoStat.mtimeMs > LOCK_STALE_MS) {
					await rm(lockPath, { force: true }).catch(() => undefined);
				}
				await sleep(80);
			}
		}

		if (ownsLock) {
			try {
				const recheck = await readDaemonInfo(options.cwd);
				if (recheck !== undefined && await checkDaemonHealth(recheck)) {
					return new DaemonClient(options.cwd, recheck);
				}
				const proc = Bun.spawn(options.spawnCommand, {
					cwd: options.cwd,
					stdin: "ignore",
					stdout: "ignore",
					stderr: "ignore",
				});
				proc.unref();

				while (Date.now() < deadline) {
					const info = await readDaemonInfo(options.cwd);
					if (info !== undefined && await checkDaemonHealth(info)) {
						return new DaemonClient(options.cwd, info);
					}
					await sleep(80);
				}
			} finally {
				await rm(lockPath, { force: true }).catch(() => undefined);
			}
		}

		const finalInfo = await readDaemonInfo(options.cwd);
		if (finalInfo !== undefined && await checkDaemonHealth(finalInfo)) {
			return new DaemonClient(options.cwd, finalInfo);
		}
		throw new Error(`Timed out waiting for Onlyne daemon in ${options.cwd}`);
	}

	async request<T>(method: string, path: string, body?: unknown): Promise<T> {
		const res = await fetch(`${this.info.ctrlUrl}${path}`, {
			method,
			headers: {
				authorization: `Bearer ${this.info.token}`,
				...(body === undefined ? {} : { "content-type": "application/json" }),
			},
			...(body === undefined ? {} : { body: JSON.stringify(body) }),
		});
		const text = await res.text();
		const parsed = text.trim().length > 0 ? JSON.parse(text) as Record<string, unknown> : {};
		if (!res.ok) {
			const msg = typeof parsed.error === "string" ? parsed.error : `Daemon HTTP ${res.status}`;
			const err = new Error(msg) as Error & { code?: string; details?: unknown };
			if (typeof parsed.code === "string") err.code = parsed.code;
			if (parsed.details !== undefined) err.details = parsed.details;
			throw err;
		}
		return parsed as T;
	}

	snapshot(): Promise<Snapshot> {
		return this.request<Snapshot>("GET", "/api/snapshot");
	}

	msg(
		params: MsgToolParams,
		caller?: CallerContext,
		samplingCallback?: { url: string; token: string },
		conversationId?: string,
	): Promise<{
		text: string;
		details: Record<string, unknown>;
	}> {
		return this.request("POST", "/api/msg", { params, caller, samplingCallback, conversationId });
	}

	read(url: string, caller?: Caller, conversationId?: string): Promise<RenderedUrl> {
		return this.request("POST", "/api/read", { url, caller, conversationId });
	}

	listUris(): Promise<{ uris: string[] }> {
		return this.request("GET", "/api/uris");
	}

	processHook(input: HookEventInput): Promise<HookResultPayload> {
		return this.request("POST", "/api/hooks", input);
	}

	connect(url: string): Promise<{ ok: boolean; record: unknown }> {
		return this.request("POST", "/api/actions/connect", { url });
	}

	refresh(): Promise<{ ok: boolean }> {
		return this.request("POST", "/api/actions/refresh");
	}

	trustBegin(nodeId: string): Promise<{ node_id: string; fingerprint: string; base_url: string }> {
		return this.request("POST", "/api/actions/trust/begin", { node_id: nodeId });
	}

	trustConfirm(nodeId: string, fingerprint: string): Promise<{ ok: boolean; entry: unknown }> {
		return this.request("POST", "/api/actions/trust/confirm", { node_id: nodeId, fingerprint });
	}

	trustAccept(nodeId: string): Promise<{ ok: boolean; entry: unknown }> {
		return this.request("POST", "/api/actions/trust/accept", { node_id: nodeId });
	}

	trustRevoke(nodeId: string): Promise<{ ok: boolean; entry: unknown }> {
		return this.request("POST", "/api/actions/trust/revoke", { node_id: nodeId });
	}

	trustRotate(): Promise<{ ok: boolean; fingerprint: string }> {
		return this.request("POST", "/api/actions/trust/rotate");
	}

	setAcquisition(policy: AcquirePolicy, allow: string[]): Promise<{ ok: boolean }> {
		return this.request("POST", "/api/actions/acquisition", { policy, allow });
	}

	setLanguage(language: "en" | "zh-CN"): Promise<{ ok: boolean; language: string }> {
		return this.request("POST", "/api/actions/language", { language });
	}

	saveConfig(payload: {
		region: string;
		node_id: string;
		bind?: string;
		port?: number;
		key_scope?: "project" | "global" | "none";
		page_size?: number;
		seeds?: string[];
		policy?: AcquirePolicy;
		allow?: string[];
		judge_mode?: NonNullable<AgyOnlyneConfig["judge"]>["mode"];
		judge_model?: string;
		judge_base_url?: string;
		language?: "en" | "zh-CN";
	}): Promise<{ ok: boolean; snapshot: Snapshot }> {
		return this.request("POST", "/api/actions/config", payload);
	}

	send(payload: {
		delivery_kind?: string;
		msg: string;
		to?: string;
		thread?: { owner: string; id: string } | string;
		in_reply_to?: { sender: string; id: string };
		relation?: "reply" | "followup";
		response?: "await" | "async" | "none";
		execution?: "existing" | "new";
	}): Promise<{ text: string; details: Record<string, unknown> }> {
		return this.request("POST", "/api/actions/send", payload);
	}

	domainAction(op: "join" | "leave", domainId: string): Promise<unknown> {
		return this.request("POST", "/api/actions/domain", { op, domain_id: domainId });
	}

	createThread(title: string): Promise<unknown> {
		return this.request("POST", "/api/actions/thread/create", { title });
	}

	updateTodo(id: string, status: "pending" | "in_progress" | "completed" | "blocked"): Promise<unknown> {
		return this.request("POST", "/api/actions/todo", { id, status });
	}

	shutdown(): Promise<{ ok: boolean }> {
		return this.request("POST", "/api/actions/shutdown");
	}

	subscribe(onEvent: (event: string) => void, signal?: AbortSignal): void {
		const loop = async (): Promise<void> => {
			while (!signal?.aborted) {
				try {
					const res = await fetch(`${this.info.ctrlUrl}/events`, {
						headers: { authorization: `Bearer ${this.info.token}` },
						signal,
					});
					if (!res.ok || !res.body) {
						await sleep(1_000);
						continue;
					}
					const reader = res.body.getReader();
					const decoder = new TextDecoder();
					let buf = "";
					while (!signal?.aborted) {
						const chunk = await reader.read();
						if (chunk.done) break;
						buf += decoder.decode(chunk.value, { stream: true });
						const parts = buf.split("\n\n");
						buf = parts.pop() ?? "";
						for (const part of parts) {
							const evLine = part.split("\n").find(l => l.startsWith("event:"));
							const name = evLine ? evLine.slice("event:".length).trim() : "message";
							onEvent(name);
						}
					}
				} catch {
					if (signal?.aborted) return;
					await sleep(1_000);
				}
			}
		};
		void loop();
	}
}
