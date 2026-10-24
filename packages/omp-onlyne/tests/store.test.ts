import { afterEach, describe, expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { ContextRegistry, SqliteStorage } from "../src/store.ts";

const directories: string[] = [];
const stores: SqliteStorage[] = [];

afterEach(async () => {
	for (const store of stores.splice(0)) store.close();
	await Promise.all(directories.splice(0).map(path => rm(path, { recursive: true, force: true })));
});

async function open(): Promise<{ storage: SqliteStorage; contexts: ContextRegistry }> {
	const directory = await mkdtemp(join(tmpdir(), "omp-onlyne-store-"));
	directories.push(directory);
	const storage = new SqliteStorage(join(directory, "omp.db"));
	stores.push(storage);
	return { storage, contexts: new ContextRegistry(storage, "urn:onlyne:agent:a", "Main") };
}

describe("SqliteStorage", () => {
	test("commits, reads and scans", async () => {
		const { storage } = await open();
		await storage.transaction(async tx => {
			await tx.put("context/c-1", { alias: "c-1" });
			await tx.put("context/c-2", { alias: "c-2" });
			await tx.put("other/x", { x: 1 });
		});
		expect(await storage.transaction(tx => tx.get<{ alias: string }>("context/c-1"))).toEqual({ alias: "c-1" });
		const rows = await storage.transaction(tx => tx.scan<{ alias: string }>("context/"));
		expect(rows.map(row => row.value.alias)).toEqual(["c-1", "c-2"]);
	});

	test("rolls back every write when the callback rejects", async () => {
		const { storage } = await open();
		await expect(
			storage.transaction(async tx => {
				await tx.put("context/c-1", { alias: "c-1" });
				throw new Error("stop");
			}),
		).rejects.toThrow("stop");
		expect(await storage.transaction(tx => tx.get("context/c-1"))).toBeUndefined();
	});

	test("serializes transactions", async () => {
		const { storage } = await open();
		const events: string[] = [];
		const gate = Promise.withResolvers<void>();
		const started = Promise.withResolvers<void>();
		const first = storage.transaction(async tx => {
			events.push("first-start");
			started.resolve();
			await gate.promise;
			await tx.put("first", 1);
			events.push("first-end");
		});
		const second = storage.transaction(async tx => {
			events.push("second-start");
			await tx.put("second", 2);
			events.push("second-end");
		});
		await started.promise;
		expect(events).toEqual(["first-start"]);
		gate.resolve();
		await Promise.all([first, second]);
		expect(events).toEqual(["first-start", "first-end", "second-start", "second-end"]);
	});
});

describe("ContextRegistry", () => {
	test("allocates stable aliases, binds and updates state", async () => {
		const { contexts } = await open();
		const first = await contexts.allocate({ owner: "urn:onlyne:agent:a", id: "research" }, "review the data");
		const second = await contexts.allocate(undefined, "write the report");
		expect(first.alias).toBe("c-1");
		expect(second.alias).toBe("c-2");
		expect(first.state).toBe("unbound");

		const bound = await contexts.bind(first.alias, {
			agent_id: "Review-1",
			session_file: "/tmp/review.jsonl",
			parent_id: "Main",
		});
		expect(bound.agent_id).toBe("Review-1");
		expect(bound.state).toBe("idle");
		expect((await contexts.find(first.alias))?.session_file).toBe("/tmp/review.jsonl");
		expect((await contexts.updateState(first.alias, "running"))?.state).toBe("running");
		expect((await contexts.list()).map(context => context.alias)).toEqual(["c-1", "c-2"]);
	});
});
