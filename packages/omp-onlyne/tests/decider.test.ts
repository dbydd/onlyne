import { describe, expect, test } from "bun:test";
import type { Entry, Page, Thread, ThreadRef } from "@onlyne/lib";
import type { ExtensionContext } from "@oh-my-pi/pi-coding-agent";
import { fitState, needsReplyOf, JudgeDecider, knownPeers, reuseThread, type Answers, type DeciderDeps, type DecisionScope, type PeerRecord, type Questions } from "../src/decider.ts";
import type { ContextRegistry } from "../src/store.ts";
import type { ContextRecord, OnlyneConfig } from "../src/types.ts";

const node = "urn:onlyne:agent:a";
const other = "urn:onlyne:agent:b";
const sameSegment = "urn:onlyne:agent:team:b";
const unique = "urn:onlyne:agent:cc";

const config: OnlyneConfig = {
	path: "/tmp/project/.onlyne/config.toml",
	region: "urn:onlyne:region:research-chamber",
	node: { id: node },
	server: { bind: "127.0.0.1" },
	threads: { page_size: 50 },
};

const peers: PeerRecord[] = [
	{ node_id: other, base_url: "http://127.0.0.1:7802", instance_id: "b-1", last_seen: "2026-01-01T00:00:00Z" },
	{ node_id: sameSegment, base_url: "http://127.0.0.1:7803", instance_id: "b-2", last_seen: "2026-01-01T00:00:00Z" },
	{ node_id: unique, base_url: "http://127.0.0.1:7804", instance_id: "cc-1", last_seen: "2026-01-01T00:00:00Z" },
];

const thread: Thread = {
	thread_ref: { owner: node, id: "research" },
	title: "Research",
	state: "open",
	next_seq: "2",
	event_seq: "2",
	created_at: "2026-01-01T00:00:00Z",
	closed_at: null,
};

const entry: Entry = {
	thread_ref: thread.thread_ref,
	seq: "1",
	message: { id: "m-1", sender: other, delivery_kind: "ask", msg: "hello" },
	authenticated_author: other,
	reply_authority: other,
	at: "2026-01-01T00:00:00Z",
};

function record(overrides: Partial<ContextRecord> & { alias: string; thread_ref?: ThreadRef }): ContextRecord {
	return {
		node_id: node,
		main_agent_id: "Main",
		summary: "review the data",
		state: "idle",
		created_at: "2026-01-01T00:00:00Z",
		...overrides,
	};
}

interface Stubs {
	threads?: Thread[];
	trust?: Array<{ node_id: string; phase: string }>;
	members?: Record<string, string[]>;
	peers?: PeerRecord[];
	inbox?: Array<{ context?: string; attention: { entry?: Entry; disposition?: string } }>;
	judge?: (state: Record<string, unknown>, questions: Questions) => Promise<Answers>;
}

function decider(records: ContextRecord[] = [], stubs: Stubs = {}): JudgeDecider {
	const contexts = {
		list: async () => records,
	} as unknown as ContextRegistry;
	const members = stubs.members ?? { [thread.thread_ref.id]: [node, other] };
	return new JudgeDecider({
		config,
		mainAgentId: "Main",
		contexts,
		listThreads: async () => stubs.threads ?? [thread],
		listJoinedThreads: async () => [],
		listMessages: async (): Promise<Page<Entry>> => ({ items: [entry], next_cursor: null }),
		listPeers: async () => stubs.peers ?? peers,
		listTrust: async () => stubs.trust ?? [],
		threadMembers: async ref => members[ref.id] ?? [],
		listReceiverContexts: async () => [],
		listInbox: async () => stubs.inbox ?? [],
		...(stubs.judge === undefined ? {} : { judge: stubs.judge }),
	});
}

/** A judge stub that answers every question key from one defaults table. */
function judged(defaults: Record<string, string>): NonNullable<Stubs["judge"]> {
	return async (_state, questions) => Object.fromEntries(Object.keys(questions).map(key => [key, { choice: defaults[key] }])); 
}

function scope(meta: Record<string, unknown>, readOnly = false): DecisionScope {
	return {
		ctx: {
			agent: { kind: readOnly ? "sub" : "main", id: readOnly ? "Sub" : "Main", name: "main", depth: 0 },
			sessionManager: { getBranch: () => [], getSessionId: () => "test-session" },
			isIdle: () => true,
			hasPendingMessages: () => false,
		} as unknown as ExtensionContext,
		input: { delivery_kind: "explicit", msg: "body", meta: meta as never },
		...(readOnly ? { readOnly: true } : {}),
	};
}

describe("explicit actions", () => {
	test("thread create and list do not need the judge", async () => {
		expect(await decider().decide(scope({ op: "create", title: "Research" }))).toEqual({
			kind: "thread",
			op: "create",
			title: "Research",
		});
		expect(await decider().decide(scope({ op: "list" }))).toEqual({ kind: "thread", op: "list" });
	});

	test("read scope routes without a semantic shortcut", async () => {
		expect(await decider().decide(scope({ scope: "directory" }))).toEqual({ kind: "read", scope: "directory" });
		// RFC 0005 §1: a read that names no attention mode reads the attention list.
		expect(await decider().decide(scope({ scope: "messages", thread: "research" }))).toEqual({
			kind: "read",
			scope: "messages",
			thread: thread.thread_ref,
			attention: "inbox",
		});
		expect(await decider().decide(scope({ scope: "message", thread: "research", seq: "1" }))).toEqual({
			kind: "read",
			scope: "message",
			thread: thread.thread_ref,
			message: { sender: other, id: "m-1" },
		});
	});

	test("resource operation needs an explicit owner and operation", async () => {
		expect(
			await decider().decide(
				scope({ resource: { owner: other, id: "im-1" }, operation: "send" }),
			),
		).toEqual({
			kind: "resource",
			resource: { owner: other, id: "im-1" },
			operation: "send",
		});
	});

	test("a missing explicit target is reported, not invented", async () => {
		await expect(decider().decide(scope({ op: "member_add", thread: "research" }))).rejects.toThrow(
			"needs a target",
		);
	});
});

describe("dynamic peer targets", () => {
	test("a unique last URN segment resolves to the absolute node id", async () => {
		expect(await decider().decide(scope({ op: "member_add", thread: "research", to: "cc" }))).toEqual({
			kind: "thread",
			op: "member_add",
			thread: thread.thread_ref,
			to: unique,
		});
	});

	test("an ambiguous short alias is rejected", async () => {
		await expect(decider().decide(scope({ op: "member_add", thread: "research", to: "b" }))).rejects.toThrow(
			"names more than one node",
		);
	});

	test("an undiscovered target is rejected", async () => {
		await expect(
			decider().decide(scope({ op: "member_add", thread: "research", to: "nobody" })),
		).rejects.toThrow("No discovered node is named nobody");
	});
});

describe("subagent slices", () => {
	test("a read of an authorized thread is allowed", async () => {
		const des = decider([record({ alias: "c-1", agent_id: "Sub", thread_ref: thread.thread_ref })]);
		expect(
			await des.decide(scope({ scope: "messages", thread: "research", attention: "thread" }, true)),
		).toEqual({
			kind: "read",
			scope: "messages",
			thread: thread.thread_ref,
			attention: "thread",
		});
	});

	test("a read outside the subagent slice is forbidden", async () => {
		const des = decider([
			record({ alias: "c-2", agent_id: "Sub", thread_ref: { owner: node, id: "elsewhere" } }),
		]);
		await expect(des.decide(scope({ scope: "messages", thread: "research" }, true))).rejects.toThrow(
			"holds no read slice",
		);
	});

	test("a subagent cannot change threads", async () => {
		await expect(decider().decide(scope({ op: "create", title: "Research" }, true))).rejects.toThrow(
			"A subagent session reads Onlyne state, it does not write it",
		);
	});
});

describe("judge state budget", () => {
	const request = { delivery_kind: "ask", msg: "review this", meta: { to: other } };

	/** The Clef backend serializes with sort_keys=True, so key order decides what a cut keeps. */
	const sortedJson = (value: unknown): string => {
		if (Array.isArray(value)) return `[${value.map(sortedJson).join(",")}]`;
		if (value === null || typeof value !== "object") return JSON.stringify(value);
		const entries = Object.entries(value as Record<string, unknown>).sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0);
		return `{${entries.map(([name, item]) => `${JSON.stringify(name)}:${sortedJson(item)}`).join(",")}}`;
	};

	test("the brief survives a cut that the backend applies to sorted state", () => {
		const history = Array.from({ length: 200 }, (_, index) => ({ role: "tool", text: "x".repeat(500), index }));
		const brief = { node, host: { idle: true }, request };
		const serialized = sortedJson(fitState({ brief, history, request }).fitted);
		expect(serialized.indexOf('"brief"')).toBeLessThan(serialized.indexOf('"history"'));
		// 16384 tokens is the backend cap; a conservative 3 characters per token still keeps the request.
		expect(serialized.slice(0, 16384 * 3)).toContain(request.msg);
	});

	test("a long history is dropped before the current request", () => {
		const history = Array.from({ length: 200 }, (_, index) => ({ role: "tool", text: "x".repeat(500), index }));
		const { fitted, dropped, chars } = fitState({ history, request, node });
		expect(chars).toBeLessThanOrEqual(40_000);
		expect(dropped).toContain("history");
		expect(fitted.request).toEqual(request);
		expect((fitted.history as unknown[]).length).toBeLessThan(history.length);
		// The rows kept are the newest ones; the front of the array is the oldest evidence.
		expect((fitted.history as Array<{ index: number }>).at(-1)?.index).toBe(199);
	});

	test("a state that still does not fit is reported, not truncated", () => {
		const { chars } = fitState({ request: { msg: "y".repeat(50_000) }, history: [] });
		expect(chars).toBeGreaterThan(40_000);
	});

	test("a state inside the budget is passed through", () => {
		const { fitted, dropped } = fitState({ history: [], request, node });
		expect(dropped).toEqual([]);
		expect(fitted).toEqual({ history: [], request, node });
	});
});

describe("reply references from the field reports", () => {
	// cyanea report 1, problem 1: a raw authority string compared against a resolved id read as
	// two contradictory errors. After the fix both spellings name the same node, so the failure
	// shape changes from "wrong target" to a judge-unavailable error from the sandboxed test.
	test("a short-name sender is not reported as a conflicting authority", async () => {
		const des = decider();
		const messages: string[] = [];
		for (const sender of [other, "b"]) {
			messages.push(await des.decide(scope({ to: other, in_reply_to: { sender, id: "m-1" }, relation: "reply" }))
				.then(() => "", (error: Error) => error.message));
		}
		expect(messages[0]).not.toContain("original reply authority");
		expect(messages[1]).not.toContain("original reply authority");
		expect(messages[0]).not.toContain("belongs to its original");
		expect(messages[1]).not.toContain("belongs to its original");
	});

	// caerulea reply authority: the ref and the authority both carry the resolved id, so a
	// short name on one side never reads as a second target. `cc` is the only unambiguous
	// short alias in the fixture; `b` is shared by two nodes and stays ambiguous by design.
	test("a short-name reference resolves to the same identity as the target", async () => {
		const des = decider([], { judge: judged({ intent: "communicate" }) });
		const action = await des.decide(scope({ to: unique, in_reply_to: { sender: "cc", id: "m-1" }, relation: "reply", response: "async", execution: "new" }));
		expect(action).toMatchObject({ kind: "send", to: unique, in_reply_to: { sender: unique, id: "m-1" } });
	});

	// decider-auto-in-reply-to, case 2: a reference the node can prove wrong is refused before
	// the wire, with the sender the id really belongs to.
	test("an id that belongs to another sender is refused with the actual owner", async () => {
		const des = decider([], {
			inbox: [{ attention: { entry: { ...entry, message: { id: "m-9", sender: other, delivery_kind: "ask", msg: "hi" } } } }],
			judge: judged({ intent: "communicate" }),
		});
		await expect(des.decide(scope({ to: unique, in_reply_to: { sender: unique, id: "m-9" }, relation: "reply", response: "async", execution: "new" })))
			.rejects.toThrow("belongs to");
	}
	);
});

describe("O-2A original selection", () => {
	const inbound: Entry = { ...entry, message: { id: "m-9", sender: other, delivery_kind: "ask", msg: "请确认第二组参数。" } };

	test("the judge picks the original and the plugin copies its exact ref", async () => {
		const asked: string[][] = [];
		const des = decider([], {
			inbox: [{ attention: { entry: inbound } }],
			judge: async (state, questions) => {
				asked.push(Object.keys(questions));
				return judged({ intent: "communicate", original: "answer_1", relation: "reply" })(state, questions);
			},
		});
		const action = await des.decide(scope({ to: other, response: "async", execution: "new" }));
		expect(asked.some(keys => keys.includes("original"))).toBe(true);
		expect(action).toMatchObject({ kind: "send", to: other, in_reply_to: { sender: other, id: "m-9" }, relation: "reply" });
	});

	test("new_message proceeds without a reference", async () => {
		const des = decider([], {
			inbox: [{ attention: { entry: inbound } }],
			judge: judged({ intent: "communicate", original: "new_message" }),
		});
		const action = await des.decide(scope({ to: other, response: "async", execution: "new" }));
		expect(action).toMatchObject({ kind: "send", to: other, response: "async", execution: "new" });
		expect((action as { in_reply_to?: unknown }).in_reply_to).toBeUndefined();
	});

	test("an empty candidate set asks nothing and sends as new", async () => {
		const asked: string[][] = [];
		const des = decider([], {
			judge: async (state, questions) => {
				asked.push(Object.keys(questions));
				return judged({ intent: "communicate" })(state, questions);
			},
		});
		await des.decide(scope({ to: other, response: "async", execution: "new" }));
		expect(asked.some(keys => keys.includes("original"))).toBe(false);
	});

	test("a record delivered to another context is not this call's candidate", async () => {
		const asked: string[][] = [];
		const des = decider([{ ...record({ alias: "ctx-1", agent_id: "Other" }) }], {
			inbox: [{ context: "ctx-1", attention: { entry: inbound } }],
			judge: async (state, questions) => {
				asked.push(Object.keys(questions));
				return judged({ intent: "communicate" })(state, questions);
			},
		});
		await des.decide(scope({ to: other, response: "async", execution: "new" }));
		expect(asked.some(keys => keys.includes("original"))).toBe(false);
	});
});

describe("hierarchical decision nodes", () => {
	/** A message whose body greets and asks, so the reply heuristic applies. */
	const inbound: Entry = {
		...entry,
		message: { id: "m-9", sender: other, delivery_kind: "ask", msg: "你好，在吗？请确认进度。" },
	};

	test("needs_reply is derived from the body heuristic, not only from meta", () => {
		expect(needsReplyOf(inbound)).toBe(true);
		expect(needsReplyOf(entry)).toBe(false);
	});
});

describe("thread reuse", () => {
	const older: Thread = { ...thread, thread_ref: { owner: node, id: "older" }, created_at: "2025-12-01T00:00:00Z" };
	const closed: Thread = { ...thread, thread_ref: { owner: node, id: "closed" }, state: "closed", created_at: "2026-03-01T00:00:00Z" };
	const foreign: Thread = { ...thread, thread_ref: { owner: other, id: "theirs" }, created_at: "2026-04-01T00:00:00Z" };

	function reuse(threads: Thread[], members: Record<string, string[]>) {
		const deps = {
			config, log: () => {},
			listThreads: async () => threads,
			threadMembers: async (ref: ThreadRef) => members[ref.id] ?? [],
		} as unknown as DeciderDeps;
		return reuseThread(deps, other);
	}

	test("continues the newest open thread the peer already belongs to", async () => {
		const members = { research: [node, other], older: [node, other], closed: [node, other], theirs: [node, other] };
		expect(await reuse([older, closed, foreign, thread], members)).toEqual(thread.thread_ref);
	});

	test("falls back to an older thread when the newest one has another member", async () => {
		expect(await reuse([older, thread], { older: [node, other], research: [node, unique] })).toEqual(older.thread_ref);
	});

	test("opens nothing when no open thread of this node lists that peer", async () => {
		expect(await reuse([older, thread], { older: [node], research: [node] })).toBeUndefined();
	});
});

describe("identity outlives liveness", () => {
	test("a trust row without a live discovery record is both addressable and a candidate", async () => {
		const deps = {
			config, log: () => {},
			contexts: { list: async () => [] } as unknown as ContextRegistry,
			listThreads: async () => [thread],
			listJoinedThreads: async () => [],
			listMessages: async (): Promise<Page<Entry>> => ({ items: [entry], next_cursor: null }),
			listPeers: async () => [],
			listTrust: async () => [{ node_id: other, phase: "exchanged", base_url: "http://127.0.0.1:7802/onlyne/v3" }],
			threadMembers: async () => [node, other],
			listReceiverContexts: async () => [],
		listInbox: async () => [],
		} as unknown as DeciderDeps;
		// Both questions read the same source, so the peer cannot resolve for one and vanish
		// from the other (audit §2.2).
		expect((await knownPeers(deps)).map(peer => peer.node_id)).toEqual([other]);
		expect(await new JudgeDecider(deps).decide(scope({ op: "member_add", thread: "research", to: other })))
			.toMatchObject({ to: other });
	});

	test("a peer with a trust row is addressable without a live discovery record", async () => {
		const des = decider([], { peers: [], trust: [{ node_id: other, phase: "exchanged" }] });
		expect(await des.decide(scope({ op: "member_add", thread: "research", to: other }))).toEqual({
			kind: "thread",
			op: "member_add",
			thread: thread.thread_ref,
			to: other,
		});
	});

	test("a revoked peer is not addressable", async () => {
		await expect(
			decider([], { peers: [], trust: [{ node_id: other, phase: "revoked" }] })
				.decide(scope({ op: "member_add", thread: "research", to: other })),
		).rejects.toThrow();
	});
});
