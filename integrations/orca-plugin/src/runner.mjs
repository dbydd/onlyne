// Injectable subprocess runner for the onlyne-sessions Orca plugin worker.
//
// The plugin worker is a plain Node process (ELECTRON_RUN_AS_NODE) whose
// environment is scrubbed down to an allowlist — PATH and HOME survive, so
// `orca` / `onlyne` resolve normally, but nothing else is inherited. Every CLI
// call goes through this module so tests can inject a fake `exec` and so the
// "CLI failed with exit 1, empty stderr, JSON error body on stdout" shape the
// Orca CLI actually uses is parsed in exactly one place.

import { execFile } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

export const DEFAULT_TIMEOUT_MS = 8000;
export const MAX_BUFFER_BYTES = 4 * 1024 * 1024;

/** Optional operator override; the worker env is scrubbed, so this is a file. */
export const CONFIG_RELATIVE_PATH = ".config/onlyne-sessions/config.json";

function defaultExec(binary, args, options) {
  return new Promise((resolve, reject) => {
    execFile(binary, args, options, (error, stdout, stderr) => {
      if (error) {
        error.stdout = stdout;
        error.stderr = stderr;
        reject(error);
        return;
      }
      resolve({ stdout, stderr });
    });
  });
}

function asText(value) {
  if (value === null || value === undefined) return "";
  return Buffer.isBuffer(value) ? value.toString("utf8") : String(value);
}

/**
 * Parse a CLI payload. Unix-CLI failures on this box exit non-zero with an
 * empty stderr and `{"ok":false,"error":{"code":…,"message":…}}` on stdout, so
 * the body — not the exit code — carries the diagnosis.
 */
export function parseCliJson(raw) {
  const text = asText(raw).trim();
  if (!text) return { ok: false, code: "empty_output", message: "command printed nothing" };
  let value;
  try {
    value = JSON.parse(text);
  } catch {
    return { ok: false, code: "bad_json", message: `command did not print JSON: ${clip(text, 240)}` };
  }
  if (value && typeof value === "object" && value.ok === false) {
    return {
      ok: false,
      code: typeof value.error?.code === "string" ? value.error.code : "cli_error",
      message:
        typeof value.error?.message === "string" && value.error.message
          ? value.error.message
          : "command reported failure",
      value,
    };
  }
  return { ok: true, value };
}

/** Orca answers are `{id, ok, result}`; bare payloads are passed through. */
export function unwrapResult(value) {
  if (value && typeof value === "object" && value.result !== undefined) return value.result;
  return value;
}

export function classifyExecError(error) {
  const code = error?.code;
  if (code === "ENOENT") return { code: "missing_binary", message: "binary not found on PATH" };
  if (code === "ETIMEDOUT" || error?.killed === true || error?.signal) {
    return { code: "timeout", message: "command timed out" };
  }
  const onStdout = parseCliJson(error?.stdout);
  if (!onStdout.ok && onStdout.code !== "empty_output" && onStdout.code !== "bad_json") {
    return { code: onStdout.code, message: onStdout.message };
  }
  const stderr = asText(error?.stderr).trim();
  const message = stderr || onStdout.message || `command failed (${code ?? "unknown"})`;
  return { code: "cli_error", message: clip(message, 240) };
}

function clip(text, limit) {
  const value = String(text ?? "");
  return value.length <= limit ? value : `${value.slice(0, limit)}…`;
}

/**
 * @param {object} [options]
 * @param {Function} [options.exec] `(binary, args, options) => Promise<{stdout, stderr}>`
 */
export function createRunner({
  exec = defaultExec,
  timeoutMs = DEFAULT_TIMEOUT_MS,
  env = null,
  cwd = null,
} = {}) {
  async function run(binary, args, { cwd: callCwd = cwd, timeout = timeoutMs } = {}) {
    try {
      const result = await exec(binary, args, {
        cwd: callCwd,
        timeout,
        env,
        encoding: "utf8",
        maxBuffer: MAX_BUFFER_BYTES,
      });
      return {
        ok: true,
        stdout: asText(result?.stdout),
        stderr: asText(result?.stderr),
        exitCode: 0,
      };
    } catch (error) {
      const failure = classifyExecError(error);
      return {
        ok: false,
        code: failure.code,
        message: failure.message,
        stdout: asText(error?.stdout),
        stderr: asText(error?.stderr),
        exitCode: typeof error?.code === "number" ? error.code : null,
      };
    }
  }

  /** Never throws for CLI-level failure: callers degrade, they do not crash. */
  async function runJson(binary, args, options = {}) {
    const result = await run(binary, args, options);
    if (!result.ok) {
      return {
        ok: false,
        code: result.code,
        message: result.message,
        stdout: result.stdout,
      };
    }
    const parsed = parseCliJson(result.stdout);
    if (!parsed.ok) {
      return { ok: false, code: parsed.code, message: parsed.message, stdout: result.stdout };
    }
    return { ok: true, value: unwrapResult(parsed.value), raw: parsed.value, stdout: result.stdout };
  }

  return { run, runJson };
}

/** Resolve the first existing candidate, else the bare name (execFile decides). */
export function resolveBinary(
  name,
  { home = homedir(), exists = existsSync, candidates = [] } = {}
) {
  for (const candidate of candidates) {
    if (exists(candidate)) return candidate;
  }
  const homeCandidates = [`${home}/.local/bin/${name}`, `${home}/bin/${name}`];
  for (const candidate of homeCandidates) {
    if (exists(candidate)) return candidate;
  }
  return name;
}

export function defaultOrcaCandidates() {
  return ["/opt/homebrew/bin/orca", "/usr/local/bin/orca", "/usr/bin/orca"];
}

export function defaultOnlyneCandidates() {
  return [
    "/opt/homebrew/bin/onlyne",
    "/usr/local/bin/onlyne",
    "/usr/bin/onlyne",
  ];
}

/**
 * Read the optional operator config. Absent or malformed is a normal state:
 * the config pins binary paths and names the server roots, and without it the
 * board still runs on PATH discovery and the tab axis alone.
 */
export function readPluginConfig({
  home = homedir(),
  exists = existsSync,
  readFile = readFileSync,
} = {}) {
  const path = join(home, CONFIG_RELATIVE_PATH);
  if (!exists(path)) return { loaded: false, path, config: {} };
  try {
    const parsed = JSON.parse(readFile(path, "utf8"));
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
      return { loaded: false, path, config: {}, error: "config must be a JSON object" };
    }
    return { loaded: true, path, config: parsed };
  } catch (error) {
    return { loaded: false, path, config: {}, error: String(error?.message ?? error) };
  }
}

/** Trim, drop blanks and dedupe a config path list; anything else is empty. */
export function normalizePathList(value) {
  if (!Array.isArray(value)) return [];
  const paths = [];
  for (const entry of value) {
    if (typeof entry !== "string") continue;
    const path = entry.trim();
    if (!path || paths.includes(path)) continue;
    paths.push(path);
  }
  return paths;
}

/**
 * `piWorkspaces` names the onlyne workspaces whose pi adapters may publish a
 * pane claim (`<workspace>/.onlyne/cache/pi-pane.json`, §2 axis A). It is a
 * separate list from `serverRoots` because the two are different directories:
 * a workspace is where the operator runs `onlyne client run` (and where Orca
 * hosts the pane), and it is the client — not the server root — that owns that
 * path. Absent or empty is a normal state: the board then scopes its tab axis
 * by the worktree heuristic alone.
 */
export const normalizePiWorkspaces = normalizePathList;

/** `serverRoots` names the onlyne server roots the board mirrors (one admin
 * socket `<root>/.onlyne/run/s` each). Absent, empty or malformed is a normal
 * state: the board then has no session axis and renders flat Orca tabs only. */
export const normalizeServerRoots = normalizePathList;

export function resolveBinaries({
  home = homedir(),
  exists = existsSync,
  readFile = readFileSync,
  env = process.env,
} = {}) {
  const { loaded, path, config, error } = readPluginConfig({ home, exists, readFile });
  const configured = (value) => (typeof value === "string" && value.trim() ? value.trim() : null);
  // `BIN_DIR` is how the repository's e2e cases pin the freshly built binaries
  // (`crates/onlyne-testkit/e2e/lib.sh`). It only wins when the file is really
  // there, so a half-set variable can never hide an installed CLI, and an
  // operator config still outranks it.
  const binDir = configured(env?.BIN_DIR);
  const inBinDir = (name) => {
    if (!binDir) return null;
    const candidate = join(binDir, name);
    return exists(candidate) ? candidate : null;
  };
  const orcaBin =
    configured(config.orcaBin) ??
    (env?.ORCA_BIN && exists(env.ORCA_BIN) ? env.ORCA_BIN : null) ??
    inBinDir("orca") ??
    resolveBinary("orca", { home, exists, candidates: defaultOrcaCandidates() });
  const onlyneBin =
    configured(config.onlyneBin) ??
    inBinDir("onlyne") ??
    resolveBinary("onlyne", { home, exists, candidates: defaultOnlyneCandidates() });
  return {
    orcaBin,
    onlyneBin,
    binDir,
    serverRoots: normalizeServerRoots(config.serverRoots),
    piWorkspaces: normalizePiWorkspaces(config.piWorkspaces),
    configPath: path,
    configLoaded: loaded,
    configError: error ?? null,
  };
}
