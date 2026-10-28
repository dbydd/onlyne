import { afterAll, describe, expect, test } from "bun:test";
import type { SendOutcome } from "@onlyne/lib";
import { setLocale } from "@onlyne/node-host/i18n";
import { summary } from "../src/tool.ts";
import { toolText } from "../src/tool-text.ts";

const PEER = "urn:onlyne:agent:dpdp";
const SELF = "urn:onlyne:agent:lazuli";

/** A delivered outcome whose receipt names this node as the thread owner. */
const delivered: SendOutcome = {
	admission: "accepted",
	receipt: {
		message_ref: { sender: SELF, id: "m-1" },
		receipt_id: "r-1",
		guarantee: "binding-defined",
		thread_ref: { owner: SELF, id: "t-1" },
		seq: "1",
		plan: { response: "async", execution: "existing" },
	},
};

/** A delivered outcome that landed in a thread the peer owns. */
const intoPeerThread: SendOutcome = {
	admission: "accepted",
	receipt: {
		message_ref: { sender: SELF, id: "m-2" },
		receipt_id: "r-2",
		guarantee: "binding-defined",
		thread_ref: { owner: PEER, id: "t-2" },
		seq: "4",
		plan: { response: "async", execution: "existing" },
	},
};

afterAll(() => setLocale("en"));

describe("msg summary", () => {
	test("a peer addressed through this node's thread reads as a send, not a self-send", () => {
		setLocale("en");
		const text = summary(delivered, PEER);
		expect(text.startsWith("Delivered")).toBe(false);
		expect(text.startsWith(`Sent to ${PEER}`)).toBe(true);
		expect(text).toContain("thread t-1 is hosted on your node, seq 1");
		// The host fact alone would read as a message to itself; the sentence names the peer that reads it.
		expect(text).toContain(`${PEER} reads it as a member`);
		expect(text).not.toEqual(summary(delivered, SELF));
		expect(text).toContain("read back: onlyne://outbound/m-1 and recorded durably");
	});

	test("a send into the peer's thread names that host and claims no notice", () => {
		setLocale("en");
		const text = summary(intoPeerThread, PEER);
		expect(text.startsWith(`Sent to ${PEER}`)).toBe(true);
		expect(text).toContain(`thread t-2 is hosted on ${PEER}, seq 4`);
		expect(text).not.toContain("hosted on your node");
		expect(text).not.toContain("grant notice");
	});

	test("a self-addressed send says so and claims no notice", () => {
		setLocale("en");
		const text = summary(delivered, SELF);
		expect(text.startsWith("Sent to yourself")).toBe(true);
		expect(text).toContain("thread t-1 is hosted on your node, seq 1");
		expect(text).not.toContain("grant notice");
	});

	test("an alias send reports the node it resolved to", () => {
		setLocale("en");
		const text = summary({ ...delivered, requested: "dpdp", resolved: PEER });
		expect(text).toContain(`Sent to ${PEER}`);
		expect(text).toContain(`resolved: ${PEER}`);
		expect(text).toContain("thread t-1 is hosted on your node, seq 1");
	});

	test("a spelling that already names the node adds nothing", () => {
		setLocale("en");
		const text = summary({ ...delivered, requested: PEER, resolved: PEER });
		expect(text).not.toContain("resolved:");
	});

	test("zh-CN states the addressee, the storage, the read-back and the notice in that order", () => {
		setLocale("zh-CN");
		const text = summary(delivered, PEER);
		const addressee = text.indexOf(`已发给 ${PEER}`);
		const storage = text.indexOf("thread t-1 建在你这侧");
		const trail = text.indexOf("读回: onlyne://outbound/m-1");
		const notice = text.indexOf("grant 通知");
		expect(addressee).toBe(0);
		expect(storage).toBeGreaterThan(addressee);
		expect(trail).toBeGreaterThan(storage);
		expect(notice).toBeGreaterThan(trail);
		expect(text).not.toContain("已送达");
	});

	test("zh-CN puts a peer-held thread at the peer and drops the notice", () => {
		setLocale("zh-CN");
		const text = summary(intoPeerThread, PEER);
		expect(text.startsWith(`已发给 ${PEER}`)).toBe(true);
		expect(text).toContain(`thread t-2 建在 ${PEER} 那侧`);
		expect(text).not.toContain("grant 通知");
		setLocale("en");
	});
});

describe("missing-trust advice", () => {
	// The invitation is already recorded; the acquisition policy on the side that owes the
	// approval submits it on the next attempt, so no menu step may be promised.
	for (const locale of ["en", "zh-CN"] as const) {
		test(`${locale} names the policy or the holder, not a host-surface step`, () => {
			setLocale(locale);
			const description = toolText("description");
			expect(description).not.toContain("host surface");
			expect(description).not.toContain("宿主界面");
			expect(description).not.toContain("try to pair");
			expect(description).not.toContain("尝试配对");
			expect(description).toContain(locale === "en" ? "acquisition policy" : "获取策略");
		});
	}
});
