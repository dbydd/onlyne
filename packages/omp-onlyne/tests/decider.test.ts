import { describe, expect, test } from "bun:test";
import type { Entry, Page, Thread, ThreadRef } from "@onlyne/lib";
import type { ExtensionContext } from "@oh-my-pi/pi-coding-agent";
import { JudgeDecider, type DecisionScope, type PeerRecord } from "../src/decider.ts";
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

function decider(records: ContextRecord[] = []): JudgeDecider {
	const contexts = {
		list: async () => records,
	} as unknown as ContextRegistry;
	return new JudgeDecider({
		config,
		mainAgentId: "Main",
		contexts,
		listThreads: async () => [thread],
		listMessages: async (): Promise<Page<Entry>> => ({ items: [entry], next_cursor: null }),
		listPeers: async () => peers,
		listReceiverContexts: async () => [],
	});
}

function scope(meta: Record<string, unknown>, readOnly = false): DecisionScope {
	return {
		ctx: {
			agent: { kind: readOnly ? "sub" : "main", id: readOnly ? "Sub" : "Main", name: "main", depth: 0 },
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
			"may only read",
		);
	});
});
