/*
 * Onlyne Desktop renderer.
 *
 * The host owns state and every effect: this file reads snapshots from
 * GET /api/state, mutates through POST /api/command, and treats
 * /api/events frames as the only streaming source. Nothing here invents a
 * bot answer or approves a dialog on its own.
 */

import { DEFAULT_MAX_AUTO_TURNS } from "../contracts.ts";
import type {
	Bot,
	Capability,
	ChatMessage,
	Conversation,
	DesktopEvent,
	DesktopSettings,
	DesktopState,
	Json,
	MediaItem,
	ModelChoice,
	RpcFrame,
	ThinkingLevel,
	WidgetEnvelope,
} from "../contracts.ts";
import type { DesktopUiApi, WidgetContext, WidgetHost, WidgetRegistry } from "./api.ts";
import { createWidgetHost, createWidgetRegistry } from "./widgets.ts";
import { registerBuiltinViews } from "./views.ts";
import { registerOmpPanels } from "./omp-panels.ts";
import { registerCharacterPanel } from "./character-panel.ts";
import { registerDeskSurfaces } from "./surfaces.ts";
import {
	BUILTIN_WORKSPACES,
	LAYOUT_STORAGE_KEY,
	defaultLayout,
	moveInRail,
	normalizeLayout,
	type DeskLayout,
	type WorkspaceSpec,
} from "./layout.ts";
import type { WorkspaceContribution } from "./api.ts";
import { chord, commandKey, formatDeskHash, formatWhen, middleEllipsis, parseDeskHash } from "../desk-format.ts";

const MOD = commandKey(navigator.platform, navigator.userAgent);

/* ------------------------------------------------------------------ */
/* small DOM helpers                                                   */
/* ------------------------------------------------------------------ */

type Child = Node | string | number | null | undefined | false;
type Attrs = Record<string, unknown>;

function h<K extends keyof HTMLElementTagNameMap>(
	tag: K,
	attrs: Attrs = {},
	...children: Child[]
): HTMLElementTagNameMap[K] {
	const node = document.createElement(tag);
	for (const [key, value] of Object.entries(attrs)) {
		if (value === undefined || value === null || value === false) continue;
		if (key.startsWith("on") && typeof value === "function") {
			node.addEventListener(key.slice(2).toLowerCase(), value as EventListener);
		} else if (key === "class") {
			node.className = String(value);
		} else if (key === "text") {
			node.textContent = String(value);
		} else if (
			key === "value" &&
			(node instanceof HTMLInputElement || node instanceof HTMLTextAreaElement || node instanceof HTMLSelectElement || node instanceof HTMLOptionElement)
		) {
			node.value = String(value);
		} else if (key === "checked" && node instanceof HTMLInputElement) {
			node.checked = Boolean(value);
		} else if (
			key === "disabled" &&
			(node instanceof HTMLButtonElement || node instanceof HTMLInputElement || node instanceof HTMLSelectElement || node instanceof HTMLTextAreaElement)
		) {
			node.disabled = Boolean(value);
		} else if (key === "hidden") {
			node.hidden = Boolean(value);
		} else {
			node.setAttribute(key, value === true ? "" : String(value));
		}
	}
	appendChildren(node, children);
	return node;
}

function appendChildren(parent: Node, children: Child[]): void {
	for (const child of children) {
		if (child === null || child === undefined || child === false) continue;
		parent.appendChild(child instanceof Node ? child : document.createTextNode(String(child)));
	}
}

function inputFrom(event: Event): HTMLInputElement | null {
	const target = event.target;
	return target instanceof HTMLInputElement ? target : null;
}

type IconName =
	| "plus"
	| "send"
	| "stop"
	| "settings"
	| "node"
	| "command"
	| "menu"
	| "close"
	| "copy"
	| "reply"
	| "refresh"
	| "attach"
	| "sticker"
	| "chat"
	| "inbox"
	| "peers"
	| "trust"
	| "threads"
	| "box"
	| "pulse"
	| "plug"
	| "team"
	| "sun"
	| "moon"
	| "search"
	| "dock";

const ICON_PATHS: Record<IconName, string> = {
	plus: "M12 5v14M5 12h14",
	send: "M4 11.5 20 4l-6.5 16-2.6-6.4L4 11.5Z",
	stop: "M7 7h10v10H7z",
	settings: "M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6Zm7.4-3a7.4 7.4 0 0 0-.1-1l2-1.5-2-3.4-2.3 1a7.4 7.4 0 0 0-1.7-1L15 3h-6l-.3 2.1a7.4 7.4 0 0 0-1.7 1l-2.3-1-2 3.4L4.7 10a7.4 7.4 0 0 0 0 2l-2 1.5 2 3.4 2.3-1a7.4 7.4 0 0 0 1.7 1L9 19h6l.3-2.1a7.4 7.4 0 0 0 1.7-1l2.3 1 2-3.4-2-1.5c.1-.3.1-.7.1-1Z",
	node: "M12 3l8 4.5v9L12 21l-8-4.5v-9L12 3Zm0 9 8-4.5M12 12v9M12 12 4 7.5",
	command: "M9 6a3 3 0 1 0-3 3h12a3 3 0 1 0-3-3v12a3 3 0 1 0 3-3H6a3 3 0 1 0 3 3V6Z",
	menu: "M4 7h16M4 12h16M4 17h16",
	close: "M6 6l12 12M18 6 6 18",
	copy: "M9 9h11v12H9zM5 15V3h11",
	reply: "M9 7 4 12l5 5M4 12h9a6 6 0 0 1 6 6v1",
	refresh: "M20 12a8 8 0 1 1-2.3-5.6M20 4v5h-5",
	attach: "M20.5 11.5 12 20a5 5 0 0 1-7-7l8.5-8.5a3.3 3.3 0 0 1 4.7 4.7l-8.5 8.5a1.7 1.7 0 0 1-2.4-2.4l7.8-7.8",
	sticker: "M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18ZM8.6 14.2a4.6 4.6 0 0 0 6.8 0M9.2 9.4v.6M14.8 9.4v.6",
	chat: "M5 6.5h14a2 2 0 0 1 2 2V16a2 2 0 0 1-2 2H9l-4 3v-3H5a2 2 0 0 1-2-2V8.5a2 2 0 0 1 2-2Z",
	inbox: "M4 13h4.5a3.5 3.5 0 0 0 7 0H20v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1v-5Zm0 0 2.2-6.2A2 2 0 0 1 8.1 5.5h7.8a2 2 0 0 1 1.9 1.3L20 13",
	peers: "M8 10.5a2.6 2.6 0 1 0 0-5.2 2.6 2.6 0 0 0 0 5.2Zm8 .6a2.6 2.6 0 1 0 0-5.2 2.6 2.6 0 0 0 0 5.2ZM3.5 18.5a4.6 4.6 0 0 1 9 0M13 18.5a4.6 4.6 0 0 1 7.5-1.2",
	trust: "M12 3.2 19 6.2v5.6c0 4.2-2.8 6.8-7 8.5-4.2-1.7-7-4.3-7-8.5V6.2l7-3Z",
	threads: "M8 6.5h12M8 12h12M8 17.5H14M4.5 6.5h.01M4.5 12h.01M4.5 17.5h.01",
	box: "M3.5 8 12 3.5 20.5 8v8.5L12 21 3.5 16.5V8Zm8.5 4.2 8.5-4.2M12 12.2V21",
	pulse: "M3 12h4l2.2-5.5 4.2 11 2.3-5.5H21",
	plug: "M9 7v4M15 7v4M8 11h8v3a4 4 0 0 1-8 0v-3ZM12 18v3",
	team: "M12 11a3 3 0 1 0 0-6 3 3 0 0 0 0 6ZM5.5 19a6.5 6.5 0 0 1 13 0",
	sun: "M12 16.2a4.2 4.2 0 1 0 0-8.4 4.2 4.2 0 0 0 0 8.4ZM12 2.6v2.1M12 19.3v2.1M2.6 12h2.1M19.3 12h2.1M5.1 5.1l1.5 1.5M17.4 17.4l1.5 1.5M18.9 5.1l-1.5 1.5M6.6 17.4l-1.5 1.5",
	moon: "M20 14.6A7.6 7.6 0 1 1 9.4 4 6.2 6.2 0 0 0 20 14.6Z",
	search: "M11 18a7 7 0 1 0 0-14 7 7 0 0 0 0 14ZM16.5 16.5 21 21",
	dock: "M4 5h16v14H4V5Zm10 0v14",
};

const WORKSPACE_ICONS: Record<string, IconName> = {
	chat: "chat",
	inbox: "inbox",
	neighbors: "peers",
	trust: "trust",
	threads: "threads",
	resources: "box",
	health: "pulse",
	integrations: "plug",
	team: "team",
	settings: "settings",
};

function icon(name: IconName, size = 16): SVGSVGElement {
	const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
	svg.setAttribute("viewBox", "0 0 24 24");
	svg.setAttribute("width", String(size));
	svg.setAttribute("height", String(size));
	svg.setAttribute("fill", "none");
	svg.setAttribute("stroke", "currentColor");
	svg.setAttribute("stroke-width", "1.8");
	svg.setAttribute("stroke-linecap", "round");
	svg.setAttribute("stroke-linejoin", "round");
	svg.setAttribute("aria-hidden", "true");
	svg.setAttribute("focusable", "false");
	const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
	path.setAttribute("d", ICON_PATHS[name]);
	svg.appendChild(path);
	return svg;
}

function emptyState(): DesktopState {
	return {
		version: 1,
		bots: [],
		conversations: [],
		messages: [],
		capabilities: [],
		media: [],
		settings: { theme: "dark", locale: "en", hostPlugins: [], rendererPlugins: [], customCss: "", nodeId: "", peers: [] },
	};
}

const THINKING_LEVELS: ThinkingLevel[] = ["inherit", "off", "minimal", "low", "medium", "high", "xhigh", "max"];

/* ------------------------------------------------------------------ */
/* module state                                                        */
/* ------------------------------------------------------------------ */

type Level = "info" | "ok" | "warn" | "error";
type Connection = "connecting" | "online" | "offline";

interface RuntimeInfo {
	status: string;
	message?: string;
	at: number;
}

interface StreamBlock {
	id: string;
	kind: "text" | "thinking" | "tool" | "raw" | "notice";
	text?: string;
	name?: string;
	args?: string;
	result?: string;
	status?: "running" | "ok" | "error" | "warn";
}

interface Stream {
	key: string;
	conversationId: string;
	botId: string;
	startedAt: number;
	blocks: StreamBlock[];
	settled: boolean;
	statusText: string;
	usage?: Record<string, number>;
}

interface FrameEntry {
	at: number;
	botId: string;
	frame: RpcFrame;
}

interface PanelEntry {
	id: string;
	title: string;
	render: (snapshot: DesktopState) => HTMLElement;
	botId?: string;
}

interface AttachmentEntry {
	key: string;
	name: string;
	contentType: string;
	size: number;
	status: "reading" | "uploading" | "done" | "error";
	progress: number;
	cancelled: boolean;
	previewUrl?: string;
	error?: string;
	media?: MediaItem;
}

/** Server-side bot fields that predate this renderer's contract bundle. */
type BotRecord = Bot & {
	configDir?: string;
	onlyne?: { nodeId?: string; peers?: string[] };
	plugins?: { host?: string[]; renderer?: string[] };
};

interface MessageRendererEntry {
	botId?: string;
	render: (message: unknown) => HTMLElement;
}

interface ThemeEntry {
	id: string;
	label: string;
	botId?: string;
	style: HTMLStyleElement;
}

interface Refs {
	app: HTMLElement;
	mast: HTMLElement;
	demoChip: HTMLElement;
	themeButton: HTMLButtonElement;
	railBots: HTMLElement;
	railFoot: HTMLElement;
	botSwitch: HTMLElement;
	dock: HTMLElement;
	dockTitle: HTMLElement;
	dockBody: HTMLElement;
	nav: HTMLElement;
	navTitle: HTMLElement;
	navSub: HTMLElement;
	navSearch: HTMLInputElement;
	navSections: HTMLElement;
	navViews: HTMLElement;
	aside: HTMLElement;
	asideTabs: HTMLElement;
	asideBody: HTMLElement;
	topTitle: HTMLElement;
	topSub: HTMLElement;
	topActions: HTMLElement;
	banner: HTMLElement;
	chatView: HTMLElement;
	panelView: HTMLElement;
	transcript: HTMLElement;
	messageList: HTMLElement;
	streamList: HTMLElement;
	widgetsAbove: HTMLElement;
	widgetsBelow: HTMLElement;
	inlineViews: HTMLElement;
	widgetStatus: HTMLElement;
	recipients: HTMLElement;
	composer: HTMLElement;
	composerInput: HTMLTextAreaElement;
	composerSend: HTMLButtonElement;
	composerStop: HTMLButtonElement;
	composerHint: HTMLElement;
	composerMedia: HTMLElement;
	composerAttach: HTMLButtonElement;
	composerSticker: HTMLButtonElement;
	composerFile: HTMLInputElement;
	stickerPopover: HTMLElement;
	topCover: HTMLButtonElement;
	statusbar: HTMLElement;
	toasts: HTMLElement;
	dialogs: HTMLElement;
	modals: HTMLElement;
	backdrop: HTMLElement;
}

let refs!: Refs;
let state: DesktopState = emptyState();
let hasState = false;
let connection: Connection = "connecting";
let sessionExpired = false;
let ws: WebSocket | null = null;
let reconnectTimer: number | undefined;
let reconnectDelay = 1000;
let socketGeneration = 0;

let selectedBotId: string | null = null;
let selectedConversationId: string | null = null;
let activePanel: string | null = null;
let navFilter = "";
let composerConversationId: string | null = null;
let memberFocusId: string | null = null;
let asideTab: "members" | "runtime" | "usage" = "members";
let asideBuiltKey = "";
let runtimeTabBotId: string | null = null;
let runtimeStatusList: HTMLElement | null = null;
let runtimeFramesBox: HTMLElement | null = null;
let runtimeModelChip: HTMLElement | null = null;
let membersOpen = window.matchMedia("(min-width: 1081px)").matches;
let shellReady = false;
let layout: DeskLayout = defaultLayout(BUILTIN_WORKSPACES.map((item) => item.id));
let extraWorkspaces: WorkspaceSpec[] = [];
let dockKey = "";
let goChord = 0;

function railCatalog(): WorkspaceSpec[] {
	return [...BUILTIN_WORKSPACES, ...extraWorkspaces].filter((item) => item.group !== "studio");
}

function catalogIds(): string[] {
	return railCatalog().map((item) => item.id);
}

function workspaceById(id: string): WorkspaceSpec | undefined {
	return [...BUILTIN_WORKSPACES, ...extraWorkspaces].find((item) => item.id === id);
}

function loadLayout(): void {
	try {
		layout = normalizeLayout(JSON.parse(localStorage.getItem(LAYOUT_STORAGE_KEY) ?? "null"), catalogIds());
	} catch {
		layout = defaultLayout(catalogIds());
	}
}

function visibleRail(): WorkspaceSpec[] {
	const byId = new Map(railCatalog().map((item) => [item.id, item]));
	return layout.rail.flatMap((id) => {
		if (layout.hidden.includes(id)) return [];
		const spec = byId.get(id);
		return spec === undefined ? [] : [spec];
	});
}

function persistLayout(): void {
	layout = normalizeLayout(layout, catalogIds());
	localStorage.setItem(LAYOUT_STORAGE_KEY, JSON.stringify(layout));
	if (refs) {
		refs.app.classList.toggle("has-dock", layout.dock !== null);
		renderRail();
		renderDock();
	}
}

function setDock(id: string | null): void {
	if (layout.dock === id) return;
	dockKey = "";
	layout = { ...layout, dock: id };
	persistLayout();
	renderStatusbar();
	writeRoute();
}

function toggleDock(): void {
	if (layout.dock) setDock(null);
	else setDock(activePanel ?? "health");
}

function resetLayout(): void {
	dockKey = "";
	layout = defaultLayout(catalogIds());
	localStorage.removeItem(LAYOUT_STORAGE_KEY);
	persistLayout();
}

function openWorkspace(spec: WorkspaceSpec): void {
	if (spec.panel === null) showChat({ focus: true });
	else openPanel(spec.panel);
}

function isTypingTarget(target: EventTarget | null): boolean {
	if (!(target instanceof HTMLElement)) return false;
	const tag = target.tagName;
	return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || target.isContentEditable;
}

function registerWorkspaceEntry(botId: string | undefined, entry: WorkspaceContribution): () => void {
	const id = botId ? `${botId}:${entry.id}` : entry.id;
	const spec: WorkspaceSpec = {
		id,
		title: entry.title,
		hint: entry.hint ?? "",
		shortcut: entry.shortcut ?? "",
		group: entry.group ?? "studio",
		panel: id,
	};
	extraWorkspaces = extraWorkspaces.filter((item) => item.id !== id);
	extraWorkspaces.push(spec);
	layout = normalizeLayout(layout, catalogIds());
	if (shellReady) renderRail();
	return () => {
		extraWorkspaces = extraWorkspaces.filter((item) => item.id !== id);
		if (layout.dock === id) layout = { ...layout, dock: null };
		layout = normalizeLayout(layout, catalogIds());
		dockKey = "";
		if (shellReady) {
			renderRail();
			renderDock();
		}
	};
}

const drafts = new Map<string, string>();
const attachmentsByConversation = new Map<string, AttachmentEntry[]>();
let mediaIndex = new Map<string, MediaItem>();
let attachmentSequence = 0;
let stickerConversationId: string | null = null;
let coverMedia: MediaItem | null = null;
const MAX_ATTACHMENT_BYTES = 25 * 1024 * 1024;
const recipients = new Map<string, Set<string>>();
const sending = new Set<string>();
const runtimeStatus = new Map<string, RuntimeInfo>();
const autoStatus = new Map<string, { active: boolean; turns: number; max: number; botId?: string; reason?: string }>();
const listeners = new Set<(event: DesktopEvent) => void>();
const panels = new Map<string, PanelEntry>();
const messageRenderers = new Map<string, MessageRendererEntry[]>();
const themeStyles = new Map<string, ThemeEntry>();
const botWidgetRegistries = new Map<string, WidgetRegistry>();
const streams = new Map<string, Stream>();
const streamEls = new Map<string, { root: HTMLElement; body: HTMLElement; status: HTMLElement }>();
const framesByConversation = new Map<string, FrameEntry[]>();
const errors: Array<{ message: string; at: number }> = [];
const loadedPlugins = new Set<string>();
const loadedBotPlugins = new Set<string>();
const botPluginUrls = new Map<string, string[]>();
const botPluginPending = new Set<string>();

let messageSignature = "";
let stickToBottom = true;
let themeSetting = "dark";
let themeDirty = false;
let customCssPreview: string | null = null;
let rpcSequence = 0;
let noticeSequence = 0;
let fieldSequence = 0;
let host: WidgetHost | null = null;
let registry: WidgetRegistry;
let globalRegistry: WidgetRegistry;
let runtimeThinkingSelect: HTMLSelectElement | null = null;
let pluginChain: Promise<void> = Promise.resolve();
let framesRefreshQueued = false;
let globalPluginsReady = false;

let frameFilter = "";
let frameShowAll = false;
let frameScope: string | "all" = "all";

/* ------------------------------------------------------------------ */
/* formatting                                                          */
/* ------------------------------------------------------------------ */

function isObj(value: unknown): value is Record<string, unknown> {
	return typeof value === "object" && value !== null && !Array.isArray(value);
}

function errText(error: unknown): string {
	if (error instanceof Error) return error.message;
	return String(error);
}

function safeJson(value: unknown): string {
	try {
		const text = JSON.stringify(value, null, 2);
		return text === undefined ? String(value) : text;
	} catch {
		return String(value);
	}
}

function compactJson(value: unknown): string {
	try {
		const text = JSON.stringify(value);
		return text === undefined ? String(value) : text;
	} catch {
		return String(value);
	}
}

function textOf(value: unknown): string {
	if (typeof value === "string") return value;
	if (value === undefined || value === null) return "";
	return safeJson(value);
}

function truncate(text: string, limit: number): string {
	return text.length <= limit ? text : `${text.slice(0, limit - 1)}…`;
}

function normalizeSpace(text: string): string {
	return text.replace(/\s+/g, " ").trim();
}

function compactNumber(value: number): string {
	if (!Number.isFinite(value)) return "—";
	const abs = Math.abs(value);
	if (abs >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}M`;
	if (abs >= 1_000) return `${(value / 1_000).toFixed(1)}k`;
	return String(Math.round(value * 100) / 100);
}

function clockTime(timestamp: number): string {
	return new Date(timestamp).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

function relativeTime(timestamp: number): string {
	const diff = Date.now() - timestamp;
	if (!Number.isFinite(diff) || diff < 60_000) return "now";
	if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m`;
	if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)}h`;
	return new Date(timestamp).toLocaleDateString();
}

function initials(name: string): string {
	const parts = name.trim().split(/\s+/).filter(Boolean);
	if (!parts.length) return "?";
	const first = parts[0];
	if (!first) return "?";
	if (parts.length === 1) return Array.from(first).slice(0, 2).join("").toUpperCase();
	const second = parts[1] ?? first;
	return (first.charAt(0) + second.charAt(0)).toUpperCase();
}

/* ------------------------------------------------------------------ */
/* media                                                               */
/* ------------------------------------------------------------------ */

const LIGHTBOX_MIN_SCALE = 0.5;
const LIGHTBOX_MAX_SCALE = 4;
const MEDIA_PAN_STEP = 40;

function mediaUrl(item: MediaItem): string {
	return item.url ?? `/api/media/${encodeURIComponent(item.id)}`;
}

function botAvatarOf(bot: Bot | undefined): MediaItem | undefined {
	const id = bot?.character?.avatar;
	if (typeof id !== "string" || !id) return undefined;
	return mediaIndex.get(id);
}

function formatBytes(size: number): string {
	if (!Number.isFinite(size) || size <= 0) return "0 B";
	const units = ["B", "KB", "MB", "GB"];
	let value = size;
	let unit = 0;
	while (value >= 1024 && unit < units.length - 1) {
		value /= 1024;
		unit += 1;
	}
	const digits = value >= 10 || unit === 0 ? 0 : 1;
	return `${value.toFixed(digits)} ${units[unit] ?? "B"}`;
}

function avatarNode(name: string, item: MediaItem | undefined): HTMLElement {
	const node = h("span", { class: "avatar sm", "aria-hidden": "true" });
	if (item) {
		node.classList.add("has-image");
		node.append(h("img", { class: "avatar-img", src: mediaUrl(item), alt: "", loading: "lazy", decoding: "async" }));
	} else {
		node.textContent = initials(name);
	}
	return node;
}

/* Lightbox: one overlay for every still image in the window. */

interface LightboxParts {
	root: HTMLElement;
	stage: HTMLElement;
	image: HTMLImageElement;
	caption: HTMLElement;
	zoom: HTMLElement;
	close: HTMLButtonElement;
}

let lightbox: LightboxParts | null = null;
let lightboxScale = 1;
let lightboxX = 0;
let lightboxY = 0;
let lightboxDrag: { pointer: number; startX: number; startY: number; originX: number; originY: number } | null = null;
let lightboxMoved = false;
let lightboxReturn: HTMLElement | null = null;

function applyLightbox(): void {
	if (!lightbox) return;
	lightbox.image.style.transform = `translate(${lightboxX.toFixed(1)}px, ${lightboxY.toFixed(1)}px) scale(${lightboxScale})`;
	lightbox.zoom.textContent = `${Math.round(lightboxScale * 100)}%`;
}

function setLightboxScale(next: number, anchorX = 0, anchorY = 0): void {
	const clamped = Math.min(LIGHTBOX_MAX_SCALE, Math.max(LIGHTBOX_MIN_SCALE, next));
	if (Math.abs(clamped - lightboxScale) < 0.0001) return;
	const ratio = clamped / lightboxScale;
	lightboxX = anchorX - (anchorX - lightboxX) * ratio;
	lightboxY = anchorY - (anchorY - lightboxY) * ratio;
	lightboxScale = clamped;
	applyLightbox();
}

function panLightbox(dx: number, dy: number): void {
	lightboxX += dx;
	lightboxY += dy;
	applyLightbox();
}

function resetLightboxView(): void {
	lightboxScale = 1;
	lightboxX = 0;
	lightboxY = 0;
	applyLightbox();
}

function ensureLightbox(): void {
	if (lightbox) return;
	const image = h("img", { class: "lightbox-image", alt: "", draggable: false });
	const stage = h("div", { class: "lightbox-stage" }, image);
	const caption = h("span", { class: "lightbox-caption" });
	const zoom = h("span", { class: "lightbox-zoom", text: "100%" });
	const hint = h("span", { class: "lightbox-hint", text: "Wheel zooms · drag or arrow keys pan · double-click resets" });
	const close = h(
		"button",
		{ class: "lightbox-close", type: "button", "aria-label": "Close media viewer", title: "Close (Esc)", onclick: () => closeLightbox() },
		icon("close", 16),
	);
	const root = h(
		"div",
		{ class: "lightbox", role: "dialog", "aria-modal": "true", "aria-label": "Media viewer", hidden: true },
		stage,
		h("div", { class: "lightbox-bar" }, caption, hint, zoom, close),
	);
	root.addEventListener("click", (event) => {
		if (lightboxMoved) {
			lightboxMoved = false;
			return;
		}
		if (event.target === root || event.target === stage) closeLightbox();
	});
	stage.addEventListener(
		"wheel",
		(event) => {
			event.preventDefault();
			const rect = stage.getBoundingClientRect();
			const anchorX = event.clientX - (rect.left + rect.width / 2);
			const anchorY = event.clientY - (rect.top + rect.height / 2);
			setLightboxScale(lightboxScale * Math.exp(-event.deltaY * 0.0015), anchorX, anchorY);
		},
		{ passive: false },
	);
	stage.addEventListener("pointerdown", (event) => {
		if (event.button !== 0) return;
		stage.setPointerCapture(event.pointerId);
		lightboxDrag = {
			pointer: event.pointerId,
			startX: event.clientX,
			startY: event.clientY,
			originX: lightboxX,
			originY: lightboxY,
		};
		lightboxMoved = false;
		stage.classList.add("dragging");
	});
	stage.addEventListener("pointermove", (event) => {
		if (!lightboxDrag || event.pointerId !== lightboxDrag.pointer) return;
		if (Math.abs(event.clientX - lightboxDrag.startX) + Math.abs(event.clientY - lightboxDrag.startY) > 3) lightboxMoved = true;
		lightboxX = lightboxDrag.originX + (event.clientX - lightboxDrag.startX);
		lightboxY = lightboxDrag.originY + (event.clientY - lightboxDrag.startY);
		applyLightbox();
	});
	const endDrag = (event: PointerEvent): void => {
		if (!lightboxDrag || event.pointerId !== lightboxDrag.pointer) return;
		lightboxDrag = null;
		stage.classList.remove("dragging");
		if (stage.hasPointerCapture(event.pointerId)) stage.releasePointerCapture(event.pointerId);
	};
	stage.addEventListener("pointerup", endDrag);
	stage.addEventListener("pointercancel", endDrag);
	stage.addEventListener("dblclick", () => resetLightboxView());
	document.addEventListener("keydown", (event) => {
		if (!lightbox || lightbox.root.hidden) return;
		const step = event.shiftKey ? MEDIA_PAN_STEP * 3 : MEDIA_PAN_STEP;
		if (event.key === "Escape") {
			event.preventDefault();
			closeLightbox();
		} else if (event.key === "ArrowLeft") {
			event.preventDefault();
			panLightbox(-step, 0);
		} else if (event.key === "ArrowRight") {
			event.preventDefault();
			panLightbox(step, 0);
		} else if (event.key === "ArrowUp") {
			event.preventDefault();
			panLightbox(0, -step);
		} else if (event.key === "ArrowDown") {
			event.preventDefault();
			panLightbox(0, step);
		} else if (event.key === "0") {
			event.preventDefault();
			resetLightboxView();
		}
	});
	document.body.append(root);
	lightbox = { root, stage, image, caption, zoom, close };
}

function openLightbox(item: MediaItem, trigger?: HTMLElement | null): void {
	ensureLightbox();
	if (!lightbox) return;
	lightboxReturn = trigger ?? (document.activeElement instanceof HTMLElement ? document.activeElement : null);
	lightbox.image.src = mediaUrl(item);
	lightbox.image.alt = item.name;
	lightbox.caption.textContent = `${item.name} · ${formatBytes(item.size)}`;
	resetLightboxView();
	lightboxMoved = false;
	lightbox.root.hidden = false;
	lightbox.close.focus();
}

function closeLightbox(): void {
	if (!lightbox || lightbox.root.hidden) return;
	lightbox.root.hidden = true;
	lightbox.image.removeAttribute("src");
	lightboxDrag = null;
	const target = lightboxReturn;
	lightboxReturn = null;
	if (target && target.isConnected) target.focus();
}

/* ------------------------------------------------------------------ */
/* domain lookups                                                      */
/* ------------------------------------------------------------------ */

function currentSettings(): DesktopSettings {
	const raw: Partial<DesktopSettings> = state.settings ?? {};
	return {
		theme: typeof raw.theme === "string" && raw.theme ? raw.theme : "dark",
		locale: raw.locale === "zh-CN" ? "zh-CN" : "en",
		hostPlugins: Array.isArray(raw.hostPlugins) ? raw.hostPlugins.filter((item): item is string => typeof item === "string") : [],
		rendererPlugins: Array.isArray(raw.rendererPlugins) ? raw.rendererPlugins.filter((item): item is string => typeof item === "string") : [],
		customCss: typeof raw.customCss === "string" ? raw.customCss : "",
		nodeId: typeof raw.nodeId === "string" ? raw.nodeId : "",
		peers: Array.isArray(raw.peers) ? raw.peers.filter((item): item is string => typeof item === "string") : [],
	};
}

function botOf(id: string | null | undefined): Bot | undefined {
	if (!id) return undefined;
	return state.bots.find((bot) => bot.id === id);
}

function conversationOf(id: string | null | undefined): Conversation | undefined {
	if (!id) return undefined;
	return state.conversations.find((conversation) => conversation.id === id);
}

function selectedConversation(): Conversation | undefined {
	return conversationOf(selectedConversationId);
}

function capabilityName(id: string): string {
	return state.capabilities.find((capability) => capability.id === id)?.name ?? id;
}

function botConfigDir(bot: Bot): string {
	const record: BotRecord = bot;
	return typeof record.configDir === "string" ? record.configDir : "";
}

function botOnlyne(bot: Bot): { nodeId: string; peers: string[] } {
	const record: BotRecord = bot;
	const raw = record.onlyne;
	return {
		nodeId: typeof raw?.nodeId === "string" ? raw.nodeId : "",
		peers: Array.isArray(raw?.peers) ? raw.peers.filter((item): item is string => typeof item === "string") : [],
	};
}

function botPlugins(bot: Bot): { host: string[]; renderer: string[] } {
	const record: BotRecord = bot;
	const raw = record.plugins;
	return {
		host: Array.isArray(raw?.host) ? raw.host.filter((item): item is string => typeof item === "string") : [],
		renderer: Array.isArray(raw?.renderer) ? raw.renderer.filter((item): item is string => typeof item === "string") : [],
	};
}

/** Bots whose plugin contributions may appear in the current window. */
function uiActiveBotIds(): Set<string> {
	const conversation = selectedConversation();
	if (conversation) return new Set(conversation.botIds);
	return selectedBotId ? new Set([selectedBotId]) : new Set();
}

function botName(id: string | null | undefined): string {
	if (!id) return "Unknown bot";
	return botOf(id)?.name ?? id;
}

function runtimeKey(conversationId: string, botId: string): string {
	return `${conversationId}::${botId}`;
}

function currentMessages(): ChatMessage[] {
	const id = selectedConversationId;
	if (!id) return [];
	return state.messages
		.filter((message) => message.conversationId === id)
		.slice()
		.sort((a, b) => a.createdAt - b.createdAt);
}

function messagesOf(conversationId: string): ChatMessage[] {
	return state.messages.filter((message) => message.conversationId === conversationId);
}

function lastMessageAt(conversationId: string): number {
	let latest = 0;
	for (const message of state.messages) {
		if (message.conversationId === conversationId && message.createdAt > latest) latest = message.createdAt;
	}
	return latest;
}

function recipientSet(conversation: Conversation): Set<string> {
	const existing = recipients.get(conversation.id);
	if (existing) {
		for (const id of [...existing]) if (!conversation.botIds.includes(id)) existing.delete(id);
		return existing;
	}
	const next = new Set(conversation.botIds);
	recipients.set(conversation.id, next);
	return next;
}

function activeBotIds(conversationId: string): string[] {
	const ids = new Set<string>();
	for (const stream of streams.values()) {
		if (stream.conversationId === conversationId && !stream.settled) ids.add(stream.botId);
	}
	for (const botId of conversationOf(conversationId)?.botIds ?? []) {
		const info = runtimeStatus.get(runtimeKey(conversationId, botId));
		const status = (info?.status ?? "").toLowerCase();
		if (status.includes("running") || status.includes("stream") || status.includes("connecting")) ids.add(botId);
	}
	return [...ids];
}

function hasActiveStream(conversationId: string): boolean {
	for (const stream of streams.values()) if (stream.conversationId === conversationId && !stream.settled) return true;
	return false;
}

function statusClass(status: string | undefined): string {
	const value = (status ?? "").toLowerCase();
	if (value.includes("run") || value.includes("stream") || value.includes("busy")) return "run";
	if (value.includes("connect") || value.includes("start") || value.includes("pending") || value.includes("queue")) return "pend";
	if (value.includes("err") || value.includes("fail")) return "err";
	if (value.includes("idle") || value.includes("ready") || value.includes("ok") || value.includes("settled") || value.includes("completed")) return "ok";
	return "none";
}

function nextRpcId(): string {
	rpcSequence += 1;
	return `ui-${rpcSequence}`;
}

function aggregateUsage(messages: ChatMessage[]): Array<{ key: string; total: number }> {
	const totals = new Map<string, number>();
	for (const message of messages) {
		if (!message.usage) continue;
		for (const [key, value] of Object.entries(message.usage)) {
			if (typeof value === "number" && Number.isFinite(value)) totals.set(key, (totals.get(key) ?? 0) + value);
		}
	}
	return [...totals].sort((a, b) => b[1] - a[1]).map(([key, total]) => ({ key, total }));
}

/* ------------------------------------------------------------------ */
/* transport                                                           */
/* ------------------------------------------------------------------ */

interface CommandReply<T> {
	ok?: boolean;
	value?: T;
	error?: string;
}

async function command<T = unknown>(action: string, payload: Record<string, unknown> = {}): Promise<T> {
	const response = await fetch("/api/command", {
		method: "POST",
		headers: { "content-type": "application/json" },
		credentials: "same-origin",
		body: JSON.stringify({ action, payload }),
	});
	const text = await response.text();
	let body: CommandReply<T> | null = null;
	if (text) {
		try {
			body = JSON.parse(text) as CommandReply<T>;
		} catch {
			body = null;
		}
	}
	if (response.status === 401) {
		expireSession();
		throw new Error("Session expired — reopen Onlyne Desktop from the launcher.");
	}
	if (!response.ok) {
		throw new Error(body?.error ?? `HTTP ${response.status}${body ? "" : `: ${truncate(text, 200)}`}`);
	}
	if (!body || body.ok !== true) throw new Error(body?.error ?? "Command failed");
	return body.value as T;
}

async function refresh(): Promise<void> {
	const response = await fetch("/api/state", {
		credentials: "same-origin",
		headers: { accept: "application/json" },
	});
	if (response.status === 401) {
		expireSession();
		throw new Error("Session expired — reopen Onlyne Desktop from the launcher.");
	}
	if (!response.ok) throw new Error(`State request failed with HTTP ${response.status}`);
	const snapshot = (await response.json()) as DesktopState;
	applyState(snapshot);
}

function expireSession(): void {
	if (sessionExpired) return;
	sessionExpired = true;
	setConnection("offline");
}

function connectEvents(): void {
	if (sessionExpired) return;
	if (ws && (ws.readyState === WebSocket.CONNECTING || ws.readyState === WebSocket.OPEN)) return;
	if (reconnectTimer !== undefined) {
		window.clearTimeout(reconnectTimer);
		reconnectTimer = undefined;
	}
	socketGeneration += 1;
	const generation = socketGeneration;
	const protocol = location.protocol === "https:" ? "wss:" : "ws:";
	let socket: WebSocket;
	try {
		socket = new WebSocket(`${protocol}//${location.host}/api/events`);
	} catch (error) {
		notify(`Cannot open the event stream: ${errText(error)}`, "error");
		setConnection("offline");
		scheduleReconnect();
		return;
	}
	ws = socket;
	if (connection === "offline") setConnection("connecting");
	socket.addEventListener("open", () => {
		if (generation !== socketGeneration) return;
		reconnectDelay = 1000;
		setConnection("online");
		void refresh().catch((error) => notify(errText(error), "error"));
	});
	socket.addEventListener("message", (event) => {
		if (generation !== socketGeneration) return;
		let parsed: DesktopEvent;
		try {
			parsed = JSON.parse(String(event.data)) as DesktopEvent;
		} catch {
			notify("The host sent a malformed event frame.", "error");
			return;
		}
		handleEvent(parsed);
	});
	socket.addEventListener("close", () => {
		if (generation !== socketGeneration) return;
		ws = null;
		if (sessionExpired) return;
		setConnection("offline");
		scheduleReconnect();
	});
	socket.addEventListener("error", () => {
		// The close handler schedules the retry; an error alone carries no detail.
	});
}

function scheduleReconnect(): void {
	if (sessionExpired || reconnectTimer !== undefined) return;
	const delay = reconnectDelay;
	reconnectDelay = Math.min(reconnectDelay * 2, 15_000);
	reconnectTimer = window.setTimeout(() => {
		reconnectTimer = undefined;
		connectEvents();
	}, delay);
}

function setConnection(next: Connection): void {
	connection = next;
	renderStatusbar();
	renderBanner();
	updateComposerState();
}

function emitLocal(event: DesktopEvent): void {
	for (const listener of listeners) {
		try {
			listener(event);
		} catch (error) {
			console.error("onlyne ui listener failed", error);
		}
	}
}

function handleEvent(event: DesktopEvent): void {
	emitLocal(event);
	const type = String(event.type ?? "");
	if (type === "snapshot") {
		const snapshot = event["state"];
		if (isObj(snapshot)) applyState(snapshot as unknown as DesktopState);
		else notify("The host snapshot event carried no state.", "error");
		return;
	}
	if (type === "rpc") {
		const conversationId = typeof event.conversationId === "string" ? event.conversationId : "";
		const botId = typeof event.botId === "string" ? event.botId : "";
		const frame = event["frame"];
		if (!conversationId || !botId || !isObj(frame) || typeof frame["type"] !== "string") {
			notify("The host sent an RPC event without a conversation, bot or frame.", "error");
			return;
		}
		ingestRpc(conversationId, botId, frame as RpcFrame);
		return;
	}
	if (type === "widget") {
		const view = event["view"];
		if (!isObj(view) || typeof view["id"] !== "string" || typeof view["protocol"] !== "string") {
			notify("The host sent a widget event without a view.", "error");
			return;
		}
		if (!host) return;
		try {
			host.handleView(view as unknown as WidgetEnvelope);
		} catch (error) {
			notify(`Widget view failed: ${errText(error)}`, "error");
		}
		return;
	}
	if (type === "runtime") {
		const conversationId = typeof event.conversationId === "string" ? event.conversationId : selectedConversationId ?? "";
		const botId = typeof event.botId === "string" ? event.botId : "";
		if (!conversationId || !botId) {
			notify("The host sent a runtime event without a conversation or bot.", "error");
			return;
		}
		const raw = event["status"];
		const status = typeof raw === "string" ? raw : isObj(raw) ? String(raw["status"] ?? raw["state"] ?? "") : "";
		const message = isObj(raw) && typeof raw["message"] === "string" ? raw["message"] : typeof event["message"] === "string" ? event["message"] : undefined;
		setRuntime(conversationId, botId, status || "unknown", message);
		return;
	}
	if (type === "auto") {
		const conversationId = typeof event.conversationId === "string" ? event.conversationId : "";
		if (!conversationId) return;
		const turns = typeof event["turns"] === "number" ? event["turns"] : 0;
		const max = typeof event["max"] === "number" ? event["max"] : DEFAULT_MAX_AUTO_TURNS;
		autoStatus.set(conversationId, {
			active: event["active"] === true,
			turns,
			max,
			...(typeof event["botId"] === "string" ? { botId: event["botId"] } : {}),
			...(typeof event["reason"] === "string" ? { reason: event["reason"] } : {}),
		});
		if (event["reason"] === "limit") notify(`The group reached its automatic turn limit (${turns}). Send a message to continue.`, "info");
		if (selectedConversationId === conversationId) {
			updateComposerState();
			renderStatusbar();
			renderTopbar();
			if (asideTab === "runtime") updateRuntimeTab();
		}
		return;
	}
	if (type === "typing") {
		const conversationId = typeof event.conversationId === "string" ? event.conversationId : "";
		const botId = typeof event.botId === "string" ? event.botId : "";
		if (!conversationId) return;
		if (event["done"] === true) typingBots.delete(conversationId);
		else typingBots.set(conversationId, { botId, demo: event["demo"] === true });
		if (selectedConversationId === conversationId) {
			messageSignature = "";
			renderMessages();
		}
		return;
	}
	if (type === "error") {
		const raw = typeof event["message"] === "string" ? event["message"] : typeof event["error"] === "string" ? event["error"] : "The host reported an unknown error.";
		const message = humanRuntimeError(raw);
		const conversationId = typeof event.conversationId === "string" ? event.conversationId : "";
		if (conversationId) {
			const last = [...messagesOf(conversationId)].reverse().find(item => item.role === "user");
			if (last) failedDelivery.set(last.id, message);
			messageSignature = "";
			if (selectedConversationId === conversationId) renderMessages();
		}
		notify(message, "error");
		return;
	}
	if (type.startsWith("onlyne.") || type === "context") return;
}

const failedDelivery = new Map<string, string>();
const typingBots = new Map<string, { botId: string; demo: boolean }>();

function humanRuntimeError(message: string): string {
	if (/executable not found|enoent|spawn omp|omp/i.test(message)) {
		return "No model runtime is available. Install OMP and put the omp command on PATH. The demo desk simulates a reply instead, and labels it Demo.";
	}
	return message;
}

function setRuntime(conversationId: string, botId: string, status: string, message?: string): void {
	runtimeStatus.set(runtimeKey(conversationId, botId), { status, message, at: Date.now() });
	if (selectedConversationId === conversationId) {
		updateComposerState();
		if (asideTab === "runtime") updateRuntimeTab();
		else renderAside();
	}
}

/* ------------------------------------------------------------------ */
/* state application                                                   */
/* ------------------------------------------------------------------ */

function applyState(snapshot: DesktopState): void {
	if (!snapshot || snapshot.version !== 1 || !Array.isArray(snapshot.bots) || !Array.isArray(snapshot.conversations)) {
		notify("The host sent an invalid desktop snapshot.", "error");
		return;
	}
	state = snapshot;
	if (!Array.isArray(state.media)) state.media = [];
	mediaIndex = new Map(state.media.map((item) => [item.id, item]));
	hasState = true;
	reconcileStreams();
	ensureSelection();
	applyAppearance();
	renderAll();
	syncHostSelection();
	activateBotPlugins();
}

function ensureSelection(): void {
	if (selectedBotId && !state.bots.some((bot) => bot.id === selectedBotId)) selectedBotId = null;
	if (selectedConversationId && !state.conversations.some((conversation) => conversation.id === selectedConversationId)) {
		selectedConversationId = null;
	}
	if (selectedConversationId) return;
	const botFilter = selectedBotId;
	const pool = botFilter ? state.conversations.filter((conversation) => conversation.botIds.includes(botFilter)) : state.conversations;
	const candidates = pool.length || botFilter ? pool : state.conversations;
	const pick = candidates.slice().sort((a, b) => b.updatedAt - a.updatedAt)[0];
	selectedConversationId = pick?.id ?? null;
}

let lastHostSelection = "";

function syncHostSelection(): void {
	const conversation = selectedConversation();
	if (!conversation) {
		lastHostSelection = "";
		emitLocal({ type: "context" });
		return;
	}
	const botId = conversation.adminBotId || conversation.botIds[0] || "";
	const key = `${conversation.id}::${botId}`;
	if (key === lastHostSelection) return;
	lastHostSelection = key;
	if (host) {
		try {
			host.select({ conversationId: conversation.id, botId });
		} catch (error) {
			notify(`Widget host selection failed: ${errText(error)}`, "error");
		}
	}
	emitLocal({ type: "context", conversationId: conversation.id, botId });
}

/* ------------------------------------------------------------------ */
/* appearance                                                          */
/* ------------------------------------------------------------------ */

function applyAppearance(): void {
	if (!hasState) return;
	const settings = currentSettings();
	if (!themeDirty) {
		const wanted = settings.theme || "dark";
		if (wanted !== themeSetting) applyTheme(wanted);
	}
	if (customCssPreview === null) applyCustomCss(settings.customCss);
	document.documentElement.lang = settings.locale;
}

function applyTheme(id: string): void {
	themeSetting = id;
	document.documentElement.dataset["theme"] =
		id === "system" ? (window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark") : id;
}

function applyCustomCss(css: string): void {
	const existing = document.getElementById("onlyne-custom-css");
	let style = existing instanceof HTMLStyleElement ? existing : null;
	if (!style) {
		style = document.createElement("style");
		style.id = "onlyne-custom-css";
		document.head.appendChild(style);
	}
	style.textContent = css;
}

function resetSettingsPreview(): void {
	themeDirty = false;
	customCssPreview = null;
	applyAppearance();
}

/* ------------------------------------------------------------------ */
/* shell                                                               */
/* ------------------------------------------------------------------ */

function buildShell(): void {
	const app = document.getElementById("app");
	if (!app) throw new Error("Missing #app");
	app.replaceChildren();
	app.className = "app";
	app.classList.toggle("members-hidden", !membersOpen);
	app.classList.toggle("has-dock", layout.dock !== null);

	const demoChip = h("span", { class: "demo-chip", hidden: true, text: "Demo" });
	const themeButton = h("button", {
		class: "theme-toggle",
		type: "button",
		"aria-label": "Toggle color theme",
		title: "Toggle theme",
		onclick: () => void toggleTheme(),
	}, icon("sun", 16), h("span", { class: "theme-label", text: "Light" }));
	const commandButton = h("button", { class: "command-trigger", type: "button", onclick: openPalette },
		icon("search", 15),
		h("span", { class: "command-label", text: "Search or jump" }),
		h("kbd", { text: chord(MOD, "K") }),
	);
	const mast = h("header", { class: "mast" },
		h("div", { class: "mast-brand" }, h("span", { class: "wordmark", text: "Onlyne" }), demoChip),
		commandButton,
		h("div", { class: "mast-tools" }, themeButton),
	);

	const railBots = h("div", { class: "rail-top", id: "rail-bots" });
	const railFoot = h(
		"div",
		{ class: "rail-foot" },
		h("button", { class: "rail-link", type: "button", title: "Keyboard shortcuts", "aria-label": "Keyboard shortcuts", onclick: openShortcuts }, icon("command")),
	);
	const rail = h("nav", { class: "rail", id: "rail", "aria-label": "Bots" }, railBots, railFoot);

	const botSwitch = h("div", { class: "bot-switch", id: "bot-switch", role: "tablist", "aria-label": "Bots" });
	const navTitle = h("div", { class: "nav-title", id: "nav-title" });
	const navSub = h("div", { class: "nav-sub", id: "nav-sub" });
	const navSearch = h("input", {
		type: "search",
		placeholder: "Filter conversations",
		"aria-label": "Filter conversations",
		oninput: (event: Event) => {
			navFilter = inputFrom(event)?.value ?? "";
			renderNav();
		},
	});
	const navSections = h("div", { class: "nav-scroll", id: "nav-sections" });
	const navViews = h("div", { class: "nav-foot", id: "nav-views", "aria-label": "Views" });
	const nav = h(
		"aside",
		{ class: "nav", id: "nav", "aria-label": "Conversations" },
		h("div", { class: "nav-head" }, botSwitch, navTitle, navSub),
		h("div", { class: "nav-search" }, navSearch),
		navSections,
		navViews,
	);

	const topTitle = h("span", { class: "top", id: "top-title" });
	const topSub = h("span", { class: "sub", id: "top-sub" });
	const topCover = h("button", {
		class: "cover-thumb",
		type: "button",
		hidden: true,
		"aria-label": "Open conversation cover",
		title: "Open conversation cover",
		onclick: () => {
			if (coverMedia) openLightbox(coverMedia, topCover);
		},
	});
	const topActions = h("div", { class: "topbar-actions", id: "top-actions" });
	const topbar = h("header", { class: "topbar" }, h("div", { class: "topbar-title" }, topCover, topTitle, topSub), topActions);
	const banner = h("div", { class: "banner", id: "banner", role: "status", hidden: true });

	const messageList = h("div", { class: "message-list", id: "message-list" });
	const streamList = h("div", { class: "stream-list", id: "stream-list" });
	const transcript = h(
		"section",
		{ class: "transcript", id: "transcript", role: "log", "aria-label": "Conversation transcript", tabindex: "0" },
		messageList,
		h("div", { class: "inline-views", id: "inline-views", "aria-live": "polite" }),
		streamList,
	);
	const inlineViews = transcript.querySelector<HTMLElement>("#inline-views")!;
	const widgetsAbove = h("div", { class: "widgets widgets-above", id: "widgets-above", "aria-label": "Runtime widgets above the composer" });
	const widgetsBelow = h("div", { class: "widgets widgets-below", id: "widgets-below", "aria-label": "Runtime widgets below the composer" });
	const widgetStatus = h("div", { class: "widget-status", id: "widget-status", role: "status" });
	const recipientsBar = h("div", { class: "recipients", id: "recipients" });
	const composerInput = h("textarea", {
		class: "composer-input",
		id: "composer-input",
		rows: "3",
		placeholder: "Write a message…",
		"aria-label": "Message text",
	});
	const composerSend = h(
		"button",
		{ class: "btn primary", type: "button", id: "composer-send", disabled: true },
		icon("send"),
		"Send",
	);
	const composerStop = h(
		"button",
		{ class: "btn danger", type: "button", id: "composer-stop", hidden: true },
		icon("stop"),
		"Stop",
	);
	const composerHint = h("span", { class: "composer-hint", id: "composer-hint" });
	const fileInput = h("input", {
		type: "file",
		class: "file-input",
		multiple: true,
		accept: "image/*,video/*,audio/*",
		hidden: true,
		tabindex: "-1",
		"aria-hidden": "true",
	});
	const composerMedia = h("div", { class: "composer-media", id: "composer-media", hidden: true });
	const composerAttach = h(
		"button",
		{
			class: "btn ghost icon",
			type: "button",
			id: "composer-attach",
			title: "Attach images, video or audio",
			"aria-label": "Attach files",
			onclick: () => fileInput.click(),
		},
		icon("attach"),
	);
	const composerSticker = h(
		"button",
		{
			class: "btn ghost icon",
			type: "button",
			id: "composer-sticker",
			title: "Stickers",
			"aria-label": "Open the sticker picker",
			onclick: () => void toggleStickerPicker(),
		},
		icon("sticker"),
	);
	const stickerPopover = h("div", { class: "sticker-popover", id: "sticker-popover", role: "dialog", "aria-label": "Stickers", hidden: true });
	const composer = h(
		"div",
		{ class: "composer", id: "composer" },
		composerMedia,
		composerInput,
		h(
			"div",
			{ class: "composer-row" },
			h("div", { class: "composer-tools" }, composerAttach, composerSticker),
			composerHint,
			h("div", { class: "composer-actions" }, composerStop, composerSend),
		),
		stickerPopover,
		fileInput,
	);
	const chatView = h(
		"div",
		{ class: "chat", id: "chat-view" },
		transcript,
		h("div", { class: "composer-wrap" }, widgetsAbove, recipientsBar, composer, widgetsBelow),
	);
	const panelView = h("div", { class: "panel-view", id: "panel-view", hidden: true });
	const view = h("div", { class: "view", id: "view" }, chatView, panelView);

	const asideTabs = h("div", { class: "tabs", id: "aside-tabs", role: "tablist", "aria-label": "Conversation details" });
	const asideBody = h("div", { class: "aside-body", id: "aside-body" });
	const aside = h("aside", { class: "aside", id: "aside", "aria-label": "Details" }, h("div", { class: "aside-head" }, asideTabs), asideBody);

	const statusbar = h(
		"footer",
		{ class: "statusbar", id: "statusbar", "aria-label": "Status" },
		h("div", { class: "statusbar-items", id: "statusbar-items" }),
		widgetStatus,
	);
	const main = h("main", { class: "main" }, topbar, banner, view, statusbar);

	const toasts = h("div", { class: "toasts", id: "toasts", "aria-live": "polite", "aria-atomic": "false" });
	const dialogs = h("div", { class: "dialog-layer", id: "dialogs" });
	const modals = h("div", { class: "modal-layer", id: "modals", hidden: true });
	const backdrop = h("div", { class: "drawer-backdrop", id: "drawer-backdrop", hidden: true });
	const dockTitle = h("span", { id: "dock-title", text: "Dock" });
	const dockBody = h("div", { class: "dock-body" });
	const dock = h("aside", { class: "dock", id: "dock", hidden: layout.dock === null, "aria-label": "Docked panel" },
		h("div", { class: "dock-head" }, dockTitle, h("button", { class: "btn ghost sm", type: "button", onclick: () => setDock(null) }, "Close")),
		dockBody,
	);
	dock.addEventListener("dragover", (event) => {
		event.preventDefault();
		dock.classList.add("drop");
	});
	dock.addEventListener("dragleave", () => dock.classList.remove("drop"));
	dock.addEventListener("drop", (event) => {
		event.preventDefault();
		dock.classList.remove("drop");
		const id = event.dataTransfer?.getData("text/plain") ?? "";
		const spec = workspaceById(id);
		if (!spec || spec.panel === null) return;
		setDock(spec.panel);
		if (activePanel === spec.panel) showChat();
	});

	app.append(mast, rail, nav, main, aside, dock, toasts, dialogs, modals, backdrop);

	refs = {
		app,
		mast,
		demoChip,
		themeButton,
		railBots,
		botSwitch,
		dock,
		dockTitle,
		dockBody,
		railFoot,
		nav,
		navTitle,
		navSub,
		navSearch,
		navSections,
		navViews,
		aside,
		asideTabs,
		asideBody,
		topTitle,
		topSub,
		topActions,
		banner,
		chatView,
		panelView,
		transcript,
		messageList,
		streamList,
		widgetsAbove,
		widgetsBelow,
		inlineViews,
		widgetStatus,
		recipients: recipientsBar,
		composer,
		composerInput,
		composerSend,
		composerStop,
		composerHint,
		composerMedia,
		composerAttach,
		composerSticker,
		composerFile: fileInput,
		stickerPopover,
		topCover,
		statusbar,
		toasts,
		dialogs,
		modals,
		backdrop,
	};
	shellReady = true;
}

function wireStaticEvents(): void {
	refs.transcript.addEventListener("mousedown", (event) => {
		const target = event.target;
		if (!(target instanceof HTMLElement)) return;
		if (target.closest("button, a, input, textarea, summary")) return;
		event.preventDefault();
	});
	refs.transcript.addEventListener("scroll", () => {
		const node = refs.transcript;
		stickToBottom = node.scrollHeight - node.scrollTop - node.clientHeight < 56;
	});
	refs.composerInput.addEventListener("input", () => {
		if (selectedConversationId) drafts.set(selectedConversationId, refs.composerInput.value);
		autoGrow();
		updateComposerState();
	});
	refs.composerInput.addEventListener("keydown", (event) => {
		if ((event.metaKey || event.ctrlKey) && event.key === "Enter") {
			event.preventDefault();
			void sendMessage();
		}
	});
	refs.composerSend.addEventListener("click", () => void sendMessage());
	refs.composerStop.addEventListener("click", () => void stopActive());
	refs.composerFile.addEventListener("change", () => {
		const files = refs.composerFile.files;
		refs.composerFile.value = "";
		if (files && files.length) void addAttachments([...files]);
	});
	refs.composerInput.addEventListener("paste", (event) => {
		const files = clipboardFiles(event);
		if (!files.length) return;
		event.preventDefault();
		void addAttachments(files);
	});
	refs.composer.addEventListener("dragover", (event) => {
		if (!event.dataTransfer) return;
		event.preventDefault();
		event.dataTransfer.dropEffect = "copy";
		refs.composer.classList.add("dragover");
	});
	refs.composer.addEventListener("dragleave", (event) => {
		const next = event.relatedTarget;
		if (next instanceof Node && refs.composer.contains(next)) return;
		refs.composer.classList.remove("dragover");
	});
	refs.composer.addEventListener("drop", (event) => {
		event.preventDefault();
		refs.composer.classList.remove("dragover");
		const files = event.dataTransfer ? [...event.dataTransfer.files] : [];
		if (files.length) void addAttachments(files);
	});
	refs.stickerPopover.addEventListener("keydown", (event) => {
		if (event.key !== "Escape") return;
		event.preventDefault();
		closeStickerPicker();
	});
	document.addEventListener("click", (event) => {
		if (refs.stickerPopover.hidden) return;
		const target = event.target;
		if (!(target instanceof Node)) return;
		if (refs.stickerPopover.contains(target) || refs.composerSticker.contains(target)) return;
		closeStickerPicker();
	});
	refs.backdrop.addEventListener("click", closeDrawers);
	document.addEventListener("keydown", (event) => {
		if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
			event.preventDefault();
			openPalette();
			return;
		}
		if ((event.metaKey || event.ctrlKey) && event.key === "\\") {
			event.preventDefault();
			toggleDock();
			return;
		}
		if ((event.metaKey || event.ctrlKey) && event.shiftKey && event.key.toLowerCase() === "n") {
			event.preventDefault();
			openConversationEditor();
			return;
		}
		if ((event.metaKey || event.ctrlKey) && event.shiftKey && event.key.toLowerCase() === "c") {
			event.preventDefault();
			openPanel("character");
			return;
		}
		if (isTypingTarget(event.target)) return;
		if (event.metaKey || event.ctrlKey || event.altKey) return;
		if (event.key === "?") {
			event.preventDefault();
			openShortcuts();
			return;
		}
		if (event.key === "g") {
			goChord = Date.now();
			return;
		}
		if (goChord !== 0 && Date.now() - goChord < 900) {
			goChord = 0;
			const spec = [...BUILTIN_WORKSPACES, ...extraWorkspaces].find((item) => item.shortcut === event.key.toLowerCase());
			if (spec) {
				event.preventDefault();
				openWorkspace(spec);
			}
			return;
		}
		goChord = 0;
	});
	window.addEventListener("online", () => {
		reconnectDelay = 1000;
		connectEvents();
	});
	window.addEventListener("offline", () => setConnection("offline"));
	document.addEventListener("visibilitychange", () => {
		if (document.visibilityState !== "visible") return;
		if (connection !== "online") {
			reconnectDelay = 1000;
			connectEvents();
		} else {
			void refresh().catch(() => undefined);
		}
	});
	window.addEventListener("hashchange", () => restoreRoute());
	window.matchMedia("(prefers-color-scheme: light)").addEventListener("change", () => {
		if (themeSetting === "system") applyTheme("system");
	});
}

/* ------------------------------------------------------------------ */
/* rendering: shell chrome                                             */
/* ------------------------------------------------------------------ */

function renderAll(): void {
	ensureSelection();
	applyAppearance();
	renderMast();
	renderRail();
	renderNav();
	renderTopbar();
	renderView();
	renderChat();
	renderAside();
	renderDock();
	renderStatusbar();
	renderBanner();
}

function renderMast(): void {
	const light = document.documentElement.dataset["theme"] === "light";
	refs.themeButton.replaceChildren(icon(light ? "moon" : "sun", 16), h("span", { class: "theme-label", text: light ? "Dark" : "Light" }));
	refs.themeButton.title = light ? "Switch to dark" : "Switch to light";
	refs.themeButton.setAttribute("aria-label", refs.themeButton.title);
}

async function toggleTheme(): Promise<void> {
	const next = document.documentElement.dataset["theme"] === "light" ? "dark" : "light";
	applyTheme(next);
	themeDirty = false;
	renderMast();
	try {
		await command("settings.save", { settings: { ...currentSettings(), theme: next } });
		await refresh().catch(() => undefined);
	} catch (error) {
		notify(errText(error), "error");
	}
}

async function markDemo(): Promise<void> {
	try {
		const health = await command<{ demo?: boolean }>("desktop.health");
		refs.demoChip.hidden = health?.demo !== true;
	} catch {
		refs.demoChip.hidden = true;
	}
}

function renderDock(): void {
	const id = layout.dock;
	refs.app.classList.toggle("has-dock", id !== null);
	refs.dock.hidden = id === null;
	if (id === null) {
		dockKey = "";
		return;
	}
	const panel = panels.get(id);
	refs.dockTitle.textContent = panel?.title ?? "Dock";
	if (id === activePanel) {
		if (dockKey === "stage") return;
		dockKey = "stage";
		refs.dockBody.replaceChildren(
			h("div", { class: "desk-empty" },
				h("h2", { text: "Already on stage" }),
				h("p", { text: "This panel is the main view. Dock a different one to keep it beside the conversation." })),
		);
		return;
	}
	if (dockKey === id && refs.dockBody.childElementCount > 0) return;
	dockKey = id;
	if (!panel) {
		refs.dockBody.replaceChildren(h("p", { class: "lede", text: "That panel is no longer registered." }));
		return;
	}
	try {
		refs.dockBody.replaceChildren(panel.render(state));
	} catch (error) {
		refs.dockBody.replaceChildren(h("div", { class: "invalid", text: errText(error) }));
	}
}

function renderRail(): void {
	refs.railBots.replaceChildren();
	let previousGroup = "";
	for (const spec of visibleRail()) {
		if (previousGroup.length > 0 && previousGroup !== spec.group) refs.railBots.append(h("span", { class: "rail-sep", "aria-hidden": "true" }));
		previousGroup = spec.group;
		const active = spec.panel === null ? activePanel === null : activePanel === spec.panel;
		const glyph = WORKSPACE_ICONS[spec.id] ?? "node";
		const button = h(
			"button",
			{
				class: `rail-link${active ? " on" : ""}`,
				type: "button",
				title: `${spec.title}. ${spec.hint}. Drag to reorder. Right-click to dock.`,
				"aria-label": spec.title,
				"aria-current": active ? "page" : undefined,
				"data-workspace": spec.id,
				onclick: () => { if (!suppressRailClick) openWorkspace(spec); },
			},
			icon(glyph, 18),
		);
		button.addEventListener("pointerdown", (event) => {
			if (event.button !== 0) return;
			const pointerId = event.pointerId;
			const startY = event.clientY;
			let dragging = false;
			let targetId = "";
			const onMove = (ev: PointerEvent): void => {
				if (ev.pointerId !== pointerId) return;
				if (!dragging && Math.abs(ev.clientY - startY) < 8) return;
				dragging = true;
				suppressRailClick = true;
				targetId = "";
				for (const item of refs.railBots.querySelectorAll<HTMLElement>(".rail-link")) {
					const box = item.getBoundingClientRect();
					const hit = ev.clientY >= box.top && ev.clientY <= box.bottom;
					item.classList.toggle("drop", hit && item.dataset["workspace"] !== spec.id);
					if (hit) targetId = item.dataset["workspace"] ?? "";
				}
			};
			const onUp = (ev: PointerEvent): void => {
				if (ev.pointerId !== pointerId) return;
				window.removeEventListener("pointermove", onMove);
				window.removeEventListener("pointerup", onUp);
				for (const item of refs.railBots.querySelectorAll(".rail-link")) item.classList.remove("drop");
				if (dragging && targetId.length > 0 && targetId !== spec.id) {
					layout = moveInRail(layout, spec.id, layout.rail.indexOf(targetId));
					persistLayout();
					refs.railBots.querySelector<HTMLButtonElement>(`[data-workspace="${spec.id}"]`)?.focus();
				}
				window.setTimeout(() => { suppressRailClick = false; }, 0);
			};
			window.addEventListener("pointermove", onMove);
			window.addEventListener("pointerup", onUp);
		});
		button.addEventListener("keydown", (event) => {
			if (!event.altKey || (event.key !== "ArrowUp" && event.key !== "ArrowDown")) return;
			event.preventDefault();
			const index = layout.rail.indexOf(spec.id);
			const to = event.key === "ArrowUp" ? Math.max(0, index - 1) : Math.min(layout.rail.length - 1, index + 1);
			if (to === index) return;
			layout = moveInRail(layout, spec.id, to);
			persistLayout();
			refs.railBots.querySelector<HTMLButtonElement>(`[data-workspace="${spec.id}"]`)?.focus();
		});
		button.addEventListener("contextmenu", (event) => {
			if (spec.panel === null) return;
			event.preventDefault();
			setDock(layout.dock === spec.panel ? null : spec.panel);
		});
		refs.railBots.append(button);
	}
}

function selectBot(id: string | null): void {
	selectedBotId = id;
	if (id) {
		const owned = state.conversations.filter((conversation) => conversation.botIds.includes(id));
		const current = conversationOf(selectedConversationId);
		if (!current || !current.botIds.includes(id)) {
			const pick = owned.slice().sort((a, b) => b.updatedAt - a.updatedAt)[0];
			selectedConversationId = pick?.id ?? null;
		}
	}
	renderAll();
	syncHostSelection();
	activateBotPlugins();
}

function renderBotSwitch(): void {
	refs.botSwitch.replaceChildren();
	const all = h("button", {
		class: `bot-chip${selectedBotId === null ? " on" : ""}`,
		type: "button",
		role: "tab",
		"aria-selected": selectedBotId === null ? "true" : "false",
		onclick: () => selectBot(null),
	}, "All");
	refs.botSwitch.append(all);
	for (const bot of state.bots) {
		const busy = state.conversations.some((conversation) => conversation.botIds.includes(bot.id) && hasActiveStream(conversation.id));
		refs.botSwitch.append(h("button", {
			class: `bot-chip${selectedBotId === bot.id ? " on" : ""}`,
			type: "button",
			role: "tab",
			title: bot.name,
			"aria-selected": selectedBotId === bot.id ? "true" : "false",
			onclick: () => selectBot(bot.id),
		}, initials(bot.name), busy ? h("span", { class: "dot run", "aria-hidden": "true" }) : false));
	}
	refs.botSwitch.append(h("button", {
		class: "bot-chip add",
		type: "button",
		title: "Create a bot",
		"aria-label": "Create a bot",
		onclick: () => openBotEditor(),
	}, icon("plus", 14)));
}

function renderNav(): void {
	renderBotSwitch();
	if (document.activeElement instanceof HTMLElement && refs.navViews.contains(document.activeElement) && activePanel !== document.activeElement.dataset["panel"]) {
		document.activeElement.blur();
	}
	refs.navTitle.textContent = selectedBotId ? botName(selectedBotId) : "All bots";
	const nodeId = currentSettings().nodeId;
	const shortNode = nodeId ? nodeId.split(":").filter(Boolean).slice(-1)[0] : "node not configured";
	refs.navSub.textContent = `${state.conversations.length} conversation${state.conversations.length === 1 ? "" : "s"} · ${shortNode}`;

	const needle = navFilter.trim().toLowerCase();
	const pool = state.conversations
		.filter((conversation) => !selectedBotId || conversation.botIds.includes(selectedBotId))
		.filter((conversation) => {
			if (!needle) return true;
			if (conversation.title.toLowerCase().includes(needle)) return true;
			return conversation.botIds.some((id) => botName(id).toLowerCase().includes(needle));
		})
		.slice()
		.sort((a, b) => b.updatedAt - a.updatedAt);

	refs.navSections.replaceChildren();
	if (!state.bots.length) {
		refs.navSections.append(
			h(
				"div",
				{ class: "empty-nav" },
				h("div", { text: "No bots yet." }),
				h("button", { class: "btn primary", type: "button", style: "margin-top:8px", onclick: () => openBotEditor() }, icon("plus"), "Create a bot"),
			),
		);
	} else {
		refs.navSections.append(
			h(
				"div",
				{ class: "section" },
				h("span", { text: "Conversations" }),
				h("button", { class: "section-action", type: "button", onclick: () => openConversationEditor() }, "+ New"),
			),
		);
		if (!pool.length) {
			refs.navSections.append(
				h(
					"div",
					{ class: "empty-nav" },
					h("div", { text: selectedBotId ? `${botName(selectedBotId)} has no conversations.` : "No conversations match the filter." }),
					h(
						"button",
						{ class: "btn", type: "button", style: "margin-top:8px", onclick: () => openConversationEditor() },
						"Start a conversation",
					),
				),
			);
		}
		for (const conversation of pool) {
			const active = conversation.id === selectedConversationId;
			const busy = hasActiveStream(conversation.id);
			const last = Math.max(lastMessageAt(conversation.id), conversation.updatedAt);
			const memberLabel = conversation.botIds.map((id) => botName(id)).join(", ");
			const button = h(
				"button",
				{
					class: `chan${active ? " on" : ""}`,
					type: "button",
					"aria-current": active ? "page" : undefined,
					title: `${conversation.title} — ${memberLabel}`,
					onclick: () => selectConversation(conversation.id),
				},
				h("span", { class: "chan-icon", text: conversation.kind === "group" ? "G" : "D" }),
				h("span", { class: "chan-title", text: conversation.title }),
				busy ? h("span", { class: "chan-dot", title: "A bot is responding" }) : h("span", { class: "chan-meta", text: relativeTime(last) }),
			);
			refs.navSections.append(button);
		}
	}
	renderNavViews();
	syncChrome();
}

const DESK_PANELS = new Set(["inbox", "neighbors", "trust", "threads", "resources", "health", "integrations", "team"]);

let libraryOpen = false;
let suppressRailClick = false;

function developerMode(): boolean {
	return localStorage.getItem("onlyne.desk.developer") === "1";
}

function syncChrome(): void {
	const docked = layout.dock !== null;
	refs.app.classList.toggle("members-hidden", docked || activePanel !== null || !membersOpen);
}

function renderNavViews(): void {
	refs.navSections.querySelectorAll(".library").forEach(node => node.remove());
	refs.navViews.replaceChildren();
	const railPanels = new Set(visibleRail().flatMap((item) => item.panel === null ? [] : [item.panel]));
	const activeBots = uiActiveBotIds();
	const entries = [...panels.values()].filter((panel) => !railPanels.has(panel.id) && (!panel.botId || activeBots.has(panel.botId)));
	if (entries.length === 0) return;
	const details = h("details", { class: "library", open: libraryOpen });
	const summary = h("summary", { text: "Library" });
	details.append(summary);
	for (const entry of entries) {
		const active = activePanel === entry.id;
		const button = h("button", {
			class: `view-link${active ? " on" : ""}`,
			type: "button",
			"data-panel": entry.id,
			"aria-current": active ? "page" : undefined,
			onclick: () => openPanel(entry.id),
		}, entry.title);
		details.append(button);
	}
	details.addEventListener("toggle", () => { libraryOpen = details.open; });
	refs.navSections.append(details);
}

function renderTopbar(): void {
	const panel = activePanel ? panels.get(activePanel) : null;
	const bar = refs.topTitle.closest(".topbar");
	bar?.classList.toggle("quiet", panel !== null);
	renderCover(panel ? undefined : selectedConversation());
	if (panel) {
		const place = [...BUILTIN_WORKSPACES, ...extraWorkspaces].find((item) => item.panel === activePanel);
		refs.topTitle.textContent = panel.title;
		refs.topSub.textContent = place?.hint || "Panel";
	} else {
		const conversation = selectedConversation();
		refs.topTitle.textContent = conversation?.title ?? "No conversation";
		if (!conversation) {
			refs.topSub.textContent = state.bots.length ? "Choose or create a conversation" : "Create a bot to begin";
		} else {
			const messages = messagesOf(conversation.id);
			const busy = activeBotIds(conversation.id).length;
			refs.topSub.textContent = `${conversation.kind} · ${messages.length} message${messages.length === 1 ? "" : "s"} · ${conversation.botIds.length} bot${conversation.botIds.length === 1 ? "" : "s"}${busy ? " · responding" : ""}`;
		}
	}

	const actions: HTMLElement[] = [];
	actions.push(
		h(
			"button",
			{ class: "btn icon ghost only-mobile", type: "button", "aria-label": "Open navigation", title: "Navigation", onclick: toggleNav },
			icon("menu"),
		),
	);
	if (panel) {
		actions.push(h("button", { class: "btn ghost", type: "button", onclick: showChat }, "Conversations"));
		actions.push(
			h("button", { class: "btn", type: "button", title: "Refresh this view", onclick: () => renderPanel() }, icon("refresh"), "Refresh"),
		);
	} else {
		const conversation = selectedConversation();
		actions.push(
			h(
				"button",
				{
					class: "btn ghost icon",
					type: "button",
					"aria-label": "Toggle members and runtime panel",
					"aria-pressed": String(membersOpen),
					title: "Members and runtime",
					onclick: toggleMembers,
				},
				icon("node"),
			),
			h(
				"button",
				{ class: "btn", type: "button", disabled: !conversation, title: "Choose a model for this conversation", onclick: () => openModelsModal() },
				"Models",
			),
			h(
				"button",
				{ class: "btn ghost", type: "button", disabled: !conversation, onclick: () => conversation && openConversationEditor(conversation.id) },
				"Settings",
			),
			h(
				"button",
				{ class: "btn ghost icon", type: "button", "aria-label": `Command palette (${chord(MOD, "K")})`, title: `Command palette (${chord(MOD, "K")})`, onclick: openPalette },
				icon("command"),
			),
		);
	}
	refs.topActions.replaceChildren(...actions);
}

function renderCover(conversation: Conversation | undefined): void {
	const bot = conversation ? botOf(conversation.adminBotId) ?? botOf(conversation.botIds[0]) : botOf(selectedBotId);
	const item = botAvatarOf(bot) ?? null;
	coverMedia = item;
	refs.topCover.hidden = !item;
	if (!item) {
		refs.topCover.replaceChildren();
		delete refs.topCover.dataset["media"];
		return;
	}
	refs.topCover.title = `${item.name} — open cover`;
	if (refs.topCover.dataset["media"] === item.id) return;
	refs.topCover.dataset["media"] = item.id;
	refs.topCover.replaceChildren(h("img", { class: "cover-img", src: mediaUrl(item), alt: "", loading: "lazy", decoding: "async" }));
}

function renderView(): void {
	let panel = activePanel ? panels.get(activePanel) : undefined;
	if (panel?.botId && !uiActiveBotIds().has(panel.botId)) {
		activePanel = null;
		panel = undefined;
		renderTopbar();
		renderNav();
	}
	if (activePanel && !panel) activePanel = null;
	refs.chatView.hidden = Boolean(panel);
	refs.panelView.hidden = !panel;
}

function renderBanner(): void {
	if (sessionExpired) {
		refs.banner.hidden = false;
		refs.banner.className = "banner err";
		refs.banner.replaceChildren(
			h("span", { text: "The launcher session expired. Reopen Onlyne Desktop through its launcher URL." }),
			h("button", { class: "btn", type: "button", onclick: () => location.reload() }, "Reload"),
		);
		return;
	}
	if (connection === "offline") {
		refs.banner.hidden = false;
		refs.banner.className = "banner warn";
		refs.banner.replaceChildren(
			h("span", { text: "Disconnected from the local Onlyne host. Retrying automatically." }),
			h(
				"button",
				{
					class: "btn",
					type: "button",
					onclick: () => {
						reconnectDelay = 1000;
						connectEvents();
					},
				},
				"Retry now",
			),
		);
		return;
	}
	refs.banner.hidden = true;
	refs.banner.replaceChildren();
}

function renderStatusbar(): void {
	const items: HTMLElement[] = [];
	const connectionLabel = connection === "online" ? "Connected" : connection === "offline" ? "Disconnected" : "Connecting…";
	items.push(
		h(
			"span",
			{ class: `status-item ${connection === "online" ? "online" : connection === "offline" ? "offline" : ""}` },
			h("span", { class: `dot ${connection === "online" ? "ok" : connection === "offline" ? "err" : "pend"}` }),
			connectionLabel,
		),
	);
	if (layout.dock) {
		const docked = panels.get(layout.dock);
		items.push(h("button", {
			class: "status-item docked",
			type: "button",
			title: "Close the dock",
			onclick: () => setDock(null),
		}, `Docked ${docked?.title ?? layout.dock}`));
	}
	const conversation = selectedConversation();
	if (conversation) items.push(h("span", { class: "status-item", title: conversation.id, text: conversation.title }));
	const auto = conversation ? autoStatus.get(conversation.id) : undefined;
	if (conversation && conversation.scene?.group?.auto === true) {
		items.push(
			h(
				"button",
				{
					class: `status-item auto${auto?.active ? " online" : ""}`,
					type: "button",
					title: "Automatic group turns — open the scene settings",
					onclick: () => openPanel("character"),
				},
				h("span", { class: `dot ${auto?.active ? "ok" : "pend"}` }),
				auto && auto.turns > 0 ? `auto ${auto.turns}/${auto.max}` : "auto on",
			),
		);
	}
	const nodeId = currentSettings().nodeId;
	if (nodeId) {
		items.push(
			h(
				"button",
				{ class: "status-item node", type: "button", title: nodeId, onclick: () => openPanel("node") },
				h("span", { class: `dot ${connection === "online" ? "ok" : "none"}` }),
				middleEllipsis(nodeId, 10, 8),
			),
		);
	}
	const frames = framesByConversation.get(selectedConversationId ?? "")?.length ?? 0;
	items.push(h("span", { class: "status-item", title: "Runtime frames received in this conversation", text: `${frames} frame${frames === 1 ? "" : "s"}` }));
	if (errors.length) {
		items.push(
			h(
				"button",
				{ class: "status-error", type: "button", title: "Open the activity view", onclick: () => openPanel("activity") },
				`${errors.length} error${errors.length === 1 ? "" : "s"}`,
			),
		);
	}
	refs.statusbar.replaceChildren(h("div", { class: "statusbar-items" }, ...items), refs.widgetStatus);
}

function toggleMembers(): void {
	membersOpen = !membersOpen;
	refs.app.classList.toggle("members-open", membersOpen);
	refs.app.classList.toggle("members-hidden", !membersOpen);
	renderTopbar();
}

function toggleNav(): void {
	const open = !refs.app.classList.contains("nav-open");
	refs.app.classList.toggle("nav-open", open);
	refs.backdrop.hidden = !open;
}

function closeDrawers(): void {
	refs.app.classList.remove("nav-open");
	refs.backdrop.hidden = true;
}

/* ------------------------------------------------------------------ */
/* rendering: chat                                                     */
/* ------------------------------------------------------------------ */

function syncComposerForConversation(): void {
	if (composerConversationId === selectedConversationId) return;
	if (composerConversationId) drafts.set(composerConversationId, refs.composerInput.value);
	composerConversationId = selectedConversationId;
	closeStickerPicker();
	refs.composerInput.value = selectedConversationId ? drafts.get(selectedConversationId) ?? "" : "";
	renderComposerMedia();
	autoGrow();
}

function autoGrow(): void {
	const input = refs.composerInput;
	input.style.height = "auto";
	input.style.height = `${Math.min(input.scrollHeight, Math.floor(window.innerHeight * 0.34))}px`;
}

function renderChat(): void {
	syncComposerForConversation();
	renderComposerMedia();
	renderMessages();
	renderStreams();
	renderRecipients();
	updateComposerState();
}

function emptyTranscript(): HTMLElement {
	if (!state.bots.length) {
		return h(
			"div",
			{ class: "transcript-empty" },
			h("strong", { text: "No bots yet" }),
			h("div", { class: "panel-note", text: "Create a bot with a name, instructions and an OMP runtime. Then start a conversation with it." }),
			h("button", { class: "btn primary", type: "button", style: "margin-top:12px", onclick: () => openBotEditor() }, icon("plus"), "Create a bot"),
		);
	}
	if (!state.conversations.length) {
		return h(
			"div",
			{ class: "transcript-empty" },
			h("strong", { text: "No conversations yet" }),
			h("div", { class: "panel-note", text: "Start a direct conversation with one bot, or a group with several bots and an admin bot." }),
			h("button", { class: "btn primary", type: "button", style: "margin-top:12px", onclick: () => openConversationEditor() }, "Start a conversation"),
		);
	}
	const conversation = selectedConversation();
	const admin = conversation ? botOf(conversation.adminBotId || conversation.botIds[0] || "") : undefined;
	const hasModel = conversation?.botIds.some((id) => Boolean(botOf(id)?.model)) === true;
	const actions = h("div", { class: "transcript-actions" });
	if (!hasModel) {
		actions.append(
			h("button", { class: "btn primary", type: "button", onclick: () => openModelsModal() }, "Choose a model"),
			h("button", { class: "btn", type: "button", onclick: () => openPanel("character") }, "Import a character"),
		);
		return h(
			"div",
			{ class: "transcript-empty" },
			h("strong", { text: `Set up ${admin?.name ?? "your bot"}` }),
			h("div", { class: "panel-note", text: "This bot has no model yet. Pick one from its runtime catalog, or import a SillyTavern card and then choose a model." }),
			actions,
		);
	}
	if (conversation?.greeting && conversation.greeted !== true) {
		actions.append(
			h("button", { class: "btn primary", type: "button", onclick: () => void startGreeting(conversation.id) }, "Start the scene"),
			h("button", { class: "btn", type: "button", onclick: () => openPanel("character") }, "Character & scene"),
		);
		return h(
			"div",
			{ class: "transcript-empty" },
			h("strong", { text: `${admin?.name ?? "Your bot"} is ready` }),
			h("div", { class: "panel-note", text: "The character card has an opening greeting. Start the scene, or type your own first line below." }),
			actions,
		);
	}
	return h(
		"div",
		{ class: "transcript-empty" },
		h("strong", { text: "No messages in this conversation" }),
		h("div", { class: "panel-note", text: "Sent messages appear here once the host admits them. Bot work streams below the transcript as it happens." }),
	);
}

async function startGreeting(conversationId: string): Promise<void> {
	try {
		await command("conversation.greet", { conversationId });
		await refresh().catch(() => undefined);
	} catch (error) {
		notify(errText(error), "error");
	}
}

function renderMessages(): void {
	const messages = currentMessages();
	const signature = `${selectedConversationId ?? ""}|${JSON.stringify(messages)}`;
	if (signature === messageSignature) return;
	messageSignature = signature;
	refs.messageList.replaceChildren();
	if (!messages.length) {
		refs.messageList.append(emptyTranscript());
	} else {
		for (const id of [...failedDelivery.keys()]) {
			const message = messages.find(item => item.id === id);
			if (message === undefined) failedDelivery.delete(id);
			else if (messages.some(item => item.conversationId === message.conversationId && item.role === "assistant" && item.createdAt >= message.createdAt)) failedDelivery.delete(id);
		}
		for (const message of messages) refs.messageList.append(renderMessage(message));
		const typing = selectedConversationId ? typingBots.get(selectedConversationId) : undefined;
		if (typing) {
			refs.messageList.append(h("div", { class: "typing", role: "status" }, `${botName(typing.botId) || "The bot"} is typing${typing.demo ? " · demo" : ""}`));
		}
	}
	if (stickToBottom) scrollToBottom();
}

function scrollToBottom(): void {
	const node = refs.transcript;
	node.scrollTop = node.scrollHeight;
}

function renderMessage(message: ChatMessage): HTMLElement {
	const role = message.role;
	const author = messageAuthor(message);
	const element = h("article", { class: `msg ${role}`, "data-id": message.id, "aria-label": `${author} message` });
	const head = h(
		"div",
		{ class: "msg-head" },
		role === "user" ? false : avatarNode(author, botAvatarOf(botOf(message.botId))),
		h("span", { class: "msg-author", text: role === "user" ? "You" : author }),
		role === "user" ? false : h("span", { class: "msg-role", text: role }),
		(() => {
			const when = formatWhen(message.createdAt);
			return h("time", { class: "msg-time", datetime: new Date(message.createdAt).toISOString(), title: when.title, text: when.label });
		})(),
		message.usage ? usageChips(message.usage) : null,
	);

	const body = h("div", { class: "msg-body" });
	const custom = messageRenderers.get(role) ?? [];
	let renderer: ((message: unknown) => HTMLElement) | null = null;
	for (const entry of custom) if (!entry.botId) renderer = entry.render;
	for (const entry of custom) if (entry.botId && entry.botId === message.botId) renderer = entry.render;
	if (renderer) {
		try {
			const node = renderer(message);
			if (node instanceof HTMLElement) body.append(node);
			else body.append(h("div", { class: "invalid", text: "This message renderer returned no element." }));
		} catch (error) {
			body.append(h("div", { class: "invalid", text: `Message renderer failed: ${errText(error)}` }));
		}
	} else if (role === "tool") {
		body.append(
			h(
				"details",
				{ class: "block tool" },
				h("summary", {}, h("span", { class: "block-head" }, h("span", { class: "tool-name", text: botOf(message.botId) ? `${botName(message.botId)} tool` : "tool" }))),
				h("div", { class: "block-body" }, h("pre", { text: message.text || textOf(message.content) })),
			),
		);
	} else if (role === "system") {
		body.append(h("div", { class: "block notice", text: message.text }));
	} else if (Array.isArray(message.content) && message.content.length) {
		for (const item of message.content) appendContentItem(body, item);
	} else {
		body.append(textBlock(message.text));
	}

	const media = Array.isArray(message.media) ? message.media : [];
	if (media.length) body.append(mediaGallery(media));
	if (message.role === "assistant" && message.text.startsWith("Demo reply")) body.prepend(h("span", { class: "chip warn", text: "Demo" }));
	const failure = failedDelivery.get(message.id);
	if (failure) {
		body.append(h("div", { class: "send-failed" },
			h("p", { text: failure }),
			h("button", { class: "btn sm", type: "button", onclick: () => void retrySend(message.conversationId) }, "Retry"),
		));
	}
	const lastAssistant = message.role === "assistant" && currentMessages().filter((item) => item.role === "assistant").at(-1)?.id === message.id;

	const actions = h(
		"div",
		{ class: "msg-actions" },
		h(
			"button",
			{ class: "msg-action", type: "button", title: "Copy message text", onclick: () => void copyText(plainText(message), "Message copied") },
			"Copy",
		),
		role === "user" || role === "assistant"
			? h(
					"button",
					{ class: "msg-action", type: "button", title: "Quote in the composer", onclick: () => quoteMessage(message) },
					"Reply",
				)
			: null,
		lastAssistant && selectedConversationId
			? h(
					"button",
					{ class: "msg-action", type: "button", title: "Delete this reply and generate it again", onclick: () => void regenerateReply(selectedConversationId ?? "") },
					"Regenerate",
				)
			: null,
		h(
			"button",
			{ class: "msg-action", type: "button", title: "Show the message JSON", onclick: () => openJsonModal("Message", message) },
			"Details",
		),
	);
	element.append(head, body, actions);
	return element;
}

async function regenerateReply(conversationId: string): Promise<void> {
	if (!conversationId) return;
	try {
		await command("message.regenerate", { conversationId });
		await refresh().catch(() => undefined);
	} catch (error) {
		notify(errText(error), "error");
	}
}

function messageAuthor(message: ChatMessage): string {
	if (message.role === "user") return "You";
	if (message.role === "system") return "System";
	if (message.role === "tool") return message.botId ? `${botName(message.botId)} tool` : "Tool";
	return message.botId ? botName(message.botId) : "Assistant";
}

function plainText(message: ChatMessage): string {
	if (message.text) return message.text;
	const parts: string[] = [];
	if (Array.isArray(message.content)) {
		for (const item of message.content) {
			if (!isObj(item)) {
				parts.push(textOf(item));
				continue;
			}
			if (item.type === "text") parts.push(String(item.text ?? ""));
			else if (item.type === "thinking") parts.push(String(item.thinking ?? ""));
			else if (item.type === "toolCall") parts.push(`${String(item.name ?? "tool")} ${compactJson(item.arguments ?? {})}`);
			else parts.push(textOf(item));
		}
	}
	return parts.filter(Boolean).join("\n\n");
}

function usageChips(usage: Record<string, number>): HTMLElement | null {
	const entries = Object.entries(usage).filter(([, value]) => typeof value === "number") as Array<[string, number]>;
	if (!entries.length) return null;
	return h(
		"span",
		{ class: "msg-usage" },
		...entries
			.slice(0, 4)
			.map(([key, value]) => h("span", { class: "chip dim", title: `${key}: ${value}`, text: `${key} ${compactNumber(value)}` })),
	);
}

function textBlock(text: string): HTMLElement {
	return h("div", { class: "block text", text });
}

function thinkingBlock(text: string, open: boolean): HTMLElement {
	const details = h(
		"details",
		{ class: "block thinking", open },
		h("summary", {}, h("span", { class: "block-head" }, h("span", { class: "tool-name", text: "thinking" }), h("span", { class: "tool-args", text: `${text.length} chars` }))),
		h("div", { class: "block-body", text }),
	);
	return details;
}

function rawBlock(value: unknown): HTMLElement {
	return h(
		"details",
		{ class: "block raw" },
		h("summary", {}, h("span", { class: "block-head" }, h("span", { class: "tool-name", text: "block" }))),
		h("div", { class: "block-body" }, h("pre", { text: safeJson(value) })),
	);
}

function imageBlock(item: Record<string, unknown>): HTMLElement {
	const data = typeof item["data"] === "string" ? item["data"] : "";
	const mime = typeof item["mimeType"] === "string" ? item["mimeType"] : "image/png";
	if (!data) return rawBlock(item);
	const image = h("img", { src: `data:${mime};base64,${data}`, alt: "Attached image" });
	image.style.maxWidth = "100%";
	image.style.borderRadius = "8px";
	return image;
}

function mediaFigure(item: MediaItem): HTMLElement {
	const url = mediaUrl(item);
	const figure = h("figure", { class: `media media-${item.kind}`, "data-media": item.id });
	if (item.kind === "image" || item.kind === "svg" || item.kind === "sticker") {
		const thumb = h("img", { class: "media-img", src: url, alt: item.name, loading: "lazy", decoding: "async" });
		figure.append(
			h(
				"button",
				{
					class: "media-open",
					type: "button",
					title: `Open ${item.name}`,
					"aria-label": `Open ${item.name}`,
					onclick: () => openLightbox(item, thumb),
				},
				thumb,
			),
		);
	} else if (item.kind === "video") {
		figure.append(h("video", { class: "media-video", src: url, controls: true, playsinline: true, preload: "metadata" }));
		figure.append(h("figcaption", { class: "media-caption", text: `${item.name} · ${formatBytes(item.size)}` }));
	} else if (item.kind === "audio") {
		figure.append(h("audio", { class: "media-audio", src: url, controls: true, preload: "metadata" }));
		figure.append(h("figcaption", { class: "media-caption", text: `${item.name} · ${formatBytes(item.size)}` }));
	} else {
		figure.append(
			h(
				"a",
				{ class: "media-file", href: url, download: item.name, title: `Download ${item.name}` },
				icon("attach", 14),
				h("span", { class: "media-file-name", text: item.name }),
				h("span", { class: "media-file-size", text: formatBytes(item.size) }),
			),
		);
	}
	return figure;
}

function mediaGallery(items: MediaItem[]): HTMLElement {
	return h("div", { class: `media-grid count-${Math.min(items.length, 4)}` }, ...items.map(mediaFigure));
}

function toolCallBlock(item: Record<string, unknown>): HTMLElement {
	const name = String(item["name"] ?? "tool");
	const args = safeJson(item["arguments"] ?? {});
	return h(
		"details",
		{ class: "block tool", "data-status": "running" },
		h(
			"summary",
			{},
			h(
				"span",
				{ class: "block-head" },
				h("span", { class: "dot run" }),
				h("span", { class: "tool-name", text: name }),
				h("span", { class: "tool-args", text: truncate(normalizeSpace(args), 120) }),
				h("span", { class: "tool-state", text: "call" }),
			),
		),
		h("div", { class: "block-body" }, h("pre", { text: args })),
	);
}

function toolResultBlock(item: Record<string, unknown>): HTMLElement {
	const output = textOf(item["output"] ?? item["text"] ?? item["content"] ?? item);
	return h(
		"details",
		{ class: "block tool", "data-status": item["isError"] === true ? "error" : "ok" },
		h(
			"summary",
			{},
			h("span", { class: "block-head" }, h("span", { class: "dot ok" }), h("span", { class: "tool-name", text: "result" }), h("span", { class: "tool-state", text: "done" })),
		),
		h("div", { class: "block-body" }, h("pre", { text: output })),
	);
}

function appendContentItem(container: HTMLElement, item: Json): void {
	if (typeof item === "string") {
		container.append(textBlock(item));
		return;
	}
	if (!isObj(item)) {
		container.append(rawBlock(item));
		return;
	}
	const type = String(item["type"] ?? "");
	if (type === "text") container.append(textBlock(String(item["text"] ?? "")));
	else if (type === "thinking") container.append(thinkingBlock(String(item["thinking"] ?? item["text"] ?? ""), false));
	else if (type === "toolCall") container.append(toolCallBlock(item));
	else if (type === "toolResult") container.append(toolResultBlock(item));
	else if (type === "image") container.append(imageBlock(item));
	else container.append(rawBlock(item));
}

function quoteMessage(message: ChatMessage): void {
	const quote = `> ${messageAuthor(message)}: ${plainText(message).replace(/\n/g, "\n> ")}\n\n`;
	const existing = refs.composerInput.value;
	refs.composerInput.value = existing ? `${existing}\n${quote}` : quote;
	if (selectedConversationId) drafts.set(selectedConversationId, refs.composerInput.value);
	autoGrow();
	updateComposerState();
	refs.composerInput.focus();
}

function renderRecipients(): void {
	const conversation = selectedConversation();
	refs.recipients.replaceChildren();
	if (!conversation || conversation.kind !== "group" || conversation.botIds.length < 2) {
		refs.recipients.hidden = true;
		return;
	}
	refs.recipients.hidden = false;
	const chosen = recipientSet(conversation);
	refs.recipients.append(h("span", { text: "Send to" }));
	for (const botId of conversation.botIds) {
		const id = `recipient-${botId}`;
		const input = h("input", {
			type: "checkbox",
			id,
			checked: chosen.has(botId),
			onchange: (event: Event) => {
				const input = inputFrom(event);
				if (!input) return;
				const checked = input.checked;
				if (checked) chosen.add(botId);
				else chosen.delete(botId);
				recipients.set(conversation.id, chosen);
				renderRecipients();
				updateComposerState();
			},
		});
		refs.recipients.append(
			h("label", { class: `recipient${chosen.has(botId) ? " on" : ""}`, for: id }, input, botName(botId)),
		);
	}
}

function attachmentsFor(conversationId: string): AttachmentEntry[] {
	const list = attachmentsByConversation.get(conversationId);
	if (list) return list;
	const created: AttachmentEntry[] = [];
	attachmentsByConversation.set(conversationId, created);
	return created;
}

function clipboardFiles(event: ClipboardEvent): File[] {
	const data = event.clipboardData;
	if (!data) return [];
	const direct = [...data.files];
	if (direct.length) return direct;
	const files: File[] = [];
	for (const item of data.items) {
		if (item.kind !== "file") continue;
		const file = item.getAsFile();
		if (file) files.push(file);
	}
	return files;
}

function attachmentNote(entry: AttachmentEntry): string {
	if (entry.status === "reading") return `Reading ${entry.progress}%`;
	if (entry.status === "uploading") return "Uploading…";
	if (entry.status === "error") return entry.error ?? "Upload failed";
	return formatBytes(entry.size);
}

function updateAttachmentChip(entry: AttachmentEntry): void {
	const chip = refs.composerMedia.querySelector<HTMLElement>(`[data-att="${entry.key}"]`);
	if (!chip) return;
	chip.className = `att ${entry.status}`;
	const bar = chip.querySelector<HTMLElement>(".att-bar");
	if (bar) bar.style.width = `${entry.progress}%`;
	const note = chip.querySelector<HTMLElement>(".att-note");
	if (note) note.textContent = attachmentNote(entry);
}

function attachmentChip(entry: AttachmentEntry): HTMLElement {
	const glyph = entry.contentType.startsWith("video/")
		? "VID"
		: entry.contentType.startsWith("audio/")
			? "AUD"
			: entry.contentType.startsWith("image/")
				? "IMG"
				: "FILE";
	const bar = h("span", { class: "att-bar" });
	bar.style.width = `${entry.progress}%`;
	return h(
		"div",
		{ class: `att ${entry.status}`, "data-att": entry.key },
		entry.previewUrl
			? h("img", { class: "att-thumb", src: entry.previewUrl, alt: "", loading: "lazy" })
			: h("span", { class: "att-thumb glyph", text: glyph }),
		h(
			"span",
			{ class: "att-main" },
			h("span", { class: "att-name", title: entry.name, text: entry.name }),
			h("span", { class: "att-note", text: attachmentNote(entry) }),
			h("span", { class: "att-track" }, bar),
		),
		h(
			"button",
			{
				class: "att-remove",
				type: "button",
				title: `Remove ${entry.name}`,
				"aria-label": `Remove ${entry.name}`,
				onclick: () => removeAttachment(entry),
			},
			icon("close", 12),
		),
	);
}

function renderComposerMedia(): void {
	const conversation = selectedConversation();
	const list = conversation ? attachmentsFor(conversation.id).filter((entry) => !entry.cancelled) : [];
	refs.composerMedia.replaceChildren(...list.map(attachmentChip));
	refs.composerMedia.hidden = list.length === 0;
}

function removeAttachment(entry: AttachmentEntry): void {
	entry.cancelled = true;
	if (entry.previewUrl) URL.revokeObjectURL(entry.previewUrl);
	const conversation = selectedConversation();
	if (conversation) {
		const list = attachmentsFor(conversation.id);
		const index = list.indexOf(entry);
		if (index >= 0) list.splice(index, 1);
	}
	renderComposerMedia();
	updateComposerState();
}

async function fileToBase64(file: File, onProgress: (ratio: number) => void): Promise<string> {
	const bytes = new Uint8Array(await file.arrayBuffer());
	const chunk = 0x8000;
	let binary = "";
	for (let offset = 0; offset < bytes.length; offset += chunk) {
		binary += String.fromCharCode(...bytes.subarray(offset, offset + chunk));
		onProgress(bytes.length ? Math.min(1, (offset + chunk) / bytes.length) : 1);
		if ((offset / chunk) % 8 === 0) {
			const { promise, resolve } = Promise.withResolvers<void>();
			window.setTimeout(resolve, 0);
			await promise;
		}
	}
	return btoa(binary);
}

async function uploadAttachment(file: File, entry: AttachmentEntry, botId: string | undefined): Promise<void> {
	try {
		const content = await fileToBase64(file, (ratio) => {
			entry.progress = Math.round(ratio * 100);
			updateAttachmentChip(entry);
		});
		if (entry.cancelled) return;
		entry.status = "uploading";
		entry.progress = 100;
		updateAttachmentChip(entry);
		const item = await command<MediaItem>("media.upload", {
			name: entry.name,
			contentType: entry.contentType,
			...(botId ? { botId } : {}),
			content,
		});
		if (entry.cancelled) return;
		entry.media = item;
		entry.status = "done";
		entry.error = undefined;
	} catch (error) {
		if (entry.cancelled) return;
		entry.status = "error";
		entry.error = errText(error);
	}
	renderComposerMedia();
	updateComposerState();
}

async function addAttachments(files: File[]): Promise<void> {
	const conversation = selectedConversation();
	if (!conversation) {
		notify("Select a conversation first.", "warn");
		return;
	}
	const list = attachmentsFor(conversation.id);
	const botId = currentBotId();
	const batch: Array<{ file: File; entry: AttachmentEntry }> = [];
	for (const file of files) {
		if (file.size === 0) {
			notify(`${file.name || "That file"} is empty.`, "warn");
			continue;
		}
		if (file.size > MAX_ATTACHMENT_BYTES) {
			notify(`${file.name || "That file"} is larger than 25 MB.`, "warn");
			continue;
		}
		attachmentSequence += 1;
		const extension = (file.type.split("/")[1] ?? "bin").replace("+xml", "") || "bin";
		const entry: AttachmentEntry = {
			key: `att-${attachmentSequence}`,
			name: file.name || `pasted-${Date.now()}.${extension}`,
			contentType: file.type || "application/octet-stream",
			size: file.size,
			status: "reading",
			progress: 0,
			cancelled: false,
		};
		if (file.type.startsWith("image/")) entry.previewUrl = URL.createObjectURL(file);
		list.push(entry);
		batch.push({ file, entry });
	}
	renderComposerMedia();
	updateComposerState();
	await Promise.all(batch.map(({ file, entry }) => uploadAttachment(file, entry, botId)));
}

function closeStickerPicker(): void {
	if (refs.stickerPopover.hidden) return;
	refs.stickerPopover.hidden = true;
	refs.composerSticker.classList.remove("on");
	stickerConversationId = null;
}

function renderStickerGrid(conversation: Conversation, botId: string, names: string[], emptyText?: string): void {
	const head = h(
		"div",
		{ class: "sticker-head" },
		h("span", { text: `Stickers · ${botName(botId)}` }),
		h(
			"button",
			{ class: "btn ghost icon sm", type: "button", "aria-label": "Close the sticker picker", onclick: closeStickerPicker },
			icon("close", 12),
		),
	);
	const body = names.length
		? h(
				"div",
				{ class: "sticker-grid" },
				...names.map((name) =>
					h(
						"button",
						{ class: "sticker-cell", type: "button", title: name, onclick: () => void sendSticker(conversation.id, botId, name) },
						h("span", { class: "sticker-glyph", text: name.slice(0, 2).toUpperCase() }),
						h("span", { class: "sticker-name", text: name }),
					),
				),
			)
		: h("div", {
				class: "panel-note",
				text: emptyText ?? `No stickers yet. Add PNG, GIF or WebP files under bots/${botId}/.omp/stickers.`,
			});
	refs.stickerPopover.replaceChildren(head, body);
}

async function toggleStickerPicker(): Promise<void> {
	if (!refs.stickerPopover.hidden) {
		closeStickerPicker();
		return;
	}
	const conversation = selectedConversation();
	if (!conversation) {
		notify("Select a conversation first.", "warn");
		return;
	}
	const botId = conversation.adminBotId || conversation.botIds[0] || "";
	if (!botId) {
		notify("This conversation has no bot to read stickers from.", "warn");
		return;
	}
	stickerConversationId = conversation.id;
	refs.stickerPopover.hidden = false;
	refs.composerSticker.classList.add("on");
	refs.stickerPopover.replaceChildren(h("div", { class: "panel-note", text: "Loading stickers…" }));
	try {
		const value = await command<{ names?: unknown }>("sticker.list", { botId });
		if (stickerConversationId !== conversation.id) return;
		const names = Array.isArray(value?.names) ? value.names.filter((name): name is string => typeof name === "string") : [];
		renderStickerGrid(conversation, botId, names);
	} catch (error) {
		if (stickerConversationId !== conversation.id) return;
		renderStickerGrid(conversation, botId, [], `Sticker list failed: ${errText(error)}`);
	}
}

async function sendSticker(conversationId: string, botId: string, name: string): Promise<void> {
	closeStickerPicker();
	try {
		await command("sticker.send", { conversationId, botId, name });
		await refresh().catch(() => undefined);
	} catch (error) {
		notify(errText(error), "error");
	}
}

function updateComposerState(): void {
	const conversation = selectedConversation();
	const hasText = refs.composerInput.value.trim().length > 0;
	const list = conversation ? attachmentsFor(conversation.id).filter((entry) => !entry.cancelled) : [];
	const ready = list.filter((entry) => entry.status === "done");
	const busyUpload = list.some((entry) => entry.status === "reading" || entry.status === "uploading");
	const chosen = conversation ? recipientSet(conversation) : new Set<string>();
	const groupMany = Boolean(conversation && conversation.kind === "group" && conversation.botIds.length > 1);
	const recipientsOk = !groupMany || chosen.size > 0;
	const busySending = Boolean(conversation && sending.has(conversation.id));
	refs.composerSend.disabled = !conversation || (!hasText && ready.length === 0) || busyUpload || !recipientsOk || busySending;
	refs.composer.classList.toggle("sending", busySending);
	refs.composerStop.hidden = !conversation || activeBotIds(conversation.id).length === 0;

	let hint: string;
	if (!conversation) hint = "Select or create a conversation.";
	else if (!recipientsOk) hint = "Choose at least one recipient.";
	else if (busyUpload) hint = `Uploading ${list.length} attachment${list.length === 1 ? "" : "s"}…`;
	else if (groupMany) hint = `${chosen.size} of ${conversation.botIds.length} recipients · ${chord(MOD, "Enter")} to send`;
	else hint = `${conversation.title} · ${chord(MOD, "Enter")} to send`;
	const auto = conversation ? autoStatus.get(conversation.id) : undefined;
	if (auto?.active) hint += ` · auto ${auto.turns}/${auto.max}`;
	else if (conversation?.scene?.group?.auto === true && auto?.reason === "limit") hint += " · auto paused";
	if (connection === "offline") hint += " · host offline";
	refs.composerHint.textContent = hint;
}

async function sendMessage(): Promise<void> {
	const conversation = selectedConversation();
	if (!conversation) {
		notify("Select a conversation first.", "warn");
		return;
	}
	const text = refs.composerInput.value.trim();
	const list = attachmentsFor(conversation.id).filter((entry) => !entry.cancelled);
	const ready = list.filter((entry) => entry.status === "done");
	if (!text && ready.length === 0) return;
	if (list.some((entry) => entry.status === "reading" || entry.status === "uploading")) {
		notify("Wait for the attachments to finish uploading.", "warn");
		return;
	}
	const groupMany = conversation.kind === "group" && conversation.botIds.length > 1;
	const chosen = [...recipientSet(conversation)];
	if (groupMany && chosen.length === 0) {
		notify("Choose at least one recipient.", "warn");
		return;
	}
	sending.add(conversation.id);
	updateComposerState();
	try {
		const payload: Record<string, unknown> = { conversationId: conversation.id, text };
		const mediaIds = ready.flatMap((entry) => (entry.media ? [entry.media.id] : []));
		if (mediaIds.length) payload["mediaIds"] = mediaIds;
		if (groupMany && chosen.length !== conversation.botIds.length) payload["botIds"] = chosen;
		await command("message.send", payload);
		drafts.set(conversation.id, "");
		refs.composerInput.value = "";
		const sent = new Set(ready);
		for (const entry of ready) if (entry.previewUrl) URL.revokeObjectURL(entry.previewUrl);
		attachmentsByConversation.set(
			conversation.id,
			list.filter((entry) => !sent.has(entry)),
		);
		renderComposerMedia();
		autoGrow();
		await refresh().catch(() => undefined);
	} catch (error) {
		refs.composerInput.value = text;
		drafts.set(conversation.id, text);
		notify(humanRuntimeError(errText(error)), "error");
	} finally {
		sending.delete(conversation.id);
		updateComposerState();
	}
}

async function retrySend(conversationId: string): Promise<void> {
	const conversation = conversationOf(conversationId);
	try {
		await command("message.retry", { conversationId, botId: conversation?.adminBotId });
		const last = [...messagesOf(conversationId)].reverse().find(item => item.role === "user");
		if (last) failedDelivery.delete(last.id);
		messageSignature = "";
		await refresh().catch(() => undefined);
	} catch (error) {
		notify(humanRuntimeError(errText(error)), "error");
	}
}

async function stopActive(): Promise<void> {
	const conversation = selectedConversation();
	if (!conversation) return;
	const targets = activeBotIds(conversation.id);
	if (!targets.length) {
		notify("Nothing is running in this conversation.", "info");
		return;
	}
	for (const botId of targets) {
		try {
			await command("runtime.stop", { conversationId: conversation.id, botId });
		} catch (error) {
			notify(errText(error), "error");
		}
	}
	await refresh().catch(() => undefined);
}

function selectConversation(id: string): void {
	if (selectedConversationId === id && activePanel === null) return;
	if (activePanel === "settings") resetSettingsPreview();
	if (selectedConversationId) drafts.set(selectedConversationId, refs.composerInput.value);
	selectedConversationId = id;
	activePanel = null;
	memberFocusId = null;
	renderAll();
	syncHostSelection();
	activateBotPlugins();
	writeRoute();
	refs.composerInput.focus();
}

/* ------------------------------------------------------------------ */
/* rendering: streaming                                                */
/* ------------------------------------------------------------------ */

function streamKey(conversationId: string, botId: string): string {
	return `${conversationId}::${botId}`;
}

function startStream(conversationId: string, botId: string): Stream {
	const key = streamKey(conversationId, botId);
	const stream: Stream = {
		key,
		conversationId,
		botId,
		startedAt: Date.now(),
		blocks: [],
		settled: false,
		statusText: "starting",
		usage: undefined,
	};
	streams.set(key, stream);
	return stream;
}

const STREAM_CREATE_TYPES = new Set([
	"agent_start",
	"turn_start",
	"message_start",
	"message_update",
	"message_end",
	"tool_execution_start",
	"tool_execution_update",
	"tool_stream_update",
	"tool_execution_end",
	"notice",
	"extension_error",
	"rpc_frame_error",
	"manual_compaction_start",
	"auto_retry_start",
]);

function streamText(stream: Stream): string {
	return stream.blocks
		.filter((block) => block.kind === "text" && block.text)
		.map((block) => block.text)
		.join("\n\n");
}

function streamTargetFor(conversationId: string, botId: string, frame: RpcFrame): Stream | null {
	const existing = streams.get(streamKey(conversationId, botId));
	const type = String(frame.type ?? "");
	if (existing && !existing.settled) return existing;
	if (STREAM_CREATE_TYPES.has(type)) return startStream(conversationId, botId);
	if (type === "response" && frame["success"] === false) return existing ?? startStream(conversationId, botId);
	return null;
}

function ingestRpc(conversationId: string, botId: string, frame: RpcFrame): void {
	const activeBefore = hasActiveStream(conversationId);
	pushFrame(conversationId, botId, frame);
	if (host) {
		try {
			host.handleRpc(frame, { conversationId, botId });
		} catch (error) {
			notify(`Widget host rejected a frame: ${errText(error)}`, "error");
		}
	}
	const stream = streamTargetFor(conversationId, botId, frame);
	if (stream) {
		ingestFrame(stream, frame);
		if (selectedConversationId === conversationId) {
			renderStreams();
			updateComposerState();
		}
		if (activeBefore !== hasActiveStream(conversationId)) renderNav();
	}
	if (asideTab === "runtime" && selectedConversationId === conversationId) queueRuntimeFrames();
	updateRuntimeFromFrame(conversationId, botId, frame);
}

function pushFrame(conversationId: string, botId: string, frame: RpcFrame): void {
	const list = framesByConversation.get(conversationId) ?? [];
	list.push({ at: Date.now(), botId, frame });
	framesByConversation.set(conversationId, list);
}

function updateRuntimeFromFrame(conversationId: string, botId: string, frame: RpcFrame): void {
	const type = String(frame.type ?? "");
	if (type === "agent_start") setRuntime(conversationId, botId, "running");
	else if (type === "session_settled") setRuntime(conversationId, botId, "idle");
	else if (type === "agent_end" && (frame["yielded"] === true || (frame["yielded"] === undefined && frame["isTerminal"] !== false))) {
		if (frame["isTerminal"] === false) setRuntime(conversationId, botId, "running");
		else setRuntime(conversationId, botId, "idle");
	}
}

function ingestFrame(stream: Stream, frame: RpcFrame): void {
	const type = String(frame.type ?? "");
	if (type === "message_start" || type === "message_update" || type === "message_end") {
		const message = frame["message"];
		const messageId = typeof frame["messageId"] === "string" ? frame["messageId"] : "stream";
		if (isObj(message) && message["role"] === "assistant") {
			if (Array.isArray(message["content"])) syncFromMessage(stream, messageId, message);
			else if (type === "message_update") applyAssistantEvent(stream, frame, messageId);
			if (isObj(message["usage"])) stream.usage = message["usage"] as Record<string, number>;
		}
		stream.statusText = type === "message_end" ? "message finished" : "writing";
		return;
	}
	if (type === "tool_execution_start") {
		const callId = String(frame["toolCallId"] ?? `tool-${Date.now()}`);
		const block = upsertToolBlock(stream, { id: callId, name: frame["toolName"], arguments: frame["args"] ?? frame["arguments"] });
		block.status = "running";
		stream.statusText = `tool ${String(frame["toolName"] ?? "")}`.trim();
		return;
	}
	if (type === "tool_execution_update" || type === "tool_stream_update") {
		const callId = String(frame["toolCallId"] ?? "");
		const block = stream.blocks.find((item) => item.kind === "tool" && item.id === `tool:${callId}`);
		if (block) block.result = textOf(frame["partialResult"] ?? frame["delta"] ?? frame["chunk"] ?? "");
		return;
	}
	if (type === "tool_execution_end") {
		const callId = String(frame["toolCallId"] ?? "");
		const block = stream.blocks.find((item) => item.kind === "tool" && item.id === `tool:${callId}`);
		if (block) {
			block.result = textOf(frame["result"] ?? frame["output"] ?? "");
			block.status = frame["isError"] === true ? "error" : "ok";
		}
		stream.statusText = "tool finished";
		return;
	}
	if (type === "prompt_result") {
		const status = String(frame["status"] ?? "");
		stream.statusText = status ? `prompt ${status}` : "prompt finished";
		stream.settled = true;
		if (status === "aborted") addNotice(stream, "The turn was aborted.", "warn");
		if (status === "error") {
			const error = frame["error"];
			const message = isObj(error) && typeof error["message"] === "string" ? error["message"] : "The prompt failed.";
			addNotice(stream, message, "error");
		}
		return;
	}
	if (type === "session_settled") {
		stream.settled = true;
		stream.statusText = "idle";
		return;
	}
	if (type === "agent_start") {
		stream.statusText = "running";
		return;
	}
	if (type === "agent_end") {
		const terminal = frame["yielded"] === true || (frame["yielded"] === undefined && frame["isTerminal"] !== false);
		stream.statusText = terminal ? "finished" : "continuing";
		if (terminal) stream.settled = true;
		return;
	}
	if (type === "queue_update") {
		const steering = Array.isArray(frame["steering"]) ? frame["steering"].length : 0;
		const followUp = Array.isArray(frame["followUp"]) ? frame["followUp"].length : 0;
		stream.statusText = steering + followUp ? `queued ${steering + followUp}` : "running";
		return;
	}
	if (type === "notice") {
		const text = textOf(frame["message"] ?? frame["notice"] ?? frame["text"]);
		if (text) addNotice(stream, text, frame["level"] === "error" ? "error" : frame["level"] === "warn" ? "warn" : undefined);
		return;
	}
	if (type === "extension_error") {
		addNotice(stream, `Extension error (${String(frame["extensionPath"] ?? "unknown")}): ${String(frame["error"] ?? "")}`, "error");
		return;
	}
	if (type === "rpc_frame_error") {
		addNotice(stream, `Frame error: ${String(frame["error"] ?? frame["message"] ?? "unknown")}`, "error");
		return;
	}
	if (type === "response") {
		const commandName = String(frame["command"] ?? "command");
		if (frame["success"] === false) {
			addNotice(stream, `${commandName} failed: ${String(frame["error"] ?? "unknown error")}`, "error");
		} else if (frame["data"] !== undefined) {
			addNotice(stream, `${commandName} → ${truncate(normalizeSpace(compactJson(frame["data"])), 160)}`);
		}
		return;
	}
	if (type === "auto_compaction_start" || type === "manual_compaction_start") {
		addNotice(stream, "Compacting context…");
		stream.statusText = "compacting";
		return;
	}
	if (type === "auto_compaction_end" || type === "manual_compaction_end") {
		addNotice(stream, frame["errorMessage"] ? `Compaction failed: ${String(frame["errorMessage"])}` : "Compaction finished.", frame["errorMessage"] ? "error" : undefined);
		return;
	}
	if (type === "auto_retry_start") {
		addNotice(stream, `Retrying: ${String(frame["errorMessage"] ?? frame["reason"] ?? "")}`, "warn");
		return;
	}
	if (type === "auto_retry_end") {
		addNotice(stream, frame["success"] === false ? "Retries exhausted." : "Retry succeeded.", frame["success"] === false ? "error" : undefined);
		return;
	}
	if (type === "goal_updated") {
		addNotice(stream, "Goal updated.");
		return;
	}
	if (type === "ttsr_triggered") {
		const rules = Array.isArray(frame["rules"]) ? frame["rules"].length : 0;
		addNotice(stream, `Rule injection (${rules} rule${rules === 1 ? "" : "s"}).`);
		return;
	}
	if (type === "irc_message") {
		addNotice(stream, textOf(frame["message"] ?? frame["text"] ?? frame));
	}
}

function addNotice(stream: Stream, text: string, status?: "warn" | "error"): void {
	noticeSequence += 1;
	stream.blocks.push({ id: `notice:${noticeSequence}`, kind: "notice", text, status });
}

function syncFromMessage(stream: Stream, messageId: string, message: Record<string, unknown>): void {
	const content = Array.isArray(message["content"]) ? message["content"] : [];
	const previous = new Map(stream.blocks.map((block) => [block.id, block]));
	const next: StreamBlock[] = [];
	for (let index = 0; index < content.length; index += 1) {
		const raw = content[index];
		if (!isObj(raw)) continue;
		const kind = String(raw["type"] ?? "");
		if (kind === "text") {
			next.push({ id: `${messageId}:${index}`, kind: "text", text: String(raw["text"] ?? "") });
		} else if (kind === "thinking") {
			next.push({ id: `${messageId}:${index}`, kind: "thinking", text: String(raw["thinking"] ?? raw["text"] ?? "") });
		} else if (kind === "toolCall") {
			const id = `tool:${String(raw["id"] ?? `${messageId}:${index}`)}`;
			const previousBlock = previous.get(id);
			next.push({
				id,
				kind: "tool",
				name: String(raw["name"] ?? "tool"),
				args: safeJson(raw["arguments"] ?? {}),
				status: previousBlock?.status ?? "running",
				result: previousBlock?.result,
			});
		} else {
			next.push({ id: `${messageId}:${index}`, kind: "raw", text: safeJson(raw) });
		}
	}
	for (const block of stream.blocks) {
		if (block.kind === "tool" && !next.some((item) => item.id === block.id)) next.push(block);
	}
	for (const block of stream.blocks) {
		if (block.kind === "notice") next.push(block);
	}
	stream.blocks = next;
}

function applyAssistantEvent(stream: Stream, frame: RpcFrame, messageId: string): void {
	const event = frame["assistantMessageEvent"];
	if (!isObj(event)) return;
	const type = String(event["type"] ?? "");
	const index = typeof event["contentIndex"] === "number" ? event["contentIndex"] : 0;
	const blockId = `${messageId}:${index}`;
	if (type === "text_delta") {
		appendDelta(stream, blockId, "text", String(event["delta"] ?? ""));
	} else if (type === "thinking_delta") {
		appendDelta(stream, blockId, "thinking", String(event["delta"] ?? ""));
	} else if (type === "text_end") {
		const block = ensureBlock(stream, blockId, "text");
		block.text = String(event["content"] ?? "");
	} else if (type === "thinking_end") {
		const block = ensureBlock(stream, blockId, "thinking");
		block.text = String(event["content"] ?? "");
	} else if (type === "toolcall_start" || type === "toolcall_end") {
		const call = isObj(event["toolCall"]) ? event["toolCall"] : isObj(event["block"]) ? event["block"] : null;
		if (call) upsertToolBlock(stream, call);
	} else if (type === "toolcall_delta") {
		const id = typeof event["toolCallId"] === "string" ? event["toolCallId"] : null;
		const toolId = `tool:${id ?? `${messageId}:tool:${index}`}`;
		const block = stream.blocks.find((item) => item.kind === "tool" && item.id === toolId);
		if (block) block.args = `${block.args ?? ""}${String(event["delta"] ?? "")}`;
	}
}

function ensureBlock(stream: Stream, id: string, kind: "text" | "thinking"): StreamBlock {
	let block = stream.blocks.find((item) => item.id === id && item.kind === kind);
	if (!block) {
		block = { id, kind, text: "" };
		stream.blocks.push(block);
	}
	return block;
}

function appendDelta(stream: Stream, id: string, kind: "text" | "thinking", delta: string): void {
	if (!delta) return;
	const block = ensureBlock(stream, id, kind);
	block.text = `${block.text ?? ""}${delta}`;
}

function upsertToolBlock(stream: Stream, call: Record<string, unknown>): StreamBlock {
	const callId = String(call["id"] ?? `anonymous-${Date.now()}`);
	const id = `tool:${callId}`;
	let block = stream.blocks.find((item) => item.kind === "tool" && item.id === id);
	if (!block) {
		block = { id, kind: "tool", name: "tool", args: "", status: "running" };
		stream.blocks.push(block);
	}
	if (typeof call["name"] === "string") block.name = call["name"];
	if (call["arguments"] !== undefined) block.args = safeJson(call["arguments"]);
	return block;
}

function reconcileStreams(): void {
	for (const [key, stream] of [...streams]) {
		const persisted = state.messages.filter(
			(message) =>
				message.conversationId === stream.conversationId &&
				message.role === "assistant" &&
				message.botId === stream.botId &&
				message.createdAt >= stream.startedAt - 1000,
		);
		if (!persisted.length) continue;
		const streamed = normalizeSpace(streamText(stream));
		const matched = streamed.length > 0 && persisted.some((message) => normalizeSpace(message.text) === streamed);
		if (stream.settled || matched) streams.delete(key);
	}
}

function renderStreams(): void {
	const conversationId = selectedConversationId;
	const list = conversationId
		? [...streams.values()].filter((stream) => stream.conversationId === conversationId).sort((a, b) => a.startedAt - b.startedAt)
		: [];
	const present = new Set(list.map((stream) => stream.key));
	for (const [key, entry] of [...streamEls]) {
		if (!present.has(key)) {
			entry.root.remove();
			streamEls.delete(key);
		}
	}
	for (const stream of list) upsertStreamElement(stream);
	if (stickToBottom) scrollToBottom();
}

function upsertStreamElement(stream: Stream): void {
	let entry = streamEls.get(stream.key);
	if (!entry) {
		const status = h("span", { class: "stream-status" });
		const body = h("div", { class: "stream-body" });
		const root = h(
			"section",
			{ class: "stream", "data-key": stream.key, "aria-label": `${botName(stream.botId)} responding` },
			h(
				"div",
				{ class: "stream-head" },
				avatarNode(botName(stream.botId), botAvatarOf(botOf(stream.botId))),
				h("span", { class: "stream-name", text: botName(stream.botId) }),
				status,
				h(
					"div",
					{ class: "stream-actions" },
					h(
						"button",
						{ class: "btn ghost sm", type: "button", title: "Copy the streamed text", onclick: () => void copyText(streamText(stream), "Response copied") },
						icon("copy", 14),
						"Copy",
					),
				),
			),
			body,
		);
		refs.streamList.append(root);
		entry = { root, body, status };
		streamEls.set(stream.key, entry);
	}
	entry.status.textContent = stream.settled ? "final response" : stream.statusText;
	entry.status.className = `stream-status${stream.settled ? "" : " live"}`;
	entry.root.setAttribute("aria-label", `${botName(stream.botId)} ${stream.settled ? "response" : "responding"}`);
	syncBlocksDom(entry.body, stream.blocks);
}

function createStreamBlock(block: StreamBlock): HTMLElement {
	if (block.kind === "text") return h("div", { class: "block text", "data-bid": block.id, "data-kind": "text" });
	if (block.kind === "notice") return h("div", { class: "block notice", "data-bid": block.id, "data-kind": "notice" });
	return h(
		"details",
		{ class: `block ${block.kind}`, "data-bid": block.id, "data-kind": block.kind },
		h("summary"),
		h("div", { class: "block-body" }),
	);
}

function syncBlocksDom(container: HTMLElement, blocks: StreamBlock[]): void {
	const seen = new Set<string>();
	for (const block of blocks) {
		seen.add(block.id);
		let element = container.querySelector<HTMLElement>(
			`:scope > [data-bid="${block.id.replace(/\\/g, "\\\\").replace(/"/g, '\\"')}"]`,
		);
		if (element && element.dataset["kind"] !== block.kind) {
			element.remove();
			element = null;
		}
		if (!element) {
			element = createStreamBlock(block);
			container.append(element);
		}
		updateStreamBlock(element, block);
	}
	for (const child of Array.from(container.children)) {
		const id = child instanceof HTMLElement ? child.dataset["bid"] : undefined;
		if (!id || !seen.has(id)) child.remove();
	}
}

function updateStreamBlock(element: HTMLElement, block: StreamBlock): void {
	if (block.kind === "text") {
		if (element.textContent !== (block.text ?? "")) element.textContent = block.text ?? "";
		return;
	}
	if (block.kind === "notice") {
		element.className = `block notice${block.status ? ` ${block.status}` : ""}`;
		if (element.textContent !== (block.text ?? "")) element.textContent = block.text ?? "";
		return;
	}
	const summary = element.querySelector("summary");
	const body = element.querySelector(".block-body");
	if (!summary || !body) return;
	if (block.kind === "thinking") {
		summary.replaceChildren(
			h(
				"span",
				{ class: "block-head" },
				h("span", { class: "tool-name", text: "thinking" }),
				h("span", { class: "tool-args", text: `${(block.text ?? "").length} chars` }),
			),
		);
		if (body.textContent !== (block.text ?? "")) body.textContent = block.text ?? "";
		return;
	}
	if (block.kind === "tool") {
		const state = block.status ?? "running";
		element.dataset["status"] = state;
		summary.replaceChildren(
			h(
				"span",
				{ class: "block-head" },
				h("span", { class: `dot ${state === "error" ? "err" : state === "ok" ? "ok" : "run"}` }),
				h("span", { class: "tool-name", text: block.name ?? "tool" }),
				h("span", { class: "tool-args", text: truncate(normalizeSpace(block.args ?? ""), 120) }),
				h("span", { class: "tool-state", text: state }),
			),
		);
		body.replaceChildren();
		if (block.args) body.append(h("pre", { text: block.args }));
		if (block.result) body.append(h("pre", { text: block.result }));
		if (!block.args && !block.result) body.append(h("span", { class: "dim", text: "No output yet." }));
		return;
	}
	summary.replaceChildren(h("span", { class: "block-head" }, h("span", { class: "tool-name", text: "block" })));
	body.replaceChildren(h("pre", { text: block.text ?? "" }));
}

/* ------------------------------------------------------------------ */
/* rendering: aside                                                    */
/* ------------------------------------------------------------------ */

function renderAside(): void {
	renderAsideTabs();
	if (asideTab === "runtime") {
		const key = `${selectedConversationId ?? "none"}:runtime`;
		if (asideBuiltKey !== key) buildRuntimeTab(key);
		else updateRuntimeTab();
		return;
	}
	asideBuiltKey = `${selectedConversationId ?? "none"}:${asideTab}`;
	if (asideTab === "members") buildMembersTab();
	else buildUsageTab();
}

function renderAsideTabs(): void {
	const tabs: Array<{ id: "members" | "runtime" | "usage"; label: string }> = [
		{ id: "members", label: "Members" },
		{ id: "runtime", label: "Runtime" },
		{ id: "usage", label: "Usage" },
	];
	refs.asideTabs.replaceChildren(
		...tabs.map((tab) =>
			h(
				"button",
				{
					class: `tab${asideTab === tab.id ? " on" : ""}`,
					type: "button",
					role: "tab",
					"aria-selected": String(asideTab === tab.id),
					onclick: () => {
						asideTab = tab.id;
						asideBuiltKey = "";
						renderAside();
					},
				},
				tab.label,
			),
		),
	);
}

function emptyAside(text: string): HTMLElement {
	return h("div", { class: "empty-nav", text });
}

function buildMembersTab(): void {
	const conversation = selectedConversation();
	refs.asideBody.replaceChildren();
	if (!conversation) {
		refs.asideBody.append(emptyAside("No conversation selected."));
		return;
	}
	if (memberFocusId && !conversation.botIds.includes(memberFocusId)) memberFocusId = null;
	const focus = memberFocusId ?? conversation.adminBotId ?? conversation.botIds[0] ?? "";
	const list = h("div", { class: "stack" });
	for (const botId of conversation.botIds) {
		const bot = botOf(botId);
		const avatar = botAvatarOf(bot);
		const info = runtimeStatus.get(runtimeKey(conversation.id, botId));
		const isAdmin = conversation.kind === "group" && conversation.adminBotId === botId;
		const isTeamAdmin = state.teamAdminId === botId;
		const remove =
			conversation.kind === "group" && conversation.botIds.length > 1
				? h(
						"button",
						{
							class: "btn ghost icon sm",
							type: "button",
							"aria-label": `Remove ${botName(botId)} from this conversation`,
							title: "Remove member",
							onclick: () => confirmRemoveMember(conversation, botId),
						},
						icon("close", 12),
					)
				: null;
		const row = h(
			"div",
			{ class: "member-row" },
			h(
				"button",
				{
					class: `member${botId === focus ? " on" : ""}`,
					type: "button",
					onclick: () => {
						memberFocusId = botId;
						renderAside();
					},
				},
				avatar ? avatarNode(bot?.name ?? botId, avatar) : null,
				h("span", { class: `dot ${statusClass(info?.status)}`, title: info?.status ?? "no runtime status" }),
				h(
					"div",
					{ class: "member-info" },
					h("div", { class: "member-name", text: bot?.name ?? botId }),
					h(
						"div",
						{ class: "member-sub", text: bot?.model ? `${bot.model.provider}/${bot.model.id}` : bot?.runtime || "no model" },
					),
				),
				isTeamAdmin ? h("span", { class: "admin-tag team", text: "team" }) : isAdmin ? h("span", { class: "admin-tag", text: "admin" }) : null,
			),
			remove,
		);
		list.append(row);
	}
	refs.asideBody.append(list);
	const focused = botOf(focus);
	if (focused) refs.asideBody.append(renderBotDetail(conversation, focused));
	if (conversation.kind === "group") refs.asideBody.append(renderGroupControls(conversation));
	else refs.asideBody.append(h("div", { class: "panel-note", text: "A direct conversation has one bot. Switch it to a group in conversation settings to add members." }));
}

function definitionList(pairs: Array<[string, string]>): HTMLElement {
	const list = h("dl", { class: "kv" });
	for (const [key, value] of pairs) {
		const dd = h("dd");
		if (value.length > 28 || value.includes("urn:onlyne:") || value.includes("/") || value.startsWith("http")) {
			dd.append(identifierNode(value));
		} else dd.textContent = value;
		list.append(h("dt", { text: key }), dd);
	}
	return list;
}

function identifierNode(value: string): HTMLElement {
	const button = h("button", { class: "id-copy", type: "button", "aria-label": `Copy ${value}`, onclick: () => void copyText(value, "Copied.") }, "Copy");
	return h("span", { class: "identifier", title: value }, h("span", { class: "identifier-text", text: middleEllipsis(value) }), button);
}

function renderBotDetail(conversation: Conversation, bot: Bot): HTMLElement {
	const messages = messagesOf(conversation.id).filter((message) => message.botId === bot.id);
	const usage = aggregateUsage(messages);
	const configDir = botConfigDir(bot);
	const onlyne = botOnlyne(bot);
	const plugins = botPlugins(bot);
	const pairs: Array<[string, string]> = [
		["Model", bot.model ? `${bot.model.provider}/${bot.model.id}` : "not set"],
		["Thinking", bot.thinking],
		["Runtime", bot.runtime || "not set"],
		["Workspace", bot.workspace || "not set"],
		["Config root", configDir || "not reported"],
		["Onlyne node", onlyne.nodeId || "host default"],
		["Onlyne peers", onlyne.peers.length ? onlyne.peers.join(", ") : "none"],
		["Host plugins", plugins.host.length ? plugins.host.join(", ") : "none"],
		["Renderer plugins", plugins.renderer.length ? plugins.renderer.join(", ") : "none"],
		["Capabilities", bot.capabilities.length ? bot.capabilities.map(capabilityName).join(", ") : "none"],
		["In conversation", String(messagesOf(conversation.id).length)],
		["From this bot", String(messages.length)],
	];
	if (usage.length) pairs.push(["Usage", usage.map((entry) => `${entry.key} ${compactNumber(entry.total)}`).join(" · ")]);
	const teamAdmin = state.teamAdminId === bot.id;
	return h(
		"div",
		{ class: "card" },
		h("div", { class: "card-head" }, avatarNode(bot.name, botAvatarOf(bot)), h("span", { text: bot.name })),
		h(
			"div",
			{ class: "card-body" },
			definitionList(pairs),
			h(
				"div",
				{ class: "panel-actions" },
				h("button", { class: "btn sm", type: "button", onclick: () => openBotEditor(bot.id) }, "Edit bot"),
				teamAdmin
					? h("span", { class: "chip dim", title: "The team administrator manages the roster and groups", text: "team administrator" })
					: h("button", { class: "btn sm", type: "button", onclick: () => confirmTeamAdmin(bot) }, "Make team admin"),
				h(
					"button",
					{
						class: "btn danger sm",
						type: "button",
						disabled: teamAdmin,
						title: teamAdmin ? "Transfer team administration before deleting this bot" : "Delete this bot",
						onclick: () => confirmDeleteBot(bot),
					},
					"Delete",
				),
			),
		),
	);
}

function renderGroupControls(conversation: Conversation): HTMLElement {
	const admin = selectEl(
		conversation.botIds.map((id) => ({ value: id, label: botName(id) })),
		conversation.adminBotId || conversation.botIds[0] || "",
	);
	admin.id = `admin-${conversation.id}`;
	admin.onchange = () => {
		const next = admin.value;
		if (!next || next === conversation.adminBotId) return;
		void updateConversation(conversation.id, { adminBotId: next });
	};
	const others = state.bots.filter((bot) => !conversation.botIds.includes(bot.id));
	const add = selectEl([{ value: "", label: "Add member…" }, ...others.map((bot) => ({ value: bot.id, label: bot.name }))], "");
	add.id = `add-${conversation.id}`;
	add.onchange = () => {
		const id = add.value;
		if (!id) return;
		void updateConversation(conversation.id, { botIds: [...conversation.botIds, id] });
		add.value = "";
	};
	return h(
		"div",
		{ class: "card" },
		h("div", { class: "card-head", text: "Group" }),
		h(
			"div",
			{ class: "card-body" },
			h("div", { class: "field" }, h("label", { for: admin.id, text: "Admin bot" }), admin),
			h(
				"div",
				{ class: "field" },
				h("label", { for: add.id, text: "Members" }),
				add,
				h("div", { class: "hint", text: "The admin bot receives the group turn and coordinates the other members." }),
			),
		),
	);
}

function buildUsageTab(): void {
	const conversation = selectedConversation();
	const scoped = conversation ? messagesOf(conversation.id) : state.messages;
	const totals = aggregateUsage(scoped);
	refs.asideBody.replaceChildren();
	refs.asideBody.append(
		h(
			"div",
			{ class: "card" },
			h("div", { class: "card-head", text: conversation ? `Usage · ${conversation.title}` : "Usage · all conversations" }),
			h(
				"div",
				{ class: "card-body" },
				totals.length
					? h(
							"table",
							{ class: "usage-table" },
							h("thead", {}, h("tr", {}, h("th", { text: "Key" }), h("th", { class: "num", text: "Total" }))),
							h(
								"tbody",
								{},
								...totals.map((entry) =>
									h("tr", {}, h("td", { text: entry.key }), h("td", { class: "num", text: compactNumber(entry.total) })),
								),
							),
						)
					: h("div", { class: "panel-note", text: "No usage recorded in persisted messages." }),
			),
		),
	);
	if (conversation) {
		const rows = conversation.botIds.map((botId) => {
			const usage = aggregateUsage(messagesOf(conversation.id).filter((message) => message.botId === botId));
			return h(
				"tr",
				{},
				h("td", { text: botName(botId) }),
				h("td", { class: "num", text: String(usage.length ? usage.map((entry) => `${entry.key} ${compactNumber(entry.total)}`).join(" · ") : "—") }),
			);
		});
		refs.asideBody.append(
			h(
				"div",
				{ class: "card" },
				h("div", { class: "card-head", text: "By bot" }),
				h(
					"div",
					{ class: "card-body" },
					h("table", { class: "usage-table" }, h("thead", {}, h("tr", {}, h("th", { text: "Bot" }), h("th", { class: "num", text: "Usage" }))), h("tbody", {}, ...rows)),
				),
			),
		);
	}
	const counts = { user: 0, assistant: 0, tool: 0, system: 0 };
	for (const message of scoped) counts[message.role] += 1;
	refs.asideBody.append(
		h(
			"div",
			{ class: "card" },
			h("div", { class: "card-head", text: "Messages" }),
			h(
				"div",
				{ class: "card-body" },
				definitionList([
					["User", String(counts.user)],
					["Assistant", String(counts.assistant)],
					["Tool", String(counts.tool)],
					["System", String(counts.system)],
				]),
			),
		),
	);
}

function buildRuntimeTab(key: string): void {
	asideBuiltKey = key;
	refs.asideBody.replaceChildren();
	const conversation = selectedConversation();
	if (!conversation) {
		runtimeStatusList = null;
		runtimeFramesBox = null;
		refs.asideBody.append(emptyAside("No conversation selected."));
		return;
	}
	const target =
		runtimeTabBotId && conversation.botIds.includes(runtimeTabBotId)
			? runtimeTabBotId
			: conversation.adminBotId || conversation.botIds[0] || "";
	runtimeTabBotId = target;
	const targetSelect = selectEl(
		conversation.botIds.map((id) => ({ value: id, label: botName(id) })),
		target,
	);
	targetSelect.id = "runtime-target";
	targetSelect.onchange = () => {
		runtimeTabBotId = targetSelect.value;
		updateRuntimeTab();
	};
	runtimeStatusList = h("div", { class: "stack" });

	const bot = botOf(target);
	const thinking = selectEl(
		THINKING_LEVELS.map((level) => ({ value: level, label: level })),
		bot?.thinking ?? "inherit",
	);
	thinking.id = "runtime-thinking";
	runtimeThinkingSelect = thinking;
	thinking.onchange = () => {
		if (!runtimeTabBotId) return;
		void applyThinking(conversation.id, runtimeTabBotId, thinking.value as ThinkingLevel);
	};

	const modelChip = h("span", { class: "chip dim", text: bot?.model ? `${bot.model.provider}/${bot.model.id}` : "model not set" });
	runtimeModelChip = modelChip;
	const modelRow = h(
		"div",
		{ class: "row" },
		h("button", { class: "btn sm", type: "button", onclick: () => openModelsModal(runtimeTabBotId ?? undefined) }, "Choose model…"),
		modelChip,
	);

	const frameInput = textArea('{"type":"get_state"}', { rows: "4" });
	frameInput.id = "runtime-frame";
	const frameResult = h("pre", { class: "palette-result" });
	const frameSend = h(
		"button",
		{ class: "btn primary sm", type: "button", onclick: () => void sendFrameCommand(conversation.id, runtimeTabBotId ?? "", frameInput, frameResult) },
		"Send frame",
	);

	runtimeFramesBox = h("div", { class: "stack" });

	refs.asideBody.append(
		h(
			"div",
			{ class: "card" },
			h("div", { class: "card-head", text: "Runtime" }),
			h(
				"div",
				{ class: "card-body" },
				h("div", { class: "field" }, h("label", { for: "runtime-target", text: "Bot" }), targetSelect),
				runtimeStatusList,
				h("div", { class: "field" }, h("label", { for: "runtime-thinking", text: "Thinking level" }), thinking),
				modelRow,
			),
		),
		h(
			"div",
			{ class: "card" },
			h("div", { class: "card-head", text: "Raw RPC frame" }),
			h(
				"div",
				{ class: "card-body" },
				h("div", { class: "field" }, h("label", { for: "runtime-frame", text: "Frame JSON" }), frameInput),
				h("div", { class: "panel-actions" }, frameSend),
				frameResult,
			),
		),
		h(
			"div",
			{ class: "card" },
			h("div", { class: "card-head", text: "Recent frames" }),
			h("div", { class: "card-body" }, runtimeFramesBox),
		),
	);
	updateRuntimeTab();
	updateRuntimeFrames();
}

function updateRuntimeTab(): void {
	if (!runtimeStatusList) return;
	const conversation = selectedConversation();
	if (!conversation) return;
	runtimeStatusList.replaceChildren(
		...conversation.botIds.map((botId) => {
			const info = runtimeStatus.get(runtimeKey(conversation.id, botId));
			const active = (info?.status ?? "").toLowerCase();
			const running = active.includes("run") || active.includes("stream") || active.includes("connect");
			return h(
				"div",
				{ class: "spread" },
				h(
					"span",
					{ class: "row" },
					h("span", { class: `dot ${statusClass(info?.status)}` }),
					h("span", { text: botName(botId) }),
					h("span", { class: "chip dim", text: info?.status ?? "unknown" }),
				),
				running
					? h(
							"button",
							{ class: "btn danger sm", type: "button", onclick: () => void stopBot(conversation.id, botId) },
							"Stop",
						)
					: h(
							"button",
							{ class: "btn sm", type: "button", title: "Stop this bot's runtime; the next message starts a fresh one", onclick: () => void restartBot(conversation.id, botId) },
							"Restart",
						),
			);
		}),
	);
	const bot = botOf(runtimeTabBotId ?? "");
	if (runtimeThinkingSelect) runtimeThinkingSelect.value = bot?.thinking ?? "inherit";
	if (runtimeModelChip) runtimeModelChip.textContent = bot?.model ? `${bot.model.provider}/${bot.model.id}` : "model not set";
}

function queueRuntimeFrames(): void {
	if (framesRefreshQueued) return;
	framesRefreshQueued = true;
	window.requestAnimationFrame(() => {
		framesRefreshQueued = false;
		if (asideTab === "runtime" && selectedConversationId) updateRuntimeFrames();
	});
}

function updateRuntimeFrames(): void {
	if (runtimeFramesBox) {
		const entries = framesFor(selectedConversationId ?? "").slice(-10).reverse();
		runtimeFramesBox.replaceChildren(
			...entries.map((entry) =>
				h(
					"details",
					{ class: "block tool" },
					h(
						"summary",
						{},
						h(
							"span",
							{ class: "block-head" },
							h("span", { class: "tool-name", text: frameLabel(entry.frame) }),
							h("span", { class: "tool-args", text: `${clockTime(entry.at)} · ${botName(entry.botId)}` }),
						),
					),
					h("div", { class: "block-body" }, h("pre", { text: safeJson(entry.frame) })),
				),
			),
		);
		if (!entries.length) runtimeFramesBox.append(h("div", { class: "panel-note", text: "No frames yet in this conversation." }));
	}
}

async function stopBot(conversationId: string, botId: string): Promise<void> {
	try {
		await command("runtime.stop", { conversationId, botId });
		await refresh().catch(() => undefined);
	} catch (error) {
		notify(errText(error), "error");
	}
}

async function restartBot(conversationId: string, botId: string): Promise<void> {
	try {
		await command("runtime.restart", { conversationId, botId });
		notify(`${botName(botId)} will start a fresh runtime on the next message.`, "ok");
		await refresh().catch(() => undefined);
	} catch (error) {
		notify(errText(error), "error");
	}
}

async function applyThinking(conversationId: string, botId: string, level: ThinkingLevel): Promise<void> {
	try {
		await command("runtime.command", {
			conversationId,
			botId,
			command: { id: nextRpcId(), type: "set_thinking_level", level },
		});
		const bot = botOf(botId);
		if (bot) await command("bot.save", { bot: { id: bot.id, thinking: level } });
		await refresh().catch(() => undefined);
	} catch (error) {
		notify(errText(error), "error");
	}
}

async function sendFrameCommand(
	conversationId: string,
	botId: string,
	input: HTMLTextAreaElement,
	result: HTMLElement,
): Promise<void> {
	result.textContent = "";
	try {
		if (!botId) throw new Error("Choose a bot first.");
		const frame = requireObject(parseJson(input.value, "Frame"), "Frame") as unknown as RpcFrame;
		if (typeof frame.type !== "string" || !frame.type) throw new Error("A frame needs a type.");
		if (typeof frame.id !== "string") frame.id = nextRpcId();
		const value = await command("runtime.command", { conversationId, botId, command: frame });
		result.textContent = value === undefined ? "done" : safeJson(value);
	} catch (error) {
		result.textContent = errText(error);
		notify(errText(error), "error");
	}
}

function frameLabel(frame: RpcFrame): string {
	const type = String(frame.type ?? "frame");
	if (type === "response" && typeof frame["command"] === "string") return `response · ${frame["command"]}`;
	return type;
}

function framesFor(conversationId: string): FrameEntry[] {
	return framesByConversation.get(conversationId) ?? [];
}

/* ------------------------------------------------------------------ */
/* rendering: models                                                   */
/* ------------------------------------------------------------------ */

function normalizeModels(value: unknown): ModelChoice[] {
	const data = isObj(value) && value["data"] !== undefined ? value["data"] : value;
	const raw = Array.isArray(data)
		? data
		: isObj(data) && Array.isArray(data["models"])
			? data["models"]
			: isObj(data) && Array.isArray(data["available"])
				? data["available"]
				: [];
	const models: ModelChoice[] = [];
	for (const item of raw) {
		if (typeof item === "string") {
			const [provider = "", ...rest] = item.split("/");
			models.push(rest.length ? { provider, id: rest.join("/") } : { provider: "", id: item });
			continue;
		}
		if (!isObj(item)) continue;
		const provider = typeof item["provider"] === "string" ? item["provider"] : typeof item["providerId"] === "string" ? item["providerId"] : "";
		const id =
			typeof item["id"] === "string"
				? item["id"]
				: typeof item["modelId"] === "string"
					? item["modelId"]
					: typeof item["name"] === "string"
						? item["name"]
						: "";
		if (id) models.push({ provider, id });
	}
	return models;
}

async function runtimeModelsForBot(botId: string, conversationId?: string): Promise<ModelChoice[]> {
	const conversation = conversationId ? conversationOf(conversationId) : state.conversations.find((item) => item.botIds.includes(botId));
	if (!conversation) throw new Error("This bot has no conversation yet; start one to read its runtime model catalog.");
	const value = await command("runtime.models", { conversationId: conversation.id, botId });
	return normalizeModels(value);
}

function openModelsModal(botId?: string): void {
	const conversation = selectedConversation();
	if (!conversation) {
		notify("Select a conversation first.", "warn");
		return;
	}
	let target = botId && conversation.botIds.includes(botId) ? botId : conversation.adminBotId || conversation.botIds[0] || "";
	if (!target) {
		notify("This conversation has no bots.", "warn");
		return;
	}
	const status = h("div", { class: "panel-note", text: "Loading models…" });
	const list = h("div", { class: "panel-grid" });
	let modal: ModalHandle;
	const selector =
		conversation.botIds.length > 1
			? selectEl(conversation.botIds.map((id) => ({ value: id, label: botName(id) })), target)
			: null;
	if (selector) {
		selector.onchange = () => {
			target = selector.value;
			void load();
		};
	}
	const load = async (): Promise<void> => {
		status.textContent = "Loading models…";
		list.replaceChildren();
		try {
			const models = await runtimeModelsForBot(target, conversation.id);
			if (!models.length) {
				status.textContent = "The runtime returned no models.";
				return;
			}
			status.textContent = `${models.length} models available for ${botName(target)}.`;
			for (const model of models) {
				list.append(
					h(
						"button",
						{
							class: "btn",
							type: "button",
							title: `${model.provider}/${model.id}`,
							onclick: () => void chooseModel(conversation.id, target, model, modal),
						},
						`${model.provider}/${model.id}`,
					),
				);
			}
		} catch (error) {
			status.textContent = errText(error);
		}
	};
	modal = openModal({
		title: "Runtime models",
		wide: true,
		body: h("div", { class: "stack" }, selector, status, list),
	});
	modal.footer.append(
		h("button", { class: "btn", type: "button", onclick: () => void load() }, icon("refresh"), "Reload"),
		h("button", { class: "btn ghost", type: "button", onclick: modal.close }, "Close"),
	);
	void load();
}

async function chooseModel(conversationId: string, botId: string, model: ModelChoice, modal: ModalHandle): Promise<void> {
	try {
		await command("runtime.command", {
			conversationId,
			botId,
			command: { id: nextRpcId(), type: "set_model", provider: model.provider, modelId: model.id },
		});
		const bot = botOf(botId);
		if (bot) await command("bot.save", { bot: { id: bot.id, model } });
		modal.close();
		notify(`Model set to ${model.provider}/${model.id}`, "ok");
		await refresh().catch(() => undefined);
	} catch (error) {
		notify(errText(error), "error");
	}
}

/* ------------------------------------------------------------------ */
/* modals                                                              */
/* ------------------------------------------------------------------ */

interface ModalHandle {
	root: HTMLElement;
	body: HTMLElement;
	footer: HTMLElement;
	close: () => void;
}

let activeModalClose: (() => void) | null = null;
let modalReturnFocus: HTMLElement | null = null;

function focusFirst(container: HTMLElement): void {
	const preferred = container.querySelector<HTMLElement>("[data-autofocus]");
	if (preferred) {
		preferred.focus();
		return;
	}
	const first = container.querySelector<HTMLElement>(
		"input:not([disabled]),textarea:not([disabled]),select:not([disabled]),button:not([disabled]),[tabindex]:not([tabindex='-1'])",
	);
	first?.focus();
}

function closeModal(): void {
	if (!activeModalClose) return;
	const close = activeModalClose;
	activeModalClose = null;
	close();
}

function openModal(options: { title: string; body: HTMLElement; wide?: boolean; narrow?: boolean; anchor?: boolean; onClose?: () => void }): ModalHandle {
	closeModal();
	modalReturnFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
	const titleId = `modal-title-${++fieldSequence}`;
	const body = h("div", { class: "modal-body" }, options.body);
	const footer = h("div", { class: "modal-foot" });
	const root = h(
		"div",
		{ class: `modal${options.wide ? " wide" : options.narrow ? " narrow" : ""}${options.anchor ? " stable" : ""}`, role: "dialog", "aria-modal": "true", "aria-labelledby": titleId },
		h(
			"div",
			{ class: "modal-head" },
			h("div", { class: "modal-title", id: titleId, text: options.title }),
			h("button", { class: "modal-close", type: "button", "aria-label": "Close", onclick: () => close() }, "×"),
		),
		body,
		footer,
	);
	const layer = refs.modals;
	layer.classList.toggle("pinned", options.anchor === true);
	layer.replaceChildren(root);
	layer.hidden = false;

	const close = (): void => {
		if (activeModalClose !== close) return;
		activeModalClose = null;
		layer.replaceChildren();
		layer.hidden = true;
		layer.classList.remove("pinned");
		if (modalReturnFocus) {
			const target = modalReturnFocus;
			modalReturnFocus = null;
			target.focus();
		}
		options.onClose?.();
	};
	activeModalClose = close;

	root.addEventListener("mousedown", (event) => {
		if (event.target === root) close();
	});
	root.addEventListener("keydown", (event) => {
		if (event.key === "Escape") {
			event.stopPropagation();
			close();
			return;
		}
		if (event.key !== "Tab") return;
		const focusable = Array.from(root.querySelectorAll<HTMLElement>(
			"a[href],button:not([disabled]),input:not([disabled]),select:not([disabled]),textarea:not([disabled]),[tabindex]:not([tabindex='-1'])",
		)).filter((element) => element.offsetParent !== null);
		if (!focusable.length) return;
		const first = focusable[0];
		const last = focusable[focusable.length - 1];
		if (!first || !last) return;
		if (event.shiftKey && document.activeElement === first) {
			event.preventDefault();
			last.focus();
		} else if (!event.shiftKey && document.activeElement === last) {
			event.preventDefault();
			first.focus();
		}
	});
	window.requestAnimationFrame(() => focusFirst(root));
	return { root, body, footer, close };
}

function confirmDialog(options: {
	title: string;
	body: string | HTMLElement;
	confirmLabel: string;
	danger?: boolean;
	onConfirm: () => Promise<unknown>;
	onDone?: () => void;
}): void {
	const errorLine = h("div", { class: "invalid" });
	const content = typeof options.body === "string" ? h("div", { class: "panel-note", text: options.body }) : options.body;
	const modal = openModal({
		title: options.title,
		narrow: true,
		body: h("div", { class: "stack" }, content, errorLine),
	});
	const cancel = h("button", { class: "btn ghost", type: "button", onclick: () => modal.close() }, "Cancel");
	const confirm = h(
		"button",
		{ class: `btn ${options.danger ? "danger" : "primary"}`, type: "button" },
		options.confirmLabel,
	);
	confirm.onclick = () => {
		confirm.disabled = true;
		errorLine.textContent = "";
		void (async () => {
			try {
				await options.onConfirm();
				modal.close();
				options.onDone?.();
			} catch (error) {
				errorLine.textContent = errText(error);
				confirm.disabled = false;
			}
		})();
	};
	modal.footer.append(cancel, confirm);
	window.requestAnimationFrame(() => confirm.focus());
}

function openJsonModal(title: string, value: unknown): void {
	const modal = openModal({
		title,
		wide: true,
		body: h("pre", { class: "pre", text: safeJson(value) }),
	});
	modal.footer.append(
		h("button", { class: "btn", type: "button", onclick: () => void copyText(safeJson(value), "Copied") }, "Copy"),
		h("button", { class: "btn ghost", type: "button", onclick: () => modal.close() }, "Close"),
	);
}

/* ------------------------------------------------------------------ */
/* form helpers                                                        */
/* ------------------------------------------------------------------ */

function textInput(value: string, attrs: Attrs = {}): HTMLInputElement {
	const input = h("input", { class: "input", type: "text", ...attrs });
	input.value = value;
	return input;
}

function textArea(value: string, attrs: Attrs = {}): HTMLTextAreaElement {
	const area = h("textarea", { class: "textarea", ...attrs });
	area.value = value;
	return area;
}

function selectEl(options: Array<{ value: string; label: string }>, value: string, attrs: Attrs = {}): HTMLSelectElement {
	const select = h("select", { class: "select", ...attrs });
	for (const option of options) select.append(h("option", { value: option.value, text: option.label }));
	select.value = value;
	return select;
}

function field(label: string, control: HTMLElement, hint?: string): HTMLElement {
	if (!control.id) control.id = `field-${++fieldSequence}`;
	return h(
		"div",
		{ class: "field" },
		h("label", { for: control.id, text: label }),
		control,
		hint ? h("div", { class: "hint", text: hint }) : null,
	);
}

function lines(value: string): string[] {
	return value
		.split("\n")
		.map((line) => line.trim())
		.filter(Boolean);
}

function parseJson(text: string, label: string): unknown {
	try {
		return JSON.parse(text);
	} catch (error) {
		throw new Error(`${label} is not valid JSON: ${errText(error)}`);
	}
}

function requireObject(value: unknown, label: string): Record<string, unknown> {
	if (!isObj(value)) throw new Error(`${label} must be a JSON object.`);
	return value;
}

function errorLine(): HTMLElement {
	return h("div", { class: "invalid" });
}

function actionButton(label: string, onclick: () => void, attrs: Attrs = {}): HTMLButtonElement {
	return h("button", { class: "btn", type: "button", ...attrs, onclick }, label);
}

/* ------------------------------------------------------------------ */
/* bot editor                                                          */
/* ------------------------------------------------------------------ */

function openBotEditor(botId?: string): void {
	const existing = botId ? botOf(botId) : undefined;
	if (botId && !existing) {
		notify(`Unknown bot: ${botId}`, "error");
		return;
	}
	const name = textInput(existing?.name ?? "", { placeholder: "Research partner", "data-autofocus": true, maxlength: "120" });
	const description = textInput(existing?.description ?? "", { placeholder: "One line shown in lists" });
	const instructions = textArea(existing?.instructions ?? "", { rows: "5", class: "textarea plain", placeholder: "System instructions for this bot" });
	const memory = textArea(existing?.memory ?? "", { rows: "4", class: "textarea plain", placeholder: "Durable memory appended to the bot session" });
	const workspace = textInput(existing?.workspace ?? "", { placeholder: "/path/to/workspace" });
	const runtime = textInput(existing?.runtime ?? "", { placeholder: "runtime driver id" });
	const runtimeIds = [...new Set(state.bots.map((bot) => bot.runtime).filter(Boolean))];
	if (runtimeIds.length) {
		runtime.setAttribute("list", "runtime-ids");
	}
	const provider = textInput(existing?.model?.provider ?? "", { placeholder: "provider" });
	const modelId = textInput(existing?.model?.id ?? "", { placeholder: "model id" });
	const thinking = selectEl(
		THINKING_LEVELS.map((level) => ({ value: level, label: level })),
		existing?.thinking ?? "inherit",
	);
	const args = textArea(safeJson(existing?.runtimeOptions?.args ?? []), { rows: "3" });
	const env = textArea(safeJson(existing?.runtimeOptions?.env ?? {}), { rows: "3" });
	const configDir = existing ? botConfigDir(existing) : "";
	const perBotOnlyne = existing ? botOnlyne(existing) : { nodeId: "", peers: [] };
	const perBotPlugins = existing ? botPlugins(existing) : { host: [], renderer: [] };
	const configDirInput = textInput(configDir, { placeholder: "reported by the host after save" });
	configDirInput.readOnly = true;
	const onlyneNode = textInput(perBotOnlyne.nodeId, { placeholder: "urn:onlyne:agent:… (empty uses the host default)" });
	const onlynePeers = textArea(perBotOnlyne.peers.join("\n"), { rows: "3", placeholder: "http://host:port/onlyne/v3" });
	const hostPluginPaths = textArea(perBotPlugins.host.join("\n"), { rows: "3", placeholder: "/path/to/host-plugin.ts" });
	const rendererPluginPaths = textArea(perBotPlugins.renderer.join("\n"), { rows: "3", placeholder: "/plugins/example.js" });
	const capabilityBox = h(
		"div",
		{ class: "check-list" },
		...state.capabilities.map((capability) => {
			const checked = existing?.capabilities.includes(capability.id) ?? false;
			return h(
				"label",
				{ class: "check" },
				h("input", { type: "checkbox", value: capability.id, checked }),
				h("span", { text: capability.name }),
				h("span", { class: "meta", text: capability.kind }),
			);
		}),
	);
	if (!state.capabilities.length) capabilityBox.append(h("div", { class: "panel-note", text: "No capabilities exist yet. Create them in the Capabilities view." }));

	const modelCatalog = selectEl([{ value: "", label: "Load runtime models…" }], "");
	modelCatalog.disabled = true;
	const modelHint = h("div", { class: "hint", text: existing ? "Load the catalog from a conversation this bot belongs to." : "Save the bot and start a conversation to load its catalog." });
	const loadModels = h(
		"button",
		{ class: "btn sm", type: "button", disabled: !existing },
		"Load models",
	);
	loadModels.onclick = () => {
		if (!existing) return;
		loadModels.disabled = true;
		modelHint.textContent = "Loading…";
		void (async () => {
			try {
				const models = await runtimeModelsForBot(existing.id);
				modelCatalog.replaceChildren(h("option", { value: "", text: models.length ? "Choose a model…" : "No models returned" }));
				for (const model of models) modelCatalog.append(h("option", { value: `${model.provider}\u0000${model.id}`, text: `${model.provider}/${model.id}` }));
				modelCatalog.disabled = !models.length;
				modelHint.textContent = models.length ? `${models.length} models available.` : "The runtime returned no models.";
			} catch (error) {
				modelHint.textContent = errText(error);
			} finally {
				loadModels.disabled = false;
			}
		})();
	};
	modelCatalog.onchange = () => {
		const value = modelCatalog.value;
		if (!value) return;
		const [nextProvider = "", ...rest] = value.split("\u0000");
		provider.value = nextProvider;
		modelId.value = rest.join("\u0000");
	};

	const formError = errorLine();
	const form = h(
		"form",
		{ class: "form" },
		h("div", { class: "grid-2" }, field("Name", name), field("Description", description)),
		field("Instructions", instructions, "Passed to the OMP runtime as the bot's system instructions."),
		field("Memory", memory, "Kept per bot and appended to the session context."),
		h("div", { class: "grid-2" }, field("Workspace", workspace), field("Runtime", runtime)),
		h(
			"div",
			{ class: "grid-2" },
			field("Model provider", provider),
			field("Model id", modelId),
		),
		h("div", { class: "row" }, h("span", { class: "panel-note", text: "Model catalog" }), loadModels),
		h("div", { class: "grid-2" }, field("Catalog", modelCatalog, undefined), field("Thinking level", thinking)),
		field("Runtime arguments", args, "JSON array of command-line arguments."),
		field("Runtime environment", env, "JSON object of extra environment variables."),
		h(
			"div",
			{ class: "card" },
			h("div", { class: "card-head", text: "This bot's configuration" }),
			h(
				"div",
				{ class: "card-body" },
				field(
					"Configuration root",
					configDirInput,
					configDir ? "This bot's .omp directory holds its models, plugins, MCP servers, Onlyne identity and memory." : "The host reports this path once the bot exists.",
				),
				h("div", { class: "grid-2" }, field("Onlyne node id", onlyneNode), field("Onlyne peers", onlynePeers, "One address per line; stored in this bot's configuration.")),
			),
		),
		h(
			"div",
			{ class: "card" },
			h("div", { class: "card-head", text: "This bot's plugins" }),
			h(
				"div",
				{ class: "card-body" },
				field("Host plugin paths", hostPluginPaths, "One path per line. Loaded inside this bot's OMP session."),
				field("Renderer plugin paths", rendererPluginPaths, "Active only while this bot's conversation is on screen."),
			),
		),
		h("div", { class: "field" }, h("span", { class: "hint", text: "Capabilities" }), capabilityBox),
		h(
			"div",
			{ class: "panel-actions" },
			h(
				"button",
				{ class: "btn ghost", type: "button", onclick: () => {
					modal.close();
					openPanel("capabilities");
				} },
				"Open Capabilities view",
			),
		),
		formError,
	);
	const runtimeDatalist = h("datalist", { id: "runtime-ids" }, ...runtimeIds.map((id) => h("option", { value: id })));
	const modal = openModal({ title: existing ? `Edit ${existing.name}` : "New bot", wide: true, body: h("div", { class: "stack" }, form, runtimeDatalist) });

	const save = h("button", { class: "btn primary", type: "button" }, existing ? "Save bot" : "Create bot");
	const cancel = h("button", { class: "btn ghost", type: "button", onclick: () => modal.close() }, "Cancel");
	modal.footer.append(cancel);
	if (existing) {
		const del = h("button", { class: "btn danger", type: "button" }, "Delete bot");
		if (state.teamAdminId === existing.id) {
			del.disabled = true;
			del.title = "Transfer team administration before deleting this bot";
		}
		del.onclick = () => {
			modal.close();
			confirmDeleteBot(existing);
		};
		modal.footer.append(del);
	}
	modal.footer.append(save);

	const submit = async (): Promise<void> => {
		formError.textContent = "";
		const trimmed = name.value.trim();
		if (!trimmed) {
			formError.textContent = "A bot needs a name.";
			name.focus();
			return;
		}
		let parsedArgs: string[];
		let parsedEnv: Record<string, string>;
		try {
			const argsValue = parseJson(args.value || "[]", "Runtime arguments");
			if (!Array.isArray(argsValue) || argsValue.some((item) => typeof item !== "string")) {
				throw new Error("Runtime arguments must be a JSON array of strings.");
			}
			parsedArgs = argsValue as string[];
			const envValue = parseJson(env.value || "{}", "Runtime environment");
			if (!isObj(envValue) || Object.values(envValue).some((item) => typeof item !== "string")) {
				throw new Error("Runtime environment must be a JSON object of strings.");
			}
			parsedEnv = envValue as Record<string, string>;
			if ((provider.value.trim() && !modelId.value.trim()) || (!provider.value.trim() && modelId.value.trim())) {
				throw new Error("A model needs both a provider and an id.");
			}
		} catch (error) {
			formError.textContent = errText(error);
			return;
		}
		const selectedCapabilities = Array.from(capabilityBox.querySelectorAll<HTMLInputElement>('input[type="checkbox"]:checked')).map(
			(input) => input.value,
		);
		const bot: Partial<BotRecord> = {
			name: trimmed,
			description: description.value.trim(),
			instructions: instructions.value,
			memory: memory.value,
			workspace: workspace.value.trim(),
			runtime: runtime.value.trim(),
			thinking: thinking.value as ThinkingLevel,
			capabilities: selectedCapabilities,
			runtimeOptions: { args: parsedArgs, env: parsedEnv },
			onlyne: { nodeId: onlyneNode.value.trim(), peers: lines(onlynePeers.value) },
			plugins: { host: lines(hostPluginPaths.value), renderer: lines(rendererPluginPaths.value) },
		};
		if (existing) bot.id = existing.id;
		if (provider.value.trim() && modelId.value.trim()) bot.model = { provider: provider.value.trim(), id: modelId.value.trim() };
		save.disabled = true;
		try {
			const value = await command("bot.save", { bot });
			await refresh().catch(() => undefined);
			modal.close();
			notify(existing ? "Bot saved." : "Bot created.", "ok");
			if (!existing) {
				if (isObj(value) && typeof value["id"] === "string") selectedBotId = value["id"];
				const newest = state.bots.slice().sort((a, b) => b.createdAt - a.createdAt)[0];
				if (newest && !selectedBotId) selectedBotId = newest.id;
				renderAll();
			}
		} catch (error) {
			formError.textContent = errText(error);
		} finally {
			save.disabled = false;
		}
	};
	form.addEventListener("submit", (event) => {
		event.preventDefault();
		void submit();
	});
	save.addEventListener("click", (event) => {
		event.preventDefault();
		void submit();
	});
	form.append(h("button", { type: "submit", class: "sr-only" }, "Submit"));
}

function confirmDeleteBot(bot: Bot): void {
	confirmDialog({
		title: "Delete bot",
		body: `Delete ${bot.name}? This removes the bot definition.`,
		confirmLabel: "Delete bot",
		danger: true,
		onConfirm: async () => {
			await command("bot.delete", { id: bot.id });
			await refresh().catch(() => undefined);
			notify(`Deleted ${bot.name}.`, "ok");
		},
	});
}

function confirmTeamAdmin(bot: Bot): void {
	confirmDialog({
		title: "Transfer team administration",
		body: `Make ${bot.name} the team administrator? It can create and edit bots, create groups and manage the roster.`,
		confirmLabel: "Transfer",
		onConfirm: async () => {
			await command("team.admin", { botId: bot.id });
			await refresh().catch(() => undefined);
			notify(`${bot.name} now manages the team.`, "ok");
		},
	});
}

/* ------------------------------------------------------------------ */
/* conversation editor                                                 */
/* ------------------------------------------------------------------ */

function openConversationEditor(conversationId?: string): void {
	const existing = conversationId ? conversationOf(conversationId) : undefined;
	if (conversationId && !existing) {
		notify(`Unknown conversation: ${conversationId}`, "error");
		return;
	}
	if (!state.bots.length) {
		notify("Create a bot before starting a conversation.", "warn");
		openBotEditor();
		return;
	}
	const title = textInput(existing?.title ?? "", { placeholder: "Conversation title", "data-autofocus": true });
	const kind = selectEl(
		[
			{ value: "direct", label: "Direct — one bot" },
			{ value: "group", label: "Group — several bots" },
		],
		existing?.kind ?? (state.bots.length > 1 ? "group" : "direct"),
	);
	const memory = textArea(existing?.memory ?? "", { rows: "3", class: "textarea plain", placeholder: "Conversation memory shared by its bots" });
	const selected = new Set<string>(existing?.botIds ?? (selectedBotId ? [selectedBotId] : []));
	if (!selected.size && state.bots[0]) selected.add(state.bots[0].id);

	const memberBox = h("div", { class: "check-list" });
	const admin = selectEl([], "", { id: "conversation-admin" });

	const refreshMembers = (): void => {
		memberBox.replaceChildren(
			...state.bots.map((bot) => {
				const checked = selected.has(bot.id);
				return h(
					"label",
					{ class: "check" },
					h("input", {
						type: "checkbox",
						value: bot.id,
						checked,
						onchange: (event: Event) => {
							const input = inputFrom(event);
							if (!input) return;
							if (input.checked) {
								if (kind.value === "direct") {
									selected.clear();
									for (const other of Array.from(memberBox.querySelectorAll<HTMLInputElement>('input[type="checkbox"]'))) {
										if (other !== input) other.checked = false;
									}
								}
								selected.add(bot.id);
							} else {
								selected.delete(bot.id);
							}
							refreshAdmin();
						},
					}),
					h("span", { text: bot.name }),
					h("span", { class: "meta", text: bot.runtime || "runtime unset" }),
				);
			}),
		);
		refreshAdmin();
	};

	const refreshAdmin = (): void => {
		const ids = [...selected];
		const previous = admin.value || existing?.adminBotId || ids[0] || "";
		admin.replaceChildren(...ids.map((id) => h("option", { value: id, text: botName(id) })));
		admin.value = ids.includes(previous) ? previous : ids[0] ?? "";
		admin.disabled = kind.value !== "group" || ids.length === 0;
	};

	kind.onchange = () => {
		if (kind.value === "direct" && selected.size > 1) {
			const keep = [...selected][0];
			selected.clear();
			if (keep) selected.add(keep);
			for (const input of Array.from(memberBox.querySelectorAll<HTMLInputElement>('input[type="checkbox"]'))) {
				input.checked = input.value === keep;
			}
		}
		refreshAdmin();
	};
	refreshMembers();

	const formError = errorLine();
	const form = h(
		"form",
		{ class: "form" },
		h("div", { class: "grid-2" }, field("Title", title), field("Kind", kind)),
		field("Members", memberBox),
		field("Admin bot", admin, "Only group conversations use an admin bot."),
		field("Conversation memory", memory, "Shared with every bot in this conversation."),
		formError,
	);
	const modal = openModal({ title: existing ? "Conversation settings" : "New conversation", body: form });
	const save = h("button", { class: "btn primary", type: "button" }, existing ? "Save conversation" : "Start conversation");
	modal.footer.append(h("button", { class: "btn ghost", type: "button", onclick: () => modal.close() }, "Cancel"), save);

	save.onclick = () => {
		formError.textContent = "";
		const trimmed = title.value.trim();
		const ids = [...selected];
		if (!trimmed) {
			formError.textContent = "A conversation needs a title.";
			title.focus();
			return;
		}
		if (!ids.length) {
			formError.textContent = "Choose at least one bot.";
			return;
		}
		if (kind.value === "direct" && ids.length !== 1) {
			formError.textContent = "A direct conversation has exactly one bot.";
			return;
		}
		const adminId = kind.value === "group" ? (ids.includes(admin.value) ? admin.value : ids[0]) : ids[0];
		const beforeIds = new Set(state.conversations.map((conversation) => conversation.id));
		const payload: Record<string, unknown> = {
			title: trimmed,
			kind: kind.value,
			botIds: ids,
			adminBotId: adminId,
			memory: memory.value,
		};
		save.disabled = true;
		void (async () => {
			try {
				if (existing) await command("conversation.update", { id: existing.id, ...payload });
				else await command("conversation.create", payload);
				await refresh().catch(() => undefined);
				modal.close();
				notify(existing ? "Conversation saved." : "Conversation created.", "ok");
				if (!existing) {
					const created = state.conversations.find((conversation) => !beforeIds.has(conversation.id));
					if (created) selectedConversationId = created.id;
					renderAll();
					syncHostSelection();
				}
			} catch (error) {
				formError.textContent = errText(error);
			} finally {
				save.disabled = false;
			}
		})();
	};
}

async function updateConversation(id: string, patch: Partial<Conversation>): Promise<void> {
	try {
		await command("conversation.update", { id, ...patch });
		await refresh().catch(() => undefined);
	} catch (error) {
		notify(errText(error), "error");
	}
}

function confirmRemoveMember(conversation: Conversation, botId: string): void {
	const next = conversation.botIds.filter((id) => id !== botId);
	const admin = conversation.adminBotId === botId ? next[0] ?? "" : conversation.adminBotId;
	confirmDialog({
		title: "Remove member",
		body: `Remove ${botName(botId)} from ${conversation.title}?`,
		confirmLabel: "Remove",
		danger: true,
		onConfirm: async () => {
			await command("conversation.update", { id: conversation.id, botIds: next, adminBotId: admin });
			await refresh().catch(() => undefined);
		},
	});
}

/* ------------------------------------------------------------------ */
/* capability editor                                                   */
/* ------------------------------------------------------------------ */

function openCapabilityEditor(capability?: Capability): void {
	const name = textInput(capability?.name ?? "", { placeholder: "Capability name", "data-autofocus": true });
	const kind = selectEl(
		[
			{ value: "mcp", label: "MCP server" },
			{ value: "skill", label: "Skill" },
			{ value: "extension", label: "Extension" },
		],
		capability?.kind ?? "mcp",
	);
	const fields = h("div", { class: "stack" });
	const buildFields = (): void => {
		fields.replaceChildren();
		if (kind.value === "mcp") {
			const config = textArea(capability?.kind === "mcp" ? safeJson(capability.config) : "{}", { rows: "6" });
			config.dataset["role"] = "mcp-config";
			fields.append(field("MCP config", config, "JSON object passed to the MCP runtime."));
		} else {
			const path = textInput(capability && capability.kind !== "mcp" ? capability.path : "", { placeholder: "/path/to/" });
			path.dataset["role"] = "capability-path";
			fields.append(field(kind.value === "skill" ? "Skill path" : "Extension path", path));
		}
	};
	buildFields();
	kind.onchange = buildFields;
	const formError = errorLine();
	const form = h("form", { class: "form" }, h("div", { class: "grid-2" }, field("Name", name), field("Kind", kind)), fields, formError);
	const modal = openModal({ title: capability ? "Edit capability" : "New capability", body: form });
	const save = h("button", { class: "btn primary", type: "button" }, capability ? "Save capability" : "Create capability");
	modal.footer.append(h("button", { class: "btn ghost", type: "button", onclick: () => modal.close() }, "Cancel"), save);
	if (capability) {
		const del = h("button", { class: "btn danger", type: "button" }, "Delete");
		del.onclick = () => {
			modal.close();
			confirmDeleteCapability(capability);
		};
		modal.footer.append(del);
	}
	save.onclick = () => {
		formError.textContent = "";
		const trimmed = name.value.trim();
		if (!trimmed) {
			formError.textContent = "A capability needs a name.";
			return;
		}
		try {
			const id = capability?.id ?? crypto.randomUUID();
			let next: Capability;
			if (kind.value === "mcp") {
				const config = requireObject(parseJson(fields.querySelector<HTMLTextAreaElement>('[data-role="mcp-config"]')?.value ?? "{}", "MCP config"), "MCP config") as unknown as Record<string, Json>;
				next = { id, name: trimmed, kind: "mcp", config };
			} else {
				const path = fields.querySelector<HTMLInputElement>('[data-role="capability-path"]')?.value.trim() ?? "";
				if (!path) throw new Error("A skill or extension needs a path.");
				next = { id, name: trimmed, kind: kind.value as "skill" | "extension", path };
			}
			save.disabled = true;
			void (async () => {
				try {
					await command("capability.save", { capability: next });
					await refresh().catch(() => undefined);
					modal.close();
					notify(capability ? "Capability saved." : "Capability created.", "ok");
					renderPanel();
				} catch (error) {
					formError.textContent = errText(error);
				} finally {
					save.disabled = false;
				}
			})();
		} catch (error) {
			formError.textContent = errText(error);
		}
	};
}

function confirmDeleteCapability(capability: Capability): void {
	confirmDialog({
		title: "Delete capability",
		body: `Delete ${capability.name}? Bots that reference it lose this capability.`,
		confirmLabel: "Delete",
		danger: true,
		onConfirm: async () => {
			await command("capability.delete", { id: capability.id });
			await refresh().catch(() => undefined);
			renderPanel();
		},
	});
}

/* ------------------------------------------------------------------ */
/* panels                                                              */
/* ------------------------------------------------------------------ */

function registerPanelInternal(id: string, title: string, render: (snapshot: DesktopState) => HTMLElement): void {
	panels.set(id, { id, title, render });
}

let routing = false;

function writeRoute(): void {
	if (routing || !shellReady) return;
	const next = formatDeskHash({
		screen: activePanel ?? "chat",
		...(selectedConversationId ? { selection: selectedConversationId } : {}),
		...(layout.dock ? { dock: layout.dock } : {}),
	});
	if (location.hash === next) return;
	history.pushState({ onlyne: true }, "", next);
}

function restoreRoute(): void {
	if (!shellReady) return;
	const route = parseDeskHash(location.hash || "#/chat");
	routing = true;
	try {
		if (route.selection && state.conversations.some(item => item.id === route.selection)) selectedConversationId = route.selection;
		const dock = route.dock && panels.has(route.dock) ? route.dock : null;
		if (layout.dock !== dock) {
			dockKey = "";
			layout = { ...layout, dock };
			persistLayout();
		}
		const screen = route.screen === "chat" || !panels.has(route.screen) ? null : route.screen;
		if (screen === null) {
			activePanel = null;
			renderAll();
		} else {
			activePanel = screen;
			renderAll();
			renderPanel();
		}
		renderStatusbar();
	} finally {
		routing = false;
	}
}

function openPanel(id: string): void {
	if (!panels.has(id)) {
		notify(`Unknown view: ${id}`, "error");
		return;
	}
	if (activePanel === "settings" && id !== "settings") resetSettingsPreview();
	activePanel = id;
	closeDrawers();
	renderRail();
	renderNav();
	renderTopbar();
	renderView();
	renderPanel();
	renderDock();
	writeRoute();
}

function showChat(options?: { focus?: boolean }): void {
	if (activePanel === "settings") resetSettingsPreview();
	activePanel = null;
	closeDrawers();
	renderRail();
	renderNav();
	renderTopbar();
	renderView();
	renderDock();
	writeRoute();
	if (options?.focus) refs.composerInput.focus();
}

function withPageHeader(id: string, title: string, content: HTMLElement): HTMLElement {
	if (content.classList.contains("desk-page") || content.querySelector(".desk-page") !== null) return content;
	const place = [...BUILTIN_WORKSPACES, ...extraWorkspaces].find(item => item.panel === id);
	return h("div", { class: "desk-page" },
		h("header", { class: "desk-page-head" },
			h("div", { class: "desk-page-copy" },
				h("p", { class: "eyebrow", text: "Desk" }),
				h("h1", { text: title }),
				place?.hint ? h("p", { class: "lede", text: place.hint }) : false,
			),
		),
		content,
	);
}

function renderPanel(): void {
	const id = activePanel;
	if (!id) return;
	const panel = panels.get(id);
	if (!panel) {
		showChat();
		return;
	}
	try {
		const rendered = panel.render(state);
		const content = withPageHeader(id, panel.title, rendered);
		refs.panelView.replaceChildren(h("div", { class: "panel" }, h("div", { class: "panel-body" }, content)));
	} catch (error) {
		refs.panelView.replaceChildren(
			h("div", { class: "panel" }, h("div", { class: "panel-body" }, h("div", { class: "invalid", text: `Panel failed: ${errText(error)}` }))),
		);
	}
}

function capabilitiesPanel(): HTMLElement {
	const wrap = h("div", { class: "panel-inner" });
	wrap.append(
		h(
			"div",
			{ class: "spread" },
			h("div", { class: "panel-note", text: "MCP servers, skills and extensions available to bots." }),
			h("button", { class: "btn primary", type: "button", onclick: () => openCapabilityEditor() }, icon("plus"), "New capability"),
		),
	);
	if (!state.capabilities.length) {
		wrap.append(h("div", { class: "empty-nav", text: "No capabilities yet." }));
		return wrap;
	}
	for (const capability of state.capabilities) {
		const rows: Array<[string, string]> = [["Kind", capability.kind]];
		if (capability.kind === "mcp") rows.push(["Config", compactJson(capability.config)]);
		else rows.push(["Path", capability.path]);
		const users = state.bots.filter((bot) => bot.capabilities.includes(capability.id)).map((bot) => bot.name);
		if (users.length) rows.push(["Used by", users.join(", ")]);
		wrap.append(
			h(
				"div",
				{ class: "card" },
				h("div", { class: "card-head" }, h("span", { text: capability.name }), h("span", { class: "chip dim", text: capability.kind })),
				h(
					"div",
					{ class: "card-body" },
					definitionList(rows),
					h(
						"div",
						{ class: "panel-actions" },
						h("button", { class: "btn sm", type: "button", onclick: () => openCapabilityEditor(capability) }, "Edit"),
						h("button", { class: "btn danger sm", type: "button", onclick: () => confirmDeleteCapability(capability) }, "Delete"),
					),
				),
			),
		);
	}
	return wrap;
}

function layoutCard(): HTMLElement {
	const box = h("div", { class: "check-grid" });
	for (const spec of railCatalog()) {
		if (spec.id === "chat") continue;
		const input = h("input", { type: "checkbox" }) as HTMLInputElement;
		input.checked = !layout.hidden.includes(spec.id);
		input.addEventListener("change", () => {
			const hidden = new Set(layout.hidden);
			if (input.checked) hidden.delete(spec.id);
			else hidden.add(spec.id);
			layout = { ...layout, hidden: [...hidden] };
			persistLayout();
		});
		box.append(h("label", { class: "check" }, input, ` ${spec.title}`));
	}
	const dock = h("select", { "aria-label": "Docked panel" });
	dock.append(h("option", { value: "", text: "Nothing docked" }));
	for (const panel of panels.values()) {
		if (panel.botId) continue;
		dock.append(h("option", { value: panel.id, text: panel.title }));
	}
	dock.value = layout.dock ?? "";
	dock.addEventListener("change", () => setDock(dock.value.length === 0 ? null : dock.value));
	return h("div", { class: "card" },
		h("div", { class: "card-head", text: "Layout" }),
		h("div", { class: "card-body" },
			h("p", { class: "field-hint", text: "Drag a rail icon, or focus it and press Alt+Up or Alt+Down. Right-click docks that panel. Hidden places stay in Jump." }),
			h("label", { class: "check" }, (() => {
				const input = h("input", { type: "checkbox" }) as HTMLInputElement;
				input.checked = developerMode();
				input.addEventListener("change", () => localStorage.setItem("onlyne.desk.developer", input.checked ? "1" : "0"));
				return input;
			})(), " Developer mode (raw commands in Jump)"),
			box,
			field("Dock", dock, "Pins a second panel beside the stage."),
			h("button", { class: "btn", type: "button", onclick: () => { resetLayout(); renderPanel(); } }, "Reset layout")));
}

function settingsPanel(): HTMLElement {
	themeDirty = false;
	customCssPreview = null;
	const settings = currentSettings();
	const activeBots = uiActiveBotIds();
	const themeRegistrations = [...themeStyles.values()].filter(
		(entry) => !entry.botId || activeBots.has(entry.botId) || entry.id === settings.theme,
	);
	const themeOptions: Array<{ value: string; label: string }> = [
		{ value: "system", label: "System" },
		{ value: "dark", label: "Dark" },
		{ value: "light", label: "Light" },
	];
	const seenThemes = new Set(themeOptions.map((option) => option.value));
	for (const entry of themeRegistrations) {
		if (seenThemes.has(entry.id)) continue;
		seenThemes.add(entry.id);
		themeOptions.push({ value: entry.id, label: entry.label });
	}
	if (settings.theme && !seenThemes.has(settings.theme)) {
		themeOptions.push({ value: settings.theme, label: `${settings.theme} (theme not loaded)` });
	}
	const theme = selectEl(themeOptions, settings.theme);
	const locale = selectEl(
		[
			{ value: "en", label: "English" },
			{ value: "zh-CN", label: "简体中文" },
		],
		settings.locale,
	);
	const customCss = textArea(settings.customCss, { rows: "6", placeholder: "/* custom CSS */" });
	const nodeId = textInput(settings.nodeId, { placeholder: "urn:onlyne:agent:…" });
	const peers = textArea(settings.peers.join("\n"), { rows: "4", placeholder: "http://host:port/onlyne/v3" });
	const hostPlugins = textArea(settings.hostPlugins.join("\n"), { rows: "3", placeholder: "/path/to/host-plugin.ts" });
	const rendererPlugins = textArea(settings.rendererPlugins.join("\n"), { rows: "3", placeholder: "/plugins/example.js" });
	const formError = errorLine();

	theme.onchange = () => {
		themeDirty = true;
		applyTheme(theme.value);
	};
	customCss.addEventListener("input", () => {
		customCssPreview = customCss.value;
		applyCustomCss(customCss.value);
	});

	const save = h("button", { class: "btn primary", type: "button" }, "Save settings");
	const reset = h(
		"button",
		{ class: "btn", type: "button", onclick: () => {
			themeDirty = false;
			customCssPreview = null;
			applyAppearance();
			renderPanel();
		} },
		"Revert changes",
	);
	save.onclick = () => {
		formError.textContent = "";
		const next: DesktopSettings = {
			theme: theme.value,
			locale: locale.value === "zh-CN" ? "zh-CN" : "en",
			hostPlugins: lines(hostPlugins.value),
			rendererPlugins: lines(rendererPlugins.value),
			customCss: customCss.value,
			nodeId: nodeId.value.trim(),
			peers: lines(peers.value),
		};
		save.disabled = true;
		void (async () => {
			try {
				await command("settings.save", { settings: next });
				themeDirty = false;
				customCssPreview = null;
				await refresh().catch(() => undefined);
				notify("Settings saved.", "ok");
				renderPanel();
			} catch (error) {
				formError.textContent = errText(error);
			} finally {
				save.disabled = false;
			}
		})();
	};

	const form = h(
		"div",
		{ class: "form" },
		h(
			"div",
			{ class: "card" },
			h("div", { class: "card-head", text: "Appearance" }),
			h(
				"div",
				{ class: "card-body" },
				h("div", { class: "grid-2" }, field("Theme", theme), field("Locale", locale, "Stored for host plugins; the built-in shell is English.")),
				field("Custom CSS", customCss, "Applied immediately as a preview; saved with the button below."),
			),
		),
		layoutCard(),
		h(
			"div",
			{ class: "card" },
			h("div", { class: "card-head", text: "Application node" }),
			h(
				"div",
				{ class: "card-body" },
				field("Node id", nodeId),
				field("Peers", peers, "Application-level peers, one address per line. Each bot also keeps its own Onlyne peers."),
			),
		),
		h(
			"div",
			{ class: "card" },
			h("div", { class: "card-head", text: "Application plugins" }),
			h(
				"div",
				{ class: "card-body" },
				field("Host plugins", hostPlugins, "Application-level host plugins, one path per line. Bot plugins live in each bot's configuration."),
				field("Renderer plugins", rendererPlugins, "Application-level window extensions. Saved for the next launch; this window uses the list injected at startup."),
			),
		),
		formError,
		h("div", { class: "form-actions" }, reset, save),
	);
	const wrap = h(
		"div",
		{ class: "panel-inner" },
		h(
			"div",
			{ class: "panel-note", text: "These are application and window preferences. Each bot's models, plugins, MCP servers, Onlyne identity and memory live in its own configuration root." },
		),
		h(
			"div",
			{ class: "panel-actions" },
			h("button", { class: "btn", type: "button", onclick: () => openPanel("node") }, icon("node"), "Node record"),
			h("button", { class: "btn", type: "button", onclick: () => openPanel("capabilities") }, "Capabilities"),
		),
		form,
	);
	return wrap;
}

/* ------------------------------------------------------------------ */
/* node panel                                                          */
/* ------------------------------------------------------------------ */

let lastOnlyneStatus: unknown = null;

function nodePanel(): HTMLElement {
	const wrap = h("div", { class: "panel-inner" });
	let botId = currentBotId() ?? state.bots[0]?.id ?? "";
	const scopeSelect =
		state.bots.length > 1
			? selectEl(
					state.bots.map((bot) => ({ value: bot.id, label: bot.name })),
					botId,
				)
			: null;
	if (scopeSelect) {
		scopeSelect.id = "node-scope";
		scopeSelect.setAttribute("aria-label", "Bot whose Onlyne node to inspect");
		scopeSelect.onchange = () => {
			botId = scopeSelect.value;
			void loadStatus();
		};
	}
	const statusBody = h("div", { class: "card-body" });
	const statusCard = h(
		"div",
		{ class: "card" },
		h(
			"div",
			{ class: "card-head" },
			h("span", { text: "Node status" }),
			h("span", { class: "chip dim", text: botId ? botName(botId) : "host default" }),
			h(
				"button",
				{ class: "btn sm", type: "button", onclick: () => void loadStatus() },
				icon("refresh", 14),
				"Refresh",
			),
		),
		statusBody,
	);

	const loadStatus = async (): Promise<void> => {
		lastOnlyneStatus = null;
		statusBody.replaceChildren(h("div", { class: "row" }, h("span", { class: "spinner" }), h("span", { class: "dim", text: "Reading node status…" })));
		try {
			const value = await command("onlyne.status", botId ? { botId } : {});
			lastOnlyneStatus = value;
			statusBody.replaceChildren(renderOnlyneStatus(value, () => void loadStatus(), botId));
		} catch (error) {
			statusBody.replaceChildren(h("div", { class: "invalid", text: errText(error) }));
		}
	};

	const connectUrl = textInput("", { placeholder: "http://host:port/onlyne/v3" });
	const connectResult = h("pre", { class: "palette-result" });
	const connect = h(
		"button",
		{ class: "btn primary", type: "button", onclick: () => void runOnlyne("connect", { url: connectUrl.value.trim() }, connectResult, botId, () => void loadStatus()) },
		"Connect",
	);
	const connectCard = h(
		"div",
		{ class: "card" },
		h("div", { class: "card-head", text: "Connect a peer" }),
		h(
			"div",
			{ class: "card-body" },
			field("Peer URL", connectUrl),
			h("div", { class: "panel-actions" }, connect),
			connectResult,
		),
	);

	const requestNode = textInput("", { placeholder: "urn:onlyne:agent:peer" });
	const requestResult = h("pre", { class: "palette-result" });
	const request = h(
		"button",
		{ class: "btn", type: "button", onclick: () => void runOnlyne("trust.request", { nodeId: requestNode.value.trim() }, requestResult, botId, () => void loadStatus()) },
		"Request trust",
	);
	const manualApproveNode = textInput("", { placeholder: "urn:onlyne:agent:peer" });
	const manualApproveFingerprint = textInput("", { placeholder: "peer fingerprint" });
	const approveCard = h(
		"div",
		{ class: "card" },
		h("div", { class: "card-head", text: "Trust" }),
		h(
			"div",
			{ class: "card-body" },
			field("Peer node", requestNode),
			h("div", { class: "panel-actions" }, request),
			requestResult,
			h("div", { class: "divider" }),
			field("Approve node", manualApproveNode),
			field("Approve fingerprint", manualApproveFingerprint, "Shown to you before approval. The panel never approves on its own."),
			h(
				"div",
				{ class: "panel-actions" },
				h(
					"button",
					{
						class: "btn primary",
						type: "button",
						onclick: () => {
							const node = manualApproveNode.value.trim();
							const fingerprint = manualApproveFingerprint.value.trim();
							if (!node || !fingerprint) {
								notify("Enter both the peer node and its fingerprint.", "warn");
								return;
							}
							approveTrust(node, fingerprint, () => void loadStatus(), botId);
						},
					},
					"Review and approve…",
				),
			),
		),
	);

	const rawAction = textInput("connect", { placeholder: "connect" });
	rawAction.setAttribute("list", "onlyne-actions");
	const rawArgs = textArea("{}", { rows: "4" });
	const rawResult = h("pre", { class: "palette-result" });
	const rawCard = h(
		"div",
		{ class: "card" },
		h("div", { class: "card-head", text: "Raw onlyne action" }),
		h(
			"div",
			{ class: "card-body" },
			h("div", { class: "grid-2" }, field("Action", rawAction), field("Arguments", rawArgs, "JSON object.")),
			h(
				"div",
				{ class: "panel-actions" },
				h(
					"button",
					{
						class: "btn",
						type: "button",
						onclick: () => {
							try {
								const args = requireObject(parseJson(rawArgs.value || "{}", "Arguments"), "Arguments");
								void runOnlyne(rawAction.value.trim(), args, rawResult, botId, () => void loadStatus());
							} catch (error) {
								rawResult.textContent = errText(error);
							}
						},
					},
					"Run action",
				),
			),
			rawResult,
		),
	);

	wrap.append(
		h(
			"div",
			{ class: "panel-note", text: "Each bot owns its Onlyne node. This panel reads and controls the selected bot's node; trust decisions require explicit fingerprint confirmation." },
		),
	);
	if (scopeSelect) wrap.append(scopeSelect);
	wrap.append(
		statusCard,
		h("div", { class: "panel-grid" }, connectCard, approveCard, rawCard),
		h("datalist", { id: "onlyne-actions" }, ...["connect", "trust.request", "trust.approve", "trust.revoke", "thread.read", "resource.list"].map((name) => h("option", { value: name }))),
	);
	void loadStatus();
	return wrap;
}

function renderOnlyneStatus(value: unknown, reload: () => void, botId?: string): HTMLElement {
	const container = h("div", { class: "stack" });
	if (isObj(value)) {
		const node = isObj(value["node"]) ? value["node"] : null;
		const trust = isObj(value["trust"]) ? value["trust"] : null;
		const summary: Array<[string, string]> = [];
		if (node) {
			summary.push(["Node", firstString(node, ["id", "nodeId"])]);
			summary.push(["Endpoint", firstString(node, ["endpoint", "baseUrl"])]);
			summary.push(["Fingerprint", firstString(node, ["fingerprint"])]);
			summary.push(["Instance", firstString(node, ["instanceId"])]);
		}
		if (trust) summary.push(["Local fingerprint", firstString(trust, ["localFingerprint"])]);
		const threads = Array.isArray(value["threads"]) ? value["threads"].length : 0;
		if (threads) summary.push(["Threads", String(threads)]);
		const inbox = isObj(value["inbox"]) ? value["inbox"]["pending"] : null;
		if (typeof inbox === "number") summary.push(["Inbox pending", String(inbox)]);
		const resources = isObj(value["resources"]) && Array.isArray(value["resources"]["owners"]) ? value["resources"]["owners"].length : 0;
		if (resources) summary.push(["Resource owners", String(resources)]);
		const present = summary.filter(([, entry]) => entry.length > 0);
		if (present.length) container.append(definitionList(present));
		const primitiveRows: Array<[string, string]> = [];
		for (const [key, entry] of Object.entries(value)) {
			if (entry === null || typeof entry !== "object") primitiveRows.push([key, textOf(entry)]);
		}
		if (primitiveRows.length) container.append(definitionList(primitiveRows));
		const rows = extractTrustRows(value);
		if (rows.length) {
			for (const row of rows) container.append(renderTrustRow(row, reload, botId));
		}
	}
	container.append(
		h(
			"details",
			{ class: "block raw" },
			h("summary", {}, h("span", { class: "block-head" }, h("span", { class: "tool-name", text: "raw status" }))),
			h("div", { class: "block-body" }, h("pre", { text: safeJson(value) })),
		),
	);
	return container;
}

function extractTrustRows(value: Record<string, unknown>): Array<Record<string, unknown>> {
	const rows: Array<Record<string, unknown>> = [];
	const seen = new Set<string>();
	const push = (item: Record<string, unknown>, source: string): void => {
		const key = firstString(item, ["nodeId", "node_id", "id"]) + "\u0000" + firstString(item, ["fingerprint", "fingerprintHex", "fingerprint_hex"]);
		if (key !== "\u0000" && seen.has(key)) return;
		if (key !== "\u0000") seen.add(key);
		rows.push({ ...item, __source: source });
	};
	const trust = isObj(value["trust"]) ? value["trust"] : null;
	if (trust) {
		for (const key of ["pending", "peers"]) {
			const candidate = trust[key];
			if (!Array.isArray(candidate)) continue;
			for (const item of candidate) if (isObj(item)) push(item, key);
		}
	}
	for (const key of ["neighbors", "peers", "trusted", "trust", "pending", "requests", "entries", "contacts"]) {
		const candidate = value[key];
		if (!Array.isArray(candidate)) continue;
		for (const item of candidate) if (isObj(item)) push(item, key);
	}
	return rows;
}

function firstString(row: Record<string, unknown>, keys: string[]): string {
	for (const key of keys) {
		const value = row[key];
		if (typeof value === "string" && value.trim()) return value;
	}
	return "";
}

function renderTrustRow(row: Record<string, unknown>, reload: () => void, botId?: string): HTMLElement {
	const nodeId = firstString(row, ["nodeId", "node_id", "id", "peer", "node"]);
	const fingerprint = firstString(row, ["fingerprint", "fingerprintHex", "fingerprint_hex", "pubkeyFingerprint"]);
	const state = firstString(row, ["state", "phase", "status", "relation"]);
	const url = firstString(row, ["url", "baseUrl", "base_url", "endpoint", "address"]);
	const source = typeof row["__source"] === "string" ? row["__source"] : "";
	const phase = firstString(row, ["phase"]);
	const lastSeen = firstString(row, ["lastSeen", "lastChecked", "verifiedAt"]);
	const pending = /request|invite|pending/i.test(state) || /pending|request/i.test(source);
	const trusted = /trust|exchanged|approved|active/i.test(state);
	const isNeighbor = source === "neighbors";
	const displayState = state || (pending ? "pending approval" : isNeighbor ? "neighbor" : "");
	const pairs: Array<[string, string]> = [];
	if (nodeId) pairs.push(["Node", nodeId]);
	if (displayState) pairs.push(["State", displayState]);
	if (phase && phase !== state) pairs.push(["Phase", phase]);
	if (fingerprint) pairs.push(["Fingerprint", fingerprint]);
	if (url) pairs.push(["Address", url]);
	if (typeof row["localApproved"] === "boolean") pairs.push(["Local approval", row["localApproved"] ? "yes" : "no"]);
	if (typeof row["remoteApproved"] === "boolean") pairs.push(["Remote approval", row["remoteApproved"] ? "yes" : "no"]);
	if (lastSeen) pairs.push(["Last seen", lastSeen]);
	const actions: HTMLElement[] = [];
	if (!isNeighbor && pending && nodeId) {
		actions.push(
			h(
				"button",
				{
					class: "btn primary sm",
					type: "button",
					disabled: !fingerprint,
					title: fingerprint ? "Review the fingerprint and approve" : "The status did not include a fingerprint; use the manual approve form",
					onclick: () => {
						if (!fingerprint) return;
						approveTrust(nodeId, fingerprint, reload, botId);
					},
				},
				"Approve…",
			),
		);
		actions.push(
			h(
				"button",
				{ class: "btn danger sm", type: "button", onclick: () => revokeTrust(nodeId, fingerprint, reload, botId) },
				"Revoke…",
			),
		);
	} else if (!isNeighbor && trusted && nodeId) {
		actions.push(
			h("button", { class: "btn sm", type: "button", onclick: () => revokeTrust(nodeId, fingerprint, reload, botId) }, "Revoke…"),
		);
	}
	return h(
		"div",
		{ class: "card" },
		h(
			"div",
			{ class: "card-head" },
			h("span", { class: `dot ${pending ? "pend" : trusted ? "ok" : "none"}` }),
			h("span", { text: nodeId || "peer" }),
			displayState ? h("span", { class: "chip dim", text: displayState }) : null,
		),
		h("div", { class: "card-body" }, definitionList(pairs), actions.length ? h("div", { class: "panel-actions" }, ...actions) : null),
	);
}

function approveTrust(nodeId: string, fingerprint: string, reload: () => void, botId?: string): void {
	let localNode = currentSettings().nodeId || "(not set)";
	let localFingerprint = "(not reported)";
	const status = lastOnlyneStatus;
	if (isObj(status)) {
		const statusNode = status["node"];
		if (isObj(statusNode)) {
			localNode = firstString(statusNode, ["id", "nodeId"]) || localNode;
			localFingerprint = firstString(statusNode, ["fingerprint"]) || localFingerprint;
		}
		const statusTrust = status["trust"];
		if (isObj(statusTrust)) localFingerprint = firstString(statusTrust, ["localFingerprint"]) || localFingerprint;
	}
	confirmDialog({
		title: "Approve peer fingerprint",
		body: h(
			"div",
			{ class: "stack" },
			h("div", { class: "panel-note", text: `Approval applies to exactly these values${botId ? ` for ${botName(botId)}` : ""}.` }),
			definitionList([
				["Local node", localNode],
				["Local fingerprint", localFingerprint],
				["Peer node", nodeId],
				["Peer fingerprint", fingerprint],
			]),
		),
		confirmLabel: "Approve fingerprint",
		onConfirm: async () => {
			await callOnlyne("trust.approve", { nodeId, fingerprint }, botId);
			notify(`Trusted ${nodeId}.`, "ok");
			await refresh().catch(() => undefined);
			reload();
		},
	});
}

function revokeTrust(nodeId: string, fingerprint: string, reload: () => void, botId?: string): void {
	confirmDialog({
		title: "Revoke trust",
		body: h(
			"div",
			{ class: "stack" },
			h("div", { class: "panel-note", text: "This clears your approval for the peer." }),
			definitionList([
				["Peer node", nodeId || "(not set)"],
				["Fingerprint", fingerprint || "(not set)"],
			]),
		),
		confirmLabel: "Revoke",
		danger: true,
		onConfirm: async () => {
			await callOnlyne("trust.revoke", { nodeId }, botId);
			notify(`Revoked ${nodeId}.`, "ok");
			await refresh().catch(() => undefined);
			reload();
		},
	});
}

async function callOnlyne(action: string, args: Record<string, unknown>, botId?: string): Promise<unknown> {
	return command("onlyne.action", { action, args, ...(botId ? { botId } : {}) });
}

async function runOnlyne(
	action: string,
	args: Record<string, unknown>,
	result: HTMLElement,
	botId?: string,
	onDone?: () => void,
): Promise<void> {
	result.textContent = "";
	if (!action) {
		result.textContent = "An action name is required.";
		return;
	}
	try {
		const value = await callOnlyne(action, args, botId);
		result.textContent = value === undefined ? "done" : safeJson(value);
		notify(`${action} completed.`, "ok");
		await refresh().catch(() => undefined);
		onDone?.();
	} catch (error) {
		result.textContent = errText(error);
		notify(errText(error), "error");
	}
}

/* ------------------------------------------------------------------ */
/* activity panel                                                      */
/* ------------------------------------------------------------------ */

function activityPanel(): HTMLElement {
	const wrap = h("div", { class: "panel-inner" });
	wrap.append(
		h(
			"div",
			{ class: "card" },
			h("div", { class: "card-head", text: "Connection" }),
			h(
				"div",
				{ class: "card-body" },
				definitionList([
					["Transport", connection],
					["Node", currentSettings().nodeId || "(not set)"],
					["Conversations", String(state.conversations.length)],
					["Bots", String(state.bots.length)],
				]),
				h(
					"div",
					{ class: "panel-actions" },
					h(
						"button",
						{
							class: "btn",
							type: "button",
							onclick: () => {
								reconnectDelay = 1000;
								connectEvents();
							},
						},
						"Reconnect",
					),
					h("button", { class: "btn", type: "button", onclick: () => void refresh().catch((error) => notify(errText(error), "error")) }, "Refresh snapshot"),
				),
			),
		),
	);
	const errorsCard = h(
		"div",
		{ class: "card" },
		h(
			"div",
			{ class: "card-head" },
			h("span", { text: "Errors" }),
			h(
				"button",
				{
					class: "btn ghost sm",
					type: "button",
					onclick: () => {
						errors.length = 0;
						renderStatusbar();
						renderPanel();
					},
				},
				"Clear",
			),
		),
		h(
			"div",
			{ class: "card-body" },
			errors.length
				? h(
						"div",
						{ class: "stack" },
						...errors
							.slice()
							.reverse()
							.map((entry) => h("div", { class: "panel-note", text: `${clockTime(entry.at)} · ${entry.message}` })),
					)
				: h("div", { class: "panel-note", text: "No errors recorded in this window." }),
		),
	);
	wrap.append(errorsCard, renderFramesCard());
	return wrap;
}

function renderFramesCard(): HTMLElement {
	const scope = selectEl(
		[{ value: "all", label: "All conversations" }, ...state.conversations.map((conversation) => ({ value: conversation.id, label: conversation.title }))],
		frameScope,
	);
	const filter = textInput(frameFilter, { placeholder: "Filter frames" });
	const listBox = h("div", { class: "stack" });
	const count = h("span", { class: "chip dim" });
	const showAll = h("button", { class: "btn ghost sm", type: "button" }, frameShowAll ? "Show last 200" : "Show all");

	const collect = (): FrameEntry[] => {
		const entries = frameScope === "all" ? [...framesByConversation.values()].flat() : framesFor(frameScope);
		const needle = frameFilter.trim().toLowerCase();
		const filtered = needle
			? entries.filter((entry) => safeJson(entry.frame).toLowerCase().includes(needle) || entry.botId.toLowerCase().includes(needle))
			: entries;
		return filtered.slice().sort((a, b) => a.at - b.at);
	};

	const paint = (): void => {
		const entries = collect();
		count.textContent = `${entries.length} frame${entries.length === 1 ? "" : "s"}`;
		const visible = frameShowAll ? entries : entries.slice(-200);
		listBox.replaceChildren(
			...visible.map((entry) =>
				h(
					"details",
					{ class: "block tool" },
					h(
						"summary",
						{},
						h(
							"span",
							{ class: "block-head" },
							h("span", { class: "tool-name", text: frameLabel(entry.frame) }),
							h("span", { class: "tool-args", text: `${clockTime(entry.at)} · ${botName(entry.botId)}` }),
						),
					),
					h("div", { class: "block-body" }, h("pre", { text: safeJson(entry.frame) })),
				),
			),
		);
		if (!visible.length) listBox.append(h("div", { class: "panel-note", text: "No frames match." }));
	};

	scope.onchange = () => {
		frameScope = scope.value;
		paint();
	};
	filter.addEventListener("input", () => {
		frameFilter = filter.value;
		paint();
	});
	showAll.onclick = () => {
		frameShowAll = !frameShowAll;
		showAll.textContent = frameShowAll ? "Show last 200" : "Show all";
		paint();
	};
	paint();

	return h(
		"div",
		{ class: "card" },
		h(
			"div",
			{ class: "card-head" },
			h("span", { text: "Runtime frames" }),
			count,
			h(
				"button",
				{ class: "btn ghost sm", type: "button", onclick: () => void copyText(safeJson(collect().map((entry) => entry.frame)), "Frames copied") },
				"Copy",
			),
			h("button", { class: "btn ghost sm", type: "button", onclick: paint }, icon("refresh", 14), "Reload"),
		),
		h(
			"div",
			{ class: "card-body" },
			h("div", { class: "grid-2" }, field("Conversation", scope), field("Filter", filter)),
			h("div", { class: "panel-actions" }, showAll),
			listBox,
		),
	);
}

/* ------------------------------------------------------------------ */
/* command palette                                                     */
/* ------------------------------------------------------------------ */

interface PaletteAction {
	name: string;
	description: string;
	payload: () => Record<string, unknown>;
}

function paletteActions(): PaletteAction[] {
	const conversation = selectedConversation();
	const botId = conversation?.adminBotId || conversation?.botIds[0] || "";
	return [
		{ name: "bot.save", description: "Create or update a bot.", payload: () => ({ bot: {} }) },
		{ name: "bot.delete", description: "Delete a bot by id.", payload: () => ({ id: selectedBotId ?? "" }) },
		{ name: "conversation.create", description: "Create a conversation.", payload: () => ({ title: "", kind: "direct", botIds: [], adminBotId: "" }) },
		{ name: "conversation.update", description: "Update a conversation.", payload: () => ({ id: conversation?.id ?? "" }) },
		{ name: "message.send", description: "Send a user message.", payload: () => ({ conversationId: conversation?.id ?? "", text: "" }) },
		{ name: "runtime.command", description: "Send one raw OMP RPC command.", payload: () => ({ conversationId: conversation?.id ?? "", botId, command: { type: "get_state" } }) },
		{ name: "runtime.respond", description: "Answer an RPC request.", payload: () => ({ conversationId: conversation?.id ?? "", botId, response: { type: "extension_ui_response", id: "", cancelled: true } }) },
		{ name: "runtime.stop", description: "Stop a bot runtime.", payload: () => ({ conversationId: conversation?.id ?? "", botId }) },
		{ name: "runtime.restart", description: "Restart a bot runtime on its next message.", payload: () => ({ conversationId: conversation?.id ?? "", botId }) },
		{ name: "runtime.health", description: "Read runtime health and last errors.", payload: () => ({}) },
		{ name: "desktop.health", description: "Read application counts and runtime errors.", payload: () => ({}) },
		{ name: "runtime.models", description: "Read the runtime model catalog.", payload: () => ({ conversationId: conversation?.id ?? "", botId }) },
		{ name: "capability.save", description: "Create or update a capability.", payload: () => ({ capability: { name: "", kind: "mcp", config: {} } }) },
		{ name: "capability.delete", description: "Delete a capability.", payload: () => ({ id: "" }) },
		{ name: "settings.save", description: "Replace desktop settings.", payload: () => ({ settings: currentSettings() }) },
		{ name: "widget.respond", description: "Answer or cancel a widget view.", payload: () => ({ id: "", cancelled: true }) },
		{ name: "onlyne.status", description: "Read node and trust status.", payload: () => (botId ? { botId } : {}) },
		{
			name: "onlyne.action",
			description: "Run one Onlyne bridge action.",
			payload: () => ({ action: "connect", args: { url: "" }, ...(botId ? { botId } : {}) }),
		},
	];
}

const RPC_PRESETS: Array<{ label: string; frame: RpcFrame }> = [
	{ label: "get_state", frame: { type: "get_state" } },
	{ label: "models", frame: { type: "get_available_models" } },
	{ label: "thinking levels", frame: { type: "get_available_thinking_levels" } },
	{ label: "get_messages", frame: { type: "get_messages" } },
	{ label: "get_session_stats", frame: { type: "get_session_stats" } },
	{ label: "abort", frame: { type: "abort" } },
	{ label: "compact", frame: { type: "compact" } },
	{ label: "new_session", frame: { type: "new_session" } },
	{ label: "set_event_filter", frame: { type: "set_event_filter", events: null, messageUpdates: "full" } },
	{ label: "set_thinking_level", frame: { type: "set_thinking_level", level: "medium" } },
];

const RESPONSE_PRESETS: Array<{ label: string; frame: RpcFrame }> = [
	{ label: "cancel dialog", frame: { type: "extension_ui_response", id: "", cancelled: true } },
	{ label: "value response", frame: { type: "extension_ui_response", id: "", value: "" } },
];

function openShortcuts(): void {
	const rows: Array<[string, string]> = [
		[chord(MOD, "K"), "Command palette"],
		[chord(MOD, "\\"), "Dock the current panel, or close the dock"],
		["?", "Shortcut list"],
		[chord(MOD, "Shift+N"), "New conversation"],
		[chord(MOD, "Shift+C"), "Character and scene"],
		[chord(MOD, "Enter"), "Send the draft"],
		["G then C", "Conversations"],
		["G then I", "Inbox"],
		["G then P", "Neighbors"],
		["G then T", "Trust"],
		["G then D", "Threads"],
		["G then R", "Resources"],
		["G then H", "Health"],
		["G then X", "Integrations"],
		["G then M", "Team"],
		["G then S", "Settings"],
	];
	const body = h("div", { class: "shortcut-list" }, ...rows.map(([keys, label]) => h("div", { class: "shortcut-row" },
		h("kbd", { text: keys }),
		h("span", { text: label }))));
	const modal = openModal({ title: "Keyboard", body });
	modal.footer.append(h("button", { class: "btn", type: "button", onclick: () => modal.close() }, "Close"));
}

function openPalette(): void {
	const actions = paletteActions();
	const conversation = selectedConversation();
	let selectedIndex = 0;

	const search = textInput("", { placeholder: "Search actions", "data-autofocus": true });
	const list = h("div", { class: "palette-list" });

	const firstAction = actions[0];
	const actionName = textInput(firstAction?.name ?? "");
	const actionPayload = textArea("{}", { rows: "6" });
	const actionResult = h("pre", { class: "palette-result" });

	const botSelect = selectEl(
		(conversation?.botIds ?? []).map((id) => ({ value: id, label: botName(id) })),
		conversation?.adminBotId || conversation?.botIds[0] || "",
	);
	const rpcFrame = textArea(safeJson({ type: "get_state" }), { rows: "6" });
	const rpcResult = h("pre", { class: "palette-result" });
	const respondFrame = textArea(safeJson({ type: "extension_ui_response", id: "", cancelled: true }), { rows: "6" });
	const respondResult = h("pre", { class: "palette-result" });

	const actionTab = h("button", { class: "palette-tab on", type: "button" }, "Action");
	const rpcTab = h("button", { class: "palette-tab", type: "button" }, "RPC command");
	const respondTab = h("button", { class: "palette-tab", type: "button" }, "RPC respond");

	const actionPane = h(
		"div",
		{ class: "palette-form" },
		h("div", { class: "grid-2" }, field("Action", actionName, undefined), field("Bot", botSelect, "Used by runtime actions.")),
		field("Payload", actionPayload, "JSON object."),
		h(
			"div",
			{ class: "panel-actions" },
			actionButton("Execute", () => void runPaletteAction(actionName.value.trim(), actionPayload, actionResult), { class: "btn primary" }),
		),
		actionResult,
	);
	const rpcPane = h(
		"div",
		{ class: "palette-form", hidden: true },
		h("div", { class: "palette-presets" }, ...RPC_PRESETS.map((preset) => h("button", { class: "palette-tab", type: "button", onclick: () => (rpcFrame.value = safeJson(preset.frame)) }, preset.label))),
		field("Frame", rpcFrame, "Any OMP RPC command; an id is added when absent."),
		h(
			"div",
			{ class: "panel-actions" },
			actionButton("Send command", () => void runPaletteRpc(conversation?.id ?? "", botSelect.value, rpcFrame, rpcResult), { class: "btn primary" }),
		),
		rpcResult,
	);
	const respondPane = h(
		"div",
		{ class: "palette-form", hidden: true },
		h("div", { class: "palette-presets" }, ...RESPONSE_PRESETS.map((preset) => h("button", { class: "palette-tab", type: "button", onclick: () => (respondFrame.value = safeJson(preset.frame)) }, preset.label))),
		field("Response frame", respondFrame, "Posted through runtime.respond for the selected bot."),
		h(
			"div",
			{ class: "panel-actions" },
			actionButton("Send response", () => void runPaletteRespond(conversation?.id ?? "", botSelect.value, respondFrame, respondResult), { class: "btn primary" }),
		),
		respondResult,
	);

	const setMode = (next: "action" | "rpc" | "respond"): void => {
		for (const tab of [actionTab, rpcTab, respondTab]) tab.classList.remove("on");
		(next === "action" ? actionTab : next === "rpc" ? rpcTab : respondTab).classList.add("on");
		actionPane.hidden = next !== "action";
		rpcPane.hidden = next !== "rpc";
		respondPane.hidden = next !== "respond";
	};
	actionTab.onclick = () => setMode("action");
	rpcTab.onclick = () => setMode("rpc");
	respondTab.onclick = () => setMode("respond");

	let closePalette = (): void => undefined;
	type PaletteRow =
		| { kind: "jump"; title: string; description: string; kbd?: string; run: () => void }
		| { kind: "action"; action: PaletteAction };
	const paletteRows = (): PaletteRow[] => {
		const jumps: PaletteRow[] = [...BUILTIN_WORKSPACES, ...extraWorkspaces].map((spec) => ({
			kind: "jump",
			title: spec.title,
			description: spec.hint || "Open",
			...(spec.shortcut ? { kbd: `G ${spec.shortcut.toUpperCase()}` } : {}),
			run: () => { openWorkspace(spec); closePalette(); },
		}));
		for (const conversation of state.conversations) {
			jumps.push({
				kind: "jump",
				title: conversation.title,
				description: conversation.kind === "group" ? "Open this group" : "Open this conversation",
				run: () => {
					selectedConversationId = conversation.id;
					showChat({ focus: true });
					closePalette();
				},
			});
		}
		for (const panel of panels.values()) {
			if (panel.botId) continue;
			if (BUILTIN_WORKSPACES.some(item => item.panel === panel.id)) continue;
			jumps.push({
				kind: "jump",
				title: panel.title,
				description: "Library",
				run: () => { openPanel(panel.id); closePalette(); },
			});
		}
		jumps.push(
			{ kind: "jump", title: "Toggle theme", description: "Switch the dark and light themes", run: () => { void toggleTheme(); closePalette(); } },
			{ kind: "jump", title: layout.dock ? "Close dock" : "Dock this panel", description: "Pin a panel beside the stage. Undo is offered.", kbd: chord(MOD, "\\"), run: () => { toggleDock(); closePalette(); } },
			{ kind: "jump", title: "Keyboard shortcuts", description: "Show the shortcut list", kbd: "?", run: () => { closePalette(); openShortcuts(); } },
		);
		const commands = developerMode() ? actions.map((action) => ({ kind: "action" as const, action })) : [];
		return [...jumps, ...commands];
	};
	const matches = (row: PaletteRow, needle: string): boolean => {
		if (!needle) return true;
		if (row.kind === "jump") return row.title.toLowerCase().includes(needle) || row.description.toLowerCase().includes(needle);
		return row.action.name.toLowerCase().includes(needle) || row.action.description.toLowerCase().includes(needle);
	};
	const paintList = (): void => {
		const needle = search.value.trim().toLowerCase();
		list.replaceChildren();
		const filtered = paletteRows().filter((row) => matches(row, needle));
		if (!filtered.length) {
			list.append(h("div", { class: "panel-note", text: "Nothing matches." }));
			return;
		}
		if (selectedIndex >= filtered.length) selectedIndex = filtered.length - 1;
		filtered.forEach((row, index) => {
			const title = row.kind === "jump" ? row.title : row.action.name;
			const description = row.kind === "jump" ? row.description : row.action.description;
			list.append(h("button", {
				class: `palette-item${index === selectedIndex ? " on" : ""}`,
				type: "button",
				onclick: () => {
					selectedIndex = index;
					if (row.kind === "jump") {
						row.run();
						return;
					}
					actionName.value = row.action.name;
					actionPayload.value = safeJson(row.action.payload());
					setMode("action");
					paintList();
				},
			},
			h("div", { class: "palette-main" }, h("span", { text: title }), row.kind === "jump" && row.kbd ? h("kbd", { class: "palette-kbd", text: row.kbd }) : false),
			h("div", { class: "palette-desc", text: description })));
		});
	};
	search.addEventListener("input", () => {
		selectedIndex = 0;
		paintList();
	});
	search.addEventListener("keydown", (event) => {
		const needle = search.value.trim().toLowerCase();
		const filtered = paletteRows().filter((row) => matches(row, needle));
		if (event.key === "ArrowDown") {
			event.preventDefault();
			selectedIndex = Math.min(selectedIndex + 1, Math.max(filtered.length - 1, 0));
			paintList();
		} else if (event.key === "ArrowUp") {
			event.preventDefault();
			selectedIndex = Math.max(selectedIndex - 1, 0);
			paintList();
		} else if (event.key === "Enter") {
			const chosen = filtered[selectedIndex];
			if (!chosen) return;
			event.preventDefault();
			if (chosen.kind === "jump") {
				chosen.run();
				return;
			}
			actionName.value = chosen.action.name;
			actionPayload.value = safeJson(chosen.action.payload());
			setMode("action");
			paintList();
		}
	});
	paintList();

	const modal = openModal({
		title: "Jump",
		anchor: true,
		wide: false,
		body: h(
			"div",
			{ class: "palette-grid" },
			h("div", { class: "palette-left" }, h("div", { class: "palette-search" }, search), list),
			developerMode()
				? h("div", { class: "palette-right" }, h("div", { class: "palette-tabs" }, actionTab, rpcTab, respondTab), actionPane, rpcPane, respondPane)
				: h("p", { class: "palette-dev-note", text: "Raw commands stay in Settings → Developer mode." }),
		),
	});
	closePalette = () => modal.close();
	modal.footer.append(h("button", { class: "btn ghost", type: "button", onclick: () => modal.close() }, "Close"));
	if (!conversation) {
		actionResult.textContent = "No conversation is selected; runtime actions need a conversation and bot.";
	}
	window.requestAnimationFrame(() => search.focus());
}

async function runPaletteAction(action: string, payload: HTMLTextAreaElement, result: HTMLElement): Promise<void> {
	result.textContent = "";
	if (!action) {
		result.textContent = "An action name is required.";
		return;
	}
	try {
		const parsed = requireObject(parseJson(payload.value || "{}", "Payload"), "Payload");
		const value = await command(action, parsed);
		result.textContent = value === undefined ? "done" : safeJson(value);
		await refresh().catch(() => undefined);
	} catch (error) {
		result.textContent = errText(error);
	}
}

async function runPaletteRpc(conversationId: string, botId: string, frameInput: HTMLTextAreaElement, result: HTMLElement): Promise<void> {
	result.textContent = "";
	if (!conversationId || !botId) {
		result.textContent = "Select a conversation and bot first.";
		return;
	}
	try {
		const frame = requireObject(parseJson(frameInput.value || "{}", "Frame"), "Frame") as unknown as RpcFrame;
		if (typeof frame.type !== "string" || !frame.type) throw new Error("A frame needs a type.");
		if (typeof frame.id !== "string") frame.id = nextRpcId();
		const value = await command("runtime.command", { conversationId, botId, command: frame });
		result.textContent = value === undefined ? "done" : safeJson(value);
	} catch (error) {
		result.textContent = errText(error);
	}
}

async function runPaletteRespond(conversationId: string, botId: string, frameInput: HTMLTextAreaElement, result: HTMLElement): Promise<void> {
	result.textContent = "";
	if (!conversationId || !botId) {
		result.textContent = "Select a conversation and bot first.";
		return;
	}
	try {
		const frame = requireObject(parseJson(frameInput.value || "{}", "Response"), "Response") as unknown as RpcFrame;
		if (typeof frame.type !== "string" || !frame.type) throw new Error("A response needs a type.");
		const value = await command("runtime.respond", { conversationId, botId, response: frame });
		result.textContent = value === undefined ? "done" : safeJson(value);
	} catch (error) {
		result.textContent = errText(error);
	}
}

/* ------------------------------------------------------------------ */
/* notifications                                                       */
/* ------------------------------------------------------------------ */

function notify(message: string, level: Level = "info", action?: { label: string; run: () => void }): void {
	const text = message.trim() || "(empty message)";
	if (level === "error") {
		errors.push({ message: text, at: Date.now() });
		while (errors.length > 200) errors.shift();
		renderStatusbar();
		if (activePanel === "activity") renderPanel();
	}
	const existing = [...refs.toasts.querySelectorAll<HTMLElement>(".toast")].find(item => item.dataset["text"] === text && item.dataset["level"] === level);
	if (existing && action === undefined) {
		const count = Number(existing.dataset["count"] ?? "1") + 1;
		existing.dataset["count"] = String(count);
		const label = existing.querySelector(".toast-text");
		if (label) label.textContent = `${text} (${count})`;
		return;
	}
	const bar = h("span", { class: "toast-progress" });
	const toast = h(
		"div",
		{ class: `toast ${level}`, role: level === "error" ? "alert" : "status", "data-text": text, "data-level": level, "data-count": "1" },
		h("span", { class: "toast-text", text }),
		action === undefined ? false : h("button", { class: "btn sm", type: "button", onclick: () => { action.run(); toast.remove(); } }, action.label),
		h("button", { class: "toast-close", type: "button", "aria-label": "Dismiss notification", onclick: () => toast.remove() }, "Close"),
		bar,
	);
	let paused = false;
	let started = Date.now();
	const life = level === "error" ? 12_000 : 6_000;
	let remaining = life;
	const tick = (): void => {
		if (!toast.isConnected || paused) return;
		const left = remaining - (Date.now() - started);
		bar.style.transform = `scaleX(${Math.max(0, left / life)})`;
		if (left <= 0) toast.remove();
		else window.setTimeout(tick, 80);
	};
	toast.addEventListener("mouseenter", () => { paused = true; remaining -= Date.now() - started; });
	toast.addEventListener("mouseleave", () => { paused = false; started = Date.now(); tick(); });
	refs.toasts.append(toast);
	while (refs.toasts.children.length > 3) refs.toasts.firstElementChild?.remove();
	tick();
}

async function copyText(text: string, label: string): Promise<void> {
	try {
		await navigator.clipboard.writeText(text);
		notify(label, "ok");
	} catch {
		notify("The clipboard is unavailable.", "error");
	}
}

/* ------------------------------------------------------------------ */
/* renderer plugins                                                    */
/* ------------------------------------------------------------------ */

function bootstrapPlugins(): { plugins: string[]; present: boolean } {
	const tag = document.getElementById("onlyne-bootstrap");
	if (!tag) return { plugins: [], present: false };
	const text = tag.textContent?.trim() ?? "";
	if (!text) return { plugins: [], present: true };
	try {
		const parsed = JSON.parse(text) as { plugins?: unknown };
		const plugins = Array.isArray(parsed.plugins)
			? parsed.plugins.filter((item): item is string => typeof item === "string" && item.trim().length > 0)
			: [];
		return { plugins, present: true };
	} catch {
		notify("The host bootstrap block is not valid JSON.", "error");
		return { plugins: [], present: true };
	}
}

function pluginUrl(entry: string): string {
	if (/^(https?:)?\/\//.test(entry) || entry.startsWith("/") || entry.startsWith("./")) return entry;
	return `/plugins/${entry}.js`;
}

function loadScriptModule(url: string): Promise<void> {
	const { promise, resolve, reject } = Promise.withResolvers<void>();
	const script = document.createElement("script");
	script.type = "module";
	script.src = url;
	script.async = true;
	script.addEventListener("load", () => resolve());
	script.addEventListener("error", () => reject(new Error("the module failed to load")));
	document.head.append(script);
	return promise;
}

async function loadRendererPlugins(): Promise<void> {
	const bootstrap = bootstrapPlugins();
	const list = bootstrap.present ? bootstrap.plugins : currentSettings().rendererPlugins.map(pluginUrl);
	for (const url of list) {
		if (loadedPlugins.has(url)) continue;
		loadedPlugins.add(url);
		try {
			await loadScriptModule(url);
		} catch (error) {
			notify(`Renderer plugin failed: ${url} — ${errText(error)}`, "error");
		}
	}
	renderNavViews();
	if (activePanel) renderPanel();
	globalPluginsReady = true;
}

function scopedPluginUrl(entry: string, botId: string): string {
	const url = new URL(pluginUrl(entry), location.href);
	url.searchParams.set("onlyneBot", botId);
	return url.href;
}

async function runScopedPlugin(botId: string, url: string, app: DesktopUiApi): Promise<void> {
	const previous = window.onlyne;
	window.onlyne = createScopedUiApi(botId, app);
	try {
		await loadScriptModule(url);
	} catch (error) {
		notify(`Renderer plugin failed for ${botName(botId)}: ${url} — ${errText(error)}`, "error");
	} finally {
		window.onlyne = previous;
	}
}

/**
 * Loads the renderer plugins of the bots visible in the current conversation.
 * Each bot gets its own module instance (query key) and scoped registration
 * surface, so one bot's panels, message renderers, themes and widget
 * protocols never activate in another bot's conversation.
 */
function activateBotPlugins(): void {
	const app = window.onlyne;
	if (!app || !globalPluginsReady) return;
	for (const botId of uiActiveBotIds()) {
		const bot = botOf(botId);
		if (!bot) continue;
		const configured = botPlugins(bot).renderer;
		if (configured.length === 0) continue;
		const served = botPluginUrls.get(botId);
		if (served === undefined) {
			if (botPluginPending.has(botId)) continue;
			botPluginPending.add(botId);
			void app.command<{ paths?: string[] }>("renderer.plugins", { botId })
				.then(result => { botPluginUrls.set(botId, result.paths ?? []); })
				.catch(() => { botPluginUrls.set(botId, []); })
				.finally(() => { botPluginPending.delete(botId); activateBotPlugins(); });
			continue;
		}
		for (const entry of served) {
			const url = scopedPluginUrl(entry, botId);
			const key = `${botId}\u0000${url}`;
			if (loadedBotPlugins.has(key)) continue;
			loadedBotPlugins.add(key);
			pluginChain = pluginChain.then(() => runScopedPlugin(botId, url, app));
		}
	}
}

/* ------------------------------------------------------------------ */
/* UI api                                                              */
/* ------------------------------------------------------------------ */

function registerPanelEntry(
	botId: string | undefined,
	id: string,
	title: string,
	render: (snapshot: DesktopState) => HTMLElement,
): () => void {
	const internal = botId ? `${botId}:${id}` : id;
	const previous = panels.get(internal);
	panels.set(internal, { id: internal, title: botId ? `${title} · ${botName(botId)}` : title, render, botId });
	renderNavViews();
	if (activePanel === internal) renderPanel();
	return () => {
		const current = panels.get(internal);
		if (!current || current.render !== render) return;
		if (previous) panels.set(internal, previous);
		else panels.delete(internal);
		if (activePanel === internal && !panels.has(internal)) showChat();
		renderNavViews();
	};
}

function registerRendererEntry(
	botId: string | undefined,
	role: string,
	render: (message: unknown) => HTMLElement,
): () => void {
	const stack = messageRenderers.get(role) ?? [];
	const entry: MessageRendererEntry = { botId, render };
	stack.push(entry);
	messageRenderers.set(role, stack);
	messageSignature = "";
	renderMessages();
	return () => {
		const current = messageRenderers.get(role);
		if (!current) return;
		const index = current.indexOf(entry);
		if (index >= 0) current.splice(index, 1);
		if (!current.length) messageRenderers.delete(role);
		messageSignature = "";
		renderMessages();
	};
}

function registerThemeEntry(botId: string | undefined, id: string, css: string): () => void {
	const registration = botId ? `${botId}:${id}` : id;
	const style = document.createElement("style");
	style.dataset["onlyneTheme"] = registration;
	style.textContent = css;
	document.head.append(style);
	themeStyles.set(registration, {
		id,
		label: botId ? `${botName(botId)} · ${id}` : id,
		botId,
		style,
	});
	return () => {
		style.remove();
		themeStyles.delete(registration);
		if (themeSetting === id) applyTheme("dark");
	};
}

function currentBotId(): string | undefined {
	const conversation = selectedConversation();
	return conversation?.adminBotId || conversation?.botIds[0] || selectedBotId || undefined;
}

function activeContext(): WidgetContext | undefined {
	const conversation = selectedConversation();
	if (!conversation) return undefined;
	const botId = conversation.adminBotId || conversation.botIds[0];
	return botId ? { conversationId: conversation.id, botId } : undefined;
}

function createUiApi(): DesktopUiApi {
	return {
		command: <T,>(action: string, payload?: Record<string, unknown>) => command<T>(action, payload ?? {}),
		state: () => state,
		subscribe: (listener) => {
			listeners.add(listener);
			return () => {
				listeners.delete(listener);
			};
		},
		activeContext,
		widgets: registry,
		registerPanel: (id, title, render) => registerPanelEntry(undefined, id, title, render),
		registerWorkspace: (entry) => registerWorkspaceEntry(undefined, entry),
		layout: {
			get: () => layout,
			update: (change) => {
				layout = normalizeLayout(change(layout), catalogIds());
				persistLayout();
			},
			reset: () => resetLayout(),
		},
		registerMessageRenderer: (role, render) => registerRendererEntry(undefined, role, render),
		registerTheme: (id, css) => registerThemeEntry(undefined, id, css),
		setTheme: (id) => {
			if (id !== "system" && id !== "dark" && id !== "light" && !themeStyles.has(id)) {
				notify(`Unknown theme: ${id}`, "error");
				return;
			}
			applyTheme(id);
		},
	};
}

function registryForBot(botId: string): WidgetRegistry {
	let scoped = botWidgetRegistries.get(botId);
	if (!scoped) {
		scoped = createWidgetRegistry();
		botWidgetRegistries.set(botId, scoped);
	}
	return scoped;
}

/** Routes widget views to the registry of the bot that produced them. */
function createRoutedRegistry(): WidgetRegistry {
	return {
		register: (protocol, renderer) => globalRegistry.register(protocol, renderer),
		render: (view, context) => {
			const scoped = botWidgetRegistries.get(view.botId);
			if (scoped && scoped.protocols().includes(view.protocol)) return scoped.render(view, context);
			return globalRegistry.render(view, context);
		},
		protocols: () => [
			...new Set([...globalRegistry.protocols(), ...[...botWidgetRegistries.values()].flatMap((registry) => registry.protocols())]),
		],
	};
}

function createScopedUiApi(botId: string, app: DesktopUiApi): DesktopUiApi {
	return {
		command: app.command,
		state: app.state,
		subscribe: app.subscribe,
		activeContext: () => {
			const conversation = selectedConversation();
			if (!conversation || !conversation.botIds.includes(botId)) return undefined;
			return { conversationId: conversation.id, botId };
		},
		widgets: registryForBot(botId),
		registerPanel: (id, title, render) => registerPanelEntry(botId, id, title, render),
		registerWorkspace: (entry) => registerWorkspaceEntry(botId, entry),
		layout: {
			get: () => layout,
			update: (change) => {
				layout = normalizeLayout(change(layout), catalogIds());
				persistLayout();
			},
			reset: () => resetLayout(),
		},
		registerMessageRenderer: (role, render) => registerRendererEntry(botId, role, render),
		registerTheme: (id, css) => registerThemeEntry(botId, id, css),
		setTheme: (id) => {
			if (id === "system" || id === "dark" || id === "light") applyTheme(id);
			else if (themeStyles.has(`${botId}:${id}`)) applyTheme(id);
			else notify(`Unknown theme for this bot: ${id}`, "error");
		},
	};
}

/* ------------------------------------------------------------------ */
/* boot                                                                */
/* ------------------------------------------------------------------ */

async function hydrate(): Promise<void> {
	try {
		await refresh();
	} catch (error) {
		notify(`Cannot read desktop state: ${errText(error)}`, "error");
	}
	applyAppearance();
	await markDemo();
	await loadRendererPlugins();
	renderAll();
	if (location.hash.length > 2) restoreRoute();
	else writeRoute();
	syncHostSelection();
	activateBotPlugins();
}

function boot(): void {
	loadLayout();
	buildShell();
	wireStaticEvents();

	globalRegistry = createWidgetRegistry();
	registerBuiltinViews(globalRegistry);
	registry = createRoutedRegistry();
	host = createWidgetHost({
		dialogs: refs.dialogs,
		widgetsAbove: refs.widgetsAbove,
		widgetsBelow: refs.widgetsBelow,
		inlineViews: refs.inlineViews,
		status: refs.widgetStatus,
		registry,
		respond: async (context, response) => {
			await command("runtime.respond", {
				conversationId: context.conversationId,
				botId: context.botId,
				response,
			});
		},
		respondView: async (id, answer, cancelled) => {
			await command("widget.respond", { id, answer, cancelled });
			await refresh().catch(() => undefined);
		},
		notify: (message, level) =>
			notify(message, level === "error" || level === "warn" || level === "warning" || level === "ok" ? (level === "warning" ? "warn" : level) : "info"),
		setEditorText: (text) => {
			refs.composerInput.value = text;
			if (selectedConversationId) drafts.set(selectedConversationId, text);
			autoGrow();
			updateComposerState();
			refs.composerInput.focus();
		},
		setTitle: (title) => {
			document.title = title ? `${title} — Onlyne` : "Onlyne Desktop";
		},
		openUrl: (url) => {
			try {
				const parsed = new URL(url, location.href);
				if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
					notify("Only http and https links can be opened.", "warn");
					return;
				}
				window.open(parsed.href, "_blank", "noopener,noreferrer");
			} catch {
				notify("That link is not a valid URL.", "error");
			}
		},
	});

	window.onlyne = createUiApi();
	const disposeOmpPanels = registerOmpPanels(window.onlyne);
	const disposeCharacterPanel = registerCharacterPanel(window.onlyne);
	const disposeSurfaces = registerDeskSurfaces(window.onlyne, {
		notify: (message, level) => notify(message, level === "error" || level === "warn" || level === "ok" ? level : "info"),
		open: (id) => { if (id === "chat") showChat(); else openPanel(id); },
		createBot: () => openBotEditor(),
	});
	window.addEventListener("pagehide", () => { disposeOmpPanels(); disposeCharacterPanel(); disposeSurfaces(); }, { once: true });

	registerPanelInternal("node", "Node record", nodePanel);
	registerPanelInternal("capabilities", "Capabilities", capabilitiesPanel);
	registerPanelInternal("settings", "Settings", settingsPanel);
	registerPanelInternal("activity", "Activity", activityPanel);

	renderAll();
	connectEvents();
	void hydrate();
}

boot();
