import type { Locale } from "../i18n.ts";

const ANSI_RE = /\x1b\[[0-9;?]*[A-Za-z]/g;

export function stripAnsi(text: string): string {
	return text.replace(ANSI_RE, "");
}

function isWideCodePoint(cp: number): boolean {
	if (cp < 0x1100) return false;
	return (
		(cp >= 0x1100 && cp <= 0x115f)
		|| cp === 0x2329
		|| cp === 0x232a
		|| (cp >= 0x2e80 && cp <= 0x303e)
		|| (cp >= 0x3040 && cp <= 0xa4cf)
		|| (cp >= 0xac00 && cp <= 0xd7a3)
		|| (cp >= 0xf900 && cp <= 0xfaff)
		|| (cp >= 0xfe10 && cp <= 0xfe19)
		|| (cp >= 0xfe30 && cp <= 0xfe6f)
		|| (cp >= 0xff00 && cp <= 0xff60)
		|| (cp >= 0xffe0 && cp <= 0xffe6)
		|| (cp >= 0x1f300 && cp <= 0x1faff)
		|| (cp >= 0x20000 && cp <= 0x3fffd)
	);
}

const graphemeSegmenter = typeof Intl !== "undefined" && typeof Intl.Segmenter === "function"
	? new Intl.Segmenter("en", { granularity: "grapheme" })
	: undefined;

function splitGraphemes(text: string): string[] {
	if (graphemeSegmenter === undefined) return Array.from(text);
	const out: string[] = [];
	for (const item of graphemeSegmenter.segment(text)) {
		out.push(item.segment);
	}
	return out;
}

export function graphemeWidth(g: string): number {
	if (g === "\n" || g === "\r" || g === "\t") return g === "\t" ? 2 : 0;
	let width = 0;
	for (const ch of g) {
		const cp = ch.codePointAt(0);
		if (cp === undefined) continue;
		if (cp < 0x20 || (cp >= 0x7f && cp <= 0x9f)) continue;
		if (cp >= 0xfe00 && cp <= 0xfe0f) continue;
		if (cp === 0x200d) continue;
		if (cp >= 0x0300 && cp <= 0x036f) continue;
		width = Math.max(width, isWideCodePoint(cp) ? 2 : 1);
	}
	return width;
}

export function visibleWidth(text: string): number {
	const plain = stripAnsi(text);
	let total = 0;
	for (const g of splitGraphemes(plain)) {
		total += graphemeWidth(g);
	}
	return total;
}

export function truncateWidth(text: string, maxWidth: number, ellipsis = "…"): string {
	if (maxWidth <= 0) return "";
	const plain = stripAnsi(text);
	if (visibleWidth(plain) <= maxWidth) return plain;
	const ellWidth = visibleWidth(ellipsis);
	const budget = maxWidth > ellWidth ? maxWidth - ellWidth : maxWidth;
	let acc = "";
	let width = 0;
	for (const g of splitGraphemes(plain)) {
		const gw = graphemeWidth(g);
		if (width + gw > budget) break;
		acc += g;
		width += gw;
	}
	return maxWidth > ellWidth ? acc + ellipsis : acc;
}

export function padRight(text: string, targetWidth: number): string {
	const cur = visibleWidth(text);
	if (cur >= targetWidth) return text;
	return text + " ".repeat(targetWidth - cur);
}

export function fitCell(text: string, width: number): string {
	const truncated = truncateWidth(text, width);
	return padRight(truncated, width);
}

export function wrapWidth(text: string, maxWidth: number): string[] {
	const width = Math.max(4, maxWidth);
	const out: string[] = [];
	for (const rawLine of text.split(/\r?\n/)) {
		if (rawLine.length === 0) {
			out.push("");
			continue;
		}
		let current = "";
		let curWidth = 0;
		for (const g of splitGraphemes(rawLine)) {
			const gw = graphemeWidth(g);
			if (curWidth + gw > width && current.length > 0) {
				out.push(current);
				current = g === " " ? "" : g;
				curWidth = g === " " ? 0 : gw;
			} else {
				current += g;
				curWidth += gw;
			}
		}
		if (current.length > 0 || out.length === 0) out.push(current);
	}
	return out;
}

export function oneLine(text: string): string {
	return text.replace(/\s+/g, " ").trim();
}

export function shortAlias(alias: string): string {
	const norm = alias.startsWith("#") ? alias.slice(1) : alias;
	return `#${norm}`;
}

export function shortId(nodeId: string): string {
	if (nodeId.startsWith("urn:onlyne:agent:")) {
		const tail = nodeId.slice("urn:onlyne:agent:".length);
		try {
			return decodeURIComponent(tail);
		} catch {
			return tail;
		}
	}
	const parts = nodeId.split(":");
	return parts[parts.length - 1] || nodeId;
}

export function clockText(iso: string | undefined): string {
	if (!iso) return "--:--";
	const ms = Date.parse(iso);
	if (!Number.isFinite(ms)) return "--:--";
	const d = new Date(ms);
	const hh = String(d.getHours()).padStart(2, "0");
	const mm = String(d.getMinutes()).padStart(2, "0");
	return `${hh}:${mm}`;
}

export function ageText(iso: string | undefined, locale: Locale): string {
	if (!iso) return locale === "zh-CN" ? "未知" : "unknown";
	const ms = Date.parse(iso);
	if (!Number.isFinite(ms)) return locale === "zh-CN" ? "未知" : "unknown";
	const diffSec = Math.max(0, Math.floor((Date.now() - ms) / 1_000));
	if (diffSec < 60) return locale === "zh-CN" ? "刚刚" : "just now";
	const mins = Math.floor(diffSec / 60);
	if (mins < 60) return locale === "zh-CN" ? `${mins} 分钟前` : `${mins}m ago`;
	const hrs = Math.floor(mins / 60);
	if (hrs < 24) return locale === "zh-CN" ? `${hrs} 小时前` : `${hrs}h ago`;
	const days = Math.floor(hrs / 24);
	return locale === "zh-CN" ? `${days} 天前` : `${days}d ago`;
}

export function sinceText(iso: string | undefined, locale: Locale): string {
	return ageText(iso, locale);
}
