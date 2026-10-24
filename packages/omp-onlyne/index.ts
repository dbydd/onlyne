import type { ExtensionAPI } from "@oh-my-pi/pi-coding-agent";
import { registerContextBinding } from "./src/binding.ts";
import { ConfigError } from "./src/config.ts";
import { registerMessageRenderers, registerProviderBoundary } from "./src/messages.ts";
import { registerTodoRecovery } from "./src/todo.ts";
import { startRuntime, stopRuntime } from "./src/runtime.ts";
import { registerMsgTool } from "./src/tool.ts";
import { registerOnlyneUrls } from "./src/urls.ts";
import { attachStatus, registerUi } from "./src/ui/index.ts";

/**
 * OMP extension entry. The factory runs for every session, including subagents;
 * the network node belongs to the main session only. State recovery and the
 * listener start on `session_start`, never during factory execution.
 */
export default function ompOnlyne(pi: ExtensionAPI): void {
	registerOnlyneUrls();
	registerMessageRenderers(pi);
	registerProviderBoundary(pi);
	registerMsgTool(pi);
	registerContextBinding(pi);
	registerTodoRecovery(pi);
	registerUi(pi);
	let detachStatus: (() => void) | undefined;

	pi.on("session_start", async (_event, ctx) => {
		if (ctx.agent.kind !== "main") return;
		if (ctx.hasUI) {
			try {
				detachStatus?.();
				detachStatus = attachStatus(ctx);
			} catch (error) {
				pi.logger.warn("onlyne status widget could not be attached", {
					error: error instanceof Error ? error.message : String(error),
				});
			}
		}
		try {
			await startRuntime(pi, ctx);
		} catch (error) {
			// A project without a saved config is the normal first-run state; /onlyne init creates it.
			if (error instanceof ConfigError && error.code === "missing") return;
			pi.logger.warn("onlyne could not start for this project", {
				error: error instanceof Error ? error.message : String(error),
			});
		}
	});

	pi.on("session_shutdown", async (_event, ctx) => {
		if (ctx.agent.kind !== "main") return;
		detachStatus?.();
		detachStatus = undefined;
		await stopRuntime();
	});
}
