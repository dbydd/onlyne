// The relay guard's policy: `<plugin package dir>/relay.toml`.
//
// The guard exists because a session narrated work in progress and then
// reported `done` with its todos untouched, leaving the downstream writer
// waiting on a handoff that never happened. The policy below is the minimum a
// session owes downstream before `onlyne_complete` may end it, expressed in
// delivery facts only: which roles this session handed something to, never
// what the text said (that is the critic layer's business, not the adapter's).
//
// The file sits next to `package.json`, so the policy travels with the plugin
// copy a generated workspace carries: `onlyne server generate` copies the
// package to `<ws>/.onlyne/agent/<pkg-name>/` and `.pi/settings.json` loads
// that copy (`crates/onlyne-server/src/generate.rs`). It is deliberately not
// `<ws>/.onlyne/config.toml`: the client parses that file as `ClientConfig`,
// which is `#[serde(deny_unknown_fields)]` and `additionalProperties: false`
// (`crates/onlyne-config/src/client.rs`, `schema/config-client.schema.json`),
// so a plugin-owned key there would make the client refuse to start. A `[local]`
// fragment merged into it has the same problem.
//
// The accepted body is a closed subset of TOML — flat `key = value` lines, the
// two keys below, one-line arrays of double-quoted strings — because this
// package parses its own files by hand and the runtime has no npm dependencies.
// Anything outside the subset is reported on stderr and ignored, the same
// degrade-don't-disable way `.pi/onlyne.json` behaves. A missing file is the
// default, which is "no guard": absent policy means the plugin behaves exactly
// as it did before this module existed.
//
//   relay_required = ["writer"]      # these roles must have received a handoff
//   relay_required_count = 2         # ... or this many distinct downstream roles
//
// `relay_required` wins when both are present.
//
// The file is the manual installation's escape hatch. A generated workspace
// carries the same policy in its spec, and the client injects it into every
// session process it spawns, so the environment comes first:
//
//   ONLYNE_RELAY_REQUIRED=writer,auditor   # the spec's `relay_required`
//   ONLYNE_RELAY_COUNT=2                   # the spec's `relay_count`
//
// A variable that is set and unparsable is reported on stderr and ignored, and
// with nothing usable in the environment the file is read as before.

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

/** File name, resolved next to the plugin's `package.json`. */
export const RELAY_FILE = "relay.toml";

/** Fixed marker a waived completion's ledger head starts with. */
export const FORCED_PREFIX = "relay-guard-forced: ";

/** No policy: an empty list and no count, both frozen together. */
export const DEFAULT_RELAY = Object.freeze({ required: Object.freeze([]), count: null });

/** The client's injected policy variables, filled from the spec's entry. */
export const RELAY_ENV_REQUIRED = "ONLYNE_RELAY_REQUIRED";
export const RELAY_ENV_COUNT = "ONLYNE_RELAY_COUNT";

/**
 * The policy file this module reads by default: beside `package.json`, the way
 * `protocol.mjs` reads the plugin version.
 */
export function relayPath() {
  return join(dirname(fileURLToPath(import.meta.url)), "..", RELAY_FILE);
}

/**
 * Whether a loaded policy guards anything: a non-empty list, or a positive
 * count. A malformed key leaves its own dimension off, so one bad line cannot
 * silently arm the guard with the wrong rule.
 *
 * @param {{ required?: string[], count?: number | null } | null | undefined} config
 */
export function relayEnabled(config) {
  if (!config) return false;
  if (Array.isArray(config.required) && config.required.length > 0) return true;
  return Number.isInteger(config.count) && config.count > 0;
}

/**
 * Parse one `relay.toml` body.
 *
 * @param {string} text
 * @param {string} [name] file name for diagnostics
 * @returns {{ required: string[], count: number | null, warning: string | null }}
 */
export function parseRelay(text, name = RELAY_FILE) {
  const config = { required: [], count: null };
  const warnings = [];
  const warn = (line, detail) => warnings.push(`${name}:${line}: ${detail}`);

  String(text)
    .split(/\r?\n/)
    .forEach((raw, index) => {
      const line = index + 1;
      const body = stripComment(raw).trim();
      if (!body) return;
      const assignment = /^([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(\S.*)$/.exec(body);
      if (!assignment) {
        warn(line, `not a \`key = value\` line (${JSON.stringify(raw.trim())}); ignored`);
        return;
      }
      const [, key, value] = assignment;
      if (key === "relay_required") {
        const list = parseStringArray(value);
        if (list === null) {
          warn(line, 'relay_required must be one line of double-quoted names, e.g. ["writer"]; ignored');
          return;
        }
        config.required = list;
        return;
      }
      if (key === "relay_required_count") {
        const count = /^[0-9]+$/.test(value) ? Number(value) : 0;
        if (count < 1) {
          warn(line, "relay_required_count must be a positive integer; ignored");
          return;
        }
        config.count = count;
        return;
      }
      warn(line, `unknown key ${JSON.stringify(key)}; ignored`);
    });

  return { ...config, warning: warnings.length > 0 ? warnings.join("; ") : null };
}

/**
 * The policy the client injected from the spec, when it injected one.
 *
 * The list is one comma-joined variable, in the order the spec wrote it; blank
 * entries are dropped, so a stray comma is not a role name. A variable that is
 * set but unparsable is reported and ignored rather than adopted, which keeps a
 * typo from arming the guard with a rule nobody wrote — and `specified` then
 * says the environment supplied nothing, so the file still gets its turn.
 *
 * @param {Record<string, string | undefined>} [env]
 * @returns {{ required: string[], count: number | null, specified: boolean, warning: string | null }}
 */
export function envRelay(env = process.env) {
  const warnings = [];
  let required = null;
  let count = null;

  const rawRequired = env[RELAY_ENV_REQUIRED];
  if (rawRequired !== undefined) {
    const names = String(rawRequired)
      .split(",")
      .map((name) => name.trim())
      .filter(Boolean);
    if (names.length > 0) required = names;
    else warnings.push(`${RELAY_ENV_REQUIRED}: no role names in ${JSON.stringify(rawRequired)}; ignored`);
  }

  const rawCount = env[RELAY_ENV_COUNT];
  if (rawCount !== undefined) {
    const text = String(rawCount).trim();
    const parsed = /^[0-9]+$/.test(text) ? Number(text) : 0;
    if (parsed > 0) count = parsed;
    else warnings.push(`${RELAY_ENV_COUNT} must be a positive integer, got ${JSON.stringify(rawCount)}; ignored`);
  }

  return {
    required: required ?? [],
    count,
    specified: required !== null || count !== null,
    warning: warnings.length > 0 ? warnings.join("; ") : null,
  };
}

/**
 * Read the policy: what the client injected from the spec first, then the file
 * beside `package.json`.
 *
 * `source` names the winner, and `present` answers the narrower question the
 * file itself raises: the environment winning means the file was never read, so
 * a stale `relay.toml` cannot outlive the spec entry that replaced it.
 *
 * @param {{ readFile?: (path: string) => string, path?: string, env?: Record<string, string | undefined> }} [options]
 * @returns {{ required: string[], count: number | null, path: string, present: boolean, source: "env" | "file" | "none", warning: string | null }}
 */
export function loadRelay(options = {}) {
  const readFile = options.readFile ?? ((path) => readFileSync(path, "utf8"));
  const path = options.path ?? relayPath();
  const injected = envRelay(options.env ?? process.env);
  if (injected.specified) {
    return {
      required: injected.required,
      count: injected.count,
      path,
      present: false,
      source: "env",
      warning: injected.warning,
    };
  }
  let raw;
  try {
    raw = readFile(path);
  } catch {
    return {
      ...DEFAULT_RELAY,
      path,
      present: false,
      source: "none",
      warning: injected.warning,
    };
  }
  const parsed = parseRelay(raw, path);
  return {
    required: parsed.required,
    count: parsed.count,
    path,
    present: true,
    source: "file",
    warning: [injected.warning, parsed.warning].filter(Boolean).join("; ") || null,
  };
}

/**
 * The verdict for one `onlyne_complete`, as a refusal message or `null`.
 *
 * `delivered` is the set of roles this session's own successful `onlyne_send`
 * calls reached. List mode is literal: every named role must be in it. Count
 * mode counts distinct downstream roles, so a send to this role itself and a
 * send back to the role that assigned the task (the upstream) do not count —
 * neither of them hands work further down the cluster.
 *
 * @param {{ required?: string[], count?: number | null } | null} config
 * @param {Iterable<string>} delivered
 * @param {{ role?: string | null, upstream?: string | null }} [context]
 * @returns {string | null}
 */
export function relayRefusal(config, delivered, context = {}) {
  if (!relayEnabled(config)) return null;
  const sent = new Set([...delivered].map((name) => String(name)));

  if (Array.isArray(config.required) && config.required.length > 0) {
    const missing = config.required.filter((name) => !sent.has(name));
    if (missing.length === 0) return null;
    return refusalText(
      `missing handoff to: ${missing.join(", ")}`,
      `this session delivered to: ${listOf([...sent])}`,
    );
  }

  const { role = null, upstream = null } = context;
  const downstream = [...sent].filter((name) => name !== role && name !== upstream);
  if (downstream.length >= config.count) return null;
  return refusalText(
    `missing handoff: ${config.count - downstream.length} of ${config.count} required distinct downstream roles`,
    `delivered downstream: ${listOf(downstream)}`,
  );
}

/** The one refusal sentence: what is missing, what exists, and the way out. */
function refusalText(shortfall, evidence) {
  return (
    `relay guard: ${shortfall} (${evidence}); ` +
    "send the missing edge with onlyne_send, then call onlyne_complete again — or call it with " +
    'force:true and a non-empty reason to waive the guard and stamp the ledger head with ' +
    `"${FORCED_PREFIX}<reason>"`
  );
}

/** A comma-joined list, or `none` when there is nothing to name. */
function listOf(names) {
  return names.length > 0 ? names.join(", ") : "none";
}

/** Everything before an unquoted `#`; the subset has no multi-line strings. */
function stripComment(line) {
  let quoted = false;
  for (let index = 0; index < line.length; index += 1) {
    const char = line[index];
    if (char === '"' && line[index - 1] !== "\\") quoted = !quoted;
    else if (char === "#" && !quoted) return line.slice(0, index);
  }
  return line;
}

/** One line of double-quoted strings, or `null` for anything else. */
function parseStringArray(body) {
  const trimmed = body.trim();
  if (!trimmed.startsWith("[") || !trimmed.endsWith("]")) return null;
  const items = [];
  let rest = trimmed.slice(1, -1).trim();
  if (rest === "") return items;
  for (;;) {
    const item = /^"((?:[^"\\]|\\.)*)"\s*/.exec(rest);
    if (!item) return null;
    items.push(item[1].replace(/\\(.)/g, "$1"));
    rest = rest.slice(item[0].length).trimStart();
    if (rest === "") return items;
    if (!rest.startsWith(",")) return null;
    rest = rest.slice(1).trimStart();
    if (rest === "") return null; // a trailing comma is not TOML
  }
}
