/**
 * The listening port survives a restart.
 *
 * A node that answers somewhere new on every start is one every peer has to be taught again,
 * and the only fix an operator has is pinning the port by hand in the configuration. The node
 * now records the port it bound, so the second start answers at the address the first one
 * published.
 */
import { afterEach, expect, test } from "bun:test";
import { mkdtemp, readFile, rm, writeFile, mkdir } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { ExtensionAPI, ExtensionContext } from "@oh-my-pi/pi-coding-agent";
import { loadConfig, parseConfig, serializeConfig } from "../src/config.ts";
import { setRuntime, startRuntime, stopRuntime } from "../src/runtime.ts";

const dirs: string[] = [];

afterEach(async () => {
	await stopRuntime();
	for (const dir of dirs.splice(0)) await rm(dir, { recursive: true, force: true });
});

/** Another test file leaves a stub runtime in the module slot; this one needs the real one. */
function fresh(): void {
	setRuntime(undefined);
}

/**
 * The session surface `start()` touches. Its timers are inert: a test never runs the library's
 * maintenance loop, and nothing here waits on a pass — only on the socket this node binds.
 */
function session(cwd: string): ExtensionContext {
	return {
		agent: { kind: "main", id: "Main", name: "main", depth: 0 },
		cwd,
		sessionManager: { getBranch: () => [], getSessionId: () => "port-test" },
		isIdle: () => true,
		hasPendingMessages: () => false,
		setTimeout: () => 0,
		setInterval: () => 0,
		clearTimer: () => {},
	} as unknown as ExtensionContext;
}

function host(logs: Array<{ name: string; fields: Record<string, unknown> }> = []): ExtensionAPI {
	const noop = () => {};
	const record = (name: string, fields: Record<string, unknown>) => { logs.push({ name, fields }); };
	return {
		logger: { info: record, warn: record, debug: noop, error: noop },
		registerTool: noop,
		appendEntry: noop,
		sendMessage: noop,
	} as unknown as ExtensionAPI;
}

async function project(): Promise<{ dir: string; path: string }> {
	const dir = await mkdtemp(join(tmpdir(), "onlyne-port-"));
	dirs.push(dir);
	await mkdir(join(dir, ".onlyne"), { recursive: true });
	const path = join(dir, ".onlyne", "config.toml");
	await writeFile(path, [
		"[node]",
		'id = "urn:onlyne:agent:ported"',
		'region = "urn:onlyne:region:port-test"',
		'key_scope = "none"',
		"",
		"[server]",
		'bind = "127.0.0.1"',
		"",
		"[threads]",
		"page_size = 50",
		"",
	].join("\n"), "utf8");
	return { dir, path };
}

test("the port bound on the first start is the port the second start answers on", async () => {
	fresh();
	const { dir, path } = await project();
	const logs: Array<{ name: string; fields: Record<string, unknown> }> = [];
	const first = await startRuntime(host(logs), session(dir));
	expect(first.config.server.port).toBeGreaterThan(0);
	// The address this node published names the same port it recorded.
	expect(first.endpoint).toContain(`:${String(first.config.server.port)}/`);
	const published = first.endpoint;
	expect(await readFile(path, "utf8")).toContain(`port = ${String(first.config.server.port)}`);
	// The write-back is a logged fact, so an operator can see which port the node took.
	expect(logs.map(entry => entry.name)).toContain("onlyne.port.persisted");
	expect(logs.find(entry => entry.name === "onlyne.port.persisted")?.fields.port).toBe(first.config.server.port);
	await stopRuntime();

	const second = await startRuntime(host(), session(dir));
	expect(second.endpoint).toBe(published);
	expect(second.config.server.port).toBe(Number(new URL(published).port));
}, 30_000);

test("a port another process holds does not stop the node: it moves and records where", async () => {
	fresh();
	const { dir, path } = await project();
	const blocker = Bun.serve({ hostname: "127.0.0.1", port: 0, fetch: () => new Response("busy") });
	try {
		const pinned = parseConfig(await readFile(path, "utf8"), path);
		await writeFile(path, serializeConfig({ ...pinned, server: { ...pinned.server, port: blocker.port } }), "utf8");
		expect((await loadConfig(dir)).server.port).toBe(blocker.port);

		const logs: Array<{ name: string; fields: Record<string, unknown> }> = [];
		const runtime = await startRuntime(host(logs), session(dir));
		// The move is stated as a warning that names the port it could not take.
		expect(logs.find(entry => entry.name === "onlyne.port.taken")?.fields).toMatchObject({ wanted: blocker.port });
		expect(runtime.config.server.port).not.toBe(blocker.port);
		expect(runtime.endpoint).toContain(`:${String(runtime.config.server.port)}/`);
		expect(await readFile(path, "utf8")).toContain(`port = ${String(runtime.config.server.port)}`);
	} finally {
		blocker.stop(true);
	}
}, 30_000);
