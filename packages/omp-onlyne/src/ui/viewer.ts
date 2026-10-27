import type { KeyExchange, MessageRef, MsgInput, SendOutcome, TrustPhase } from "@onlyne/lib";
import {
	type Component,
	type DescribeContext,
	Editor,
	type NativeChild,
	type NativeNode,
	type NativeScroll,
	type NativeUiEvent,
	type Theme,
	type TspSpan,
	getEditorTheme,
	matchesKey,
	node,
	span,
	text as textNode,
} from "@oh-my-pi/pi-tui";
import { normalizeServiceUrl, shortNodeId } from "../config.ts";
import { getRuntime } from "../runtime.ts";
import type { OnlyneStatus, ViewerDiscovery, ViewerMessage, ViewerSnapshot, ViewerThread } from "../types.ts";
import { ageText, clockText, fit, oneLine, shortAlias, sinceText } from "./format.ts";

const REGION_PREFIX = "urn:onlyne:region:";

/** The label column of a fact row: wide enough for the longest label (四格汉字), one gap before the value. */
const FACT_LABEL = "10ch";

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

/** The stage names, in the words the approval flow uses: we wait on `invited`, the peer on `requested`. */
const PHASE_LABEL: Record<TrustPhase, string> = {
	exchanged: "已认证",
	requested: "等对方确认",
	invited: "待你确认",
	revoked: "已撤销",
};

const PHASE_TONE: Record<TrustPhase, string> = {
	exchanged: "success",
	requested: "info",
	invited: "warning",
	revoked: "error",
};

const PHASE_SHORT: Record<TrustPhase, string> = {
	exchanged: "已认证",
	requested: "等对方",
	invited: "待确认",
	revoked: "已撤销",
};

interface Peer {
	record: ViewerDiscovery;
	trust?: { phase: TrustPhase; fingerprint: string };
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

/** What the main column shows: one thread, or the queue waiting on this node. */
type View = { kind: "thread"; alias: string } | { kind: "inbox" };

/** A described node with a scroll request attached; `node()` only builds the wire shape. */
function scrolling(described: NativeNode, scroll: NativeScroll): NativeNode {
	return { ...described, scroll } as NativeNode;
}

/** A TSP line extent, for heights the terminal must never recompute from content. */
type LineExtent = `${number}lines`;

function linesOf(count: number): LineExtent {
	return `${Math.max(1, Math.floor(count))}lines` as LineExtent;
}

/**
 * The Onlyne surface, laid out as the design's four columns: the domain rail,
 * the thread sidebar, the conversation, and the member column with the
 * selected member's detail. Every operation is a click on a described node or
 * a select on a list row; `draftText` is the one way message text enters,
 * because a native editor reports text through the edit events itself.
 */
export class OnlyneViewer implements Component {
	private snapshot: ViewerSnapshot | undefined;
	private status: OnlyneStatus | undefined;
	private loading = true;
	private error: string | undefined;
	/** The main column's subject; undefined shows the first thread. */
	private view: View | undefined;
	/** The domain the rail highlights; undefined follows this node's own region. */
	private domainKey: string | undefined;
	private memberKey: string | undefined;
	/** An explicit recipient starts a new direct message. */
	private target: string | undefined;
	private replyTo: MessageRef | undefined;
	/** The composer's text, mirrored from the editor so a rebuild keeps it. */
	private draft = "";
	private composer: Editor | undefined;
	private sending = false;
	/** The address card: a node typed by hand, and the one read in flight. */
	private addressOpen = false;
	private address: Editor | undefined;
	private addressText = "";
	private connecting = false;
	/** The exchange whose fingerprint card is open; confirming is the session's own call. */
	private exchange?: KeyExchange;
	/** Only the newest request may open a card: a slower earlier lookup must not repaint it. */
	private exchangeToken = 0;
	/** Bumped to ask the terminal to scroll the message list to its newest row. */
	private messageScroll = 0;
	private messageScrollBy: NativeScroll["by"] = "end";
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
	// The four columns need the pane: a fixed `lg` sheet is about 68 cells and drops one of them.
	readonly nativeOverlay = { role: "onlyne.viewer", size: "full" as const, head: "Onlyne" };

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
		const view = this.view;
		if (view?.kind === "thread") {
			const named = threads.find(thread => thread.alias === view.alias);
			if (named !== undefined) return named;
		}
		if (view?.kind === "inbox") return undefined;
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
			.filter(entry => entry.phase === "invited")
			.map(entry => ({ node_id: entry.node_id, fingerprint: entry.fingerprint, base_url: entry.base_url }));
	}

	/** This node's trust row for a peer, undefined when the two never exchanged keys. */
	private trustOf(nodeId: string): Peer["trust"] {
		return this.snapshot?.trust?.find(entry => entry.node_id === nodeId);
	}

	/**
	 * Peers grouped by the domain they declare. Expired records stay in the
	 * list, flagged: the member column is the one place a user can revoke or
	 * retry a node whose observation aged out.
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
					: { trust: { phase: trust.phase, fingerprint: trust.fingerprint } }),
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

	private sendBlocked(): string | undefined {
		if (this.error !== undefined) return this.error;
		if (this.draft.trim() === "") return "输入内容后发送";
		if (this.target === undefined && this.replyTo === undefined && this.selectedThread() === undefined) {
			return "先打开线程，或选择收件人";
		}
		return undefined;
	}

	// ── recurring pieces ────────────────────────────────────────────────────

	private button(label: string, act: string, options: { tone?: "accent" | "error"; title?: string } = {}): NativeNode {
		return node(
			"row",
			// Tone stays on the glyph, never the row: `accent` is the terminal's button paint and
			// reads as a selection highlight when spread over a whole row.
			{ role: "omp.btn", align: "center", actions: { click: act }, title: options.title ?? label },
			[textNode([span(label, options.tone === undefined ? undefined : options.tone === "error" ? "error" : "accent strong")])],
			act,
		);
	}

	/** The dim one-line label that separates the sidebar's sections inside its scrolling list. */
	private heading(label: string, count: number): NativeNode {
		return node(
			"item",
			{
				label: [span(label, "dim")],
				...(count === 0 ? {} : { value: [span(String(count), "dim")] }),
				disabled: true,
				role: "onlyne.heading",
			},
			undefined,
			`head:${label}`,
		);
	}

	/**
	 * Label/value pairs as rows with a fixed label column. A `kv` grid sizes its
	 * label column in characters, so a Chinese label (two cells per character)
	 * leaves the value touching it and each row starting the value at a
	 * different column; a `ch` bound counts the cells the terminal draws.
	 */
	private facts(items: ReadonlyArray<{ k: string; v: string }>): NativeNode {
		return node(
			"col",
			{ role: "onlyne.facts", gap: "xs", align: "stretch" },
			items.map((item, index) =>
				node(
					"row",
					{ gap: "none", align: "baseline" },
					[
						textNode([span(item.k, "dim")], { truncate: "end", min: { w: FACT_LABEL }, max: { w: FACT_LABEL } }),
						textNode([span(item.v)], { wrap: "word", grow: 1, shrink: 1 }),
					],
					`fact-${index}`,
				),
			),
			"facts",
		);
	}

	// ── columns ─────────────────────────────────────────────────────────────

	/** The domain rail: one letter per declared domain, with the count waiting on this user. */
	private railNode(): NativeNode {
		const groups = this.domainGroups();
		const current = this.currentDomain();
		const rows: NativeChild[] = groups.map(group => {
			const waiting = group.peers.filter(peer => peer.trust?.phase === "invited").length;
			const open = current?.id === group.id;
			return node(
				"item",
				{
					// The rail names the domain: one letter cannot be told apart from another, and
					// the column width already truncates a long name.
					label: [span(group.label, open ? "strong" : "muted")],
					...(waiting === 0 ? {} : { value: [span(String(waiting), "error")] }),
					...(open ? { tone: "accent" } : {}),
					title: `${group.label} · ${group.peers.length} 节点`,
				},
				undefined,
				`domain:${group.id ?? ""}`,
			);
		});
		return node(
			"col",
			// Wide enough for a one-word domain name (`daily-workbench` is 15 cells) so the rail
			// names a domain instead of truncating it to a prefix two domains can share.
			{ role: "onlyne.rail", gap: "none", min: { w: "16ch" }, max: { w: "22ch" } },
			[
				node(
					"list",
					{
						role: "onlyne.rail.list",
						grow: 1,
						basis: 0,
						selected: current === undefined ? null : `domain:${current.id ?? ""}`,
						empty: [span("无域", "muted")],
					},
					rows,
					"domains",
				),
			],
			"rail",
		);
	}

	/**
	 * The thread sidebar: this node's identity, then every row that opens
	 * something, in one list that scrolls inside its own cap.
	 */
	private sidebar(): NativeNode {
		const invites = this.invites();
		const threads = this.threads();
		const domain = this.currentDomain();
		const fingerprint = this.snapshot?.identity?.fingerprint;
		const queue = this.snapshot?.queue.length ?? 0;
		const selected = this.selectedThread();
		const rows: NativeChild[] = [];

		rows.push(this.heading("待你确认", invites.length));
		for (const invite of invites) {
			rows.push(
				node(
					"item",
					{
						label: [span("▢ ", "warning"), span(shortNodeId(invite.node_id), "strong")],
						value: [span(invite.fingerprint.slice(0, 8), "dim")],
						tone: "warning",
						title: `${invite.node_id}\n点击核对指纹`,
					},
					undefined,
					`invite:${invite.node_id}`,
				),
			);
		}

		rows.push(this.heading("线程", threads.length));
		for (const thread of threads) {
			const open = selected?.alias === thread.alias && this.view?.kind !== "inbox";
			rows.push(
				node(
					"item",
					{
						label: [span(`# ${thread.title}`, open ? "strong" : undefined), ...(thread.state === "closed" ? [span(" 已关闭", "dim")] : [])],
						// One number per row, always the same number: the unread count. A row that
						// showed the message total once everything was read made two rows of equal
						// width mean different things.
						value: thread.unread > 0 ? [span(String(thread.unread), "info")] : [],
						title: `${thread.title} · ${thread.messages.length} 条消息 · ${shortAlias(thread.alias)}`,
					},
					undefined,
					`thread:${thread.alias}`,
				),
			);
		}
		if (threads.length === 0) {
			rows.push(node("item", { label: [span("还没有线程", "muted")], disabled: true }, undefined, "head:empty-threads"));
		}

		// The queue is its own section, so it needs no heading: the row names it and counts
		// what opening it shows.
		rows.push(
			node(
				"item",
				{
					label: [span("✉ 收件箱", "muted")],
					value: [span(String(queue), queue === 0 ? "dim" : "info")],
				},
				undefined,
				"inbox",
			),
		);

		return node(
			"col",
			{ role: "onlyne.threads", gap: "sm", align: "stretch", min: { w: "24ch" }, max: { w: "36ch" } },
			[
				node(
					"col",
					{ gap: "none" },
					[
						textNode([span(regionLabel(domain?.id ?? this.snapshot?.region), "dim")], { truncate: "end" }),
						textNode([span(shortNodeId(this.status?.node_id ?? ""), "strong")], { truncate: "end" }),
						textNode([span(fingerprint ?? "无节点密钥", "dim")], { truncate: "end" }),
					],
					"head",
				),
				node(
					"list",
					{
						role: "onlyne.threads.list",
						grow: 1,
						basis: 0,
						selected: this.view?.kind === "inbox" ? "inbox" : selected === undefined ? null : `thread:${selected.alias}`,
						empty: [span("还没有线程", "muted")],
					},
					rows,
					"rows",
				),
			],
			"threads",
		);
	}

	/** The approval bar: one row per pending request, with both answers on it. */
	private approvalsCard(): NativeChild[] {
		const invites = this.invites();
		if (invites.length === 0) return [];
		const mine = this.snapshot?.identity?.fingerprint ?? "本节点没有密钥";
		return invites.map(invite =>
			node(
				"card",
				{
					role: "onlyne.approval",
					tone: "warning",
					head: [span(`${shortNodeId(invite.node_id)} 请求与你交换密钥`, "strong")],
				},
				[
					this.facts([
						{ k: "对方指纹", v: invite.fingerprint },
						{ k: "本机指纹", v: mine },
						{ k: "端点", v: invite.base_url },
					]),
					node(
						"row",
						{ gap: "sm", align: "center" },
						[
							this.button("同意", `onlyne.accept:${invite.node_id}`, { tone: "accent", title: "双方指纹一致，批准这次交换" }),
							this.button("拒绝", `onlyne.reject:${invite.node_id}`, { title: "不批准这次申请" }),
							this.button("查看详情", `onlyne.member:${invite.node_id}`, { title: "在成员栏打开这位节点" }),
						],
						"acts",
					),
				],
				`approval-${invite.node_id}`,
			),
		);
	}

	private messageCard(message: ViewerMessage): NativeNode {
		const out = message.direction === "out";
		const messageKey = `${encodeURIComponent(message.ref.sender)}/${encodeURIComponent(message.ref.id)}`;
		const answered = this.replyTo?.sender === message.ref.sender && this.replyTo.id === message.ref.id;
		// A message is a flat entry, not a box: `section` gives the head and body placement of a
		// card without its ring. A `list` row is one fixed line, so a multi-line message must not
		// be the row itself.
		return node(
			"section",
			{
				role: "omp.btn",
				...(answered ? { tone: "accent" } : {}),
				head: [
					span(out ? "→ " : "← ", out ? "accent" : "dim"),
					span(plainAuthor(message.author), "strong"),
					span(`  ${clockText(message.at) || "—"}`, "dim"),
					...(message.relation === undefined ? [] : [span(`  ${message.relation}`, "dim")]),
				],
				title: `#${message.seq} · 点击回复这一条`,
				actions: { click: `onlyne.message:${messageKey}` },
			},
			[textNode(message.text, { wrap: "word" })],
			`message-${messageKey}`,
		);
	}

	private composerNode(): NativeNode {
		const thread = this.selectedThread();
		const blocked = this.sendBlocked();
		if (this.composer === undefined) {
			const editor = new Editor(getEditorTheme());
			editor.disableSubmit = true;
			editor.setText(this.draft);
			editor.setMaxHeight(3);
			this.composer = editor;
		}
		// One caret at a time: the address card owns it while it is open.
		this.composer.focused = this.focused && !this.addressOpen;
		const where = this.replyTo !== undefined
			? `回复 ${shortNodeId(this.replyTo.sender)} · ${this.replyTo.id}`
			: this.target !== undefined
				? `新消息发送到 ${shortNodeId(this.target)}`
				: thread === undefined ? "先打开线程，或选择收件人" : `发言到 # ${thread.title} · ${shortNodeId(thread.ref.owner)} 托管`;
		return node(
			"card",
			// `bare` keeps the head-over-body grouping without a ring: the editor draws its own
			// field, and a card ring around it reads as a box inside a box.
			{ role: "onlyne.composer", inset: true, variant: "bare" },
			[
				this.composer,
				node(
					"row",
					{ gap: "md", align: "center", justify: "between" },
					[
						textNode([span(where, blocked === undefined ? "dim" : "warning")], { wrap: "word", grow: 1 }),
						...(this.replyTo === undefined ? [] : [this.button("取消回复", "onlyne.reply.clear", { title: "不再引用这一条" })]),
						...(this.target === undefined ? [] : [this.button("改回线程发言", "onlyne.target:", { title: "清除手动指定的收件人" })]),
						node(
							"row",
							{
								role: "omp.btn",
								align: "center",
								actions: { click: "onlyne.send" },
								title: blocked ?? "发送这条消息",
							},
							[textNode([span(this.sending ? "发送中…" : "发送", blocked === undefined ? "accent strong" : "muted")])],
							"send",
						),
					],
					"row",
				),
			],
			"composer",
		);
	}

	/** The queue the inbox row opens: one scrolling list, grouped by what each row is. */
	private inboxRows(): NativeNode {
		const queue = this.snapshot?.queue ?? [];
		const rows: NativeChild[] = [];
		for (const [head, kind] of [["来信", "inbound"], ["出站", "outbound"], ["本地上下文", "context"], ["判定", "decision"]] as const) {
			const items = queue.filter(entry => entry.kind === kind);
			if (items.length === 0) continue;
			rows.push(this.heading(head, items.length));
			for (const entry of items) {
				rows.push(
					node(
						"item",
						{
							label: [span(entry.label, "strong")],
							detail: [span(oneLine(entry.detail), "dim")],
							...(entry.at === undefined ? {} : { value: [span(ageText(entry.at), "dim")] }),
							title: entry.detail,
						},
						undefined,
						`queue:${entry.id}`,
					),
				);
			}
		}
		return node(
			"list",
			{
				role: "onlyne.inbox.list",
				grow: 1,
				basis: 0,
				empty: [span("队列是空的。", "muted")],
			},
			rows,
			"queue",
		);
	}

	/** The main column: the open thread with its composer, or the queue. */
	private flowNode(): NativeNode {
		const children: NativeChild[] = [];
		if (this.error !== undefined) {
			children.push(textNode([span(this.error, "error")], { wrap: "word" }));
			children.push(this.button("重新读取", "onlyne.reload"));
		}
		if (this.notice !== undefined) {
			children.push(textNode([span(this.notice.text, this.notice.error ? "error" : "info")], { wrap: "word" }));
		}
		if (this.exchange !== undefined) {
			children.push(
				node(
					"card",
					{ role: "onlyne.exchange", tone: "warning", head: [span(`与 ${shortNodeId(this.exchange.node_id)} 交换密钥`, "strong")] },
					[
						this.facts([
							{ k: "对方节点", v: this.exchange.node_id },
							{ k: "对方指纹", v: this.exchange.fingerprint },
							{ k: "本机指纹", v: this.snapshot?.identity?.fingerprint ?? "本节点没有密钥" },
						]),
						node("row", { gap: "sm", align: "center" }, [
							this.button("确认并发送申请", "onlyne.exchange.confirm", { tone: "accent" }),
							this.button("取消", "onlyne.exchange.cancel"),
						], "acts"),
					],
					"exchange",
				),
			);
		}

		if (this.view?.kind === "inbox") {
			children.push(
				node("row", { role: "onlyne.thread-head", gap: "md", align: "center", justify: "between" }, [
					node("col", { gap: "none", grow: 1 }, [
						textNode([span("✉ 收件箱", "strong")], { truncate: "end" }),
						textNode([span(`${this.snapshot?.queue.length ?? 0} 条待处理`, "dim")], { truncate: "end" }),
					]),
				], "thread-head"),
				this.inboxRows(),
			);
		} else {
			const thread = this.selectedThread();
			if (thread === undefined) {
				children.push(textNode([span("从左边打开一个线程，或在成员栏选一位节点发起对话。", "muted")], { wrap: "word", lines: 3 }));
			} else {
				const messages = thread.messages;
				const last = messages.at(-1);
				children.push(
					node("row", { role: "onlyne.thread-head", gap: "md", align: "center", justify: "between" }, [
						node("col", { gap: "none", grow: 1 }, [
							textNode([span(`# ${thread.title}`, "strong"), ...(thread.state === "closed" ? [span(" · 已关闭", "dim")] : [])], { truncate: "end" }),
							textNode([span(`${messages.length} 条消息 · ${new Set(messages.map(message => message.author)).size} 位发言者 · ${shortNodeId(thread.ref.owner)} 托管${last === undefined ? "" : ` · 最近一条 ${sinceText(last.at) || "时间未知"}`}`, "dim")], { truncate: "end" }),
						]),
					], "thread-head"),
					...this.approvalsCard(),
					scrolling(
						node(
							"list",
							{
								role: "onlyne.messages",
								grow: 1,
								basis: 0,
								empty: [span("这个线程还没有消息。", "muted")],
							},
							messages.map(message => this.messageCard(message)),
							"messages",
						),
						{ by: this.messageScrollBy, n: this.messageScroll },
					),
					this.composerNode(),
				);
			}
		}
		if (this.loading) children.push(node("spinner", { label: [span("读取 Onlyne 状态…", "muted")] }, undefined, "loading"));
		return node(
			"col",
			{ role: "onlyne.flow", gap: "sm", grow: 1, shrink: 1, align: "stretch", min: { w: 0 } },
			children,
			"flow",
		);
	}

	/**
	 * A node reached by address instead of by discovery: mDNS never leaves the link, and a
	 * remote node on a public name has no advertisement at all. The address is read once and
	 * saved as a seed, so the next start finds the node again without this card.
	 */
	private addressCard(): NativeNode {
		if (this.address === undefined) {
			const editor = new Editor(getEditorTheme());
			editor.disableSubmit = true;
			editor.setText(this.addressText);
			editor.setMaxHeight(1);
			editor.onChange = () => {
				this.addressText = editor.getExpandedText();
				this.described = undefined;
				this.requestRender();
			};
			this.address = editor;
		}
		this.address.focused = this.focused;
		return node(
			"card",
			{ role: "onlyne.connect", inset: true, head: [span("连接一个节点", "strong")] },
			[
				this.address,
				textNode(
					[span("origin（http://host:7801）或服务地址（http://host:7801/onlyne/v3）都可以；连上后保存为种子，下次启动自动联系。", "dim")],
					{ wrap: "word" },
				),
				node(
					"row",
					{ gap: "sm", align: "center", wrap: true },
					[
						this.button(this.connecting ? "连接中…" : "连接", "onlyne.connect.submit", { tone: "accent", title: "读取这个地址并保存为种子" }),
						this.button("取消", "onlyne.connect.cancel", { title: "关掉地址输入" }),
					],
					"acts",
				),
			],
			"connect",
		);
	}

	/** The member column: the current domain's peers, then the selected one's detail. */
	private memberColumn(): NativeNode {
		const peers = this.peers();
		const rows: NativeChild[] = peers.map(peer => {
			const phase = peer.trust?.phase;
			return node(
				"item",
				{
					label: [span("● ", phase === undefined ? "muted" : PHASE_TONE[phase]), span(shortNodeId(peer.record.node_id), "strong")],
					// Every row here belongs to the selected domain, so the region would repeat
					// the column head and truncate the id it sits next to.
					value: [span(phase === undefined ? "无认证" : PHASE_SHORT[phase], phase === undefined ? "dim" : PHASE_TONE[phase])],
					title: `${peer.record.node_id}\n${peer.record.base_url}`,
				},
				undefined,
				`peer:${peer.record.node_id}`,
			);
		});
		const children: NativeChild[] = [
			node("row", { gap: "sm", align: "center", justify: "between" }, [
				textNode([span(`域内节点 · ${peers.length}`, "dim")]),
				node(
					"row",
					{ gap: "xs", align: "center" },
					[
						...(this.addressOpen ? [] : [this.button("添加地址", "onlyne.connect.open", { title: "按地址连接一个发现不到的节点" })]),
						this.button("刷新", "onlyne.discovery.refresh", { title: "重新观察一轮发现" }),
					],
					"acts",
				),
			], "head"),
			...(this.addressOpen ? [this.addressCard()] : []),
			node(
				"list",
				{
					role: "onlyne.members.list",
					grow: 1,
					basis: 0,
					selected: this.memberKey === undefined ? null : `peer:${this.memberKey}`,
					empty: [span("这个域还没有可达节点。", "muted")],
				},
				rows,
				"roster",
			),
		];
		const peer = peers.find(entry => entry.record.node_id === this.memberKey);
		if (peer !== undefined) {
			const phase = peer.trust?.phase;
			const stale = Date.parse(peer.record.expires_at) <= Date.now();
			children.push(
				node(
					"card",
					{
						role: "onlyne.member-detail",
						head: [span(shortNodeId(peer.record.node_id), "strong"), ...(phase === undefined ? [] : [span(` · ${PHASE_LABEL[phase]}`, PHASE_TONE[phase])])],
					},
					[
						this.facts([
							{ k: "指纹", v: peer.trust?.fingerprint ?? "未交换" },
							{ k: "域", v: regionLabel(peer.record.region) },
							{ k: "状态", v: phase === undefined ? "无认证" : PHASE_LABEL[phase] },
							{ k: "端点", v: peer.record.base_url },
							{ k: "来源", v: peer.record.source === "bootstrap" ? "按地址连接（已存为种子）" : peer.record.source },
							{ k: "最后可见", v: `${sinceText(peer.record.last_seen) || "时间未知"}${stale ? " · 记录已过期" : ""}` },
						]),
						node("row", { gap: "sm", align: "center", wrap: true }, [
							this.button("用作发送目标", `onlyne.target:${peer.record.node_id}`, { title: "把消息发给这位节点" }),
							...(phase === "invited" ? [this.button("同意交换", `onlyne.accept:${peer.record.node_id}`, { tone: "accent" })] : []),
							...(phase === undefined || phase === "revoked" ? [this.button("请求交换密钥", `onlyne.request:${peer.record.node_id}`, { title: "读取对方指纹，核对后发送申请" })] : []),
							...(phase === "exchanged" || phase === "requested" ? [this.button("撤销信任", `onlyne.revoke:${peer.record.node_id}`, { tone: "error" })] : []),
						], "acts"),
					],
					"member-detail",
				),
			);
		}
		return node(
			"col",
			{ role: "onlyne.members", gap: "sm", align: "stretch", min: { w: "24ch" }, max: { w: "34ch" } },
			children,
			"members",
		);
	}

	describe(cx: DescribeContext): NativeNode {
		const key = `${this.version}:${cx.cols}:${this.rows()}`;
		if (this.described !== undefined && this.describedKey === key) return this.described;
		// The full sheet draws a title row and one inset row around the body: bound the columns
		// to the rest of the pane, or the composer lands past the bottom edge.
		const body = linesOf(Math.max(8, this.rows() - 2));
		const columns = node(
			"row",
			{ gap: "md", align: "stretch", grow: 1, shrink: 1, min: { h: body }, max: { h: body } },
			[this.railNode(), this.sidebar(), this.flowNode(), this.memberColumn()],
			"columns",
		);
		this.described = node("card", { role: "onlyne.viewer", grow: 1 }, [columns], "root");
		this.describedKey = key;
		return this.described;
	}

	// ── events ──────────────────────────────────────────────────────────────

	/** A row of the sidebar or the member list: its id names what the click opens. */
	private selectRow(id: string): void {
		if (id.startsWith("thread:")) {
			const alias = id.slice("thread:".length);
			if (!this.threads().some(thread => thread.alias === alias)) return;
			this.view = { kind: "thread", alias };
			this.target = undefined;
			this.replyTo = undefined;
			this.messageScrollBy = "end";
			this.messageScroll++;
		} else if (id.startsWith("invite:")) {
			this.memberKey = id.slice("invite:".length);
		} else if (id.startsWith("domain:")) {
			this.domainKey = id.slice("domain:".length) || undefined;
			this.memberKey = undefined;
		} else if (id.startsWith("peer:")) {
			const peer = id.slice("peer:".length);
			this.memberKey = this.memberKey === peer ? undefined : peer;
		} else if (id === "inbox") {
			this.view = { kind: "inbox" };
		} else {
			return;
		}
		this.changed();
	}

	handleNativeEvent(event: NativeUiEvent): void {
		if (this.disposed || this.signal.aborted) return;
		if (event.type === "edit" || event.type === "undo") {
			if (this.addressOpen) {
				this.address?.handleNativeEvent(event);
				this.addressText = this.address?.getExpandedText() ?? this.addressText;
			} else {
				this.composer?.handleNativeEvent(event);
				this.draft = this.composer?.getExpandedText() ?? this.draft;
			}
			this.described = undefined;
			this.requestRender();
			return;
		}
		// The keypath carries the component's ancestry (`root/columns/sidebar/rows`), so match its leaf.
		if (event.type === "select" || event.type === "activate") {
			const leaf = event.key.slice(event.key.lastIndexOf("/") + 1);
			if (leaf === "rows" || leaf === "roster" || leaf === "domains") this.selectRow(event.item);
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
		if (act === "onlyne.reply.clear") {
			this.replyTo = undefined;
			this.changed();
			return;
		}
		if (act === "onlyne.discovery.refresh") {
			void this.refreshDiscovery();
			return;
		}
		if (act === "onlyne.connect.open") {
			this.addressOpen = true;
			this.changed();
			return;
		}
		if (act === "onlyne.connect.cancel") {
			this.closeAddress();
			return;
		}
		if (act === "onlyne.connect.submit") {
			void this.connect();
			return;
		}
		if (act === "onlyne.exchange.confirm") {
			void this.confirmExchange();
			return;
		}
		if (act === "onlyne.exchange.cancel") {
			this.exchangeToken++;
			this.exchange = undefined;
			this.changed();
			return;
		}
		if (act.startsWith("onlyne.domain:")) {
			this.domainKey = act.slice("onlyne.domain:".length) || undefined;
			this.memberKey = undefined;
			this.changed();
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
			this.replyTo = undefined;
			this.changed();
			return;
		}
		if (act.startsWith("onlyne.message:")) {
			const identity = act.slice("onlyne.message:".length);
			const thread = this.selectedThread();
			const message = thread?.messages.find(entry => `${encodeURIComponent(entry.ref.sender)}/${encodeURIComponent(entry.ref.id)}` === identity);
			if (message !== undefined && thread !== undefined) {
				if (message.ref.sender === this.status?.node_id) return;
				this.replyTo = message.ref;
				this.target = undefined;
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
			return;
		}
		if (act.startsWith("onlyne.revoke:")) {
			void this.revoke(act.slice("onlyne.revoke:".length));
			return;
		}
		if (act.startsWith("onlyne.request:")) {
			void this.request(act.slice("onlyne.request:".length));
		}
	}

	/** Escape closes the overlay; the page keys scroll the open list; everything else is the composer's. */
	handleInput(data: string): void {
		if (this.disposed || this.signal.aborted) return;
		if (matchesKey(data, "escape")) {
			if (this.addressOpen) {
				this.closeAddress();
				return;
			}
			this.done();
			return;
		}
		if (this.addressOpen) {
			// A single-line field: Enter is the one way to submit it from the keyboard.
			if (matchesKey(data, "enter")) {
				void this.connect();
				return;
			}
			this.address?.handleInput(data);
			this.addressText = this.address?.getExpandedText() ?? this.addressText;
			this.described = undefined;
			this.requestRender();
			return;
		}
		if (matchesKey(data, "ctrl+enter")) {
			void this.send();
			return;
		}
		if (matchesKey(data, "pageUp") || matchesKey(data, "pageDown") || matchesKey(data, "end")) {
			this.messageScrollBy = matchesKey(data, "pageUp") ? "page-up" : matchesKey(data, "pageDown") ? "page-down" : "end";
			this.messageScroll++;
			this.changed();
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

	private closeAddress(): void {
		this.addressOpen = false;
		this.address = undefined;
		this.addressText = "";
		this.changed();
	}

	/**
	 * Reads the typed address through the runtime, which owns normalization, the discovery
	 * read and the seed write. The node keeps running; the new peer arrives in the member
	 * list, where the ordinary exchange request applies to it.
	 */
	private async connect(): Promise<void> {
		if (this.connecting) return;
		const runtime = getRuntime();
		if (runtime === undefined) {
			this.setNotice("Onlyne 还没启动，先运行 /onlyne init。", true);
			return;
		}
		const typed = this.addressText.trim();
		if (typed.length === 0) {
			this.setNotice("先填写地址，例如 http://127.0.0.1:7801/onlyne/v3", true);
			return;
		}
		this.connecting = true;
		this.changed();
		let seed: string | undefined;
		try {
			seed = normalizeServiceUrl(typed);
			await runtime.contact(typed);
			this.setNotice(`已连接 ${seed}，并保存为种子；下次启动会自动联系。`);
			this.closeAddress();
		} catch (error) {
			seed = undefined;
			this.setNotice(`连接失败：${errorText(error)}`, true);
		} finally {
			this.connecting = false;
			await this.reload();
		}
		if (seed === undefined) return;
		// Land on the new peer, in its own domain: its detail card is where the exchange starts.
		const target = seed;
		const record = this.snapshot?.discovery.find(entry => entry.base_url === target);
		if (record === undefined) return;
		this.domainKey = record.region;
		this.memberKey = record.node_id;
		this.changed();
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

	private async request(nodeId: string): Promise<void> {
		const runtime = getRuntime();
		if (runtime === undefined) return;
		const token = ++this.exchangeToken;
		this.exchange = undefined;
		this.changed();
		try {
			const session = await runtime.trustBegin(nodeId);
			if (this.disposed || this.signal.aborted || token !== this.exchangeToken) return;
			this.exchange = session;
			this.changed();
		} catch (error) {
			if (token !== this.exchangeToken) return;
			this.setNotice(`请求失败：${errorText(error)}`, true);
		}
	}

	private async confirmExchange(): Promise<void> {
		const session = this.exchange;
		if (session === undefined) return;
		this.exchange = undefined;
		this.changed();
		try {
			await session.confirm(session.fingerprint);
			this.setNotice(`已提交与 ${shortNodeId(session.node_id)} 的密钥交换，双方批准状态见成员栏。`);
		} catch (error) {
			this.setNotice(`请求失败：${errorText(error)}`, true);
		}
		await this.reload();
	}

	/**
	 * The composer sends down the one message path the tool uses: the node's own
	 * client with this session as the consumption context, so a click and a `msg`
	 * call leave the same records. A reply names the message it answers, which is
	 * what lands it in the thread holding that message.
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
		const meta: NonNullable<MsgInput["meta"]> = {};
		if (this.replyTo !== undefined) {
			meta.in_reply_to = { sender: this.replyTo.sender, id: this.replyTo.id };
			meta.relation = "reply";
		} else if (this.target !== undefined) {
			meta.to = this.target;
		} else if (thread !== undefined) {
			meta.to = thread.ref.owner;
			meta.thread = { owner: thread.ref.owner, id: thread.ref.id };
		}
		const input: MsgInput = {
			delivery_kind: this.replyTo !== undefined
				? `回复 ${shortNodeId(this.replyTo.sender)}`
				: thread === undefined
					? `给 ${shortNodeId(this.target ?? "")} 发一条消息`
					: `在 ${thread.title} 里发一条消息`,
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
				this.replyTo = undefined;
				this.messageScrollBy = "end";
				this.messageScroll++;
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
		];
		if (this.error !== undefined) lines.push(this.theme.fg("error", fit(this.error, width)));
		for (const line of this.ansiRows(width)) lines.push(line);
		this.rendered = { key, lines };
		return this.rendered.lines;
	}

	private ansiRows(width: number): string[] {
		const lines: string[] = [];
		const short = (text: string): string => fit(text, width);
		if (this.view?.kind === "inbox") {
			lines.push(short(`收件箱 · ${this.snapshot?.queue.length ?? 0} 条待处理`));
			for (const entry of this.snapshot?.queue ?? []) {
				lines.push(short(`  ${entry.kind} ${entry.label} · ${oneLine(entry.detail)}`));
			}
			return lines;
		}
		const thread = this.selectedThread();
		lines.push(short(`线程 ${this.threads().length} · ${thread === undefined ? "未选择" : thread.title}`));
		for (const invite of this.invites()) lines.push(short(`待确认 · ${shortNodeId(invite.node_id)} · ${invite.fingerprint}`));
		for (const message of thread?.messages ?? []) {
			lines.push(short(`${message.direction === "out" ? "→" : "←"} [${message.seq}] ${plainAuthor(message.author)} ${clockText(message.at)}`));
			for (const line of message.text.split("\n").slice(0, 6)) lines.push(short(`  ${line}`));
		}
		if (this.addressOpen) lines.push(short(`连接地址：${oneLine(this.addressText) || "（空）"} · Enter 连接`));
		const blocked = this.sendBlocked();
		lines.push(short(`传入：${oneLine(this.draft) || "（空）"}${blocked === undefined ? "" : ` · ${blocked}`}`));
		return lines;
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
		this.address = undefined;
		this.described = undefined;
		this.rendered = undefined;
	}
}
