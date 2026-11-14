import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import type { TrustSeed } from "@onlyne/lib";
import { DEFAULT_ACQUISITION, parseAllowBlock, type AcquirePolicy, type AcquisitionPolicy } from "@onlyne/node-host/acquire";
import type { Locale } from "@onlyne/node-host/i18n";
import type { OnlyneConfig } from "@onlyne/node-host/types";

export const CONFIG_DIR = ".onlyne";
export const CONFIG_FILE = "config.toml";
export const DB_FILE = "agy.db";
export const DAEMON_FILE = "agy-daemon.json";

export const DEFAULT_BIND = "127.0.0.1";
export const DEFAULT_PAGE_SIZE = 50;
export const DEFAULT_REGION_PREFIX = "urn:onlyne:region:";
export const DEFAULT_NODE_PREFIX = "urn:onlyne:agent:";

export type JudgeMode = "auto" | "sampling" | "openai" | "gemini" | "agy-cli" | "none";
export type WakeMode = "auto" | "agentapi" | "hook";

export interface JudgeConfig {
	mode?: JudgeMode;
	model?: string;
	base_url?: string;
	api_key_env?: string;
	api_key?: string;
	timeout_ms?: number;
}

export interface WakeConfig {
	mode?: WakeMode;
	escalate_ms?: number;
}

export interface AgyOnlyneConfig extends OnlyneConfig {
	judge?: JudgeConfig;
	wake?: WakeConfig;
}

export function configDir(cwd: string): string {
	return join(cwd, CONFIG_DIR);
}

export function configPath(cwd: string): string {
	return join(configDir(cwd), CONFIG_FILE);
}

export function dbPath(cwd: string): string {
	return join(configDir(cwd), DB_FILE);
}

export function daemonFilePath(cwd: string): string {
	return join(configDir(cwd), DAEMON_FILE);
}

export function normalizeRegion(raw: string): string {
	const trimmed = raw.trim();
	if (trimmed.length === 0) throw new Error("region is required");
	if (trimmed.startsWith("urn:")) return trimmed;
	return `${DEFAULT_REGION_PREFIX}${encodeURIComponent(trimmed)}`;
}

export function shortRegion(region: string): string {
	if (!region.startsWith(DEFAULT_REGION_PREFIX)) return region;
	const tail = region.slice(DEFAULT_REGION_PREFIX.length);
	try {
		return decodeURIComponent(tail);
	} catch {
		return tail;
	}
}

export function normalizeNodeId(raw: string): string {
	const trimmed = raw.trim();
	if (trimmed.length === 0) throw new Error("node.id is required");
	if (trimmed.startsWith("urn:")) return trimmed;
	return `${DEFAULT_NODE_PREFIX}${encodeURIComponent(trimmed)}`;
}

export function shortNodeId(nodeId: string): string {
	if (!nodeId.startsWith(DEFAULT_NODE_PREFIX)) {
		const idx = nodeId.lastIndexOf(":");
		return idx >= 0 && idx < nodeId.length - 1 ? nodeId.slice(idx + 1) : nodeId;
	}
	const tail = nodeId.slice(DEFAULT_NODE_PREFIX.length);
	try {
		return decodeURIComponent(tail);
	} catch {
		return tail;
	}
}

export function normalizeServiceUrl(raw: string): string {
	const trimmed = raw.trim();
	if (trimmed.length === 0) throw new Error("service URL is empty");
	const withScheme = /^[a-zA-Z][a-zA-Z0-9+.-]*:\/\//.test(trimmed) ? trimmed : `http://${trimmed}`;
	const parsed = new URL(withScheme);
	const cleaned = parsed.pathname.replace(/\/+$/, "");
	if (cleaned === "" || cleaned === "/") parsed.pathname = "/onlyne/v3";
	else if (!cleaned.endsWith("/onlyne/v3")) parsed.pathname = `${cleaned}/onlyne/v3`;
	else parsed.pathname = cleaned;
	return parsed.toString().replace(/\/+$/, "");
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
	return value !== null && typeof value === "object" && !Array.isArray(value)
		? value as Record<string, unknown>
		: undefined;
}

function asString(value: unknown): string | undefined {
	return typeof value === "string" && value.trim().length > 0 ? value.trim() : undefined;
}

function asPort(value: unknown): number | undefined {
	if (value === undefined) return undefined;
	if (typeof value !== "number" || !Number.isInteger(value) || value < 0 || value > 65535) {
		throw new Error(`Invalid server.port: ${String(value)}`);
	}
	return value;
}

function parseAcquisition(raw: unknown): AcquisitionPolicy | undefined {
	const rec = asRecord(raw);
	if (rec === undefined) return undefined;
	const policy = asString(rec.policy);
	if (policy !== "all" && policy !== "segment" && policy !== "none") {
		throw new Error(`Invalid acquisition.policy: ${String(rec.policy)}`);
	}
	const allow: string[] = [];
	if (Array.isArray(rec.allow)) {
		for (const item of rec.allow) {
			if (typeof item !== "string") continue;
			const trimmed = item.trim();
			if (trimmed.length === 0) continue;
			if (parseAllowBlock(trimmed) === undefined) {
				throw new Error(`Invalid acquisition.allow entry: ${trimmed}`);
			}
			allow.push(trimmed);
		}
	}
	return {
		policy,
		allow: allow.length > 0 || policy !== "segment" ? allow : [...DEFAULT_ACQUISITION.allow],
	};
}

function parseTrust(raw: unknown): TrustSeed[] | undefined {
	if (!Array.isArray(raw)) return undefined;
	const out: TrustSeed[] = [];
	for (const entry of raw) {
		const rec = asRecord(entry);
		if (rec === undefined) continue;
		const node_id = asString(rec.node_id);
		const fingerprint = asString(rec.fingerprint);
		const url = asString(rec.url);
		if (node_id === undefined || fingerprint === undefined) continue;
		out.push({
			node_id: normalizeNodeId(node_id),
			fingerprint,
			...(url === undefined ? {} : { url }),
		});
	}
	return out.length > 0 ? out : undefined;
}

function parseJudge(raw: unknown): JudgeConfig | undefined {
	const rec = asRecord(raw);
	if (rec === undefined) return undefined;
	const mode = asString(rec.mode);
	if (mode !== undefined && !["auto", "sampling", "openai", "gemini", "agy-cli", "none"].includes(mode)) {
		throw new Error(`Invalid judge.mode: ${mode}`);
	}
	const timeout = typeof rec.timeout_ms === "number" && Number.isFinite(rec.timeout_ms) && rec.timeout_ms > 0
		? Math.floor(rec.timeout_ms)
		: undefined;
	return {
		...(mode === undefined ? {} : { mode: mode as JudgeMode }),
		...(asString(rec.model) === undefined ? {} : { model: asString(rec.model) }),
		...(asString(rec.base_url) === undefined ? {} : { base_url: asString(rec.base_url) }),
		...(asString(rec.api_key_env) === undefined ? {} : { api_key_env: asString(rec.api_key_env) }),
		...(asString(rec.api_key) === undefined ? {} : { api_key: asString(rec.api_key) }),
		...(timeout === undefined ? {} : { timeout_ms: timeout }),
	};
}

function parseWake(raw: unknown): WakeConfig | undefined {
	const rec = asRecord(raw);
	if (rec === undefined) return undefined;
	const mode = asString(rec.mode);
	if (mode !== undefined && !["auto", "agentapi", "hook"].includes(mode)) {
		throw new Error(`Invalid wake.mode: ${mode}`);
	}
	const escalate = typeof rec.escalate_ms === "number" && Number.isFinite(rec.escalate_ms) && rec.escalate_ms > 0
		? Math.floor(rec.escalate_ms)
		: undefined;
	return {
		...(mode === undefined ? {} : { mode: mode as WakeMode }),
		...(escalate === undefined ? {} : { escalate_ms: escalate }),
	};
}

export function parseConfig(text: string, path: string): AgyOnlyneConfig {
	const parsed = asRecord(Bun.TOML.parse(text));
	if (parsed === undefined) throw new Error(`Invalid TOML config at ${path}`);

	const node = asRecord(parsed.node) ?? {};
	const server = asRecord(parsed.server) ?? {};
	const threads = asRecord(parsed.threads) ?? {};
	const discovery = asRecord(parsed.discovery);

	const rawRegion = asString(node.region) ?? asString(parsed.region);
	if (rawRegion === undefined) throw new Error(`Missing region in ${path}`);
	const rawId = asString(node.id);
	if (rawId === undefined) throw new Error(`Missing node.id in ${path}`);

	const rawScope = asString(node.key_scope);
	const key_scope = rawScope === "project" || rawScope === "global" || rawScope === "none" ? rawScope : "project";
	const rawLang = asString(parsed.language);
	const language: Locale | undefined = rawLang === "zh-CN" || rawLang === "en" ? rawLang : undefined;

	const rawPage = threads.page_size;
	const page_size = typeof rawPage === "number" && Number.isInteger(rawPage) && rawPage > 0 ? rawPage : DEFAULT_PAGE_SIZE;

	const seeds = Array.isArray(discovery?.seeds)
		? discovery.seeds.filter((item): item is string => typeof item === "string" && item.trim().length > 0).map(normalizeServiceUrl)
		: undefined;
	const acquisition = parseAcquisition(parsed.acquisition);
	const trust = parseTrust(parsed.trust);
	const judge = parseJudge(parsed.judge);
	const wake = parseWake(parsed.wake);
	const port = asPort(server.port);
	const public_url = asString(server.public_url);
	const key_ref = asString(node.key_ref);

	const config: AgyOnlyneConfig = {
		path,
		...(language === undefined ? {} : { language }),
		region: normalizeRegion(rawRegion),
		node: {
			id: normalizeNodeId(rawId),
			key_scope,
			...(key_ref === undefined ? {} : { key_ref }),
		},
		server: {
			bind: asString(server.bind) ?? DEFAULT_BIND,
			...(port === undefined ? {} : { port }),
			...(public_url === undefined ? {} : { public_url }),
		},
		threads: { page_size },
		...(seeds !== undefined && seeds.length > 0 ? { discovery: { seeds } } : {}),
		...(trust !== undefined ? { trust } : {}),
		...(acquisition !== undefined ? { acquisition } : {}),
		...(judge !== undefined ? { judge } : {}),
		...(wake !== undefined ? { wake } : {}),
	};
	return validateConfig(config);
}

export function validateConfig(config: AgyOnlyneConfig): AgyOnlyneConfig {
	if (!config.region || config.region.trim().length === 0) throw new Error("region is required");
	if (!config.node?.id || config.node.id.trim().length === 0) throw new Error("node.id is required");
	if (!config.server?.bind || config.server.bind.trim().length === 0) throw new Error("server.bind is required");
	if (config.server.port !== undefined && (!Number.isInteger(config.server.port) || config.server.port < 0 || config.server.port > 65535)) {
		throw new Error(`Invalid server.port: ${config.server.port}`);
	}
	if (!Number.isInteger(config.threads?.page_size) || config.threads.page_size < 1 || config.threads.page_size > 200) {
		throw new Error(`Invalid threads.page_size: ${config.threads?.page_size}`);
	}
	if (config.acquisition?.policy === "segment") {
		for (const entry of config.acquisition.allow) {
			if (parseAllowBlock(entry) === undefined) throw new Error(`Invalid acquisition.allow entry: ${entry}`);
		}
	}
	return config;
}

function tomlString(value: string): string {
	return JSON.stringify(value);
}

function tomlArray(values: readonly string[]): string {
	return `[${values.map(tomlString).join(", ")}]`;
}

export function serializeConfig(config: AgyOnlyneConfig): string {
	const validated = validateConfig(config);
	const lines: string[] = [
		"# Onlyne project configuration. Credentials stay behind key_ref references.",
	];
	if (validated.language !== undefined) {
		lines.push(`language = ${tomlString(validated.language)}`);
	}
	lines.push("", "[node]");
	lines.push(`id = ${tomlString(validated.node.id)}`);
	lines.push(`region = ${tomlString(validated.region)}`);
	if (validated.node.key_scope !== undefined) lines.push(`key_scope = ${tomlString(validated.node.key_scope)}`);
	if (validated.node.key_ref !== undefined) lines.push(`key_ref = ${tomlString(validated.node.key_ref)}`);

	lines.push("", "[server]");
	lines.push(`bind = ${tomlString(validated.server.bind)}`);
	if (validated.server.port !== undefined) lines.push(`port = ${validated.server.port}`);
	if (validated.server.public_url !== undefined) lines.push(`public_url = ${tomlString(validated.server.public_url)}`);

	lines.push("", "[threads]");
	lines.push(`page_size = ${validated.threads.page_size}`);

	const acquisition = validated.acquisition ?? DEFAULT_ACQUISITION;
	lines.push("", "[acquisition]");
	lines.push(`policy = ${tomlString(acquisition.policy)}`);
	lines.push(`allow = ${tomlArray(acquisition.allow)}`);

	if (validated.discovery?.seeds !== undefined && validated.discovery.seeds.length > 0) {
		lines.push("", "[discovery]");
		lines.push(`seeds = ${tomlArray(validated.discovery.seeds)}`);
	}

	if (validated.judge !== undefined && Object.keys(validated.judge).length > 0) {
		lines.push("", "[judge]");
		if (validated.judge.mode !== undefined) lines.push(`mode = ${tomlString(validated.judge.mode)}`);
		if (validated.judge.model !== undefined) lines.push(`model = ${tomlString(validated.judge.model)}`);
		if (validated.judge.base_url !== undefined) lines.push(`base_url = ${tomlString(validated.judge.base_url)}`);
		if (validated.judge.api_key_env !== undefined) lines.push(`api_key_env = ${tomlString(validated.judge.api_key_env)}`);
		if (validated.judge.api_key !== undefined) lines.push(`api_key = ${tomlString(validated.judge.api_key)}`);
		if (validated.judge.timeout_ms !== undefined) lines.push(`timeout_ms = ${validated.judge.timeout_ms}`);
	}

	if (validated.wake !== undefined && Object.keys(validated.wake).length > 0) {
		lines.push("", "[wake]");
		if (validated.wake.mode !== undefined) lines.push(`mode = ${tomlString(validated.wake.mode)}`);
		if (validated.wake.escalate_ms !== undefined) lines.push(`escalate_ms = ${validated.wake.escalate_ms}`);
	}

	if (validated.trust !== undefined) {
		for (const item of validated.trust) {
			lines.push("", "[[trust]]");
			lines.push(`node_id = ${tomlString(item.node_id)}`);
			lines.push(`fingerprint = ${tomlString(item.fingerprint)}`);
		}
	}

	lines.push("");
	return lines.join("\n");
}

export async function loadConfig(cwd: string): Promise<AgyOnlyneConfig | undefined> {
	const path = configPath(cwd);
	if (!existsSync(path)) return undefined;
	const text = await readFile(path, "utf8");
	return parseConfig(text, path);
}

export async function saveConfig(config: AgyOnlyneConfig): Promise<void> {
	const content = serializeConfig(config);
	const dir = dirname(config.path);
	await mkdir(dir, { recursive: true });
	const tmp = join(dir, `.config.${process.pid}.${Date.now()}.${crypto.randomUUID().slice(0, 8)}.tmp`);
	await writeFile(tmp, content, { mode: 0o600 });
	await rename(tmp, config.path);
}

export function isAcquisitionPolicy(value: unknown): value is AcquisitionPolicy {
	const rec = asRecord(value);
	if (rec === undefined) return false;
	const policy = rec.policy as AcquirePolicy;
	return (policy === "all" || policy === "segment" || policy === "none") && Array.isArray(rec.allow);
}
