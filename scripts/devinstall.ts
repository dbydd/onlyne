/**
 * Prepares this checkout for local development.
 *
 *   bun run devinstall
 *
 * It installs workspace dependencies, registers every package with `bun link` so another project
 * can consume the working tree, exposes the two gateway CLIs on PATH, and links the OMP plugin
 * through OMP itself. Every step is idempotent, so running it again after a pull is safe.
 */
import { homedir } from "node:os";
import { readdir } from "node:fs/promises";
import { join } from "node:path";

const ROOT = join(import.meta.dir, "..");
const PACKAGES = join(ROOT, "packages");
const BIN_DIR = join(homedir(), ".bun", "bin");
const BINS = ["onlyne-im-gateway", "onlyne-mcp-gateway"];

function run(cmd: string[], cwd: string): boolean {
  const result = Bun.spawnSync({ cmd, cwd, stdout: "pipe", stderr: "pipe" });
  if (result.exitCode === 0) return true;
  console.error(`\nfailed: ${cmd.join(" ")}  (in ${cwd})`);
  const detail = result.stderr.toString().trim() || result.stdout.toString().trim();
  if (detail.length > 0) console.error(detail);
  return false;
}

console.log("== bun install");
if (!run(["bun", "install"], ROOT)) process.exit(1);

const entries = await readdir(PACKAGES, { withFileTypes: true });
const directories: string[] = [];
for (const entry of entries) {
  if (!entry.isDirectory()) continue;
  const directory = join(PACKAGES, entry.name);
  if (!(await Bun.file(join(directory, "package.json")).exists())) continue;
  directories.push(directory);
}
directories.sort();

console.log(`\n== bun link (${directories.length} packages)`);
for (const directory of directories) {
  const manifest = (await Bun.file(join(directory, "package.json")).json()) as { name: string };
  if (!run(["bun", "link"], directory)) process.exit(1);
  console.log(`   ${manifest.name}`);
}

console.log("\n== gateway binaries");
for (const name of BINS) {
  const path = join(BIN_DIR, name);
  console.log(`   ${(await Bun.file(path).exists()) ? "ok  " : "MISS"} ${path}`);
}

console.log("\n== OMP plugin");
const plugin = join(PACKAGES, "omp-onlyne");
if (Bun.which("omp") === null) {
  console.log(`   omp is not on PATH. After installing it, run:\n   omp plugin link ${plugin}`);
} else {
  if (!run(["omp", "plugin", "link", plugin], ROOT)) process.exit(1);
  const listed = Bun.spawnSync({ cmd: ["omp", "plugin", "list", "--json"], cwd: ROOT, stdout: "pipe", stderr: "pipe" });
  try {
    const registry = JSON.parse(listed.stdout.toString()) as { npm?: Array<{ name: string; enabled: boolean; path: string }> };
    const found = registry.npm?.find(entry => entry.name === "omp-onlyne");
    console.log(found === undefined
      ? "   MISS omp-onlyne is not in the plugin list"
      : `   ${found.enabled ? "ok  " : "OFF "} ${found.name} -> ${found.path}`);
  } catch {
    console.log("   could not read the OMP plugin list; run omp plugin list yourself");
  }
}

console.log("\nDone. Source edits take effect on the next command, because every entry is a symlink.");
