import type { ExtensionAPI, ExtensionContext } from "@oh-my-pi/pi-coding-agent";
import {
	type Component,
	type DescribeContext,
	Editor,
	FormField,
	type NativeChild,
	type NativeNode,
	type NativeUiEvent,
	Text,
	type Theme,
	getEditorTheme,
	matchesKey,
	node,
	span,
	text as textNode,
} from "@oh-my-pi/pi-tui";
import { configPath, normalizeNodeId, normalizeRegion, normalizeServiceUrl, saveConfig, shortNodeId } from "../config.ts";
import { startRuntime } from "../runtime.ts";
import type { OnlyneConfig } from "../types.ts";

const FIELDS = ["region", "nodeId", "bind", "keyScope", "pageSize", "contact"] as const;
type FieldName = (typeof FIELDS)[number];

const LABELS: Record<FieldName, string> = {
	region: "Region",
	nodeId: "Node id",
	bind: "Server bind",
	keyScope: "Node key (project/global/none)",
	pageSize: "Page size",
	contact: "Node addresses (optional)",
};

const HINTS: Record<FieldName, string> = {
	region: "短名保存为 urn:onlyne:region:<短名>",
	nodeId: "短名保存为 urn:onlyne:agent:<短名>",
	bind: "127.0.0.1",
	keyScope: "project：密钥存 .onlyne/keys（git 内需 ignore）；global：存 ~/.config/onlyne/keys；none：回环开发绑定",
	pageSize: "1..100，默认 50",
	contact: "自动发现到不了的节点按地址联系；保存为 discovery.seeds，每次启动都会联系。多个地址用逗号分隔",
};

/** The values the command handler derived from Tern and the saved config. */
export interface BootstrapDefaults {
	/** Saved region when re-running, otherwise the Tern session suggestion; empty without Tern. */
	region: string;
	regionSource: string;
	nodeId: string;
	bind: string;
	keyScope: string;
	pageSize: number;
	/** Existing configuration; the draft carries what the form does not edit. */
	current?: OnlyneConfig;
}

type DraftField = { editor: Editor; field: FormField };

/** First-run form. Save writes the project config and starts the node; cancel writes nothing. */
export class OnlyneBootstrap implements Component {
	private readonly fields: Record<FieldName, DraftField>;
	private active: FieldName = "region";
	private saving = false;
	private error: string | undefined;
	private described: NativeNode | undefined;
	private disposed = false;
	focused = true;
	readonly nativeOverlay = {
		role: "onlyne.bootstrap",
		size: "lg" as const,
		anchor: "center" as const,
		head: "Onlyne init",
	};

	constructor(
		private readonly pi: ExtensionAPI,
		private readonly ctx: ExtensionContext,
		private readonly defaults: BootstrapDefaults,
		private readonly theme: Theme,
		private readonly requestRender: () => void,
		private readonly done: () => void,
		private readonly signal: AbortSignal,
	) {
		this.fields = {
			region: this.makeField(LABELS.region, defaults.region, HINTS.region),
			nodeId: this.makeField(LABELS.nodeId, shortNodeId(defaults.nodeId), HINTS.nodeId),
			bind: this.makeField(LABELS.bind, defaults.bind, HINTS.bind),
			keyScope: this.makeField(LABELS.keyScope, defaults.keyScope, HINTS.keyScope),
			pageSize: this.makeField(LABELS.pageSize, String(defaults.pageSize), HINTS.pageSize),
			contact: this.makeField(LABELS.contact, (defaults.current?.discovery?.seeds ?? []).join(", "), HINTS.contact),
		};
	}

	private changed(): void {
		if (this.disposed || this.signal.aborted) return;
		this.described = undefined;
		this.requestRender();
	}

	private makeField(label: string, value: string, hint: string): DraftField {
		const editor = new Editor(getEditorTheme());
		editor.disableSubmit = true;
		editor.setText(value);
		editor.setMaxHeight(2);
		editor.onChange = () => this.changed();
		const field = new FormField(editor, {
			label,
			hint,
			theme: {
				label: text => this.theme.bold(text),
				description: text => this.theme.fg("muted", text),
				error: text => this.theme.fg("error", text),
				hint: text => this.theme.fg("muted", text),
			},
		});
		return { editor, field };
	}

	private value(name: FieldName): string {
		return this.fields[name].editor.getExpandedText().trim();
	}

	/** The URI that saving will store; empty input reports the required field instead. */
	private regionPreview(): string {
		const raw = this.value("region");
		return raw.length === 0 ? "Region 必填；短名将保存为 urn:onlyne:region:<短名>" : `将保存为 ${normalizeRegion(raw)}`;
	}

	private async save(): Promise<void> {
		if (this.disposed || this.signal.aborted || this.saving) return;
		const region = this.value("region");
		const nodeId = this.value("nodeId");
		const bind = this.value("bind");
		const keyScope = this.value("keyScope").trim().toLowerCase();
		if (region.length === 0) {
			this.fail("Region 不能为空。");
			return;
		}
		const scope = keyScope === "" ? "project" : keyScope;
		if (scope !== "project" && scope !== "global" && scope !== "none") {
			this.fail("Node key 只能是 project、global 或 none。");
			return;
		}
		let seeds: string[];
		try {
			seeds = this.value("contact")
				.split(/[,\s]+/)
				.map(part => part.trim())
				.filter(part => part.length > 0)
				.map(normalizeServiceUrl);
		} catch (error) {
			this.fail(error instanceof Error ? error.message : String(error));
			return;
		}
		const previous = this.defaults.current;
		const config: OnlyneConfig = {
			path: configPath(this.ctx.cwd),
			region: normalizeRegion(region),
			node: {
				id: normalizeNodeId(nodeId),
				key_scope: scope,
				// The form does not edit these; saving must not erase them.
				...(previous?.node.key_ref === undefined ? {} : { key_ref: previous.node.key_ref }),
			},
			server: { bind, ...(previous?.server.public_url === undefined ? {} : { public_url: previous.server.public_url }) },
			threads: { page_size: Number(this.value("pageSize")) },
			...(seeds.length === 0 ? {} : { discovery: { seeds } }),
			...(previous?.trust === undefined ? {} : { trust: previous.trust }),
		};
		this.saving = true;
		this.error = undefined;
		this.changed();
		let persisted = false;
		try {
			await saveConfig(config);
			persisted = true;
			const runtime = await startRuntime(this.pi, this.ctx);
			// The start already read every saved seed; a seed typed just now is read again only
			// to report its failure in the form the user typed it in.
			const known = new Set(previous?.discovery?.seeds ?? []);
			for (const seed of seeds.filter(seed => !known.has(seed))) {
				try { await runtime.contact(seed); }
				catch (error) {
					if (this.signal.aborted) return;
					this.fail(`配置已保存并启动，但 ${seed} 没连上：${error instanceof Error ? error.message : String(error)}`);
					return;
				}
			}
			if (this.disposed || this.signal.aborted) return;
			this.ctx.ui.notify(`Onlyne 已初始化：${config.region}`, "info");
			this.done();
		} catch (error) {
			if (this.signal.aborted) return;
			const reason = error instanceof Error ? error.message : String(error);
			this.error = persisted ? `配置已写入，启动失败：${reason}` : `未保存：${reason}`;
		} finally {
			if (!this.disposed && !this.signal.aborted) {
				this.saving = false;
				this.changed();
			}
		}
	}

	private fail(message: string): void {
		this.error = message;
		this.changed();
	}

	private fieldRow(name: FieldName): NativeNode {
		const selected = name === this.active;
		return node(
			"row",
			{
				role: "omp.btn",
				actions: { click: `onlyne.bootstrap.field.${name}` },
				title: LABELS[name],
				tone: selected ? "accent" : undefined,
			},
			[
				node("text", {
					spans: [span(LABELS[name], "strong"), span(`: ${this.value(name) || "[empty]"}`, "muted")],
					wrap: "word",
				}),
			],
			name,
		);
	}

	private button(label: string, act: string): NativeNode {
		return node("row", { role: "omp.btn", actions: { click: act }, title: label }, [textNode(label)], act);
	}

	describe(_cx: DescribeContext): NativeNode {
		if (this.described !== undefined) return this.described;
		const body: NativeChild[] = [node("text", { text: this.header(), wrap: "word" })];
		for (const name of FIELDS) {
			if (name === this.active && !this.saving) {
				const draft = this.fields[name];
				draft.field.focused = this.focused;
				body.push(draft.field);
			} else body.push(this.fieldRow(name));
			if (name === "region") body.push(node("text", { text: this.regionPreview(), wrap: "word" }));
		}
		if (this.error !== undefined) body.push(node("text", { spans: [span(this.error, "error")], wrap: "word" }));
		body.push(
			node("col", { gap: "xs" }, [
				this.button(this.saving ? "保存中…" : "保存并启动", "onlyne.bootstrap.save"),
				this.button("取消", "onlyne.bootstrap.cancel"),
			]),
		);
		body.push(node("text", { text: "Tab/Shift+Tab 换行 · Enter 下一项 · Ctrl+Enter 保存 · Esc 取消", wrap: "word" }));
		this.described = node("card", { role: "onlyne.bootstrap" }, body);
		return this.described;
	}

	handleNativeEvent(event: NativeUiEvent): void {
		if (this.disposed || this.signal.aborted || event.type !== "action") return;
		const act = event.act;
		if (act === "onlyne.bootstrap.save") {
			void this.save();
			return;
		}
		if (act === "onlyne.bootstrap.cancel" || act === "onlyne.close") {
			this.done();
			return;
		}
		if (act.startsWith("onlyne.bootstrap.field.")) {
			const name = act.slice("onlyne.bootstrap.field.".length) as FieldName;
			if (FIELDS.includes(name)) {
				this.active = name;
				this.changed();
			}
		}
	}

	handleInput(data: string): void {
		if (this.disposed || this.signal.aborted) return;
		if (matchesKey(data, "escape")) {
			this.done();
			return;
		}
		if (matchesKey(data, "ctrl+enter")) {
			void this.save();
			return;
		}
		if (matchesKey(data, "tab") || matchesKey(data, "shift+tab")) {
			const step = matchesKey(data, "shift+tab") ? FIELDS.length - 1 : 1;
			this.active = FIELDS[(FIELDS.indexOf(this.active) + step) % FIELDS.length]!;
			this.changed();
			return;
		}
		if (matchesKey(data, "enter")) {
			this.active = FIELDS[(FIELDS.indexOf(this.active) + 1) % FIELDS.length]!;
			this.changed();
			return;
		}
		this.fields[this.active].editor.handleInput(data);
		this.changed();
	}

	render(width: number): readonly string[] {
		const lines: string[] = [this.theme.bold("Onlyne init"), this.header()];
		for (const name of FIELDS) {
			if (name === this.active && !this.saving) {
				const draft = this.fields[name];
				draft.field.focused = this.focused;
				lines.push(...draft.field.render(width));
			} else lines.push(`${name === this.active ? ">" : " "} ${LABELS[name]}: ${this.value(name) || "[empty]"}`);
			if (name === "region") lines.push(this.regionPreview());
		}
		if (this.error !== undefined) lines.push(this.theme.fg("error", this.error));
		lines.push("Tab/Shift+Tab 换行 · Enter 下一项 · Ctrl+Enter 保存 · Esc 取消");
		return new Text(lines.join("\n"), 0, 0).render(width);
	}

	invalidate(): void {
		this.described = undefined;
	}

	private header(): string {
		const existing = this.defaults.current === undefined ? "" : "已存在项目配置，保存将覆盖。";
		return `${this.defaults.regionSource}。保存后启动本节点并发布发现。${existing}`;
	}

	dispose(): void {
		if (this.disposed) return;
		this.disposed = true;
		for (const name of FIELDS) this.fields[name].field.dispose();
		this.described = undefined;
	}
}
