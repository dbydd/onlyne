import { afterEach, describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { OnlyneError, type Json } from "@onlyne/lib";
import { configPath, normalizeNodeId, type AgyOnlyneConfig } from "../src/config.ts";
import { settleIntent } from "../src/core/guard.ts";
import { AgyRuntime } from "../src/runtime/host.ts";

const runtimes: AgyRuntime[] = [];
const tempDirs: string[] = [];

function makeTempDir(prefix: string): string {
	const dir = mkdtempSync(join(tmpdir(), prefix));
	tempDirs.push(dir);
	return dir;
}

function makeConfig(dir: string, id: string): AgyOnlyneConfig {
	return {
		path: configPath(dir),
		region: "urn:onlyne:region:local",
		node: { id: normalizeNodeId(id), key_scope: "project" },
		server: { bind: "127.0.0.1" },
		threads: { page_size: 50 },
		judge: { mode: "none" },
	};
}

function mutation(key: string) {
	return {
		idempotency_key: key,
		retry_until: new Date(Date.now() + 60_000).toISOString(),
	};
}

afterEach(async () => {
	while (runtimes.length > 0) {
		await runtimes.pop()!.close().catch(() => {});
	}
	while (tempDirs.length > 0) {
		rmSync(tempDirs.pop()!, { recursive: true, force: true });
	}
});

describe("ActionGuard, settleIntent & onlyne:// URLs", () => {
	test("settleIntent overrides operate_resource -> communicate when addressed without complete meta.resource", () => {
		const res = settleIntent("operate_resource", { to: "beta", msg: "hi" });
		expect(res.intent).toBe("communicate");
		expect(res.override?.chosen).toBe("operate_resource");
		expect(res.override?.fallback).toBe("communicate");

		const unchanged = settleIntent("operate_resource", {
			to: "beta",
			resource: { owner: "beta", id: "gw-1" },
			operation: "status",
		});
		expect(unchanged.intent).toBe("operate_resource");
		expect(unchanged.override).toBeUndefined();
	});

	test("enforces subagent read-only context slices across onlyne:// URLs while allowing main caller full views", async () => {
		const dir = makeTempDir("agy-guard-");
		const cfg = makeConfig(dir, "guard-node");
		const rt = await AgyRuntime.start({
			cwd: dir,
			config: cfg,
			inMemory: true,
			discovery: false,
			bindHttp: true,
		});
		runtimes.push(rt);

		// Create two threads owned by guard-node
		const t1 = await rt.node.threads.create(rt.node.auth, {
			title: "Allowed Subagent Thread",
			...mutation("create-t1"),
		});
		const t2 = await rt.node.threads.create(rt.node.auth, {
			title: "Secret Main Thread",
			...mutation("create-t2"),
		});

		// Admit a local message into t1
		await rt.node.messages.admit(rt.node.auth, {
			message: {
				id: "m-1",
				sender: cfg.node.id,
				delivery_kind: "note",
				msg: "Context slice payload",
				meta: { thread: t1.thread_ref },
			},
			target_endpoint: cfg.node.id,
			plan: { response: "none", execution: "existing" },
			...mutation("admit-m1"),
		});

		// Allocate a subagent context scoped strictly to t1 and bind it to conv-sub-1
		const subCtx = await rt.contexts.allocate(t1.thread_ref, "sub-work", {
			id: "m-1",
			sender: cfg.node.id,
			delivery_kind: "note",
			msg: "Context slice payload",
		});
		await rt.contexts.bind(subCtx.alias, { agent_id: "conv-sub-1" });

		// Main caller succeeds across node-wide routes
		const root = await rt.readUrl({ agent_id: "conv-main", main: true }, "onlyne://");
		expect(root.content).toContain("guard-node");
		const neighbors = await rt.readUrl({ agent_id: "conv-main", main: true }, "onlyne://neighbors");
		expect(neighbors.content).toContain("# Onlyne Neighbors");
		const threads = await rt.readUrl({ agent_id: "conv-main", main: true }, "onlyne://threads");
		expect(threads.content).toContain("Allowed Subagent Thread");
		expect(threads.content).toContain("Secret Main Thread");
		const resources = await rt.readUrl({ agent_id: "conv-main", main: true }, "onlyne://resources");
		expect(resources.contentType).toBe("application/json");

		// Subagent can read its own slice messages, contexts, and todos
		const subMsgs = await rt.readUrl(
			{ agent_id: "conv-sub-1", main: false },
			`onlyne://threads/${encodeURIComponent(t1.thread_ref.owner)}/${encodeURIComponent(t1.thread_ref.id)}/messages`,
		);
		expect(subMsgs.content).toContain("Context slice payload");

		const subCtxView = await rt.readUrl({ agent_id: "conv-sub-1", main: false }, "onlyne://contexts");
		const parsedCtx = JSON.parse(subCtxView.content) as Array<{ agent_id: string; alias: string }>;
		expect(parsedCtx.length).toBe(1);
		expect(parsedCtx[0]?.alias).toBe(subCtx.alias);

		// Subagent is blocked from node-wide listings and unallocated thread t2
		await expectForbidden(() => rt.readUrl({ agent_id: "conv-sub-1", main: false }, "onlyne://neighbors"));
		await expectForbidden(() => rt.readUrl({ agent_id: "conv-sub-1", main: false }, "onlyne://threads"));
		await expectForbidden(() => rt.readUrl({ agent_id: "conv-sub-1", main: false }, "onlyne://inbox"));
		await expectForbidden(() => rt.readUrl({ agent_id: "conv-sub-1", main: false }, "onlyne://outbound"));
		await expectForbidden(() => rt.readUrl({ agent_id: "conv-sub-1", main: false }, "onlyne://resources"));
		await expectForbidden(() =>
			rt.readUrl(
				{ agent_id: "conv-sub-1", main: false },
				`onlyne://threads/${encodeURIComponent(t2.thread_ref.owner)}/${encodeURIComponent(t2.thread_ref.id)}/messages`,
			),
		);
	});
});

async function expectForbidden(fn: () => Promise<unknown>): Promise<void> {
	try {
		await fn();
		throw new Error("Expected OnlyneError(forbidden), but call succeeded");
	} catch (err) {
		expect(err).toBeInstanceOf(OnlyneError);
		expect((err as OnlyneError).error.code).toBe("forbidden");
	}
}
