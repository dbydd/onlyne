import { afterEach, describe, expect, test } from "bun:test";
import { MemoryStorage } from "@onlyne/lib";
import type { Action, OnlyneNode } from "@onlyne/lib";
import { ContextRegistry } from "../src/store.ts";
import { createOnlyneNode } from "../src/node.ts";
import type { OnlyneConfig } from "../src/types.ts";

const servers: Array<{ stop(force?: boolean): void }> = [];
afterEach(() => { for (const server of servers.splice(0)) server.stop(true); });

function config(id: string): OnlyneConfig {
	return {
		path: `/tmp/${id}/.onlyne/config.toml`,
		region: "urn:onlyne:region:test",
		node: { id },
		server: { bind: "127.0.0.1" },
		threads: { page_size: 50 },
	};
}
const runtime = { supports: async () => true, inject: async () => ({ context: "main", consumed: false }) };

function serve(id: string, decide: () => Promise<Action>): { node: OnlyneNode; endpoint: string } {
	let handler: (request: Request) => Promise<Response> = async () => new Response("starting", { status: 503 });
	const server = Bun.serve({ hostname: "127.0.0.1", port: 0, fetch: request => handler(request) });
	servers.push(server);
	const endpoint = `http://127.0.0.1:${String(server.port)}/onlyne/v3`;
	const storage = new MemoryStorage();
	const node = createOnlyneNode({
		config: config(id), endpoint, instanceId: crypto.randomUUID(), storage,
		contexts: new ContextRegistry(storage, id, "Main"), runtime, decide,
	});
	handler = request => node.fetch(request);
	return { node, endpoint };
}

describe("two nodes", () => {
	test("discovery resolves the sender and first contact opens a thread", async () => {
		const b = serve("urn:onlyne:agent:b", async () => ({ kind: "read", scope: "directory" }));
		const a = serve("urn:onlyne:agent:a", async () => ({ kind: "send", to: "urn:onlyne:agent:b", response: "async", execution: "new" }));
		await a.node.discovery.observe(b.endpoint, "test", "urn:onlyne:agent:b");
		await b.node.discovery.observe(a.endpoint, "test", "urn:onlyne:agent:a");
		const outcome = await a.node.client.msg({ delivery_kind: "ask", msg: "hello" });
		expect(outcome).toMatchObject({ admission: "accepted" });
		const inbox = await b.node.messages.inbox(b.node.auth, { page_size: 10 });
		expect(inbox.items).toHaveLength(1);
		expect(inbox.items[0]?.attention.message_ref.sender).toBe("urn:onlyne:agent:a");
		expect(inbox.items[0]?.attention.entry?.message.msg).toBe("hello");
		expect((await b.node.threads.list(b.node.auth)).items).toHaveLength(1);
	});
});
