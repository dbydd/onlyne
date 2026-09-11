// Shared fixtures for the node:test suites (not a *.test.mjs file, so
// `node --test src/*.test.mjs` never runs it directly).

import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { MAPPING_RELATIVE_PATH } from "./mapping.mjs";
import { CLIENT_SOCKET_RELATIVE_PATH, normalizeSessionRow } from "./onlyne-cli.mjs";
import { normalizeTerminalRow } from "./orca-cli.mjs";

const created = [];

/** Creates a real role workspace directory under the system temp dir. */
export function makeWorkspace({ name = "role-ws", lines = [], socket = false } = {}) {
  const root = mkdtempSync(join(tmpdir(), `onlyne-plugin-${name}-`));
  created.push(root);
  const mapping = join(root, MAPPING_RELATIVE_PATH);
  mkdirSync(join(root, ".onlyne/cache"), { recursive: true });
  writeFileSync(mapping, lines.map((line) => `${JSON.stringify(line)}\n`).join(""));
  if (socket) {
    mkdirSync(join(root, ".onlyne/run"), { recursive: true });
    writeFileSync(join(root, CLIENT_SOCKET_RELATIVE_PATH), "");
  }
  return root;
}

export function makeDir({ name = "plain" } = {}) {
  const root = mkdtempSync(join(tmpdir(), `onlyne-plugin-${name}-`));
  created.push(root);
  return root;
}

export function cleanupAll() {
  for (const path of created.splice(0)) rmSync(path, { recursive: true, force: true });
}

export function mappingRow(overrides = {}) {
  return {
    pane_key: "tab-1:leaf-1",
    handle: "term_11111111-1111-4111-8111-111111111111",
    task_id: "task-alpha",
    session_id: "sess-alpha",
    role: "planner",
    worktree_selector: "path:/tmp/role",
    title: "onlyne:task-alpha",
    state: "spawned",
    updated_at: "2026-09-11T00:00:00Z",
    ...overrides,
  };
}

export function terminalRow(overrides = {}) {
  return {
    handle: "term_11111111-1111-4111-8111-111111111111",
    tabId: "tab-1",
    leafId: "leaf-1",
    title: "onlyne:task-alpha",
    connected: true,
    writable: true,
    lastOutputAt: 1_789_000_000_000,
    agentIdentity: "omp",
    ...overrides,
  };
}

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

/** Fake `orca` CLI with the same call surface as src/orca-cli.mjs. */
export function fakeOrca({
  worktrees = [],
  terminalsByPath = {},
  worktreeListFailure = null,
  terminalFailure = null,
  switchFailure = null,
} = {}) {
  const calls = { listWorktrees: 0, listTerminals: [], switchTerminal: [] };
  return {
    calls,
    async listWorktrees() {
      calls.listWorktrees += 1;
      if (worktreeListFailure) return worktreeListFailure;
      return { ok: true, rows: worktrees };
    },
    async listTerminals(selector) {
      calls.listTerminals.push(selector);
      if (terminalFailure) return terminalFailure;
      const path = selector.replace(/^path:/, "");
      const rows = (terminalsByPath[path] ?? []).map(normalizeTerminalRow).filter(Boolean);
      return { ok: true, rows };
    },
    async switchTerminal(handle) {
      calls.switchTerminal.push(handle);
      if (switchFailure) return switchFailure;
      return { ok: true, value: { switched: true } };
    },
  };
}

export function fakeOnlyne({ sessionsBySocket = {}, failure = null, calls = [] } = {}) {
  return {
    calls,
    async querySessions(socketPath) {
      calls.push(socketPath);
      if (failure) return failure;
      const rows = sessionsBySocket[socketPath];
      if (!rows) return { ok: false, code: "cli_surface_mismatch", message: "no such socket" };
      return { ok: true, sessions: rows.map(normalizeSessionRow).filter(Boolean) };
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
