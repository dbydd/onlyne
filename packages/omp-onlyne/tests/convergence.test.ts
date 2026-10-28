/**
 * The four delivery shapes that left an entry `injected: false` forever, each run once end to
 * end: a real node, the real host bridge, and the library's own maintenance pass driving both.
 *
 * The reports named one death per shape: an idle session's fragment waited for an assembly that
 * never came, a busy session's next-turn fragment was offered again inside the same turn, a lost
 * binding re-asked the judge every pass, and a projection whose resource had moved belonged to a
 * handler that could never act on it. Every shape has to end in one of two states: the row is
 * injected after the host confirms consumption, or it carries a settlement naming why it ended.
 * The judge is a model call, so the number of judgements is part of the assertion, not a detail.
 */
import { afterEach, describe, expect, test } from "bun:test";
import { MemoryStorage, OnlyneError, type Action, type Entry, type Json, type Mutation, type OnlyneNode, type Resource } from "@onlyne/lib";
import type { ExtensionAPI, ExtensionContext } from "@oh-my-pi/pi-coding-agent";
import type { JudgeDecider } from "../src/decider.ts";
import { createRuntimeBridge, type BridgeDeps } from "../src/messages.ts";
import { createOnlyneNode } from "../src/node.ts";
import { ContextRegistry } from "../src/store.ts";
import type { OnlyneConfig } from "../src/types.ts";

const PEER = "urn:onlyne:agent:peer";
const SELF = "urn:onlyne:agent:self";
const OWNER = "urn:onlyne:agent:owner";
const HANDLER = "urn:onlyne:agent:handler";
const THIRD = "urn:onlyne:agent:third";

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

function mutation(): Mutation {
	return { idempotency_key: crypto.randomUUID(), retry_until: new Date(Date.now() + 60_000).toISOString() };
}

/** The library's loop is never started here: each test calls `maintain()` itself, pass by pass. */
const clock = { now: () => Date.now(), setTimer: () => 0, clearTimer: () => {} };

interface Host {
	node: OnlyneNode;
	endpoint: string;
	/** One entry per message the bridge handed the host. */
	sends: Array<{ options: Record<string, unknown> | undefined }>;
	logs: string[];
	/** Model calls the bridge made; the bound every shape is measured against. */
	judged: { count: number };
}

/**
 * One node whose runtime is the real bridge. `decide` answers the node's own sends; `decision`
 * answers an inbound entry the way the judge would; `idle` is the main session's state.
 */
function serve(
	id: string,
	decide: () => Promise<Action>,
	decision: () => Promise<unknown>,
	idle: () => boolean,
): Host {
	let handler: (request: Request) => Promise<Response> = async () => new Response("starting", { status: 503 });
	const server = Bun.serve({ hostname: "127.0.0.1", port: 0, fetch: request => handler(request) });
	servers.push(server);
	const endpoint = `http://127.0.0.1:${String(server.port)}/onlyne/v3`;
	const storage = new MemoryStorage();
	const contexts = new ContextRegistry(storage, id, "Main");
	const sends: Array<{ options: Record<string, unknown> | undefined }> = [];
	const logs: string[] = [];
	const judged = { count: 0 };
	const deps = {
		nodeId: id,
		pi: {
			sendMessage: (_message: unknown, options?: Record<string, unknown>) => { sends.push({ options }); },
			logger: {
				info: (name: string) => { logs.push(name); },
				warn: (name: string) => { logs.push(name); },
				debug: () => {},
			},
		} as unknown as ExtensionAPI,
		main: { agent: { kind: "main", id: "Main", name: "main", depth: 0 }, isIdle: idle } as unknown as ExtensionContext,
		decider: {
			decideInbound: async () => {
				judged.count += 1;
				return await decision();
			},
		} as unknown as JudgeDecider,
		contexts,
	} as unknown as BridgeDeps;
	const node = createOnlyneNode({
		config: config(id), endpoint, instanceId: crypto.randomUUID(), storage,
		contexts, runtime: createRuntimeBridge(deps), clock, decide,
	});
	handler = request => node.fetch(request);
	return { node, endpoint, sends, logs, judged };
}

async function meet(left: Host, right: Host): Promise<void> {
	await left.node.discovery.observe(right.endpoint, "test", right.node.auth.actor);
	await right.node.discovery.observe(left.endpoint, "test", left.node.auth.actor);
}

type HeldRow = { injected: boolean; settlement?: { via: string; handler?: string }; attention: { entry?: Entry } };

/** Every row this node holds, after the pull that delivered it. */
async function held(host: Host): Promise<HeldRow[]> {
	return (await host.node.messages.inbox(host.node.auth, { page_size: 10 })).items as unknown as HeldRow[];
}

/**
 * The host's confirmation, which is the only thing that closes an injected row. The library
 * commits the flag before it hands the owed todo to `runtime.todo`, and this test has no live
 * OMP session to accept that todo, so the rejection is expected here; the assertion that
 * follows is what proves the commit landed.
 */
async function confirm(host: Host): Promise<void> {
	const [row] = await held(host);
	await host.node.client.confirmConsumption(row!.attention.entry!, "Main").catch(() => undefined);
}

describe("delivery convergence", () => {
	test("an idle session's fragment: one trigger, one judgement, injected once confirmed", async () => {
		const peer = serve(PEER, async () => ({ kind: "send", to: SELF, response: "async", execution: "existing" }), async () => ({ action: "fragment" }), () => true);
		const self = serve(SELF, async () => ({ kind: "read", scope: "directory" }), async () => ({ action: "fragment" }), () => true);
		await meet(peer, self);
		await peer.node.client.msg({ delivery_kind: "ask", msg: "hello" });
		await self.node.client.reconcile();
		expect(await held(self)).toHaveLength(1);

		for (let pass = 0; pass < 3; pass += 1) await self.node.client.maintain();
		// An idle session never reaches the next assembly, so the fragment starts a turn instead.
		expect(self.sends).toEqual([{ options: { triggerTurn: true } }]);
		expect(self.logs).toContain("onlyne.delivery.escalated");
		expect(self.judged.count).toBe(1);

		await confirm(self);
		expect((await held(self))[0]?.injected).toBe(true);
	});

	test("a busy session's wake: one delivery across five passes, injected once confirmed", async () => {
		const peer = serve(PEER, async () => ({ kind: "send", to: SELF, response: "async", execution: "existing" }), async () => ({ action: "wake", insertion: "follow_up" }), () => false);
		const self = serve(SELF, async () => ({ kind: "read", scope: "directory" }), async () => ({ action: "wake", insertion: "follow_up" }), () => false);
		await meet(peer, self);
		await peer.node.client.msg({ delivery_kind: "ask", msg: "hello" });
		await self.node.client.reconcile();

		for (let pass = 0; pass < 5; pass += 1) await self.node.client.maintain();
		// A follow-up waits for the running turn to end; a second copy would answer the same
		// message twice, which is the duplicate the report measured.
		expect(self.sends).toEqual([{ options: { deliverAs: "followUp" } }]);
		expect(self.judged.count).toBe(1);

		await confirm(self);
		expect((await held(self))[0]?.injected).toBe(true);
	});

	test("a lost binding: judged once, escalated once past the budget, injected once confirmed", async () => {
		const peer = serve(PEER, async () => ({ kind: "send", to: SELF, response: "async", execution: "existing" }), async () => { throw new OnlyneError("context_lost", "gone"); }, () => false);
		const self = serve(SELF, async () => ({ kind: "read", scope: "directory" }), async () => { throw new OnlyneError("context_lost", "gone"); }, () => false);
		await meet(peer, self);
		await peer.node.client.msg({ delivery_kind: "ask", msg: "hello" });
		await self.node.client.reconcile();

		// The row stays owed while the judgement fails, so a pass reports the failure and the
		// next one retries; past the re-decision budget the entry is delivered instead.
		for (let pass = 0; pass < 3; pass += 1) await self.node.client.maintain().catch(() => undefined);
		expect(self.judged.count).toBe(1);
		expect(self.sends).toEqual([{ options: { triggerTurn: true } }]);
		expect(self.logs).toContain("onlyne.delivery.escalated");

		await confirm(self);
		expect((await held(self))[0]?.injected).toBe(true);
	});

	test("a projection whose resource moved settles with a reason and never reaches the judge", async () => {
		const owner = serve(OWNER, async () => ({ kind: "read", scope: "directory" }), async () => ({ action: "wake", insertion: "follow_up" }), () => false);
		const handler = serve(HANDLER, async () => ({ kind: "read", scope: "directory" }), async () => ({ action: "wake", insertion: "follow_up" }), () => false);
		await meet(owner, handler);

		// The resource already belongs to a third node: this is the state a completed handover
		// leaves, and the handler named here can never act on a projection of it again.
		const resource: Resource = {
			resource_ref: { owner: OWNER, id: "chat-1" },
			kind: "im_conversation",
			operations: ["read", "send"],
			binding: { handler: THIRD, cursor: "0" },
			log_readers: [OWNER, HANDLER],
			event_seq: "0",
			revision: "rev-1",
			retention_ms: 60_000,
		};
		await owner.node.resources.register(owner.node.auth, resource);

		const thread = await owner.node.threads.create(owner.node.auth, { ...mutation(), title: "projections" });
		await owner.node.threads.member(owner.node.auth, thread.thread_ref, HANDLER, { access: ["post", "read"] }, mutation());
		await owner.node.messages.admit(owner.node.auth, {
			idempotency_key: crypto.randomUUID(),
			retry_until: new Date(Date.now() + 60_000).toISOString(),
			message: {
				id: "proj-1", sender: OWNER, delivery_kind: "资源投影", msg: "投影 chat-1。",
				meta: {
					thread: thread.thread_ref, mentions: [HANDLER],
					data: { resource_ref: resource.resource_ref } as unknown as { [key: string]: Json },
				},
			},
			target_endpoint: OWNER,
			plan: { response: "async", execution: "existing" },
		});

		await owner.node.client.maintain();
		await handler.node.client.reconcile();
		expect(await held(handler)).toHaveLength(1);

		await handler.node.client.maintain();
		const [row] = await held(handler);
		// Terminal, with the reason and the handler the resource moved to: no judgement, no send.
		expect(row?.injected).toBe(true);
		expect(row?.settlement).toMatchObject({ via: "handover", handler: THIRD });
		expect(handler.judged.count).toBe(0);
		expect(handler.sends).toHaveLength(0);
	});
});
