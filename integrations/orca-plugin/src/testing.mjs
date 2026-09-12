// Shared fixtures for the node:test suites (not a *.test.mjs file, so
// `node --test` never runs it as a suite). Every fixture is a plain object:
// the plugin reads no file of its own, so the suites need no temp directory.

import { normalizeRoleRow, normalizeSessionRow } from "./onlyne-cli.mjs";
import { normalizeTerminalRow } from "./orca-cli.mjs";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

/** Repository root, from this file's own location (src/ -> package -> root). */
const REPO_ROOT = fileURLToPath(new URL("../../..", import.meta.url));

/**
 * The text of a file as the repository has it committed, or null when this copy
 * is not a git checkout at all (a package vendored into `<ws>/.onlyne/agent/`).
 *
 * `panel.html` is committed as a placeholder and legitimately rewritten in the
 * working tree by a dev install — that is the whole point of the phase-3 design
 * (`src/panel-document.mjs`) — so the working copy cannot answer the question
 * "what does a content-addressed install keep?". Git can.
 */
export function committedText(relativePath, { exec = execFileSync } = {}) {
  try {
    return exec("git", ["show", `HEAD:${relativePath}`], {
      cwd: REPO_ROOT,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    });
  } catch {
    return null;
  }
}

const TAB_ID = "45e603f7-0772-48aa-bcf6-832272747713";
const LEAF_ID = "b6d067b6-9255-4f5c-a13f-24f194ea0560";

/**
 * The pane key Orca reports for the default `tabRow()` — `<tabId>:<leafId>`.
 * It is the same spelling a session reports over the protocol, which is what
 * makes the two sides comparable as a plain set intersection.
 */
export const PANE_KEY = `${TAB_ID}:${LEAF_ID}`;

/**
 * One `orca terminal list` row as Orca 1.4.198 prints it. `worktreePath` is
 * carried as information only: the tab axis is scoped by the pane a session
 * reports, never by a worktree.
 */
export function tabRow(overrides = {}) {
  return {
    handle: "term_11111111-1111-4111-8111-111111111111",
    tabId: TAB_ID,
    leafId: LEAF_ID,
    title: "onlyne:task-alpha",
    connected: true,
    writable: true,
    lastOutputAt: 1_789_000_000_000,
    worktreeId: "2ea2fe23-829c-4a8f-bcac-4129eb78a164",
    worktreePath: "/repo/other-swarm",
    ...overrides,
  };
}

/**
 * One `sessions` answer row (the shape crates/onlyne-proto defines).
 *
 * `paneKey` is the Orca pane the session's own process reports over the
 * protocol (`projection.observed.host.orca.pane_key`). It defaults to the
 * default `tabRow()`'s pane, so a fixture that does not care about scoping
 * keeps its tab on the board's tab axis; pass `paneKey: null` for a session
 * that reported none, or another key for one that runs somewhere else.
 */
export function sessionRow({ paneKey = PANE_KEY, lifecycle = "working", agent = "running", ...overrides } = {}) {
  const projection = {
    lifecycle,
    agent,
    delivery: "accepted",
    resource: "attached",
    ...(paneKey ? { observed: { host: { orca: { pane_key: paneKey } } } } : {}),
  };
  return {
    task_id: "task-alpha",
    role: "planner",
    session_id: "sess-alpha",
    public_lifecycle: lifecycle,
    projection,
    updated_at: "2026-09-11T00:01:00Z",
    ...overrides,
  };
}

/** One `roles` answer row, as `res_role_info.json` carries it. */
export function roleRow(overrides = {}) {
  return {
    name: "planner",
    admin: false,
    max_sessions: 3,
    spec_hash: "abc123",
    state: "online",
    sessions: 1,
    ...overrides,
  };
}

/** Fake `orca` CLI with the call surface of src/orca-cli.mjs. */
export function fakeOrca({ tabs = [], tabFailure = null, switchFailure = null } = {}) {
  const calls = { listTerminals: 0, switchTerminal: [] };
  return {
    calls,
    async listTerminals() {
      calls.listTerminals += 1;
      if (tabFailure) return tabFailure;
      return { ok: true, rows: tabs.map((row) => normalizeTerminalRow(row)).filter(Boolean) };
    },
    async switchTerminal(handle) {
      calls.switchTerminal.push(handle);
      if (switchFailure) return switchFailure;
      return { ok: true, value: { switched: true } };
    },
  };
}

/**
 * Fake onlyne admin surface, with the call surface of src/onlyne-cli.mjs.
 *
 * `sessions` / `roles` map a server root to raw rows; a root with no entry
 * answers zero rows. `failures` maps either a root (both verbs) or
 * `"<verb> <root>"` (one verb) to a normalized failure object, which is how an
 * unreachable root and a half-broken one are both expressible.
 */
export function fakeOnlyne({ sessions = {}, roles = {}, failures = {} } = {}) {
  const calls = [];
  const answer = (verb, root) => {
    calls.push(`${verb} ${root}`);
    const failure = failures[`${verb} ${root}`] ?? failures[root];
    if (failure) return failure;
    const rows = (verb === "sessions" ? sessions : roles)[root] ?? [];
    return verb === "sessions"
      ? { ok: true, sessions: rows.map(normalizeSessionRow).filter(Boolean) }
      : { ok: true, roles: rows.map(normalizeRoleRow).filter(Boolean) };
  };
  return {
    calls,
    async querySessions(root) {
      return answer("sessions", root);
    },
    async queryRoles(root) {
      return answer("roles", root);
    },
  };
}

export function collectNotifications() {
  const sent = [];
  const notify = async (title, body) => {
    sent.push({ title, body });
    return { delivered: true };
  };
  return { sent, notify };
}
