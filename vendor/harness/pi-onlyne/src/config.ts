import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

export type InboundMode = "auto-handle" | "queue-only" | "muted";
export type ReplyMode = "guarded-explicit" | "explicit-only" | "implicit-final";
export interface OnlyneRule { channel: string; conversation?: string; mode: InboundMode }
export interface OnlyneConfig {
	watch: { autoStart: boolean };
	inbound: { defaultMode: InboundMode; rules: OnlyneRule[] };
	outbound: { defaultReplyMode: ReplyMode; guardedExplicit: { reminders: number; noOutputFallbackText: string }; retry: { attempts: number; concurrency: number } };
}
export const defaultConfig: OnlyneConfig = {
	watch: { autoStart: false },
	inbound: { defaultMode: "auto-handle", rules: [] },
	outbound: { defaultReplyMode: "guarded-explicit", guardedExplicit: { reminders: 2, noOutputFallbackText: "Onlyne/Pi error: no valid reply was produced." }, retry: { attempts: 2, concurrency: 8 } },
};
export function configPath(cwd: string) { return join(cwd, ".pi", "onlyne.json"); }
export function loadConfig(cwd: string): OnlyneConfig {
	const path = configPath(cwd);
	if (!existsSync(path)) return structuredClone(defaultConfig);
	const parsed = JSON.parse(readFileSync(path, "utf8"));
	return { ...structuredClone(defaultConfig), ...parsed, watch: { ...defaultConfig.watch, ...parsed.watch }, inbound: { ...defaultConfig.inbound, ...parsed.inbound, rules: parsed.inbound?.rules ?? [] }, outbound: { ...defaultConfig.outbound, ...parsed.outbound, guardedExplicit: { ...defaultConfig.outbound.guardedExplicit, ...parsed.outbound?.guardedExplicit }, retry: { ...defaultConfig.outbound.retry, ...parsed.outbound?.retry } } };
}
export function saveConfig(cwd: string, config: OnlyneConfig) { const path = configPath(cwd); mkdirSync(dirname(path), { recursive: true }); writeFileSync(path, `${JSON.stringify(config, null, 2)}\n`); }
export function inboundModeFor(config: OnlyneConfig, channel: string, conversation?: string): InboundMode {
	return config.inbound.rules.find((r) => r.channel === channel && r.conversation === conversation)?.mode ?? config.inbound.rules.find((r) => r.channel === channel && !r.conversation)?.mode ?? config.inbound.defaultMode;
}
