/**
 * Sets the version of the publishable packages and keeps internal ranges in step.
 *
 *   bun scripts/version.ts list
 *   bun scripts/version.ts 3.1.0              # every @onlyne/* package
 *   bun scripts/version.ts 0.2.0 omp-onlyne   # one package
 *   bun scripts/version.ts 3.1.0 --dry-run
 *
 * A dependency range is rewritten only when the package it points at is part of the same run,
 * so bumping omp-onlyne alone leaves the @onlyne/* ranges untouched. Each manifest is edited as
 * text, so indentation and key order stay exactly as the file had them.
 */
import { readdir } from "node:fs/promises";
import { join } from "node:path";

const ROOT = join(import.meta.dir, "..");
const PACKAGES = join(ROOT, "packages");
const SECTIONS = ["dependencies", "peerDependencies", "devDependencies"] as const;
const SEMVER = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/;

interface Manifest {
  name: string;
  version: string;
  path: string;
  dependencies?: Record<string, string>;
  peerDependencies?: Record<string, string>;
  devDependencies?: Record<string, string>;
}

async function readManifests(): Promise<Manifest[]> {
  const entries = await readdir(PACKAGES, { withFileTypes: true });
  const manifests: Manifest[] = [];
  for (const entry of entries) {
    if (!entry.isDirectory()) continue;
    const path = join(PACKAGES, entry.name, "package.json");
    if (!(await Bun.file(path).exists())) continue;
    const body = (await Bun.file(path).json()) as Omit<Manifest, "path">;
    manifests.push({ ...body, path });
  }
  return manifests.sort((left, right) => left.name.localeCompare(right.name));
}

function list(manifests: Manifest[]): void {
  const width = Math.max(...manifests.map(manifest => manifest.name.length));
  for (const manifest of manifests) {
    const directory = manifest.path.slice(PACKAGES.length + 1).replace(/\/package\.json$/, "");
    console.log(`${manifest.name.padEnd(width)}  ${manifest.version.padEnd(10)}  packages/${directory}`);
  }
}

/**
 * Replaces an exact `"key": "value"` pair. A package's own version must appear once. One
 * dependency may appear in several sections, so a range replaces every occurrence.
 */
function replacePair(text: string, key: string, from: string, to: string, exactlyOne: boolean): string {
  const before = `${JSON.stringify(key)}: ${JSON.stringify(from)}`;
  const parts = text.split(before);
  const found = parts.length - 1;
  if (found === 0 || (exactlyOne && found > 1)) {
    throw new Error(`Expected ${exactlyOne ? "one" : "at least one"} ${before} in the manifest, found ${found}.`);
  }
  return parts.join(`${JSON.stringify(key)}: ${JSON.stringify(to)}`);
}

async function setVersion(manifests: Manifest[], version: string, targets: string[], dryRun: boolean): Promise<void> {
  const chosen = targets.length === 0
    ? manifests.filter(manifest => manifest.name.startsWith("@onlyne/"))
    : manifests.filter(manifest => targets.includes(manifest.name));
  const unknown = targets.filter(target => !manifests.some(manifest => manifest.name === target));
  if (unknown.length > 0) throw new Error(`No such package: ${unknown.join(", ")}`);
  if (chosen.length === 0) {
    throw new Error("No package matched. Name a package, or leave the list empty for every @onlyne/* package.");
  }

  const bumped = new Map(chosen.map(manifest => [manifest.name, version]));
  const lines: string[] = [];

  for (const manifest of manifests) {
    let text = await Bun.file(manifest.path).text();
    let touched = false;
    // One dependency can sit in several sections. The first replacement rewrites every
    // occurrence of the pair, so the remaining sections must not look for it again.
    const rewritten = new Set<string>();
    if (bumped.has(manifest.name)) {
      text = replacePair(text, "version", manifest.version, version, true);
      lines.push(`${manifest.name}: ${manifest.version} -> ${version}`);
      touched = true;
    }
    for (const section of SECTIONS) {
      for (const [name, range] of Object.entries(manifest[section] ?? {})) {
        const wanted = bumped.get(name);
        if (wanted === undefined || range === `^${wanted}` || rewritten.has(name)) continue;
        text = replacePair(text, name, range, `^${wanted}`, false);
        rewritten.add(name);
        lines.push(`${manifest.name} ${section}.${name}: ${range} -> ^${wanted}`);
        touched = true;
      }
    }
    if (touched && !dryRun) await Bun.write(manifest.path, text);
  }

  for (const line of lines) console.log(`${dryRun ? "[dry-run] " : ""}${line}`);
  if (lines.length === 0) console.log("Already at that version; nothing to write.");
  else if (!dryRun) console.log(`\nRun bun install to refresh the lockfile.`);
}

const [version, ...rest] = process.argv.slice(2);
const dryRun = rest.includes("--dry-run");
const targets = rest.filter(argument => argument !== "--dry-run");
const manifests = await readManifests();

if (version === undefined || version === "list") list(manifests);
else {
  if (!SEMVER.test(version)) throw new Error(`Expected a version like 3.1.0, got ${JSON.stringify(version)}`);
  await setVersion(manifests, version, targets, dryRun);
}
