import type { ExtensionAPI } from "@oh-my-pi/pi-coding-agent";
import { AgentRegistry } from "@oh-my-pi/pi-coding-agent/registry/agent-registry";
import { getRuntime, type PluginRuntime } from "./runtime.ts";

let registryRegistered = false;

/**
 * Task-spawned sessions are bound to the context alias their dispatch named.
 * The name is only the marker: the registry reference must still prove the
 * session is a child of this node's main agent before any record is bound.
 */
export function registerContextBinding(pi: ExtensionAPI): void {
	pi.on("tool_result", async (event, _ctx) => {
		if (event.toolName !== "task") return;
		const runtime = getRuntime();
		if (runtime === undefined) return;
		try {
			const input = event.input;
			const items = Array.isArray(input.tasks) ? input.tasks : [input];
			const details = event.details as
				| { progress?: unknown; results?: unknown }
				| undefined;
			for (const candidate of [...array(details?.progress), ...array(details?.results)]) {
				const id = stringField(candidate, "id");
				if (id === undefined) continue;
				const index = numberField(candidate, "index");
				const name = index === undefined ? undefined : stringField(items[index], "name");
				const alias = aliasOf(name);
				if (alias === undefined) continue;
				await bind(runtime, alias, id);
			}
		} catch (error) {
			pi.logger.warn("onlyne context binding failed", {
				error: error instanceof Error ? error.message : String(error),
			});
		}
	});

	if (registryRegistered) return;
	registryRegistered = true;
	AgentRegistry.global().onChange(event => {
		const runtime = getRuntime();
		if (runtime === undefined) return;
		if (event.type === "removed") {
			void updateState(runtime, event.ref.id, "lost").catch(report(pi));
			return;
		}
		if (event.type === "status_changed") {
			void updateState(runtime, event.ref.id, stateOf(event.ref.status)).catch(report(pi));
			return;
		}
		const alias = aliasOf(event.ref.displayName);
		if (alias === undefined) return;
		void bind(runtime, alias, event.ref.id).catch(report(pi));
	});
}

async function bind(
	runtime: PluginRuntime,
	alias: string,
	agentId: string,
): Promise<void> {
	const existing = await runtime.contexts.find(alias);
	if (existing === undefined) return;
	if (existing.node_id !== runtime.config.node.id) return;
	if (existing.main_agent_id !== runtime.mainAgentId) return;
	if (existing.agent_id !== undefined && existing.agent_id !== agentId) return;
	if (existing.agent_id === agentId && existing.session_file !== undefined) return;
	const ref = AgentRegistry.global().get(agentId);
	if (ref === undefined || ref.kind !== "sub" || ref.parentId !== runtime.mainAgentId) return;
	await runtime.contexts.bind(alias, {
		agent_id: agentId,
		...(ref.sessionFile === null || ref.sessionFile === undefined ? {} : { session_file: ref.sessionFile }),
		...(ref.parentId === undefined ? {} : { parent_id: ref.parentId }),
	});
}

/** An alias-recorded update follows the bound agent id, never a display name. */
async function updateState(
	runtime: PluginRuntime,
	agentId: string,
	state: "idle" | "running" | "parked" | "lost",
): Promise<void> {
	const records = await runtime.contexts.list();
	const record = records.find(candidate =>
		candidate.node_id === runtime.config.node.id &&
		candidate.main_agent_id === runtime.mainAgentId &&
		candidate.agent_id === agentId,
	);
	if (record === undefined) return;
	await runtime.contexts.updateState(record.alias, state);
}

/**
 * The task name carries the alias; the host may append a numeric suffix to a
 * repeated name. Nothing else about the name is trusted.
 */
function aliasOf(name: string | undefined): string | undefined {
	if (name === undefined) return undefined;
	return /^onlyne-(c-[0-9]+)(?:-[0-9]+)?$/.exec(name)?.[1];
}

function array(value: unknown): unknown[] {
	return Array.isArray(value) ? value : [];
}

function stringField(value: unknown, key: string): string | undefined {
	if (value === null || typeof value !== "object") return undefined;
	const found = (value as Record<string, unknown>)[key];
	return typeof found === "string" && found.length > 0 ? found : undefined;
}

function numberField(value: unknown, key: string): number | undefined {
	if (value === null || typeof value !== "object") return undefined;
	const found = (value as Record<string, unknown>)[key];
	return typeof found === "number" && Number.isInteger(found) ? found : undefined;
}

function stateOf(status: string): "idle" | "running" | "parked" | "lost" {
	if (status === "running") return "running";
	if (status === "parked") return "parked";
	if (status === "aborted") return "lost";
	return "idle";
}

function report(pi: ExtensionAPI): (error: unknown) => void {
	return error => {
		pi.logger.warn("onlyne context state update failed", {
			error: error instanceof Error ? error.message : String(error),
		});
	};
}
