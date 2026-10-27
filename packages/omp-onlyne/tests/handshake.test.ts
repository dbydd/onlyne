import { describe, expect, test } from "bun:test";
import { MemoryStorage, OnlyneError } from "@onlyne/lib";
import type { TrustEntry } from "@onlyne/lib";
import { DEFAULT_ACQUISITION, type AcquisitionPolicy } from "../src/acquire.ts";
import {
	AcquisitionLog,
	approvePeer,
	handshakeEntry,
	handshakeFacts,
	revokePeer,
	type ApprovalDeps,
	type ApprovalJudgement,
} from "../src/handshake.ts";

const PEER = "urn:onlyne:agent:peer";
const FINGERPRINT = "0123456789abcdef";
const LOOPBACK = "http://127.0.0.1:7801/onlyne/v3";
const PUBLIC = "https://elsewhere.example/onlyne/v3";
const AT = "2026-10-27T00:00:00.000Z";

const closed: AcquisitionPolicy = { policy: "none", allow: [] };

const entry: TrustEntry = {
	node_id: PEER, public_key: "", fingerprint: FINGERPRINT, state: "pending",
	first_seen: AT, last_checked: AT, local_approved: true, remote_approved: false, base_url: LOOPBACK,
};

/** The host side of one approval, with the calls it would commit recorded instead of made. */
function harness(policy: AcquisitionPolicy, address: { declared?: string; recorded?: string[] } = {}) {
	const log = new AcquisitionLog(new MemoryStorage());
	const calls: string[] = [];
	const audited: ApprovalJudgement[] = [];
	const deps: ApprovalDeps = {
		policy,
		log,
		observed: async (node, fingerprint) => (await log.observation(node, fingerprint))?.observed,
		declared: async () => address.declared,
		recorded: async () => address.recorded ?? [],
		audit: judgement => audited.push(judgement),
		accept: async () => {
			calls.push("accept");
			return entry;
		},
		revoke: async () => { calls.push("revoke"); },
		now: () => AT,
	};
	return { deps, log, calls, audited };
}

async function failure(work: Promise<unknown>): Promise<OnlyneError> {
	const error = await work.then(() => undefined, (reason: unknown) => reason);
	if (!(error instanceof OnlyneError)) throw new Error(`expected an OnlyneError, got ${String(error)}`);
	return error;
}

describe("agent-initiated approval", () => {
	test("policy none refuses it and the peer keeps its invitation", async () => {
		const { deps, log, calls, audited } = harness(closed, { declared: LOOPBACK });
		await log.observe(PEER, FINGERPRINT, { observed: "127.0.0.1", at: AT, cause: "peer-statement" });
		await log.markDelivered(PEER, FINGERPRINT, "invited", AT);
		const error = await failure(approvePeer(deps, PEER, FINGERPRINT));
		expect(error.error.code).toBe("forbidden");
		expect(error.error.details).toMatchObject({ policy_denied: "policy_denied_none", policy: "none" });
		expect(calls).toEqual([]);
		expect(await log.delivered(PEER, FINGERPRINT, "invited")).toBe(true);
		expect(audited).toHaveLength(1);
		expect(audited[0]?.verdict.reason).toBe("policy_denied_none");
	});

	test("segment refuses a declared address outside the allow set", async () => {
		const { deps, log, calls, audited } = harness(DEFAULT_ACQUISITION, { declared: PUBLIC });
		await log.observe(PEER, FINGERPRINT, { observed: "127.0.0.1", at: AT, cause: "peer-statement" });
		const error = await failure(approvePeer(deps, PEER, FINGERPRINT));
		expect(error.error.code).toBe("forbidden");
		expect(error.error.details).toMatchObject({ policy_denied: "policy_denied_declared", observed: "127.0.0.1", declared: PUBLIC });
		expect(calls).toEqual([]);
		expect(audited[0]?.input).toEqual({ observed: "127.0.0.1", declared: PUBLIC, recorded: [] });
	});

	test("segment refuses a statement this host never observed", async () => {
		const { deps, calls } = harness(DEFAULT_ACQUISITION, { declared: LOOPBACK });
		const error = await failure(approvePeer(deps, PEER, FINGERPRINT));
		expect(error.error.details).toMatchObject({ policy_denied: "policy_unobserved", policy: "segment" });
		expect(calls).toEqual([]);
	});

	test("segment approves a loopback statement whose addresses all stay home", async () => {
		const { deps, log, calls, audited } = harness(DEFAULT_ACQUISITION, { declared: LOOPBACK, recorded: [LOOPBACK] });
		await log.observe(PEER, FINGERPRINT, { observed: "127.0.0.1", at: AT, cause: "peer-statement" });
		await log.markDelivered(PEER, FINGERPRINT, "invited", AT);
		expect(await approvePeer(deps, PEER, FINGERPRINT)).toEqual(entry);
		expect(calls).toEqual(["accept"]);
		expect(audited[0]?.verdict).toEqual({ allowed: true, reason: "policy_segment", matched: "127.0.0.0/8" });
		// The successful op settles the report it answered.
		expect(await log.delivered(PEER, FINGERPRINT, "invited")).toBe(false);
	});

	test("revocation is never judged, even under policy none", async () => {
		const { deps, log, calls } = harness(closed);
		await log.markDelivered(PEER, FINGERPRINT, "requested", AT);
		await revokePeer(deps, PEER);
		expect(calls).toEqual(["revoke"]);
		expect(await log.delivered(PEER, FINGERPRINT, "requested")).toBe(false);
	});
});

describe("handshake entry", () => {
	test("an invitation that needs a decision carries both action identifiers", () => {
		const judgement = {
			at: AT, node_id: PEER, fingerprint: FINGERPRINT, policy: DEFAULT_ACQUISITION,
			input: { observed: "127.0.0.1", declared: PUBLIC, recorded: [] },
			verdict: { allowed: false, reason: "policy_denied_declared" as const, matched: "127.0.0.0/8" },
		};
		const invited = handshakeEntry({
			node_id: PEER, fingerprint: FINGERPRINT, phase: "invited", declared: PUBLIC,
			observed: "127.0.0.1", policy: DEFAULT_ACQUISITION, judgement, approved: false,
		});
		expect(invited.trust).toEqual({ node_id: PEER, fingerprint: FINGERPRINT, phase: "invited", pending: true });
		for (const text of [PEER, FINGERPRINT, PUBLIC, "127.0.0.1", "segment", "policy_denied_declared", "trust_accept", "trust_revoke"]) {
			expect(invited.msg).toContain(text);
		}
	});

	test("an auto-approved invitation states the result and asks for nothing", () => {
		const approved = handshakeEntry({
			node_id: PEER, fingerprint: FINGERPRINT, phase: "invited", declared: LOOPBACK,
			observed: "127.0.0.1", policy: DEFAULT_ACQUISITION, approved: true,
		});
		expect(approved.trust.pending).toBe(false);
		expect(approved.msg).toContain("segment");
		expect(approved.msg).not.toContain("trust_accept");
	});

	test("policy none offers no approval, only the revocation", () => {
		const held = handshakeEntry({
			node_id: PEER, fingerprint: FINGERPRINT, phase: "invited", declared: LOOPBACK,
			observed: "127.0.0.1", policy: closed, approved: false,
		});
		expect(held.trust.pending).toBe(true);
		expect(held.msg).not.toContain("trust_accept");
		expect(held.msg).toContain("trust_revoke");
	});

	test("a local approval waiting on the peer is delivered without work", () => {
		const waiting = handshakeEntry({
			node_id: PEER, fingerprint: FINGERPRINT, phase: "requested", declared: LOOPBACK,
			policy: DEFAULT_ACQUISITION, approved: true,
		});
		expect(waiting.trust.pending).toBe(false);
		expect(waiting.msg).not.toContain("trust_accept");
		expect(waiting.msg).not.toContain("trust_revoke");
	});

	test("a status update is delivered but carries no action", () => {
		for (const phase of ["exchanged", "revoked"] as const) {
			const status = handshakeEntry({ node_id: PEER, fingerprint: FINGERPRINT, phase, declared: LOOPBACK, policy: DEFAULT_ACQUISITION, approved: true });
			expect(status.trust.pending).toBe(false);
			expect(status.msg).not.toContain("trust_accept");
			expect(status.msg).not.toContain("trust_revoke");
		}
		expect(handshakeFacts({ data: { trust: { node_id: PEER, fingerprint: FINGERPRINT, phase: "exchanged", pending: false } } }))
			.toEqual({ node_id: PEER, fingerprint: FINGERPRINT, phase: "exchanged", pending: false });
		expect(handshakeFacts({})).toBeUndefined();
	});
});

describe("observation record", () => {
	test("the address a statement arrived from survives until the approval reads it", async () => {
		const log = new AcquisitionLog(new MemoryStorage());
		expect(await log.observation(PEER, FINGERPRINT)).toBeUndefined();
		await log.observe(PEER, FINGERPRINT, { observed: "10.1.2.3", at: AT, cause: "peer-statement" });
		expect(await log.observation(PEER, FINGERPRINT)).toEqual({ observed: "10.1.2.3", at: AT, cause: "peer-statement" });
		await log.transition(PEER, FINGERPRINT, "2026-10-27T00:01:00.000Z", "local-accept");
		expect(await log.observation(PEER, FINGERPRINT)).toEqual({ observed: "10.1.2.3", at: "2026-10-27T00:01:00.000Z", cause: "local-accept" });
		// An unobserved statement is recorded as such, so a later approval cannot invent one.
		await log.observe(PEER, FINGERPRINT, { at: AT, cause: "peer-statement" });
		expect(await log.observation(PEER, FINGERPRINT)).toEqual({ at: AT, cause: "peer-statement" });
	});
});
