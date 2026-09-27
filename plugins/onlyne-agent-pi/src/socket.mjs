// The adapter socket a pi session dials, resolved the v2 way.
//
// v2 keeps every local socket in one machine-level runtime directory —
// `$ONLYNE_RUNTIME_DIR` when the operator sets it and non-empty, `/tmp/onlyne-<uid>/`
// otherwise, `0700` — as `<digest>.sock`, `digest` the first 16 hex characters
// of `sha256` over the canonical workspace root. Nothing binds inside the tree
// any more: `<root>/.onlyne/run/s` and the `run/socket` marker v1 published the
// served path in are both deleted, and `run/s` survives only as the spelling
// operators print (`crates/onlyne-wire/src/socket.rs`).
//
// The client injects `ONLYNE_SOCKET` into every session process it spawns, so a
// pi started by a client needs nothing else and this module's first answer —
// that variable, verbatim — is what such a session uses. A pi started by hand
// (`pi -e <plugin>`) carries no such variable, and there is no path inside the
// tree to fall back on: it has to find the client the way the plan says an
// external runtime does (`docs/v2-PLAN.md` line 289), by reading the
// registration files in the runtime directory — `<digest>.json`, one per live
// endpoint, holding `kind`, `role`, `root`, `pid`, `version` and `runtime`.
//
// So a hand-started pi reads those files and the order of its answers is:
//
//  1. the registration whose `root` is the workspace this process runs in — the
//     exact tree, or the nearest registered root containing it, which is the
//     tree the CLI's own upward walk lands on (`owner_root` in
//     `crates/onlyne-cli/src/socket.rs`). No arbitrary pick can arise on this
//     key: the roots containing one directory are a chain, so "nearest" is
//     stated by the paths themselves, and two registrations naming one tree
//     name one socket, because the socket is its digest.
//  2. only when no registered root covers the working directory, the `runtime`
//     field, which is the plan's discovery key for the external placement where
//     one runtime serves several clients. Secondary in both senses: consulted
//     last, and only a single matching client is an answer — several clients
//     running pi sessions are refused by name rather than picked from, because
//     a wrong pick dials a client that serves another role's sessions. (The
//     field carries the session backend a client spawns under, `herdr`/`orca`/
//     `acp`/`exec`, so today it names this plugin's runtime only where that
//     backend is pi itself.)
//  3. nothing at all: `SocketResolutionError` naming the workspace, the
//     runtime directory and the registration file — and every registration the
//     directory does hold — so the operator reads what was missing rather than
//     a raw `ENOENT` for a path v2 never creates.
//
// The derivation below has to agree byte for byte with `workspace_digest` in
// `crates/onlyne-wire/src/socket.rs` and `workspace_digest` in
// `crates/onlyne-testkit/e2e/lib.sh`; a digest one byte off dials a path no
// daemon holds.

import { createHash } from "node:crypto";
import { readFileSync, readdirSync, realpathSync } from "node:fs";
import { tmpdir } from "node:os";
import { isAbsolute, join, resolve, sep } from "node:path";

/** Environment variable holding the path a client injected, used verbatim. */
export const SOCKET_ENV = "ONLYNE_SOCKET";

/** Environment variable that replaces the default runtime directory; `RUNTIME_DIR_ENV` in `crates/onlyne-wire/src/socket.rs`. */
export const RUNTIME_DIR_ENV = "ONLYNE_RUNTIME_DIR";

/** Socket leaf inside a runtime directory: `<digest>.sock`. */
export const SOCKET_SUFFIX = ".sock";

/** Registration leaf inside a runtime directory: `<digest>.json`. */
export const REGISTRATION_SUFFIX = ".json";

/** The runtime this plugin is, the value it matches a client's `runtime` field against. */
export const RUNTIME_NAME = "pi";

/** How many hex characters of the digest name a runtime file; `workspace_digest` hashes 8 bytes. */
const DIGEST_LENGTH = 16;

/** Raised when no client registration answers for a workspace, or several do. */
export class SocketResolutionError extends Error {
  /**
   * @param {string} message what was looked for and what was found
   * @param {{ path: string, root: string, kind: string | null, runtime: string | null }[]} candidates
   */
  constructor(message, candidates = []) {
    super(message);
    this.name = "SocketResolutionError";
    this.candidates = candidates;
  }
}

/**
 * The machine-level runtime directory: the override when set and non-empty,
 * `/tmp/onlyne-<uid>` otherwise.
 *
 * Nothing is created or checked here, and the override is taken without
 * trimming, both matching `runtime_dir_path` (an empty value is no override, a
 * value holding only spaces is a directory name). The `/tmp` base is fixed
 * rather than `os.tmpdir()` because a launchd-started daemon and an interactive
 * shell see different `TMPDIR` values, and one base is what makes both compute
 * the same path for the same root.
 *
 * @param {Record<string, string | undefined>} env the process environment
 * @param {{ uid?: number | null }} [options]
 * @returns {string} an absolute directory path
 */
export function runtimeDir(env, options = {}) {
  const override = typeof env[RUNTIME_DIR_ENV] === "string" ? env[RUNTIME_DIR_ENV] : "";
  if (override !== "") return override;
  const uid = options.uid ?? (typeof process.getuid === "function" ? process.getuid() : null);
  // Off unix the reference has no uid to name: `%TEMP%` already scopes the
  // directory per user and the endpoint's own owner-only descriptor guards it.
  return uid === null ? join(tmpdir(), "onlyne-user") : join("/tmp", `onlyne-${uid}`);
}

/**
 * The canonical spelling of `root`, the one `absolute_path` takes.
 *
 * A root that exists is resolved through its symlinks, which is what keeps
 * macOS's `/var` and `/private/var` one digest. A root that does not exist has
 * nothing to canonicalize and falls back to the lexical absolute.
 *
 * @param {string} root
 * @param {{ realpath?: (path: string) => string }} [options]
 * @returns {string}
 */
export function canonicalRoot(root, options = {}) {
  const realpath = options.realpath ?? ((path) => realpathSync(path));
  try {
    return realpath(root);
  } catch {
    return resolve(isAbsolute(root) ? root : join(process.cwd(), root));
  }
}

/**
 * The identity of one owner tree: `sha256` over its canonical spelling, the
 * first 16 lowercase hex characters.
 *
 * Separators become `/` and the whole spelling is lowercased before hashing, so
 * `C:\Work` and `c:/work` digest alike — the reason a daemon and every client
 * derive one file name without talking to each other.
 *
 * @param {string} root
 * @param {{ realpath?: (path: string) => string }} [options]
 * @returns {string} 16 hex characters
 */
export function workspaceDigest(root, options = {}) {
  const spelling = canonicalRoot(root, options)
    .replaceAll("\\", "/")
    .replace(/[A-Z]/g, (character) => character.toLowerCase());
  return createHash("sha256").update(spelling, "utf8").digest("hex").slice(0, DIGEST_LENGTH);
}

/**
 * The socket one owner root is bound to: `<runtime_dir>/<digest>.sock`.
 *
 * @param {string} root the workspace root
 * @param {Record<string, string | undefined>} env
 * @param {{ readFile?: (path: string) => string, readDir?: (path: string) => string[], realpath?: (path: string) => string, uid?: number | null }} [options]
 * @returns {string}
 */
export function socketPath(root, env, options = {}) {
  return join(runtimeDir(env, options), `${workspaceDigest(root, options)}${SOCKET_SUFFIX}`);
}

/**
 * The registration belonging to that socket: `<runtime_dir>/<digest>.json`.
 *
 * @param {string} root the workspace root
 * @param {Record<string, string | undefined>} env
 * @param {{ readFile?: (path: string) => string, readDir?: (path: string) => string[], realpath?: (path: string) => string, uid?: number | null }} [options]
 * @returns {string}
 */
export function registrationPath(root, env, options = {}) {
  return join(runtimeDir(env, options), `${workspaceDigest(root, options)}${REGISTRATION_SUFFIX}`);
}

/**
 * Every client registration the runtime directory holds, in file-name order.
 *
 * A missing runtime directory is no registrations rather than an error: a
 * plugin started before any daemon should see an empty machine. A file that is
 * not `.json`, does not parse, or names no root is skipped, because one stray
 * file must not blind the reader to every other tree — the same rule
 * `list_registrations` follows in `crates/onlyne-wire/src/socket.rs`. A
 * registration of another kind (a server root's admin endpoint, say) is skipped
 * here as well: this plugin speaks the adapter protocol of a role workspace's
 * client, and an admin socket answers those frames with `unknown op`.
 *
 * @param {Record<string, string | undefined>} env
 * @param {{ readFile?: (path: string) => string, readDir?: (path: string) => string[], realpath?: (path: string) => string, uid?: number | null }} [options]
 * @returns {{ path: string, root: string, kind: string | null, runtime: string | null }[]}
 */
export function listClientRegistrations(env, options = {}) {
  const readDir = options.readDir ?? ((path) => readdirSync(path));
  const readFile = options.readFile ?? ((path) => readFileSync(path, "utf8"));
  const dir = runtimeDir(env, options);
  let names;
  try {
    names = readDir(dir);
  } catch {
    return [];
  }
  const found = [];
  for (const name of [...names].sort()) {
    if (typeof name !== "string" || !name.endsWith(REGISTRATION_SUFFIX)) continue;
    let parsed;
    try {
      parsed = JSON.parse(readFile(join(dir, name)));
    } catch {
      continue;
    }
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) continue;
    if (parsed.kind !== "client") continue;
    if (typeof parsed.root !== "string" || parsed.root === "") continue;
    found.push({
      path: join(dir, name),
      root: canonicalRoot(parsed.root, options),
      kind: parsed.kind,
      runtime: typeof parsed.runtime === "string" && parsed.runtime !== "" ? parsed.runtime : null,
    });
  }
  return found;
}

/**
 * The socket path to dial for the pi working directory `cwd`.
 *
 * @param {Record<string, string | undefined>} env the process environment
 * @param {string} cwd the pi working directory, inside the role workspace
 * @param {{ readFile?: (path: string) => string, readDir?: (path: string) => string[], realpath?: (path: string) => string, uid?: number | null, runtime?: string }} [options]
 * @returns {string} an absolute path
 * @throws {SocketResolutionError} when no client answers for the workspace, or several do
 */
export function resolveSocketPath(env, cwd, options = {}) {
  const injected = typeof env[SOCKET_ENV] === "string" ? env[SOCKET_ENV].trim() : "";
  if (injected) return injected;

  const dir = runtimeDir(env, options);
  const here = canonicalRoot(cwd, options);
  const candidates = listClientRegistrations(env, options);

  // The tree this process runs in answers first: the exact workspace, or the
  // nearest registered root containing it — the same tree `owner_root` walks up
  // to in `crates/onlyne-cli/src/socket.rs`, so a pi started in a subdirectory
  // reaches the client of the workspace it belongs to. Roots containing one
  // directory form a chain, so the longest one is this tree and never a coin
  // toss; a second registration of that same tree names the same socket.
  const covering = candidates.filter((entry) => covers(entry.root, here));
  if (covering.length > 0) {
    const nearest = covering.reduce((best, entry) => (entry.root.length > best.root.length ? entry : best));
    return join(dir, `${workspaceDigest(nearest.root, options)}${SOCKET_SUFFIX}`);
  }

  // No registered root contains the working directory. The `runtime` field is
  // the plan's discovery key for an external placement, where one runtime
  // serves several clients (`docs/v2-PLAN.md` line 289); it stays secondary, so
  // it is consulted only here, and only a single answer is an answer.
  const runtime = (options.runtime ?? RUNTIME_NAME).toLowerCase();
  const matching = candidates.filter((entry) => entry.runtime !== null && entry.runtime.toLowerCase() === runtime);
  if (matching.length === 1) {
    return join(dir, `${workspaceDigest(matching[0].root, options)}${SOCKET_SUFFIX}`);
  }
  const wanted = join(dir, `${workspaceDigest(here, options)}${REGISTRATION_SUFFIX}`);
  let seen;
  if (matching.length > 1) {
    seen =
      `and ${matching.length} clients there name runtime ${runtime} — ${describe(matching)} — ` +
      "so which one serves this session is ambiguous";
  } else if (candidates.length === 0) {
    seen = "and the directory holds no client registration";
  } else {
    seen = `and the ${candidates.length} client registration(s) there belong to other roots: ${describe(candidates)}`;
  }
  throw new SocketResolutionError(
    `onlyne: no client is registered for ${here}: looked for ${wanted} in ${dir}, ${seen}. ` +
      "Start that workspace's client, or name the socket with ONLYNE_SOCKET",
    matching.length > 0 ? matching : candidates,
  );
}

/** `true` when `root` is `here` or a directory containing it. */
function covers(root, here) {
  return root === here || here.startsWith(root.endsWith(sep) ? root : `${root}${sep}`);
}

/** `<path> (root <root>, runtime <runtime>)` per entry, for an error message. */
function describe(entries) {
  return entries
    .map((entry) => `${entry.path} (root ${entry.root}, runtime ${entry.runtime ?? "-"})`)
    .join(", ");
}
