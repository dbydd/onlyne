import type { ExtensionAPI } from "@oh-my-pi/pi-coding-agent";
import { ConfigError, loadConfig } from "../config.ts";
import { getRuntime, startRuntime } from "../runtime.ts";
import { detectTernSession } from "../tern.ts";
import type { OnlyneConfig } from "../types.ts";
import { OnlyneBootstrap, type BootstrapDefaults } from "./bootstrap.ts";
import { OnlyneViewer } from "./viewer.ts";
import { LOCALES, setLocale, type Locale } from "@onlyne/node-host/i18n";
import { saveConfig } from "../config.ts";
import { commandText as t } from "./command-text.ts";
import { registerMsgTool } from "../tool.ts";
import { refreshMailCards } from "./mail-message.ts";

export { attachStatus, OnlyneStatus } from "./status.ts";

/** The subcommands the slash command accepts, with the one-line hint the editor shows. */
const SUBCOMMANDS = ["init", "config", "menu", "language"] as const;

export function registerUi(pi: ExtensionAPI): void {
	let active: AbortController | undefined;
	const cancel = (): void => {
		active?.abort();
		active = undefined;
	};
	pi.on("session_switch", cancel);
	pi.on("session_branch", cancel);
	pi.on("session_tree", cancel);
	pi.on("session_shutdown", cancel);

	pi.registerCommand("onlyne", {
		description: t("description"),
		getArgumentCompletions(argumentPrefix: string) {
			const prefix = argumentPrefix.trim().toLowerCase();
			const matches = SUBCOMMANDS.filter(item => item.startsWith(prefix));
			return matches.length === 0 ? null : matches.map(item => ({ label: item, value: item, description: t(item) }));
		},
		handler: async (args, ctx) => {
			const command = args.trim();
			if (command === "language" || command.startsWith("language ")) {
				const language = command.slice("language".length).trim();
				if (!LOCALES.includes(language as Locale)) { ctx.ui.notify(t("usage"), "warning"); return; }
				try {
					const runtime = getRuntime();
					const config = runtime?.config ?? await loadConfig(ctx.cwd);
					await saveConfig({ ...config, language: language as Locale });
					if (runtime !== undefined) runtime.config.language = language as Locale;
					setLocale(language as Locale);
					registerMsgTool(pi);
					refreshMailCards();
					ctx.ui.notify(t("selected", { language }), "info");
				} catch (error) { ctx.ui.notify(error instanceof Error ? error.message : String(error), "error"); }
				return;
			}
			if (command !== "" && command !== "init" && command !== "config" && command !== "menu" && command !== "settings") {
				ctx.ui.notify(t("usage"), "warning");
				return;
			}
			if (!ctx.hasUI) {
				pi.sendMessage({
					customType: "onlyne.ui-unavailable",
					content: t("unavailable"),
					display: true,
					attribution: "agent",
				});
				return;
			}
			if (active !== undefined) return;
			const controller = new AbortController();
			active = controller;
			try {
				let runtime = getRuntime();
				if (command === "init") {
					const existing = runtime?.config ?? (await loadConfig(ctx.cwd).catch(() => undefined));
					if (existing !== undefined) {
						ctx.ui.notify(t("configured"), "info");
						return;
					}
				}
				// A saved config with the runtime down is not a first run: start the node and show
				// the viewer. Opening the init form here is what made `/onlyne` look like it had
				// cleared the project.
				if (runtime === undefined && command !== "init" && command !== "config") {
					try {
						runtime = await startRuntime(pi, ctx);
					} catch (error) {
						if (!(error instanceof ConfigError && error.code === "missing")) throw error;
					}
				}
				const needsBootstrap = command === "init" || command === "config" || command === "settings" || runtime === undefined;
				if (needsBootstrap) {
					const tern = detectTernSession();
					const current = runtime?.config ?? (await loadConfig(ctx.cwd).catch(() => undefined));
					const savedRegion = current?.region;
					const regionSource = savedRegion !== undefined
						? t("savedRegion")
						: tern === undefined
							? t("noTern")
							: t("ternRegion", { name: tern });
					const defaults: BootstrapDefaults = {
						region: savedRegion ?? tern ?? "",
						regionSource,
						nodeId: current?.node.id ?? "",
						bind: current?.server.bind ?? "127.0.0.1",
						keyScope: current?.node.key_scope ?? "project",
						pageSize: current?.threads.page_size ?? 50,
						...(current === undefined ? {} : { current }),
					};
					await ctx.ui.custom<void>(
						(tui, theme, _keys, done) =>
							new OnlyneBootstrap(
								pi,
								ctx,
								defaults,
								theme,
								() => tui.requestRender(),
								() => done(),
								controller.signal,
							),
						{
							overlay: true,
							overlayOptions: { anchor: "center", width: "70%", minWidth: 56, maxHeight: "85%" },
							signal: controller.signal,
						},
					);
				} else {
					await ctx.ui.custom<void>(
						(tui, theme, _keys, done) =>
							new OnlyneViewer(
								theme,
								() => tui.requestRender(),
								() => done(),
								controller.signal,
								() => tui.terminal.rows,
							),
						{
							overlay: true,
							// Fullscreen is what turns on pointer reporting: a normal overlay never gets mouse
							// events, so every click on a described node would go nowhere. The alt screen also
							// gives the sheet the whole pane, which the four columns need.
							overlayOptions: { margin: 0, fullscreen: true },
							signal: controller.signal,
						},
					);
				}
			} catch (error) {
				if (!controller.signal.aborted) {
					const message = error instanceof Error ? error.message : String(error);
					pi.logger.warn("onlyne.ui.failed", { error: message });
					ctx.ui.notify(message, "error");
				}
			} finally {
				if (active === controller) active = undefined;
			}
		},
	});
}
