import { describe, expect, test } from "bun:test";
import { DEFAULT_ACQUISITION, allowMatch, evaluateAcquisition, parseAllowBlock } from "../src/acquire.ts";
import type { AcquisitionPolicy, Resolver } from "../src/acquire.ts";

const loopback = DEFAULT_ACQUISITION;
const open: AcquisitionPolicy = { policy: "all", allow: [] };
const closed: AcquisitionPolicy = { policy: "none", allow: [] };

const none: Resolver = async () => [];

describe("acquisition policy", () => {
	test("segment approves a loopback statement whose addresses all stay home", async () => {
		expect(
			await evaluateAcquisition(loopback, {
				observed: "127.0.0.1",
				declared: "http://127.0.0.1:7801/onlyne/v3",
				recorded: ["http://127.0.0.1:7801/onlyne/v3"],
			}, none),
		).toEqual({ allowed: true, reason: "policy_segment", matched: "127.0.0.0/8" });
	});

	test("segment refuses a declared address outside the allow set", async () => {
		expect(
			await evaluateAcquisition(loopback, { observed: "127.0.0.1", declared: "https://elsewhere.example/onlyne/v3" }, none),
		).toEqual({ allowed: false, reason: "policy_denied_declared", matched: "127.0.0.0/8" });
	});

	test("segment refuses when any recorded address leaves the allow set", async () => {
		expect(
			await evaluateAcquisition(loopback, {
				observed: "127.0.0.1",
				declared: "http://127.0.0.1:7801/onlyne/v3",
				recorded: ["http://127.0.0.1:7801/onlyne/v3", "https://elsewhere.example/onlyne/v3"],
			}, none),
		).toEqual({ allowed: false, reason: "policy_denied_recorded", matched: "127.0.0.0/8" });
	});

	test("segment refuses an unobserved statement and a remote connection", async () => {
		expect(await evaluateAcquisition(loopback, { declared: "http://127.0.0.1:7801/onlyne/v3" }, none)).toEqual({
			allowed: false,
			reason: "policy_unobserved",
		});
		expect(await evaluateAcquisition(loopback, { observed: "10.1.2.3" }, none)).toEqual({
			allowed: false,
			reason: "policy_denied_observed",
		});
	});

	test("none refuses and all approves without an address", async () => {
		expect(await evaluateAcquisition(closed, { observed: "127.0.0.1" }, none)).toEqual({ allowed: false, reason: "policy_denied_none" });
		expect(await evaluateAcquisition(open, {}, none)).toEqual({ allowed: true, reason: "policy_all" });
	});

	test("a declared name is judged by every address it resolves to", async () => {
		const inside: Resolver = async host => ({ "peer.test": ["127.0.0.2"] })[host] ?? [];
		expect(await allowMatch("http://peer.test:7801/onlyne/v3", loopback.allow, inside)).toBe("127.0.0.0/8");
		// One address outside the set denies the whole name: a name is a set of addresses, and the
		// host cannot choose which one a later connection will use.
		const mixed: Resolver = async host => ({ "peer.test": ["127.0.0.2", "10.0.0.5"] })[host] ?? [];
		expect(await allowMatch("http://peer.test:7801/onlyne/v3", loopback.allow, mixed)).toBeUndefined();
		expect(await allowMatch("http://peer.test:7801/onlyne/v3", loopback.allow, none)).toBeUndefined();
	});

	test("an allow entry may name the host it accepts", async () => {
		const policy: AcquisitionPolicy = { policy: "segment", allow: ["localhost", "127.0.0.0/8"] };
		expect(await allowMatch("http://localhost:7801/onlyne/v3", policy.allow, none)).toBe("localhost");
		// The name entry does not widen anything else: an unrelated name still resolves or fails.
		expect(await allowMatch("http://peer.test:7801/onlyne/v3", policy.allow, none)).toBeUndefined();
	});

	test("a mapped address is judged as the address it maps", async () => {
		expect(await allowMatch("::ffff:127.0.0.1", loopback.allow, none)).toBe("127.0.0.0/8");
		expect(await allowMatch("http://[::1]:7801/onlyne/v3", loopback.allow, none)).toBe("::1/128");
	});

	test("allow blocks carry a family and an in-range prefix", () => {
		expect(parseAllowBlock("127.0.0.0/8")).toEqual({ network: "127.0.0.0", prefix: 8, type: "ipv4" });
		expect(parseAllowBlock("[::1]/128")).toEqual({ network: "::1", prefix: 128, type: "ipv6" });
		expect(parseAllowBlock("localhost")).toEqual({ name: "localhost" });
		expect(parseAllowBlock("127.0.0.0")).toBeUndefined();
		expect(parseAllowBlock("127.0.0.0/33")).toBeUndefined();
		expect(parseAllowBlock("::1/129")).toBeUndefined();
	});
});
