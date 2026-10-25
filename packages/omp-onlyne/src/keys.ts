import { chmod, mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { chmodSync, existsSync, mkdirSync, readFileSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { basename, join } from "node:path";
import type { KeyMaterial } from "@onlyne/lib";
import { execFile } from "node:child_process";
import { generateKeyMaterial } from "@onlyne/lib";

/** RFC 0017 §2: the private key rides in its own file as base64 DER; the public half is derived on load. */
const PRIVATE_NAME = "priv.pem";
const PUBLIC_NAME = "pub.key";

function projectKeysRoot(cwd: string): string {
	return join(cwd, ".onlyne", "keys");
}

function globalKeysRoot(): string {
	return join(homedir(), ".config", "onlyne", "keys");
}

function encodePrivate(material: KeyMaterial): string {
	return Buffer.from(material.private_key).toString("base64");
}

function decodePrivate(text: string): Uint8Array {
	return new Uint8Array(Buffer.from(text, "base64"));
}

/** Refuses group/world-readable private keys and symlinked or foreign-owned files. */
function assertPrivatePermissions(path: string): void {
	const stats = statSync(path, { throwIfNoEntry: false });
	if (stats === undefined) return;
	if (stats.isSymbolicLink()) throw new Error(`The node key must not be a symlink: ${path}`);
	if ((stats.mode & 0o077) !== 0) {
		throw new Error(`The node key is readable beyond its owner. Fix it with: chmod 600 ${path}`);
	}
}

async function assertDirectoryPermissions(path: string): Promise<void> {
	const stats = await statSync(path, { throwIfNoEntry: false });
	if (stats === undefined || (stats.mode & 0o077) !== 0) {
		throw new Error(`The key directory is readable beyond its owner. Fix it with: chmod 700 ${path}`);
	}
}

/** The node id names a subdirectory; a traversal-looking id is a configuration error. */
function keyDirectory(root: string, nodeId: string): string {
	if (nodeId.includes("/") || nodeId.includes("\\") || nodeId.includes("..")) {
		throw new Error(`The node id cannot name a key directory: ${nodeId}`);
	}
	return join(root, encodeURIComponent(nodeId));
}

/**
 * The git check is `git check-ignore --quiet`, the real semantics: an ancestor ignore,
 * a negation rule, or a .git/info/exclude all read the same way the user's git does.
 */
function ignoredByGit(root: string, target: string): Promise<boolean> {
	return new Promise(resolve => {
		// `cwd` is the project root: the key directory does not exist yet on a first run.
		execFile("git", ["check-ignore", "--quiet", "--", target], { cwd: root }, error => {
			// Exit 0 = ignored, exit 1 = tracked-or-untracked but not ignored, other = outside a work tree.
			resolve(error === null || (typeof (error as { code?: number }).code === "number" && (error as { code?: number }).code === 0));
		});
	});
}

async function insideGitWorkTree(path: string): Promise<boolean> {
	return new Promise(resolve => {
		execFile("git", ["rev-parse", "--is-inside-work-tree"], { cwd: path }, (error, stdout) => {
			resolve(error === null && stdout.trim() === "true");
		});
	});
}

/**
 * Resolves the key directory for a node (RFC 0017 §2). A project key inside a git work
 * tree whose ignore rules do not cover `.onlyne/keys` is refused, with both escapes named.
 */
export async function ensureKeyDir(cwd: string, nodeId: string, scope: "project" | "global"): Promise<string> {
	// The global directory holds the node's identity by default; a project directory is an
	// override for this working tree, read first when both exist.
	const root = scope === "project" ? projectKeysRoot(cwd) : globalKeysRoot();
	const dir = keyDirectory(root, nodeId);
	if (scope === "global") {
		await mkdir(dir, { recursive: true });
		await chmod(dir, 0o700);
		return dir;
	}
	if (await insideGitWorkTree(cwd) && !(await ignoredByGit(cwd, dir)) && !(await ignoredByGit(cwd, projectKeysRoot(cwd)))) {
		throw new Error(
			"Refusing to create a node key inside a git work tree: .onlyne/keys is not ignored. "
			+ "Add '.onlyne/keys' to .gitignore, or set node.key_scope = \"global\" in the config.",
		);
	}
	await mkdir(dir, { recursive: true });
	await chmod(dir, 0o700);
	return dir;
}

/** Loads the node's identity key, generating it on first use. */
export async function loadOrCreateKey(dir: string, nodeId: string): Promise<KeyMaterial> {
	await assertDirectoryPermissions(dir);
	const privatePath = join(dir, PRIVATE_NAME);
	if (!existsSync(privatePath)) {
		const material = generateKeyMaterial();
		await writeKeyMaterial(dir, material);
		return material;
	}
	assertPrivatePermissions(privatePath);
	const base: { private_key: Uint8Array; public_key: Uint8Array } = {
		private_key: decodePrivate((await readFile(privatePath, "utf8")).trim()),
		public_key: new Uint8Array(),
	};
	// The public half always comes from the private key: the file is a cache, never a source.
	const { createPrivateKey, createPublicKey } = await import("node:crypto");
	const pair = createPrivateKey({ key: Buffer.from(base.private_key), format: "der", type: "pkcs8" });
	const jwk = createPublicKey(pair).export({ format: "jwk" });
	return { private_key: base.private_key, public_key: new Uint8Array(Buffer.from(String(jwk.x), "base64url")) };
}

/**
 * Writes the key pair atomically. `replace` is the `plugins.keys.replace` path — the only
 * caller allowed to overwrite an existing private key, because `trust.rotate` has already
 * committed the rotation window and must complete the swap.
 */
export async function writeKeyMaterial(dir: string, material: KeyMaterial, options: { replace?: boolean } = {}): Promise<void> {
	const privatePath = join(dir, PRIVATE_NAME);
	if (existsSync(privatePath) && options.replace !== true) throw new Error(`A node key already exists at ${privatePath}; use trust rotation instead of overwriting it.`);
	const publicPath = join(dir, PUBLIC_NAME);
	const stagingPrivate = join(dir, `.${basename(PRIVATE_NAME)}.${process.pid}.tmp`);
	const stagingPublic = join(dir, `.${basename(PUBLIC_NAME)}.${process.pid}.tmp`);
	// Every fallible step runs before the private key is renamed: a replace that throws must
	// leave the old key usable, or the rotation proof for peers pinning it is gone for good.
	await writeFile(stagingPrivate, encodePrivate(material), { mode: 0o600 });
	await chmod(stagingPrivate, 0o600);
	await writeFile(stagingPublic, Buffer.from(material.public_key).toString("base64"), { mode: 0o644 });
	await chmod(stagingPublic, 0o644);
	// The public file is a cache derived from the private key, so it may be replaced first.
	await rename(stagingPublic, publicPath);
	await rename(stagingPrivate, privatePath);
}

/** Synchronous guard for the startup path: the existing key must already be private. */
export function assertKeyPermissions(dir: string): void {
	assertPrivatePermissions(join(dir, PRIVATE_NAME));
	void chmod;
	void chmodSync;
	void mkdirSync;
	void readFileSync;
}
