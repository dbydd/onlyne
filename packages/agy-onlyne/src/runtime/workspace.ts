import { existsSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { configPath } from "../config.ts";
import { AntigravityBridge } from "./antigravity.ts";

export interface WorkspaceResolveOptions {
	cwd?: string;
	env?: Record<string, string | undefined>;
	mcpRoots?: ReadonlyArray<{ uri: string; name?: string }>;
	bridge?: AntigravityBridge;
}

export function uriToFsPath(raw: string): string | undefined {
	if (!raw) return undefined;
	if (raw.startsWith("file://")) {
		try {
			return fileURLToPath(raw);
		} catch {
			return undefined;
		}
	}
	return resolve(raw);
}

function isDir(path: string): boolean {
	try {
		return statSync(path).isDirectory();
	} catch {
		return false;
	}
}

function looksInsidePluginBundle(path: string): boolean {
	const norm = resolve(path);
	return norm.includes("/.gemini/config/plugins/") || norm.includes("/plugins/onlyne");
}

function walkForWorkspace(startDir: string): string | undefined {
	let current = resolve(startDir);
	if (looksInsidePluginBundle(current)) return undefined;
	while (true) {
		if (existsSync(configPath(current)) || existsSync(join(current, ".git"))) {
			return current;
		}
		const parent = dirname(current);
		if (parent === current || parent === "/") break;
		current = parent;
	}
	return undefined;
}

/**
 * Resolves the workspace root whose `.onlyne/` directory this Antigravity node owns.
 * Never returns the plugin installation bundle directory or `/`.
 */
export async function resolveWorkspaceRoot(options: WorkspaceResolveOptions = {}): Promise<string> {
	const env = options.env ?? process.env;
	if (options.cwd && options.cwd.trim().length > 0) {
		return resolve(options.cwd.trim());
	}
	const envWorkspace = env.ONLYNE_WORKSPACE ?? env.ONLYNE_CWD ?? env.ANTIGRAVITY_WORKSPACE;
	if (envWorkspace && envWorkspace.trim().length > 0 && isDir(envWorkspace.trim())) {
		return resolve(envWorkspace.trim());
	}

	const rootPaths = (options.mcpRoots ?? [])
		.map(root => uriToFsPath(root.uri))
		.filter((item): item is string => item !== undefined && isDir(item));
	if (rootPaths.length > 0) {
		const configured = rootPaths.find(dir => existsSync(configPath(dir)));
		if (configured !== undefined) return configured;
		const gitRoot = rootPaths.find(dir => existsSync(join(dir, ".git")));
		if (gitRoot !== undefined) return gitRoot;
		return rootPaths[0]!;
	}

	const convId = env.ANTIGRAVITY_CONVERSATION_ID;
	if (convId && convId.trim().length > 0) {
		const bridge = options.bridge ?? AntigravityBridge.fromEnv(env);
		if (bridge !== undefined) {
			const meta = await bridge.getConversationMetadata(convId.trim()).catch(() => undefined);
			const fromMeta = meta?.workspacePaths.find(dir => isDir(dir));
			if (fromMeta !== undefined) return resolve(fromMeta);
		}
	}

	const cwd = process.cwd();
	const walked = walkForWorkspace(cwd);
	if (walked !== undefined) return walked;
	if (!looksInsidePluginBundle(cwd) && cwd !== "/" && cwd !== homedir()) {
		return resolve(cwd);
	}
	return join(homedir(), ".config", "onlyne", "default-workspace");
}
