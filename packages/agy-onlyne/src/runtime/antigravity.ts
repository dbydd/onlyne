import { uriToFsPath } from "./workspace.ts";

export interface ConversationMetadata {
	conversationId: string;
	parentConversationId?: string;
	rootConversationId?: string;
	isSubagent: boolean;
	workspacePaths: string[];
}

export interface AgentApiRunner {
	exec(args: string[], timeoutMs?: number): Promise<{ exitCode: number; stdout: string; stderr: string }>;
}

const defaultRunner: (exe: string) => AgentApiRunner = exe => ({
	async exec(args, timeoutMs = 8_000): Promise<{ exitCode: number; stdout: string; stderr: string }> {
		const proc = Bun.spawn([exe, ...args], { stdout: "pipe", stderr: "pipe" });
		const timer = setTimeout(() => {
			try {
				proc.kill();
			} catch {
				// Ignore kill failure when process already exited.
			}
		}, timeoutMs);
		try {
			const [stdout, stderr, exitCode] = await Promise.all([
				new Response(proc.stdout).text(),
				new Response(proc.stderr).text(),
				proc.exited,
			]);
			return { exitCode, stdout, stderr };
		} finally {
			clearTimeout(timer);
		}
	},
});

export class AntigravityBridge {
	constructor(private readonly runner: AgentApiRunner) {}

	static fromEnv(env: Record<string, string | undefined> = process.env): AntigravityBridge | undefined {
		const exe = env.ANTIGRAVITY_AGENTAPI_EXE;
		if (!exe || exe.trim().length === 0) return undefined;
		if (!env.ANTIGRAVITY_LS_ADDRESS || !env.ANTIGRAVITY_CSRF_TOKEN) return undefined;
		return new AntigravityBridge(defaultRunner(exe.trim()));
	}

	async getConversationMetadata(conversationId: string): Promise<ConversationMetadata | undefined> {
		if (!conversationId) return undefined;
		const res = await this.runner.exec(["agentapi", "get-conversation-metadata", conversationId], 6_000);
		if (res.exitCode !== 0) return undefined;
		try {
			const parsed = JSON.parse(res.stdout) as {
				response?: {
					conversationMetadata?: {
						metadata?: {
							parentConversationId?: string;
							rootConversationId?: string;
							subagentSpec?: unknown;
							workspaceUris?: string[];
							workspaces?: Array<{ workspaceFolderAbsoluteUri?: string; gitRootAbsoluteUri?: string }>;
						};
					};
				};
			};
			const meta = parsed.response?.conversationMetadata?.metadata;
			if (!meta) return undefined;
			const uris = new Set<string>(meta.workspaceUris ?? []);
			for (const ws of meta.workspaces ?? []) {
				if (ws.workspaceFolderAbsoluteUri) uris.add(ws.workspaceFolderAbsoluteUri);
				if (ws.gitRootAbsoluteUri) uris.add(ws.gitRootAbsoluteUri);
			}
			const workspacePaths = [...uris]
				.map(raw => uriToFsPath(raw))
				.filter((path): path is string => path !== undefined);
			const parentConversationId = meta.parentConversationId && meta.parentConversationId.length > 0
				? meta.parentConversationId
				: undefined;
			const rootConversationId = meta.rootConversationId && meta.rootConversationId.length > 0
				? meta.rootConversationId
				: undefined;
			const isSubagent = Boolean(meta.subagentSpec) || (parentConversationId !== undefined && parentConversationId !== conversationId);
			return {
				conversationId,
				...(parentConversationId === undefined ? {} : { parentConversationId }),
				...(rootConversationId === undefined ? {} : { rootConversationId }),
				isSubagent,
				workspacePaths,
			};
		} catch {
			return undefined;
		}
	}

	async sendMessage(conversationId: string, content: string, title?: string): Promise<boolean> {
		if (!conversationId || !content) return false;
		const args = ["agentapi", "send-message"];
		if (title && title.trim().length > 0) args.push(`--title=${title.trim()}`);
		args.push(conversationId, content);
		const res = await this.runner.exec(args, 10_000);
		return res.exitCode === 0;
	}

	async newConversation(
		prompt: string,
		options?: { model?: string; title?: string; profile?: string },
	): Promise<{ conversationId?: string; ok: boolean; error?: string }> {
		const args = ["agentapi", "new-conversation"];
		if (options?.model) args.push(`--model=${options.model}`);
		if (options?.title) args.push(`--title=${options.title}`);
		if (options?.profile) args.push(`--profile=${options.profile}`);
		args.push(prompt);
		const res = await this.runner.exec(args, 12_000);
		if (res.exitCode !== 0) {
			return { ok: false, error: res.stderr.trim() || res.stdout.trim() || `exit ${res.exitCode}` };
		}
		try {
			const parsed = JSON.parse(res.stdout) as { conversationId?: string; response?: { conversationId?: string } };
			const cid = parsed.conversationId ?? parsed.response?.conversationId;
			return { ok: true, ...(cid ? { conversationId: cid } : {}) };
		} catch {
			const match = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/i.exec(res.stdout);
			return { ok: true, ...(match ? { conversationId: match[0] } : {}) };
		}
	}
}
