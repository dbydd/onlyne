import { afterEach, describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { Json } from "@onlyne/lib";
import { configPath, normalizeNodeId, type AgyOnlyneConfig } from "../src/config.ts";
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
		node: { id, key_scope: "project" },
		server: { bind: "127.0.0.1" },
		threads: { page_size: 50 },
		acquisition: { policy: "all", allow: ["127.0.0.1/32", "::1/128"] },
		judge: { mode: "none" },
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

const waitForInboxMessage = async (rt: AgyRuntime, text: string) => {
	for (let i = 0; i < 40; i += 1) {
		const page = await rt.node.messages.inbox(rt.node.auth, { page_size: 50 });
		const found = page.items.find(item => item.attention.entry?.message.msg === text);
		if (found) return found;
		await Bun.sleep(25);
	}
	const page = await rt.node.messages.inbox(rt.node.auth, { page_size: 50 });
	return page.items.find(item => item.attention.entry?.message.msg === text);
};

describe("SystemOne JudgeDecider & single msg entry", () => {
	test("interprets msg via JudgeDecider across direct send, draft/send_draft, thread reply, trust ops, and multi-target broadcast", async () => {
		const dirA = makeTempDir("agy-msg-a-");
		const dirB = makeTempDir("agy-msg-b-");
		const dirC = makeTempDir("agy-msg-c-");

		const nodeAId = normalizeNodeId("msg-alpha");
		const nodeBId = normalizeNodeId("msg-beta");
		const nodeCId = normalizeNodeId("msg-gamma");

		let lastJudgeQuestions: string[] = [];

		const startNode = async (dir: string, id: string) => AgyRuntime.start({
			cwd: dir,
			config: makeConfig(dir, id),
			inMemory: true,
			discovery: false,
			bindHttp: true,
			customJudge: async (_state, questions) => {
				lastJudgeQuestions = Object.keys(questions);
				const answers: Record<string, { choice?: string; confidence?: string }> = {};
				for (const [key, q] of Object.entries(questions)) {
					const keys = Object.keys(q.criteria);
					if (key === "intent") {
						answers[key] = { choice: "communicate", confidence: "high" };
					} else if (key === "original") {
						answers[key] = { choice: "new_message", confidence: "high" };
					} else if (key === "relation") {
						answers[key] = { choice: "reply", confidence: "high" };
					} else if (key === "target") {
						answers[key] = { choice: keys[0] ?? "none", confidence: "high" };
					} else if (key === "response") {
						answers[key] = { choice: "async", confidence: "high" };
					} else if (key === "execution") {
						answers[key] = { choice: keys.includes("existing") ? "existing" : "new", confidence: "high" };
					} else if (key === "context" || key === "context_alias") {
						answers[key] = { choice: keys.includes("main") ? "main" : (keys[0] ?? "main"), confidence: "high" };
					} else if (key === "todo") {
						answers[key] = { choice: "none", confidence: "high" };
					} else {
						answers[key] = { choice: keys[0] ?? "", confidence: "high" };
					}
				}
				return answers;
			},
		});

		const [rtA, rtB, rtC] = await Promise.all([
			startNode(dirA, nodeAId),
			startNode(dirB, nodeBId),
			startNode(dirC, nodeCId),
		]);
		runtimes.push(rtA, rtB, rtC);

		// Pair A <-> B and A <-> C over loopback HTTP
		await Promise.all([
			rtA.connectSeed(rtB.endpoint),
			rtA.connectSeed(rtC.endpoint),
			rtB.connectSeed(rtA.endpoint),
			rtC.connectSeed(rtA.endpoint),
		]);
		await Promise.all([
			rtA.trustConfirm(nodeBId, rtB.fingerprint!),
			rtB.trustConfirm(nodeAId, rtA.fingerprint!),
			rtA.trustConfirm(nodeCId, rtC.fingerprint!),
			rtC.trustConfirm(nodeAId, rtA.fingerprint!),
		]);

		const caller = { agent_id: "conv-main", main: true };

		// 1. Direct message from A to B via single msg entry
		const sent = await rtA.executeMsgTool(caller, {
			delivery_kind: "question",
			msg: "Hello Beta from SystemOne",
			meta: { to: "msg-beta" },
		});
		expect(lastJudgeQuestions.length).toBeGreaterThan(0);
		expect(sent.text).toContain("msg-beta");

		const incoming = await waitForInboxMessage(rtB, "Hello Beta from SystemOne");
		expect(incoming).toBeDefined();

		// 2. Reply from B back to A referencing the incoming thread & message id
		const reply = await rtB.executeMsgTool(caller, {
			delivery_kind: "answer",
			msg: "Roger Alpha, test green",
			meta: {
				to: nodeAId,
				thread: incoming!.attention.thread_ref as unknown as Json,
				in_reply_to: {
					sender: incoming!.attention.entry!.message.sender,
					id: incoming!.attention.entry!.message.id,
				},
				relation: "reply",
			},
		});
		expect(reply.text).toContain("msg-alpha");

		const aIncoming = await waitForInboxMessage(rtA, "Roger Alpha, test green");
		expect(aIncoming).toBeDefined();

		// 3. Stage a draft and send it via send_draft
		const drafted = await rtA.executeMsgTool(caller, {
			delivery_kind: "note",
			msg: "Drafted memo awaiting release",
			meta: { op: "draft", to: nodeBId },
		});
		const draftObj = (drafted.details as { draft?: { draft_id?: string } }).draft;
		expect(draftObj?.draft_id).toBeDefined();

		const bCountBefore = (await rtB.node.messages.inbox(rtB.node.auth, { page_size: 50 })).items.length;
		const sentDraft = await rtA.executeMsgTool(caller, {
			delivery_kind: "note",
			msg: "Released memo with edits",
			meta: { op: "send_draft", draft_id: draftObj!.draft_id! },
		});
		expect(sentDraft.text).toContain("msg-beta");
		const released = await waitForInboxMessage(rtB, "Released memo with edits");
		expect(released).toBeDefined();
		const bInboxAfterDraft = await rtB.node.messages.inbox(rtB.node.auth, { page_size: 50 });
		expect(bInboxAfterDraft.items.length).toBeGreaterThan(bCountBefore);

		// 4. Multi-target array meta.to dispatches to both Beta and Gamma
		const multi = await rtA.executeMsgTool(caller, {
			delivery_kind: "notify",
			msg: "Broadcast release notice",
			meta: { to: [nodeBId, nodeCId] },
		});
		expect((multi.details as { count?: number }).count).toBe(2);
		const cIncoming = await waitForInboxMessage(rtC, "Broadcast release notice");
		expect(cIncoming).toBeDefined();

		// 5. Key rotation through meta.op = trust_rotate updates A's fingerprint
		const oldFp = rtA.fingerprint;
		const rotated = await rtA.executeMsgTool(caller, {
			delivery_kind: "trust",
			msg: "rotate key",
			meta: { op: "trust_rotate" },
		});
		const newFp = (rotated.details as { fingerprint?: string }).fingerprint;
		expect(newFp).toBeDefined();
		expect(newFp).not.toBe(oldFp);
	});
});
