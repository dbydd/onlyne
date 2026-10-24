import { truncateToWidth, visibleWidth } from "@oh-my-pi/pi-tui";
import type { QueueItem } from "../types.ts";

const KIND_ORDER: Record<QueueItem["kind"], number> = {
	inbound: 0,
	outbound: 1,
	context: 2,
	decision: 3,
};

const KIND_LABEL: Record<QueueItem["kind"], string> = {
	inbound: "来信",
	outbound: "出站",
	context: "上下文",
	decision: "判定",
};

export function kindLabel(kind: QueueItem["kind"]): string {
	return KIND_LABEL[kind] ?? kind;
}

export function kindRank(kind: QueueItem["kind"]): number {
	return KIND_ORDER[kind] ?? 99;
}

/** Collapse a body to one line without losing the first useful characters. */
export function oneLine(text: string): string {
	return text.replace(/\s+/g, " ").trim();
}

/** Short elapsed time for a queue row, computed when the snapshot is loaded. */
export function ageText(at: string | undefined, now = Date.now()): string {
	if (at === undefined) return "";
	const time = Date.parse(at);
	if (!Number.isFinite(time)) return "";
	const seconds = Math.max(0, Math.floor((now - time) / 1000));
	if (seconds < 60) return `${seconds}s`;
	const minutes = Math.floor(seconds / 60);
	if (minutes < 60) return `${minutes}m`;
	const hours = Math.floor(minutes / 60);
	if (hours < 48) return `${hours}h`;
	return `${Math.floor(hours / 24)}d`;
}

export function clockText(at: string): string {
	const time = Date.parse(at);
	if (!Number.isFinite(time)) return at;
	return new Date(time).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
}

export function fit(text: string, width: number): string {
	return truncateToWidth(text, Math.max(0, width));
}

export function pad(text: string, width: number): string {
	const clipped = fit(text, width);
	return clipped + " ".repeat(Math.max(0, width - visibleWidth(clipped)));
}
