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
import { configPath, loadConfig, normalizeRegion, saveConfig } from "../config.ts";
import { getRuntime } from "../runtime.ts";
import type { OnlyneConfig } from "../types.ts";

const FIELDS = ["region", "nodeId", "bind", "keyRef", "pageSize"] as const;
type FieldName = (typeof FIELDS)[number];

const LABELS: Record<FieldName, string> = {
	region: "Region",
	nodeId: "Node id",
	bind: "Server bind",
	keyRef: "Key ref",
	pageSize: "Page size",
};

const HINTS: Record<FieldName, string> = {
	region: "短名保存为 urn:onlyne:region:<短名>",
	nodeId: "urn:onlyne:agent:<name>",
	bind: "127.0.0.1",
	keyRef: "env:NAME 或 file:/path；可留空",
	pageSize: "1..100",
};

type DraftField = { editor: Editor; field: FormField };

/** Editable project config. The draft lives only in this overlay; cancel writes nothing. */
export class OnlyneSettings implements Component {
	private config: OnlyneConfig | undefined;
	private fields: Record<FieldName, DraftField> | undefined;
	private active: FieldName = "region";
	private loading = true;
	private saving = false;
	private error: string | undefined;
	private notice: string | undefined;
	private described: NativeNode | undefined;
	private disposed = false;
	private generation = 0;
	focused = true;
	readonly nativeOverlay = { role: "onlyne.settings", size: "lg" as const, anchor: "center" as const, head: "Onlyne settings" };

	constructor(
		private readonly cwd: string,
		initial: OnlyneConfig | undefined,
		private readonly theme: Theme,
		private readonly requestRender: () => void,
		private readonly done: () => void,
		private readonly signal: AbortSignal,
	) {
		if (initial !== undefined) {
			this.config = initial;
			this.buildFields(initial);
			this.loading = false;
		} else {
			void this.load();
		}
	}

	private changed(): void {
		if (this.disposed || this.signal.aborted) return;
		this.described = undefined;
		this.requestRender();
	}

	private async load(): Promise<void> {
		if (this.disposed || this.signal.aborted) return;
		const generation = ++this.generation;
		this.loading = true;
		this.error = undefined;
		this.notice = undefined;
		this.changed();
		try {
			const config = await loadConfig(this.cwd);
			if (this.disposed || this.signal.aborted || generation !== this.generation) return;
			this.config = config;
			this.buildFields(config);
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

	private makeField(label: string, value: string, hint: string): DraftField {
		const editor = new Editor(getEditorTheme());
		editor.disableSubmit = true;
		editor.setText(value);
		editor.setMaxHeight(4);
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

	private buildFields(config: OnlyneConfig): void {
		this.discardFields();
		this.fields = {
			region: this.makeField(LABELS.region, config.region, HINTS.region),
			nodeId: this.makeField(LABELS.nodeId, config.node.id, HINTS.nodeId),
			bind: this.makeField(LABELS.bind, config.server.bind, HINTS.bind),
			keyRef: this.makeField(LABELS.keyRef, config.node.key_ref ?? "", HINTS.keyRef),
			pageSize: this.makeField(LABELS.pageSize, String(config.threads.page_size), HINTS.pageSize),
		};
		this.active = "region";
		this.described = undefined;
	}

	private discardFields(): void {
		if (this.fields === undefined) return;
		for (const name of FIELDS) this.fields[name].field.dispose();
		this.fields = undefined;
	}

	private value(name: FieldName): string {
		return this.fields?.[name].editor.getExpandedText().trim() ?? "";
	}

	/** Read-only observation: the endpoint the running node actually bound. */
	private runtimeEndpoint(): string | undefined {
		const runtime: unknown = getRuntime();
		if (runtime === null || typeof runtime !== "object" || !("endpoint" in runtime)) return undefined;
		const endpoint = runtime.endpoint;
		return typeof endpoint === "string" && endpoint.length > 0 ? endpoint : undefined;
	}

	private async save(): Promise<void> {
		if (this.disposed || this.signal.aborted || this.saving) return;
		const fields = this.fields;
		const base = this.config;
		if (fields === undefined || base === undefined) return;
		const region = this.value("region");
		const keyRef = this.value("keyRef");
		if (region.length === 0) {
			this.error = "Region 不能为空。";
			this.changed();
			return;
		}
		const next: OnlyneConfig = {
			...base,
			path: base.path.length > 0 ? base.path : configPath(this.cwd),
			region: normalizeRegion(region),
			node: { id: this.value("nodeId"), ...(keyRef.length === 0 ? {} : { key_ref: keyRef }) },
			server: { bind: this.value("bind") },
			threads: { page_size: Number(this.value("pageSize")) },
		};
		this.saving = true;
		this.error = undefined;
		this.notice = undefined;
		this.changed();
		try {
			let saved = next;
			const runtime = getRuntime();
			if (runtime === undefined) saved = await saveConfig(next);
			else await runtime.updateConfig(next);
			if (this.disposed || this.signal.aborted) return;
			this.config = saved;
			this.notice = `已保存：${saved.region}`;
		} catch (error) {
			if (this.signal.aborted) return;
			this.error = error instanceof Error ? error.message : String(error);
		} finally {
			if (!this.disposed && !this.signal.aborted) {
				this.saving = false;
				this.changed();
			}
		}
	}

	private fieldRow(name: FieldName): NativeNode {
		const selected = name === this.active;
		return node(
			"row",
			{
				role: "omp.btn",
				actions: { click: `onlyne.settings.field.${name}` },
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
		const body: NativeChild[] = [];
		const fields = this.fields;
		if (this.loading) {
			body.push(textNode("加载配置…"));
		} else if (fields === undefined) {
			body.push(node("text", { spans: [span(this.error ?? "配置未加载。", "error")], wrap: "word" }));
			body.push(node("col", { gap: "xs" }, [this.button("重试", "onlyne.settings.reload")]));
		} else {
			for (const name of FIELDS) {
				if (name === this.active && !this.saving) {
					const draft = fields[name];
					draft.field.focused = this.focused;
					body.push(draft.field);
				} else body.push(this.fieldRow(name));
			}
			const endpoint = this.runtimeEndpoint();
			body.push(
				node("text", {
					spans: [span("Endpoint: ", "strong"), span(endpoint ?? "运行时未启动", "muted")],
					wrap: "word",
				}),
			);
			if (this.error !== undefined) body.push(node("text", { spans: [span(this.error, "error")], wrap: "word" }));
			if (this.notice !== undefined) body.push(node("text", { spans: [span(this.notice, "success")], wrap: "word" }));
			body.push(
				node("col", { gap: "xs" }, [
					this.button(this.saving ? "保存中…" : "保存", "onlyne.settings.save"),
					this.button("取消", "onlyne.settings.cancel"),
				]),
			);
			body.push(node("text", { text: "Tab/Shift+Tab 换行 · Enter 下一项 · Ctrl+Enter 保存 · Esc 取消", wrap: "word" }));
		}
		this.described = node("card", { role: "onlyne.settings" }, body);
		return this.described;
	}

	handleNativeEvent(event: NativeUiEvent): void {
		if (this.disposed || this.signal.aborted || event.type !== "action") return;
		const act = event.act;
		if (act === "onlyne.settings.save") {
			void this.save();
			return;
		}
		if (act === "onlyne.settings.cancel" || act === "onlyne.close") {
			this.done();
			return;
		}
		if (act === "onlyne.settings.reload") {
			void this.load();
			return;
		}
		if (act.startsWith("onlyne.settings.field.")) {
			const name = act.slice("onlyne.settings.field.".length) as FieldName;
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
		const fields = this.fields;
		if (fields === undefined) {
			if (matchesKey(data, "r")) void this.load();
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
		fields[this.active].editor.handleInput(data);
		this.changed();
	}

	render(width: number): readonly string[] {
		const lines: string[] = [this.theme.bold("Onlyne settings")];
		const fields = this.fields;
		if (this.loading) lines.push("加载配置…");
		else if (fields === undefined) {
			lines.push(this.theme.fg("error", this.error ?? "配置未加载。"), "r 重试 · Esc 关闭");
		} else {
			for (const name of FIELDS) {
				if (name === this.active && !this.saving) {
					const draft = fields[name];
					draft.field.focused = this.focused;
					lines.push(...draft.field.render(width));
				} else lines.push(`${name === this.active ? ">" : " "} ${LABELS[name]}: ${this.value(name) || "[empty]"}`);
			}
			lines.push(`Endpoint: ${this.runtimeEndpoint() ?? "运行时未启动"}`);
			if (this.error !== undefined) lines.push(this.theme.fg("error", this.error));
			if (this.notice !== undefined) lines.push(this.theme.fg("success", this.notice));
			lines.push("Tab/Shift+Tab 换行 · Enter 下一项 · Ctrl+Enter 保存 · Esc 取消");
		}
		return new Text(lines.join("\n"), 0, 0).render(width);
	}

	invalidate(): void {
		this.described = undefined;
	}

	dispose(): void {
		if (this.disposed) return;
		this.disposed = true;
		this.discardFields();
		this.described = undefined;
	}
}
