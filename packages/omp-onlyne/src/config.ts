import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import type { OnlyneConfig } from "./types.ts";
import type { Locale } from "@onlyne/node-host/i18n";
import { hostText as t } from "./host-text.ts";

const CONFIG_RELATIVE_PATH = join(".onlyne", "config.toml");
const REGION_PREFIX = "urn:onlyne:region:";
const AGENT_PREFIX = "urn:onlyne:agent:";
/** RFC 0011 §5.1: the one service path every HTTP binding answers on. */
const SERVICE_PATH = "/onlyne/v3";
const DEFAULT_BIND = "127.0.0.1";
const DEFAULT_PAGE_SIZE = 50;

/** The scheme of an absolute URI, per RFC 3986: ALPHA *( ALPHA / DIGIT / "+" / "-" / "." ) ":". */
const ABSOLUTE_URI = /^[A-Za-z][A-Za-z0-9+.-]*:/;

export class ConfigError extends Error {
	readonly code: "missing" | "invalid";

	constructor(code: "missing" | "invalid", message: string) {
		super(message);
		this.name = "ConfigError";
		this.code = code;
	}
}

/** `<cwd>/.onlyne/config.toml`; configuration is a project fact. */
export function configPath(cwd: string): string {
	return join(cwd, CONFIG_RELATIVE_PATH);
}

/**
 * A region is a stable absolute URI. A short name is accepted from the user and
 * saved as `urn:onlyne:region:<URI-encoded name>`; an absolute URI is kept as is.
 */
export function normalizeRegion(value: string): string {
	const trimmed = value.trim();
	if (trimmed.length === 0) throw new ConfigError("invalid", t("regionEmpty"));
	return ABSOLUTE_URI.test(trimmed) && !/\s/.test(trimmed) ? trimmed : `${REGION_PREFIX}${encodeURIComponent(trimmed)}`;
}

/**
 * A node id is a stable absolute URI. A short name is accepted from the user and
 * saved as `urn:onlyne:agent:<URI-encoded name>`; an absolute URI is kept as is.
 */
export function normalizeNodeId(value: string): string {
	const trimmed = value.trim();
	if (trimmed.length === 0) throw new ConfigError("invalid", t("nodeIdEmpty"));
	return ABSOLUTE_URI.test(trimmed) && !/\s/.test(trimmed) ? trimmed : `${AGENT_PREFIX}${encodeURIComponent(trimmed)}`;
}

/** The short name of an agent URN, for display in a form; a value without that prefix is returned unchanged. */
export function shortNodeId(value: string): string {
	if (!value.startsWith(AGENT_PREFIX)) return value;
	const name = value.slice(AGENT_PREFIX.length);
	try {
		return decodeURIComponent(name);
	} catch {
		return name;
	}
}

/**
 * A node address as the rest of the plugin uses it: the absolute service URL. An origin
 * (`http://host:7801`), a service URL, and a bare `host:port` all name the same node, and
 * only the service URL is stored or contacted, so one address has one spelling.
 */
export function normalizeServiceUrl(value: string): string {
	const raw = value.trim();
	if (raw.length === 0) throw new ConfigError("invalid", t("addressEmpty"));
	// A bare `host:port` reads as a scheme to `URL`; it is the common hand-typed form.
	const candidate = /^[A-Za-z][A-Za-z0-9+.-]*:\/\//.test(raw) ? raw : `http://${raw}`;
	let url: URL;
	try {
		url = new URL(candidate);
	} catch {
		throw new ConfigError("invalid", t("addressUrl", { value: JSON.stringify(raw) }));
	}
	if (url.protocol !== "http:" && url.protocol !== "https:") {
		throw new ConfigError("invalid", t("addressScheme", { value: JSON.stringify(raw) }));
	}
	const path = url.pathname.replace(/\/+$/, "");
	if (path.length === 0 || path === "/") return `${url.origin}${SERVICE_PATH}`;
	if (path === SERVICE_PATH) return `${url.origin}${SERVICE_PATH}`;
	throw new ConfigError(
		"invalid",
		t("addressShape", { service: SERVICE_PATH, value: JSON.stringify(raw) }),
	);
}

/** Read and validate the project configuration. A missing file is an error, not defaults. */
export async function loadConfig(cwd: string): Promise<OnlyneConfig> {
	const path = configPath(cwd);
	let text: string;
	try {
		text = await readFile(path, "utf8");
	} catch (error) {
		if (typeof error === "object" && error !== null && "code" in error && error.code === "ENOENT") {
			throw new ConfigError("missing", t("configMissing", { path }));
		}
		throw error;
	}
	return parseConfig(text, path);
}

/**
 * Validate, then write the project configuration atomically; the file is either
 * the old or the new contents. Returns the configuration as written.
 */
export async function saveConfig(config: OnlyneConfig): Promise<OnlyneConfig> {
	const next = validateConfig(config);
	const text = serializeConfig(next);
	await mkdir(dirname(next.path), { recursive: true });
	const temporary = `${next.path}.${process.pid}.${Date.now()}.tmp`;
	await writeFile(temporary, text, "utf8");
	await rename(temporary, next.path);
	return next;
}

/** Parse and validate the project configuration. Exported for tests. */
export function parseConfig(text: string, path: string): OnlyneConfig {
	let raw: Record<string, unknown>;
	try {
		raw = Bun.TOML.parse(text) as Record<string, unknown>;
	} catch (error) {
		throw new ConfigError("invalid", t("configParse", { path, error: error instanceof Error ? error.message : String(error) }));
	}
	const node = table(raw, "node", path);
	const server = table(raw, "server", path);
	const threads = table(raw, "threads", path);
	const discovery = raw.discovery === undefined ? undefined : table(raw, "discovery", path);
	rejectRetired(raw, node, server, threads);
	const id = optionalString(node, "id", "node.id");
	const region = optionalString(node, "region", "node.region");
	const keyRef = optionalString(node, "key_ref", "node.key_ref");
	const keyScope = optionalString(node, "key_scope", "node.key_scope");
	const publicUrl = optionalString(server, "public_url", "server.public_url");
	const seeds = discovery === undefined ? undefined : stringArray(discovery, "seeds", "discovery.seeds");
	const trust = trustSeeds(raw, path);
	const bind = optionalString(server, "bind", "server.bind");
	const pageSize = optionalInteger(threads, "page_size", "threads.page_size");
	return validateConfig({
		path,
		...(raw.language === undefined ? {} : { language: raw.language as Locale }),
		region: region ?? "",
		node: { id: id ?? "", ...(keyRef === undefined ? {} : { key_ref: keyRef }), ...(keyScope === undefined ? {} : { key_scope: keyScope as "project" | "global" | "none" }) },
		server: { bind: bind ?? DEFAULT_BIND, ...(publicUrl === undefined ? {} : { public_url: publicUrl }) },
		threads: { page_size: pageSize ?? DEFAULT_PAGE_SIZE },
		...(seeds === undefined && trust === undefined ? {} : { discovery: { ...(seeds === undefined ? {} : { seeds }), ...(trust === undefined ? {} : { trust }) } }),
		...(trust === undefined ? {} : { trust }),
	});
}

/** Serialize a validated configuration in a stable section order. */
export function serializeConfig(config: OnlyneConfig): string {
	const lines: string[] = [
		"# Onlyne project configuration. Credentials stay behind key_ref references.",
		...(config.language === undefined ? [] : [`language = ${tomlString(config.language)}`, ""]),
		"[node]",
		`id = ${tomlString(config.node.id)}`,
		`region = ${tomlString(config.region)}`,
	];
	if (config.node.key_ref !== undefined) lines.push(`key_ref = ${tomlString(config.node.key_ref)}`);
	if (config.node.key_scope !== undefined) lines.push(`key_scope = ${tomlString(config.node.key_scope)}`);
	lines.push(
		"",
		"[server]",
		`bind = ${tomlString(config.server.bind)}`,
		...(config.server.public_url === undefined ? [] : [`public_url = ${tomlString(config.server.public_url)}`]),
		"",
		"[threads]",
		`page_size = ${String(config.threads.page_size)}`,
	);
	if (config.discovery?.seeds !== undefined || config.trust !== undefined) {
		lines.push("", "[discovery]");
		const seeds = config.discovery?.seeds ?? [];
		if (seeds.length > 0) lines.push(`seeds = [${seeds.map(seed => tomlString(seed)).join(", ")}]`);
		for (const entry of config.trust ?? []) {
			lines.push("", "[[trust]]", `node_id = ${tomlString(entry.node_id)}`, `fingerprint = ${tomlString(entry.fingerprint)}`);
		}
	}
	return `${lines.join("\n")}\n`;
}

function validateConfig(config: OnlyneConfig): OnlyneConfig {
	if (config.language !== undefined && config.language !== "en" && config.language !== "zh-CN") {
		throw new ConfigError("invalid", t("languageInvalid"));
	}
	const region = normalizeRegion(config.region);
	const id = normalizeNodeId(config.node.id);
	const keyRef = config.node.key_ref?.trim();
	const bind = config.server.bind.trim();
	const publicUrl = config.server.public_url?.trim();
	const keyScope = config.node.key_scope;
	if (keyScope !== undefined && keyScope !== "project" && keyScope !== "global" && keyScope !== "none") {
		throw new ConfigError("invalid", t("keyScopeInvalid", { value: JSON.stringify(keyScope) }));
	}
	if (publicUrl !== undefined && publicUrl.length === 0) throw new ConfigError("invalid", t("publicUrlEmpty"));
	// A seed is contacted at every start, so it is stored in the one spelling the node uses.
	const seeds = [...new Set((config.discovery?.seeds ?? []).map(seed => seed.trim()).filter(seed => seed.length > 0).map(normalizeServiceUrl))];
	if (bind.length === 0) throw new ConfigError("invalid", t("bindEmpty"));
	if (/[\s/]/.test(bind)) {
		throw new ConfigError("invalid", t("bindShape", { value: JSON.stringify(bind) }));
	}
	const pageSize = config.threads.page_size;
	if (!Number.isInteger(pageSize) || pageSize < 1 || pageSize > 100) {
		throw new ConfigError("invalid", t("pageSizeRange", { value: String(pageSize) }));
	}
	return {
		path: config.path,
		...(config.language === undefined ? {} : { language: config.language }),
		region,
		node: { id, ...(keyRef === undefined || keyRef.length === 0 ? {} : { key_ref: keyRef }), ...(keyScope === undefined ? {} : { key_scope: keyScope }) },
		server: { bind, ...(publicUrl === undefined ? {} : { public_url: publicUrl }) },
		threads: { page_size: pageSize },
		...(seeds.length === 0 ? {} : { discovery: { seeds } }),
		...(config.trust === undefined ? {} : { trust: config.trust }),
	};
}

function rejectRetired(
	raw: Record<string, unknown>,
	node: Record<string, unknown>,
	server: Record<string, unknown>,
	threads: Record<string, unknown>,
): void {
	rejectKey(node, "endpoint", t("retiredEndpoint"));
	rejectKey(server, "port", t("retiredPort"));
	rejectKey(raw, "peers", t("retiredPeers"));
	rejectKey(raw, "omp", t("retiredOmp"));
	rejectKey(threads, "default_attention", t("retiredAttention"));
}

function rejectKey(source: Record<string, unknown>, key: string, message: string): void {
	if (source[key] !== undefined) throw new ConfigError("invalid", message);
}

function table(root: Record<string, unknown>, key: string, path: string): Record<string, unknown> {
	const value = root[key];
	if (value === undefined) return {};
	if (!isTable(value)) throw new ConfigError("invalid", t("tableExpected", { path, key }));
	return value;
}

function optionalString(source: Record<string, unknown>, key: string, field: string): string | undefined {
	const value = source[key];
	if (value === undefined) return undefined;
	if (typeof value !== "string") throw new ConfigError("invalid", t("stringExpected", { field }));
	return value;
}

function optionalInteger(source: Record<string, unknown>, key: string, field: string): number | undefined {
	const value = source[key];
	if (value === undefined) return undefined;
	if (typeof value !== "number" || !Number.isInteger(value)) {
		throw new ConfigError("invalid", t("integerExpected", { field }));
	}
	return value;
}

/** An array of non-empty strings under `source.name`. */
function stringArray(source: Record<string, unknown>, key: string, field: string): string[] | undefined {
	const value = source[key];
	if (value === undefined) return undefined;
	if (!Array.isArray(value) || !value.every(entry => typeof entry === "string" && entry.length > 0)) {
		throw new ConfigError("invalid", t("stringArrayExpected", { field }));
	}
	return value;
}

/** `[[trust]]` rows: a node id and a 16-hex-character fingerprint each. */
function trustSeeds(raw: Record<string, unknown>, path: string): Array<{ node_id: string; fingerprint: string }> | undefined {
	const value = raw.trust;
	if (value === undefined) return undefined;
	if (!Array.isArray(value)) throw new ConfigError("invalid", t("trustArray", { path }));
	const entries: Array<{ node_id: string; fingerprint: string }> = [];
	for (const row of value) {
		if (row === null || typeof row !== "object" || Array.isArray(row)) {
			throw new ConfigError("invalid", t("trustArray", { path }));
		}
		const record = row as Record<string, unknown>;
		const node_id = typeof record.node_id === "string" ? record.node_id.trim() : "";
		const fingerprint = typeof record.fingerprint === "string" ? record.fingerprint.trim() : "";
		if (node_id.length === 0 || !/^[0-9a-f]{16}$/.test(fingerprint)) {
			throw new ConfigError("invalid", t("trustEntry", { path }));
		}
		entries.push({ node_id, fingerprint });
	}
	return entries;
}

function isTable(value: unknown): value is Record<string, unknown> {
	return value !== null && typeof value === "object" && !Array.isArray(value);
}

function tomlString(value: string): string {
	return `"${value.replace(/\\/g, "\\\\").replace(/"/g, '\\"').replace(/\n/g, "\\n").replace(/\r/g, "\\r").replace(/\t/g, "\\t")}"`;
}

