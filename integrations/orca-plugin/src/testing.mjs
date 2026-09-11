// Shared fixtures for the node:test suites (not a *.test.mjs file, so
// `node --test` never runs it as a suite). Every fixture is a plain object:
// the plugin reads no file of its own, so the suites need no temp directory.

import { normalizeRoleRow, normalizeSessionRow } from "./onlyne-cli.mjs";
import { normalizeTerminalRow } from "./orca-cli.mjs";

/** One `orca terminal list` row as Orca 1.4.198 prints it. */
export function tabRow(overrides = {}) {
  return {
    handle: "term_11111111-1111-4111-8111-111111111111",
    tabId: "45e603f7-0772-48aa-bcf6-832272747713",
    leafId: "b6d067b6-9255-4f5c-a13f-24f194ea0560",
    title: "onlyne:task-alpha",
    connected: true,
    writable: true,
    lastOutputAt: 1_789_000_000_000,
    worktreeId: "2ea2fe23-829c-4a8f-bcac-4129eb78a164",
    ...overrides,
  };
}

/** One `sessions` answer row (the shape crates/onlyne-proto defines). */
export function sessionRow(overrides = {}) {
  return {
    task_id: "task-alpha",
    role: "planner",
    session_id: "sess-alpha",
    public_lifecycle: "working",
    projection: { lifecycle: "working", agent: "running", delivery: "accepted", resource: "attached" },
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
