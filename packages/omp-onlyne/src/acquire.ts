import { BlockList, isIP } from "node:net";

/** RFC 0018 §2: how far a peer may be acquired without a human confirming first. */
export type AcquirePolicy = "all" | "segment" | "none";

/** RFC 0018 §5: host-side acquisition settings. The library never reads them. */
export interface AcquisitionPolicy {
	policy: AcquirePolicy;
	/** CIDR blocks; only read when `policy` is `segment`. */
	allow: string[];
}

/**
 * The default delegation: loopback only. Discovery excludes loopback by default, so the two
 * defaults together mean a remote peer is neither discovered nor approved (RFC 0018 §5.1).
 */
export const DEFAULT_ACQUISITION: AcquisitionPolicy = {
	policy: "segment",
	allow: ["127.0.0.0/8", "::1/128"],
};

/** The configured policy, or the documented default when the config carries none. */
export function acquisitionOf(config: { acquisition?: AcquisitionPolicy }): AcquisitionPolicy {
	return config.acquisition ?? DEFAULT_ACQUISITION;
}

export type AcquireReason =
	| "policy_all"
	| "policy_segment"
	| "policy_denied_none"
	| "policy_unobserved"
	| "policy_denied_observed"
	| "policy_denied_declared"
	| "policy_denied_recorded";

/** RFC 0018 §2.1–2.2: what the host observed, what the peer declared, what discovery recorded. */
export interface AcquireInput {
	/** The address this statement's connection arrived from; absent when it was not observed. */
	observed?: string;
	/** The `base_url` the peer wrote into its own statement. */
	declared?: string;
	/** Every discovery address recorded for this node id. */
	recorded?: readonly string[];
}

export interface AcquireVerdict {
	allowed: boolean;
	reason: AcquireReason;
	/** The allow entry the observed address matched, when one did. */
	matched?: string;
}

interface Block {
	network: string;
	prefix: number;
	type: "ipv4" | "ipv6";
}

/** One `[acquisition] allow` entry: `network/prefix`, with an IPv6 host optionally bracketed. */
export function parseAllowBlock(value: string): Block | undefined {
	const trimmed = value.trim();
	const slash = trimmed.lastIndexOf("/");
	if (slash <= 0) return undefined;
	const prefix = Number(trimmed.slice(slash + 1));
	if (!Number.isInteger(prefix) || prefix < 0) return undefined;
	const network = literalHost(trimmed.slice(0, slash));
	if (network === undefined) return undefined;
	if (prefix > (network.type === "ipv4" ? 32 : 128)) return undefined;
	return { network: network.address, prefix, type: network.type };
}

/**
 * The literal address an address or service URL names. A host name is not an address: judging
 * it would require a resolution the policy never authorized, so it matches no block.
 */
function literalHost(value: string): { address: string; type: "ipv4" | "ipv6" } | undefined {
	const trimmed = value.trim();
	const host = trimmed.includes("://") ? urlHost(trimmed) : trimmed;
	if (host === undefined) return undefined;
	const bare = host.startsWith("[") && host.endsWith("]") ? host.slice(1, -1) : host;
	const mapped = /^::ffff:(\d+\.\d+\.\d+\.\d+)$/i.exec(bare);
	if (mapped !== null) return { address: mapped[1]!, type: "ipv4" };
	const family = isIP(bare);
	if (family === 4) return { address: bare, type: "ipv4" };
	if (family === 6) return { address: bare, type: "ipv6" };
	return undefined;
}

function urlHost(value: string): string | undefined {
	try {
		return new URL(value).hostname;
	} catch {
		return undefined;
	}
}

/**
 * The allow entry `value` falls in, or undefined. Unreadable entries are skipped here because
 * `parseConfig` already refused them; a hand-built config with a bad entry matches nothing.
 */
export function allowMatch(value: string, allow: readonly string[]): string | undefined {
	const target = literalHost(value);
	if (target === undefined) return undefined;
	for (const entry of allow) {
		const block = parseAllowBlock(entry);
		if (block === undefined || block.type !== target.type) continue;
		const blocks = new BlockList();
		blocks.addSubnet(block.network, block.prefix, block.type);
		if (blocks.check(target.address, block.type)) return entry.trim();
	}
	return undefined;
}

/**
 * RFC 0018 §2: decide whether this peer may be approved without a human confirming first.
 *
 * The declared address is checked because the trust row stores it and `peerBase` falls back to
 * it when discovery has no record. The recorded addresses are checked because
 * `discovery.resolve()` re-probes every one of them before it connects, so a record outside the
 * allow set is an outbound request the policy never authorized.
 */
export function evaluateAcquisition(policy: AcquisitionPolicy, input: AcquireInput): AcquireVerdict {
	if (policy.policy === "none") return { allowed: false, reason: "policy_denied_none" };
	if (policy.policy === "all") return { allowed: true, reason: "policy_all" };
	if (input.observed === undefined) return { allowed: false, reason: "policy_unobserved" };
	const observed = allowMatch(input.observed, policy.allow);
	if (observed === undefined) return { allowed: false, reason: "policy_denied_observed" };
	if (input.declared !== undefined && allowMatch(input.declared, policy.allow) === undefined) {
		return { allowed: false, reason: "policy_denied_declared", matched: observed };
	}
	for (const address of input.recorded ?? []) {
		if (allowMatch(address, policy.allow) === undefined) {
			return { allowed: false, reason: "policy_denied_recorded", matched: observed };
		}
	}
	return { allowed: true, reason: "policy_segment", matched: observed };
}
