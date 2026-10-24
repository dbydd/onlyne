import { afterEach, describe, expect, test } from "bun:test";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
	ConfigError,
	configPath,
	loadConfig,
	normalizeNodeId,
	normalizeRegion,
	parseConfig,
	saveConfig,
	serializeConfig,
	shortNodeId,
} from "../src/config.ts";
import type { OnlyneConfig } from "../src/types.ts";

const SAMPLE = `# project config
[node]
id = "urn:onlyne:agent:lazuli"
region = "urn:onlyne:region:research-chamber"
key_ref = "env:ONLYNE_NODE_TOKEN"

[server]
bind = "127.0.0.1"

[threads]
page_size = 50
`;

const PATH = "/tmp/project/.onlyne/config.toml";

const directories: string[] = [];

afterEach(async () => {
	await Promise.all(directories.splice(0).map(path => rm(path, { recursive: true, force: true })));
});

async function project(): Promise<string> {
	const path = await mkdtemp(join(tmpdir(), "omp-onlyne-config-"));
	directories.push(path);
	return path;
}

function draft(cwd: string): OnlyneConfig {
	return {
		path: configPath(cwd),
		region: "research-chamber",
		node: { id: "urn:onlyne:agent:lazuli" },
		server: { bind: "127.0.0.1" },
		threads: { page_size: 25 },
	};
}

describe("config", () => {
	test("parses the project config into the reduced shape", () => {
		expect(parseConfig(SAMPLE, PATH)).toEqual({
			path: PATH,
			region: "urn:onlyne:region:research-chamber",
			node: { id: "urn:onlyne:agent:lazuli", key_ref: "env:ONLYNE_NODE_TOKEN" },
			server: { bind: "127.0.0.1" },
			threads: { page_size: 50 },
		});
	});

	test("normalizes short regions and keeps absolute URIs", () => {
		expect(normalizeRegion("research chamber")).toBe("urn:onlyne:region:research%20chamber");
		expect(normalizeRegion("  research-chamber  ")).toBe("urn:onlyne:region:research-chamber");
		expect(normalizeRegion("urn:onlyne:region:research-chamber")).toBe("urn:onlyne:region:research-chamber");
		expect(() => normalizeRegion("   ")).toThrow(ConfigError);
	});

	test("normalizes a short stored region on load", () => {
		const text = SAMPLE.replace('region = "urn:onlyne:region:research-chamber"', 'region = "research-chamber"');
		expect(parseConfig(text, PATH).region).toBe("urn:onlyne:region:research-chamber");
	});

	test("normalizes a short stored node id on load", () => {
		const text = SAMPLE.replace('id = "urn:onlyne:agent:lazuli"', 'id = "lazuli"');
		expect(parseConfig(text, PATH).node.id).toBe("urn:onlyne:agent:lazuli");
	});

	test("defaults bind and page size", () => {
		const text = `[node]\nid = "urn:onlyne:agent:a"\nregion = "urn:onlyne:region:r"\n`;
		expect(parseConfig(text, PATH)).toMatchObject({
			server: { bind: "127.0.0.1" },
			threads: { page_size: 50 },
		});
	});

	test("rejects a missing or empty node id", () => {
		expect(() => parseConfig(SAMPLE.replace('id = "urn:onlyne:agent:lazuli"\n', ""), PATH)).toThrow(ConfigError);
		expect(() => parseConfig(SAMPLE.replace('id = "urn:onlyne:agent:lazuli"', 'id = ""'), PATH)).toThrow(ConfigError);
		expect(() => parseConfig(SAMPLE.replace('id = "urn:onlyne:agent:lazuli"', 'id = "   "'), PATH)).toThrow(
			ConfigError,
		);
	});

	test("normalizes short node ids and keeps absolute URIs", () => {
		expect(normalizeNodeId("lazuli")).toBe("urn:onlyne:agent:lazuli");
		expect(normalizeNodeId("research chamber")).toBe("urn:onlyne:agent:research%20chamber");
		expect(normalizeNodeId("  lazuli  ")).toBe("urn:onlyne:agent:lazuli");
		expect(normalizeNodeId("urn:onlyne:agent:lazuli")).toBe("urn:onlyne:agent:lazuli");
		expect(normalizeNodeId("https://example.com/node")).toBe("https://example.com/node");
		expect(() => normalizeNodeId("   ")).toThrow(ConfigError);
	});

	test("shows the short name of a node id", () => {
		expect(shortNodeId("urn:onlyne:agent:lazuli")).toBe("lazuli");
		expect(shortNodeId("urn:onlyne:agent:research%20chamber")).toBe("research chamber");
		expect(shortNodeId("urn:onlyne:region:research-chamber")).toBe("urn:onlyne:region:research-chamber");
		expect(shortNodeId("https://example.com/node")).toBe("https://example.com/node");
	});

	test("round-trips short names through the node id", () => {
		for (const name of ["lazuli", "research chamber", "hash#tag", "agent/one"]) {
			expect(shortNodeId(normalizeNodeId(name))).toBe(name);
		}
	});

	test("rejects a missing or empty region", () => {
		expect(() =>
			parseConfig(SAMPLE.replace('region = "urn:onlyne:region:research-chamber"\n', ""), PATH),
		).toThrow(ConfigError);
		expect(() =>
			parseConfig(SAMPLE.replace('region = "urn:onlyne:region:research-chamber"', 'region = ""'), PATH),
		).toThrow(ConfigError);
	});

	test("rejects an invalid bind and page size", () => {
		expect(() => parseConfig(SAMPLE.replace('bind = "127.0.0.1"', 'bind = ""'), PATH)).toThrow(ConfigError);
		expect(() => parseConfig(SAMPLE.replace('bind = "127.0.0.1"', 'bind = "127.0.0.1 7801"'), PATH)).toThrow(
			ConfigError,
		);
		expect(() => parseConfig(SAMPLE.replace("page_size = 50", "page_size = 0"), PATH)).toThrow(ConfigError);
		expect(() => parseConfig(SAMPLE.replace("page_size = 50", "page_size = 101"), PATH)).toThrow(ConfigError);
	});

	test("rejects retired keys instead of migrating them", () => {
		const retired = [
			SAMPLE.replace('key_ref = "env:ONLYNE_NODE_TOKEN"', 'key_ref = "env:ONLYNE_NODE_TOKEN"\nendpoint = "http://127.0.0.1:7801/onlyne/v3"'),
			SAMPLE.replace('bind = "127.0.0.1"', 'bind = "127.0.0.1"\nport = 7801'),
			SAMPLE.replace("page_size = 50", 'page_size = 50\ndefault_attention = "inbox"'),
			`${SAMPLE}\n[peers."urn:onlyne:agent:reviewer"]\nendpoint = "http://127.0.0.1:7802/onlyne/v3"\n`,
			`${SAMPLE}\n[omp]\nspawn = "async"\n`,
		];
		for (const text of retired) expect(() => parseConfig(text, PATH)).toThrow(ConfigError);
	});

	test("round-trips through serializeConfig", () => {
		const first = parseConfig(SAMPLE, PATH);
		expect(parseConfig(serializeConfig(first), first.path)).toEqual(first);
	});

	test("rejects a missing file instead of inventing an identity", async () => {
		const cwd = await project();
		const error: unknown = await loadConfig(cwd).catch(value => value);
		expect(error).toBeInstanceOf(ConfigError);
		expect(error).toMatchObject({ code: "missing" });
	});

	test("validates before writing and reloads what it wrote", async () => {
		const cwd = await project();
		const config = draft(cwd);
		const saved = await saveConfig(config);
		expect(saved.region).toBe("urn:onlyne:region:research-chamber");
		expect(await loadConfig(cwd)).toEqual(saved);
		expect((await readFile(config.path, "utf8")).startsWith("# Onlyne project configuration")).toBe(true);
	});

	test("does not write an invalid configuration", async () => {
		const cwd = await project();
		await expect(saveConfig({ ...draft(cwd), node: { id: "   " } })).rejects.toBeInstanceOf(ConfigError);
		await expect(loadConfig(cwd)).rejects.toMatchObject({ code: "missing" });
	});

	test("accepts literal strings and comments", async () => {
		const cwd = await project();
		const path = configPath(cwd);
		await mkdir(join(cwd, ".onlyne"), { recursive: true });
		await writeFile(
			path,
			SAMPLE.replace('id = "urn:onlyne:agent:lazuli"', "id = 'urn:onlyne:agent:hash#tag' # comment"),
			"utf8",
		);
		expect((await loadConfig(cwd)).node.id).toBe("urn:onlyne:agent:hash#tag");
	});
});
