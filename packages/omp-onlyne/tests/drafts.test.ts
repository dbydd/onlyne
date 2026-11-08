import { describe, expect, test } from "bun:test";
import { MemoryStorage, type Json, type MsgInput, type PreparedSend, type WorkRef } from "@onlyne/lib";
import type { ExtensionContext } from "@oh-my-pi/pi-coding-agent";
import { createDraft, sendDraft } from "../src/drafts.ts";
import type { PluginRuntime } from "../src/runtime.ts";

const self = "urn:onlyne:agent:a";
const peer = "urn:onlyne:agent:b";
const work: WorkRef = { unit: "unit-3", parent: null, attempt: 1 };
const workJson = work as unknown as Json;

const ctx = { agent: { kind: "main", id: "Main" } } as unknown as ExtensionContext;

/** A runtime whose node answers `prepare` from the input and records what `send` carries. */
function runtimeOf(preparedInputs: MsgInput[], sent: PreparedSend[]): PluginRuntime {
	const storage = new MemoryStorage();
	return {
		storage,
		contexts: { list: async () => [] },
		decider: { refs: { peerId: async (value: string) => value } },
		node: {
			client: {
				prepare: async (input: MsgInput): Promise<PreparedSend> => {
					preparedInputs.push(input);
					return {
						to: peer,
						message: {
							id: "m-2", sender: self, delivery_kind: input.delivery_kind, msg: input.msg,
							meta: { ...(input.meta?.work === undefined ? {} : { work: input.meta.work as unknown as WorkRef }), in_reply_to: { sender: peer, id: "m-1" }, relation: "reply" },
						},
						plan: { response: "none", execution: "new" },
					};
				},
				send: async (_to: string, message: PreparedSend["message"], plan: PreparedSend["plan"]) => {
					sent.push({ to: peer, message, plan });
					return { admission: "accepted" };
				},
			},
		},
	} as unknown as PluginRuntime;
}

describe("draft work metadata", () => {
	test("a draft keeps the work unit it was prepared with, and sends it unchanged", async () => {
		const preparedInputs: MsgInput[] = [];
		const sent: PreparedSend[] = [];
		const runtime = runtimeOf(preparedInputs, sent);
		const draft = await createDraft(runtime, ctx, {
			delivery_kind: "draft the reply", msg: "answer",
			meta: { in_reply_to: { sender: peer, id: "m-1" }, relation: "reply", work: workJson },
		}) as { draft_id: string };

		// The work ref reaches the library's prepare call, which is what builds the message.
		expect(preparedInputs[0]?.meta?.work).toEqual(workJson);
		const stored = await runtime.storage.transaction(tx => tx.get<{ prepared: PreparedSend }>(`draft/${draft.draft_id}`));
		expect(stored?.prepared.message.meta?.work).toEqual(work);

		// Confirmation sends the prepared message itself, so the ref cannot be dropped there.
		await sendDraft(runtime, ctx, { id: draft.draft_id });
		expect(sent[0]?.message.meta?.work).toEqual(work);
	});
});
