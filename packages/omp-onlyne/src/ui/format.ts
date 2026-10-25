import { truncateToWidth } from "@oh-my-pi/pi-tui";

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

/** The head of a thread alias: its first UUID segment, which is what a narrow row can afford. */
export function shortAlias(alias: string): string {
	const head = alias.split("-", 1)[0] ?? alias;
	return head === "" ? alias : head;
}

export function fit(text: string, width: number): string {
	return truncateToWidth(text, Math.max(0, width));
}
