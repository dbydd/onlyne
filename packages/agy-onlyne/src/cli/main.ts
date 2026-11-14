#!/usr/bin/env bun
import { resolve } from "node:path";
import type { Json } from "@onlyne/lib";
import { getGuiHtml } from "../gui/build-html.ts";
import { DaemonServer } from "../runtime/daemon.ts";
import { DaemonClient, checkDaemonHealth, defaultDaemonCommand, readDaemonInfo } from "../runtime/daemon-client.ts";
import { resolveWorkspaceRoot } from "../runtime/workspace.ts";
import { runOnlyneTui } from "../tui/app.ts";

interface ParsedCli {
	positionals: string[];
	cwd?: string;
	init: boolean;
	open: boolean;
	meta?: string;
	help: boolean;
}

function parseArgs(argv: string[]): ParsedCli {
	const out: ParsedCli = {
		positionals: [],
		init: false,
		open: false,
		help: false,
	};
	for (let i = 0; i < argv.length; i += 1) {
		const arg = argv[i]!;
		if (arg === "--help" || arg === "-h") {
			out.help = true;
		} else if (arg === "--init") {
			out.init = true;
		} else if (arg === "--open") {
			out.open = true;
		} else if (arg === "--cwd" && i + 1 < argv.length) {
			out.cwd = resolve(argv[++i]!);
		} else if (arg.startsWith("--cwd=")) {
			out.cwd = resolve(arg.slice("--cwd=".length));
		} else if (arg === "--meta" && i + 1 < argv.length) {
			out.meta = argv[++i]!;
		} else if (arg.startsWith("--meta=")) {
			out.meta = arg.slice("--meta=".length);
		} else {
			out.positionals.push(arg);
		}
	}
	return out;
}

function printHelp(): void {
	process.stdout.write(
		[
			"onlyne-agy — Onlyne v3 Antigravity Node CLI, TUI & Web GUI",
			"",
			"Usage:",
			"  onlyne-agy [tui] [--init] [--cwd <dir>]      Launch interactive 4-column TUI panel",
			"  onlyne-agy init [--cwd <dir>]                Open node configuration wizard in TUI",
			"  onlyne-agy gui [--open] [--cwd <dir>]        Print (or open) local Web GUI URL",
			"  onlyne-agy status [--cwd <dir>]              Print live node JSON status",
			"  onlyne-agy read <onlyne://...> [--cwd <dir>] Read an onlyne:// view",
			"  onlyne-agy msg <kind> <msg> [--meta <json>]  Send a message via Onlyne SystemOne",
			"  onlyne-agy daemon <run|start|stop|status>    Manage background workspace daemon",
			"",
		].join("\n"),
	);
}

function openExternalUrl(url: string): void {
	const cmd = process.platform === "darwin"
		? ["open", url]
		: process.platform === "win32"
			? ["cmd", "/c", "start", "", url]
			: ["xdg-open", url];
	try {
		const proc = Bun.spawn(cmd, { stdout: "ignore", stderr: "ignore", stdin: "ignore" });
		proc.unref();
	} catch {
		// Fallback gracefully; URL is already printed to stdout
	}
}

export async function runCli(argv: string[], entryPath = import.meta.path): Promise<number> {
	const args = parseArgs(argv);
	if (args.help) {
		printHelp();
		return 0;
	}

	const cmd = args.positionals[0] ?? (args.init ? "init" : "tui");
	const cwd = await resolveWorkspaceRoot({ cwd: args.cwd });
	const spawnCommand = defaultDaemonCommand(entryPath, cwd);

	if (cmd === "daemon") {
		const sub = args.positionals[1] ?? "status";
		if (sub === "run") {
			const daemon = await DaemonServer.start({
				cwd,
				renderGuiHtml: getGuiHtml,
			});
			const onSignal = (): void => {
				void daemon.close().then(() => process.exit(0));
			};
			process.on("SIGINT", onSignal);
			process.on("SIGTERM", onSignal);
			// Keep running until signal or shutdown HTTP call closes server
			await new Promise<void>(() => undefined);
			return 0;
		}
		if (sub === "start") {
			const client = await DaemonClient.connectOrSpawn({ cwd, spawnCommand });
			process.stdout.write(`${JSON.stringify({ ok: true, ctrlUrl: client.ctrlUrl, guiUrl: client.guiUrl })}\n`);
			return 0;
		}
		if (sub === "stop") {
			const info = await readDaemonInfo(cwd);
			if (!info || !(await checkDaemonHealth(info))) {
				process.stdout.write(`${JSON.stringify({ ok: true, stopped: false })}\n`);
				return 0;
			}
			const client = DaemonClient.fromInfo(info);
			await client.shutdown();
			process.stdout.write(`${JSON.stringify({ ok: true, stopped: true })}\n`);
			return 0;
		}
		if (sub === "status") {
			const info = await readDaemonInfo(cwd);
			const healthy = info ? await checkDaemonHealth(info) : false;
			process.stdout.write(`${JSON.stringify({ running: healthy, ...(info ? info : {}) }, null, 2)}\n`);
			return 0;
		}
		throw new Error(`Unknown daemon subcommand: ${sub}`);
	}

	if (cmd === "tui" || cmd === "init") {
		const client = await DaemonClient.connectOrSpawn({ cwd, spawnCommand });
		await runOnlyneTui({
			cwd,
			client,
			openBootstrapFirst: cmd === "init" || args.init,
		});
		return 0;
	}

	if (cmd === "gui") {
		const client = await DaemonClient.connectOrSpawn({ cwd, spawnCommand });
		process.stdout.write(`${client.guiUrl}\n`);
		if (args.open) openExternalUrl(client.guiUrl);
		return 0;
	}

	if (cmd === "status") {
		const client = await DaemonClient.connectOrSpawn({ cwd, spawnCommand });
		const snap = await client.snapshot();
		process.stdout.write(`${JSON.stringify(snap.status, null, 2)}\n`);
		return 0;
	}

	if (cmd === "read") {
		const url = args.positionals[1];
		if (!url) throw new Error("Usage: onlyne-agy read <onlyne://...>");
		const client = await DaemonClient.connectOrSpawn({ cwd, spawnCommand });
		const res = await client.read(url, { agent_id: "cli", main: true });
		process.stdout.write(`${res.content}\n`);
		return 0;
	}

	if (cmd === "msg") {
		const kind = args.positionals[1];
		const text = args.positionals[2];
		if (!kind || text === undefined) {
			throw new Error("Usage: onlyne-agy msg <delivery_kind> <msg> [--meta <json>]");
		}
		let meta: Record<string, Json> | undefined;
		if (args.meta) {
			const parsed = JSON.parse(args.meta) as unknown;
			if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) {
				throw new Error("--meta must be a JSON object");
			}
			meta = parsed as Record<string, Json>;
		}
		const client = await DaemonClient.connectOrSpawn({ cwd, spawnCommand });
		const res = await client.msg({ delivery_kind: kind, msg: text, ...(meta ? { meta } : {}) }, {
			agent_id: "cli",
			main: true,
			read_only: false,
		});
		process.stdout.write(`${res.text}\n`);
		return 0;
	}

	throw new Error(`Unknown command: ${cmd}. Run with --help.`);
}

if (import.meta.main) {
	void runCli(process.argv.slice(2)).then(
		code => process.exit(code),
		err => {
			process.stderr.write(`${err instanceof Error ? err.message : String(err)}\n`);
			process.exit(1);
		},
	);
}
