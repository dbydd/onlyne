// The `orca` CLI surface this plugin depends on (all read-only except the
// explicit `terminal switch` the user triggers from a command).
//
//    orca worktree list --json                       -> {result:{worktrees:[…]}}
//    orca terminal list --worktree <selector> --json -> {result:{terminals:[…]}}
//    orca terminal switch --terminal <handle> --json -> focus that tab in the UI
//    orca status --json                              -> app/runtime readiness
//
// Observed on Orca 1.4.198: `terminal list` rows carry tabId/leafId but no
// paneKey field, so paneKey is derived as `${tabId}:${leafId}` — the same
// spelling the backend writes into the mapping file.

export const WORKTREE_SELECTOR_PREFIX = "path:";

export function worktreeSelector(path) {
  return `${WORKTREE_SELECTOR_PREFIX}${path}`;
}

export function normalizeWorktreeRow(row) {
  if (!row || typeof row !== "object") return null;
  const path = typeof row.path === "string" && row.path ? row.path : null;
  if (!path) return null;
  return {
    worktreeId: typeof row.id === "string" ? row.id : null,
    instanceId: typeof row.instanceId === "string" ? row.instanceId : null,
    path,
    displayName: typeof row.displayName === "string" ? row.displayName : null,
    branch: typeof row.branch === "string" ? row.branch : null,
    isBare: row.isBare === true,
    isMainWorktree: row.isMainWorktree === true,
    kind: typeof row.kind === "string" ? row.kind : null,
  };
}

export function paneKeyOf(row) {
  if (!row || typeof row !== "object") return null;
  if (typeof row.paneKey === "string" && row.paneKey) return row.paneKey;
  const tabId = typeof row.tabId === "string" ? row.tabId : null;
  const leafId = typeof row.leafId === "string" ? row.leafId : null;
  return tabId && leafId ? `${tabId}:${leafId}` : null;
}

export function normalizeTerminalRow(row) {
  if (!row || typeof row !== "object") return null;
  const handle = typeof row.handle === "string" && row.handle ? row.handle : null;
  if (!handle) return null;
  return {
    handle,
    paneKey: paneKeyOf(row),
    tabId: typeof row.tabId === "string" ? row.tabId : null,
    leafId: typeof row.leafId === "string" ? row.leafId : null,
    title: typeof row.title === "string" ? row.title : null,
    connected: row.connected === true,
    writable: row.writable === true,
    orphaned: row.orphaned === true,
    lastOutputAt: typeof row.lastOutputAt === "number" ? row.lastOutputAt : null,
    agentIdentity: typeof row.agentIdentity === "string" ? row.agentIdentity : null,
    worktreeId: typeof row.worktreeId === "string" ? row.worktreeId : null,
    worktreePath: typeof row.worktreePath === "string" ? row.worktreePath : null,
    exitCause: row.exitCause ?? null,
  };
}

function rowsOf(value, key) {
  const rows = value?.[key];
  return Array.isArray(rows) ? rows : [];
}

export function createOrcaCli({ runner, binary = "orca" }) {
  async function listWorktrees(options) {
    const result = await runner.runJson(binary, ["worktree", "list", "--json"], options);
    if (!result.ok) return result;
    const rows = rowsOf(result.value, "worktrees").map(normalizeWorktreeRow).filter(Boolean);
    return { ok: true, rows };
  }

  async function listTerminals(selector, options) {
    const args = ["terminal", "list"];
    if (selector) args.push("--worktree", selector);
    args.push("--json");
    const result = await runner.runJson(binary, args, options);
    if (!result.ok) return result;
    const rows = rowsOf(result.value, "terminals").map(normalizeTerminalRow).filter(Boolean);
    return { ok: true, rows };
  }

  async function switchTerminal(handle, options) {
    return runner.runJson(binary, ["terminal", "switch", "--terminal", handle, "--json"], options);
  }

  async function status(options) {
    const result = await runner.runJson(binary, ["status", "--json"], options);
    if (!result.ok) return result;
    return { ok: true, value: result.value };
  }

  return { listWorktrees, listTerminals, switchTerminal, status };
}
