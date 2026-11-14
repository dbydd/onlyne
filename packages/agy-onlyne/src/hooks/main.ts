import { dirname, join } from "node:path";
import { DaemonClient, defaultDaemonCommand } from "../runtime/daemon-client.ts";
import { handleHookEvent } from "./handler.ts";

function defaultCliEntryPath(): string {
	const here = import.meta.path;
	if (here.endsWith("/dist/hook.js") || here.endsWith("/plugin/dist/hook.js")) {
		return join(dirname(here), "cli.js");
	}
	if (here.endsWith("/dist/hooks/main.js")) {
		return join(dirname(here), "../cli/main.js");
	}
	return join(dirname(here), "../cli/main.ts");
}

async function run(): Promise<void> {
	const stdinText = await Bun.stdin.text();
	const cliEntry = defaultCliEntryPath();
	const payload = await handleHookEvent({
		stdinText,
		getClient: cwd =>
			DaemonClient.connectOrSpawn({
				cwd,
				spawnCommand: defaultDaemonCommand(cliEntry, cwd),
			}),
	});
	process.stdout.write(`${JSON.stringify(payload)}\n`);
}

if (import.meta.main) {
	void run().catch(() => {
		process.stdout.write("{}\n");
	});
}
