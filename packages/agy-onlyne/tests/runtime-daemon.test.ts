import { afterEach, describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { configPath, loadConfig, normalizeNodeId, type AgyOnlyneConfig } from "../src/config.ts";
import { getGuiHtml } from "../src/gui/build-html.ts";
import { DaemonServer } from "../src/runtime/daemon.ts";
import { DaemonClient } from "../src/runtime/daemon-client.ts";

const servers: DaemonServer[] = [];
const tempDirs: string[] = [];

function makeTempDir(prefix: string): string {
	const dir = mkdtempSync(join(tmpdir(), prefix));
	tempDirs.push(dir);
	return dir;
}

function makeConfig(dir: string, id: string): AgyOnlyneConfig {
	return {
		path: configPath(dir),
		region: "urn:onlyne:region:local",
		node: { id: normalizeNodeId(id), key_scope: "project" },
		server: { bind: "127.0.0.1" },
		threads: { page_size: 50 },
		acquisition: { policy: "all", allow: ["127.0.0.1/32", "::1/128"] },
		judge: { mode: "none" },
	};
}

afterEach(async () => {
	while (servers.length > 0) {
		await servers.pop()!.close().catch(() => {});
	}
	while (tempDirs.length > 0) {
		rmSync(tempDirs.pop()!, { recursive: true, force: true });
	}
});

describe("DaemonServer, DaemonClient IPC & GUI HTTP/SSE transport", () => {
	test("connects two workspace daemons, serves GUI HTML, exchanges trust keys, updates acquisition, and persists config", async () => {
		const dirA = makeTempDir("agy-daemon-a-");
		const dirB = makeTempDir("agy-daemon-b-");

		const daemonA = await DaemonServer.start({
			cwd: dirA,
			renderGuiHtml: getGuiHtml,
			runtimeOptions: {
				inMemory: true,
				discovery: false,
				bindHttp: true,
				config: makeConfig(dirA, "daemon-alpha"),
			},
		});
		const daemonB = await DaemonServer.start({
			cwd: dirB,
			renderGuiHtml: getGuiHtml,
			runtimeOptions: {
				inMemory: true,
				discovery: false,
				bindHttp: true,
				config: makeConfig(dirB, "daemon-beta"),
			},
		});
		servers.push(daemonA, daemonB);

		const clientA = DaemonClient.fromInfo(daemonA.info());
		const clientB = DaemonClient.fromInfo(daemonB.info());

		// 1. Authenticated /ui serves compiled single-file GUI HTML; unauthorized fails
		const unauthUi = await fetch(`${daemonA.ctrlUrl}/ui`);
		expect(unauthUi.status).toBe(401);

		const authUi = await fetch(clientA.guiUrl);
		expect(authUi.status).toBe(200);
		const html = await authUi.text();
		expect(html.toLowerCase()).toContain("<!doctype html>");
		expect(html).toContain("Onlyne");

		// 2. Connect seeds & inspect discovery
		const snapB0 = await clientB.snapshot();
		await clientA.connect(snapB0.status.endpoint);
		const snapA1 = await clientA.snapshot();
		expect(snapA1.discovery.some(d => d.node_id === "urn:onlyne:agent:daemon-beta")).toBe(true);

		// 3. Begin + Confirm interactive key exchange
		const beginRes = await clientA.trustBegin("daemon-beta");
		expect(beginRes.node_id).toBe("urn:onlyne:agent:daemon-beta");
		expect(snapB0.fingerprint).toBeDefined();
		expect(beginRes.fingerprint).toBe(snapB0.fingerprint!);

		await clientA.trustConfirm("daemon-beta", beginRes.fingerprint);
		await clientB.trustConfirm("daemon-alpha", (await clientA.snapshot()).fingerprint!);

		const snapA2 = await clientA.snapshot();
		const entryB = snapA2.trust.find(tr => tr.node_id === "urn:onlyne:agent:daemon-beta");
		expect(entryB?.phase).toBe("exchanged");

		// 4. Update acquisition rules + language
		await clientA.setAcquisition("segment", ["127.0.0.1/32"]);
		await clientA.setLanguage("zh-CN");
		const snapA3 = await clientA.snapshot();
		expect(snapA3.acquisition.policy).toBe("segment");
		expect(snapA3.acquisition.allow).toEqual(["127.0.0.1/32"]);
		expect(snapA3.language).toBe("zh-CN");

		// 5. Save config via daemon action and verify persisted .onlyne/config.toml
		const saved = await clientA.saveConfig({
			region: "shanghai",
			node_id: "daemon-alpha-2",
			bind: "127.0.0.1",
			key_scope: "project",
			page_size: 40,
			policy: "segment",
			allow: ["127.0.0.1/32"],
			judge_mode: "none",
			language: "en",
		});
		expect(saved.ok).toBe(true);
		expect(saved.snapshot.configured).toBe(true);
		const diskCfg = await loadConfig(dirA);
		expect(diskCfg?.region).toBe("urn:onlyne:region:shanghai");
		expect(diskCfg?.threads.page_size).toBe(40);
	});
});
