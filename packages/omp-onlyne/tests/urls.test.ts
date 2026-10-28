import { afterEach, describe, expect, test } from "bun:test";
import { MemoryStorage } from "@onlyne/lib";
import type { Action, OnlyneNode, Outbound } from "@onlyne/lib";
import { InternalUrlRouter } from "@oh-my-pi/pi-coding-agent/internal-urls";
import type { ResolveContext } from "@oh-my-pi/pi-coding-agent/internal-urls";
import { createOnlyneNode } from "../src/node.ts";
import { TRUST_THREAD } from "../src/handshake.ts";
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
	conversation?: string;
	at?: string | null;
	status?: string;
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
}

/**
 * a greets b, b replies, a answers: three hops over two nodes, with no thread named by hand.
 * The greeting opens a thread a owns, so b learns it from the grant and pulls the entry.
 */
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
	await b.node.client.reconcile();
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
	const aThread = (await a.node.threads.list(a.node.auth, { page_size: 10 })).items[0];
	if (aThread === undefined) throw new Error("the exchange minted no thread");
	return {
		a: pluginFor(a, "urn:onlyne:agent:a"),
		b: pluginFor(b, "urn:onlyne:agent:b"),
		helloId,
		replyId,
		thirdId: third.request.message.id,
		aThreadId: aThread.thread_ref.id,
	};
}

describe("onlyne:// outbound", () => {
	test("a detail whose landing thread belongs to the peer still reads back", async () => {
		const x = await exchange();
		setRuntime(x.a);
		const detail = JSON.parse(await readView(`onlyne://outbound/${x.helloId}`)) as OutboundDetail;
		expect(detail.landed).toEqual({ thread: `urn:onlyne:agent:a/${x.aThreadId}`, seq: "1", guarantee: "binding-defined" });
		// The reply is read from this node's own history, not from the peer's thread.
		expect(detail.reply?.by).toBe("urn:onlyne:agent:b");
		expect(detail.reply?.body).toBe("got it");
		expect(detail.entry).toBe(`onlyne://messages/${encodeURIComponent("urn:onlyne:agent:a")}/${x.helloId}`);
		expect(detail.conversation).toBe(`onlyne://conversations/${encodeURIComponent("urn:onlyne:agent:a")}/${x.helloId}`);
	});

	test("a detail carries the reference the list shows", async () => {
		const x = await exchange();
		setRuntime(x.b);
		const detail = JSON.parse(await readView(`onlyne://outbound/${x.replyId}`)) as OutboundDetail;
		expect(detail.in_reply_to).toEqual({ sender: "urn:onlyne:agent:a", id: x.helloId });
		expect(detail.entry).toBe(`onlyne://messages/${encodeURIComponent("urn:onlyne:agent:b")}/${x.replyId}`);
		expect(detail.status).toBe("accepted");
	});

	test("a reply that names no thread lands where the original is", async () => {
		const x = await exchange();
		setRuntime(x.a);
		const detail = JSON.parse(await readView(`onlyne://outbound/${x.thirdId}`)) as OutboundDetail;
		// All three hops share the thread a started: the greeting, b's reply, and a's answer.
		expect(detail.landed).toEqual({ thread: `urn:onlyne:agent:a/${x.aThreadId}`, seq: "3", guarantee: "binding-defined" });
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

	test("a member lists the thread it was invited into, marked read-only", async () => {
		const x = await exchange();
		setRuntime(x.b);
		const list = await readView("onlyne://threads");
		expect(list).toContain("read-only");
		expect(list).toContain(`urn:onlyne:agent:a/${x.aThreadId}`);
	});

	test("a member reads a joined thread by its id from the owner", async () => {
		const x = await exchange();
		setRuntime(x.b);
		const page = await readView(`onlyne://threads/${x.aThreadId}`);
		// The owner holds the thread: its state and members come from the describe read.
		expect(page).toContain("Read-only");
		expect(page).toContain("hello");
		expect(page).toContain("urn:onlyne:agent:b");
	});

	test("the handshake thread stays out of the listing and opens by reference", async () => {
		const x = await exchange();
		setRuntime(x.a);
		// Written the way the runtime writes it: this node's own thread, one entry, self-mentioned.
		await x.a.node.threads.create(x.a.node.auth, {
			title: TRUST_THREAD, id: TRUST_THREAD, idempotency_key: crypto.randomUUID(),
			retry_until: new Date(Date.now() + 60_000).toISOString(),
		});
		const sent = await x.a.node.client.send("urn:onlyne:agent:a", {
			id: "trust-1", sender: "urn:onlyne:agent:a", delivery_kind: "trust", msg: "握手邀请：请核对指纹。",
			meta: {
				thread: { owner: "urn:onlyne:agent:a", id: TRUST_THREAD }, mentions: ["urn:onlyne:agent:a"],
				data: { trust: { node_id: "urn:onlyne:agent:b", fingerprint: "0123456789abcdef", phase: "invited", pending: true } },
			},
		}, { response: "none", execution: "existing" });
		expect(sent.admission).toBe("accepted");

		// The inbox row an agent sees names the entry it can open, not a separate notice id.
		const rows = (await x.a.node.messages.inbox(x.a.node.auth, { page_size: 10 })).items;
		// A reply by the id a row shows has to resolve, so the row and its entry name one message.
		expect(rows.every(item => item.attention.message_ref.id === item.attention.entry?.message.id)).toBe(true);
		const row = rows
			.find(item => item.attention.thread_ref.id === TRUST_THREAD);
		expect(row?.attention.message_ref.id).toBe("trust-1");
		expect(row?.attention.entry?.message.id).toBe(row?.attention.message_ref.id);

		// Every reference the inbox row offers resolves: id alone, owner/id, and the entry path.
		for (const alias of [TRUST_THREAD, `urn%3Aonlyne%3Aagent%3Aa/${TRUST_THREAD}`, "urn:onlyne:agent:a/trust"]) {
			expect(await readView(`onlyne://threads/${alias}`)).toContain("握手邀请");
		}
		expect(await readView(`onlyne://threads/${TRUST_THREAD}/messages/1`)).toContain("握手邀请");
		// The conversation listings still leave it out: it is this node's record, not a conversation.
		expect(await readView("onlyne://threads")).not.toContain("/trust");
	});
});

describe("onlyne:// routes", () => {
	test("an unknown route points at the root view", async () => {
		const x = await exchange();
		setRuntime(x.a);
		await expect(readView("onlyne://nowhere")).rejects.toThrow(/lists the available views/);
	});

	test("onlyne://resources lists owner inventories and describe reads JSON", async () => {
		const x = await exchange();
		setRuntime(x.a);
		await x.a.node.resources.register(x.a.node.auth, {
			resource_ref: { owner: "urn:onlyne:agent:a", id: "camera" },
			kind: "device",
			operations: ["capture"],
			binding: { handler: "urn:onlyne:agent:a", cursor: "0" },
			log_readers: [],
			revision: "rev-1",
			event_seq: "0",
			retention_ms: 10_000,
		});
		const root = await readView("onlyne://");
		expect(root).toContain("onlyne://resources");
		const list = await readView("onlyne://resources");
		expect(list).toContain("urn:onlyne:agent:a");
		expect(list).toContain("camera");
		const item = JSON.parse(await readView("onlyne://resources/urn%3Aonlyne%3Aagent%3Aa/camera")) as { kind?: string };
		expect(item.kind).toBe("device");
	});

	test("onlyne://resources denies subagents", async () => {
		const x = await exchange();
		setRuntime(x.a);
		registerOnlyneUrls();
		const subcaller = { session: { getAgentId: () => "Subagent" } } as unknown as ResolveContext;
		await expect(InternalUrlRouter.instance().resolve("onlyne://resources", subcaller)).rejects.toThrow(/main OMP session/);
	});
});
