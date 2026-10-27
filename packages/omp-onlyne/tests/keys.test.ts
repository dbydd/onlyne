import { afterEach, describe, expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { generateKeyMaterial } from "@onlyne/lib";
import { loadOrCreateKey, writeKeyMaterial } from "@onlyne/node-host/keys";

const dirs: string[] = [];
afterEach(async () => { for (const dir of dirs.splice(0)) await rm(dir, { recursive: true, force: true }); });

async function keyDir(): Promise<string> {
	const dir = await mkdtemp(join(tmpdir(), "onlyne-keys-"));
	dirs.push(dir);
	return dir;
}

describe("key files", () => {
	// trust.rotate reaches the host through plugins.keys.replace; a create-path guard must not
	// block it, or an initialized node can never change its identity (RFC 0017 §2).
	test("replace rewrites the stored key, create refuses to", async () => {
		const dir = await keyDir();
		const first = await loadOrCreateKey(dir, "urn:onlyne:agent:keyed");
		await expect(writeKeyMaterial(dir, generateKeyMaterial())).rejects.toThrow(/already exists/);
		const next = generateKeyMaterial();
		await writeKeyMaterial(dir, next, { replace: true });
		const reloaded = await loadOrCreateKey(dir, "urn:onlyne:agent:keyed");
		expect(Buffer.from(reloaded.private_key)).toEqual(Buffer.from(next.private_key));
		expect(Buffer.from(reloaded.private_key)).not.toEqual(Buffer.from(first.private_key));
	});
});
