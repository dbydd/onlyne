import {
	type Component,
	type DescribeContext,
	type NativeChild,
	type NativeNode,
	type NativeUiEvent,
	Text,
	type Theme,
	matchesKey,
	node,
	span,
	text as textNode,
} from "@oh-my-pi/pi-tui";
import { getRuntime } from "../runtime.ts";
import type { QueueItem, ViewerMessage, ViewerSnapshot, ViewerThread } from "../types.ts";
import { ageText, clockText, fit, kindLabel, kindRank, oneLine, pad } from "./format.ts";

type Page = "queue" | "chat";

/** Read-only view of the current OMP process: queue first, threads and messages second. */
export class OnlyneViewer implements Component {
	private snapshot: ViewerSnapshot | undefined;
	private loading = true;
	private error: string | undefined;
	private page: Page = "queue";
	private queueIndex = 0;
	private threadIndex = 0;
	private messageIndex = 0;
	private expanded = false;
	private ages = new Map<string, string>();
	private described: NativeNode | undefined;
	private disposed = false;
	private generation = 0;
	focused = true;
	readonly nativeOverlay = { role: "onlyne.viewer", size: "lg" as const, head: "Onlyne" };

	constructor(
		private readonly theme: Theme,
		private readonly requestRender: () => void,
		private readonly done: () => void,
		private readonly signal: AbortSignal,
	) {
		void this.reload();
	}

	private changed(): void {
		if (this.disposed || this.signal.aborted) return;
		this.described = undefined;
		this.requestRender();
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
			this.error = error instanceof Error ? error.message : String(error);
		} finally {
			if (!this.disposed && !this.signal.aborted && generation === this.generation) {
				this.loading = false;
				this.changed();
			}
		}
	}

	private clampSelection(): void {
		const queue = this.queueItems();
		this.queueIndex = Math.min(this.queueIndex, Math.max(0, queue.length - 1));
		const threads = this.threads();
		this.threadIndex = Math.min(this.threadIndex, Math.max(0, threads.length - 1));
		const messages = threads[this.threadIndex]?.messages ?? [];
		this.messageIndex = Math.min(this.messageIndex, Math.max(0, messages.length - 1));
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

	private summary(): string {
		const threads = this.threads();
		const unread = threads.reduce((total, thread) => total + Math.max(0, thread.unread), 0);
		const contexts = this.snapshot?.contexts.length ?? 0;
		const region = this.snapshot?.region === undefined || this.snapshot.region.length === 0 ? "" : `${this.snapshot.region} · `;
		return `${region}${this.page === "queue" ? "1/2 队列" : "2/2 对话"} · ${threads.length} 个 thread · ${unread} 条未读 · ${contexts} 个上下文`;
	}

	private tabs(): NativeNode {
		const tab = (label: string, active: boolean, act: string): NativeNode =>
			node(
				"row",
				{ role: "omp.btn", actions: { click: act }, title: label, tone: active ? "accent" : undefined },
				[textNode(label)],
				act,
			);
		return node("row", { gap: "sm" }, [tab("1 队列", this.page === "queue", "onlyne.page.queue"), tab("2 对话", this.page === "chat", "onlyne.page.chat")]);
	}

	private describeQueue(): NativeChild[] {
		const items = this.queueItems();
		if (items.length === 0) return [textNode("没有待处理消息。")];
		const body: NativeChild[] = [];
		let index = 0;
		for (const kind of ["inbound", "outbound", "context", "decision"] as const) {
			const group = items.filter((item) => item.kind === kind);
			if (group.length === 0) continue;
			body.push(
				node(
					"text",
					{ spans: [span(kindLabel(kind), "strong"), span(` ${group.length}`, "muted")], wrap: "word" },
					undefined,
					`group-${kind}`,
				),
			);
			for (const item of group) {
				const selected = index === this.queueIndex;
				body.push(
					node(
						"col",
						{ gap: "xs", actions: { click: `onlyne.queue:${item.id}` }, tone: selected ? "accent" : undefined },
						[
							node("text", {
								spans: [
									span(selected ? "> " : "  ", "muted"),
									span(item.label, "strong"),
									span(item.at === undefined ? "" : ` · ${this.ages.get(item.id) ?? ""}`, "muted"),
								],
								wrap: "word",
							}),
							node("text", { text: oneLine(item.detail), wrap: "word" }),
						],
						item.id,
					),
				);
				index++;
			}
		}
		return body;
	}

	private describeMessages(thread: ViewerThread): NativeChild[] {
		if (thread.messages.length === 0) return [textNode("没有消息。")];
		return thread.messages.map((message, index) => {
			const selected = index === this.messageIndex;
			return node(
				"col",
				{ gap: "xs", actions: { click: `onlyne.message:${message.seq}` }, tone: selected ? "accent" : undefined },
				[
					node("text", {
						spans: [span(selected ? "> " : "  ", "muted"), span(this.messageHead(message), "strong")],
						wrap: "word",
					}),
					node("text", { text: selected && this.expanded ? message.text : oneLine(message.text), wrap: "word" }),
				],
				message.seq,
			);
		});
	}

	private messageHead(message: ViewerMessage): string {
		const direction = message.direction === "in" ? "in" : "out";
		const relation = message.relation === undefined ? "" : ` · ${message.relation}`;
		return `[${message.seq}] ${direction} · ${message.author} · ${clockText(message.at)}${relation}`;
	}

	private describeChat(cx: DescribeContext): NativeChild[] {
		const threads = this.threads();
		if (threads.length === 0) return [textNode("没有 thread。")];
		const thread = threads[this.threadIndex] ?? threads[0]!;
		const list: NativeChild[] = threads.map((candidate, index) => {
			const selected = index === this.threadIndex;
			const unread = candidate.unread > 0 ? ` · ${candidate.unread} 未读` : "";
			return node(
				"row",
				{ role: "omp.btn", actions: { click: `onlyne.thread:${candidate.alias}` }, title: candidate.alias, tone: selected ? "accent" : undefined },
				[
					node("text", {
						spans: [
							span(selected ? "> " : "  ", "muted"),
							span(candidate.alias, "strong"),
							span(` · ${candidate.title}${unread}`, "muted"),
						],
						wrap: "word",
					}),
				],
				candidate.alias,
			);
		});
		const messages = this.describeMessages(thread);
		if (cx.cols >= 92) {
			return [
				node("row", { gap: "md", align: "start" }, [
					node("col", { gap: "xs" }, list, "threads"),
					node("col", { gap: "xs" }, messages, "messages"),
				]),
			];
		}
		return [
			node("text", { spans: [span("Threads", "strong")] }, undefined, "threads-head"),
			node("col", { gap: "xs" }, list, "threads"),
			node("text", { spans: [span("Messages", "strong")] }, undefined, "messages-head"),
			node("col", { gap: "xs" }, messages, "messages"),
		];
	}

	private footer(): NativeChild {
		return node("col", { gap: "xs" }, [
			node("text", {
				text: "Tab 翻页 · j/k 移动 · Ctrl+d/u 半页 · [ ] 切换 thread · Enter 展开 · r 刷新 · Esc 关闭",
				wrap: "word",
			}),
			node("row", { role: "omp.btn", actions: { click: "onlyne.close" }, title: "关闭" }, [textNode("关闭")], "close"),
		]);
	}

	describe(cx: DescribeContext): NativeNode {
		if (this.described !== undefined) return this.described;
		const body: NativeChild[] = [this.tabs(), node("text", { text: this.summary(), wrap: "word" })];
		if (this.loading) body.push(textNode("加载中…"));
		else if (this.error !== undefined) body.push(node("text", { spans: [span(this.error, "error")], wrap: "word" }));
		else if (this.page === "queue") body.push(...this.describeQueue());
		else body.push(...this.describeChat(cx));
		body.push(this.footer());
		this.described = node("card", { role: "onlyne.viewer" }, body);
		return this.described;
	}

	handleNativeEvent(event: NativeUiEvent): void {
		if (this.disposed || this.signal.aborted || event.type !== "action") return;
		const act = event.act;
		if (act === "onlyne.close") {
			this.done();
			return;
		}
		if (act === "onlyne.page.queue" || act === "onlyne.page.chat") {
			this.page = act === "onlyne.page.queue" ? "queue" : "chat";
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
		if (matchesKey(data, "tab")) {
			this.page = this.page === "queue" ? "chat" : "queue";
			this.changed();
			return;
		}
		if (matchesKey(data, "shift+tab")) {
			this.page = this.page === "queue" ? "chat" : "queue";
			this.changed();
			return;
		}
		if (matchesKey(data, "r")) {
			void this.reload();
			return;
		}
		if (this.page === "queue") this.handleQueueKey(data);
		else this.handleChatKey(data);
	}

	private handleQueueKey(data: string): void {
		const items = this.queueItems();
		if (items.length === 0) return;
		const half = Math.max(1, Math.floor(items.length / 2));
		if (matchesKey(data, "down") || matchesKey(data, "j")) this.queueIndex = Math.min(items.length - 1, this.queueIndex + 1);
		else if (matchesKey(data, "up") || matchesKey(data, "k")) this.queueIndex = Math.max(0, this.queueIndex - 1);
		else if (matchesKey(data, "g") || data === "g") this.queueIndex = 0;
		else if (matchesKey(data, "shift+g") || data === "G") this.queueIndex = items.length - 1;
		else if (matchesKey(data, "ctrl+d")) this.queueIndex = Math.min(items.length - 1, this.queueIndex + half);
		else if (matchesKey(data, "ctrl+u")) this.queueIndex = Math.max(0, this.queueIndex - half);
		else return;
		this.changed();
	}

	private handleChatKey(data: string): void {
		const threads = this.threads();
		if (threads.length === 0) return;
		if (matchesKey(data, "[") || data === "[") {
			this.threadIndex = (this.threadIndex + threads.length - 1) % threads.length;
			this.messageIndex = 0;
			this.expanded = false;
			this.changed();
			return;
		}
		if (matchesKey(data, "]") || data === "]") {
			this.threadIndex = (this.threadIndex + 1) % threads.length;
			this.messageIndex = 0;
			this.expanded = false;
			this.changed();
			return;
		}
		const messages = threads[this.threadIndex]?.messages ?? [];
		if (messages.length === 0) return;
		const half = Math.max(1, Math.floor(messages.length / 2));
		if (matchesKey(data, "down") || matchesKey(data, "j")) this.messageIndex = Math.min(messages.length - 1, this.messageIndex + 1);
		else if (matchesKey(data, "up") || matchesKey(data, "k")) this.messageIndex = Math.max(0, this.messageIndex - 1);
		else if (matchesKey(data, "g") || data === "g") this.messageIndex = 0;
		else if (matchesKey(data, "shift+g") || data === "G") this.messageIndex = messages.length - 1;
		else if (matchesKey(data, "ctrl+d")) this.messageIndex = Math.min(messages.length - 1, this.messageIndex + half);
		else if (matchesKey(data, "ctrl+u")) this.messageIndex = Math.max(0, this.messageIndex - half);
		else if (matchesKey(data, "enter")) this.expanded = !this.expanded;
		else return;
		this.changed();
	}

	private queueLines(): string[] {
		const items = this.queueItems();
		if (items.length === 0) return ["没有待处理消息。"];
		const lines: string[] = [];
		let index = 0;
		for (const kind of ["inbound", "outbound", "context", "decision"] as const) {
			const group = items.filter((item) => item.kind === kind);
			if (group.length === 0) continue;
			lines.push(`${kindLabel(kind)} ${group.length}`);
			for (const item of group) {
				const selected = index === this.queueIndex;
				const head = `${selected ? ">" : " "} ${item.label}${item.at === undefined ? "" : ` · ${this.ages.get(item.id) ?? ""}`}`;
				lines.push(selected ? this.theme.bold(head) : head);
				lines.push(`  ${oneLine(item.detail)}`);
				index++;
			}
		}
		return lines;
	}

	private threadLines(width: number): string[] {
		const threads = this.threads();
		if (threads.length === 0) return ["没有 thread。"];
		return threads.map((thread, index) => {
			const selected = index === this.threadIndex;
			const unread = thread.unread > 0 ? ` (${thread.unread})` : "";
			const line = `${selected ? ">" : " "} ${thread.alias} · ${thread.title}${unread}`;
			return selected ? this.theme.bold(fit(line, width)) : fit(line, width);
		});
	}

	private messageLines(width: number): string[] {
		const thread = this.selectedThread();
		if (thread === undefined || thread.messages.length === 0) return ["没有消息。"];
		const lines: string[] = [];
		for (const [index, message] of thread.messages.entries()) {
			const selected = index === this.messageIndex;
			const head = `${selected ? ">" : " "} ${this.messageHead(message)}`;
			lines.push(selected ? this.theme.bold(fit(head, width)) : fit(head, width));
			const body = selected && this.expanded ? message.text : oneLine(message.text);
			for (const line of body.split("\n")) lines.push(`  ${fit(line, Math.max(1, width - 2))}`);
		}
		return lines;
	}

	render(width: number): readonly string[] {
		const lines: string[] = [this.theme.bold("Onlyne"), `1 队列  2 对话   ${this.summary()}`];
		if (this.loading) lines.push("加载中…");
		else if (this.error !== undefined) lines.push(this.theme.fg("error", this.error));
		else if (this.page === "queue") lines.push(...this.queueLines());
		else {
			const threads = this.threads();
			if (width >= 92 && threads.length > 0) {
				const leftWidth = Math.max(18, Math.min(34, Math.floor(width * 0.34)));
				const rightWidth = Math.max(10, width - leftWidth - 2);
				const left = this.threadLines(leftWidth);
				const right = this.messageLines(rightWidth);
				for (let index = 0; index < Math.max(left.length, right.length); index++) {
					lines.push(`${pad(left[index] ?? "", leftWidth)}  ${fit(right[index] ?? "", rightWidth)}`);
				}
			} else {
				lines.push("Threads", ...this.threadLines(width), "Messages", ...this.messageLines(width));
			}
		}
		lines.push("Tab 翻页 · j/k 移动 · Ctrl+d/u 半页 · [ ] 切换 thread · Enter 展开 · r 刷新 · Esc 关闭");
		return new Text(lines.join("\n"), 0, 0).render(width);
	}

	invalidate(): void {
		this.described = undefined;
	}

	dispose(): void {
		if (this.disposed) return;
		this.disposed = true;
		this.described = undefined;
	}
}
