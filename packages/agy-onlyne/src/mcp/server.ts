import { McpServer, ResourceTemplate } from "@modelcontextprotocol/sdk/server/mcp.js";
import { RESOURCE_MIME_TYPE, registerAppResource, registerAppTool } from "@modelcontextprotocol/ext-apps/server";
import type { Json } from "@onlyne/lib";
import { z } from "zod/v4";
import type { CallerContext } from "../core/decider.ts";
import type { Caller } from "../core/urls.ts";
import { getGuiHtml } from "../gui/build-html.ts";
import type { DaemonClient } from "../runtime/daemon-client.ts";

export const ONLYNE_PANEL_URI = "ui://onlyne/panel";

export interface OnlyneMcpServerOptions {
	name?: string;
	version?: string;
	getClient: () => Promise<DaemonClient>;
	renderPanelHtml?: () => string | Promise<string>;
	getSamplingCallback?: () => { url: string; token: string } | undefined;
	getConversationId?: () => string | undefined;
	getCaller?: () => CallerContext | undefined;
}

const ALLOWED_PANEL_ACTIONS = new Set([
	"connect",
	"refresh",
	"trust/begin",
	"trust/confirm",
	"trust/accept",
	"trust/revoke",
	"trust/rotate",
	"acquisition",
	"language",
	"config",
	"send",
	"domain",
	"thread/create",
	"todo",
]);

function readCallerFromContext(caller?: CallerContext): Caller | undefined {
	if (!caller) return undefined;
	return { agent_id: caller.agent_id, main: caller.main };
}

function serializeToolError(err: unknown): {
	isError: true;
	content: Array<{ type: "text"; text: string }>;
} {
	const msg = err instanceof Error ? err.message : String(err);
	const code = err !== null && typeof err === "object" && "code" in err
		? String((err as { code?: unknown }).code ?? "error")
		: "error";
	return {
		isError: true,
		content: [{ type: "text", text: `[${code}] ${msg}` }],
	};
}

export function createOnlyneMcpServer(options: OnlyneMcpServerOptions): McpServer {
	const server = new McpServer(
		{
			name: options.name ?? "onlyne",
			version: options.version ?? "3.2.0",
		},
		{
			capabilities: {
				resources: {},
				tools: {},
			},
			instructions: [
				"Onlyne decentralized communication node.",
				"Use `msg({ delivery_kind, msg, meta })` as the single action entry for sending messages, replying to threads, drafting, managing trust, joining domains, or creating threads.",
				"Use `onlyne_read` (or read `onlyne://` resources) to inspect status, threads, peers, inbox, drafts, and handover reports.",
				"Use `onlyne_panel` to open the interactive Onlyne GUI panel.",
			].join(" "),
		},
	);

	// 1. Single Agent-Facing Message Entry: msg({ delivery_kind, msg, meta })
	server.registerTool(
		"msg",
		{
			title: "Onlyne Message",
			description: [
				"Single decentralized Onlyne v3 communication entry (SystemOne interpreted).",
				"Use delivery_kind + msg + optional meta to message peers, post or reply in threads,",
				"manage drafts (draft/send_draft), key exchange (trust), domains (domain), or threads (thread).",
			].join(" "),
			inputSchema: {
				delivery_kind: z
					.string()
					.min(1)
					.describe("Intent or delivery kind: note, question, answer, handoff, notify, request, report, decision, draft, send_draft, trust, domain, thread, wake, etc."),
				msg: z
					.string()
					.describe("Message body, title, instruction, or payload text."),
				meta: z
					.record(z.string(), z.unknown())
					.optional()
					.describe("Optional structured hints (to, thread, in_reply_to, relation, response, execution, work, op, domain_id, node_id, draft_id, delivery_kind, etc.)."),
			},
		},
		async args => {
			try {
				const client = await options.getClient();
				const caller = options.getCaller?.();
				const convId = options.getConversationId?.();
				const samplingCb = options.getSamplingCallback?.();
				const res = await client.msg(
					{
						delivery_kind: args.delivery_kind,
						msg: args.msg,
						...(args.meta === undefined ? {} : { meta: args.meta as Record<string, Json> }),
					},
					caller,
					samplingCb,
					convId,
				);
				return {
					content: [
						{
							type: "text" as const,
							text: res.text,
						},
					],
				};
			} catch (err) {
				return serializeToolError(err);
			}
		},
	);

	// 2. Read Tool for onlyne:// URIs
	server.registerTool(
		"onlyne_read",
		{
			title: "Read Onlyne URI",
			description: "Read a structured Onlyne v3 view (onlyne://status, onlyne://threads, onlyne://threads/<owner>/<id>, onlyne://peers, onlyne://inbox, onlyne://drafts, onlyne://handover).",
			inputSchema: {
				url: z.string().min(1).describe("An onlyne:// URL to read."),
			},
		},
		async args => {
			try {
				const client = await options.getClient();
				const caller = readCallerFromContext(options.getCaller?.());
				const convId = options.getConversationId?.();
				const rendered = await client.read(args.url, caller, convId);
				return {
					content: [{ type: "text", text: rendered.content }],
				};
			} catch (err) {
				return serializeToolError(err);
			}
		},
	);

	// 3. MCP Resource Template for onlyne://{+path}
	server.registerResource(
		"onlyne-views",
		new ResourceTemplate("onlyne://{+path}", {
			list: async () => {
				try {
					const client = await options.getClient();
					const { uris } = await client.listUris();
					return {
						resources: uris.map(uri => ({
							uri,
							name: uri.replace(/^onlyne:\/\//, "") || "root",
							mimeType: "text/plain",
						})),
					};
				} catch {
					return {
						resources: [
							{ uri: "onlyne://", name: "root", mimeType: "text/plain" },
							{ uri: "onlyne://neighbors", name: "neighbors", mimeType: "text/plain" },
							{ uri: "onlyne://threads", name: "threads", mimeType: "text/plain" },
							{ uri: "onlyne://inbox", name: "inbox", mimeType: "text/plain" },
							{ uri: "onlyne://outbound", name: "outbound", mimeType: "text/plain" },
							{ uri: "onlyne://contexts", name: "contexts", mimeType: "text/plain" },
							{ uri: "onlyne://resources", name: "resources", mimeType: "text/plain" },
							{ uri: "onlyne://todo", name: "todo", mimeType: "text/plain" },
						],
					};
				}
			},
		}),
		{
			description: "Live decentralized Onlyne v3 node views",
			mimeType: "text/plain",
		},
		async uri => {
			const client = await options.getClient();
			const caller = readCallerFromContext(options.getCaller?.());
			const convId = options.getConversationId?.();
			const rendered = await client.read(uri.href, caller, convId);
			return {
				contents: [
					{
						uri: uri.href,
						mimeType: "text/plain",
						text: rendered.content,
					},
				],
			};
		},
	);

	// 4. Antigravity MCP App Entrypoint Tool + Resource (GUI Panel)
	registerAppTool(
		server,
		"onlyne_panel",
		{
			title: "Onlyne Panel",
			description: "Open the interactive Onlyne decentralized communication panel (threads, key exchange, discovery, trust policy, node setup).",
			inputSchema: {
				view: z.enum(["thread", "inbox", "trust", "todos", "setup"]).optional(),
			},
			_meta: {
				ui: {
					resourceUri: ONLYNE_PANEL_URI,
				},
				"com.google.antigravity/ui": {
					entrypoints: [
						{ type: "aux_pane", title: "Onlyne" },
						{ type: "global", title: "Onlyne" },
					],
				},
			},
		},
		async args => {
			try {
				const client = await options.getClient();
				const snap = await client.snapshot();
				return {
					content: [
						{
							type: "text" as const,
							text: JSON.stringify(
								{
									view: args.view ?? "thread",
									configured: snap.configured,
									status: snap.status,
									gui_url: client.guiUrl,
								},
								null,
								2,
							),
						},
					],
				};
			} catch (err) {
				return serializeToolError(err);
			}
		},
	);

	registerAppResource(
		server,
		"Onlyne Panel",
		ONLYNE_PANEL_URI,
		{
			description: "Onlyne v3 interactive GUI panel",
			mimeType: RESOURCE_MIME_TYPE,
		},
		async () => {
			const html = options.renderPanelHtml
				? await options.renderPanelHtml()
				: getGuiHtml();
			return {
				contents: [
					{
						uri: ONLYNE_PANEL_URI,
						mimeType: RESOURCE_MIME_TYPE,
						text: html,
					},
				],
			};
		},
	);

	// 5. App-Only UI Tools (hidden from LLM: visibility: ["app"])
	registerAppTool(
		server,
		"onlyne_panel_snapshot",
		{
			title: "Onlyne Panel Snapshot",
			description: "Fetch live Onlyne node snapshot for the GUI panel.",
			inputSchema: {},
			_meta: {
				ui: {
					resourceUri: ONLYNE_PANEL_URI,
					visibility: ["app"],
				},
			},
		},
		async () => {
			try {
				const client = await options.getClient();
				const snap = await client.snapshot();
				return {
					content: [{ type: "text" as const, text: JSON.stringify(snap) }],
				};
			} catch (err) {
				return serializeToolError(err);
			}
		},
	);

	registerAppTool(
		server,
		"onlyne_panel_action",
		{
			title: "Onlyne Panel Action",
			description: "Dispatch an interactive GUI action to the Onlyne daemon.",
			inputSchema: {
				action: z.string().min(1),
				payload: z.record(z.string(), z.unknown()).optional(),
			},
			_meta: {
				ui: {
					resourceUri: ONLYNE_PANEL_URI,
					visibility: ["app"],
				},
			},
		},
		async args => {
			try {
				if (!ALLOWED_PANEL_ACTIONS.has(args.action)) {
					throw new Error(`Disallowed panel action: ${args.action}`);
				}
				const client = await options.getClient();
				const result = await client.request<unknown>(
					"POST",
					`/api/actions/${args.action}`,
					args.payload ?? {},
				);
				return {
					content: [{ type: "text" as const, text: JSON.stringify(result) }],
				};
			} catch (err) {
				return serializeToolError(err);
			}
		},
	);

	return server;
}
