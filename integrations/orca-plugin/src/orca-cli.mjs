// The `orca` CLI surface this plugin depends on (all read-only except the
// explicit `terminal switch` the user triggers from a command).
//
//    orca terminal list --json                       -> {result:{terminals:[…]}}
//    orca terminal switch --terminal <handle> --json -> focus that tab in the UI
//    orca status --json                              -> app/runtime readiness
//
// Measured on Orca 1.4.198: `terminal list` without `--worktree` answers every
// tab of every worktree in one flat list — handle, tabId, leafId, title,
// connected, writable, lastOutputAt, worktreeId. The plugin never asks per
// worktree: the backend no longer registers one worktree per role, so the
// supervisor's own worktree list is the whole tab axis. A row carries no
// `paneKey` on this build (newer builds may), so paneKey is derived as
// `${tabId}:${leafId}` — the spelling the tooling elsewhere uses for a pane.

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
    lastOutputAt: typeof row.lastOutputAt === "number" ? row.lastOutputAt : null,
    worktreeId: typeof row.worktreeId === "string" ? row.worktreeId : null,
    // The absolute worktree directory: the only field that says which swarm a
    // tab belongs to, since every session tab of one swarm shares a worktree.
    worktreePath: typeof row.worktreePath === "string" ? row.worktreePath : null,
  };
}

function rowsOf(value, key) {
  const rows = value?.[key];
  return Array.isArray(rows) ? rows : [];
}

export function createOrcaCli({ runner, binary = "orca" }) {
  /** Every tab of every worktree, flat: one call, no `--worktree` selector. */
  async function listTerminals() {
    const result = await runner.runJson(binary, ["terminal", "list", "--json"]);
    if (!result.ok) return result;
    const rows = rowsOf(result.value, "terminals").map(normalizeTerminalRow).filter(Boolean);
    return { ok: true, rows };
  }

  async function switchTerminal(handle) {
    return runner.runJson(binary, ["terminal", "switch", "--terminal", handle, "--json"]);
  }

  async function status() {
    const result = await runner.runJson(binary, ["status", "--json"]);
    if (!result.ok) return result;
    return { ok: true, value: result.value };
  }

  return { listTerminals, switchTerminal, status };
}
