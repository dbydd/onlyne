import type { ExtensionContext } from "@oh-my-pi/pi-coding-agent";
import {
	type Component,
	type NativeNode,
	node,
	span,
	text as textNode,
	type Theme,
} from "@oh-my-pi/pi-tui";
import { getRuntime, type PluginRuntime } from "../runtime.ts";
import type { OnlyneStatus as OnlyneStatusSnapshot } from "../types.ts";
import { shortNodeId } from "../config.ts";
import { fit, oneLine } from "./format.ts";

const REGION_PREFIX = "urn:onlyne:region:";
const REFRESH_MS = 2_000;
const WIDGET_KEY = "onlyne";

/** A region URI is stored for stability; the row shows the short name the user typed. */
function regionLabel(region: string): string {
	if (!region.startsWith(REGION_PREFIX)) return region;
	const name = region.slice(REGION_PREFIX.length);
	try {
		return decodeURIComponent(name);
	} catch {
		return name;
	}
}

/** One `label value` module of the status row. */
interface Module {
	label: string;
	value: string;
	tone?: string;
}

/**
 * One fixed row above the editor: region, node and neighbor counts, pending
 * alerts and unread messages. The modules are laid out across the full width
 * (`justify: between`), never left-aligned; the separators are layout gaps, not
 * drawn characters. `render` and `describe` project the same modules, so the
 * ANSI row and the Tern card cannot disagree, and neither repeats the line.
 */
export class OnlyneStatus implements Component {
	private readonly theme: Theme;
	private readonly requestRender: () => void;
	private runtime: PluginRuntime | undefined;
	private snapshot: OnlyneStatusSnapshot | undefined;
	private unsubscribe: (() => void) | undefined;
	private timer: Timer | undefined;
	private disposed = false;
	private refreshing = false;
	private row: string | undefined;
	private rows: readonly string[] | undefined;
	private described: NativeNode | undefined;

	constructor(theme: Theme, requestRender: () => void) {
		this.theme = theme;
		this.requestRender = requestRender;
		void this.refresh();
		this.timer = setInterval(() => void this.refresh(), REFRESH_MS);
	}

	/** Re-read the runtime and its status; a restart is a new runtime object and a new subscription. */
	private async refresh(): Promise<void> {
		if (this.disposed || this.refreshing) return;
		this.refreshing = true;
		try {
			const runtime = getRuntime();
			if (runtime !== this.runtime) {
				this.unsubscribe?.();
				this.unsubscribe = undefined;
				this.runtime = runtime;
				if (runtime !== undefined) this.unsubscribe = runtime.onChange(() => void this.refresh());
			}
			const next = await runtime?.status();
			if (this.disposed) return;
			// The rendered row is the whole observable state, so an identical row needs no repaint.
			const before = this.modules();
			this.snapshot = next;
			if (JSON.stringify(before) !== JSON.stringify(this.modules())) this.changed();
		} catch {
			// The status read is best effort: keep the last good row instead of throwing into the host.
		} finally {
			this.refreshing = false;
		}
	}

	private changed(): void {
		if (this.disposed) return;
		this.row = undefined;
		this.rows = undefined;
		this.described = undefined;
		this.requestRender();
	}

	private modules(): Module[] {
		const snapshot = this.snapshot;
		if (snapshot === undefined) {
			return this.runtime === undefined
				? [{ label: "onlyne", value: "unconfigured", tone: "warning" }]
				: [{ label: "onlyne", value: "starting", tone: "muted" }];
		}
		const modules: Module[] = [
			{ label: "nodes", value: String(snapshot.nodes) },
			{ label: "neighbors", value: String(snapshot.neighbors) },
			{ label: "alert", value: String(snapshot.alerts) },
			{ label: "unread", value: String(snapshot.unread) },
		];
		if (snapshot.discovery_error !== undefined) {
			modules.push({ label: "discovery", value: oneLine(snapshot.discovery_error), tone: "warning" });
		}
		return modules;
	}

	/** The node id and region lead the row; without a runtime there is nothing to lead with. */
	private lead(): { text: string; tone: string } {
		const snapshot = this.snapshot;
		if (snapshot === undefined) return { text: "", tone: "muted" };
		const parts = [shortNodeId(snapshot.node_id), regionLabel(snapshot.region)].filter(part => part !== "");
		return { text: parts.join(" · "), tone: "accent strong" };
	}

	/** ANSI: one row with the modules spread across the width, region pinned left. */
	render(width: number): readonly string[] {
		const lead = this.lead();
		const cells = this.modules().map(module => `${module.label} ${module.value}`);
		const head = lead.text === "" ? "" : `${lead.text}  `;
		// Even columns over the remaining width, so the modules are spread out rather than left-aligned.
		const each = cells.length === 0 ? 0 : Math.max(1, Math.floor(Math.max(0, width - head.length) / cells.length));
		let row = head;
		for (const [index, cell] of cells.entries()) {
			row += " ".repeat(index === 0 ? 0 : Math.max(0, each - cells[index - 1]!.length)) + cell;
		}
		row = fit(row, width);
		if (row === this.row && this.rows !== undefined) return this.rows;
		this.row = row;
		this.rows = [row];
		return this.rows;
	}

	describe(): NativeNode {
		if (this.described !== undefined) return this.described;
		const lead = this.lead();
		const children: NativeNode[] = [];
		if (lead.text !== "") {
			children.push(textNode([span(lead.text, lead.tone)], { truncate: "end" }));
		}
		for (const module of this.modules()) {
			children.push(
				textNode([span(`${module.label} `, "muted"), span(module.value, module.tone ?? "strong")], { truncate: "end" }),
			);
		}
		// A plain row, not a card: a card adds head/body chrome and a second visual row,
		// which pushed the widget taller than the one line it is meant to occupy.
		this.described = node(
			"row",
			{ role: "omp.widget.onlyne", justify: "between", align: "center", gap: "md" },
			children,
			"status",
		);
		return this.described;
	}

	dispose(): void {
		if (this.disposed) return;
		this.disposed = true;
		if (this.timer !== undefined) {
			clearInterval(this.timer);
			this.timer = undefined;
		}
		this.unsubscribe?.();
		this.unsubscribe = undefined;
		this.described = undefined;
	}
}

/**
 * Mount the status row above the editor. The returned cleanup removes the
 * widget and stops the component's timer and runtime subscription.
 */
export function attachStatus(ctx: ExtensionContext): () => void {
	let component: OnlyneStatus | undefined;
	ctx.ui.setWidget(
		WIDGET_KEY,
		(tui, theme) => {
			component?.dispose();
			component = new OnlyneStatus(theme, () => tui.requestRender());
			return component;
		},
		{ placement: "aboveEditor" },
	);
	return () => {
		ctx.ui.setWidget(WIDGET_KEY, undefined);
		component?.dispose();
		component = undefined;
	};
}
