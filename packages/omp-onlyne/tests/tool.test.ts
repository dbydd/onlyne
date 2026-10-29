/**
 * The two mechanical ops the `msg` tool gained.
 *
 * RFC 0018 §2/§3.1: `trust_begin` is judged against the acquisition policy before its first
 * outbound request, so a refusal sends nothing. RFC 0018 §3.1 and the thread action the decider
 * already produced: `member_add` puts a peer in a thread, and an array send that names no thread
 * gives each target its own instead of folding a private message into a group.
 *
 * A live session's judge picks the plan for a plain send; the stub below supplies that plan, so
 * the thread a send lands in is the only thing under test.
 */
import { describe, expect, test } from "bun:test";
import {
	MemoryStorage, OnlyneError, OnlyneNode, trustPhase,
	type Action, type DiscoveryRecord, type KeyExchange, type Mutation, type Plugins, type ThreadRef, type Transport, type TrustEntry,
} from "@onlyne/lib";
import type { ExtensionAPI, ExtensionContext } from "@oh-my-pi/pi-coding-agent";
import { DEFAULT_ACQUISITION, type AcquisitionPolicy } from "../src/acquire.ts";
import { currentDecision, JudgeDecider, reuseThread, type DeciderDeps, type PeerRecord } from "../src/decider.ts";
import { AcquisitionLog, beginPeer, type ApprovalDeps, type ApprovalJudgement } from "../src/handshake.ts";
import { outboundObserved, setRuntime, type PluginRuntime } from "../src/runtime.ts";
import type { ContextRegistry } from "../src/store.ts";
import { registerMsgTool } from "../src/tool.ts";
import type { OnlyneConfig } from "../src/types.ts";

const NODE = "urn:onlyne:agent:mine";
const ALPHA = "urn:onlyne:agent:alpha";
const BETA = "urn:onlyne:agent:beta";
const LOOPBACK = "http://127.0.0.1:7101/onlyne/v3";
const SEEN = "2026-10-27T00:00:00.000Z";
const AT = "2026-10-27T00:01:00.000Z";
const FINGERPRINT = "0123456789abcdef";

const config: OnlyneConfig = {
	path: "/tmp/project/.onlyne/config.toml",
	region: "urn:onlyne:region:research-chamber",
	node: { id: NODE },
	server: { bind: "127.0.0.1" },
	threads: { page_size: 50 },
};

function mutation(): Mutation {
	return { idempotency_key: crypto.randomUUID(), retry_until: new Date(Date.now() + 60_000).toISOString() };
}

function context(kind: "main" | "sub"): ExtensionContext {
	return {
		agent: { kind, id: kind === "main" ? "Main" : "Sub", name: "main", depth: 0 },
		sessionManager: { getBranch: () => [], getSessionId: () => "test-session" },
		isIdle: () => true,
		hasPendingMessages: () => false,
		cwd: "/tmp/project",
	} as unknown as ExtensionContext;
}

interface ToolResult {
	content: Array<{ type: string; text: string }>;
	details?: { result?: unknown };
	isError?: boolean;
}

type Execute = (toolCallId: string, raw: unknown, signal: undefined, update: undefined, ctx: ExtensionContext) => Promise<ToolResult>;

/**
 * One registered `msg` tool over a stubbed host. Each call binds a fresh tool definition, so a
 * test's runtime is the one `getRuntime()` answers with.
 */
function harness(host: Partial<PluginRuntime>): { call(meta: Record<string, unknown>, kind?: "main" | "sub"): Promise<ToolResult> } {
	setRuntime(host as unknown as PluginRuntime); // a stub host: the tool reads only the members it uses
	let execute: Execute | undefined;
	const pi = {
		registerTool: (definition: { name: string; execute: Execute }) => {
			if (definition.name === "msg") execute = definition.execute;
		},
		appendEntry: () => {},
		logger: { info: () => {}, warn: () => {}, debug: () => {} },
	} as unknown as ExtensionAPI;
	registerMsgTool(pi);
	return {
		async call(meta, kind = "main") {
			if (execute === undefined) throw new Error("the msg tool did not register");
			return execute("call-1", { delivery_kind: "test the tool", msg: "body", meta }, undefined, undefined, context(kind));
		},
	};
}

interface TrustRig {
	deps: ApprovalDeps;
	exchange: (nodeId: string) => Promise<KeyExchange>;
	calls: string[];
	audited: ApprovalJudgement[];
}

/** The host side of one initiation: the judgement reads these facts, then the exchange records itself. */
function trustRig(policy: AcquisitionPolicy, facts: { declared?: string; recorded?: string[] }): TrustRig {
	const calls: string[] = [];
	const audited: ApprovalJudgement[] = [];
	const entry: TrustEntry = {
		node_id: ALPHA, public_key: "", fingerprint: FINGERPRINT, state: "pending",
		first_seen: AT, last_checked: AT, local_approved: true, remote_approved: false, base_url: LOOPBACK,
	};
	const session: KeyExchange = {
		node_id: ALPHA, fingerprint: FINGERPRINT, public_key: "", base_url: LOOPBACK,
		confirm: async () => {
			calls.push("confirm");
			return entry;
		},
	};
	return {
		calls,
		audited,
		deps: {
			policy,
			log: new AcquisitionLog(new MemoryStorage()),
			observed: async () => undefined,
			declared: async () => facts.declared,
			recorded: async () => facts.recorded ?? [],
			audit: judgement => audited.push(judgement),
			accept: async () => {
				calls.push("accept");
				return entry;
			},
			revoke: async () => { calls.push("revoke"); },
			now: () => AT,
		},
		exchange: async () => {
			calls.push("begin");
			return session;
		},
	};
}

/** A decider over a fixed peer set; only the deterministic resolution is used here. */
function peerDecider(peers: PeerRecord[]): JudgeDecider {
	return new JudgeDecider({
		config,
		mainAgentId: "Main",
		contexts: { list: async () => [] } as unknown as ContextRegistry,
		listThreads: async () => [],
		listJoinedThreads: async () => [],
		listMessages: async () => ({ items: [], next_cursor: null }),
		threadMembers: async () => [],
		listPeers: async () => peers,
		listTrust: async () => [],
		listReceiverContexts: async () => [],
		listInbox: async () => [],
	});
}

const peers: PeerRecord[] = [
	{ node_id: ALPHA, base_url: LOOPBACK, instance_id: "alpha-1", last_seen: SEEN },
	{ node_id: BETA, base_url: "http://127.0.0.1:7102/onlyne/v3", instance_id: "beta-1", last_seen: SEEN },
];

/** The tool's whole trust path over the rig: resolve, judge, exchange. */
function trustHost(rig: TrustRig, peerSet: PeerRecord[] = peers): Partial<PluginRuntime> {
	return {
		decider: peerDecider(peerSet),
		acquireBegin: async node => (await beginPeer(rig.deps, rig.exchange, { node_id: node, observed: LOOPBACK })).entry,
	};
}

function record(value: unknown): ThreadRef | undefined {
	if (value === null || typeof value !== "object" || Array.isArray(value)) return undefined;
	const owner = "owner" in value ? value.owner : undefined;
	const id = "id" in value ? value.id : undefined;
	return typeof owner === "string" && typeof id === "string" ? { owner, id } : undefined;
}

interface Rig {
	node: OnlyneNode;
	decider: JudgeDecider;
	grants: string[];
}

/** A real node over memory storage: threads, memberships and local admissions all commit for real. */
function rig(): Rig {
	const storage = new MemoryStorage();
	let node!: OnlyneNode;
	const deps: DeciderDeps = {
		config,
		mainAgentId: "Main",
		contexts: { list: async () => [] } as unknown as ContextRegistry,
		listThreads: async () => (await node.threads.list(node.auth, { page_size: 100 })).items,
		listJoinedThreads: async () => [],
		listMessages: async () => ({ items: [], next_cursor: null }),
		threadMembers: async ref => (await node.threads.describe(node.auth, ref)).members.map(member => member.peer),
		listPeers: async () => peers,
		listTrust: async () => [],
		listReceiverContexts: async () => [],
		listInbox: async () => [],
	};
	const decider = new JudgeDecider(deps);
	const grants: string[] = [];
	const transport = {
		request: async (target: string, _method: string, path: string) => {
			grants.push(`${target} ${path}`);
			return {};
		},
	} as unknown as Transport; // the grant notice is the only thing this node sends; it is recorded, not delivered
	const plugins: Plugins = {
		storage,
		transport,
		resolve: async () => {
			throw new OnlyneError("not_found", "this test node reaches no peer directly");
		},
		authenticate: async () => ({ actor: NODE }),
		decide: async (input): Promise<Action> => {
			const op = input.meta?.op;
			if (op === "member_add" || op === "member_remove") {
				const scope = currentDecision();
				if (scope === undefined) throw new OnlyneError("decision_unavailable", "no session is bound to this call");
				scope.input = input;
				return decider.decide(scope);
			}
			const to = await decider.peerId(String(input.meta?.to));
			const named = record(input.meta?.thread);
			const thread = named ?? await reuseThread(deps, to);
			return { kind: "send", to, response: "async", execution: "existing", ...(thread === undefined ? {} : { thread }) };
		},
	};
	node = new OnlyneNode({
		node_id: NODE, base_url: "http://127.0.0.1:7100/onlyne/v3", plugins,
		now: () => Date.now(), id: () => crypto.randomUUID(), wait_ms: 1_000,
	});
	return { node, decider, grants };
}

describe("trust_begin", () => {
	test("the outbound judgement reads an address this host observed, not the caller's", () => {
		const discovered = (base_url: string, verified: boolean): DiscoveryRecord => ({
			node_id: ALPHA, instance_id: "alpha-1", base_url, source: "mdns",
			last_seen: SEEN, received_at: SEEN, expires_at: SEEN, ...(verified ? { verified_at: SEEN } : {}),
		});
		// A statement this host received is the strongest address it holds, over any probe since.
		expect(outboundObserved({ statement: "127.0.0.1", records: [discovered(LOOPBACK, true)], declared: LOOPBACK })).toBe("127.0.0.1");
		// A discovered peer that never sent one is judged where discovery verified it answering.
		expect(outboundObserved({ records: [discovered("https://unverified.example", false), discovered(LOOPBACK, true)] })).toBe(LOOPBACK);
		// No record at all: the address the library's own fallback would connect to.
		expect(outboundObserved({ records: [], declared: LOOPBACK })).toBe(LOOPBACK);
		expect(outboundObserved({ records: [] })).toBeUndefined();
	});

	test("policy none refuses it with the matched rule and sends nothing", async () => {
		const rig = trustRig({ policy: "none", allow: [] }, { declared: LOOPBACK });
		const result = await harness(trustHost(rig)).call({ op: "trust_begin", node_id: "alpha" });
		expect(result.isError).toBe(true);
		expect(result.content[0]?.text).toContain("policy_denied_none");
		// The judgement ran before the exchange: no key read, no statement, no committed row.
		expect(rig.calls).toEqual([]);
		expect(rig.audited).toHaveLength(1);
		expect(rig.audited[0]?.verdict.reason).toBe("policy_denied_none");
	});

	test("segment allows a loopback peer and reports the phase that followed", async () => {
		const rig = trustRig(DEFAULT_ACQUISITION, { declared: LOOPBACK, recorded: [LOOPBACK] });
		const result = await harness(trustHost(rig)).call({ op: "trust_begin", node_id: "alpha" });
		expect(result.isError).toBeUndefined();
		expect(rig.calls).toEqual(["begin", "confirm"]);
		const entry = result.details?.result as TrustEntry; // the tool returns the row the confirmation committed
		expect(entry.node_id).toBe(ALPHA);
		expect(trustPhase(entry)).toBe("requested");
		expect(result.content[0]?.text).toContain("requested");
	});

	test("a subagent cannot begin an exchange", async () => {
		const rig = trustRig(DEFAULT_ACQUISITION, { declared: LOOPBACK });
		const result = await harness(trustHost(rig)).call({ op: "trust_begin", node_id: "alpha" }, "sub");
		expect(result.isError).toBe(true);
		expect(result.content[0]?.text).toContain("main session");
		expect(rig.calls).toEqual([]);
	});

	test("an unknown alias is not_found and names the action that asked", async () => {
		const rig = trustRig(DEFAULT_ACQUISITION, { declared: LOOPBACK });
		const result = await harness(trustHost(rig)).call({ op: "trust_begin", node_id: "ghost" });
		expect(result.isError).toBe(true);
		expect(result.content[0]?.text).toContain("trust_begin");
		expect(result.content[0]?.text).toContain("ghost");
		expect(rig.calls).toEqual([]);
	});

	test("an ambiguous alias names both matches", async () => {
		const rig = trustRig(DEFAULT_ACQUISITION, { declared: LOOPBACK });
		const ambiguous: PeerRecord[] = [
			{ node_id: "urn:onlyne:agent:team:b", base_url: LOOPBACK, instance_id: "b-1", last_seen: SEEN },
			{ node_id: "urn:onlyne:agent:branch:b", base_url: LOOPBACK, instance_id: "b-2", last_seen: SEEN },
		];
		const result = await harness(trustHost(rig, ambiguous)).call({ op: "trust_begin", node_id: "b" });
		expect(result.isError).toBe(true);
		expect(result.content[0]?.text).toContain("urn:onlyne:agent:team:b");
		expect(result.content[0]?.text).toContain("urn:onlyne:agent:branch:b");
		expect(rig.calls).toEqual([]);
	});
});

describe("thread membership through the tool", () => {
	test("member_add puts a peer in the thread, and an array send with meta.thread lands both there", async () => {
		const { node, decider, grants } = rig();
		const thread = await node.threads.create(node.auth, { title: "plan", id: "plan", ...mutation() });
		const tool = harness({ node, decider });

		const added = await tool.call({ op: "member_add", thread: "plan", to: "alpha" });
		expect(added.isError).toBeUndefined();
		expect(added.content[0]?.text).toContain(ALPHA);

		const sent = await tool.call({ to: [ALPHA, BETA], thread: thread.thread_ref });
		expect(sent.isError).toBeUndefined();
		const rows = sent.details?.result as Array<{ error?: string }>; // one row per target
		expect(rows.map(row => row.error)).toEqual([undefined, undefined]);

		const threads = (await node.threads.list(node.auth, { page_size: 100 })).items;
		expect(threads.map(thread => thread.thread_ref.id).sort()).toEqual(["plan"]);
		const entries = (await node.messages.list(node.auth, thread.thread_ref, { attention: "thread", page_size: 100 })).items;
		expect(entries.map(entry => entry.message.sender)).toEqual([NODE, NODE]);
		// The owner holds the thread rather than a member row: the two peers it added are its members.
		const members = (await node.threads.describe(node.auth, thread.thread_ref)).members.map(member => member.peer);
		expect(members.sort()).toEqual([ALPHA, BETA].sort());
		// The membership notice each target needed to learn the thread went out with its send.
		expect(grants).toEqual([`${ALPHA} /grants`, `${BETA} /grants`]);
	});

	test("the same array send without meta.thread gives each target its own thread", async () => {
		const { node, decider } = rig();
		await node.threads.create(node.auth, { title: "plan", id: "plan", ...mutation() });
		const tool = harness({ node, decider });
		const members = await tool.call({ op: "member_add", thread: "plan", to: [ALPHA, BETA] });
		expect(members.isError).toBeUndefined();

		const sent = await tool.call({ to: [ALPHA, BETA] });
		expect(sent.isError).toBeUndefined();
		const rows = sent.details?.result as Array<{ error?: string }>; // one row per target
		expect(rows.map(row => row.error)).toEqual([undefined, undefined]);

		const threads = (await node.threads.list(node.auth, { page_size: 100 })).items;
		// The group keeps its three members; the two direct sends did not fold into it.
		const direct = threads.filter(thread => thread.thread_ref.id !== "plan");
		expect(direct).toHaveLength(2);
		// The owner is the thread's owner, not a member row: each direct thread lists its one peer.
		const parties = (await Promise.all(direct.map(async thread =>
			(await node.threads.describe(node.auth, thread.thread_ref)).members.map(member => member.peer).join(",")))).sort();
		expect(parties).toEqual([ALPHA, BETA].sort());
	});
});

describe("the send receipt", () => {
	test("names the peer it addressed and where the entry is stored", async () => {
		// The thread is this node's, the addressee is a peer: the receipt must not read as a self-send.
		const { node, decider, grants } = rig();
		const result = await harness({ node, decider }).call({ to: "alpha" });
		expect(result.isError).toBeUndefined();
		const text = result.content[0]?.text ?? "";
		expect(text.startsWith("Delivered")).toBe(false);
		expect(text.startsWith(`Sent to ${ALPHA}`)).toBe(true);
		expect(text).toContain("is hosted on your node");
		expect(text).toContain(`${ALPHA} reads it as a member`);
		expect(text).toContain("read back: onlyne://outbound/");
		expect(grants).toEqual([`${ALPHA} /grants`]);
	});

	test("a self-addressed send says so and claims no notice", async () => {
		const { node, decider } = rig();
		const thread = await node.threads.create(node.auth, { title: "notes", id: "notes", ...mutation() });
		const result = await harness({ node, decider }).call({ to: "self", thread: thread.thread_ref });
		expect(result.isError).toBeUndefined();
		const text = result.content[0]?.text ?? "";
		expect(text.startsWith("Sent to yourself")).toBe(true);
		expect(text).toContain("is hosted on your node");
		expect(text).not.toContain("grant notice");
	});
});
