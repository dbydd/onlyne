import { describe, expect, test } from "bun:test";
import type { Entry } from "@onlyne/lib";
import type { ExtensionContext } from "@oh-my-pi/pi-coding-agent";
import { createRuntimeBridge, type BridgeDeps } from "../src/messages.ts";
import type { ContextRegistry } from "../src/store.ts";
import type { JudgeDecider } from "../src/decider.ts";

/** One entry per test: the bridge keys its attempts by thread and seq, for the process lifetime. */
function entryAt(seq: string): Entry {
	return {
		thread_ref: { owner: "urn:onlyne:agent:peer", id: `thread-${seq}` },
		seq,
		message: { id: `m-${seq}`, sender: "urn:onlyne:agent:peer", delivery_kind: "note", msg: "status" },
		authenticated_author: "urn:onlyne:agent:peer",
		reply_authority: "urn:onlyne:agent:peer",
		at: "2026-01-01T00:00:00Z",
	};
}

/** A bridge whose session is idle or busy, recording every delivery it submits. */
function bridge(idle: boolean, action: "fragment" | "wake") {
	const sent: Array<{ options: unknown }> = [];
	const logged: string[] = [];
	const deps = {
		nodeId: "urn:onlyne:agent:self",
		pi: {
			sendMessage: (_message: unknown, options: unknown) => { sent.push({ options }); },
			logger: { info: (name: string) => logged.push(name), debug: () => {}, warn: (name: string) => logged.push(name) },
		},
		main: { agent: { id: "Main" }, isIdle: () => idle } as unknown as ExtensionContext,
		decider: { decideInbound: async () => ({ action }) } as unknown as JudgeDecider,
		contexts: { list: async () => [] } as unknown as ContextRegistry,
	} as unknown as BridgeDeps;
	return { runtime: createRuntimeBridge(deps), sent, logged };
}

describe("delivery of a fragment", () => {
	test("an idle session gets a trigger instead of a queue it will never reach", async () => {
		const { runtime, sent, logged } = bridge(true, "fragment");
		await runtime.inject(entryAt("1"));
		expect(sent).toHaveLength(1);
		expect(sent[0]!.options).toEqual({ triggerTurn: true });
		expect(logged).toContain("onlyne.delivery.escalated");
	});

	test("a busy session keeps the quiet next-turn delivery", async () => {
		const { runtime, sent, logged } = bridge(false, "fragment");
		await runtime.inject(entryAt("2"));
		expect(sent).toHaveLength(1);
		expect(sent[0]!.options).toEqual({ deliverAs: "nextTurn" });
		expect(logged).not.toContain("onlyne.delivery.escalated");
	});

	test("an unconfirmed delivery is forced through once, then reported", async () => {
		const { runtime, sent, logged } = bridge(false, "fragment");
		const entry = entryAt("3");
		await runtime.inject(entry);
		await runtime.inject(entry);
		await runtime.inject(entry);
		// Two held passes do nothing new.
		expect(sent).toHaveLength(1);
		await runtime.inject(entry);
		// The third held pass delivers it as a trigger, once.
		expect(sent).toHaveLength(2);
		expect(sent[1]!.options).toEqual({ triggerTurn: true });
		await runtime.inject(entry);
		expect(sent).toHaveLength(2);
		expect(logged).toContain("onlyne.delivery.unconfirmed");
	});
});
