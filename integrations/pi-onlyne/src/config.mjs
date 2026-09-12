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
import { join } from "node:path";

/** Where the switch file lives, relative to the pi working directory. */
export const CONFIG_RELATIVE_PATH = join(".pi", "onlyne.json");

/** Defaults: on, and connecting as soon as a session starts. */
export const DEFAULT_CONFIG = Object.freeze({ enabled: true, autoStart: true });

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
    return { ...DEFAULT_CONFIG, path, warning: null, present: false };
  }
  let parsed;
  try {
    parsed = JSON.parse(raw);
  } catch (error) {
    return {
      ...DEFAULT_CONFIG,
      path,
      warning: `${path} is not valid JSON (${error instanceof Error ? error.message : String(error)}); using defaults`,
      present: true,
    };
  }
  if (!parsed || typeof parsed !== "object") {
    return { ...DEFAULT_CONFIG, path, warning: `${path} must hold a JSON object; using defaults`, present: true };
  }
  const watch = parsed.watch && typeof parsed.watch === "object" ? parsed.watch : {};
  return {
    enabled: typeof parsed.enabled === "boolean" ? parsed.enabled : DEFAULT_CONFIG.enabled,
    autoStart: typeof watch.autoStart === "boolean" ? watch.autoStart : DEFAULT_CONFIG.autoStart,
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
