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

afterAll(() => setLocale("en"));

describe("msg summary", () => {
	test("an alias send reports the node it resolved to", () => {
		setLocale("en");
		const text = summary({ ...delivered, requested: "dpdp", resolved: PEER });
		expect(text).toContain(`resolved: ${PEER}`);
		expect(text).toContain(`thread t-1 seq 1`);
	});

	test("a spelling that already names the node adds nothing", () => {
		setLocale("en");
		const text = summary({ ...delivered, requested: PEER, resolved: PEER });
		expect(text).not.toContain("resolved:");
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
