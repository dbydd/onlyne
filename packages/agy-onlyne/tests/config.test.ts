import { afterEach, describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
	configPath,
	loadConfig,
	normalizeNodeId,
	normalizeRegion,
	normalizeServiceUrl,
	parseConfig,
	saveConfig,
	serializeConfig,
	type AgyOnlyneConfig,
} from "../src/config.ts";

const tempDirs: string[] = [];

function makeTempDir(prefix: string): string {
	const dir = mkdtempSync(join(tmpdir(), prefix));
	tempDirs.push(dir);
	return dir;
}

afterEach(() => {
	while (tempDirs.length > 0) {
		const dir = tempDirs.pop()!;
		rmSync(dir, { recursive: true, force: true });
	}
});

describe("agy-onlyne config", () => {
	test("normalizes region, node id, and service URLs", () => {
		expect(normalizeRegion("local")).toBe("urn:onlyne:region:local");
		expect(normalizeRegion("urn:onlyne:region:shanghai")).toBe("urn:onlyne:region:shanghai");
		expect(normalizeNodeId("agy-alpha")).toBe("urn:onlyne:agent:agy-alpha");
		expect(normalizeNodeId("urn:onlyne:node:custom")).toBe("urn:onlyne:node:custom");
		expect(normalizeServiceUrl("http://127.0.0.1:7801/")).toBe("http://127.0.0.1:7801/onlyne/v3");
		expect(normalizeServiceUrl("http://127.0.0.1:7801/onlyne/v3/")).toBe("http://127.0.0.1:7801/onlyne/v3");
	});

	test("round-trips full config through TOML serialize/parse and disk save/load", async () => {
		const dir = makeTempDir("agy-onlyne-cfg-");
		const cfgPath = configPath(dir);
		const cfg: AgyOnlyneConfig = {
			path: cfgPath,
			language: "zh-CN",
			region: normalizeRegion("beijing"),
			node: {
				id: normalizeNodeId("agy-test"),
				key_scope: "global",
			},
			server: {
				bind: "0.0.0.0",
				port: 7811,
				public_url: "http://10.0.0.5:7811/onlyne/v3",
			},
			threads: { page_size: 80 },
			discovery: {
				seeds: ["http://127.0.0.1:7802"],
			},
			acquisition: {
				policy: "segment",
				allow: ["127.0.0.1/32", "10.0.0.0/8"],
			},
			trust: [
				{
					node_id: "urn:onlyne:agent:peer-b",
					fingerprint: "fp-peer-b",
				},
			],
			judge: {
				mode: "gemini",
				model: "gemini-2.5-flash",
				timeout_ms: 15000,
			},
			wake: {
				mode: "hook",
				escalate_ms: 9000,
			},
		};

		const toml = serializeConfig(cfg);
		const parsed = parseConfig(toml, cfgPath);
		expect(parsed.region).toBe("urn:onlyne:region:beijing");
		expect(parsed.node.id).toBe("urn:onlyne:agent:agy-test");
		expect(parsed.node.key_scope).toBe("global");
		expect(parsed.server.port).toBe(7811);
		expect(parsed.server.public_url).toBe("http://10.0.0.5:7811/onlyne/v3");
		expect(parsed.threads.page_size).toBe(80);
		expect(parsed.discovery?.seeds).toEqual(["http://127.0.0.1:7802/onlyne/v3"]);
		expect(parsed.acquisition?.policy).toBe("segment");
		expect(parsed.acquisition?.allow).toEqual(["127.0.0.1/32", "10.0.0.0/8"]);
		expect(parsed.trust?.[0]?.node_id).toBe("urn:onlyne:agent:peer-b");
		expect(parsed.judge?.mode).toBe("gemini");
		expect(parsed.judge?.model).toBe("gemini-2.5-flash");
		expect(parsed.wake?.mode).toBe("hook");
		expect(parsed.wake?.escalate_ms).toBe(9000);
		expect(parsed.language).toBe("zh-CN");

		await saveConfig(cfg);
		const loaded = await loadConfig(dir);
		expect(loaded).toBeDefined();
		expect(loaded?.node.id).toBe("urn:onlyne:agent:agy-test");
		expect(loaded?.discovery?.seeds).toEqual(["http://127.0.0.1:7802/onlyne/v3"]);
	});

	test("accepts legacy [node].region and rejects invalid judge.mode / CIDR / port", () => {
		const legacy = parseConfig(
			`
[node]
id = "alpha"
region = "tokyo"
[server]
bind = "127.0.0.1"
`,
			"/tmp/test.toml",
		);
		expect(legacy.region).toBe("urn:onlyne:region:tokyo");
		expect(legacy.node.id).toBe("urn:onlyne:agent:alpha");

		expect(() => parseConfig(
			`
region = "local"
[node]
id = "a"
[server]
bind = "127.0.0.1"
port = 99999
`,
			"/tmp/bad-port.toml",
		)).toThrow("server.port");

		expect(() => parseConfig(
			`
region = "local"
[node]
id = "a"
[server]
bind = "127.0.0.1"
[acquisition]
policy = "segment"
allow = ["127.0.0.1/99"]
`,
			"/tmp/bad-cidr.toml",
		)).toThrow("acquisition.allow");

		expect(() => parseConfig(
			`
region = "local"
[node]
id = "a"
[server]
bind = "127.0.0.1"
[judge]
mode = "invalid-backend"
`,
			"/tmp/bad-judge.toml",
		)).toThrow("judge.mode");
	});
});
