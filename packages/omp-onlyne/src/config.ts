import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import type { OnlyneConfig } from "./types.ts";

const CONFIG_RELATIVE_PATH = join(".onlyne", "config.toml");
const REGION_PREFIX = "urn:onlyne:region:";
const AGENT_PREFIX = "urn:onlyne:agent:";
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
	if (trimmed.length === 0) throw new ConfigError("invalid", "region must not be empty.");
	return ABSOLUTE_URI.test(trimmed) && !/\s/.test(trimmed) ? trimmed : `${REGION_PREFIX}${encodeURIComponent(trimmed)}`;
}

/**
 * A node id is a stable absolute URI. A short name is accepted from the user and
 * saved as `urn:onlyne:agent:<URI-encoded name>`; an absolute URI is kept as is.
 */
export function normalizeNodeId(value: string): string {
	const trimmed = value.trim();
	if (trimmed.length === 0) throw new ConfigError("invalid", "node.id must not be empty.");
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

/** Read and validate the project configuration. A missing file is an error, not defaults. */
export async function loadConfig(cwd: string): Promise<OnlyneConfig> {
	const path = configPath(cwd);
	let text: string;
	try {
		text = await readFile(path, "utf8");
	} catch (error) {
		if (typeof error === "object" && error !== null && "code" in error && error.code === "ENOENT") {
			throw new ConfigError("missing", `No Onlyne configuration at ${path}. Run /onlyne init to create it.`);
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
		throw new ConfigError("invalid", `${path}: ${error instanceof Error ? error.message : String(error)}`);
	}
	const node = table(raw, "node", path);
	const server = table(raw, "server", path);
	const threads = table(raw, "threads", path);
	rejectRetired(raw, node, server, threads);
	const id = optionalString(node, "id", "node.id");
	const region = optionalString(node, "region", "node.region");
	const keyRef = optionalString(node, "key_ref", "node.key_ref");
	const bind = optionalString(server, "bind", "server.bind");
	const pageSize = optionalInteger(threads, "page_size", "threads.page_size");
	return validateConfig({
		path,
		region: region ?? "",
		node: { id: id ?? "", ...(keyRef === undefined ? {} : { key_ref: keyRef }) },
		server: { bind: bind ?? DEFAULT_BIND },
		threads: { page_size: pageSize ?? DEFAULT_PAGE_SIZE },
	});
}

/** Serialize a validated configuration in a stable section order. */
export function serializeConfig(config: OnlyneConfig): string {
	const lines: string[] = [
		"# Onlyne project configuration. Credentials stay behind key_ref references.",
		"[node]",
		`id = ${tomlString(config.node.id)}`,
		`region = ${tomlString(config.region)}`,
	];
	if (config.node.key_ref !== undefined) lines.push(`key_ref = ${tomlString(config.node.key_ref)}`);
	lines.push(
		"",
		"[server]",
		`bind = ${tomlString(config.server.bind)}`,
		"",
		"[threads]",
		`page_size = ${String(config.threads.page_size)}`,
	);
	return `${lines.join("\n")}\n`;
}

function validateConfig(config: OnlyneConfig): OnlyneConfig {
	const region = normalizeRegion(config.region);
	const id = normalizeNodeId(config.node.id);
	const keyRef = config.node.key_ref?.trim();
	const bind = config.server.bind.trim();
	if (bind.length === 0) throw new ConfigError("invalid", "server.bind must not be empty.");
	if (/[\s/]/.test(bind)) {
		throw new ConfigError("invalid", `server.bind must be a hostname or IP address, got ${JSON.stringify(bind)}.`);
	}
	const pageSize = config.threads.page_size;
	if (!Number.isInteger(pageSize) || pageSize < 1 || pageSize > 100) {
		throw new ConfigError("invalid", `threads.page_size must be an integer from 1 to 100, got ${String(pageSize)}.`);
	}
	return {
		path: config.path,
		region,
		node: { id, ...(keyRef === undefined || keyRef.length === 0 ? {} : { key_ref: keyRef }) },
		server: { bind },
		threads: { page_size: pageSize },
	};
}

function rejectRetired(
	raw: Record<string, unknown>,
	node: Record<string, unknown>,
	server: Record<string, unknown>,
	threads: Record<string, unknown>,
): void {
	rejectKey(node, "endpoint", "node.endpoint is not supported; the endpoint comes from server.bind and the assigned port.");
	rejectKey(server, "port", "server.port is not supported; the operating system assigns the listening port.");
	rejectKey(raw, "peers", "peers is not supported; peers are discovered over the network and kept in the project SQLite.");
	rejectKey(raw, "omp", "omp is not supported; delivery and spawning are decided by the OMP judge.");
	rejectKey(threads, "default_attention", "threads.default_attention is not supported; reads default to the attention list.");
}

function rejectKey(source: Record<string, unknown>, key: string, message: string): void {
	if (source[key] !== undefined) throw new ConfigError("invalid", message);
}

function table(root: Record<string, unknown>, key: string, path: string): Record<string, unknown> {
	const value = root[key];
	if (value === undefined) return {};
	if (!isTable(value)) throw new ConfigError("invalid", `${path}: ${key} must be a table.`);
	return value;
}

function optionalString(source: Record<string, unknown>, key: string, field: string): string | undefined {
	const value = source[key];
	if (value === undefined) return undefined;
	if (typeof value !== "string") throw new ConfigError("invalid", `${field} must be a string.`);
	return value;
}

function optionalInteger(source: Record<string, unknown>, key: string, field: string): number | undefined {
	const value = source[key];
	if (value === undefined) return undefined;
	if (typeof value !== "number" || !Number.isInteger(value)) {
		throw new ConfigError("invalid", `${field} must be an integer.`);
	}
	return value;
}

function isTable(value: unknown): value is Record<string, unknown> {
	return value !== null && typeof value === "object" && !Array.isArray(value);
}

function tomlString(value: string): string {
	return `"${value.replace(/\\/g, "\\\\").replace(/"/g, '\\"').replace(/\n/g, "\\n").replace(/\r/g, "\\r").replace(/\t/g, "\\t")}"`;
}

