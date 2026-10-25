import {
	type Component,
	type DescribeContext,
	type NativeNode,
	type NativeScroll,
	type NativeUiEvent,
	type Theme,
	type TspSpan,
	matchesKey,
	node,
	span,
	text as textNode,
	wrapTextWithAnsi,
} from "@oh-my-pi/pi-tui";
import { shortNodeId } from "../config.ts";
import { getRuntime } from "../runtime.ts";
import type { QueueItem, ViewerDiscovery, ViewerMessage, ViewerSnapshot, ViewerThread } from "../types.ts";
import { ageText, clockText, fit, kindLabel, kindRank, oneLine, pad, shortAlias } from "./format.ts";

type Page = "queue" | "chat" | "discovery" | "contacts";

const PAGES: readonly { id: Page; label: string }[] = [
	{ id: "queue", label: "队列" },
	{ id: "chat", label: "对话" },
	{ id: "discovery", label: "发现" },
	{ id: "contacts", label: "联系人" },
];

const FOOTER = "1-4 切页 · j/k 移动 · [/] 切域 · t 线程 · space 勾选 · x 撤销信任 · enter 详情 · r 刷新 · Esc 关闭";

function errorText(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}

/** One cell of a body row: the ANSI render uses the text, the native one the style token. */
type Cell = readonly [text: string, style?: string];

/**
 * One fixed-height body row. A row with `right` is drawn as two columns; the
 * single-column form fills the width.
 */
interface BodyRow {
	left: readonly Cell[];
	right?: readonly Cell[];
	leftWidth?: number;
	act?: string;
	leftAct?: string;
	rightAct?: string;
	selected?: boolean;
	key?: string;
	/** Card presentation: a head line plus a wrapping description, instead of one flat row. */
	card?: { head: string; description: string; tone?: string };
}

/** One aggregated domain of the discovery page: its label plus the node records in it. */
interface DiscoveryGroup {
	id?: string;
	label: string;
	nodes: ViewerDiscovery[];
}

/** A node whose key exchange waits for the y/n confirmation, fingerprint already verified. */
interface PendingExchange {
	node_id: string;
	fingerprint: string;
	domain?: string;
}

/** RFC 0017 trust rows the runtime adds to the snapshot; kept local until the parent types it. */
interface ViewerTrust {
	node_id: string;
	fingerprint: string;
	state: "pending" | "trusted" | "revoked";
	base_url?: string;
}

/** Read-only view of the current OMP process: queue, threads, discovery and contacts over a fixed body. */
export class OnlyneViewer implements Component {
	private snapshot: ViewerSnapshot | undefined;
	private loading = true;
	private error: string | undefined;
	private page: Page = "queue";
	private queueIndex = 0;
	private threadIndex = 0;
	private messageIndex = 0;
	private domainIndex = 0;
	private nodeIndex = 0;
	private contactIndex = 0;
	/** Node ids ticked for key exchange, this viewing session only. */
	private checked = new Set<string>();
	private pendingConfirm: PendingExchange | undefined;
	private expanded = false;
	/** Scroll request for the detail text box; bumped per scroll key, like raw-sse's stream page. */
	private detailScroll: NativeScroll | undefined;
	/** The chat page's thread column, folded away to give the messages the full width. */
	private threadFolded = false;
	private notice: { text: string; error: boolean } | undefined;
	private noticeTimer: Timer | undefined;
	private ages = new Map<string, string>();
	/** Scroll offsets, one per scrolling list. */
	private offsets = new Map<string, number>();
	private described: NativeNode | undefined;
	private describedKey: string | undefined;
	private rendered: { key: string; lines: readonly string[] } | undefined;
	private lastWidth = 100;
	private version = 0;
	private disposed = false;
	private generation = 0;
	focused = true;
	readonly nativeOverlay = { role: "onlyne.viewer", size: "lg" as const, head: "Onlyne" };

	constructor(
		private readonly theme: Theme,
		private readonly requestRender: () => void,
		private readonly done: () => void,
		private readonly signal: AbortSignal,
		private readonly rows: () => number = () => 24,
	) {
		void this.reload();
	}

	private changed(): void {
		if (this.disposed || this.signal.aborted) return;
		this.version++;
		this.described = undefined;
		this.rendered = undefined;
		this.requestRender();
	}

	/** The body window, fixed by the terminal height so message content never changes the frame. */
	private bodyHeight(): number {
		return Math.max(6, Math.min(40, Math.floor(this.rows() * 0.8) - 4));
	}

	async reload(): Promise<void> {
		if (this.disposed || this.signal.aborted) return;
		const generation = ++this.generation;
		this.loading = true;
		this.error = undefined;
		this.changed();
		const runtime = getRuntime();
		if (runtime === undefined) {
			this.snapshot = undefined;
			this.loading = false;
			this.error = "未配置 Onlyne。先运行 /onlyne settings 填写项目配置。";
			this.changed();
			return;
		}
		try {
			const snapshot = await runtime.snapshot();
			if (this.disposed || this.signal.aborted || generation !== this.generation) return;
			this.snapshot = snapshot;
			const now = Date.now();
			this.ages = new Map(snapshot.queue.map((item) => [item.id, ageText(item.at, now)]));
			this.clampSelection();
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

	private clampSelection(): void {
		this.queueIndex = Math.min(this.queueIndex, Math.max(0, this.queueItems().length - 1));
		const threads = this.threads();
		this.threadIndex = Math.min(this.threadIndex, Math.max(0, threads.length - 1));
		this.messageIndex = Math.min(this.messageIndex, Math.max(0, (threads[this.threadIndex]?.messages.length ?? 0) - 1));
		const domains = this.discoveryDomains();
		this.domainIndex = Math.min(this.domainIndex, Math.max(0, domains.length - 1));
		this.nodeIndex = Math.min(this.nodeIndex, Math.max(0, (domains[this.domainIndex]?.nodes.length ?? 0) - 1));
		this.contactIndex = Math.min(this.contactIndex, Math.max(0, this.contactSources().length - 1));
	}

	private queueItems(): QueueItem[] {
		const items = this.snapshot?.queue ?? [];
		return [...items].sort((left, right) => {
			const byKind = kindRank(left.kind) - kindRank(right.kind);
			if (byKind !== 0) return byKind;
			const byTime = (left.at ?? "").localeCompare(right.at ?? "");
			return byTime !== 0 ? byTime : left.id.localeCompare(right.id);
		});
	}

	private threads(): ViewerThread[] {
		const threads = this.snapshot?.threads ?? [];
		return [...threads].sort((left, right) => {
			const bySeq = Number(right.last_seq) - Number(left.last_seq);
			if (Number.isFinite(bySeq) && bySeq !== 0) return bySeq;
			return left.alias.localeCompare(right.alias);
		});
	}

	private selectedThread(): ViewerThread | undefined {
		return this.threads()[this.threadIndex];
	}

	/**
	 * Move the list's offset so `selected` stays visible, then return that
	 * `height`-row window and where it starts.
	 */
	private window<T>(key: string, items: readonly T[], selected: number, height: number): { start: number; items: readonly T[] } {
		const count = items.length;
		if (count === 0 || height <= 0) return { start: 0, items: [] };
		const maxStart = Math.max(0, count - height);
		let start = Math.min(Math.max(0, this.offsets.get(key) ?? 0), maxStart);
		if (selected < start) start = selected;
		else if (selected >= start + height) start = selected - height + 1;
		start = Math.min(Math.max(0, start), maxStart);
		this.offsets.set(key, start);
		return { start, items: items.slice(start, start + height) };
	}

	private pageCount(page: Page): number {
		switch (page) {
			case "queue":
				return this.queueItems().length;
			case "chat":
				return this.selectedThread()?.messages.length ?? 0;
			case "discovery":
				return this.discoveryDomains()[this.domainIndex]?.nodes.length ?? 0;
			case "contacts":
				return this.contactSources().length;
		}
	}

	private pageSelection(page: Page): number {
		switch (page) {
			case "queue":
				return this.queueIndex;
			case "chat":
				return this.messageIndex;
			case "discovery":
				return this.nodeIndex;
			case "contacts":
				return this.contactIndex;
		}
	}

	private setSelection(page: Page, index: number): void {
		switch (page) {
			case "queue":
				this.queueIndex = index;
				return;
			case "chat":
				this.messageIndex = index;
				return;
			case "discovery":
				this.nodeIndex = index;
				return;
			case "contacts":
				this.contactIndex = index;
				return;
		}
	}

	/** Rows of the list the page's selection moves in, used for the half-page step. */
	private pageWindowHeight(page: Page): number {
		const body = this.bodyHeight();
		if (page === "discovery") return Math.max(1, body - 2);
		if (page === "chat") return Math.max(1, this.chatLayout(this.lastWidth).messageHeight);
		return body;
	}

	private summaryText(): string {
		const index = PAGES.findIndex((page) => page.id === this.page) + 1;
		const threads = this.threads();
		const unread = threads.reduce((total, thread) => total + Math.max(0, thread.unread), 0);
		const region = this.snapshot?.region === undefined || this.snapshot.region.length === 0 ? "" : `${this.snapshot.region} · `;
		const discovery = this.snapshot?.discovery_error;
		const alert = discovery === undefined ? "" : ` · ⚠ ${oneLine(discovery)}`;
		return `${index}/4 · ${region}${threads.length} thread · ${unread} 未读 · ${this.snapshot?.contacts.length ?? 0} 可达${alert}`;
	}

	private titleLine(): string {
		// The overlay sheet already draws `Onlyne` from `nativeOverlay.head`; a second
		// copy here reads as a duplicated heading.
		const title = "";
		if (this.notice === undefined) return title;
		const notice = this.notice.error ? this.theme.fg("error", this.notice.text) : this.theme.fg("accent", this.notice.text);
		return `${title}  ${notice}`;
	}

	private tabLine(width: number): string {
		// Tabs own their row and fill it: an equal column each, the active one bold.
		const slot = Math.max(1, Math.floor(width / PAGES.length));
		return PAGES.map((page, index) => {
			const label = `${index + 1} ${page.label}`;
			const cell = pad(label, slot);
			return page.id === this.page ? this.theme.bold(cell) : cell;
		}).join("");
	}

	private messageHead(message: ViewerMessage): string {
		const direction = message.direction === "in" ? "in" : "out";
		const relation = message.relation === undefined ? "" : ` · ${message.relation}`;
		return `[${message.seq}] ${direction} · ${message.author} · ${clockText(message.at)}${relation}`;
	}

	private queueRows(): BodyRow[] {
		const items = this.queueItems();
		if (items.length === 0) return [{ left: [["没有待处理消息。", "muted"]] }];
		const { start, items: visible } = this.window("queue", items, this.queueIndex, this.bodyHeight());
		return visible.map((item, offset) => {
			const selected = start + offset === this.queueIndex;
			const age = this.ages.get(item.id) ?? ageText(item.at);
			const cells: Cell[] = [
				[selected ? "> " : "  ", "muted"],
				[`[${kindLabel(item.kind)}] `, "dim"],
				[item.label, "strong"],
			];
			if (age !== "") cells.push([` · ${age}`, "muted"]);
			const detail = oneLine(item.detail);
			if (detail !== "") cells.push([` · ${detail}`, "muted"]);
			return {
				left: cells, selected, act: `onlyne.queue:${item.id}`, key: item.id,
				card: { head: item.label, description: [kindLabel(item.kind), detail, age === "" ? "" : age].filter(part => part !== "").join(" · ") },
			};
		});
	}

	private threadRow(thread: ViewerThread, selected: boolean): BodyRow {
		const unread = thread.unread > 0 ? ` · ${thread.unread} 未读` : "";
		const closed = thread.state === "closed" ? " · 已关闭" : "";
		const label = shortAlias(thread.alias);
		return {
			left: [
				[selected ? "> " : "  ", "muted"],
				[label, "strong"],
				[` · ${thread.title}${unread}${closed}`, "muted"],
			],
			selected,
			act: `onlyne.thread:${thread.alias}`,
			key: thread.alias,
			card: { head: `${label} · ${thread.title}`, description: [unread.replace(" · ", ""), closed.replace(" · ", "")].filter(part => part !== "").join(" · ") },
		};
	}

	private messageRow(message: ViewerMessage, selected: boolean): BodyRow {
		const marker = message.direction === "out" ? "→" : "←";
		const author = message.author.split(":").pop() ?? message.author;
		const relation = message.relation === undefined ? "" : ` · ${message.relation}`;
		// A message rebuilt from an outbound receipt has no send time stored beside it.
		const at = clockText(message.at);
		const head = `${at === "" ? author : `${at} · ${author}`}${relation}`;
		return {
			left: [[marker, message.direction === "out" ? "accent" : "muted"]],
			right: [
				[head, "muted"],
				[` · ${oneLine(message.text)}`, ""],
			],
			selected,
			act: `onlyne.message:${message.seq}`,
			key: message.seq,
			card: {
				head: `${marker} ${head}`,
				description: oneLine(message.text),
			},
		};
	}

	/**
	 * The chat page as two columns. The rows fallback pairs threads and messages line by
	 * line, which a message card cannot survive: the card is a head over a body line, so it
	 * covers the row below it. Here each column stacks its own cards instead, and the thread
	 * column is a `section` the terminal folds.
	 */
	private chatBody(cx: DescribeContext, layout: { threadHeight: number; messageHeight: number; threadWidth: number }): NativeNode | undefined {
		const threads = this.threads();
		if (threads.length === 0) return undefined;
		const thread = threads[this.threadIndex] ?? threads[0]!;
		const threadWindow = this.window("threads", threads, this.threadIndex, layout.threadHeight);
		const threadCards = threadWindow.items.map((candidate, offset) => {
			const row = this.threadRow(candidate, threadWindow.start + offset === this.threadIndex);
			const card = row.card!;
			return node(
				"card",
				{
					...(row.selected === true ? {} : { variant: "bare" as const }),
					selected: row.selected === true,
					actions: { click: row.act! },
					title: card.head,
				},
				[
					textNode([span(card.head, "strong")], { wrap: "none", truncate: "end", lines: 1, shrink: 1, min: { w: 0 } }),
					...(card.description === ""
						? []
						: [textNode([span(card.description, "muted")], { wrap: "none", truncate: "end", lines: 1, shrink: 1, min: { w: 0 } })]),
				],
				candidate.alias,
			);
		});
		// Each message card is a head line over a body line: two rows of the list.
		const visible = Math.max(1, Math.floor(layout.messageHeight / 2));
		const messageWindow = this.window("messages", thread.messages, this.messageIndex, visible);
		const messageCards = messageWindow.items.map((message, offset) => {
			const selected = messageWindow.start + offset === this.messageIndex;
			const card = this.messageRow(message, selected).card!;
			return node(
				"card",
				{
					...(selected ? {} : { variant: "bare" as const }),
					selected,
					actions: { click: `onlyne.message:${message.seq}` },
					title: card.head,
				},
				[
					textNode([span(card.head, "muted")], { wrap: "none", truncate: "end", lines: 1, shrink: 1, min: { w: 0 } }),
					textNode([span(card.description)], { wrap: "none", truncate: "end", lines: 1, shrink: 1, min: { w: 0 } }),
				],
				message.seq,
			);
		});
		// Folded, the section keeps its head so the same click brings the list back.
		const columns: NativeNode[] = [
			node(
				"section",
				{
					head: `线程 · ${threads.length}`,
					collapsible: true,
					collapsed: this.threadFolded,
					...(this.threadFolded
						? {}
						: {
								max: { w: `${layout.threadWidth}ch` as const, h: `${layout.threadHeight}lines` as const },
								shrink: 0,
							}),
				},
				threadCards,
				"threads",
			),
		];
		columns.push(
			node(
				"col",
				{
					grow: 1,
					shrink: 1,
					basis: 0 as const,
					max: { h: `${layout.messageHeight}lines` as const },
				},
				messageCards,
				"messages",
			),
		);
		// The rows fallback fills exactly bodyHeight rows; the columns hug their content,
		// so the chat row carries the height the page owes the frame.
		const chatHeight = Math.max(layout.threadHeight, layout.messageHeight);
		return node(
			"row",
			{
				gap: "sm",
				align: "start",
				role: "onlyne.chat",
				min: { h: `${chatHeight}lines` as const },
				max: { h: `${chatHeight}lines` as const },
			},
			columns,
			"chat",
		);
	}

	/**
	 * The discovery page as two columns, mirroring the chat body: the domains are a
	 * `section` on the left, the node checklist grows beside it. A node card is a
	 * head line over its address, so a window of floor(height / 2) fits.
	 */
	private discoveryBody(layout: { threadWidth: number }): NativeNode | undefined {
		const groups = this.discoveryDomains();
		if (groups.length === 0) return undefined;
		const body = this.bodyHeight();
		const domainWindow = this.window("domains", groups, this.domainIndex, Math.max(1, Math.floor((body - 1) / 2)));
		const domainCards = domainWindow.items.map((group, offset) => {
			const index = domainWindow.start + offset;
			const row = this.domainRow(group, index === this.domainIndex, index);
			const card = row.card!;
			return node(
				"card",
				{
					...(row.selected === true ? {} : { variant: "bare" as const }),
					selected: row.selected === true,
					actions: { click: row.act! },
					title: card.head,
				},
				[
					textNode([span(card.head, "strong")], { wrap: "none", truncate: "end", lines: 1, shrink: 1, min: { w: 0 } }),
					textNode([span(card.description, "muted")], { wrap: "none", truncate: "end", lines: 1, shrink: 1, min: { w: 0 } }),
				],
				group.id ?? "no-domain",
			);
		});
		const refresh = node(
			"card",
			{ variant: "bare" as const, actions: { click: "onlyne.discovery.refresh" }, title: "刷新发现" },
			[textNode([span("刷新发现", "strong")], { wrap: "none", truncate: "end", lines: 1, shrink: 1, min: { w: 0 } })],
			"refresh",
		);
		const group = groups[this.domainIndex] ?? groups[0]!;
		const nodes = group.nodes;
		const nodeWindow = this.window("discovery-nodes", nodes, this.nodeIndex, Math.max(1, Math.floor(body / 2)));
		const nodeCards = nodes.length === 0
			? [textNode([span("该域没有发现节点。", "muted")])]
			: nodeWindow.items.map((record, offset) => {
					const index = nodeWindow.start + offset;
					const row = this.discoveryNodeRow(record, index === this.nodeIndex, index);
					const card = row.card!;
					return node(
						"card",
						{
							...(row.selected === true ? {} : { variant: "bare" as const }),
							selected: row.selected === true,
							actions: { click: row.act! },
							title: card.head,
						},
						[
							textNode([span(card.head, "strong")], { wrap: "none", truncate: "end", lines: 1, shrink: 1, min: { w: 0 } }),
							textNode([span(card.description, "muted")], { wrap: "none", truncate: "end", lines: 1, shrink: 1, min: { w: 0 } }),
						],
						record.node_id,
					);
				});
		return node(
			"row",
			{
				gap: "sm",
				align: "start",
				role: "onlyne.discovery",
				min: { h: `${body}lines` as const },
				max: { h: `${body}lines` as const },
			},
			[
				node(
					"section",
					{
						head: `域 · ${groups.length}`,
						max: { w: `${layout.threadWidth}ch` as const, h: `${body}lines` as const },
						shrink: 0,
					},
					[refresh, ...domainCards],
					"domains",
				),
				node(
					"col",
					{ grow: 1, shrink: 1, basis: 0 as const, max: { h: `${body}lines` as const } },
					nodeCards,
					"discovery-nodes",
				),
			],
			"discovery",
		);
	}

	/** The selected message as one ANSI block: a dim head, then the body with its escapes dropped. */
	private detailText(): string | undefined {
		const message = this.selectedThread()?.messages[this.messageIndex];
		if (message === undefined) return undefined;
		const body = message.text.replace(/\x1b\[[0-9;?]*[A-Za-z]/g, "");
		return `${this.theme.fg("dim", this.messageHead(message))}\n${body}`;
	}

	/** The rows fallback for a terminal without the native surface: the head plus the body's first lines. */
	private detailRows(count: number, columnWidth: number): BodyRow[] {
		if (count <= 0) return [];
		const message = this.selectedThread()?.messages[this.messageIndex];
		if (message === undefined) return Array.from({ length: count }, () => ({ left: [] as Cell[] }));
		const lines = wrapTextWithAnsi(message.text, Math.max(20, columnWidth - 2)).slice(0, Math.max(1, count - 1));
		const rows: BodyRow[] = [{ left: [["↳ ", "muted"], [this.messageHead(message), "dim"]], key: "detail-head" }];
		for (const line of lines) rows.push({ left: [[line]], key: `detail-body-${rows.length}` });
		return rows.slice(0, count);
	}

	private chatLayout(width: number): { stacked: boolean; threadHeight: number; messageHeight: number; detailHeight: number; threadWidth: number } {
		const body = this.bodyHeight();
		const detail = this.expanded && (this.selectedThread()?.messages.length ?? 0) > 0 ? Math.max(4, Math.floor(body / 3)) : 0;
		if (width >= 92) {
			const detailHeight = Math.min(detail, Math.max(0, body - 1));
			return {
				stacked: false,
				threadHeight: body,
				messageHeight: Math.max(1, body - detailHeight),
				detailHeight,
				threadWidth: Math.max(18, Math.min(34, Math.floor(width * 0.3))),
			};
		}
		const threadHeight = Math.max(2, Math.min(6, Math.floor(body * 0.3)));
		const area = Math.max(1, body - threadHeight - 2);
		const detailHeight = Math.min(detail, Math.max(0, area - 1));
		return { stacked: true, threadHeight, messageHeight: Math.max(1, area - detailHeight), detailHeight, threadWidth: width };
	}

	private chatRows(width: number): BodyRow[] {
		const threads = this.threads();
		if (threads.length === 0) return [{ left: [["没有 thread。", "muted"]] }];
		const layout = this.chatLayout(width);
		const thread = threads[this.threadIndex] ?? threads[0]!;
		const messages = thread.messages;
		const threadWindow = this.window("threads", threads, this.threadIndex, layout.threadHeight);
		const threadRows = threadWindow.items.map((candidate, offset) =>
			this.threadRow(candidate, threadWindow.start + offset === this.threadIndex),
		);
		const messageWindow = this.window("messages", messages, this.messageIndex, layout.messageHeight);
		const messageRows = messageWindow.items.map((message, offset) =>
			this.messageRow(message, messageWindow.start + offset === this.messageIndex),
		);
		// The band lives in the message column, so it wraps to that column's width.
		const detailRows = this.detailRows(layout.detailHeight, layout.stacked ? width : width - layout.threadWidth - 2);

		if (layout.stacked || this.threadFolded) {
			const rows: BodyRow[] = [{ left: [[`Threads · ${threads.length}`, "muted"]], key: "threads-head" }];
			rows.push(...threadRows);
			rows.push({ left: [[`Messages · ${shortAlias(thread.alias)} · ${thread.title}`, "muted"]], key: "messages-head" });
			// One column here: the marker leads the message's own line instead of a column of its own.
			rows.push(
				...(messages.length === 0
					? [{ left: [["没有消息。", "muted"]] } as BodyRow]
					: messageRows.map(row => ({ ...row, left: [...row.left, ...(row.right ?? [])], right: undefined }))),
			);
			// The band belongs at the foot of the body, whatever the list length.
			for (let pad = rows.length + detailRows.length; pad < this.bodyHeight(); pad++) rows.push({ left: [] });
			rows.push(...detailRows);
			return rows;
		}

		const height = this.bodyHeight();
		const rightRows: BodyRow[] = messages.length === 0 ? [{ left: [["没有消息。", "muted"]] }] : [...messageRows];
		// Pad before the band so it sits at the bottom of the body, above the footer.
		for (let pad = rightRows.length + detailRows.length; pad < height; pad++) rightRows.push({ left: [] });
		rightRows.push(...detailRows);
		const rows: BodyRow[] = [];
		for (let index = 0; index < height; index++) {
			const left = threadRows[index];
			const right = rightRows[index];
			rows.push({
				left: left?.left ?? [],
				// A message row is marker + head + body; the column beside the threads takes
				// all three, or the right side shows a direction arrow and nothing else.
				right: [...(right?.left ?? []), ...(right?.right ?? [])],
				leftWidth: layout.threadWidth,
				leftAct: left?.act,
				rightAct: right?.act,
				selected: right?.selected === true,
				card: right?.card,
				key: `${left?.key ?? ""}/${right?.key ?? ""}`,
			});
		}
		return rows;
	}

	/** Discovery records grouped by the domain they declare; records without one group last. */
	private discoveryDomains(): DiscoveryGroup[] {
		const groups = new Map<string, ViewerDiscovery[]>();
		for (const record of this.snapshot?.discovery ?? []) {
			const id = record.region ?? "";
			const nodes = groups.get(id);
			if (nodes === undefined) groups.set(id, [record]);
			else nodes.push(record);
		}
		return [...groups.entries()]
			.map(([id, nodes]) => ({
				...(id === "" ? {} : { id }),
				label: id === "" ? "未入域" : this.domainLabel(id),
				nodes: [...nodes].sort((left, right) => left.node_id.localeCompare(right.node_id) || left.base_url.localeCompare(right.base_url)),
			}))
			.sort((left, right) => {
				if ((left.id === undefined) !== (right.id === undefined)) return left.id === undefined ? 1 : -1;
				return left.label.localeCompare(right.label);
			});
	}

	/** The columns show the bare domain label; the full id stays in the snapshot. */
	private domainLabel(id: string): string {
		return id.startsWith("urn:onlyne:region:") ? id.slice("urn:onlyne:region:".length) : id;
	}

	private trustFor(nodeId: string): ViewerTrust | undefined {
		return this.snapshot?.trust?.find(entry => entry.node_id === nodeId);
	}

	private domainRow(group: DiscoveryGroup, selected: boolean, index: number): BodyRow {
		return {
			left: [[group.label, "strong"], [` · ${group.nodes.length} 节点`, "muted"]],
			selected,
			act: `onlyne.domain:${index}`,
			key: group.id ?? "no-domain",
			card: { head: group.label, description: `${group.nodes.length} 节点` },
		};
	}

	private discoveryNodeRow(record: ViewerDiscovery, selected: boolean, index: number): BodyRow {
		const checked = this.checked.has(record.node_id);
		const trust = this.trustFor(record.node_id);
		const glyph = trust === undefined ? "" : trust.state === "trusted" ? "✓" : trust.state === "pending" ? "◌" : "✕";
		const cells: Cell[] = [
			[checked ? "[x] " : "[ ] ", checked ? "success" : "muted"],
			[shortNodeId(record.node_id), "strong"],
			[` · ${record.base_url}`, "path"],
			[` · ${record.source}`, "muted"],
		];
		if (glyph !== "") cells.push([` · ${glyph}`, trust?.state === "trusted" ? "success" : trust?.state === "pending" ? "accent" : "error"]);
		if (record.error !== undefined) cells.push([` · ⚠ ${oneLine(record.error)}`, "error"]);
		const description = [record.base_url, record.source, glyph, record.error === undefined ? "" : `⚠ ${oneLine(record.error)}`]
			.filter(part => part !== "")
			.join(" · ");
		return {
			left: cells,
			selected,
			act: `onlyne.node:${index}`,
			key: `${record.node_id}/${record.base_url}`,
			card: {
				head: `${checked ? "[x]" : "[ ]"} ${shortNodeId(record.node_id)}`,
				description,
				tone: record.error === undefined ? undefined : "warning",
			},
		};
	}

	private discoveryRows(): BodyRow[] {
		const rows: BodyRow[] = [
			{
				left: [
					["刷新发现", "strong"],
					[" · 重新验证可达节点", "muted"],
				],
				act: "onlyne.discovery.refresh",
				key: "refresh",
			},
		];
		const groups = this.discoveryDomains();
		if (groups.length === 0) {
			rows.push({ left: [["没有发现记录。", "muted"]] });
			return rows;
		}
		const group = groups[this.domainIndex] ?? groups[0]!;
		rows.push({
			left: [
				[`域 ${this.domainIndex + 1}/${groups.length} · `, "muted"],
				[group.label, "strong"],
				[" · [ / ] 切换域", "muted"],
			],
			key: "domain-head",
		});
		if (group.nodes.length === 0) {
			rows.push({ left: [["该域没有发现节点。", "muted"]] });
			return rows;
		}
		const height = Math.max(1, this.bodyHeight() - 2);
		const { start, items } = this.window("discovery-nodes", group.nodes, this.nodeIndex, height);
		for (const [offset, record] of items.entries()) {
			rows.push(this.discoveryNodeRow(record, start + offset === this.nodeIndex, start + offset));
		}
		return rows;
	}

	/** Contacts plus every pending trust invite whose contact record is already gone. */
	private contactSources(): { node_id: string; base_url?: string; region?: string; last_seen?: string }[] {
		const contacts = this.snapshot?.contacts ?? [];
		const seen = new Set(contacts.map(contact => contact.node_id));
		const invites = (this.snapshot?.trust ?? []).filter(entry => entry.state === "pending" && !seen.has(entry.node_id));
		return [
			...contacts.map(contact => ({
				node_id: contact.node_id,
				base_url: contact.base_url,
				...(contact.region === undefined ? {} : { region: contact.region }),
				last_seen: contact.last_seen,
			})),
			...invites.map(entry => ({ node_id: entry.node_id })),
		];
	}

	private contactRows(): BodyRow[] {
		const sources = this.contactSources();
		if (sources.length === 0) return [{ left: [["没有可达联系人。", "muted"]] }];
		const { start, items } = this.window("contacts", sources, this.contactIndex, this.bodyHeight());
		return items.map((source, offset) => {
			const index = start + offset;
			const trust = this.trustFor(source.node_id);
			const checked = this.checked.has(source.node_id);
			const age = source.last_seen === undefined ? "" : ageText(source.last_seen);
			const glyph = trust === undefined ? "" : trust.state === "trusted" ? "✓" : trust.state === "pending" ? "◌" : "✕";
			const cells: Cell[] = [[index === this.contactIndex ? "> " : "  ", "muted"]];
			if (trust?.state === "pending") cells.push([checked ? "[x] " : "[ ] ", checked ? "success" : "muted"]);
			cells.push([shortNodeId(source.node_id), "strong"]);
			if (source.region !== undefined) cells.push([` · ${this.domainLabel(source.region)}`, "muted"]);
			if (source.base_url !== undefined) cells.push([` · ${source.base_url}`, "path"]);
			if (age !== "") cells.push([` · ${age}`, "muted"]);
			if (glyph !== "") cells.push([` · ${glyph}`, trust?.state === "trusted" ? "success" : trust?.state === "pending" ? "accent" : "error"]);
			const description = [
				source.region === undefined ? "" : this.domainLabel(source.region),
				source.base_url ?? "",
				age,
				trust?.state === "pending" ? "◌ 待确认" : glyph,
			].filter(part => part !== "").join(" · ");
			return {
				left: cells,
				selected: index === this.contactIndex,
				act: `onlyne.contacts:${index}`,
				key: `${source.node_id}/${source.base_url ?? ""}`,
				card: { head: shortNodeId(source.node_id), description },
			};
		});
	}

	/** Exactly `bodyHeight()` rows, padded with blanks so the frame never resizes. */
	private bodyRows(width: number): BodyRow[] {
		const height = this.bodyHeight();
		let rows: BodyRow[];
		if (this.loading) rows = [{ left: [["加载中…", "muted"]] }];
		else if (this.error !== undefined) rows = [{ left: [[this.error, "error"]] }];
		else if (this.page === "queue") rows = this.queueRows();
		else if (this.page === "chat") rows = this.chatRows(width);
		else if (this.page === "discovery") rows = this.discoveryRows();
		else rows = this.contactRows();
		while (rows.length < height) rows.push({ left: [] });
		rows.length = height;
		return rows;
	}

	private styleCells(cells: readonly Cell[], selected: boolean): string {
		const text = cells.map((cell) => cell[0]).join("");
		if (cells.some((cell) => cell[1] === "error")) return this.theme.fg("error", text);
		return selected ? this.theme.bold(text) : text;
	}

	private renderRow(row: BodyRow, width: number): string {
		if (row.right !== undefined) {
			const leftWidth = Math.min(Math.max(1, row.leftWidth ?? 20), Math.max(1, width - 2));
			const rightWidth = Math.max(1, width - leftWidth - 2);
			return `${pad(this.styleCells(row.left, row.selected === true), leftWidth)}  ${fit(this.styleCells(row.right, false), rightWidth)}`;
		}
		return fit(this.styleCells(row.left, row.selected === true), width);
	}

	private spans(cells: readonly Cell[]): TspSpan[] {
		if (cells.length === 0) return [span(" ")];
		return cells.map((cell) => span(cell[0], cell[1]));
	}

	private titleNode(): NativeNode {
		// No `Onlyne` copy: the sheet head carries it. This row holds the transient
		// notice only, and is omitted entirely when there is none.
		if (this.notice === undefined) return node("col", {}, [], "title");
		return node(
			"row",
			{ gap: "sm", align: "baseline" },
			[textNode([span(this.notice.text, this.notice.error ? "error" : "accent")], { grow: 1, truncate: "end" })],
			"title",
		);
	}

	/** The summary wraps under the title, so a long region URI never squeezes the tabs. */
	private summaryNode(): NativeNode {
		return node(
			"col",
			{},
			[textNode([span(this.summaryText(), "muted")], { wrap: "word", truncate: "end" })],
			"summary",
		);
	}

	private tabsNode(): NativeNode {
		const tabs = PAGES.map((page, index) =>
			node(
				"row",
				{
					role: "omp.btn",
					actions: { click: `onlyne.page.${page.id}` },
					title: `${index + 1} ${page.label}`,
					tone: page.id === this.page ? "accent" : undefined,
				},
				[textNode([span(`${index + 1} ${page.label}`, page.id === this.page ? "strong" : "muted")])],
				`tab-${page.id}`,
			),
		);
		// Each tab grows to an equal share, so the row is filled rather than left-packed.
		const tabRow = node(
			"row",
			{ gap: "sm", align: "center" },
			tabs.map((tab, index) => node("col", { key: `tab-wrap-${index}`, grow: 1 }, [tab], `tab-wrap-${index}`)),
			"tabs",
		);
		return tabRow;
	}

	private describeRow(row: BodyRow, index: number): NativeNode {
		// Every selectable row is a bare card: `selected` only exists on `card`, and a
		// `tone: accent` fills the row like a button. The terminal draws `selected: true`
		// as a quiet background band instead.
		const asCard = row.card ?? {
			head: row.left.map(cell => cell[0]).join(""),
			description: "",
		};
		const actions = row.act ?? row.leftAct;
		if (row.right === undefined) {
			return node(
				"card",
				{
					...(row.selected === true ? {} : { variant: "bare" as const }),
					selected: row.selected === true,
					...(actions === undefined ? {} : { actions: { click: actions } }),
					...(actions === undefined ? {} : { title: asCard.head }),
				},
				[
					// One head row plus one clamped description row keeps the frame at bodyHeight;
					// the ANSI row already carries the same text as a single line.
					textNode([span(asCard.head, "strong")], { wrap: "none", truncate: "end", lines: 1, shrink: 1, min: { w: 0 } }),
					...(asCard.description === "" ? [] : [textNode([span(asCard.description, "muted")], { wrap: "none", truncate: "end", lines: 1, shrink: 1, min: { w: 0 } })]),
				],
				row.key ?? `row-${index}`,
			);
		}
		// Two-column chat rows: each column is its own bare card so either side can show
		// a selection band. The left column is capped at the width the ANSI layout gives
		// it and the right one grows into the rest; without both bounds a long line widens
		// its card past the sheet and the row pushes past the column beside it.
		const leftBound = row.leftWidth === undefined ? {} : { max: { w: `${row.leftWidth}ch` as const } };
		// A message keeps its head-over-body card on the right; the thread column beside it
		// is a single line, and only the message carries the selection there.
		const rightChildren =
			row.card === undefined
				? [textNode(this.spans(row.right ?? []), { wrap: "none", truncate: "end", grow: 1, shrink: 1, min: { w: 0 } })]
				: [
						textNode([span(row.card.head, "muted")], { wrap: "none", truncate: "end", lines: 1, shrink: 1, min: { w: 0 } }),
						...(row.card.description === ""
							? []
							: [textNode([span(row.card.description)], { wrap: "none", truncate: "end", lines: 1, shrink: 1, min: { w: 0 } })]),
					];
		return node(
			"row",
			{ gap: "sm", align: "start" },
			[
				node("card", {
					variant: "bare",
					shrink: 1,
					...leftBound,
					...(row.leftAct === undefined ? {} : { actions: { click: row.leftAct } }),
				}, [textNode(this.spans(row.left), { wrap: "none", truncate: "end", shrink: 1, min: { w: 0 } })], `${row.key ?? index}/left`),
				node("card", {
					...(row.selected === true ? {} : { variant: "bare" as const }),
					selected: row.selected === true,
					grow: 1,
					shrink: 1,
					basis: 0 as const,
					...(row.rightAct === undefined ? {} : { actions: { click: row.rightAct } }),
				}, rightChildren, `${row.key ?? index}/right`),
			],
			row.key ?? `row-${index}`,
		);
	}

	private footerNode(): NativeNode {
		return node(
			"row",
			{ gap: "sm", align: "center" },
			[
				textNode([span(FOOTER, "muted")], { grow: 1, truncate: "end" }),
				node("row", { role: "omp.btn", actions: { click: "onlyne.close" }, title: "关闭" }, [textNode("关闭")], "close"),
			],
			"footer",
		);
	}

	describe(cx: DescribeContext): NativeNode {
		this.lastWidth = cx.cols;
		const height = this.bodyHeight();
		const key = `${this.version}:${cx.cols}:${height}`;
		if (this.described !== undefined && this.describedKey === key) return this.described;
		// The native surface takes the detail band as one `ansi` block: the terminal draws it
		// as a mini terminal and scrolls it itself (the jobs sheet shows job output the same
		// way), so the whole message stays reachable without the viewer windowing it.
		const layout = this.chatLayout(cx.cols);
		const chatPage = this.page === "chat";
		const detail = chatPage && layout.detailHeight > 0 ? this.detailText() : undefined;
		const chat = chatPage ? this.chatBody(cx, layout) : undefined;
		// The discovery page takes the same two-column body; below the chat width it
		// falls back to the single-column rows like every other page.
		const discovery = this.page === "discovery" && !layout.stacked ? this.discoveryBody(layout) : undefined;
		const body = chat ?? discovery;
		const rows = body === undefined ? this.bodyRows(cx.cols).filter(row => detail === undefined || row.key?.includes("detail-") !== true) : [];
		const children: NativeNode[] = [
			this.titleNode(),
			this.summaryNode(),
			this.tabsNode(),
			...(body === undefined
				? rows.map((row, index) => this.describeRow(row, index))
				: [body, node("col", { grow: 1 }, [], "page-spacer")]),
			...(detail === undefined
				? []
				: [
						{
							...node(
								"ansi",
								{
									text: detail,
									cols: Math.max(20, (this.threadFolded ? cx.cols : cx.cols - layout.threadWidth) - 4),
									max: { h: `${layout.detailHeight}lines` as const },
									role: "onlyne.detail",
								},
								undefined,
								"detail",
							),
							...(this.detailScroll === undefined ? {} : { scroll: this.detailScroll }),
						},
					]),
			this.footerNode(),
		];
		this.described = node("card", { role: "onlyne.viewer" }, children);
		this.describedKey = key;
		return this.described;
	}

	handleNativeEvent(event: NativeUiEvent): void {
		if (this.disposed || this.signal.aborted) return;
		if (event.type === "toggle") {
			if (event.key === "threads" || event.key.endsWith("/threads")) {
				this.threadFolded = event.collapsed;
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
		if (act.startsWith("onlyne.page.")) {
			const id = act.slice("onlyne.page.".length) as Page;
			if (PAGES.some((page) => page.id === id)) this.switchPage(id);
			return;
		}
		if (act === "onlyne.discovery.refresh") {
			void this.refreshDiscovery();
			return;
		}
		if (act.startsWith("onlyne.domain:")) {
			const index = Number(act.slice("onlyne.domain:".length));
			if (!Number.isInteger(index) || index < 0) return;
			this.domainIndex = Math.min(index, Math.max(0, this.discoveryDomains().length - 1));
			this.nodeIndex = 0;
			this.offsets.delete("discovery-nodes");
			this.changed();
			return;
		}
		if (act.startsWith("onlyne.node:")) {
			const index = Number(act.slice("onlyne.node:".length));
			if (!Number.isInteger(index) || index < 0) return;
			this.nodeIndex = Math.min(index, Math.max(0, this.pageCount("discovery") - 1));
			this.toggleChecked();
			return;
		}
		if (act.startsWith("onlyne.contacts:")) {
			const index = Number(act.slice("onlyne.contacts:".length));
			if (!Number.isInteger(index) || index < 0) return;
			this.contactIndex = Math.min(index, Math.max(0, this.contactSources().length - 1));
			this.changed();
			return;
		}
		if (act.startsWith("onlyne.thread:")) {
			const alias = act.slice("onlyne.thread:".length);
			const index = this.threads().findIndex((thread) => thread.alias === alias);
			if (index >= 0) {
				this.threadIndex = index;
				this.messageIndex = 0;
				this.expanded = false;
				this.offsets.delete("messages");
				this.changed();
			}
			return;
		}
		if (act.startsWith("onlyne.queue:")) {
			const id = act.slice("onlyne.queue:".length);
			const index = this.queueItems().findIndex((item) => item.id === id);
			if (index >= 0) {
				this.queueIndex = index;
				this.changed();
			}
			return;
		}
		if (act.startsWith("onlyne.message:")) {
			const seq = act.slice("onlyne.message:".length);
			const messages = this.selectedThread()?.messages ?? [];
			const index = messages.findIndex((message) => message.seq === seq);
			if (index >= 0) {
				if (this.messageIndex === index) this.expanded = !this.expanded;
				else {
					this.messageIndex = index;
					this.expanded = true;
				}
				this.detailScroll = undefined;
				this.changed();
			}
		}
	}

	handleInput(data: string): void {
		if (this.disposed || this.signal.aborted) return;
		if (matchesKey(data, "escape") || matchesKey(data, "q")) {
			this.done();
			return;
		}
		if (matchesKey(data, "shift+tab")) {
			this.cyclePage(-1);
			return;
		}
		if (matchesKey(data, "tab")) {
			this.cyclePage(1);
			return;
		}
		if (matchesKey(data, "r")) {
			void this.reload();
			return;
		}
		const pageIndex = PAGES.findIndex((_page, index) => data === String(index + 1));
		if (pageIndex >= 0) {
			this.switchPage(PAGES[pageIndex]!.id);
			return;
		}
		if (this.page === "chat") this.handleChatKey(data);
		else this.handleListKey(this.page, data);
	}

	private cyclePage(delta: number): void {
		const index = PAGES.findIndex((page) => page.id === this.page);
		this.switchPage(PAGES[(index + delta + PAGES.length) % PAGES.length]!.id);
	}

	/** Every page change funnels here so an open confirmation never leaks across pages. */
	private switchPage(page: Page): void {
		if (page !== "discovery" && page !== "contacts") this.cancelExchange();
		this.page = page;
		this.changed();
	}

	private moveIndex(data: string, selection: number, count: number, height: number): number | undefined {
		const half = Math.max(1, Math.floor(height / 2));
		if (matchesKey(data, "down") || matchesKey(data, "j")) return Math.min(count - 1, selection + 1);
		if (matchesKey(data, "up") || matchesKey(data, "k")) return Math.max(0, selection - 1);
		if (matchesKey(data, "ctrl+d")) return Math.min(count - 1, selection + half);
		if (matchesKey(data, "ctrl+u")) return Math.max(0, selection - half);
		if (data === "g") return 0;
		if (data === "G") return count - 1;
		return undefined;
	}

	private handleListKey(page: Page, data: string): void {
		if ((page === "discovery" || page === "contacts") && this.handleConfirmKey(data)) return;
		if ((page === "discovery" || page === "contacts") && data === "x") {
			void this.revokeSelected();
			return;
		}
		if (page === "discovery") {
			if (matchesKey(data, "space")) {
				this.toggleChecked();
				return;
			}
			if (data === "[" || data === "]") {
				this.cycleDomain(data === "[" ? -1 : 1);
				return;
			}
		}
		if (page === "contacts" && matchesKey(data, "space")) {
			this.beginContactExchange();
			return;
		}
		const count = this.pageCount(page);
		if (count === 0) return;
		const next = this.moveIndex(data, this.pageSelection(page), count, this.pageWindowHeight(page));
		if (next === undefined) return;
		this.setSelection(page, next);
		this.changed();
	}

	/** y/n or enter resolves the open confirmation before any movement key is read. */
	private handleConfirmKey(data: string): boolean {
		if (this.pendingConfirm === undefined) return false;
		if (data === "n") {
			this.cancelExchange();
			return true;
		}
		if (data === "y" || matchesKey(data, "enter")) {
			void this.acceptExchange();
			return true;
		}
		return false;
	}

	/** x on a trusted or pending row calls trustRevoke; the confirm bar's r keeps n purely local. */
	private async revokeSelected(): Promise<void> {
		const nodeId = this.page === "discovery"
			? this.discoveryDomains()[this.domainIndex]?.nodes[this.nodeIndex]?.node_id
			: this.contactSources()[this.contactIndex]?.node_id;
		const runtime = getRuntime();
		if (nodeId === undefined || runtime === undefined) return;
		this.notice = { text: `正在撤销 ${shortNodeId(nodeId)} 的信任…`, error: false };
		this.changed();
		try {
			await runtime.trustRevoke(nodeId);
			this.notice = { text: `已撤销 ${shortNodeId(nodeId)} 的信任`, error: false };
		} catch (error) {
			this.notice = { text: `撤销失败：${errorText(error)}`, error: true };
		}
		await this.reload();
		this.clearNoticeLater();
	}

	private handleChatKey(data: string): void {
		const threads = this.threads();
		if (threads.length === 0) return;
		if (data === "[" || data === "]") {
			const delta = data === "[" ? -1 : 1;
			this.threadIndex = (this.threadIndex + delta + threads.length) % threads.length;
			this.messageIndex = 0;
			this.expanded = false;
			this.detailScroll = undefined;
			this.offsets.delete("messages");
			this.changed();
			return;
		}
		const messages = threads[this.threadIndex]?.messages ?? [];
		if (data === "t") {
			this.threadFolded = !this.threadFolded;
			this.changed();
			return;
		}
		if (matchesKey(data, "enter")) {
			if (messages.length === 0) return;
			this.expanded = !this.expanded;
			this.detailScroll = undefined;
			this.changed();
			return;
		}
		// While the band is open the movement keys move through the message instead of the
		// list: the band shows a window of the wrapped body and this is what shifts it.
		if (this.expanded && messages.length > 0) {
			const by = matchesKey(data, "up") || matchesKey(data, "k")
				? "line-up" as const
				: matchesKey(data, "down") || matchesKey(data, "j")
					? "line-down" as const
					: matchesKey(data, "pageUp")
						? "page-up" as const
						: matchesKey(data, "pageDown")
							? "page-down" as const
							: data === "g"
								? "start" as const
								: data === "G"
									? "end" as const
									: undefined;
			if (by !== undefined) {
				this.detailScroll = { by, n: (this.detailScroll?.n ?? 0) + 1 };
				this.changed();
				return;
			}
		}
		this.handleListKey("chat", data);
	}

	private cycleDomain(delta: number): void {
		const count = this.discoveryDomains().length;
		if (count === 0) return;
		this.domainIndex = (this.domainIndex + delta + count) % count;
		this.nodeIndex = 0;
		this.offsets.delete("discovery-nodes");
		this.changed();
	}

	private toggleChecked(): void {
		const record = this.discoveryDomains()[this.domainIndex]?.nodes[this.nodeIndex];
		if (record === undefined) return;
		if (this.checked.has(record.node_id)) {
			this.checked.delete(record.node_id);
			if (this.pendingConfirm?.node_id === record.node_id) this.pendingConfirm = undefined;
			this.changed();
			return;
		}
		void this.beginExchange(record);
	}

	/**
	 * Space on a discovery node: tick it as local consent, verify the peer's key
	 * (RFC 0017 §4 — the discovery record carries no key of its own), then ask for
	 * the y/n confirmation with both fingerprints on screen.
	 */
	private async beginExchange(record: ViewerDiscovery): Promise<void> {
		this.checked.add(record.node_id);
		this.pendingConfirm = undefined;
		this.changed();
		const runtime = getRuntime();
		if (runtime === undefined) {
			this.checked.delete(record.node_id);
			this.notice = { text: "未配置 Onlyne。", error: true };
			this.changed();
			return;
		}
		let fingerprint = this.trustFor(record.node_id)?.fingerprint;
		if (fingerprint === undefined) {
			this.notice = { text: `正在校验 ${shortNodeId(record.node_id)} 的公钥…`, error: false };
			this.changed();
			try {
				const inspected = await runtime.trustInspect(record.node_id);
				if (this.disposed || this.signal.aborted || !this.checked.has(record.node_id)) return;
				fingerprint = inspected.fingerprint;
			} catch (error) {
				if (this.disposed || this.signal.aborted) return;
				this.checked.delete(record.node_id);
				this.notice = { text: `校验失败：${errorText(error)}`, error: true };
				this.changed();
				this.clearNoticeLater();
				return;
			}
		}
		const pending: PendingExchange = {
			node_id: record.node_id,
			fingerprint,
			...(record.region === undefined ? {} : { domain: record.region }),
		};
		this.pendingConfirm = pending;
		this.notice = { text: this.confirmText(pending), error: false };
		this.changed();
	}

	/** Space on a contact with a pending exchange confirms it; the invite already carries the key. */
	private beginContactExchange(): void {
		const source = this.contactSources()[this.contactIndex];
		if (source === undefined) return;
		const trust = this.trustFor(source.node_id);
		if (trust?.state !== "pending") return;
		if (this.checked.has(source.node_id)) {
			this.checked.delete(source.node_id);
			if (this.pendingConfirm?.node_id === source.node_id) this.pendingConfirm = undefined;
			this.changed();
			return;
		}
		this.checked.add(source.node_id);
		const pending: PendingExchange = {
			node_id: source.node_id,
			fingerprint: trust.fingerprint,
			...(source.region === undefined ? {} : { domain: source.region }),
		};
		this.pendingConfirm = pending;
		this.notice = { text: this.confirmText(pending), error: false };
		this.changed();
	}

	private confirmText(pending: PendingExchange): string {
		const own = this.snapshot?.identity?.fingerprint ?? "未配置";
		const where = pending.domain === undefined
			? `与 ${shortNodeId(pending.node_id)} 交换密钥`
			: `该节点处于 ${this.domainLabel(pending.domain)} 域中`;
		return `${where} · 对方指纹 ${pending.fingerprint} · 本机指纹 ${own} · 确认交换密钥 y/n`;
	}

	/** Local consent goes to the runtime; the peer still has to approve, so the row stays pending. */
	private async acceptExchange(): Promise<void> {
		const pending = this.pendingConfirm;
		if (pending === undefined) return;
		this.pendingConfirm = undefined;
		const runtime = getRuntime();
		if (runtime === undefined) {
			this.notice = { text: "未配置 Onlyne。", error: true };
			this.changed();
			return;
		}
		this.notice = { text: `正在提交 ${shortNodeId(pending.node_id)} 的密钥交换…`, error: false };
		this.changed();
		try {
			await runtime.trustAccept(pending.node_id, pending.fingerprint);
			// A confirm bar opened while the round-trip ran keeps the headline.
			if (this.pendingConfirm === undefined) {
				this.notice = { text: `已同意交换，等待 ${shortNodeId(pending.node_id)} 确认`, error: false };
				this.changed();
			}
		} catch (error) {
			this.checked.delete(pending.node_id);
			if (this.pendingConfirm === undefined) {
				this.notice = { text: `交换失败：${errorText(error)}`, error: true };
				this.changed();
			}
		}
		await this.reload();
		this.clearNoticeLater();
	}

	private cancelExchange(): void {
		const pending = this.pendingConfirm;
		if (pending === undefined) return;
		this.pendingConfirm = undefined;
		this.checked.delete(pending.node_id);
		this.notice = undefined;
		this.changed();
	}

	private async refreshDiscovery(): Promise<void> {
		const runtime = getRuntime();
		this.notice = { text: "刷新发现…", error: false };
		this.changed();
		if (runtime === undefined) {
			this.notice = { text: "未配置 Onlyne。", error: true };
			this.changed();
			return;
		}
		try {
			await runtime.refreshDiscovery();
			this.notice = { text: "发现已刷新", error: false };
		} catch (error) {
			this.notice = { text: `刷新失败：${errorText(error)}`, error: true };
		}
		await this.reload();
		this.clearNoticeLater();
	}

	private clearNoticeLater(): void {
		clearTimeout(this.noticeTimer);
		this.noticeTimer = setTimeout(() => {
			this.noticeTimer = undefined;
			if (this.disposed || this.signal.aborted) return;
			this.notice = undefined;
			this.changed();
		}, 5_000);
	}

	render(width: number): readonly string[] {
		this.lastWidth = width;
		const key = `${this.version}:${width}`;
		if (this.rendered !== undefined && this.rendered.key === key) return this.rendered.lines;
		// The ANSI path has no title row: a notice takes the summary's line so the
		// confirmation bar stays visible and the frame keeps its height.
		const header = this.notice === undefined ? this.summaryText() : this.notice.text;
		const lines: string[] = [fit(header, width), fit(this.tabLine(width), width)];
		for (const row of this.bodyRows(width)) lines.push(this.renderRow(row, width));
		lines.push(fit(FOOTER, width));
		this.rendered = { key, lines };
		return this.rendered.lines;
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
		this.described = undefined;
		this.rendered = undefined;
	}
}
