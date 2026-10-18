// `.pi/onlyne.json`, the plugin's own switch file.
//
// The workspace layout keeps the old key shape (`watch.autoStart`) because that
// is what the generated role templates and operator habits carry; the client
// does not read this file at all (§11 line 399 downgraded the readiness gates to
// generate-time template advice), so the only consumer is this extension. A
// malformed or missing file falls back to the defaults and reports a warning
// instead of disabling the session: the extension's own `enabled` key is the one
// deliberate off switch.

import { readFileSync } from "node:fs";
import { dirname, isAbsolute, join, resolve } from "node:path";

/** Where the switch file lives, relative to the pi working directory. */
export const CONFIG_RELATIVE_PATH = join(".pi", "onlyne.json");

/** Where the workspace's own client configuration lives. */
export const WORKSPACE_CONFIG_RELATIVE_PATH = join(".onlyne", "config.toml");

/** Defaults: on, and connecting as soon as a session starts. */
export const DEFAULT_CONFIG = Object.freeze({
  enabled: true,
  autoStart: true,
});

/**
 * Read `.pi/onlyne.json`.
 *
 * @param {string} cwd
 * @param {{ readFile?: (path: string) => string }} [options]
 * @returns {{ enabled: boolean, autoStart: boolean, path: string, warning: string | null, present: boolean }}
 */
export function loadConfig(cwd, options = {}) {
  const readFile = options.readFile ?? ((path) => readFileSync(path, "utf8"));
  const path = join(cwd, CONFIG_RELATIVE_PATH);
  let raw;
  try {
    raw = readFile(path);
  } catch {
    return {
      ...DEFAULT_CONFIG,
      role: null,
      sessionId: null,
      workspace: null,
      resident: false,
      path,
      warning: null,
      present: false,
    };
  }
  let parsed;
  try {
    parsed = JSON.parse(raw);
  } catch (error) {
    return {
      ...DEFAULT_CONFIG,
      role: null,
      sessionId: null,
      workspace: null,
      resident: false,
      path,
      warning: `${path} is not valid JSON (${error instanceof Error ? error.message : String(error)}); using defaults`,
      present: true,
    };
  }
  if (!parsed || typeof parsed !== "object") {
    return {
      ...DEFAULT_CONFIG,
      role: null,
      sessionId: null,
      workspace: null,
      resident: false,
      path,
      warning: `${path} must hold a JSON object; using defaults`,
      present: true,
    };
  }
  const watch = parsed.watch && typeof parsed.watch === "object" ? parsed.watch : {};
  const rawSessionId = parsed.sessionId ?? parsed.session_id ?? parsed.session_alias;
  return {
    enabled: typeof parsed.enabled === "boolean" ? parsed.enabled : DEFAULT_CONFIG.enabled,
    autoStart: typeof watch.autoStart === "boolean" ? watch.autoStart : DEFAULT_CONFIG.autoStart,
    role: typeof parsed.role === "string" && parsed.role.trim() !== "" ? parsed.role.trim() : null,
    sessionId: typeof rawSessionId === "string" && rawSessionId.trim() !== "" ? rawSessionId.trim() : null,
    workspace: typeof parsed.workspace === "string" && parsed.workspace.trim() !== "" ? parsed.workspace.trim() : null,
    resident: typeof parsed.resident === "boolean" ? parsed.resident : false,
    path,
    warning: null,
    present: true,
  };
}

/**
 * The session identity the client injects into a spawned agent process
 * (`crates/onlyne-client/src/dispatch.rs`). All three are required: with any one
 * missing this is a plain pi session and the extension stays out of the way.
 *
 * @param {Record<string, string | undefined>} env
 * @returns {{ role: string, sessionId: string, taskId: string } | null}
 */
export function sessionIdentity(env) {
  const role = env.ONLYNE_ROLE;
  const sessionId = env.ONLYNE_SESSION_ID;
  const taskId = env.ONLYNE_TASK_ID;
  if (!role || !sessionId || !taskId) return null;
  return { role, sessionId, taskId };
}

/**
 * Parse the subset of `<workspace>/.onlyne/config.toml` relevant to a
 * pre-started resident agent (`role`, `placement`, `[client.session]`).
 *
 * @param {string} raw
 * @returns {{ role: string | null, placement: string | null, scope: string | null, sessionId: string | null }}
 */
export function parseWorkspaceConfigToml(raw) {
  let section = "";
  let role = null;
  let placement = null;
  let scope = null;
  let sessionId = null;
  for (const rawLine of String(raw).split(/\r?\n/)) {
    const line = rawLine.replace(/#.*$/, "").trim();
    if (!line) continue;
    const secMatch = line.match(/^\[([^\]]+)\]$/);
    if (secMatch) {
      section = secMatch[1].trim();
      continue;
    }
    const kvMatch = line.match(/^([A-Za-z0-9_.-]+)\s*=\s*"([^"]*)"/);
    if (!kvMatch) continue;
    const [, key, val] = kvMatch;
    const trimmed = val.trim();
    if (!trimmed) continue;
    if (section === "" && key === "role") role = trimmed;
    else if (section === "" && key === "placement") placement = trimmed.toLowerCase();
    else if (section === "client.session" && key === "scope") scope = trimmed.toLowerCase();
    else if (section === "client.session" && (key === "session_id" || key === "session_alias")) {
      sessionId = sessionId ?? trimmed;
    }
  }
  return { role, placement, scope, sessionId };
}

/**
 * Discover a resident / pre-started session identity when the agent is launched
 * before `onlyne-client` (no `ONLYNE_TASK_ID` in `env`).
 *
 * Activates when any of the following holds:
 * 1. `ONLYNE_ROLE` is set in `env` (with optional `ONLYNE_SESSION_ID` / `ONLYNE_WORKSPACE`),
 * 2. `.pi/onlyne.json` sets `"role"`, `"sessionId"`, or `"resident": true`, or
 * 3. `<workspace>/.onlyne/config.toml` declares `placement = "external"`,
 *    `[client.session] scope = "role"`, or `[client.session] session_id` / `session_alias`.
 *
 * @param {Record<string, string | undefined>} env
 * @param {string} cwd
 * @param {{ readFile?: (path: string) => string }} [options]
 * @returns {{ role: string, sessionId: string | null, taskId: string | null, resident: true, workspaceRoot: string } | null}
 */
export function residentIdentity(env, cwd, options = {}) {
  const readFile = options.readFile ?? ((path) => readFileSync(path, "utf8"));
  const config = loadConfig(cwd, options);
  const envRole = typeof env.ONLYNE_ROLE === "string" && env.ONLYNE_ROLE.trim() !== "" ? env.ONLYNE_ROLE.trim() : null;
  const envSessionId =
    typeof (env.ONLYNE_SESSION_ID ?? env.ONLYNE_SESSION_ALIAS) === "string" &&
    (env.ONLYNE_SESSION_ID ?? env.ONLYNE_SESSION_ALIAS).trim() !== ""
      ? (env.ONLYNE_SESSION_ID ?? env.ONLYNE_SESSION_ALIAS).trim()
      : null;
  const envTaskId =
    typeof env.ONLYNE_TASK_ID === "string" && env.ONLYNE_TASK_ID.trim() !== "" ? env.ONLYNE_TASK_ID.trim() : null;
  const envWorkspace =
    typeof env.ONLYNE_WORKSPACE === "string" && env.ONLYNE_WORKSPACE.trim() !== ""
      ? env.ONLYNE_WORKSPACE.trim()
      : null;
  const envResident =
    typeof env.ONLYNE_RESIDENT === "string" &&
    ["1", "true", "yes", "on"].includes(env.ONLYNE_RESIDENT.trim().toLowerCase());

  const hintRole = envRole ?? config.role;
  const explicitWorkspace = envWorkspace ?? config.workspace;
  let workspaceRoot = explicitWorkspace
    ? isAbsolute(explicitWorkspace)
      ? explicitWorkspace
      : resolve(cwd, explicitWorkspace)
    : cwd;

  let toml = { role: null, placement: null, scope: null, sessionId: null };
  const candidateRoots = [];
  if (explicitWorkspace) {
    candidateRoots.push(workspaceRoot);
  } else {
    let cur = resolve(cwd);
    for (let depth = 0; depth < 4; depth++) {
      candidateRoots.push(cur);
      if (hintRole) candidateRoots.push(join(cur, hintRole));
      const parent = dirname(cur);
      if (parent === cur) break;
      cur = parent;
    }
  }
  for (const candidate of candidateRoots) {
    try {
      const raw = readFile(join(candidate, WORKSPACE_CONFIG_RELATIVE_PATH));
      const parsed = parseWorkspaceConfigToml(raw);
      if (parsed.role && (!hintRole || parsed.role === hintRole)) {
        toml = parsed;
        workspaceRoot = candidate;
        break;
      }
    } catch {
      /* try next candidate */
    }
  }
  if (!explicitWorkspace && !toml.role && hintRole && workspaceRoot === cwd) {
    // If started in a cluster root (`<root>/.onlyne/spec.toml` exists), the
    // role workspace will live at `<root>/<role>`.
    try {
      readFile(join(cwd, ".onlyne", "spec.toml"));
      workspaceRoot = join(cwd, hintRole);
    } catch {
      /* cwd itself is the workspace root */
    }
  }

  const role = envRole ?? config.role ?? toml.role;
  if (!role) return null;

  const optedIn = Boolean(
    envRole ||
      envResident ||
      config.resident ||
      config.role ||
      config.sessionId ||
      toml.placement === "external" ||
      toml.scope === "role" ||
      toml.sessionId,
  );
  if (!optedIn) return null;

  return {
    role,
    sessionId: envSessionId ?? config.sessionId ?? toml.sessionId ?? null,
    taskId: envTaskId,
    resident: true,
    workspaceRoot,
  };
}
