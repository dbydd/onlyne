import { lookup } from "node:dns/promises";
import { BlockList, isIP } from "node:net";

/** RFC 0018 §2: how far a peer may be acquired without a human confirming first. */
export type AcquirePolicy = "all" | "segment" | "none";

/** RFC 0018 §5: host-side acquisition settings. The library never reads them. */
export interface AcquisitionPolicy {
	policy: AcquirePolicy;
	/** CIDR blocks and host names; only read when `policy` is `segment`. */
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

/** Turns a host name into the addresses it names; an empty list means it names none. */
export type Resolver = (host: string) => Promise<string[]>;

const dnsResolver: Resolver = async host => (await lookup(host, { all: true })).map(row => row.address);

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
	/** The discovery addresses recorded for this node id when the statement arrived. */
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

const HOST_NAME = /^[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?$/;

/** One `[acquisition] allow` entry: `network/prefix`, or a host name to accept as written. */
export function parseAllowBlock(value: string): Block | { name: string } | undefined {
	const trimmed = value.trim();
	if (trimmed.length === 0) return undefined;
	const slash = trimmed.lastIndexOf("/");
	if (slash <= 0) {
		// A name is matched by its own spelling, so listing it involves no resolution. A bare
		// address is not a block: it needs a prefix to say what it covers.
		if (isIP(trimmed) !== 0 || !HOST_NAME.test(trimmed)) return undefined;
		return { name: trimmed.toLowerCase() };
	}
	const prefix = Number(trimmed.slice(slash + 1));
	if (!Number.isInteger(prefix) || prefix < 0) return undefined;
	const network = literalAddress(trimmed.slice(0, slash));
	if (network === undefined) return undefined;
	if (prefix > (network.type === "ipv4" ? 32 : 128)) return undefined;
	return { network: network.address, prefix, type: network.type };
}

/** The host a value names: a URL's hostname, or the value itself when it is already a host. */
function hostOf(value: string): string | undefined {
	const trimmed = value.trim();
	if (trimmed.length === 0) return undefined;
	if (!trimmed.includes("://")) return trimmed.replace(/^\[(.*)\]$/, "$1");
	try {
		const host = new URL(trimmed).hostname;
		return host.replace(/^\[(.*)\]$/, "$1");
	} catch {
		return undefined;
	}
}

/** A host that is already a literal address, with IPv4-mapped IPv6 read as IPv4. */
function literalAddress(host: string): { address: string; type: "ipv4" | "ipv6" } | undefined {
	const bare = host.startsWith("[") && host.endsWith("]") ? host.slice(1, -1) : host;
	const mapped = /^::ffff:(\d+\.\d+\.\d+\.\d+)$/i.exec(bare);
	if (mapped !== null) return { address: mapped[1]!, type: "ipv4" };
	const family = isIP(bare);
	if (family === 4) return { address: bare, type: "ipv4" };
	if (family === 6) return { address: bare, type: "ipv6" };
	return undefined;
}

function blockHolds(address: { address: string; type: "ipv4" | "ipv6" }, block: Block): boolean {
	if (block.type !== address.type) return false;
	const blocks = new BlockList();
	blocks.addSubnet(block.network, block.prefix, block.type);
	return blocks.check(address.address, block.type);
}

/** The allow entry an address falls in, or undefined. */
function entryFor(address: { address: string; type: "ipv4" | "ipv6" }, allow: readonly string[]): string | undefined {
	for (const entry of allow) {
		const block = parseAllowBlock(entry);
		if (block === undefined || "name" in block) continue;
		if (blockHolds(address, block)) return entry.trim();
	}
	return undefined;
}

/**
 * The allow entry a value matches, or undefined. A literal address is judged as itself. A host
 * name matches an allow entry of the same name; otherwise it is resolved and every address it
 * names must fall inside the allow set (RFC 0018 §2.1) — a name that resolves to nothing, or to
 * one address outside the set, matches nothing.
 */
export async function allowMatch(value: string, allow: readonly string[], resolve: Resolver = dnsResolver): Promise<string | undefined> {
	const host = hostOf(value);
	if (host === undefined) return undefined;
	const literal = literalAddress(host);
	if (literal !== undefined) return entryFor(literal, allow);
	const named = allow.find(entry => {
		const block = parseAllowBlock(entry);
		return block !== undefined && "name" in block && block.name === host.toLowerCase();
	});
	if (named !== undefined) return named.trim();
	const resolved = await resolve(host).catch(() => [] as string[]);
	let matched: string | undefined;
	for (const address of resolved) {
		const target = literalAddress(address);
		if (target === undefined) return undefined;
		const entry = entryFor(target, allow);
		if (entry === undefined) return undefined;
		matched ??= entry;
	}
	return matched;
}

/**
 * RFC 0018 §2: decide whether this peer may be approved without a human confirming first.
 *
 * The declared address is checked because the trust row stores it and `peerBase` falls back to
 * it when discovery has no record. The recorded addresses are checked because
 * `discovery.resolve()` re-probes every one of them before it connects, so a record outside the
 * allow set is an outbound request the policy never authorized.
 */
export async function evaluateAcquisition(
	policy: AcquisitionPolicy,
	input: AcquireInput,
	resolve: Resolver = dnsResolver,
): Promise<AcquireVerdict> {
	if (policy.policy === "none") return { allowed: false, reason: "policy_denied_none" };
	if (policy.policy === "all") return { allowed: true, reason: "policy_all" };
	if (input.observed === undefined) return { allowed: false, reason: "policy_unobserved" };
	const observed = await allowMatch(input.observed, policy.allow, resolve);
	if (observed === undefined) return { allowed: false, reason: "policy_denied_observed" };
	if (input.declared !== undefined && await allowMatch(input.declared, policy.allow, resolve) === undefined) {
		return { allowed: false, reason: "policy_denied_declared", matched: observed };
	}
	for (const address of input.recorded ?? []) {
		if (await allowMatch(address, policy.allow, resolve) === undefined) {
			return { allowed: false, reason: "policy_denied_recorded", matched: observed };
		}
	}
	return { allowed: true, reason: "policy_segment", matched: observed };
}
