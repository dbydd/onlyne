import { existsSync } from "node:fs";
import { configPath } from "../config.ts";
import type { HookEventInput, HookResultPayload } from "../runtime/delivery.ts";
import { readDaemonInfo } from "../runtime/daemon-client.ts";
import type { DaemonClient } from "../runtime/daemon-client.ts";
import { resolveWorkspaceRoot } from "../runtime/workspace.ts";

export interface HookHandlerOptions {
	stdinText: string;
	env?: Record<string, string | undefined>;
	getClient: (cwd: string) => Promise<DaemonClient>;
	hasConfiguredWorkspace?: (cwd: string) => Promise<boolean> | boolean;
}

function parseHookInput(stdinText: string): HookEventInput | undefined {
	const trimmed = stdinText.trim();
	if (!trimmed) return undefined;
	try {
		const raw = JSON.parse(trimmed) as Partial<HookEventInput>;
		if (raw === null || typeof raw !== "object" || typeof raw.hookEventName !== "string") {
			return undefined;
		}
		return {
			hookEventName: raw.hookEventName,
			...(typeof raw.conversationId === "string" ? { conversationId: raw.conversationId } : {}),
			...(Array.isArray(raw.workspacePaths) ? { workspacePaths: raw.workspacePaths.filter((p): p is string => typeof p === "string") } : {}),
			...(typeof raw.transcriptPath === "string" ? { transcriptPath: raw.transcriptPath } : {}),
			...(typeof raw.modelName === "string" ? { modelName: raw.modelName } : {}),
			...(typeof raw.lastUserInput === "string" ? { lastUserInput: raw.lastUserInput } : {}),
			...(typeof raw.cwd === "string" ? { cwd: raw.cwd } : {}),
		};
	} catch {
		return undefined;
	}
}

export async function handleHookEvent(options: HookHandlerOptions): Promise<HookResultPayload> {
	const input = parseHookInput(options.stdinText);
	if (!input) return {};

	const firstWorkspace = input.cwd ?? input.workspacePaths?.find(p => p.trim().length > 0);
	const cwd = await resolveWorkspaceRoot({
		cwd: firstWorkspace,
		env: options.env,
	});

	const isReady = options.hasConfiguredWorkspace
		? await options.hasConfiguredWorkspace(cwd)
		: (existsSync(configPath(cwd)) || (await readDaemonInfo(cwd)) !== undefined);
	if (!isReady) {
		return {};
	}

	try {
		const client = await options.getClient(cwd);
		return await client.processHook(input);
	} catch {
		return {};
	}
}
