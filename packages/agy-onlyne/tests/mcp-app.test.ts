import { afterEach, describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { RESOURCE_MIME_TYPE } from "@modelcontextprotocol/ext-apps/server";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { InMemoryTransport } from "@modelcontextprotocol/sdk/inMemory.js";
import { configPath, normalizeNodeId } from "../src/config.ts";
import { getGuiHtml } from "../src/gui/build-html.ts";
import { ONLYNE_PANEL_URI, createOnlyneMcpServer } from "../src/mcp/server.ts";
import { DaemonServer } from "../src/runtime/daemon.ts";
import { DaemonClient } from "../src/runtime/daemon-client.ts";

const cleanups: Array<() => Promise<void>> = [];
const tempDirs: string[] = [];

function makeTempDir(prefix: string): string {
	const dir = mkdtempSync(join(tmpdir(), prefix));
	tempDirs.push(dir);
	return dir;
}

afterEach(async () => {
	while (cleanups.length > 0) {
		await cleanups.pop()!().catch(() => {});
	}
	while (tempDirs.length > 0) {
		rmSync(tempDirs.pop()!, { recursive: true, force: true });
	}
});

describe("MCP Server & Antigravity MCP App GUI Panel", () => {
	test("registers msg, onlyne_read, onlyne:// resource template, onlyne_panel entrypoints, and app-only snapshot/action tools", async () => {
		const dir = makeTempDir("agy-mcp-");
		const daemon = await DaemonServer.start({
			cwd: dir,
			renderGuiHtml: getGuiHtml,
			runtimeOptions: {
				inMemory: true,
				discovery: false,
				bindHttp: true,
				config: {
					path: configPath(dir),
					region: "urn:onlyne:region:local",
					node: { id: normalizeNodeId("mcp-panel-node"), key_scope: "project" },
					server: { bind: "127.0.0.1" },
					threads: { page_size: 50 },
					judge: { mode: "none" },
				},
			},
		});
		const client = DaemonClient.fromInfo(daemon.info());

		const server = createOnlyneMcpServer({
			getClient: async () => client,
			renderPanelHtml: getGuiHtml,
			getConversationId: () => "conv-mcp-main",
			getCaller: () => ({ agent_id: "conv-mcp-main", main: true }),
		});

		const [clientTransport, serverTransport] = InMemoryTransport.createLinkedPair();
		const mcpClient = new Client({ name: "test-host", version: "1.0.0" });
		await Promise.all([
			server.connect(serverTransport),
			mcpClient.connect(clientTransport),
		]);
		cleanups.push(async () => {
			await mcpClient.close();
			await server.close();
			await daemon.close();
		});

		// 1. Inspect tool list and Antigravity UI / MCP App metadata
		const listed = await mcpClient.listTools();
		const byName = new Map(listed.tools.map(tool => [tool.name, tool]));
		expect(byName.has("msg")).toBe(true);
		expect(byName.has("onlyne_read")).toBe(true);
		expect(byName.has("onlyne_panel")).toBe(true);
		expect(byName.has("onlyne_panel_snapshot")).toBe(true);
		expect(byName.has("onlyne_panel_action")).toBe(true);

		const panelTool = byName.get("onlyne_panel")!;
		const panelMeta = panelTool._meta as Record<string, unknown>;
		expect((panelMeta.ui as { resourceUri?: string }).resourceUri).toBe(ONLYNE_PANEL_URI);
		const agyUi = panelMeta["com.google.antigravity/ui"] as { entrypoints?: Array<{ type: string; title: string }> };
		expect(agyUi.entrypoints?.map(e => e.type)).toEqual(["aux_pane", "global"]);

		const appSnapshotMeta = byName.get("onlyne_panel_snapshot")!._meta as { ui?: { visibility?: string[] } };
		const appActionMeta = byName.get("onlyne_panel_action")!._meta as { ui?: { visibility?: string[] } };
		expect(appSnapshotMeta.ui?.visibility).toEqual(["app"]);
		expect(appActionMeta.ui?.visibility).toEqual(["app"]);

		// 2. Read ui://onlyne/panel MCP App HTML resource
		const res = await mcpClient.readResource({ uri: ONLYNE_PANEL_URI });
		expect(res.contents[0]?.mimeType).toBe(RESOURCE_MIME_TYPE);
		const html = (res.contents[0] as { text?: string }).text ?? "";
		expect(html.toLowerCase()).toContain("<!doctype html>");
		expect(html).toContain("onlyne_panel_action");

		// 3. Invoke onlyne_panel, onlyne_read, and onlyne:// resource
		const panelCall = await mcpClient.callTool({ name: "onlyne_panel", arguments: { view: "trust" } });
		const panelJson = JSON.parse((panelCall.content as Array<{ text: string }>)[0]!.text) as {
			view: string;
			status: { node_id: string };
			gui_url: string;
		};
		expect(panelJson.view).toBe("trust");
		expect(panelJson.status.node_id).toBe("urn:onlyne:agent:mcp-panel-node");
		expect(panelJson.gui_url).toContain("/ui?token=");

		const readRes = await mcpClient.callTool({ name: "onlyne_read", arguments: { url: "onlyne://" } });
		expect((readRes.content as Array<{ text: string }>)[0]!.text).toContain("mcp-panel-node");

		const directResource = await mcpClient.readResource({ uri: "onlyne://neighbors" });
		expect((directResource.contents[0] as { text?: string }).text).toContain("# Onlyne Neighbors");

		// 4. App-facing snapshot and action updates locale
		const langRes = await mcpClient.callTool({
			name: "onlyne_panel_action",
			arguments: { action: "language", payload: { language: "zh-CN" } },
		});
		expect(langRes.isError).not.toBe(true);

		const snapRes = await mcpClient.callTool({ name: "onlyne_panel_snapshot", arguments: {} });
		const snap = JSON.parse((snapRes.content as Array<{ text: string }>)[0]!.text) as { language: string };
		expect(snap.language).toBe("zh-CN");

		const badAction = await mcpClient.callTool({
			name: "onlyne_panel_action",
			arguments: { action: "not_allowed" },
		});
		expect(badAction.isError).toBe(true);
	});
});
