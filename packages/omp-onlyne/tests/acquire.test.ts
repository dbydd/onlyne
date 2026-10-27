import { describe, expect, test } from "bun:test";
import { DEFAULT_ACQUISITION, allowMatch, evaluateAcquisition, parseAllowBlock } from "../src/acquire.ts";
import type { AcquisitionPolicy } from "../src/acquire.ts";

const loopback = DEFAULT_ACQUISITION;
const open: AcquisitionPolicy = { policy: "all", allow: [] };
const closed: AcquisitionPolicy = { policy: "none", allow: [] };

describe("acquisition policy", () => {
	test("segment approves a loopback statement whose addresses all stay home", () => {
		expect(
			evaluateAcquisition(loopback, {
				observed: "127.0.0.1",
				declared: "http://127.0.0.1:7801/onlyne/v3",
				recorded: ["http://127.0.0.1:7801/onlyne/v3"],
			}),
		).toEqual({ allowed: true, reason: "policy_segment", matched: "127.0.0.0/8" });
	});

	test("segment refuses a declared address outside the allow set", () => {
		expect(
			evaluateAcquisition(loopback, { observed: "127.0.0.1", declared: "https://elsewhere.example/onlyne/v3" }),
		).toEqual({ allowed: false, reason: "policy_denied_declared", matched: "127.0.0.0/8" });
	});

	test("segment refuses when any recorded address leaves the allow set", () => {
		expect(
			evaluateAcquisition(loopback, {
				observed: "127.0.0.1",
				declared: "http://127.0.0.1:7801/onlyne/v3",
				recorded: ["http://127.0.0.1:7801/onlyne/v3", "https://elsewhere.example/onlyne/v3"],
			}),
		).toEqual({ allowed: false, reason: "policy_denied_recorded", matched: "127.0.0.0/8" });
	});

	test("segment refuses an unobserved statement and a remote connection", () => {
		expect(evaluateAcquisition(loopback, { declared: "http://127.0.0.1:7801/onlyne/v3" })).toEqual({
			allowed: false,
			reason: "policy_unobserved",
		});
		expect(evaluateAcquisition(loopback, { observed: "10.1.2.3" })).toEqual({
			allowed: false,
			reason: "policy_denied_observed",
		});
	});

	test("none refuses and all approves without an address", () => {
		expect(evaluateAcquisition(closed, { observed: "127.0.0.1" })).toEqual({ allowed: false, reason: "policy_denied_none" });
		expect(evaluateAcquisition(open, {})).toEqual({ allowed: true, reason: "policy_all" });
	});

	test("a name is not an address and matches no block", () => {
		expect(allowMatch("http://localhost:7801/onlyne/v3", loopback.allow)).toBeUndefined();
		expect(allowMatch("::ffff:127.0.0.1", loopback.allow)).toBe("127.0.0.0/8");
		expect(allowMatch("http://[::1]:7801/onlyne/v3", loopback.allow)).toBe("::1/128");
	});

	test("allow blocks carry a family and an in-range prefix", () => {
		expect(parseAllowBlock("127.0.0.0/8")).toEqual({ network: "127.0.0.0", prefix: 8, type: "ipv4" });
		expect(parseAllowBlock("[::1]/128")).toEqual({ network: "::1", prefix: 128, type: "ipv6" });
		expect(parseAllowBlock("127.0.0.0")).toBeUndefined();
		expect(parseAllowBlock("127.0.0.0/33")).toBeUndefined();
		expect(parseAllowBlock("::1/129")).toBeUndefined();
	});
});
