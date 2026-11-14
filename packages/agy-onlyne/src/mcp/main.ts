import { randomBytes, timingSafeEqual } from "node:crypto";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { dirname, join } from "node:path";
import { DaemonClient, defaultDaemonCommand } from "../runtime/daemon-client.ts";
import { resolveWorkspaceRoot } from "../runtime/workspace.ts";
import { createOnlyneMcpServer } from "./server.ts";

function safeEqual(a: string, b: string): boolean {
	const ab = Buffer.from(a);
	const bb = Buffer.from(b);
	return ab.length === bb.length && timingSafeEqual(ab, bb);
}

function defaultCliEntryPath(): string {
	const here = import.meta.path;
	if (here.endsWith("/dist/mcp.js") || here.endsWith("/plugin/dist/mcp.js")) {
		return join(dirname(here), "cli.js");
	}
	if (here.endsWith("/dist/mcp/main.js")) {
		return join(dirname(here), "../cli/main.js");
	}
	return join(dirname(here), "../cli/main.ts");
}

function createLoopbackSamplingBridge(getServer: () => ReturnType<typeof createOnlyneMcpServer>) {
	const token = randomBytes(24).toString("hex");
	const srv = Bun.serve({
		hostname: "127.0.0.1",
		port: 0,
		fetch: async req => {
			const url = new URL(req.url);
			if (req.method !== "POST" || url.pathname !== "/sampling") {
				return new Response("Not Found", { status: 404 });
			}
			const auth = req.headers.get("authorization") ?? "";
			const got = auth.toLowerCase().startsWith("bearer ") ? auth.slice(7).trim() : "";
			if (!safeEqual(got, token)) {
				return Response.json({ error: "Unauthorized" }, { status: 401 });
			}
			const server = getServer();
			const caps = server.server.getClientCapabilities();
			if (!caps?.sampling) {
				return Response.json({ error: "Client does not support MCP sampling" }, { status: 501 });
			}
			const body = await req.json() as {
				systemPrompt?: string;
				messages: Array<{ role: "user"; content: { type: "text"; text: string } }>;
				maxTokens?: number;
			};
			const res = await server.server.createMessage({
				systemPrompt: body.systemPrompt,
				messages: body.messages,
				maxTokens: body.maxTokens ?? 400,
			});
			const contentBlock = Array.isArray(res.content) ? res.content[0] : res.content;
			return Response.json({
				content: contentBlock ?? { type: "text", text: "" },
			});
		},
	});

	return {
		url: `http://127.0.0.1:${srv.port}/sampling`,
		token,
		close: () => srv.stop(true),
	};
}

export async function runOnlyneMcpStdio(): Promise<void> {
	let cachedClientPromise: Promise<DaemonClient> | undefined;
	let serverRef!: ReturnType<typeof createOnlyneMcpServer>;
	const cliEntry = defaultCliEntryPath();
	const samplingBridge = createLoopbackSamplingBridge(() => serverRef);

	const getClient = (): Promise<DaemonClient> => {
		if (!cachedClientPromise) {
			cachedClientPromise = (async () => {
				let mcpRoots: Array<{ uri: string; name?: string }> | undefined;
				try {
					const caps = serverRef.server.getClientCapabilities();
					if (caps?.roots) {
						const res = await serverRef.server.listRoots();
						mcpRoots = res.roots;
					}
				} catch {
					// Host might not expose roots cleanly on first tick
				}
				const workspace = await resolveWorkspaceRoot({ mcpRoots });
				return await DaemonClient.connectOrSpawn({
					cwd: workspace,
					spawnCommand: defaultDaemonCommand(cliEntry, workspace),
				});
			})();
		}
		return cachedClientPromise;
	};

	serverRef = createOnlyneMcpServer({
		getClient,
		getSamplingCallback: () => {
			const caps = serverRef.server.getClientCapabilities();
			return caps?.sampling ? { url: samplingBridge.url, token: samplingBridge.token } : undefined;
		},
		getConversationId: () => process.env.ANTIGRAVITY_CONVERSATION_ID || undefined,
	});

	const transport = new StdioServerTransport();
	await serverRef.connect(transport);

	const cleanup = (): void => {
		samplingBridge.close();
	};
	process.on("SIGINT", cleanup);
	process.on("SIGTERM", cleanup);
	transport.onclose = cleanup;
}

if (import.meta.main) {
	void runOnlyneMcpStdio().catch(err => {
		process.stderr.write(`onlyne-agy-mcp error: ${err instanceof Error ? err.stack ?? err.message : String(err)}\n`);
		process.exit(1);
	});
}
