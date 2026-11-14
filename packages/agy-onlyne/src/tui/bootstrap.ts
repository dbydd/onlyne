import { basename } from "node:path";
import type { AcquirePolicy } from "@onlyne/node-host/acquire";
import { parseAllowBlock } from "@onlyne/node-host/acquire";
import {
	normalizeServiceUrl,
	shortNodeId,
	shortRegion,
	type AgyOnlyneConfig,
	type JudgeMode,
} from "../config.ts";
import type { Locale } from "../i18n.ts";
import { fitCell, padRight, wrapWidth } from "./format.ts";
import { tuiText } from "./text.ts";

export const BOOTSTRAP_FIELDS = [
	"region",
	"node_id",
	"bind",
	"key_scope",
	"page_size",
	"seeds",
	"policy",
	"allow",
	"judge_mode",
	"judge_model",
	"judge_base_url",
] as const;

export type BootstrapField = (typeof BOOTSTRAP_FIELDS)[number];

export interface BootstrapValues {
	region: string;
	node_id: string;
	bind: string;
	key_scope: string;
	page_size: string;
	seeds: string;
	policy: string;
	allow: string;
	judge_mode: string;
	judge_model: string;
	judge_base_url: string;
}

export interface BootstrapState {
	values: BootstrapValues;
	focusIndex: number; // 0..10 fields, 11 = Save, 12 = Cancel (if allowCancel)
	allowCancel: boolean;
	saving: boolean;
	error?: string;
}

export interface ValidatedBootstrapPayload {
	region: string;
	node_id: string;
	bind: string;
	key_scope: "project" | "global" | "none";
	page_size: number;
	seeds: string[];
	policy: AcquirePolicy;
	allow: string[];
	judge_mode: JudgeMode;
	judge_model?: string;
	judge_base_url?: string;
}

export function createBootstrapState(options: {
	workspace: string;
	current?: AgyOnlyneConfig;
	allowCancel?: boolean;
}): BootstrapState {
	const cfg = options.current;
	const dirName = basename(options.workspace).replace(/[^a-zA-Z0-9._-]+/g, "-") || "agy";
	return {
		values: {
			region: cfg?.region ? shortRegion(cfg.region) : "local",
			node_id: cfg?.node?.id ? shortNodeId(cfg.node.id) : `agy-${dirName}`,
			bind: cfg?.server?.bind ?? "127.0.0.1",
			key_scope: cfg?.node?.key_scope ?? "project",
			page_size: String(cfg?.threads?.page_size ?? 50),
			seeds: (cfg?.discovery?.seeds ?? []).join(", "),
			policy: cfg?.acquisition?.policy ?? "segment",
			allow: (cfg?.acquisition?.allow ?? ["127.0.0.1/32", "::1/128"]).join(", "),
			judge_mode: cfg?.judge?.mode ?? "auto",
			judge_model: cfg?.judge?.model ?? "",
			judge_base_url: cfg?.judge?.base_url ?? "",
		},
		focusIndex: 0,
		allowCancel: options.allowCancel ?? true,
		saving: false,
	};
}

export function validateBootstrapValues(
	values: BootstrapValues,
	locale: Locale,
): { ok: true; payload: ValidatedBootstrapPayload } | { ok: false; error: string } {
	const strings = tuiText(locale);
	const region = values.region.trim();
	if (!region) return { ok: false, error: strings.bootstrap.errors.regionRequired };
	const nodeId = values.node_id.trim();
	if (!nodeId) return { ok: false, error: strings.bootstrap.errors.nodeIdRequired };
	const bind = values.bind.trim();
	if (!bind) return { ok: false, error: strings.bootstrap.errors.bindRequired };

	const scopeRaw = values.key_scope.trim().toLowerCase() || "project";
	if (scopeRaw !== "project" && scopeRaw !== "global" && scopeRaw !== "none") {
		return { ok: false, error: strings.bootstrap.errors.keyScopeInvalid };
	}

	const pageSize = Number(values.page_size.trim());
	if (!Number.isInteger(pageSize) || pageSize < 1 || pageSize > 200) {
		return { ok: false, error: strings.bootstrap.errors.pageSizeInvalid };
	}

	let seeds: string[];
	try {
		seeds = values.seeds
			.split(/[,\s]+/)
			.map(s => s.trim())
			.filter(Boolean)
			.map(normalizeServiceUrl);
	} catch (err) {
		return { ok: false, error: err instanceof Error ? err.message : String(err) };
	}

	const policyRaw = values.policy.trim().toLowerCase() || "segment";
	if (policyRaw !== "all" && policyRaw !== "segment" && policyRaw !== "none") {
		return { ok: false, error: strings.bootstrap.errors.policyInvalid };
	}

	const allow = values.allow
		.split(/[,\s]+/)
		.map(s => s.trim())
		.filter(Boolean);
	for (const entry of allow) {
		if (parseAllowBlock(entry) === undefined) {
			return { ok: false, error: `${strings.bootstrap.errors.allowInvalid}: ${entry}` };
		}
	}

	const judgeRaw = values.judge_mode.trim().toLowerCase() || "auto";
	if (
		judgeRaw !== "auto"
		&& judgeRaw !== "sampling"
		&& judgeRaw !== "openai"
		&& judgeRaw !== "gemini"
		&& judgeRaw !== "agy-cli"
		&& judgeRaw !== "none"
	) {
		return { ok: false, error: strings.bootstrap.errors.judgeModeInvalid };
	}

	return {
		ok: true,
		payload: {
			region,
			node_id: nodeId,
			bind,
			key_scope: scopeRaw,
			page_size: pageSize,
			seeds,
			policy: policyRaw,
			allow,
			judge_mode: judgeRaw,
			judge_model: values.judge_model.trim() || undefined,
			judge_base_url: values.judge_base_url.trim() || undefined,
		},
	};
}

export function bootstrapFocusLimit(state: BootstrapState): number {
	return BOOTSTRAP_FIELDS.length + (state.allowCancel ? 2 : 1);
}

export function renderBootstrapLines(
	state: BootstrapState,
	width: number,
	height: number,
	locale: Locale,
	color = true,
): string[] {
	const strings = tuiText(locale);
	const c = {
		cyan: (s: string) => (color ? `\x1b[36m${s}\x1b[0m` : s),
		bold: (s: string) => (color ? `\x1b[1m${s}\x1b[0m` : s),
		dim: (s: string) => (color ? `\x1b[2m${s}\x1b[0m` : s),
		err: (s: string) => (color ? `\x1b[31m${s}\x1b[0m` : s),
		rev: (s: string) => (color ? `\x1b[7m${s}\x1b[0m` : `[>${s}<]`),
	};

	const innerWidth = Math.max(40, width - 4);
	const lines: string[] = [];
	lines.push(c.bold(fitCell(strings.bootstrap.title, innerWidth)));
	for (const line of wrapWidth(strings.bootstrap.subtitle, innerWidth)) {
		lines.push(c.dim(padRight(line, innerWidth)));
	}
	lines.push("─".repeat(innerWidth));

	for (let i = 0; i < BOOTSTRAP_FIELDS.length; i += 1) {
		const field = BOOTSTRAP_FIELDS[i]!;
		const focused = state.focusIndex === i;
		const meta = strings.bootstrap.fields[field];
		const label = fitCell(meta.label, 16);
		const rawVal = state.values[field];
		const valBox = fitCell(focused ? `${rawVal}▏` : rawVal, Math.max(16, innerWidth - 20));
		const row = focused ? `${c.cyan("▸")} ${c.bold(label)} ${c.rev(valBox)}` : `  ${c.dim(label)} ${valBox}`;
		lines.push(row);
		if (focused) {
			for (const h of wrapWidth(`  ${meta.hint}`, innerWidth)) {
				lines.push(c.dim(h));
			}
		}
	}

	lines.push("─".repeat(innerWidth));
	const saveFocus = state.focusIndex === BOOTSTRAP_FIELDS.length;
	const cancelFocus = state.focusIndex === BOOTSTRAP_FIELDS.length + 1;
	const saveBtn = saveFocus ? c.rev(strings.bootstrap.save) : c.cyan(strings.bootstrap.save);
	const cancelBtn = state.allowCancel
		? (cancelFocus ? c.rev(strings.bootstrap.cancel) : c.dim(strings.bootstrap.cancel))
		: "";
	lines.push(`${saveBtn}  ${cancelBtn}`);

	if (state.error) {
		for (const errLine of wrapWidth(`! ${state.error}`, innerWidth)) {
			lines.push(c.err(errLine));
		}
	}
	if (state.saving) {
		lines.push(c.cyan("Saving…"));
	}

	lines.push(c.dim(fitCell(strings.bootstrap.hint, innerWidth)));
	while (lines.length < height - 2) lines.push("");
	return lines.slice(0, Math.max(10, height));
}
