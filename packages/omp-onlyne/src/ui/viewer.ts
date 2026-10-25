import type { MsgInput, SendOutcome } from "@onlyne/lib";
import {
	type Component,
	type DescribeContext,
	Editor,
	type NativeChild,
	type NativeNode,
	type NativeUiEvent,
	type Theme,
	type TspSpan,
	getEditorTheme,
	matchesKey,
	node,
	span,
	text as textNode,
} from "@oh-my-pi/pi-tui";
import { shortNodeId } from "../config.ts";
import { getRuntime } from "../runtime.ts";
import type { OnlyneStatus, ViewerDiscovery, ViewerMessage, ViewerSnapshot, ViewerThread } from "../types.ts";
import { ageText, clockText, fit, oneLine, shortAlias } from "./format.ts";

type Page = "queue" | "chat" | "discovery" | "contacts";

const PAGES: readonly { id: Page; label: string }[] = [
	{ id: "queue", label: "队列" },
	{ id: "chat", label: "对话" },
	{ id: "discovery", label: "发现" },
	{ id: "contacts", label: "联系人" },
];

const REGION_PREFIX = "urn:onlyne:region:";

/** A region URI is stored for stability; every label shows the short name the user typed. */
function regionLabel(region: string | undefined): string {
	if (region === undefined) return "未声明域";
	if (!region.startsWith(REGION_PREFIX)) return region;
	const name = region.slice(REGION_PREFIX.length);
	try {
		return decodeURIComponent(name);
	} catch {
		return name;
	}
}

function errorText(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}

function plainAuthor(author: string): string {
	const tail = author.split(":").pop();
	return tail === undefined || tail === "" ? shortNodeId(author) : tail;
}

type TrustState = "pending" | "trusted" | "revoked";

const STATE_LABEL: Record<TrustState, string> = {
	trusted: "已认证",
	pending: "申请中",
	revoked: "已撤销",
};

const STATE_TONE: Record<TrustState, string> = {
	trusted: "success",
	pending: "info",
	revoked: "error",
};

interface Peer {
	record: ViewerDiscovery;
	trust?: { state: TrustState; fingerprint: string; local_approved: boolean; remote_approved: boolean };
}

/** One declared domain with the peers that claim it; `id` is undefined for peers that declare none. */
interface DomainGroup {
	id?: string;
	label: string;
	peers: Peer[];
}

interface Invite {
	node_id: string;
	fingerprint: string;
	base_url: string;
}

/** A TSP line extent, for heights the terminal must never recompute from content. */
type LineExtent = `${number}lines`;

function linesOf(count: number): LineExtent {
	return `${Math.max(1, Math.floor(count))}lines` as LineExtent;
}

/**
 * Row budgets for the regions inside the sheet. The sheet's own size is the
 * terminal's (`lg`); these are caps only — a `min` would demand more rows than
 * the pane has and get the frame clipped. Content past a cap is named, never drawn.
 */
interface Geometry {
	body: number;
	messages: number;
	threads: number;
	members: number;
	pageRows: number;
}

/**
 * The Onlyne surface: a domain rail, the thread column with what waits for
 * confirmation, the message flow with its composer, and the member column with
 * the selected member's detail. Every operation is a click on a described node;
 * `draftText` is the one way message text enters, because a native editor
 * reports text through the pointer/keyboard edit events itself.
 */
export class OnlyneViewer implements Component {
	private snapshot: ViewerSnapshot | undefined;
	private status: OnlyneStatus | undefined;
	private loading = true;
	private error: string | undefined;
	private page: Page = "chat";
	/** The domain the rail highlights; undefined follows this node's own region. */
	private domainKey: string | undefined;
	private threadAlias: string | undefined;
	private memberKey: string | undefined;
	/** The recipient the composer names, else the open thread's own peer. */
	private target: string | undefined;
	/** The composer's text, mirrored from the editor so a rebuild keeps it. */
	private draft = "";
	private composer: Editor | undefined;
	private sending = false;
	private notice: { text: string; error: boolean } | undefined;
	private noticeTimer: Timer | undefined;
	private unsubscribe: (() => void) | undefined;
	private described: NativeNode | undefined;
	private describedKey: string | undefined;
	private rendered: { key: string; lines: readonly string[] } | undefined;
	private version = 0;
	private disposed = false;
	private generation = 0;
	focused = true;
	readonly nativeOverlay = { role: "onlyne.viewer", size: "lg" as const, head: "Onlyne" };

	/** The text the composer holds; a native `edit`/`undo` event updates it through `handleNativeEvent`. */
	get draftText(): string {
		return this.draft;
	}

	constructor(
		private readonly theme: Theme,
		private readonly requestRender: () => void,
		private readonly done: () => void,
		private readonly signal: AbortSignal,
		private readonly rows: () => number = () => 40,
	) {
		void this.reload();
		this.unsubscribe = getRuntime()?.onChange(() => {
			if (this.disposed || this.signal.aborted) return;
			void this.reload();
		});
	}

	private changed(): void {
		if (this.disposed || this.signal.aborted) return;
		this.version++;
		this.described = undefined;
		this.rendered = undefined;
		this.requestRender();
	}

	/**
	 * The sheet's height is the terminal's, so the regions inside it only need
	 * caps: what the chrome (status, tabs, gaps) leaves for the body.
	 */
	private geometry(): Geometry {
		const rows = Math.max(20, Math.floor(this.rows()));
		const body = Math.max(8, rows - 12);
		return {
			body,
			messages: Math.max(1, Math.floor((body - 10) / 5)),
			threads: Math.max(1, Math.floor((body - 9) / 3)),
			members: Math.max(1, Math.floor((body - 4) / 3)),
			pageRows: Math.max(2, body - 4),
		};
	}

	/** What the fixed frame shows of a list, and how many rows it left out. */
	private shown<T>(items: readonly T[], budget: number): { rows: readonly T[]; more: number } {
		if (items.length <= budget) return { rows: items, more: 0 };
		const keep = Math.max(1, budget - 1);
		return { rows: items.slice(0, keep), more: items.length - keep };
	}

	private moreText(count: number): NativeNode {
		return textNode([span(`…另有 ${count} 条未显示`, "muted")], { truncate: "end" });
	}

	async reload(): Promise<void> {
		if (this.disposed || this.signal.aborted) return;
		const generation = ++this.generation;
		const runtime = getRuntime();
		if (runtime === undefined) {
			this.snapshot = undefined;
			this.status = undefined;
			this.loading = false;
			this.error = "未配置 Onlyne。先运行 /onlyne init 填写项目配置。";
			this.changed();
			return;
		}
		this.loading = this.snapshot === undefined;
		try {
			const [snapshot, status] = await Promise.all([runtime.snapshot(), runtime.status()]);
			if (this.disposed || this.signal.aborted || generation !== this.generation) return;
			this.snapshot = snapshot;
			this.status = status;
			if (this.domainKey === undefined) this.domainKey = snapshot.region;
			this.error = undefined;
		} catch (error) {
			if (this.signal.aborted || generation !== this.generation) return;
			this.error = errorText(error);
		} finally {
			if (!this.disposed && !this.signal.aborted && generation === this.generation) {
				this.loading = false;
				this.changed();
			}
		}
	}

	private setNotice(text: string, error = false): void {
		this.notice = { text, error };
		clearTimeout(this.noticeTimer);
		this.noticeTimer = setTimeout(() => {
			this.noticeTimer = undefined;
			this.notice = undefined;
			this.changed();
		}, 8000);
		this.changed();
	}

	// ── data ────────────────────────────────────────────────────────────────

	private threads(): ViewerThread[] {
		const threads = this.snapshot?.threads ?? [];
		return [...threads].sort((left, right) => {
			const bySeq = Number(right.last_seq) - Number(left.last_seq);
			if (Number.isFinite(bySeq) && bySeq !== 0) return bySeq;
			return left.alias.localeCompare(right.alias);
		});
	}

	private selectedThread(): ViewerThread | undefined {
		const threads = this.threads();
		const named = threads.find(thread => thread.alias === this.threadAlias);
		if (named !== undefined) return named;
		// A message card's peer target names the conversation it belongs to.
		if (this.target !== undefined) {
			const live = threads.find(thread => thread.messages.some(message => message.author === this.target));
			if (live !== undefined) return live;
		}
		return threads[0];
	}

	/** Key requests the peer already approved: the ones waiting for this user. */
	private invites(): Invite[] {
		return (this.snapshot?.trust ?? [])
			.filter(entry => entry.state === "pending" && entry.remote_approved && !entry.local_approved)
			.map(entry => ({ node_id: entry.node_id, fingerprint: entry.fingerprint, base_url: entry.base_url }));
	}

	/** This node's trust row for a peer, undefined when the two never exchanged keys. */
	private trustOf(nodeId: string): Peer["trust"] {
		return this.snapshot?.trust?.find(entry => entry.node_id === nodeId);
	}

	/**
	 * Peers grouped by the domain they declare. Expired records stay in the
	 * list, flagged: this page is the one place a user can revoke or retry a
	 * node whose observation aged out.
	 */
	private domainGroups(): DomainGroup[] {
		const groups = new Map<string, DomainGroup>();
		for (const record of this.snapshot?.discovery ?? []) {
			if (record.node_id === this.status?.node_id) continue;
			const key = record.region ?? "";
			const group = groups.get(key) ?? { ...(record.region === undefined ? {} : { id: record.region }), label: regionLabel(record.region), peers: [] };
			groups.set(key, group);
			const trust = this.snapshot?.trust?.find(entry => entry.node_id === record.node_id);
			group.peers.push({
				record,
				...(trust === undefined
					? {}
					: { trust: { state: trust.state, fingerprint: trust.fingerprint, local_approved: trust.local_approved, remote_approved: trust.remote_approved } }),
			});
		}
		const own = this.snapshot?.region;
		for (const group of groups.values()) {
			group.peers.sort((left, right) => left.record.node_id.localeCompare(right.record.node_id));
		}
		return [...groups.values()].sort((left, right) => {
			if (left.id === own) return -1;
			if (right.id === own) return 1;
			return left.label.localeCompare(right.label);
		});
	}

	private currentDomain(): DomainGroup | undefined {
		const groups = this.domainGroups();
		if (this.domainKey !== undefined) {
			const picked = groups.find(group => group.id === this.domainKey);
			if (picked !== undefined) return picked;
		}
		return groups.find(group => group.id === this.snapshot?.region) ?? groups[0];
	}

	private peers(): Peer[] {
		return this.currentDomain()?.peers ?? [];
	}

	/** The thread's own peer: the author of its last inbound message, else its owner prefix. */
	private threadPeer(thread: ViewerThread | undefined): string | undefined {
		if (thread === undefined) return undefined;
		const inbound = [...thread.messages].reverse().find(message => message.direction === "in");
		return inbound?.author;
	}

	private sendBlocked(): string | undefined {
		if (this.error !== undefined) return this.error;
		if (this.draft.trim() === "") return "输入内容后发送";
		if (this.target === undefined && this.threadPeer(this.selectedThread()) === undefined) {
			return "先在线程栏选一位成员，或打开一个线程";
		}
		return undefined;
	}

	// ── recurring pieces ────────────────────────────────────────────────────

	private button(label: string, act: string, options: { tone?: "accent" | "error"; title?: string } = {}): NativeNode {
		return node(
			"row",
			{ role: "omp.btn", align: "center", actions: { click: act }, title: options.title ?? label, ...(options.tone === undefined ? {} : { tone: options.tone }) },
			[textNode([span(label, options.tone === undefined ? undefined : options.tone === "error" ? "error" : "strong")])],
			act,
		);
	}

	/** Peer chrome shared by every list: the trust dot, the short id, its domain and state. */
	private peerHead(peer: Peer): TspSpan[] {
		const state = peer.trust?.state;
		return [
			span("● ", state === undefined ? "muted" : STATE_TONE[state]),
			span(shortNodeId(peer.record.node_id), "strong"),
			span(` · ${regionLabel(peer.record.region)}`, "dim"),
			span(` · ${state === undefined ? "无认证" : STATE_LABEL[state]}`, state === undefined ? "dim" : STATE_TONE[state]),
		];
	}

	private memberCard(peer: Peer, withActions: boolean): NativeNode {
		const id = peer.record.node_id;
		const state = peer.trust?.state;
		const stale = Date.parse(peer.record.expires_at) <= Date.now();
		const waiting = peer.trust?.state === "pending" && peer.trust.remote_approved && !peer.trust.local_approved;
		const acts: NativeChild[] = [];
		if (withActions) {
			if (waiting) acts.push(this.button("待你确认", "onlyne.page.contacts", { tone: "accent", title: "到联系人页核对指纹并批准" }));
			else if (state !== "trusted") acts.push(this.button("请求交换密钥", `onlyne.request:${id}`, { title: "勾选并请求交换密钥" }));
			if (state === "trusted" || state === "pending") acts.push(this.button("撤销信任", `onlyne.revoke:${id}`, { tone: "error" }));
		}
		return node(
			"card",
			{
				role: "omp.btn",
				head: this.peerHead(peer),
				title: `${id}\n点击查看详情`,
				actions: { click: `onlyne.member:${id}` },
				selected: this.memberKey === id,
			},
			[
				textNode([span(peer.record.base_url, "dim"), span(` · 最后可见 ${ageText(peer.record.last_seen) || "未知"}`, "dim"), ...(stale ? [span(" · 记录已过期", "warning")] : []), ...(peer.record.error === undefined ? [] : [span(` · ${peer.record.error}`, "error")]),], { wrap: "word" }),
				...(acts.length === 0 ? [] : [node("row", { gap: "sm", align: "center" }, acts, "acts")]),
			],
			`peer-${id}`,
		);
	}

	// ── columns ─────────────────────────────────────────────────────────────

	/** The status strip: region, topology, live alerts and unread. Modules spread, never left-packed. */
	private statusRow(): NativeNode {
		const status = this.status;
		const alerts = status?.alerts ?? this.invites().length;
		const modules: Array<[string, string, string]> = [
			["域", regionLabel(status?.region), "strong"],
			["节点", status === undefined ? "…" : String(status.nodes), "strong"],
			["邻居", status === undefined ? "…" : String(status.neighbors), "strong"],
			["告警", String(alerts), alerts > 0 ? "strong warning" : "muted"],
			["未读", String(status?.unread ?? 0), (status?.unread ?? 0) > 0 ? "strong info" : "muted"],
		];
		return node(
			"row",
			{ role: "onlyne.status", gap: "md", justify: "between", align: "center" },
			modules.map(([label, value, tone], index) =>
				node(
					"col",
					{ gap: "none", min: { w: "8ch" } },
					[textNode([span(label, "dim")]), node("text", { spans: [span(value, tone)] })],
					`status-${index}`,
				),
			),
			"status",
		);
	}

	private railNode(): NativeNode {
		const groups = this.domainGroups();
		const current = this.currentDomain();
		const waiting = this.invites().length;
		const buttons: NativeChild[] = groups.map(group =>
			node(
				"row",
				{
					role: "omp.btn",
					align: "center",
					actions: { click: `onlyne.domain:${group.id ?? ""}` },
					title: group.label,
					tone: current?.id === group.id ? "accent" : undefined,
				},
				[textNode([span(group.label.slice(0, 4), current?.id === group.id ? "strong" : "muted")], { truncate: "end" })],
				`domain-${group.id ?? "none"}`,
			),
		);
		if (waiting > 0) {
			buttons.push(
				node(
					"row",
					{ role: "omp.btn", align: "center", actions: { click: "onlyne.page.contacts" }, title: `${waiting} 个密钥申请等你确认` },
					[node("badge", { text: String(waiting) }, undefined, "badge"), textNode([span("确认", "strong")])],
					"waiting",
				),
			);
		}
		return node(
			"col",
			{ role: "onlyne.rail", gap: "xs", align: "stretch", min: { w: "10ch" } },
			buttons.length === 0 ? [textNode([span("无域", "muted")])] : buttons,
			"rail",
		);
	}

	/** The page strip: one click per page, spread across the width. */
	private tabsNode(): NativeNode {
		return node(
			"tabs",
			{ items: PAGES.map(page => ({ id: page.id, label: page.label })), active: this.page },
			undefined,
			"tabs",
		);
	}

	/** The thread column: this node's header, the domain's peers, its threads, the inbox entry. */
	private threadColumn(): NativeNode {
		const domain = this.currentDomain();
		const peers = this.peers();
		const threads = this.threads();
		const geo = this.geometry();
		const unread = threads.reduce((total, thread) => total + Math.max(0, thread.unread), 0);
		const fingerprint = this.snapshot?.identity?.fingerprint;
		const memberList = this.shown(peers, Math.min(3, geo.members));
		const threadList = this.shown(threads, geo.threads);
		return node(
			"col",
			{
				role: "onlyne.threads",
				gap: "sm",
				min: { w: "24ch" },
				max: { w: "38ch", h: linesOf(geo.body) },
			},
			[
				node(
					"section",
					{ head: [span(domain?.label ?? regionLabel(this.snapshot?.region), "strong")] },
					[
						textNode([span(shortNodeId(this.status?.node_id ?? ""), "strong")], { wrap: "word" }),
						textNode([span(fingerprint ?? "无节点密钥", "dim")], { wrap: "word" }),
					],
					"identity",
				),
				node(
					"section",
					{ head: [span(`成员 · ${peers.length}`, "dim")] },
					peers.length === 0
						? [textNode([span("这个域还没有可达节点。到「发现」页看看候选。", "muted")], { wrap: "word" })]
						: [
								...memberList.rows.map(peer => this.memberCard(peer, false)),
								...(memberList.more === 0 ? [] : [this.moreText(memberList.more)]),
							],
					"members",
				),
				node(
					"section",
					{ head: [span(`线程 · ${threads.length}`, "dim")] },
					threads.length === 0
						? [textNode([span("还没有线程。", "muted")], { wrap: "word" })]
						: [
								...threadList.rows.map(thread =>
									node(
										"card",
										{
											role: "omp.btn",
											head: [
												span(`# ${thread.title}`, "strong"),
												...(thread.unread > 0 ? [span(` · ${thread.unread} 未读`, "info")] : []),
												...(thread.state === "closed" ? [span(" · 已关闭", "dim")] : []),
											],
											title: `${thread.title} · ${shortAlias(thread.alias)}`,
											actions: { click: `onlyne.thread:${thread.alias}` },
											selected: this.selectedThread()?.alias === thread.alias,
										},
										[textNode([span(`${thread.messages.length} 条消息`, "dim")], { truncate: "end" })],
										`thread-${thread.alias}`,
									),
								),
								...(threadList.more === 0 ? [] : [this.moreText(threadList.more)]),
							],
					"list",
				),
				node(
					"row",
					{ role: "omp.btn", align: "center", gap: "sm", actions: { click: "onlyne.page.queue" }, title: "打开待处理队列" },
					[textNode([span("✉ 收件箱", "muted")]), textNode([span(`${unread} 未读`, unread > 0 ? "info" : "dim")])],
					"inbox",
				),
			],
			"threads",
		);
	}

	/**
	 * The flow shows one line pointing at the approval page. Approving is a
	 * fingerprint comparison, and it has exactly one surface: the contacts page.
	 */
	private approvalsCard(): NativeChild[] {
		const invites = this.invites();
		if (invites.length === 0) return [];
		return [
			node(
				"card",
				{
					role: "omp.btn",
					tone: "warning",
					head: [span(`${invites.length} 个密钥申请等你确认`, "strong")],
					title: "到联系人页核对指纹并批准",
					actions: { click: "onlyne.page.contacts" },
				},
				[textNode([span(invites.map(invite => shortNodeId(invite.node_id)).join("、"), "dim")], { truncate: "end" })],
				"approvals",
			),
		];
	}

	private messageCard(message: ViewerMessage, waiting: boolean): NativeNode {
		const out = message.direction === "out";
		return node(
			"card",
			{
				role: "omp.btn",
				head: [
					span(out ? "→ " : "← ", out ? "accent" : "dim"),
					span(plainAuthor(message.author), "strong"),
					span(` ${clockText(message.at) || "—"} `, "dim"),
					span(out ? "已送达" : waiting ? "等你回复" : "已读", out ? "success" : waiting ? "info" : "success"),
					...(message.relation === undefined ? [] : [span(` · ${message.relation}`, "dim")]),
				],
				title: `#${message.seq} · 点击以此为发送目标`,
				actions: { click: `onlyne.message:${message.seq}` },
				selected: this.target !== undefined && this.target === message.author,
			},
			[textNode(message.text, { wrap: "word", lines: 3, truncate: "end" })],
			`message-${message.seq}`,
		);
	}

	/** The message flow: the head, the messages, then the composer. */
	private flowNode(): NativeNode {
		const thread = this.selectedThread();
		const invites = this.invites();
		const geo = this.geometry();
		if (thread === undefined) {
			return node(
				"col",
				{ role: "onlyne.flow", gap: "sm", grow: 1, min: { w: 0 }, max: { h: linesOf(geo.body) } },
				[
					...this.approvalsCard(),
					textNode([span("从左边打开一个线程，或在「发现」页连接一位节点发起对话。", "muted")], { wrap: "word", lines: 3 }),
					this.composerNode(),
				],
				"flow",
			);
		}
		const messages = thread.messages;
		const peer = this.threadPeer(thread);
		const last = messages.at(-1);
		// The newest messages stay in view and each body is clamped: an older history is
		// named rather than drawn, so no amount of content can push the composer off.
		const visible = messages.slice(-geo.messages);
		return node(
			"col",
			{ role: "onlyne.flow", gap: "sm", grow: 1, min: { w: 0 }, max: { h: linesOf(geo.body) } },
			[
				node("row", { role: "onlyne.thread-head", gap: "md", align: "center", justify: "between" }, [
					node("col", { gap: "none", grow: 1 }, [
						textNode([span(`# ${thread.title}`, "strong"), ...(thread.state === "closed" ? [span(" · 已关闭", "dim")] : [])], { truncate: "end" }),
						textNode([span(`${messages.length} 条消息 · ${this.peers().length} 位成员${last === undefined ? "" : ` · 最近 ${ageText(last.at) || "未知"}前`}`, "dim")], { truncate: "end" }),
					]),
					...(peer === undefined ? [] : [this.button("详情", `onlyne.member:${peer}`, { title: "这位对端的详情" })]),
				], "thread-head"),
				...this.approvalsCard(),
				node(
					"col",
					{ gap: "sm", grow: 1, shrink: 1, min: { w: 0 }, max: { h: linesOf(geo.body - 6) } },
					messages.length === 0
						? [textNode([span("这个线程还没有消息。", "muted")], { wrap: "word" })]
						: [
								...(visible.length === messages.length ? [] : [textNode([span(`更早的 ${messages.length - visible.length} 条不在这一屏`, "muted")], { truncate: "end" })]),
								...visible.map((message, index) =>
									this.messageCard(
										message,
										index === visible.length - 1 && !invites.some(invite => invite.node_id === message.author),
									),
								),
							],
					"messages",
				),
				this.composerNode(),
			],
			"flow",
		);
	}

	private composerNode(): NativeNode {
		const thread = this.selectedThread();
		const target = this.target ?? this.threadPeer(thread);
		const blocked = this.sendBlocked();
		if (this.composer === undefined) {
			const editor = new Editor(getEditorTheme());
			editor.disableSubmit = true;
			editor.setText(this.draft);
			editor.setMaxHeight(3);
			this.composer = editor;
		}
		this.composer.focused = this.focused;
		return node(
			"card",
			{ role: "onlyne.composer", inset: true },
			[
				this.composer,
				node("row", { gap: "md", align: "center", justify: "between" }, [
					textNode([span( target === undefined ? "先选择收件人：线程栏里的一位成员，或一个线程" : `发送到 ${shortNodeId(target)}${thread === undefined ? "" : ` · 线程 ${thread.title}`}`, blocked === undefined ? "dim" : "warning", ),], { wrap: "word", grow: 1 }),
					...(this.target === undefined ? [] : [this.button("改回线程对端", "onlyne.target:", { title: "清除手动指定的收件人" })]),
					node(
						"row",
						{
							role: "omp.btn",
							align: "center",
							actions: { click: "onlyne.send" },
							tone: blocked === undefined ? "accent" : undefined,
							title: blocked ?? "发送这条消息",
						},
						[textNode([span(this.sending ? "发送中…" : "发送", blocked === undefined ? "strong" : "muted")])],
						"send",
					),
				], "row"),
			],
			"composer",
		);
	}

	private memberColumn(): NativeNode {
		const peers = this.peers();
		const geo = this.geometry();
		const roster = this.shown(peers, geo.members);
		const children: NativeChild[] = [
			node(
				"section",
				{ head: [span(`成员 · ${peers.length}`, "dim")] },
				peers.length === 0
					? [textNode([span("这个域还没有可达节点。", "muted")], { wrap: "word" })]
					: [
							...roster.rows.map(peer => this.memberCard(peer, false)),
							...(roster.more === 0 ? [] : [this.moreText(roster.more)]),
						],
				"roster",
			),
		];
		const peer = peers.find(entry => entry.record.node_id === this.memberKey);
		if (peer !== undefined) {
			const state = peer.trust?.state;
			const waiting = peer.trust?.state === "pending" && peer.trust.remote_approved && !peer.trust.local_approved;
			children.push(
				node(
					"card",
					{ role: "onlyne.member-detail", head: [span(shortNodeId(peer.record.node_id), "strong"), ...(state === undefined ? [] : [span(` · ${STATE_LABEL[state]}`, STATE_TONE[state])])] },
					[
						node("kv", {
							layout: "grid",
							items: [
								{ k: "指纹", v: peer.trust?.fingerprint ?? "未交换" },
								{ k: "域", v: regionLabel(peer.record.region) },
								{ k: "状态", v: state === undefined ? "无认证" : STATE_LABEL[state] },
								{ k: "端点", v: peer.record.base_url },
								{ k: "最后可见", v: ageText(peer.record.last_seen) || "未知" },
							],
						}),
						node("row", { gap: "sm", align: "center", wrap: true }, [
							this.button("用作发送目标", `onlyne.target:${peer.record.node_id}`, { title: "把消息发给这位节点" }),
							...(waiting ? [this.button("同意交换", `onlyne.accept:${peer.record.node_id}`, { tone: "accent" })] : []),
							...(state === "trusted" || state === "pending" ? [this.button("撤销信任", `onlyne.revoke:${peer.record.node_id}`, { tone: "error" })] : []),
						], "acts"),
					],
					"member-detail",
				),
			);
		}
		return node(
			"col",
			{
				role: "onlyne.members",
				gap: "sm",
				min: { w: "22ch" },
				max: { w: "32ch", h: linesOf(geo.body) },
			},
			children,
			"members",
		);
	}

	// ── pages ───────────────────────────────────────────────────────────────

	private queuePage(): NativeNode[] {
		const queue = this.snapshot?.queue ?? [];
		const geo = this.geometry();
		const sections: NativeChild[] = [];
		const sectionsOf = 4;
		const budget = Math.max(1, Math.floor(geo.pageRows / sectionsOf));
		for (const [head, kind] of [["来信", "inbound"], ["出站", "outbound"], ["本地上下文", "context"], ["判定", "decision"]] as const) {
			const rows = queue.filter(item => item.kind === kind);
			const page = this.shown(rows, budget);
			sections.push(
				node(
					"section",
					{ head: [span(`${head} · ${rows.length}`, "dim")] },
					rows.length === 0
						? [textNode([span("空。", "muted")])]
						: [
								...page.rows.map(item =>
									node(
										"card",
										{ role: "onlyne.queue-row", head: [span(item.label, "strong"), ...(item.at === undefined ? [] : [span(` · ${ageText(item.at)}前`, "dim")])] },
										[textNode(item.detail, { wrap: "word" })],
										item.id,
									),
								),
								...(page.more === 0 ? [] : [this.moreText(page.more)]),
							],
					head,
				),
			);
		}
		return [node("col", { gap: "sm", max: { h: linesOf(geo.body) } }, sections, "queue")];
	}

	private discoveryPage(): NativeNode[] {
		const groups = this.domainGroups();
		const geo = this.geometry();
		const children: NativeChild[] = [
			node("row", { gap: "sm", align: "center", justify: "between" }, [
				textNode([span("按域分组的可达候选。交换密钥从这里发起；同域的节点在发现之后会自动发起一次。", "dim")], { wrap: "word", grow: 1 }),
				this.button("刷新发现", "onlyne.discovery.refresh", { title: "重新观察一轮发现" }),
			]),
		];
		if (this.snapshot?.discovery_error !== undefined) {
			children.push(node("text", { spans: [span(`发现失败：${this.snapshot.discovery_error}`, "error")], wrap: "word" }, undefined, "discovery-error"));
		}
		if (groups.length === 0) {
			children.push(textNode([span("还没发现任何节点。确认对方已经启动，并和你在同一网络上。", "muted")], { wrap: "word" }));
		}
		const budget = Math.max(2, Math.floor(geo.pageRows / Math.max(1, groups.length)));
		for (const group of groups) {
			const page = this.shown(group.peers, budget);
			children.push(
				node(
					"section",
					{ head: [span(group.label, "strong"), span(` · ${group.peers.length} 节点`, "dim")] },
					[
						...page.rows.map(peer => this.memberCard(peer, true)),
						...(page.more === 0 ? [] : [this.moreText(page.more)]),
					],
					`domain-${group.id ?? "none"}`,
				),
			);
		}
		return [node("col", { gap: "sm", max: { h: linesOf(geo.body) } }, children, "discovery")];
	}

	/**
	 * Two lists and nothing else: what waits for this node's approval, then the
	 * peers whose keys were exchanged both ways. Requesting an exchange is a
	 * discovery-page job; here the only act is approving or revoking.
	 */
	private contactsPage(): NativeNode[] {
		const geo = this.geometry();
		const invites = this.invites();
		const mine = this.snapshot?.identity?.fingerprint ?? "本节点没有密钥";
		const trusted = (this.snapshot?.contacts ?? []).filter(contact => this.trustOf(contact.node_id)?.state === "trusted");
		const approvalList = this.shown(invites, Math.max(1, geo.pageRows >> 2));
		const contactList = this.shown(trusted, Math.max(1, geo.pageRows >> 1));
		const children: NativeChild[] = [
			textNode([span("上面是等你批准的申请：核对双方指纹，一致再同意。下面是双方都已批准、可以直接发消息的节点。", "dim")], { wrap: "word" }),
			node(
				"section",
				{ head: [span(`待确认 · ${invites.length}`, invites.length === 0 ? "dim" : "strong")] },
				invites.length === 0
					? [textNode([span("没有等你确认的申请。", "muted")], { wrap: "word" })]
					: [
							...approvalList.rows.map(invite =>
								node(
									"card",
									{ role: "onlyne.approval", tone: "warning", head: [span(`${shortNodeId(invite.node_id)} 请求与你交换密钥`, "strong")] },
									[
										node("kv", {
											layout: "grid",
											items: [
												{ k: "对方指纹", v: invite.fingerprint },
												{ k: "本机指纹", v: mine },
												{ k: "端点", v: invite.base_url },
											],
										}),
										node("row", { gap: "sm", align: "center" }, [
											this.button("同意", `onlyne.accept:${invite.node_id}`, { tone: "accent", title: "双方指纹一致，批准这次交换" }),
											this.button("拒绝", `onlyne.reject:${invite.node_id}`, { title: "不批准这次申请" }),
										], "acts"),
									],
									`approval-${invite.node_id}`,
								),
							),
							...(approvalList.more === 0 ? [] : [this.moreText(approvalList.more)]),
						],
				"approvals",
			),
			node(
				"section",
				{ head: [span(`已认证 · ${trusted.length}`, "dim")] },
				trusted.length === 0
					? [textNode([span("还没有已认证的联系人。到「发现」页找到对方，请求交换密钥。", "muted")], { wrap: "word" })]
					: [
							...contactList.rows.map(contact =>
								node(
									"card",
									{
										role: "omp.btn",
										head: [
											span("● ", "success"),
											span(shortNodeId(contact.node_id), "strong"),
											span(` · ${regionLabel(contact.region)}`, "dim"),
											span(" · 已认证", "success"),
										],
										actions: { click: `onlyne.member:${contact.node_id}` },
										title: contact.node_id,
									},
									[
										textNode([span(`${contact.base_url} · 最后可见 ${ageText(contact.last_seen) || "未知"}`, "dim")], { wrap: "word" }),
										node("row", { gap: "sm", align: "center" }, [
											this.button("用作发送目标", `onlyne.target:${contact.node_id}`),
											this.button("撤销信任", `onlyne.revoke:${contact.node_id}`, { tone: "error" }),
										], "acts"),
									],
									`contact-${contact.node_id}`,
								),
							),
							...(contactList.more === 0 ? [] : [this.moreText(contactList.more)]),
						],
				"trusted",
			),
		];
		return [node("col", { gap: "sm", max: { h: linesOf(geo.body) } }, children, "contacts")];
	}

	describe(cx: DescribeContext): NativeNode {
		const geo = this.geometry();
		const key = `${this.version}:${cx.cols}:${this.rows()}:${this.page}`;
		if (this.described !== undefined && this.describedKey === key) return this.described;
		// Column widths are constraints, never arithmetic on the terminal: the flow takes what the rest leaves.
		const row = { gap: "md", align: "stretch", grow: 1, max: { h: linesOf(geo.body) } } as const;
		const body: NativeChild[] =
			this.page === "chat"
				? [node("row", row, [this.threadColumn(), this.flowNode(), this.memberColumn()], "columns")]
				: [
						node(
							"row",
							row,
							[
								this.railNode(),
								node("col", { gap: "sm", grow: 1, min: { w: 0 } }, [
									...(this.page === "queue" ? this.queuePage() : this.page === "discovery" ? this.discoveryPage() : this.contactsPage()),
								], "page"),
							],
							"columns",
						),
					];
		const children: NativeChild[] = [this.statusRow(), this.tabsNode()];
		if (this.notice !== undefined) {
			children.push(node("text", { spans: [span(this.notice.text, this.notice.error ? "error" : "info")], wrap: "word" }, undefined, "notice"));
		}
		if (this.error !== undefined) {
			children.push(node("text", { spans: [span(this.error, "error")], wrap: "word" }, undefined, "error"));
			children.push(this.button("重新读取", "onlyne.reload"));
		}
		if (this.loading) children.push(node("spinner", { label: [span("读取 Onlyne 状态…", "muted")] }, undefined, "loading"));
		children.push(...body);
		this.described = node("card", { role: "onlyne.viewer" }, children, "root");
		this.describedKey = key;
		return this.described;
	}

	// ── events ──────────────────────────────────────────────────────────────

	handleNativeEvent(event: NativeUiEvent): void {
		if (this.disposed || this.signal.aborted) return;
		if (event.type === "edit" || event.type === "undo") {
			this.composer?.handleNativeEvent(event);
			this.draft = this.composer?.getExpandedText() ?? this.draft;
			this.described = undefined;
			this.requestRender();
			return;
		}
		if ((event.type === "select" || event.type === "activate") && event.key === "tabs") {
			const page = PAGES.find(entry => entry.id === event.item);
			if (page !== undefined && page.id !== this.page) {
				this.page = page.id;
				this.changed();
			}
			return;
		}
		if (event.type !== "action") return;
		const act = event.act;
		if (act === "onlyne.close") {
			this.done();
			return;
		}
		if (act === "onlyne.reload") {
			void this.reload();
			return;
		}
		if (act === "onlyne.send") {
			void this.send();
			return;
		}
		if (act === "onlyne.discovery.refresh") {
			void this.refreshDiscovery();
			return;
		}
		if (act.startsWith("onlyne.page.")) {
			const id = act.slice("onlyne.page.".length) as Page;
			if (PAGES.some(page => page.id === id)) {
				this.page = id;
				this.changed();
			}
			return;
		}
		if (act.startsWith("onlyne.domain:")) {
			this.domainKey = act.slice("onlyne.domain:".length) || undefined;
			this.memberKey = undefined;
			this.changed();
			return;
		}
		if (act.startsWith("onlyne.thread:")) {
			const alias = act.slice("onlyne.thread:".length);
			if (this.threads().some(thread => thread.alias === alias)) {
				this.threadAlias = alias;
				this.target = undefined;
				this.page = "chat";
				this.changed();
			}
			return;
		}
		if (act.startsWith("onlyne.member:")) {
			const id = act.slice("onlyne.member:".length);
			this.memberKey = this.memberKey === id ? undefined : id;
			this.changed();
			return;
		}
		if (act.startsWith("onlyne.target:")) {
			const id = act.slice("onlyne.target:".length);
			this.target = id === "" || this.target === id ? undefined : id;
			this.changed();
			return;
		}
		if (act.startsWith("onlyne.message:")) {
			const seq = act.slice("onlyne.message:".length);
			const message = this.selectedThread()?.messages.find(entry => entry.seq === seq);
			if (message !== undefined) {
				this.target = message.author;
				this.changed();
			}
			return;
		}
		if (act.startsWith("onlyne.accept:")) {
			void this.accept(act.slice("onlyne.accept:".length));
			return;
		}
		if (act.startsWith("onlyne.reject:")) {
			// The runtime's only refusal is a revocation, which leaves the peer able to ask again.
			void this.revoke(act.slice("onlyne.reject:".length), "拒绝");
		}
		if (act.startsWith("onlyne.revoke:")) {
			void this.revoke(act.slice("onlyne.revoke:".length));
			return;
		}
		if (act.startsWith("onlyne.request:")) {
			void this.request(act.slice("onlyne.request:".length));
		}
	}

	/** Escape closes the overlay; every other key belongs to the composer, which the terminal drives by pointer. */
	handleInput(data: string): void {
		if (this.disposed || this.signal.aborted) return;
		if (matchesKey(data, "escape")) {
			this.done();
			return;
		}
		if (matchesKey(data, "ctrl+enter")) {
			void this.send();
			return;
		}
		if (this.composer === undefined) return;
		this.composer.handleInput(data);
		this.draft = this.composer.getExpandedText();
		this.described = undefined;
	}

	private async refreshDiscovery(): Promise<void> {
		const runtime = getRuntime();
		if (runtime === undefined) return;
		this.setNotice("正在刷新发现…");
		try {
			await runtime.refreshDiscovery();
			this.setNotice("发现已刷新。");
		} catch (error) {
			this.setNotice(`刷新失败：${errorText(error)}`, true);
		}
		await this.reload();
	}

	private async accept(nodeId: string): Promise<void> {
		const runtime = getRuntime();
		const invite = this.invites().find(entry => entry.node_id === nodeId);
		if (runtime === undefined || invite === undefined) return;
		try {
			await runtime.trustAccept(nodeId, invite.fingerprint);
			this.setNotice(`已批准 ${shortNodeId(nodeId)} 的密钥。`);
		} catch (error) {
			this.setNotice(`批准失败：${errorText(error)}`, true);
		}
		await this.reload();
	}

	private async revoke(nodeId: string, verb = "撤销"): Promise<void> {
		const runtime = getRuntime();
		if (runtime === undefined) return;
		try {
			await runtime.trustRevoke(nodeId);
			this.setNotice(
				verb === "拒绝"
					? `已拒绝 ${shortNodeId(nodeId)} 的这次申请；对方再次申请时会重新出现。`
					: `已撤销 ${shortNodeId(nodeId)} 的信任。`,
			);
		} catch (error) {
			this.setNotice(`${verb}失败：${errorText(error)}`, true);
		}
		await this.reload();
	}

	/** Ask a peer for a key exchange; the domain's automatic pass does this on its own for same-domain nodes. */
	private async request(nodeId: string): Promise<void> {
		const runtime = getRuntime();
		if (runtime === undefined) return;
		try {
			await runtime.trustInspect(nodeId);
			this.setNotice(`已向 ${shortNodeId(nodeId)} 请求交换密钥；对方批准后这里会出现确认。`);
		} catch (error) {
			this.setNotice(`请求失败：${errorText(error)}`, true);
		}
		await this.reload();
	}

	/**
	 * The composer sends down the one message path the tool uses: the node's own
	 * client with this session as the consumption context, so a click and a `msg`
	 * call leave the same records.
	 */
	private async send(): Promise<void> {
		if (this.sending) return;
		const runtime = getRuntime();
		const blocked = this.sendBlocked();
		if (blocked !== undefined) {
			this.setNotice(blocked, true);
			return;
		}
		if (runtime === undefined) return;
		const thread = this.selectedThread();
		const target = this.target ?? this.threadPeer(thread);
		const meta: Record<string, string> = {};
		if (target !== undefined) meta.to = target;
		const input: MsgInput = {
			delivery_kind: thread === undefined
				? `给 ${shortNodeId(target ?? "")} 发一条消息`
				: `在 ${thread.title} 里回一条消息`,
			msg: this.draft.trim(),
			...(Object.keys(meta).length === 0 ? {} : { meta }),
		};
		this.sending = true;
		this.changed();
		try {
			const outcome = (await runtime.node.client.msg(input, { context: runtime.mainAgentId })) as SendOutcome;
			this.setNotice(this.describeOutcome(outcome), outcome.admission === "refused" || outcome.error !== undefined);
			if (outcome.admission === "accepted" || outcome.admission === "unknown") {
				this.draft = "";
				this.composer?.setText("");
			}
		} catch (error) {
			this.setNotice(`发送失败：${errorText(error)}`, true);
		} finally {
			this.sending = false;
			await this.reload();
		}
	}

	private describeOutcome(outcome: SendOutcome): string {
		if (outcome.admission === "refused") return `未送达：${outcome.error?.message ?? "对方拒绝"}`;
		const reply = outcome.reply?.message.msg;
		if (reply !== undefined) return `已送达，对方回了：${oneLine(reply) || "（空）"}`;
		if (outcome.admission === "accepted") return "已送达。";
		return "投递结果未知，会在重试窗口内继续。";
	}

	/** The ANSI fallback for a terminal without the native surface: a listing, no interaction. */
	render(width: number): readonly string[] {
		const key = `${this.version}:${width}`;
		if (this.rendered !== undefined && this.rendered.key === key) return this.rendered.lines;
		const status = this.status;
		const lines: string[] = [
			fit(
				`Onlyne · ${regionLabel(status?.region)} · 节点 ${status?.nodes ?? "…"} · 邻居 ${status?.neighbors ?? "…"} · 告警 ${status?.alerts ?? 0} · 未读 ${status?.unread ?? 0}`,
				width,
			),
			fit(PAGES.map(page => (page.id === this.page ? `[${page.label}]` : ` ${page.label} `)).join(" "), width),
		];
		if (this.error !== undefined) lines.push(this.theme.fg("error", fit(this.error, width)));
		for (const line of this.ansiRows(width)) lines.push(line);
		this.rendered = { key, lines };
		return this.rendered.lines;
	}

	private ansiRows(width: number): string[] {
		const geo = this.geometry();
		const lines: string[] = [];
		const short = (text: string): string => fit(text, width);
		if (this.page === "chat") {
			const thread = this.selectedThread();
			lines.push(short(`线程 ${this.threads().length} · ${thread === undefined ? "未选择" : thread.title}`));
			const messages = thread?.messages ?? [];
			const visible = messages.slice(-geo.messages);
			if (visible.length < messages.length) lines.push(short(`更早的 ${messages.length - visible.length} 条不在这一屏`));
			for (const message of visible) {
				lines.push(short(`${message.direction === "out" ? "→" : "←"} [${message.seq}] ${plainAuthor(message.author)} ${clockText(message.at)}`));
				for (const line of message.text.split("\n").slice(0, 6)) lines.push(short(`  ${line}`));
			}
			const blocked = this.sendBlocked();
			lines.push(short(`传入：${oneLine(this.draft) || "（空）"}${blocked === undefined ? "" : ` · ${blocked}`}`));
			return this.fitRows(lines, geo.body);
		}
		if (this.page === "queue") {
			for (const item of this.snapshot?.queue ?? []) lines.push(short(`${item.kind} ${item.label} · ${oneLine(item.detail)}`));
			return this.fitRows(lines, geo.body);
		}
		if (this.page === "discovery") {
			for (const group of this.domainGroups()) {
				lines.push(short(group.label));
				for (const peer of group.peers) {
					lines.push(short(`  ${shortNodeId(peer.record.node_id)} · ${peer.trust === undefined ? "无认证" : STATE_LABEL[peer.trust.state]} · ${peer.record.base_url}`));
				}
			}
			return this.fitRows(lines, geo.body);
		}
		const invites = this.invites();
		lines.push(short(`待确认 · ${invites.length}`));
		for (const invite of invites) lines.push(short(`  ${shortNodeId(invite.node_id)} · 对方 ${invite.fingerprint} · 本机 ${this.snapshot?.identity?.fingerprint ?? "无密钥"}`));
		const trusted = (this.snapshot?.contacts ?? []).filter(contact => this.trustOf(contact.node_id)?.state === "trusted");
		lines.push(short(`已认证 · ${trusted.length}`));
		for (const contact of trusted) lines.push(short(`  ${shortNodeId(contact.node_id)} · ${regionLabel(contact.region)} · ${contact.base_url}`));
		return this.fitRows(lines, geo.body);
	}

	/** Exactly the body's rows: the fallback pads and trims, so it cannot resize the sheet either. */
	private fitRows(lines: string[], rows: number): string[] {
		const out = lines.slice(0, rows);
		while (out.length < rows) out.push("");
		return out;
	}

	invalidate(): void {
		this.version++;
		this.described = undefined;
		this.rendered = undefined;
	}

	dispose(): void {
		if (this.disposed) return;
		this.disposed = true;
		clearTimeout(this.noticeTimer);
		this.noticeTimer = undefined;
		this.unsubscribe?.();
		this.unsubscribe = undefined;
		this.composer = undefined;
		this.described = undefined;
		this.rendered = undefined;
	}
}
