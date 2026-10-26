import { networkInterfaces } from "node:os";
import { FetchTransport, OnlyneError, OnlyneNode, trustPhase, type Action, type Auth, type Json, type KeyHook, type KeyMaterial, type MaintenanceClock, type MsgInput, type Runtime, type SigningTransportOptions, type Storage, type Target } from "@onlyne/lib";
import type { ContextRegistry } from "./store.ts";
import type { OnlyneConfig } from "./types.ts";
import { loadOrCreateKey, writeKeyMaterial } from "./keys.ts";

export interface NodeDeps {
	config: OnlyneConfig;
	endpoint: string;
	instanceId: string;
	storage: Storage;
	contexts: ContextRegistry;
	decide(input: MsgInput): Promise<Action>;
	runtime: Runtime;
	/** The timer primitives of the library's own maintenance loop (RFC 0017 §6). */
	clock: MaintenanceClock;
	/** The directory the host resolved for this node's identity key; absent keeps the loopback development binding. */
	keyDir?: string;
}

export function advertisedHost(bind: string): string {
	if (bind !== "0.0.0.0" && bind !== "::") return bind;
	for (const addresses of Object.values(networkInterfaces())) {
		for (const address of addresses ?? []) {
			if (!address.internal && address.family === "IPv4") return address.address;
		}
	}
	throw new OnlyneError("unavailable", "No concrete network address is available for this bind host.");
}

export function createOnlyneNode(deps: NodeDeps): OnlyneNode {
	const { config } = deps;
	if (config.node.key_ref) throw new OnlyneError("unsupported_delivery", "External credential references are not implemented; use the managed node key.");
	// Without a key directory the node keeps the loopback development binding (RFC 0001 §1).
	if (deps.keyDir === undefined && !["127.0.0.1", "::1", "localhost"].includes(config.server.bind)) {
		throw new OnlyneError("unsupported_delivery", "The unauthenticated development binding requires a loopback bind host. Create a node key to serve a remote address.");
	}
	let node: OnlyneNode;
	const authenticate = async (request: Request): Promise<Auth | null> => {
		const actor = request.headers.get("Onlyne-Actor");
		if (!actor || !/^[A-Za-z][A-Za-z0-9+.-]*:[^\s]+$/.test(actor)) return null;
		if (actor === config.node.id) throw new OnlyneError("forbidden", "HTTP callers cannot claim this node's management identity.");
		return { actor };
	};
	const resolve = async (id: string): Promise<Target> => {
		const target = id === config.node.id
			? { node_id: id, base_url: deps.endpoint }
			: await node.discovery.resolve(id);
		return { ...target, headers: { "Onlyne-Actor": config.node.id } };
	};
	const known = async (id: string): Promise<boolean> => {
		if (id === config.node.id) return true;
		return node.discovery.known(id);
	};
	const keyDir = deps.keyDir;
	const keys: KeyHook | undefined = keyDir === undefined ? undefined : Object.assign(
		() => loadOrCreateKey(keyDir, config.node.id),
		{ replace: (material: KeyMaterial) => writeKeyMaterial(keyDir, material, { replace: true }) },
	);
	// The library's own signing transport, built here for one reason: a missing trust row has to
	// end at this host's menu. Signing still happens only for a keyed node, and the trust reads
	// resolve lazily because the service exists only once the node is built.
	const signing: SigningTransportOptions | undefined = keys === undefined ? undefined : {
		trust: {
			entry: id => node.trust.entry(id),
			signRequest: (request, target, raw) => node.trust.signRequest(request, target, raw),
			verifyResponse: (target, request, response, body) => node.trust.verifyResponse(target, request, response, body),
		},
		guidance: details => ({
			entry: details.required_action === "confirm_exchange"
				? ["/onlyne", "联系人", "待确认", "同意"]
				: ["/onlyne", "发现", "请求交换密钥"],
		}),
	};
	node = new OnlyneNode({
		node_id: config.node.id, base_url: deps.endpoint, instance_id: deps.instanceId,
		now: () => Date.now(), id: () => crypto.randomUUID(),
		authentication: keys === undefined ? ["none"] : ["signature"], plugins: {
			storage: deps.storage, authenticate, resolve, decide: deps.decide, runtime: deps.runtime, maintenanceClock: deps.clock,
			...(keys === undefined ? {} : { keys }),
			...(signing === undefined ? {} : { transport: new FetchTransport(resolve, fetch, signing) }),
			...(config.trust === undefined ? {} : { trust_seeds: config.trust }),
			domainCandidates: id => node.discovery.candidates(id),
			// The keyless development binding checks no identity (RFC 0001 §1): every declared
			// actor may open a thread. A keyed node admits only peers it has verified.
			mayAdmit: async actor => keys === undefined || await known(actor),
			inboundThread: async actor => keys === undefined || await known(actor),
			executionContexts: async () => (await deps.contexts.list()).map(record => ({ alias: record.alias, summary: record.summary, state: record.state })) as Json,
			mention: async alias => {
				const name = alias.replace(/^@/, "");
				if (name === "self" || name === config.node.id) return config.node.id;
				const matches = (await node.discovery.listRecords()).filter(record => record.node_id === name || record.node_id.split(":").at(-1) === name);
				const ids = [...new Set(matches.map(record => record.node_id))];
				if (ids.length !== 1) throw new OnlyneError(ids.length ? "ambiguous_target" : "not_found", `No unique discovered node is named ${name}.`);
				return ids[0]!;
			},
			directory: async () => ({ node: config.node.id, region: config.region, endpoint: deps.endpoint, peers: await node.discovery.listRecords(), contexts: await deps.contexts.list() }) as unknown as Json,
			// The same rows onlyne://neighbors renders: every observed node with its trust state.
			neighbors: async () => {
				const records = await node.discovery.listRecords();
				const trust = new Map((await node.trust.list(node.auth)).map(entry => [entry.node_id, entry]));
				return records.map(record => {
					const entry = trust.get(record.node_id);
					return {
						node_id: record.node_id, base_url: record.base_url,
						...(record.membership?.domain_id === undefined ? {} : { region: record.membership.domain_id }),
						...(record.last_seen === undefined ? {} : { last_seen: record.last_seen }),
						verified: record.verified_at !== undefined && Date.parse(record.expires_at) > Date.now(),
						phase: entry === undefined ? "no_exchange" : trustPhase(entry),
						fingerprint: entry?.fingerprint ?? null,
					};
				});
			},
		},
	});
	return node;
}
