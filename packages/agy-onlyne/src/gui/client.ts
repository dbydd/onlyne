import {
	App,
	PostMessageTransport,
	applyDocumentTheme,
	applyHostFonts,
	applyHostStyleVariables,
} from "@modelcontextprotocol/ext-apps";
import type { Snapshot } from "../core/types.ts";

export interface GuiTransport {
	snapshot(): Promise<Snapshot>;
	action(name: string, payload?: Record<string, unknown>): Promise<unknown>;
	subscribe(onUpdate: () => void): () => void;
	hostActions(): {
		supportsModelContext: boolean;
		supportsSendMessage: boolean;
		updateAgentContext(text: string): Promise<void>;
		sendAgentMessage(text: string): Promise<void>;
	};
}

interface InjectedHttpConfig {
	ctrlUrl?: string;
	token: string;
}

declare global {
	interface Window {
		__ONLYNE_HTTP__?: InjectedHttpConfig;
	}
}

function parseToolTextJson(result: { content?: Array<{ type?: string; text?: string }>; isError?: boolean }): unknown {
	const firstText = result.content?.find(c => c.type === "text" && typeof c.text === "string")?.text;
	if (result.isError) {
		throw new Error(firstText || "Tool action failed");
	}
	if (!firstText) return {};
	try {
		return JSON.parse(firstText);
	} catch {
		return { text: firstText };
	}
}

function createHttpTransport(cfg: InjectedHttpConfig): GuiTransport {
	const base = cfg.ctrlUrl ?? window.location.origin;
	const headers = (withBody = false): Record<string, string> => ({
		authorization: `Bearer ${cfg.token}`,
		...(withBody ? { "content-type": "application/json" } : {}),
	});

	const fetchJson = async <T>(method: string, path: string, body?: unknown): Promise<T> => {
		const res = await fetch(`${base}${path}`, {
			method,
			headers: headers(body !== undefined),
			...(body !== undefined ? { body: JSON.stringify(body) } : {}),
		});
		const text = await res.text();
		const parsed = text.trim() ? JSON.parse(text) as Record<string, unknown> : {};
		if (!res.ok) {
			throw new Error(typeof parsed.error === "string" ? parsed.error : `HTTP ${res.status}`);
		}
		return parsed as T;
	};

	return {
		snapshot: () => fetchJson<Snapshot>("GET", "/api/snapshot"),
		action: (name, payload) => fetchJson("POST", `/api/actions/${name}`, payload ?? {}),
		subscribe: onUpdate => {
			const es = new EventSource(`${base}/events?token=${encodeURIComponent(cfg.token)}`);
			const handler = (): void => {
				onUpdate();
			};
			for (const ev of ["snapshot", "delivery", "wake", "config", "ready"]) {
				es.addEventListener(ev, handler);
			}
			es.onerror = () => undefined;
			return () => {
				es.close();
			};
		},
		hostActions: () => ({
			supportsModelContext: false,
			supportsSendMessage: false,
			updateAgentContext: async () => undefined,
			sendAgentMessage: async () => undefined,
		}),
	};
}

async function createMcpAppTransport(): Promise<GuiTransport> {
	const app = new App({ name: "onlyne-panel", version: "3.2.0" }, {});

	const syncHostContext = (ctx: ReturnType<App["getHostContext"]>): void => {
		if (!ctx) return;
		if (ctx.theme) applyDocumentTheme(ctx.theme);
		if (ctx.styles?.variables) applyHostStyleVariables(ctx.styles.variables);
		if (ctx.styles?.css?.fonts) applyHostFonts(ctx.styles.css.fonts);
	};

	app.onhostcontextchanged = ctx => {
		syncHostContext(ctx);
	};

	await app.connect(new PostMessageTransport(window.parent, window.parent));
	syncHostContext(app.getHostContext());

	return {
		snapshot: async () => {
			const res = await app.callServerTool({
				name: "onlyne_panel_snapshot",
				arguments: {},
			});
			return parseToolTextJson(res) as Snapshot;
		},
		action: async (name, payload) => {
			const res = await app.callServerTool({
				name: "onlyne_panel_action",
				arguments: { action: name, ...(payload === undefined ? {} : { payload }) },
			});
			return parseToolTextJson(res);
		},
		subscribe: onUpdate => {
			app.ontoolresult = () => {
				onUpdate();
			};
			const timer = window.setInterval(() => {
				onUpdate();
			}, 3_500);
			return () => {
				window.clearInterval(timer);
				app.ontoolresult = undefined;
			};
		},
		hostActions: () => {
			const caps = app.getHostCapabilities();
			return {
				supportsModelContext: Boolean(caps?.updateModelContext),
				supportsSendMessage: Boolean(caps?.message),
				updateAgentContext: async text => {
					await app.updateModelContext({ content: [{ type: "text", text }] });
				},
				sendAgentMessage: async text => {
					await app.sendMessage({ role: "user", content: [{ type: "text", text }] });
				},
			};
		},
	};
}

export async function createGuiTransport(): Promise<GuiTransport> {
	if (typeof window !== "undefined" && window.__ONLYNE_HTTP__?.token) {
		return createHttpTransport(window.__ONLYNE_HTTP__);
	}
	if (typeof window !== "undefined") {
		const params = new URLSearchParams(window.location.search);
		const urlToken = params.get("token");
		if (urlToken) {
			return createHttpTransport({ token: urlToken });
		}
	}
	return await createMcpAppTransport();
}
