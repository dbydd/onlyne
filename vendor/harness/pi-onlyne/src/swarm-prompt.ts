import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";

export interface SwarmPromptConfig {
	enabled: boolean;
	// If set, the template file path relative to the workspace root (e.g. "prompts/swarm-task.md").
	// Absolute paths are not supported; the file is read from the workspace root.
	template?: string;
	// Fallback inline template when no template file is configured.
	// Placeholders: {task_id}, {from}, {transfer_send_to}, {attempt}, {payload}
	fallback?: string;
}

const DEFAULT_PROMPT = `Onlyne swarm task {task_id} (from {from}):

{payload}

This session carries this one task only. Restore context from files, work, spawn continuations with swarm_send when another unit must continue, then exit with swarm_complete. Downstream results travel through files and the ledger; nothing waits here.`;

export function readSwarmPromptConfig(workspaceRoot: string): SwarmPromptConfig {
	const cfgPath = join(workspaceRoot, ".pi", "onlyne.json");
	if (!existsSync(cfgPath)) {
		return { enabled: true, fallback: DEFAULT_PROMPT };
	}
	try {
		const parsed = JSON.parse(readFileSync(cfgPath, "utf8"));
		const sw = parsed?.swarm_prompt;
		if (sw === false || (sw && sw.enabled === false)) return { enabled: false, fallback: DEFAULT_PROMPT };
		return {
			enabled: sw?.enabled !== false,
			template: sw?.template,
			fallback: sw?.fallback ?? DEFAULT_PROMPT,
		};
	} catch {
		return { enabled: true, fallback: DEFAULT_PROMPT };
	}
}

export function resolveSwarmPrompt(workspaceRoot: string, header: { task_id: string; from: string; transfer_send_to: string; attempt: number }, payload: string): string | null {
	const cfg = readSwarmPromptConfig(workspaceRoot);
	if (!cfg.enabled) return null;
	let tmpl = cfg.fallback ?? DEFAULT_PROMPT;
	if (cfg.template) {
		const abs = join(workspaceRoot, cfg.template);
		if (existsSync(abs)) {
			tmpl = readFileSync(abs, "utf8");
		}
	}
	return tmpl
		.replace(/{task_id}/g, header.task_id)
		.replace(/{from}/g, header.from)
		.replace(/{transfer_send_to}/g, header.transfer_send_to)
		.replace(/{attempt}/g, String(header.attempt))
		.replace(/{payload}/g, payload);
}
