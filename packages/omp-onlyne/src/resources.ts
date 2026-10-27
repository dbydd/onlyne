import { OnlyneError, trustPhase, type OnlyneNode, type ResourceReadQuery } from "@onlyne/lib";
import { hostText as t } from "./host-text.ts";

export async function resourceOwners(node: OnlyneNode): Promise<string[]> {
	const trusted = await node.trust.list(node.auth);
	return [node.auth.actor, ...trusted.filter(entry => trustPhase(entry) === "exchanged").map(entry => entry.node_id)];
}

export async function resourceQuery(node: OnlyneNode, meta: Record<string, unknown>): Promise<ResourceReadQuery> {
	const op = meta.op ?? "list";
	const cursor = typeof meta.cursor === "string" ? meta.cursor : undefined;
	const size = meta.page_size;
	if (size !== undefined && (typeof size !== "number" || !Number.isInteger(size) || size < 1 || size > 100)) {
		throw new OnlyneError("invalid_request", t("resourcePageSize"));
	}
	const page = { ...(cursor === undefined ? {} : { cursor }), ...(size === undefined ? {} : { page_size: size as number }) };
	if (op === "list") {
		if (meta.owner !== undefined && (typeof meta.owner !== "string" || meta.owner.length === 0)) throw new OnlyneError("invalid_request", t("resourceOwner"));
		const owners = typeof meta.owner === "string" ? [meta.owner] : await resourceOwners(node);
		return { op, owners, ...page };
	}
	const ref = meta.resource as { owner?: unknown; id?: unknown } | null | undefined;
	if (ref === null || typeof ref !== "object" || typeof ref.owner !== "string" || ref.owner.length === 0 || typeof ref.id !== "string" || ref.id.length === 0) {
		throw new OnlyneError("invalid_request", t("resourceRef"));
	}
	const resource = { owner: ref.owner, id: ref.id };
	if (op === "describe") return { op, resource };
	if (op === "log") {
		if (meta.since !== undefined && (typeof meta.since !== "string" || !/^(0|[1-9][0-9]*)$/.test(meta.since))) throw new OnlyneError("invalid_request", t("resourceSince"));
		return { op, resource, ...page, ...(meta.since === undefined ? {} : { since: meta.since as string }) };
	}
	if (op === "operations") return { op, resource, ...page };
	if (op === "event" && typeof meta.event_id === "string" && meta.event_id.length > 0) return { op, resource, event_id: meta.event_id };
	if (op === "status" && typeof meta.operation_id === "string" && meta.operation_id.length > 0) return { op, resource, operation_id: meta.operation_id };
	throw new OnlyneError("invalid_request", t("resourceOp"));
}
