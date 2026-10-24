import { networkInterfaces } from "node:os";
import { OnlyneError, OnlyneNode, type Action, type Auth, type Json, type MsgInput, type Runtime, type Storage, type Target } from "@onlyne/lib";
import type { ContextRegistry } from "./store.ts";
import type { OnlyneConfig } from "./types.ts";

export interface NodeDeps {
	config: OnlyneConfig;
	endpoint: string;
	instanceId: string;
	storage: Storage;
	contexts: ContextRegistry;
	decide(input: MsgInput): Promise<Action>;
	runtime: Runtime;
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
	if (config.node.key_ref) throw new OnlyneError("unsupported_delivery", "Credential exchange is not implemented. Leave key_ref empty for the loopback development binding.");
	if (!["127.0.0.1", "::1", "localhost"].includes(config.server.bind)) {
		throw new OnlyneError("unsupported_delivery", "The unauthenticated development binding requires a loopback bind host. Remote authentication needs the security binding.");
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
	node = new OnlyneNode({
		node_id: config.node.id, base_url: deps.endpoint, instance_id: deps.instanceId,
		authentication: ["none"], plugins: {
			storage: deps.storage, authenticate, resolve, decide: deps.decide, runtime: deps.runtime,
			domainCandidates: id => node.discovery.candidates(id),
			mayAdmit: actor => known(actor),
			inboundThread: actor => known(actor),
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
		},
	});
	return node;
}
