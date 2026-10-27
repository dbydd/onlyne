import { afterEach, describe, expect, test } from "bun:test";
import { MemoryStorage, generateKeyMaterial } from "@onlyne/lib";
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
const runtime = { supports: async () => true, inject: async () => ({ context: "main", consumed: false }), notify: () => {} };
// A test never runs the library's loop; the clock is only there because the node requires one.
const clock = { now: () => Date.now(), setTimer: () => 0, clearTimer: () => {} };

function serve(id: string, decide: () => Promise<Action>): { node: OnlyneNode; endpoint: string } {
	let handler: (request: Request) => Promise<Response> = async () => new Response("starting", { status: 503 });
	const server = Bun.serve({ hostname: "127.0.0.1", port: 0, fetch: request => handler(request) });
	servers.push(server);
	const endpoint = `http://127.0.0.1:${String(server.port)}/onlyne/v3`;
	const storage = new MemoryStorage();
	const node = createOnlyneNode({
		config: config(id), endpoint, instanceId: crypto.randomUUID(), storage,
		contexts: new ContextRegistry(storage, id, "Main"), runtime, clock, decide,
	});
	handler = request => node.fetch(request);
	return { node, endpoint };
}

describe("two nodes", () => {
	test("discovery resolves the sender and first contact opens a thread the sender owns", async () => {
		const b = serve("urn:onlyne:agent:b", async () => ({ kind: "read", scope: "directory" }));
		const a = serve("urn:onlyne:agent:a", async () => ({ kind: "send", to: "urn:onlyne:agent:b", response: "async", execution: "new" }));
		await a.node.discovery.observe(b.endpoint, "test", "urn:onlyne:agent:b");
		await b.node.discovery.observe(a.endpoint, "test", "urn:onlyne:agent:a");
		const outcome = await a.node.client.msg({ delivery_kind: "ask", msg: "hello" });
		expect(outcome).toMatchObject({ admission: "accepted" });
		// The initiator owns the conversation; b is a member and pulls the entry from a.
		expect((await a.node.threads.list(a.node.auth)).items).toHaveLength(1);
		await b.node.client.reconcile();
		const inbox = await b.node.messages.inbox(b.node.auth, { page_size: 10 });
		expect(inbox.items).toHaveLength(1);
		expect(inbox.items[0]?.attention.message_ref.sender).toBe("urn:onlyne:agent:a");
		expect(inbox.items[0]?.attention.entry?.message.msg).toBe("hello");
		expect(inbox.items[0]?.attention.thread_ref.owner).toBe("urn:onlyne:agent:a");
	});
});

describe("admission", () => {
	const owner = "urn:onlyne:agent:owner";
	const approved = "urn:onlyne:agent:approved";
	/** A keyed node: the keyless binding admits every declared actor, so it cannot show the gate. */
	function keyed(storage: MemoryStorage): OnlyneNode {
		const material = generateKeyMaterial();
		return createOnlyneNode({
			config: config(owner), endpoint: "http://127.0.0.1:1/onlyne/v3", instanceId: crypto.randomUUID(), storage,
			contexts: new ContextRegistry(storage, owner, "Main"), runtime, clock,
			decide: async () => ({ kind: "read", scope: "directory" }),
			keys: Object.assign(async () => material, { replace: async () => undefined }),
		});
	}
	const request = (sender: string, id: string) => ({
		idempotency_key: crypto.randomUUID(),
		retry_until: new Date(Date.now() + 60_000).toISOString(),
		message: { id, sender, delivery_kind: "ask", msg: "hello" },
		target_endpoint: owner,
		plan: { response: "async" as const, execution: "existing" as const },
	});

	// Design §1.1: admission reads the trust phase, not the discovery catalog. A restart clears the
	// in-process discovery set; the durable approval is what lets the peer in.
	test("a completed exchange admits the peer even though this process discovered nobody", async () => {
		const storage = new MemoryStorage();
		await storage.transaction(async tx => {
			await tx.put(`trust/${encodeURIComponent(approved)}`, {
				node_id: approved, public_key: "peer-key", fingerprint: "peer-fingerprint", state: "trusted",
				first_seen: new Date().toISOString(), last_checked: new Date().toISOString(),
				local_approved: true, remote_approved: true, base_url: "https://peer.test/onlyne/v3",
			});
		});
		const node = keyed(storage);
		expect(node.discovery.known(approved)).toBe(false);
		const receipt = await node.messages.admit({ actor: approved }, request(approved, "m-1"));
		expect(receipt.thread_ref.owner).toBe(owner);
	});

	test("a peer with no completed exchange is refused", async () => {
		const node = keyed(new MemoryStorage());
		const stranger = "urn:onlyne:agent:stranger";
		await expect(node.messages.admit({ actor: stranger }, request(stranger, "m-2")))
			.rejects.toMatchObject({ error: { code: "not_found" } });
	});
});
