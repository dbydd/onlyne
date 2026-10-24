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
import { fit, oneLine } from "./format.ts";

const REGION_PREFIX = "urn:onlyne:region:";
const REFRESH_MS = 2_000;
const WIDGET_KEY = "onlyne";

/** One `label value` field of the status row; the value alone when unlabelled. */
interface Segment {
	label?: string;
	value: string;
	tone?: string;
}

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

/**
 * One fixed row above the editor: region, node and neighbor counts, pending
 * alerts and unread messages. `render` and `describe` read the same line, so the
 * ANSI row and the Tern card cannot disagree.
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
			const before = this.line();
			this.snapshot = next;
			if (this.line() !== before) this.changed();
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

	private segments(): Segment[] {
		const snapshot = this.snapshot;
		if (snapshot === undefined) {
			return this.runtime === undefined
				? [{ value: "onlyne", tone: "accent strong" }, { value: "unconfigured", tone: "warning" }]
				: [{ value: "onlyne", tone: "accent strong" }, { value: "starting", tone: "muted" }];
		}
		const segments: Segment[] = [
			{ value: regionLabel(snapshot.region), tone: "accent strong" },
			{ label: "nodes", value: String(snapshot.nodes) },
			{ label: "neighbors", value: String(snapshot.neighbors) },
			{ label: "alert", value: String(snapshot.alerts) },
			{ label: "unread", value: String(snapshot.unread) },
		];
		if (snapshot.discovery_error !== undefined) {
			segments.push({ label: "discovery:", value: oneLine(snapshot.discovery_error), tone: "warning" });
		}
		return segments;
	}

	private line(): string {
		return this.segments()
			.map(segment => (segment.label === undefined ? segment.value : `${segment.label} ${segment.value}`))
			.join(" | ");
	}

	render(width: number): readonly string[] {
		let text = this.line();
		if (this.snapshot?.discovery_error !== undefined) {
			try {
				text = this.theme.fg("warning", text);
			} catch {
				// A host theme without the token leaves the row plain.
			}
		}
		const row = fit(text, width);
		if (row === this.row && this.rows !== undefined) return this.rows;
		this.row = row;
		this.rows = [row];
		return this.rows;
	}

	describe(): NativeNode {
		if (this.described !== undefined) return this.described;
		const head: { t: string; s?: string }[] = [];
		this.segments().forEach((segment, index) => {
			if (index > 0) head.push(span(" | ", "dim"));
			if (segment.label !== undefined) head.push(span(`${segment.label} `, "muted"));
			head.push(span(segment.value, segment.tone ?? "strong"));
		});
		// TspCardStatus has no "warning": a discovery failure is the card's error state.
		this.described = node(
			"card",
			{
				role: "omp.widget.onlyne",
				variant: "bare",
				status: this.snapshot?.discovery_error === undefined ? "done" : "error",
				head,
			},
			[textNode(this.line(), { truncate: "end" })],
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
