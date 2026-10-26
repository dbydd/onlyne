import { afterEach, describe, expect, test } from "bun:test";
import { MemoryStorage } from "@onlyne/lib";
import type { Action, OnlyneNode, Outbound } from "@onlyne/lib";
import { InternalUrlRouter } from "@oh-my-pi/pi-coding-agent/internal-urls";
import type { ResolveContext } from "@oh-my-pi/pi-coding-agent/internal-urls";
import { createOnlyneNode } from "../src/node.ts";
import { setRuntime, type PluginRuntime } from "../src/runtime.ts";
import { ContextRegistry } from "../src/store.ts";
import type { OnlyneConfig } from "../src/types.ts";
import { registerOnlyneUrls } from "../src/urls.ts";

const servers: Array<{ stop(force?: boolean): void }> = [];
afterEach(() => {
	setRuntime(undefined);
	for (const server of servers.splice(0)) server.stop(true);
});

function config(id: string): OnlyneConfig {
	return {
		path: `/tmp/${id}/.onlyne/config.toml`,
		region: "urn:onlyne:region:test",
		node: { id },
		server: { bind: "127.0.0.1" },
		threads: { page_size: 50 },
	};
}
const host = { supports: async () => true, inject: async () => ({ context: "main", consumed: false }), notify: () => {} };
// A test never runs the library's loop; the clock is only there because the node requires one.
const clock = { now: () => Date.now(), setTimer: () => 0, clearTimer: () => {} };

interface Peer {
	node: OnlyneNode;
	storage: MemoryStorage;
	endpoint: string;
}

function serve(id: string, decide: () => Promise<Action>): Peer {
	let handler: (request: Request) => Promise<Response> = async () => new Response("starting", { status: 503 });
	const server = Bun.serve({ hostname: "127.0.0.1", port: 0, fetch: request => handler(request) });
	servers.push(server);
	const endpoint = `http://127.0.0.1:${String(server.port)}/onlyne/v3`;
	const storage = new MemoryStorage();
	const node = createOnlyneNode({
		config: config(id), endpoint, instanceId: crypto.randomUUID(), storage,
		contexts: new ContextRegistry(storage, id, "Main"), runtime: host, clock, decide,
	});
	handler = request => node.fetch(request);
	return { node, storage, endpoint };
}

/** The plugin's runtime as the URL views consume it: config, node, storage, contexts. */
function pluginFor(peer: Peer, id: string): PluginRuntime {
	return {
		config: config(id),
		endpoint: peer.endpoint,
		mainAgentId: "Main",
		node: peer.node,
		storage: peer.storage,
		contexts: new ContextRegistry(peer.storage, id, "Main"),
	} as unknown as PluginRuntime;
}

async function readView(url: string): Promise<string> {
	registerOnlyneUrls();
	const caller = { session: { getAgentId: () => "Main" } } as unknown as ResolveContext;
	return (await InternalUrlRouter.instance().resolve(url, caller)).content;
}

interface OutboundDetail {
	message_id?: string;
	entry?: string;
	landed?: { thread: string; seq: string; guarantee: string };
	reply?: { by: string; seq: string; message_id: string; body: string };
	reply_unavailable?: string;
	in_reply_to?: { sender: string; id: string };
}

interface Exchange {
	a: PluginRuntime;
	b: PluginRuntime;
	helloId: string;
	replyId: string;
	thirdId: string;
	aThreadId: string;
	bThreadId: string;
}

/** a greets b, b replies, a answers: three hops over two nodes, with no thread named by hand. */
async function exchange(): Promise<Exchange> {
	let hop = 0;
	let helloId = "";
	let replyId = "";
	const a = serve("urn:onlyne:agent:a", async () => hop++ === 0
		? { kind: "send", to: "urn:onlyne:agent:b", response: "async", execution: "new" }
		: { kind: "send", to: "urn:onlyne:agent:b", response: "async", execution: "new", in_reply_to: { sender: "urn:onlyne:agent:b", id: replyId }, relation: "reply" });
	const b = serve("urn:onlyne:agent:b", async () => ({
		kind: "send", to: "urn:onlyne:agent:a", response: "none", execution: "new",
		in_reply_to: { sender: "urn:onlyne:agent:a", id: helloId }, relation: "reply",
	}));
	await a.node.discovery.observe(b.endpoint, "test", "urn:onlyne:agent:b");
	await b.node.discovery.observe(a.endpoint, "test", "urn:onlyne:agent:a");
	await a.node.client.msg({ delivery_kind: "greet b", msg: "hello" });
	const inbox = await b.node.messages.inbox(b.node.auth, { page_size: 10 });
	helloId = inbox.items[0]?.attention.entry?.message.id ?? "";
	expect(helloId).not.toBe("");
	await b.node.client.msg({ delivery_kind: "reply to a", msg: "got it" });
	const reply = (await b.storage.transaction(tx => tx.scan<Outbound>("outbound/")))[0]?.value;
	if (reply === undefined) throw new Error("the reply left no outbound record");
	replyId = reply.request.message.id;
	await a.node.client.msg({ delivery_kind: "answer b", msg: "thanks" });
	const third = (await a.storage.transaction(tx => tx.scan<Outbound>("outbound/")))
		.map(row => row.value).find(row => row.request.message.id !== helloId);
	if (third === undefined) throw new Error("the third hop left no outbound record");
	const bThread = (await b.node.threads.list(b.node.auth, { page_size: 10 })).items[0];
	const aThread = (await a.node.threads.list(a.node.auth, { page_size: 10 })).items[0];
	if (bThread === undefined || aThread === undefined) throw new Error("the exchange minted no thread");
	return {
		a: pluginFor(a, "urn:onlyne:agent:a"),
		b: pluginFor(b, "urn:onlyne:agent:b"),
		helloId,
		replyId,
		thirdId: third.request.message.id,
		aThreadId: aThread.thread_ref.id,
		bThreadId: bThread.thread_ref.id,
	};
}

describe("onlyne:// outbound", () => {
	test("a detail whose landing thread belongs to the peer still reads back", async () => {
		const x = await exchange();
		setRuntime(x.a);
		const detail = JSON.parse(await readView(`onlyne://outbound/${x.helloId}`)) as OutboundDetail;
		expect(detail.landed).toEqual({ thread: `urn:onlyne:agent:b/${x.bThreadId}`, seq: "1", guarantee: "binding-defined" });
		expect(detail.reply).toBeUndefined();
		expect(detail.reply_unavailable).toContain("urn:onlyne:agent:b");
		expect(detail.entry).toBe(`onlyne://threads/${x.bThreadId}/messages/1`);
	});

	test("a detail carries the reference the list shows", async () => {
		const x = await exchange();
		setRuntime(x.b);
		const detail = JSON.parse(await readView(`onlyne://outbound/${x.replyId}`)) as OutboundDetail;
		expect(detail.in_reply_to).toEqual({ sender: "urn:onlyne:agent:a", id: x.helloId });
		expect(detail.entry).toBe(`onlyne://threads/${x.aThreadId}/messages/1`);
	});

	test("a reply that names no thread lands where the original is", async () => {
		const x = await exchange();
		setRuntime(x.a);
		const detail = JSON.parse(await readView(`onlyne://outbound/${x.thirdId}`)) as OutboundDetail;
		expect(detail.landed).toEqual({ thread: `urn:onlyne:agent:b/${x.bThreadId}`, seq: "2", guarantee: "binding-defined" });
		expect(detail.in_reply_to).toEqual({ sender: "urn:onlyne:agent:b", id: x.replyId });
	});
});

describe("onlyne:// threads", () => {
	test("a thread page carries the entry path and the reply target", async () => {
		const x = await exchange();
		setRuntime(x.a);
		const page = await readView(`onlyne://threads/${x.aThreadId}`);
		expect(page).toContain(`onlyne://threads/${x.aThreadId}/messages/1`);
		expect(page).toContain(`→ reply to urn:onlyne:agent:a/${x.helloId}`);
	});
});

describe("onlyne:// routes", () => {
	test("an unknown route points at the root view", async () => {
		const x = await exchange();
		setRuntime(x.a);
		await expect(readView("onlyne://nowhere")).rejects.toThrow(/lists the available views/);
	});
});
