import type { ExtensionAPI } from "@oh-my-pi/pi-coding-agent";
import { ConfigError, loadConfig } from "../config.ts";
import { getRuntime, startRuntime } from "../runtime.ts";
import { detectTernSession } from "../tern.ts";
import type { OnlyneConfig } from "../types.ts";
import { OnlyneBootstrap, type BootstrapDefaults } from "./bootstrap.ts";
import { OnlyneViewer } from "./viewer.ts";

export { attachStatus, OnlyneStatus } from "./status.ts";

const USAGE = "用法：/onlyne 或 /onlyne menu 打开界面 · /onlyne init 首次创建配置 · /onlyne config 修改配置（含节点密钥）。";

/** The subcommands the slash command accepts, with the one-line hint the editor shows. */
const SUBCOMMANDS: ReadonlyArray<{ label: string; description: string }> = [
	{ label: "init", description: "首次创建项目配置（含节点密钥）；已配置时改用 config" },
	{ label: "config", description: "修改项目配置：region、节点身份、密钥位置、种子地址" },
	{ label: "menu", description: "打开 Onlyne 界面（与直接 /onlyne 相同）" },
];

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
		description: "Show the Onlyne queue and threads, or edit the project config",
		getArgumentCompletions(argumentPrefix: string) {
			const prefix = argumentPrefix.trim().toLowerCase();
			const matches = SUBCOMMANDS.filter(item => item.label.startsWith(prefix));
			return matches.length === 0 ? null : matches.map(item => ({ label: item.label, value: item.label, description: item.description }));
		},
		handler: async (args, ctx) => {
			const command = args.trim();
			if (command !== "" && command !== "init" && command !== "config" && command !== "menu" && command !== "settings") {
				ctx.ui.notify(USAGE, "warning");
				return;
			}
			if (!ctx.hasUI) {
				pi.sendMessage({
					customType: "onlyne.ui-unavailable",
					content: "Onlyne 的查看界面和配置面板需要交互式终端。请在交互式终端运行 /onlyne init 生成项目配置。",
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
						ctx.ui.notify("这个项目已经配置过，修改请用 /onlyne config。", "info");
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
						? "Region 来自当前配置"
						: tern === undefined
							? "未检测到 Tern session；请填写 region"
							: `Region 建议值来自 Tern session "${tern}"，可修改`;
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
							),
						{
							overlay: true,
							// Four columns: the domain rail, threads, the message flow and members.
							overlayOptions: { anchor: "center", width: "92%", minWidth: 90, maxHeight: "90%" },
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
