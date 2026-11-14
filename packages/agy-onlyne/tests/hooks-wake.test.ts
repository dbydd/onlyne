import { afterEach, describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { configPath, normalizeNodeId, saveConfig, type AgyOnlyneConfig } from "../src/config.ts";
import { handleHookEvent } from "../src/hooks/handler.ts";
import { AgyRuntime } from "../src/runtime/host.ts";

const runtimes: AgyRuntime[] = [];
const tempDirs: string[] = [];

function makeTempDir(prefix: string): string {
	const dir = mkdtempSync(join(tmpdir(), prefix));
	tempDirs.push(dir);
	return dir;
}

afterEach(async () => {
	while (runtimes.length > 0) {
		await runtimes.pop()!.close().catch(() => {});
	}
	while (tempDirs.length > 0) {
		rmSync(tempDirs.pop()!, { recursive: true, force: true });
	}
});

describe("Antigravity wake hooks & transcript consumption confirmation", () => {
	test("unconfigured workspace without running daemon returns {} without spawning client", async () => {
		const dir = makeTempDir("agy-hook-empty-");
		let spawned = false;
		const res = await handleHookEvent({
			stdinText: JSON.stringify({
				hookEventName: "PreInvocation",
				conversationId: "conv-1",
				cwd: dir,
			}),
			getClient: async () => {
				spawned = true;
				throw new Error("Should not be called on unconfigured workspace");
			},
		});
		expect(res).toEqual({});
		expect(spawned).toBe(false);
	});

	test("injects [[onlyne:<token>]] via PreInvocation and confirms consumption from JSONL transcript", async () => {
		const senderDir = makeTempDir("agy-hook-sender-");
		const receiverDir = makeTempDir("agy-hook-wake-");

		const senderId = normalizeNodeId("wake-sender");
		const receiverId = normalizeNodeId("wake-node");

		const makeCfg = (dir: string, id: string): AgyOnlyneConfig => ({
			path: configPath(dir),
			region: "urn:onlyne:region:local",
			node: { id, key_scope: "project" },
			server: { bind: "127.0.0.1" },
			threads: { page_size: 50 },
			acquisition: { policy: "all", allow: ["127.0.0.1/32", "::1/128"] },
			judge: { mode: "none" },
			wake: { mode: "hook", escalate_ms: 60_000 },
		});

		const senderCfg = makeCfg(senderDir, senderId);
		const receiverCfg = makeCfg(receiverDir, receiverId);
		await saveConfig(receiverCfg);

		const startOpts = (dir: string, cfg: AgyOnlyneConfig) => AgyRuntime.start({
			cwd: dir,
			config: cfg,
			inMemory: true,
			discovery: false,
			bindHttp: true,
			customJudge: async (_state, questions) => {
				const out: Record<string, { choice?: string; confidence?: string }> = {};
				for (const [k, q] of Object.entries(questions)) {
					const keys = Object.keys(q.criteria);
					if (k === "intent") out[k] = { choice: "communicate", confidence: "high" };
					else if (k === "response") out[k] = { choice: "async", confidence: "high" };
					else if (k === "execution") out[k] = { choice: keys.includes("existing") ? "existing" : "new", confidence: "high" };
					else if (k === "context" || k === "route") out[k] = { choice: keys.includes("main") ? "main" : (keys[0] ?? "main"), confidence: "high" };
					else if (k === "todo") out[k] = { choice: "none", confidence: "high" };
					else out[k] = { choice: keys[0] ?? "", confidence: "high" };
				}
				return out;
			},
		});

		const [sender, receiver] = await Promise.all([
			startOpts(senderDir, senderCfg),
			startOpts(receiverDir, receiverCfg),
		]);
		runtimes.push(sender, receiver);

		await Promise.all([
			sender.connectSeed(receiver.endpoint),
			receiver.connectSeed(sender.endpoint),
		]);
		await Promise.all([
			sender.trustConfirm(receiverId, receiver.fingerprint!),
			receiver.trustConfirm(senderId, sender.fingerprint!),
		]);

		await receiver.delivery.registerConversation({
			conversationId: "conv-main-1",
			workspace: receiverDir,
		});

		// Send real message from sender -> receiver; wait briefly for receiver onMessage delivery
		await sender.executeMsgTool(
			{ agent_id: "sender-main", main: true },
			{
				delivery_kind: "question",
				msg: "Please run verification suite",
				meta: { to: receiverId },
			},
		);

		let pendingList = await receiver.delivery.listPending();
		for (let i = 0; i < 20 && pendingList.length === 0; i += 1) {
			await Bun.sleep(25);
			pendingList = await receiver.delivery.listPending();
		}
		expect(pendingList).toHaveLength(1);
		const token = pendingList[0]!.token;
		expect(token.length).toBeGreaterThan(4);

		const transcriptPath = join(receiverDir, "transcript.jsonl");
		writeFileSync(transcriptPath, `${JSON.stringify({ type: "USER_INPUT", content: "hi" })}\n`, "utf8");

		// PreInvocation returns injectSteps containing [[onlyne:<token>]]
		const pre = await receiver.delivery.processHookEvent({
			hookEventName: "PreInvocation",
			conversationId: "conv-main-1",
			transcriptPath,
			cwd: receiverDir,
		});
		expect(pre.injectSteps?.length).toBe(1);
		const sysText = pre.injectSteps?.[0]?.systemMessage.systemMessage ?? "";
		expect(sysText).toContain(`[[onlyne:${token}]]`);
		expect(sysText).toContain("Please run verification suite");

		// Simulate transcript recording the injected [[onlyne:<token>]]
		writeFileSync(
			transcriptPath,
			`${JSON.stringify({ type: "USER_INPUT", content: "hi" })}\n${JSON.stringify({ type: "MODEL", content: `Handled [[onlyne:${token}]]` })}\n`,
			"utf8",
		);

		// First PostToolUse marks first observation; Stop confirms deterministically & clears pending
		const firstTool = await receiver.delivery.processHookEvent({
			hookEventName: "PostToolUse",
			conversationId: "conv-main-1",
			transcriptPath,
			cwd: receiverDir,
		});
		expect(firstTool).toEqual({});
		expect(await receiver.delivery.listPending()).toHaveLength(1);

		const stop = await receiver.delivery.processHookEvent({
			hookEventName: "Stop",
			conversationId: "conv-main-1",
			transcriptPath,
			cwd: receiverDir,
		});
		expect(stop).toEqual({});
		expect(await receiver.delivery.listPending()).toHaveLength(0);
	});
});
